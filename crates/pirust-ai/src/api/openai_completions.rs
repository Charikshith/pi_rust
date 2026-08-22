//! Rust port of the deterministic helper functions from Pi's
//! `packages/ai/src/api/openai-completions.ts` — the pieces every
//! `openai-completions` provider (cerebras, deepseek, xai, groq, together,
//! openrouter, nvidia, zai, etc.) calls on every streamed chunk / tool call.
//!
//! This module ports the pure, dependency-free helpers that the full stream
//! adapter will call in its event loop, plus the message/tool conversion layer
//! (`convertMessages`/`convertTools`) and the compatibility detection
//! (`detectCompat`/`getCompat`). Their outputs are pinned to Pi's literal
//! outputs via the oracle values captured by running real Pi (see the
//! `#[cfg(test)]` block). The large `stream`/`streamSimple` event loop and
//! `buildParams` remain a later feat-008 wave.
//!
//! Oracle source: `../pi/packages/ai/src/api/openai-completions.ts`,
//! `../pi/packages/ai/src/utils/hash.ts`, `../pi/packages/ai/src/types.ts`.

use std::collections::{HashMap, HashSet};

use futures::StreamExt;

use serde_json::{json, Map, Value};

use crate::api::anthropic_messages::calculate_cost;
use crate::api::constrained_sampling::{
    append_grammar_tool_input_json_delta, create_grammar_tool_input_properties,
    get_grammar_tool_input, get_json_schema_tool_parameters, resolve_grammar_constrained_sampling,
    resolve_json_schema_strict_sampling, GrammarConstrainedSampling, GrammarToolInputJsonBuffer,
};
use crate::api::transform_messages::{transform_messages_with_normalizer, NormalizeCtx};
use crate::api::OpenAICompletionsOptions;
use crate::json_repair::parse_streaming_json;
use crate::stream::{assistant_message_stream, AssistantMessageEventStream, AssistantMessageSink};
use crate::types::content::{
    AssistantContent, TextContent, TextTag, ThinkingContent, ThinkingTag, ToolCall, ToolCallTag,
    UserContent,
};
use crate::types::event::AssistantMessageEvent;
use crate::types::ids::{SessionAffinityFormat, StopReason};
use crate::types::message::{
    AssistantMessage, AssistantRole, Context, Message, UserMessageContent,
};
use crate::types::model::{Modality, Model};
use crate::types::usage::{Cost, Usage};

/// `sanitizeSurrogates` — remove unpaired UTF-16 surrogate code units (TS
/// `utils/sanitize-unicode.ts`). JS strings index by UTF-16 code units; a lone
/// high (0xD800-0xDBFF not followed by a low) or low (0xDC00-0xDFFF not
/// preceded by a high) surrogate is dropped. Valid emoji (paired surrogates)
/// are untouched. Operates on the UTF-16 code-unit sequence, so strings that
/// are not valid UTF-8 in Rust must already have been decoded leniently.
pub fn sanitize_surrogates(text: &str) -> String {
    let units: Vec<u16> = text.encode_utf16().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < units.len() {
        let u = units[i];
        if (0xD800..=0xDBFF).contains(&u) {
            // high surrogate: keep only if followed by a low surrogate
            if i + 1 < units.len() && (0xDC00..=0xDFFF).contains(&units[i + 1]) {
                out.push_str(&String::from_utf16_lossy(&units[i..i + 2]));
                i += 2;
            } else {
                i += 1;
            }
        } else if (0xDC00..=0xDFFF).contains(&u) {
            // lone low surrogate: drop
            i += 1;
        } else {
            out.push_str(&String::from_utf16_lossy(&[u]));
            i += 1;
        }
    }
    out
}

/// TS `ChatCompletionContentPartText` — a text part inside a content array.
pub fn text_part(text: impl Into<String>) -> Value {
    json!({ "type": "text", "text": sanitize_surrogates(&text.into()) })
}

/// TS `ChatCompletionContentPartImage` — `{type, image_url:{url}}` with a
/// data URI assembled from the block's mime type + base64 payload.
pub fn image_part(mime_type: &str, data: &str) -> Value {
    json!({
        "type": "image_url",
        "image_url": { "url": format!("data:{mime_type};base64,{data}") }
    })
}

/// The resolved compatibility settings for an `openai-completions` model
/// (TS `ResolvedOpenAICompletionsCompat` — `Required<OpenAICompletionsCompat>`
/// with the four optional fields re-opened). All fields are always present in
/// the resolved value; the optional subset stays `Option` in Rust only because
/// it mirrors the TS `| undefined` union. `compat` JSON on a [`Model`] maps onto
/// this via [`OpenAICompletionsCompat`].
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedOpenAICompletionsCompat {
    pub supports_store: bool,
    pub supports_developer_role: bool,
    pub supports_reasoning_effort: bool,
    pub supports_usage_in_streaming: bool,
    pub supports_finish_reason: bool,
    pub max_tokens_field: MaxTokensField,
    pub requires_tool_result_name: bool,
    pub requires_assistant_after_tool_result: bool,
    pub requires_thinking_as_text: bool,
    pub requires_reasoning_content_on_assistant_messages: bool,
    pub thinking_format: ThinkingFormat,
    pub open_router_routing: Value,
    pub vercel_gateway_routing: Value,
    pub chat_template_kwargs: Value,
    pub chat_template_args: Value,
    pub zai_tool_stream: bool,
    pub supports_thinking_token_budget: bool,
    pub thinking_token_budget_field: Option<ThinkingTokenBudgetField>,
    pub supports_strict_mode: bool,
    pub supports_openai_grammar_tools: bool,
    pub cache_control_format: Option<CacheControlFormat>,
    pub send_session_affinity_headers: bool,
    pub deferred_tools_mode: Option<DeferredToolsMode>,
    pub session_affinity_format: SessionAffinityFormat,
    pub supports_long_cache_retention: bool,
}

/// TS `OpenAICompletionsCompat` — the optional per-model compat overrides
/// (`Model.compat` for an openai-completions model). Each field, when present,
/// overrides the auto-detected value; `None` means "use detection".
#[derive(Debug, Clone, Default, PartialEq, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct OpenAICompletionsCompat {
    pub supports_store: Option<bool>,
    pub supports_developer_role: Option<bool>,
    pub supports_reasoning_effort: Option<bool>,
    pub supports_usage_in_streaming: Option<bool>,
    pub supports_finish_reason: Option<bool>,
    pub max_tokens_field: Option<MaxTokensField>,
    pub requires_tool_result_name: Option<bool>,
    pub requires_assistant_after_tool_result: Option<bool>,
    pub requires_thinking_as_text: Option<bool>,
    pub requires_reasoning_content_on_assistant_messages: Option<bool>,
    pub thinking_format: Option<ThinkingFormat>,
    pub open_router_routing: Option<Value>,
    pub vercel_gateway_routing: Option<Value>,
    pub chat_template_kwargs: Option<Value>,
    pub chat_template_args: Option<Value>,
    pub zai_tool_stream: Option<bool>,
    pub supports_thinking_token_budget: Option<bool>,
    pub thinking_token_budget_field: Option<ThinkingTokenBudgetField>,
    pub supports_strict_mode: Option<bool>,
    pub supports_openai_grammar_tools: Option<bool>,
    pub cache_control_format: Option<CacheControlFormat>,
    pub send_session_affinity_headers: Option<bool>,
    pub deferred_tools_mode: Option<DeferredToolsMode>,
    pub session_affinity_format: Option<SessionAffinityFormat>,
    pub supports_long_cache_retention: Option<bool>,
}

impl OpenAICompletionsCompat {
    /// Deserialize `model.compat` (opaque JSON) into typed overrides; `None`
    /// or non-object JSON yields `None` (TS: `model.compat` absent → no overrides).
    pub fn from_model_compat(compat: Option<&Value>) -> Option<Self> {
        compat.and_then(|v| v.as_object()).map(|_| {
            serde_json::from_value(compat.cloned().unwrap_or(Value::Null)).unwrap_or_default()
        })
    }
}

/// TS `maxTokensField` union.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaxTokensField {
    MaxCompletionTokens,
    MaxTokens,
}

/// TS `thinkingFormat` union.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ThinkingFormat {
    Openai,
    Openrouter,
    Deepseek,
    Together,
    Baseten,
    Zai,
    Qwen,
    ChatTemplate,
    QwenChatTemplate,
    StringThinking,
    AntLing,
}

/// TS `ThinkingTokenBudgetField` union.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThinkingTokenBudgetField {
    ThinkingTokenBudget,
    ThinkingBudget,
    ThinkingBudgetTokens,
}

/// TS `cacheControlFormat` union.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CacheControlFormat {
    Anthropic,
}

/// TS `deferredToolsMode` union.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeferredToolsMode {
    Kimi,
}

/// Fast deterministic hash to shorten long strings (TS `shortHash`,
/// `utils/hash.ts`). JS uses 32-bit wrapping `Math.imul` and signed `>>> 0`
/// normalization; Rust must use explicit `wrapping_` arithmetic to reproduce
/// the exact bit patterns that feed `toString(36)`.
pub fn short_hash(str: &str) -> String {
    let mut h1: u32 = 0xdead_beef;
    let mut h2: u32 = 0x41c6_ce57;
    for ch in str.encode_utf16() {
        let ch = ch as u32;
        h1 = imul(h1 ^ ch, 2654435761);
        h2 = imul(h2 ^ ch, 1597334677);
    }
    h1 = imul(h1 ^ (h1 >> 16), 2246822507) ^ imul(h2 ^ (h2 >> 13), 3266489909);
    h2 = imul(h2 ^ (h2 >> 16), 2246822507) ^ imul(h1 ^ (h1 >> 13), 3266489909);
    format!("{}{}", to_base36(h2), to_base36(h1))
}

/// `Math.imul(a, b)`: 32-bit wrapping multiplication.
fn imul(a: u32, b: u32) -> u32 {
    a.wrapping_mul(b)
}

/// JS `toString(36)`: base-36 of an unsigned 32-bit value using `0-9a-z`.
fn to_base36(mut value: u32) -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut out = Vec::new();
    loop {
        out.push(DIGITS[(value % 36) as usize]);
        value /= 36;
        if value == 0 {
            break;
        }
    }
    out.reverse();
    String::from_utf8(out).unwrap()
}

/// Map an OpenAI Chat Completions `finish_reason` to Pi's [`StopReason`]
/// (TS `mapStopReason`, `openai-completions.ts`). The error cases carry an
/// explanatory message pinned to Pi's exact wording.
pub fn map_stop_reason(reason: &str) -> (StopReason, Option<String>) {
    match reason {
        "stop" | "end" => (StopReason::Stop, None),
        "length" => (StopReason::Length, None),
        "function_call" | "tool_calls" => (StopReason::ToolUse, None),
        "content_filter" => (
            StopReason::Error,
            Some("Provider finish_reason: content_filter".to_string()),
        ),
        "network_error" => (
            StopReason::Error,
            Some("Provider finish_reason: network_error".to_string()),
        ),
        _ => (
            StopReason::Error,
            Some(format!("Provider finish_reason: {reason}")),
        ),
    }
}

/// Raw usage chunk as emitted by OpenAI-compatible endpoints (the TS inline
/// type in `parseChunkUsage`). The three cache placements are normalized into
/// `cache_read`; `prompt_tokens_details.cached_tokens` wins over
/// `prompt_cache_hit_tokens` and top-level `cached_tokens`.
#[derive(Debug, Clone, Copy, Default)]
pub struct RawChunkUsage {
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub cached_tokens: Option<u64>,
    pub prompt_cache_hit_tokens: Option<u64>,
    pub prompt_details_cached_tokens: Option<u64>,
    pub prompt_details_cache_write_tokens: Option<u64>,
    pub completion_details_reasoning_tokens: Option<u64>,
}

/// Point-in-time raw-usage → [`Usage`], matching TS `parseChunkUsage`
/// (`openai-completions.ts`). Normalizes the three cache-token placements
/// (`prompt_tokens_details.cached_tokens`, `prompt_cache_hit_tokens`, top-level
/// `cached_tokens`) into `cache_read`; computes the billable `input` as
/// `prompt_tokens - cache_read - cache_write` (a non-negative floor); then runs
/// Pi's cost model via [`calculate_cost`]. `reasoning` is the OpenAI
/// `completion_tokens_details.reasoning_tokens` subset of `output`.
pub fn parse_chunk_usage(raw: &RawChunkUsage, model: &Model) -> Usage {
    let prompt_tokens = raw.prompt_tokens.unwrap_or(0);
    let cache_read = raw
        .prompt_details_cached_tokens
        .or(raw.prompt_cache_hit_tokens)
        .or(raw.cached_tokens)
        .unwrap_or(0);
    let cache_write = raw.prompt_details_cache_write_tokens.unwrap_or(0);
    let input = prompt_tokens.saturating_sub(cache_read + cache_write);
    let output = raw.completion_tokens.unwrap_or(0);
    let reasoning = raw.completion_details_reasoning_tokens.unwrap_or(0);
    let mut usage = Usage {
        input,
        output,
        cache_read,
        cache_write,
        total_tokens: Some(input + output + cache_read + cache_write),
        cost: crate::types::usage::Cost {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
            total: 0.0,
        },
        cache_write1h: None,
        reasoning: Some(reasoning),
    };
    calculate_cost(model, &mut usage);
    usage
}

/// TS `hasHeader` — whether a headers record has a non-empty value for a
/// case-insensitive name.
pub fn has_header(headers: Option<&Map<String, Value>>, name: &str) -> bool {
    let Some(headers) = headers else {
        return false;
    };
    let expected = name.to_lowercase();
    headers.iter().any(|(key, value)| {
        key.to_lowercase() == expected
            && !matches!(value, Value::Null)
            && value.as_str().is_none_or(|s| !s.trim().is_empty())
    })
}

/// TS `getClientApiKey` — the provider API key: explicit `api_key` wins, else
/// an Authorization/cf-aig header implies a pre-authenticated client ("unused"),
/// else an error naming the provider.
pub fn get_client_api_key(
    provider: &str,
    api_key: Option<&str>,
    headers: Option<&Map<String, Value>>,
) -> Result<String, String> {
    if let Some(key) = api_key {
        return Ok(key.to_string());
    }
    if has_header(headers, "authorization") || has_header(headers, "cf-aig-authorization") {
        return Ok("unused".to_string());
    }
    Err(format!("No API key for provider: {provider}"))
}

/// TS `hasToolHistory` — whether the conversation contains tool calls or tool
/// results (Anthropic-via-proxy requires the `tools` param when it does).
pub fn has_tool_history(messages: &[Message]) -> bool {
    messages.iter().any(|msg| match msg {
        Message::ToolResult(_) => true,
        Message::Assistant(a) => a
            .content
            .iter()
            .any(|block| matches!(block, AssistantContent::ToolCall(_))),
        _ => false,
    })
}

/// TS `getDeferredToolNames` — the set of tool names made available by tool
/// results (from `addedToolNames`).
pub fn get_deferred_tool_names(messages: &[Message]) -> HashSet<String> {
    let mut names = HashSet::new();
    for msg in messages {
        if let Message::ToolResult(tool) = msg {
            for name in tool.added_tool_names.iter().flatten() {
                names.insert(name.clone());
            }
        }
    }
    names
}

/// TS `getToolsByName` — filter `tools` down to the named ones, preserving
/// declaration order.
pub fn get_tools_by_name<'a>(
    tools: Option<&'a [crate::types::message::Tool]>,
    names: &HashSet<String>,
) -> Vec<&'a crate::types::message::Tool> {
    let Some(tools) = tools else {
        return Vec::new();
    };
    tools
        .iter()
        .filter(|tool| names.contains(&tool.name))
        .collect()
}

/// TS `isTextContentBlock` / `isThinkingContentBlock` / `isToolCallBlock` /
/// `isImageContentBlock` — content-block discriminators.
pub fn is_text_content_block(block: &AssistantContent) -> bool {
    matches!(block, AssistantContent::Text(_))
}
pub fn is_thinking_content_block(block: &AssistantContent) -> bool {
    matches!(block, AssistantContent::Thinking(_))
}
pub fn is_tool_call_block(block: &AssistantContent) -> bool {
    matches!(block, AssistantContent::ToolCall(_))
}
pub fn is_image_content_block(block: &UserContent) -> bool {
    matches!(block, UserContent::Image(_))
}

/// `detectCompat` — auto-detect compatibility settings from provider name and
/// base URL. Used as the base when `model.compat` is not set; explicit
/// `model.compat` entries override these detected values.
pub fn detect_compat(model: &Model) -> ResolvedOpenAICompletionsCompat {
    let provider = &model.provider.0;
    let base_url = &model.base_url;

    let is_zai = provider == "zai"
        || provider == "zai-coding-cn"
        || base_url.contains("api.z.ai")
        || base_url.contains("open.bigmodel.cn");
    let is_together = provider == "together"
        || base_url.contains("api.together.ai")
        || base_url.contains("api.together.xyz");
    let is_moonshot = provider == "moonshotai"
        || provider == "moonshotai-cn"
        || base_url.contains("api.moonshot.");
    let is_open_router = provider == "openrouter" || base_url.contains("openrouter.ai");
    let is_cloudflare_workers_ai =
        provider == "cloudflare-workers-ai" || base_url.contains("api.cloudflare.com");
    let is_cloudflare_ai_gateway =
        provider == "cloudflare-ai-gateway" || base_url.contains("gateway.ai.cloudflare.com");
    let is_nvidia = provider == "nvidia" || base_url.contains("integrate.api.nvidia.com");
    let is_ant_ling = provider == "ant-ling" || base_url.contains("api.ant-ling.com");
    let is_deep_seek = provider == "deepseek" || base_url.to_lowercase().contains("deepseek.com");

    let is_non_standard = is_nvidia
        || provider == "cerebras"
        || base_url.contains("cerebras.ai")
        || provider == "xai"
        || base_url.contains("api.x.ai")
        || is_together
        || base_url.contains("chutes.ai")
        || is_deep_seek
        || is_zai
        || is_moonshot
        || provider == "opencode"
        || base_url.contains("opencode.ai")
        || is_cloudflare_workers_ai
        || is_cloudflare_ai_gateway
        || is_ant_ling;

    let use_max_tokens = base_url.contains("chutes.ai")
        || is_deep_seek
        || is_moonshot
        || is_cloudflare_ai_gateway
        || is_together
        || is_nvidia
        || is_ant_ling
        || is_zai;

    let is_grok = provider == "xai" || base_url.contains("api.x.ai");
    let is_open_router_developer_role_model =
        is_open_router && (model.id.starts_with("anthropic/") || model.id.starts_with("openai/"));
    let cache_control_format = if provider == "openrouter" && model.id.starts_with("anthropic/") {
        Some(CacheControlFormat::Anthropic)
    } else {
        None
    };

    ResolvedOpenAICompletionsCompat {
        supports_store: !is_non_standard,
        supports_developer_role: is_open_router_developer_role_model
            || (!is_non_standard && !is_open_router),
        supports_reasoning_effort: !is_grok
            && !is_zai
            && !is_moonshot
            && !is_together
            && !is_cloudflare_ai_gateway
            && !is_nvidia
            && !is_ant_ling,
        supports_usage_in_streaming: true,
        supports_finish_reason: true,
        max_tokens_field: if use_max_tokens {
            MaxTokensField::MaxTokens
        } else {
            MaxTokensField::MaxCompletionTokens
        },
        requires_tool_result_name: false,
        requires_assistant_after_tool_result: false,
        requires_thinking_as_text: false,
        requires_reasoning_content_on_assistant_messages: is_deep_seek,
        thinking_format: if is_deep_seek {
            ThinkingFormat::Deepseek
        } else if is_zai {
            ThinkingFormat::Zai
        } else if is_together {
            ThinkingFormat::Together
        } else if is_ant_ling {
            ThinkingFormat::AntLing
        } else if is_open_router {
            ThinkingFormat::Openrouter
        } else {
            ThinkingFormat::Openai
        },
        open_router_routing: Value::Object(Map::new()),
        vercel_gateway_routing: Value::Object(Map::new()),
        chat_template_kwargs: Value::Object(Map::new()),
        chat_template_args: Value::Object(Map::new()),
        zai_tool_stream: false,
        supports_thinking_token_budget: false,
        thinking_token_budget_field: None,
        supports_strict_mode: !is_moonshot
            && !is_together
            && !is_cloudflare_ai_gateway
            && !is_nvidia,
        supports_openai_grammar_tools: false,
        cache_control_format,
        send_session_affinity_headers: false,
        deferred_tools_mode: None,
        session_affinity_format: if is_open_router {
            SessionAffinityFormat::Openrouter
        } else {
            SessionAffinityFormat::Openai
        },
        supports_long_cache_retention: !(is_together
            || is_cloudflare_workers_ai
            || is_cloudflare_ai_gateway
            || is_nvidia
            || is_ant_ling),
    }
}

/// `getCompat` — resolve compatibility settings for a model: auto-detect from
/// provider/URL, then overlay explicit `model.compat` values.
pub fn get_compat(model: &Model) -> ResolvedOpenAICompletionsCompat {
    let detected = detect_compat(model);
    let Some(overrides) = OpenAICompletionsCompat::from_model_compat(model.compat.as_ref()) else {
        return detected;
    };

    ResolvedOpenAICompletionsCompat {
        supports_store: overrides.supports_store.unwrap_or(detected.supports_store),
        supports_developer_role: overrides
            .supports_developer_role
            .unwrap_or(detected.supports_developer_role),
        supports_reasoning_effort: overrides
            .supports_reasoning_effort
            .unwrap_or(detected.supports_reasoning_effort),
        supports_usage_in_streaming: overrides
            .supports_usage_in_streaming
            .unwrap_or(detected.supports_usage_in_streaming),
        supports_finish_reason: overrides
            .supports_finish_reason
            .unwrap_or(detected.supports_finish_reason),
        max_tokens_field: overrides
            .max_tokens_field
            .unwrap_or(detected.max_tokens_field),
        requires_tool_result_name: overrides
            .requires_tool_result_name
            .unwrap_or(detected.requires_tool_result_name),
        requires_assistant_after_tool_result: overrides
            .requires_assistant_after_tool_result
            .unwrap_or(detected.requires_assistant_after_tool_result),
        requires_thinking_as_text: overrides
            .requires_thinking_as_text
            .unwrap_or(detected.requires_thinking_as_text),
        requires_reasoning_content_on_assistant_messages: overrides
            .requires_reasoning_content_on_assistant_messages
            .unwrap_or(detected.requires_reasoning_content_on_assistant_messages),
        thinking_format: overrides
            .thinking_format
            .unwrap_or(detected.thinking_format),
        open_router_routing: overrides
            .open_router_routing
            .unwrap_or(detected.open_router_routing),
        vercel_gateway_routing: overrides
            .vercel_gateway_routing
            .unwrap_or(detected.vercel_gateway_routing),
        chat_template_kwargs: overrides
            .chat_template_kwargs
            .unwrap_or(detected.chat_template_kwargs),
        chat_template_args: overrides
            .chat_template_args
            .unwrap_or(detected.chat_template_args),
        zai_tool_stream: overrides
            .zai_tool_stream
            .unwrap_or(detected.zai_tool_stream),
        supports_thinking_token_budget: overrides
            .supports_thinking_token_budget
            .unwrap_or(detected.supports_thinking_token_budget),
        thinking_token_budget_field: overrides
            .thinking_token_budget_field
            .or(detected.thinking_token_budget_field),
        supports_strict_mode: overrides
            .supports_strict_mode
            .unwrap_or(detected.supports_strict_mode),
        supports_openai_grammar_tools: overrides
            .supports_openai_grammar_tools
            .unwrap_or(detected.supports_openai_grammar_tools),
        cache_control_format: overrides
            .cache_control_format
            .or(detected.cache_control_format),
        send_session_affinity_headers: overrides
            .send_session_affinity_headers
            .unwrap_or(detected.send_session_affinity_headers),
        deferred_tools_mode: overrides
            .deferred_tools_mode
            .or(detected.deferred_tools_mode),
        session_affinity_format: overrides
            .session_affinity_format
            .unwrap_or(detected.session_affinity_format),
        supports_long_cache_retention: overrides
            .supports_long_cache_retention
            .unwrap_or(detected.supports_long_cache_retention),
    }
}

/// One converted Chat Completions message param. Kept as JSON [`Value`] so
/// byte order and open-ended provider fields (`reasoning_content`, custom
/// tools, Kimi system-tools) are preserved exactly as Pi serializes them.
pub type ChatCompletionMessageParam = Value;

/// `convertTools` — convert pi `Tool`s to OpenAI Chat Completions tool
/// definitions, applying grammar/strict constrained-sampling resolution.
/// Grammar tools become `{type:"custom",custom:{...}}`; strict JSON-schema
/// tools become `{type:"function", function:{..., strict}}`.
/// `convertTools` — convert pi `Tool`s to OpenAI Chat Completions tool
/// definitions, applying grammar/strict constrained-sampling resolution.
/// Grammar tools become `{type:"custom",custom:{...}}`; strict JSON-schema
/// tools become `{type:"function", function:{..., strict}}`. Returns `Err`
/// with Pi's exact wording when a constrained tool cannot be resolved (TS
/// `resolveGrammarConstrainedSampling`/`resolveJsonSchemaStrictSampling` throw).
pub fn convert_tools(
    tools: &[crate::types::message::Tool],
    compat: &ResolvedOpenAICompletionsCompat,
) -> Result<Vec<Value>, String> {
    tools
        .iter()
        .map(|tool| {
            let grammar =
                resolve_grammar_constrained_sampling(tool, compat.supports_openai_grammar_tools)?;
            if let Some(grammar) = grammar {
                let GrammarConstrainedSampling {
                    format,
                    definition,
                    input_property: _, // not part of the tool def; used for the call's `input`
                } = grammar;
                let syntax = match format {
                    crate::api::constrained_sampling::GrammarFormatChoice::Lark => "lark",
                    crate::api::constrained_sampling::GrammarFormatChoice::Regex => "regex",
                };
                return Ok(json!({
                    "type": "custom",
                    "custom": {
                        "name": tool.name,
                        "description": tool.description,
                        "format": {
                            "type": "grammar",
                            "grammar": { "syntax": syntax, "definition": definition }
                        }
                    }
                }));
            }

            let strict_value =
                resolve_json_schema_strict_sampling(tool, compat.supports_strict_mode)?;
            let parameters =
                get_json_schema_tool_parameters(tool, strict_value).map_err(|e| e.0)?;
            let mut function = Map::new();
            function.insert("name".into(), Value::String(tool.name.clone()));
            function.insert(
                "description".into(),
                Value::String(tool.description.clone()),
            );
            function.insert("parameters".into(), parameters);
            // Only include strict if provider supports it. Some reject unknown fields.
            if compat.supports_strict_mode {
                function.insert("strict".into(), Value::Bool(strict_value.unwrap_or(false)));
            }
            Ok(json!({
                "type": "function",
                "function": function
            }))
        })
        .collect()
}

/// `normalizeToolCallId` — handle pipe-separated IDs from the OpenAI Responses
/// API (`{call_id}|{id}` where `{id}` can be 400+ chars with special chars
/// `+/=`), sanitize to `[a-zA-Z0-9_-]`, combine `call_id_item_id`, and truncate
/// to OpenAI's 40-char limit (hashing when needed).
pub fn normalize_tool_call_id(id: &str, model: &Model) -> String {
    if id.contains('|') {
        let separator_index = id.find('|').unwrap();
        let call_id = sanitize_id_part(&id[..separator_index]);
        let item_id = sanitize_id_part(&id[separator_index + 1..]);
        let combined_id = if !item_id.is_empty() {
            format!("{call_id}_{item_id}")
        } else {
            call_id.clone()
        };
        if combined_id.len() <= 40 {
            return combined_id;
        }
        let hash = short_hash(id);
        let hash = &hash[..hash.len().min(8)];
        let prefix_len = (40usize.saturating_sub(hash.len() + 1)).max(1);
        let prefix = &call_id[..call_id.len().min(prefix_len)];
        return format!("{prefix}_{hash}");
    }
    if model.provider.0 == "openai" && id.chars().count() > 40 {
        // JS truncates at 40 UTF-16 code units, which can split a surrogate
        // pair into a lone surrogate. Rust truncates at 40 chars instead —
        // identical on ASCII ids (the only ids OpenAI emits), and it never
        // produces an invalid code point.
        return id.chars().take(40).collect();
    }
    id.to_string()
}

/// `[^a-zA-Z0-9_-]` → `_` (shared by normalizeToolCallId's two halves).
/// TS's `replace(/[^a-zA-Z0-9_-]/g, "_")` iterates UTF-16 code units, so a
/// non-BMP char becomes two `_`; iterating chars (Rust) would yield one. The
/// halves are compared and truncated by length afterward, so a single `_`
/// instead of two would only matter on pathological ids. Both are `_`-heavy
/// after sanitize; the difference is not observable on any real tool-call id
/// (all ASCII).
fn sanitize_id_part(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// `convertMessages` — convert pi `Context.messages` to OpenAI Chat
/// Completions message params, applying the compat-driven transforms.
/// `grammar_tool_input_properties` maps tool name → the grammar input property
/// (from `createGrammarToolInputProperties`) used to build custom-tool calls.
pub fn convert_messages(
    model: &Model,
    context: &Context,
    compat: &ResolvedOpenAICompletionsCompat,
    grammar_tool_input_properties: Option<&HashMap<String, String>>,
) -> Result<Vec<ChatCompletionMessageParam>, String> {
    let mut params: Vec<ChatCompletionMessageParam> = Vec::new();

    let normalize = |id: &str, _ctx: &NormalizeCtx| -> String { normalize_tool_call_id(id, model) };
    let transformed_messages =
        transform_messages_with_normalizer(model, &context.messages, Some(&normalize));

    if let Some(system_prompt) = &context.system_prompt {
        let use_developer_role = model.reasoning && compat.supports_developer_role;
        let role = if use_developer_role {
            "developer"
        } else {
            "system"
        };
        params.push(json!({
            "role": role,
            "content": sanitize_surrogates(system_prompt),
        }));
    }

    let mut last_role: Option<String> = None;
    let mut i = 0;
    while i < transformed_messages.len() {
        let msg = &transformed_messages[i];

        // Some providers don't allow user messages directly after tool results;
        // insert a synthetic assistant message to bridge the gap.
        if compat.requires_assistant_after_tool_result
            && last_role.as_deref() == Some("toolResult")
            && matches!(msg, Message::User(_))
        {
            params.push(json!({
                "role": "assistant",
                "content": "I have processed the tool results.",
            }));
        }

        match msg {
            Message::User(user) => match &user.content {
                UserMessageContent::Text(text) => {
                    params.push(json!({
                        "role": "user",
                        "content": sanitize_surrogates(text),
                    }));
                }
                UserMessageContent::Blocks(blocks) => {
                    let content: Vec<Value> = blocks
                        .iter()
                        .map(|item| match item {
                            UserContent::Text(text) => text_part(&text.text),
                            UserContent::Image(image) => image_part(&image.mime_type, &image.data),
                        })
                        .collect();
                    if content.is_empty() {
                        i += 1;
                        continue;
                    }
                    params.push(json!({
                        "role": "user",
                        "content": content,
                    }));
                }
            },
            Message::Assistant(assistant) => {
                let mut assistant_msg = Map::new();
                assistant_msg.insert("role".into(), Value::String("assistant".into()));
                // Some providers don't accept null content; use empty string instead.
                assistant_msg.insert(
                    "content".into(),
                    if compat.requires_assistant_after_tool_result {
                        Value::String(String::new())
                    } else {
                        Value::Null
                    },
                );

                let assistant_text_parts: Vec<Value> = assistant
                    .content
                    .iter()
                    .filter(|b| matches!(b, AssistantContent::Text(t) if !t.text.trim().is_empty()))
                    .map(|b| {
                        let AssistantContent::Text(block) = b else {
                            unreachable!()
                        };
                        text_part(&block.text)
                    })
                    .collect();
                let assistant_text: String = assistant
                    .content
                    .iter()
                    .filter_map(|b| match b {
                        AssistantContent::Text(t) if !t.text.trim().is_empty() => {
                            Some(t.text.as_str())
                        }
                        _ => None,
                    })
                    .collect();

                let non_empty_thinking_blocks: Vec<&crate::types::content::ThinkingContent> =
                    assistant
                        .content
                        .iter()
                        .filter_map(|b| match b {
                            AssistantContent::Thinking(t) if !t.thinking.trim().is_empty() => {
                                Some(t)
                            }
                            _ => None,
                        })
                        .collect();

                if !non_empty_thinking_blocks.is_empty() {
                    if compat.requires_thinking_as_text {
                        let thinking_text = non_empty_thinking_blocks
                            .iter()
                            .map(|b| sanitize_surrogates(&b.thinking))
                            .collect::<Vec<_>>()
                            .join("\n\n");
                        let mut content_array = Vec::new();
                        content_array.push(text_part(thinking_text));
                        content_array.extend(assistant_text_parts.iter().cloned());
                        assistant_msg.insert("content".into(), Value::Array(content_array));
                    } else {
                        if !assistant_text.is_empty() {
                            assistant_msg.insert("content".into(), Value::String(assistant_text));
                        }

                        // Use the signature from the first thinking block if
                        // available (llama.cpp server + gpt-oss).
                        let mut signature = non_empty_thinking_blocks[0]
                            .thinking_signature
                            .clone()
                            .unwrap_or_default();
                        if model.provider.0 == "opencode-go" && signature == "reasoning" {
                            signature = "reasoning_content".to_string();
                        }
                        if !signature.is_empty() {
                            let thinking_text = non_empty_thinking_blocks
                                .iter()
                                .map(|b| b.thinking.clone())
                                .collect::<Vec<_>>()
                                .join("\n");
                            assistant_msg.insert(signature, Value::String(thinking_text));
                        }
                    }
                } else if !assistant_text.is_empty() {
                    assistant_msg.insert("content".into(), Value::String(assistant_text));
                }

                let tool_calls: Vec<&ToolCall> = assistant
                    .content
                    .iter()
                    .filter_map(|b| match b {
                        AssistantContent::ToolCall(tc) => Some(tc),
                        _ => None,
                    })
                    .collect();
                if !tool_calls.is_empty() {
                    let tool_call_params: Vec<Value> = tool_calls
                        .iter()
                        .map(|tc| {
                            let custom_input_property = grammar_tool_input_properties
                                .and_then(|props| props.get(&tc.name))
                                .cloned();
                            if let Some(property) = custom_input_property {
                                let input = get_grammar_tool_input(
                                    &tc.name,
                                    &tc.arguments,
                                    &property,
                                )?;
                                return Ok(json!({
                                    "id": tc.id,
                                    "type": "custom",
                                    "custom": {
                                        "name": tc.name,
                                        "input": sanitize_surrogates(&input),
                                    },
                                }));
                            }
                            Ok(json!({
                                "id": tc.id,
                                "type": "function",
                                "function": {
                                    "name": tc.name,
                                    "arguments": serde_json::to_string(&tc.arguments).unwrap_or_default(),
                                },
                            }))
                        })
                        .collect::<Result<Vec<Value>, String>>()?;
                    assistant_msg.insert("tool_calls".into(), Value::Array(tool_call_params));

                    // `reasoning_details` from the JSON-parsed `thoughtSignature`s.
                    let reasoning_details: Vec<Value> = tool_calls
                        .iter()
                        .filter(|tc| tc.thought_signature.is_some())
                        .filter_map(|tc| {
                            serde_json::from_str::<Value>(
                                tc.thought_signature.as_deref().unwrap_or(""),
                            )
                            .ok()
                        })
                        .filter(|v| !v.is_null())
                        .collect();
                    if !reasoning_details.is_empty() {
                        assistant_msg
                            .insert("reasoning_details".into(), Value::Array(reasoning_details));
                    }
                }

                if compat.requires_reasoning_content_on_assistant_messages
                    && model.reasoning
                    && !assistant_msg.contains_key("reasoning_content")
                {
                    assistant_msg.insert("reasoning_content".into(), Value::String(String::new()));
                }

                // Skip assistant messages with no content and no tool calls.
                let has_content = match assistant_msg.get("content") {
                    Some(Value::Null) | None => false,
                    Some(Value::String(s)) => !s.is_empty(),
                    Some(Value::Array(a)) => !a.is_empty(),
                    Some(_) => true,
                };
                if !has_content && !assistant_msg.contains_key("tool_calls") {
                    i += 1;
                    continue;
                }
                params.push(Value::Object(assistant_msg));
            }
            Message::ToolResult(_) => {
                let mut image_blocks: Vec<Value> = Vec::new();
                let mut deferred_tool_names = HashSet::new();
                let mut j = i;

                while j < transformed_messages.len()
                    && matches!(transformed_messages[j], Message::ToolResult(_))
                {
                    let tool_msg = match &transformed_messages[j] {
                        Message::ToolResult(t) => t,
                        _ => unreachable!(),
                    };

                    let text_result: String = tool_msg
                        .content
                        .iter()
                        .filter_map(|c| match c {
                            UserContent::Text(t) => Some(t.text.as_str()),
                            _ => None,
                        })
                        .map(|s| s.to_string())
                        .collect::<Vec<_>>()
                        .join("\n");
                    let has_images = tool_msg
                        .content
                        .iter()
                        .any(|c| matches!(c, UserContent::Image(_)));
                    let has_text = !text_result.is_empty();
                    let tool_result_text = if has_text {
                        text_result
                    } else if has_images {
                        "(see attached image)".to_string()
                    } else {
                        "(no tool output)".to_string()
                    };

                    let mut tool_result_msg = Map::new();
                    tool_result_msg.insert("role".into(), Value::String("tool".into()));
                    tool_result_msg.insert(
                        "content".into(),
                        Value::String(sanitize_surrogates(&tool_result_text)),
                    );
                    tool_result_msg.insert(
                        "tool_call_id".into(),
                        Value::String(tool_msg.tool_call_id.clone()),
                    );
                    if compat.requires_tool_result_name && !tool_msg.tool_name.is_empty() {
                        tool_result_msg
                            .insert("name".into(), Value::String(tool_msg.tool_name.clone()));
                    }
                    params.push(Value::Object(tool_result_msg));

                    if compat.deferred_tools_mode == Some(DeferredToolsMode::Kimi) {
                        for name in tool_msg.added_tool_names.iter().flatten() {
                            deferred_tool_names.insert(name.clone());
                        }
                    }

                    if has_images && model.input.contains(&Modality::Image) {
                        for block in &tool_msg.content {
                            if let UserContent::Image(image) = block {
                                image_blocks.push(image_part(&image.mime_type, &image.data));
                            }
                        }
                    }
                    j += 1;
                }

                i = j - 1;

                if !image_blocks.is_empty() {
                    if compat.requires_assistant_after_tool_result {
                        params.push(json!({
                            "role": "assistant",
                            "content": "I have processed the tool results.",
                        }));
                    }

                    let mut content = Vec::new();
                    content.push(text_part("Attached image(s) from tool result:"));
                    content.extend(image_blocks);
                    params.push(json!({
                        "role": "user",
                        "content": content,
                    }));
                    last_role = Some("user".to_string());
                } else {
                    last_role = Some("toolResult".to_string());
                }

                if !deferred_tool_names.is_empty() {
                    let deferred_tools =
                        get_tools_by_name(context.tools.as_deref(), &deferred_tool_names);
                    if !deferred_tools.is_empty() {
                        let kimi_tools = convert_tools(
                            &deferred_tools
                                .iter()
                                .map(|t| (*t).clone())
                                .collect::<Vec<_>>(),
                            compat,
                        )?;
                        params.push(json!({
                            "role": "system",
                            "tools": kimi_tools,
                        }));
                    }
                }
            }
        }

        if !matches!(msg, Message::ToolResult(_)) {
            last_role = match msg {
                Message::User(_) => Some("user".to_string()),
                Message::Assistant(_) => Some("assistant".to_string()),
                Message::ToolResult(_) => Some("toolResult".to_string()),
            };
        }
        i += 1;
    }

    Ok(params)
}

/// TS `CacheRetention` resolution (`resolveCacheRetention`, :194-201): explicit
/// option wins, else the `PI_CACHE_RETENTION` env var (`"long"` → `Long`), else
/// `Short`.
pub fn resolve_cache_retention(
    cache_retention: Option<crate::types::ids::CacheRetention>,
    env: Option<&std::collections::HashMap<String, String>>,
) -> crate::types::ids::CacheRetention {
    if let Some(retention) = cache_retention {
        return retention;
    }
    let env_value = env
        .and_then(|e| e.get("PI_CACHE_RETENTION"))
        .cloned()
        .or_else(|| std::env::var("PI_CACHE_RETENTION").ok());
    if env_value.as_deref() == Some("long") {
        crate::types::ids::CacheRetention::Long
    } else {
        crate::types::ids::CacheRetention::Short
    }
}

/// `clampOpenAIPromptCacheKey` (`api/openai-prompt-cache.ts`): truncate the
/// session id to 64 chars. JS `Array.from(key).slice(0,64)` is code-point based;
/// Rust `chars().take(64)` is the same (no surrogate splitting).
pub fn clamp_openai_prompt_cache_key(key: Option<&str>) -> Option<String> {
    let key = key?;
    let chars: Vec<char> = key.chars().collect();
    if chars.len() <= 64 {
        Some(key.to_string())
    } else {
        Some(chars.into_iter().take(64).collect())
    }
}

/// `getCompatCacheControl` (:891-898): Anthropic cache control for OpenRouter
/// Anthropic models; `"none"` retention or a non-anthropic format yields `None`.
fn get_compat_cache_control(
    compat: &ResolvedOpenAICompletionsCompat,
    cache_retention: crate::types::ids::CacheRetention,
) -> Option<Value> {
    if compat.cache_control_format != Some(CacheControlFormat::Anthropic)
        || cache_retention == crate::types::ids::CacheRetention::None
    {
        return None;
    }
    let ttl = if cache_retention == crate::types::ids::CacheRetention::Long
        && compat.supports_long_cache_retention
    {
        Some("1h")
    } else {
        None
    };
    let mut obj = serde_json::Map::new();
    obj.insert("type".into(), Value::String("ephemeral".into()));
    if let Some(ttl) = ttl {
        obj.insert("ttl".into(), Value::String(ttl.to_string()));
    }
    Some(Value::Object(obj))
}

/// `addCacheControlToLastTool` (:938-946): attach cache control to the last tool.
fn add_cache_control_to_last_tool(tools: Option<&mut Vec<Value>>, cache_control: &Value) {
    if let Some(tools) = tools {
        if let Some(last) = tools.last_mut() {
            if let Some(obj) = last.as_object_mut() {
                obj.insert("cache_control".into(), cache_control.clone());
            }
        }
    }
}

/// `addCacheControlToLastConversationMessage` (:914-923): walk messages from
/// the end, applying cache control to the first user/assistant/tool message with
/// text content.
fn add_cache_control_to_last_conversation_message(messages: &mut [Value], cache_control: &Value) {
    for message in messages.iter_mut().rev() {
        let role = message.get("role").and_then(|r| r.as_str());
        if matches!(role, Some("user") | Some("assistant") | Some("tool"))
            && add_cache_control_to_text_content(message, cache_control)
        {
            return;
        }
    }
}

/// `addCacheControlToSystemPrompt` (:926-935): apply to the first system/developer message.
fn add_cache_control_to_system_prompt(messages: &mut [Value], cache_control: &Value) {
    for message in messages.iter_mut() {
        let role = message.get("role").and_then(|r| r.as_str());
        if matches!(role, Some("system") | Some("developer")) {
            add_cache_control_to_text_content(message, cache_control);
            return;
        }
    }
}

/// `addCacheControlToTextContent` (:958-993): string content becomes a text part
/// with cache control; array content gets it on the last text part.
fn add_cache_control_to_text_content(message: &mut Value, cache_control: &Value) -> bool {
    let content = message.get_mut("content").map(|c| c.take());
    match content {
        Some(Value::String(s)) if !s.is_empty() => {
            message["content"] =
                json!([{ "type": "text", "text": s, "cache_control": cache_control }]);
            true
        }
        Some(Value::Array(mut parts)) => {
            for part in parts.iter_mut().rev() {
                if part.get("type").and_then(|t| t.as_str()) == Some("text") {
                    if let Some(obj) = part.as_object_mut() {
                        obj.insert("cache_control".into(), cache_control.clone());
                    }
                    message["content"] = Value::Array(parts);
                    return true;
                }
            }
            message["content"] = Value::Array(parts);
            false
        }
        _ => false,
    }
}

/// `thinkingBudgetForLevel` (`simple-options.ts:68-73`) with the default budgets
/// merged under custom ones; `xhigh`/`max` clamp to `high` (JS `clampReasoning`).
fn thinking_budget_for_level(
    reasoning_level: crate::types::ids::ThinkingLevel,
    custom_budgets: Option<&crate::types::ids::ThinkingBudgets>,
) -> u64 {
    let default = crate::types::ids::ThinkingBudgets {
        minimal: Some(1024),
        low: Some(2048),
        medium: Some(8192),
        high: Some(16384),
    };
    let level = match reasoning_level {
        crate::types::ids::ThinkingLevel::Xhigh | crate::types::ids::ThinkingLevel::Max => {
            crate::types::ids::ThinkingLevel::High
        }
        other => other,
    };
    let custom = custom_budgets;
    let value = match level {
        crate::types::ids::ThinkingLevel::Minimal => {
            custom.and_then(|c| c.minimal).or(default.minimal)
        }
        crate::types::ids::ThinkingLevel::Low => custom.and_then(|c| c.low).or(default.low),
        crate::types::ids::ThinkingLevel::Medium => {
            custom.and_then(|c| c.medium).or(default.medium)
        }
        crate::types::ids::ThinkingLevel::High => custom.and_then(|c| c.high).or(default.high),
        crate::types::ids::ThinkingLevel::Xhigh | crate::types::ids::ThinkingLevel::Max => {
            unreachable!()
        }
    };
    value.unwrap_or(0)
}

/// `clampThinkingBudgetToAnswerRoom` (`simple-options.ts:75-77`).
fn clamp_thinking_budget_to_answer_room(thinking_budget: u64, ceiling: u64) -> u64 {
    thinking_budget.min(ceiling.saturating_sub(1024))
}

/// `clampMaxTokensToContext` (`simple-options.ts:9-11`).
pub fn clamp_max_tokens_to_context(model: &Model, context: &Context, max_tokens: u64) -> u64 {
    if model.context_window == 0 {
        return max_tokens.max(1);
    }
    let available = (model.context_window as i64)
        - (crate::api::estimate::estimate_context_tokens(context) as i64)
        - 4096;
    (max_tokens as i64).min(available.max(1)) as u64
}

/// `resolveClampedThinkingBudget` (:900-908).
fn resolve_clamped_thinking_budget(
    model: &Model,
    options: &OpenAICompletionsOptions,
    params: &Value,
) -> Option<u64> {
    if options.reasoning_effort.is_none() || !model.reasoning {
        return None;
    }
    let ceiling = params
        .get("max_tokens")
        .and_then(|v| v.as_u64())
        .or_else(|| params.get("max_completion_tokens").and_then(|v| v.as_u64()))
        .unwrap_or(model.max_tokens);
    let budget = clamp_thinking_budget_to_answer_room(
        thinking_budget_for_level(
            options.reasoning_effort.unwrap(),
            options.thinking_budgets.as_ref(),
        ),
        ceiling,
    );
    (budget > 0).then_some(budget)
}

/// `resolveThinkingTokenBudgetField` (:890-896).
fn resolve_thinking_token_budget_field(
    compat: &ResolvedOpenAICompletionsCompat,
) -> Option<ThinkingTokenBudgetField> {
    if let Some(field) = compat.thinking_token_budget_field {
        return Some(field);
    }
    if compat.supports_thinking_token_budget {
        return Some(ThinkingTokenBudgetField::ThinkingTokenBudget);
    }
    None
}

/// `resolveChatTemplateKwargValue` (:931-949).
fn resolve_chat_template_kwarg_value(
    model: &Model,
    options: &OpenAICompletionsOptions,
    value: &Value,
    thinking_budget: Option<u64>,
) -> Option<Value> {
    if !value.is_object() || value.is_null() {
        return Some(value.clone());
    }
    let reasoning_effort = options.reasoning_effort;
    if reasoning_effort.is_none()
        && value.get("omitWhenOff").and_then(|v| v.as_bool()) == Some(true)
    {
        return None;
    }
    if value.get("$var").and_then(|v| v.as_str()) == Some("thinking.enabled") {
        return Some(Value::Bool(reasoning_effort.is_some()));
    }
    if value.get("$var").and_then(|v| v.as_str()) == Some("thinking.budget") {
        return thinking_budget.map(|b| Value::Number(b.into()));
    }
    let mapped = reasoning_effort
        .and_then(|level| model.thinking_level_map.as_ref().and_then(|m| m.get(level)))
        .or_else(|| {
            reasoning_effort
                .is_none()
                .then(|| {
                    model
                        .thinking_level_map
                        .as_ref()
                        .and_then(|m| m.off_value())
                })
                .flatten()
        });
    match mapped {
        Some(s) => Some(Value::String(s.to_string())),
        None => reasoning_effort.map(|level| {
            let name = match level {
                crate::types::ids::ThinkingLevel::Minimal => "minimal",
                crate::types::ids::ThinkingLevel::Low => "low",
                crate::types::ids::ThinkingLevel::Medium => "medium",
                crate::types::ids::ThinkingLevel::High => "high",
                crate::types::ids::ThinkingLevel::Xhigh => "xhigh",
                crate::types::ids::ThinkingLevel::Max => "max",
            };
            Value::String(name.to_string())
        }),
    }
}

/// `buildChatTemplateValues` (:913-927).
fn build_chat_template_values(
    model: &Model,
    options: &OpenAICompletionsOptions,
    values: &Value,
    thinking_budget: Option<u64>,
) -> Option<Value> {
    let obj = values.as_object()?;
    let mut resolved = serde_json::Map::new();
    for (key, value) in obj {
        if let Some(v) = resolve_chat_template_kwarg_value(model, options, value, thinking_budget) {
            resolved.insert(key.clone(), v);
        }
    }
    if resolved.is_empty() {
        None
    } else {
        Some(Value::Object(resolved))
    }
}

/// `buildParams` (:690-882): assemble the OpenAI Chat Completions streaming
/// request body. Returns the exact JSON Pi emits (key order preserved) so the
/// request bytes match real Pi.
pub fn build_params(
    model: &Model,
    context: &Context,
    options: Option<&OpenAICompletionsOptions>,
    compat: Option<&ResolvedOpenAICompletionsCompat>,
) -> Result<Value, String> {
    let compat = match compat {
        Some(c) => c.clone(),
        None => get_compat(model),
    };
    let owned_options = options.cloned().unwrap_or_default();
    let options = &owned_options;
    let cache_retention =
        resolve_cache_retention(options.base.cache_retention, options.base.env.as_ref());
    let grammar_tool_input_properties = create_grammar_tool_input_properties(
        context.tools.as_deref(),
        compat.supports_openai_grammar_tools,
    );
    let messages = convert_messages(
        model,
        context,
        &compat,
        Some(&grammar_tool_input_properties),
    )?;
    let cache_control = get_compat_cache_control(&compat, cache_retention);

    let mut messages = messages;
    if let Some(cc) = &cache_control {
        add_cache_control_to_system_prompt(&mut messages, cc);
    }

    let mut params = serde_json::Map::new();
    params.insert("model".into(), Value::String(model.id.clone()));
    params.insert("messages".into(), Value::Array(messages.clone()));
    params.insert("stream".into(), Value::Bool(true));

    // prompt_cache_key / prompt_cache_retention (literal order in TS).
    let has_openai_base = model.base_url.contains("api.openai.com");
    let cache_key = if (has_openai_base
        && cache_retention != crate::types::ids::CacheRetention::None)
        || (cache_retention == crate::types::ids::CacheRetention::Long
            && compat.supports_long_cache_retention)
    {
        clamp_openai_prompt_cache_key(options.base.session_id.as_deref())
    } else {
        None
    };
    if let Some(key) = cache_key {
        params.insert("prompt_cache_key".into(), Value::String(key));
    }
    if cache_retention == crate::types::ids::CacheRetention::Long
        && compat.supports_long_cache_retention
    {
        params.insert("prompt_cache_retention".into(), Value::String("24h".into()));
    }

    if compat.supports_usage_in_streaming {
        params.insert("stream_options".into(), json!({ "include_usage": true }));
    }

    if compat.supports_store {
        params.insert("store".into(), Value::Bool(false));
    }

    if let Some(max_tokens) = options.base.max_tokens {
        match compat.max_tokens_field {
            MaxTokensField::MaxTokens => {
                params.insert("max_tokens".into(), Value::Number(max_tokens.into()));
            }
            MaxTokensField::MaxCompletionTokens => {
                params.insert(
                    "max_completion_tokens".into(),
                    Value::Number(max_tokens.into()),
                );
            }
        }
    }

    if let Some(temperature) = options.base.temperature {
        params.insert(
            "temperature".into(),
            Value::Number(
                serde_json::Number::from_f64(temperature).unwrap_or(serde_json::Number::from(0)),
            ),
        );
    }

    let deferred_tool_names = if compat.deferred_tools_mode == Some(DeferredToolsMode::Kimi) {
        get_deferred_tool_names(&context.messages)
    } else {
        HashSet::new()
    };
    let active_tools: Vec<&crate::types::message::Tool> = context
        .tools
        .as_deref()
        .unwrap_or_default()
        .iter()
        .filter(|t| !deferred_tool_names.contains(&t.name))
        .collect();
    if !active_tools.is_empty() {
        let converted = convert_tools(
            &active_tools
                .iter()
                .map(|t| (*t).clone())
                .collect::<Vec<_>>(),
            &compat,
        )?;
        let mut converted = converted;
        if let Some(cc) = &cache_control {
            add_cache_control_to_last_tool(Some(&mut converted), cc);
        }
        params.insert("tools".into(), Value::Array(converted));
        if compat.zai_tool_stream {
            params.insert("tool_stream".into(), Value::Bool(true));
        }
    } else if has_tool_history(&context.messages) {
        // Anthropic (via LiteLLM/proxy) requires tools param when conversation has tool_calls/tool_results.
        params.insert("tools".into(), Value::Array(Vec::new()));
    }

    // Cache control: last user/assistant/tool message (TS applies to `messages`
    // array after tools are added).
    if let Some(cc) = &cache_control {
        add_cache_control_to_last_conversation_message(&mut messages, cc);
        if let Some(arr) = params.get_mut("messages").and_then(|m| m.as_array_mut()) {
            *arr = messages.clone();
        }
    }

    if let Some(tool_choice) = &options.tool_choice {
        params.insert("tool_choice".into(), tool_choice.clone());
    }

    let thinking_token_budget_field = resolve_thinking_token_budget_field(&compat);
    let thinking_budget =
        resolve_clamped_thinking_budget(model, options, &Value::Object(params.clone()));

    // Thinking-format branches (mutate params in TS).
    if compat.thinking_format == ThinkingFormat::Zai && model.reasoning {
        let thinking = if options.reasoning_effort.is_some() {
            json!({ "type": "enabled", "clear_thinking": false })
        } else {
            json!({ "type": "disabled" })
        };
        params.insert("thinking".into(), thinking);
        if options.reasoning_effort.is_some() && compat.supports_reasoning_effort {
            let mapped = options
                .reasoning_effort
                .and_then(|level| model.thinking_level_map.as_ref().and_then(|m| m.get(level)));
            let effort = mapped.map(|s| s.to_string()).unwrap_or_else(|| {
                options
                    .reasoning_effort
                    .map(|l| {
                        let name = match l {
                            crate::types::ids::ThinkingLevel::Minimal => "minimal",
                            crate::types::ids::ThinkingLevel::Low => "low",
                            crate::types::ids::ThinkingLevel::Medium => "medium",
                            crate::types::ids::ThinkingLevel::High => "high",
                            crate::types::ids::ThinkingLevel::Xhigh => "xhigh",
                            crate::types::ids::ThinkingLevel::Max => "max",
                        };
                        name.to_string()
                    })
                    .unwrap_or_default()
            });
            if !effort.is_empty() {
                params.insert("reasoning_effort".into(), Value::String(effort));
            }
        }
    } else if compat.thinking_format == ThinkingFormat::Qwen && model.reasoning {
        params.insert(
            "enable_thinking".into(),
            Value::Bool(options.reasoning_effort.is_some()),
        );
        if options.reasoning_effort.is_some() && compat.supports_reasoning_effort {
            let effort = options
                .reasoning_effort
                .and_then(|level| model.thinking_level_map.as_ref().and_then(|m| m.get(level)))
                .map(|s| s.to_string())
                .or_else(|| options.reasoning_effort.map(|l| level_name(l).to_string()));
            if let Some(e) = effort {
                params.insert("reasoning_effort".into(), Value::String(e));
            }
        }
    } else if compat.thinking_format == ThinkingFormat::QwenChatTemplate && model.reasoning {
        params.insert(
            "chat_template_kwargs".into(),
            json!({ "enable_thinking": options.reasoning_effort.is_some(), "preserve_thinking": true }),
        );
    } else if compat.thinking_format == ThinkingFormat::ChatTemplate && model.reasoning {
        if let Some(values) = build_chat_template_values(
            model,
            options,
            &compat.chat_template_kwargs,
            thinking_budget,
        ) {
            params.insert("chat_template_kwargs".into(), values);
        }
    } else if compat.thinking_format == ThinkingFormat::Baseten && model.reasoning {
        if let Some(args) =
            build_chat_template_values(model, options, &compat.chat_template_args, thinking_budget)
        {
            params.insert("chat_template_args".into(), args);
        }
        if compat.supports_reasoning_effort {
            let requested = options.reasoning_effort;
            let mapped = requested
                .and_then(|level| model.thinking_level_map.as_ref().and_then(|m| m.get(level)))
                .or_else(|| {
                    requested
                        .is_none()
                        .then(|| {
                            model
                                .thinking_level_map
                                .as_ref()
                                .and_then(|m| m.off_value())
                        })
                        .flatten()
                });
            let effort = mapped
                .map(|s| s.to_string())
                .or_else(|| requested.map(|l| level_name(l).to_string()));
            if let Some(e) = effort {
                params.insert("reasoning_effort".into(), Value::String(e));
            }
        }
    } else if compat.thinking_format == ThinkingFormat::Deepseek && model.reasoning {
        if options.reasoning_effort.is_some() {
            params.insert("thinking".into(), json!({ "type": "enabled" }));
        } else if model
            .thinking_level_map
            .as_ref()
            .is_none_or(|m| m.off.is_none())
        {
            // JS: `model.thinkingLevelMap?.off !== null` — absent map or absent `off`
            // passes (undefined !== null); explicit `off: null` blocks the disable.
            params.insert("thinking".into(), json!({ "type": "disabled" }));
        }
        if options.reasoning_effort.is_some() && compat.supports_reasoning_effort {
            let effort = options
                .reasoning_effort
                .and_then(|level| model.thinking_level_map.as_ref().and_then(|m| m.get(level)))
                .map(|s| s.to_string())
                .or_else(|| options.reasoning_effort.map(|l| level_name(l).to_string()));
            if let Some(e) = effort {
                params.insert("reasoning_effort".into(), Value::String(e));
            }
        }
    } else if compat.thinking_format == ThinkingFormat::Openrouter && model.reasoning {
        if options.reasoning_effort.is_some() {
            let effort = options
                .reasoning_effort
                .and_then(|level| model.thinking_level_map.as_ref().and_then(|m| m.get(level)))
                .map(|s| s.to_string())
                .unwrap_or_else(|| level_name(options.reasoning_effort.unwrap()).to_string());
            params.insert("reasoning".into(), json!({ "effort": effort }));
        } else if model
            .thinking_level_map
            .as_ref()
            .is_none_or(|m| m.off.is_none())
        {
            let effort = model
                .thinking_level_map
                .as_ref()
                .and_then(|m| m.off_value())
                .unwrap_or("none");
            params.insert("reasoning".into(), json!({ "effort": effort }));
        }
    } else if compat.thinking_format == ThinkingFormat::AntLing
        && model.reasoning
        && options.reasoning_effort.is_some()
    {
        if let Some(effort) = options
            .reasoning_effort
            .and_then(|level| model.thinking_level_map.as_ref().and_then(|m| m.get(level)))
        {
            params.insert("reasoning".into(), json!({ "effort": effort }));
        }
    } else if compat.thinking_format == ThinkingFormat::Together && model.reasoning {
        params.insert(
            "reasoning".into(),
            json!({ "enabled": options.reasoning_effort.is_some() }),
        );
        if options.reasoning_effort.is_some() && compat.supports_reasoning_effort {
            let effort = options
                .reasoning_effort
                .and_then(|level| model.thinking_level_map.as_ref().and_then(|m| m.get(level)))
                .map(|s| s.to_string())
                .or_else(|| options.reasoning_effort.map(|l| level_name(l).to_string()));
            if let Some(e) = effort {
                params.insert("reasoning_effort".into(), Value::String(e));
            }
        }
    } else if compat.thinking_format == ThinkingFormat::StringThinking && model.reasoning {
        if options.reasoning_effort.is_some() {
            let thinking = options
                .reasoning_effort
                .and_then(|level| model.thinking_level_map.as_ref().and_then(|m| m.get(level)))
                .map(|s| s.to_string())
                .unwrap_or_else(|| level_name(options.reasoning_effort.unwrap()).to_string());
            params.insert("thinking".into(), Value::String(thinking));
        } else if model
            .thinking_level_map
            .as_ref()
            .is_none_or(|m| m.off.is_none())
        {
            let thinking = model
                .thinking_level_map
                .as_ref()
                .and_then(|m| m.off_value())
                .unwrap_or("none");
            params.insert("thinking".into(), Value::String(thinking.to_string()));
        }
    } else if options.reasoning_effort.is_some()
        && model.reasoning
        && compat.supports_reasoning_effort
    {
        // OpenAI-style reasoning_effort.
        let effort = options
            .reasoning_effort
            .and_then(|level| model.thinking_level_map.as_ref().and_then(|m| m.get(level)))
            .map(|s| s.to_string())
            .unwrap_or_else(|| level_name(options.reasoning_effort.unwrap()).to_string());
        params.insert("reasoning_effort".into(), Value::String(effort));
    } else if options.reasoning_effort.is_none()
        && model.reasoning
        && compat.supports_reasoning_effort
    {
        if let Some(off) = model
            .thinking_level_map
            .as_ref()
            .and_then(|m| m.off_value())
        {
            params.insert("reasoning_effort".into(), Value::String(off.to_string()));
        }
    }

    // Cap reasoning with a top-level budget field.
    if let Some(field) = thinking_token_budget_field {
        if let Some(budget) = thinking_budget {
            let name = match field {
                ThinkingTokenBudgetField::ThinkingTokenBudget => "thinking_token_budget",
                ThinkingTokenBudgetField::ThinkingBudget => "thinking_budget",
                ThinkingTokenBudgetField::ThinkingBudgetTokens => "thinking_budget_tokens",
            };
            params.insert(name.into(), Value::Number(budget.into()));
        }
    }

    // OpenRouter provider routing preferences.
    if let Some(routing) = compat.open_router_routing.as_object() {
        if !routing.is_empty() {
            params.insert("provider".into(), Value::Object(routing.clone()));
        }
    }

    // Vercel AI Gateway provider routing preferences.
    if let Some(routing) = compat.vercel_gateway_routing.as_object() {
        let only = routing.get("only").cloned();
        let order = routing.get("order").cloned();
        if only.is_some() || order.is_some() {
            let mut gateway = serde_json::Map::new();
            if let Some(o) = only {
                gateway.insert("only".into(), o);
            }
            if let Some(o) = order {
                gateway.insert("order".into(), o);
            }
            params.insert("providerOptions".into(), json!({ "gateway": gateway }));
        }
    }

    // samplingParams merged last (custom keys override named fields).
    if let Some(sampling) = &options.base.sampling_params {
        if let Some(obj) = sampling.as_object() {
            for (k, v) in obj {
                params.insert(k.clone(), v.clone());
            }
        }
    }

    Ok(Value::Object(params))
}

/// Level name for the reasoning effort wire value (TS string union).
fn level_name(level: crate::types::ids::ThinkingLevel) -> &'static str {
    match level {
        crate::types::ids::ThinkingLevel::Minimal => "minimal",
        crate::types::ids::ThinkingLevel::Low => "low",
        crate::types::ids::ThinkingLevel::Medium => "medium",
        crate::types::ids::ThinkingLevel::High => "high",
        crate::types::ids::ThinkingLevel::Xhigh => "xhigh",
        crate::types::ids::ThinkingLevel::Max => "max",
    }
}

/// `ThinkingLevel` → `ModelThinkingLevel` (the extended union with `off`).
fn level_to_model(
    level: crate::types::ids::ThinkingLevel,
) -> crate::types::ids::ModelThinkingLevel {
    match level {
        crate::types::ids::ThinkingLevel::Minimal => crate::types::ids::ModelThinkingLevel::Minimal,
        crate::types::ids::ThinkingLevel::Low => crate::types::ids::ModelThinkingLevel::Low,
        crate::types::ids::ThinkingLevel::Medium => crate::types::ids::ModelThinkingLevel::Medium,
        crate::types::ids::ThinkingLevel::High => crate::types::ids::ModelThinkingLevel::High,
        crate::types::ids::ThinkingLevel::Xhigh => crate::types::ids::ModelThinkingLevel::Xhigh,
        crate::types::ids::ThinkingLevel::Max => crate::types::ids::ModelThinkingLevel::Max,
    }
}

/// `ModelThinkingLevel` → `ThinkingLevel` (panics on `Off` — callers must
/// handle it first).
fn model_to_level(
    level: crate::types::ids::ModelThinkingLevel,
) -> crate::types::ids::ThinkingLevel {
    match level {
        crate::types::ids::ModelThinkingLevel::Off => {
            panic!("model_to_level(Off): callers map Off to None first")
        }
        crate::types::ids::ModelThinkingLevel::Minimal => crate::types::ids::ThinkingLevel::Minimal,
        crate::types::ids::ModelThinkingLevel::Low => crate::types::ids::ThinkingLevel::Low,
        crate::types::ids::ModelThinkingLevel::Medium => crate::types::ids::ThinkingLevel::Medium,
        crate::types::ids::ModelThinkingLevel::High => crate::types::ids::ThinkingLevel::High,
        crate::types::ids::ModelThinkingLevel::Xhigh => crate::types::ids::ThinkingLevel::Xhigh,
        crate::types::ids::ModelThinkingLevel::Max => crate::types::ids::ThinkingLevel::Max,
    }
}

// ---------------------------------------------------------------------------
// `stream` / `streamSimple` (TS `stream` `:204-619`, `streamSimple` `:620-640`)
// ---------------------------------------------------------------------------

/// TS `clampThinkingLevel` (`models.ts:913-927`): clamp a requested thinking
/// level to the model's supported set. `getSupportedThinkingLevels` (models.ts
/// `:902-910`) filters the extended levels by `model.reasoning` and the
/// `thinkingLevelMap` null/absent semantics.
pub fn get_supported_thinking_levels(model: &Model) -> Vec<crate::types::ids::ModelThinkingLevel> {
    use crate::types::ids::ModelThinkingLevel::*;
    if !model.reasoning {
        return vec![Off];
    }
    const EXTENDED: [crate::types::ids::ModelThinkingLevel; 7] =
        [Off, Minimal, Low, Medium, High, Xhigh, Max];
    EXTENDED
        .iter()
        .copied()
        .filter(|level| {
            let map = model.thinking_level_map.as_ref();
            match level {
                // `map[level] === null` → unsupported (explicit null)
                Xhigh | Max => map.is_some_and(|m| {
                    m.get(crate::types::ids::ThinkingLevel::Xhigh).is_some()
                        || m.get(crate::types::ids::ThinkingLevel::Max).is_some()
                }),
                // absent key → supported; explicit null → unsupported
                Off => map.is_none_or(|m| m.off_value().is_some()),
                _ => true,
            }
        })
        .collect()
}

/// TS `clampThinkingLevel` (`models.ts:913-927`).
pub fn clamp_thinking_level(
    model: &Model,
    level: crate::types::ids::ModelThinkingLevel,
) -> crate::types::ids::ModelThinkingLevel {
    use crate::types::ids::ModelThinkingLevel::*;
    let available = get_supported_thinking_levels(model);
    if available.contains(&level) {
        return level;
    }
    const EXTENDED: [crate::types::ids::ModelThinkingLevel; 7] =
        [Off, Minimal, Low, Medium, High, Xhigh, Max];
    let requested_index = EXTENDED.iter().position(|l| *l == level);
    let Some(requested_index) = requested_index else {
        return available.first().copied().unwrap_or(Off);
    };
    // search upward then downward for the nearest supported level
    for candidate in &EXTENDED[requested_index..] {
        if available.contains(candidate) {
            return *candidate;
        }
    }
    for candidate in EXTENDED[..requested_index].iter().rev() {
        if available.contains(candidate) {
            return *candidate;
        }
    }
    available.first().copied().unwrap_or(Off)
}

/// `streamSimple` (TS `:620-640`): map the coarse options onto full
/// `OpenAICompletionsOptions` and delegate to `stream`. The synchronous
/// `getClientApiKey` auth assert (`:622`) is intentionally omitted — auth is
/// enforced inside `produce` as an `error` event.
pub fn stream_simple(
    model: &Model,
    context: &Context,
    opts: Option<crate::api::SimpleStreamOptions>,
) -> AssistantMessageEventStream {
    let opts = opts.unwrap_or_default();
    let mut base = opts.base.clone();
    // TS `buildBaseOptions` (simple-options.ts `:28-61`): clamp max_tokens to
    // context (already exported), keep the rest of the shared options.
    if let Some(mt) = base.max_tokens {
        base.max_tokens = Some(clamp_max_tokens_to_context(model, context, mt));
    } else {
        base.max_tokens = Some(clamp_max_tokens_to_context(
            model,
            context,
            model.max_tokens,
        ));
    }
    // TS `streamSimple` `:634-641`: `clampThinkingLevel(model, reasoning)` then
    // `reasoningEffort = clamped === "off" ? undefined : clamped`. In the Rust
    // model "off" is represented as `None` (the anthropic adapter's convention),
    // so a reasoning-level request that clamps to Off yields `None`.
    let clamped = opts
        .reasoning
        .map(|l| clamp_thinking_level(model, level_to_model(l)));
    let reasoning_effort = match clamped {
        None | Some(crate::types::ids::ModelThinkingLevel::Off) => None,
        Some(l) => Some(model_to_level(l)),
    };
    stream(
        model,
        context,
        Some(OpenAICompletionsOptions {
            base,
            tool_choice: None,
            reasoning_effort,
            thinking_budgets: opts.thinking_budgets,
        }),
    )
}

/// The `stream` adapter (TS `stream` `:204-619`). Spawns the producer over the
/// transport's SSE byte stream and emits `AssistantMessageEvent`s.
///
/// This wave wires the deterministic state machine to a transport; the
/// `onPayload`/`onResponse`/`retryProviderRequest`/`normalizeProviderError`
/// pieces remain a later wave.
pub fn stream(
    model: &Model,
    context: &Context,
    opts: Option<OpenAICompletionsOptions>,
) -> AssistantMessageEventStream {
    let (sink, stream) = assistant_message_stream();
    let model = model.clone();
    let context = context.clone();
    let opts = opts.unwrap_or_default();
    tokio::spawn(produce(model, context, opts, sink));
    stream
}

/// Build the outbound OpenAI request (TS `createClient` + `buildParams`; the
/// URL is `{baseUrl}/chat/completions`, headers assembled from `model.headers`
/// + options.headers, body = `buildParams` output).
fn build_request(
    model: &Model,
    context: &Context,
    opts: &OpenAICompletionsOptions,
) -> Result<crate::http::HttpRequest, String> {
    let params = build_params(model, context, Some(opts), None)?;
    let mut headers: Vec<(String, String)> = Vec::new();
    if let Some(extra) = &opts.base.headers {
        headers.extend(extra.iter().cloned());
    }
    let url = format!("{}/chat/completions", model.base_url.trim_end_matches('/'));
    Ok(crate::http::HttpRequest {
        url,
        headers,
        body: params.to_string(),
    })
}

/// The producer task (TS IIFE `:206-614`): send the request, iterate SSE
/// chunks, run the state machine, emit `done` or `error`.
async fn produce(
    model: Model,
    context: Context,
    opts: OpenAICompletionsOptions,
    mut sink: AssistantMessageSink,
) {
    let mut output = init_output(&model);
    let result = run_produce(&model, &context, &opts, &mut output, &mut sink).await;
    match result {
        Ok(()) => {
            let reason = output.stop_reason;
            sink.push(AssistantMessageEvent::Done {
                reason,
                message: output.clone(),
            });
            sink.end(None);
        }
        Err(message) => {
            output.stop_reason = StopReason::Error;
            output.error_message = Some(message);
            let reason = output.stop_reason;
            sink.push(AssistantMessageEvent::Error {
                reason,
                error: output.clone(),
            });
            sink.end(None);
        }
    }
}

async fn run_produce(
    model: &Model,
    context: &Context,
    opts: &OpenAICompletionsOptions,
    output: &mut AssistantMessage,
    sink: &mut AssistantMessageSink,
) -> Result<(), String> {
    let compat = get_compat(model);
    let grammar_tool_input_properties = create_grammar_tool_input_properties(
        context.tools.as_deref(),
        compat.supports_openai_grammar_tools,
    );
    let request = build_request(model, context, opts)?;

    // Resolve the transport (TS `createClient` `:641-689` + `options.fetch`):
    // injected transport skips client construction entirely.
    let transport: std::sync::Arc<dyn crate::http::DynTransport> = match &opts.base.transport {
        Some(t) => t.clone(),
        None => std::sync::Arc::new(crate::http::ReqwestTransport::new()),
    };
    let byte_stream = transport
        .send_dyn(request)
        .await
        .map_err(|e| crate::http::TransportError::to_string(&e))?;

    // TS `:557`: pre-loop `start` event.
    sink.push(AssistantMessageEvent::Start {
        partial: output.clone(),
    });

    // Parse SSE into chunk values (OpenAI chat-completions uses plain
    // `data: {json}` lines, no `event:` names).
    let byte_stream = byte_stream.map(|chunk| {
        chunk.map_err(|e| {
            crate::sse::SseError::Transport(crate::http::TransportError::to_string(&e))
        })
    });
    let mut events = crate::sse::iterate_sse_messages(byte_stream);
    let mut chunks = Vec::new();
    while let Some(item) = events.next().await {
        let sse = item.map_err(|e| match e {
            crate::sse::SseError::ServerError(data) => data,
            crate::sse::SseError::Aborted => "Request was aborted".to_string(),
            crate::sse::SseError::Transport(message) => message,
        })?;
        if let Some(chunk) = parse_chunk(&sse.data) {
            chunks.push(chunk);
        }
    }

    run_stream_state_machine(
        model,
        &compat,
        &grammar_tool_input_properties,
        chunks.into_iter(),
        output,
        sink,
    )
}

/// Unit type binding the module's free functions to the [`ProviderStreams`] trait.
#[derive(Debug, Clone, Copy, Default)]
pub struct OpenAICompletions;

impl crate::api::ProviderStreams for OpenAICompletions {
    fn stream(
        &self,
        model: &Model,
        ctx: &Context,
        opts: Option<crate::api::StreamOptions>,
    ) -> AssistantMessageEventStream {
        let openai = opts.map(|base| OpenAICompletionsOptions {
            base,
            ..Default::default()
        });
        stream(model, ctx, openai)
    }

    fn stream_simple(
        &self,
        model: &Model,
        ctx: &Context,
        opts: Option<crate::api::SimpleStreamOptions>,
    ) -> AssistantMessageEventStream {
        stream_simple(model, ctx, opts)
    }
}

// ---------------------------------------------------------------------------
// Streaming state machine (TS `stream` `:204-618`)
// ---------------------------------------------------------------------------
//
// The deterministic core of Pi's `stream`: a chunk->event state machine that
// builds the output `AssistantMessage` incrementally and emits
// `AssistantMessageEvent`s as text/thinking/tool-call blocks open, stream
// deltas and close. Transport (HTTP + retry + error normalization) is a later
// wave; this state machine is fed raw parsed chunk `Value`s and is exercised
// directly by the golden test via a canned SSE body (same pattern as
// `anthropic_golden.rs`).
//
// ## Divergence from Pi (documented, same as anthropic_golden.rs)
//
// Pi pushes the SAME mutable `output` object by reference into every event, so
// a captured tape's `partial` field always reflects the FINAL message state
// (JS aliasing). A faithful Rust port emits an OWNED incremental snapshot at
// each emission. Tests therefore assert the tape's event `type` sequence +
// emission-stable fields, and byte-compare only the TERMINAL event's
// message/error (which equals the final message).
// ---------------------------------------------------------------------------
// Streaming state machine (TS `stream` `:204-618`)
// ---------------------------------------------------------------------------
//
// The deterministic core of Pi's `stream`: a chunk->event state machine that
// builds the output `AssistantMessage` incrementally and emits
// `AssistantMessageEvent`s as text/thinking/tool-call blocks open, stream
// deltas and close. Transport (HTTP + retry + error normalization) is a later
// wave; this state machine is fed raw parsed chunk `Value`s and is exercised
// directly by the golden test via a canned SSE body (same pattern as
// `anthropic_golden.rs`).
//
// ## Divergence from Pi (documented, same as anthropic_golden.rs)
//
// Pi pushes the SAME mutable `output` object by reference into every event, so
// a captured tape's `partial` field always reflects the FINAL message state
// (JS aliasing). A faithful Rust port emits an OWNED incremental snapshot at
// each emission. Tests therefore assert the tape's event `type` sequence +
// emission-stable fields, and byte-compare only the TERMINAL event's
// message/error (which equals the final message).
//
// ## Scratch model (Rust side-channel, never serialized)
//
// TS hides `partialArgs`/`customInput`/`streamIndex` as transient properties
// on the tool-call block. Rust keeps them in parallel maps keyed by the block
// index inside `output.content` — the single source of truth, mutated in
// place exactly like Pi's `output`. The `ToolCall.partial_json` field is NOT
// used here; it is a separate persisted-transient for interrupted sessions.

/// Date.now() (TS `:507`). Non-deterministic; golden tests zero it before compare.
pub fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// The initial `output` literal (TS `:492-508`). Key-insertion order per §4e:
/// role, content, api, provider, model, usage, stopReason, timestamp.
fn init_output(model: &Model) -> AssistantMessage {
    AssistantMessage {
        role: AssistantRole::Assistant,
        content: Vec::new(),
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: Some(model.id.clone()),
        response_model: None,
        diagnostics: None,
        usage: Usage {
            input: 0,
            output: 0,
            cache_read: 0,
            cache_write: 0,
            total_tokens: Some(0),
            cost: Cost {
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
                total: 0.0,
            },
            cache_write1h: None,
            reasoning: None,
        },
        stop_reason: StopReason::Stop,
        timestamp: now_millis(),
        response_id: None,
        raw_stop_reason: None,
        error_message: None,
        end_turn: None,
    }
}

/// A `tool_calls` entry inside a chunk's `choices[0].delta` (TS
/// `StreamingToolCallDelta`). All fields optional; `custom` is the non-standard
/// grammar-tool input form some providers emit.
#[derive(Debug, Clone, Default)]
struct StreamingToolCallDelta {
    index: Option<u64>,
    id: Option<String>,
    function_name: Option<String>,
    function_arguments: Option<String>,
    custom_name: Option<String>,
    custom_input: Option<String>,
}

impl StreamingToolCallDelta {
    fn from_value(v: &Value) -> Self {
        let mut d = StreamingToolCallDelta::default();
        if let Some(obj) = v.as_object() {
            d.index = obj.get("index").and_then(Value::as_u64);
            d.id = obj.get("id").and_then(Value::as_str).map(String::from);
            if let Some(f) = obj.get("function").and_then(Value::as_object) {
                d.function_name = f.get("name").and_then(Value::as_str).map(String::from);
                d.function_arguments = f.get("arguments").and_then(Value::as_str).map(String::from);
            }
            if let Some(c) = obj.get("custom").and_then(Value::as_object) {
                d.custom_name = c.get("name").and_then(Value::as_str).map(String::from);
                d.custom_input = c.get("input").and_then(Value::as_str).map(String::from);
            }
        }
        d
    }
}

/// The streaming state: block scratch maps keyed by index into
/// `output.content` (Rust side-channel for TS's transient block properties).
#[derive(Default)]
struct StreamingState {
    tool_call_blocks_by_index: HashMap<u64, usize>,
    tool_call_blocks_by_id: HashMap<String, usize>,
    partial_args: HashMap<usize, String>,
    custom_inputs: HashMap<usize, (String, GrammarToolInputJsonBuffer)>,
}

/// True when the block is an OpenAI encrypted reasoning detail (`type:
/// "reasoning.encrypted"` with non-empty string id/data) — TS
/// `isEncryptedReasoningDetail` `:131-146`.
fn is_encrypted_reasoning_detail(detail: &Value) -> bool {
    let obj = match detail.as_object() {
        Some(o) => o,
        None => return false,
    };
    obj.get("type").and_then(Value::as_str) == Some("reasoning.encrypted")
        && obj
            .get("id")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty())
        && obj
            .get("data")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty())
}

/// Parse one SSE `data:` payload into a chunk `Value`, skipping non-objects
/// (TS `for await` loop `:565-566`: `if (!chunk || typeof chunk !== "object")
/// continue`) and the terminal `[DONE]` sentinel.
fn parse_chunk(data: &str) -> Option<Value> {
    if data == "[DONE]" {
        return None;
    }
    let value: Value = serde_json::from_str(data).ok()?;
    value.is_object().then_some(value)
}

/// Port of the TS `stream` event loop's per-chunk body + the tail block
/// finalization + stop-reason resolution (`:565-618`). Emits events via
/// `sink`. Returns `Err(message)` for the throw paths (→ catch → `error`
/// event).
///
/// `compat` supplies `supports_finish_reason`; `grammar_tool_input_properties`
/// maps tool names → custom-input property (TS `createGrammarToolInputProperties`
/// result, already ported).
#[allow(clippy::too_many_arguments)]
pub fn run_stream_state_machine(
    model: &Model,
    compat: &ResolvedOpenAICompletionsCompat,
    grammar_tool_input_properties: &HashMap<String, String>,
    chunks: impl Iterator<Item = Value>,
    output: &mut AssistantMessage,
    sink: &mut AssistantMessageSink,
) -> Result<(), String> {
    let mut state = StreamingState::default();
    let mut text_block: Option<usize> = None;
    let mut thinking_block: Option<usize> = None;
    let mut has_finish_reason = false;
    let mut pending_reasoning_details: HashMap<String, String> = HashMap::new();

    for chunk in chunks {
        // output.responseId ||= chunk.id (TS `:568`)
        if output.response_id.is_none() {
            if let Some(id) = chunk.get("id").and_then(Value::as_str) {
                output.response_id = Some(id.to_string());
            }
        }
        // output.responseModel ||= chunk.model when it differs (TS `:569-571`)
        if output.response_model.is_none() {
            if let Some(cm) = chunk.get("model").and_then(Value::as_str) {
                if !cm.is_empty() && cm != model.id {
                    output.response_model = Some(cm.to_string());
                }
            }
        }
        // output.usage = parseChunkUsage(chunk.usage, model) (TS `:572-573`)
        if let Some(u) = chunk.get("usage") {
            output.usage = parse_chunk_usage(&RawChunkUsage::from_value(u), model);
        }

        let choice = chunk
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|a| a.first());
        let Some(choice) = choice else {
            continue;
        };

        // Fallback: usage in choice.usage (Moonshot) (TS `:578-581`)
        if chunk.get("usage").is_none() {
            if let Some(u) = choice.get("usage") {
                output.usage = parse_chunk_usage(&RawChunkUsage::from_value(u), model);
            }
        }

        if let Some(fr) = choice.get("finish_reason").and_then(Value::as_str) {
            output.raw_stop_reason = Some(fr.to_string());
            let finish_reason_result = map_stop_reason(fr);
            output.stop_reason = finish_reason_result.0;
            if let Some(em) = finish_reason_result.1 {
                output.error_message = Some(em);
            }
            has_finish_reason = true;
        }

        let Some(delta) = choice.get("delta") else {
            continue;
        };

        // content delta (TS `:585-596`)
        let content_delta = delta.get("content").and_then(Value::as_str);
        if let Some(cd) = content_delta {
            if !cd.is_empty() {
                let block_idx = ensure_text_block(&mut state, output, &mut text_block, sink);
                if let AssistantContent::Text(t) = &mut output.content[block_idx] {
                    t.text.push_str(cd);
                }
                sink.push(AssistantMessageEvent::TextDelta {
                    content_index: block_idx as u32,
                    delta: cd.to_string(),
                    partial: output.clone(),
                });
            }
        }

        // reasoning fields (TS `:598-621`): first non-empty of
        // reasoning_content / reasoning / reasoning_text
        let reasoning_fields = ["reasoning_content", "reasoning", "reasoning_text"];
        let mut found_reasoning_field: Option<&str> = None;
        for field in reasoning_fields {
            if delta
                .get(field)
                .and_then(Value::as_str)
                .is_some_and(|s| !s.is_empty())
            {
                found_reasoning_field = Some(field);
                break;
            }
        }
        if let Some(field) = found_reasoning_field {
            if let Some(delta_text) = delta.get(field).and_then(Value::as_str) {
                if !delta_text.is_empty() {
                    let thinking_signature =
                        if model.provider.0 == "opencode-go" && field == "reasoning" {
                            "reasoning_content"
                        } else {
                            field
                        };
                    let block_idx = ensure_thinking_block(
                        &mut state,
                        output,
                        &mut thinking_block,
                        thinking_signature,
                        sink,
                    );
                    if let AssistantContent::Thinking(t) = &mut output.content[block_idx] {
                        t.thinking.push_str(delta_text);
                    }
                    sink.push(AssistantMessageEvent::ThinkingDelta {
                        content_index: block_idx as u32,
                        delta: delta_text.to_string(),
                        partial: output.clone(),
                    });
                }
            }
        }

        // tool_calls deltas (TS `:622-657`)
        if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
            for tc in tool_calls {
                let delta_tc = StreamingToolCallDelta::from_value(tc);
                let block_idx = ensure_tool_call_block(
                    &mut state,
                    output,
                    &delta_tc,
                    grammar_tool_input_properties,
                    &mut pending_reasoning_details,
                    sink,
                );
                // block.id ||= toolCall.id (TS `:624-628`)
                if let Some(id) = &delta_tc.id {
                    let block = &mut output.content[block_idx];
                    if let AssistantContent::ToolCall(tc) = block {
                        if tc.id.is_empty() {
                            tc.id = id.clone();
                            state.tool_call_blocks_by_id.insert(id.clone(), block_idx);
                        }
                    }
                }
                // block.name ||= name (TS `:629-632`)
                let name = delta_tc
                    .function_name
                    .clone()
                    .or_else(|| delta_tc.custom_name.clone());
                if let Some(name) = &name {
                    let block = &mut output.content[block_idx];
                    if let AssistantContent::ToolCall(tc) = block {
                        if tc.name.is_empty() {
                            tc.name = name.clone();
                        }
                    }
                }

                let mut delta_text = String::new();
                if let Some(fa) = &delta_tc.function_arguments {
                    delta_text = fa.clone();
                    // partialArgs accumulation (TS `:636-638`)
                    state
                        .partial_args
                        .entry(block_idx)
                        .or_default()
                        .push_str(fa);
                    // block.arguments = parseStreamingJson(block.partialArgs)
                    let partial = state
                        .partial_args
                        .get(&block_idx)
                        .cloned()
                        .unwrap_or_default();
                    if let AssistantContent::ToolCall(tc) = &mut output.content[block_idx] {
                        tc.arguments = parse_streaming_json(&partial)
                            .as_object()
                            .cloned()
                            .unwrap_or_default();
                    }
                } else if let Some(ci) = &delta_tc.custom_input {
                    // nextInput = getCustomToolCallInput(block) + input (TS `:640-642`)
                    let next = get_custom_tool_call_input(&state, output, block_idx) + ci;
                    delta_text =
                        append_custom_tool_call_input(&mut state, output, block_idx, &next, false)
                            .unwrap_or_default();
                }
                sink.push(AssistantMessageEvent::ToolcallDelta {
                    content_index: block_idx as u32,
                    delta: delta_text,
                    partial: output.clone(),
                });
            }
        }

        // reasoning_details (TS `:659-672`)
        if let Some(details) = delta.get("reasoning_details").and_then(Value::as_array) {
            for detail in details {
                if is_encrypted_reasoning_detail(detail) {
                    let serialized = serde_json::to_string(detail).unwrap_or_default();
                    if let Some(id) = detail.get("id").and_then(Value::as_str) {
                        if let Some(&block_idx) = state.tool_call_blocks_by_id.get(id) {
                            if let AssistantContent::ToolCall(tc) = &mut output.content[block_idx] {
                                tc.thought_signature = Some(serialized.clone());
                            }
                        } else {
                            pending_reasoning_details.insert(id.to_string(), serialized);
                        }
                    }
                }
            }
        }
    }

    // Tail: finishBlock for every block in content order (TS `:674-676`)
    let block_count = output.content.len();
    for block_idx in 0..block_count {
        finish_block(&mut state, output, block_idx, sink);
    }

    // TS `:678-695`
    if !has_finish_reason && !compat.supports_finish_reason {
        output.stop_reason = if output
            .content
            .iter()
            .any(|b| matches!(b, AssistantContent::ToolCall(_)))
        {
            StopReason::ToolUse
        } else {
            StopReason::Stop
        };
    }
    if output.stop_reason == StopReason::Error {
        return Err(output
            .error_message
            .clone()
            .unwrap_or_else(|| "Provider returned an error stop reason".to_string()));
    }
    // TS `:691-692`: throw when the stream ended without a finish_reason.
    // Pi's second clause `|| output.stopReason === "pending"` is a logical
    // subset of the first here — `stopReason` stays its initial "pending" value
    // EXACTLY when `supportsFinishReason && !hasFinishReason` (the fallback at
    // `:678-683` already resolved it to toolUse/stop otherwise), so the two
    // clauses are provably equivalent. It is collapsed into one check.
    if compat.supports_finish_reason && !has_finish_reason {
        return Err("Stream ended without finish_reason".to_string());
    }

    Ok(())
}

// --- helpers used by the state machine --------------------------------------

/// TS `ensureTextBlock` `:536-543`: create the single text block + emit
/// `text_start` on first use. Returns the block index.
fn ensure_text_block(
    state: &mut StreamingState,
    output: &mut AssistantMessage,
    text_block: &mut Option<usize>,
    sink: &mut AssistantMessageSink,
) -> usize {
    if text_block.is_none() {
        let idx = output.content.len();
        output.content.push(AssistantContent::Text(TextContent {
            kind: TextTag::Text,
            text: String::new(),
            text_signature: None,
        }));
        state.tool_call_blocks_by_index.clear(); // no-op; index maps are tool-only
        *text_block = Some(idx);
        sink.push(AssistantMessageEvent::TextStart {
            content_index: idx as u32,
            partial: output.clone(),
        });
    }
    text_block.unwrap()
}

/// TS `ensureThinkingBlock(thinkingSignature)` `:545-552`.
fn ensure_thinking_block(
    state: &mut StreamingState,
    output: &mut AssistantMessage,
    thinking_block: &mut Option<usize>,
    thinking_signature: &str,
    sink: &mut AssistantMessageSink,
) -> usize {
    if thinking_block.is_none() {
        let idx = output.content.len();
        output
            .content
            .push(AssistantContent::Thinking(ThinkingContent {
                kind: ThinkingTag::Thinking,
                thinking: String::new(),
                thinking_signature: Some(thinking_signature.to_string()),
                redacted: None,
            }));
        state.tool_call_blocks_by_index.clear(); // no-op; index maps are tool-only
        *thinking_block = Some(idx);
        sink.push(AssistantMessageEvent::ThinkingStart {
            content_index: idx as u32,
            partial: output.clone(),
        });
    }
    thinking_block.unwrap()
}

/// TS `ensureToolCallBlock(toolCall)` `:556-592`.
#[allow(clippy::too_many_arguments)]
fn ensure_tool_call_block(
    state: &mut StreamingState,
    output: &mut AssistantMessage,
    delta: &StreamingToolCallDelta,
    grammar_tool_input_properties: &HashMap<String, String>,
    pending_reasoning_details: &mut HashMap<String, String>,
    sink: &mut AssistantMessageSink,
) -> usize {
    let stream_index = delta.index;
    let name = delta
        .function_name
        .clone()
        .or_else(|| delta.custom_name.clone())
        .unwrap_or_default();

    let mut block_idx = stream_index.and_then(|i| state.tool_call_blocks_by_index.get(&i).copied());
    if block_idx.is_none() {
        if let Some(id) = &delta.id {
            block_idx = state.tool_call_blocks_by_id.get(id).copied();
        }
    }
    if block_idx.is_none() {
        // TS `:568-585`: create a new block. The "input" fallback should not be
        // taken — but gives unknown tools a place to stash data.
        let custom_input_property = if delta.custom_name.is_some() && delta.function_name.is_none()
        {
            Some(
                grammar_tool_input_properties
                    .get(&name)
                    .cloned()
                    .unwrap_or_else(|| "input".to_string()),
            )
        } else {
            None
        };
        let id = delta.id.clone().unwrap_or_default();
        let (arguments, custom_input) = match &custom_input_property {
            Some(prop) => {
                let mut m = Map::new();
                m.insert(prop.clone(), Value::String(String::new()));
                (
                    m,
                    Some((
                        prop.clone(),
                        GrammarToolInputJsonBuffer {
                            input: String::new(),
                            started: false,
                            closed: false,
                        },
                    )),
                )
            }
            None => (Map::new(), None),
        };
        let idx = output.content.len();
        output.content.push(AssistantContent::ToolCall(ToolCall {
            kind: ToolCallTag::ToolCall,
            id: id.clone(),
            name: name.clone(),
            arguments,
            thought_signature: None,
            partial_json: None,
        }));
        if let Some(si) = stream_index {
            state.tool_call_blocks_by_index.insert(si, idx);
        }
        if !id.is_empty() {
            state.tool_call_blocks_by_id.insert(id, idx);
        }
        if let Some(ci) = custom_input {
            state.custom_inputs.insert(idx, ci);
        }
        block_idx = Some(idx);
        sink.push(AssistantMessageEvent::ToolcallStart {
            content_index: idx as u32,
            partial: output.clone(),
        });
    }
    let block_idx = block_idx.unwrap();

    // TS `:594-600`: backfill streamIndex on an existing block
    if let Some(si) = stream_index {
        state
            .tool_call_blocks_by_index
            .entry(si)
            .or_insert(block_idx);
    }
    // TS `:601-602`: backfill id
    if let Some(id) = &delta.id {
        state
            .tool_call_blocks_by_id
            .entry(id.clone())
            .or_insert(block_idx);
    }

    // block.name ||= name (TS `:604-606`)
    if !name.is_empty() {
        let block = &mut output.content[block_idx];
        if let AssistantContent::ToolCall(tc) = block {
            if tc.name.is_empty() {
                tc.name = name.clone();
            }
        }
    }
    // custom input backfill (TS `:607-613`): install a customInput on an
    // existing block that lacks one
    if delta.custom_name.is_some()
        && delta.function_name.is_none()
        && !state.custom_inputs.contains_key(&block_idx)
    {
        let prop = {
            let block = &output.content[block_idx];
            match block {
                AssistantContent::ToolCall(tc) => grammar_tool_input_properties
                    .get(&tc.name)
                    .cloned()
                    .unwrap_or_else(|| "input".to_string()),
                _ => "input".to_string(),
            }
        };
        let block = &mut output.content[block_idx];
        if let AssistantContent::ToolCall(tc) = block {
            let mut m = Map::new();
            m.insert(prop.clone(), Value::String(String::new()));
            tc.arguments = m;
        }
        state.custom_inputs.insert(
            block_idx,
            (
                prop,
                GrammarToolInputJsonBuffer {
                    input: String::new(),
                    started: false,
                    closed: false,
                },
            ),
        );
    }
    // applyPendingReasoningDetail (TS `:614-616`)
    if let Some(id) = &delta.id {
        if let Some(detail) = pending_reasoning_details.remove(id) {
            let block = &mut output.content[block_idx];
            if let AssistantContent::ToolCall(tc) = block {
                tc.thought_signature = Some(detail);
            }
        }
    }
    block_idx
}

/// TS `finishBlock(block)` `:517-535`: emit the `*_end` event, finalizing
/// tool-call args (parse `partialArgs` or flush the grammar custom input).
fn finish_block(
    state: &mut StreamingState,
    output: &mut AssistantMessage,
    block_idx: usize,
    sink: &mut AssistantMessageSink,
) {
    let content_index = block_idx as u32;
    match &mut output.content[block_idx] {
        AssistantContent::Text(t) => {
            let content = t.text.clone();
            sink.push(AssistantMessageEvent::TextEnd {
                content_index,
                content,
                partial: output.clone(),
            });
        }
        AssistantContent::Thinking(t) => {
            let content = t.thinking.clone();
            sink.push(AssistantMessageEvent::ThinkingEnd {
                content_index,
                content,
                partial: output.clone(),
            });
        }
        AssistantContent::ToolCall(tc) => {
            // customInput flush or parseStreamingJson(partialArgs) (TS `:523-531`)
            let mut flush_delta: Option<String> = None;
            if state.custom_inputs.contains_key(&block_idx) {
                let (prop, buffer) = state.custom_inputs.get_mut(&block_idx).unwrap();
                let next = tc
                    .arguments
                    .get(prop)
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                if let Ok(Some(delta)) =
                    append_grammar_tool_input_json_delta(buffer, prop, &next, true)
                {
                    flush_delta = (!delta.is_empty()).then_some(delta);
                }
            } else if let Some(partial) = state.partial_args.get(&block_idx).cloned() {
                tc.arguments = parse_streaming_json(&partial)
                    .as_object()
                    .cloned()
                    .unwrap_or_default();
            }
            // strip scratch (TS `:533-535`) before emitting, so the terminal
            // event/snapshot carries only parsed arguments
            state.partial_args.remove(&block_idx);
            state.custom_inputs.remove(&block_idx);
            let tool_call = tc.clone();
            if let Some(delta) = flush_delta {
                sink.push(AssistantMessageEvent::ToolcallDelta {
                    content_index,
                    delta,
                    partial: output.clone(),
                });
            }
            sink.push(AssistantMessageEvent::ToolcallEnd {
                content_index,
                tool_call,
                partial: output.clone(),
            });
        }
    }
}

/// TS `getCustomToolCallInput` `:511-515`: the current value of the
/// custom-input property inside `block.arguments`.
fn get_custom_tool_call_input(
    state: &StreamingState,
    output: &AssistantMessage,
    block_idx: usize,
) -> String {
    let Some((prop, _)) = state.custom_inputs.get(&block_idx) else {
        return String::new();
    };
    let block = &output.content[block_idx];
    match block {
        AssistantContent::ToolCall(tc) => match tc.arguments.get(prop) {
            Some(Value::String(s)) => s.clone(),
            _ => String::new(),
        },
        _ => String::new(),
    }
}

/// `appendCustomToolCallInput(block, nextInput, close)` (TS `:516-526`):
/// monotonic append to the grammar JSON buffer, then `block.arguments =
/// { [property]: nextInput }`. Returns the delta.
fn append_custom_tool_call_input(
    state: &mut StreamingState,
    output: &mut AssistantMessage,
    block_idx: usize,
    next_input: &str,
    close: bool,
) -> Option<String> {
    let (prop, buffer) = state.custom_inputs.get_mut(&block_idx)?;
    let delta = append_grammar_tool_input_json_delta(buffer, prop, next_input, close).ok()?;
    let block = &mut output.content[block_idx];
    if let AssistantContent::ToolCall(tc) = block {
        let mut m = Map::new();
        m.insert(prop.clone(), Value::String(next_input.to_string()));
        tc.arguments = m;
    }
    delta
}

/// Raw usage chunk → [`RawChunkUsage`] (the TS inline parse in `parseChunkUsage`).
impl RawChunkUsage {
    fn from_value(v: &Value) -> Self {
        let mut r = RawChunkUsage::default();
        let obj = match v.as_object() {
            Some(o) => o,
            None => return r,
        };
        r.prompt_tokens = obj.get("prompt_tokens").and_then(Value::as_u64);
        r.completion_tokens = obj.get("completion_tokens").and_then(Value::as_u64);
        r.cached_tokens = obj.get("cached_tokens").and_then(Value::as_u64);
        r.prompt_cache_hit_tokens = obj.get("prompt_cache_hit_tokens").and_then(Value::as_u64);
        if let Some(d) = obj.get("prompt_tokens_details").and_then(Value::as_object) {
            r.prompt_details_cached_tokens = d.get("cached_tokens").and_then(Value::as_u64);
            r.prompt_details_cache_write_tokens =
                d.get("cache_write_tokens").and_then(Value::as_u64);
        }
        if let Some(d) = obj
            .get("completion_tokens_details")
            .and_then(Value::as_object)
        {
            r.completion_details_reasoning_tokens =
                d.get("reasoning_tokens").and_then(Value::as_u64);
        }
        r
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    /// `shortHash` oracle values captured by running real Pi
    /// (`node scripts/...` driving `packages/ai/src/utils/hash.ts`).
    #[test]
    fn short_hash_matches_oracle() {
        let cases = [
            ("tool_call_abc", "1etmnurhqmven"),
            ("", "k4n83c7h0j2b"),
            (&format!("long-id-{}", "x".repeat(60)), "rvr9x64hrspo"),
            // Unicode exercises UTF-16 code-unit iteration (JS string indices
            // are UTF-16 code units, not bytes).
            ("hello wörld", "xruvbz94tpvu"),
            ("日本語テスト", "1urlhu61qrqskw"),
            (&"x".repeat(45), "v8b4f88ov0d9"),
        ];
        for (input, expected) in cases {
            assert_eq!(short_hash(input), expected, "shortHash({input:?})");
        }
    }

    #[test]
    fn map_stop_reason_covers_all_cases() {
        assert_eq!(map_stop_reason("stop"), (StopReason::Stop, None));
        assert_eq!(map_stop_reason("end"), (StopReason::Stop, None));
        assert_eq!(map_stop_reason("length"), (StopReason::Length, None));
        assert_eq!(
            map_stop_reason("function_call"),
            (StopReason::ToolUse, None)
        );
        assert_eq!(map_stop_reason("tool_calls"), (StopReason::ToolUse, None));
        assert_eq!(
            map_stop_reason("content_filter"),
            (
                StopReason::Error,
                Some("Provider finish_reason: content_filter".to_string()),
            )
        );
        assert_eq!(
            map_stop_reason("network_error"),
            (
                StopReason::Error,
                Some("Provider finish_reason: network_error".to_string()),
            )
        );
        assert_eq!(
            map_stop_reason("bogus"),
            (
                StopReason::Error,
                Some("Provider finish_reason: bogus".to_string()),
            )
        );
    }

    #[test]
    fn parse_chunk_usage_divides_cache_placements() {
        // DeepSeek reports cache hits in prompt_cache_hit_tokens; the
        // cache-written tokens reduce the billable input floor at zero.
        let model = crate::providers::faux::Faux::new().get_model().clone();
        let usage = parse_chunk_usage(
            &RawChunkUsage {
                prompt_tokens: Some(100),
                completion_tokens: Some(30),
                prompt_cache_hit_tokens: Some(60),
                prompt_details_cache_write_tokens: Some(5),
                completion_details_reasoning_tokens: Some(4),
                ..Default::default()
            },
            &model,
        );
        assert_eq!(usage.input, 35); // 100 - 60 - 5
        assert_eq!(usage.output, 30);
        assert_eq!(usage.cache_read, 60);
        assert_eq!(usage.cache_write, 5);
        assert_eq!(usage.reasoning, Some(4));
        assert_eq!(usage.total_tokens, Some(35 + 30 + 60 + 5));

        // prompt_tokens_details.cached_tokens wins over the others.
        let usage = parse_chunk_usage(
            &RawChunkUsage {
                prompt_tokens: Some(50),
                completion_tokens: Some(1),
                cached_tokens: Some(9),
                prompt_cache_hit_tokens: Some(8),
                prompt_details_cached_tokens: Some(7),
                ..Default::default()
            },
            &model,
        );
        assert_eq!(usage.cache_read, 7);
        assert_eq!(usage.input, 43);

        // Sub-cache-write robustness: no negative input.
        let usage = parse_chunk_usage(
            &RawChunkUsage {
                prompt_tokens: Some(5),
                prompt_details_cache_write_tokens: Some(20),
                ..Default::default()
            },
            &model,
        );
        assert_eq!(usage.input, 0);
    }
}
