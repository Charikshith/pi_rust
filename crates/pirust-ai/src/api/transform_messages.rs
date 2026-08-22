//! Rust port of Pi's `packages/ai/src/api/transform-messages.ts` — the
//! cross-provider message normalizer used by the `openai-completions` (and
//! anthropic) adapters before conversion.
//!
//! Behavior preserved 1:1:
//! - Unsupported-image downgrade: outputs a placeholder text for image blocks
//!   when the model cannot see images, coalescing consecutive images.
//! - Per-message transform: drops/plain-texts thinking blocks, re-applies the
//!   same-model/thought-signature rules, strips cross-model `thoughtSignature`,
//!   and normalizes tool-call IDs via a caller-supplied closure.
//! - Second pass: skips error/aborted assistant turns and synthesizes `No result
//!   provided` tool results for orphaned tool calls.

use std::collections::{HashMap, HashSet};

use crate::types::content::{AssistantContent, TextContent, ToolCall, UserContent};
use crate::types::message::{
    AssistantMessage, Message, ToolResultMessage, UserMessage, UserMessageContent,
};
use crate::types::model::Modality;

const NON_VISION_USER_IMAGE_PLACEHOLDER: &str = "(image omitted: model does not support images)";
const NON_VISION_TOOL_IMAGE_PLACEHOLDER: &str =
    "(tool image omitted: model does not support images)";

/// `replaceImagesWithPlaceholder` — replace image blocks with a single
/// placeholder, coalescing runs of images, and preserving the
/// previous-was-placeholder flag when a text block equals the placeholder.
fn replace_images_with_placeholder(
    blocks: Vec<UserContent>,
    placeholder: &str,
) -> Vec<UserContent> {
    let mut result: Vec<UserContent> = Vec::new();
    let mut previous_was_placeholder = false;
    for block in blocks {
        match block {
            UserContent::Image(_) => {
                if !previous_was_placeholder {
                    result.push(UserContent::Text(TextContent::new(placeholder)));
                }
                previous_was_placeholder = true;
            }
            UserContent::Text(text) => {
                previous_was_placeholder = text.text == placeholder;
                result.push(UserContent::Text(text));
            }
        }
    }
    result
}

fn includes_image_modality(model: &crate::types::model::Model) -> bool {
    model.input.contains(&Modality::Image)
}

/// `downgradeUnsupportedImages` — when the model cannot see images, replace
/// image content in user/tool messages with a placeholder.
fn downgrade_unsupported_images(
    messages: Vec<Message>,
    model: &crate::types::model::Model,
) -> Vec<Message> {
    if includes_image_modality(model) {
        return messages;
    }
    messages
        .into_iter()
        .map(|msg| match msg {
            Message::User(user) => {
                let blocks = match user.content {
                    UserMessageContent::Blocks(blocks) => blocks,
                    UserMessageContent::Text(text) => {
                        return Message::User(UserMessage {
                            content: UserMessageContent::Text(text),
                            ..user
                        })
                    }
                };
                Message::User(UserMessage {
                    content: UserMessageContent::Blocks(replace_images_with_placeholder(
                        blocks,
                        NON_VISION_USER_IMAGE_PLACEHOLDER,
                    )),
                    ..user
                })
            }
            Message::ToolResult(tool) => Message::ToolResult(ToolResultMessage {
                content: replace_images_with_placeholder(
                    tool.content,
                    NON_VISION_TOOL_IMAGE_PLACEHOLDER,
                ),
                ..tool
            }),
            other => other,
        })
        .collect()
}

/// Context handed to the tool-call-id normalizer (TS `(id, model, source)`).
pub struct NormalizeCtx<'a> {
    pub model: &'a crate::types::model::Model,
    pub source: &'a AssistantMessage,
}

/// A tool-call-id normalizer `(id, model, source) -> String` (TS
/// `normalizeToolCallId`).
pub trait NormalizeToolCallId: Fn(&str, &NormalizeCtx) -> String + Sync + Send {}
impl<F: Fn(&str, &NormalizeCtx) -> String + Sync + Send> NormalizeToolCallId for F {}

/// `transformMessages` — normalize a message list for a target model. The
/// optional `normalize_tool_call_id` maps a tool-call id (and its source
/// assistant message) to a provider-compatible id.
pub fn transform_messages_with_normalizer(
    model: &crate::types::model::Model,
    messages: Vec<Message>,
    normalize_tool_call_id: Option<&dyn NormalizeToolCallId>,
) -> Vec<Message> {
    let mut id_map: HashMap<String, String> = HashMap::new();
    let image_aware = downgrade_unsupported_images(messages, model);

    // First pass: transform messages (image downgrade already applied; thinking
    // blocks, tool-call ID normalization).
    let transformed: Vec<Message> = image_aware
        .into_iter()
        .map(|msg| match msg {
            Message::User(_) => msg,
            Message::ToolResult(tool) => {
                if let Some(normalized_id) = id_map.get(&tool.tool_call_id) {
                    if *normalized_id != tool.tool_call_id {
                        return Message::ToolResult(ToolResultMessage {
                            tool_call_id: normalized_id.clone(),
                            ..tool
                        });
                    }
                }
                Message::ToolResult(tool)
            }
            Message::Assistant(mut assistant) => {
                let is_same_model = assistant.provider == model.provider
                    && assistant.api == model.api
                    && assistant.model.as_deref() == Some(model.id.as_str());

                let mut transformed_content: Vec<AssistantContent> = Vec::new();
                let content = std::mem::take(&mut assistant.content);
                for block in content {
                    match block {
                        AssistantContent::Thinking(thinking) => {
                            if thinking.redacted == Some(true) {
                                // Redacted thinking is opaque encrypted content, only valid for
                                // the same model. Drop it cross-model to avoid API errors.
                                if is_same_model {
                                    transformed_content.push(AssistantContent::Thinking(thinking));
                                }
                            } else if is_same_model && thinking.thinking_signature.is_some() {
                                // Keep thinking blocks with signatures (needed for replay) even
                                // if the thinking text is empty (encrypted reasoning).
                                transformed_content.push(AssistantContent::Thinking(thinking));
                            } else if thinking.thinking.trim().is_empty() {
                                // Skip empty thinking blocks.
                            } else if is_same_model {
                                transformed_content.push(AssistantContent::Thinking(thinking));
                            } else {
                                // Convert thinking to plain text cross-model.
                                transformed_content.push(AssistantContent::Text(TextContent::new(
                                    thinking.thinking,
                                )));
                            }
                        }
                        AssistantContent::Text(text) => {
                            transformed_content.push(AssistantContent::Text(text));
                        }
                        AssistantContent::ToolCall(tool_call) => {
                            let mut normalized_tool_call = tool_call;
                            if !is_same_model && normalized_tool_call.thought_signature.is_some() {
                                normalized_tool_call.thought_signature = None;
                            }
                            if !is_same_model {
                                if let Some(normalize) = normalize_tool_call_id {
                                    let ctx = NormalizeCtx {
                                        model,
                                        source: &assistant,
                                    };
                                    let normalized_id = normalize(&normalized_tool_call.id, &ctx);
                                    if normalized_id != normalized_tool_call.id {
                                        id_map.insert(
                                            normalized_tool_call.id.clone(),
                                            normalized_id.clone(),
                                        );
                                        normalized_tool_call.id = normalized_id;
                                    }
                                }
                            }
                            transformed_content
                                .push(AssistantContent::ToolCall(normalized_tool_call));
                        }
                    }
                }
                Message::Assistant(AssistantMessage {
                    content: transformed_content,
                    ..assistant
                })
            }
        })
        .collect();

    let mut result: Vec<Message> = Vec::new();
    let mut pending_tool_calls: Vec<ToolCall> = Vec::new();
    let mut existing_tool_result_ids: HashSet<String> = HashSet::new();

    for msg in transformed {
        match &msg {
            Message::Assistant(assistant) => {
                // If we have pending orphaned tool calls from a previous assistant,
                // insert synthetic results now.
                insert_synthetic_tool_results(
                    &mut result,
                    &mut pending_tool_calls,
                    &mut existing_tool_result_ids,
                );
                // Skip errored/aborted assistant turns entirely (incomplete replays).
                if assistant.stop_reason == crate::types::StopReason::Error
                    || assistant.stop_reason == crate::types::StopReason::Aborted
                {
                    continue;
                }
                let tool_calls: Vec<ToolCall> = assistant
                    .content
                    .iter()
                    .filter_map(|b| match b {
                        AssistantContent::ToolCall(tc) => Some(tc.clone()),
                        _ => None,
                    })
                    .collect();
                if !tool_calls.is_empty() {
                    pending_tool_calls = tool_calls;
                    existing_tool_result_ids.clear();
                }
                result.push(msg);
            }
            Message::ToolResult(tool) => {
                existing_tool_result_ids.insert(tool.tool_call_id.clone());
                result.push(msg);
            }
            Message::User(_) => {
                // User message interrupts tool flow — insert synthetic results for orphaned calls.
                insert_synthetic_tool_results(
                    &mut result,
                    &mut pending_tool_calls,
                    &mut existing_tool_result_ids,
                );
                result.push(msg);
            }
        }
    }
    // If the conversation ends with unresolved tool calls, synthesize results now.
    insert_synthetic_tool_results(
        &mut result,
        &mut pending_tool_calls,
        &mut existing_tool_result_ids,
    );
    result
}

fn insert_synthetic_tool_results(
    result: &mut Vec<Message>,
    pending_tool_calls: &mut Vec<ToolCall>,
    existing_tool_result_ids: &mut HashSet<String>,
) {
    if pending_tool_calls.is_empty() {
        return;
    }
    for tc in pending_tool_calls.drain(..) {
        if !existing_tool_result_ids.contains(&tc.id) {
            result.push(Message::ToolResult(ToolResultMessage {
                role: crate::types::message::ToolResultRole::ToolResult,
                tool_call_id: tc.id.clone(),
                tool_name: tc.name.clone(),
                content: vec![UserContent::Text(TextContent::new("No result provided"))],
                details: None,
                added_tool_names: None,
                is_error: true,
                timestamp: timestamp_ms(),
            }));
        }
    }
    existing_tool_result_ids.clear();
}

fn timestamp_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::faux::Faux;
    use crate::types::content::*;
    use crate::types::message::*;
    use crate::types::usage::{Cost, Usage};
    use crate::types::{Api, Modality, ProviderId, StopReason};
    use serde_json::Value;

    fn usage_zero() -> Usage {
        Usage {
            input: 0,
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

    fn model_with_image(supports_image: bool) -> crate::types::model::Model {
        let mut model = Faux::new().get_model().clone();
        if !supports_image {
            model.input = vec![Modality::Text];
        }
        model
    }

    fn user_text(s: &str) -> Message {
        Message::User(UserMessage {
            role: UserRole::User,
            content: UserMessageContent::Text(s.to_string()),
            timestamp: 0,
        })
    }

    fn user_with_image() -> Message {
        Message::User(UserMessage {
            role: UserRole::User,
            content: UserMessageContent::Blocks(vec![
                UserContent::Text(TextContent::new("look")),
                UserContent::Image(ImageContent {
                    kind: ImageTag::Image,
                    data: "abc".into(),
                    mime_type: "image/png".into(),
                }),
                UserContent::Text(TextContent::new("after")),
            ]),
            timestamp: 0,
        })
    }

    #[test]
    fn no_image_model_downgrades_images_with_coalescing() {
        let model = model_with_image(false);
        let out = transform_messages_with_normalizer(&model, vec![user_with_image()], None);
        // "look", "(image omitted...)", "after" — a single coalesced placeholder.
        if let Message::User(u) = &out[0] {
            let blocks = match &u.content {
                UserMessageContent::Blocks(b) => b,
                _ => panic!("expected blocks"),
            };
            assert_eq!(blocks.len(), 3);
            assert_eq!(
                blocks[1],
                UserContent::Text(TextContent::new(NON_VISION_USER_IMAGE_PLACEHOLDER))
            );
        } else {
            panic!("expected user");
        }
    }

    #[test]
    fn image_downgrade_matches_real_pi_oracle() {
        // Ground truth from running real Pi's transformMessages on the same input.
        let model = model_with_image(false);
        let out = transform_messages_with_normalizer(&model, vec![user_with_image()], None);
        let actual = serde_json::to_string(&out).unwrap();
        assert_eq!(
            actual,
            "[{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"look\"},{\"type\":\"text\",\"text\":\"(image omitted: model does not support images)\"},{\"type\":\"text\",\"text\":\"after\"}],\"timestamp\":0}]"
        );
    }

    #[test]
    fn orphaned_tool_result_matches_real_pi_oracle() {
        // Ground truth from running real Pi's transformMessages (assistant with
        // tool call, user interrupts, normalizeToolCallId = "norm_" + id).
        let model = model_with_image(true);
        let assistant_with_tool = Message::Assistant(AssistantMessage {
            role: AssistantRole::Assistant,
            content: vec![AssistantContent::ToolCall(ToolCall {
                kind: ToolCallTag::ToolCall,
                id: "call_1".into(),
                name: "bash".into(),
                arguments: serde_json::Map::new(),
                thought_signature: None,
                partial_json: None,
            })],
            api: Api::from("openai-completions"),
            provider: ProviderId("p".into()),
            model: Some("other".into()),
            response_model: None,
            diagnostics: None,
            usage: usage_zero(),
            stop_reason: StopReason::Stop,
            timestamp: 0,
            response_id: None,
            raw_stop_reason: None,
            error_message: None,
            end_turn: None,
        });
        let out = transform_messages_with_normalizer(
            &model,
            vec![assistant_with_tool, user_text("go")],
            Some(&|id: &str, _ctx: &NormalizeCtx| -> String { format!("norm_{id}") }),
        );
        // Zero out the synthetic timestamp before comparing (nondeterministic).
        let mut out = out;
        if let Message::ToolResult(t) = &mut out[1] {
            t.timestamp = 0;
        }
        let actual = serde_json::to_string(&out).unwrap();
        // Compare structurally (key order is a struct-declaration nuance, not
        // part of the transform's contract; the anthropic adapter's field order
        // governs the Rust struct).
        let expected: Value = serde_json::from_str(
            "[{\"role\":\"assistant\",\"api\":\"openai-completions\",\"provider\":\"p\",\"model\":\"other\",\"content\":[{\"type\":\"toolCall\",\"id\":\"norm_call_1\",\"name\":\"bash\",\"arguments\":{}}],\"usage\":{\"input\":0,\"output\":0,\"cacheRead\":0,\"cacheWrite\":0,\"cost\":{\"input\":0,\"output\":0,\"cacheRead\":0,\"cacheWrite\":0,\"total\":0}},\"stopReason\":\"stop\",\"timestamp\":0},{\"role\":\"toolResult\",\"toolCallId\":\"norm_call_1\",\"toolName\":\"bash\",\"content\":[{\"type\":\"text\",\"text\":\"No result provided\"}],\"isError\":true,\"timestamp\":0},{\"role\":\"user\",\"content\":\"go\",\"timestamp\":0}]",
        )
        .unwrap();
        let actual: Value = serde_json::from_str(&actual).unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn image_model_passes_through() {
        let model = model_with_image(true);
        let out = transform_messages_with_normalizer(&model, vec![user_with_image()], None);
        if let Message::User(u) = &out[0] {
            match &u.content {
                UserMessageContent::Blocks(b) => assert_eq!(b.len(), 3, "unchanged"),
                _ => panic!(),
            }
        }
    }

    #[test]
    fn skips_error_and_aborted_assistant_turns() {
        let model = model_with_image(true);
        let mut err_assistant = AssistantMessage {
            role: AssistantRole::Assistant,
            content: vec![AssistantContent::Text(TextContent::new("partial"))],
            api: Api::from("openai-completions"),
            provider: ProviderId("test".into()),
            model: Some("m".into()),
            response_model: None,
            diagnostics: None,
            usage: usage_zero(),
            stop_reason: StopReason::Error,
            timestamp: 0,
            response_id: None,
            raw_stop_reason: None,
            error_message: None,
            end_turn: None,
        };
        let before = vec![Message::Assistant(err_assistant.clone())];
        let out = transform_messages_with_normalizer(&model, before, None);
        assert!(
            out.is_empty(),
            "errored assistant turn must be dropped, got {out:?}"
        );

        err_assistant.stop_reason = StopReason::Aborted;
        let out = transform_messages_with_normalizer(
            &model,
            vec![Message::Assistant(err_assistant)],
            None,
        );
        assert!(out.is_empty(), "aborted assistant turn must be dropped");
    }

    #[test]
    fn synthesizes_orphaned_tool_results_before_user() {
        let model = model_with_image(true);
        let assistant_with_tool = Message::Assistant(AssistantMessage {
            role: AssistantRole::Assistant,
            content: vec![AssistantContent::ToolCall(ToolCall {
                kind: ToolCallTag::ToolCall,
                id: "call_1".into(),
                name: "bash".into(),
                arguments: serde_json::Map::new(),
                thought_signature: None,
                partial_json: None,
            })],
            api: Api::from("openai-completions"),
            provider: ProviderId("test".into()),
            model: Some("m".into()),
            response_model: None,
            diagnostics: None,
            usage: usage_zero(),
            stop_reason: StopReason::Stop,
            timestamp: 0,
            response_id: None,
            raw_stop_reason: None,
            error_message: None,
            end_turn: None,
        });
        let out = transform_messages_with_normalizer(
            &model,
            vec![assistant_with_tool, user_text("hi")],
            None,
        );
        // assistant(call_1), user interrupts -> synthetic error result for call_1, then user.
        assert_eq!(out.len(), 3);
        match &out[1] {
            Message::ToolResult(t) => {
                assert_eq!(t.tool_call_id, "call_1");
                assert!(t.is_error);
            }
            _ => panic!("expected synthetic tool result"),
        }
    }

    #[test]
    fn normalizes_tool_call_ids_cross_model() {
        let model = model_with_image(true);
        let assistant = Message::Assistant(AssistantMessage {
            role: AssistantRole::Assistant,
            content: vec![AssistantContent::ToolCall(ToolCall {
                kind: ToolCallTag::ToolCall,
                id: "pipe|longitem".into(),
                name: "bash".into(),
                arguments: serde_json::Map::new(),
                thought_signature: None,
                partial_json: None,
            })],
            api: Api::from("openai-completions"),
            provider: ProviderId("other".into()),
            model: Some("x".into()),
            response_model: None,
            diagnostics: None,
            usage: usage_zero(),
            stop_reason: StopReason::Stop,
            timestamp: 0,
            response_id: None,
            raw_stop_reason: None,
            error_message: None,
            end_turn: None,
        });
        let tool_result = Message::ToolResult(ToolResultMessage {
            role: ToolResultRole::ToolResult,
            tool_call_id: "pipe|longitem".into(),
            tool_name: "bash".into(),
            content: vec![UserContent::Text(TextContent::new("ok"))],
            details: None,
            added_tool_names: None,
            is_error: false,
            timestamp: 0,
        });
        let normalizer = |id: &str, _ctx: &NormalizeCtx| -> String { format!("norm_{id}") };
        let out = transform_messages_with_normalizer(
            &model,
            vec![assistant, tool_result],
            Some(&normalizer),
        );
        // The tool call id is normalized in the assistant message AND the tool result.
        match &out[0] {
            Message::Assistant(a) => {
                let tc = match &a.content[0] {
                    AssistantContent::ToolCall(tc) => tc,
                    _ => panic!(),
                };
                assert_eq!(tc.id, "norm_pipe|longitem");
            }
            _ => panic!(),
        }
        match &out[1] {
            Message::ToolResult(t) => assert_eq!(t.tool_call_id, "norm_pipe|longitem"),
            _ => panic!(),
        }
    }
}
