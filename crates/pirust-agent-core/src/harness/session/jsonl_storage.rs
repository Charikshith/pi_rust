//! JSONL session storage — port of
//! `packages/agent/src/harness/session/jsonl-storage.ts`.
//!
//! Spec: `docs/analysis/07-agent-core-spec.md` §8 (`SessionStorage` impl), §1.4
//! (exact v3 per-entry key order), §11 (v1 legacy fixtures require the
//! v1-parse + v1→v3 migration adapter). `[LEAF]` depending on `harness::types`
//! + `session::uuid`.
//!
//! [`JsonlSessionStorage`] persists an append-only tree to a file: [`create`]
//! writes the v3 header line, then every entry is one compact `JSON.stringify`
//! line (byte-identical to Pi via serde — proven by `tests/session_golden.rs`).
//! The crate is `#![forbid(unsafe_code)]`; `std::fs` is used directly here per
//! the port brief (Pi injects a `FileSystem`; that seam is not needed for the v3
//! byte contract).
//!
//! # v1 legacy format
//!
//! v1 files have no `version` field and no per-entry `id` (spec §11.A). The
//! ported [`parse_header_line`] / [`parse_entry_line`] validators DETECT this and
//! return the documented `SessionError` (`"unsupported session version"` /
//! `"is missing entry id"`). The v1→v3 migration itself is intentionally
//! [deferred](migrate_v1_to_v3) — it is a coding-agent concern per §11.A.
//!
//! [`create`]: JsonlSessionStorage::create

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use async_trait::async_trait;
use serde_json::Value;

use super::uuid::{SystemSource, Uuidv7Generator, Uuidv7Source};
use super::{
    build_labels_by_id, entry_id, entry_parent_id, entry_type_tag, leaf_id_after_entry,
    update_label_cache, Clock,
};
use crate::harness::types::{
    JsonlSessionMetadata, SessionError, SessionErrorCode, SessionHeader, SessionHeaderTag,
    SessionStorage, SessionTreeEntry,
};

/// Options for [`JsonlSessionStorage::create`] (jsonl-storage.ts:213-218).
#[derive(Debug, Clone, Default)]
pub struct JsonlCreateOptions {
    pub cwd: String,
    pub session_id: String,
    pub parent_session_path: Option<String>,
    pub metadata: Option<Value>,
}

fn invalid_session(path: &Path, message: &str) -> SessionError {
    SessionError::new(
        SessionErrorCode::InvalidSession,
        format!("Invalid JSONL session file {}: {message}", path.display()),
    )
}

fn invalid_entry(path: &Path, line_number: usize, message: &str) -> SessionError {
    SessionError::new(
        SessionErrorCode::InvalidEntry,
        format!(
            "Invalid JSONL session file {}: line {line_number} {message}",
            path.display()
        ),
    )
}

fn storage_error(message: impl Into<String>, err: std::io::Error) -> SessionError {
    SessionError {
        code: SessionErrorCode::Storage,
        message: message.into(),
        source: Some(Box::new(err)),
    }
}

/// Parse + validate the v3 header line (jsonl-storage.ts:58-94). Rejects v1 files
/// (`version !== 3`) with `"unsupported session version"`.
fn parse_header_line(line: &str, path: &Path) -> Result<SessionHeader, SessionError> {
    let parsed: Value = serde_json::from_str(line)
        .map_err(|_| invalid_session(path, "first line is not a valid session header"))?;
    let obj = parsed
        .as_object()
        .ok_or_else(|| invalid_session(path, "first line is not a valid session header"))?;

    if obj.get("type").and_then(Value::as_str) != Some("session") {
        return Err(invalid_session(
            path,
            "first line is not a valid session header",
        ));
    }
    if obj.get("version").and_then(Value::as_u64) != Some(3) {
        return Err(invalid_session(path, "unsupported session version"));
    }
    let id = obj
        .get("id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let Some(id) = id else {
        return Err(invalid_session(path, "session header is missing id"));
    };
    let timestamp = obj
        .get("timestamp")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let Some(timestamp) = timestamp else {
        return Err(invalid_session(path, "session header is missing timestamp"));
    };
    let cwd = obj
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let Some(cwd) = cwd else {
        return Err(invalid_session(path, "session header is missing cwd"));
    };
    let parent_session = match obj.get("parentSession") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(_) => {
            return Err(invalid_session(
                path,
                "session header parentSession must be a string",
            ))
        }
    };
    let metadata = match obj.get("metadata") {
        None | Some(Value::Null) => None,
        Some(v @ Value::Object(_)) => Some(v.clone()),
        Some(_) => {
            return Err(invalid_session(
                path,
                "session header metadata must be an object",
            ))
        }
    };

    Ok(SessionHeader {
        kind: SessionHeaderTag::Session,
        version: 3,
        id: id.to_string(),
        timestamp: timestamp.to_string(),
        cwd: cwd.to_string(),
        parent_session,
        metadata,
    })
}

/// Parse + validate one entry line (jsonl-storage.ts:96-125). Rejects v1 entries
/// (no `id`) with `"is missing entry id"`, then decodes the v3 shape.
fn parse_entry_line(
    line: &str,
    path: &Path,
    line_number: usize,
) -> Result<SessionTreeEntry, SessionError> {
    let parsed: Value = serde_json::from_str(line)
        .map_err(|_| invalid_entry(path, line_number, "is not valid JSON"))?;
    let obj = parsed
        .as_object()
        .ok_or_else(|| invalid_entry(path, line_number, "is not a valid session entry"))?;

    if !matches!(obj.get("type"), Some(Value::String(_))) {
        return Err(invalid_entry(path, line_number, "is missing entry type"));
    }
    if obj
        .get("id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .is_none()
    {
        return Err(invalid_entry(path, line_number, "is missing entry id"));
    }
    match obj.get("parentId") {
        Some(Value::Null) | Some(Value::String(_)) => {}
        _ => return Err(invalid_entry(path, line_number, "has invalid parentId")),
    }
    if obj
        .get("timestamp")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .is_none()
    {
        return Err(invalid_entry(path, line_number, "is missing timestamp"));
    }
    if obj.get("type").and_then(Value::as_str) == Some("leaf") {
        match obj.get("targetId") {
            Some(Value::Null) | Some(Value::String(_)) => {}
            _ => return Err(invalid_entry(path, line_number, "has invalid targetId")),
        }
    }

    serde_json::from_value(parsed)
        .map_err(|_| invalid_entry(path, line_number, "is not a valid session entry"))
}

fn header_to_metadata(header: &SessionHeader, path: &Path) -> JsonlSessionMetadata {
    JsonlSessionMetadata {
        id: header.id.clone(),
        created_at: header.timestamp.clone(),
        cwd: header.cwd.clone(),
        path: path.display().to_string(),
        parent_session_path: header.parent_session.clone(),
        metadata: header.metadata.clone(),
    }
}

/// v1 → v3 session migration.
///
/// TODO(feat-003): DEFERRED. Full migration (rebuilding per-entry `id`/`parentId`
/// links and converting `firstKeptEntryIndex` → `firstKeptEntryId`) is a
/// coding-agent concern per spec §11.A and is out of scope for the v3 read/write
/// port. [`JsonlSessionStorage::open`] rejects v1 files up front via
/// [`parse_header_line`] (`"unsupported session version"`); this stub records the
/// intended entry point.
pub fn migrate_v1_to_v3(_lines: &[String]) -> Result<Vec<SessionTreeEntry>, SessionError> {
    Err(SessionError::new(
        SessionErrorCode::Unknown,
        "v1→v3 session migration is not implemented (deferred; see spec §11.A)",
    ))
}

/// Interior tree state guarded by the storage `Mutex`.
struct State<S: Uuidv7Source> {
    entries: Vec<SessionTreeEntry>,
    by_id: HashMap<String, SessionTreeEntry>,
    labels_by_id: HashMap<String, String>,
    current_leaf_id: Option<String>,
    generator: Uuidv7Generator<S>,
}

impl<S: Uuidv7Source> State<S> {
    /// `generateEntryId` (jsonl-storage.ts:36-44).
    fn next_entry_id(&mut self) -> String {
        for _ in 0..100 {
            let full = self.generator.generate();
            let short = full[full.len() - 8..].to_string();
            if !self.by_id.contains_key(&short) {
                return short;
            }
        }
        self.generator.generate()
    }
}

/// File-backed [`SessionStorage`] (jsonl-storage.ts:180).
pub struct JsonlSessionStorage<S: Uuidv7Source + Send + Sync = SystemSource> {
    path: PathBuf,
    metadata: JsonlSessionMetadata,
    clock: Box<dyn Clock>,
    state: Mutex<State<S>>,
}

fn write_file(path: &Path, contents: &str) -> Result<(), SessionError> {
    std::fs::write(path, contents)
        .map_err(|e| storage_error(format!("Failed to write session {}", path.display()), e))
}

fn append_file(path: &Path, contents: &str) -> Result<(), SessionError> {
    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| storage_error(format!("Failed to open session {}", path.display()), e))?;
    file.write_all(contents.as_bytes())
        .map_err(|e| storage_error(format!("Failed to append session {}", path.display()), e))
}

impl JsonlSessionStorage<SystemSource> {
    /// Create a new v3 session file, writing the header line and returning an
    /// empty store (jsonl-storage.ts:210-234). System uuid source; the header
    /// `timestamp` comes from `clock`.
    pub fn create(
        path: impl Into<PathBuf>,
        options: JsonlCreateOptions,
        clock: Box<dyn Clock>,
    ) -> Result<Self, SessionError> {
        Self::create_with_source(path, options, clock, SystemSource)
    }

    /// Open an existing v3 session file, replaying its entries (jsonl-storage.ts:205-208).
    /// v1 files are rejected via [`parse_header_line`].
    pub fn open(path: impl Into<PathBuf>, clock: Box<dyn Clock>) -> Result<Self, SessionError> {
        Self::open_with_source(path, clock, SystemSource)
    }
}

impl<S: Uuidv7Source + Send + Sync + 'static> JsonlSessionStorage<S> {
    /// [`JsonlSessionStorage::create`] with an injected uuid `source`.
    pub fn create_with_source(
        path: impl Into<PathBuf>,
        options: JsonlCreateOptions,
        clock: Box<dyn Clock>,
        source: S,
    ) -> Result<Self, SessionError> {
        let path = path.into();
        let header = SessionHeader {
            kind: SessionHeaderTag::Session,
            version: 3,
            id: options.session_id,
            timestamp: clock.now_iso(),
            cwd: options.cwd,
            parent_session: options.parent_session_path,
            metadata: options.metadata,
        };
        let line = serde_json::to_string(&header).map_err(|e| {
            SessionError::new(
                SessionErrorCode::Storage,
                format!("Failed to encode header: {e}"),
            )
        })?;
        write_file(&path, &format!("{line}\n"))?;
        let metadata = header_to_metadata(&header, &path);
        Ok(Self {
            path,
            metadata,
            clock,
            state: Mutex::new(State {
                entries: Vec::new(),
                by_id: HashMap::new(),
                labels_by_id: HashMap::new(),
                current_leaf_id: None,
                generator: Uuidv7Generator::with_source(source),
            }),
        })
    }

    /// [`JsonlSessionStorage::open`] with an injected uuid `source`.
    pub fn open_with_source(
        path: impl Into<PathBuf>,
        clock: Box<dyn Clock>,
        source: S,
    ) -> Result<Self, SessionError> {
        let path = path.into();
        let content = std::fs::read_to_string(&path)
            .map_err(|e| storage_error(format!("Failed to read session {}", path.display()), e))?;
        let lines: Vec<&str> = content
            .split('\n')
            .filter(|l| !l.trim().is_empty())
            .collect();
        if lines.is_empty() {
            return Err(invalid_session(&path, "missing session header"));
        }
        let header = parse_header_line(lines[0], &path)?;
        let mut entries: Vec<SessionTreeEntry> = Vec::new();
        let mut current_leaf_id: Option<String> = None;
        for (i, line) in lines.iter().enumerate().skip(1) {
            let entry = parse_entry_line(line, &path, i + 1)?;
            current_leaf_id = leaf_id_after_entry(&entry);
            entries.push(entry);
        }
        let by_id: HashMap<String, SessionTreeEntry> = entries
            .iter()
            .map(|e| (entry_id(e).to_string(), e.clone()))
            .collect();
        let labels_by_id = build_labels_by_id(&entries);
        let metadata = header_to_metadata(&header, &path);
        Ok(Self {
            path,
            metadata,
            clock,
            state: Mutex::new(State {
                entries,
                by_id,
                labels_by_id,
                current_leaf_id,
                generator: Uuidv7Generator::with_source(source),
            }),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State<S>> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }
}

#[async_trait]
impl<S: Uuidv7Source + Send + Sync + 'static> SessionStorage for JsonlSessionStorage<S> {
    type Metadata = JsonlSessionMetadata;

    async fn get_metadata(&self) -> Result<Self::Metadata, SessionError> {
        Ok(self.metadata.clone())
    }

    async fn get_leaf_id(&self) -> Result<Option<String>, SessionError> {
        let state = self.lock();
        if let Some(ref lid) = state.current_leaf_id {
            if !state.by_id.contains_key(lid) {
                return Err(SessionError::new(
                    SessionErrorCode::InvalidSession,
                    format!("Entry {lid} not found"),
                ));
            }
        }
        Ok(state.current_leaf_id.clone())
    }

    async fn set_leaf_id(&self, leaf_id: Option<String>) -> Result<(), SessionError> {
        let (entry, line) = {
            let mut state = self.lock();
            if let Some(ref lid) = leaf_id {
                if !state.by_id.contains_key(lid) {
                    return Err(SessionError::new(
                        SessionErrorCode::NotFound,
                        format!("Entry {lid} not found"),
                    ));
                }
            }
            let entry = SessionTreeEntry::Leaf {
                id: state.next_entry_id(),
                parent_id: state.current_leaf_id.clone(),
                timestamp: self.clock.now_iso(),
                target_id: leaf_id.clone(),
            };
            let line = serde_json::to_string(&entry).map_err(|e| {
                SessionError::new(
                    SessionErrorCode::Storage,
                    format!("Failed to encode leaf: {e}"),
                )
            })?;
            (entry, line)
        };
        append_file(&self.path, &format!("{line}\n"))?;
        let mut state = self.lock();
        let key = entry_id(&entry).to_string();
        state.entries.push(entry.clone());
        state.by_id.insert(key, entry);
        state.current_leaf_id = leaf_id;
        Ok(())
    }

    async fn create_entry_id(&self) -> Result<String, SessionError> {
        Ok(self.lock().next_entry_id())
    }

    async fn append_entry(&self, entry: SessionTreeEntry) -> Result<(), SessionError> {
        let line = serde_json::to_string(&entry).map_err(|e| {
            SessionError::new(
                SessionErrorCode::Storage,
                format!("Failed to encode entry: {e}"),
            )
        })?;
        append_file(&self.path, &format!("{line}\n"))?;
        let mut state = self.lock();
        let key = entry_id(&entry).to_string();
        state.entries.push(entry.clone());
        state.by_id.insert(key, entry.clone());
        update_label_cache(&mut state.labels_by_id, &entry);
        state.current_leaf_id = leaf_id_after_entry(&entry);
        Ok(())
    }

    async fn get_entry(&self, id: &str) -> Result<Option<SessionTreeEntry>, SessionError> {
        Ok(self.lock().by_id.get(id).cloned())
    }

    async fn find_entries(&self, entry_type: &str) -> Result<Vec<SessionTreeEntry>, SessionError> {
        Ok(self
            .lock()
            .entries
            .iter()
            .filter(|e| entry_type_tag(e) == entry_type)
            .cloned()
            .collect())
    }

    async fn get_label(&self, id: &str) -> Result<Option<String>, SessionError> {
        Ok(self.lock().labels_by_id.get(id).cloned())
    }

    async fn get_path_to_root(
        &self,
        leaf_id: Option<String>,
    ) -> Result<Vec<SessionTreeEntry>, SessionError> {
        let Some(leaf_id) = leaf_id else {
            return Ok(Vec::new());
        };
        let state = self.lock();
        let mut path: Vec<SessionTreeEntry> = Vec::new();
        let mut current = state.by_id.get(&leaf_id).cloned().ok_or_else(|| {
            SessionError::new(
                SessionErrorCode::NotFound,
                format!("Entry {leaf_id} not found"),
            )
        })?;
        loop {
            path.insert(0, current.clone());
            let Some(parent_id) = entry_parent_id(&current).map(str::to_string) else {
                break;
            };
            let parent = state.by_id.get(&parent_id).cloned().ok_or_else(|| {
                SessionError::new(
                    SessionErrorCode::InvalidSession,
                    format!("Entry {parent_id} not found"),
                )
            })?;
            current = parent;
        }
        Ok(path)
    }

    async fn get_entries(&self) -> Result<Vec<SessionTreeEntry>, SessionError> {
        Ok(self.lock().entries.clone())
    }
}
