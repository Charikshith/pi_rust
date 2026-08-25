//! OpenAI Responses API adapter — Rust port of `packages/ai/src/api/openai-responses.ts`
//! (stream/streamSimple + getCompat + buildParams + createClient + retry + error
//! normalization).
//!
//! The message/tool conversion and the streaming state machine live in
//! [`super::openai_responses_shared`] (the `openai-responses-shared.ts` port). This module
//! is the transport + params glue: resolve auth, build the request, retry the POST per
//! `provider-retry.ts`, run the shared state machine, normalize errors.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::{json, Map, Value};

use crate::api::openai_completions::{clamp_openai_prompt_cache_key, resolve_cache_retention};
use crate::api::openai_responses_shared::{
    convert_responses_messages, convert_responses_tools, split_deferred_tools,
    ConvertResponsesToolsOptions, DeferredToolsMode, ResponsesStreamOptions, ResponsesStreamState,
    ServiceTierMode,
};
use crate::http::{ByteStream, DynTransport};
use crate::stream::{assistant_message_stream, AssistantMessageEventStream, AssistantMessageSink};
use crate::types::event::AssistantMessageEvent;
use crate::types::ids::{CacheRetention, StopReason, ThinkingLevel};
use crate::types::message::{AssistantMessage, Context};
use crate::types::model::Model;
use crate::utils::error_body::{format_provider_error, normalize_provider_error};
use crate::utils::provider_retry::{retry_provider_request, ProviderError};
use futures::StreamExt;

/// OpenAI Responses-specific options (TS `OpenAIResponsesOptions`).
#[derive(Clone, Default)]
pub struct OpenAIResponsesOptions {
    pub base: crate::api::StreamOptions,
    pub reasoning_effort: Option<ThinkingLevel>,
    pub reasoning_summary: Option<String>,
    pub service_tier: Option<Value>,
    pub tool_choice: Option<Value>,
}

/// `getCompat(model)` — the `Required<OpenAIResponsesCompat>` resolution
/// (`openai-responses.ts:71-81`), detecting defaults from the provider/baseUrl and
/// layering `model.compat` overrides.
#[derive(Debug, Clone)]
pub struct ResolvedOpenAIResponsesCompat {
    pub supports_developer_role: bool,
    pub session_affinity_format: crate::types::ids::SessionAffinityFormat,
    pub supports_long_cache_retention: bool,
    pub supports_strict_mode: bool,
    pub supports_openai_grammar_tools: bool,
    pub supports_additional_tools: bool,
    pub supports_tool_search: bool,
    pub supports_explicit_prompt_cache_mode: bool,
}

/// `detectSessionAffinityFormat` — openrouter (by provider or baseUrl) else openai.
fn detect_session_affinity_format(model: &Model) -> crate::types::ids::SessionAffinityFormat {
    if model.provider.0 == "openrouter" || model.base_url.contains("openrouter.ai") {
        crate::types::ids::SessionAffinityFormat::Openrouter
    } else {
        crate::types::ids::SessionAffinityFormat::Openai
    }
}

/// `getCompat` (`openai-responses.ts:71-81`).
pub fn get_responses_compat(model: &Model) -> ResolvedOpenAIResponsesCompat {
    let compat = model.compat.as_ref().and_then(Value::as_object);
    let bool_field = |name: &str, default: bool| -> bool {
        compat
            .and_then(|c| c.get(name))
            .and_then(Value::as_bool)
            .unwrap_or(default)
    };
    let session_affinity_format = compat
        .and_then(|c| c.get("sessionAffinityFormat"))
        .and_then(Value::as_str)
        .map(parse_session_affinity_format)
        .unwrap_or_else(|| detect_session_affinity_format(model));

    ResolvedOpenAIResponsesCompat {
        supports_developer_role: bool_field("supportsDeveloperRole", true),
        session_affinity_format,
        supports_long_cache_retention: bool_field("supportsLongCacheRetention", true),
        supports_strict_mode: bool_field("supportsStrictMode", false),
        supports_openai_grammar_tools: bool_field("supportsOpenAIGrammarTools", false),
        supports_additional_tools: bool_field("supportsAdditionalTools", false),
        supports_tool_search: bool_field("supportsToolSearch", false),
        supports_explicit_prompt_cache_mode: bool_field("supportsExplicitPromptCacheMode", false),
    }
}

fn parse_session_affinity_format(s: &str) -> crate::types::ids::SessionAffinityFormat {
    match s {
        "openrouter" => crate::types::ids::SessionAffinityFormat::Openrouter,
        "openai-nosession" => crate::types::ids::SessionAffinityFormat::OpenaiNosession,
        _ => crate::types::ids::SessionAffinityFormat::Openai,
    }
}

/// `getPromptCacheRetention` — `"24h"` only when retention is `long` and the compat
/// supports it.
fn get_prompt_cache_retention(
    compat: &ResolvedOpenAIResponsesCompat,
    cache_retention: CacheRetention,
) -> Option<&'static str> {
    if cache_retention == CacheRetention::Long && compat.supports_long_cache_retention {
        Some("24h")
    } else {
        None
    }
}

/// `getClientApiKey(provider, apiKey, headers)` — explicit key, else a pre-auth header
/// implies `"unused"`, else an error naming the provider.
fn get_client_api_key(
    provider: &str,
    api_key: Option<&str>,
    headers: Option<&Vec<(String, String)>>,
) -> Result<String, String> {
    if let Some(key) = api_key {
        return Ok(key.to_string());
    }
    let has_auth = headers.is_some_and(|hs| {
        hs.iter().any(|(k, v)| {
            let k = k.to_lowercase();
            (k == "authorization" || k == "cf-aig-authorization") && !v.trim().is_empty()
        })
    });
    if has_auth {
        return Ok("unused".to_string());
    }
    Err(format!("No API key for provider: {provider}"))
}

/// `streamSimple` (TS `:187-199`): map coarse options → full options.
pub fn stream_simple(
    model: &Model,
    context: &Context,
    opts: Option<crate::api::SimpleStreamOptions>,
) -> AssistantMessageEventStream {
    let opts = opts.unwrap_or_default();
    let mut base = opts.base.clone();
    // TS `buildBaseOptions`: clamp max_tokens to context.
    if let Some(mt) = base.max_tokens {
        base.max_tokens = Some(crate::api::openai_completions::clamp_max_tokens_to_context(
            model, context, mt,
        ));
    } else {
        base.max_tokens = Some(crate::api::openai_completions::clamp_max_tokens_to_context(
            model,
            context,
            model.max_tokens,
        ));
    }
    let clamped = opts.reasoning.map(|l| {
        crate::api::openai_completions::clamp_thinking_level(
            model,
            crate::api::openai_completions::level_to_model(l),
        )
    });
    let reasoning_effort = match clamped {
        None | Some(crate::types::ids::ModelThinkingLevel::Off) => None,
        Some(l) => Some(crate::api::openai_completions::model_to_level(l)),
    };
    stream(
        model,
        context,
        Some(OpenAIResponsesOptions {
            base,
            reasoning_effort,
            reasoning_summary: None,
            service_tier: None,
            tool_choice: None,
        }),
    )
}

/// The `stream` adapter (TS `:104-185`). Spawns the producer.
pub fn stream(
    model: &Model,
    context: &Context,
    opts: Option<OpenAIResponsesOptions>,
) -> AssistantMessageEventStream {
    let (sink, stream) = assistant_message_stream();
    let model = model.clone();
    let context = context.clone();
    let opts = opts.unwrap_or_default();
    tokio::spawn(produce(model, context, opts, sink));
    stream
}

async fn produce(
    model: Model,
    context: Context,
    opts: OpenAIResponsesOptions,
    mut sink: AssistantMessageSink,
) {
    let mut output = crate::api::openai_completions::init_output(&model);
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

#[allow(clippy::too_many_arguments)]
async fn run_produce(
    model: &Model,
    context: &Context,
    opts: &OpenAIResponsesOptions,
    output: &mut AssistantMessage,
    sink: &mut AssistantMessageSink,
) -> Result<(), String> {
    // TS `:109-119`: resolve auth, cache, compat, grammar props.
    let api_key = get_client_api_key(
        &model.provider.0,
        opts.base.api_key.as_deref(),
        opts.base.headers.as_ref(),
    )?;
    let cache_retention =
        resolve_cache_retention(opts.base.cache_retention, opts.base.env.as_ref());
    let cache_session_id = if cache_retention == CacheRetention::None {
        None
    } else {
        opts.base.session_id.as_deref()
    };
    let compat = get_responses_compat(model);
    let grammar_tool_input_properties =
        crate::api::constrained_sampling::create_grammar_tool_input_properties(
            context.tools.as_deref(),
            compat.supports_openai_grammar_tools,
        );

    // buildParams
    let mut params = build_params(
        model,
        context,
        opts,
        &compat,
        &grammar_tool_input_properties,
    )?;
    if let Some(on_payload) = &opts.base.on_payload {
        if let Some(next) = on_payload(params.clone(), model.clone()).await {
            params = next;
        }
    }

    // TS `:123-133`: retry the POST, then onResponse.
    let url = format!("{}/responses", model.base_url.trim_end_matches('/'));
    let request = crate::http::HttpRequest {
        url,
        headers: build_client_headers(model, context, opts, &compat, cache_session_id),
        body: params.to_string(),
        authorization: Some(api_key),
    };

    let transport: Arc<dyn DynTransport> = match &opts.base.transport {
        Some(t) => t.clone(),
        None => Arc::new(crate::http::ReqwestTransport::new()),
    };

    let token = opts.base.signal.clone().unwrap_or_default();
    let (byte_stream, response) = retry_provider_request(
        || {
            let transport = transport.clone();
            let request = request.clone();
            async move { transport.send_dyn(request).await }
        },
        opts.base.max_retries,
        opts.base.max_retry_delay_ms,
        &token,
        |error: &crate::http::TransportError| match error {
            crate::http::TransportError::Status {
                status,
                body,
                headers,
            } => ProviderError::from_status(*status, headers.clone(), body.clone()),
            other => ProviderError::from_request(other.to_string()),
        },
    )
    .await
    .map_err(|e| {
        let (status, body, message) = match &e {
            crate::http::TransportError::Status { status, body, .. } => (
                Some(*status),
                Some(body.clone()),
                format!("HTTP {status}: {body}"),
            ),
            other => (None, None, other.to_string()),
        };
        format_provider_error(
            &normalize_provider_error(status, body, message),
            Some("OpenAI API error"),
        )
    })?;

    if let Some(on_response) = &opts.base.on_response {
        on_response(
            crate::api::ProviderResponse {
                status: response.status,
                headers: response.headers.clone(),
            },
            model.clone(),
        )
        .await;
    }

    sink.push(AssistantMessageEvent::Start {
        partial: output.clone(),
    });

    // Parse the SSE stream into JSON events (OpenAI Responses emits
    // `data: {json}` lines with an `event:` field for some; we only need the JSON).
    let byte_stream: ByteStream = byte_stream;
    let sse_stream = byte_stream.map(|chunk| {
        chunk.map_err(|e| {
            crate::sse::SseError::Transport(crate::http::TransportError::to_string(&e))
        })
    });
    // B4: feed each parsed event into the state machine as it arrives,
    // rather than collecting every SSE event into a `Vec<Value>` and running
    // the state machine only after the stream ends — which held the whole
    // parsed response in memory and left the consumer seeing nothing until
    // the answer was complete.
    let mut sse_events = crate::sse::iterate_sse_messages(sse_stream);
    let mut state = ResponsesStreamState::new(Some(&ResponsesStreamOptions {
        service_tier: opts.service_tier.clone(),
        grammar_tool_input_properties: Some(grammar_tool_input_properties.clone()),
        service_tier_mode: ServiceTierMode::OpenAi,
    }));
    while let Some(item) = sse_events.next().await {
        let sse = item.map_err(|e| match e {
            crate::sse::SseError::ServerError(data) => data,
            crate::sse::SseError::Aborted => "Request was aborted".to_string(),
            crate::sse::SseError::Transport(message) => message,
        })?;
        if sse.data == "[DONE]" {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<Value>(&sse.data) {
            if v.is_object() {
                state.feed(&v, output, sink, model)?;
            }
        }
    }

    state.finish()?;

    // TS `:166-172`: aborted → throw; the "pending" sentinel is provably unreachable
    // (processResponsesStream always sets a concrete stop reason or throws first).
    if opts.base.signal.as_ref().is_some_and(|s| s.is_cancelled()) {
        return Err("Request was aborted".to_string());
    }
    if output.stop_reason == StopReason::Aborted || output.stop_reason == StopReason::Error {
        return Err(output
            .error_message
            .clone()
            .unwrap_or_else(|| "An unknown error occurred".to_string()));
    }
    Ok(())
}

/// `buildParams` (`openai-responses.ts:262-342`).
fn build_params(
    model: &Model,
    context: &Context,
    opts: &OpenAIResponsesOptions,
    compat: &ResolvedOpenAIResponsesCompat,
    grammar_tool_input_properties: &HashMap<String, String>,
) -> Result<Value, String> {
    let deferred_tools_mode = if compat.supports_additional_tools {
        Some(DeferredToolsMode::AdditionalTools)
    } else if compat.supports_tool_search {
        Some(DeferredToolsMode::ToolSearch)
    } else {
        None
    };
    let placement = split_deferred_tools(context, deferred_tools_mode.is_some());
    let allowed_providers: std::collections::HashSet<String> =
        ["openai", "openai-codex", "opencode"]
            .iter()
            .map(|s| s.to_string())
            .collect();
    let messages = convert_responses_messages(
        model,
        context,
        &allowed_providers,
        Some(
            &crate::api::openai_responses_shared::ConvertResponsesMessagesOptions {
                include_system_prompt: true,
                grammar_tool_input_properties: Some(grammar_tool_input_properties.clone()),
                deferred_tools: Some(placement.deferred_owned()),
                deferred_tools_mode,
                tool_options: ConvertResponsesToolsOptions {
                    strict: None,
                    supports_strict_mode: compat.supports_strict_mode,
                    supports_openai_grammar_tools: compat.supports_openai_grammar_tools,
                    defer_loading: false,
                },
            },
        ),
    )?;

    let cache_retention =
        resolve_cache_retention(opts.base.cache_retention, opts.base.env.as_ref());
    let disable_implicit_prompt_cache =
        cache_retention == CacheRetention::None && compat.supports_explicit_prompt_cache_mode;

    let mut params = Map::new();
    params.insert("model".into(), Value::String(model.id.clone()));
    params.insert("input".into(), Value::Array(messages));
    params.insert("stream".into(), Value::Bool(true));
    params.insert(
        "prompt_cache_key".into(),
        if cache_retention == CacheRetention::None {
            Value::Null
        } else {
            match clamp_openai_prompt_cache_key(opts.base.session_id.as_deref()) {
                Some(k) => Value::String(k),
                None => Value::Null,
            }
        },
    );
    params.insert(
        "prompt_cache_retention".into(),
        match get_prompt_cache_retention(compat, cache_retention) {
            Some(r) => Value::String(r.to_string()),
            None => Value::Null,
        },
    );
    params.insert(
        "prompt_cache_options".into(),
        if disable_implicit_prompt_cache {
            json!({ "mode": "explicit" })
        } else {
            Value::Null
        },
    );
    params.insert("store".into(), Value::Bool(false));

    if let Some(max_tokens) = opts.base.max_tokens {
        params.insert("max_output_tokens".into(), Value::from(max_tokens.max(16)));
    }
    if let Some(temperature) = opts.base.temperature {
        params.insert("temperature".into(), Value::from(temperature));
    }
    if let Some(service_tier) = &opts.service_tier {
        params.insert("service_tier".into(), service_tier.clone());
    }
    if !placement.immediate.is_empty() {
        let immediate: Vec<crate::types::message::Tool> =
            placement.immediate.iter().map(|t| (*t).clone()).collect();
        params.insert(
            "tools".into(),
            Value::Array(convert_responses_tools(
                &immediate,
                &ConvertResponsesToolsOptions {
                    strict: None,
                    supports_strict_mode: compat.supports_strict_mode,
                    supports_openai_grammar_tools: compat.supports_openai_grammar_tools,
                    defer_loading: false,
                },
            )?),
        );
    }
    if let Some(tool_choice) = &opts.tool_choice {
        params.insert("tool_choice".into(), tool_choice.clone());
    }

    if model.reasoning {
        if opts.reasoning_effort.is_some() || opts.reasoning_summary.is_some() {
            let effort = match opts.reasoning_effort {
                Some(effort) => {
                    let mapped = model
                        .thinking_level_map
                        .as_ref()
                        .and_then(|m| m.get(effort));
                    Value::String(
                        mapped
                            .unwrap_or(match effort {
                                ThinkingLevel::Minimal => "minimal",
                                ThinkingLevel::Low => "low",
                                ThinkingLevel::Medium => "medium",
                                ThinkingLevel::High => "high",
                                ThinkingLevel::Xhigh => "xhigh",
                                ThinkingLevel::Max => "max",
                            })
                            .to_string(),
                    )
                }
                None => Value::String("medium".to_string()),
            };
            let summary = opts.reasoning_summary.as_deref().unwrap_or("auto");
            params.insert(
                "reasoning".into(),
                json!({ "effort": effort, "summary": summary }),
            );
            params.insert("include".into(), json!(["reasoning.encrypted_content"]));
        } else if model.provider.0 != "github-copilot"
            && model
                .thinking_level_map
                .as_ref()
                .is_some_and(|m| m.off_value().is_some())
        {
            let off = model
                .thinking_level_map
                .as_ref()
                .and_then(|m| m.off_value())
                .unwrap_or("none");
            params.insert("reasoning".into(), json!({ "effort": off }));
        }
        if model.provider.0 == "xai" {
            params.insert("include".into(), json!(["reasoning.encrypted_content"]));
        }
    }

    // Last so custom keys override the named request fields.
    if let Some(sampling_params) = &opts.base.sampling_params {
        if let Some(obj) = sampling_params.as_object() {
            for (k, v) in obj {
                params.insert(k.clone(), v.clone());
            }
        }
    }

    Ok(Value::Object(params))
}

/// `createClient` headers (TS `:207-236`): model.headers → copilot dynamic → session-affinity
/// → options.headers → xai user-agent.
fn build_client_headers(
    model: &Model,
    context: &Context,
    opts: &OpenAIResponsesOptions,
    compat: &ResolvedOpenAIResponsesCompat,
    session_id: Option<&str>,
) -> Vec<(String, String)> {
    let mut headers: Vec<(String, String)> = model
        .headers
        .as_ref()
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default();

    if model.provider.0 == "github-copilot" {
        let has_images =
            crate::api::openai_completions::has_copilot_vision_input(&context.messages);
        headers.push((
            "X-Initiator".into(),
            crate::api::openai_completions::infer_copilot_initiator(&context.messages).into(),
        ));
        headers.push(("Openai-Intent".into(), "conversation-edits".into()));
        if has_images {
            headers.push(("Copilot-Vision-Request".into(), "true".into()));
        }
    }

    if let Some(session_id) = session_id {
        match compat.session_affinity_format {
            crate::types::ids::SessionAffinityFormat::Openrouter => {
                headers.push(("x-session-id".into(), session_id.to_string()));
            }
            crate::types::ids::SessionAffinityFormat::Openai => {
                headers.push(("session_id".into(), session_id.to_string()));
                headers.push(("x-client-request-id".into(), session_id.to_string()));
            }
            crate::types::ids::SessionAffinityFormat::OpenaiNosession => {
                headers.push(("x-client-request-id".into(), session_id.to_string()));
            }
        }
    }

    if let Some(extra) = &opts.base.headers {
        headers.extend(extra.iter().cloned());
    }

    if model.provider.0 == "xai" {
        headers.retain(|(k, _)| !k.eq_ignore_ascii_case("user-agent"));
        headers.push(("User-Agent".into(), "pi (browser)".to_string()));
    }

    headers
}

/// Unit type binding the module's free functions to [`crate::api::ProviderStreams`].
#[derive(Debug, Clone, Copy, Default)]
pub struct OpenAIResponses;

impl crate::api::ProviderStreams for OpenAIResponses {
    fn stream(
        &self,
        model: &Model,
        ctx: &Context,
        opts: Option<crate::api::StreamOptions>,
    ) -> AssistantMessageEventStream {
        let openai = opts.map(|base| OpenAIResponsesOptions {
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
