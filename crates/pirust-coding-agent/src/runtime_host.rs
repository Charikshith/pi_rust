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
//! - **`bind_extensions`** is a no-op (`Ok(())`, ignoring the binding): there are no
//!   extensions to bind (feat-007), so the six `commandContextActions` closures it would
//!   otherwise wire up are never invoked by anything.
//! - **`new_session`/`fork`/`switch_session`/`navigate_tree`/`reload`** are consequently
//!   unreachable this wave: in Pi they are only ever invoked *by* an extension command
//!   through `commandContextActions`, and `bind_extensions` never hands them to one.
//!   Each returns a harmless default and is marked as such.
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

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use futures::future::BoxFuture;
use pirust_agent_core::agent::Agent;
use pirust_agent_core::harness::messages::AgentMessage;
use pirust_agent_core::harness::types::SessionHeader;
use pirust_agent_core::types::AgentEvent;
use serde_json::Value;

use crate::print_mode::{
    AgentSessionRuntimeHost, Cancelled, ExtensionBinding, NavigateTreeOptions, PrintModeSession,
    RebindSessionFn, SessionEventListener, SessionStateView, Subscription, ThrownValue,
};
use crate::session::SessionManager;

/// The slice of `AgentSession` print mode touches, backed by a real [`Agent`] +
/// [`SessionManager`] — see the module docs for what is deliberately not modeled.
pub struct SingleTurnSession {
    agent: Agent,
    session_manager: Mutex<SessionManager>,
    /// How many of `agent.messages()` have already been appended to the session file —
    /// the diff point for the message-level persistence the module docs describe.
    persisted: AtomicUsize,
    /// The listener `subscribe()` registered, kept so `prompt()` can also emit the
    /// synthetic `AgentSettled` event `to_session_event` cannot produce (see its own
    /// docs — `AgentSettled` has no `AgentEvent` counterpart; `AgentSession` synthesizes
    /// it itself, once per prompt, after the last `agent_end`).
    listener: Mutex<Option<SessionEventListener>>,
}

impl SingleTurnSession {
    pub fn new(agent: Agent, session_manager: SessionManager) -> Arc<Self> {
        Arc::new(Self {
            agent,
            session_manager: Mutex::new(session_manager),
            persisted: AtomicUsize::new(0),
            listener: Mutex::new(None),
        })
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

    async fn bind_extensions(&self, _binding: ExtensionBinding) -> Result<(), ThrownValue> {
        // No extensions to bind this wave (feat-007) — see module docs.
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
