//! Conversions between `pirust-agent-core`/`pirust-ai`'s runtime types and
//! this crate's wire-protocol schema types (`crate::protocol::schemas`).
//!
//! **This whole module is a pirust-side addition, not a port** (plan.md's
//! own framing for all of Wave 6): nothing in `pi_space/pi` implements
//! `PiServerService`, so there is no `protocol.ts`/`sessions.ts` construction
//! site to check these mappings against. Every simplification below is
//! named, not silently assumed correct.
//!
//! **Named simplifications (no oracle exists to pin a "correct" answer):**
//! - `usage` is always `None` on every converted transcript item — a full
//!   `pirust_ai::types::Usage` -> wire `Usage` conversion is straightforward
//!   but adds a second parallel type mapping for no behavior this wave's own
//!   end-to-end test needs to observe; add it when a caller actually reads
//!   `TranscriptItem` usage data.
//! - A tool result's wire `input` field (the original call's arguments) has
//!   no source on `pirust_ai::types::ToolResultMessage` alone — the
//!   arguments lived on the assistant's `ToolCall` content block, not the
//!   result. Correlating back by `tool_call_id` across the transcript is
//!   possible but not done this wave; `input` is `ProtocolJson::Null` here.
//! - `ThinkingLevelMap` -> `supported_thinking_levels`: the map's own doc
//!   comment says `Some(None)` means "explicitly unsupported" and
//!   `Some(Some(_))` means "supported with this provider value", but leaves
//!   the absent case (`None`, key omitted) undocumented for levels other
//!   than the base one. This treats an absent `off` as supported (the
//!   universal default) and an absent non-off level as unsupported — a
//!   reasonable reading, not a verified one.
//! - `AgentHarnessErrorCode` (9 variants) collapses onto
//!   `PiServerOperationErrorCode` (5 variants, only one of which — `busy` —
//!   has a matching harness code) via [`harness_error_to_pi_error`];
//!   everything else becomes `invalid_request` with the harness's own
//!   message text, since the wire protocol has no equivalent finer-grained
//!   vocabulary for harness-internal failures.

use pirust_agent_core::harness::messages::AgentMessage;
use pirust_agent_core::harness::session::v4::types::Entry;
use pirust_agent_core::harness::AgentHarnessPhase;
use pirust_agent_core::types::AgentEvent;
use pirust_agent_core::types::ThinkingLevel as CoreThinkingLevel;
use pirust_ai::types::{
    AssistantContent as CoreAssistantContent, AssistantMessage as CoreAssistantMessage, Message,
    Model, StopReason, UserContent as CoreUserContent, UserMessageContent,
};

use crate::errors::{PiServerError, PiServerOperationErrorCode};
use crate::protocol::schemas::{
    AssistantContent, AssistantTranscriptItem, AssistantTranscriptItemCommon, CompleteStopReason,
    ModelCost, ModelInputKind, ModelMetadata, ModelRef, ProtocolJson, SessionPhase, ThinkingLevel,
    ToolContent, ToolTranscriptItem, ToolTranscriptItemCommon, TranscriptItem, TranscriptProgress,
    UserContent, UserTranscriptItem,
};

pub fn model_ref(model: &Model) -> ModelRef {
    ModelRef {
        provider: model.provider.0.clone(),
        id: model.id.clone(),
    }
}

pub fn map_phase(phase: AgentHarnessPhase) -> SessionPhase {
    match phase {
        AgentHarnessPhase::Idle => SessionPhase::Idle,
        AgentHarnessPhase::Turn => SessionPhase::Turn,
        AgentHarnessPhase::Compaction => SessionPhase::Compaction,
        AgentHarnessPhase::BranchSummary => SessionPhase::BranchSummary,
        AgentHarnessPhase::Retry => SessionPhase::Retry,
    }
}

pub fn map_thinking_level(level: CoreThinkingLevel) -> ThinkingLevel {
    match level {
        CoreThinkingLevel::Off => ThinkingLevel::Off,
        CoreThinkingLevel::Minimal => ThinkingLevel::Minimal,
        CoreThinkingLevel::Low => ThinkingLevel::Low,
        CoreThinkingLevel::Medium => ThinkingLevel::Medium,
        CoreThinkingLevel::High => ThinkingLevel::High,
        CoreThinkingLevel::Xhigh => ThinkingLevel::XHigh,
        CoreThinkingLevel::Max => ThinkingLevel::Max,
    }
}

pub fn map_thinking_level_to_core(level: ThinkingLevel) -> CoreThinkingLevel {
    match level {
        ThinkingLevel::Off => CoreThinkingLevel::Off,
        ThinkingLevel::Minimal => CoreThinkingLevel::Minimal,
        ThinkingLevel::Low => CoreThinkingLevel::Low,
        ThinkingLevel::Medium => CoreThinkingLevel::Medium,
        ThinkingLevel::High => CoreThinkingLevel::High,
        ThinkingLevel::XHigh => CoreThinkingLevel::Xhigh,
        ThinkingLevel::Max => CoreThinkingLevel::Max,
    }
}

fn map_stop_reason(reason: StopReason) -> Option<CompleteStopReason> {
    match reason {
        StopReason::Stop => Some(CompleteStopReason::Stop),
        StopReason::Length => Some(CompleteStopReason::Length),
        StopReason::ToolUse => Some(CompleteStopReason::ToolUse),
        StopReason::Error | StopReason::Aborted => None,
    }
}

pub fn json_value_to_protocol_json(value: &serde_json::Value) -> ProtocolJson {
    match value {
        serde_json::Value::Null => ProtocolJson::Null,
        serde_json::Value::Bool(b) => ProtocolJson::Bool(*b),
        serde_json::Value::Number(n) => ProtocolJson::Number(n.as_f64().unwrap_or(0.0)),
        serde_json::Value::String(s) => ProtocolJson::Text(s.clone()),
        serde_json::Value::Array(items) => {
            ProtocolJson::Array(items.iter().map(json_value_to_protocol_json).collect())
        }
        serde_json::Value::Object(map) => ProtocolJson::Map(
            map.iter()
                .map(|(k, v)| (k.clone(), json_value_to_protocol_json(v)))
                .collect(),
        ),
    }
}

fn user_content(content: &UserMessageContent) -> Vec<UserContent> {
    match content {
        UserMessageContent::Text(text) => vec![UserContent::Text { text: text.clone() }],
        UserMessageContent::Blocks(blocks) => blocks
            .iter()
            .map(|block| match block {
                CoreUserContent::Text(text) => UserContent::Text {
                    text: text.text.clone(),
                },
                CoreUserContent::Image(image) => UserContent::Image {
                    data: image.data.clone(),
                    mime_type: image.mime_type.clone(),
                },
            })
            .collect(),
    }
}

fn assistant_content(content: &[CoreAssistantContent]) -> Vec<AssistantContent> {
    content
        .iter()
        .map(|block| match block {
            CoreAssistantContent::Text(text) => AssistantContent::Text {
                text: text.text.clone(),
            },
            CoreAssistantContent::Thinking(thinking) => AssistantContent::Thinking {
                thinking: thinking.thinking.clone(),
                redacted: thinking.redacted,
            },
            CoreAssistantContent::ToolCall(call) => AssistantContent::ToolCall {
                tool_call_id: call.id.clone(),
                tool_name: call.name.clone(),
                input: ProtocolJson::Map(
                    call.arguments
                        .iter()
                        .map(|(k, v)| (k.clone(), json_value_to_protocol_json(v)))
                        .collect(),
                ),
            },
        })
        .collect()
}

/// `terminal = false` for a still-streaming assistant message (harness
/// `MessageStart`/`MessageUpdate` events) — produces `Streaming`, never
/// `Complete`/`Error`/`Aborted`: a message the harness hasn't finished
/// can't honestly claim a `stop_reason` yet. `terminal = true` for anything
/// read back out of `AgentHarness::entries()` (append-only, so always
/// finished).
fn assistant_transcript_item(
    id: String,
    timestamp: i64,
    msg: &CoreAssistantMessage,
    fallback_model_id: &str,
    terminal: bool,
) -> AssistantTranscriptItem {
    let common = AssistantTranscriptItemCommon {
        id,
        content: assistant_content(&msg.content),
        model: ModelRef {
            provider: msg.provider.0.clone(),
            id: msg
                .model
                .clone()
                .unwrap_or_else(|| fallback_model_id.to_string()),
        },
        response_model: msg.response_model.clone(),
        usage: None,
        timestamp,
    };
    if !terminal {
        return AssistantTranscriptItem::Streaming(common);
    }
    match msg.stop_reason {
        StopReason::Error => AssistantTranscriptItem::Error {
            common,
            error_message: msg.error_message.clone(),
        },
        StopReason::Aborted => AssistantTranscriptItem::Aborted {
            common,
            error_message: msg.error_message.clone(),
        },
        other => AssistantTranscriptItem::Complete {
            common,
            stop_reason: map_stop_reason(other).unwrap_or(CompleteStopReason::Stop),
        },
    }
}

fn tool_transcript_item(
    id: String,
    timestamp: i64,
    msg: &pirust_ai::types::ToolResultMessage,
) -> ToolTranscriptItem {
    let common = ToolTranscriptItemCommon {
        id,
        tool_call_id: msg.tool_call_id.clone(),
        tool_name: msg.tool_name.clone(),
        // See module doc: the original call arguments aren't reachable from
        // a `ToolResultMessage` alone this wave.
        input: ProtocolJson::Null,
        content: msg
            .content
            .iter()
            .map(|block| match block {
                CoreUserContent::Text(text) => ToolContent::Text {
                    text: text.text.clone(),
                },
                CoreUserContent::Image(image) => ToolContent::Image {
                    data: image.data.clone(),
                    mime_type: image.mime_type.clone(),
                },
            })
            .collect(),
        details: msg.details.as_ref().map(json_value_to_protocol_json),
        usage: None,
        timestamp,
    };
    if msg.is_error {
        ToolTranscriptItem::Error(common)
    } else {
        ToolTranscriptItem::Complete(common)
    }
}

/// Converts one `AgentMessage` into a wire `TranscriptItem`, or `None` for
/// message kinds the wire vocabulary has no slot for (named simplification,
/// not silent): `BashExecution`/`Custom`/`BranchSummary`/`CompactionSummary`
/// are agent-core-only bookkeeping messages with no `TranscriptItem`
/// counterpart in `schemas.rs` today.
pub fn agent_message_to_transcript_item(
    id: String,
    timestamp: i64,
    msg: &AgentMessage,
    fallback_model_id: &str,
    terminal: bool,
) -> Option<TranscriptItem> {
    match msg {
        AgentMessage::Llm(Message::User(user)) => Some(TranscriptItem::User(UserTranscriptItem {
            id,
            content: user_content(&user.content),
            timestamp,
        })),
        AgentMessage::Llm(Message::Assistant(assistant)) => Some(TranscriptItem::Assistant(
            assistant_transcript_item(id, timestamp, assistant, fallback_model_id, terminal),
        )),
        AgentMessage::Llm(Message::ToolResult(result)) => Some(TranscriptItem::Tool(
            tool_transcript_item(id, timestamp, result),
        )),
        AgentMessage::BashExecution(_)
        | AgentMessage::Custom(_)
        | AgentMessage::BranchSummary(_)
        | AgentMessage::CompactionSummary(_) => None,
    }
}

/// Builds the full transcript from a harness's append-only entry history —
/// always `terminal = true` (see [`agent_message_to_transcript_item`]).
pub fn entries_to_transcript(entries: &[Entry], fallback_model_id: &str) -> Vec<TranscriptItem> {
    entries
        .iter()
        .filter_map(|entry| match entry {
            Entry::Message(message_entry) => agent_message_to_transcript_item(
                message_entry.id.clone(),
                message_entry.timestamp,
                &message_entry.message,
                fallback_model_id,
                true,
            ),
            Entry::ModelChange(_)
            | Entry::ThinkingLevel(_)
            | Entry::ActiveTools(_)
            | Entry::Compaction(_)
            | Entry::BranchSummary(_)
            | Entry::Custom(_) => None,
        })
        .collect()
}

/// Maps a subset of `HarnessEvent::Loop` events onto `TranscriptProgress`
/// for real-time delivery via `PiSessionRuntime::subscribe`. Only
/// `MessageStart`/`MessageUpdate`/`MessageEnd` carry a `TranscriptProgress`
/// equivalent today; every other `AgentEvent` variant (turn/tool-execution/
/// agent-lifecycle events) has no wire-progress counterpart and is dropped
/// here, not forwarded as a fabricated shape.
///
/// **Named simplification:** `MessageUpdate` (a per-token streaming delta)
/// is flattened to `ItemUpdated` with the message's current partial state,
/// not `AssistantDelta`'s exact per-block/per-token shape — building the
/// latter needs `AssistantMessageEvent`'s own content-index/delta-kind
/// bookkeeping threaded through, which nothing in this wave's own
/// end-to-end test needs to observe.
///
/// **`User` items only ever produce `ItemStarted`, never `ItemUpdated`/
/// `ItemFinished`** — the wire schema restricts those two to
/// assistant/tool items (`TranscriptProgress`'s own doc comments in
/// `schemas.rs`); a user prompt's own `MessageEnd` (the harness appends it
/// to the tree the same way as any other message) is therefore dropped
/// here rather than emitted as an invalid `item_finished`.
pub fn agent_event_to_progress(
    event: &AgentEvent,
    fallback_model_id: &str,
) -> Option<TranscriptProgress> {
    match event {
        AgentEvent::MessageStart { message } => agent_message_to_transcript_item(
            temp_id(message),
            current_millis(),
            message,
            fallback_model_id,
            false,
        )
        .map(|item| TranscriptProgress::ItemStarted { item }),
        AgentEvent::MessageUpdate { message, .. } => agent_message_to_transcript_item(
            temp_id(message),
            current_millis(),
            message,
            fallback_model_id,
            false,
        )
        .filter(|item| !matches!(item, TranscriptItem::User(_)))
        .map(|item| TranscriptProgress::ItemUpdated { item }),
        AgentEvent::MessageEnd { message } => agent_message_to_transcript_item(
            temp_id(message),
            current_millis(),
            message,
            fallback_model_id,
            true,
        )
        .filter(is_wire_terminal_item)
        .map(|item| TranscriptProgress::ItemFinished { item }),
        _ => None,
    }
}

/// Mirrors `TranscriptItem`'s own (private) `is_terminal` in `schemas.rs`:
/// a `User` item is never terminal, an `Assistant` item is terminal unless
/// `Streaming`, a `Tool` item is terminal unless `Running`.
fn is_wire_terminal_item(item: &TranscriptItem) -> bool {
    match item {
        TranscriptItem::User(_) => false,
        TranscriptItem::Assistant(a) => !matches!(a, AssistantTranscriptItem::Streaming(_)),
        TranscriptItem::Tool(t) => !matches!(t, ToolTranscriptItem::Running(_)),
    }
}

/// `MessageStart`/`MessageUpdate`/`MessageEnd` events carry the raw
/// `AgentMessage`, not the session-tree entry id `entries()` assigns later —
/// this wave has no way to look that id up before the entry is actually
/// persisted, so progress events use a content-independent placeholder
/// derived from role + timestamp instead of a stable id. Named, not silent:
/// a client correlating progress events by id against the eventual
/// persisted transcript cannot rely on this id matching.
fn temp_id(message: &AgentMessage) -> String {
    match message {
        AgentMessage::Llm(Message::User(_)) => format!("progress-user-{}", current_millis()),
        AgentMessage::Llm(Message::Assistant(_)) => {
            format!("progress-assistant-{}", current_millis())
        }
        AgentMessage::Llm(Message::ToolResult(_)) => format!("progress-tool-{}", current_millis()),
        _ => format!("progress-{}", current_millis()),
    }
}

fn current_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn thinking_level_supported(field: Option<&Option<String>>, is_off: bool) -> bool {
    match field {
        None => is_off,
        Some(None) => false,
        Some(Some(_)) => true,
    }
}

/// Port of real Pi's `nonNegativeNumber` (`packages/server/src/protocol.ts`):
/// `toProtocolModelMetadata` clamps every cost field through this before it
/// reaches the wire, since the builtin catalog carries real Pi's own
/// unknown-pricing sentinel (`-1_000_000`, e.g. `openrouter/auto`) which
/// would otherwise fail `ModelCostSchema`'s `minimum: 0` on the wire. This
/// crate's own `model_metadata` below skipped that clamp until a real
/// `pirust-orchestrator` binary was driven through a real handshake against
/// the real builtin catalog and hit it live.
fn non_negative_number(value: f64) -> f64 {
    if value.is_finite() {
        value.max(0.0)
    } else {
        0.0
    }
}

pub fn model_metadata(model: &Model, authenticated: bool) -> ModelMetadata {
    let map = model.thinking_level_map.as_ref();
    let mut supported = Vec::new();
    for (level, field) in [
        (ThinkingLevel::Off, map.map(|m| &m.off)),
        (ThinkingLevel::Minimal, map.map(|m| &m.minimal)),
        (ThinkingLevel::Low, map.map(|m| &m.low)),
        (ThinkingLevel::Medium, map.map(|m| &m.medium)),
        (ThinkingLevel::High, map.map(|m| &m.high)),
        (ThinkingLevel::XHigh, map.map(|m| &m.xhigh)),
        (ThinkingLevel::Max, map.map(|m| &m.max)),
    ] {
        let is_off = matches!(level, ThinkingLevel::Off);
        let present = thinking_level_supported(field.and_then(|f| f.as_ref()), is_off);
        if present {
            supported.push(level);
        }
    }
    if supported.is_empty() {
        supported.push(ThinkingLevel::Off);
    }

    ModelMetadata {
        provider: model.provider.0.clone(),
        id: model.id.clone(),
        name: model.name.clone(),
        api: model.api.0.clone(),
        reasoning: model.reasoning,
        input: model
            .input
            .iter()
            .map(|modality| match modality {
                pirust_ai::types::Modality::Text => ModelInputKind::Text,
                pirust_ai::types::Modality::Image => ModelInputKind::Image,
            })
            .collect(),
        // Real Pi: `Math.max(1, Math.floor(model.contextWindow))` /
        // `...maxTokens`. `model.context_window`/`max_tokens` are already
        // integer `u64`s here (no `Math.floor` equivalent needed), so only
        // the "at least 1" floor is ported.
        context_window: (model.context_window as i64).max(1),
        max_tokens: (model.max_tokens as i64).max(1),
        cost: ModelCost {
            input: non_negative_number(model.cost.rates.input),
            output: non_negative_number(model.cost.rates.output),
            cache_read: non_negative_number(model.cost.rates.cache_read),
            cache_write: non_negative_number(model.cost.rates.cache_write),
        },
        supported_thinking_levels: supported,
        authenticated,
    }
}

pub fn harness_error_to_pi_error(
    error: pirust_agent_core::harness::types::AgentHarnessError,
) -> PiServerError {
    if error.code == pirust_agent_core::harness::types::AgentHarnessErrorCode::Busy {
        PiServerError::busy(Some(error.message), None)
    } else {
        PiServerError::new(
            PiServerOperationErrorCode::InvalidRequest,
            error.message,
            None,
        )
    }
}
