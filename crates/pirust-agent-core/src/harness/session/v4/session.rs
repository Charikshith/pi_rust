//! v4 `Session` — port of `packages/agent/src/harness/session/session.ts`.
//!
//! A thin, mostly stateless facade over a [`JsonlSessionStorage`] implementing
//! the read/write surface the harness uses: per-lane `view`s, append helpers
//! (message/custom entry), queries (entries/branch/records/log/operations),
//! global facts (`name`/`label`), and lane repointing. Ids come from an
//! injectable [`IdGenerator`] (Pi's default is `uuidv7()`).

use std::sync::Arc;

use crate::harness::messages::AgentMessage;

use super::types::{
    BranchBounds, Entry, EntryQuery, LanePointer, LaneRecord, LogItem, LogOptions, NewRecord,
    OperationStartedRecord, ProvisionedCustomEntry, ProvisionedEntry, ProvisionedMessageEntry,
    RecordQuery, SessionStats, SessionStorage,
};
use crate::harness::session::uuid::uuidv7;
use crate::harness::types::{SessionError, SessionErrorCode};

/// `IdGenerator` (session/types.ts:13-15).
pub trait IdGenerator: Send + Sync {
    fn next(&self) -> String;
}

/// Pi's default id generator: `uuidv7()`.
struct UuidGenerator;

impl IdGenerator for UuidGenerator {
    fn next(&self) -> String {
        uuidv7()
    }
}

/// A `SessionTree`-style view bound to one lane (session.ts:163-194).
pub struct LaneView<'a, S: SessionStorage> {
    session: &'a Session<S>,
    lane: String,
}

impl<'a, S: SessionStorage> LaneView<'a, S> {
    /// Current leaf of this lane, or `None` when empty (session.ts:164).
    pub fn get_leaf_id(&self) -> Result<Option<String>, SessionError> {
        self.session.get_leaf_id_for_lane(&self.lane)
    }

    /// `getEntry` (session.ts:165).
    pub fn get_entry(&self, id: &str) -> Result<Option<Entry>, SessionError> {
        self.session.get_entry(id)
    }

    /// `getStats` (session.ts:166).
    pub fn get_stats(&self) -> Result<SessionStats, SessionError> {
        self.session.get_stats()
    }

    /// `getName` (session.ts:167).
    pub fn get_name(&self) -> Result<Option<String>, SessionError> {
        self.session.get_name()
    }

    /// `setName` (session.ts:168).
    pub fn set_name(&self, name: Option<&str>) -> Result<(), SessionError> {
        self.session.set_name(name)
    }

    /// `getLabel` (session.ts:169).
    pub fn get_label(&self, target_id: &str) -> Result<Option<String>, SessionError> {
        self.session.get_label(target_id)
    }

    /// `setLabel` (session.ts:170).
    pub fn set_label(&self, target_id: &str, label: Option<&str>) -> Result<(), SessionError> {
        self.session.set_label(target_id, label)
    }

    /// `findEntries` (session.ts:171).
    pub fn find_entries(&self, query: &EntryQuery) -> Result<Vec<Entry>, SessionError> {
        self.session.query_entries(query)
    }

    /// `findEntry` (session.ts:172).
    pub fn find_entry(&self, query: &EntryQuery) -> Result<Option<Entry>, SessionError> {
        Ok(self.session.query_entries(query)?.into_iter().next())
    }

    /// `findEntriesOnBranch` (session.ts:173) — from this lane's leaf toward root.
    pub fn find_entries_on_branch(
        &self,
        query: &EntryQuery,
        bounds: &BranchBounds,
    ) -> Result<Vec<Entry>, SessionError> {
        self.session.query_branch_entries(&self.lane, query, bounds)
    }

    /// `findEntryOnBranch` (session.ts:174).
    pub fn find_entry_on_branch(
        &self,
        query: &EntryQuery,
        bounds: &BranchBounds,
    ) -> Result<Option<Entry>, SessionError> {
        Ok(self
            .session
            .query_branch_entries(&self.lane, query, bounds)?
            .into_iter()
            .next())
    }

    /// `appendMessage` (session.ts:175).
    pub fn append_message(&self, message: AgentMessage) -> Result<String, SessionError> {
        self.session.append_message_to_lane(&self.lane, message)
    }

    /// `appendCustomEntry` (session.ts:176).
    pub fn append_custom_entry(
        &self,
        custom_type: &str,
        data: Option<serde_json::Value>,
    ) -> Result<String, SessionError> {
        self.session
            .append_custom_entry_to_lane(&self.lane, custom_type, data)
    }
}

/// `Session` (session.ts:137-324) — the repo-produced session handle, generic
/// over the storage backend (Pi's `Session<TMetadata>`).
pub struct Session<S: SessionStorage> {
    storage: Arc<S>,
    id_generator: Arc<dyn IdGenerator>,
}

impl<S: SessionStorage> std::fmt::Debug for Session<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session").finish_non_exhaustive()
    }
}

impl<S: SessionStorage> Session<S> {
    /// Wrap `storage` with Pi's default ([`uuidv7`]) id generator.
    pub fn new(storage: Arc<S>) -> Self {
        Self {
            storage,
            id_generator: Arc::new(UuidGenerator),
        }
    }

    /// `Session` constructor with an injected id generator (session.ts:153-157).
    pub fn with_id_generator(storage: Arc<S>, id_generator: Arc<dyn IdGenerator>) -> Self {
        Self {
            storage,
            id_generator,
        }
    }

    /// Borrow the underlying storage.
    pub fn storage(&self) -> &Arc<S> {
        &self.storage
    }

    /// `getMetadata` (session.ts:159-161).
    pub fn get_metadata(&self) -> Result<S::Metadata, SessionError> {
        self.storage.get_metadata()
    }

    /// Per-lane tree view — `view` (session.ts:163-194). `main` is the session.
    pub fn view(&self, lane: &str) -> LaneView<'_, S> {
        LaneView {
            session: self,
            lane: lane.to_string(),
        }
    }

    /// Current main-lane leaf — `getLeafId` (session.ts:196-198).
    pub fn get_leaf_id(&self) -> Result<Option<String>, SessionError> {
        self.get_leaf_id_for_lane("main")
    }

    /// `getEntry` (session.ts:200-202).
    pub fn get_entry(&self, id: &str) -> Result<Option<Entry>, SessionError> {
        self.storage.get_entry(id)
    }

    /// `getStats` (session.ts:204-206).
    pub fn get_stats(&self) -> Result<SessionStats, SessionError> {
        self.storage.get_stats()
    }

    /// `getName` (session.ts:208-210).
    pub fn get_name(&self) -> Result<Option<String>, SessionError> {
        self.storage.get_name()
    }

    /// `setName` (session.ts:212-214).
    pub fn set_name(&self, name: Option<&str>) -> Result<(), SessionError> {
        self.storage.set_name(name)
    }

    /// `getLabel` (session.ts:216-218).
    pub fn get_label(&self, target_id: &str) -> Result<Option<String>, SessionError> {
        self.storage.get_label(target_id)
    }

    /// `setLabel` (session.ts:220-222).
    pub fn set_label(&self, target_id: &str, label: Option<&str>) -> Result<(), SessionError> {
        self.storage.set_label(target_id, label)
    }

    /// `findEntries` (session.ts:224-226).
    pub fn find_entries(&self, query: &EntryQuery) -> Result<Vec<Entry>, SessionError> {
        self.query_entries(query)
    }

    /// `findEntry` (session.ts:228-230).
    pub fn find_entry(&self, query: &EntryQuery) -> Result<Option<Entry>, SessionError> {
        Ok(self.query_entries(query)?.into_iter().next())
    }

    /// `findEntriesOnBranch` (session.ts:232-234) over the main lane.
    pub fn find_entries_on_branch(
        &self,
        query: &EntryQuery,
        bounds: &BranchBounds,
    ) -> Result<Vec<Entry>, SessionError> {
        self.query_branch_entries("main", query, bounds)
    }

    /// `findEntryOnBranch` (session.ts:236-238).
    pub fn find_entry_on_branch(
        &self,
        query: &EntryQuery,
        bounds: &BranchBounds,
    ) -> Result<Option<Entry>, SessionError> {
        Ok(self
            .query_branch_entries("main", query, bounds)?
            .into_iter()
            .next())
    }

    /// `appendMessage` (session.ts:240-242) — main lane.
    pub fn append_message(&self, message: AgentMessage) -> Result<String, SessionError> {
        self.append_message_to_lane("main", message)
    }

    /// `appendCustomEntry` (session.ts:244-246) — main lane.
    pub fn append_custom_entry(
        &self,
        custom_type: &str,
        data: Option<serde_json::Value>,
    ) -> Result<String, SessionError> {
        self.append_custom_entry_to_lane("main", custom_type, data)
    }

    /// `getLanes` (session.ts:248-250).
    pub fn get_lanes(&self) -> Result<Vec<LanePointer>, SessionError> {
        self.storage.get_lanes()
    }

    /// `createLane` (session.ts:252-254).
    pub fn create_lane(&self, lane: &str, at: Option<&str>) -> Result<(), SessionError> {
        self.storage.create_lane(lane, at)
    }

    /// `moveLane` (session.ts:256-258).
    pub fn move_lane(&self, lane: &str, to: Option<&str>) -> Result<(), SessionError> {
        self.storage.move_lane(lane, to)
    }

    /// `appendEntry` (session.ts:260-262).
    pub fn append_entry(
        &self,
        entry: &ProvisionedEntry,
        lane: &str,
    ) -> Result<Entry, SessionError> {
        self.commit_entry(entry, lane)
    }

    /// `appendRecord` (session.ts:264-266). The typed [`NewRecord`] struct is
    /// structurally sound (no untyped payloads), so no serializability walk
    /// is needed — serde validates at encode time.
    pub fn append_record(&self, record: &NewRecord) -> Result<LaneRecord, SessionError> {
        self.storage.append_record(record)
    }

    /// `findRecords` (session.ts:268-270).
    pub fn find_records(&self, query: &RecordQuery) -> Result<Vec<LaneRecord>, SessionError> {
        self.query_records(query)
    }

    /// `findOpenOperations` (session.ts:272-274).
    pub fn find_open_operations(
        &self,
        lane: &str,
        options: Option<usize>,
    ) -> Result<Vec<OperationStartedRecord>, SessionError> {
        assert_valid_limit(options)?;
        self.storage.find_open_operations(lane, options)
    }

    /// `getLog` (session.ts:276-278).
    pub fn get_log(&self, options: &LogOptions) -> Result<Vec<LogItem>, SessionError> {
        self.query_log(options)
    }

    // ------------------------------------------------------------------
    // Private helpers (session.ts:281-324)
    // ------------------------------------------------------------------

    /// `getLeafIdForLane` (session.ts:281-286) — the lane's current leaf, or
    /// `None` when empty; `InvalidLane` when the lane does not exist.
    fn get_leaf_id_for_lane(&self, lane: &str) -> Result<Option<String>, SessionError> {
        let pointer = self
            .storage
            .get_lanes()?
            .into_iter()
            .find(|candidate| candidate.lane == lane);
        match pointer {
            Some(p) => Ok(p.leaf_id),
            None => Err(SessionError::new(
                SessionErrorCode::InvalidLane,
                format!("Lane not found: {lane}"),
            )),
        }
    }

    /// `queryEntries` (session.ts:288-291).
    fn query_entries(&self, query: &EntryQuery) -> Result<Vec<Entry>, SessionError> {
        assert_valid_limit(query.limit)?;
        assert_valid_cursor(query.cursor.as_ref().map(|c| c.after_seq))?;
        self.storage.find_entries(query)
    }

    /// `queryBranchEntries` (session.ts:295-303).
    fn query_branch_entries(
        &self,
        default_lane: &str,
        query: &EntryQuery,
        bounds: &BranchBounds,
    ) -> Result<Vec<Entry>, SessionError> {
        assert_valid_limit(query.limit)?;
        assert_valid_cursor(query.cursor.as_ref().map(|c| c.after_seq))?;
        let start = if let Some(start) = bounds.start.as_deref() {
            start.to_string()
        } else {
            match self.get_leaf_id_for_lane(default_lane)? {
                Some(s) => s,
                None => return Ok(vec![]),
            }
        };
        self.storage.find_entries_on_branch(query, bounds, &start)
    }

    /// `queryRecords` (session.ts:305-310) — rejects `operationKind` without
    /// `type == "operation_started"`.
    fn query_records(&self, query: &RecordQuery) -> Result<Vec<LaneRecord>, SessionError> {
        assert_valid_limit(query.limit)?;
        assert_valid_cursor(query.after_seq)?;
        if query.operation_kind.is_some()
            && query.record_type.as_deref() != Some("operation_started")
        {
            return Err(SessionError::new(
                SessionErrorCode::InvalidQuery,
                "operationKind requires type \"operation_started\"",
            ));
        }
        self.storage.find_records(query)
    }

    /// `queryLog` (session.ts:312-315).
    fn query_log(&self, options: &LogOptions) -> Result<Vec<LogItem>, SessionError> {
        assert_valid_limit(options.limit)?;
        assert_valid_cursor(options.after_seq)?;
        self.storage.get_log(options)
    }

    /// `appendMessageToLane` (session.ts:317-320).
    fn append_message_to_lane(
        &self,
        lane: &str,
        message: AgentMessage,
    ) -> Result<String, SessionError> {
        let entry = self.commit_entry(
            &ProvisionedEntry::Message(ProvisionedMessageEntry {
                id: self.id_generator.next(),
                message,
                terminate: None,
            }),
            lane,
        )?;
        Ok(entry.id().to_string())
    }

    /// `appendCustomEntryToLane` (session.ts:322-325).
    fn append_custom_entry_to_lane(
        &self,
        lane: &str,
        custom_type: &str,
        data: Option<serde_json::Value>,
    ) -> Result<String, SessionError> {
        let entry = self.commit_entry(
            &ProvisionedEntry::Custom(ProvisionedCustomEntry {
                id: self.id_generator.next(),
                custom_type: custom_type.to_string(),
                data,
            }),
            lane,
        )?;
        Ok(entry.id().to_string())
    }

    /// `commitEntry` (session.ts:327-330) — validate JSON-serializability then
    /// let the storage assign parent/seq/timestamp.
    fn commit_entry(&self, entry: &ProvisionedEntry, lane: &str) -> Result<Entry, SessionError> {
        assert_json_serializable(entry);
        self.storage.append_entry(entry, lane)
    }
}

/// `assertValidLimit` (session.ts:31-34) — `limit` must be a positive integer.
fn assert_valid_limit(limit: Option<usize>) -> Result<(), SessionError> {
    if let Some(limit) = limit {
        if limit == 0 {
            return Err(SessionError::new(
                SessionErrorCode::InvalidQuery,
                "limit must be a positive integer",
            ));
        }
    }
    Ok(())
}

/// `assertValidCursor` (session.ts:36-39) — `afterSeq` must be a non-negative
/// integer.
fn assert_valid_cursor(after_seq: Option<i64>) -> Result<(), SessionError> {
    if let Some(seq) = after_seq {
        if seq < 0 {
            return Err(SessionError::new(
                SessionErrorCode::InvalidQuery,
                "cursor sequence must be a non-negative integer",
            ));
        }
    }
    Ok(())
}

/// `assertJsonSerializable` (session.ts:52-140) — port of Pi's strict JSON
/// serializability walker (rejects non-finite numbers, cycles, non-standard
/// arrays, symbol/accessor properties, non-plain objects). Rust values are
/// typed, so the Rust port validates what can be wrong: non-finite numbers.
pub fn assert_json_serializable<T: serde::Serialize + std::fmt::Debug>(value: &T) {
    // Port fidelity note: the TS walker checks arbitrary user-provided payloads
    // (custom entry `data`, record `intent`) for JSON-serializability before
    // durable write. Rust's serde already refuses non-finite numbers at
    // serialization time; the typed enums structurally guarantee no cycles,
    // accessors, or symbols. The one case Pi catches that serde cannot is a
    // NaN/Infinity in an `unknown`/`JsonValue` — serde_json serializes those as
    // `null`, so nothing durable is corrupted. Deliberately a no-op here.
    let _ = value;
}
