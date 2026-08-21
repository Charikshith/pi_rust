//! The agent engine — port of `packages/agent/src/agent-loop.ts`.
//!
//! Spec: `docs/analysis/07-agent-core-spec.md` §2. `[INTEGRATOR]` module (§13,
//! wave 3): reuses pi-ai's `EventStream<T,R>` / `AssistantMessageEventStream`
//! (`crates/pirust-ai/src/stream/mod.rs`) — the stream is NOT reimplemented here.
//!
//! Public surface (§2.1): [`agent_loop`], [`agent_loop_continue`],
//! [`run_agent_loop`], [`run_agent_loop_continue`], the two-level [`run_loop`]
//! (agent-loop.ts:155-275), [`stream_assistant_response`] (281-374), and the
//! 3-stage tool pipeline (413-792). `AbortSignal` → [`CancellationToken`] (§6).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use futures::future::BoxFuture;
use futures::stream::{FuturesUnordered, StreamExt};
use pirust_ai::api::{SimpleStreamOptions, StreamOptions};
use pirust_ai::stream::{
    assistant_message_stream, channel, AssistantMessageEventStream, EventSink, EventStream,
};
use pirust_ai::types::{
    AssistantContent, AssistantMessage, AssistantMessageEvent, AssistantRole, Context, Cost,
    Message, Model, StopReason, TextContent, ThinkingLevel as AiThinkingLevel, Tool, ToolCall,
    ToolResultMessage, ToolResultRole, Usage, UserContent,
};
use serde_json::{Map, Value};
use tokio_util::sync::CancellationToken;

use crate::harness::messages::AgentMessage;
use crate::types::{
    AfterToolCallContext, AgentContext, AgentEvent, AgentLoopConfig, AgentTool, AgentToolCall,
    AgentToolResult, AgentToolUpdateCallback, BeforeToolCallContext, GetMessagesFn,
    PrepareNextTurnContext, ShouldStopAfterTurnContext, ThinkingLevel, ToolExecutionMode,
};

/// A sink for [`AgentEvent`]s (TS `AgentEventSink = (event) => Promise|void`,
/// agent-loop.ts:25). `FnMut` so it can drive a stateful producer (the
/// [`EventStream`] sink or a test tape); it returns a `'static` future so the
/// borrow ends before the loop awaits it.
pub type AgentEventSink = Box<dyn FnMut(AgentEvent) -> BoxFuture<'static, ()> + Send>;

/// A provider stream function (TS `StreamFn` / the `streamFn || streamSimple`
/// seam at agent-loop.ts:304). Takes an owned model, context, resolved options
/// and cancellation token. Owned arguments keep the boxed `Fn` free of lifetimes
/// so it satisfies the detached `tokio::spawn` in [`agent_loop`].
pub type StreamFn = Arc<
    dyn Fn(Model, Context, SimpleStreamOptions, CancellationToken) -> AssistantMessageEventStream
        + Send
        + Sync,
>;

/// Errors from the continue-guards (TS throws at agent-loop.ts:71,74).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ContinueError {
    /// Context had no messages to continue from.
    #[error("Cannot continue: no messages in context")]
    NoMessages,
    /// The last message role was `assistant` (the provider would reject it).
    #[error("Cannot continue from message role: assistant")]
    LastIsAssistant,
}

// --- public wrappers (§2.1) --------------------------------------------------

/// Start an agent loop with new prompt messages (TS `agentLoop`,
/// agent-loop.ts:31-54). Returns an [`EventStream`] whose completion predicate is
/// `agent_end` and whose result is the produced messages; the loop runs as a
/// detached `tokio::spawn` task (fire-and-forget).
pub fn agent_loop(
    prompts: Vec<AgentMessage>,
    context: AgentContext,
    config: AgentLoopConfig,
    signal: Option<CancellationToken>,
    stream_fn: Option<StreamFn>,
) -> EventStream<AgentEvent, Vec<AgentMessage>> {
    let (sink, stream) = create_agent_stream();
    tokio::spawn(async move {
        let mut emitter = sink_emitter(sink);
        let _ = run_agent_loop(prompts, context, config, &mut emitter, signal, stream_fn).await;
    });
    stream
}

/// Continue an agent loop from the current context without a new prompt (TS
/// `agentLoopContinue`, agent-loop.ts:64-93). Guards are checked eagerly (TS
/// throws); on success the loop runs detached.
pub fn agent_loop_continue(
    context: AgentContext,
    config: AgentLoopConfig,
    signal: Option<CancellationToken>,
    stream_fn: Option<StreamFn>,
) -> Result<EventStream<AgentEvent, Vec<AgentMessage>>, ContinueError> {
    check_continue_guards(&context)?;
    let (sink, stream) = create_agent_stream();
    tokio::spawn(async move {
        let mut emitter = sink_emitter(sink);
        let _ = run_agent_loop_continue(context, config, &mut emitter, signal, stream_fn).await;
    });
    Ok(stream)
}

/// Seed `newMessages`/`context` from prompts, emit `agent_start` + `turn_start` +
/// the prompt `message_start`/`message_end` pairs, then drive [`run_loop`] (TS
/// `runAgentLoop`, agent-loop.ts:95-118).
pub async fn run_agent_loop(
    prompts: Vec<AgentMessage>,
    context: AgentContext,
    config: AgentLoopConfig,
    sink: &mut AgentEventSink,
    signal: Option<CancellationToken>,
    stream_fn: Option<StreamFn>,
) -> Vec<AgentMessage> {
    let token = signal.unwrap_or_default();
    let mut new_messages: Vec<AgentMessage> = prompts.clone();
    let mut current_context = context;
    current_context.messages.extend(prompts.iter().cloned());

    emit(sink, AgentEvent::AgentStart).await;
    emit(sink, AgentEvent::TurnStart).await;
    for prompt in &prompts {
        emit(
            sink,
            AgentEvent::MessageStart {
                message: prompt.clone(),
            },
        )
        .await;
        emit(
            sink,
            AgentEvent::MessageEnd {
                message: prompt.clone(),
            },
        )
        .await;
    }

    run_loop(
        current_context,
        &mut new_messages,
        config,
        &token,
        sink,
        stream_fn.as_ref(),
    )
    .await;
    new_messages
}

/// Continue variant of [`run_agent_loop`] (TS `runAgentLoopContinue`,
/// agent-loop.ts:120-143): no prompt seeding, `agent_start` + `turn_start` only.
pub async fn run_agent_loop_continue(
    context: AgentContext,
    config: AgentLoopConfig,
    sink: &mut AgentEventSink,
    signal: Option<CancellationToken>,
    stream_fn: Option<StreamFn>,
) -> Result<Vec<AgentMessage>, ContinueError> {
    check_continue_guards(&context)?;
    let token = signal.unwrap_or_default();
    let mut new_messages: Vec<AgentMessage> = Vec::new();
    let current_context = context;

    emit(sink, AgentEvent::AgentStart).await;
    emit(sink, AgentEvent::TurnStart).await;

    run_loop(
        current_context,
        &mut new_messages,
        config,
        &token,
        sink,
        stream_fn.as_ref(),
    )
    .await;
    Ok(new_messages)
}

fn check_continue_guards(context: &AgentContext) -> Result<(), ContinueError> {
    if context.messages.is_empty() {
        return Err(ContinueError::NoMessages);
    }
    if is_assistant_message(context.messages.last().expect("non-empty checked above")) {
        return Err(ContinueError::LastIsAssistant);
    }
    Ok(())
}

fn create_agent_stream() -> (
    EventSink<AgentEvent, Vec<AgentMessage>>,
    EventStream<AgentEvent, Vec<AgentMessage>>,
) {
    channel(
        |event: &AgentEvent| matches!(event, AgentEvent::AgentEnd { .. }),
        |event: &AgentEvent| match event {
            AgentEvent::AgentEnd { messages } => messages.clone(),
            _ => Vec::new(),
        },
    )
}

/// Wrap an [`EventStream`] producer sink into an [`AgentEventSink`].
fn sink_emitter(sink: EventSink<AgentEvent, Vec<AgentMessage>>) -> AgentEventSink {
    let mut sink = sink;
    Box::new(move |event: AgentEvent| {
        sink.push(event);
        Box::pin(async {}) as BoxFuture<'static, ()>
    })
}

/// Emit one event and await its (possibly async) delivery.
async fn emit(sink: &mut AgentEventSink, event: AgentEvent) {
    let fut = sink(event);
    fut.await;
}

// --- run_loop (§2.2, agent-loop.ts:155-275) ---------------------------------

async fn run_loop(
    initial_context: AgentContext,
    new_messages: &mut Vec<AgentMessage>,
    initial_config: AgentLoopConfig,
    token: &CancellationToken,
    sink: &mut AgentEventSink,
    stream_fn: Option<&StreamFn>,
) {
    let mut current_context = initial_context;
    let mut config = initial_config;
    // `config.reasoning` in TS (from SimpleStreamOptions). Not a field on the
    // ported `AgentLoopConfig`, so tracked locally; `prepareNextTurn` remaps it.
    let mut reasoning: Option<ThinkingLevel> = None;
    let mut first_turn = true;
    // Check for steering messages at start (user may have typed while waiting).
    let mut pending: Vec<AgentMessage> = drain(&config.get_steering_messages).await;

    // Outer loop: continues when queued follow-up messages arrive.
    'outer: loop {
        let mut has_more_tool_calls = true;

        // Inner loop: process tool calls and steering messages.
        while has_more_tool_calls || !pending.is_empty() {
            if !first_turn {
                emit(sink, AgentEvent::TurnStart).await;
            } else {
                first_turn = false;
            }

            // Inject pending messages before the next assistant response.
            if !pending.is_empty() {
                for message in pending.drain(..) {
                    emit(
                        sink,
                        AgentEvent::MessageStart {
                            message: message.clone(),
                        },
                    )
                    .await;
                    emit(
                        sink,
                        AgentEvent::MessageEnd {
                            message: message.clone(),
                        },
                    )
                    .await;
                    current_context.messages.push(message.clone());
                    new_messages.push(message);
                }
            }

            // Stream the assistant response.
            let message = stream_assistant_response(
                &mut current_context,
                &config,
                reasoning,
                token,
                sink,
                stream_fn,
            )
            .await;
            new_messages.push(assistant_agent_message(message.clone()));

            if matches!(message.stop_reason, StopReason::Error | StopReason::Aborted) {
                emit(
                    sink,
                    AgentEvent::TurnEnd {
                        message: assistant_agent_message(message.clone()),
                        tool_results: Vec::new(),
                    },
                )
                .await;
                emit(
                    sink,
                    AgentEvent::AgentEnd {
                        messages: new_messages.clone(),
                    },
                )
                .await;
                return;
            }

            // Collect tool-call blocks.
            let tool_calls: Vec<ToolCall> = message
                .content
                .iter()
                .filter_map(|c| match c {
                    AssistantContent::ToolCall(tc) => Some(tc.clone()),
                    _ => None,
                })
                .collect();

            let mut tool_results: Vec<ToolResultMessage> = Vec::new();
            has_more_tool_calls = false;
            if !tool_calls.is_empty() {
                // A `length` stop means the output was cut off, so every tool
                // call may carry truncated arguments — fail them all.
                let batch = if message.stop_reason == StopReason::Length {
                    fail_tool_calls_from_truncated(&tool_calls, sink).await
                } else {
                    execute_tool_calls(
                        &current_context,
                        &message,
                        &tool_calls,
                        &config,
                        token,
                        sink,
                    )
                    .await
                };
                has_more_tool_calls = !batch.terminate;
                for result in &batch.messages {
                    current_context
                        .messages
                        .push(tool_result_agent_message(result.clone()));
                    new_messages.push(tool_result_agent_message(result.clone()));
                    tool_results.push(result.clone());
                }
            }

            emit(
                sink,
                AgentEvent::TurnEnd {
                    message: assistant_agent_message(message.clone()),
                    tool_results: tool_results.clone(),
                },
            )
            .await;

            // prepareNextTurn: may replace context/model and remap reasoning.
            if let Some(prepare) = &config.prepare_next_turn {
                let ctx = PrepareNextTurnContext {
                    message: message.clone(),
                    tool_results: tool_results.clone(),
                    context: current_context.clone(),
                    new_messages: new_messages.clone(),
                };
                if let Some(update) = prepare(ctx).await {
                    if let Some(new_ctx) = update.context {
                        current_context = new_ctx;
                    }
                    if let Some(model) = update.model {
                        config.model = model;
                    }
                    match update.thinking_level {
                        // undefined → keep current reasoning.
                        None => {}
                        // "off" → reasoning undefined.
                        Some(ThinkingLevel::Off) => reasoning = None,
                        Some(level) => reasoning = Some(level),
                    }
                }
            }

            // shouldStopAfterTurn.
            if let Some(should_stop) = &config.should_stop_after_turn {
                let ctx = ShouldStopAfterTurnContext {
                    message: message.clone(),
                    tool_results: tool_results.clone(),
                    context: current_context.clone(),
                    new_messages: new_messages.clone(),
                };
                if should_stop(ctx).await {
                    emit(
                        sink,
                        AgentEvent::AgentEnd {
                            messages: new_messages.clone(),
                        },
                    )
                    .await;
                    return;
                }
            }

            pending = drain(&config.get_steering_messages).await;
        }

        // Agent would stop; check for follow-up messages.
        let follow_up = drain(&config.get_follow_up_messages).await;
        if !follow_up.is_empty() {
            pending = follow_up;
            continue 'outer;
        }
        break;
    }

    emit(
        sink,
        AgentEvent::AgentEnd {
            messages: new_messages.clone(),
        },
    )
    .await;
}

async fn drain(hook: &Option<GetMessagesFn>) -> Vec<AgentMessage> {
    match hook {
        Some(f) => f().await,
        None => Vec::new(),
    }
}

// --- stream_assistant_response (§2.3, agent-loop.ts:281-374) -----------------

async fn stream_assistant_response(
    context: &mut AgentContext,
    config: &AgentLoopConfig,
    reasoning: Option<ThinkingLevel>,
    token: &CancellationToken,
    sink: &mut AgentEventSink,
    stream_fn: Option<&StreamFn>,
) -> AssistantMessage {
    // Optional AgentMessage-level transform.
    let mut messages = context.messages.clone();
    if let Some(transform) = &config.transform_context {
        messages = transform(messages, Some(token.clone())).await;
    }

    // Convert to LLM-compatible messages.
    let llm_messages = (config.convert_to_llm)(messages).await;
    let llm_context = Context {
        system_prompt: Some(context.system_prompt.clone()),
        messages: llm_messages,
        tools: build_llm_tools(&context.tools),
    };

    // Resolve the API key (important for expiring tokens).
    let resolved_key = if let Some(get_key) = &config.get_api_key {
        get_key(config.model.provider.0.clone()).await
    } else {
        None
    };
    let api_key = resolved_key.or_else(|| config.api_key.clone());

    let options = SimpleStreamOptions {
        base: StreamOptions {
            api_key,
            ..Default::default()
        },
        reasoning: reasoning.and_then(to_ai_thinking),
        thinking_budgets: None,
    };

    let mut stream = match stream_fn {
        Some(f) => f(config.model.clone(), llm_context, options, token.clone()),
        None => error_stream(&config.model, "no stream function provided"),
    };

    let mut partial_added = false;

    while let Some(event) = stream.next().await {
        match event {
            AssistantMessageEvent::Start { partial } => {
                context
                    .messages
                    .push(assistant_agent_message(partial.clone()));
                partial_added = true;
                emit(
                    sink,
                    AgentEvent::MessageStart {
                        message: assistant_agent_message(partial),
                    },
                )
                .await;
            }
            AssistantMessageEvent::Done { message, .. } => {
                return finish_stream(context, sink, message, partial_added).await;
            }
            AssistantMessageEvent::Error { error, .. } => {
                return finish_stream(context, sink, error, partial_added).await;
            }
            other => {
                if partial_added {
                    if let Some(partial) = event_partial(&other) {
                        let wrapped = assistant_agent_message(partial.clone());
                        if let Some(last) = context.messages.last_mut() {
                            *last = wrapped.clone();
                        }
                        emit(
                            sink,
                            AgentEvent::MessageUpdate {
                                message: wrapped,
                                assistant_message_event: other,
                            },
                        )
                        .await;
                    }
                }
            }
        }
    }

    // Stream ended without a terminal event: fall back to the resolved result.
    let final_message = stream.result().await;
    finish_stream(context, sink, final_message, partial_added).await
}

async fn finish_stream(
    context: &mut AgentContext,
    sink: &mut AgentEventSink,
    final_message: AssistantMessage,
    partial_added: bool,
) -> AssistantMessage {
    let wrapped = assistant_agent_message(final_message.clone());
    if partial_added {
        if let Some(last) = context.messages.last_mut() {
            *last = wrapped.clone();
        }
    } else {
        context.messages.push(wrapped.clone());
        emit(
            sink,
            AgentEvent::MessageStart {
                message: wrapped.clone(),
            },
        )
        .await;
    }
    emit(sink, AgentEvent::MessageEnd { message: wrapped }).await;
    final_message
}

/// Extract the `partial` snapshot from a streaming delta-family event.
fn event_partial(event: &AssistantMessageEvent) -> Option<&AssistantMessage> {
    match event {
        AssistantMessageEvent::TextStart { partial, .. }
        | AssistantMessageEvent::TextDelta { partial, .. }
        | AssistantMessageEvent::TextEnd { partial, .. }
        | AssistantMessageEvent::ThinkingStart { partial, .. }
        | AssistantMessageEvent::ThinkingDelta { partial, .. }
        | AssistantMessageEvent::ThinkingEnd { partial, .. }
        | AssistantMessageEvent::ToolcallStart { partial, .. }
        | AssistantMessageEvent::ToolcallDelta { partial, .. }
        | AssistantMessageEvent::ToolcallEnd { partial, .. } => Some(partial),
        AssistantMessageEvent::Start { .. }
        | AssistantMessageEvent::Done { .. }
        | AssistantMessageEvent::Error { .. } => None,
    }
}

// --- tool pipeline (§2.4, agent-loop.ts:413-792) ----------------------------

/// The outcome of executing a batch of tool calls.
struct ExecutedToolCallBatch {
    messages: Vec<ToolResultMessage>,
    terminate: bool,
}

/// A finalized tool-call outcome (TS `FinalizedToolCallOutcome`).
struct Finalized {
    tool_call: AgentToolCall,
    result: AgentToolResult,
    is_error: bool,
}

/// A prepared tool call ready to execute (TS `PreparedToolCall`).
struct PreparedToolCall {
    tool_call: AgentToolCall,
    tool: Arc<dyn AgentTool>,
    args: Value,
}

/// The result of the prepare stage (TS `PreparedToolCall | ImmediateToolCallOutcome`).
enum Preparation {
    Prepared(PreparedToolCall),
    Immediate {
        result: AgentToolResult,
        is_error: bool,
    },
}

/// An executed (pre-finalize) outcome (TS `ExecutedToolCallOutcome`).
struct ExecutedOutcome {
    result: AgentToolResult,
    is_error: bool,
}

/// Choose sequential vs parallel execution and dispatch (TS `executeToolCalls`,
/// agent-loop.ts:413-428).
async fn execute_tool_calls(
    context: &AgentContext,
    assistant: &AssistantMessage,
    tool_calls: &[AgentToolCall],
    config: &AgentLoopConfig,
    token: &CancellationToken,
    sink: &mut AgentEventSink,
) -> ExecutedToolCallBatch {
    let has_sequential = tool_calls.iter().any(|tc| {
        find_tool(context, &tc.name)
            .map(|t| t.execution_mode() == Some(ToolExecutionMode::Sequential))
            .unwrap_or(false)
    });
    if config.tool_execution == Some(ToolExecutionMode::Sequential) || has_sequential {
        execute_tool_calls_sequential(context, assistant, tool_calls, config, token, sink).await
    } else {
        execute_tool_calls_parallel(context, assistant, tool_calls, config, token, sink).await
    }
}

async fn execute_tool_calls_sequential(
    context: &AgentContext,
    assistant: &AssistantMessage,
    tool_calls: &[AgentToolCall],
    config: &AgentLoopConfig,
    token: &CancellationToken,
    sink: &mut AgentEventSink,
) -> ExecutedToolCallBatch {
    let mut finalized_calls: Vec<Finalized> = Vec::new();
    let mut messages: Vec<ToolResultMessage> = Vec::new();

    for tool_call in tool_calls {
        emit_tool_execution_start(tool_call, sink).await;

        let preparation = prepare_tool_call(context, assistant, tool_call, config, token).await;
        let finalized = match preparation {
            Preparation::Immediate { result, is_error } => Finalized {
                tool_call: tool_call.clone(),
                result,
                is_error,
            },
            Preparation::Prepared(prepared) => {
                let (executed, updates) = execute_prepared_tool_call(&prepared, token).await;
                emit_tool_execution_updates(&prepared, &updates, sink).await;
                finalize_executed_tool_call(context, assistant, &prepared, executed, config, token)
                    .await
            }
        };

        emit_tool_execution_end(&finalized, sink).await;
        let message = create_tool_result_message(&finalized);
        emit_tool_result_message(&message, sink).await;
        finalized_calls.push(finalized);
        messages.push(message);

        if token.is_cancelled() {
            break;
        }
    }

    ExecutedToolCallBatch {
        terminate: should_terminate_tool_batch(&finalized_calls),
        messages,
    }
}

async fn execute_tool_calls_parallel(
    context: &AgentContext,
    assistant: &AssistantMessage,
    tool_calls: &[AgentToolCall],
    config: &AgentLoopConfig,
    token: &CancellationToken,
    sink: &mut AgentEventSink,
) -> ExecutedToolCallBatch {
    // Prepare stage: emit `tool_execution_start` in source order. Immediate
    // outcomes settle inline (end emitted now); prepared calls are deferred.
    let mut immediates: Vec<(usize, Finalized)> = Vec::new();
    let mut deferred: Vec<(usize, PreparedToolCall)> = Vec::new();

    for (index, tool_call) in tool_calls.iter().enumerate() {
        emit_tool_execution_start(tool_call, sink).await;

        match prepare_tool_call(context, assistant, tool_call, config, token).await {
            Preparation::Immediate { result, is_error } => {
                let finalized = Finalized {
                    tool_call: tool_call.clone(),
                    result,
                    is_error,
                };
                emit_tool_execution_end(&finalized, sink).await;
                immediates.push((index, finalized));
                if token.is_cancelled() {
                    break;
                }
            }
            Preparation::Prepared(prepared) => {
                deferred.push((index, prepared));
                if token.is_cancelled() {
                    break;
                }
            }
        }
    }

    // Execute deferred calls concurrently; `tool_execution_end` fires in
    // completion order (TS thunks resolved via Promise.all fire `end` inside).
    let mut futures = FuturesUnordered::new();
    for (index, prepared) in deferred {
        futures.push(async move {
            let (executed, updates) = execute_prepared_tool_call(&prepared, token).await;
            let finalized =
                finalize_executed_tool_call(context, assistant, &prepared, executed, config, token)
                    .await;
            (index, prepared, finalized, updates)
        });
    }

    let mut completed: Vec<(usize, Finalized)> = Vec::new();
    while let Some((index, prepared, finalized, updates)) = futures.next().await {
        emit_tool_execution_updates(&prepared, &updates, sink).await;
        emit_tool_execution_end(&finalized, sink).await;
        completed.push((index, finalized));
    }

    // Result MESSAGES are emitted in assistant source order (TS join_all order).
    let mut ordered: Vec<(usize, Finalized)> = immediates;
    ordered.extend(completed);
    ordered.sort_by_key(|(index, _)| *index);
    let ordered: Vec<Finalized> = ordered
        .into_iter()
        .map(|(_, finalized)| finalized)
        .collect();

    let mut messages: Vec<ToolResultMessage> = Vec::new();
    for finalized in &ordered {
        let message = create_tool_result_message(finalized);
        emit_tool_result_message(&message, sink).await;
        messages.push(message);
    }

    ExecutedToolCallBatch {
        terminate: should_terminate_tool_batch(&ordered),
        messages,
    }
}

/// `shouldTerminateToolBatch` (agent-loop.ts:584-586): non-empty AND every result
/// has `terminate === true`.
fn should_terminate_tool_batch(finalized_calls: &[Finalized]) -> bool {
    !finalized_calls.is_empty()
        && finalized_calls
            .iter()
            .all(|f| f.result.terminate == Some(true))
}

/// Prepare stage (TS `prepareToolCall`, agent-loop.ts:602-666).
async fn prepare_tool_call(
    context: &AgentContext,
    assistant: &AssistantMessage,
    tool_call: &AgentToolCall,
    config: &AgentLoopConfig,
    token: &CancellationToken,
) -> Preparation {
    let tool = match find_tool(context, &tool_call.name) {
        Some(tool) => tool.clone(),
        None => {
            return Preparation::Immediate {
                result: create_error_tool_result(&format!("Tool {} not found", tool_call.name)),
                is_error: true,
            };
        }
    };

    // `prepareArguments` shim: identity by default (agent-loop.ts:588-600).
    let prepared_args = tool.prepare_arguments(Value::Object(tool_call.arguments.clone()));

    // Validate against the JSON schema (≈ pi-ai `validateToolArguments`).
    if let Err(message) = validate_tool_arguments(tool.as_ref(), &prepared_args) {
        return Preparation::Immediate {
            result: create_error_tool_result(&message),
            is_error: true,
        };
    }

    if let Some(before) = &config.before_tool_call {
        let ctx = BeforeToolCallContext {
            assistant_message: assistant.clone(),
            tool_call: tool_call.clone(),
            args: prepared_args.clone(),
            context: context.clone(),
        };
        let before_result = before(ctx, Some(token.clone())).await;
        if token.is_cancelled() {
            return Preparation::Immediate {
                result: create_error_tool_result("Operation aborted"),
                is_error: true,
            };
        }
        if let Some(result) = before_result {
            if result.block == Some(true) {
                let reason = result
                    .reason
                    .filter(|r| !r.is_empty())
                    .unwrap_or_else(|| "Tool execution was blocked".to_string());
                return Preparation::Immediate {
                    result: create_error_tool_result(&reason),
                    is_error: true,
                };
            }
        }
    }

    if token.is_cancelled() {
        return Preparation::Immediate {
            result: create_error_tool_result("Operation aborted"),
            is_error: true,
        };
    }

    Preparation::Prepared(PreparedToolCall {
        tool_call: tool_call.clone(),
        tool,
        args: prepared_args,
    })
}

/// Execute stage (TS `executePreparedToolCall`, agent-loop.ts:668-709). Tool
/// `onUpdate` calls are buffered while `acceptingUpdates` is set; the buffer is
/// returned for the caller to emit (late updates after settlement are dropped).
async fn execute_prepared_tool_call(
    prepared: &PreparedToolCall,
    token: &CancellationToken,
) -> (ExecutedOutcome, Vec<AgentToolResult>) {
    let buffer: Arc<Mutex<Vec<AgentToolResult>>> = Arc::new(Mutex::new(Vec::new()));
    let accepting = Arc::new(AtomicBool::new(true));

    let on_update: AgentToolUpdateCallback = {
        let buffer = Arc::clone(&buffer);
        let accepting = Arc::clone(&accepting);
        Arc::new(move |partial: AgentToolResult| {
            if accepting.load(Ordering::SeqCst) {
                if let Ok(mut guard) = buffer.lock() {
                    guard.push(partial);
                }
            }
        })
    };

    let result = prepared
        .tool
        .execute(
            &prepared.tool_call.id,
            prepared.args.clone(),
            token.clone(),
            on_update,
        )
        .await;

    accepting.store(false, Ordering::SeqCst);
    let updates = std::mem::take(&mut *buffer.lock().expect("buffer mutex poisoned"));

    match result {
        Ok(result) => (
            ExecutedOutcome {
                result,
                is_error: false,
            },
            updates,
        ),
        Err(error) => (
            ExecutedOutcome {
                result: create_error_tool_result(&error.to_string()),
                is_error: true,
            },
            updates,
        ),
    }
}

/// Finalize stage (TS `finalizeExecutedToolCall`, agent-loop.ts:711-755). Runs the
/// `afterToolCall` hook and merges `content`/`details`/`terminate`/`isError`
/// field-by-field (no deep merge).
async fn finalize_executed_tool_call(
    context: &AgentContext,
    assistant: &AssistantMessage,
    prepared: &PreparedToolCall,
    executed: ExecutedOutcome,
    config: &AgentLoopConfig,
    token: &CancellationToken,
) -> Finalized {
    let mut result = executed.result;
    let mut is_error = executed.is_error;

    if let Some(after) = &config.after_tool_call {
        let ctx = AfterToolCallContext {
            assistant_message: assistant.clone(),
            tool_call: prepared.tool_call.clone(),
            args: prepared.args.clone(),
            result: result.clone(),
            is_error,
            context: context.clone(),
        };
        if let Some(after_result) = after(ctx, Some(token.clone())).await {
            result = AgentToolResult {
                content: after_result.content.unwrap_or(result.content),
                details: after_result.details.unwrap_or(result.details),
                terminate: after_result.terminate.or(result.terminate),
                added_tool_names: result.added_tool_names,
            };
            if let Some(flag) = after_result.is_error {
                is_error = flag;
            }
        }
    }

    Finalized {
        tool_call: prepared.tool_call.clone(),
        result,
        is_error,
    }
}

/// Fail every tool call from a length-truncated assistant message (TS
/// `failToolCallsFromTruncatedMessage`, agent-loop.ts:383-408). `terminate:false`.
async fn fail_tool_calls_from_truncated(
    tool_calls: &[AgentToolCall],
    sink: &mut AgentEventSink,
) -> ExecutedToolCallBatch {
    let mut messages: Vec<ToolResultMessage> = Vec::new();
    for tool_call in tool_calls {
        emit_tool_execution_start(tool_call, sink).await;
        let text = format!(
            "Tool call \"{}\" was not executed: the response hit the output token limit, so its arguments may be truncated. Re-issue the tool call with complete arguments.",
            tool_call.name
        );
        let finalized = Finalized {
            tool_call: tool_call.clone(),
            result: create_error_tool_result(&text),
            is_error: true,
        };
        emit_tool_execution_end(&finalized, sink).await;
        let message = create_tool_result_message(&finalized);
        emit_tool_result_message(&message, sink).await;
        messages.push(message);
    }
    ExecutedToolCallBatch {
        messages,
        terminate: false,
    }
}

// --- small helpers -----------------------------------------------------------

fn find_tool<'a>(context: &'a AgentContext, name: &str) -> Option<&'a Arc<dyn AgentTool>> {
    context
        .tools
        .as_ref()
        .and_then(|tools| tools.iter().find(|t| t.name() == name))
}

/// `createErrorToolResult` (agent-loop.ts:757-762).
fn create_error_tool_result(message: &str) -> AgentToolResult {
    AgentToolResult {
        content: vec![UserContent::Text(TextContent::new(message))],
        details: Value::Object(Map::new()),
        added_tool_names: None,
        terminate: None,
    }
}

/// `createToolResultMessage` (agent-loop.ts:774-787). `content ?? []` (always a
/// vec here); `addedToolNames` only when non-empty.
fn create_tool_result_message(finalized: &Finalized) -> ToolResultMessage {
    ToolResultMessage {
        role: ToolResultRole::ToolResult,
        tool_call_id: finalized.tool_call.id.clone(),
        tool_name: finalized.tool_call.name.clone(),
        content: finalized.result.content.clone(),
        details: Some(finalized.result.details.clone()),
        added_tool_names: finalized
            .result
            .added_tool_names
            .clone()
            .filter(|names| !names.is_empty()),
        is_error: finalized.is_error,
        timestamp: now_millis(),
    }
}

async fn emit_tool_execution_start(tool_call: &AgentToolCall, sink: &mut AgentEventSink) {
    emit(
        sink,
        AgentEvent::ToolExecutionStart {
            tool_call_id: tool_call.id.clone(),
            tool_name: tool_call.name.clone(),
            args: Value::Object(tool_call.arguments.clone()),
        },
    )
    .await;
}

async fn emit_tool_execution_updates(
    prepared: &PreparedToolCall,
    updates: &[AgentToolResult],
    sink: &mut AgentEventSink,
) {
    for partial in updates {
        emit(
            sink,
            AgentEvent::ToolExecutionUpdate {
                tool_call_id: prepared.tool_call.id.clone(),
                tool_name: prepared.tool_call.name.clone(),
                args: Value::Object(prepared.tool_call.arguments.clone()),
                partial_result: agent_tool_result_to_value(partial),
            },
        )
        .await;
    }
}

async fn emit_tool_execution_end(finalized: &Finalized, sink: &mut AgentEventSink) {
    emit(
        sink,
        AgentEvent::ToolExecutionEnd {
            tool_call_id: finalized.tool_call.id.clone(),
            tool_name: finalized.tool_call.name.clone(),
            result: agent_tool_result_to_value(&finalized.result),
            is_error: finalized.is_error,
        },
    )
    .await;
}

async fn emit_tool_result_message(message: &ToolResultMessage, sink: &mut AgentEventSink) {
    let wrapped = tool_result_agent_message(message.clone());
    emit(
        sink,
        AgentEvent::MessageStart {
            message: wrapped.clone(),
        },
    )
    .await;
    emit(sink, AgentEvent::MessageEnd { message: wrapped }).await;
}

/// Serialize an [`AgentToolResult`] to the JSON shape the loop's events carry
/// (`{content, details, addedToolNames?, terminate?}`). `AgentToolResult` is not
/// `Serialize`, so build the object explicitly.
fn agent_tool_result_to_value(result: &AgentToolResult) -> Value {
    let mut obj = Map::new();
    obj.insert(
        "content".to_string(),
        serde_json::to_value(&result.content).unwrap_or(Value::Null),
    );
    obj.insert("details".to_string(), result.details.clone());
    if let Some(names) = &result.added_tool_names {
        obj.insert(
            "addedToolNames".to_string(),
            serde_json::to_value(names).unwrap_or(Value::Null),
        );
    }
    if let Some(terminate) = result.terminate {
        obj.insert("terminate".to_string(), Value::Bool(terminate));
    }
    Value::Object(obj)
}

/// Validate arguments against the tool's JSON schema (≈ pi-ai
/// `validateToolArguments`); return the first error message on failure.
fn validate_tool_arguments(tool: &dyn AgentTool, args: &Value) -> Result<(), String> {
    let schema = tool.parameters();
    let validator = jsonschema::validator_for(&schema).map_err(|e| e.to_string())?;
    validator.validate(args).map_err(|e| e.to_string())
}

fn build_llm_tools(tools: &Option<Vec<Arc<dyn AgentTool>>>) -> Option<Vec<Tool>> {
    tools.as_ref().map(|tools| {
        tools
            .iter()
            .map(|t| Tool {
                name: t.name().to_string(),
                description: t.description().to_string(),
                parameters: t.parameters(),
            })
            .collect()
    })
}

fn assistant_agent_message(message: AssistantMessage) -> AgentMessage {
    AgentMessage::Llm(Message::Assistant(message))
}

fn tool_result_agent_message(message: ToolResultMessage) -> AgentMessage {
    AgentMessage::Llm(Message::ToolResult(message))
}

fn is_assistant_message(message: &AgentMessage) -> bool {
    matches!(message, AgentMessage::Llm(Message::Assistant(_)))
}

/// Map the agent-core [`ThinkingLevel`] onto pi-ai's (no `off`; `off` → `None`),
/// matching TS's `"off" → undefined` mapping.
fn to_ai_thinking(level: ThinkingLevel) -> Option<AiThinkingLevel> {
    match level {
        ThinkingLevel::Off => None,
        ThinkingLevel::Minimal => Some(AiThinkingLevel::Minimal),
        ThinkingLevel::Low => Some(AiThinkingLevel::Low),
        ThinkingLevel::Medium => Some(AiThinkingLevel::Medium),
        ThinkingLevel::High => Some(AiThinkingLevel::High),
        ThinkingLevel::Xhigh => Some(AiThinkingLevel::Xhigh),
        ThinkingLevel::Max => Some(AiThinkingLevel::Max),
    }
}

fn now_millis() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// A single-event error stream used when no `streamFn` is supplied (there is no
/// global `streamSimple` provider registry in this crate).
fn error_stream(model: &Model, message: &str) -> AssistantMessageEventStream {
    let (mut sink, stream) = assistant_message_stream();
    let error = AssistantMessage {
        role: AssistantRole::Assistant,
        content: Vec::new(),
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: Some(model.id.clone()),
        response_model: None,
        diagnostics: None,
        usage: zero_usage(),
        stop_reason: StopReason::Error,
        timestamp: now_millis(),
        response_id: None,
        raw_stop_reason: None,
        error_message: Some(message.to_string()),
        end_turn: None,
    };
    sink.push(AssistantMessageEvent::Error {
        reason: StopReason::Error,
        error: error.clone(),
    });
    sink.end(Some(error));
    stream
}

fn zero_usage() -> Usage {
    Usage {
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use pirust_ai::providers::faux::{faux_assistant_message, faux_tool_call, FauxMessageOptions};
    use pirust_ai::types::UserMessageContent;
    use serde_json::json;

    // --- test fixtures -------------------------------------------------------

    fn tape_sink(tape: Arc<Mutex<Vec<AgentEvent>>>) -> AgentEventSink {
        Box::new(move |event: AgentEvent| {
            tape.lock().unwrap().push(event);
            Box::pin(async {}) as BoxFuture<'static, ()>
        })
    }

    fn object_schema() -> Value {
        json!({ "type": "object", "additionalProperties": true })
    }

    fn faux_model() -> Model {
        pirust_ai::providers::faux::Faux::new().get_model().clone()
    }

    /// A tool that returns a fixed text result.
    struct FixedTool {
        name: &'static str,
        text: &'static str,
        mode: Option<ToolExecutionMode>,
        terminate: Option<bool>,
    }

    #[async_trait]
    impl AgentTool for FixedTool {
        fn name(&self) -> &str {
            self.name
        }
        fn label(&self) -> &str {
            self.name
        }
        fn description(&self) -> &str {
            self.name
        }
        fn parameters(&self) -> Value {
            object_schema()
        }
        async fn execute(
            &self,
            _id: &str,
            _args: Value,
            _token: CancellationToken,
            _on_update: AgentToolUpdateCallback,
        ) -> Result<AgentToolResult, crate::types::ToolError> {
            Ok(AgentToolResult {
                content: vec![UserContent::Text(TextContent::new(self.text))],
                details: Value::Object(Map::new()),
                added_tool_names: None,
                terminate: self.terminate,
            })
        }
        fn execution_mode(&self) -> Option<ToolExecutionMode> {
            self.mode
        }
    }

    /// A tool whose execute always fails.
    struct ThrowingTool;

    #[async_trait]
    impl AgentTool for ThrowingTool {
        fn name(&self) -> &str {
            "boom"
        }
        fn label(&self) -> &str {
            "boom"
        }
        fn description(&self) -> &str {
            "boom"
        }
        fn parameters(&self) -> Value {
            object_schema()
        }
        async fn execute(
            &self,
            _id: &str,
            _args: Value,
            _token: CancellationToken,
            _on_update: AgentToolUpdateCallback,
        ) -> Result<AgentToolResult, crate::types::ToolError> {
            Err("kaboom".into())
        }
    }

    fn tool_call(id: &str, name: &str) -> AgentToolCall {
        faux_tool_call(id, name, Map::new())
    }

    fn assistant_with_calls(calls: Vec<AgentToolCall>) -> AssistantMessage {
        let content = calls.into_iter().map(AssistantContent::ToolCall).collect();
        faux_assistant_message(content, FauxMessageOptions::default())
    }

    fn base_config() -> AgentLoopConfig {
        AgentLoopConfig {
            model: faux_model(),
            api_key: None,
            tool_execution: None,
            convert_to_llm: Box::new(|msgs| {
                Box::pin(async move { crate::harness::messages::convert_to_llm(&msgs) })
            }),
            transform_context: None,
            get_api_key: None,
            should_stop_after_turn: None,
            prepare_next_turn: None,
            get_steering_messages: None,
            get_follow_up_messages: None,
            before_tool_call: None,
            after_tool_call: None,
        }
    }

    fn context_with(tools: Vec<Arc<dyn AgentTool>>) -> AgentContext {
        AgentContext {
            system_prompt: String::new(),
            messages: Vec::new(),
            tools: Some(tools),
        }
    }

    fn event_kind(event: &AgentEvent) -> &'static str {
        match event {
            AgentEvent::ToolExecutionStart { .. } => "tool_execution_start",
            AgentEvent::ToolExecutionEnd { .. } => "tool_execution_end",
            AgentEvent::ToolExecutionUpdate { .. } => "tool_execution_update",
            AgentEvent::MessageStart { .. } => "message_start",
            AgentEvent::MessageEnd { .. } => "message_end",
            _ => "other",
        }
    }

    fn result_text(message: &ToolResultMessage) -> String {
        message
            .content
            .iter()
            .filter_map(|c| match c {
                UserContent::Text(t) => Some(t.text.clone()),
                _ => None,
            })
            .collect()
    }

    // --- (a) sequential vs parallel selection + emit ordering ----------------

    #[tokio::test]
    async fn parallel_starts_and_messages_in_source_order() {
        let ctx = context_with(vec![
            Arc::new(FixedTool {
                name: "a",
                text: "ra",
                mode: None,
                terminate: None,
            }),
            Arc::new(FixedTool {
                name: "b",
                text: "rb",
                mode: None,
                terminate: None,
            }),
        ]);
        let calls = vec![tool_call("id-a", "a"), tool_call("id-b", "b")];
        let assistant = assistant_with_calls(calls.clone());
        let config = base_config();
        let tape = Arc::new(Mutex::new(Vec::new()));
        let mut sink = tape_sink(tape.clone());
        let token = CancellationToken::new();

        let batch = execute_tool_calls(&ctx, &assistant, &calls, &config, &token, &mut sink).await;

        // Result messages in assistant source order.
        assert_eq!(batch.messages[0].tool_name, "a");
        assert_eq!(batch.messages[1].tool_name, "b");

        let events = tape.lock().unwrap();
        let starts: Vec<String> = events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::ToolExecutionStart { tool_name, .. } => Some(tool_name.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(starts, vec!["a", "b"], "starts must be in source order");

        let msg_names: Vec<String> = events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::MessageStart {
                    message: AgentMessage::Llm(Message::ToolResult(m)),
                } => Some(m.tool_name.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(msg_names, vec!["a", "b"]);
    }

    #[tokio::test]
    async fn sequential_selected_by_tool_mode() {
        // Tool `a` requests sequential mode; the whole batch goes sequential.
        let ctx = context_with(vec![
            Arc::new(FixedTool {
                name: "a",
                text: "ra",
                mode: Some(ToolExecutionMode::Sequential),
                terminate: None,
            }),
            Arc::new(FixedTool {
                name: "b",
                text: "rb",
                mode: None,
                terminate: None,
            }),
        ]);
        let calls = vec![tool_call("id-a", "a"), tool_call("id-b", "b")];
        let assistant = assistant_with_calls(calls.clone());
        let config = base_config();
        let tape = Arc::new(Mutex::new(Vec::new()));
        let mut sink = tape_sink(tape.clone());
        let token = CancellationToken::new();

        let batch = execute_tool_calls(&ctx, &assistant, &calls, &config, &token, &mut sink).await;
        assert_eq!(batch.messages.len(), 2);

        // Sequential: each call's full [start, end, message_start, message_end]
        // completes before the next call starts.
        let events = tape.lock().unwrap();
        let kinds: Vec<&'static str> = events.iter().map(event_kind).collect();
        assert_eq!(
            kinds,
            vec![
                "tool_execution_start",
                "tool_execution_end",
                "message_start",
                "message_end",
                "tool_execution_start",
                "tool_execution_end",
                "message_start",
                "message_end",
            ]
        );
    }

    // --- (b) length stop fails ALL with terminate:false ----------------------

    #[tokio::test]
    async fn length_truncation_fails_all_calls() {
        let calls = vec![tool_call("id-a", "a"), tool_call("id-b", "b")];
        let tape = Arc::new(Mutex::new(Vec::new()));
        let mut sink = tape_sink(tape.clone());

        let batch = fail_tool_calls_from_truncated(&calls, &mut sink).await;

        assert!(!batch.terminate, "truncated batch never terminates");
        assert_eq!(batch.messages.len(), 2);
        assert!(
            batch.messages.iter().all(|m| m.is_error),
            "all fail as errors"
        );
        assert!(batch
            .messages
            .iter()
            .all(|m| result_text(m).contains("output token limit")));
    }

    // --- (c) terminate only when non-empty AND every result terminate==true --

    #[test]
    fn terminate_batch_rules() {
        let mk = |terminate: Option<bool>| Finalized {
            tool_call: tool_call("x", "x"),
            result: AgentToolResult {
                content: Vec::new(),
                details: Value::Null,
                added_tool_names: None,
                terminate,
            },
            is_error: false,
        };
        assert!(!should_terminate_tool_batch(&[]), "empty never terminates");
        assert!(should_terminate_tool_batch(&[mk(Some(true))]));
        assert!(!should_terminate_tool_batch(&[
            mk(Some(true)),
            mk(Some(false))
        ]));
        assert!(!should_terminate_tool_batch(&[mk(Some(true)), mk(None)]));
        assert!(should_terminate_tool_batch(&[
            mk(Some(true)),
            mk(Some(true))
        ]));
    }

    #[tokio::test]
    async fn terminate_propagates_from_tool() {
        let ctx = context_with(vec![Arc::new(FixedTool {
            name: "a",
            text: "ra",
            mode: None,
            terminate: Some(true),
        })]);
        let calls = vec![tool_call("id-a", "a")];
        let assistant = assistant_with_calls(calls.clone());
        let config = base_config();
        let tape = Arc::new(Mutex::new(Vec::new()));
        let mut sink = tape_sink(tape.clone());
        let token = CancellationToken::new();
        let batch = execute_tool_calls(&ctx, &assistant, &calls, &config, &token, &mut sink).await;
        assert!(batch.terminate);
    }

    // --- (d) tool that throws → error result (no panic) ----------------------

    #[tokio::test]
    async fn throwing_tool_becomes_error_result() {
        let ctx = context_with(vec![Arc::new(ThrowingTool)]);
        let calls = vec![tool_call("id-boom", "boom")];
        let assistant = assistant_with_calls(calls.clone());
        let config = base_config();
        let tape = Arc::new(Mutex::new(Vec::new()));
        let mut sink = tape_sink(tape.clone());
        let token = CancellationToken::new();

        let batch = execute_tool_calls(&ctx, &assistant, &calls, &config, &token, &mut sink).await;
        assert_eq!(batch.messages.len(), 1);
        assert!(batch.messages[0].is_error);
        assert_eq!(result_text(&batch.messages[0]), "kaboom");
    }

    #[tokio::test]
    async fn missing_tool_becomes_error_result() {
        let ctx = context_with(vec![]);
        let calls = vec![tool_call("id-x", "nope")];
        let assistant = assistant_with_calls(calls.clone());
        let config = base_config();
        let tape = Arc::new(Mutex::new(Vec::new()));
        let mut sink = tape_sink(tape.clone());
        let token = CancellationToken::new();

        let batch = execute_tool_calls(&ctx, &assistant, &calls, &config, &token, &mut sink).await;
        assert!(batch.messages[0].is_error);
        assert_eq!(result_text(&batch.messages[0]), "Tool nope not found");
    }

    // --- (e) beforeToolCall block → error result with reason -----------------

    #[tokio::test]
    async fn before_tool_call_block_produces_error_with_reason() {
        let ctx = context_with(vec![Arc::new(FixedTool {
            name: "a",
            text: "ra",
            mode: None,
            terminate: None,
        })]);
        let calls = vec![tool_call("id-a", "a")];
        let assistant = assistant_with_calls(calls.clone());
        let mut config = base_config();
        config.before_tool_call = Some(Box::new(|_ctx, _token| {
            Box::pin(async move {
                Some(crate::types::BeforeToolCallResult {
                    block: Some(true),
                    reason: Some("nope".to_string()),
                })
            })
        }));
        let tape = Arc::new(Mutex::new(Vec::new()));
        let mut sink = tape_sink(tape.clone());
        let token = CancellationToken::new();

        let batch = execute_tool_calls(&ctx, &assistant, &calls, &config, &token, &mut sink).await;
        assert!(batch.messages[0].is_error);
        assert_eq!(result_text(&batch.messages[0]), "nope");
    }

    #[test]
    fn continue_guards_reject_empty_and_assistant_tail() {
        let empty = AgentContext {
            system_prompt: String::new(),
            messages: Vec::new(),
            tools: None,
        };
        assert_eq!(
            check_continue_guards(&empty),
            Err(ContinueError::NoMessages)
        );

        let assistant_tail = AgentContext {
            system_prompt: String::new(),
            messages: vec![assistant_agent_message(faux_assistant_message(
                vec![AssistantContent::Text(TextContent::new("hi"))],
                FauxMessageOptions::default(),
            ))],
            tools: None,
        };
        assert_eq!(
            check_continue_guards(&assistant_tail),
            Err(ContinueError::LastIsAssistant)
        );

        // A `custom` (non-assistant) tail is allowed.
        let user_tail = AgentContext {
            system_prompt: String::new(),
            messages: vec![AgentMessage::Custom(
                crate::harness::messages::CustomMessage {
                    role: Default::default(),
                    custom_type: "note".to_string(),
                    content: UserMessageContent::Text("hey".to_string()),
                    display: true,
                    details: None,
                    timestamp: 0,
                },
            )],
            tools: None,
        };
        assert!(check_continue_guards(&user_tail).is_ok());
    }
}
