//! Agent-core message variants + LLM conversion — port of
//! `packages/agent/src/harness/messages.ts`.
//!
//! Spec: `docs/analysis/07-agent-core-spec.md` §1.1 (the 4 custom variants),
//! §1.2 (timestamp string→number flip in constructors), §10 (`convert_to_llm`).
//! `[LEAF]` (§13).
//!
//! `AgentMessage` is the closed union `Message | CustomAgentMessages[...]`
//! (types.ts:314 + the declaration-merge seam messages.ts:54-61). Pi merges FOUR
//! agent-core-only variants into it; Rust models the union as one enum whose
//! `Llm` arm REUSES pi-ai's `Message` (byte-verified in feat-001) so the
//! user/assistant/toolResult wire shape stays byte-identical.
//!
//! Discrimination is by the inner `role` string: the enum is `#[serde(untagged)]`
//! and every arm carries its own `role` unit-tag field (pi-ai's `Message`
//! variants already do), so a wrong `role` value is rejected by that field and
//! the untagged match is deterministic (`bashExecution` is agent-core-only).

use pirust_ai::types::{
    Message, TextContent, UserContent, UserMessage, UserMessageContent, UserRole,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Prefix wrapping a compaction summary when it re-enters the LLM context
/// (messages.ts:4-7). Kept verbatim — the trailing blank line + `<summary>` are
/// part of the byte contract.
pub const COMPACTION_SUMMARY_PREFIX: &str =
    "The conversation history before this point was compacted into the following summary:\n\n<summary>\n";

/// Suffix closing a compaction summary (messages.ts:9-10).
pub const COMPACTION_SUMMARY_SUFFIX: &str = "\n</summary>";

/// Prefix wrapping a branch summary when it re-enters the LLM context
/// (messages.ts:12-15).
pub const BRANCH_SUMMARY_PREFIX: &str =
    "The following is a summary of a branch that this conversation came back from:\n\n<summary>\n";

/// Suffix closing a branch summary (messages.ts:17).
pub const BRANCH_SUMMARY_SUFFIX: &str = "</summary>";

/// Unit tag serializing as `"bashExecution"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum BashExecutionRole {
    #[serde(rename = "bashExecution")]
    #[default]
    BashExecution,
}

/// Unit tag serializing as `"custom"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum CustomRole {
    #[serde(rename = "custom")]
    #[default]
    Custom,
}

/// Unit tag serializing as `"branchSummary"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum BranchSummaryRole {
    #[serde(rename = "branchSummary")]
    #[default]
    BranchSummary,
}

/// Unit tag serializing as `"compactionSummary"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum CompactionSummaryRole {
    #[serde(rename = "compactionSummary")]
    #[default]
    CompactionSummary,
}

/// Record of a bash command executed outside the LLM turn (messages.ts:19-29).
///
/// Key order (byte contract, §1.1): `role, command, output, exitCode, cancelled,
/// truncated, fullOutputPath?, timestamp, excludeFromContext?`. `exitCode` is
/// `number | undefined` — `0` is emitted (only `undefined` is omitted).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BashExecutionMessage {
    #[serde(rename = "role", default)]
    pub role: BashExecutionRole,
    pub command: String,
    pub output: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i64>,
    pub cancelled: bool,
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub full_output_path: Option<String>,
    /// Unix timestamp in milliseconds.
    pub timestamp: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exclude_from_context: Option<bool>,
}

/// Application-defined message carried in the transcript (messages.ts:31-38).
///
/// Key order (constructor messages.ts:110-117): `role, customType, content,
/// display, details?, timestamp`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomMessage {
    #[serde(rename = "role", default)]
    pub role: CustomRole,
    pub custom_type: String,
    /// `string | (TextContent | ImageContent)[]` — reuses pi-ai's user content union.
    pub content: UserMessageContent,
    pub display: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    /// Unix timestamp in milliseconds.
    pub timestamp: i64,
}

/// Summary of a branch this conversation came back from (messages.ts:40-45).
///
/// Key order (constructor messages.ts:82-87): `role, summary, fromId, timestamp`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchSummaryMessage {
    #[serde(rename = "role", default)]
    pub role: BranchSummaryRole,
    pub summary: String,
    pub from_id: String,
    /// Unix timestamp in milliseconds.
    pub timestamp: i64,
}

/// Summary of compacted history (messages.ts:47-52).
///
/// Key order (constructor messages.ts:95-100): `role, summary, tokensBefore,
/// timestamp`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionSummaryMessage {
    #[serde(rename = "role", default)]
    pub role: CompactionSummaryRole,
    pub summary: String,
    pub tokens_before: i64,
    /// Unix timestamp in milliseconds.
    pub timestamp: i64,
}

/// Union of LLM messages + agent-core custom messages (types.ts:314, merged with
/// messages.ts:54-61).
///
/// `#[serde(untagged)]`: each arm carries its own `role` field, so serialization
/// is byte-identical to the inner value and deserialization dispatches on `role`.
/// Order matters for the untagged match — `Llm` (pi-ai `Message`) is tried first;
/// its role unit-tags reject any non-LLM role so the four agent-core arms below
/// only capture their own `role` strings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
#[allow(clippy::large_enum_variant)] // `Llm(Message)` dwarfs the four small variants (see mod.rs:92 precedent)
pub enum AgentMessage {
    /// `user` / `assistant` / `toolResult` — reuses pi-ai's byte-verified `Message`.
    Llm(Message),
    BashExecution(BashExecutionMessage),
    Custom(CustomMessage),
    BranchSummary(BranchSummaryMessage),
    CompactionSummary(CompactionSummaryMessage),
}

impl BashExecutionMessage {
    /// Whether this execution is hidden from the LLM context.
    fn excluded(&self) -> bool {
        self.exclude_from_context == Some(true)
    }
}

/// Render a bash execution as user-visible text for the LLM (messages.ts:63-79).
pub fn bash_execution_to_text(msg: &BashExecutionMessage) -> String {
    let mut text = format!("Ran `{}`\n", msg.command);
    if msg.output.is_empty() {
        text.push_str("(no output)");
    } else {
        text.push_str(&format!("```\n{}\n```", msg.output));
    }
    if msg.cancelled {
        text.push_str("\n\n(command cancelled)");
    } else if let Some(code) = msg.exit_code {
        if code != 0 {
            text.push_str(&format!("\n\nCommand exited with code {code}"));
        }
    }
    if msg.truncated {
        if let Some(path) = &msg.full_output_path {
            text.push_str(&format!("\n\n[Output truncated. Full output: {path}]"));
        }
    }
    text
}

/// Build a [`BranchSummaryMessage`] from a `branch_summary` entry, flipping the
/// entry's ISO-string timestamp to a Unix-ms number (§1.2, messages.ts:81-88).
pub fn create_branch_summary_message(
    summary: String,
    from_id: String,
    timestamp_ms: i64,
) -> BranchSummaryMessage {
    BranchSummaryMessage {
        role: BranchSummaryRole::BranchSummary,
        summary,
        from_id,
        timestamp: timestamp_ms,
    }
}

/// Build a [`CompactionSummaryMessage`] from a `compaction` entry (§1.2,
/// messages.ts:90-101).
pub fn create_compaction_summary_message(
    summary: String,
    tokens_before: i64,
    timestamp_ms: i64,
) -> CompactionSummaryMessage {
    CompactionSummaryMessage {
        role: CompactionSummaryRole::CompactionSummary,
        summary,
        tokens_before,
        timestamp: timestamp_ms,
    }
}

/// Build a [`CustomMessage`] from a `custom_message` entry (§1.2, messages.ts:103-118).
pub fn create_custom_message(
    custom_type: String,
    content: UserMessageContent,
    display: bool,
    details: Option<Value>,
    timestamp_ms: i64,
) -> CustomMessage {
    CustomMessage {
        role: CustomRole::Custom,
        custom_type,
        content,
        display,
        details,
        timestamp: timestamp_ms,
    }
}

/// Build a `user` [`Message`] wrapping the given text blocks at `timestamp`.
fn user_text_message(text: String, timestamp: i64) -> Message {
    Message::User(UserMessage {
        role: UserRole::User,
        content: UserMessageContent::Blocks(vec![UserContent::Text(TextContent::new(text))]),
        timestamp,
    })
}

/// Default harness converter: `AgentMessage[]` → LLM `Message[]` (messages.ts:120-164,
/// §10).
///
/// - `bashExecution` → user text via [`bash_execution_to_text`], unless
///   `excludeFromContext` (dropped);
/// - `custom` → user (string content promoted to a single text block);
/// - `branchSummary` / `compactionSummary` → user wrapped in the prefix/suffix
///   constants;
/// - `user` / `assistant` / `toolResult` → passthrough;
/// - anything else → dropped.
pub fn convert_to_llm(messages: &[AgentMessage]) -> Vec<Message> {
    messages
        .iter()
        .filter_map(|m| match m {
            AgentMessage::BashExecution(b) => {
                if b.excluded() {
                    None
                } else {
                    Some(user_text_message(bash_execution_to_text(b), b.timestamp))
                }
            }
            AgentMessage::Custom(c) => {
                let content: Vec<UserContent> = match &c.content {
                    UserMessageContent::Text(s) => {
                        vec![UserContent::Text(TextContent::new(s.clone()))]
                    }
                    UserMessageContent::Blocks(blocks) => blocks.clone(),
                };
                Some(Message::User(UserMessage {
                    role: UserRole::User,
                    content: UserMessageContent::Blocks(content),
                    timestamp: c.timestamp,
                }))
            }
            AgentMessage::BranchSummary(bs) => {
                let text = format!(
                    "{BRANCH_SUMMARY_PREFIX}{}{BRANCH_SUMMARY_SUFFIX}",
                    bs.summary
                );
                Some(user_text_message(text, bs.timestamp))
            }
            AgentMessage::CompactionSummary(cs) => {
                let text = format!(
                    "{COMPACTION_SUMMARY_PREFIX}{}{COMPACTION_SUMMARY_SUFFIX}",
                    cs.summary
                );
                Some(user_text_message(text, cs.timestamp))
            }
            AgentMessage::Llm(msg) => Some(msg.clone()),
        })
        .collect()
}
