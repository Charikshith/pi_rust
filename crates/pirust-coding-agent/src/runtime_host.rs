//! `main.rs`'s wiring glue: adapters that satisfy [`print_mode`], [`session`] and
//! [`migrations`]' console/runtime seams using the pieces `main.rs` actually builds
//! (an [`OutputGuard`], a [`pirust_agent_core::agent::Agent`], a [`SessionManager`]).
//!
//! None of this exists in `core/agent-session.ts` as a separate concept — Pi's real
//! `AgentSession` (3283 lines) *is* both the event-bus and the console sink, for the
//! interactive TUI as much as for print mode. `sdk.rs` deliberately does not build that
//! object (see its module docs), so this module supplies the narrow slice print mode
//! actually calls, scoped to one headless turn.
//!
//! # What is out of scope here (named, not silently dropped)
//!
//! - **`bind_extensions`** (Wave 6) builds the extension runner from the built-ins,
//!   binds real action closures (`getActiveTools`/`setActiveTools` → the agent's
//!   tool list, `appendEntry` → `SessionManager.append_custom_entry`), forwards
//!   agent events, installs the agent-loop hooks (`transform_context`/`before_tool_call`/
//!   `after_tool_call`), and emits `session_start`. The six `commandContextActions`
//!   closures in the binding are **not** wired to extension commands (no extension
//!   command invokes them this wave; `new_session`/`fork`/`switch_session`/`navigate_tree`/
//!   `reload` each return a harmless default).
//! - **Session persistence is message-level, not event-level.** Pi's real
//!   `AgentSession` persists through per-event hooks
//!   (`_installAgentToolHooks`/`_installAgentNextTurnRefresh`) as the loop runs, so a
//!   session file can reflect a turn that is still streaming. Here, [`SingleTurnSession`]
//!   diffs [`Agent::messages`] once per completed `prompt()` call (after
//!   [`Agent::wait_for_idle`]) and appends whatever is new. For a one-shot `-p`/`--json`
//!   run driven by `print_mode.rs` (which itself only calls `session.prompt()` in a
//!   sequential loop, never mid-stream), the on-disk result is the same messages in the
//!   same order — only the *timing* of when they hit disk narrows (end-of-turn, not
//!   mid-turn). Harden to event-level if a crash-mid-turn resume scenario is exercised.
//! - **`set_rebind_session`'s callback is stored and never invoked** — nothing in this
//!   wave swaps the session under the runtime (no `/fork`, no extension `new_session`),
//!   so the callback is dead by construction, not by omission.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use futures::future::BoxFuture;
use pirust_agent_core::agent::Agent;
use pirust_agent_core::harness::messages::AgentMessage;
use pirust_agent_core::harness::types::SessionHeader;
use pirust_agent_core::types::{
    AfterToolCallContext, AfterToolCallResult, AgentEvent, AgentTool, BeforeToolCallContext,
    BeforeToolCallResult,
};
use pirust_extension_api::events::ExtensionEvent;
use pirust_extension_api::loader::built_in_extensions;
use pirust_extension_api::runner::ExtensionRunner;
use pirust_extension_api::runtime::ExtensionRuntime;
use serde_json::Value;

use crate::print_mode::{
    AgentSessionRuntimeHost, Cancelled, ExtensionBinding, NavigateTreeOptions, PrintModeSession,
    RebindSessionFn, SessionEventListener, SessionStateView, Subscription, ThrownValue,
    ToolApprovalDecider, ToolApprovalDecision, ToolApprovalRequest,
};
use crate::session::SessionManager;

/// The slice of `AgentSession` print mode touches, backed by a real [`Agent`] +
/// [`SessionManager`] — see the module docs for what is deliberately not modeled.
pub struct SingleTurnSession {
    agent: Agent,
    session_manager: Arc<Mutex<SessionManager>>,
    /// How many of `agent.messages()` have already been appended to the session file —
    /// the diff point for the message-level persistence the module docs describe.
    persisted: AtomicUsize,
    /// The listener `subscribe()` registered, kept so `prompt()` can also emit the
    /// synthetic `AgentSettled` event `to_session_event` cannot produce (see its own
    /// docs — `AgentSettled` has no `AgentEvent` counterpart; `AgentSession` synthesizes
    /// it itself, once per prompt, after the last `agent_end`).
    listener: Mutex<Option<SessionEventListener>>,
    /// The full tool registry (`create_all_tools` output) — Pi's `_toolRegistry`
    /// (agent-session.ts:2540-2570): `setActiveToolsByName` filters through it.
    tool_registry: HashMap<String, Arc<dyn AgentTool>>,
    /// The extension runner, bound by `bind_extensions` (Wave 6). `None` until
    /// the first bind — the `bindCore`-less pre-bind state (Pi asserts on
    /// `runner.assertActive()`).
    extension_runner: Mutex<Option<Arc<Mutex<ExtensionRunner>>>>,
    /// Whether the extension event listener is already registered on the agent
    /// (bind may be called once; a second bind re-binds the runtime only).
    extension_listener_registered: AtomicBool,
    /// The interactive layer's tool-approval decider (`set_tool_approval_decider`).
    /// None = always allow. `before_tool_call` consults it; a non-allow decision
    /// blocks the tool with a user-visible reason.
    tool_approval_decider: Arc<Mutex<Option<ToolApprovalDecider>>>,
}

impl SingleTurnSession {
    pub fn new(
        agent: Agent,
        session_manager: SessionManager,
        tool_registry: Vec<(pirust_tools::ToolName, pirust_tools::Tool)>,
    ) -> Arc<Self> {
        let tool_registry = tool_registry
            .into_iter()
            .map(|(name, tool)| (name.as_str().to_string(), tool))
            .collect::<HashMap<_, _>>();
        let this = Arc::new(Self {
            agent,
            session_manager: Arc::new(Mutex::new(session_manager)),
            persisted: AtomicUsize::new(0),
            listener: Mutex::new(None),
            tool_registry,
            extension_runner: Mutex::new(None),
            extension_listener_registered: AtomicBool::new(false),
            tool_approval_decider: Arc::new(Mutex::new(None)),
        });
        this.install_tool_approval_hook();
        this
    }

    /// Append every message the last `prompt()`/`wait_for_idle()` produced that has not
    /// already been persisted (module docs: message-level, end-of-turn).
    fn persist_new_messages(&self) {
        let messages = self.agent.messages();
        let already = self.persisted.load(Ordering::SeqCst);
        if messages.len() <= already {
            return;
        }
        let mut manager = self
            .session_manager
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        for message in &messages[already..] {
            // A write failure here has no Pi analogue to fall back to (session-manager.ts's
            // own appends are unchecked `fs` calls that would throw); logging and moving on
            // keeps a one-shot run from losing the assistant's actual answer over a disk error.
            if let Err(error) = manager.append_message(message) {
                eprintln!("Warning: failed to persist session entry: {error}");
            }
        }
        self.persisted.store(messages.len(), Ordering::SeqCst);
    }

    /// Install the agent-loop `before_tool_call` hook that consults the
    /// interactive layer's approval decider (`tool_approval_decider`). When no
    /// decider is registered the hook passes every tool through, preserving
    /// the default allow behaviour.
    fn install_tool_approval_hook(&self) {
        let decider = Arc::clone(&self.tool_approval_decider);
        self.agent
            .set_before_tool_call(Some(Arc::new(move |ctx, _token| {
                let decider = Arc::clone(&decider);
                Box::pin(async move {
                    let request = ToolApprovalRequest {
                        tool_name: ctx.tool_call.name.clone(),
                        args: ctx.args.clone(),
                    };
                    // Clone the decider out of the lock so the guard is dropped
                    // before the await (the future must stay Send).
                    let decider = decider.lock().unwrap_or_else(|e| e.into_inner()).clone();
                    let decision = match decider {
                        Some(d) => d(request).await,
                        None => ToolApprovalDecision::RunOnce,
                    };
                    match decision {
                        ToolApprovalDecision::RunOnce | ToolApprovalDecision::AlwaysAllow => None,
                        ToolApprovalDecision::Deny => {
                            Some(pirust_agent_core::types::BeforeToolCallResult {
                                block: Some(true),
                                reason: Some("Tool execution was denied by the user".to_string()),
                            })
                        }
                    }
                })
            })));
    }

    /// `session.bindExtensions(bindings)` (agent-session.ts:2330-2354) — build
    /// the extension runner, bind the real action runtime (Pi's `bindCore`,
    /// agent-session.ts:2458-2520), forward agent events to extensions, and
    /// install the agent-loop hooks (Pi's `_installAgentToolHooks`,
    /// agent-session.ts:481-537).
    fn bind_extension_runner(&self, binding: &ExtensionBinding) {
        let mode = match binding.mode {
            crate::print_mode::ExtensionBindMode::Print => {
                pirust_extension_api::context::ExtensionMode::Print
            }
            crate::print_mode::ExtensionBindMode::Json => {
                pirust_extension_api::context::ExtensionMode::Json
            }
            crate::print_mode::ExtensionBindMode::Tui => {
                pirust_extension_api::context::ExtensionMode::Tui
            }
        };
        let cwd = std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| ".".into());

        // Create the shared runtime Arc FIRST, load extensions against it, then
        // build the runner over the same Arc — so the extension closures captured
        // at factory time reference the slots `bind_runtime` mutates (Pi: the
        // runner's single `runtime` object).
        let runtime_arc: Arc<ExtensionRuntime> = Arc::new(ExtensionRuntime::noop());
        let builtins = built_in_extensions()
            .iter()
            .map(|factory| {
                pirust_extension_api::loader::load_with_runtime(factory, &cwd, &runtime_arc)
            })
            .collect::<Vec<_>>();
        let mut runner = ExtensionRunner::new_with_runtime(builtins, cwd, mode, runtime_arc);

        // Build the real runtime actions (`bindCore`, agent-session.ts:2458-2520).
        let agent = self.agent.clone();
        let session_manager = Arc::clone(&self.session_manager);
        let tool_registry = Arc::new(
            self.tool_registry
                .iter()
                .map(|(name, tool)| (name.clone(), Arc::clone(tool)))
                .collect::<HashMap<_, _>>(),
        );

        let runtime = ExtensionRuntime {
            // `getActiveTools: () => this.getActiveToolNames()` (agent-session.ts:2494)
            // — names of the currently active tools (`agent.state.tools`).
            get_active_tools: Arc::new(Mutex::new(Box::new({
                let agent = agent.clone();
                move || agent.tool_names()
            }))),
            // `getAllTools: () => this.getAllTools()` (agent-session.ts:2495) —
            // names of every known tool (the full registry, Pi's `_toolDefinitions`).
            get_all_tools: Arc::new(Mutex::new(Box::new({
                let tool_registry = Arc::clone(&tool_registry);
                move || tool_registry.keys().cloned().collect()
            }))),
            // `setActiveTools: (toolNames) => this.setActiveToolsByName(toolNames)`
            // (agent-session.ts:2496; setActiveToolsByName :936-955) — filter
            // through the registry, set the agent's tools.
            set_active_tools: Arc::new(Mutex::new(Box::new({
                let agent = agent.clone();
                let tool_registry = Arc::clone(&tool_registry);
                move |tool_names: Vec<String>| {
                    let tools = tool_names
                        .iter()
                        .filter_map(|name| tool_registry.get(name).cloned())
                        .collect::<Vec<_>>();
                    agent.set_tools(&tools);
                }
            }))),
            // `appendEntry: (customType, data) => this.sessionManager.appendCustomEntry(...)`
            // (agent-session.ts:2478-2483) — the coding-agent `SessionManager`'s sync
            // append (session.rs:2083), mirroring Pi's sync `appendCustomEntry`.
            append_entry: Arc::new(Mutex::new(Box::new({
                let session_manager = session_manager.clone();
                move |custom_type: String, data: Option<Value>| {
                    let mut manager = session_manager.lock().unwrap_or_else(|e| e.into_inner());
                    if let Err(error) = manager.append_custom_entry(&custom_type, data) {
                        eprintln!("Warning: failed to append custom entry: {error}");
                    }
                }
            }))),
            // `sendMessage` / `sendUserMessage` (agent-session.ts:2467-2477) — queue a
            // custom/user message. `Agent` has no queueing for custom messages this
            // wave (print/interactive single-turn sessions have no follow-up UI); a
            // custom message is dropped with a warning, matching the pre-bind no-op.
            send_message: Arc::new(Mutex::new(Box::new(|_, _| {
                eprintln!("Warning: extension sendMessage is not supported in single-turn mode");
            }))),
            send_user_message: Arc::new(Mutex::new(Box::new(|_, _| {
                eprintln!(
                    "Warning: extension sendUserMessage is not supported in single-turn mode"
                );
            }))),
        };
        runner.bind_runtime(runtime);

        // Store the runner, then forward agent events + install hooks.
        let shared = Arc::new(Mutex::new(runner));
        *self.extension_runner.lock().unwrap() = Some(Arc::clone(&shared));

        self.install_extension_hooks(&shared);
        self.forward_extension_events(&shared);

        // `await this._extensionRunner.emit(this._sessionStartEvent)`
        // (agent-session.ts:2351) — the session_start event fires once per bind
        // (reason: "startup" for a fresh run). Plan-mode's session_start handler
        // reads the `plan` flag and restores persisted state here.
        shared.lock().unwrap().emit(&ExtensionEvent::SessionStart {
            reason: pirust_extension_api::events::SessionStartReason::Startup,
            previous_session_file: None,
        });
    }

    /// Test seam: the bound extension runner (None before `bind_extensions`).
    pub fn extension_runner_for_test(&self) -> Option<Arc<Mutex<ExtensionRunner>>> {
        self.extension_runner.lock().unwrap().clone()
    }

    /// Test seam: a command context with `has_ui: true` (matches the plan-mode
    /// tests' `tui_command_context`).
    pub fn tui_command_context_for_test(
        &self,
    ) -> pirust_extension_api::context::ExtensionCommandContext {
        use pirust_extension_api::context::ExtensionContext;
        pirust_extension_api::context::ExtensionCommandContext {
            base: ExtensionContext {
                mode: pirust_extension_api::context::ExtensionMode::Tui,
                has_ui: true,
                cwd: "/proj".into(),
                is_idle: Box::new(|| true),
                signal: None,
                abort: Box::new(|| {}),
                has_pending_messages: Box::new(|| false),
                shutdown: Box::new(|| {}),
                get_context_usage: Box::new(|| None),
                get_system_prompt: Box::new(String::new),
            },
            wait_for_idle: Box::new(|| {}),
            reload: Box::new(|| {}),
        }
    }

    /// Test seam: the session manager's non-header entries as JSON values.
    pub fn entries_for_test(&self) -> Vec<serde_json::Value> {
        let manager = self
            .session_manager
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        manager
            .get_entries()
            .into_iter()
            .map(|e| (*e).clone())
            .collect()
    }

    /// Forward `AgentEvent`s to the extension runner (`_emitExtensionEvent`,
    /// agent-session.ts:735-817). Registered once (before the UI listener) so
    /// extensions see events even with no UI subscribed.
    fn forward_extension_events(&self, runner: &Arc<Mutex<ExtensionRunner>>) {
        if self
            .extension_listener_registered
            .swap(true, Ordering::SeqCst)
        {
            return;
        }
        let runner = Arc::clone(runner);
        self.agent.subscribe(Arc::new(
            move |event: AgentEvent, _token: tokio_util::sync::CancellationToken| {
                let runner = Arc::clone(&runner);
                Box::pin(async move {
                    if let Some(ext) = to_extension_event(&event) {
                        runner.lock().unwrap().emit(&ext);
                    }
                }) as BoxFuture<'static, ()>
            },
        ));
    }

    /// Install the agent-loop hooks (`_installAgentToolHooks`, agent-session.ts:481-537):
    /// `transform_context` → `emit_context`, `before_tool_call` → `emit_tool_call`,
    /// `after_tool_call` → `emit_tool_result`.
    fn install_extension_hooks(&self, runner: &Arc<Mutex<ExtensionRunner>>) {
        let runner_1 = Arc::clone(runner);

        self.agent.set_transform_context(Some(Arc::new(
            move |messages: Vec<AgentMessage>, _token| {
                let runner = Arc::clone(&runner_1);
                Box::pin(async move {
                    let messages_value =
                        serde_json::to_value(&messages).unwrap_or(Value::Array(Vec::new()));
                    let filtered = runner.lock().unwrap().emit_context(&messages_value);
                    serde_json::from_value(filtered).unwrap_or(messages)
                }) as BoxFuture<'static, Vec<AgentMessage>>
            },
        )));

        let runner_2 = Arc::clone(runner);
        self.agent.set_before_tool_call(Some(Arc::new(
            move |ctx: BeforeToolCallContext, _token| {
                let runner = Arc::clone(&runner_2);
                Box::pin(async move {
                    let event = ExtensionEvent::ToolCall {
                        tool_call_id: ctx.tool_call.id.clone(),
                        tool_name: ctx.tool_call.name.clone(),
                        input: ctx.args.clone(),
                    };
                    match runner.lock().unwrap().emit_tool_call(&event) {
                        Some(result) if result.block => Some(BeforeToolCallResult {
                            block: Some(true),
                            reason: result.reason,
                        }),
                        Some(_) | None => None,
                    }
                }) as BoxFuture<'static, Option<BeforeToolCallResult>>
            },
        )));

        let runner_3 = Arc::clone(runner);
        self.agent
            .set_after_tool_call(Some(Arc::new(move |ctx: AfterToolCallContext, _token| {
                let runner = Arc::clone(&runner_3);
                Box::pin(async move {
                    let event = ExtensionEvent::ToolResult {
                        tool_call_id: ctx.tool_call.id.clone(),
                        tool_name: ctx.tool_call.name.clone(),
                        input: ctx.args.clone(),
                        content: serde_json::to_value(&ctx.result.content).unwrap_or(Value::Null),
                        is_error: ctx.is_error,
                    };
                    match runner.lock().unwrap().emit_tool_result(&event) {
                        Some(result) => Some(AfterToolCallResult {
                            content: result.content.and_then(|c| serde_json::from_value(c).ok()),
                            details: result.details,
                            is_error: result.is_error,
                            terminate: None,
                        }),
                        None => None,
                    }
                }) as BoxFuture<'static, Option<AfterToolCallResult>>
            })));
    }
}

#[async_trait::async_trait]
impl PrintModeSession for SingleTurnSession {
    fn header(&self) -> Option<SessionHeader> {
        let manager = self
            .session_manager
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let header: &Value = manager.get_header()?;
        serde_json::from_value(header.clone()).ok()
    }

    async fn bind_extensions(&self, binding: ExtensionBinding) -> Result<(), ThrownValue> {
        self.bind_extension_runner(&binding);
        Ok(())
    }

    fn subscribe(&self, listener: SessionEventListener) -> Subscription {
        // `AgentEvent` -> `AgentSessionEvent`, matching the widening `agent-session.ts`
        // documents (print_mode.rs's own docs on `AgentSessionEvent`): every loop event
        // passes through as `Value` payloads, `agent_end` is widened with `willRetry`.
        // `willRetry` is always `false` here: this wave's `print_mode.rs` calls
        // `session.prompt()` sequentially with no steering/follow-up queue in play, so
        // there is never a queued retry to report (see `sdk.rs`'s own deferral of
        // `blockImages`/session-restore for the same "no queue this wave" reason).
        *self.listener.lock().unwrap_or_else(|e| e.into_inner()) = Some(listener.clone());
        self.agent.subscribe(Arc::new(move |event, _token| {
            let mapped = to_session_event(event);
            listener(&mapped);
            Box::pin(async {})
        }));
        // Agent-core's `subscribe` has no unsubscribe handle (listeners live for the
        // Agent's lifetime); a one-shot headless run never needs to remove one, so the
        // returned `Subscription`'s thunk is a documented no-op, not a lost capability.
        Subscription::new(|| {})
    }

    async fn prompt(
        &self,
        text: &str,
        _options: Option<crate::print_mode::PromptOptions>,
    ) -> Result<(), ThrownValue> {
        // `_options.images` (the initial prompt's attachments) has no `Agent::prompt`
        // counterpart yet — `Agent::prompt` takes text only. Image attachments on the
        // initial message are consequently dropped this wave; text is not. Named here
        // rather than in `sdk.rs` because this is the call site that drops them.
        self.agent
            .prompt(text)
            .await
            .map_err(|error| ThrownValue::Error(error.to_string()))?;
        self.agent.wait_for_idle().await;
        self.persist_new_messages();
        // `agent-session.ts:563-564` — once no automatic retry/compaction/follow-up
        // remains. This wave's `print_mode.rs` calls `prompt()` sequentially with no
        // queue in play (see `subscribe`'s own note on `will_retry` always being
        // `false`), so idle here always means settled.
        if let Some(listener) = self
            .listener
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        {
            listener(&crate::print_mode::AgentSessionEvent::AgentSettled);
        }
        Ok(())
    }

    fn state(&self) -> SessionStateView {
        SessionStateView {
            messages: self.agent.messages(),
        }
    }

    async fn wait_for_idle(&self) {
        self.agent.wait_for_idle().await;
    }

    async fn navigate_tree(
        &self,
        _target_id: &str,
        _options: Option<NavigateTreeOptions>,
    ) -> Cancelled {
        // Unreachable this wave — see module docs.
        Cancelled { cancelled: false }
    }

    async fn reload(&self) {
        // Unreachable this wave — see module docs.
    }

    fn set_tool_approval_decider(&self, decider: ToolApprovalDecider) {
        *self
            .tool_approval_decider
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(decider);
    }
}

/// `AgentEvent` (agent-core's loop union) -> `AgentSessionEvent` (coding-agent's widened
/// union), losslessly re-serialized through `Value` — the same seam `print_mode.rs`
/// already assumes (see its module docs on why `AgentSessionEvent` cannot reuse
/// `AgentEvent` directly).
fn to_session_event(event: AgentEvent) -> crate::print_mode::AgentSessionEvent {
    use crate::print_mode::AgentSessionEvent as Out;
    let value = |m: &AgentMessage| serde_json::to_value(m).unwrap_or(Value::Null);
    match event {
        AgentEvent::AgentStart => Out::AgentStart,
        AgentEvent::AgentEnd { messages } => Out::AgentEnd {
            messages: messages.iter().map(value).collect(),
            will_retry: false,
        },
        AgentEvent::TurnStart => Out::TurnStart,
        AgentEvent::TurnEnd {
            message,
            tool_results,
        } => Out::TurnEnd {
            message: value(&message),
            tool_results: tool_results
                .iter()
                .map(|r| serde_json::to_value(r).unwrap_or(Value::Null))
                .collect(),
        },
        AgentEvent::MessageStart { message } => Out::MessageStart {
            message: value(&message),
        },
        AgentEvent::MessageUpdate {
            assistant_message_event,
            message,
        } => Out::MessageUpdate {
            assistant_message_event: serde_json::to_value(assistant_message_event)
                .unwrap_or(Value::Null),
            message: value(&message),
        },
        AgentEvent::MessageEnd { message } => Out::MessageEnd {
            message: value(&message),
        },
        AgentEvent::ToolExecutionStart {
            tool_call_id,
            tool_name,
            args,
        } => Out::ToolExecutionStart {
            tool_call_id,
            tool_name,
            args,
        },
        AgentEvent::ToolExecutionUpdate {
            tool_call_id,
            tool_name,
            args,
            partial_result,
        } => Out::ToolExecutionUpdate {
            tool_call_id,
            tool_name,
            args,
            partial_result,
        },
        AgentEvent::ToolExecutionEnd {
            tool_call_id,
            tool_name,
            result,
            is_error,
        } => Out::ToolExecutionEnd {
            tool_call_id,
            tool_name,
            result,
            is_error,
        },
    }
}

/// `AgentEvent` (agent-core's loop union) -> [`ExtensionEvent`] — the subset
/// `_emitExtensionEvent` (agent-session.ts:735-817) forwards to the extension
/// runner. Variants without an extension counterpart (`AgentStart` is emitted
/// by the runner's `emit_before_agent_start` path, not here) return `None`.
fn to_extension_event(event: &AgentEvent) -> Option<ExtensionEvent> {
    let value = |m: &AgentMessage| serde_json::to_value(m).unwrap_or(Value::Null);
    match event {
        AgentEvent::AgentStart => Some(ExtensionEvent::AgentStart),
        AgentEvent::AgentEnd { messages } => Some(ExtensionEvent::AgentEnd {
            messages: Value::Array(messages.iter().map(value).collect()),
        }),
        AgentEvent::TurnStart => Some(ExtensionEvent::TurnStart {
            turn_index: 0,
            timestamp: 0,
        }),
        AgentEvent::TurnEnd {
            message,
            tool_results,
        } => Some(ExtensionEvent::TurnEnd {
            turn_index: 0,
            message: value(message),
            tool_results: Value::Array(
                tool_results
                    .iter()
                    .map(|r| serde_json::to_value(r).unwrap_or(Value::Null))
                    .collect(),
            ),
        }),
        AgentEvent::MessageStart { message } => Some(ExtensionEvent::MessageStart {
            message: value(message),
        }),
        AgentEvent::MessageUpdate {
            assistant_message_event,
            message,
        } => Some(ExtensionEvent::MessageUpdate {
            message: value(message),
            assistant_message_event: serde_json::to_value(assistant_message_event)
                .unwrap_or(Value::Null),
        }),
        AgentEvent::MessageEnd { message } => Some(ExtensionEvent::MessageEnd {
            message: value(message),
        }),
        AgentEvent::ToolExecutionStart {
            tool_call_id,
            tool_name,
            args,
        } => Some(ExtensionEvent::ToolExecutionStart {
            tool_call_id: tool_call_id.clone(),
            tool_name: tool_name.clone(),
            args: args.clone(),
        }),
        AgentEvent::ToolExecutionUpdate {
            tool_call_id,
            tool_name,
            args,
            partial_result,
        } => Some(ExtensionEvent::ToolExecutionUpdate {
            tool_call_id: tool_call_id.clone(),
            tool_name: tool_name.clone(),
            args: args.clone(),
            partial_result: partial_result.clone(),
        }),
        AgentEvent::ToolExecutionEnd {
            tool_call_id,
            tool_name,
            result,
            is_error,
        } => Some(ExtensionEvent::ToolExecutionEnd {
            tool_call_id: tool_call_id.clone(),
            tool_name: tool_name.clone(),
            result: result.clone(),
            is_error: *is_error,
        }),
    }
}

/// Wraps a [`SingleTurnSession`] behind [`AgentSessionRuntimeHost`] — the two traits are
/// split in `print_mode.rs` because Pi's runtime can swap the session underneath a fixed
/// host; this wave's host never does, so `session()` always returns the same instance.
pub struct SingleTurnRuntimeHost {
    session: Arc<SingleTurnSession>,
}

impl SingleTurnRuntimeHost {
    pub fn new(session: Arc<SingleTurnSession>) -> Self {
        Self { session }
    }
}

impl AgentSessionRuntimeHost for SingleTurnRuntimeHost {
    fn session(&self) -> Arc<dyn PrintModeSession> {
        Arc::clone(&self.session) as Arc<dyn PrintModeSession>
    }

    fn set_rebind_session(&self, _rebind: RebindSessionFn) {
        // Never invoked this wave — see module docs.
    }

    fn dispose(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }

    fn new_session(&self, _options: Value) -> BoxFuture<'_, Value> {
        // Unreachable this wave — see module docs.
        Box::pin(async { Value::Null })
    }

    fn fork(&self, _entry_id: String, _options: Value) -> BoxFuture<'_, Cancelled> {
        // Unreachable this wave — see module docs.
        Box::pin(async { Cancelled { cancelled: false } })
    }

    fn switch_session(&self, _session_path: String, _options: Value) -> BoxFuture<'_, Value> {
        // Unreachable this wave — see module docs.
        Box::pin(async { Value::Null })
    }
}

/// `getMissingSessionCwdIssue` (`core/session-cwd.ts:14-33`) — the stored session's cwd no
/// longer exists on disk.
pub struct SessionCwdIssue {
    pub session_file: Option<String>,
    pub session_cwd: String,
    pub fallback_cwd: String,
}

/// `formatMissingSessionCwdError` (`core/session-cwd.ts:35-38`).
pub fn format_missing_session_cwd_error(issue: &SessionCwdIssue) -> String {
    let session_file = issue
        .session_file
        .as_deref()
        .map(|f| format!("\nSession file: {f}"))
        .unwrap_or_default();
    format!(
        "Stored session working directory does not exist: {}{session_file}\nCurrent working directory: {}",
        issue.session_cwd, issue.fallback_cwd
    )
}

/// `getMissingSessionCwdIssue(sessionManager, fallbackCwd)` (`core/session-cwd.ts:14-33`).
pub fn missing_session_cwd_issue(
    manager: &SessionManager,
    fallback_cwd: &str,
) -> Option<SessionCwdIssue> {
    let session_file = manager.get_session_file()?;
    let session_cwd = manager.get_cwd();
    if session_cwd.is_empty() || std::path::Path::new(session_cwd).exists() {
        return None;
    }
    Some(SessionCwdIssue {
        session_file: Some(session_file.to_string()),
        session_cwd: session_cwd.to_string(),
        fallback_cwd: fallback_cwd.to_string(),
    })
}
