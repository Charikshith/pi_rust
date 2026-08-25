//! `anthropic-messages` streaming adapter — Rust port of
//! `packages/ai/src/api/anthropic-messages.ts`.
//!
//! See `docs/analysis/06-anthropic-runtime-spec.md` §4. This module drives the Anthropic raw
//! SSE stream through an incrementally-built `AssistantMessage`, emitting `AssistantMessageEvent`s
//! in a fixed order (§4b), and produces the final message in *runtime key-insertion order*
//! (§4e) — the reason `types::message::AssistantMessage` / `types::usage::Usage` were reordered.
//! Request construction (§4a), stop-reason mapping (§4g), and cost (§4d) live here too.
//!
//! Like every `src/api/*` module, it IS a [`ProviderStreams`] — it exports `stream` and
//! `stream_simple` and also provides the [`AnthropicMessages`] unit implementing the trait.
//!
//! ## Divergences from Pi (documented, intentional)
//! - Every emitted event carries an *owned* `partial` snapshot cloned at emission time. Pi
//!   emits the same mutable `output` object by reference, so its captured tape shows every
//!   `partial` pointing at the FINAL mutated state (JS aliasing). Only the terminal
//!   `done.message` / `error.error` is byte-compared against Pi (spec §Oracle / golden test).
//! - `partialJson` accumulation is kept in a side scratch buffer keyed by content position,
//!   never on the `ToolCall` struct, so the struct's `partial_json` stays `None` throughout —
//!   matching Pi's post-`content_block_stop` state where the scratch buffer is deleted (§4c).
//! - `transformMessages` / `splitDeferredTools` (external Pi helpers, not ported) are omitted
//!   from request construction; the body build is best-effort per §4a and is NOT oracle-verified.

use std::collections::HashMap;

use serde_json::{Map, Value};

use crate::auth::{is_oauth_token, resolve_api_key};
use crate::http::{DynTransport, HttpRequest, ReqwestTransport, TransportError};
use crate::sse::{iterate_anthropic_events, SseError};
use crate::stream::{assistant_message_stream, AssistantMessageEventStream, AssistantMessageSink};
use crate::types::content::{
    AssistantContent, TextContent, ThinkingContent, ThinkingTag, ToolCall, ToolCallTag,
};
use crate::types::event::AssistantMessageEvent;
use crate::types::ids::StopReason;
use crate::types::message::{
    AssistantMessage, AssistantRole, Context, Message, UserMessageContent,
};
use crate::types::model::{Model, ModelCostRates};
use crate::types::usage::{Cost, Usage};

use futures::StreamExt;
use std::sync::Arc;

use super::{AnthropicOptions, ProviderStreams, SimpleStreamOptions, StreamOptions};

const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Stream a completion against the Anthropic Messages API (TS `stream`, `anthropic-messages.ts:484`).
///
/// Per the `StreamFunction` contract this must not panic after invocation — request/runtime
/// failures are delivered as an `error` event on the returned stream (spec §6).
pub fn stream(
    model: &Model,
    ctx: &Context,
    opts: Option<AnthropicOptions>,
) -> AssistantMessageEventStream {
    let (sink, stream) = assistant_message_stream();
    let model = model.clone();
    let ctx = ctx.clone();
    let opts = opts.unwrap_or_default();
    tokio::spawn(produce(model, ctx, opts, sink));
    stream
}

/// Stream with the coarse "simple" options (TS `streamSimple`, `:786`).
///
/// Minimal port: maps the base + `reasoning` level onto [`AnthropicOptions`] and delegates. The
/// `buildBaseOptions`/`adjustMaxTokensForThinking`/`clampMaxTokensToContext` helpers
/// (`simple-options.ts`, not ported) and the synchronous `assertRequestAuth` at `:791` are
/// intentionally omitted — auth is enforced inside `produce` as an `error` event.
pub fn stream_simple(
    model: &Model,
    ctx: &Context,
    opts: Option<SimpleStreamOptions>,
) -> AssistantMessageEventStream {
    let opts = opts.unwrap_or_default();
    let force_adaptive = force_adaptive_thinking(model);
    let anthropic = if opts.reasoning.is_none() {
        AnthropicOptions {
            base: opts.base,
            thinking_enabled: Some(false),
            ..Default::default()
        }
    } else if force_adaptive {
        AnthropicOptions {
            base: opts.base,
            thinking_enabled: Some(true),
            effort: Some("high".to_string()),
            ..Default::default()
        }
    } else {
        AnthropicOptions {
            base: opts.base,
            thinking_enabled: Some(true),
            ..Default::default()
        }
    };
    stream(model, ctx, Some(anthropic))
}

/// Unit type binding the module's free functions to the [`ProviderStreams`] trait.
#[derive(Debug, Clone, Copy, Default)]
pub struct AnthropicMessages;

impl ProviderStreams for AnthropicMessages {
    fn stream(
        &self,
        model: &Model,
        ctx: &Context,
        opts: Option<StreamOptions>,
    ) -> AssistantMessageEventStream {
        let anthropic = opts.map(|base| AnthropicOptions {
            base,
            ..Default::default()
        });
        stream(model, ctx, anthropic)
    }

    fn stream_simple(
        &self,
        model: &Model,
        ctx: &Context,
        opts: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream {
        stream_simple(model, ctx, opts)
    }
}

// ---------------------------------------------------------------------------------------------
// Producer (TS async IIFE, `:491-756`)
// ---------------------------------------------------------------------------------------------

/// A runtime failure surfaced from the state machine (the TS `throw` inside the IIFE `try`).
struct Failure {
    message: String,
    aborted: bool,
}

/// The async producer task (TS `(async () => { ... })()`, `:491`). Builds the incremental
/// `output`, drives the state machine, and terminates the stream with `done` or `error`.
async fn produce(
    model: Model,
    ctx: Context,
    opts: AnthropicOptions,
    mut sink: AssistantMessageSink,
) {
    let mut output = init_output(&model);
    match run_machine(&model, &ctx, &opts, &mut output, &mut sink).await {
        Ok(()) => {
            // TS `:743-744`: success path.
            let reason = output.stop_reason;
            sink.push(AssistantMessageEvent::Done {
                reason,
                message: output.clone(),
            });
            sink.end(None);
        }
        Err(failure) => {
            // TS catch `:745-754`. Scratch `index`/`partialJson` are side buffers here, so
            // nothing on the content blocks needs clearing.
            output.stop_reason = if failure.aborted {
                StopReason::Aborted
            } else {
                StopReason::Error
            };
            output.error_message = Some(failure.message);
            let reason = output.stop_reason;
            sink.push(AssistantMessageEvent::Error {
                reason,
                error: output.clone(),
            });
            sink.end(None);
        }
    }
}

/// The initial `output` literal (TS `:492-508`), key-insertion order per §4e.
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

/// `Date.now()` equivalent (TS `:507`). Non-deterministic; golden tests zero this before
/// comparison.
fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Side-channel scratch state (Pi's transient `Block.index` / `Block.partialJson`, kept off the
/// serialized structs). `block_indices[i]` is the Anthropic content-block index for
/// `output.content[i]` (`None` once its `content_block_stop` "deletes" it); `partial_json[i]` is
/// the accumulated tool-arg JSON for a tool block.
#[derive(Default)]
struct Scratch {
    block_indices: Vec<Option<u64>>,
    partial_json: HashMap<usize, String>,
}

impl Scratch {
    /// `blocks.findIndex(b => b.index === event.index)` (TS `:620` etc.).
    fn find(&self, index: u64) -> Option<usize> {
        self.block_indices
            .iter()
            .position(|slot| *slot == Some(index))
    }
}

/// Run the SSE decode + Anthropic event state machine (TS `:510-741`), returning `Err` for the
/// paths that TS turns into a `throw` (→ catch → `error` event).
async fn run_machine(
    model: &Model,
    ctx: &Context,
    opts: &AnthropicOptions,
    output: &mut AssistantMessage,
    sink: &mut AssistantMessageSink,
) -> Result<(), Failure> {
    // Resolve the transport (TS `:514-544`): an injected transport is Pi's `options.client`
    // path, which skips `createClient`/`assertRequestAuth` entirely.
    let (transport, is_oauth): (Arc<dyn DynTransport>, bool) = match &opts.transport {
        Some(transport) => (transport.clone(), false),
        None => {
            let api_key = resolve_api_key(opts.base.api_key.as_deref(), &env_map(opts));
            assert_request_auth(&model.provider.0, api_key.as_deref(), &opts.base.headers)?;
            let is_oauth = api_key.as_deref().map(is_oauth_token).unwrap_or(false);
            (
                Arc::new(ReqwestTransport::new()) as Arc<dyn DynTransport>,
                is_oauth,
            )
        }
    };

    let request = build_request(model, ctx, opts, is_oauth);
    let (byte_stream, _response) = transport
        .send_dyn(request)
        .await
        .map_err(|error| failure(transport_message(&error)))?;

    // TS `:557`: pre-loop `start` event.
    sink.push(AssistantMessageEvent::Start {
        partial: output.clone(),
    });

    let mut scratch = Scratch::default();
    let mut saw_message_start = false;
    let mut saw_message_end = false;

    let byte_stream = byte_stream
        .map(|chunk| chunk.map_err(|error| SseError::Transport(transport_message(&error))));
    let mut events = iterate_anthropic_events(byte_stream);

    while let Some(item) = events.next().await {
        let sse = match item {
            Ok(sse) => sse,
            // TS `:455-457`: `event: error` → throw the raw data. Transport/abort likewise.
            Err(SseError::ServerError(data)) => return Err(failure(data)),
            Err(SseError::Aborted) => {
                return Err(Failure {
                    message: "Request was aborted".to_string(),
                    aborted: true,
                })
            }
            Err(SseError::Transport(message)) => return Err(failure(message)),
        };

        // TS `:464`: parseJsonWithRepair; a parse failure is the diagnostic throw (`:471-476`).
        let event = crate::json_repair::parse_json_with_repair(&sse.data).map_err(|error| {
            failure(format!(
                "Could not parse Anthropic SSE event {}: {}; data={}; raw={}",
                sse.event.as_deref().unwrap_or(""),
                error,
                sse.data,
                sse.raw.join("\\n"),
            ))
        })?;

        match event.get("type").and_then(Value::as_str) {
            Some("message_start") => {
                saw_message_start = true;
                handle_message_start(model, output, &event);
            }
            Some("content_block_start") => {
                handle_content_block_start(is_oauth, ctx, output, &mut scratch, &event, sink);
            }
            Some("content_block_delta") => {
                handle_content_block_delta(output, &mut scratch, &event, sink);
            }
            Some("content_block_stop") => {
                handle_content_block_stop(output, &mut scratch, &event, sink);
            }
            Some("message_delta") => {
                handle_message_delta(model, output, &event)?;
            }
            Some("message_stop") => {
                saw_message_end = true;
            }
            _ => {}
        }
    }

    // TS `iterateAnthropicEvents` `:479-481`: ended before message_stop.
    if saw_message_start && !saw_message_end {
        return Err(failure(
            "Anthropic stream ended before message_stop".to_string(),
        ));
    }

    // TS `:739-741`: a refusal/error/aborted stop reason becomes a throw.
    if matches!(output.stop_reason, StopReason::Aborted | StopReason::Error) {
        return Err(Failure {
            message: output
                .error_message
                .clone()
                .unwrap_or_else(|| "An unknown error occurred".to_string()),
            aborted: matches!(output.stop_reason, StopReason::Aborted),
        });
    }

    Ok(())
}

fn failure(message: String) -> Failure {
    Failure {
        message,
        aborted: false,
    }
}

fn transport_message(error: &TransportError) -> String {
    error.to_string()
}

// ---------------------------------------------------------------------------------------------
// Event handlers (TS `:563-732`)
// ---------------------------------------------------------------------------------------------

/// TS `message_start` `:563-575`.
fn handle_message_start(model: &Model, output: &mut AssistantMessage, event: &Value) {
    let message = event.get("message");
    output.response_id = message
        .and_then(|m| m.get("id"))
        .and_then(Value::as_str)
        .map(str::to_string);

    // 0.84.2 (`output.model = event.message.model`, anthropic-messages.ts:604): the
    // wire's `message_start.message.model` now OVERWRITES the requested model id. When
    // absent (fixture SSE has no `message.model`), the JS value becomes `undefined` and
    // `JSON.stringify` drops the key — so here it maps to `None` and is omitted.
    output.model = message
        .and_then(|m| m.get("model"))
        .and_then(Value::as_str)
        .map(str::to_string);

    let usage = message.and_then(|m| m.get("usage"));
    output.usage.input = usage_u64(usage, "input_tokens");
    output.usage.output = usage_u64(usage, "output_tokens");
    output.usage.cache_read = usage_u64(usage, "cache_read_input_tokens");
    output.usage.cache_write = usage_u64(usage, "cache_creation_input_tokens");
    output.usage.cache_write1h = Some(
        usage
            .and_then(|u| u.get("cache_creation"))
            .and_then(|c| c.get("ephemeral_1h_input_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0),
    );
    output.usage.total_tokens = Some(
        output.usage.input
            + output.usage.output
            + output.usage.cache_read
            + output.usage.cache_write,
    );
    calculate_cost(model, &mut output.usage);
}

/// `usage[key] || 0` (TS falsy coalesce): missing/null/0 → 0.
fn usage_u64(usage: Option<&Value>, key: &str) -> u64 {
    usage
        .and_then(|u| u.get(key))
        .and_then(Value::as_u64)
        .unwrap_or(0)
}

/// TS `content_block_start` `:576-617`.
fn handle_content_block_start(
    is_oauth: bool,
    ctx: &Context,
    output: &mut AssistantMessage,
    scratch: &mut Scratch,
    event: &Value,
    sink: &mut AssistantMessageSink,
) {
    let index = event.get("index").and_then(Value::as_u64).unwrap_or(0);
    let block = event.get("content_block");
    let block_type = block.and_then(|b| b.get("type")).and_then(Value::as_str);

    match block_type {
        Some("text") => {
            output
                .content
                .push(AssistantContent::Text(TextContent::new("")));
            scratch.block_indices.push(Some(index));
            let content_index = (output.content.len() - 1) as u32;
            sink.push(AssistantMessageEvent::TextStart {
                content_index,
                partial: output.clone(),
            });
        }
        Some("thinking") => {
            output
                .content
                .push(AssistantContent::Thinking(ThinkingContent {
                    kind: ThinkingTag::Thinking,
                    thinking: String::new(),
                    thinking_signature: Some(String::new()),
                    redacted: None,
                }));
            scratch.block_indices.push(Some(index));
            let content_index = (output.content.len() - 1) as u32;
            sink.push(AssistantMessageEvent::ThinkingStart {
                content_index,
                partial: output.clone(),
            });
        }
        Some("redacted_thinking") => {
            let data = block
                .and_then(|b| b.get("data"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            output
                .content
                .push(AssistantContent::Thinking(ThinkingContent {
                    kind: ThinkingTag::Thinking,
                    thinking: "[Reasoning redacted]".to_string(),
                    thinking_signature: Some(data),
                    redacted: Some(true),
                }));
            scratch.block_indices.push(Some(index));
            let content_index = (output.content.len() - 1) as u32;
            sink.push(AssistantMessageEvent::ThinkingStart {
                content_index,
                partial: output.clone(),
            });
        }
        Some("tool_use") => {
            let id = block
                .and_then(|b| b.get("id"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let raw_name = block
                .and_then(|b| b.get("name"))
                .and_then(Value::as_str)
                .unwrap_or("");
            let name = if is_oauth {
                from_claude_code_name(raw_name, ctx)
            } else {
                raw_name.to_string()
            };
            let arguments = block
                .and_then(|b| b.get("input"))
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            output.content.push(AssistantContent::ToolCall(ToolCall {
                kind: ToolCallTag::ToolCall,
                id,
                name,
                arguments,
                thought_signature: None,
                namespace: None,
                partial_json: None,
            }));
            let position = output.content.len() - 1;
            scratch.block_indices.push(Some(index));
            scratch.partial_json.insert(position, String::new());
            let content_index = position as u32;
            sink.push(AssistantMessageEvent::ToolcallStart {
                content_index,
                partial: output.clone(),
            });
        }
        _ => {}
    }
}

/// TS `content_block_delta` `:618-663`.
fn handle_content_block_delta(
    output: &mut AssistantMessage,
    scratch: &mut Scratch,
    event: &Value,
    sink: &mut AssistantMessageSink,
) {
    let index = event.get("index").and_then(Value::as_u64).unwrap_or(0);
    let delta = event.get("delta");
    let delta_type = delta.and_then(|d| d.get("type")).and_then(Value::as_str);
    let Some(position) = scratch.find(index) else {
        return;
    };

    match delta_type {
        Some("text_delta") => {
            let text = delta
                .and_then(|d| d.get("text"))
                .and_then(Value::as_str)
                .unwrap_or("");
            if let Some(AssistantContent::Text(block)) = output.content.get_mut(position) {
                block.text.push_str(text);
                sink.push(AssistantMessageEvent::TextDelta {
                    content_index: position as u32,
                    delta: text.to_string(),
                    partial: output.clone(),
                });
            }
        }
        Some("thinking_delta") => {
            let text = delta
                .and_then(|d| d.get("thinking"))
                .and_then(Value::as_str)
                .unwrap_or("");
            if let Some(AssistantContent::Thinking(block)) = output.content.get_mut(position) {
                block.thinking.push_str(text);
                sink.push(AssistantMessageEvent::ThinkingDelta {
                    content_index: position as u32,
                    delta: text.to_string(),
                    partial: output.clone(),
                });
            }
        }
        Some("input_json_delta") => {
            let fragment = delta
                .and_then(|d| d.get("partial_json"))
                .and_then(Value::as_str)
                .unwrap_or("");
            if let Some(AssistantContent::ToolCall(_)) = output.content.get(position) {
                let accumulated = scratch.partial_json.entry(position).or_default();
                accumulated.push_str(fragment);
                let arguments = parse_arguments(accumulated);
                if let Some(AssistantContent::ToolCall(block)) = output.content.get_mut(position) {
                    block.arguments = arguments;
                }
                sink.push(AssistantMessageEvent::ToolcallDelta {
                    content_index: position as u32,
                    delta: fragment.to_string(),
                    partial: output.clone(),
                });
            }
        }
        Some("signature_delta") => {
            let signature = delta
                .and_then(|d| d.get("signature"))
                .and_then(Value::as_str)
                .unwrap_or("");
            if let Some(AssistantContent::Thinking(block)) = output.content.get_mut(position) {
                let sig = block.thinking_signature.get_or_insert_with(String::new);
                sig.push_str(signature);
            }
        }
        _ => {}
    }
}

/// TS `content_block_stop` `:664-695`.
fn handle_content_block_stop(
    output: &mut AssistantMessage,
    scratch: &mut Scratch,
    event: &Value,
    sink: &mut AssistantMessageSink,
) {
    let index = event.get("index").and_then(Value::as_u64).unwrap_or(0);
    let Some(position) = scratch.find(index) else {
        return;
    };
    // TS `delete block.index`.
    scratch.block_indices[position] = None;

    match output.content.get(position) {
        Some(AssistantContent::Text(block)) => {
            let content = block.text.clone();
            sink.push(AssistantMessageEvent::TextEnd {
                content_index: position as u32,
                content,
                partial: output.clone(),
            });
        }
        Some(AssistantContent::Thinking(block)) => {
            let content = block.thinking.clone();
            sink.push(AssistantMessageEvent::ThinkingEnd {
                content_index: position as u32,
                content,
                partial: output.clone(),
            });
        }
        Some(AssistantContent::ToolCall(_)) => {
            let accumulated = scratch.partial_json.remove(&position).unwrap_or_default();
            let arguments = parse_arguments(&accumulated);
            let tool_call =
                if let Some(AssistantContent::ToolCall(block)) = output.content.get_mut(position) {
                    block.arguments = arguments;
                    // `partial_json` stays None (Pi deletes the scratch buffer on stop).
                    block.clone()
                } else {
                    return;
                };
            sink.push(AssistantMessageEvent::ToolcallEnd {
                content_index: position as u32,
                tool_call,
                partial: output.clone(),
            });
        }
        None => {}
    }
}

/// `parseStreamingJson` returning a tool-arg object map (TS `:648`, `:684`).
fn parse_arguments(partial_json: &str) -> Map<String, Value> {
    match crate::json_repair::parse_streaming_json(partial_json) {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

/// TS `message_delta` `:696-732`.
fn handle_message_delta(
    model: &Model,
    output: &mut AssistantMessage,
    event: &Value,
) -> Result<(), Failure> {
    let delta = event.get("delta");
    if let Some(stop_reason) = delta
        .and_then(|d| d.get("stop_reason"))
        .and_then(Value::as_str)
    {
        // 0.84.2 (`output.rawStopReason = event.delta.stop_reason`, :738): set BEFORE
        // the mapped stopReason, so it lands between responseId and errorMessage in the
        // runtime key order.
        output.raw_stop_reason = Some(stop_reason.to_string());
        let stop_details = delta.and_then(|d| d.get("stop_details"));
        let mapped = map_stop_reason(stop_reason, stop_details)?;
        output.stop_reason = mapped.stop_reason;
        if let Some(message) = mapped.error_message {
            output.error_message = Some(message);
        }
    }

    if let Some(usage) = event.get("usage") {
        if let Some(value) = present_u64(usage, "input_tokens") {
            output.usage.input = value;
        }
        if let Some(value) = present_u64(usage, "output_tokens") {
            output.usage.output = value;
        }
        if let Some(value) = present_u64(usage, "cache_read_input_tokens") {
            output.usage.cache_read = value;
        }
        if let Some(value) = present_u64(usage, "cache_creation_input_tokens") {
            output.usage.cache_write = value;
        }
        if let Some(thinking) = usage
            .get("output_tokens_details")
            .and_then(|d| d.get("thinking_tokens"))
        {
            if !thinking.is_null() {
                output.usage.reasoning = thinking.as_u64();
            }
        }
    }

    output.usage.total_tokens = Some(
        output.usage.input
            + output.usage.output
            + output.usage.cache_read
            + output.usage.cache_write,
    );
    calculate_cost(model, &mut output.usage);
    Ok(())
}

/// A field that is present AND non-null (TS `!= null`), as a `u64`.
fn present_u64(usage: &Value, key: &str) -> Option<u64> {
    match usage.get(key) {
        Some(value) if !value.is_null() => Some(value.as_u64().unwrap_or(0)),
        _ => None,
    }
}

// ---------------------------------------------------------------------------------------------
// Stop-reason mapping (TS `mapStopReason`, `:1287-1313`)
// ---------------------------------------------------------------------------------------------

struct MappedStopReason {
    stop_reason: StopReason,
    error_message: Option<String>,
}

fn map_stop_reason(
    reason: &str,
    stop_details: Option<&Value>,
) -> Result<MappedStopReason, Failure> {
    let plain = |stop_reason| {
        Ok(MappedStopReason {
            stop_reason,
            error_message: None,
        })
    };
    match reason {
        "end_turn" => plain(StopReason::Stop),
        "max_tokens" => plain(StopReason::Length),
        "tool_use" => plain(StopReason::ToolUse),
        "refusal" => {
            let explanation = stop_details
                .and_then(|d| d.get("explanation"))
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .unwrap_or("The model refused to complete the request")
                .to_string();
            Ok(MappedStopReason {
                stop_reason: StopReason::Error,
                error_message: Some(explanation),
            })
        }
        "pause_turn" => plain(StopReason::Stop),
        "stop_sequence" => plain(StopReason::Stop),
        "sensitive" => plain(StopReason::Error),
        other => Err(failure(format!("Unhandled stop reason: {other}"))),
    }
}

// ---------------------------------------------------------------------------------------------
// Cost (TS `calculateCost`, `models.ts:639-659`)
// ---------------------------------------------------------------------------------------------

/// Port of `calculateCost` (`models.ts:639-659`): pick the highest matching pricing tier by
/// input-token threshold, split 1h cache writes (billed at `2×input`) from short writes, and
/// fill `usage.cost` in place. Arithmetic mirrors JS `number` (f64) operation order so the
/// resulting bit patterns match `JSON.stringify` (e.g. `7.750625`, `0.000037000000000000005`).
pub fn calculate_cost(model: &Model, usage: &mut Usage) {
    let input_tokens = usage.input + usage.cache_read + usage.cache_write;
    let mut rates: &ModelCostRates = &model.cost.rates;
    let mut matched_threshold: i128 = -1;
    if let Some(tiers) = &model.cost.tiers {
        for tier in tiers {
            if input_tokens > tier.input_tokens_above
                && (tier.input_tokens_above as i128) > matched_threshold
            {
                rates = &tier.rates;
                matched_threshold = tier.input_tokens_above as i128;
            }
        }
    }

    let long_write = usage.cache_write1h.unwrap_or(0) as f64;
    let short_write = usage.cache_write as f64 - long_write;
    usage.cost.input = (rates.input / 1_000_000.0) * usage.input as f64;
    usage.cost.output = (rates.output / 1_000_000.0) * usage.output as f64;
    usage.cost.cache_read = (rates.cache_read / 1_000_000.0) * usage.cache_read as f64;
    usage.cost.cache_write =
        (rates.cache_write * short_write + rates.input * 2.0 * long_write) / 1_000_000.0;
    usage.cost.total =
        usage.cost.input + usage.cost.output + usage.cost.cache_read + usage.cost.cache_write;
}

// ---------------------------------------------------------------------------------------------
// Request construction (TS §4a) — best-effort, NOT oracle-verified
// ---------------------------------------------------------------------------------------------

fn env_map(opts: &AnthropicOptions) -> std::collections::BTreeMap<String, String> {
    opts.base
        .env
        .as_ref()
        .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default()
}

/// TS `assertRequestAuth` `:280-290`.
fn assert_request_auth(
    provider: &str,
    api_key: Option<&str>,
    headers: &Option<Vec<(String, String)>>,
) -> Result<(), Failure> {
    if api_key.is_some() {
        return Ok(());
    }
    let has = |name: &str| {
        headers.as_ref().is_some_and(|hs| {
            hs.iter()
                .any(|(k, v)| k.eq_ignore_ascii_case(name) && !v.trim().is_empty())
        })
    };
    if has("authorization") || has("x-api-key") || has("cf-aig-authorization") {
        return Ok(());
    }
    Err(failure(format!("No API key for provider: {provider}")))
}

/// Build the outbound request (TS `buildParams` `:920-1047` + `createClient` headers `:832-918`).
/// Best-effort per §4a; `transformMessages`/`splitDeferredTools` are not ported, so message and
/// tool conversion is a direct pass over the context. NOT oracle-verified.
fn build_request(
    model: &Model,
    ctx: &Context,
    opts: &AnthropicOptions,
    is_oauth: bool,
) -> HttpRequest {
    let url = format!("{}/v1/messages", model.base_url);

    let mut body = Map::new();
    body.insert("model".to_string(), Value::String(model.id.clone()));
    body.insert("messages".to_string(), Value::Array(convert_messages(ctx)));
    let max_tokens = opts.base.max_tokens.unwrap_or(model.max_tokens);
    body.insert("max_tokens".to_string(), Value::from(max_tokens));
    body.insert("stream".to_string(), Value::Bool(true));

    if let Some(system) = &ctx.system_prompt {
        body.insert(
            "system".to_string(),
            Value::Array(vec![json_text_block(system)]),
        );
    }

    if opts.base.temperature.is_some() && opts.thinking_enabled != Some(true) {
        if let Some(temp) = opts.base.temperature {
            if let Some(number) = serde_json::Number::from_f64(temp) {
                body.insert("temperature".to_string(), Value::Number(number));
            }
        }
    }

    if let Some(tools) = &ctx.tools {
        if !tools.is_empty() {
            body.insert("tools".to_string(), Value::Array(convert_tools(tools)));
        }
    }

    if let Some(tool_choice) = &opts.tool_choice {
        let value = match tool_choice {
            Value::String(name) => {
                let mut m = Map::new();
                m.insert("type".to_string(), Value::String(name.clone()));
                Value::Object(m)
            }
            other => other.clone(),
        };
        body.insert("tool_choice".to_string(), value);
    }

    // B3: `thinking` was never serialized at all — the model's reasoning
    // level had no effect on the actual request. Gated on `model.reasoning`
    // (`:1000-1029`); adaptive models get `{type:"adaptive"}` plus a
    // top-level `output_config.effort` when `effort` is set, older models
    // get `{type:"enabled"|"disabled"}`. `display` defaults to "summarized".
    if model.reasoning {
        let display = opts
            .thinking_display
            .clone()
            .unwrap_or_else(|| "summarized".to_string());
        if force_adaptive_thinking(model) {
            let mut adaptive = Map::new();
            adaptive.insert("type".to_string(), Value::String("adaptive".to_string()));
            adaptive.insert("display".to_string(), Value::String(display));
            body.insert("thinking".to_string(), Value::Object(adaptive));
            if let Some(effort) = &opts.effort {
                let mut output_config = Map::new();
                output_config.insert("effort".to_string(), Value::String(effort.clone()));
                body.insert("output_config".to_string(), Value::Object(output_config));
            }
        } else if opts.thinking_enabled == Some(true) {
            let budget = opts.thinking_budget_tokens.unwrap_or(1024);
            let mut enabled = Map::new();
            enabled.insert("type".to_string(), Value::String("enabled".to_string()));
            enabled.insert("budget_tokens".to_string(), Value::from(budget));
            enabled.insert("display".to_string(), Value::String(display));
            body.insert("thinking".to_string(), Value::Object(enabled));
        } else {
            // `thinkingLevelMap.off === null` (an explicit JSON null, not an
            // absent field) means this model can't have thinking disabled —
            // skip the block entirely rather than sending `{type:"disabled"}`.
            let off_blocked = matches!(
                model.thinking_level_map.as_ref().and_then(|m| m.off.as_ref()),
                Some(None)
            );
            if !off_blocked {
                let mut disabled = Map::new();
                disabled.insert("type".to_string(), Value::String("disabled".to_string()));
                body.insert("thinking".to_string(), Value::Object(disabled));
            }
        }
    }

    // `metadata: {user_id}` only when present (`:1031-1036`) — previously
    // dropped entirely regardless of whether the caller set it.
    if let Some(user_id) = opts.base.metadata.as_ref().and_then(|m| m.user_id.clone()) {
        let mut metadata = Map::new();
        metadata.insert("user_id".to_string(), Value::String(user_id));
        body.insert("metadata".to_string(), Value::Object(metadata));
    }

    // Extra sampling params merged last over the request body — previously
    // dropped entirely, so callers had no way to pass provider-specific
    // extras through.
    if let Some(Value::Object(extra)) = &opts.base.sampling_params {
        for (key, value) in extra {
            body.insert(key.clone(), value.clone());
        }
    }

    let body = Value::Object(body).to_string();

    let mut headers = vec![
        ("accept".to_string(), "application/json".to_string()),
        ("content-type".to_string(), "application/json".to_string()),
        (
            "anthropic-version".to_string(),
            ANTHROPIC_VERSION.to_string(),
        ),
        (
            "anthropic-dangerous-direct-browser-access".to_string(),
            "true".to_string(),
        ),
    ];
    if let Some(api_key) = resolve_api_key(opts.base.api_key.as_deref(), &env_map(opts)) {
        if is_oauth {
            headers.push(("authorization".to_string(), format!("Bearer {api_key}")));
        } else {
            headers.push(("x-api-key".to_string(), api_key));
        }
    }
    if let Some(extra) = &opts.base.headers {
        headers.extend(extra.iter().cloned());
    }

    HttpRequest {
        url,
        headers,
        body,
        authorization: None,
    }
}

fn json_text_block(text: &str) -> Value {
    let mut m = Map::new();
    m.insert("type".to_string(), Value::String("text".to_string()));
    m.insert("text".to_string(), Value::String(text.to_string()));
    Value::Object(m)
}

/// Minimal `convertMessages` (TS `:1089-1254`) covering the message shapes exercised by the
/// golden fixtures (user string/blocks, assistant, tool result). NOT oracle-verified.
fn convert_messages(ctx: &Context) -> Vec<Value> {
    let mut out = Vec::new();
    for message in &ctx.messages {
        match message {
            Message::User(user) => match &user.content {
                UserMessageContent::Text(text) => {
                    if !text.trim().is_empty() {
                        let mut m = Map::new();
                        m.insert("role".to_string(), Value::String("user".to_string()));
                        m.insert("content".to_string(), Value::String(text.clone()));
                        out.push(Value::Object(m));
                    }
                }
                UserMessageContent::Blocks(blocks) => {
                    let content = serde_json::to_value(blocks).unwrap_or(Value::Null);
                    let mut m = Map::new();
                    m.insert("role".to_string(), Value::String("user".to_string()));
                    m.insert("content".to_string(), content);
                    out.push(Value::Object(m));
                }
            },
            Message::Assistant(assistant) => {
                let content = serde_json::to_value(&assistant.content).unwrap_or(Value::Null);
                let mut m = Map::new();
                m.insert("role".to_string(), Value::String("assistant".to_string()));
                m.insert("content".to_string(), content);
                out.push(Value::Object(m));
            }
            Message::ToolResult(result) => {
                let mut block = Map::new();
                block.insert("type".to_string(), Value::String("tool_result".to_string()));
                block.insert(
                    "tool_use_id".to_string(),
                    Value::String(result.tool_call_id.clone()),
                );
                block.insert(
                    "content".to_string(),
                    serde_json::to_value(&result.content).unwrap_or(Value::Null),
                );
                block.insert("is_error".to_string(), Value::Bool(result.is_error));
                let mut m = Map::new();
                m.insert("role".to_string(), Value::String("user".to_string()));
                m.insert(
                    "content".to_string(),
                    Value::Array(vec![Value::Object(block)]),
                );
                out.push(Value::Object(m));
            }
        }
    }
    out
}

/// Minimal `convertTools` (TS `:1260-1285`). NOT oracle-verified.
fn convert_tools(tools: &[crate::types::message::Tool]) -> Vec<Value> {
    tools
        .iter()
        .map(|tool| {
            let properties = tool
                .parameters
                .get("properties")
                .cloned()
                .unwrap_or_else(|| Value::Object(Map::new()));
            let required = tool
                .parameters
                .get("required")
                .cloned()
                .unwrap_or_else(|| Value::Array(Vec::new()));
            let mut schema = Map::new();
            schema.insert("type".to_string(), Value::String("object".to_string()));
            schema.insert("properties".to_string(), properties);
            schema.insert("required".to_string(), required);

            let mut m = Map::new();
            m.insert("name".to_string(), Value::String(tool.name.clone()));
            m.insert(
                "description".to_string(),
                Value::String(tool.description.clone()),
            );
            m.insert("input_schema".to_string(), Value::Object(schema));
            Value::Object(m)
        })
        .collect()
}

/// TS `fromClaudeCodeName` `:103-110`: map a Claude-Code tool name back to the caller's
/// original casing when a matching tool exists (case-insensitive).
fn from_claude_code_name(name: &str, ctx: &Context) -> String {
    if let Some(tools) = &ctx.tools {
        let lower = name.to_lowercase();
        if let Some(matched) = tools.iter().find(|t| t.name.to_lowercase() == lower) {
            return matched.name.clone();
        }
    }
    name.to_string()
}

/// TS `model.compat?.forceAdaptiveThinking === true`.
fn force_adaptive_thinking(model: &Model) -> bool {
    model
        .compat
        .as_ref()
        .and_then(|c| c.get("forceAdaptiveThinking"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}
