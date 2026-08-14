//! Content blocks (TS `TextContent`, `ThinkingContent`, `ImageContent`, `ToolCall`)
//! and the per-message content arrays.
//!
//! Each block carries an explicit `type` discriminant via a unit-enum tag, so a block
//! serializes identically whether standalone (e.g. `toolcall_end.toolCall`) or inside
//! a content array. Arrays are `#[serde(untagged)]`; the tag enums make the untagged
//! match unambiguous and validate the discriminant on the way in.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Unit tag serializing as `"text"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum TextTag {
    #[serde(rename = "text")]
    #[default]
    Text,
}

/// Unit tag serializing as `"thinking"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ThinkingTag {
    #[serde(rename = "thinking")]
    #[default]
    Thinking,
}

/// Unit tag serializing as `"image"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ImageTag {
    #[serde(rename = "image")]
    #[default]
    Image,
}

/// Unit tag serializing as `"toolCall"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ToolCallTag {
    #[serde(rename = "toolCall")]
    #[default]
    ToolCall,
}

/// Plain text content (TS `TextContent`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextContent {
    #[serde(rename = "type", default)]
    pub kind: TextTag,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_signature: Option<String>,
}

impl TextContent {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            kind: TextTag::Text,
            text: text.into(),
            text_signature: None,
        }
    }
}

/// Reasoning/thinking content (TS `ThinkingContent`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThinkingContent {
    #[serde(rename = "type", default)]
    pub kind: ThinkingTag,
    pub thinking: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_signature: Option<String>,
    /// True when the content was redacted by safety filters; the opaque payload
    /// lives in `thinking_signature`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub redacted: Option<bool>,
}

/// Base64 image content (TS `ImageContent`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageContent {
    #[serde(rename = "type", default)]
    pub kind: ImageTag,
    /// Base64-encoded image data.
    pub data: String,
    /// MIME type, e.g. `image/png`.
    pub mime_type: String,
}

/// A model-issued tool call (TS `ToolCall`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCall {
    #[serde(rename = "type", default)]
    pub kind: ToolCallTag,
    pub id: String,
    pub name: String,
    pub arguments: Map<String, Value>,
    /// Google-specific opaque signature for reusing thought context.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thought_signature: Option<String>,
    /// Transient streaming accumulator for the raw tool-argument JSON fragments
    /// (`ToolCall & { partialJson }` in Pi's api adapters). Deleted on normal tool-call
    /// completion, but persisted into sessions when a turn is aborted/interrupted
    /// mid-stream — so it must round-trip. Absent on completed calls.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub partial_json: Option<String>,
}

/// A content block permitted in user messages and tool results
/// (TS `TextContent | ImageContent`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum UserContent {
    Text(TextContent),
    Image(ImageContent),
}

/// A content block permitted in assistant messages
/// (TS `TextContent | ThinkingContent | ToolCall`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AssistantContent {
    Text(TextContent),
    Thinking(ThinkingContent),
    ToolCall(ToolCall),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip<T>(value: &T, expected_json: &str)
    where
        T: Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
    {
        let json = serde_json::to_string(value).unwrap();
        assert_eq!(json, expected_json, "serialized form must match Pi");
        let back: T = serde_json::from_str(&json).unwrap();
        assert_eq!(&back, value, "round-trip must be lossless");
    }

    #[test]
    fn text_content_omits_absent_signature() {
        roundtrip(&TextContent::new("hi"), r#"{"type":"text","text":"hi"}"#);
    }

    #[test]
    fn tool_call_carries_type_when_standalone() {
        let mut args = Map::new();
        args.insert("path".into(), Value::String("/tmp/x".into()));
        let tc = ToolCall {
            kind: ToolCallTag::ToolCall,
            id: "call_1".into(),
            name: "read".into(),
            arguments: args,
            thought_signature: None,
            partial_json: None,
        };
        roundtrip(
            &tc,
            r#"{"type":"toolCall","id":"call_1","name":"read","arguments":{"path":"/tmp/x"}}"#,
        );
    }

    #[test]
    fn assistant_content_untagged_dispatch() {
        let json = r#"{"type":"thinking","thinking":"hmm"}"#;
        let parsed: AssistantContent = serde_json::from_str(json).unwrap();
        assert!(matches!(parsed, AssistantContent::Thinking(_)));
        assert_eq!(serde_json::to_string(&parsed).unwrap(), json);
    }

    #[test]
    fn image_mime_type_is_camel_case() {
        let img = UserContent::Image(ImageContent {
            kind: ImageTag::Image,
            data: "AAAA".into(),
            mime_type: "image/png".into(),
        });
        roundtrip(
            &img,
            r#"{"type":"image","data":"AAAA","mimeType":"image/png"}"#,
        );
    }
}
