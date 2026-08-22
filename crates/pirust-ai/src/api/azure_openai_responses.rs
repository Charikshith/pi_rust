//! Azure OpenAI Responses adapter — Rust port of
//! `packages/ai/src/api/azure-openai-responses.ts` (stream/streamSimple + azure config
//! resolution + buildParams + createClient).
//!
//! Shares the message/tool conversion and the streaming state machine with
//! [`super::openai_responses_shared`] and the transport/retry shape with
//! [`super::openai_responses`]. Azure differs only in: no `apiKey` header-based fallback
//! (a key is required), deployment-name / base-url / api-version resolution, and no
//! service-tier pricing.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::{json, Map, Value};

use crate::api::openai_completions::clamp_openai_prompt_cache_key;
use crate::api::openai_responses_shared::{
    convert_responses_messages, convert_responses_tools, process_responses_stream,
    ConvertResponsesToolsOptions, ResponsesStreamOptions, ServiceTierMode,
};
use crate::http::DynTransport;
use crate::stream::{assistant_message_stream, AssistantMessageEventStream, AssistantMessageSink};
use crate::types::event::AssistantMessageEvent;
use crate::types::ids::StopReason;
use crate::types::message::{AssistantMessage, Context};
use crate::types::model::Model;
use crate::utils::error_body::{format_provider_error, normalize_provider_error};
use crate::utils::provider_retry::{retry_provider_request, ProviderError};

const DEFAULT_AZURE_API_VERSION: &str = "v1";
const OPENAI_RESPONSES_MIN_OUTPUT_TOKENS: u64 = 16;

/// Azure OpenAI Responses-specific options (TS `AzureOpenAIResponsesOptions`).
#[derive(Clone, Default)]
pub struct AzureOpenAIResponsesOptions {
    pub base: crate::api::StreamOptions,
    pub reasoning_effort: Option<crate::types::ids::ThinkingLevel>,
    pub tool_choice: Option<Value>,
    pub reasoning_summary: Option<String>,
    pub azure_api_version: Option<String>,
    pub azure_resource_name: Option<String>,
    pub azure_base_url: Option<String>,
    pub azure_deployment_name: Option<String>,
}

/// `parseDeploymentNameMap` — `"a=b,c=d"` → `Map`.
fn parse_deployment_name_map(value: Option<&str>) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let Some(value) = value else {
        return map;
    };
    for entry in value.split(',') {
        let trimmed = entry.trim();
        if trimmed.is_empty() {
            continue;
        }
        let mut parts = trimmed.splitn(2, '=');
        let (Some(model_id), Some(deployment)) = (parts.next(), parts.next()) else {
            continue;
        };
        if model_id.is_empty() || deployment.is_empty() {
            continue;
        }
        map.insert(model_id.trim().to_string(), deployment.trim().to_string());
    }
    map
}

/// `resolveDeploymentName` — explicit option, else the env map, else the model id.
fn resolve_deployment_name(model: &Model, opts: &AzureOpenAIResponsesOptions) -> String {
    if let Some(name) = &opts.azure_deployment_name {
        return name.clone();
    }
    let env_map = crate::auth::get_provider_env_value(
        "AZURE_OPENAI_DEPLOYMENT_NAME_MAP",
        opts.base.env.as_ref(),
    );
    if let Some(mapped) = parse_deployment_name_map(env_map.as_deref()).get(&model.id) {
        return mapped.clone();
    }
    model.id.clone()
}

/// `normalizeAzureBaseUrl` (`azure-openai-responses.ts:185-214`).
fn normalize_azure_base_url(base_url: &str) -> Result<String, String> {
    let trimmed = base_url.trim().trim_end_matches('/');
    let url = url::Url::parse(trimmed)
        .map_err(|_| format!("Invalid Azure OpenAI base URL: {base_url}"))?;
    let host = url.host_str().unwrap_or("");
    let is_azure_host = host.ends_with(".openai.azure.com")
        || host.ends_with(".cognitiveservices.azure.com")
        || host.ends_with(".ai.azure.com");
    let normalized_path = url.path().trim_end_matches('/').to_string();

    let mut out = url;
    if is_azure_host
        && (normalized_path.is_empty()
            || normalized_path == "/"
            || normalized_path == "/openai"
            || normalized_path == "/openai/v1/responses")
    {
        out.set_path("/openai/v1");
        out.set_query(None);
    }
    let s = out.to_string();
    Ok(s.trim_end_matches('/').to_string())
}

/// `buildDefaultBaseUrl(resourceName)`.
fn build_default_base_url(resource_name: &str) -> String {
    format!("https://{resource_name}.openai.azure.com/openai/v1")
}

/// `resolveAzureConfig` — apiVersion / baseUrl (explicit → env → model.baseUrl → error).
fn resolve_azure_config(
    model: &Model,
    opts: &AzureOpenAIResponsesOptions,
) -> Result<(String, String), String> {
    let api_version = opts
        .azure_api_version
        .clone()
        .or_else(|| {
            crate::auth::get_provider_env_value("AZURE_OPENAI_API_VERSION", opts.base.env.as_ref())
        })
        .unwrap_or_else(|| DEFAULT_AZURE_API_VERSION.to_string());

    let base_url = opts
        .azure_base_url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .or_else(|| {
            crate::auth::get_provider_env_value("AZURE_OPENAI_BASE_URL", opts.base.env.as_ref())
                .map(|s| s.trim().to_string())
        });
    let resource_name = opts.azure_resource_name.clone().or_else(|| {
        crate::auth::get_provider_env_value("AZURE_OPENAI_RESOURCE_NAME", opts.base.env.as_ref())
    });

    let mut resolved_base_url = base_url;
    if resolved_base_url.is_none() {
        if let Some(resource_name) = &resource_name {
            resolved_base_url = Some(build_default_base_url(resource_name));
        }
    }
    if resolved_base_url.is_none() && !model.base_url.is_empty() {
        resolved_base_url = Some(model.base_url.clone());
    }
    let Some(resolved_base_url) = resolved_base_url else {
        return Err("Azure OpenAI base URL is required. Set AZURE_OPENAI_BASE_URL or AZURE_OPENAI_RESOURCE_NAME, or pass azureBaseUrl, azureResourceName, or model.baseUrl.".to_string());
    };

    Ok((normalize_azure_base_url(&resolved_base_url)?, api_version))
}

/// `streamSimple` (TS `:162-180`).
pub fn stream_simple(
    model: &Model,
    context: &Context,
    opts: Option<crate::api::SimpleStreamOptions>,
) -> AssistantMessageEventStream {
    let opts = opts.unwrap_or_default();
    let mut base = opts.base.clone();
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
        Some(AzureOpenAIResponsesOptions {
            base,
            reasoning_effort,
            ..Default::default()
        }),
    )
}

/// The `stream` adapter (TS `:69-160`).
pub fn stream(
    model: &Model,
    context: &Context,
    opts: Option<AzureOpenAIResponsesOptions>,
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
    opts: AzureOpenAIResponsesOptions,
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

async fn run_produce(
    model: &Model,
    context: &Context,
    opts: &AzureOpenAIResponsesOptions,
    output: &mut AssistantMessage,
    sink: &mut AssistantMessageSink,
) -> Result<(), String> {
    let deployment_name = resolve_deployment_name(model, opts);

    // TS `:82-84`: azure REQUIRES an api key (no header fallback).
    let Some(api_key) = opts.base.api_key.clone() else {
        return Err(format!("No API key for provider: {}", model.provider.0));
    };

    let grammar_tool_input_properties =
        crate::api::constrained_sampling::create_grammar_tool_input_properties(
            context.tools.as_deref(),
            model
                .compat
                .as_ref()
                .and_then(|c| c.get("supportsOpenAIGrammarTools"))
                .and_then(Value::as_bool)
                .unwrap_or(false),
        );

    let mut params = build_params(
        model,
        context,
        opts,
        &deployment_name,
        &grammar_tool_input_properties,
    )?;
    if let Some(on_payload) = &opts.base.on_payload {
        if let Some(next) = on_payload(params.clone(), model.clone()).await {
            params = next;
        }
    }

    let (base_url, api_version) = resolve_azure_config(model, opts)?;
    let url = format!("{base_url}/responses?api-version={api_version}");

    let mut headers: Vec<(String, String)> = model
        .headers
        .as_ref()
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default();
    if let Some(extra) = &opts.base.headers {
        headers.extend(extra.iter().cloned());
    }

    let request = crate::http::HttpRequest {
        url,
        headers,
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
            Some("Azure OpenAI API error"),
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

    use futures::StreamExt;
    let sse_stream = byte_stream.map(|chunk| {
        chunk.map_err(|e| {
            crate::sse::SseError::Transport(crate::http::TransportError::to_string(&e))
        })
    });
    let mut sse_events = crate::sse::iterate_sse_messages(sse_stream);
    let mut json_events: Vec<Value> = Vec::new();
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
                json_events.push(v);
            }
        }
    }

    process_responses_stream(
        json_events.into_iter(),
        output,
        sink,
        model,
        Some(&ResponsesStreamOptions {
            service_tier: None,
            grammar_tool_input_properties: Some(grammar_tool_input_properties.clone()),
            service_tier_mode: ServiceTierMode::Disabled,
        }),
    )?;

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

/// `buildParams` (`azure-openai-responses.ts:274-337`).
fn build_params(
    model: &Model,
    context: &Context,
    opts: &AzureOpenAIResponsesOptions,
    deployment_name: &str,
    grammar_tool_input_properties: &HashMap<String, String>,
) -> Result<Value, String> {
    let allowed_providers: std::collections::HashSet<String> = [
        "openai",
        "openai-codex",
        "opencode",
        "azure-openai-responses",
    ]
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
                deferred_tools: None,
                deferred_tools_mode: None,
                tool_options: ConvertResponsesToolsOptions::default(),
            },
        ),
    )?;

    let supports_strict_mode = model
        .compat
        .as_ref()
        .and_then(|c| c.get("supportsStrictMode"))
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let supports_openai_grammar_tools = model
        .compat
        .as_ref()
        .and_then(|c| c.get("supportsOpenAIGrammarTools"))
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let mut params = Map::new();
    params.insert("model".into(), Value::String(deployment_name.to_string()));
    params.insert("input".into(), Value::Array(messages));
    params.insert("stream".into(), Value::Bool(true));
    params.insert(
        "prompt_cache_key".into(),
        match clamp_openai_prompt_cache_key(opts.base.session_id.as_deref()) {
            Some(k) => Value::String(k),
            None => Value::Null,
        },
    );
    params.insert("store".into(), Value::Bool(false));

    if let Some(max_tokens) = opts.base.max_tokens {
        params.insert(
            "max_output_tokens".into(),
            Value::from(max_tokens.max(OPENAI_RESPONSES_MIN_OUTPUT_TOKENS)),
        );
    }
    if let Some(temperature) = opts.base.temperature {
        params.insert("temperature".into(), Value::from(temperature));
    }
    if context.tools.as_ref().is_some_and(|t| !t.is_empty()) {
        let tools: Vec<crate::types::message::Tool> = context.tools.as_ref().unwrap().to_vec();
        params.insert(
            "tools".into(),
            Value::Array(convert_responses_tools(
                &tools,
                &ConvertResponsesToolsOptions {
                    strict: None,
                    supports_strict_mode,
                    supports_openai_grammar_tools,
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
                                crate::types::ids::ThinkingLevel::Minimal => "minimal",
                                crate::types::ids::ThinkingLevel::Low => "low",
                                crate::types::ids::ThinkingLevel::Medium => "medium",
                                crate::types::ids::ThinkingLevel::High => "high",
                                crate::types::ids::ThinkingLevel::Xhigh => "xhigh",
                                crate::types::ids::ThinkingLevel::Max => "max",
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
        } else if model
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
    }

    if let Some(sampling_params) = &opts.base.sampling_params {
        if let Some(obj) = sampling_params.as_object() {
            for (k, v) in obj {
                params.insert(k.clone(), v.clone());
            }
        }
    }

    Ok(Value::Object(params))
}

/// Unit type binding the module's free functions to [`crate::api::ProviderStreams`].
#[derive(Debug, Clone, Copy, Default)]
pub struct AzureOpenAIResponses;

impl crate::api::ProviderStreams for AzureOpenAIResponses {
    fn stream(
        &self,
        model: &Model,
        ctx: &Context,
        opts: Option<crate::api::StreamOptions>,
    ) -> AssistantMessageEventStream {
        let azure = opts.map(|base| AzureOpenAIResponsesOptions {
            base,
            ..Default::default()
        });
        stream(model, ctx, azure)
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
