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
#[cfg(feature = "wasm-extensions")]
use pirust_extension_api::registration::Extension;
use pirust_extension_api::runner::ExtensionRunner;
use pirust_extension_api::runtime::ExtensionRuntime;
use serde_json::Value;

/// feat-010 Wave 4: discover and load real `.wasm` extensions from
/// `<agent_dir>/extensions/*.wasm`, additive to the compile-time built-ins
/// `bind_extension_runner` already builds. Two resilience rules, both load-
/// bearing: a missing (or unreadable) extensions directory is not an error —
/// zero extensions found, startup proceeds silently, exactly like a user who
/// never created the folder; and a single bad `.wasm` file is logged (not
/// panicked on) and skipped, so one broken third-party extension can never
/// take the whole session down for everyone else's extensions.
///
/// Takes a `&ConfigEnv` rather than snapshotting the process environment
/// itself, matching this module's own established test convention (see
/// `config.rs`'s module doc: tests build a `ConfigEnv` literal instead of
/// calling `std::env::set_var`, which is process-global and races under
/// `cargo test`'s parallel threads) — this keeps the function itself
/// injectable/testable the same way.
/// Wave 5: `<name>.wasm.limits.json` next to `<name>.wasm`, e.g.
/// `{"fuel": 500000000, "max_memory_bytes": 33554432}`. Either field may be
/// omitted (falls back to `WasmExtensionLimits::default()`'s value — see
/// `wasm/mod.rs`). A sidecar's LIMITS come from the filesystem, not from the
/// extension's own `pi_activate` payload — a self-declared ceiling from an
/// untrusted guest is not a real sandbox control, only an operator-owned
/// file is.
#[cfg(feature = "wasm-extensions")]
fn load_extension_limits(
    wasm_path: &std::path::Path,
) -> pirust_extension_api::wasm::WasmExtensionLimits {
    let sidecar = wasm_path.with_extension("wasm.limits.json");
    let Ok(contents) = std::fs::read_to_string(&sidecar) else {
        return pirust_extension_api::wasm::WasmExtensionLimits::default();
    };
    match serde_json::from_str(&contents) {
        Ok(limits) => limits,
        Err(error) => {
            eprintln!(
                "Warning: ignoring malformed wasm extension limits sidecar {}: {error}",
                sidecar.display()
            );
            pirust_extension_api::wasm::WasmExtensionLimits::default()
        }
    }
}

#[cfg(feature = "wasm-extensions")]
fn discover_wasm_extensions(
    runtime: &Arc<ExtensionRuntime>,
    config: &crate::config::ConfigEnv,
) -> Vec<Extension> {
    use pirust_extension_api::wasm::WasmExtensionLoader;

    let Ok(agent_dir) = config.agent_dir() else {
        return Vec::new();
    };
    let extensions_dir = std::path::Path::new(&agent_dir).join("extensions");
    let Ok(entries) = std::fs::read_dir(&extensions_dir) else {
        return Vec::new(); // no extensions directory yet — not an error
    };

    let mut loaded = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("wasm") {
            continue;
        }
        let limits = load_extension_limits(&path);
        match WasmExtensionLoader::load_with_limits(&path, Arc::clone(runtime), limits) {
            Ok(extension) => loaded.push(extension),
            Err(error) => {
                eprintln!(
                    "Warning: failed to load wasm extension {}: {error}",
                    path.display()
                );
            }
        }
    }
    loaded
}

use crate::print_mode::{
    AgentSessionEvent, AgentSessionRuntimeHost, Cancelled, CompactionReason, ExtensionBinding,
    NavigateTreeOptions, PrintModeSession, RebindSessionFn, SessionEventListener, SessionStateView,
    Subscription, ThrownValue, ToolApprovalDecider, ToolApprovalDecision, ToolApprovalRequest,
    TuiRuntimeInfo, TuiRuntimeStatus,
};
use crate::session::{entries_to_agent_messages, SessionManager};
use pirust_agent_core::harness::compaction::v4::prepare_compaction_from_messages;
use pirust_agent_core::harness::compaction::DEFAULT_COMPACTION_SETTINGS;
use pirust_agent_core::harness::messages::create_compaction_summary_message;

/// The slice of `AgentSession` print mode touches, backed by a real [`Agent`] +
/// [`SessionManager`] — see the module docs for what is deliberately not modeled.
pub struct SingleTurnSession {
    agent: Agent,
    session_manager: Arc<Mutex<SessionManager>>,
    /// How many of `agent.messages()` have already been appended to the session file —
    /// the diff point for the message-level persistence the module docs describe.
    persisted: AtomicUsize,
    /// The listener the interactive layer currently wants events delivered to.
    /// Read by three places: `prompt()`'s synthetic `AgentSettled` emission
    /// (`AgentSettled` has no `AgentEvent` counterpart — see `to_session_event`'s
    /// own docs; `AgentSession` synthesizes it itself, once per prompt, after
    /// the last `agent_end`), `compact()`'s synthetic `CompactionStart`/
    /// `CompactionEnd` emission, and the single agent-event forwarder
    /// `subscribe()` installs (see `session_listener_installed` below).
    /// `Arc<Mutex<...>>`, not a bare `Mutex<...>`: the forwarder closure
    /// captured by `agent.subscribe` (agent.rs:341, `AgentListener: 'static`)
    /// cannot borrow `&self`, so it needs its own owned handle onto this same
    /// storage — cloning the `Arc` gives it that handle while every other
    /// reader keeps observing the identical slot, not a stale copy.
    listener: Arc<Mutex<Option<SessionEventListener>>>,
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
    /// Guards the one-time install of `subscribe()`'s agent-event forwarder —
    /// same shape and same underlying reason as `extension_listener_registered`
    /// immediately above. `Agent::subscribe` (agent-core/src/agent.rs:341) only
    /// ever *pushes* onto `listeners: Mutex<Vec<AgentListener>>` (agent.rs:267);
    /// there is no removal API (confirmed by grepping agent.rs for every mutator
    /// of that field — `push` at :341 is the only one). So a second
    /// `agent.subscribe(...)` call does not replace the first forwarder, it adds
    /// a second one that lives for the `Agent`'s lifetime. That is latent today
    /// because `print_mode.rs`'s `rebind_session` (`print_mode.rs:1139-1174`),
    /// the only caller of `SingleTurnSession::subscribe`, currently runs once
    /// at startup — but `rebind_session` is also the callback `/new`, `/fork`,
    /// `/clone` and in-place `/resume` are meant to fire, and it re-calls
    /// `session.subscribe()` on every rebind. Without this guard, each rebind
    /// on the same (never-swapped-this-wave, per the module docs) session would
    /// permanently stack one more forwarder — a session rebound twice would
    /// then deliver every event three times over `--json`. Install the
    /// forwarder once; every later `subscribe()` call only replaces `listener`'s
    /// contents, which the already-installed forwarder reads fresh per event.
    session_listener_installed: AtomicBool,
    /// The interactive layer's tool-approval decider (`set_tool_approval_decider`).
    /// None = always allow. `before_tool_call` consults it; a non-allow decision
    /// blocks the tool with a user-visible reason.
    tool_approval_decider: Arc<Mutex<Option<ToolApprovalDecider>>>,
    /// The `/model` picker's flattened model catalogue — every `Model` across every
    /// `ComposedProvider` the running `ModelRuntime` composed (`models.rs::ComposedProvider`),
    /// resolved against by [`TuiRuntimeInfo::set_model_by_name`] to turn a `provider`/`model_id`
    /// string pair (what the TUI's `ModelPicker` actually holds — `interactive_pickers.rs`'s
    /// `ModelEntry` is deliberately string-only) into a real `Model` it can hand to
    /// [`TuiRuntimeInfo::set_model`]. Empty until [`Self::set_model_catalog`] populates it.
    ///
    /// Populated by a setter, not a `SingleTurnSession::new` parameter: the `ModelRuntime` this
    /// is flattened from lives only in `main.rs`, and only in its interactive arm, built *after*
    /// `SingleTurnSession::new` already returns (`main.rs:526` constructs the session,
    /// `main.rs:540-541` builds the model-entry catalogue from `model_runtime.providers()` a few
    /// lines later, in a branch print mode never reaches). `SingleTurnSession::new`'s signature
    /// is otherwise fixed by every non-interactive caller — `tests/wave6_binding.rs` and this
    /// module's own `session_mutation_tests` construct it directly from just an `Agent` +
    /// `SessionManager` + tool registry, with no `ModelRuntime` in hand to satisfy a constructor
    /// parameter with. `Mutex<Vec<Model>>` over `OnceLock`: nothing here requires the catalogue
    /// be set exactly once, so a plain `Mutex` lets a later call simply replace it (last write
    /// wins) instead of panicking or silently failing on a hypothetical second call.
    model_catalog: Mutex<Vec<pirust_ai::types::Model>>,
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
            listener: Arc::new(Mutex::new(None)),
            tool_registry,
            extension_runner: Mutex::new(None),
            extension_listener_registered: AtomicBool::new(false),
            session_listener_installed: AtomicBool::new(false),
            tool_approval_decider: Arc::new(Mutex::new(None)),
            model_catalog: Mutex::new(Vec::new()),
        });
        this.install_tool_approval_hook();
        this
    }

    /// Populate [`Self::model_catalog`] — see that field's doc comment for why this is a
    /// post-construction setter rather than a `Self::new` parameter. `main.rs`'s interactive
    /// arm is the only intended caller, once, right after building both the session and the
    /// `ModelRuntime` it flattens from (`main.rs:526,540-541`); nothing here enforces that,
    /// a later call just replaces the previous catalogue wholesale.
    pub fn set_model_catalog(&self, models: Vec<pirust_ai::types::Model>) {
        let mut catalog = self.model_catalog.lock().unwrap_or_else(|e| e.into_inner());
        *catalog = models;
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

    /// `/compact` — the deterministic half of `AgentHarness::compact_inner`
    /// (`harness/mod.rs:681-723`), rehomed onto a flat `Agent` message list
    /// instead of a v4 session tree. See
    /// `PrintModeSession::compact`'s doc comment for why
    /// `prepare_compaction_from_messages` (not `prepare_compaction` directly)
    /// is the right call here.
    ///
    /// LLM summary generation is deferred exactly as it is in
    /// `AgentHarness::compact_inner` (`harness/mod.rs:693-695`) — this writes
    /// the same `"[summary generation deferred]"` placeholder, not a
    /// regression introduced here, a pre-existing limitation shared by every
    /// compaction path in the codebase.
    ///
    /// # Persistence and in-memory state, kept in lockstep
    ///
    /// 1. `persist_new_messages()` first, so every message in
    ///    `agent.messages()` has a corresponding on-disk `"message"` entry at
    ///    the same index — the invariant the id lookup below depends on.
    /// 2. The on-disk `"compaction"` entry's `firstKeptEntryId` is resolved
    ///    to the real id of the on-disk entry at
    ///    `messages.len() - retained_tail.len()` — the entry
    ///    `session.rs::build_context_entries` (session.rs:1295-1329) will
    ///    walk forward from when it later reconstructs context for this
    ///    session. An empty `retained_tail` uses `""`, a ID that can never
    ///    match a real entry, which that function's own documented "not
    ///    found = keep nothing before the marker" behaviour turns into
    ///    exactly the right answer (nothing from before compaction is kept).
    /// 3. `agent.set_messages(&[summary, ...retained_tail])` — this is what
    ///    makes `/compact` a *real* compaction and not a cosmetic notice:
    ///    `Agent::set_messages` (agent.rs:360) genuinely replaces the
    ///    in-memory history the next prompt will send, shrinking it exactly
    ///    like the RPC/harness path does.
    /// 4. `self.persisted` is reset to `new_messages.len()` — without this,
    ///    `persist_new_messages`'s `messages.len() <= already` guard
    ///    (`already` now stale and too large after the shrink) would never
    ///    pass again, permanently breaking persistence for every later
    ///    prompt in the session.
    async fn compact_inner(&self) -> Result<(), String> {
        self.persist_new_messages();
        let messages = self.agent.messages();

        let preparation = prepare_compaction_from_messages(&messages, &DEFAULT_COMPACTION_SETTINGS)
            .map_err(|error| error.to_string())?;
        let Some(preparation) = preparation else {
            return Err("Nothing to compact".to_string());
        };

        let tail_len = preparation.retained_tail.len();
        let cut_index = messages.len().saturating_sub(tail_len);

        let first_kept_entry_id = if tail_len == 0 {
            String::new()
        } else {
            let manager = self
                .session_manager
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let message_entries: Vec<&Value> = manager
                .get_entries()
                .into_iter()
                .filter(|entry| entry.get("type").and_then(Value::as_str) == Some("message"))
                .collect();
            let Some(entry) = message_entries.get(cut_index) else {
                return Err(
                    "Compaction failed: could not locate the retained-tail entry on disk"
                        .to_string(),
                );
            };
            let Some(id) = entry.get("id").and_then(Value::as_str) else {
                return Err("Compaction failed: retained-tail entry has no id".to_string());
            };
            id.to_string()
        };

        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let summary = "[summary generation deferred]".to_string();
        let summary_message = create_compaction_summary_message(
            summary.clone(),
            preparation.tokens_before,
            timestamp,
        );

        {
            let mut manager = self
                .session_manager
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            manager
                .append_compaction(
                    &summary,
                    &first_kept_entry_id,
                    preparation.tokens_before,
                    None,
                    Some(false),
                )
                .map_err(|error| format!("{error}"))?;
        }

        let mut new_messages = Vec::with_capacity(1 + tail_len);
        new_messages.push(AgentMessage::CompactionSummary(summary_message));
        new_messages.extend(preparation.retained_tail.iter().cloned());
        self.agent.set_messages(&new_messages);
        self.persisted.store(new_messages.len(), Ordering::SeqCst);

        Ok(())
    }

    /// Install the agent-loop `before_tool_call` hook that consults the
    /// interactive layer's approval decider (`tool_approval_decider`). When no
    /// decider is registered the hook passes every tool through, preserving
    /// the default allow behaviour.
    fn install_tool_approval_hook(&self) {
        let decider = Arc::clone(&self.tool_approval_decider);
        self.agent
            .set_before_tool_call(Some(Arc::new(move |ctx, token| {
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
                        // B2: race the decider against cancellation, so a
                        // Ctrl+C while a prompt is parked awaiting approval
                        // resolves as Deny instead of leaving the run stuck
                        // on a oneshot no one will ever answer.
                        Some(d) => match &token {
                            Some(t) => tokio::select! {
                                decision = d(request) => decision,
                                _ = t.cancelled() => ToolApprovalDecision::Deny,
                            },
                            None => d(request).await,
                        },
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
        #[allow(unused_mut)] // only mutated when the wasm-extensions feature is on
        let mut builtins = built_in_extensions()
            .iter()
            .map(|factory| {
                pirust_extension_api::loader::load_with_runtime(factory, &cwd, &runtime_arc)
            })
            .collect::<Vec<_>>();
        #[cfg(feature = "wasm-extensions")]
        builtins.extend(discover_wasm_extensions(
            &runtime_arc,
            &crate::config::ConfigEnv::from_process_env(),
        ));
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
            // `ctx.abort()` (Wave 5) — cancel the agent's current run, if any
            // (`Agent::abort`, agent.rs:437; agent.ts:310-312).
            abort: Arc::new(Mutex::new(Box::new({
                let agent = agent.clone();
                move || agent.abort()
            }))),
            // `ctx.shutdown()` (Wave 5) — this session's process IS the whole
            // pirust run (headless or interactive single-turn), so a
            // graceful shutdown request exits the process directly rather
            // than needing a separate quit flag threaded back in from
            // main.rs.
            shutdown: Arc::new(Mutex::new(Box::new(|| std::process::exit(0)))),
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
        let decider = Arc::clone(&self.tool_approval_decider);
        self.agent.set_before_tool_call(Some(Arc::new(
            move |ctx: BeforeToolCallContext, token| {
                let runner = Arc::clone(&runner_2);
                let decider = Arc::clone(&decider);
                Box::pin(async move {
                    let event = ExtensionEvent::ToolCall {
                        tool_call_id: ctx.tool_call.id.clone(),
                        tool_name: ctx.tool_call.name.clone(),
                        input: ctx.args.clone(),
                    };
                    if let Some(result) = runner.lock().unwrap().emit_tool_call(&event) {
                        if result.block {
                            return Some(BeforeToolCallResult {
                                block: Some(true),
                                reason: result.reason,
                            });
                        }
                    }
                    // `set_before_tool_call` is single-slot (B1): binding extensions
                    // used to fully replace `install_tool_approval_hook`'s hook, so
                    // the TUI's approval prompt never ran once extensions were bound.
                    // Consult the interactive layer's decider here too, after
                    // extensions had their say, so approval still gates every tool.
                    let decider = decider.lock().unwrap_or_else(|e| e.into_inner()).clone();
                    let decision = match decider {
                        // B2: race against cancellation so a Ctrl+C while
                        // parked on an approval prompt resolves to Deny
                        // instead of hanging forever on an unanswered oneshot.
                        Some(d) => {
                            let request = ToolApprovalRequest {
                                tool_name: ctx.tool_call.name.clone(),
                                args: ctx.args.clone(),
                            };
                            match &token {
                                Some(t) => tokio::select! {
                                    decision = d(request) => decision,
                                    _ = t.cancelled() => ToolApprovalDecision::Deny,
                                },
                                None => d(request).await,
                            }
                        }
                        None => ToolApprovalDecision::RunOnce,
                    };
                    match decision {
                        ToolApprovalDecision::RunOnce | ToolApprovalDecision::AlwaysAllow => None,
                        ToolApprovalDecision::Deny => Some(BeforeToolCallResult {
                            block: Some(true),
                            reason: Some("Tool execution was denied by the user".to_string()),
                        }),
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
        *self.listener.lock().unwrap_or_else(|e| e.into_inner()) = Some(listener);

        // Install the agent-event forwarder AT MOST ONCE — see
        // `session_listener_installed`'s doc comment for why re-installing on
        // every `subscribe()` call would be wrong (`Agent::subscribe` has no
        // removal API, so a second install would permanently double delivery,
        // a third would triple it, ...). Every `subscribe()` call, including
        // this one whether or not it is the first, only replaces `self.listener`
        // above; the forwarder installed below always re-reads that slot per
        // event, so it always delivers to whichever listener is current.
        if !self.session_listener_installed.swap(true, Ordering::SeqCst) {
            let slot = Arc::clone(&self.listener);
            self.agent.subscribe(Arc::new(move |event, _token| {
                // Clone the listener `Arc` out of the lock and drop the guard
                // *before* calling it, not while still holding it: this
                // forwarder runs inside the agent's own event dispatch
                // (agent.rs:812-813), and a listener that synchronously
                // triggered a rebind (-> a new `subscribe()` call -> a write
                // to this same `slot`) would deadlock against a guard still
                // held here.
                let current = slot.lock().unwrap_or_else(|e| e.into_inner()).clone();
                if let Some(listener) = current {
                    let mapped = to_session_event(event);
                    listener(&mapped);
                }
                Box::pin(async {})
            }));
        }

        // Unlike `forward_extension_events`'s forwarder (installed once and never
        // detached, because extensions have no notion of "the current listener"
        // to swap), this session has exactly one live listener at a time by
        // contract: `print_mode.rs`'s `rebind_session` (`print_mode.rs:1155-1159`)
        // always calls the previous `Subscription`'s `unsubscribe()` before
        // installing the next one via a fresh `subscribe()` call. So this thunk
        // clears `self.listener` back to `None`: once cleared, the single
        // permanently-installed forwarder above finds nothing to call until the
        // next `subscribe()` repopulates the slot. This does NOT remove the
        // agent-level forwarder itself — there still isn't a way to (see
        // `session_listener_installed`'s doc comment) — it only detaches the
        // *current* listener from it, which is all any caller here needs.
        let slot = Arc::clone(&self.listener);
        Subscription::new(move || {
            *slot.lock().unwrap_or_else(|e| e.into_inner()) = None;
        })
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

    fn abort(&self) {
        self.agent.abort();
    }

    fn set_tool_approval_decider(&self, decider: ToolApprovalDecider) {
        *self
            .tool_approval_decider
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(decider);
    }

    #[cfg(feature = "wasm-extensions")]
    fn reload_wasm_extensions(&self) -> Result<usize, String> {
        let guard = self
            .extension_runner
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let Some(runner_arc) = guard.as_ref() else {
            return Err("extensions have not been bound yet".to_string());
        };
        let mut runner = runner_arc.lock().unwrap_or_else(|e| e.into_inner());
        let runtime = runner.runtime();
        let config = crate::config::ConfigEnv::from_process_env();
        let discovered = discover_wasm_extensions(&runtime, &config);
        let newly_added = new_extensions_only(&runner.extensions, discovered);
        let count = newly_added.len();
        runner.extensions.extend(newly_added);
        Ok(count)
    }

    /// `/name <name>` — write the session label through the `SessionManager`
    /// this session already owns.
    ///
    /// Returns the *sanitized* name `append_session_info` actually wrote (it
    /// collapses each run of newlines to one space, then trims), not the raw
    /// argument, so the TUI can echo what really landed on disk.
    fn set_session_name(&self, name: &str) -> Result<String, String> {
        let mut manager = self
            .session_manager
            .lock()
            .map_err(|_| "session store is unavailable".to_string())?;
        manager
            .append_session_info(name)
            .map_err(|error| format!("{error}"))
    }

    /// `/compact` — emit `CompactionStart`/`CompactionEnd` (the contract
    /// `PrintModeSession::compact`'s doc comment describes) around
    /// [`Self::compact_inner`], mirroring `prompt()`'s own pattern of
    /// cloning the listener out of its lock and calling it directly for a
    /// synthetic event no `AgentEvent` produces.
    async fn compact(&self, reason: CompactionReason) -> Result<(), String> {
        let listener = self
            .listener
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(listener) = &listener {
            listener(&AgentSessionEvent::CompactionStart { reason });
        }
        let result = self.compact_inner().await;
        if let Some(listener) = &listener {
            let event = match &result {
                Ok(()) => AgentSessionEvent::CompactionEnd {
                    reason,
                    result: None,
                    aborted: false,
                    will_retry: false,
                    error_message: None,
                },
                Err(error) => AgentSessionEvent::CompactionEnd {
                    reason,
                    result: None,
                    aborted: true,
                    will_retry: false,
                    error_message: Some(error.clone()),
                },
            };
            listener(&event);
        }
        result
    }

    /// `/new` — wipe both the on-disk session and the live transcript back to
    /// empty. `SessionManager::new_session(None)` (`session.rs:2165`) resets
    /// `file_entries` to a fresh header, clears `by_id`/labels/`leaf_id`, and
    /// (for a persisting manager) computes a brand-new session-file path —
    /// i.e. this is a genuinely new file, not a truncation of the old one.
    ///
    /// `persisted` reasoning: after `new_session` the on-disk store holds
    /// zero `"message"` entries, and `agent.set_messages(&[])` makes
    /// `agent.messages()` empty too — both sides of `persist_new_messages`'s
    /// diff (`messages.len()` vs `persisted`) are 0, so resetting `persisted`
    /// to 0 is the only value that keeps them in lockstep. Anything else
    /// (e.g. leaving a stale nonzero count) would make the next `prompt()`'s
    /// `messages.len() <= already` guard (`runtime_host.rs:229`) permanently
    /// true for the first `already` new messages, silently dropping them
    /// from disk.
    fn start_new_session(&self) -> Result<(), String> {
        {
            let mut manager = self
                .session_manager
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            manager
                .new_session(None)
                .map_err(|error| format!("failed to start a new session: {error}"))?;
        } // guard dropped before touching `agent`, matching `fork_from`/
          // `switch_to_session_file` below — `new_session` never calls into
          // `agent` itself, but keeping the shape consistent avoids a future
          // edit accidentally growing a lock-held call into `agent`.
        self.agent.set_messages(&[]);
        self.persisted.store(0, Ordering::SeqCst);
        Ok(())
    }

    /// `/clone` — same conversation, new file. `SessionManager::
    /// create_branched_session(leaf_id)` (`session.rs:2775`) rewrites
    /// `file_entries` in place to a fresh header + the retained path up to
    /// (and including) `leaf_id`, re-parented, and — for a persisting
    /// manager — points `session_file` at a newly computed path. Passing the
    /// *current* leaf (`get_leaf_id`, `session.rs:2613`) means the branched
    /// path is the entire current conversation, not a truncation of it: the
    /// clone is a full copy, one entry-id-preserving relink away from the
    /// original file.
    ///
    /// Deliberately does not touch `agent` at all, matching the brief:
    /// "clone" duplicates the on-disk file, it does not rebind the live
    /// session to it (there is no live effect to undo if a caller decides
    /// not to switch to the clone). Because `agent`/`persisted` are
    /// untouched, there is nothing to reason about keeping them in
    /// lockstep with here — they already are, before and after.
    fn clone_session(&self) -> Result<Option<String>, String> {
        let mut manager = self
            .session_manager
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let Some(leaf) = manager.get_leaf_id().map(str::to_string) else {
            return Err("cannot clone a session with no messages yet".to_string());
        };
        manager
            .create_branched_session(&leaf)
            .map_err(|error| format!("failed to clone session: {error}"))
    }

    /// `/fork <entry_id>` — branch the store at `entry_id` (`SessionManager::
    /// create_branched_session`, `session.rs:2775`) *and* rewind the live
    /// transcript to match, unlike `clone_session` above. After branching,
    /// this same manager's `get_branch(None)` (`session.rs:2656`, default =
    /// current leaf) walks the *new* branched path root-first — the branch
    /// operation relinks the manager's own leaf to the fork point, so no
    /// separate `get_branch(Some(entry_id))` call is needed or more correct;
    /// they are the same path post-branch. `entries_to_agent_messages`
    /// (`session.rs:1595`) is infallible (it skips a malformed entry with an
    /// `eprintln!` warning rather than erroring), so once
    /// `create_branched_session` itself succeeds there is no
    /// half-completed state left to report — the rebuild below cannot fail
    /// separately from the branch.
    ///
    /// Lock discipline: the manager lock is taken once, used for both the
    /// branch mutation and the follow-up `get_branch` read (neither touches
    /// `agent`), then dropped at the end of this block — `agent.set_messages`
    /// and `self.persisted.store` below run with no lock held, so a
    /// reentrant call into this session from inside `Agent`'s machinery
    /// cannot deadlock against it.
    ///
    /// `persisted` reasoning: the rebuilt `messages` are exactly the
    /// messages `entries_to_agent_messages` derived from the *entries now on
    /// disk* for the branched path, so `messages.len()` is precisely how
    /// many of `agent.messages()` already have an on-disk counterpart —
    /// storing that into `persisted` is not an approximation, it is the
    /// literal count. Leaving `persisted` at its old (larger, pre-fork)
    /// value would make `persist_new_messages`'s `messages.len() <= already`
    /// guard true even though the new, shorter transcript has messages that
    /// were never appended to the *new* file, permanently losing them.
    fn fork_from(&self, entry_id: &str) -> Result<Option<String>, String> {
        let (path, messages) = {
            let mut manager = self
                .session_manager
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let path = manager
                .create_branched_session(entry_id)
                .map_err(|error| format!("failed to fork session: {error}"))?;
            let branch = manager.get_branch(None);
            (path, entries_to_agent_messages(&branch))
        }; // lock dropped here — see the doc comment above.
        let message_count = messages.len();
        self.agent.set_messages(&messages);
        self.persisted.store(message_count, Ordering::SeqCst);
        Ok(path)
    }

    /// `/import` and in-place `/resume` — both just mean "point this live
    /// session at a different file on disk," so both share this one seam.
    /// `SessionManager::set_session_file(path)` (`session.rs:2113`) resolves
    /// and loads `path`'s entries into this same manager, replacing
    /// `file_entries` in place; `get_branch(None)` (`session.rs:2656`) then
    /// walks the *newly loaded* leaf-to-root path, and
    /// `entries_to_agent_messages` (`session.rs:1595`) rebuilds it into
    /// `AgentMessage`s the same way `fork_from` above does.
    ///
    /// Known pre-existing wrinkle in `set_session_file` (out of scope to fix
    /// here — `session.rs` is not mine to edit this wave): on a bad-but-
    /// existing path it assigns `self.session_file` to the bad path and
    /// (via `load_entries_from_file` returning `Ok(Vec::new())` for
    /// unparseable content) clears `self.file_entries` *before* discovering
    /// the content is invalid and returning `Err(SessionError::
    /// NotAPiSession)` (`session.rs:2113-2131`). This wrapper cannot undo
    /// that corruption inside `SessionManager` itself, but it does stop it
    /// from spreading further: the `?` below returns immediately on that
    /// `Err`, before `get_branch`/`entries_to_agent_messages` run and before
    /// `agent`/`persisted` are touched at all — so `SingleTurnSession`'s own
    /// externally-visible state (the live transcript, `persisted`) stays
    /// exactly what it was before the failed switch, even though the
    /// underlying manager's buffer is left mid-transition.
    ///
    /// Lock discipline: identical shape to `fork_from` — one lock scope
    /// covers `set_session_file` + `get_branch` (neither touches `agent`),
    /// the guard drops before `agent.set_messages`/`self.persisted.store`
    /// run.
    ///
    /// `persisted` reasoning: identical to `fork_from` — `messages.len()`
    /// is exactly how many of the rebuilt `agent.messages()` came from
    /// entries that are already on disk (they were loaded FROM disk), so
    /// storing that count is not a guess, it is the true diff point for
    /// whatever the *next* `prompt()` appends on top of this newly loaded
    /// history.
    fn switch_to_session_file(&self, path: &str) -> Result<(), String> {
        let messages = {
            let mut manager = self
                .session_manager
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            manager
                .set_session_file(path)
                .map_err(|error| format!("failed to switch to session file {path}: {error}"))?;
            let branch = manager.get_branch(None);
            entries_to_agent_messages(&branch)
        }; // lock dropped here — see the doc comment above.
        let message_count = messages.len();
        self.agent.set_messages(&messages);
        self.persisted.store(message_count, Ordering::SeqCst);
        Ok(())
    }
}

/// Wave 5: the pure merge step behind `reload_wasm_extensions` — which
/// freshly `discover_wasm_extensions()`-ed extensions are actually new,
/// keyed by `resolved_path` against what the runner already has loaded.
/// Split out so this de-dup logic is unit-testable without spinning up a
/// full `SingleTurnSession` (which would need a real `agent_dir` env var —
/// this module's own established convention, see `config.rs`'s doc comment,
/// avoids process-global env vars in tests to not race under `cargo test`'s
/// parallel threads).
#[cfg(feature = "wasm-extensions")]
fn new_extensions_only(already_loaded: &[Extension], discovered: Vec<Extension>) -> Vec<Extension> {
    let existing: std::collections::HashSet<&str> = already_loaded
        .iter()
        .map(|ext| ext.resolved_path.as_str())
        .collect();
    discovered
        .into_iter()
        .filter(|ext| !existing.contains(ext.resolved_path.as_str()))
        .collect()
}

impl TuiRuntimeInfo for SingleTurnSession {
    fn runtime_status(&self) -> TuiRuntimeStatus {
        let model = self.agent.model();
        let thinking_level = self.agent.thinking_level();
        let tools_enabled = !self.agent.tool_names().is_empty();
        // Aggregate token usage + cost across the assistant messages in the
        // transcript (the session's current context).
        let mut context_tokens = 0u64;
        let mut cost = 0.0f64;
        for message in self.agent.messages() {
            if let AgentMessage::Llm(pirust_ai::types::Message::Assistant(assistant)) = &message {
                context_tokens = context_tokens
                    .saturating_add(assistant.usage.input)
                    .saturating_add(assistant.usage.output);
                cost += assistant.usage.cost.total;
            }
        }
        TuiRuntimeStatus {
            provider: model.provider.0.clone(),
            model: model.id.clone(),
            model_name: model.name.clone(),
            context_window: model.context_window,
            reasoning_supported: model.reasoning,
            thinking_level: crate::models::thinking_level_as_str(thinking_level).to_string(),
            context_tokens,
            cost,
            tools_enabled,
        }
    }

    /// The agent's currently-active tool names, for the first-run welcome
    /// block. `Agent::tool_names` (`agent.rs:390`) is the same source
    /// `runtime_status`'s `tools_enabled` already reduces to a boolean.
    fn tool_names(&self) -> Vec<String> {
        self.agent.tool_names()
    }

    /// Every resumable session on disk, newest first.
    ///
    /// `SessionEnv::list_all` (reached via `SessionManager::list_sessions`)
    /// already sorts by `modified` descending, so no re-sorting here. A store that
    /// cannot be read yields an empty list rather than an error: `/resume` with
    /// nothing to resume is a normal state, and the picker says so.
    ///
    /// The model column is left blank. `SessionInfo` carries no model — a
    /// session's model only exists as `model_change` entries inside its
    /// transcript, so filling this in would mean opening every session file on
    /// disk just to render a list. That trade is not worth it, and an empty
    /// column is honest.
    fn session_entries(&self) -> Vec<crate::interactive_pickers::SessionEntry> {
        let Ok(manager) = self.session_manager.lock() else {
            return Vec::new();
        };
        let Ok(infos) = manager.list_sessions() else {
            return Vec::new();
        };
        crate::interactive_pickers::load_session_entries(&infos, &std::collections::HashMap::new())
    }

    /// `/model` — see `TuiRuntimeInfo::set_model`'s doc comment
    /// (`print_mode.rs:725-750`, "Verified facts" in the task brief) for why
    /// `Agent::set_model` alone is sufficient: `build_stream_fn` (`sdk.rs:
    /// 246-314`) takes `model` as a call-time argument and dispatches on
    /// `model.api` per call, so there is no cached per-model provider
    /// adapter anywhere that would need rebuilding after a switch.
    /// `Agent::set_model` (`agent.rs:370`) mutates `AgentInner` in place —
    /// visible to every clone of this `Agent` immediately (module docs,
    /// "Verified facts").
    ///
    /// No `persisted` interaction, unlike the other four methods: this
    /// never touches `agent`'s message history, only `AgentInner`'s `model`
    /// field, so the message-level diff point `persisted` tracks
    /// (`runtime_host.rs:150`) is unaffected either way — there is nothing
    /// to keep in lockstep here.
    fn set_model(&self, model: &pirust_ai::types::Model) -> Result<(), String> {
        self.agent.set_model(model);
        Ok(())
    }

    /// `/model` (live switch, resolved from a `provider`/`model_id` string pair) — see
    /// `TuiRuntimeInfo::set_model_by_name`'s doc comment (`print_mode.rs:752-772`) for why this
    /// seam exists alongside [`Self::set_model`]. Resolves against [`Self::model_catalog`]
    /// (populated by [`Self::set_model_catalog`]; see that field's doc comment for why it is not
    /// a constructor argument), matching on `Model::provider.0` against `provider` — the same
    /// field `runtime_status` above already reduces to a `String` (`model.provider.0.clone()`,
    /// `runtime_host.rs:1108`) — because `models.rs::compose_model_provider` sets
    /// `provider: ProviderId(provider_id.to_string())` from the exact `provider_id` string
    /// `ComposedProvider::id` also holds (`models.rs:2677`), which is in turn what
    /// `interactive_pickers::load_model_entries` copies into `ModelEntry::provider`
    /// (`interactive_pickers.rs:150`) — the same string the TUI passes back in here. So
    /// `Model::provider.0` and `ComposedProvider::id` are the same value by construction; no
    /// separate lookup through `ComposedProvider` is needed.
    ///
    /// Two distinct failure messages, not one generic "not found": an empty catalogue (nothing
    /// was ever set via `set_model_catalog`) and an unmatched `provider`/`model_id` pair (a
    /// populated catalogue that just does not contain what was asked for) are different user
    /// problems — the first means this session was never wired to a `ModelRuntime` at all, the
    /// second means the specific model name was wrong or stale.
    ///
    /// The matched `&Model` is used directly out of the lock guard and never cloned:
    /// `Agent::set_model` takes `&Model` (`agent.rs:370`), so nothing here needs an owned copy.
    fn set_model_by_name(&self, provider: &str, model_id: &str) -> Result<(), String> {
        let catalog = self.model_catalog.lock().unwrap_or_else(|e| e.into_inner());
        if catalog.is_empty() {
            return Err(format!(
                "cannot switch to {provider}/{model_id}: this session has no model catalog \
                 (set_model_catalog was never called)"
            ));
        }
        let Some(model) = catalog
            .iter()
            .find(|model| model.provider.0 == provider && model.id == model_id)
        else {
            return Err(format!(
                "unknown model \"{provider}/{model_id}\": no match in this session's model catalog"
            ));
        };
        self.agent.set_model(model);
        Ok(())
    }

    /// `/tree` — see `TuiRuntimeInfo::branch_entries`'s doc comment (`print_mode.rs:774-785`).
    /// Same lock-then-project shape as [`Self::session_entries`] just above: `list_branches`
    /// (`session.rs:2756`) returns `Vec<BranchInfo<'_>>` borrowed from the `SessionManager`
    /// guard, so the borrow and the projection into owned `BranchEntry`s
    /// (`interactive_pickers::load_branch_entries`) both happen inside this one lock scope —
    /// nothing borrowed leaves it, only the owned `Vec<BranchEntry>` does. `list_branches`'s
    /// pre-order walk is passed through unmodified and unsorted, per that function's own
    /// contract (`interactive_pickers.rs:294-299`).
    ///
    /// A poisoned lock yields an empty list rather than panicking, matching
    /// `Self::session_entries`: `/tree` with nothing to show is a normal, honest state.
    fn branch_entries(&self) -> Vec<crate::interactive_pickers::BranchEntry> {
        let Ok(manager) = self.session_manager.lock() else {
            return Vec::new();
        };
        let branches = manager.list_branches();
        crate::interactive_pickers::load_branch_entries(&branches)
    }
}

/// `AgentEvent` (agent-core's loop union) -> `AgentSessionEvent` (coding-agent's widened
/// union), losslessly re-serialized through `Value` — the same seam `print_mode.rs`
/// already assumes (see its module docs on why `AgentSessionEvent` cannot reuse
/// `AgentEvent` directly).
pub(crate) fn to_session_event(event: AgentEvent) -> crate::print_mode::AgentSessionEvent {
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

/// feat-010 Wave 4: real, black-box tests for [`discover_wasm_extensions`].
/// In-module (not `tests/`) so a `ConfigEnv` literal can be built directly —
/// matching `config.rs`'s own established convention of never calling
/// `std::env::set_var` in tests (process-global, races under parallel test
/// threads) — without needing to make an internal helper `pub`.
#[cfg(all(test, feature = "wasm-extensions"))]
mod wasm_extension_discovery_tests {
    use super::*;
    use std::path::PathBuf;
    use std::process::Command;

    /// Builds the real `pirust-extension-api`'s `wasm-hello` fixture for
    /// `wasm32-unknown-unknown` (same fixture, same build invocation
    /// `pirust-extension-api/tests/wasm_extension_test.rs` already uses) and
    /// returns the compiled `.wasm` path.
    fn build_wasm_hello() -> PathBuf {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crates/ parent should exist")
            .join("pirust-extension-api")
            .join("examples")
            .join("wasm-hello");
        let manifest = dir.join("Cargo.toml");

        let status = Command::new(env!("CARGO"))
            .args([
                "build",
                "--manifest-path",
                manifest
                    .to_str()
                    .expect("manifest path should be valid UTF-8"),
                "--target",
                "wasm32-unknown-unknown",
                "--release",
            ])
            .status()
            .expect("failed to spawn cargo to build the wasm-hello fixture");
        assert!(status.success(), "building the wasm-hello fixture failed");

        dir.join("target")
            .join("wasm32-unknown-unknown")
            .join("release")
            .join("wasm_hello.wasm")
    }

    fn config_with_agent_dir(agent_dir: &std::path::Path) -> crate::config::ConfigEnv {
        crate::config::ConfigEnv {
            identity: crate::config::PIRUST,
            platform: crate::config::Platform::current(),
            home_dir: None,
            agent_dir_override: Some(agent_dir.display().to_string()),
        }
    }

    #[test]
    fn missing_extensions_directory_is_not_an_error() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // <tmp>/extensions does not exist at all.
        let config = config_with_agent_dir(tmp.path());
        let runtime = Arc::new(ExtensionRuntime::noop());
        assert!(discover_wasm_extensions(&runtime, &config).is_empty());
    }

    #[test]
    fn loads_a_real_wasm_extension_from_the_extensions_directory() {
        let wasm_hello = build_wasm_hello();
        let tmp = tempfile::tempdir().expect("tempdir");
        let extensions_dir = tmp.path().join("extensions");
        std::fs::create_dir_all(&extensions_dir).expect("create extensions dir");
        std::fs::copy(&wasm_hello, extensions_dir.join("wasm-hello.wasm"))
            .expect("copy wasm-hello fixture into place");

        let config = config_with_agent_dir(tmp.path());
        let runtime = Arc::new(ExtensionRuntime::noop());
        let loaded = discover_wasm_extensions(&runtime, &config);

        assert_eq!(loaded.len(), 1, "exactly one .wasm file was placed");
        let extension = &loaded[0];
        assert!(
            extension.tools.contains_key("echo"),
            "the real wasm-hello fixture registers an 'echo' tool"
        );

        // Prove it is genuinely callable, not just present in the map.
        let echo = &extension.tools["echo"];
        let ctx = pirust_extension_api::ExtensionContext {
            mode: pirust_extension_api::ExtensionMode::Print,
            has_ui: false,
            cwd: ".".to_string(),
            is_idle: Box::new(|| true),
            signal: None,
            abort: Box::new(|| {}),
            has_pending_messages: Box::new(|| false),
            shutdown: Box::new(|| {}),
            get_context_usage: Box::new(|| None),
            get_system_prompt: Box::new(String::new),
        };
        let params = serde_json::json!({"ping": "pong"});
        let result = (echo.definition.execute)(pirust_extension_api::ToolCallParams {
            tool_call_id: "t1",
            params: &params,
            ctx: &ctx,
        })
        .expect("echo tool should round-trip its input");
        assert_eq!(result, params);
    }

    #[test]
    fn a_bad_wasm_file_does_not_block_a_good_one_in_the_same_directory() {
        let wasm_hello = build_wasm_hello();
        let tmp = tempfile::tempdir().expect("tempdir");
        let extensions_dir = tmp.path().join("extensions");
        std::fs::create_dir_all(&extensions_dir).expect("create extensions dir");
        std::fs::copy(&wasm_hello, extensions_dir.join("good.wasm"))
            .expect("copy wasm-hello fixture into place");
        // Not valid wasm at all — must be skipped, not fatal.
        std::fs::write(
            extensions_dir.join("broken.wasm"),
            b"not a real wasm module",
        )
        .expect("write broken fixture");

        let config = config_with_agent_dir(tmp.path());
        let runtime = Arc::new(ExtensionRuntime::noop());
        let loaded = discover_wasm_extensions(&runtime, &config);

        assert_eq!(
            loaded.len(),
            1,
            "the broken file must be skipped, the good one must still load"
        );
        assert!(loaded[0].tools.contains_key("echo"));
    }

    /// Wave 5: a `<name>.wasm.limits.json` sidecar with a deliberately tiny
    /// fuel budget must actually take effect — proven by a plain `echo` call
    /// (which succeeds comfortably under the production default) now
    /// trapping. Without the sidecar being read, this would always succeed.
    #[test]
    fn sidecar_limits_file_overrides_the_default_fuel_budget() {
        let wasm_hello = build_wasm_hello();
        let tmp = tempfile::tempdir().expect("tempdir");
        let extensions_dir = tmp.path().join("extensions");
        std::fs::create_dir_all(&extensions_dir).expect("create extensions dir");
        let wasm_path = extensions_dir.join("wasm-hello.wasm");
        std::fs::copy(&wasm_hello, &wasm_path).expect("copy wasm-hello fixture into place");
        std::fs::write(
            extensions_dir.join("wasm-hello.wasm.limits.json"),
            br#"{"fuel": 2000000}"#,
        )
        .expect("write limits sidecar");

        let config = config_with_agent_dir(tmp.path());
        let runtime = Arc::new(ExtensionRuntime::noop());
        let loaded = discover_wasm_extensions(&runtime, &config);
        assert_eq!(
            loaded.len(),
            1,
            "2,000,000 fuel must be enough for pi_activate itself to succeed"
        );

        let echo = &loaded[0].tools["echo"];
        let ctx = pirust_extension_api::ExtensionContext {
            mode: pirust_extension_api::ExtensionMode::Print,
            has_ui: false,
            cwd: ".".to_string(),
            is_idle: Box::new(|| true),
            signal: None,
            abort: Box::new(|| {}),
            has_pending_messages: Box::new(|| false),
            shutdown: Box::new(|| {}),
            get_context_usage: Box::new(|| None),
            get_system_prompt: Box::new(String::new),
        };
        // The fuel budget is a per-instance LIFETIME budget (wasm/mod.rs's
        // doc comment) — repeat the call enough times that 2,000,000 units
        // must run out, where the production default (200,000,000) would
        // handle this many calls comfortably (see the leak-regression test
        // in pirust-extension-api's own test suite).
        let mut trapped = false;
        for _ in 0..2000 {
            let result = (echo.definition.execute)(pirust_extension_api::ToolCallParams {
                tool_call_id: "t2",
                params: &serde_json::json!({"ping": "pong"}),
                ctx: &ctx,
            });
            if result.is_err() {
                trapped = true;
                break;
            }
        }
        assert!(
            trapped,
            "a 2,000,000-unit fuel override must exhaust before 2000 echo calls do"
        );
    }

    /// Wave 5: `new_extensions_only` is the pure merge step behind
    /// `SingleTurnSession::reload_wasm_extensions` — proves a freshly
    /// discovered extension is treated as new against an empty "already
    /// loaded" set, and as already-present (filtered out) once it IS in
    /// that set, keyed by `resolved_path`.
    #[test]
    fn new_extensions_only_dedupes_by_resolved_path() {
        let wasm_hello = build_wasm_hello();
        let tmp = tempfile::tempdir().expect("tempdir");
        let extensions_dir = tmp.path().join("extensions");
        std::fs::create_dir_all(&extensions_dir).expect("create extensions dir");
        std::fs::copy(&wasm_hello, extensions_dir.join("wasm-hello.wasm"))
            .expect("copy wasm-hello fixture into place");
        let config = config_with_agent_dir(tmp.path());
        let runtime = Arc::new(ExtensionRuntime::noop());

        let first_discovery = discover_wasm_extensions(&runtime, &config);
        assert_eq!(first_discovery.len(), 1);

        // `Extension` holds `Box<dyn Fn>` closures (not `Clone`), so re-use
        // this discovery's own result as the "already loaded" set for the
        // second check instead of cloning it.
        let already_loaded = new_extensions_only(&[], first_discovery);
        assert_eq!(
            already_loaded.len(),
            1,
            "against an empty already-loaded set, the discovered extension is new"
        );

        let second_discovery = discover_wasm_extensions(&runtime, &config);
        let added_against_loaded = new_extensions_only(&already_loaded, second_discovery);
        assert_eq!(
            added_against_loaded.len(),
            0,
            "the same path, already loaded, must not be added again"
        );
    }
}

/// Real, black-box tests for the five session-mutation methods
/// (`start_new_session`, `clone_session`, `fork_from`,
/// `switch_to_session_file`, `set_model`). In-module rather than an
/// integration test under `tests/`, so private fields (`session_manager`,
/// `persisted`) can be inspected directly to prove the "kept in lockstep
/// with what's on disk" bookkeeping each doc comment above promises, not
/// just the externally-visible `Result`.
///
/// Scaffolding is modeled on `tests/tui_compact.rs`'s `make_session()` (the
/// established minimal-field `AgentOptions`/`AssistantMessage`/`UserMessage`
/// construction template in this crate) rather than reusing an in-file
/// helper — there was no pre-existing `#[cfg(test)]` module for
/// `SingleTurnSession` in this file before this one (only the unrelated,
/// `wasm-extensions`-gated `wasm_extension_discovery_tests` above existed).
#[cfg(test)]
mod session_mutation_tests {
    use super::*;
    use pirust_agent_core::agent::AgentOptions;
    use pirust_agent_core::types::ThinkingLevel;
    use pirust_ai::providers::faux::Faux;
    use pirust_ai::types::{
        AssistantContent, AssistantMessage, Message, TextContent, UserMessage, UserMessageContent,
        UserRole,
    };
    use std::path::Path;

    fn dummy_model() -> pirust_ai::types::Model {
        Faux::new().get_model().clone()
    }

    /// Same minimal-field construction `tui_compact.rs`'s test module uses.
    fn assistant_text_message(text: &str) -> AgentMessage {
        AgentMessage::Llm(Message::Assistant(AssistantMessage {
            role: Default::default(),
            content: vec![AssistantContent::Text(TextContent::new(text))],
            api: "anthropic-messages".into(),
            provider: "anthropic".into(),
            model: None,
            response_model: None,
            diagnostics: None,
            usage: pirust_ai::types::Usage {
                input: 0,
                output: 0,
                cache_read: 0,
                cache_write: 0,
                cache_write1h: None,
                reasoning: None,
                total_tokens: None,
                cost: pirust_ai::types::Cost {
                    input: 0.0,
                    output: 0.0,
                    cache_read: 0.0,
                    cache_write: 0.0,
                    total: 0.0,
                },
            },
            stop_reason: pirust_ai::types::StopReason::Stop,
            timestamp: 0,
            response_id: None,
            raw_stop_reason: None,
            error_message: None,
            end_turn: None,
        }))
    }

    fn user_text_message(text: &str) -> AgentMessage {
        AgentMessage::Llm(Message::User(UserMessage {
            role: UserRole::User,
            content: UserMessageContent::Text(text.to_string()),
            timestamp: 0,
        }))
    }

    fn build_agent(
        messages: Vec<AgentMessage>,
    ) -> (Agent, Vec<(pirust_tools::ToolName, pirust_tools::Tool)>) {
        let tool_registry = pirust_tools::create_all_tools("/proj", None);
        let agent = Agent::new(AgentOptions {
            system_prompt: "test".into(),
            model: dummy_model(),
            thinking_level: ThinkingLevel::Off,
            tools: tool_registry
                .iter()
                .map(|(_, t)| t.clone())
                .collect::<Vec<_>>(),
            messages,
            convert_to_llm: None,
            transform_context: None,
            stream_fn: None,
            get_api_key: None,
            before_tool_call: None,
            after_tool_call: None,
            steering_mode: pirust_agent_core::types::QueueMode::OneAtATime,
            follow_up_mode: pirust_agent_core::types::QueueMode::OneAtATime,
            session_id: None,
            tool_execution: pirust_agent_core::types::ToolExecutionMode::Parallel,
        });
        (agent, tool_registry)
    }

    /// An in-memory (non-persisting) session — for tests that never need a
    /// real file on disk (`start_new_session`, `set_model`).
    fn make_in_memory_session(messages: Vec<AgentMessage>) -> (Arc<SingleTurnSession>, Agent) {
        let (agent, tool_registry) = build_agent(messages);
        let env =
            crate::session::SessionEnv::new(crate::config::ConfigEnv::from_process_env(), "/proj");
        let manager = env.in_memory(Some("/proj"), None).unwrap();
        let session = SingleTurnSession::new(agent.clone(), manager, tool_registry);
        (session, agent)
    }

    /// A persisting session rooted under `dir` — needed for `fork_from` and
    /// `switch_to_session_file`, both of which operate on
    /// `SessionManager::session_file` (`create_branched_session`/
    /// `set_session_file`), which an in-memory (`persist: false`) manager
    /// never populates with a real path. Distinct `ConfigEnv` literal per
    /// call (own `agent_dir_override`), never `std::env::set_var` — this
    /// crate's established test convention (`config.rs`'s module doc; also
    /// followed by `wasm_extension_discovery_tests::config_with_agent_dir`
    /// above), since the process environment is global and would race under
    /// `cargo test`'s parallel threads.
    fn make_persisting_session(
        dir: &Path,
        messages: Vec<AgentMessage>,
    ) -> (Arc<SingleTurnSession>, Agent) {
        let (agent, tool_registry) = build_agent(messages);
        let config = crate::config::ConfigEnv {
            identity: crate::config::PIRUST,
            platform: crate::config::Platform::current(),
            home_dir: None,
            agent_dir_override: Some(dir.join("agent").display().to_string()),
        };
        let cwd = dir.join("project").display().to_string();
        std::fs::create_dir_all(&cwd).expect("mkdir project cwd");
        let env = crate::session::SessionEnv::new(config, cwd.clone());
        let manager = env
            .create(&cwd, None, None)
            .expect("create a persisting session manager");
        let session = SingleTurnSession::new(agent.clone(), manager, tool_registry);
        (session, agent)
    }

    #[test]
    fn start_new_session_empties_the_transcript_and_resets_persisted() {
        let seed = vec![user_text_message("hi"), assistant_text_message("hello")];
        let (session, agent) = make_in_memory_session(seed);
        session.persist_new_messages();
        assert_eq!(
            session.entries_for_test().len(),
            2,
            "sanity: both seeded messages were persisted before starting a new session"
        );

        session
            .start_new_session()
            .expect("starting a new session should succeed");

        assert_eq!(agent.messages().len(), 0, "live transcript was wiped");
        assert_eq!(
            session.persisted.load(Ordering::SeqCst),
            0,
            "persisted counter reset to match the now-empty transcript"
        );
        assert_eq!(
            session.entries_for_test().len(),
            0,
            "the new session has no message entries on disk"
        );
    }

    #[test]
    fn clone_session_creates_a_new_file_without_touching_the_live_transcript() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let seed = vec![user_text_message("hi"), assistant_text_message("hello")];
        let (session, agent) = make_persisting_session(tmp.path(), seed.clone());
        session.persist_new_messages();
        let original_file = {
            let manager = session
                .session_manager
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            manager.get_session_file().map(str::to_string)
        };

        let cloned_path = session
            .clone_session()
            .expect("cloning a session with messages should succeed");

        assert!(
            cloned_path.is_some(),
            "a persisting session's clone reports a new file path"
        );
        assert_ne!(
            cloned_path, original_file,
            "the clone must be a distinct file from the original"
        );
        assert_eq!(
            agent.messages(),
            seed,
            "clone_session must not touch the live transcript"
        );
    }

    #[test]
    fn fork_from_rebuilds_the_live_transcript_to_the_fork_point() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let seed = vec![
            user_text_message("first"),
            assistant_text_message("second"),
            user_text_message("third"),
        ];
        let (session, agent) = make_persisting_session(tmp.path(), seed.clone());
        session.persist_new_messages();
        let entries = session.entries_for_test();
        let message_entries: Vec<_> = entries
            .iter()
            .filter(|e| e.get("type").and_then(|t| t.as_str()) == Some("message"))
            .collect();
        assert_eq!(message_entries.len(), 3, "sanity: all 3 messages persisted");
        let fork_point = message_entries[0]
            .get("id")
            .and_then(|v| v.as_str())
            .expect("first message entry has an id")
            .to_string();

        let path = session
            .fork_from(&fork_point)
            .expect("forking at a real entry id should succeed");

        assert!(
            path.is_some(),
            "a persisting session's fork reports a new file path"
        );
        assert_eq!(
            agent.messages(),
            vec![seed[0].clone()],
            "the live transcript was rewound to exactly the forked-from message"
        );
        assert_eq!(
            session.persisted.load(Ordering::SeqCst),
            1,
            "persisted matches the 1 message now actually on disk in the branched file"
        );
    }

    #[test]
    fn switch_to_session_file_loads_a_different_files_messages() {
        let tmp_a = tempfile::tempdir().expect("tempdir a");
        let tmp_b = tempfile::tempdir().expect("tempdir b");

        let seed_a = vec![user_text_message("session a solo message")];
        let (session_a, agent_a) = make_persisting_session(tmp_a.path(), seed_a.clone());
        session_a.persist_new_messages();

        let seed_b = vec![
            user_text_message("session b first"),
            assistant_text_message("session b second"),
        ];
        let (session_b, _agent_b) = make_persisting_session(tmp_b.path(), seed_b.clone());
        // `seed_b` contains an assistant message, so `persist_entry` (session.rs:2292-2318)
        // actually flushes a real file to disk here — without an assistant message present
        // the entries stay buffered in memory only (see `switch_to_session_file`'s own doc
        // comment on this file for why that distinction matters for this test).
        session_b.persist_new_messages();
        let path_b = {
            let manager = session_b
                .session_manager
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            manager
                .get_session_file()
                .expect("a persisting session with an assistant message has a real file")
                .to_string()
        };

        session_a
            .switch_to_session_file(&path_b)
            .expect("switching to a real, valid session file should succeed");

        assert_eq!(
            agent_a.messages(),
            seed_b,
            "session a's live transcript now holds session b's messages"
        );
        assert_eq!(
            session_a.persisted.load(Ordering::SeqCst),
            2,
            "persisted matches the 2 messages loaded from session b's file"
        );
    }

    #[test]
    fn switch_to_session_file_on_a_bad_path_errors_without_corrupting_state() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let seed = vec![user_text_message("keep me")];
        let (session, agent) = make_persisting_session(tmp.path(), seed.clone());
        session.persist_new_messages();

        // An existing-but-invalid file: `load_entries_from_file` (session.rs:829-849)
        // silently skips every unparseable line and finds no valid header, so this
        // yields `Ok(Vec::new())`; combined with the file's nonzero size,
        // `set_session_file` (session.rs:2113-2131) returns
        // `Err(SessionError::NotAPiSession)`.
        let garbage_path = tmp.path().join("not-a-session.jsonl");
        std::fs::write(&garbage_path, b"this is not jsonl at all\nneither is this")
            .expect("write garbage fixture");

        let result = session.switch_to_session_file(
            garbage_path
                .to_str()
                .expect("tempdir path should be valid UTF-8"),
        );

        assert!(
            result.is_err(),
            "an existing-but-invalid session file must be reported as an error"
        );
        assert_eq!(
            agent.messages(),
            seed,
            "a failed switch must not touch the live transcript"
        );
        assert_eq!(
            session.persisted.load(Ordering::SeqCst),
            1,
            "a failed switch must not touch the persisted counter either"
        );
    }

    #[test]
    fn set_model_switches_the_live_agents_model() {
        let (session, agent) = make_in_memory_session(Vec::new());
        let mut new_model = dummy_model();
        new_model.id = format!("{}-but-different", new_model.id);

        session
            .set_model(&new_model)
            .expect("set_model always succeeds — see the doc comment on Self::set_model");

        assert_eq!(
            agent.model().id,
            new_model.id,
            "the live agent now reports the switched-to model"
        );
    }

    #[test]
    fn branch_entries_is_empty_for_a_fresh_session() {
        let (session, _agent) = make_in_memory_session(Vec::new());
        assert!(
            session.branch_entries().is_empty(),
            "a session with no entries has no branches to show"
        );
    }

    #[test]
    fn branch_entries_returns_entries_for_a_forked_session() {
        let (session, _agent) = make_in_memory_session(Vec::new());
        // `manager.branch` only moves the leaf pointer (session.rs:2799-2805) — unlike
        // `fork_from`/`create_branched_session`, it does not rewrite `file_entries`, so
        // appending after rewinding the leaf produces a genuine two-child fork in this
        // same manager, exactly what `list_branches` needs to flag `is_branch_point`.
        let root_id = {
            let mut manager = session
                .session_manager
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let root_id = manager
                .append_message(&user_text_message("root"))
                .expect("append root");
            manager
                .append_message(&assistant_text_message("child a"))
                .expect("append child a");
            manager.branch(&root_id).expect("rewind leaf back to root");
            manager
                .append_message(&assistant_text_message("child b"))
                .expect("append child b");
            root_id
        };

        let entries = session.branch_entries();

        assert_eq!(entries.len(), 3, "root + both children of the fork");
        let root_entry = entries
            .iter()
            .find(|entry| entry.id == root_id)
            .expect("root entry is present");
        assert!(
            root_entry.is_branch_point,
            "root has two children, so it is a branch point"
        );
        assert_eq!(root_entry.child_count, 2);
    }

    #[test]
    fn set_model_by_name_switches_to_a_cataloged_model() {
        let (session, agent) = make_in_memory_session(Vec::new());
        let original_model = dummy_model();
        let mut other_model = original_model.clone();
        other_model.id = format!("{}-catalog-target", original_model.id);
        session.set_model_catalog(vec![original_model.clone(), other_model.clone()]);

        session
            .set_model_by_name(&other_model.provider.0, &other_model.id)
            .expect("a provider/model pair present in the catalog should resolve");

        assert_eq!(
            agent.model().id,
            other_model.id,
            "the live agent now reports the switched-to model"
        );
        assert_eq!(
            session.runtime_status().model,
            other_model.id,
            "runtime_status reflects the switched-to model"
        );
    }

    #[test]
    fn set_model_by_name_distinguishes_unknown_model_from_empty_catalog() {
        let (session, _agent) = make_in_memory_session(Vec::new());

        let empty_catalog_err = session
            .set_model_by_name("some-provider", "some-model")
            .expect_err("an empty catalog must not report success");

        session.set_model_catalog(vec![dummy_model()]);
        let unknown_model_err = session
            .set_model_by_name("nonexistent-provider", "nonexistent-model")
            .expect_err("an unmatched provider/model pair must not report success");

        assert_ne!(
            empty_catalog_err, unknown_model_err,
            "an empty catalog and an unknown model are different problems and must be \
             reported differently"
        );
    }
}
