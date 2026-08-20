//! Stateful `Agent` wrapper — port of `packages/agent/src/agent.ts`.
//!
//! Spec: `docs/analysis/07-agent-core-spec.md` §3. Depends on `types` + the
//! `agent_loop` engine (§13, wave 4).
//!
//! Public surface (§3): [`Agent`], [`AgentOptions`], the copy-on-assign
//! `MutableAgentState`, the [`PendingMessageQueue`], the single-active-run guard
//! (`prompt`/`continue` err if a run is active), `run_with_lifecycle`
//! (AbortController → [`CancellationToken`], `is_streaming`, executor,
//! [`AgentInner::handle_run_failure`] synthesizing an aborted/error assistant
//! message driven `message_start → message_end → turn_end → agent_end`,
//! `finish_run`), `wait_for_idle`, `abort`, `create_loop_config`
//! (`skipInitialSteeringPoll`), and `process_events` (reduce state then await
//! listeners in subscription order with the run signal; err outside a run).
//!
//! # Rust adaptation notes
//!
//! Pi's `Agent` is a single mutable object driven on Node's single-threaded event
//! loop. This port keeps the object's shared state behind an [`Arc`]`<`[`AgentInner`]`>`
//! so the loop's `'static` event sink and the listener futures can capture it. The
//! low-level loop sink ([`crate::agent_loop::AgentEventSink`]) is infallible, so —
//! unlike Pi — a throwing listener cannot itself unwind the run; `handle_run_failure`
//! fires when the *executor* future fails (e.g. a fatal error before/around the
//! loop), preserving the failure-synthesis tail exactly.

use std::collections::HashSet;
use std::future::Future;
use std::sync::{Arc, Mutex};

use futures::future::BoxFuture;
use pirust_ai::types::{
    AssistantContent, AssistantMessage, AssistantRole, Cost, Message, Model, StopReason,
    TextContent, Usage, UserContent, UserMessage, UserMessageContent, UserRole,
};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::agent_loop::{run_agent_loop, run_agent_loop_continue, AgentEventSink, StreamFn};
use crate::harness::messages::AgentMessage;
use crate::types::{
    AfterToolCallContext, AfterToolCallResult, AgentContext, AgentEvent, AgentLoopConfig,
    AgentTool, BeforeToolCallContext, BeforeToolCallResult, QueueMode, ThinkingLevel,
    ToolExecutionMode,
};

pub use crate::types::QueueMode as AgentQueueMode;

// --- shareable hook aliases --------------------------------------------------
//
// `crate::types` declares the loop hooks as `Box<dyn Fn…>` (single-owner, consumed
// by one `AgentLoopConfig`). The `Agent` must build a *fresh* config per run, so it
// stores the user's hooks as `Arc<dyn Fn…>` and re-wraps them in a `Box` each time.

/// `convertToLlm` (agent.ts:99). Async so it can await user conversion.
pub type ConvertToLlmArc =
    Arc<dyn Fn(Vec<AgentMessage>) -> BoxFuture<'static, Vec<Message>> + Send + Sync>;
/// `transformContext` (agent.ts:100).
pub type TransformContextArc = Arc<
    dyn Fn(Vec<AgentMessage>, Option<CancellationToken>) -> BoxFuture<'static, Vec<AgentMessage>>
        + Send
        + Sync,
>;
/// `getApiKey` (agent.ts:102).
pub type GetApiKeyArc = Arc<dyn Fn(String) -> BoxFuture<'static, Option<String>> + Send + Sync>;
/// `beforeToolCall` (agent.ts:105).
pub type BeforeToolCallArc = Arc<
    dyn Fn(
            BeforeToolCallContext,
            Option<CancellationToken>,
        ) -> BoxFuture<'static, Option<BeforeToolCallResult>>
        + Send
        + Sync,
>;
/// `afterToolCall` (agent.ts:106).
pub type AfterToolCallArc = Arc<
    dyn Fn(
            AfterToolCallContext,
            Option<CancellationToken>,
        ) -> BoxFuture<'static, Option<AfterToolCallResult>>
        + Send
        + Sync,
>;

/// A lifecycle listener (agent.ts:173). Awaited in subscription order with the
/// active run's cancellation token. `'static` + `Send`/`Sync` so it can be driven
/// from the loop's detached-capable event sink; event + token are passed by clone.
pub type AgentListener =
    Arc<dyn Fn(AgentEvent, CancellationToken) -> BoxFuture<'static, ()> + Send + Sync>;

// --- errors ------------------------------------------------------------------

/// Errors surfaced by [`Agent`] control methods (agent.ts throws at 338-342,
/// 349-351, 355, 371, 471, 569).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AgentError {
    /// `prompt()` while a run is active (agent.ts:338-342).
    #[error(
        "Agent is already processing a prompt. Use steer() or followUp() to queue messages, or wait for completion."
    )]
    BusyPrompt,
    /// `continue()` while a run is active (agent.ts:349-351).
    #[error("Agent is already processing. Wait for completion before continuing.")]
    BusyContinue,
    /// `run_with_lifecycle` re-check (agent.ts:470-472).
    #[error("Agent is already processing.")]
    Busy,
    /// `continue()` with an empty transcript (agent.ts:354-356).
    #[error("No messages to continue from")]
    NoMessages,
    /// `continue()` from an assistant tail with nothing queued (agent.ts:371).
    #[error("Cannot continue from message role: assistant")]
    ContinueFromAssistant,
    /// `process_events` invoked with no active run (agent.ts:568-569).
    #[error("Agent listener invoked outside active run")]
    OutsideRun,
}

// --- PendingMessageQueue (agent.ts:123-157) ----------------------------------

/// FIFO queue of pending steering / follow-up messages (agent.ts:123-157).
///
/// [`Self::drain`] returns every queued message (`all`) or just the oldest one
/// (`one-at-a-time`).
#[derive(Debug)]
pub struct PendingMessageQueue {
    messages: Vec<AgentMessage>,
    /// Drain policy.
    pub mode: QueueMode,
}

impl PendingMessageQueue {
    /// New queue with the given drain policy.
    pub fn new(mode: QueueMode) -> Self {
        Self {
            messages: Vec::new(),
            mode,
        }
    }

    /// Append a message (agent.ts:131-133).
    pub fn enqueue(&mut self, message: AgentMessage) {
        self.messages.push(message);
    }

    /// Whether any messages are queued (agent.ts:135-137).
    pub fn has_items(&self) -> bool {
        !self.messages.is_empty()
    }

    /// Drain per the mode (agent.ts:139-152): all at once, or the oldest one.
    pub fn drain(&mut self) -> Vec<AgentMessage> {
        match self.mode {
            QueueMode::All => std::mem::take(&mut self.messages),
            QueueMode::OneAtATime => {
                if self.messages.is_empty() {
                    Vec::new()
                } else {
                    vec![self.messages.remove(0)]
                }
            }
        }
    }

    /// Clear all queued messages (agent.ts:154-156).
    pub fn clear(&mut self) {
        self.messages.clear();
    }
}

// --- MutableAgentState (agent.ts:60-94) --------------------------------------

/// Internal mutable state (agent.ts:60-94). `tools` / `messages` are
/// copy-on-assign: the setters clone the incoming slice so callers never share
/// the stored backing store.
struct MutableAgentState {
    system_prompt: String,
    model: Model,
    thinking_level: ThinkingLevel,
    tools: Vec<Arc<dyn AgentTool>>,
    messages: Vec<AgentMessage>,
    is_streaming: bool,
    streaming_message: Option<AgentMessage>,
    pending_tool_calls: HashSet<String>,
    error_message: Option<String>,
}

impl MutableAgentState {
    /// Copy-on-assign `tools` setter (agent.ts:80-82).
    fn set_tools(&mut self, tools: &[Arc<dyn AgentTool>]) {
        self.tools = tools.to_vec();
    }

    /// Copy-on-assign `messages` setter (agent.ts:86-88).
    fn set_messages(&mut self, messages: &[AgentMessage]) {
        self.messages = messages.to_vec();
    }
}

// --- AgentOptions (agent.ts:97-121) ------------------------------------------

/// Options for constructing an [`Agent`] (agent.ts:97-121).
///
/// The `model` is required (Rust has no `DEFAULT_MODEL` fake); every other field
/// has a default via [`AgentOptions::new`].
pub struct AgentOptions {
    /// Initial system prompt.
    pub system_prompt: String,
    /// Model used for provider requests.
    pub model: Model,
    /// Reasoning level.
    pub thinking_level: ThinkingLevel,
    /// Initial tools.
    pub tools: Vec<Arc<dyn AgentTool>>,
    /// Initial transcript.
    pub messages: Vec<AgentMessage>,
    /// `AgentMessage[]` → LLM conversion (default: filter to user/assistant/toolResult).
    pub convert_to_llm: Option<ConvertToLlmArc>,
    /// Optional `AgentMessage`-level transform.
    pub transform_context: Option<TransformContextArc>,
    /// Provider stream function (default: none → the loop emits an error stream).
    pub stream_fn: Option<StreamFn>,
    /// Optional per-provider API-key resolver.
    pub get_api_key: Option<GetApiKeyArc>,
    /// Optional pre-tool-call hook.
    pub before_tool_call: Option<BeforeToolCallArc>,
    /// Optional post-tool-call hook.
    pub after_tool_call: Option<AfterToolCallArc>,
    /// Steering drain policy (default: one-at-a-time).
    pub steering_mode: QueueMode,
    /// Follow-up drain policy (default: one-at-a-time).
    pub follow_up_mode: QueueMode,
    /// Session id forwarded to providers.
    pub session_id: Option<String>,
    /// Tool execution strategy (default: parallel).
    pub tool_execution: ToolExecutionMode,
}

impl AgentOptions {
    /// Defaults for every field except the required `model` (agent.ts:210-228).
    pub fn new(model: Model) -> Self {
        Self {
            system_prompt: String::new(),
            model,
            thinking_level: ThinkingLevel::Off,
            tools: Vec::new(),
            messages: Vec::new(),
            convert_to_llm: None,
            transform_context: None,
            stream_fn: None,
            get_api_key: None,
            before_tool_call: None,
            after_tool_call: None,
            steering_mode: QueueMode::OneAtATime,
            follow_up_mode: QueueMode::OneAtATime,
            session_id: None,
            tool_execution: ToolExecutionMode::Parallel,
        }
    }
}

// --- Agent (agent.ts:171-575) ------------------------------------------------

/// Shared, interior-mutable core of an [`Agent`]. Held behind an [`Arc`] so the
/// loop's `'static` event sink and the listener futures can capture it.
struct AgentInner {
    state: Mutex<MutableAgentState>,
    listeners: Mutex<Vec<AgentListener>>,
    steering: Arc<Mutex<PendingMessageQueue>>,
    follow_up: Arc<Mutex<PendingMessageQueue>>,
    /// The active run's cancellation token, `Some` while a run is in flight
    /// (the single-active-run guard, agent.ts:159-163,198,338).
    active_run: Mutex<Option<CancellationToken>>,
    /// `true` while a run is active — `false` transitions unblock `wait_for_idle`.
    idle: watch::Sender<bool>,
    // config
    convert_to_llm: ConvertToLlmArc,
    /// `agent.transformContext` (agent.ts:100) — mutable after construction
    /// (Pi's `_installAgentNextTurnRefresh`-style assignment); the extension
    /// host sets it at bind time.
    transform_context: Mutex<Option<TransformContextArc>>,
    stream_fn: Option<StreamFn>,
    get_api_key: Option<GetApiKeyArc>,
    /// `agent.beforeToolCall` (agent.ts:105) — mutable after construction.
    before_tool_call: Mutex<Option<BeforeToolCallArc>>,
    /// `agent.afterToolCall` (agent.ts:106) — mutable after construction.
    after_tool_call: Mutex<Option<AfterToolCallArc>>,
    session_id: Option<String>,
    tool_execution: ToolExecutionMode,
}

/// Stateful wrapper around the low-level agent loop (agent.ts:171).
///
/// `Agent` owns the current transcript, emits lifecycle events, executes tools,
/// and exposes queueing APIs for steering and follow-up messages.
#[derive(Clone)]
pub struct Agent {
    inner: Arc<AgentInner>,
}

impl Agent {
    /// Construct an `Agent` (agent.ts:210-229).
    pub fn new(options: AgentOptions) -> Self {
        let convert_to_llm = options.convert_to_llm.unwrap_or_else(|| {
            Arc::new(|msgs| Box::pin(async move { default_convert_to_llm(msgs) }))
        });
        let (idle, _rx) = watch::channel(false);
        let inner = AgentInner {
            state: Mutex::new(MutableAgentState {
                system_prompt: options.system_prompt,
                model: options.model,
                thinking_level: options.thinking_level,
                tools: options.tools,
                messages: options.messages,
                is_streaming: false,
                streaming_message: None,
                pending_tool_calls: HashSet::new(),
                error_message: None,
            }),
            listeners: Mutex::new(Vec::new()),
            steering: Arc::new(Mutex::new(PendingMessageQueue::new(options.steering_mode))),
            follow_up: Arc::new(Mutex::new(PendingMessageQueue::new(options.follow_up_mode))),
            active_run: Mutex::new(None),
            idle,
            convert_to_llm,
            transform_context: Mutex::new(options.transform_context),
            stream_fn: options.stream_fn,
            get_api_key: options.get_api_key,
            before_tool_call: Mutex::new(options.before_tool_call),
            after_tool_call: Mutex::new(options.after_tool_call),
            session_id: options.session_id,
            tool_execution: options.tool_execution,
        };
        Self {
            inner: Arc::new(inner),
        }
    }

    /// Subscribe to lifecycle events (agent.ts:241-244). Listeners are awaited in
    /// subscription order and included in the run's settlement.
    pub fn subscribe(&self, listener: AgentListener) {
        self.inner.listeners.lock().unwrap().push(listener);
    }

    /// Snapshot of the current transcript.
    pub fn messages(&self) -> Vec<AgentMessage> {
        self.inner.state.lock().unwrap().messages.clone()
    }

    /// Whether a run is currently streaming (`state.isStreaming`).
    pub fn is_streaming(&self) -> bool {
        self.inner.state.lock().unwrap().is_streaming
    }

    /// The most recent turn's error message, if any (`state.errorMessage`).
    pub fn error_message(&self) -> Option<String> {
        self.inner.state.lock().unwrap().error_message.clone()
    }

    /// Replace the transcript (copy-on-assign, agent.ts:86-88).
    pub fn set_messages(&self, messages: &[AgentMessage]) {
        self.inner.state.lock().unwrap().set_messages(messages);
    }

    /// Replace the tools (copy-on-assign, agent.ts:80-82).
    pub fn set_tools(&self, tools: &[Arc<dyn AgentTool>]) {
        self.inner.state.lock().unwrap().set_tools(tools);
    }

    /// `getActiveToolNames` (agent-session.ts:909-911) — `agent.state.tools.map(t => t.name)`.
    pub fn tool_names(&self) -> Vec<String> {
        self.inner
            .state
            .lock()
            .unwrap()
            .tools
            .iter()
            .map(|t| t.name().to_string())
            .collect()
    }

    /// `agent.transformContext = fn` (agent.ts:100) — post-construction hook
    /// assignment (the extension host's `_installAgentNextTurnRefresh` does
    /// this in Pi).
    pub fn set_transform_context(&self, hook: Option<TransformContextArc>) {
        *self.inner.transform_context.lock().unwrap() = hook;
    }

    /// `agent.beforeToolCall = fn` (agent.ts:105) — post-construction hook
    /// assignment (Pi's `_installAgentToolHooks`).
    pub fn set_before_tool_call(&self, hook: Option<BeforeToolCallArc>) {
        *self.inner.before_tool_call.lock().unwrap() = hook;
    }

    /// `agent.afterToolCall = fn` (agent.ts:106) — post-construction hook
    /// assignment (Pi's `_installAgentToolHooks`).
    pub fn set_after_tool_call(&self, hook: Option<AfterToolCallArc>) {
        *self.inner.after_tool_call.lock().unwrap() = hook;
    }

    /// Queue a steering message (agent.ts:274-276).
    pub fn steer(&self, message: AgentMessage) {
        self.inner.steering.lock().unwrap().enqueue(message);
    }

    /// Queue a follow-up message (agent.ts:279-281).
    pub fn follow_up(&self, message: AgentMessage) {
        self.inner.follow_up.lock().unwrap().enqueue(message);
    }

    /// True when either queue has pending messages (agent.ts:300-302).
    pub fn has_queued_messages(&self) -> bool {
        self.inner.steering.lock().unwrap().has_items()
            || self.inner.follow_up.lock().unwrap().has_items()
    }

    /// Abort the current run, if any (agent.ts:310-312).
    pub fn abort(&self) {
        if let Some(token) = self.inner.active_run.lock().unwrap().as_ref() {
            token.cancel();
        }
    }

    /// Resolve when the current run and its awaited listeners have finished
    /// (agent.ts:319-321).
    pub async fn wait_for_idle(&self) {
        let mut rx = self.inner.idle.subscribe();
        loop {
            if !*rx.borrow_and_update() {
                return;
            }
            if rx.changed().await.is_err() {
                return;
            }
        }
    }

    /// Start a new run from prompt text (agent.ts:334-345).
    pub async fn prompt(&self, text: &str) -> Result<(), AgentError> {
        let message = user_text_message(text);
        self.inner.run_prompt_messages(vec![message], false).await
    }

    /// Start a new run from explicit messages (agent.ts:335,343-344).
    pub async fn prompt_messages(&self, messages: Vec<AgentMessage>) -> Result<(), AgentError> {
        self.inner.run_prompt_messages(messages, false).await
    }

    /// Continue from the current transcript (agent.ts:348-375).
    pub async fn continue_run(&self) -> Result<(), AgentError> {
        self.inner.continue_run().await
    }
}

impl AgentInner {
    /// `continue()` (agent.ts:348-375): guard, inspect the tail, and either drain
    /// steering (skip-initial) / follow-up, reject an assistant tail, or continue.
    async fn continue_run(self: &Arc<Self>) -> Result<(), AgentError> {
        if self.active_run.lock().unwrap().is_some() {
            return Err(AgentError::BusyContinue);
        }
        let last = self.state.lock().unwrap().messages.last().cloned();
        let Some(last) = last else {
            return Err(AgentError::NoMessages);
        };
        if is_assistant(&last) {
            let queued_steering = self.steering.lock().unwrap().drain();
            if !queued_steering.is_empty() {
                return self.run_prompt_messages(queued_steering, true).await;
            }
            let queued_follow_ups = self.follow_up.lock().unwrap().drain();
            if !queued_follow_ups.is_empty() {
                return self.run_prompt_messages(queued_follow_ups, false).await;
            }
            return Err(AgentError::ContinueFromAssistant);
        }
        self.run_continuation().await
    }

    /// `runPromptMessages` (agent.ts:396-410).
    async fn run_prompt_messages(
        self: &Arc<Self>,
        messages: Vec<AgentMessage>,
        skip_initial_steering_poll: bool,
    ) -> Result<(), AgentError> {
        if self.active_run.lock().unwrap().is_some() {
            return Err(AgentError::BusyPrompt);
        }
        let inner = Arc::clone(self);
        self.run_with_lifecycle(move |signal| {
            let inner = Arc::clone(&inner);
            async move {
                let mut sink = inner.event_sink();
                let config = inner.create_loop_config(skip_initial_steering_poll);
                let context = inner.create_context_snapshot();
                run_agent_loop(
                    messages,
                    context,
                    config,
                    &mut sink,
                    Some(signal),
                    inner.stream_fn.clone(),
                )
                .await;
                Ok(())
            }
        })
        .await
    }

    /// `runContinuation` (agent.ts:412-422).
    async fn run_continuation(self: &Arc<Self>) -> Result<(), AgentError> {
        let inner = Arc::clone(self);
        self.run_with_lifecycle(move |signal| {
            let inner = Arc::clone(&inner);
            async move {
                let mut sink = inner.event_sink();
                let config = inner.create_loop_config(false);
                let context = inner.create_context_snapshot();
                let _ = run_agent_loop_continue(
                    context,
                    config,
                    &mut sink,
                    Some(signal),
                    inner.stream_fn.clone(),
                )
                .await;
                Ok(())
            }
        })
        .await
    }

    /// `createContextSnapshot` (agent.ts:424-430).
    fn create_context_snapshot(&self) -> AgentContext {
        let state = self.state.lock().unwrap();
        AgentContext {
            system_prompt: state.system_prompt.clone(),
            messages: state.messages.clone(),
            tools: Some(state.tools.clone()),
        }
    }

    /// The loop event sink = `process_events` (agent.ts:405). `'static`, capturing
    /// the shared core.
    fn event_sink(self: &Arc<Self>) -> AgentEventSink {
        let inner = Arc::clone(self);
        Box::new(move |event: AgentEvent| {
            let inner = Arc::clone(&inner);
            Box::pin(async move {
                let _ = inner.process_events(event).await;
            }) as BoxFuture<'static, ()>
        })
    }

    /// `createLoopConfig` (agent.ts:432-467). `getSteeringMessages` honors a
    /// one-shot `skipInitialSteeringPoll` flag.
    fn create_loop_config(self: &Arc<Self>, skip_initial_steering_poll: bool) -> AgentLoopConfig {
        let (model, thinking) = {
            let state = self.state.lock().unwrap();
            (state.model.clone(), state.thinking_level)
        };
        // reasoning: "off" → None else the level (agent.ts:436).
        let _reasoning = if thinking == ThinkingLevel::Off {
            None
        } else {
            Some(thinking)
        };

        let convert = Arc::clone(&self.convert_to_llm);
        let convert_to_llm =
            Box::new(move |msgs: Vec<AgentMessage>| convert(msgs)) as crate::types::ConvertToLlmFn;

        let transform_context = self.transform_context.lock().unwrap().clone().map(|arc| {
            Box::new(move |msgs, token| arc(msgs, token)) as crate::types::TransformContextFn
        });
        let get_api_key = self
            .get_api_key
            .clone()
            .map(|arc| Box::new(move |p| arc(p)) as crate::types::GetApiKeyFn);
        let before_tool_call = self.before_tool_call.lock().unwrap().clone().map(|arc| {
            Box::new(move |ctx, token| arc(ctx, token)) as crate::types::BeforeToolCallFn
        });
        let after_tool_call = self.after_tool_call.lock().unwrap().clone().map(|arc| {
            Box::new(move |ctx, token| arc(ctx, token)) as crate::types::AfterToolCallFn
        });

        // one-shot skip flag captured by getSteeringMessages (agent.ts:459-463).
        let skip = Arc::new(std::sync::atomic::AtomicBool::new(
            skip_initial_steering_poll,
        ));
        let steering = Arc::clone(&self.steering);
        let get_steering_messages = Box::new(move || {
            let skip = Arc::clone(&skip);
            let steering = Arc::clone(&steering);
            Box::pin(async move {
                if skip.swap(false, std::sync::atomic::Ordering::SeqCst) {
                    Vec::new()
                } else {
                    steering.lock().unwrap().drain()
                }
            }) as BoxFuture<'static, Vec<AgentMessage>>
        }) as crate::types::GetMessagesFn;

        let follow_up = Arc::clone(&self.follow_up);
        let get_follow_up_messages = Box::new(move || {
            let follow_up = Arc::clone(&follow_up);
            Box::pin(async move { follow_up.lock().unwrap().drain() })
                as BoxFuture<'static, Vec<AgentMessage>>
        }) as crate::types::GetMessagesFn;

        let _ = &self.session_id; // forwarded to providers via stream options (deferred field)

        AgentLoopConfig {
            model,
            api_key: None,
            tool_execution: Some(self.tool_execution),
            convert_to_llm,
            transform_context,
            get_api_key,
            should_stop_after_turn: None,
            prepare_next_turn: None,
            get_steering_messages: Some(get_steering_messages),
            get_follow_up_messages: Some(get_follow_up_messages),
            before_tool_call,
            after_tool_call,
        }
    }

    /// `runWithLifecycle` (agent.ts:469-492): set the active run, flip streaming
    /// state, run the executor, synthesize a failure on error, then `finishRun`.
    async fn run_with_lifecycle<Fut>(
        &self,
        executor: impl FnOnce(CancellationToken) -> Fut,
    ) -> Result<(), AgentError>
    where
        Fut: Future<Output = Result<(), String>>,
    {
        {
            let mut guard = self.active_run.lock().unwrap();
            if guard.is_some() {
                return Err(AgentError::Busy);
            }
            *guard = Some(CancellationToken::new());
        }
        let _ = self.idle.send(true);
        let token = self
            .active_run
            .lock()
            .unwrap()
            .as_ref()
            .expect("active run just set")
            .clone();

        {
            let mut state = self.state.lock().unwrap();
            state.is_streaming = true;
            state.streaming_message = None;
            state.error_message = None;
        }

        let result = executor(token.clone()).await;
        if let Err(error) = result {
            self.handle_run_failure(error, token.is_cancelled()).await;
        }
        self.finish_run();
        Ok(())
    }

    /// `handleRunFailure` (agent.ts:494-510): synthesize an aborted/error assistant
    /// message and drive `message_start → message_end → turn_end → agent_end`.
    async fn handle_run_failure(&self, error: String, aborted: bool) {
        let model = self.state.lock().unwrap().model.clone();
        let failure = AssistantMessage {
            role: AssistantRole::Assistant,
            content: vec![AssistantContent::Text(TextContent::new(""))],
            api: model.api.clone(),
            provider: model.provider.clone(),
            model: model.id.clone(),
            response_model: None,
            diagnostics: None,
            usage: empty_usage(),
            stop_reason: if aborted {
                StopReason::Aborted
            } else {
                StopReason::Error
            },
            timestamp: now_millis(),
            response_id: None,
            error_message: Some(error),
        };
        let message = AgentMessage::Llm(Message::Assistant(failure));
        let _ = self
            .process_events(AgentEvent::MessageStart {
                message: message.clone(),
            })
            .await;
        let _ = self
            .process_events(AgentEvent::MessageEnd {
                message: message.clone(),
            })
            .await;
        let _ = self
            .process_events(AgentEvent::TurnEnd {
                message: message.clone(),
                tool_results: Vec::new(),
            })
            .await;
        let _ = self
            .process_events(AgentEvent::AgentEnd {
                messages: vec![message],
            })
            .await;
    }

    /// `finishRun` (agent.ts:512-518).
    fn finish_run(&self) {
        {
            let mut state = self.state.lock().unwrap();
            state.is_streaming = false;
            state.streaming_message = None;
            state.pending_tool_calls = HashSet::new();
        }
        *self.active_run.lock().unwrap() = None;
        let _ = self.idle.send(false);
    }

    /// `processEvents` (agent.ts:527-574): reduce state for the event, then await
    /// every listener in subscription order with the run signal. Errs outside a run.
    async fn process_events(&self, event: AgentEvent) -> Result<(), AgentError> {
        {
            let mut state = self.state.lock().unwrap();
            match &event {
                AgentEvent::MessageStart { message }
                | AgentEvent::MessageUpdate { message, .. } => {
                    state.streaming_message = Some(message.clone());
                }
                AgentEvent::MessageEnd { message } => {
                    state.streaming_message = None;
                    state.messages.push(message.clone());
                }
                AgentEvent::ToolExecutionStart { tool_call_id, .. } => {
                    state.pending_tool_calls.insert(tool_call_id.clone());
                }
                AgentEvent::ToolExecutionEnd { tool_call_id, .. } => {
                    state.pending_tool_calls.remove(tool_call_id);
                }
                AgentEvent::TurnEnd {
                    message: AgentMessage::Llm(Message::Assistant(a)),
                    ..
                } => {
                    if let Some(err) = a.error_message.as_ref() {
                        state.error_message = Some(err.clone());
                    }
                }
                AgentEvent::AgentEnd { .. } => {
                    state.streaming_message = None;
                }
                _ => {}
            }
        }

        let signal = self.active_run.lock().unwrap().as_ref().cloned();
        let Some(signal) = signal else {
            return Err(AgentError::OutsideRun);
        };
        let listeners = self.listeners.lock().unwrap().clone();
        for listener in listeners {
            listener(event.clone(), signal.clone()).await;
        }
        Ok(())
    }
}

/// Default converter (agent.ts:32-36): keep only user / assistant / toolResult.
fn default_convert_to_llm(messages: Vec<AgentMessage>) -> Vec<Message> {
    messages
        .into_iter()
        .filter_map(|m| match m {
            AgentMessage::Llm(msg) => Some(msg),
            _ => None,
        })
        .collect()
}

fn is_assistant(message: &AgentMessage) -> bool {
    matches!(message, AgentMessage::Llm(Message::Assistant(_)))
}

fn user_text_message(text: &str) -> AgentMessage {
    AgentMessage::Llm(Message::User(UserMessage {
        role: UserRole::User,
        content: UserMessageContent::Blocks(vec![UserContent::Text(TextContent::new(text))]),
        timestamp: now_millis(),
    }))
}

fn empty_usage() -> Usage {
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

fn now_millis() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    use pirust_ai::api::{ProviderStreams, SimpleStreamOptions};
    use pirust_ai::providers::faux::{faux_text_message, Faux};
    use pirust_ai::types::{Context, Model};

    fn faux_model() -> Model {
        Faux::new().get_model().clone()
    }

    /// A stream function that replays `faux_text_message("done")` on each call.
    fn done_stream_fn() -> StreamFn {
        Arc::new(
            move |model: Model, ctx: Context, opts: SimpleStreamOptions, _token| {
                let faux = Faux::new().with_token_size(1000, 1000);
                faux.set_responses(vec![faux_text_message("done").into()]);
                faux.stream_simple(&model, &ctx, Some(opts))
            },
        )
    }

    fn assistant_message(text: &str) -> AgentMessage {
        AgentMessage::Llm(Message::Assistant(faux_text_message(text)))
    }

    // (a) single-active-run guard rejects a concurrent prompt (agent.ts:338-342).
    #[tokio::test]
    async fn single_active_run_guard_rejects_concurrent_prompt() {
        let mut options = AgentOptions::new(faux_model());
        options.stream_fn = Some(done_stream_fn());
        let agent = Agent::new(options);

        let captured: Arc<Mutex<Option<AgentError>>> = Arc::new(Mutex::new(None));
        let tried = Arc::new(AtomicBool::new(false));
        {
            let agent2 = agent.clone();
            let captured = Arc::clone(&captured);
            let tried = Arc::clone(&tried);
            agent.subscribe(Arc::new(move |_event, _signal| {
                let agent2 = agent2.clone();
                let captured = Arc::clone(&captured);
                let tried = Arc::clone(&tried);
                Box::pin(async move {
                    // First event only: a reentrant prompt must hit the guard.
                    if !tried.swap(true, Ordering::SeqCst) {
                        let result = agent2.prompt("concurrent").await;
                        *captured.lock().unwrap() = result.err();
                    }
                }) as BoxFuture<'static, ()>
            }));
        }

        agent.prompt("first").await.expect("first prompt ok");
        assert_eq!(*captured.lock().unwrap(), Some(AgentError::BusyPrompt));
    }

    // (b) continue() from an assistant tail with nothing queued rejects
    //     (agent.ts:358-372).
    #[tokio::test]
    async fn continue_from_assistant_tail_rejects() {
        let mut options = AgentOptions::new(faux_model());
        options.stream_fn = Some(done_stream_fn());
        options.messages = vec![assistant_message("earlier answer")];
        let agent = Agent::new(options);

        let error = agent.continue_run().await.expect_err("must reject");
        assert_eq!(error, AgentError::ContinueFromAssistant);
        assert_eq!(
            error.to_string(),
            "Cannot continue from message role: assistant"
        );
    }

    // (c) run-failure synthesis: an aborted/error assistant + the
    //     message_start → message_end → turn_end → agent_end tail (agent.ts:494-510).
    #[tokio::test]
    async fn run_failure_synthesizes_error_assistant_tail() {
        let mut options = AgentOptions::new(faux_model());
        options.stream_fn = Some(done_stream_fn());
        let agent = Agent::new(options);

        let tape: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let failure: Arc<Mutex<Option<AssistantMessage>>> = Arc::new(Mutex::new(None));
        {
            let tape = Arc::clone(&tape);
            let failure = Arc::clone(&failure);
            agent.subscribe(Arc::new(move |event: AgentEvent, _signal| {
                let tape = Arc::clone(&tape);
                let failure = Arc::clone(&failure);
                let kind = match &event {
                    AgentEvent::MessageStart { .. } => "message_start",
                    AgentEvent::MessageEnd { message } => {
                        if let AgentMessage::Llm(Message::Assistant(a)) = message {
                            *failure.lock().unwrap() = Some(a.clone());
                        }
                        "message_end"
                    }
                    AgentEvent::TurnEnd { .. } => "turn_end",
                    AgentEvent::AgentEnd { .. } => "agent_end",
                    _ => "other",
                }
                .to_string();
                Box::pin(async move {
                    tape.lock().unwrap().push(kind);
                }) as BoxFuture<'static, ()>
            }));
        }

        // Drive the real lifecycle with a failing executor (the loop is infallible
        // in Rust, so the executor is the fault-injection point — see module docs).
        agent
            .inner
            .run_with_lifecycle(|_signal| async { Err("boom".to_string()) })
            .await
            .expect("lifecycle returns Ok after synthesizing the failure");

        assert_eq!(
            *tape.lock().unwrap(),
            vec!["message_start", "message_end", "turn_end", "agent_end"],
        );
        let failure = failure.lock().unwrap().clone().expect("failure message");
        assert_eq!(failure.stop_reason, StopReason::Error);
        assert_eq!(failure.error_message.as_deref(), Some("boom"));

        // The synthesized failure message is recorded in the transcript.
        let messages = agent.messages();
        assert!(matches!(
            messages.last(),
            Some(AgentMessage::Llm(Message::Assistant(_)))
        ));
        // Run cleared itself (idle again).
        assert!(!agent.is_streaming());
        assert_eq!(agent.error_message().as_deref(), Some("boom"));
    }

    // process_events invoked outside a run errs (agent.ts:568-569).
    #[tokio::test]
    async fn process_events_outside_run_errs() {
        let options = AgentOptions::new(faux_model());
        let agent = Agent::new(options);
        let result = agent.inner.process_events(AgentEvent::AgentStart).await;
        assert_eq!(result, Err(AgentError::OutsideRun));
    }

    #[test]
    fn pending_queue_drains_by_mode() {
        let mut all = PendingMessageQueue::new(QueueMode::All);
        all.enqueue(assistant_message("a"));
        all.enqueue(assistant_message("b"));
        assert_eq!(all.drain().len(), 2);
        assert!(!all.has_items());

        let mut one = PendingMessageQueue::new(QueueMode::OneAtATime);
        one.enqueue(assistant_message("a"));
        one.enqueue(assistant_message("b"));
        assert_eq!(one.drain().len(), 1);
        assert!(one.has_items());
        assert_eq!(one.drain().len(), 1);
        assert!(!one.has_items());
    }
}
