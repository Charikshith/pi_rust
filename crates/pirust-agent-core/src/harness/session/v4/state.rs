//! v4 `SessionState` — port of `packages/agent/src/harness/session/state.ts`.
//!
//! The in-memory replay core of the 0.84.2 mutation-log session model: applies
//! [`SessionMutation`]s (entries / lane records / lane moves / facts) in
//! strict `seq` order, and answers the read queries (entries, branch walks,
//! records, open operations, log, name/labels, stats, fork mutations).

use std::collections::{HashMap, HashSet};

use super::types::{
    v4_error, BranchBounds, Entry, EntryOrder, EntryQuery, ForkOptions, ForkPosition, LanePointer,
    LaneRecord, LogItem, LogOptions, OperationIntent, OperationStartedRecord, RecordQuery,
    SessionMutation, SessionStats,
};
use crate::harness::types::{SessionError, SessionErrorCode};

/// `invalidMutation` (state.ts:19-21).
fn invalid_mutation(message: impl Into<String>) -> SessionError {
    v4_error(
        SessionErrorCode::InvalidEntry,
        format!("Invalid session mutation: {}", message.into()),
    )
}

/// `assertValidLimit` (state.ts:24-27).
fn assert_valid_limit(limit: Option<usize>) -> Result<(), SessionError> {
    if limit.is_some_and(|l| l == 0) {
        return Err(v4_error(
            SessionErrorCode::InvalidQuery,
            "limit must be a positive integer",
        ));
    }
    Ok(())
}

/// `assertValidCursor` (state.ts:29-32).
fn assert_valid_cursor(after_seq: Option<i64>) -> Result<(), SessionError> {
    if after_seq.is_some_and(|s| s < 0) {
        return Err(v4_error(
            SessionErrorCode::InvalidQuery,
            "cursor sequence must be a non-negative integer",
        ));
    }
    Ok(())
}

/// `ordered` (state.ts:34-40).
fn ordered<T>(items: &[T], order: Option<EntryOrder>) -> Vec<&T> {
    match order {
        Some(EntryOrder::OldestFirst) | None => items.iter().collect(),
        Some(EntryOrder::NewestFirst) => items.iter().rev().collect(),
    }
}

/// The in-memory session state (state.ts:42-51).
#[derive(Debug, Default)]
pub struct SessionState {
    sequence: i64,
    used_ids: HashSet<String>,
    entries: Vec<Entry>,
    entries_by_id: HashMap<String, Entry>,
    records: Vec<LaneRecord>,
    open_operations_by_lane: HashMap<String, HashMap<String, OperationStartedRecord>>,
    lanes: HashMap<String, Option<String>>,
    log: Vec<LogItem>,
    stats: SessionStats,
    name: Option<String>,
    labels: HashMap<String, String>,
}

impl SessionState {
    /// Fresh state (an empty session before any mutation is applied). The
    /// `main` lane exists from birth with a `null` leaf — state.ts:57 seeds
    /// `new Map([["main", null]])`.
    pub fn new() -> Self {
        Self {
            sequence: 0,
            used_ids: HashSet::new(),
            entries: Vec::new(),
            entries_by_id: HashMap::new(),
            records: Vec::new(),
            open_operations_by_lane: HashMap::new(),
            lanes: HashMap::from([("main".to_string(), None)]),
            log: Vec::new(),
            stats: SessionStats::default(),
            name: None,
            labels: HashMap::new(),
        }
    }

    /// `get nextSequence()` (state.ts:53-55).
    pub fn next_sequence(&self) -> i64 {
        self.sequence + 1
    }

    /// `getLanes()` (state.ts:57-59).
    pub fn get_lanes(&self) -> Vec<LanePointer> {
        let mut lanes: Vec<LanePointer> = self
            .lanes
            .iter()
            .map(|(lane, leaf_id)| LanePointer {
                lane: lane.clone(),
                leaf_id: leaf_id.clone(),
            })
            .collect();
        lanes.sort_by(|a, b| a.lane.cmp(&b.lane));
        lanes
    }

    /// `requireLane` (state.ts:61-64).
    pub fn require_lane(&self, lane: &str) -> Result<Option<String>, SessionError> {
        match self.lanes.get(lane) {
            Some(leaf) => Ok(leaf.clone()),
            None => Err(v4_error(
                SessionErrorCode::InvalidLane,
                format!("Lane not found: {lane}"),
            )),
        }
    }

    /// `validateNewLane` (state.ts:66-68).
    pub fn validate_new_lane(&self, lane: &str) -> Result<(), SessionError> {
        if self.lanes.contains_key(lane) {
            return Err(v4_error(
                SessionErrorCode::AlreadyExists,
                format!("Lane already exists: {lane}"),
            ));
        }
        Ok(())
    }

    /// `validateTarget` (state.ts:70-73).
    pub fn validate_target(&self, target_id: Option<&str>) -> Result<(), SessionError> {
        if let Some(id) = target_id {
            if !self.entries_by_id.contains_key(id) {
                return Err(v4_error(
                    SessionErrorCode::NotFound,
                    format!("Entry not found: {id}"),
                ));
            }
        }
        Ok(())
    }

    /// `validateUnusedId` (state.ts:75-77).
    pub fn validate_unused_id(&self, id: &str) -> Result<(), SessionError> {
        if self.used_ids.contains(id) {
            return Err(v4_error(
                SessionErrorCode::AlreadyExists,
                format!("Session id already exists: {id}"),
            ));
        }
        Ok(())
    }

    /// `applyMutation` (state.ts:79-167).
    pub fn apply_mutation(&mut self, mutation: &SessionMutation) -> Result<(), SessionError> {
        let seq = mutation.seq();
        if seq != self.sequence + 1 {
            return Err(invalid_mutation(format!("has non-consecutive seq {seq}")));
        }

        match mutation {
            SessionMutation::Entry { lane, entry } => {
                if self.used_ids.contains(entry.id()) {
                    return Err(invalid_mutation(format!(
                        "contains duplicate id {}",
                        entry.id()
                    )));
                }
                if let Some(lane_name) = lane {
                    let leaf_id = self.lanes.get(lane_name);
                    if leaf_id.is_none() {
                        return Err(invalid_mutation(format!(
                            "references missing lane {lane_name}"
                        )));
                    }
                    if entry_parent(entry).as_deref() != leaf_id.unwrap().as_deref() {
                        return Err(invalid_mutation("does not chain to the lane leaf"));
                    }
                }
                if let Some(parent) = entry_parent(entry) {
                    if !self.entries_by_id.contains_key(&parent) {
                        return Err(invalid_mutation(format!(
                            "references missing parent {parent}"
                        )));
                    }
                }
                self.sequence = seq;
                self.used_ids.insert(entry.id().to_string());
                self.entries.push(entry.clone());
                self.entries_by_id
                    .insert(entry.id().to_string(), entry.clone());
                if let Some(lane_name) = lane {
                    self.lanes
                        .insert(lane_name.clone(), Some(entry.id().to_string()));
                }
                self.log.push(LogItem::Entry {
                    seq,
                    entry: entry.clone(),
                });
                if entry.entry_type() == "message" {
                    self.stats.message_count += 1;
                }
            }
            SessionMutation::Record { record } => {
                if !self.lanes.contains_key(record.lane()) {
                    return Err(invalid_mutation(format!(
                        "references missing lane {}",
                        record.lane()
                    )));
                }
                if self.used_ids.contains(record.id()) {
                    return Err(invalid_mutation(format!(
                        "contains duplicate id {}",
                        record.id()
                    )));
                }
                self.sequence = seq;
                self.used_ids.insert(record.id().to_string());
                self.records.push(record.clone());
                match record {
                    LaneRecord::OperationStarted(r) => {
                        self.open_operations_by_lane
                            .entry(r.lane.clone())
                            .or_default()
                            .insert(r.id.clone(), r.clone());
                    }
                    LaneRecord::OperationFinished(r) => {
                        self.open_operations_by_lane
                            .get_mut(&r.lane)
                            .map(|m| m.remove(&r.run_id));
                    }
                    LaneRecord::Usage(r) => {
                        self.stats.cached_tokens += r.usage.cache_read as i64;
                        self.stats.uncached_tokens += (r.usage.input + r.usage.cache_write) as i64;
                        self.stats.total_tokens += r.usage.total_tokens.unwrap_or(0) as i64;
                        self.stats.cost_total += r.usage.cost.total;
                    }
                    _ => {}
                }
                self.log.push(LogItem::Record {
                    seq,
                    record: record.clone(),
                });
            }
            SessionMutation::Lane { lane, leaf_id, .. } => {
                if let Some(leaf) = leaf_id {
                    if !self.entries_by_id.contains_key(leaf) {
                        return Err(invalid_mutation(format!(
                            "references missing lane target {leaf}"
                        )));
                    }
                }
                self.sequence = seq;
                self.lanes.insert(lane.clone(), leaf_id.clone());
                self.log.push(LogItem::Lane {
                    seq,
                    lane: lane.clone(),
                    leaf_id: leaf_id.clone(),
                });
            }
            SessionMutation::FactLabel {
                target_id, label, ..
            } => {
                if !self.entries_by_id.contains_key(target_id) {
                    return Err(invalid_mutation(format!(
                        "references missing label target {target_id}"
                    )));
                }
                self.sequence = seq;
                match label {
                    Some(l) => {
                        self.labels.insert(target_id.clone(), l.clone());
                    }
                    None => {
                        self.labels.remove(target_id);
                    }
                }
                self.log.push(LogItem::FactLabel {
                    seq,
                    target_id: target_id.clone(),
                    label: label.clone(),
                });
            }
            SessionMutation::FactName { name, .. } => {
                self.sequence = seq;
                self.name = name.clone();
                self.log.push(LogItem::FactName {
                    seq,
                    name: name.clone(),
                });
            }
        }
        Ok(())
    }

    /// `getEntry` (state.ts:169-171).
    pub fn get_entry(&self, id: &str) -> Option<&Entry> {
        self.entries_by_id.get(id)
    }

    /// `findEntries` (state.ts:173-183).
    pub fn find_entries(&self, query: &EntryQuery) -> Result<Vec<Entry>, SessionError> {
        assert_valid_limit(query.limit)?;
        assert_valid_cursor(query.cursor.map(|c| c.after_seq))?;
        let mut results: Vec<Entry> = Vec::new();
        for entry in ordered(&self.entries, query.order) {
            if !matches_entry_query(entry, query) {
                continue;
            }
            results.push(entry.clone());
            if results.len() == query.limit.unwrap_or(usize::MAX) {
                break;
            }
        }
        Ok(results)
    }

    /// `findEntriesOnBranch` (state.ts:185-204).
    pub fn find_entries_on_branch(
        &self,
        query: &EntryQuery,
        bounds: &BranchBounds,
        start: &str,
    ) -> Result<Vec<Entry>, SessionError> {
        assert_valid_limit(query.limit)?;
        assert_valid_cursor(query.cursor.map(|c| c.after_seq))?;
        let mut results: Vec<Entry> = Vec::new();
        if query.order == Some(EntryOrder::OldestFirst) {
            let mut walk = self.walk_to_root(Some(start.to_string()), bounds)?;
            walk.reverse();
            for entry in walk {
                let reached_bound = bounds
                    .stop_at_id
                    .as_ref()
                    .is_some_and(|id| id == entry.id())
                    || bounds
                        .stop_at_type
                        .as_ref()
                        .is_some_and(|t| t == entry.entry_type());
                if matches_entry_query(&entry, query) {
                    results.push(entry.clone());
                }
                if reached_bound || results.len() == query.limit.unwrap_or(usize::MAX) {
                    break;
                }
            }
        } else {
            for entry in self.walk_to_root(Some(start.to_string()), bounds)? {
                if matches_entry_query(&entry, query) {
                    results.push(entry.clone());
                }
                if results.len() == query.limit.unwrap_or(usize::MAX) {
                    break;
                }
            }
        }
        Ok(results)
    }

    /// `findRecords` (state.ts:206-215).
    pub fn find_records(&self, query: &RecordQuery) -> Result<Vec<LaneRecord>, SessionError> {
        assert_valid_limit(query.limit)?;
        assert_valid_cursor(query.after_seq)?;
        let mut results: Vec<LaneRecord> = Vec::new();
        for record in ordered(&self.records, query.order) {
            if !matches_record_query(record, query) {
                continue;
            }
            results.push(record.clone());
            if results.len() == query.limit.unwrap_or(usize::MAX) {
                break;
            }
        }
        Ok(results)
    }

    /// `findOpenOperations` (state.ts:217-222). `options.limit` caps the result.
    pub fn find_open_operations(
        &self,
        lane: &str,
        limit: Option<usize>,
    ) -> Result<Vec<OperationStartedRecord>, SessionError> {
        let mut open: Vec<OperationStartedRecord> = self
            .open_operations_by_lane
            .get(lane)
            .map(|m| m.values().cloned().collect())
            .unwrap_or_default();
        open.reverse();
        open.truncate(limit.unwrap_or(usize::MAX));
        Ok(open)
    }

    /// `getLog` (state.ts:224-232).
    pub fn get_log(&self, options: &LogOptions) -> Result<Vec<LogItem>, SessionError> {
        assert_valid_limit(options.limit)?;
        assert_valid_cursor(options.after_seq)?;
        let mut results: Vec<LogItem> = Vec::new();
        for item in &self.log {
            if options.after_seq.is_some_and(|s| item_seq(item) <= s) {
                continue;
            }
            results.push(item.clone());
            if results.len() == options.limit.unwrap_or(usize::MAX) {
                break;
            }
        }
        Ok(results)
    }

    /// `getName` (state.ts:234-236).
    pub fn get_name(&self) -> Option<String> {
        self.name.clone()
    }

    /// `getLabel` (state.ts:238-240).
    pub fn get_label(&self, id: &str) -> Option<String> {
        self.labels.get(id).cloned()
    }

    /// `getStats` (state.ts:242-244).
    pub fn get_stats(&self) -> SessionStats {
        self.stats
    }

    /// `createForkMutations` (state.ts:246-293).
    pub fn create_fork_mutations(
        &self,
        options: &ForkOptions,
    ) -> Result<Vec<SessionMutation>, SessionError> {
        let (copied_entries, fork_lanes): (Vec<Entry>, Vec<LanePointer>) = match options {
            ForkOptions::Tree => {
                let copied = self.find_entries(&EntryQuery {
                    order: Some(EntryOrder::OldestFirst),
                    ..Default::default()
                })?;
                (copied, self.get_lanes())
            }
            ForkOptions::Branch { entry_id, position } => {
                let selected_entry_id = match entry_id {
                    Some(id) => Some(id.clone()),
                    None => self.require_lane("main")?,
                };
                let mut target_id: Option<String> = None;
                if let Some(selected) = selected_entry_id.as_deref() {
                    let entry = self.get_entry(selected).cloned();
                    match entry {
                        Some(e) if e.entry_type() == "message" => {
                            let pos = *position.as_ref().unwrap_or(&if entry_id.is_none() {
                                ForkPosition::At
                            } else {
                                ForkPosition::Before
                            });
                            target_id = if pos == ForkPosition::At {
                                Some(e.id().to_string())
                            } else {
                                entry_parent(&e)
                            };
                        }
                        _ => {
                            return Err(v4_error(
                                SessionErrorCode::InvalidForkTarget,
                                format!("Fork target is not a message entry: {selected}"),
                            ));
                        }
                    }
                }
                let copied = match target_id.as_deref() {
                    None => Vec::new(),
                    Some(t) => self.find_entries_on_branch(
                        &EntryQuery {
                            order: Some(EntryOrder::OldestFirst),
                            ..Default::default()
                        },
                        &BranchBounds::default(),
                        t,
                    )?,
                };
                (
                    copied,
                    vec![LanePointer {
                        lane: "main".to_string(),
                        leaf_id: target_id,
                    }],
                )
            }
        };

        let mut mutations: Vec<SessionMutation> = Vec::new();
        let mut sequence = 1;
        for source_entry in &copied_entries {
            mutations.push(SessionMutation::Entry {
                lane: None,
                entry: clone_with_seq(source_entry, sequence),
            });
            sequence += 1;
        }
        for pointer in &fork_lanes {
            mutations.push(SessionMutation::Lane {
                seq: sequence,
                lane: pointer.lane.clone(),
                leaf_id: pointer.leaf_id.clone(),
            });
            sequence += 1;
        }
        if let Some(name) = &self.name {
            mutations.push(SessionMutation::FactName {
                seq: sequence,
                name: Some(name.clone()),
            });
            sequence += 1;
        }
        for entry in &copied_entries {
            if let Some(label) = self.labels.get(entry.id()) {
                mutations.push(SessionMutation::FactLabel {
                    seq: sequence,
                    target_id: entry.id().to_string(),
                    label: Some(label.clone()),
                });
                sequence += 1;
            }
        }
        Ok(mutations)
    }

    /// `walkToRoot` (state.ts:295-318).
    fn walk_to_root(
        &self,
        start: Option<String>,
        bounds: &BranchBounds,
    ) -> Result<Vec<Entry>, SessionError> {
        let mut out: Vec<Entry> = Vec::new();
        let Some(start) = start else {
            return Ok(out);
        };
        let mut visited = HashSet::new();
        let mut current = self.entries_by_id.get(&start).cloned().ok_or_else(|| {
            v4_error(
                SessionErrorCode::NotFound,
                format!("Entry not found: {start}"),
            )
        })?;
        loop {
            if visited.contains(current.id()) {
                return Err(invalid_mutation(format!(
                    "Session branch contains a cycle at {}",
                    current.id()
                )));
            }
            visited.insert(current.id().to_string());
            out.push(current.clone());
            if current.id() == bounds.stop_at_id.as_deref().unwrap_or("")
                || bounds
                    .stop_at_type
                    .as_deref()
                    .is_some_and(|t| t == current.entry_type())
                || current.parent_id().is_none()
            {
                break;
            }
            let parent_id = current.parent_id().cloned().unwrap();
            current = self.entries_by_id.get(&parent_id).cloned().ok_or_else(|| {
                v4_error(
                    SessionErrorCode::InvalidEntry,
                    format!("Entry not found: {parent_id}"),
                )
            })?;
        }
        Ok(out)
    }
}

/// The entry's `parentId` (Option).
fn entry_parent(entry: &Entry) -> Option<String> {
    entry.parent_id().cloned()
}

/// `matchesEntryQuery` (state.ts:320-327).
fn matches_entry_query(entry: &Entry, query: &EntryQuery) -> bool {
    (query.entry_type.is_none() || query.entry_type.as_deref() == Some(entry.entry_type()))
        && (query.custom_type.is_none()
            || (entry.entry_type() == "custom"
                && entry.custom_type() == query.custom_type.as_deref()))
        && (query.cursor.is_none()
            || match query.order {
                Some(EntryOrder::OldestFirst) | None => {
                    entry.seq() > query.cursor.unwrap().after_seq
                }
                Some(EntryOrder::NewestFirst) => entry.seq() < query.cursor.unwrap().after_seq,
            })
}

/// `matchesRecordQuery` (state.ts:329-338).
fn matches_record_query(record: &LaneRecord, query: &RecordQuery) -> bool {
    (query.lane.is_none() || query.lane.as_deref() == Some(record.lane()))
        && (query.record_type.is_none()
            || query.record_type.as_deref() == Some(record.record_type()))
        && (query.run_id.is_none()
            || match record {
                LaneRecord::OperationStarted(r) => Some(r.id.as_str()) == query.run_id.as_deref(),
                _ => record_run_id(record) == query.run_id.as_deref(),
            })
        && (query.operation_kind.is_none()
            || match record {
                LaneRecord::OperationStarted(r) => {
                    Some(operation_kind(&r.intent)) == query.operation_kind.as_deref()
                }
                _ => false,
            })
        && (query.after_seq.is_none() || record.seq() > query.after_seq.unwrap())
}

/// `record.runId` for records that carry one (runId-matching helper).
fn record_run_id(record: &LaneRecord) -> Option<&str> {
    match record {
        LaneRecord::AbortRequested(r) => Some(&r.run_id),
        LaneRecord::OperationFinished(r) => Some(&r.run_id),
        LaneRecord::StepAttempt(r) => Some(&r.run_id),
        LaneRecord::ToolStarted(r) => Some(&r.run_id),
        LaneRecord::QueueEnqueued(r) => r.run_id.as_deref(),
        LaneRecord::WriteDeferred(r) => Some(&r.run_id),
        LaneRecord::Usage(r) => r.cause.run_id.as_deref(),
        LaneRecord::OperationStarted(r) => Some(&r.id),
        LaneRecord::QueueCancelled(r) => r.run_id.as_deref(),
    }
}

/// `intent.kind` for an operation-started record.
fn operation_kind(intent: &OperationIntent) -> &str {
    match intent {
        OperationIntent::Run { .. } => "run",
        OperationIntent::Compaction { .. } => "compaction",
        OperationIntent::Navigation { .. } => "navigation",
    }
}

/// `{...entry, seq}` (fork copies renumber seq from 1).
fn clone_with_seq(entry: &Entry, seq: i64) -> Entry {
    let mut clone = entry.clone();
    set_entry_seq(&mut clone, seq);
    clone
}

fn set_entry_seq(entry: &mut Entry, seq: i64) {
    match entry {
        Entry::Message(e) => e.seq = seq,
        Entry::ModelChange(e) => e.seq = seq,
        Entry::ThinkingLevel(e) => e.seq = seq,
        Entry::ActiveTools(e) => e.seq = seq,
        Entry::Compaction(e) => e.seq = seq,
        Entry::BranchSummary(e) => e.seq = seq,
        Entry::Custom(e) => e.seq = seq,
    }
}

/// `log item seq` (getLog filter).
fn item_seq(item: &LogItem) -> i64 {
    match item {
        LogItem::Entry { seq, .. }
        | LogItem::Record { seq, .. }
        | LogItem::Lane { seq, .. }
        | LogItem::FactName { seq, .. }
        | LogItem::FactLabel { seq, .. } => *seq,
    }
}
