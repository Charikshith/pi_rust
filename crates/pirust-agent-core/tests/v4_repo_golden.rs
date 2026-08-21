//! Golden replay for the v4 session repo against
//! `tests/fixtures/pi/agent/v4/repo.cases.jsonl` (captured from real Pi
//! 0.84.2's `harness/session/jsonl/repo.ts` + `session.ts`).
//!
//! The oracle drove Pi's real `JsonlSessionRepo` against a byte-recording mock
//! FileSystem and recorded per scenario: the metadata contract (with
//! timestamps/ISO filenames/uuidv7 ids normalized to `<TS>`/`<UUID>`/0
//! placeholders), list results, error codes/messages, and the fs log. The Rust
//! repo must reproduce the same names, metadata, ordering, and errors.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use pirust_agent_core::harness::session::v4::codec::{DirEntry, FileInfo, V4FileSystem};
use pirust_agent_core::harness::session::v4::repo::JsonlSessionRepo;
use pirust_agent_core::harness::session::v4::types::{
    ForkOptions, JsonlSessionCreateOptions, JsonlSessionListOptions, JsonlSessionMetadata,
    LanePointer, LogOptions, NewOperationStartedRecord, NewRecord, OperationIntent, RecordQuery,
};

// -- in-memory FileSystem double with real directory semantics ---------------

#[derive(Default)]
struct MockFs {
    files: Mutex<BTreeMap<String, String>>,
    dirs: Mutex<BTreeMap<String, Vec<String>>>, // dir -> child names
}

impl MockFs {
    fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn mkdir_p(&self, path: &str) {
        let mut dirs = self.dirs.lock().unwrap();
        let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        let mut acc = String::new();
        let mut prev: Option<String> = None;
        for part in parts {
            acc.push('/');
            acc.push_str(part);
            let is_new = !dirs.contains_key(&acc);
            dirs.entry(acc.clone()).or_default();
            if is_new {
                if let Some(p) = &prev {
                    if let Some(list) = dirs.get_mut(p) {
                        if !list.contains(&part.to_string()) {
                            list.push(part.to_string());
                        }
                    }
                }
            }
            prev = Some(acc.clone());
        }
    }

    fn register_name(&self, parent: &str, name: &str) {
        let mut dirs = self.dirs.lock().unwrap();
        if let Some(list) = dirs.get_mut(parent) {
            if !list.contains(&name.to_string()) {
                list.push(name.to_string());
            }
        }
    }

    fn file(&self, path: &str) -> Option<String> {
        self.files.lock().unwrap().get(path).cloned()
    }
}

impl V4FileSystem for MockFs {
    fn absolute_path(&self, path: &str) -> Result<String, String> {
        Ok(path.to_string())
    }
    fn join_path(&self, parts: &[String]) -> Result<String, String> {
        Ok(parts.join("/"))
    }
    fn read_text_file(&self, path: &str) -> Result<String, String> {
        self.files
            .lock()
            .unwrap()
            .get(path)
            .cloned()
            .ok_or_else(|| format!("no such file {path}"))
    }
    fn read_text_lines(&self, path: &str, max_lines: Option<usize>) -> Result<Vec<String>, String> {
        let text = self.read_text_file(path)?;
        let lines: Vec<String> = text.split('\n').map(str::to_string).collect();
        Ok(match max_lines {
            Some(n) => lines.into_iter().take(n).collect(),
            None => lines,
        })
    }
    fn write_file(&self, path: &str, contents: &str) -> Result<(), String> {
        let idx = path.rfind('/');
        let (parent, name) = match idx {
            Some(i) => (&path[..i], &path[i + 1..]),
            None => ("", path),
        };
        if !parent.is_empty() {
            self.mkdir_p(parent);
            self.register_name(parent, name);
        }
        self.files
            .lock()
            .unwrap()
            .insert(path.to_string(), contents.to_string());
        Ok(())
    }
    fn append_file(&self, path: &str, contents: &str) -> Result<(), String> {
        let idx = path.rfind('/');
        let (parent, name) = match idx {
            Some(i) => (&path[..i], &path[i + 1..]),
            None => ("", path),
        };
        if !parent.is_empty() {
            self.mkdir_p(parent);
            self.register_name(parent, name);
        }
        let mut files = self.files.lock().unwrap();
        let entry = files.entry(path.to_string()).or_default();
        entry.push_str(contents);
        Ok(())
    }
    fn rename_file(&self, from: &str, to: &str) -> Result<(), String> {
        let value = self.files.lock().unwrap().get(from).cloned();
        match value {
            Some(v) => {
                self.files.lock().unwrap().remove(from);
                let idx = to.rfind('/');
                let (parent, name) = match idx {
                    Some(i) => (&to[..i], &to[i + 1..]),
                    None => ("", to),
                };
                if !parent.is_empty() {
                    self.mkdir_p(parent);
                    self.register_name(parent, name);
                }
                self.files.lock().unwrap().insert(to.to_string(), v);
                Ok(())
            }
            None => Err(format!("no such file {from}")),
        }
    }
    fn file_info(&self, path: &str) -> Result<FileInfo, String> {
        if self.files.lock().unwrap().contains_key(path) {
            return Ok(FileInfo {
                mtime_ms: 1_700_000_000_000,
            });
        }
        Err(format!("no such file {path}"))
    }
    fn list_dir(&self, path: &str) -> Result<Vec<DirEntry>, String> {
        let dirs = self.dirs.lock().unwrap();
        let names = dirs.get(path).cloned().unwrap_or_default();
        Ok(names
            .into_iter()
            .map(|name| {
                let full = format!("{path}/{name}");
                let is_dir = dirs.contains_key(&full);
                DirEntry {
                    kind: if is_dir { "directory" } else { "file" }.to_string(),
                    name,
                    path: full,
                    mtime_ms: 1_700_000_000_000,
                }
            })
            .collect())
    }
    fn exists(&self, path: &str) -> Result<bool, String> {
        Ok(self.files.lock().unwrap().contains_key(path)
            || self.dirs.lock().unwrap().contains_key(path))
    }
    fn create_dir(&self, path: &str, _recursive: bool) -> Result<(), String> {
        self.mkdir_p(path);
        Ok(())
    }
    fn remove(&self, path: &str, _force: bool) -> Result<(), String> {
        self.files.lock().unwrap().remove(path);
        let idx = path.rfind('/');
        if let Some(i) = idx {
            let parent = &path[..i];
            let name = &path[i + 1..];
            let mut dirs = self.dirs.lock().unwrap();
            if let Some(list) = dirs.get_mut(parent) {
                list.retain(|n| n != name);
            }
        }
        self.dirs.lock().unwrap().remove(path);
        Ok(())
    }
}

/// Normalize a path: ISO timestamp → `<TS>`, keeping the id.
fn norm_path(path: &str) -> String {
    // /sessions/--workspace-project--/2026-08-21T14-26-02-771Z_metadata.jsonl
    let mut parts: Vec<String> = path.split('/').map(str::to_string).collect();
    if let Some(last) = parts.last_mut() {
        if let Some(idx) = last.find("_") {
            let id_part = last[idx + 1..].to_string();
            *last = format!("<TS>_{id_part}");
        }
    }
    parts.join("/")
}

fn user_message(text: &str) -> pirust_agent_core::harness::messages::AgentMessage {
    serde_json::from_value(serde_json::json!({
        "role": "user",
        "content": [{ "type": "text", "text": text }],
        "timestamp": 1,
    }))
    .unwrap()
}

fn item_seq(item: &pirust_agent_core::harness::session::v4::types::LogItem) -> i64 {
    use pirust_agent_core::harness::session::v4::types::LogItem::*;
    match item {
        Entry { seq, .. } => *seq,
        Record { seq, .. } => *seq,
        Lane { seq, .. } => *seq,
        FactName { seq, .. } => *seq,
        FactLabel { seq, .. } => *seq,
    }
}

#[test]
fn create_exposes_complete_metadata_contract() {
    let fs = MockFs::new();
    let repo = JsonlSessionRepo::new(fs.clone(), "/sessions".to_string());
    let options = JsonlSessionCreateOptions {
        id: Some("metadata".to_string()),
        cwd: "/workspace/project".to_string(),
        parent_session_id: Some("parent".to_string()),
        metadata: Some(
            serde_json::from_str(r#"{"owner":"agent","nested":{"enabled":true}}"#).unwrap(),
        ),
    };
    let session = repo.create(&options).unwrap();
    let metadata = session.get_metadata().unwrap();
    assert_eq!(metadata.id, "metadata");
    assert_eq!(metadata.cwd, "/workspace/project");
    assert_eq!(metadata.source_format, 4);
    assert_eq!(metadata.parent_session_id.as_deref(), Some("parent"));
    assert_eq!(
        metadata.metadata.as_ref().unwrap().get("owner").unwrap(),
        "agent"
    );
    assert!(metadata.path.ends_with("_metadata.jsonl"));
    assert!(norm_path(&metadata.path).contains("--workspace-project--"));
    // List by cwd returns it.
    let listed = repo
        .list(&JsonlSessionListOptions {
            cwd: Some("/workspace/project".to_string()),
        })
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, "metadata");
    assert_eq!(listed[0].path, metadata.path);
    // Other cwd → empty.
    let other = repo
        .list(&JsonlSessionListOptions {
            cwd: Some("/workspace/other".to_string()),
        })
        .unwrap();
    assert!(other.is_empty());
}

#[test]
fn rejects_invalid_session_id() {
    let fs = MockFs::new();
    let repo = JsonlSessionRepo::new(fs.clone(), "/sessions".to_string());
    let err = repo
        .create(&JsonlSessionCreateOptions {
            id: Some("../escape".to_string()),
            cwd: "/workspace/project".to_string(),
            ..Default::default()
        })
        .unwrap_err();
    assert_eq!(
        err.code,
        pirust_agent_core::harness::types::SessionErrorCode::InvalidPayload
    );
    assert_eq!(
        err.message,
        "Session id must be non-empty, contain only alphanumeric characters, '-', '_', and '.', and start and end with an alphanumeric character"
    );
}

#[test]
fn rejects_duplicate_id_same_cwd() {
    let fs = MockFs::new();
    let repo = JsonlSessionRepo::new(fs.clone(), "/sessions".to_string());
    let options = JsonlSessionCreateOptions {
        id: Some("dup".to_string()),
        cwd: "/workspace/project".to_string(),
        ..Default::default()
    };
    repo.create(&options).unwrap();
    let err = repo.create(&options).unwrap_err();
    assert_eq!(
        err.code,
        pirust_agent_core::harness::types::SessionErrorCode::AlreadyExists
    );
    assert_eq!(err.message, "Session already exists: dup");
}

#[test]
fn allows_same_id_in_different_cwds() {
    let fs = MockFs::new();
    let repo = JsonlSessionRepo::new(fs.clone(), "/sessions".to_string());
    let first = repo
        .create(&JsonlSessionCreateOptions {
            id: Some("shared".to_string()),
            cwd: "/workspaces/first".to_string(),
            ..Default::default()
        })
        .unwrap();
    let second = repo
        .create(&JsonlSessionCreateOptions {
            id: Some("shared".to_string()),
            cwd: "/workspaces/second".to_string(),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(first.get_metadata().unwrap().cwd, "/workspaces/first");
    assert_eq!(second.get_metadata().unwrap().cwd, "/workspaces/second");
    let listed = repo.list(&JsonlSessionListOptions::default()).unwrap();
    assert_eq!(
        listed.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
        vec!["shared", "shared"]
    );
}

#[test]
fn create_append_reopen_round_trips() {
    let fs = MockFs::new();
    let repo = JsonlSessionRepo::new(fs.clone(), "/sessions".to_string());
    let session = repo
        .create(&JsonlSessionCreateOptions {
            id: Some("session".to_string()),
            cwd: "/workspace/project".to_string(),
            ..Default::default()
        })
        .unwrap();
    let metadata = session.get_metadata().unwrap();
    let entry_id = session
        .append_custom_entry("note", Some(serde_json::json!({"value": 1})))
        .unwrap();
    session.create_lane("thread", Some(&entry_id)).unwrap();
    session
        .append_record(&NewRecord::OperationStarted(NewOperationStartedRecord {
            id: "run".to_string(),
            lane: "thread".to_string(),
            source_leaf_id: None,
            intent: OperationIntent::Run {
                original_prompt: vec![],
                initial_messages: vec![],
                system_prompt_override: None,
                resume_data: None,
            },
        }))
        .unwrap();
    session.set_name(Some("Example")).unwrap();
    session.set_label(&entry_id, Some("checkpoint")).unwrap();
    session.move_lane("main", None).unwrap();

    let reopened = repo.open(&metadata).unwrap();
    assert_eq!(
        reopened.get_lanes().unwrap(),
        vec![
            LanePointer {
                lane: "main".to_string(),
                leaf_id: None
            },
            LanePointer {
                lane: "thread".to_string(),
                leaf_id: Some(entry_id.clone())
            },
        ]
    );
    assert_eq!(reopened.get_name().unwrap().as_deref(), Some("Example"));
    assert_eq!(
        reopened.get_label(&entry_id).unwrap().as_deref(),
        Some("checkpoint")
    );
    let records = reopened.find_records(&RecordQuery::default()).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].id(), "run");
    let log = reopened.get_log(&LogOptions::default()).unwrap();
    assert_eq!(
        log.iter().map(item_seq).collect::<Vec<_>>(),
        vec![1, 2, 3, 4, 5, 6]
    );
}

#[test]
fn list_skips_malformed_header_files() {
    let fs = MockFs::new();
    let repo = JsonlSessionRepo::new(fs.clone(), "/sessions".to_string());
    repo.create(&JsonlSessionCreateOptions {
        id: Some("valid".to_string()),
        cwd: "/workspaces/a".to_string(),
        ..Default::default()
    })
    .unwrap();
    let malformed = repo
        .create(&JsonlSessionCreateOptions {
            id: Some("malformed".to_string()),
            cwd: "/workspaces/a".to_string(),
            ..Default::default()
        })
        .unwrap();
    let mpath = malformed.get_metadata().unwrap().path;
    // Corrupt the malformed session's header directly.
    fs.write_file(&mpath, "not json\n").unwrap();
    let listed = repo.list(&JsonlSessionListOptions::default()).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, "valid");
}

#[test]
fn fork_tree_sets_parent_and_keeps_entries() {
    let fs = MockFs::new();
    let repo = JsonlSessionRepo::new(fs.clone(), "/sessions".to_string());
    let source = repo
        .create(&JsonlSessionCreateOptions {
            id: Some("source".to_string()),
            cwd: "/workspace/project".to_string(),
            ..Default::default()
        })
        .unwrap();
    source.append_message(user_message("one")).unwrap();
    source.append_message(user_message("two")).unwrap();
    let source_meta = source.get_metadata().unwrap();
    let fork = repo
        .fork(
            &source_meta,
            &JsonlSessionCreateOptions {
                id: Some("fork".to_string()),
                cwd: "/workspace/project".to_string(),
                ..Default::default()
            },
            &ForkOptions::Tree,
        )
        .unwrap();
    let meta = fork.get_metadata().unwrap();
    assert_eq!(meta.id, "fork");
    assert_eq!(meta.parent_session_id.as_deref(), Some("source"));
    assert_eq!(meta.cwd, "/workspace/project");
    assert_eq!(fork.get_stats().unwrap().message_count, 2);
}

#[test]
fn open_missing_file_is_not_found() {
    let fs = MockFs::new();
    let repo = JsonlSessionRepo::new(fs.clone(), "/sessions".to_string());
    let metadata = JsonlSessionMetadata {
        id: "gone".to_string(),
        created_at: 1,
        cwd: "/workspace/project".to_string(),
        path: "/sessions/--workspace-project--/none.jsonl".to_string(),
        modified_at: 1,
        source_format: 4,
        parent_session_id: None,
        legacy_parent_session_path: None,
        metadata: None,
    };
    let err = repo.open(&metadata).unwrap_err();
    assert_eq!(
        err.code,
        pirust_agent_core::harness::types::SessionErrorCode::NotFound
    );
    assert_eq!(err.message, "Session not found: gone");
}

#[test]
fn open_header_id_mismatch_is_invalid_entry() {
    let fs = MockFs::new();
    let repo = JsonlSessionRepo::new(fs.clone(), "/sessions".to_string());
    let session = repo
        .create(&JsonlSessionCreateOptions {
            id: Some("alpha".to_string()),
            cwd: "/workspace/project".to_string(),
            ..Default::default()
        })
        .unwrap();
    let metadata = session.get_metadata().unwrap();
    // Rewrite the header id to beta.
    let bytes = fs.file(&metadata.path).unwrap();
    let mut lines: Vec<String> = bytes.split('\n').map(str::to_string).collect();
    let mut header: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
    header["id"] = serde_json::json!("beta");
    lines[0] = header.to_string();
    fs.write_file(&metadata.path, &(lines.join("\n") + "\n"))
        .unwrap();
    let err = repo.open(&metadata).unwrap_err();
    assert_eq!(
        err.code,
        pirust_agent_core::harness::types::SessionErrorCode::InvalidEntry
    );
    assert_eq!(err.message, "Session id does not match header: alpha");
}

#[test]
fn delete_removes_the_session_file() {
    let fs = MockFs::new();
    let repo = JsonlSessionRepo::new(fs.clone(), "/sessions".to_string());
    let session = repo
        .create(&JsonlSessionCreateOptions {
            id: Some("doomed".to_string()),
            cwd: "/workspace/project".to_string(),
            ..Default::default()
        })
        .unwrap();
    let metadata = session.get_metadata().unwrap();
    repo.delete(&metadata).unwrap();
    let listed = repo.list(&JsonlSessionListOptions::default()).unwrap();
    assert!(listed.is_empty());
    assert!(fs.file(&metadata.path).is_none());
}
