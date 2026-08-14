//! The streaming event protocol (TS `AssistantMessageEvent`).
//!
//! Internally-tagged on `type`. Streams emit `start`, then `*_start`/`*_delta`/`*_end`
//! updates each carrying a `partial` snapshot, and terminate with `done` (success) or
//! `error`. Every event's `type` value and camelCase fields match the TS union exactly.

use serde::{Deserialize, Serialize};

use super::content::ToolCall;
use super::ids::StopReason;
use super::message::AssistantMessage;

/// A single streaming event (TS `AssistantMessageEvent`).
///
/// `reason` on `done` is one of `stop`/`length`/`toolUse` and on `error` one of
/// `aborted`/`error`; both reuse [`StopReason`] here (the TS `Extract<...>` narrowing
/// is a compile-time-only refinement with no wire effect).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum AssistantMessageEvent {
    Start {
        partial: AssistantMessage,
    },
    TextStart {
        content_index: u32,
        partial: AssistantMessage,
    },
    TextDelta {
        content_index: u32,
        delta: String,
        partial: AssistantMessage,
    },
    TextEnd {
        content_index: u32,
        content: String,
        partial: AssistantMessage,
    },
    ThinkingStart {
        content_index: u32,
        partial: AssistantMessage,
    },
    ThinkingDelta {
        content_index: u32,
        delta: String,
        partial: AssistantMessage,
    },
    ThinkingEnd {
        content_index: u32,
        content: String,
        partial: AssistantMessage,
    },
    ToolcallStart {
        content_index: u32,
        partial: AssistantMessage,
    },
    ToolcallDelta {
        content_index: u32,
        delta: String,
        partial: AssistantMessage,
    },
    ToolcallEnd {
        content_index: u32,
        tool_call: ToolCall,
        partial: AssistantMessage,
    },
    Done {
        reason: StopReason,
        message: AssistantMessage,
    },
    Error {
        reason: StopReason,
        error: AssistantMessage,
    },
}

#[cfg(test)]
mod tests {
    use super::super::content::{AssistantContent, TextContent};
    use super::super::ids::{Api, ProviderId};
    use super::super::message::AssistantRole;
    use super::super::usage::{Cost, Usage};
    use super::*;

    fn sample_partial() -> AssistantMessage {
        AssistantMessage {
            role: AssistantRole::Assistant,
            content: vec![AssistantContent::Text(TextContent::new(""))],
            api: Api::from("anthropic-messages"),
            provider: ProviderId::from("anthropic"),
            model: "claude".into(),
            response_model: None,
            response_id: None,
            diagnostics: None,
            usage: Usage {
                input: 0,
                output: 0,
                cache_read: 0,
                cache_write: 0,
                cache_write1h: None,
                reasoning: None,
                total_tokens: Some(0),
                cost: Cost {
                    input: 0.0,
                    output: 0.0,
                    cache_read: 0.0,
                    cache_write: 0.0,
                    total: 0.0,
                },
            },
            stop_reason: StopReason::Stop,
            error_message: None,
            timestamp: 0,
        }
    }

    #[test]
    fn text_delta_tag_and_fields() {
        let ev = AssistantMessageEvent::TextDelta {
            content_index: 0,
            delta: "hi".into(),
            partial: sample_partial(),
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert!(
            json.starts_with(r#"{"type":"text_delta","contentIndex":0,"delta":"hi","partial":"#)
        );
        assert_eq!(
            serde_json::from_str::<AssistantMessageEvent>(&json).unwrap(),
            ev
        );
    }

    #[test]
    fn toolcall_end_tag_is_toolcall_end() {
        let json = serde_json::to_string(&AssistantMessageEvent::Start {
            partial: sample_partial(),
        })
        .unwrap();
        assert!(json.starts_with(r#"{"type":"start","partial":"#));

        // Verify the snake_case tag mapping for the toolcall family without a full value.
        let done = AssistantMessageEvent::Done {
            reason: StopReason::ToolUse,
            message: sample_partial(),
        };
        let djson = serde_json::to_string(&done).unwrap();
        assert!(djson.starts_with(r#"{"type":"done","reason":"toolUse","message":"#));
    }
}
