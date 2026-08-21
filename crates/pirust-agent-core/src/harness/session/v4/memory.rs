//! v4 in-memory session storage/repo — port of
//! `packages/agent/src/harness/session/memory.ts`.
//!
//! [`InMemorySessionStorage`] is the reference [`SessionStorage`] backend used
//! by tests and by the harness when no durable backend is needed: it replays
//! mutations into a [`SessionState`] exactly like the JSONL backend but
//! persists nothing. [`InMemorySessionRepo`] is the matching in-memory
//! [`SessionRepo`].

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use super::session::Session;
use super::state::SessionState;
use super::types::{
    BranchBounds, Entry, EntryQuery, ForkOptions, LanePointer, LaneRecord, LogItem, LogOptions,
    NewRecord, OperationStartedRecord, ProvisionedEntry, RecordQuery, SessionCreateOptions,
    SessionMetadata, SessionRepo, SessionStats, SessionStorage,
};
use crate::harness::session::uuid::uuidv7;
use crate::harness::types::{SessionError, SessionErrorCode};

/// `InMemorySessionStorage` (memory.ts:16-99).
pub struct InMemorySessionStorage {
    metadata: SessionMetadata,
    state: Mutex<SessionState>,
}

impl InMemorySessionStorage {
    /// `constructor` (memory.ts:19-22) — `structuredClone(metadata)`.
    pub fn new(metadata: SessionMetadata) -> Self {
        Self {
            metadata,
            state: Mutex::new(SessionState::new()),
        }
    }

    /// `fork` (memory.ts:24-28).
    pub fn fork(
        &self,
        metadata: SessionMetadata,
        options: &ForkOptions,
    ) -> Result<InMemorySessionStorage, SessionError> {
        let storage = InMemorySessionStorage::new(metadata);
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        for mutation in state.create_fork_mutations(options)? {
            storage
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .apply_mutation(&mutation)?;
        }
        Ok(storage)
    }
}

impl SessionStorage for InMemorySessionStorage {
    type Metadata = SessionMetadata;

    /// `getMetadata` (memory.ts:30-32) — `structuredClone`.
    fn get_metadata(&self) -> Result<Self::Metadata, SessionError> {
        Ok(self.metadata.clone())
    }

    /// `getLanes` (memory.ts:34-36).
    fn get_lanes(&self) -> Result<Vec<LanePointer>, SessionError> {
        Ok(self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_lanes())
    }

    /// `createLane` (memory.ts:38-43).
    fn create_lane(&self, lane: &str, at: Option<&str>) -> Result<(), SessionError> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.validate_new_lane(lane)?;
        state.validate_target(at)?;
        let mutation = super::types::SessionMutation::Lane {
            seq: state.next_sequence(),
            lane: lane.to_string(),
            leaf_id: at.map(str::to_string),
        };
        state.apply_mutation(&mutation)
    }

    /// `moveLane` (memory.ts:45-50).
    fn move_lane(&self, lane: &str, to: Option<&str>) -> Result<(), SessionError> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.require_lane(lane)?;
        state.validate_target(to)?;
        let mutation = super::types::SessionMutation::Lane {
            seq: state.next_sequence(),
            lane: lane.to_string(),
            leaf_id: to.map(str::to_string),
        };
        state.apply_mutation(&mutation)
    }

    /// `appendEntry` (memory.ts:52-61) — assign parentId/seq/timestamp, apply.
    fn append_entry(&self, entry: &ProvisionedEntry, lane: &str) -> Result<Entry, SessionError> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let parent_id = state.require_lane(lane)?;
        state.validate_unused_id(entry.id())?;
        let promoted = entry.clone().promote(
            parent_id,
            state.next_sequence(),
            crate::harness::session::v4::repo::now_ms(),
        );
        state.apply_mutation(&super::types::SessionMutation::Entry {
            lane: Some(lane.to_string()),
            entry: promoted.clone(),
        })?;
        Ok(promoted)
    }

    /// `appendRecord` (memory.ts:63-78).
    fn append_record(&self, record: &NewRecord) -> Result<LaneRecord, SessionError> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.require_lane(record.lane())?;
        state.validate_unused_id(record.id())?;
        let current_open_operation_id = state
            .find_open_operations(record.lane(), Some(1))?
            .first()
            .map(|op| op.id.clone());
        if record.record_type() == "operation_started" && current_open_operation_id.is_some() {
            let current_id = current_open_operation_id.unwrap_or_default();
            return Err(SessionError::new(
                SessionErrorCode::Storage,
                format!(
                    "Lane {} already has an open operation {current_id}",
                    record.lane()
                ),
            ));
        }
        let promoted = record.clone().promote(
            state.next_sequence(),
            crate::harness::session::v4::repo::now_ms(),
        );
        state.apply_mutation(&super::types::SessionMutation::Record {
            record: promoted.clone(),
        })?;
        Ok(promoted)
    }

    /// `getEntry` (memory.ts:80-83).
    fn get_entry(&self, id: &str) -> Result<Option<Entry>, SessionError> {
        Ok(self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_entry(id)
            .cloned())
    }

    /// `findEntries` (memory.ts:85-87).
    fn find_entries(&self, query: &EntryQuery) -> Result<Vec<Entry>, SessionError> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .find_entries(query)
    }

    /// `findEntriesOnBranch` (memory.ts:89-91).
    fn find_entries_on_branch(
        &self,
        query: &EntryQuery,
        bounds: &BranchBounds,
        start: &str,
    ) -> Result<Vec<Entry>, SessionError> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .find_entries_on_branch(query, bounds, start)
    }

    /// `findRecords` (memory.ts:93-99).
    fn find_records(&self, query: &RecordQuery) -> Result<Vec<LaneRecord>, SessionError> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .find_records(query)
    }

    /// `findOpenOperations` (memory.ts:101-103).
    fn find_open_operations(
        &self,
        lane: &str,
        options: Option<usize>,
    ) -> Result<Vec<OperationStartedRecord>, SessionError> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .find_open_operations(lane, options)
    }

    /// `getLog` (memory.ts:105-107).
    fn get_log(&self, options: &LogOptions) -> Result<Vec<LogItem>, SessionError> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_log(options)
    }

    /// `getName` (memory.ts:109-111).
    fn get_name(&self) -> Result<Option<String>, SessionError> {
        Ok(self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_name())
    }

    /// `setName` (memory.ts:113-115).
    fn set_name(&self, name: Option<&str>) -> Result<(), SessionError> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let mutation = super::types::SessionMutation::FactName {
            seq: state.next_sequence(),
            name: name.map(str::to_string),
        };
        state.apply_mutation(&mutation)
    }

    /// `getLabel` (memory.ts:117-119).
    fn get_label(&self, id: &str) -> Result<Option<String>, SessionError> {
        Ok(self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_label(id))
    }

    /// `setLabel` (memory.ts:121-128).
    fn set_label(&self, id: &str, label: Option<&str>) -> Result<(), SessionError> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.validate_target(Some(id))?;
        let mutation = super::types::SessionMutation::FactLabel {
            seq: state.next_sequence(),
            target_id: id.to_string(),
            label: label.map(str::to_string),
        };
        state.apply_mutation(&mutation)
    }

    /// `getStats` (memory.ts:130-132).
    fn get_stats(&self) -> Result<SessionStats, SessionError> {
        Ok(self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_stats())
    }
}

/// `InMemorySessionRepo` (memory.ts:134-168).
///
/// Holds `Arc<InMemorySessionStorage>` so `Session` handles and the map entry
/// share ONE state object (JS objects are by-reference; `InMemorySessionStorage`
/// deep-clones on `Clone`).
pub struct InMemorySessionRepo {
    sessions: Mutex<HashMap<String, Arc<InMemorySessionStorage>>>,
}

impl InMemorySessionRepo {
    /// `constructor` (memory.ts:135).
    pub fn new() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
        }
    }
}

impl Default for InMemorySessionRepo {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionRepo for InMemorySessionRepo {
    type Session = Session<InMemorySessionStorage>;
    type Metadata = SessionMetadata;
    type CreateOptions = SessionCreateOptions;
    type ListOptions = ();
    type ForkOptions = super::types::MemoryForkOptions;

    /// `create` (memory.ts:138-146).
    fn create(&self, options: Self::CreateOptions) -> Result<Self::Session, SessionError> {
        let id = options.id.clone().unwrap_or_else(uuidv7);
        let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        if sessions.contains_key(&id) {
            return Err(SessionError::new(
                SessionErrorCode::AlreadyExists,
                format!("Session already exists: {id}"),
            ));
        }
        let storage = Arc::new(InMemorySessionStorage::new(SessionMetadata {
            id: id.clone(),
            created_at: crate::harness::session::v4::repo::now_ms(),
            parent_session_id: options.parent_session_id,
        }));
        sessions.insert(id.clone(), storage.clone());
        Ok(Session::new(storage))
    }

    /// `open` (memory.ts:148-150).
    fn open(&self, metadata: Self::Metadata) -> Result<Self::Session, SessionError> {
        Ok(Session::new(self.require_storage(&metadata.id)?))
    }

    /// `list` (memory.ts:152-154).
    fn list(
        &self,
        _options: Option<Self::ListOptions>,
    ) -> Result<Vec<Self::Metadata>, SessionError> {
        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .map(|storage| storage.get_metadata())
            .collect()
    }

    /// `delete` (memory.ts:156-158).
    fn delete(&self, metadata: Self::Metadata) -> Result<(), SessionError> {
        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&metadata.id);
        Ok(())
    }

    /// `fork` (memory.ts:160-166).
    fn fork(
        &self,
        source: Self::Metadata,
        options: Self::ForkOptions,
    ) -> Result<Self::Session, SessionError> {
        let source_storage = self.require_storage(&source.id)?;
        let id = options.id.clone().unwrap_or_else(uuidv7);
        let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        if sessions.contains_key(&id) {
            return Err(SessionError::new(
                SessionErrorCode::AlreadyExists,
                format!("Session already exists: {id}"),
            ));
        }
        let storage = Arc::new(source_storage.fork(
            SessionMetadata {
                id: id.clone(),
                created_at: crate::harness::session::v4::repo::now_ms(),
                parent_session_id: options.parent_session_id.or(Some(source.id.clone())),
            },
            options.fork.as_ref().unwrap_or(&ForkOptions::Tree),
        )?);
        sessions.insert(id.clone(), storage.clone());
        Ok(Session::new(storage))
    }
}

impl InMemorySessionRepo {
    /// `requireStorage` (memory.ts:168-171).
    fn require_storage(&self, id: &str) -> Result<Arc<InMemorySessionStorage>, SessionError> {
        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .cloned()
            .ok_or_else(|| {
                SessionError::new(
                    SessionErrorCode::NotFound,
                    format!("Session not found: {id}"),
                )
            })
    }
}
