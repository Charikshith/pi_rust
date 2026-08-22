//! Shared utilities for the OpenAI Responses API family — Rust port of
//! `packages/ai/src/api/openai-responses-shared.ts` (792 lines).
//!
//! Three exported functions, all byte-faithful to Pi:
//! - [`convert_responses_messages`] — `Context.messages` → OpenAI Responses `input` items
//!   (user/assistant/toolResult, deferred-tool anchoring, text signatures, grammar
//!   custom-tool calls, tool-search / additional-tools placement).
//! - [`convert_responses_tools`] — `Tool` list → OpenAI `tools` array (grammar custom tools
//!   and JSON-schema strict/function tools).
//! - [`process_responses_stream`] — the deterministic Responses SSE event → event-tape +
//!   final `AssistantMessage` state machine.
//!
//! The OpenAI Responses wire events are modeled as opaque `serde_json::Value` objects (the
//! `ResponseStreamEvent` union); only the fields the state machine reads are pulled out.

use std::collections::HashMap;

use serde_json::{Map, Value};

use crate::api::constrained_sampling::{
    append_grammar_tool_input_json_delta, get_grammar_tool_input, get_json_schema_tool_parameters,
    resolve_grammar_constrained_sampling, resolve_json_schema_strict_sampling,
    GrammarConstrainedSampling, GrammarToolInputJsonBuffer,
};
use crate::api::openai_completions::{sanitize_surrogates, short_hash};
use crate::api::transform_messages::{transform_messages_with_normalizer, NormalizeCtx};
use crate::stream::AssistantMessageSink;
use crate::types::content::{
    AssistantContent, TextContent, ThinkingContent, ToolCall, UserContent,
};
use crate::types::event::AssistantMessageEvent;
use crate::types::ids::{StopReason, TextSignaturePhase, TextSignatureV1};
use crate::types::message::{AssistantMessage, Context, Message, Tool};
use crate::types::model::{Modality, Model};

// =============================================================================
// Text signatures (TS encodeTextSignatureV1 / parseTextSignature)
// =============================================================================

/// `encodeTextSignatureV1(id, phase)` — `{"v":1,"id":...,"phase":...}` (phase omitted
/// when absent).
pub fn encode_text_signature_v1(id: &str, phase: Option<TextSignaturePhase>) -> String {
    serde_json::to_string(&TextSignatureV1 {
        v: 1,
        id: id.to_string(),
        phase,
    })
    .expect("TextSignatureV1 serializes")
}

/// Parsed form of a `textSignature` (TS `parseTextSignature`): versioned JSON when it
/// starts with `{` and parses to `v===1`, else the legacy plain-string id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedTextSignature {
    pub id: String,
    pub phase: Option<TextSignaturePhase>,
}

/// `parseTextSignature` — JSON `{v:1,id,...}` form when it starts with `{`, else the
/// signature string itself is the id.
pub fn parse_text_signature(signature: Option<&str>) -> Option<ParsedTextSignature> {
    let signature = signature?;
    if signature.starts_with('{') {
        if let Ok(parsed) = serde_json::from_str::<Value>(signature) {
            if parsed.get("v").and_then(Value::as_u64) == Some(1) {
                if let Some(id) = parsed.get("id").and_then(Value::as_str) {
                    let phase = match parsed.get("phase").and_then(Value::as_str) {
                        Some("commentary") => Some(TextSignaturePhase::Commentary),
                        Some("final_answer") => Some(TextSignaturePhase::FinalAnswer),
                        _ => None,
                    };
                    return Some(ParsedTextSignature {
                        id: id.to_string(),
                        phase,
                    });
                }
            }
        }
    }
    Some(ParsedTextSignature {
        id: signature.to_string(),
        phase: None,
    })
}

// =============================================================================
// Tool-result output conversion (TS convertToolResultOutput)
// =============================================================================

/// `convertToolResultOutput` — text-only (or image-less model) tool results collapse to a
/// string; image-capable models get a `[{input_text},{input_image}...]` array.
fn convert_tool_result_output(model: &Model, content: &[UserContent]) -> Value {
    let text_result = content
        .iter()
        .filter_map(|c| match c {
            UserContent::Text(t) => Some(t.text.as_str()),
            UserContent::Image(_) => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    let images: Vec<&crate::types::content::ImageContent> = content
        .iter()
        .filter_map(|c| match c {
            UserContent::Image(i) => Some(i),
            UserContent::Text(_) => None,
        })
        .collect();
    let has_text = !text_result.is_empty();

    if images.is_empty() || !model.input.contains(&Modality::Image) {
        let s = if has_text {
            text_result
        } else if !images.is_empty() {
            "(see attached image)".to_string()
        } else {
            "(no tool output)".to_string()
        };
        return Value::String(sanitize_surrogates(&s));
    }

    let mut output: Vec<Value> = Vec::new();
    if has_text {
        let mut obj = Map::new();
        obj.insert("type".into(), Value::String("input_text".into()));
        obj.insert(
            "text".into(),
            Value::String(sanitize_surrogates(&text_result)),
        );
        output.push(Value::Object(obj));
    }
    for image in images {
        let mut obj = Map::new();
        obj.insert("type".into(), Value::String("input_image".into()));
        obj.insert("detail".into(), Value::String("auto".into()));
        obj.insert(
            "image_url".into(),
            Value::String(format!("data:{};base64,{}", image.mime_type, image.data)),
        );
        output.push(Value::Object(obj));
    }
    Value::Array(output)
}

// =============================================================================
// Tool placement (TS splitDeferredTools, deferred-tools.ts)
// =============================================================================

/// The `splitDeferredTools` result: the immediate tools sent in `tools`, and the
/// deferred (transcript-loaded) tools keyed by name.
pub struct ToolPlacement<'a> {
    pub immediate: Vec<&'a Tool>,
    pub deferred: HashMap<String, &'a Tool>,
}

/// `splitDeferredTools(context, enabled)` with identity name normalization (the only form
/// the Responses adapters use). Unique tools by name (first wins); when `enabled`, tools
/// whose `addedToolNames` were never *used* by an earlier assistant tool-call become
/// deferred.
pub fn split_deferred_tools(context: &Context, enabled: bool) -> ToolPlacement<'_> {
    let mut unique: Vec<&Tool> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for tool in context.tools.iter().flatten() {
        if seen.insert(tool.name.as_str()) {
            unique.push(tool);
        }
    }

    if !enabled {
        return ToolPlacement {
            immediate: unique,
            deferred: HashMap::new(),
        };
    }

    let mut used_names: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut deferred_names: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for message in &context.messages {
        match message {
            Message::Assistant(a) => {
                for block in &a.content {
                    if let AssistantContent::ToolCall(tc) = block {
                        used_names.insert(tc.name.as_str());
                    }
                }
            }
            Message::ToolResult(t) => {
                for name in t.added_tool_names.iter().flatten() {
                    if !used_names.contains(name.as_str()) {
                        deferred_names.insert(name.as_str());
                    }
                }
            }
            Message::User(_) => {}
        }
    }

    let mut immediate = Vec::new();
    let mut deferred = HashMap::new();
    for tool in unique {
        if deferred_names.contains(tool.name.as_str()) {
            deferred.insert(tool.name.clone(), tool);
        } else {
            immediate.push(tool);
        }
    }
    ToolPlacement {
        immediate,
        deferred,
    }
}

// =============================================================================
// Message conversion (TS convertResponsesMessages)
// =============================================================================

/// Options for [`convert_responses_messages`] (TS `ConvertResponsesMessagesOptions`).
/// `include_system_prompt` defaults to `true` (TS `options?.includeSystemPrompt ?? true`).
#[derive(Debug, Clone)]
pub struct ConvertResponsesMessagesOptions {
    pub include_system_prompt: bool,
    pub grammar_tool_input_properties: Option<HashMap<String, String>>,
    pub deferred_tools: Option<HashMap<String, Tool>>,
    pub deferred_tools_mode: Option<DeferredToolsMode>,
    pub tool_options: ConvertResponsesToolsOptions,
}

impl Default for ConvertResponsesMessagesOptions {
    fn default() -> Self {
        Self {
            include_system_prompt: true,
            grammar_tool_input_properties: None,
            deferred_tools: None,
            deferred_tools_mode: None,
            tool_options: ConvertResponsesToolsOptions::default(),
        }
    }
}

/// TS `ConvertResponsesToolsOptions`.
#[derive(Debug, Clone, Default)]
pub struct ConvertResponsesToolsOptions {
    /// `strict`: explicit default (codex passes `null`; responses leaves it unset).
    pub strict: Option<Option<bool>>,
    pub supports_strict_mode: bool,
    pub supports_openai_grammar_tools: bool,
    pub defer_loading: bool,
}

/// TS `deferredToolsMode` (`"additional-tools" | "tool-search"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeferredToolsMode {
    AdditionalTools,
    ToolSearch,
}

/// `normalizeIdPart` — sanitize `[^a-zA-Z0-9_-]` → `_`, truncate to 64 code units, strip
/// trailing `_`. All real ids are ASCII; Rust `chars()` matches JS UTF-16 here.
fn normalize_id_part(part: &str) -> String {
    let sanitized: String = part
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let normalized = if sanitized.chars().count() > 64 {
        sanitized.chars().take(64).collect::<String>()
    } else {
        sanitized
    };
    normalized.trim_end_matches('_').to_string()
}

/// `buildForeignResponsesItemId` — `fc_${shortHash(itemId)}` truncated to 64.
fn build_foreign_responses_item_id(item_id: &str) -> String {
    let normalized = format!("fc_{}", short_hash(item_id));
    if normalized.chars().count() > 64 {
        normalized.chars().take(64).collect()
    } else {
        normalized
    }
}

/// `convertResponsesMessages` — normalize a message list into OpenAI Responses `input`
/// items. `allowed_tool_call_providers` gates the pipe-separated id handling (TS
/// `OPENAI_TOOL_CALL_PROVIDERS` / `AZURE_TOOL_CALL_PROVIDERS`).
pub fn convert_responses_messages(
    model: &Model,
    context: &Context,
    allowed_tool_call_providers: &std::collections::HashSet<String>,
    options: Option<&ConvertResponsesMessagesOptions>,
) -> Result<Vec<Value>, String> {
    let options = options.cloned().unwrap_or_default();
    let mut messages: Vec<Value> = Vec::new();
    let mut loaded_tool_names: std::collections::HashSet<String> = std::collections::HashSet::new();

    // TS normalizeToolCallId closure `:139-158`.
    let normalize_tool_call_id = |id: &str, ctx: &NormalizeCtx| -> String {
        if !allowed_tool_call_providers.contains(model.provider.0.as_str()) {
            return normalize_id_part(id);
        }
        if !id.contains('|') {
            return normalize_id_part(id);
        }
        let mut parts = id.splitn(3, '|');
        let call_id = parts.next().unwrap_or("");
        let item_id = parts.next().unwrap_or("");
        let normalized_call_id = normalize_id_part(call_id);
        let is_foreign_tool_call =
            ctx.source.provider != model.provider || ctx.source.api != model.api;
        let mut normalized_item_id = if is_foreign_tool_call {
            build_foreign_responses_item_id(item_id)
        } else {
            normalize_id_part(item_id)
        };
        if !normalized_item_id.starts_with("fc_") {
            normalized_item_id = normalize_id_part(&format!("fc_{normalized_item_id}"));
        }
        format!("{normalized_call_id}|{normalized_item_id}")
    };

    let transformed =
        transform_messages_with_normalizer(model, &context.messages, Some(&normalize_tool_call_id));

    let include_system_prompt = options.include_system_prompt;
    if include_system_prompt {
        if let Some(system_prompt) = &context.system_prompt {
            let supports_developer_role = model
                .compat
                .as_ref()
                .and_then(|c| c.get("supportsDeveloperRole"))
                .and_then(Value::as_bool)
                .unwrap_or(true);
            let role = if model.reasoning && supports_developer_role {
                "developer"
            } else {
                "system"
            };
            let mut obj = Map::new();
            obj.insert("role".into(), Value::String(role.into()));
            obj.insert(
                "content".into(),
                Value::String(sanitize_surrogates(system_prompt)),
            );
            messages.push(Value::Object(obj));
        }
    }

    let mut msg_index = 0usize;
    for msg in &transformed {
        match msg {
            Message::User(user) => match &user.content {
                crate::types::message::UserMessageContent::Text(text) => {
                    let mut role = Map::new();
                    role.insert("role".into(), Value::String("user".into()));
                    role.insert(
                        "content".into(),
                        Value::Array(vec![{
                            let mut c = Map::new();
                            c.insert("type".into(), Value::String("input_text".into()));
                            c.insert("text".into(), Value::String(sanitize_surrogates(text)));
                            Value::Object(c)
                        }]),
                    );
                    messages.push(Value::Object(role));
                }
                crate::types::message::UserMessageContent::Blocks(blocks) => {
                    let mut content: Vec<Value> = Vec::new();
                    for item in blocks {
                        match item {
                            UserContent::Text(t) => {
                                let mut c = Map::new();
                                c.insert("type".into(), Value::String("input_text".into()));
                                c.insert(
                                    "text".into(),
                                    Value::String(sanitize_surrogates(&t.text)),
                                );
                                content.push(Value::Object(c));
                            }
                            UserContent::Image(img) => {
                                let mut c = Map::new();
                                c.insert("type".into(), Value::String("input_image".into()));
                                c.insert("detail".into(), Value::String("auto".into()));
                                c.insert(
                                    "image_url".into(),
                                    Value::String(format!(
                                        "data:{};base64,{}",
                                        img.mime_type, img.data
                                    )),
                                );
                                content.push(Value::Object(c));
                            }
                        }
                    }
                    if content.is_empty() {
                        continue;
                    }
                    let mut role = Map::new();
                    role.insert("role".into(), Value::String("user".into()));
                    role.insert("content".into(), Value::Array(content));
                    messages.push(Value::Object(role));
                }
            },
            Message::Assistant(assistant) => {
                let mut output: Vec<Value> = Vec::new();
                let is_same_provider_and_api =
                    assistant.provider == model.provider && assistant.api == model.api;
                let is_same_model = is_same_provider_and_api
                    && assistant.model.as_deref() == Some(model.id.as_str());
                let is_different_model = is_same_provider_and_api
                    && assistant.model.as_deref() != Some(model.id.as_str());
                let mut text_block_index = 0usize;

                for block in &assistant.content {
                    match block {
                        AssistantContent::Thinking(thinking) => {
                            if let Some(signature) = &thinking.thinking_signature {
                                // TS: `JSON.parse(block.thinkingSignature)` pushed verbatim.
                                if let Ok(reasoning_item) = serde_json::from_str::<Value>(signature)
                                {
                                    output.push(reasoning_item);
                                }
                            }
                        }
                        AssistantContent::Text(text) => {
                            let parsed_signature =
                                parse_text_signature(text.text_signature.as_deref());
                            let fallback_message_id = if text_block_index == 0 {
                                format!("msg_pi_{msg_index}")
                            } else {
                                format!("msg_pi_{msg_index}_{text_block_index}")
                            };
                            text_block_index += 1;
                            let msg_id = match &parsed_signature {
                                Some(p) if p.id.len() > 64 => {
                                    format!("msg_{}", short_hash(&p.id))
                                }
                                Some(p) => p.id.clone(),
                                None => fallback_message_id,
                            };
                            let mut content = Map::new();
                            content.insert("type".into(), Value::String("output_text".into()));
                            content.insert(
                                "text".into(),
                                Value::String(sanitize_surrogates(&text.text)),
                            );
                            content.insert("annotations".into(), Value::Array(vec![]));
                            let mut item = Map::new();
                            item.insert("type".into(), Value::String("message".into()));
                            item.insert("role".into(), Value::String("assistant".into()));
                            item.insert(
                                "content".into(),
                                Value::Array(vec![Value::Object(content)]),
                            );
                            item.insert("status".into(), Value::String("completed".into()));
                            item.insert("id".into(), Value::String(msg_id));
                            if let Some(p) = &parsed_signature {
                                if let Some(phase) = p.phase {
                                    item.insert(
                                        "phase".into(),
                                        Value::String(phase_to_str(phase).into()),
                                    );
                                }
                            }
                            output.push(Value::Object(item));
                        }
                        AssistantContent::ToolCall(tool_call) => {
                            let mut split = tool_call.id.splitn(2, '|');
                            let call_id = split.next().unwrap_or("").to_string();
                            let item_id_raw = split.next().map(String::from);
                            let custom_input_property = options
                                .grammar_tool_input_properties
                                .as_ref()
                                .and_then(|m| m.get(&tool_call.name))
                                .cloned();

                            let mut item_id = item_id_raw;
                            if (is_different_model
                                && item_id.as_deref().is_some_and(|s| s.starts_with("fc_")))
                                || (custom_input_property.is_none()
                                    && !item_id.as_deref().is_some_and(|s| s.starts_with("fc_")))
                            {
                                item_id = None;
                            }

                            let can_replay_namespace = is_same_model
                                || options
                                    .deferred_tools
                                    .as_ref()
                                    .is_some_and(|m| m.contains_key(&tool_call.name));

                            if let Some(prop) = &custom_input_property {
                                let input = get_grammar_tool_input(
                                    &tool_call.name,
                                    &tool_call.arguments,
                                    prop,
                                )?;
                                let mut item = Map::new();
                                item.insert(
                                    "type".into(),
                                    Value::String("custom_tool_call".into()),
                                );
                                if let Some(id) = &item_id {
                                    item.insert("id".into(), Value::String(id.clone()));
                                }
                                item.insert("call_id".into(), Value::String(call_id.clone()));
                                item.insert("name".into(), Value::String(tool_call.name.clone()));
                                item.insert(
                                    "input".into(),
                                    Value::String(sanitize_surrogates(&input)),
                                );
                                if can_replay_namespace {
                                    if let Some(ns) = &tool_call.namespace {
                                        item.insert("namespace".into(), Value::String(ns.clone()));
                                    }
                                }
                                output.push(Value::Object(item));
                            } else {
                                let mut item = Map::new();
                                item.insert("type".into(), Value::String("function_call".into()));
                                if let Some(id) = &item_id {
                                    item.insert("id".into(), Value::String(id.clone()));
                                }
                                item.insert("call_id".into(), Value::String(call_id));
                                item.insert("name".into(), Value::String(tool_call.name.clone()));
                                item.insert(
                                    "arguments".into(),
                                    Value::String(
                                        serde_json::to_string(&tool_call.arguments)
                                            .expect("arguments serialize"),
                                    ),
                                );
                                if can_replay_namespace {
                                    if let Some(ns) = &tool_call.namespace {
                                        item.insert("namespace".into(), Value::String(ns.clone()));
                                    }
                                }
                                output.push(Value::Object(item));
                            }
                        }
                    }
                }
                if output.is_empty() {
                    continue;
                }
                messages.extend(output);
            }
            Message::ToolResult(tool_result) => {
                let call_id = tool_result
                    .tool_call_id
                    .split('|')
                    .next()
                    .unwrap_or("")
                    .to_string();
                let output = convert_tool_result_output(model, &tool_result.content);

                let is_grammar = options
                    .grammar_tool_input_properties
                    .as_ref()
                    .is_some_and(|m| m.contains_key(&tool_result.tool_name));
                let mut item = Map::new();
                if is_grammar {
                    item.insert(
                        "type".into(),
                        Value::String("custom_tool_call_output".into()),
                    );
                } else {
                    item.insert("type".into(), Value::String("function_call_output".into()));
                }
                item.insert("call_id".into(), Value::String(call_id));
                item.insert("output".into(), output);
                messages.push(Value::Object(item));

                // Deferred tools (TS `:319-355`).
                let mut deferred: Vec<&Tool> = Vec::new();
                for name in tool_result.added_tool_names.iter().flatten() {
                    if loaded_tool_names.contains(name) {
                        continue;
                    }
                    if let Some(tool) = options.deferred_tools.as_ref().and_then(|m| m.get(name)) {
                        loaded_tool_names.insert(name.clone());
                        deferred.push(tool);
                    }
                }

                if !deferred.is_empty()
                    && options.deferred_tools_mode == Some(DeferredToolsMode::AdditionalTools)
                {
                    let mut item = Map::new();
                    item.insert("type".into(), Value::String("additional_tools".into()));
                    item.insert("role".into(), Value::String("developer".into()));
                    item.insert(
                        "tools".into(),
                        Value::Array(convert_responses_tools(
                            &deferred.iter().map(|t| (**t).clone()).collect::<Vec<_>>(),
                            &options.tool_options,
                        )?),
                    );
                    messages.push(Value::Object(item));
                } else if !deferred.is_empty()
                    && options.deferred_tools_mode == Some(DeferredToolsMode::ToolSearch)
                {
                    let names = deferred.iter().map(|t| t.name.clone()).collect::<Vec<_>>();
                    let search_call_id = format!(
                        "pi_tool_load_{}",
                        short_hash(&format!("{}:{}", tool_result.tool_call_id, names.join(",")))
                    );
                    let mut call = Map::new();
                    call.insert("type".into(), Value::String("tool_search_call".into()));
                    call.insert("call_id".into(), Value::String(search_call_id.clone()));
                    call.insert("execution".into(), Value::String("client".into()));
                    call.insert("status".into(), Value::String("completed".into()));
                    let mut args = Map::new();
                    args.insert("query".into(), Value::String(names.join(" ")));
                    args.insert("limit".into(), Value::from(names.len()));
                    call.insert("arguments".into(), Value::Object(args));
                    messages.push(Value::Object(call));

                    let mut output_item = Map::new();
                    output_item.insert("type".into(), Value::String("tool_search_output".into()));
                    output_item.insert("call_id".into(), Value::String(search_call_id));
                    output_item.insert("execution".into(), Value::String("client".into()));
                    output_item.insert("status".into(), Value::String("completed".into()));
                    let mut defer_options = options.tool_options.clone();
                    defer_options.defer_loading = true;
                    output_item.insert(
                        "tools".into(),
                        Value::Array(convert_responses_tools(
                            &deferred.iter().map(|t| (**t).clone()).collect::<Vec<_>>(),
                            &defer_options,
                        )?),
                    );
                    messages.push(Value::Object(output_item));
                }
            }
        }
        msg_index += 1;
    }

    Ok(messages)
}

fn phase_to_str(phase: TextSignaturePhase) -> &'static str {
    match phase {
        TextSignaturePhase::Commentary => "commentary",
        TextSignaturePhase::FinalAnswer => "final_answer",
    }
}

// =============================================================================
// Tool conversion (TS convertResponsesTools)
// =============================================================================

/// `convertResponsesTools` — grammar tools become `{type:"custom",...format}`; the rest
/// become `{type:"function",...parameters,strict}`.
pub fn convert_responses_tools(
    tools: &[Tool],
    options: &ConvertResponsesToolsOptions,
) -> Result<Vec<Value>, String> {
    let default_strict = options.strict.flatten().unwrap_or(false);
    let supports_strict_mode = options.supports_strict_mode;
    let supports_openai_grammar_tools = options.supports_openai_grammar_tools;

    tools
        .iter()
        .map(|tool| {
            let grammar =
                resolve_grammar_constrained_sampling(tool, supports_openai_grammar_tools)?;
            if let Some(grammar) = grammar {
                let GrammarConstrainedSampling {
                    format, definition, ..
                } = grammar;
                let syntax = match format {
                    crate::api::constrained_sampling::GrammarFormatChoice::Lark => "lark",
                    crate::api::constrained_sampling::GrammarFormatChoice::Regex => "regex",
                };
                let mut format_obj = Map::new();
                format_obj.insert("type".into(), Value::String("grammar".into()));
                format_obj.insert("syntax".into(), Value::String(syntax.into()));
                format_obj.insert("definition".into(), Value::String(definition));
                let mut item = Map::new();
                item.insert("type".into(), Value::String("custom".into()));
                item.insert("name".into(), Value::String(tool.name.clone()));
                item.insert(
                    "description".into(),
                    Value::String(tool.description.clone()),
                );
                item.insert("format".into(), Value::Object(format_obj));
                if options.defer_loading {
                    item.insert("defer_loading".into(), Value::Bool(true));
                }
                return Ok(Value::Object(item));
            }

            let constrained_strict =
                resolve_json_schema_strict_sampling(tool, supports_strict_mode)?;
            let strict = constrained_strict.unwrap_or(default_strict);
            let parameters =
                get_json_schema_tool_parameters(tool, Some(strict)).map_err(|e| e.0)?;
            let mut item = Map::new();
            item.insert("type".into(), Value::String("function".into()));
            item.insert("name".into(), Value::String(tool.name.clone()));
            item.insert(
                "description".into(),
                Value::String(tool.description.clone()),
            );
            item.insert("parameters".into(), parameters);
            if options.defer_loading {
                item.insert("defer_loading".into(), Value::Bool(true));
            }
            if supports_strict_mode {
                item.insert("strict".into(), Value::Bool(strict));
            }
            Ok(Value::Object(item))
        })
        .collect()
}

// =============================================================================
// Stream processing (TS processResponsesStream)
// =============================================================================

/// Options for [`process_responses_stream`] (TS `OpenAIResponsesStreamOptions`).
#[derive(Debug, Clone, Default)]
pub struct ResponsesStreamOptions {
    /// Requested service tier (TS `serviceTier`).
    pub service_tier: Option<Value>,
    /// Tool name → grammar input property (TS `grammarToolInputProperties`).
    pub grammar_tool_input_properties: Option<HashMap<String, String>>,
    /// Which service-tier resolution/pricing to apply (azure = none).
    pub service_tier_mode: ServiceTierMode,
}

/// Service-tier handling mode. `Disabled` (azure) skips pricing; `OpenAi` and `Codex`
/// share the multiplier but differ in how a `"default"` response tier is resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ServiceTierMode {
    #[default]
    Disabled,
    OpenAi,
    Codex,
}

/// `mapStopReason(status, incompleteReason)` — TS `openai-responses-shared.ts:762-792`.
fn map_responses_stop_reason(
    status: Option<&str>,
    incomplete_reason: Option<&str>,
) -> Result<(StopReason, Option<String>), String> {
    let Some(status) = status else {
        return Ok((StopReason::Stop, None));
    };
    match status {
        "completed" => Ok((StopReason::Stop, None)),
        "incomplete" => {
            if incomplete_reason == Some("max_output_tokens") {
                Ok((StopReason::Length, None))
            } else {
                Ok((
                    StopReason::Error,
                    Some(match incomplete_reason {
                        Some(r) => format!("Response incomplete: {r}"),
                        None => "Response incomplete without a provider reason".to_string(),
                    }),
                ))
            }
        }
        "failed" | "cancelled" => Ok((StopReason::Error, None)),
        "in_progress" | "queued" => Ok((StopReason::Stop, None)),
        other => Err(format!("Unhandled stop reason: {other}")),
    }
}

/// `getServiceTierCostMultiplier` — flex 0.5, priority 2 (2.5 for gpt-5.5), else 1.
fn get_service_tier_cost_multiplier(model: &Model, service_tier: Option<&Value>) -> f64 {
    match service_tier.and_then(Value::as_str) {
        Some("flex") => 0.5,
        Some("priority") => {
            if model.id == "gpt-5.5" {
                2.5
            } else {
                2.0
            }
        }
        _ => 1.0,
    }
}

/// `applyServiceTierPricing` — multiply the cost categories (total is re-summed).
fn apply_service_tier_pricing(
    output: &mut AssistantMessage,
    service_tier: Option<&Value>,
    model: &Model,
) {
    let multiplier = get_service_tier_cost_multiplier(model, service_tier);
    if multiplier == 1.0 {
        return;
    }
    let cost = &mut output.usage.cost;
    cost.input *= multiplier;
    cost.output *= multiplier;
    cost.cache_read *= multiplier;
    cost.cache_write *= multiplier;
    cost.total = cost.input + cost.output + cost.cache_read + cost.cache_write;
}

/// Streaming scratch slot (TS `ResponsesOutputSlot`).
enum OutputSlot {
    Thinking {
        block_idx: usize,
    },
    Text {
        block_idx: usize,
    },
    ToolCall {
        block_idx: usize,
        has_partial_json: bool,
        has_custom_input: bool,
    },
}

/// `processResponsesStream` — fold OpenAI Responses SSE events into the event tape and
/// final `AssistantMessage`. Events are parsed JSON objects (the `type` field selects the
/// arm). `output` is mutated in place; the caller finalizes `done`/`error` like TS.
pub fn process_responses_stream(
    events: impl Iterator<Item = Value>,
    output: &mut AssistantMessage,
    sink: &mut AssistantMessageSink,
    model: &Model,
    options: Option<&ResponsesStreamOptions>,
) -> Result<(), String> {
    let options = options.cloned().unwrap_or_default();
    let mut saw_terminal_response_event = false;
    let mut output_slots: HashMap<u64, OutputSlot> = HashMap::new();
    let mut reasoning_blocks_by_id: HashMap<String, usize> = HashMap::new();
    let mut custom_inputs: HashMap<usize, (String, GrammarToolInputJsonBuffer)> = HashMap::new();

    // applyMessagePhaseStopReason (TS `:443-447`)
    let apply_message_phase_stop_reason = |output: &mut AssistantMessage, item: &Value| {
        if item.get("type").and_then(Value::as_str) == Some("message")
            && item.get("phase").and_then(Value::as_str) == Some("final_answer")
        {
            output.stop_reason = StopReason::Stop;
        }
    };

    for event in events {
        let Some(ev_type) = event.get("type").and_then(Value::as_str) else {
            continue;
        };
        match ev_type {
            "response.created" => {
                if let Some(id) = event
                    .get("response")
                    .and_then(|r| r.get("id"))
                    .and_then(Value::as_str)
                {
                    output.response_id = Some(id.to_string());
                }
            }
            "response.output_item.added" => {
                let Some(output_index) = event.get("output_index").and_then(Value::as_u64) else {
                    continue;
                };
                let Some(item) = event.get("item") else {
                    continue;
                };
                create_responses_slot(
                    output_index,
                    item,
                    output,
                    sink,
                    &mut output_slots,
                    &mut custom_inputs,
                    &options,
                );
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                let Some(output_index) = event.get("output_index").and_then(Value::as_u64) else {
                    continue;
                };
                let Some(delta) = event.get("delta").and_then(Value::as_str) else {
                    continue;
                };
                let Some(slot) = output_slots.get(&output_index) else {
                    continue;
                };
                if let OutputSlot::Thinking { block_idx } = slot {
                    let block_idx = *block_idx;
                    if let AssistantContent::Thinking(t) = &mut output.content[block_idx] {
                        t.thinking.push_str(delta);
                    }
                    sink.push(AssistantMessageEvent::ThinkingDelta {
                        content_index: block_idx as u32,
                        delta: delta.to_string(),
                        partial: output.clone(),
                    });
                }
            }
            "response.reasoning_summary_part.done" => {
                let Some(output_index) = event.get("output_index").and_then(Value::as_u64) else {
                    continue;
                };
                let Some(slot) = output_slots.get(&output_index) else {
                    continue;
                };
                if let OutputSlot::Thinking { block_idx } = slot {
                    let block_idx = *block_idx;
                    if let AssistantContent::Thinking(t) = &mut output.content[block_idx] {
                        t.thinking.push_str("\n\n");
                    }
                    sink.push(AssistantMessageEvent::ThinkingDelta {
                        content_index: block_idx as u32,
                        delta: "\n\n".to_string(),
                        partial: output.clone(),
                    });
                }
            }
            "response.output_text.delta" | "response.refusal.delta" => {
                let Some(output_index) = event.get("output_index").and_then(Value::as_u64) else {
                    continue;
                };
                let Some(delta) = event.get("delta").and_then(Value::as_str) else {
                    continue;
                };
                let Some(slot) = output_slots.get(&output_index) else {
                    continue;
                };
                if let OutputSlot::Text { block_idx } = slot {
                    let block_idx = *block_idx;
                    if let AssistantContent::Text(t) = &mut output.content[block_idx] {
                        t.text.push_str(delta);
                    }
                    sink.push(AssistantMessageEvent::TextDelta {
                        content_index: block_idx as u32,
                        delta: delta.to_string(),
                        partial: output.clone(),
                    });
                }
            }
            "response.function_call_arguments.delta" => {
                let Some(output_index) = event.get("output_index").and_then(Value::as_u64) else {
                    continue;
                };
                let Some(delta) = event.get("delta").and_then(Value::as_str) else {
                    continue;
                };
                let Some(slot) = output_slots.get(&output_index) else {
                    continue;
                };
                if let OutputSlot::ToolCall {
                    block_idx,
                    has_partial_json: true,
                    ..
                } = slot
                {
                    let block_idx = *block_idx;
                    let partial_json = {
                        let AssistantContent::ToolCall(tc) = &mut output.content[block_idx] else {
                            continue;
                        };
                        let partial = tc.partial_json.get_or_insert_with(String::new);
                        partial.push_str(delta);
                        partial.clone()
                    };
                    let parsed = crate::json_repair::parse_streaming_json(&partial_json);
                    if let AssistantContent::ToolCall(tc) = &mut output.content[block_idx] {
                        tc.arguments = parsed.as_object().cloned().unwrap_or_else(Map::new);
                    }
                    sink.push(AssistantMessageEvent::ToolcallDelta {
                        content_index: block_idx as u32,
                        delta: delta.to_string(),
                        partial: output.clone(),
                    });
                }
            }
            "response.function_call_arguments.done" => {
                let Some(output_index) = event.get("output_index").and_then(Value::as_u64) else {
                    continue;
                };
                let Some(arguments) = event.get("arguments").and_then(Value::as_str) else {
                    continue;
                };
                let Some(slot) = output_slots.get(&output_index) else {
                    continue;
                };
                if let OutputSlot::ToolCall {
                    block_idx,
                    has_partial_json: true,
                    ..
                } = slot
                {
                    let block_idx = *block_idx;
                    let previous_partial_json = {
                        let AssistantContent::ToolCall(tc) = &output.content[block_idx] else {
                            continue;
                        };
                        tc.partial_json.clone().unwrap_or_default()
                    };
                    let parsed = crate::json_repair::parse_streaming_json(arguments);
                    if let AssistantContent::ToolCall(tc) = &mut output.content[block_idx] {
                        tc.partial_json = Some(arguments.to_string());
                        tc.arguments = parsed.as_object().cloned().unwrap_or_else(Map::new);
                    }
                    if arguments.starts_with(&previous_partial_json) {
                        let delta = &arguments[previous_partial_json.len()..];
                        if !delta.is_empty() {
                            sink.push(AssistantMessageEvent::ToolcallDelta {
                                content_index: block_idx as u32,
                                delta: delta.to_string(),
                                partial: output.clone(),
                            });
                        }
                    }
                }
            }
            "response.custom_tool_call_input.delta" => {
                let Some(output_index) = event.get("output_index").and_then(Value::as_u64) else {
                    continue;
                };
                let Some(delta) = event.get("delta").and_then(Value::as_str) else {
                    continue;
                };
                let Some(slot) = output_slots.get(&output_index) else {
                    continue;
                };
                if let OutputSlot::ToolCall {
                    block_idx,
                    has_custom_input: true,
                    ..
                } = slot
                {
                    let block_idx = *block_idx;
                    let next =
                        get_custom_tool_call_input(output, &custom_inputs, block_idx) + delta;
                    let delta = append_custom_tool_call_input(
                        output,
                        &mut custom_inputs,
                        block_idx,
                        &next,
                        false,
                    )?;
                    if let Some(delta) = delta {
                        sink.push(AssistantMessageEvent::ToolcallDelta {
                            content_index: block_idx as u32,
                            delta,
                            partial: output.clone(),
                        });
                    }
                }
            }
            "response.custom_tool_call_input.done" => {
                let Some(output_index) = event.get("output_index").and_then(Value::as_u64) else {
                    continue;
                };
                let Some(input) = event.get("input").and_then(Value::as_str) else {
                    continue;
                };
                let Some(slot) = output_slots.get(&output_index) else {
                    continue;
                };
                if let OutputSlot::ToolCall {
                    block_idx,
                    has_custom_input: true,
                    ..
                } = slot
                {
                    let block_idx = *block_idx;
                    let delta = append_custom_tool_call_input(
                        output,
                        &mut custom_inputs,
                        block_idx,
                        input,
                        true,
                    )?;
                    if let Some(delta) = delta {
                        sink.push(AssistantMessageEvent::ToolcallDelta {
                            content_index: block_idx as u32,
                            delta,
                            partial: output.clone(),
                        });
                    }
                }
            }
            "response.output_item.done" => {
                let Some(item) = event.get("item") else {
                    continue;
                };
                apply_message_phase_stop_reason(output, item);
                let Some(output_index) = event.get("output_index").and_then(Value::as_u64) else {
                    continue;
                };
                let item_type = item.get("type").and_then(Value::as_str);
                // getOrCreateSlot (TS `:515-517`): create the slot if not seen (item.done
                // without a prior item.added).
                if !output_slots.contains_key(&output_index) {
                    create_responses_slot(
                        output_index,
                        item,
                        output,
                        sink,
                        &mut output_slots,
                        &mut custom_inputs,
                        &options,
                    );
                }
                let Some(slot) = output_slots.get(&output_index) else {
                    continue;
                };
                match (item_type, slot) {
                    (Some("reasoning"), OutputSlot::Thinking { block_idx }) => {
                        let block_idx = *block_idx;
                        let summary_text = item
                            .get("summary")
                            .and_then(Value::as_array)
                            .map(|s| {
                                s.iter()
                                    .filter_map(|c| c.get("text").and_then(Value::as_str))
                                    .collect::<Vec<_>>()
                                    .join("\n\n")
                            })
                            .unwrap_or_default();
                        let content_text = item
                            .get("content")
                            .and_then(Value::as_array)
                            .map(|c| {
                                c.iter()
                                    .filter_map(|c| c.get("text").and_then(Value::as_str))
                                    .collect::<Vec<_>>()
                                    .join("\n\n")
                            })
                            .unwrap_or_default();
                        let id = item
                            .get("id")
                            .and_then(Value::as_str)
                            .map(String::from)
                            .unwrap_or_default();
                        let signature = serde_json::to_string(item).unwrap_or_default();
                        let (final_thinking, end_content) = {
                            let AssistantContent::Thinking(t) = &mut output.content[block_idx]
                            else {
                                continue;
                            };
                            let existing = t.thinking.clone();
                            let final_thinking = if !summary_text.is_empty() {
                                summary_text
                            } else if !content_text.is_empty() {
                                content_text
                            } else {
                                existing
                            };
                            t.thinking_signature = Some(signature);
                            (final_thinking.clone(), final_thinking)
                        };
                        if let AssistantContent::Thinking(t) = &mut output.content[block_idx] {
                            t.thinking = final_thinking;
                        }
                        if !id.is_empty() {
                            reasoning_blocks_by_id.insert(id, block_idx);
                        }
                        sink.push(AssistantMessageEvent::ThinkingEnd {
                            content_index: block_idx as u32,
                            content: end_content,
                            partial: output.clone(),
                        });
                        output_slots.remove(&output_index);
                    }
                    (Some("message"), OutputSlot::Text { block_idx }) => {
                        let block_idx = *block_idx;
                        let text = item
                            .get("content")
                            .and_then(Value::as_array)
                            .map(|c| {
                                c.iter()
                                    .map(|part| {
                                        let t = part.get("type").and_then(Value::as_str);
                                        if t == Some("output_text") {
                                            part.get("text").and_then(Value::as_str).unwrap_or("")
                                        } else {
                                            part.get("refusal")
                                                .and_then(Value::as_str)
                                                .unwrap_or("")
                                        }
                                    })
                                    .collect::<Vec<_>>()
                                    .join("")
                            })
                            .unwrap_or_default();
                        let id = item.get("id").and_then(Value::as_str).unwrap_or("");
                        let phase = item.get("phase").and_then(Value::as_str);
                        let phase_enum = match phase {
                            Some("commentary") => Some(TextSignaturePhase::Commentary),
                            Some("final_answer") => Some(TextSignaturePhase::FinalAnswer),
                            _ => None,
                        };
                        let signature = encode_text_signature_v1(id, phase_enum);
                        if let AssistantContent::Text(t) = &mut output.content[block_idx] {
                            t.text = text.clone();
                            t.text_signature = Some(signature);
                        }
                        sink.push(AssistantMessageEvent::TextEnd {
                            content_index: block_idx as u32,
                            content: text,
                            partial: output.clone(),
                        });
                        output_slots.remove(&output_index);
                    }
                    (
                        Some("function_call"),
                        OutputSlot::ToolCall {
                            block_idx,
                            has_partial_json: true,
                            ..
                        },
                    ) => {
                        let block_idx = *block_idx;
                        let arguments = item.get("arguments").and_then(Value::as_str).unwrap_or("");
                        let namespace = item
                            .get("namespace")
                            .and_then(Value::as_str)
                            .map(String::from);
                        let parsed = crate::json_repair::parse_streaming_json(arguments);
                        let end_tool_call = {
                            let AssistantContent::ToolCall(tc) = &mut output.content[block_idx]
                            else {
                                continue;
                            };
                            tc.arguments = parsed.as_object().cloned().unwrap_or_else(Map::new);
                            if namespace.is_some() {
                                tc.namespace = namespace;
                            }
                            tc.partial_json = None;
                            tc.clone()
                        };
                        sink.push(AssistantMessageEvent::ToolcallEnd {
                            content_index: block_idx as u32,
                            tool_call: end_tool_call,
                            partial: output.clone(),
                        });
                        output_slots.remove(&output_index);
                    }
                    (
                        Some("custom_tool_call"),
                        OutputSlot::ToolCall {
                            block_idx,
                            has_custom_input: true,
                            ..
                        },
                    ) => {
                        let block_idx = *block_idx;
                        let namespace = item
                            .get("namespace")
                            .and_then(Value::as_str)
                            .map(String::from);
                        let input = item.get("input").and_then(Value::as_str);
                        let delta = if let Some(input) = input {
                            append_custom_tool_call_input(
                                output,
                                &mut custom_inputs,
                                block_idx,
                                input,
                                true,
                            )?
                        } else {
                            let current =
                                get_custom_tool_call_input(output, &custom_inputs, block_idx);
                            append_custom_tool_call_input(
                                output,
                                &mut custom_inputs,
                                block_idx,
                                &current,
                                true,
                            )?
                        };
                        if let Some(delta) = delta {
                            sink.push(AssistantMessageEvent::ToolcallDelta {
                                content_index: block_idx as u32,
                                delta,
                                partial: output.clone(),
                            });
                        }
                        let end_tool_call = {
                            let AssistantContent::ToolCall(tc) = &mut output.content[block_idx]
                            else {
                                continue;
                            };
                            if namespace.is_some() {
                                tc.namespace = namespace;
                            }
                            tc.partial_json = None;
                            tc.clone()
                        };
                        custom_inputs.remove(&block_idx);
                        sink.push(AssistantMessageEvent::ToolcallEnd {
                            content_index: block_idx as u32,
                            tool_call: end_tool_call,
                            partial: output.clone(),
                        });
                        output_slots.remove(&output_index);
                    }
                    _ => {}
                }
            }
            "response.completed" | "response.incomplete" => {
                let Some(response) = event.get("response") else {
                    continue;
                };
                finalize_responses_response(
                    response,
                    output,
                    model,
                    &mut saw_terminal_response_event,
                    &reasoning_blocks_by_id,
                    &options,
                )?;
            }
            "error" => {
                let code = event.get("code").and_then(Value::as_str).unwrap_or("");
                let message = event.get("message").and_then(Value::as_str).unwrap_or("");
                let msg = if message.is_empty() && code.is_empty() {
                    "Unknown error".to_string()
                } else {
                    format!("Error Code {code}: {message}")
                };
                return Err(msg);
            }
            "response.failed" => {
                let response = event.get("response");
                let status = response
                    .and_then(|r| r.get("status"))
                    .and_then(Value::as_str);
                output.raw_stop_reason = status.map(String::from);
                let error = response.and_then(|r| r.get("error"));
                let details = response.and_then(|r| r.get("incomplete_details"));
                let msg = if let Some(error) = error {
                    let code = error
                        .get("code")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown");
                    let message = error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("no message");
                    format!("{code}: {message}")
                } else if let Some(reason) = details
                    .and_then(|d| d.get("reason"))
                    .and_then(Value::as_str)
                {
                    format!("incomplete: {reason}")
                } else {
                    "Unknown error (no error details in response)".to_string()
                };
                return Err(msg);
            }
            _ => {}
        }
    }

    if !saw_terminal_response_event {
        return Err("OpenAI Responses stream ended before a terminal response event".to_string());
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn create_responses_slot(
    output_index: u64,
    item: &Value,
    output: &mut AssistantMessage,
    sink: &mut AssistantMessageSink,
    output_slots: &mut HashMap<u64, OutputSlot>,
    custom_inputs: &mut HashMap<usize, (String, GrammarToolInputJsonBuffer)>,
    options: &ResponsesStreamOptions,
) {
    let Some(item_type) = item.get("type").and_then(Value::as_str) else {
        return;
    };
    match item_type {
        "reasoning" => {
            let block_idx = output.content.len();
            output
                .content
                .push(AssistantContent::Thinking(ThinkingContent {
                    kind: crate::types::content::ThinkingTag::Thinking,
                    thinking: String::new(),
                    thinking_signature: None,
                    redacted: None,
                }));
            output_slots.insert(output_index, OutputSlot::Thinking { block_idx });
            sink.push(AssistantMessageEvent::ThinkingStart {
                content_index: block_idx as u32,
                partial: output.clone(),
            });
        }
        "message" => {
            let block_idx = output.content.len();
            output.content.push(AssistantContent::Text(TextContent {
                kind: crate::types::content::TextTag::Text,
                text: String::new(),
                text_signature: None,
            }));
            output_slots.insert(output_index, OutputSlot::Text { block_idx });
            sink.push(AssistantMessageEvent::TextStart {
                content_index: block_idx as u32,
                partial: output.clone(),
            });
        }
        "function_call" => {
            let call_id = item.get("call_id").and_then(Value::as_str).unwrap_or("");
            let id = item.get("id").and_then(Value::as_str).unwrap_or("");
            let name = item.get("name").and_then(Value::as_str).unwrap_or("");
            let arguments = item.get("arguments").and_then(Value::as_str).unwrap_or("");
            let namespace = item
                .get("namespace")
                .and_then(Value::as_str)
                .map(String::from);
            let block_idx = output.content.len();
            output.content.push(AssistantContent::ToolCall(ToolCall {
                kind: crate::types::content::ToolCallTag::ToolCall,
                id: format!("{call_id}|{id}"),
                name: name.to_string(),
                arguments: Map::new(),
                thought_signature: None,
                namespace,
                partial_json: Some(arguments.to_string()),
            }));
            output_slots.insert(
                output_index,
                OutputSlot::ToolCall {
                    block_idx,
                    has_partial_json: true,
                    has_custom_input: false,
                },
            );
            sink.push(AssistantMessageEvent::ToolcallStart {
                content_index: block_idx as u32,
                partial: output.clone(),
            });
        }
        "custom_tool_call" => {
            let call_id = item.get("call_id").and_then(Value::as_str).unwrap_or("");
            let id = item.get("id").and_then(Value::as_str).unwrap_or("");
            let name = item.get("name").and_then(Value::as_str).unwrap_or("");
            let input = item.get("input").and_then(Value::as_str).unwrap_or("");
            let namespace = item
                .get("namespace")
                .and_then(Value::as_str)
                .map(String::from);
            let input_property = options
                .grammar_tool_input_properties
                .as_ref()
                .and_then(|m| m.get(name))
                .cloned()
                .unwrap_or_else(|| "input".to_string());
            let mut arguments = Map::new();
            arguments.insert(input_property.clone(), Value::String(input.to_string()));
            let block_idx = output.content.len();
            output.content.push(AssistantContent::ToolCall(ToolCall {
                kind: crate::types::content::ToolCallTag::ToolCall,
                id: format!("{call_id}|{id}"),
                name: name.to_string(),
                arguments,
                thought_signature: None,
                namespace,
                partial_json: None,
            }));
            custom_inputs.insert(
                block_idx,
                (
                    input_property,
                    GrammarToolInputJsonBuffer {
                        input: String::new(),
                        started: false,
                        closed: false,
                    },
                ),
            );
            output_slots.insert(
                output_index,
                OutputSlot::ToolCall {
                    block_idx,
                    has_partial_json: false,
                    has_custom_input: true,
                },
            );
            sink.push(AssistantMessageEvent::ToolcallStart {
                content_index: block_idx as u32,
                partial: output.clone(),
            });
        }
        _ => {}
    }
}

/// `getCustomToolCallInput` — the current accumulated input string for a custom-tool block.
fn get_custom_tool_call_input(
    output: &AssistantMessage,
    custom_inputs: &HashMap<usize, (String, GrammarToolInputJsonBuffer)>,
    block_idx: usize,
) -> String {
    let Some((property, _)) = custom_inputs.get(&block_idx) else {
        return String::new();
    };
    match &output.content[block_idx] {
        AssistantContent::ToolCall(tc) => match tc.arguments.get(property) {
            Some(Value::String(s)) => s.clone(),
            _ => String::new(),
        },
        _ => String::new(),
    }
}

/// `appendCustomToolCallInput` — run the grammar json-buffer append and set the block's
/// argument property to the accumulated input.
fn append_custom_tool_call_input(
    output: &mut AssistantMessage,
    custom_inputs: &mut HashMap<usize, (String, GrammarToolInputJsonBuffer)>,
    block_idx: usize,
    next_input: &str,
    close: bool,
) -> Result<Option<String>, String> {
    let Some((property, buffer)) = custom_inputs.get_mut(&block_idx) else {
        return Ok(None);
    };
    let delta = append_grammar_tool_input_json_delta(buffer, property, next_input, close)?;
    if let AssistantContent::ToolCall(tc) = &mut output.content[block_idx] {
        tc.arguments = {
            let mut m = Map::new();
            m.insert(property.clone(), Value::String(next_input.to_string()));
            m
        };
    }
    Ok(delta)
}

#[allow(clippy::too_many_arguments)]
fn finalize_responses_response(
    response: &Value,
    output: &mut AssistantMessage,
    model: &Model,
    saw_terminal_response_event: &mut bool,
    reasoning_blocks_by_id: &HashMap<String, usize>,
    options: &ResponsesStreamOptions,
) -> Result<(), String> {
    *saw_terminal_response_event = true;

    // backfillReasoningSignatures (TS `:518-532`).
    if let Some(output_items) = response.get("output").and_then(Value::as_array) {
        for item in output_items {
            if item.get("type").and_then(Value::as_str) != Some("reasoning") {
                continue;
            }
            let Some(_) = item.get("encrypted_content") else {
                continue;
            };
            let Some(id) = item.get("id").and_then(Value::as_str) else {
                continue;
            };
            let Some(&block_idx) = reasoning_blocks_by_id.get(id) else {
                continue;
            };
            let AssistantContent::Thinking(block) = &mut output.content[block_idx] else {
                continue;
            };
            let Some(signature) = &block.thinking_signature else {
                continue;
            };
            let Ok(mut stored) = serde_json::from_str::<Value>(signature) else {
                continue;
            };
            if stored.get("encrypted_content").is_some() {
                continue;
            }
            if let Some(encrypted) = item.get("encrypted_content") {
                if let Some(obj) = stored.as_object_mut() {
                    obj.insert("encrypted_content".into(), encrypted.clone());
                }
            }
            block.thinking_signature = Some(serde_json::to_string(&stored).unwrap_or_default());
        }
    }

    if let Some(id) = response.get("id").and_then(Value::as_str) {
        output.response_id = Some(id.to_string());
    }

    if let Some(usage) = response.get("usage") {
        let input_details = usage.get("input_tokens_details");
        let cached_tokens = input_details
            .and_then(|d| d.get("cached_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let cache_write_tokens = input_details
            .and_then(|d| d.get("cache_write_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let input_tokens = usage
            .get("input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let output_tokens = usage
            .get("output_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let reasoning = usage
            .get("output_tokens_details")
            .and_then(|d| d.get("reasoning_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let total_tokens = usage
            .get("total_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);

        output.usage = crate::types::usage::Usage {
            input: input_tokens.saturating_sub(cached_tokens + cache_write_tokens),
            output: output_tokens,
            cache_read: cached_tokens,
            cache_write: cache_write_tokens,
            reasoning: Some(reasoning),
            total_tokens: Some(total_tokens),
            cost: crate::types::usage::Cost {
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
                total: 0.0,
            },
            cache_write1h: None,
        };
    }
    crate::api::anthropic_messages::calculate_cost(model, &mut output.usage);

    if options.service_tier_mode != ServiceTierMode::Disabled {
        let response_tier = response.get("service_tier");
        let request_tier = options.service_tier.as_ref();
        let service_tier = match options.service_tier_mode {
            ServiceTierMode::Codex => {
                // resolveCodexServiceTier: "default" → the request tier; else response ?? request.
                match response_tier.and_then(Value::as_str) {
                    Some("default") => request_tier,
                    _ => response_tier.or(request_tier),
                }
            }
            _ => response_tier.or(request_tier),
        };
        apply_service_tier_pricing(output, service_tier, model);
    }

    // status → stop reason (TS `:585-597`).
    let status = response.get("status").and_then(Value::as_str);
    let incomplete_details = response.get("incomplete_details");
    let incomplete_reason = incomplete_details
        .and_then(|d| d.get("reason"))
        .and_then(Value::as_str);
    output.raw_stop_reason = match (status, incomplete_reason) {
        (Some(s), Some(r)) => Some(format!("{s}.{r}")),
        (Some(s), None) => Some(s.to_string()),
        (None, _) => None,
    };
    let (stop_reason, error_message) = map_responses_stop_reason(status, incomplete_reason)?;
    output.stop_reason = stop_reason;
    output.error_message = error_message;
    if output
        .content
        .iter()
        .any(|b| matches!(b, AssistantContent::ToolCall(_)))
        && output.stop_reason == StopReason::Stop
    {
        output.stop_reason = StopReason::ToolUse;
    }
    Ok(())
}

// =============================================================================
// Deferred-tools result (re-export for the adapters)
// =============================================================================

impl<'a> ToolPlacement<'a> {
    /// The deferred tools as owned `Tool`s (used by the adapters to build
    /// `deferred_tools: HashMap<String, Tool>` for the converter).
    pub fn deferred_owned(&self) -> HashMap<String, Tool> {
        self.deferred
            .iter()
            .map(|(k, v)| (k.clone(), (*v).clone()))
            .collect()
    }
}
