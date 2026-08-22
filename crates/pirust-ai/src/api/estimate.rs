//! Port of Pi's context token estimation (`packages/ai/src/utils/estimate.ts`).
//!
//! `estimateContextTokens` feeds `clampMaxTokensToContext` in
//! `simple-options.ts`, which caps the `max_tokens`/`max_completion_tokens`
//! request field. Ported as a pure, borrow-based function over pi's
//! [`Context`] — idiomatic Rust: no mutation, no intermediate arrays, same
//! arithmetic (the byte-exact `max_tokens` contract is pinned by the
//! openai-completions buildParams goldens).
//!
//! Oracle: `../pi/packages/ai/src/utils/estimate.ts`.

use serde_json::Value;

use crate::types::content::AssistantContent;
use crate::types::message::{Context, Message, UserMessageContent};
use crate::types::usage::Usage;

/// Approximate characters contributed by one image block (estimate.ts:7).
const ESTIMATED_IMAGE_CHARS: u64 = 4800;
/// Characters per token (estimate.ts:6).
const CHARS_PER_TOKEN: u64 = 4;

/// `calculateContextTokens` — `usage.totalTokens || input + output + cacheRead + cacheWrite`
/// (estimate.ts:10-12). The JS `||` falls through when `totalTokens` is 0/absent.
pub fn calculate_context_tokens(usage: &Usage) -> u64 {
    match usage.total_tokens {
        Some(t) if t != 0 => t,
        _ => usage.input + usage.output + usage.cache_read + usage.cache_write,
    }
}

/// `safeJsonStringify` (estimate.ts:27-33): compact JSON, `"[unserializable]"`
/// on failure. serde_json emits the same compact form as `JSON.stringify`.
fn safe_json_stringify(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "[unserializable]".to_string())
}

/// JS `.length` semantics — UTF-16 code units (estimate.ts uses `.length`).
fn utf16_len(s: &str) -> u64 {
    s.encode_utf16().count() as u64
}

/// `estimateTextAndImageContentChars` (estimate.ts:34-39).
fn estimate_text_and_image_content_chars(content: &UserMessageContent) -> u64 {
    match content {
        UserMessageContent::Text(s) => utf16_len(s),
        UserMessageContent::Blocks(blocks) => blocks
            .iter()
            .map(|b| match b {
                crate::types::content::UserContent::Text(t) => utf16_len(&t.text),
                crate::types::content::UserContent::Image(_) => ESTIMATED_IMAGE_CHARS,
            })
            .sum(),
    }
}

/// `estimateTextAndImageContentTokens` (estimate.ts:42).
fn estimate_text_and_image_content_tokens(content: &UserMessageContent) -> u64 {
    estimate_text_and_image_content_chars(content).div_ceil(CHARS_PER_TOKEN)
}

/// `estimateTextAndImageContentTokens` (estimate.ts:42) over a block array
/// (user content blocks / tool-result content).
fn estimate_blocks_tokens(blocks: &[crate::types::content::UserContent]) -> u64 {
    let chars: u64 = blocks
        .iter()
        .map(|b| match b {
            crate::types::content::UserContent::Text(t) => utf16_len(&t.text),
            crate::types::content::UserContent::Image(_) => ESTIMATED_IMAGE_CHARS,
        })
        .sum();
    chars.div_ceil(CHARS_PER_TOKEN)
}

/// `estimateMessageTokens` (estimate.ts:45-59): user/toolResult count their
/// content blocks; assistant messages sum text/thinking/toolcall chars.
pub fn estimate_message_tokens(message: &Message) -> u64 {
    match message {
        Message::User(user) => estimate_text_and_image_content_tokens(&user.content),
        Message::ToolResult(tool) => estimate_blocks_tokens(&tool.content),
        Message::Assistant(assistant) => {
            let mut chars = 0u64;
            for block in &assistant.content {
                match block {
                    AssistantContent::Text(t) => chars += utf16_len(&t.text),
                    AssistantContent::Thinking(th) => chars += utf16_len(&th.thinking),
                    AssistantContent::ToolCall(tc) => {
                        chars += utf16_len(&tc.name)
                            + utf16_len(&safe_json_stringify(&Value::Object(tc.arguments.clone())));
                    }
                }
            }
            chars.div_ceil(CHARS_PER_TOKEN)
        }
    }
}

/// `getLastAssistantUsageInfo` (estimate.ts:61-84): the most recent assistant
/// message whose usage describes the current prefix.
fn get_last_assistant_usage(messages: &[Message]) -> Option<(usize, &Usage)> {
    let mut latest_prefix_timestamp = i64::MIN;
    let mut usage_info: Option<(usize, &Usage)> = None;

    for (i, message) in messages.iter().enumerate() {
        if let Message::Assistant(assistant) = message {
            // A newer prefix message inserted after this response (e.g. a compaction
            // summary) means its usage cannot describe the current prefix.
            let usage_applies_to_prefix = assistant.timestamp >= latest_prefix_timestamp;
            if usage_applies_to_prefix
                && assistant.stop_reason != crate::types::ids::StopReason::Aborted
                && assistant.stop_reason != crate::types::ids::StopReason::Error
                && calculate_context_tokens(&assistant.usage) > 0
            {
                usage_info = Some((i, &assistant.usage));
            }
        }
        latest_prefix_timestamp = latest_prefix_timestamp.max(message_timestamp(message));
    }

    usage_info
}

/// Unix timestamp of a message (estimate.ts indexes `message.timestamp`).
fn message_timestamp(message: &Message) -> i64 {
    match message {
        Message::User(user) => user.timestamp,
        Message::Assistant(assistant) => assistant.timestamp,
        Message::ToolResult(tool) => tool.timestamp,
    }
}

/// `estimateMessages` (estimate.ts:86-105).
fn estimate_messages(messages: &[Message]) -> (u64, u64, u64) {
    if let Some((index, usage)) = get_last_assistant_usage(messages) {
        let usage_tokens = calculate_context_tokens(usage);
        let trailing: u64 = messages[index + 1..]
            .iter()
            .map(estimate_message_tokens)
            .sum();
        return (usage_tokens + trailing, usage_tokens, trailing);
    }

    let tokens: u64 = messages.iter().map(estimate_message_tokens).sum();
    (tokens, 0, tokens)
}

/// `estimateToolsTokens` (estimate.ts:107-110).
fn estimate_tools_tokens(tools: Option<&[crate::types::message::Tool]>) -> u64 {
    match tools {
        Some(t) if !t.is_empty() => {
            let json = serde_json::to_string(t).unwrap_or_else(|_| "[unserializable]".to_string());
            utf16_len(&json).div_ceil(CHARS_PER_TOKEN)
        }
        _ => 0,
    }
}

/// `estimateContextTokens` (estimate.ts:113-143) for a full `Context`.
pub fn estimate_context_tokens(context: &Context) -> u64 {
    let (tokens, _usage_tokens, _trailing) = estimate_messages(&context.messages);

    // When the most recent usable usage is present, tool names added after it
    // (deferred tools) contribute their definitions to the trailing estimate.
    if let Some((index, _)) = get_last_assistant_usage(&context.messages) {
        let added_names: std::collections::HashSet<&str> = context.messages[index + 1..]
            .iter()
            .filter_map(|m| match m {
                Message::ToolResult(t) => t.added_tool_names.as_deref(),
                _ => None,
            })
            .flatten()
            .map(|s| s.as_str())
            .collect();
        let added: Vec<crate::types::message::Tool> = context
            .tools
            .as_deref()
            .unwrap_or_default()
            .iter()
            .filter(|t| added_names.contains(t.name.as_str()))
            .cloned()
            .collect();
        let added_tool_tokens = estimate_tools_tokens(Some(&added));
        return tokens + added_tool_tokens;
    }

    let prefix_tokens = (context
        .system_prompt
        .as_deref()
        .map(|p| utf16_len(p).div_ceil(CHARS_PER_TOKEN))
        .unwrap_or(0))
        + estimate_tools_tokens(context.tools.as_deref());

    tokens + prefix_tokens
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ids::{StopReason, ThinkingLevel};
    use crate::types::message::{AssistantMessage, ToolResultMessage, UserMessage};
    use crate::types::usage::Cost;

    fn user(content: &str) -> Message {
        Message::User(UserMessage {
            role: crate::types::message::UserRole::User,
            content: UserMessageContent::Text(content.to_string()),
            timestamp: 1,
        })
    }

    fn tool_result(
        tool_call_id: &str,
        text: &str,
        added_tool_names: Option<Vec<String>>,
    ) -> Message {
        Message::ToolResult(ToolResultMessage {
            role: crate::types::message::ToolResultRole::ToolResult,
            tool_call_id: tool_call_id.to_string(),
            tool_name: String::new(),
            content: vec![crate::types::content::UserContent::Text(
                crate::types::content::TextContent::new(text),
            )],
            details: None,
            added_tool_names,
            is_error: false,
            timestamp: 1,
        })
    }

    fn usage(input: u64) -> Usage {
        Usage {
            input,
            output: 0,
            cache_read: 0,
            cache_write: 0,
            total_tokens: None,
            cost: Cost {
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
                total: 0.0,
            },
            cache_write1h: None,
            reasoning: None,
        }
    }

    #[test]
    fn plain_text_user_messages_estimate_by_chars() {
        // "hi" → ceil(2/4) = 1 token.
        assert_eq!(estimate_message_tokens(&user("hi")), 1);
        assert_eq!(estimate_message_tokens(&user("hello world")), 3); // ceil(11/4)
    }

    #[test]
    fn assistant_usage_cuts_off_the_prefix() {
        // user("hi") + assistant(usage 100) + user("hello") → 100 + ceil(5/4) = 102.
        let assistant = Message::Assistant(AssistantMessage {
            role: crate::types::message::AssistantRole::Assistant,
            content: vec![],
            api: crate::types::ids::Api::from("openai-completions"),
            provider: crate::types::ids::ProviderId("test".into()),
            model: None,
            response_model: None,
            diagnostics: None,
            usage: usage(100),
            stop_reason: StopReason::Stop,
            timestamp: 2,
            response_id: None,
            raw_stop_reason: None,
            error_message: None,
            end_turn: None,
        });
        let messages = vec![user("hi"), assistant, user("hello")];
        let context = Context {
            system_prompt: None,
            messages,
            tools: None,
        };
        assert_eq!(estimate_context_tokens(&context), 102);
    }

    #[test]
    fn tools_add_definitions_when_no_usage() {
        let tool = crate::types::message::Tool {
            name: "base_tool".into(),
            description: "The base_tool tool".into(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["value"],
                "properties": { "value": { "type": "string" } }
            }),
            constrained_sampling: None,
        };
        let context = Context {
            system_prompt: None,
            messages: vec![user("hi")],
            tools: Some(vec![tool]),
        };
        // The oracle pins estimateContextTokens = 39 for exactly this context
        // (1 user token + 38 tool-definition tokens) → the buildParams "together"
        // fixture max_tokens 126937.
        assert_eq!(estimate_context_tokens(&context), 39);
    }

    #[test]
    fn system_prompt_and_tools_count_in_prefix() {
        let tool = crate::types::message::Tool {
            name: "t".into(),
            description: "d".into(),
            parameters: serde_json::json!({ "type": "object" }),
            constrained_sampling: None,
        };
        let context = Context {
            system_prompt: Some("sys".into()),
            messages: vec![user("hi")],
            tools: Some(vec![tool]),
        };
        // prefix = ceil(3/4)=1 (sys) + tools(JSON len/4) ; messages = 1 (user).
        let tokens = estimate_context_tokens(&context);
        assert!(tokens >= 2, "prefix + user, got {tokens}");
    }

    #[test]
    fn deferred_tool_names_add_after_usage() {
        // user + assistant(usage) + toolResult(addedToolNames=[t2]) → 100 + tool(t2) tokens.
        let assistant = Message::Assistant(AssistantMessage {
            role: crate::types::message::AssistantRole::Assistant,
            content: vec![],
            api: crate::types::ids::Api::from("openai-completions"),
            provider: crate::types::ids::ProviderId("test".into()),
            model: None,
            response_model: None,
            diagnostics: None,
            usage: usage(100),
            stop_reason: StopReason::Stop,
            timestamp: 2,
            response_id: None,
            raw_stop_reason: None,
            error_message: None,
            end_turn: None,
        });
        let messages = vec![
            user("hi"),
            assistant,
            tool_result("c1", "done", Some(vec!["t2".into()])),
        ];
        let t2 = crate::types::message::Tool {
            name: "t2".into(),
            description: "d".into(),
            parameters: serde_json::json!({ "type": "object" }),
            constrained_sampling: None,
        };
        let context = Context {
            system_prompt: None,
            messages,
            tools: Some(vec![t2]),
        };
        let tokens = estimate_context_tokens(&context);
        assert!(tokens > 100, "usage + deferred tool tokens, got {tokens}");
        let _ = ThinkingLevel::High;
    }
}
