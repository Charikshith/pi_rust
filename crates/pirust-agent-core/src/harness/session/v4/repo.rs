//! v4 JSONL session repo — port of `packages/agent/src/harness/session/jsonl/repo.ts`.
//!
//! [`JsonlSessionRepo`] owns the on-disk layout: a `sessionsRoot` containing one
//! directory per cwd (named `--<cwd with separators>--`) holding session files
//! named `<ISO-timestamp>_<id>.jsonl`. It validates session ids, guards
//! same-process create/fork races, lists sessions by reading each file's header
//! line, and opens/fork/delete sessions by path.

use std::collections::HashSet;
use std::sync::Arc;

use super::codec::{metadata_from_header, parse_header, JsonlV4Header, V4FileSystem};
use super::session::Session;
use super::storage::JsonlSessionStorage;
use super::types::{JsonlSessionCreateOptions, JsonlSessionListOptions, JsonlSessionMetadata};
use crate::harness::types::{SessionError, SessionErrorCode};

/// `SESSION_ID_PATTERN` (repo.ts:25).
fn validate_session_id(id: &str) -> Result<(), SessionError> {
    let valid = !id.is_empty()
        && id.len() <= 100
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
        && id.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
        && id.chars().last().is_some_and(|c| c.is_ascii_alphanumeric());
    if valid {
        Ok(())
    } else {
        Err(SessionError::new(
            SessionErrorCode::InvalidPayload,
            "Session id must be non-empty, contain only alphanumeric characters, '-', '_', and '.', and start and end with an alphanumeric character",
        ))
    }
}

/// `jsonlSessionDirectoryName` (repo.ts:27-29) — encode a cwd as a directory
/// name: strip a leading `/` or `\`, replace separators/colons with `-`, wrap
/// in `--...--`.
pub fn jsonl_session_directory_name(cwd: &str) -> String {
    let stripped = cwd.trim_start_matches(['/', '\\']);
    let replaced = stripped
        .chars()
        .map(|c| {
            if c == '/' || c == '\\' || c == ':' {
                '-'
            } else {
                c
            }
        })
        .collect::<String>();
    format!("--{replaced}--")
}

/// `sessionFileName` (repo.ts:99-101) — `<ISO-timestamp with separators>_<id>.jsonl`.
pub fn session_file_name(created_at: i64, id: &str) -> String {
    // Pi: new Date(createdAt).toISOString().replace(/[:.]/g, "-")
    let iso = iso_ts(created_at);
    format!("{iso}_{id}.jsonl")
}

/// `Date.prototype.toISOString()` with `:` and `.` replaced by `-`.
fn iso_ts(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let millis = ms.rem_euclid(1000);
    let days = secs.div_euclid(86400);
    let sec_of_day = secs.rem_euclid(86400);
    // Civil-from-days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    let h = sec_of_day / 3600;
    let mi = (sec_of_day % 3600) / 60;
    let s = sec_of_day % 60;
    format!("{y:04}-{m:02}-{d:02}T{h:02}-{mi:02}-{s:02}-{millis:03}Z")
}

/// Error helper for repo-level fs failures — `fileResult` (errors.ts:11-21).
fn file_result<T>(result: Result<T, String>, message: &str) -> Result<T, SessionError> {
    match result {
        Ok(value) => Ok(value),
        Err(err) => {
            let code = if err == "not_found" {
                SessionErrorCode::NotFound
            } else {
                SessionErrorCode::Storage
            };
            Err(SessionError::new(code, format!("{message}: {err}")))
        }
    }
}

/// `JsonlSessionRepo` (repo.ts:103-218).
pub struct JsonlSessionRepo {
    fs: Arc<dyn V4FileSystem>,
    sessions_root: String,
    /// Same-process create/fork race guard (`activeCreateDestinations`).
    active_create_destinations: std::sync::Mutex<HashSet<String>>,
}

impl JsonlSessionRepo {
    /// `constructor` (repo.ts:114-118).
    pub fn new(fs: Arc<dyn V4FileSystem>, sessions_root: String) -> Self {
        Self {
            fs,
            sessions_root,
            active_create_destinations: std::sync::Mutex::new(HashSet::new()),
        }
    }

    /// `create` (repo.ts:121-124).
    pub fn create(
        &self,
        options: &JsonlSessionCreateOptions,
    ) -> Result<Session<JsonlSessionStorage>, SessionError> {
        let destination = self.resolve_create_destination(options)?;
        self.claim_create_destination(&destination, || {
            let (header, path) = self.prepare_create(&destination, options)?;
            let storage = JsonlSessionStorage::create(self.fs.clone(), &path, &header)?;
            Ok(Session::new(Arc::new(storage)))
        })
    }

    /// `open` (repo.ts:126-128).
    pub fn open(
        &self,
        metadata: &JsonlSessionMetadata,
    ) -> Result<Session<JsonlSessionStorage>, SessionError> {
        let storage = self.load_storage(metadata)?;
        Ok(Session::new(Arc::new(storage)))
    }

    /// `list` (repo.ts:130-132).
    pub fn list(
        &self,
        options: &JsonlSessionListOptions,
    ) -> Result<Vec<JsonlSessionMetadata>, SessionError> {
        self.list_direct(options)
    }

    /// `delete` (repo.ts:134-136).
    pub fn delete(&self, metadata: &JsonlSessionMetadata) -> Result<(), SessionError> {
        file_result(
            self.fs.remove(&metadata.path, true),
            &format!("Failed to delete session {}", metadata.path),
        )
    }

    /// `fork` (repo.ts:138-154).
    pub fn fork(
        &self,
        source: &JsonlSessionMetadata,
        options: &JsonlSessionCreateOptions,
        fork_options: &super::types::ForkOptions,
    ) -> Result<Session<JsonlSessionStorage>, SessionError> {
        let source_storage = self.load_storage(source)?;
        let create_options = JsonlSessionCreateOptions {
            id: options.id.clone(),
            parent_session_id: options
                .parent_session_id
                .clone()
                .or(Some(source.id.clone())),
            cwd: options.cwd.clone(),
            metadata: options.metadata.clone(),
        };
        let destination = self.resolve_create_destination(&create_options)?;
        self.claim_create_destination(&destination, || {
            let (header, path) = self.prepare_create(&destination, &create_options)?;
            let storage = source_storage.fork(&path, &header, fork_options)?;
            Ok(Session::new(Arc::new(storage)))
        })
    }

    // ------------------------------------------------------------------
    // Private helpers
    // ------------------------------------------------------------------

    /// `loadStorage` (repo.ts:156-158).
    fn load_storage(
        &self,
        metadata: &JsonlSessionMetadata,
    ) -> Result<JsonlSessionStorage, SessionError> {
        load_jsonl_session_storage(self.fs.clone(), &self.sessions_root, metadata)
    }

    /// `resolveCreateDestination` (repo.ts:160-164).
    fn resolve_create_destination(
        &self,
        options: &JsonlSessionCreateOptions,
    ) -> Result<(String, String), SessionError> {
        let id = options
            .id
            .clone()
            .unwrap_or_else(crate::harness::session::uuid::create_session_id);
        validate_session_id(&id)?;
        let cwd = file_result(
            self.fs.absolute_path(&options.cwd),
            &format!("Failed to resolve session cwd {}", options.cwd),
        )?;
        Ok((id, cwd))
    }

    /// `claimCreateDestination` (repo.ts:169-184).
    fn claim_create_destination<T>(
        &self,
        destination: &(String, String),
        operation: impl FnOnce() -> Result<T, SessionError>,
    ) -> Result<T, SessionError> {
        let key = format!("{}\0{}", destination.1, destination.0);
        let mut active = self
            .active_create_destinations
            .lock()
            .map_err(|_| SessionError::new(SessionErrorCode::Storage, "repo lock poisoned"))?;
        if active.contains(&key) {
            return Err(SessionError::new(
                SessionErrorCode::AlreadyExists,
                format!("Session already exists: {}", destination.0),
            ));
        }
        active.insert(key.clone());
        drop(active);
        let result = operation();
        let mut active = self
            .active_create_destinations
            .lock()
            .map_err(|_| SessionError::new(SessionErrorCode::Storage, "repo lock poisoned"))?;
        active.remove(&key);
        result
    }

    /// `prepareCreate` (repo.ts:186-207).
    fn prepare_create(
        &self,
        destination: &(String, String),
        options: &JsonlSessionCreateOptions,
    ) -> Result<(JsonlV4Header, String), SessionError> {
        let (id, cwd) = destination;
        if self.session_id_exists(id, cwd)? {
            return Err(SessionError::new(
                SessionErrorCode::AlreadyExists,
                format!("Session already exists: {id}"),
            ));
        }
        let created_at = now_ms();
        let session_directory = self.session_directory(cwd)?;
        let path = file_result(
            self.fs
                .join_path(&[session_directory.clone(), session_file_name(created_at, id)]),
            &format!("Failed to resolve path for session {id}"),
        )?;
        let header = JsonlV4Header {
            kind: "header".to_string(),
            version: 4,
            id: id.clone(),
            created_at,
            cwd: cwd.clone(),
            parent_session_id: options.parent_session_id.clone(),
            legacy_parent_session_path: None,
            metadata: options.metadata.clone(),
        };
        file_result(
            self.fs.create_dir(&session_directory, true),
            "Failed to create sessions directory",
        )?;
        Ok((header, path))
    }

    /// `listDirect` (repo.ts:209-211).
    fn list_direct(
        &self,
        options: &JsonlSessionListOptions,
    ) -> Result<Vec<JsonlSessionMetadata>, SessionError> {
        list_jsonl_session_metadata(&self.fs, &self.sessions_root, options)
    }

    /// `sessionIdExists` (repo.ts:213-219) — any non-directory file whose name
    /// ends with `_<id>.jsonl`.
    fn session_id_exists(&self, id: &str, cwd: &str) -> Result<bool, SessionError> {
        let suffix = format!("_{id}.jsonl");
        let directory = self.session_directory(cwd)?;
        let exists = file_result(
            self.fs.exists(&directory),
            &format!("Failed to check sessions directory {directory}"),
        )?;
        if !exists {
            return Ok(false);
        }
        let entries = file_result(
            self.fs.list_dir(&directory),
            &format!("Failed to list sessions directory {directory}"),
        )?;
        Ok(entries
            .iter()
            .any(|entry| entry.kind != "directory" && entry.name.ends_with(&suffix)))
    }

    /// `sessionDirectory` (repo.ts:221-224).
    fn session_directory(&self, cwd: &str) -> Result<String, SessionError> {
        file_result(
            self.fs
                .join_path(&[self.root()?, jsonl_session_directory_name(cwd)]),
            &format!("Failed to resolve sessions directory for {cwd}"),
        )
    }

    /// `root` (repo.ts:226-230) — memoized absolute sessions root.
    fn root(&self) -> Result<String, SessionError> {
        file_result(
            self.fs.absolute_path(&self.sessions_root),
            &format!("Failed to resolve sessions root {}", self.sessions_root),
        )
    }
}

/// `listJsonlSessionMetadata` (repo.ts:46-57).
pub fn list_jsonl_session_metadata(
    fs: &Arc<dyn V4FileSystem>,
    sessions_root: &str,
    query: &JsonlSessionListOptions,
) -> Result<Vec<JsonlSessionMetadata>, SessionError> {
    let mut metadata = Vec::new();
    for directory in jsonl_session_directories(fs, sessions_root, query)? {
        let entries = file_result(
            fs.list_dir(&directory),
            &format!("Failed to list sessions directory {directory}"),
        )?
        .into_iter()
        .filter(|entry| entry.kind != "directory" && entry.name.ends_with(".jsonl"));
        for entry in entries {
            let first_line = file_result(
                fs.read_text_lines(&entry.path, Some(1)),
                &format!("Failed to read session header {}", entry.path),
            )?
            .into_iter()
            .next();
            let Some(first_line) = first_line else {
                continue;
            };
            let header_result = parse_header(&first_line);
            let Ok(header) = header_result else { continue };
            metadata.push(metadata_from_header(
                &header,
                entry.path.clone(),
                entry.mtime_ms,
            ));
        }
    }
    metadata.sort_by_key(|right| std::cmp::Reverse(right.modified_at));
    Ok(metadata)
}

/// `jsonlSessionDirectories` (repo.ts:33-44) — the cwd-encoded directories to
/// scan, optionally filtered to one cwd.
fn jsonl_session_directories(
    fs: &Arc<dyn V4FileSystem>,
    sessions_root: &str,
    options: &JsonlSessionListOptions,
) -> Result<Vec<String>, SessionError> {
    let sessions_root = jsonl_sessions_root(fs, sessions_root)?;
    if let Some(cwd) = &options.cwd {
        let resolved_cwd = file_result(
            fs.absolute_path(cwd),
            &format!("Failed to resolve session cwd {cwd}"),
        )?;
        let directory = jsonl_session_directory(fs, &sessions_root, &resolved_cwd)?;
        let exists = file_result(
            fs.exists(&directory),
            &format!("Failed to check sessions directory {directory}"),
        )?;
        return Ok(if exists { vec![directory] } else { vec![] });
    }
    let exists = file_result(
        fs.exists(&sessions_root),
        &format!("Failed to check sessions directory {sessions_root}"),
    )?;
    if !exists {
        return Ok(vec![]);
    }
    Ok(file_result(
        fs.list_dir(&sessions_root),
        &format!("Failed to list sessions directory {sessions_root}"),
    )?
    .into_iter()
    .filter(|entry| entry.kind == "directory" || entry.kind == "symlink")
    .map(|entry| entry.path)
    .collect())
}

/// `jsonlSessionsRoot` (repo.ts:31-32).
fn jsonl_sessions_root(
    fs: &Arc<dyn V4FileSystem>,
    sessions_root: &str,
) -> Result<String, SessionError> {
    file_result(
        fs.absolute_path(sessions_root),
        &format!("Failed to resolve sessions root {sessions_root}"),
    )
}

/// `jsonlSessionDirectory` (repo.ts:33-36).
fn jsonl_session_directory(
    fs: &Arc<dyn V4FileSystem>,
    sessions_root: &str,
    cwd: &str,
) -> Result<String, SessionError> {
    file_result(
        fs.join_path(&[sessions_root.to_string(), jsonl_session_directory_name(cwd)]),
        &format!("Failed to resolve sessions directory for {cwd}"),
    )
}

/// `loadJsonlSessionStorage` (repo.ts:59-68).
pub fn load_jsonl_session_storage(
    fs: Arc<dyn V4FileSystem>,
    _sessions_root: &str,
    metadata: &JsonlSessionMetadata,
) -> Result<JsonlSessionStorage, SessionError> {
    let exists = file_result(
        fs.exists(&metadata.path),
        &format!("Failed to check session {}", metadata.path),
    )?;
    if !exists {
        return Err(SessionError::new(
            SessionErrorCode::NotFound,
            format!("Session not found: {}", metadata.id),
        ));
    }
    let storage = JsonlSessionStorage::load(fs, &metadata.path)?;
    let loaded_metadata = storage.get_metadata()?;
    if loaded_metadata.id != metadata.id {
        return Err(SessionError::new(
            SessionErrorCode::InvalidEntry,
            format!("Session id does not match header: {}", metadata.id),
        ));
    }
    Ok(storage)
}

/// `Date.now()` — wall clock in milliseconds.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// `SessionRepo` impl (session/types.ts:361-378)
// ---------------------------------------------------------------------------

impl super::types::SessionRepo for JsonlSessionRepo {
    type Session = Session<JsonlSessionStorage>;
    type Metadata = JsonlSessionMetadata;
    type CreateOptions = JsonlSessionCreateOptions;
    type ListOptions = JsonlSessionListOptions;
    type ForkOptions = super::types::JsonlSessionForkOptions;

    fn create(&self, options: Self::CreateOptions) -> Result<Self::Session, SessionError> {
        self.create(&options)
    }

    fn open(&self, metadata: Self::Metadata) -> Result<Self::Session, SessionError> {
        self.open(&metadata)
    }

    fn list(
        &self,
        options: Option<Self::ListOptions>,
    ) -> Result<Vec<Self::Metadata>, SessionError> {
        self.list(&options.unwrap_or_default())
    }

    fn delete(&self, metadata: Self::Metadata) -> Result<(), SessionError> {
        self.delete(&metadata)
    }

    fn fork(
        &self,
        source: Self::Metadata,
        options: Self::ForkOptions,
    ) -> Result<Self::Session, SessionError> {
        let create_options = JsonlSessionCreateOptions {
            id: options.id,
            parent_session_id: options.parent_session_id,
            cwd: options.cwd,
            metadata: options.metadata,
        };
        let fork_options = options.fork.unwrap_or(super::types::ForkOptions::Tree);
        self.fork(&source, &create_options, &fork_options)
    }
}
