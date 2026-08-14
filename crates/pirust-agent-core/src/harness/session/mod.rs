//! Session tree + storage — port of `packages/agent/src/harness/session/`.
//!
//! Spec: `docs/analysis/07-agent-core-spec.md` §1.4 (tree entries), §4
//! (append-only writes, leaf-repointing), §7 (UUIDv7). Depends on
//! `harness::types` + `harness::messages` (§13, wave 2).
//!
//! Present submodules:
//! - `uuid`: UUIDv7 generation + injectable source (§7, `[LEAF]`).
//! - `memory_storage`: `InMemorySessionStorage` (memory-storage.ts:42).
//! - `jsonl_storage`: `JsonlSessionStorage` + v1 parse/migration adapter
//!   (jsonl-storage.ts:180).
//!
//! This module hosts the [`Session`] wrapper (session.ts:137-338) plus the
//! context-building free functions ([`build_session_context`] /
//! [`default_context_entry_transform`], session.ts:37-135) and the small
//! entry/clock helpers shared with the two storage backends.
//!
//! # Injectable clock
//!
//! Pi reads `new Date().toISOString()` directly for every entry timestamp. This
//! port threads an injectable [`Clock`] so entry timestamps are deterministic in
//! tests; [`SystemClock`] reproduces the wall-clock default.

pub mod jsonl_storage;
pub mod memory_storage;
pub mod uuid;

use serde_json::Value;

use super::messages::{
    create_branch_summary_message, create_compaction_summary_message, create_custom_message,
    AgentMessage,
};
use super::types::{
    SessionContext, SessionContextModel, SessionError, SessionStorage, SessionTreeEntry,
};

use pirust_ai::types::Message;

// ===========================================================================
// Injectable clock (entry `timestamp` ISO strings)
// ===========================================================================

/// Source of the ISO-8601 timestamp stamped on every appended entry.
///
/// Pi uses `new Date().toISOString()` inline; this seam keeps that default
/// ([`SystemClock`]) while letting tests inject a fixed value ([`FixedClock`]).
pub trait Clock: Send + Sync {
    /// Current time as an ISO-8601 UTC string (`YYYY-MM-DDTHH:MM:SS.sssZ`).
    fn now_iso(&self) -> String;
}

/// Wall-clock [`Clock`] — `SystemTime::now()` formatted like `Date#toISOString`.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_iso(&self) -> String {
        system_now_iso()
    }
}

/// Fixed [`Clock`] returning the same ISO string on every call (test seam).
#[derive(Debug, Clone)]
pub struct FixedClock(pub String);

impl Clock for FixedClock {
    fn now_iso(&self) -> String {
        self.0.clone()
    }
}

/// Current wall-clock time as an ISO-8601 UTC string.
pub(crate) fn system_now_iso() -> String {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    epoch_ms_to_iso8601(ms)
}

// ---------------------------------------------------------------------------
// Civil-date <-> epoch-ms conversion (Howard Hinnant's algorithms).
// Needed because entry timestamps are ISO strings while inner message
// timestamps are Unix-ms numbers (spec §1.2): the custom-message constructors
// flip string -> number via `new Date(iso).getTime()`.
// ---------------------------------------------------------------------------

/// Days since 1970-01-01 for a proleptic-Gregorian civil date.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// Civil date `(year, month, day)` from days since 1970-01-01.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Format Unix milliseconds as an ISO-8601 UTC string (matches JS
/// `Date#toISOString`: always `.sss` milliseconds and a trailing `Z`).
pub(crate) fn epoch_ms_to_iso8601(ms: i64) -> String {
    let days = ms.div_euclid(86_400_000);
    let ms_of_day = ms.rem_euclid(86_400_000);
    let (y, m, d) = civil_from_days(days);
    let secs = ms_of_day / 1000;
    let millis = ms_of_day % 1000;
    let hh = secs / 3600;
    let mm = (secs % 3600) / 60;
    let ss = secs % 60;
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}.{millis:03}Z")
}

/// Parse the canonical `Date#toISOString` form (`YYYY-MM-DDTHH:MM:SS.sssZ`) into
/// Unix milliseconds. Mirrors `new Date(iso).getTime()` for that format; returns
/// `0` for anything it cannot parse (our entry timestamps are always canonical).
fn iso8601_to_epoch_ms(s: &str) -> i64 {
    fn parse(s: &str) -> Option<i64> {
        let s = s.strip_suffix('Z').unwrap_or(s);
        let (date, time) = s.split_once('T')?;
        let mut dp = date.split('-');
        let y: i64 = dp.next()?.parse().ok()?;
        let mo: i64 = dp.next()?.parse().ok()?;
        let d: i64 = dp.next()?.parse().ok()?;
        let (hms, frac) = match time.split_once('.') {
            Some((a, b)) => (a, Some(b)),
            None => (time, None),
        };
        let mut tp = hms.split(':');
        let hh: i64 = tp.next()?.parse().ok()?;
        let mm: i64 = tp.next()?.parse().ok()?;
        let ss: i64 = tp.next().unwrap_or("0").parse().ok()?;
        // Milliseconds: pad/truncate the fractional part to exactly 3 digits.
        let millis: i64 = match frac {
            Some(f) => {
                let mut f = f.to_string();
                f.truncate(3);
                while f.len() < 3 {
                    f.push('0');
                }
                f.parse().ok()?
            }
            None => 0,
        };
        let days = days_from_civil(y, mo, d);
        Some(days * 86_400_000 + hh * 3_600_000 + mm * 60_000 + ss * 1000 + millis)
    }
    parse(s).unwrap_or(0)
}

// ===========================================================================
// Entry accessors + label cache (shared with both storage backends)
// ===========================================================================

/// The `id` field common to every entry variant.
pub(crate) fn entry_id(entry: &SessionTreeEntry) -> &str {
    match entry {
        SessionTreeEntry::Message { id, .. }
        | SessionTreeEntry::ThinkingLevelChange { id, .. }
        | SessionTreeEntry::ModelChange { id, .. }
        | SessionTreeEntry::ActiveToolsChange { id, .. }
        | SessionTreeEntry::Compaction { id, .. }
        | SessionTreeEntry::BranchSummary { id, .. }
        | SessionTreeEntry::Custom { id, .. }
        | SessionTreeEntry::CustomMessage { id, .. }
        | SessionTreeEntry::Label { id, .. }
        | SessionTreeEntry::SessionInfo { id, .. }
        | SessionTreeEntry::Leaf { id, .. } => id.as_str(),
    }
}

/// The `parentId` field common to every entry variant.
pub(crate) fn entry_parent_id(entry: &SessionTreeEntry) -> Option<&str> {
    match entry {
        SessionTreeEntry::Message { parent_id, .. }
        | SessionTreeEntry::ThinkingLevelChange { parent_id, .. }
        | SessionTreeEntry::ModelChange { parent_id, .. }
        | SessionTreeEntry::ActiveToolsChange { parent_id, .. }
        | SessionTreeEntry::Compaction { parent_id, .. }
        | SessionTreeEntry::BranchSummary { parent_id, .. }
        | SessionTreeEntry::Custom { parent_id, .. }
        | SessionTreeEntry::CustomMessage { parent_id, .. }
        | SessionTreeEntry::Label { parent_id, .. }
        | SessionTreeEntry::SessionInfo { parent_id, .. }
        | SessionTreeEntry::Leaf { parent_id, .. } => parent_id.as_deref(),
    }
}

/// The `type` tag string for an entry (used by `find_entries`).
pub(crate) fn entry_type_tag(entry: &SessionTreeEntry) -> &'static str {
    match entry {
        SessionTreeEntry::Message { .. } => "message",
        SessionTreeEntry::ThinkingLevelChange { .. } => "thinking_level_change",
        SessionTreeEntry::ModelChange { .. } => "model_change",
        SessionTreeEntry::ActiveToolsChange { .. } => "active_tools_change",
        SessionTreeEntry::Compaction { .. } => "compaction",
        SessionTreeEntry::BranchSummary { .. } => "branch_summary",
        SessionTreeEntry::Custom { .. } => "custom",
        SessionTreeEntry::CustomMessage { .. } => "custom_message",
        SessionTreeEntry::Label { .. } => "label",
        SessionTreeEntry::SessionInfo { .. } => "session_info",
        SessionTreeEntry::Leaf { .. } => "leaf",
    }
}

/// Leaf id implied after applying an entry: a `leaf` entry points at its
/// `targetId`; any other entry becomes the new leaf itself
/// (memory-storage.ts:38-40).
pub(crate) fn leaf_id_after_entry(entry: &SessionTreeEntry) -> Option<String> {
    match entry {
        SessionTreeEntry::Leaf { target_id, .. } => target_id.clone(),
        other => Some(entry_id(other).to_string()),
    }
}

/// Apply a `label` entry to the label cache (set on non-empty trimmed label,
/// otherwise clear) — memory-storage.ts:10-18.
pub(crate) fn update_label_cache(
    labels_by_id: &mut std::collections::HashMap<String, String>,
    entry: &SessionTreeEntry,
) {
    if let SessionTreeEntry::Label {
        target_id, label, ..
    } = entry
    {
        let trimmed = label.as_deref().map(str::trim).filter(|s| !s.is_empty());
        match trimmed {
            Some(l) => {
                labels_by_id.insert(target_id.clone(), l.to_string());
            }
            None => {
                labels_by_id.remove(target_id);
            }
        }
    }
}

/// Build the label cache by replaying every `label` entry (memory-storage.ts:20-26).
pub(crate) fn build_labels_by_id(
    entries: &[SessionTreeEntry],
) -> std::collections::HashMap<String, String> {
    let mut labels_by_id = std::collections::HashMap::new();
    for entry in entries {
        update_label_cache(&mut labels_by_id, entry);
    }
    labels_by_id
}

// ===========================================================================
// Context building (session.ts:37-135)
// ===========================================================================

/// Derive model / thinking-level / active-tools state from a root-first path
/// (session.ts:37-55). Assistant messages also advance the model.
fn derive_session_context_state(
    path_entries: &[SessionTreeEntry],
) -> (String, Option<SessionContextModel>, Option<Vec<String>>) {
    let mut thinking_level = "off".to_string();
    let mut model: Option<SessionContextModel> = None;
    let mut active_tool_names: Option<Vec<String>> = None;

    for entry in path_entries {
        match entry {
            SessionTreeEntry::ThinkingLevelChange {
                thinking_level: tl, ..
            } => {
                thinking_level = tl.clone();
            }
            SessionTreeEntry::ModelChange {
                provider, model_id, ..
            } => {
                model = Some(SessionContextModel {
                    provider: provider.clone(),
                    model_id: model_id.clone(),
                });
            }
            SessionTreeEntry::Message {
                message: AgentMessage::Llm(Message::Assistant(a)),
                ..
            } => {
                model = Some(SessionContextModel {
                    provider: a.provider.0.clone(),
                    model_id: a.model.clone(),
                });
            }
            SessionTreeEntry::ActiveToolsChange {
                active_tool_names: names,
                ..
            } => {
                active_tool_names = Some(names.clone());
            }
            _ => {}
        }
    }

    (thinking_level, model, active_tool_names)
}

/// Collapse history at the LAST compaction point (session.ts:57-80): emit the
/// compaction, then every entry from its `firstKeptEntryId` up to the compaction,
/// then everything after it. Entries before `firstKeptEntryId` are dropped.
pub fn default_context_entry_transform(path_entries: &[SessionTreeEntry]) -> Vec<SessionTreeEntry> {
    let mut compaction_idx: Option<usize> = None;
    for (i, entry) in path_entries.iter().enumerate() {
        if matches!(entry, SessionTreeEntry::Compaction { .. }) {
            compaction_idx = Some(i);
        }
    }
    let Some(compaction_idx) = compaction_idx else {
        return path_entries.to_vec();
    };
    let SessionTreeEntry::Compaction {
        first_kept_entry_id,
        ..
    } = &path_entries[compaction_idx]
    else {
        unreachable!("compaction_idx points at a compaction entry");
    };

    let mut entries: Vec<SessionTreeEntry> = vec![path_entries[compaction_idx].clone()];
    let mut found_first_kept = false;
    for entry in &path_entries[..compaction_idx] {
        if entry_id(entry) == first_kept_entry_id {
            found_first_kept = true;
        }
        if found_first_kept {
            entries.push(entry.clone());
        }
    }
    for entry in &path_entries[compaction_idx + 1..] {
        entries.push(entry.clone());
    }
    entries
}

/// Project one context entry into zero or more in-context messages
/// (session.ts:93-123). Custom (data) entries produce nothing without a
/// projector (projectors are an unported extension seam).
fn session_entry_to_context_messages(entry: &SessionTreeEntry) -> Vec<AgentMessage> {
    match entry {
        SessionTreeEntry::Message { message, .. } => vec![message.clone()],
        SessionTreeEntry::CustomMessage {
            custom_type,
            content,
            display,
            details,
            timestamp,
            ..
        } => vec![AgentMessage::Custom(create_custom_message(
            custom_type.clone(),
            content.clone(),
            *display,
            details.clone(),
            iso8601_to_epoch_ms(timestamp),
        ))],
        SessionTreeEntry::Compaction {
            summary,
            tokens_before,
            timestamp,
            ..
        } => vec![AgentMessage::CompactionSummary(
            create_compaction_summary_message(
                summary.clone(),
                *tokens_before,
                iso8601_to_epoch_ms(timestamp),
            ),
        )],
        SessionTreeEntry::BranchSummary {
            from_id,
            summary,
            timestamp,
            ..
        } if !summary.is_empty() => {
            vec![AgentMessage::BranchSummary(create_branch_summary_message(
                summary.clone(),
                from_id.clone(),
                iso8601_to_epoch_ms(timestamp),
            ))]
        }
        _ => vec![],
    }
}

/// Build the full [`SessionContext`] from a root-first path (session.ts:125-135).
/// State is derived from the raw path; messages come from the compaction-collapsed
/// entries.
pub fn build_session_context(path_entries: &[SessionTreeEntry]) -> SessionContext {
    let (thinking_level, model, active_tool_names) = derive_session_context_state(path_entries);
    let context_entries = default_context_entry_transform(path_entries);
    let messages = context_entries
        .iter()
        .flat_map(session_entry_to_context_messages)
        .collect();
    SessionContext {
        messages,
        thinking_level,
        model,
        active_tool_names,
    }
}

// ===========================================================================
// Session (session.ts:137-338)
// ===========================================================================

/// Optional branch-summary payload appended by [`Session::move_to`]
/// (session.ts:318-337).
#[derive(Debug, Clone, Default)]
pub struct BranchSummaryInput {
    pub summary: String,
    pub details: Option<Value>,
    pub from_hook: Option<bool>,
}

/// Append-only session tree over a [`SessionStorage`] backend (session.ts:137).
///
/// Every `append*` method links the new entry to the current leaf
/// (`parentId = getLeafId()`) and lets the storage advance the leaf, giving the
/// append-only tree of spec §4.2. [`Session::move_to`] is the leaf-repointing
/// primitive underlying branch / fork / navigate (§4.4).
pub struct Session<St: SessionStorage> {
    storage: St,
    clock: Box<dyn Clock>,
}

impl<St: SessionStorage> Session<St> {
    /// Wrap `storage` with the wall-clock [`SystemClock`].
    pub fn new(storage: St) -> Self {
        Self {
            storage,
            clock: Box::new(SystemClock),
        }
    }

    /// Wrap `storage` with an injected [`Clock`] (deterministic timestamps).
    pub fn with_clock(storage: St, clock: Box<dyn Clock>) -> Self {
        Self { storage, clock }
    }

    /// Borrow the underlying storage.
    pub fn storage(&self) -> &St {
        &self.storage
    }

    /// Session metadata (session.ts:146-148).
    pub async fn get_metadata(&self) -> Result<St::Metadata, SessionError> {
        self.storage.get_metadata().await
    }

    /// Active leaf id (session.ts:154-156).
    pub async fn get_leaf_id(&self) -> Result<Option<String>, SessionError> {
        self.storage.get_leaf_id().await
    }

    /// Look up an entry by id (session.ts:158-160).
    pub async fn get_entry(&self, id: &str) -> Result<Option<SessionTreeEntry>, SessionError> {
        self.storage.get_entry(id).await
    }

    /// Every entry in the store (session.ts:162-164).
    pub async fn get_entries(&self) -> Result<Vec<SessionTreeEntry>, SessionError> {
        self.storage.get_entries().await
    }

    /// Root-first path from `from_id` (or the current leaf) — session.ts:166-169.
    pub async fn get_branch(
        &self,
        from_id: Option<String>,
    ) -> Result<Vec<SessionTreeEntry>, SessionError> {
        let leaf_id = match from_id {
            Some(id) => Some(id),
            None => self.storage.get_leaf_id().await?,
        };
        self.storage.get_path_to_root(leaf_id).await
    }

    /// Build the model context for the current branch (session.ts:175-177).
    pub async fn build_context(&self) -> Result<SessionContext, SessionError> {
        Ok(build_session_context(&self.get_branch(None).await?))
    }

    /// Label string for an entry, if any (session.ts:189-191).
    pub async fn get_label(&self, id: &str) -> Result<Option<String>, SessionError> {
        self.storage.get_label(id).await
    }

    /// Latest non-empty session name (session.ts:193-196).
    pub async fn get_session_name(&self) -> Result<Option<String>, SessionError> {
        let entries = self.storage.find_entries("session_info").await?;
        Ok(entries.iter().rev().find_map(|e| {
            if let SessionTreeEntry::SessionInfo { name, .. } = e {
                name.as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
            } else {
                None
            }
        }))
    }

    /// Shared append: link to the current leaf, stamp the clock, persist, return
    /// the new id (session.ts:198-201 with `parentId`/`timestamp` inlined per
    /// method).
    async fn next_ids(&self) -> Result<(String, Option<String>, String), SessionError> {
        let id = self.storage.create_entry_id().await?;
        let parent_id = self.storage.get_leaf_id().await?;
        let timestamp = self.clock.now_iso();
        Ok((id, parent_id, timestamp))
    }

    async fn append(&self, entry: SessionTreeEntry) -> Result<String, SessionError> {
        let id = entry_id(&entry).to_string();
        self.storage.append_entry(entry).await?;
        Ok(id)
    }

    /// Append a conversation message (session.ts:203-211).
    pub async fn append_message(&self, message: AgentMessage) -> Result<String, SessionError> {
        let (id, parent_id, timestamp) = self.next_ids().await?;
        self.append(SessionTreeEntry::Message {
            id,
            parent_id,
            timestamp,
            message,
        })
        .await
    }

    /// Append a thinking-level change (session.ts:213-221).
    pub async fn append_thinking_level_change(
        &self,
        thinking_level: String,
    ) -> Result<String, SessionError> {
        let (id, parent_id, timestamp) = self.next_ids().await?;
        self.append(SessionTreeEntry::ThinkingLevelChange {
            id,
            parent_id,
            timestamp,
            thinking_level,
        })
        .await
    }

    /// Append a model change (session.ts:223-232).
    pub async fn append_model_change(
        &self,
        provider: String,
        model_id: String,
    ) -> Result<String, SessionError> {
        let (id, parent_id, timestamp) = self.next_ids().await?;
        self.append(SessionTreeEntry::ModelChange {
            id,
            parent_id,
            timestamp,
            provider,
            model_id,
        })
        .await
    }

    /// Append an active-tools change (session.ts:234-242).
    pub async fn append_active_tools_change(
        &self,
        active_tool_names: Vec<String>,
    ) -> Result<String, SessionError> {
        let (id, parent_id, timestamp) = self.next_ids().await?;
        self.append(SessionTreeEntry::ActiveToolsChange {
            id,
            parent_id,
            timestamp,
            active_tool_names,
        })
        .await
    }

    /// Append a compaction record (session.ts:244-262).
    pub async fn append_compaction(
        &self,
        summary: String,
        first_kept_entry_id: String,
        tokens_before: i64,
        details: Option<Value>,
        from_hook: Option<bool>,
    ) -> Result<String, SessionError> {
        let (id, parent_id, timestamp) = self.next_ids().await?;
        self.append(SessionTreeEntry::Compaction {
            id,
            parent_id,
            timestamp,
            summary,
            first_kept_entry_id,
            tokens_before,
            details,
            from_hook,
        })
        .await
    }

    /// Append an application-defined entry (session.ts:264-273).
    pub async fn append_custom_entry(
        &self,
        custom_type: String,
        data: Option<Value>,
    ) -> Result<String, SessionError> {
        let (id, parent_id, timestamp) = self.next_ids().await?;
        self.append(SessionTreeEntry::Custom {
            id,
            parent_id,
            timestamp,
            custom_type,
            data,
        })
        .await
    }

    /// Append an application-defined message entry (session.ts:275-291).
    pub async fn append_custom_message_entry(
        &self,
        custom_type: String,
        content: pirust_ai::types::UserMessageContent,
        display: bool,
        details: Option<Value>,
    ) -> Result<String, SessionError> {
        let (id, parent_id, timestamp) = self.next_ids().await?;
        self.append(SessionTreeEntry::CustomMessage {
            id,
            parent_id,
            timestamp,
            custom_type,
            content,
            display,
            details,
        })
        .await
    }

    /// Append a label pointing at another entry (session.ts:293-305). Errors if
    /// the target does not exist.
    pub async fn append_label(
        &self,
        target_id: String,
        label: Option<String>,
    ) -> Result<String, SessionError> {
        if self.storage.get_entry(&target_id).await?.is_none() {
            return Err(SessionError::new(
                super::types::SessionErrorCode::NotFound,
                format!("Entry {target_id} not found"),
            ));
        }
        let (id, parent_id, timestamp) = self.next_ids().await?;
        self.append(SessionTreeEntry::Label {
            id,
            parent_id,
            timestamp,
            target_id,
            label,
        })
        .await
    }

    /// Append a session name, sanitizing embedded newlines (session.ts:307-316).
    pub async fn append_session_name(&self, name: &str) -> Result<String, SessionError> {
        let sanitized = collapse_newlines(name);
        let (id, parent_id, timestamp) = self.next_ids().await?;
        self.append(SessionTreeEntry::SessionInfo {
            id,
            parent_id,
            timestamp,
            name: Some(sanitized),
        })
        .await
    }

    /// Repoint the leaf to `entry_id` (branch / fork / navigate primitive), then
    /// optionally append a `branch_summary` whose parent is the new leaf
    /// (session.ts:318-337). Returns the branch-summary id, if one was written.
    pub async fn move_to(
        &self,
        entry_id: Option<String>,
        summary: Option<BranchSummaryInput>,
    ) -> Result<Option<String>, SessionError> {
        if let Some(ref eid) = entry_id {
            if self.storage.get_entry(eid).await?.is_none() {
                return Err(SessionError::new(
                    super::types::SessionErrorCode::NotFound,
                    format!("Entry {eid} not found"),
                ));
            }
        }
        self.storage.set_leaf_id(entry_id.clone()).await?;
        let Some(summary) = summary else {
            return Ok(None);
        };
        let id = self.storage.create_entry_id().await?;
        let timestamp = self.clock.now_iso();
        let from_id = entry_id.clone().unwrap_or_else(|| "root".to_string());
        let new_id = self
            .append(SessionTreeEntry::BranchSummary {
                id,
                parent_id: entry_id,
                timestamp,
                from_id,
                summary: summary.summary,
                details: summary.details,
                from_hook: summary.from_hook,
            })
            .await?;
        Ok(Some(new_id))
    }
}

/// Replace runs of `\r`/`\n` with a single space and trim (session.ts:308).
fn collapse_newlines(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut prev_was_newline = false;
    for ch in name.chars() {
        if ch == '\r' || ch == '\n' {
            if !prev_was_newline {
                out.push(' ');
            }
            prev_was_newline = true;
        } else {
            out.push(ch);
            prev_was_newline = false;
        }
    }
    out.trim().to_string()
}
