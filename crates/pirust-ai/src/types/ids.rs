//! Provider/API identifiers and scalar enums.
//!
//! Port of the identifier and enum types at the top of `packages/ai/src/types.ts`
//! (`KnownApi`, `Api`, `KnownProvider`, `ThinkingLevel`, `CacheRetention`,
//! `Transport`, `StopReason`, ...). TS models these as string-literal unions with an
//! open `| (string & {})` escape hatch; we mirror that with transparent newtypes for
//! the open ones and closed `enum`s for the fixed ones.

use serde::{Deserialize, Serialize};

/// A wire-protocol identifier (`Model.api`). Known values live in [`known_api`];
/// custom providers may use any string, matching TS `Api = KnownApi | (string & {})`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Api(pub String);

impl Api {
    /// True if this is one of the ten built-in [`known_api`] adapters.
    pub fn is_known(&self) -> bool {
        known_api::ALL.contains(&self.0.as_str())
    }
}

impl From<&str> for Api {
    fn from(s: &str) -> Self {
        Api(s.to_string())
    }
}

/// The ten built-in wire protocols (TS `KnownApi`).
pub mod known_api {
    pub const OPENAI_COMPLETIONS: &str = "openai-completions";
    pub const MISTRAL_CONVERSATIONS: &str = "mistral-conversations";
    pub const OPENAI_RESPONSES: &str = "openai-responses";
    pub const AZURE_OPENAI_RESPONSES: &str = "azure-openai-responses";
    pub const OPENAI_CODEX_RESPONSES: &str = "openai-codex-responses";
    pub const ANTHROPIC_MESSAGES: &str = "anthropic-messages";
    pub const BEDROCK_CONVERSE_STREAM: &str = "bedrock-converse-stream";
    pub const GOOGLE_GENERATIVE_AI: &str = "google-generative-ai";
    pub const GOOGLE_VERTEX: &str = "google-vertex";
    pub const PI_MESSAGES: &str = "pi-messages";

    /// All known API identifiers, in declaration order.
    pub const ALL: &[&str] = &[
        OPENAI_COMPLETIONS,
        MISTRAL_CONVERSATIONS,
        OPENAI_RESPONSES,
        AZURE_OPENAI_RESPONSES,
        OPENAI_CODEX_RESPONSES,
        ANTHROPIC_MESSAGES,
        BEDROCK_CONVERSE_STREAM,
        GOOGLE_GENERATIVE_AI,
        GOOGLE_VERTEX,
        PI_MESSAGES,
    ];
}

/// A host identifier (`Model.provider`). TS `ProviderId = KnownProvider | string`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProviderId(pub String);

impl From<&str> for ProviderId {
    fn from(s: &str) -> Self {
        ProviderId(s.to_string())
    }
}

/// An image-generation wire protocol. TS `ImagesApi = KnownImagesApi | (string & {})`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ImagesApi(pub String);

/// Thinking effort level (TS `ThinkingLevel`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThinkingLevel {
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

/// Model thinking level including the disabled state (TS `ModelThinkingLevel`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelThinkingLevel {
    Off,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

/// Prompt-cache retention preference (TS `CacheRetention`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CacheRetention {
    None,
    Short,
    Long,
}

/// Preferred transport (TS `Transport`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Transport {
    Sse,
    Websocket,
    WebsocketCached,
    Auto,
}

/// Terminal reason for an assistant turn (TS `StopReason`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StopReason {
    Stop,
    Length,
    ToolUse,
    Error,
    Aborted,
}

/// Session-affinity header format (TS `SessionAffinityFormat`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SessionAffinityFormat {
    Openai,
    OpenaiNosession,
    Openrouter,
}

/// Token budgets per thinking level (TS `ThinkingBudgets`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThinkingBudgets {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minimal: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub low: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub medium: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub high: Option<u64>,
}

/// A `pi`-controlled variable reference inside chat-template kwargs
/// (the `{ $var, omitWhenOff }` arm of TS `ChatTemplateKwargValue`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatTemplateVar {
    #[serde(rename = "$var")]
    pub var: ChatTemplateVarName,
    #[serde(rename = "omitWhenOff", skip_serializing_if = "Option::is_none")]
    pub omit_when_off: Option<bool>,
}

/// Allowed `$var` targets (TS literal union).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChatTemplateVarName {
    #[serde(rename = "thinking.enabled")]
    ThinkingEnabled,
    #[serde(rename = "thinking.effort")]
    ThinkingEffort,
}

/// A chat-template kwarg value (TS `ChatTemplateKwargValue`): scalar or `$var` ref.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ChatTemplateKwargValue {
    Var(ChatTemplateVar),
    Bool(bool),
    Number(f64),
    String(String),
    Null,
}

/// The `commentary`/`final_answer` phase marker in [`TextSignatureV1`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextSignaturePhase {
    Commentary,
    FinalAnswer,
}

/// Parsed form of a versioned text signature (TS `TextSignatureV1`). Stored on the
/// wire as a JSON string inside `TextContent.textSignature`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextSignatureV1 {
    pub v: u8,
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<TextSignaturePhase>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_known_detection() {
        assert!(Api::from("anthropic-messages").is_known());
        assert!(!Api::from("my-custom-api").is_known());
    }

    #[test]
    fn enum_wire_values() {
        assert_eq!(
            serde_json::to_string(&StopReason::ToolUse).unwrap(),
            "\"toolUse\""
        );
        assert_eq!(
            serde_json::to_string(&Transport::WebsocketCached).unwrap(),
            "\"websocket-cached\""
        );
        assert_eq!(
            serde_json::to_string(&ThinkingLevel::Xhigh).unwrap(),
            "\"xhigh\""
        );
        assert_eq!(
            serde_json::to_string(&SessionAffinityFormat::OpenaiNosession).unwrap(),
            "\"openai-nosession\""
        );
    }

    #[test]
    fn chat_template_var_var_key() {
        let v = ChatTemplateKwargValue::Var(ChatTemplateVar {
            var: ChatTemplateVarName::ThinkingEnabled,
            omit_when_off: Some(true),
        });
        assert_eq!(
            serde_json::to_string(&v).unwrap(),
            r#"{"$var":"thinking.enabled","omitWhenOff":true}"#
        );
    }
}
