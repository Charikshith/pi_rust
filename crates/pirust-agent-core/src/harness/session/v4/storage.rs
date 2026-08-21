//! v4 JSONL storage — port of `packages/agent/src/harness/session/jsonl/storage.ts`.
//!
//! [`JsonlSessionStorage`] is the per-session write/read facade over a single
//! v4 JSONL file: `create` writes the header, every write op appends one
//! mutation line and then applies it to the in-memory [`SessionState`], and
//! `load` replays the file (repairing a torn tail and an unterminated final
//! line exactly the way Pi does). Mutating operations run one-at-a-time under
//! an internal lock, mirroring TS `tail`-enqueue semantics.

use std::sync::{Arc, Mutex, MutexGuard};

use super::codec::{
    encode_header, encode_mutation, invalid_file, metadata_from_header, parse_header,
    parse_mutation, JsonlDecodeError, JsonlV4Header, V4FileSystem,
};
use super::state::SessionState;
use super::types::{
    BranchBounds, Entry, EntryQuery, ForkOptions, JsonlSessionMetadata, LanePointer, LaneRecord,
    LogItem, LogOptions, NewRecord, OperationStartedRecord, ProvisionedEntry, RecordQuery,
    SessionMutation, SessionStats,
};
use crate::harness::types::{SessionError, SessionErrorCode};

/// `SessionError` with the `storage` code.
fn storage_error(message: impl Into<String>) -> SessionError {
    SessionError::new(SessionErrorCode::Storage, message)
}

/// `publishFileAtomically` (storage.ts:16-35) — build a complete sibling temp
/// file then atomically rename it over the destination. On failure the temp
/// file is removed best-effort and the original error is preserved.
fn publish_file_atomically<F>(
    fs: &dyn V4FileSystem,
    destination_path: &str,
    populate: F,
) -> Result<(), SessionError>
where
    F: FnOnce(&str) -> Result<(), SessionError>,
{
    let temp_path = format!("{destination_path}.tmp");
    let result = (|| {
        populate(&temp_path)?;
        fs.rename_file(&temp_path, destination_path)
            .map_err(|_| storage_error(format!("Failed to publish staged file {destination_path}")))
    })();
    if result.is_err() {
        // Best-effort cleanup; preserve the original error.
        let _ = fs.remove(&temp_path, true);
    }
    result
}

/// `JsonlSessionStorage` (storage.ts:37-277).
pub struct JsonlSessionStorage {
    fs: Arc<dyn V4FileSystem>,
    metadata: JsonlSessionMetadata,
    state: Mutex<SessionState>,
    /// Write serialization (TS `tail`-enqueue): every mutating op appends its
    /// mutation line before applying it to state, and concurrent ops must be
    /// ordered.
    write_lock: Mutex<()>,
}

impl std::fmt::Debug for JsonlSessionStorage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JsonlSessionStorage")
            .field("metadata", &self.metadata)
            .finish_non_exhaustive()
    }
}

impl JsonlSessionStorage {
    /// `create` (storage.ts:46-50) — write the header line and return a storage
    /// bound to the new file.
    pub fn create(
        fs: Arc<dyn V4FileSystem>,
        path: &str,
        header: &JsonlV4Header,
    ) -> Result<Self, SessionError> {
        fs.write_file(path, &encode_header(header))
            .map_err(|_| storage_error(format!("Failed to initialize session {path}")))?;
        let file_info = fs
            .file_info(path)
            .map_err(|_| storage_error(format!("Failed to read session metadata {path}")))?;
        Ok(Self::new(
            fs,
            metadata_from_header(header, path.to_string(), file_info.mtime_ms),
        ))
    }

    /// `load` (storage.ts:52-91) — parse the file, replay mutations into a
    /// fresh state; repair a torn tail (syntax error on the final line) by
    /// atomically publishing the valid prefix, and repair an unterminated final
    /// line by appending a newline.
    pub fn load(fs: Arc<dyn V4FileSystem>, path: &str) -> Result<Self, SessionError> {
        let content = fs
            .read_text_file(path)
            .map_err(|_| storage_error(format!("Failed to read session {path}")))?;
        let mut physical_lines: Vec<&str> = content.split('\n').collect();
        if physical_lines.last() == Some(&"") {
            physical_lines.pop();
        }
        if physical_lines.is_empty() || physical_lines[0].is_empty() {
            return Err(invalid_file(
                path,
                1,
                &JsonlDecodeError {
                    kind: "schema".to_string(),
                    message: "is missing a header".to_string(),
                },
            ));
        }
        let header_result = parse_header(physical_lines[0]);
        let header = match header_result {
            Ok(h) => h,
            Err(e) => return Err(invalid_file(path, 1, &e)),
        };
        let file_info = fs
            .file_info(path)
            .map_err(|_| storage_error(format!("Failed to read session metadata {path}")))?;
        let storage = Self::new(
            fs,
            metadata_from_header(&header, path.to_string(), file_info.mtime_ms),
        );

        for (index, line) in physical_lines.iter().enumerate().skip(1) {
            let mutation_result = parse_mutation(line);
            let mutation = match mutation_result {
                Ok(m) => m,
                Err(e) => {
                    let is_torn_tail = index == physical_lines.len() - 1 && e.kind == "syntax";
                    if is_torn_tail {
                        // Drop the unacknowledged partial append by atomically
                        // publishing the valid prefix.
                        let valid_prefix = format!("{}\n", physical_lines[..index].join("\n"));
                        publish_file_atomically(storage.fs.as_ref(), path, |temp_path| {
                            storage
                                .fs
                                .write_file(temp_path, &valid_prefix)
                                .map_err(|_| {
                                    storage_error(format!(
                                        "Failed to stage torn-tail repair {path}"
                                    ))
                                })
                        })?;
                        return Ok(storage);
                    }
                    return Err(invalid_file(path, index + 1, &e));
                }
            };
            let mut state = storage.state.lock().unwrap();
            if let Err(error) = state.apply_mutation(&mutation) {
                if error.code == SessionErrorCode::InvalidEntry {
                    return Err(invalid_file(
                        path,
                        index + 1,
                        &JsonlDecodeError {
                            kind: "schema".to_string(),
                            message: error.message,
                        },
                    ));
                }
                return Err(error);
            }
        }
        if !content.ends_with('\n') {
            storage.fs.append_file(path, "\n").map_err(|_| {
                storage_error(format!("Failed to repair unterminated session tail {path}"))
            })?;
        }
        Ok(storage)
    }

    /// `fork` (storage.ts:93-101) — atomically publish a new file carrying the
    /// fork mutations (copied entries renumbered from seq 1, lanes, facts).
    pub fn fork(
        &self,
        path: &str,
        header: &JsonlV4Header,
        options: &ForkOptions,
    ) -> Result<Self, SessionError> {
        let mutations = self.state_lock().create_fork_mutations(options)?;
        publish_file_atomically(self.fs.as_ref(), path, |temp_path| {
            let target = Self::create(self.fs.clone(), temp_path, header)?;
            for mutation in &mutations {
                target.append_mutation(mutation)?;
                let _ = target.state_mut().apply_mutation(mutation);
            }
            Ok(())
        })?;
        Self::load(self.fs.clone(), path)
    }

    /// `drain` (storage.ts:103-105) — wait for queued writes to settle. Writes
    /// are synchronous under the write lock, so nothing to await.
    pub fn drain(&self) -> Result<(), SessionError> {
        Ok(())
    }

    /// `getMetadata` (storage.ts:107-109).
    pub fn get_metadata(&self) -> Result<JsonlSessionMetadata, SessionError> {
        Ok(self.metadata.clone())
    }

    /// `getLanes` (storage.ts:111-113).
    pub fn get_lanes(&self) -> Result<Vec<LanePointer>, SessionError> {
        Ok(self.state_lock().get_lanes())
    }

    /// `createLane` (storage.ts:115-120).
    pub fn create_lane(&self, lane: &str, at: Option<&str>) -> Result<(), SessionError> {
        self.enqueue(move |storage| {
            let state = storage.state_mut();
            state.validate_new_lane(lane)?;
            state.validate_target(at)?;
            let mutation = SessionMutation::Lane {
                seq: state.next_sequence(),
                lane: lane.to_string(),
                leaf_id: at.map(str::to_string),
            };
            drop(state);
            storage.append_mutation(&mutation)?;
            storage.state_mut().apply_mutation(&mutation)
        })
    }

    /// `moveLane` (storage.ts:122-127).
    pub fn move_lane(&self, lane: &str, to: Option<&str>) -> Result<(), SessionError> {
        self.enqueue(move |storage| {
            let state = storage.state_mut();
            state.require_lane(lane)?;
            state.validate_target(to)?;
            let mutation = SessionMutation::Lane {
                seq: state.next_sequence(),
                lane: lane.to_string(),
                leaf_id: to.map(str::to_string),
            };
            drop(state);
            storage.append_mutation(&mutation)?;
            storage.state_mut().apply_mutation(&mutation)
        })
    }

    /// `appendEntry` (storage.ts:129-140) — assign parentId/seq/timestamp,
    /// append the mutation, apply it, return the committed entry.
    pub fn append_entry(
        &self,
        new_entry: &ProvisionedEntry,
        lane: &str,
    ) -> Result<Entry, SessionError> {
        let provisioned = new_entry.clone();
        self.enqueue(move |storage| {
            let state = storage.state_mut();
            let parent_id = state.require_lane(lane)?;
            state.validate_unused_id(provisioned.id())?;
            let entry =
                provisioned
                    .clone()
                    .promote(parent_id.clone(), state.next_sequence(), now_ms());
            let mutation = SessionMutation::Entry {
                lane: Some(lane.to_string()),
                entry: entry.clone(),
            };
            drop(state);
            storage.append_mutation(&mutation)?;
            storage.state_mut().apply_mutation(&mutation)?;
            Ok(entry)
        })
    }

    /// `appendRecord` (storage.ts:142-160) — reject a second open operation on
    /// a lane, assign seq/timestamp, append + apply.
    pub fn append_record(&self, new_record: &NewRecord) -> Result<LaneRecord, SessionError> {
        let record_clone = new_record.clone();
        self.enqueue(move |storage| {
            let state = storage.state_mut();
            state.require_lane(record_clone.lane())?;
            state.validate_unused_id(record_clone.id())?;
            let current_open = state
                .find_open_operations(record_clone.lane(), Some(1))?
                .pop();
            if record_clone.record_type() == "operation_started" && current_open.is_some() {
                let current_id = current_open.map(|r| r.id).unwrap_or_default();
                return Err(SessionError::new(
                    SessionErrorCode::Storage,
                    format!(
                        "Lane {} already has an open operation {current_id}",
                        record_clone.lane()
                    ),
                ));
            }
            let record = record_clone.promote(state.next_sequence(), now_ms());
            let mutation = SessionMutation::Record {
                record: record.clone(),
            };
            drop(state);
            storage.append_mutation(&mutation)?;
            storage.state_mut().apply_mutation(&mutation)?;
            Ok(record)
        })
    }

    /// `getEntry` (storage.ts:162-165).
    pub fn get_entry(&self, id: &str) -> Result<Option<Entry>, SessionError> {
        Ok(self.state_lock().get_entry(id).cloned())
    }

    /// `findEntries` (storage.ts:167-169).
    pub fn find_entries(&self, query: &EntryQuery) -> Result<Vec<Entry>, SessionError> {
        self.state_lock().find_entries(query)
    }

    /// `findEntriesOnBranch` (storage.ts:171-173).
    pub fn find_entries_on_branch(
        &self,
        query: &EntryQuery,
        bounds: &BranchBounds,
        start: &str,
    ) -> Result<Vec<Entry>, SessionError> {
        self.state_lock()
            .find_entries_on_branch(query, bounds, start)
    }

    /// `findRecords` (storage.ts:175-184).
    pub fn find_records(&self, query: &RecordQuery) -> Result<Vec<LaneRecord>, SessionError> {
        self.state_lock().find_records(query)
    }

    /// `findOpenOperations` (storage.ts:186-188) — unfinished operation starts,
    /// newest first.
    pub fn find_open_operations(
        &self,
        lane: &str,
        options: Option<usize>,
    ) -> Result<Vec<OperationStartedRecord>, SessionError> {
        self.state_lock().find_open_operations(lane, options)
    }

    /// `getLog` (storage.ts:190-192).
    pub fn get_log(&self, options: &LogOptions) -> Result<Vec<LogItem>, SessionError> {
        self.state_lock().get_log(options)
    }

    /// `getName` (storage.ts:194-196).
    pub fn get_name(&self) -> Result<Option<String>, SessionError> {
        Ok(self.state_lock().get_name())
    }

    /// `setName` (storage.ts:198-203).
    pub fn set_name(&self, name: Option<&str>) -> Result<(), SessionError> {
        let name = name.map(str::to_string);
        self.enqueue(move |storage| {
            let mutation = SessionMutation::FactName {
                seq: storage.state_lock().next_sequence(),
                name: name.clone(),
            };
            storage.append_mutation(&mutation)?;
            storage.state_mut().apply_mutation(&mutation)
        })
    }

    /// `getLabel` (storage.ts:205-207).
    pub fn get_label(&self, id: &str) -> Result<Option<String>, SessionError> {
        Ok(self.state_lock().get_label(id))
    }

    /// `setLabel` (storage.ts:209-221).
    pub fn set_label(&self, id: &str, label: Option<&str>) -> Result<(), SessionError> {
        let label = label.map(str::to_string);
        self.enqueue(move |storage| {
            let state = storage.state_mut();
            state.validate_target(Some(id))?;
            let mutation = SessionMutation::FactLabel {
                seq: state.next_sequence(),
                target_id: id.to_string(),
                label: label.clone(),
            };
            drop(state);
            storage.append_mutation(&mutation)?;
            storage.state_mut().apply_mutation(&mutation)
        })
    }

    /// `getStats` (storage.ts:223-225).
    pub fn get_stats(&self) -> Result<SessionStats, SessionError> {
        Ok(self.state_lock().get_stats())
    }

    /// Internal constructor (mirrors the TS private constructor).
    fn new(fs: Arc<dyn V4FileSystem>, metadata: JsonlSessionMetadata) -> Self {
        Self {
            fs,
            metadata,
            state: Mutex::new(SessionState::new()),
            write_lock: Mutex::new(()),
        }
    }

    fn state_lock(&self) -> MutexGuard<'_, SessionState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn state_mut(&self) -> MutexGuard<'_, SessionState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// `enqueue` (storage.ts:227-232) — serialize mutating operations. The TS
    /// version chains on a `tail` promise; the Rust version takes the write
    /// lock so concurrent callers run one-at-a-time in acquisition order.
    fn enqueue<T>(
        &self,
        operation: impl FnOnce(&Self) -> Result<T, SessionError>,
    ) -> Result<T, SessionError> {
        let _guard = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        operation(self)
    }

    /// `appendMutation` (storage.ts:234-237) — append the encoded line to the
    /// session file.
    fn append_mutation(&self, mutation: &SessionMutation) -> Result<(), SessionError> {
        self.fs
            .append_file(&self.metadata.path, &encode_mutation(mutation))
            .map_err(|_| storage_error(format!("Failed to append session {}", self.metadata.path)))
    }
}

/// Unix-ms "now" — `Date.now()` (storage.ts assigns `timestamp: Date.now()`).
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// `SessionStorage` impl (session/types.ts:290-327)
// ---------------------------------------------------------------------------

impl super::types::SessionStorage for JsonlSessionStorage {
    type Metadata = JsonlSessionMetadata;

    fn get_metadata(&self) -> Result<Self::Metadata, SessionError> {
        self.get_metadata()
    }

    fn get_lanes(&self) -> Result<Vec<LanePointer>, SessionError> {
        self.get_lanes()
    }

    fn create_lane(&self, lane: &str, at: Option<&str>) -> Result<(), SessionError> {
        self.create_lane(lane, at)
    }

    fn move_lane(&self, lane: &str, to: Option<&str>) -> Result<(), SessionError> {
        self.move_lane(lane, to)
    }

    fn append_entry(&self, entry: &ProvisionedEntry, lane: &str) -> Result<Entry, SessionError> {
        self.append_entry(entry, lane)
    }

    fn append_record(&self, record: &NewRecord) -> Result<LaneRecord, SessionError> {
        self.append_record(record)
    }

    fn get_entry(&self, id: &str) -> Result<Option<Entry>, SessionError> {
        self.get_entry(id)
    }

    fn find_entries(&self, query: &EntryQuery) -> Result<Vec<Entry>, SessionError> {
        self.find_entries(query)
    }

    fn find_entries_on_branch(
        &self,
        query: &EntryQuery,
        bounds: &BranchBounds,
        start: &str,
    ) -> Result<Vec<Entry>, SessionError> {
        self.find_entries_on_branch(query, bounds, start)
    }

    fn find_records(&self, query: &RecordQuery) -> Result<Vec<LaneRecord>, SessionError> {
        self.find_records(query)
    }

    fn find_open_operations(
        &self,
        lane: &str,
        options: Option<usize>,
    ) -> Result<Vec<OperationStartedRecord>, SessionError> {
        self.find_open_operations(lane, options)
    }

    fn get_log(&self, options: &LogOptions) -> Result<Vec<LogItem>, SessionError> {
        self.get_log(options)
    }

    fn get_name(&self) -> Result<Option<String>, SessionError> {
        self.get_name()
    }

    fn set_name(&self, name: Option<&str>) -> Result<(), SessionError> {
        self.set_name(name)
    }

    fn get_label(&self, id: &str) -> Result<Option<String>, SessionError> {
        self.get_label(id)
    }

    fn set_label(&self, id: &str, label: Option<&str>) -> Result<(), SessionError> {
        self.set_label(id, label)
    }

    fn get_stats(&self) -> Result<SessionStats, SessionError> {
        self.get_stats()
    }
}
