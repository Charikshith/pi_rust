//! Harness — orchestration, sessions, compaction. Port of
//! `packages/agent/src/harness/`.
//!
//! Spec: `docs/analysis/07-agent-core-spec.md` §4 (`AgentHarness` orchestration
//! & session tree). `[INTEGRATOR]` at `mod.rs` level; leaf submodules below.
//!
//! Module tree (§13). Present now:
//! - `types`: foundational Result/error taxonomy + FS/Shell/Env traits +
//!   `SessionTreeEntry` + event/hook types (lands FIRST).
//! - `messages`: 4 custom message variants + `convert_to_llm` + prefixes.
//! - `session`: `Session`, storage traits + in-memory/jsonl impls, uuid.
//! - `compaction`: estimate / find-cut-point / prepare / compact.
//!
//! # `AgentHarness` (§4)
//!
//! The [`AgentHarness`] drives the low-level loop directly (it does NOT use the
//! [`crate::agent::Agent`] class — agent-harness.ts:565) and layers the
//! HARNESS-OWN events on top of the loop's [`AgentEvent`]s in the exact positions
//! Pi emits them (spec §1 note + §4.2):
//! - `after_provider_response` — right after each provider response, before the
//!   assistant message surfaces (agent-harness.ts:371-377);
//! - `save_point` — after each turn's session-tree writes flush
//!   (agent-harness.ts:504);
//! - `settled` — the terminal event after `agent_end` (agent-harness.ts:511).
//!
//! Session writes are append-only (§4.2): the user prompt, assistant messages,
//! and tool-result messages land in the tree on each `message_end`
//! (agent-harness.ts:489-492). Leaf-repointing (§4.4) is preserved via
//! [`AgentHarness::navigate_tree`], which delegates to [`session::Session::move_to`].
//!
//! ## Rust adaptation: `after_provider_response`
//!
//! Pi wires `after_provider_response` through the `onResponse` callback that its
//! `models.streamSimple` invokes when the provider responds
//! (agent-harness.ts:371). The ported [`crate::agent_loop::StreamFn`] is a plain
//! synchronous factory with no such callback (feat-002 deferred the callbacks on
//! `SimpleStreamOptions`). So the harness records a pending
//! `after_provider_response` the instant its stream function is invoked, and
//! FLUSHES it at the very start of the next handled loop event — which is always
//! the assistant `message_start`. Because the harness awaits the loop inline on a
//! single task, this reproduces Pi's exact ordering:
//! `… message_end(prompt), after_provider_response, message_start(assistant) …`.

pub mod compaction;
pub mod messages;
pub mod session;
pub mod types;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use futures::future::BoxFuture;
use pirust_ai::types::{AssistantMessage, Message, Model};
use tokio_util::sync::CancellationToken;

use crate::agent_loop::{run_agent_loop, AgentEventSink, StreamFn};
use crate::harness::compaction::{v4 as compaction_v4, DEFAULT_COMPACTION_SETTINGS};
use crate::harness::messages::{convert_to_llm, AgentMessage};
use crate::harness::session::v4::session::Session as V4Session;
use crate::harness::session::v4::types::{
    Entry, EntryOrder, EntryQuery, ProvisionedActiveToolsEntry, ProvisionedCompactionEntry,
    ProvisionedEntry, ProvisionedModelChangeEntry, ProvisionedThinkingLevelEntry,
    SessionStorage as V4SessionStorage,
};
use crate::harness::types::SessionError;
use crate::harness::types::{AgentHarnessError, AgentHarnessErrorCode, PendingSessionWrite};
use crate::types::{
    AgentContext, AgentEvent, AgentLoopConfig, AgentLoopTurnUpdate, AgentTool, ThinkingLevel,
    ToolExecutionMode,
};

use pirust_ai::types::{TextContent, UserContent, UserMessage, UserMessageContent, UserRole};

// ===========================================================================
// Harness events (§4.3) — loop events + the three harness-own events
// ===========================================================================

/// The `after_provider_response` payload (agent-harness.ts:371-377).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AfterProviderResponse {
    /// HTTP-ish status of the provider response.
    pub status: u16,
    /// Response headers (lower-cased keys, insertion order not significant).
    pub headers: HashMap<String, String>,
}

/// Every event a [`AgentHarness`] subscriber can observe: the loop's
/// [`AgentEvent`] union PLUS the three harness-own events (spec §1 note, §4.2).
///
/// Pi forwards loop events verbatim to subscribers (`emitAny`) and emits its own
/// events via `emitOwn` — both hit the same wildcard listener. This enum unifies
/// the two so a single subscriber sees the full ordered tape.
// The `Loop` variant carries a full `AgentEvent` (itself a large message union);
// the size spread is inherent to faithfully forwarding the event, which is moved
// through, not stored in bulk.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub enum HarnessEvent {
    /// A forwarded low-level loop event (agent-harness.ts `emitAny`).
    Loop(AgentEvent),
    /// After a provider response (agent-harness.ts:371-377).
    AfterProviderResponse(AfterProviderResponse),
    /// After a turn's session writes flush (agent-harness.ts:504).
    SavePoint {
        /// Whether any pending mutations were flushed at this save point.
        had_pending_mutations: bool,
    },
    /// Terminal event after `agent_end` (agent-harness.ts:511).
    Settled {
        /// Number of queued next-turn messages at settlement.
        next_turn_count: usize,
    },
    /// Leaf repointed by [`AgentHarness::navigate_tree`] (agent-harness.ts:814-820).
    SessionTree {
        /// The new leaf id after repointing.
        new_leaf_id: Option<String>,
        /// The previous leaf id.
        old_leaf_id: Option<String>,
    },
    /// A compaction entry was appended (agent-harness.ts:722).
    SessionCompact {
        /// The appended compaction entry id.
        compaction_entry_id: String,
        /// Whether the summary came from a hook.
        from_hook: bool,
    },
}

impl HarnessEvent {
    /// The wire `type` string for this event (matches Pi's `event.type`).
    pub fn event_type(&self) -> &'static str {
        match self {
            HarnessEvent::Loop(event) => loop_event_type(event),
            HarnessEvent::AfterProviderResponse(_) => "after_provider_response",
            HarnessEvent::SavePoint { .. } => "save_point",
            HarnessEvent::Settled { .. } => "settled",
            HarnessEvent::SessionTree { .. } => "session_tree",
            HarnessEvent::SessionCompact { .. } => "session_compact",
        }
    }
}

/// The `type` string of a low-level loop [`AgentEvent`].
fn loop_event_type(event: &AgentEvent) -> &'static str {
    match event {
        AgentEvent::AgentStart => "agent_start",
        AgentEvent::AgentEnd { .. } => "agent_end",
        AgentEvent::TurnStart => "turn_start",
        AgentEvent::TurnEnd { .. } => "turn_end",
        AgentEvent::MessageStart { .. } => "message_start",
        AgentEvent::MessageUpdate { .. } => "message_update",
        AgentEvent::MessageEnd { .. } => "message_end",
        AgentEvent::ToolExecutionStart { .. } => "tool_execution_start",
        AgentEvent::ToolExecutionUpdate { .. } => "tool_execution_update",
        AgentEvent::ToolExecutionEnd { .. } => "tool_execution_end",
    }
}

/// A harness event subscriber (agent-harness.ts:1003). `'static` + `Send`/`Sync`
/// so it can be driven from the loop's event sink; events are passed by clone.
pub type HarnessListener = Arc<dyn Fn(HarnessEvent) -> BoxFuture<'static, ()> + Send + Sync>;

// ===========================================================================
// Phase FSM (agent-harness.ts:165)
// ===========================================================================

/// Harness phase FSM (agent-harness.ts:165). Public control methods reject unless
/// `Idle`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentHarnessPhase {
    Idle,
    Turn,
    Compaction,
    BranchSummary,
    Retry,
}

// ===========================================================================
// Shared interior-mutable core
// ===========================================================================

/// Shared, interior-mutable core of an [`AgentHarness`]. Held behind an [`Arc`] so
/// the loop's `'static` event sink and the wrapped stream function can capture it.
struct HarnessShared<St: V4SessionStorage + Send + Sync + 'static> {
    session: V4Session<St>,
    /// Writes queued while a run is active; flushed at turn boundaries (§4.2).
    pending_writes: Mutex<Vec<PendingSessionWrite>>,
    /// Wildcard subscribers (agent-harness.ts subscribe / `emitAny`+`emitOwn`).
    subscribers: Mutex<Vec<HarnessListener>>,
    /// A pending `after_provider_response`, set the moment the stream function is
    /// invoked and flushed before the next handled loop event.
    pending_response: Mutex<Option<AfterProviderResponse>>,
    phase: Mutex<AgentHarnessPhase>,
    /// Steering queue drained at each turn boundary (agent-harness.ts:445).
    steer_queue: Mutex<Vec<AgentMessage>>,
    /// Follow-up queue drained when the loop would otherwise stop
    /// (agent-harness.ts:446).
    follow_up_queue: Mutex<Vec<AgentMessage>>,
    /// Next-turn queue count reported in `settled` (agent-harness.ts:511).
    next_turn_count: Mutex<usize>,
    /// The in-flight turn's cancellation token, set for the duration of
    /// `execute_turn` — `abort()` cancels it (feat-012 RPC `abort` command).
    active_token: Mutex<Option<CancellationToken>>,
}

impl<St: V4SessionStorage + Send + Sync + 'static> HarnessShared<St> {
    /// Emit an event to every subscriber, in subscription order (agent-harness.ts
    /// `emitAny`/`emitOwn`, 212-230).
    async fn emit(&self, event: HarnessEvent) {
        let listeners = self.subscribers.lock().unwrap().clone();
        for listener in listeners {
            listener(event.clone()).await;
        }
    }

    /// Flush the pending `after_provider_response`, if any (see module docs).
    async fn flush_pending_response(&self) {
        let pending = self.pending_response.lock().unwrap().take();
        if let Some(response) = pending {
            self.emit(HarnessEvent::AfterProviderResponse(response))
                .await;
        }
    }

    /// Drain the queued session writes to the tree (agent-harness.ts:462-486).
    async fn flush_pending_session_writes(&self) {
        loop {
            let write = {
                let mut writes = self.pending_writes.lock().unwrap();
                if writes.is_empty() {
                    break;
                }
                writes.remove(0)
            };
            let _ = self.apply_pending_write(write).await;
        }
    }

    async fn apply_pending_write(&self, write: PendingSessionWrite) -> Result<(), SessionError> {
        match write {
            PendingSessionWrite::Message { message } => {
                self.session.append_message(message).map(|_| ())
            }
            PendingSessionWrite::ModelChange { provider, model_id } => {
                let entry = ProvisionedEntry::ModelChange(ProvisionedModelChangeEntry {
                    id: self.session.new_id(),
                    provider,
                    model_id,
                });
                self.session.append_entry(&entry, "main").map(|_| ())
            }
            PendingSessionWrite::ThinkingLevelChange { thinking_level } => {
                let entry = ProvisionedEntry::ThinkingLevel(ProvisionedThinkingLevelEntry {
                    id: self.session.new_id(),
                    thinking_level,
                });
                self.session.append_entry(&entry, "main").map(|_| ())
            }
            PendingSessionWrite::ActiveToolsChange { active_tool_names } => {
                let entry = ProvisionedEntry::ActiveTools(ProvisionedActiveToolsEntry {
                    id: self.session.new_id(),
                    active_tool_names,
                });
                self.session.append_entry(&entry, "main").map(|_| ())
            }
            PendingSessionWrite::Compaction {
                summary,
                first_kept_entry_id: _,
                tokens_before,
                details,
                from_hook: _,
            } => {
                let entry = ProvisionedEntry::Compaction(ProvisionedCompactionEntry {
                    id: self.session.new_id(),
                    summary,
                    retained_tail: Vec::new(),
                    tokens_before,
                    details,
                    usage: None,
                });
                self.session.append_entry(&entry, "main").map(|_| ())
            }
            PendingSessionWrite::BranchSummary { .. } => Ok(()),
            PendingSessionWrite::Custom { custom_type, data } => self
                .session
                .append_custom_entry(&custom_type, data)
                .map(|_| ()),
            // v4 has no custom_message / label / session_info tree entries —
            // custom entries cover application data; labels and the session name
            // are lane-level mutation-log facts (0.84.2 contract).
            PendingSessionWrite::CustomMessage { .. } => Ok(()),
            PendingSessionWrite::Label { target_id, label } => {
                self.session.set_label(&target_id, label.as_deref())
            }
            PendingSessionWrite::SessionInfo { name } => self.session.set_name(name.as_deref()),
            PendingSessionWrite::Leaf { target_id } => {
                self.session.move_lane("main", target_id.as_deref())
            }
        }
    }

    /// The loop event sink (agent-harness.ts:488-515): append messages, flush
    /// writes at turn boundaries, and layer the harness-own events in position.
    async fn handle_agent_event(&self, event: AgentEvent) {
        // Any pending `after_provider_response` precedes the current event (the
        // assistant `message_start` — see module docs).
        self.flush_pending_response().await;

        match &event {
            AgentEvent::MessageEnd { message } => {
                let _ = self.session.append_message(message.clone());
                self.emit(HarnessEvent::Loop(event.clone())).await;
            }
            AgentEvent::TurnEnd { .. } => {
                self.emit(HarnessEvent::Loop(event.clone())).await;
                let had_pending_mutations = !self.pending_writes.lock().unwrap().is_empty();
                self.flush_pending_session_writes().await;
                self.emit(HarnessEvent::SavePoint {
                    had_pending_mutations,
                })
                .await;
            }
            AgentEvent::AgentEnd { .. } => {
                self.flush_pending_session_writes().await;
                *self.phase.lock().unwrap() = AgentHarnessPhase::Idle;
                self.emit(HarnessEvent::Loop(event.clone())).await;
                let next_turn_count = *self.next_turn_count.lock().unwrap();
                self.emit(HarnessEvent::Settled { next_turn_count }).await;
            }
            _ => {
                self.emit(HarnessEvent::Loop(event.clone())).await;
            }
        }
    }
}

// ===========================================================================
// Options + harness
// ===========================================================================

/// Construction options for an [`AgentHarness`] (agent-harness.ts:183-206).
pub struct AgentHarnessOptions<St: V4SessionStorage + Send + Sync + 'static> {
    /// The provider stream function (faux for tests). Wrapped internally to emit
    /// `after_provider_response`.
    pub provider: StreamFn,
    /// The active model.
    pub model: Model,
    /// The v4 mutation-log session.
    pub session: V4Session<St>,
    /// Available tools.
    pub tools: Vec<Arc<dyn AgentTool>>,
    /// Active tool names (defaults to every tool's name).
    pub active_tool_names: Option<Vec<String>>,
    /// System prompt (string form).
    pub system_prompt: String,
    /// Reasoning level.
    pub thinking_level: ThinkingLevel,
}

impl<St: V4SessionStorage + Send + Sync + 'static> AgentHarnessOptions<St> {
    /// Minimal options with sensible defaults.
    pub fn new(provider: StreamFn, model: Model, session: V4Session<St>) -> Self {
        Self {
            provider,
            model,
            session,
            tools: Vec::new(),
            active_tool_names: None,
            system_prompt: "You are a helpful assistant.".to_string(),
            thinking_level: ThinkingLevel::Off,
        }
    }
}

/// Batteries-included agent orchestrator (agent-harness.ts:157). Drives the loop
/// directly, writes the session tree append-only, and emits the harness-own
/// events on top of the loop tape.
pub struct AgentHarness<St: V4SessionStorage + Send + Sync + 'static> {
    shared: Arc<HarnessShared<St>>,
    provider: StreamFn,
    /// Interior-mutable so `set_model`/`set_thinking_level` (feat-012 RPC) can
    /// mutate through `&self` — matches `Agent`'s own `Mutex<AgentState>` pattern.
    model: Mutex<Model>,
    thinking_level: Mutex<ThinkingLevel>,
    system_prompt: String,
    tools: Vec<Arc<dyn AgentTool>>,
    active_tool_names: Vec<String>,
}

impl<St: V4SessionStorage + Send + Sync + 'static> AgentHarness<St> {
    /// Construct a harness (agent-harness.ts:183-206).
    pub fn new(options: AgentHarnessOptions<St>) -> Self {
        let active_tool_names = options
            .active_tool_names
            .unwrap_or_else(|| options.tools.iter().map(|t| t.name().to_string()).collect());
        let shared = Arc::new(HarnessShared {
            session: options.session,
            pending_writes: Mutex::new(Vec::new()),
            subscribers: Mutex::new(Vec::new()),
            pending_response: Mutex::new(None),
            phase: Mutex::new(AgentHarnessPhase::Idle),
            steer_queue: Mutex::new(Vec::new()),
            follow_up_queue: Mutex::new(Vec::new()),
            next_turn_count: Mutex::new(0),
            active_token: Mutex::new(None),
        });
        Self {
            shared,
            provider: options.provider,
            model: Mutex::new(options.model),
            thinking_level: Mutex::new(options.thinking_level),
            system_prompt: options.system_prompt,
            tools: options.tools,
            active_tool_names,
        }
    }

    /// Register a wildcard subscriber (agent-harness.ts:1003).
    pub fn subscribe(&self, listener: HarnessListener) {
        self.shared.subscribers.lock().unwrap().push(listener);
    }

    /// Borrow the session (read-only), e.g. to inspect entries in tests.
    pub fn session(&self) -> &V4Session<St> {
        &self.shared.session
    }

    /// Current phase.
    pub fn phase(&self) -> AgentHarnessPhase {
        *self.shared.phase.lock().unwrap()
    }

    /// The active model.
    pub fn model(&self) -> Model {
        self.model.lock().unwrap().clone()
    }

    /// Replace the active model (feat-012 RPC `set_model`/`cycle_model`).
    /// Applies to the next turn — matches `Agent::set_model`'s copy-on-assign.
    pub fn set_model(&self, model: Model) {
        *self.model.lock().unwrap() = model;
    }

    /// The current reasoning level.
    pub fn thinking_level(&self) -> ThinkingLevel {
        *self.thinking_level.lock().unwrap()
    }

    /// Replace the reasoning level (feat-012 RPC `set_thinking_level`).
    pub fn set_thinking_level(&self, level: ThinkingLevel) {
        *self.thinking_level.lock().unwrap() = level;
    }

    /// Queued steer + follow-up message count (feat-012 RPC `get_state`'s
    /// `pendingMessageCount`).
    pub fn pending_message_count(&self) -> usize {
        self.shared.steer_queue.lock().unwrap().len()
            + self.shared.follow_up_queue.lock().unwrap().len()
    }

    /// The current branch's context messages (feat-012 RPC `get_messages` /
    /// `get_state.messageCount` / `get_last_assistant_text`).
    pub fn messages(&self) -> Vec<AgentMessage> {
        build_v4_context_messages(&self.shared.session).unwrap_or_default()
    }

    /// The current branch's entries, root-first (feat-012 RPC `get_entries`).
    pub fn entries(&self) -> Result<Vec<Entry>, AgentHarnessError> {
        v4_branch_entries(&self.shared.session)
            .map_err(|e| AgentHarnessError::new(AgentHarnessErrorCode::Session, e.message.clone()))
    }

    /// Cancel the in-flight turn, if any (feat-012 RPC `abort`). Best-effort:
    /// this port has no distinct aborted-vs-settled terminal event yet, so
    /// cancellation surfaces through the loop's own early-stop behavior —
    /// named here, not silently assumed identical to Pi's `session.abort()`.
    pub fn abort(&self) {
        if let Some(token) = self.shared.active_token.lock().unwrap().as_ref() {
            token.cancel();
        }
    }

    /// The tools currently selected as active (`activeToolNames` order).
    fn active_tools(&self) -> Vec<Arc<dyn AgentTool>> {
        self.active_tool_names
            .iter()
            .filter_map(|name| self.tools.iter().find(|t| t.name() == name).cloned())
            .collect()
    }

    /// Wrap the raw provider so invoking it records a pending
    /// `after_provider_response` (see module docs).
    fn create_stream_fn(&self) -> StreamFn {
        let shared = Arc::clone(&self.shared);
        let provider = Arc::clone(&self.provider);
        Arc::new(move |model, ctx, opts, token| {
            *shared.pending_response.lock().unwrap() = Some(AfterProviderResponse {
                status: 200,
                headers: HashMap::new(),
            });
            provider(model, ctx, opts, token)
        })
    }

    fn event_sink(&self) -> AgentEventSink {
        let shared = Arc::clone(&self.shared);
        Box::new(move |event: AgentEvent| {
            let shared = Arc::clone(&shared);
            Box::pin(async move { shared.handle_agent_event(event).await })
                as BoxFuture<'static, ()>
        })
    }

    /// Bridge harness state into an [`AgentLoopConfig`] (agent-harness.ts:399-448).
    /// `prepareNextTurn` flushes pending writes and rebuilds the context from the
    /// session; the steering / follow-up drains pull from the internal queues.
    fn create_loop_config(&self) -> AgentLoopConfig {
        let shared = Arc::clone(&self.shared);
        let model = self.model();
        let system_prompt = self.system_prompt.clone();
        let active_tools = self.active_tools();
        let thinking_level = self.thinking_level();

        let prepare_next_turn = {
            let shared = Arc::clone(&shared);
            let model = model.clone();
            let system_prompt = system_prompt.clone();
            let active_tools = active_tools.clone();
            Box::new(move |_ctx| {
                let shared = Arc::clone(&shared);
                let model = model.clone();
                let system_prompt = system_prompt.clone();
                let active_tools = active_tools.clone();
                Box::pin(async move {
                    shared.flush_pending_session_writes().await;
                    let messages = build_v4_context_messages(&shared.session).unwrap_or_default();
                    Some(AgentLoopTurnUpdate {
                        context: Some(AgentContext {
                            system_prompt,
                            messages,
                            tools: Some(active_tools),
                        }),
                        model: Some(model),
                        thinking_level: Some(thinking_level),
                    })
                }) as BoxFuture<'static, Option<AgentLoopTurnUpdate>>
            }) as crate::types::PrepareNextTurnFn
        };

        let get_steering_messages = {
            let shared = Arc::clone(&shared);
            Box::new(move || {
                let shared = Arc::clone(&shared);
                Box::pin(async move { std::mem::take(&mut *shared.steer_queue.lock().unwrap()) })
                    as BoxFuture<'static, Vec<AgentMessage>>
            }) as crate::types::GetMessagesFn
        };

        let get_follow_up_messages = {
            let shared = Arc::clone(&shared);
            Box::new(move || {
                let shared = Arc::clone(&shared);
                Box::pin(
                    async move { std::mem::take(&mut *shared.follow_up_queue.lock().unwrap()) },
                ) as BoxFuture<'static, Vec<AgentMessage>>
            }) as crate::types::GetMessagesFn
        };

        AgentLoopConfig {
            model,
            api_key: None,
            tool_execution: Some(ToolExecutionMode::Parallel),
            convert_to_llm: Box::new(|msgs| Box::pin(async move { convert_to_llm(&msgs) })),
            transform_context: None,
            get_api_key: None,
            should_stop_after_turn: None,
            prepare_next_turn: Some(prepare_next_turn),
            get_steering_messages: Some(get_steering_messages),
            get_follow_up_messages: Some(get_follow_up_messages),
            before_tool_call: None,
            after_tool_call: None,
        }
    }

    /// Run one prompt turn to completion, returning the final assistant message
    /// (agent-harness.ts:608-621).
    pub async fn prompt(&self, text: &str) -> Result<AssistantMessage, AgentHarnessError> {
        {
            let mut phase = self.shared.phase.lock().unwrap();
            if *phase != AgentHarnessPhase::Idle {
                return Err(AgentHarnessError::new(
                    AgentHarnessErrorCode::Busy,
                    "AgentHarness is busy",
                ));
            }
            *phase = AgentHarnessPhase::Turn;
        }
        let result = self.execute_turn(text).await;
        if result.is_err() {
            *self.shared.phase.lock().unwrap() = AgentHarnessPhase::Idle;
        }
        result
    }

    /// Build the turn state, seed the user prompt, drive the loop, and pluck the
    /// last assistant message (agent-harness.ts:531-606).
    async fn execute_turn(&self, text: &str) -> Result<AssistantMessage, AgentHarnessError> {
        let context = build_v4_context(&self.shared.session).map_err(|e| {
            AgentHarnessError::new(AgentHarnessErrorCode::Session, e.message.clone())
        })?;
        let ctx = AgentContext {
            system_prompt: self.system_prompt.clone(),
            messages: context.messages,
            tools: Some(self.active_tools()),
        };
        let prompts = vec![user_message(text)];

        let config = self.create_loop_config();
        let stream_fn = self.create_stream_fn();
        let mut sink = self.event_sink();
        let token = CancellationToken::new();
        *self.shared.active_token.lock().unwrap() = Some(token.clone());

        let new_messages = run_agent_loop(
            prompts,
            ctx,
            config,
            &mut sink,
            Some(token),
            Some(stream_fn),
        )
        .await;
        *self.shared.active_token.lock().unwrap() = None;

        for message in new_messages.iter().rev() {
            if let AgentMessage::Llm(Message::Assistant(assistant)) = message {
                return Ok(assistant.clone());
            }
        }
        Err(AgentHarnessError::new(
            AgentHarnessErrorCode::InvalidState,
            "AgentHarness prompt completed without an assistant message",
        ))
    }

    /// Queue a steering message for the current run (agent-harness.ts:657-661).
    pub fn steer(&self, message: AgentMessage) {
        self.shared.steer_queue.lock().unwrap().push(message);
    }

    /// Queue a follow-up message for the current run (agent-harness.ts:663-667).
    pub fn follow_up(&self, message: AgentMessage) {
        self.shared.follow_up_queue.lock().unwrap().push(message);
    }

    /// Compaction wiring (agent-harness.ts:686-730). Runs the DETERMINISTIC
    /// [`prepare_compaction`] where Pi does; the LLM summary generation is deferred
    /// (a stub), so this appends a compaction with a placeholder summary using the
    /// prepared cut point / token estimate — it never blocks on an LLM.
    pub async fn compact(&self) -> Result<CompactionOutcome, AgentHarnessError> {
        {
            let mut phase = self.shared.phase.lock().unwrap();
            if *phase != AgentHarnessPhase::Idle {
                return Err(AgentHarnessError::new(
                    AgentHarnessErrorCode::Busy,
                    "compact() requires idle harness",
                ));
            }
            *phase = AgentHarnessPhase::Compaction;
        }
        let result = self.compact_inner().await;
        *self.shared.phase.lock().unwrap() = AgentHarnessPhase::Idle;
        result
    }

    async fn compact_inner(&self) -> Result<CompactionOutcome, AgentHarnessError> {
        let branch = v4_branch_entries(&self.shared.session).map_err(|e| {
            AgentHarnessError::new(AgentHarnessErrorCode::Session, e.message.clone())
        })?;
        let preparation = compaction_v4::prepare_compaction(&branch, &DEFAULT_COMPACTION_SETTINGS)
            .map_err(|e| AgentHarnessError::new(AgentHarnessErrorCode::Compaction, e.message))?;
        let Some(preparation) = preparation else {
            return Err(AgentHarnessError::new(
                AgentHarnessErrorCode::Compaction,
                "Nothing to compact",
            ));
        };
        // LLM summary generation deferred (stub): use a placeholder. Pi calls
        // `compact(...)` here to produce the summary text via `models.completeSimple`.
        let summary = "[summary generation deferred]".to_string();
        let entry = ProvisionedEntry::Compaction(ProvisionedCompactionEntry {
            id: self.shared.session.new_id(),
            summary: summary.clone(),
            retained_tail: preparation.retained_tail.clone(),
            tokens_before: preparation.tokens_before,
            details: None,
            usage: None,
        });
        let entry_id = self
            .shared
            .session
            .append_entry(&entry, "main")
            .map(|e| e.id().to_string())
            .map_err(|e| {
                AgentHarnessError::new(AgentHarnessErrorCode::Session, e.message.clone())
            })?;
        self.shared
            .emit(HarnessEvent::SessionCompact {
                compaction_entry_id: entry_id,
                from_hook: false,
            })
            .await;
        Ok(CompactionOutcome {
            summary,
            tokens_before: preparation.tokens_before,
            retained_tail: preparation.retained_tail.clone(),
        })
    }

    /// Leaf-repointing (§4.4, agent-harness.ts:732-827). Preserved even though
    /// branch/fork is not exercised by the feat-003 acceptance test: repoints the
    /// current leaf to `target_id` via [`session::Session::move_to`] and emits
    /// `session_tree`. Branch-summary generation is deferred (LLM).
    pub async fn navigate_tree(&self, target_id: &str) -> Result<(), AgentHarnessError> {
        {
            let mut phase = self.shared.phase.lock().unwrap();
            if *phase != AgentHarnessPhase::Idle {
                return Err(AgentHarnessError::new(
                    AgentHarnessErrorCode::Busy,
                    "navigateTree() requires idle harness",
                ));
            }
            *phase = AgentHarnessPhase::BranchSummary;
        }
        let result = self.navigate_tree_inner(target_id).await;
        *self.shared.phase.lock().unwrap() = AgentHarnessPhase::Idle;
        result
    }

    async fn navigate_tree_inner(&self, target_id: &str) -> Result<(), AgentHarnessError> {
        let old_leaf_id = self.shared.session.get_leaf_id().map_err(|e| {
            AgentHarnessError::new(AgentHarnessErrorCode::Session, e.message.clone())
        })?;
        if old_leaf_id.as_deref() == Some(target_id) {
            return Ok(());
        }
        self.shared
            .session
            .move_lane("main", Some(target_id))
            .map_err(|e| {
                AgentHarnessError::new(AgentHarnessErrorCode::BranchSummary, e.message.clone())
            })?;
        let new_leaf_id = self.shared.session.get_leaf_id().map_err(|e| {
            AgentHarnessError::new(AgentHarnessErrorCode::Session, e.message.clone())
        })?;
        self.shared
            .emit(HarnessEvent::SessionTree {
                new_leaf_id,
                old_leaf_id,
            })
            .await;
        Ok(())
    }
}

/// The result of a [`AgentHarness::compact`] run — mirrors the v4 oracle's
/// `CompactResult` (compaction.ts:99-109) minus the deferred `usage`/`details`.
#[derive(Debug, Clone, PartialEq)]
pub struct CompactionOutcome {
    /// The summary text (a deferred placeholder in this port).
    pub summary: String,
    /// The estimated context tokens before compaction.
    pub tokens_before: i64,
    /// Recent messages retained after compaction, stored on the compaction entry
    /// (`retainedTail`).
    pub retained_tail: Vec<AgentMessage>,
}

/// Build a `user` prompt message (agent-harness.ts:37-41). `pub` so callers
/// (feat-012 RPC `steer`/`follow_up`) can build the same shape the harness
/// itself uses for `prompt`.
pub fn user_message(text: &str) -> AgentMessage {
    AgentMessage::Llm(Message::User(UserMessage {
        role: UserRole::User,
        content: UserMessageContent::Blocks(vec![UserContent::Text(TextContent::new(text))]),
        timestamp: now_millis(),
    }))
}

fn now_millis() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Fetch the v4 main-lane branch entries, leaf-first (the mutation-log tree
/// walks leaf → root). `buildSessionContext` needs root-first, so reverse.
fn v4_branch_entries<St: V4SessionStorage + Send + Sync + 'static>(
    session: &V4Session<St>,
) -> Result<Vec<Entry>, SessionError> {
    let mut entries = session.find_entries_on_branch(
        &EntryQuery {
            order: Some(EntryOrder::NewestFirst),
            ..EntryQuery::default()
        },
        &Default::default(),
    )?;
    entries.reverse();
    Ok(entries)
}

/// `buildSessionContext` over the v4 main-lane branch (0.84.2 oracle).
fn build_v4_context<St: V4SessionStorage + Send + Sync + 'static>(
    session: &V4Session<St>,
) -> Result<crate::harness::session::v4::context::SessionContext, SessionError> {
    let entries = v4_branch_entries(session)?;
    Ok(crate::harness::session::v4::context::build_session_context(
        &entries,
        &Default::default(),
    ))
}

/// The context messages for the harness's current branch (0.84.2 oracle).
fn build_v4_context_messages<St: V4SessionStorage + Send + Sync + 'static>(
    session: &V4Session<St>,
) -> Result<Vec<AgentMessage>, SessionError> {
    Ok(build_v4_context(session)?.messages)
}
