//! Conversation messages (TS `UserMessage`, `AssistantMessage`, `ToolResultMessage`,
//! `Message`), plus `Context` and `Tool`.
//!
//! `Message` is an internally-tagged enum on `role`, matching TS `{ role: "user" | ... }`.
//! `timestamp` is Unix milliseconds (`i64`, matching the JS `number`).

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::content::{AssistantContent, UserContent};
use super::ids::{Api, ProviderId, StopReason};
use super::usage::Usage;

/// User message body: a bare string or a list of text/image blocks
/// (TS `string | (TextContent | ImageContent)[]`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum UserMessageContent {
    Text(String),
    Blocks(Vec<UserContent>),
}

/// Unit tag serializing as `"user"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum UserRole {
    #[serde(rename = "user")]
    #[default]
    User,
}

/// Unit tag serializing as `"assistant"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum AssistantRole {
    #[serde(rename = "assistant")]
    #[default]
    Assistant,
}

/// Unit tag serializing as `"toolResult"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ToolResultRole {
    #[serde(rename = "toolResult")]
    #[default]
    ToolResult,
}

/// A user-authored message (TS `UserMessage`). Carries an explicit `role` field so it
/// serializes correctly both standalone and inside [`Message`] (role is first key).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserMessage {
    #[serde(rename = "role", default)]
    pub role: UserRole,
    pub content: UserMessageContent,
    /// Unix timestamp in milliseconds.
    pub timestamp: i64,
}

/// An assistant turn (TS `AssistantMessage`, minus the `role` tag).
///
/// `diagnostics` is carried as opaque JSON for now; the concrete
/// `AssistantMessageDiagnostic` shape (defined in `utils/diagnostics.ts`) is ported
/// in feat-002 alongside the runtime that produces it.
///
/// FIELD ORDER matches the Anthropic adapter's *runtime insertion order*, not the TS
/// interface's declaration order (spec §4e, `docs/analysis/06-anthropic-runtime-spec.md`):
/// the adapter's initial object literal is `role, content, api, provider, model, usage,
/// stopReason, timestamp`, then `responseId` is inserted in `message_start` (AFTER
/// `timestamp`), `rawStopReason`/`endTurn` in `message_delta`, and `errorMessage` in the
/// refusal/catch paths (AFTER `responseId`). So `response_id` is declared after `timestamp`.
/// `response_model`/`diagnostics` are never set by this adapter and are absent
/// (`skip_serializing_if`) in every current oracle, so their position (before `usage`)
/// is byte-irrelevant today; kept there per the TS layout.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantMessage {
    #[serde(rename = "role", default)]
    pub role: AssistantRole,
    pub content: Vec<AssistantContent>,
    pub api: Api,
    pub provider: ProviderId,
    /// Requested model id. NOTE: 0.84.2's adapter overwrites this from the wire's
    /// `message_start.message.model` (anthropic-messages.ts:604); when that field is
    /// absent the value becomes `undefined` and `JSON.stringify` omits the key, so it
    /// must be optional (the 0.84.2 oracle fixtures omit it when the SSE has no
    /// `message.model`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Concrete resolved model when it differs from the requested one (e.g. OpenRouter `auto`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_model: Option<String>,
    /// Redacted provider/runtime diagnostics (concrete type ported in feat-002).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<Vec<Value>>,
    pub usage: Usage,
    pub stop_reason: StopReason,
    /// Unix timestamp in milliseconds.
    pub timestamp: i64,
    /// Provider-specific response/message identifier, when exposed. Declared AFTER
    /// `timestamp` to match Pi's runtime key order — the adapter inserts `responseId` in
    /// `message_start`, after the initial literal that ends with `timestamp` (spec §4e).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_id: Option<String>,
    /// Provider stop reason as the wire reported it (TS `rawStopReason`). INSERTED by the
    /// Anthropic adapter in `message_delta` (`output.rawStopReason = event.delta.stop_reason`),
    /// i.e. AFTER `responseId` and BEFORE `errorMessage` — must match that runtime key order.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_stop_reason: Option<String>,
    /// NOTE: declared after `response_id` to match Pi's *runtime* object key order
    /// (the TS interface lists it before `timestamp`, but the constructed object — and
    /// thus `JSON.stringify` — emits it last; verified against real session fixtures).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    /// Provider indication of whether the model explicitly ended its turn (TS `endTurn`).
    /// Declared last to match the TS interface's tail position; not set by the anthropic
    /// adapter today, so absent from every current oracle.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_turn: Option<bool>,
}

/// The result of executing a tool call (TS `ToolResultMessage`, minus the `role` tag).
/// `details` is the generic `TDetails` (defaults to `any`), carried as opaque JSON.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResultMessage {
    #[serde(rename = "role", default)]
    pub role: ToolResultRole,
    pub tool_call_id: String,
    pub tool_name: String,
    pub content: Vec<UserContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    /// Tool names from `Context.tools` that became available after this result.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub added_tool_names: Option<Vec<String>>,
    pub is_error: bool,
    /// Unix timestamp in milliseconds.
    pub timestamp: i64,
}

/// A single conversation message (TS `Message` union). Untagged: each variant's struct
/// carries its own explicit `role` field (so a message serializes identically whether
/// standalone or wrapped), and the role unit-enum tags make the untagged match
/// deterministic — a wrong `role` value is rejected by the variant's role field.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Message {
    User(UserMessage),
    Assistant(AssistantMessage),
    ToolResult(ToolResultMessage),
}

/// OpenAI grammar variants for constrained sampling (TS `GrammarFormat`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GrammarFormat {
    OpenaiLark,
    OpenaiRegex,
}

/// Per-provider grammar encodings of the same intended language (TS
/// `GrammarVariants` = `Partial<Record<GrammarFormat,string>>`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrammarVariants {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub openai_lark: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub openai_regex: Option<String>,
}

/// Provider-side constrained-sampling config for a tool (TS
/// `ConstrainedSamplingConfig`). Rendered from JSON; the `json_schema` variant
/// selects strict JSON-schema sampling, `grammar` provides grammar encodings.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum ConstrainedSamplingConfig {
    #[serde(rename = "json_schema")]
    JsonSchema {
        strict: ConstrainedSamplingStrictness,
    },
    Grammar {
        variants: GrammarVariants,
    },
}

/// How strictly a JSON-schema-constrained tool must be sampled (TS `"prefer"
/// | "require"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConstrainedSamplingStrictness {
    Prefer,
    Require,
}

/// A tool definition offered to the model (TS `Tool`). `parameters` is a JSON Schema
/// (TS `TSchema`); ported as opaque JSON — `schemars`/`jsonschema` integration lands
/// with the tool crate (feat-004).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tool {
    pub name: String,
    pub description: String,
    pub parameters: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub constrained_sampling: Option<ConstrainedSamplingConfig>,
}

/// The full request context sent to a provider (TS `Context`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Context {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    pub messages: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Tool>>,
}

#[cfg(test)]
mod tests {
    use super::super::content::{TextContent, ToolCall, ToolCallTag};
    use super::super::usage::{Cost, Usage};
    use super::*;
    use serde_json::Map;

    #[test]
    fn user_message_string_content_roundtrips() {
        let msg = Message::User(UserMessage {
            role: UserRole::User,
            content: UserMessageContent::Text("hello".into()),
            timestamp: 1_700_000_000_000,
        });
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(
            json,
            r#"{"role":"user","content":"hello","timestamp":1700000000000}"#
        );
        assert_eq!(serde_json::from_str::<Message>(&json).unwrap(), msg);
    }

    #[test]
    fn assistant_message_with_tool_call_roundtrips() {
        let msg = Message::Assistant(AssistantMessage {
            role: AssistantRole::Assistant,
            content: vec![
                AssistantContent::Text(TextContent::new("on it")),
                AssistantContent::ToolCall(ToolCall {
                    kind: ToolCallTag::ToolCall,
                    id: "c1".into(),
                    name: "read".into(),
                    arguments: Map::new(),
                    thought_signature: None,
                    partial_json: None,
                }),
            ],
            api: Api::from("anthropic-messages"),
            provider: ProviderId::from("anthropic"),
            model: Some("claude".into()),
            response_model: None,
            response_id: None,
            diagnostics: None,
            usage: Usage {
                input: 1,
                output: 1,
                cache_read: 0,
                cache_write: 0,
                cache_write1h: None,
                reasoning: None,
                total_tokens: Some(2),
                cost: Cost {
                    input: 0.0,
                    output: 0.0,
                    cache_read: 0.0,
                    cache_write: 0.0,
                    total: 0.0,
                },
            },
            stop_reason: StopReason::ToolUse,
            error_message: None,
            raw_stop_reason: None,
            end_turn: None,
            timestamp: 1,
        });
        let json = serde_json::to_string(&msg).unwrap();
        // role tag first, camelCase keys, absent optionals omitted, stopReason camelCase
        assert!(json.starts_with(
            r#"{"role":"assistant","content":[{"type":"text","text":"on it"},{"type":"toolCall""#
        ));
        assert!(json.contains(r#""stopReason":"toolUse""#));
        assert!(!json.contains("responseModel"));
        assert_eq!(serde_json::from_str::<Message>(&json).unwrap(), msg);
    }
}
