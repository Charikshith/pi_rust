//! v4 session data model — port of `packages/agent/src/harness/session/types.ts`.
//!
//! 0.84.2 replaced the v3 tree-of-entries JSONL with a **mutation-log** model:
//! a session file is a v4 header line followed by `seq`-numbered mutations of
//! kind `entry` / `record` / `lane` / `fact`. This module defines the data
//! types (entries, lane records, mutations, queries, stats, metadata) that
//! [`super::state`] replays and [`super::codec`] serializes.
//!
//! This is a NEW module alongside the v3 [`super::super::jsonl_storage`] tree
//! model; the v3 code stays intact until the v4 port fully lands.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use pirust_ai::types::{StopReason, Usage};

use crate::harness::messages::AgentMessage;
use crate::harness::types::{SessionError, SessionErrorCode};

// ===========================================================================
// Scalars
// ===========================================================================

/// `JsonValue` (session/types.ts:6).
pub type JsonValue = Value;

/// `SessionStopReason = Exclude<StopReason, "pending"> | "deferred"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionStopReason {
    #[serde(rename = "stop")]
    Stop,
    #[serde(rename = "length")]
    Length,
    #[serde(rename = "toolUse")]
    ToolUse,
    #[serde(rename = "maxTokens")]
    MaxTokens,
    #[serde(rename = "aborted")]
    Aborted,
    #[serde(rename = "error")]
    Error,
    #[serde(rename = "deferred")]
    Deferred,
}

impl SessionStopReason {
    /// From a `StopReason` (never `pending` / `maxTokens` — those are not
    /// modeled in this crate's `StopReason`).
    pub fn from_stop_reason(reason: StopReason) -> Option<Self> {
        Some(match reason {
            StopReason::Stop => Self::Stop,
            StopReason::Length => Self::Length,
            StopReason::ToolUse => Self::ToolUse,
            StopReason::Aborted => Self::Aborted,
            StopReason::Error => Self::Error,
        })
    }
}

// ===========================================================================
// Entries
// ===========================================================================

/// Shared entry base (session/types.ts:21-27): `type, id, seq, parentId,
/// timestamp`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EntryBase {
    pub id: String,
    /// Shared sequence; read-side, storage-assigned.
    pub seq: i64,
    /// Storage-assigned: the appending lane's leaf.
    #[serde(rename = "parentId")]
    pub parent_id: Option<String>,
    /// Unix ms, storage-assigned.
    pub timestamp: i64,
}

/// A conversation message entry (session/types.ts:29-32).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessageEntry {
    pub id: String,
    pub seq: i64,
    #[serde(rename = "parentId")]
    pub parent_id: Option<String>,
    pub timestamp: i64,
    pub message: AgentMessage,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminate: Option<bool>,
}

/// Model change entry (session/types.ts:34-37).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelChangeEntry {
    pub id: String,
    pub seq: i64,
    #[serde(rename = "parentId")]
    pub parent_id: Option<String>,
    pub timestamp: i64,
    pub provider: String,
    #[serde(rename = "modelId")]
    pub model_id: String,
}

/// Thinking-level change entry (session/types.ts:39-41).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThinkingLevelEntry {
    pub id: String,
    pub seq: i64,
    #[serde(rename = "parentId")]
    pub parent_id: Option<String>,
    pub timestamp: i64,
    #[serde(rename = "thinkingLevel")]
    pub thinking_level: String,
}

/// Active-tools change entry (session/types.ts:43-45).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActiveToolsEntry {
    pub id: String,
    pub seq: i64,
    #[serde(rename = "parentId")]
    pub parent_id: Option<String>,
    pub timestamp: i64,
    #[serde(rename = "activeToolNames")]
    pub active_tool_names: Vec<String>,
}

/// Compaction entry (session/types.ts:47-53).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompactionEntry {
    pub id: String,
    pub seq: i64,
    #[serde(rename = "parentId")]
    pub parent_id: Option<String>,
    pub timestamp: i64,
    pub summary: String,
    #[serde(rename = "retainedTail")]
    pub retained_tail: Vec<AgentMessage>,
    #[serde(rename = "tokensBefore")]
    pub tokens_before: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

/// Branch summary entry (session/types.ts:55-60).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BranchSummaryEntry {
    pub id: String,
    pub seq: i64,
    #[serde(rename = "parentId")]
    pub parent_id: Option<String>,
    pub timestamp: i64,
    #[serde(rename = "fromId")]
    pub from_id: String,
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

/// Custom application entry (session/types.ts:62-65).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CustomEntry {
    pub id: String,
    pub seq: i64,
    #[serde(rename = "parentId")]
    pub parent_id: Option<String>,
    pub timestamp: i64,
    #[serde(rename = "customType")]
    pub custom_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// Any session entry (session/types.ts:67-69). Discriminated on `type` (the
/// TS union's discriminant) so the right variant is picked by the wire value,
/// never by declaration order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Entry {
    #[serde(rename = "message")]
    Message(MessageEntry),
    #[serde(rename = "model_change")]
    ModelChange(ModelChangeEntry),
    #[serde(rename = "thinking_level_change")]
    ThinkingLevel(ThinkingLevelEntry),
    #[serde(rename = "active_tools_change")]
    ActiveTools(ActiveToolsEntry),
    #[serde(rename = "compaction")]
    Compaction(CompactionEntry),
    #[serde(rename = "branch_summary")]
    BranchSummary(BranchSummaryEntry),
    #[serde(rename = "custom")]
    Custom(CustomEntry),
}

impl Entry {
    /// `entry.type` discriminant.
    pub fn entry_type(&self) -> &str {
        match self {
            Entry::Message(_) => "message",
            Entry::ModelChange(_) => "model_change",
            Entry::ThinkingLevel(_) => "thinking_level_change",
            Entry::ActiveTools(_) => "active_tools_change",
            Entry::Compaction(_) => "compaction",
            Entry::BranchSummary(_) => "branch_summary",
            Entry::Custom(_) => "custom",
        }
    }

    pub fn id(&self) -> &str {
        match self {
            Entry::Message(e) => &e.id,
            Entry::ModelChange(e) => &e.id,
            Entry::ThinkingLevel(e) => &e.id,
            Entry::ActiveTools(e) => &e.id,
            Entry::Compaction(e) => &e.id,
            Entry::BranchSummary(e) => &e.id,
            Entry::Custom(e) => &e.id,
        }
    }

    pub fn seq(&self) -> i64 {
        match self {
            Entry::Message(e) => e.seq,
            Entry::ModelChange(e) => e.seq,
            Entry::ThinkingLevel(e) => e.seq,
            Entry::ActiveTools(e) => e.seq,
            Entry::Compaction(e) => e.seq,
            Entry::BranchSummary(e) => e.seq,
            Entry::Custom(e) => e.seq,
        }
    }

    /// The entry's `parentId`.
    pub fn parent_id(&self) -> Option<&String> {
        match self {
            Entry::Message(e) => e.parent_id.as_ref(),
            Entry::ModelChange(e) => e.parent_id.as_ref(),
            Entry::ThinkingLevel(e) => e.parent_id.as_ref(),
            Entry::ActiveTools(e) => e.parent_id.as_ref(),
            Entry::Compaction(e) => e.parent_id.as_ref(),
            Entry::BranchSummary(e) => e.parent_id.as_ref(),
            Entry::Custom(e) => e.parent_id.as_ref(),
        }
    }

    /// `custom` type name, when this is a custom entry.
    pub fn custom_type(&self) -> Option<&str> {
        match self {
            Entry::Custom(e) => Some(&e.custom_type),
            _ => None,
        }
    }
}

/// `ProvisionedEntry` — an entry minus the storage-assigned `parentId`/`seq`/
/// `timestamp` (session/types.ts:71-73).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ProvisionedEntry {
    Message(ProvisionedMessageEntry),
    ModelChange(ProvisionedModelChangeEntry),
    ThinkingLevel(ProvisionedThinkingLevelEntry),
    ActiveTools(ProvisionedActiveToolsEntry),
    Compaction(ProvisionedCompactionEntry),
    BranchSummary(ProvisionedBranchSummaryEntry),
    Custom(ProvisionedCustomEntry),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProvisionedMessageEntry {
    pub id: String,
    pub message: AgentMessage,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminate: Option<bool>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProvisionedModelChangeEntry {
    pub id: String,
    pub provider: String,
    #[serde(rename = "modelId")]
    pub model_id: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProvisionedThinkingLevelEntry {
    pub id: String,
    #[serde(rename = "thinkingLevel")]
    pub thinking_level: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProvisionedActiveToolsEntry {
    pub id: String,
    #[serde(rename = "activeToolNames")]
    pub active_tool_names: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProvisionedCompactionEntry {
    pub id: String,
    pub summary: String,
    #[serde(rename = "retainedTail")]
    pub retained_tail: Vec<AgentMessage>,
    #[serde(rename = "tokensBefore")]
    pub tokens_before: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProvisionedBranchSummaryEntry {
    pub id: String,
    #[serde(rename = "fromId")]
    pub from_id: String,
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProvisionedCustomEntry {
    pub id: String,
    #[serde(rename = "customType")]
    pub custom_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

// ===========================================================================
// Lane records
// ===========================================================================

/// Record base (session/types.ts:75-79).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecordBase {
    pub id: String,
    pub seq: i64,
    pub lane: String,
    pub timestamp: i64,
}

/// Operation intent (session/types.ts:85-113).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum OperationIntent {
    #[serde(rename = "run")]
    Run {
        #[serde(rename = "originalPrompt")]
        original_prompt: Vec<AgentMessage>,
        #[serde(rename = "initialMessages")]
        initial_messages: Vec<ProvisionedEntry>,
        #[serde(skip_serializing_if = "Option::is_none")]
        system_prompt_override: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        resume_data: Option<serde_json::Map<String, JsonValue>>,
    },
    #[serde(rename = "compaction")]
    Compaction {
        #[serde(skip_serializing_if = "Option::is_none")]
        custom_instructions: Option<String>,
        #[serde(rename = "resultEntryId")]
        result_entry_id: String,
    },
    #[serde(rename = "navigation")]
    Navigation {
        #[serde(rename = "targetId")]
        target_id: Option<String>,
        summarize: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        custom_instructions: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        summary_entry_id: Option<String>,
    },
}

/// Operation-started record (session/types.ts:81-114).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OperationStartedRecord {
    pub id: String,
    pub seq: i64,
    pub lane: String,
    pub timestamp: i64,
    #[serde(rename = "sourceLeafId")]
    pub source_leaf_id: Option<String>,
    pub intent: OperationIntent,
}

/// Abort-requested record (session/types.ts:116-119).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AbortRequestedRecord {
    pub id: String,
    pub seq: i64,
    pub lane: String,
    pub timestamp: i64,
    #[serde(rename = "runId")]
    pub run_id: String,
}

/// Operation-finished record (session/types.ts:121-125).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OperationFinishedRecord {
    pub id: String,
    pub seq: i64,
    pub lane: String,
    pub timestamp: i64,
    #[serde(rename = "runId")]
    pub run_id: String,
    pub outcome: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<OperationError>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OperationError {
    pub code: String,
    pub message: String,
}

/// `CompactionReason` (session/types.ts:127).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CompactionReason {
    #[serde(rename = "manual")]
    Manual,
    #[serde(rename = "threshold")]
    Threshold,
    #[serde(rename = "overflow")]
    Overflow,
}

/// Step-attempt record (session/types.ts:129-145).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepAttemptRecord {
    pub id: String,
    pub seq: i64,
    pub lane: String,
    pub timestamp: i64,
    #[serde(rename = "runId")]
    pub run_id: String,
    pub step: String,
    pub attempt: i64,
    #[serde(rename = "resultEntryId")]
    pub result_entry_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compaction_reason: Option<CompactionReason>,
}

/// Tool-started record (session/types.ts:147-156).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolStartedRecord {
    pub id: String,
    pub seq: i64,
    pub lane: String,
    pub timestamp: i64,
    #[serde(rename = "runId")]
    pub run_id: String,
    #[serde(rename = "assistantEntryId")]
    pub assistant_entry_id: String,
    #[serde(rename = "toolIndex")]
    pub tool_index: i64,
    #[serde(rename = "toolCallId")]
    pub tool_call_id: String,
    #[serde(rename = "toolName")]
    pub tool_name: String,
    #[serde(rename = "effectiveArgs")]
    pub effective_args: serde_json::Map<String, Value>,
    #[serde(rename = "resultEntryId")]
    pub result_entry_id: String,
    pub replay: String,
}

/// Queue-enqueued record (session/types.ts:158-168).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueueEnqueuedRecord {
    pub id: String,
    pub seq: i64,
    pub lane: String,
    pub timestamp: i64,
    pub queue: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    pub target: ProvisionedEntry,
}

/// Queue-cancelled record (session/types.ts:170-173).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueueCancelledRecord {
    pub id: String,
    pub seq: i64,
    pub lane: String,
    pub timestamp: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(rename = "entryId")]
    pub entry_id: String,
}

/// Write-deferred record (session/types.ts:175-178).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WriteDeferredRecord {
    pub id: String,
    pub seq: i64,
    pub lane: String,
    pub timestamp: i64,
    #[serde(rename = "runId")]
    pub run_id: String,
    pub target: ProvisionedEntry,
}

/// Usage-record cause (session/types.ts:180-188).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageRecordCause {
    pub cause: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entry_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<SessionStopReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<JsonValue>,
}

/// Usage record (session/types.ts:180-189).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageRecord {
    pub id: String,
    pub seq: i64,
    pub lane: String,
    pub timestamp: i64,
    pub usage: Usage,
    #[serde(flatten)]
    pub cause: UsageRecordCause,
}

/// Any lane record (session/types.ts:191-197). Discriminated on `type` — the
/// TS union's discriminant — so the right variant is picked regardless of
/// declaration order (untagged would let an early short variant swallow a
/// later one's extra fields, as the oracle caught).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum LaneRecord {
    #[serde(rename = "operation_started")]
    OperationStarted(OperationStartedRecord),
    #[serde(rename = "abort_requested")]
    AbortRequested(AbortRequestedRecord),
    #[serde(rename = "operation_finished")]
    OperationFinished(OperationFinishedRecord),
    #[serde(rename = "step_attempt")]
    StepAttempt(StepAttemptRecord),
    #[serde(rename = "tool_started")]
    ToolStarted(ToolStartedRecord),
    #[serde(rename = "queue_enqueued")]
    QueueEnqueued(QueueEnqueuedRecord),
    #[serde(rename = "queue_cancelled")]
    QueueCancelled(QueueCancelledRecord),
    #[serde(rename = "write_deferred")]
    WriteDeferred(WriteDeferredRecord),
    #[serde(rename = "usage")]
    Usage(UsageRecord),
}

impl LaneRecord {
    pub fn id(&self) -> &str {
        match self {
            LaneRecord::OperationStarted(r) => &r.id,
            LaneRecord::AbortRequested(r) => &r.id,
            LaneRecord::OperationFinished(r) => &r.id,
            LaneRecord::StepAttempt(r) => &r.id,
            LaneRecord::ToolStarted(r) => &r.id,
            LaneRecord::QueueEnqueued(r) => &r.id,
            LaneRecord::QueueCancelled(r) => &r.id,
            LaneRecord::WriteDeferred(r) => &r.id,
            LaneRecord::Usage(r) => &r.id,
        }
    }

    pub fn seq(&self) -> i64 {
        match self {
            LaneRecord::OperationStarted(r) => r.seq,
            LaneRecord::AbortRequested(r) => r.seq,
            LaneRecord::OperationFinished(r) => r.seq,
            LaneRecord::StepAttempt(r) => r.seq,
            LaneRecord::ToolStarted(r) => r.seq,
            LaneRecord::QueueEnqueued(r) => r.seq,
            LaneRecord::QueueCancelled(r) => r.seq,
            LaneRecord::WriteDeferred(r) => r.seq,
            LaneRecord::Usage(r) => r.seq,
        }
    }

    pub fn lane(&self) -> &str {
        match self {
            LaneRecord::OperationStarted(r) => &r.lane,
            LaneRecord::AbortRequested(r) => &r.lane,
            LaneRecord::OperationFinished(r) => &r.lane,
            LaneRecord::StepAttempt(r) => &r.lane,
            LaneRecord::ToolStarted(r) => &r.lane,
            LaneRecord::QueueEnqueued(r) => &r.lane,
            LaneRecord::QueueCancelled(r) => &r.lane,
            LaneRecord::WriteDeferred(r) => &r.lane,
            LaneRecord::Usage(r) => &r.lane,
        }
    }

    /// `record.type` discriminant.
    pub fn record_type(&self) -> &str {
        match self {
            LaneRecord::OperationStarted(_) => "operation_started",
            LaneRecord::AbortRequested(_) => "abort_requested",
            LaneRecord::OperationFinished(_) => "operation_finished",
            LaneRecord::StepAttempt(_) => "step_attempt",
            LaneRecord::ToolStarted(_) => "tool_started",
            LaneRecord::QueueEnqueued(_) => "queue_enqueued",
            LaneRecord::QueueCancelled(_) => "queue_cancelled",
            LaneRecord::WriteDeferred(_) => "write_deferred",
            LaneRecord::Usage(_) => "usage",
        }
    }
}

// ===========================================================================
// Mutations
// ===========================================================================

/// A session mutation (session/state.ts:8-14).
#[derive(Debug, Clone, PartialEq)]
pub enum SessionMutation {
    Entry {
        lane: Option<String>,
        entry: Entry,
    },
    Record {
        record: LaneRecord,
    },
    Lane {
        seq: i64,
        lane: String,
        leaf_id: Option<String>,
    },
    FactName {
        seq: i64,
        name: Option<String>,
    },
    FactLabel {
        seq: i64,
        target_id: String,
        label: Option<String>,
    },
}

impl SessionMutation {
    /// The mutation's `seq` (all four kinds carry one; `entry`/`record` from the
    /// inner value).
    pub fn seq(&self) -> i64 {
        match self {
            SessionMutation::Entry { entry, .. } => entry.seq(),
            SessionMutation::Record { record } => record.seq(),
            SessionMutation::Lane { seq, .. }
            | SessionMutation::FactName { seq, .. }
            | SessionMutation::FactLabel { seq, .. } => *seq,
        }
    }
}

impl ProvisionedEntry {
    /// The provisioned entry's `id`.
    pub fn id(&self) -> &str {
        match self {
            ProvisionedEntry::Message(e) => &e.id,
            ProvisionedEntry::ModelChange(e) => &e.id,
            ProvisionedEntry::ThinkingLevel(e) => &e.id,
            ProvisionedEntry::ActiveTools(e) => &e.id,
            ProvisionedEntry::Compaction(e) => &e.id,
            ProvisionedEntry::BranchSummary(e) => &e.id,
            ProvisionedEntry::Custom(e) => &e.id,
        }
    }

    /// Promote a provisioned entry into a full [`Entry`] by assigning the
    /// storage-owned fields (parentId/seq/timestamp) — storage.ts
    /// `appendEntry`'s `{ ...structuredClone(newEntry), parentId, seq,
    /// timestamp }`.
    pub fn promote(self, parent_id: Option<String>, seq: i64, timestamp: i64) -> Entry {
        match self {
            ProvisionedEntry::Message(e) => Entry::Message(MessageEntry {
                id: e.id,
                message: e.message,
                terminate: e.terminate,
                seq,
                parent_id,
                timestamp,
            }),
            ProvisionedEntry::ModelChange(e) => Entry::ModelChange(ModelChangeEntry {
                id: e.id,
                provider: e.provider,
                model_id: e.model_id,
                seq,
                parent_id,
                timestamp,
            }),
            ProvisionedEntry::ThinkingLevel(e) => Entry::ThinkingLevel(ThinkingLevelEntry {
                id: e.id,
                thinking_level: e.thinking_level,
                seq,
                parent_id,
                timestamp,
            }),
            ProvisionedEntry::ActiveTools(e) => Entry::ActiveTools(ActiveToolsEntry {
                id: e.id,
                active_tool_names: e.active_tool_names,
                seq,
                parent_id,
                timestamp,
            }),
            ProvisionedEntry::Compaction(e) => Entry::Compaction(CompactionEntry {
                id: e.id,
                summary: e.summary,
                retained_tail: e.retained_tail,
                tokens_before: e.tokens_before,
                details: e.details,
                usage: e.usage,
                seq,
                parent_id,
                timestamp,
            }),
            ProvisionedEntry::BranchSummary(e) => Entry::BranchSummary(BranchSummaryEntry {
                id: e.id,
                from_id: e.from_id,
                summary: e.summary,
                details: e.details,
                usage: e.usage,
                seq,
                parent_id,
                timestamp,
            }),
            ProvisionedEntry::Custom(e) => Entry::Custom(CustomEntry {
                id: e.id,
                custom_type: e.custom_type,
                data: e.data,
                seq,
                parent_id,
                timestamp,
            }),
        }
    }
}

impl NewRecord {
    /// The record's `id`.
    pub fn id(&self) -> &str {
        match self {
            NewRecord::OperationStarted(r) => &r.id,
            NewRecord::AbortRequested(r) => &r.id,
            NewRecord::OperationFinished(r) => &r.id,
            NewRecord::StepAttempt(r) => &r.id,
            NewRecord::ToolStarted(r) => &r.id,
            NewRecord::QueueEnqueued(r) => &r.id,
            NewRecord::QueueCancelled(r) => &r.id,
            NewRecord::WriteDeferred(r) => &r.id,
            NewRecord::Usage(r) => &r.id,
        }
    }

    /// The record's `lane`.
    pub fn lane(&self) -> &str {
        match self {
            NewRecord::OperationStarted(r) => &r.lane,
            NewRecord::AbortRequested(r) => &r.lane,
            NewRecord::OperationFinished(r) => &r.lane,
            NewRecord::StepAttempt(r) => &r.lane,
            NewRecord::ToolStarted(r) => &r.lane,
            NewRecord::QueueEnqueued(r) => &r.lane,
            NewRecord::QueueCancelled(r) => &r.lane,
            NewRecord::WriteDeferred(r) => &r.lane,
            NewRecord::Usage(r) => &r.lane,
        }
    }

    /// The record's `type` discriminant.
    pub fn record_type(&self) -> &str {
        match self {
            NewRecord::OperationStarted(_) => "operation_started",
            NewRecord::AbortRequested(_) => "abort_requested",
            NewRecord::OperationFinished(_) => "operation_finished",
            NewRecord::StepAttempt(_) => "step_attempt",
            NewRecord::ToolStarted(_) => "tool_started",
            NewRecord::QueueEnqueued(_) => "queue_enqueued",
            NewRecord::QueueCancelled(_) => "queue_cancelled",
            NewRecord::WriteDeferred(_) => "write_deferred",
            NewRecord::Usage(_) => "usage",
        }
    }

    /// Promote a new record into a full [`LaneRecord`] by assigning the
    /// storage-owned fields (seq/timestamp) — storage.ts `appendRecord`'s
    /// `{ ...structuredClone(newRecord), seq, timestamp }`.
    pub fn promote(self, seq: i64, timestamp: i64) -> LaneRecord {
        match self {
            NewRecord::OperationStarted(r) => {
                LaneRecord::OperationStarted(OperationStartedRecord {
                    id: r.id,
                    lane: r.lane,
                    source_leaf_id: r.source_leaf_id,
                    intent: r.intent,
                    seq,
                    timestamp,
                })
            }
            NewRecord::AbortRequested(r) => LaneRecord::AbortRequested(AbortRequestedRecord {
                id: r.id,
                lane: r.lane,
                run_id: r.run_id,
                seq,
                timestamp,
            }),
            NewRecord::OperationFinished(r) => {
                LaneRecord::OperationFinished(OperationFinishedRecord {
                    id: r.id,
                    lane: r.lane,
                    run_id: r.run_id,
                    outcome: r.outcome,
                    error: r.error,
                    seq,
                    timestamp,
                })
            }
            NewRecord::StepAttempt(r) => LaneRecord::StepAttempt(StepAttemptRecord {
                id: r.id,
                lane: r.lane,
                run_id: r.run_id,
                step: r.step,
                attempt: r.attempt,
                result_entry_id: r.result_entry_id,
                compaction_reason: r.compaction_reason,
                seq,
                timestamp,
            }),
            NewRecord::ToolStarted(r) => LaneRecord::ToolStarted(ToolStartedRecord {
                id: r.id,
                lane: r.lane,
                run_id: r.run_id,
                assistant_entry_id: r.assistant_entry_id,
                tool_index: r.tool_index,
                tool_call_id: r.tool_call_id,
                tool_name: r.tool_name,
                effective_args: r.effective_args,
                result_entry_id: r.result_entry_id,
                replay: r.replay,
                seq,
                timestamp,
            }),
            NewRecord::QueueEnqueued(r) => LaneRecord::QueueEnqueued(QueueEnqueuedRecord {
                id: r.id,
                lane: r.lane,
                queue: r.queue,
                run_id: r.run_id,
                target: r.target,
                seq,
                timestamp,
            }),
            NewRecord::QueueCancelled(r) => LaneRecord::QueueCancelled(QueueCancelledRecord {
                id: r.id,
                lane: r.lane,
                run_id: r.run_id,
                entry_id: r.entry_id,
                seq,
                timestamp,
            }),
            NewRecord::WriteDeferred(r) => LaneRecord::WriteDeferred(WriteDeferredRecord {
                id: r.id,
                lane: r.lane,
                run_id: r.run_id,
                target: r.target,
                seq,
                timestamp,
            }),
            NewRecord::Usage(r) => LaneRecord::Usage(UsageRecord {
                id: r.id,
                lane: r.lane,
                usage: r.usage,
                cause: r.cause,
                seq,
                timestamp,
            }),
        }
    }
}

/// `NewRecord` — a record minus storage-assigned `seq`/`timestamp`.
#[derive(Debug, Clone, PartialEq)]
pub enum NewRecord {
    OperationStarted(NewOperationStartedRecord),
    AbortRequested(NewAbortRequestedRecord),
    OperationFinished(NewOperationFinishedRecord),
    StepAttempt(NewStepAttemptRecord),
    ToolStarted(NewToolStartedRecord),
    QueueEnqueued(NewQueueEnqueuedRecord),
    QueueCancelled(NewQueueCancelledRecord),
    WriteDeferred(NewWriteDeferredRecord),
    Usage(NewUsageRecord),
}

#[derive(Debug, Clone, PartialEq)]
pub struct NewOperationStartedRecord {
    pub id: String,
    pub lane: String,
    pub source_leaf_id: Option<String>,
    pub intent: OperationIntent,
}
#[derive(Debug, Clone, PartialEq)]
pub struct NewAbortRequestedRecord {
    pub id: String,
    pub lane: String,
    pub run_id: String,
}
#[derive(Debug, Clone, PartialEq)]
pub struct NewOperationFinishedRecord {
    pub id: String,
    pub lane: String,
    pub run_id: String,
    pub outcome: String,
    pub error: Option<OperationError>,
}
#[derive(Debug, Clone, PartialEq)]
pub struct NewStepAttemptRecord {
    pub id: String,
    pub lane: String,
    pub run_id: String,
    pub step: String,
    pub attempt: i64,
    pub result_entry_id: String,
    pub compaction_reason: Option<CompactionReason>,
}
#[derive(Debug, Clone, PartialEq)]
pub struct NewToolStartedRecord {
    pub id: String,
    pub lane: String,
    pub run_id: String,
    pub assistant_entry_id: String,
    pub tool_index: i64,
    pub tool_call_id: String,
    pub tool_name: String,
    pub effective_args: serde_json::Map<String, Value>,
    pub result_entry_id: String,
    pub replay: String,
}
#[derive(Debug, Clone, PartialEq)]
pub struct NewQueueEnqueuedRecord {
    pub id: String,
    pub lane: String,
    pub queue: String,
    pub run_id: Option<String>,
    pub target: ProvisionedEntry,
}
#[derive(Debug, Clone, PartialEq)]
pub struct NewQueueCancelledRecord {
    pub id: String,
    pub lane: String,
    pub run_id: Option<String>,
    pub entry_id: String,
}
#[derive(Debug, Clone, PartialEq)]
pub struct NewWriteDeferredRecord {
    pub id: String,
    pub lane: String,
    pub run_id: String,
    pub target: ProvisionedEntry,
}
#[derive(Debug, Clone, PartialEq)]
pub struct NewUsageRecord {
    pub id: String,
    pub lane: String,
    pub usage: Usage,
    pub cause: UsageRecordCause,
}

// ===========================================================================
// Queries / views / stats
// ===========================================================================

/// `EntryOrder` (session/types.ts:199).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EntryOrder {
    #[default]
    NewestFirst,
    OldestFirst,
}

/// `EntryCursor` (session/types.ts:201-203).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntryCursor {
    pub after_seq: i64,
}

/// `EntryQuery` (session/types.ts:205-212).
#[derive(Debug, Clone, Default)]
pub struct EntryQuery {
    pub entry_type: Option<String>,
    pub custom_type: Option<String>,
    pub order: Option<EntryOrder>,
    pub limit: Option<usize>,
    pub cursor: Option<EntryCursor>,
}

/// `BranchBounds` (session/types.ts:214-219).
#[derive(Debug, Clone, Default)]
pub struct BranchBounds {
    pub start: Option<String>,
    pub stop_at_type: Option<String>,
    pub stop_at_id: Option<String>,
}

/// `RecordQuery` (session/types.ts:221-237).
#[derive(Debug, Clone, Default)]
pub struct RecordQuery {
    pub lane: Option<String>,
    pub record_type: Option<String>,
    pub run_id: Option<String>,
    pub operation_kind: Option<String>,
    pub after_seq: Option<i64>,
    pub order: Option<EntryOrder>,
    pub limit: Option<usize>,
}

/// `SessionStats` (session/types.ts:262-267).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SessionStats {
    pub message_count: i64,
    pub cached_tokens: i64,
    pub uncached_tokens: i64,
    pub total_tokens: i64,
    pub cost_total: f64,
}

/// `LanePointer` (session/types.ts:269-272).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LanePointer {
    pub lane: String,
    pub leaf_id: Option<String>,
}

/// `LogItem` (session/types.ts:274-279).
#[derive(Debug, Clone, PartialEq)]
pub enum LogItem {
    Entry {
        seq: i64,
        entry: Entry,
    },
    Record {
        seq: i64,
        record: LaneRecord,
    },
    Lane {
        seq: i64,
        lane: String,
        leaf_id: Option<String>,
    },
    FactName {
        seq: i64,
        name: Option<String>,
    },
    FactLabel {
        seq: i64,
        target_id: String,
        label: Option<String>,
    },
}

/// `LogOptions` (session/types.ts:281-284).
#[derive(Debug, Clone, Copy, Default)]
pub struct LogOptions {
    pub after_seq: Option<i64>,
    pub limit: Option<usize>,
}

/// `SessionMetadata` (session/types.ts:251-255).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionMetadata {
    pub id: String,
    #[serde(rename = "createdAt")]
    pub created_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
}

/// `ForkOptions` (session/types.ts:349).
#[derive(Debug, Clone, PartialEq)]
pub enum ForkOptions {
    Branch {
        entry_id: Option<String>,
        position: Option<ForkPosition>,
    },
    Tree,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForkPosition {
    Before,
    At,
}

/// `JsonlSessionMetadata` (jsonl/types.ts:13-24).
#[derive(Debug, Clone, PartialEq)]
pub struct JsonlSessionMetadata {
    pub id: String,
    pub created_at: i64,
    pub cwd: String,
    pub path: String,
    pub modified_at: i64,
    pub source_format: u8,
    pub parent_session_id: Option<String>,
    pub legacy_parent_session_path: Option<String>,
    pub metadata: Option<serde_json::Map<String, Value>>,
}

/// `JsonlSessionRepoOptions` (jsonl/types.ts:26-31).
#[derive(Debug, Clone)]
pub struct JsonlSessionRepoOptions {
    /// Root containing coding-agent-compatible cwd-encoded session directories.
    pub sessions_root: String,
}

/// `JsonlSessionCreateOptions` (jsonl/types.ts:33-36) — `SessionCreateOptions` +
/// `cwd` + opaque `metadata`.
#[derive(Debug, Clone, Default)]
pub struct JsonlSessionCreateOptions {
    pub id: Option<String>,
    pub parent_session_id: Option<String>,
    pub cwd: String,
    pub metadata: Option<serde_json::Map<String, Value>>,
}

/// `JsonlSessionListOptions` (jsonl/types.ts:38-40).
#[derive(Debug, Clone, Default)]
pub struct JsonlSessionListOptions {
    pub cwd: Option<String>,
}

/// Fork options for the JSON repo — TS `ForkOptions & JsonlSessionCreateOptions`
/// (jsonl/repo.ts:142-143): the fork scope plus create fields (id, parent),
/// flattened into one object.
#[derive(Debug, Clone, Default)]
pub struct JsonlSessionForkOptions {
    pub fork: Option<ForkOptions>,
    pub id: Option<String>,
    pub parent_session_id: Option<String>,
    pub cwd: String,
    pub metadata: Option<serde_json::Map<String, Value>>,
}

/// Fork options for the in-memory repo — TS `ForkOptions & SessionCreateOptions`
/// (memory.ts:160): fork scope + id + parentSessionId, flattened.
#[derive(Debug, Clone, Default)]
pub struct MemoryForkOptions {
    pub fork: Option<ForkOptions>,
    pub id: Option<String>,
    pub parent_session_id: Option<String>,
}

/// Error helper for the v4 session layer (types.ts:360-371).
pub fn v4_error(code: SessionErrorCode, message: impl Into<String>) -> SessionError {
    SessionError::new(code, message)
}

// ===========================================================================
// Storage / repo traits (session/types.ts:290-378)
// ===========================================================================

/// `SessionStorage<TMetadata>` (session/types.ts:290-327) — the read/write
/// contract every v4 session backend implements. Mirrors the v3 trait in
/// `harness::types` but with the v4 mutation-log vocabulary (lanes, records,
/// log, open operations).
pub trait SessionStorage: Send + Sync {
    /// Metadata type for this backend (TS `TMetadata extends SessionMetadata`).
    type Metadata: Send + Sync;

    /// `getMetadata`.
    fn get_metadata(&self) -> Result<Self::Metadata, SessionError>;

    // Lanes
    fn get_lanes(&self) -> Result<Vec<LanePointer>, SessionError>;
    fn create_lane(&self, lane: &str, at: Option<&str>) -> Result<(), SessionError>;
    fn move_lane(&self, lane: &str, to: Option<&str>) -> Result<(), SessionError>;

    // Entries and Records
    fn append_entry(&self, entry: &ProvisionedEntry, lane: &str) -> Result<Entry, SessionError>;
    fn append_record(&self, record: &NewRecord) -> Result<LaneRecord, SessionError>;

    // Reads
    fn get_entry(&self, id: &str) -> Result<Option<Entry>, SessionError>;
    fn find_entries(&self, query: &EntryQuery) -> Result<Vec<Entry>, SessionError>;
    /// `start` is mandatory here (as opposed to the tree's findEntriesOnBranch).
    fn find_entries_on_branch(
        &self,
        query: &EntryQuery,
        bounds: &BranchBounds,
        start: &str,
    ) -> Result<Vec<Entry>, SessionError>;
    fn find_records(&self, query: &RecordQuery) -> Result<Vec<LaneRecord>, SessionError>;
    fn find_open_operations(
        &self,
        lane: &str,
        options: Option<usize>,
    ) -> Result<Vec<OperationStartedRecord>, SessionError>;
    fn get_log(&self, options: &LogOptions) -> Result<Vec<LogItem>, SessionError>;

    // Global facts
    fn get_name(&self) -> Result<Option<String>, SessionError>;
    fn set_name(&self, name: Option<&str>) -> Result<(), SessionError>;
    fn get_label(&self, id: &str) -> Result<Option<String>, SessionError>;
    fn set_label(&self, id: &str, label: Option<&str>) -> Result<(), SessionError>;
    fn get_stats(&self) -> Result<SessionStats, SessionError>;
}

/// `SessionCreateOptions` (session/types.ts:333-336).
#[derive(Debug, Clone, Default)]
pub struct SessionCreateOptions {
    pub id: Option<String>,
    pub parent_session_id: Option<String>,
}

/// `SessionRepo<TMetadata, TCreateOptions, TListOptions>`
/// (session/types.ts:361-378).
pub trait SessionRepo: Send + Sync {
    /// Concrete session handle produced by this repo.
    type Session: Send + Sync;
    /// Metadata describing a stored session.
    type Metadata: Send + Sync;
    /// Backend-specific create options (TS `TCreateOptions`).
    type CreateOptions: Send + Sync;
    /// Backend-specific list filter (TS `TListOptions`).
    type ListOptions: Send + Sync;
    /// Fork options — TS `ForkOptions & TCreateOptions` (flattened).
    type ForkOptions: Send + Sync;

    fn create(&self, options: Self::CreateOptions) -> Result<Self::Session, SessionError>;
    fn open(&self, metadata: Self::Metadata) -> Result<Self::Session, SessionError>;
    fn list(&self, options: Option<Self::ListOptions>)
        -> Result<Vec<Self::Metadata>, SessionError>;
    fn delete(&self, metadata: Self::Metadata) -> Result<(), SessionError>;
    fn fork(
        &self,
        source: Self::Metadata,
        options: Self::ForkOptions,
    ) -> Result<Self::Session, SessionError>;
}
