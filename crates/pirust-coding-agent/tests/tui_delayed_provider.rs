//! Delayed-provider black-box tests for the interactive TUI (feat-013).
//!
//! The audit (`docs/tui-design-audit.md` #12/#14) requires that acceptance not
//! be measured by component presence alone: submit, streaming, cancellation,
//! errors, and resize must be exercised through the *public interaction
//! contract* with a provider that responds on a delay, not via unit tests that
//! drive channels directly.
//!
//! `InteractiveMode` is `!Send` (Rc-based TUI), so each scenario runs on the
//! main test thread; a driver thread feeds keys through the captured
//! `on_input` callback. The `DelayedSession` stub emits a canned event stream
//! only after the caller's prompt has been observed, mirroring a slow
//! provider.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use pirust_agent_core::harness::types::SessionHeader;
use pirust_coding_agent::interactive_mode::InteractiveSession;
use pirust_coding_agent::print_mode::{
    AgentSessionEvent, Cancelled, CompactionReason, ExtensionBinding, NavigateTreeOptions,
    PrintModeSession, PromptOptions, SessionEventListener, SessionStateView, Subscription,
    ThrownValue, ToolApprovalDecider, ToolApprovalDecision, TuiRuntimeInfo, TuiRuntimeStatus,
};
use pirust_tui::terminal::Terminal;

type InputSlot = Arc<Mutex<Option<Box<dyn FnMut(&str) + Send>>>>;

/// A terminal that captures `on_input` so a test thread can drive keys.
struct DriveTerminal {
    input_slot: InputSlot,
    size: Arc<Mutex<(u16, u16)>>,
    writes: Arc<Mutex<String>>,
}

impl Terminal for DriveTerminal {
    fn start(
        &mut self,
        on_input: Box<dyn FnMut(&str) + Send>,
        _on_resize: Box<dyn FnMut() + Send>,
    ) {
        *self.input_slot.lock().unwrap() = Some(on_input);
    }
    fn stop(&mut self) {}
    fn drain_input(&mut self, _max_ms: u64, _idle_ms: u64) {}
    fn write(&mut self, data: &str) {
        self.writes.lock().unwrap().push_str(data);
    }
    fn columns(&self) -> u16 {
        self.size.lock().unwrap().0
    }
    fn rows(&self) -> u16 {
        self.size.lock().unwrap().1
    }
    fn kitty_protocol_active(&self) -> bool {
        false
    }
    fn move_by(&mut self, _lines: i32) {}
    fn hide_cursor(&mut self) {}
    fn show_cursor(&mut self) {}
    fn clear_line(&mut self) {}
    fn clear_from_cursor(&mut self) {}
    fn clear_screen(&mut self) {}
    fn set_title(&mut self, _title: &str) {}
    fn set_progress(&mut self, _active: bool) {}
}

impl DriveTerminal {
    fn new() -> Self {
        Self {
            input_slot: Arc::new(Mutex::new(None)),
            size: Arc::new(Mutex::new((80, 24))),
            writes: Arc::new(Mutex::new(String::new())),
        }
    }
}

/// Grab the captured `on_input` callback.
fn take_on_input(input_slot: &InputSlot) -> Box<dyn FnMut(&str) + Send> {
    for _ in 0..200 {
        if let Some(cb) = input_slot.lock().unwrap().take() {
            return cb;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("terminal should have captured on_input");
}

/// A session whose `prompt` waits for a release flag before emitting the
/// canned stream — simulating a delayed provider.
struct DelayedSession {
    listener: Arc<Mutex<Option<SessionEventListener>>>,
    prompt_seen: Arc<AtomicBool>,
    /// How many times `prompt` has been entered. `prompt_seen` only says
    /// "at least one", which cannot distinguish a queued prompt that later ran
    /// from one that was silently dropped.
    prompt_count: Arc<AtomicUsize>,
    release: Arc<AtomicBool>,
    /// The assistant text the canned stream emits. Overridable so a test can
    /// feed Markdown and assert the renderer consumed the markers.
    stream_text: Arc<Mutex<String>>,
    /// Reasoning text to emit as a `thinking` content block alongside the
    /// answer. Empty (the default) emits no thinking block at all.
    thinking_text: Arc<Mutex<String>>,
    fail_prompt: Arc<AtomicBool>,
    /// When set, `prompt` requests a tool approval before proceeding.
    ask_approval: Arc<AtomicBool>,
    /// The decider `InteractiveMode` installs (`set_tool_approval_decider`),
    /// invoked mid-prompt to mirror the agent's `before_tool_call` hook.
    decider: Arc<Mutex<Option<ToolApprovalDecider>>>,
    /// The decision the decider returned for the last request.
    last_decision: Arc<Mutex<Option<ToolApprovalDecision>>>,
    /// Set by `abort()` (B2: cooperative cancellation) — observed by the
    /// release-wait loop below so a cancelled prompt unwinds on its own
    /// instead of relying on a hard task abort dropping it mid-await.
    aborted: Arc<AtomicBool>,
    /// How many times `compact` has been entered — lets a mutual-exclusion
    /// test assert `/compact` was never actually invoked while a prompt was
    /// in flight (the guard lives in `run_compact`, before any call reaches
    /// this session at all).
    compact_count: Arc<AtomicUsize>,
    /// When set, `compact` returns `Err` instead of `Ok`, so a test can
    /// assert the TUI shows a failure notice rather than "Compaction
    /// finished".
    fail_compact: Arc<AtomicBool>,
}

impl DelayedSession {
    fn new() -> Self {
        Self {
            listener: Arc::new(Mutex::new(None)),
            prompt_seen: Arc::new(AtomicBool::new(false)),
            prompt_count: Arc::new(AtomicUsize::new(0)),
            release: Arc::new(AtomicBool::new(false)),
            stream_text: Arc::new(Mutex::new("Hello from the delayed provider".to_string())),
            thinking_text: Arc::new(Mutex::new(String::new())),
            fail_prompt: Arc::new(AtomicBool::new(false)),
            ask_approval: Arc::new(AtomicBool::new(false)),
            decider: Arc::new(Mutex::new(None)),
            last_decision: Arc::new(Mutex::new(None)),
            aborted: Arc::new(AtomicBool::new(false)),
            compact_count: Arc::new(AtomicUsize::new(0)),
            fail_compact: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Emit the canned assistant stream through the captured listener.
    ///
    /// `text` is ignored in favour of `stream_text` so a test can set the body
    /// before submitting; the parameter is kept because existing callers pass
    /// it and the default value matches what they assert on.
    fn emit_stream(&self, _text: &str) {
        let listener = self.listener.lock().unwrap().clone();
        let text = self.stream_text.lock().unwrap().clone();
        let thinking = self.thinking_text.lock().unwrap().clone();
        // A `thinking` block, shaped exactly like `pirust_ai`'s
        // `ThinkingContent` (`crates/pirust-ai/src/types/content.rs:65`):
        // `{"type":"thinking","thinking":"…"}`. Emitted before the text block,
        // which is the order a real provider streams them in.
        let content = |body: &str| {
            let mut blocks = Vec::new();
            if !thinking.is_empty() {
                blocks.push(serde_json::json!({"type": "thinking", "thinking": thinking}));
            }
            blocks.push(serde_json::json!({"type": "text", "text": body}));
            serde_json::json!({ "role": "assistant", "content": blocks })
        };
        if let Some(listener) = listener {
            listener(&AgentSessionEvent::MessageStart {
                message: serde_json::json!({"role": "assistant", "content": []}),
            });
            listener(&AgentSessionEvent::MessageUpdate {
                assistant_message_event: serde_json::json!({}),
                message: content(&text),
            });
            listener(&AgentSessionEvent::MessageEnd {
                message: content(&text),
            });
            listener(&AgentSessionEvent::AgentEnd {
                messages: vec![],
                will_retry: false,
            });
        }
    }
}

#[async_trait::async_trait]
impl PrintModeSession for DelayedSession {
    fn header(&self) -> Option<SessionHeader> {
        None
    }
    async fn bind_extensions(&self, _binding: ExtensionBinding) -> Result<(), ThrownValue> {
        Ok(())
    }
    fn subscribe(&self, listener: SessionEventListener) -> Subscription {
        *self.listener.lock().unwrap() = Some(listener);
        Subscription::new(|| {})
    }
    async fn prompt(&self, text: &str, _options: Option<PromptOptions>) -> Result<(), ThrownValue> {
        self.prompt_seen.store(true, Ordering::SeqCst);
        self.prompt_count.fetch_add(1, Ordering::SeqCst);
        // Optionally request a tool approval (mirrors the agent's
        // `before_tool_call` hook flow, which blocks the loop awaiting the
        // user's decision).
        if self.ask_approval.load(Ordering::SeqCst) {
            let decision = {
                let request = pirust_coding_agent::print_mode::ToolApprovalRequest {
                    tool_name: "bash".to_string(),
                    args: serde_json::json!({ "command": "rm -rf /" }),
                };
                let decider = self.decider.lock().unwrap().clone();
                match decider.as_ref() {
                    Some(d) => d(request).await,
                    None => ToolApprovalDecision::RunOnce,
                }
            };
            *self.last_decision.lock().unwrap() = Some(decision);
        }
        // Wait until released or cooperatively aborted.
        while !self.release.load(Ordering::SeqCst) {
            if self.aborted.load(Ordering::SeqCst) {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        if self.fail_prompt.load(Ordering::SeqCst) {
            return Err(ThrownValue::Error("provider exploded".into()));
        }
        let _ = text;
        self.emit_stream("Hello from the delayed provider");
        Ok(())
    }
    fn state(&self) -> SessionStateView {
        SessionStateView {
            messages: Vec::new(),
        }
    }
    async fn wait_for_idle(&self) {}
    async fn navigate_tree(
        &self,
        _target_id: &str,
        _options: Option<NavigateTreeOptions>,
    ) -> Cancelled {
        Cancelled { cancelled: false }
    }
    async fn reload(&self) {}

    fn set_tool_approval_decider(&self, decider: ToolApprovalDecider) {
        *self.decider.lock().unwrap() = Some(decider);
    }

    fn abort(&self) {
        self.aborted.store(true, Ordering::SeqCst);
    }

    /// Mirrors `SingleTurnSession::compact`'s real event contract
    /// (`runtime_host.rs`) — emit `CompactionStart`, then `CompactionEnd`
    /// with `aborted` reflecting the outcome — so tests exercise
    /// `InteractiveMode`'s reaction to those events, not a stand-in.
    async fn compact(&self, reason: CompactionReason) -> Result<(), String> {
        self.compact_count.fetch_add(1, Ordering::SeqCst);
        let listener = self.listener.lock().unwrap().clone();
        if let Some(listener) = &listener {
            listener(&AgentSessionEvent::CompactionStart { reason });
        }
        let result = if self.fail_compact.load(Ordering::SeqCst) {
            Err("synthetic compaction failure".to_string())
        } else {
            Ok(())
        };
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
}

impl TuiRuntimeInfo for DelayedSession {
    fn runtime_status(&self) -> TuiRuntimeStatus {
        TuiRuntimeStatus {
            provider: "test-provider".into(),
            model: "test-model".into(),
            model_name: "Test Model".into(),
            context_window: 1_000_000,
            reasoning_supported: true,
            thinking_level: "off".into(),
            context_tokens: 0,
            cost: 0.0,
            tools_enabled: true,
        }
    }
}

fn make_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("runtime")
}

/// A handle to the captured input callback before the terminal is moved.
#[derive(Clone)]
struct TerminalHandles {
    input: InputSlot,
    writes: Arc<Mutex<String>>,
}

impl TerminalHandles {
    fn grab(terminal: &DriveTerminal) -> Self {
        Self {
            input: Arc::clone(&terminal.input_slot),
            writes: Arc::clone(&terminal.writes),
        }
    }
}

/// Type `hello` and submit via the editor.
fn type_and_submit(on_input: &mut Box<dyn FnMut(&str) + Send>) {
    for ch in ["h", "e", "l", "l", "o"] {
        on_input(ch);
    }
    on_input("\r");
}

#[test]
fn delayed_submit_streams_then_completes() {
    let terminal = Box::new(DriveTerminal::new());
    let handles = TerminalHandles::grab(&terminal);
    let session = Arc::new(DelayedSession::new());
    let runtime = make_runtime();
    let mut mode = pirust_coding_agent::interactive_mode::InteractiveMode::new(
        terminal,
        Arc::clone(&session) as Arc<dyn InteractiveSession>,
        runtime.handle().clone(),
    );

    let prompt_seen = Arc::clone(&session.prompt_seen);
    let release = Arc::clone(&session.release);
    let on_input_slot = handles.input;
    thread::spawn(move || {
        let mut on_input = take_on_input(&on_input_slot);
        type_and_submit(&mut on_input);
        // Wait until the prompt has started, then release the stream.
        for _ in 0..200 {
            if prompt_seen.load(Ordering::SeqCst) {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        release.store(true, Ordering::SeqCst);
        thread::sleep(Duration::from_millis(100));
        on_input("\u{4}"); // quit
    });

    runtime.block_on(mode.run_async());
    assert!(
        session.prompt_seen.load(Ordering::SeqCst),
        "prompt should have been invoked"
    );
    // The streamed assistant text must have been written to the terminal.
    let writes = handles.writes.lock().unwrap().clone();
    assert!(
        writes.contains("Hello from the delayed provider"),
        "streamed text should be rendered to the terminal, got: {writes:?}"
    );
}

/// A streamed turn must repaint through the TUI's line diff, not by throwing
/// the diff away and rewriting the whole screen.
///
/// `request_render(true)` clears `previous_lines`/`previous_width`, which sends
/// the next `poll()` down `do_render`'s `full_render` path. Every content
/// update in `InteractiveMode` used to pass `true`, so a single turn — user
/// line, message start, each stream delta, message end, separator, status
/// refreshes — cost one full-screen repaint each. `docs/tui-design-audit.md`
/// names this directly: "Rust does not guarantee speed if every stream update
/// causes a full terminal redraw."
///
/// Measured on this scenario (a one-message turn, 80x24): forcing gives 3 full
/// redraws / 4046 bytes written, every run; diffing gives 1 / 2681-3878. The
/// one remaining full redraw is the startup frame, which the loop's own resize
/// check legitimately forces. The saving grows with the transcript, because a
/// full redraw rewrites every row on screen while the diff rewrites only the
/// rows that changed.
#[test]
fn streaming_a_turn_does_not_force_full_redraws() {
    let terminal = Box::new(DriveTerminal::new());
    let handles = TerminalHandles::grab(&terminal);
    let session = Arc::new(DelayedSession::new());
    let runtime = make_runtime();
    let mut mode = pirust_coding_agent::interactive_mode::InteractiveMode::new(
        terminal,
        Arc::clone(&session) as Arc<dyn InteractiveSession>,
        runtime.handle().clone(),
    );

    let prompt_seen = Arc::clone(&session.prompt_seen);
    let release = Arc::clone(&session.release);
    let on_input_slot = handles.input;
    thread::spawn(move || {
        let mut on_input = take_on_input(&on_input_slot);
        type_and_submit(&mut on_input);
        for _ in 0..200 {
            if prompt_seen.load(Ordering::SeqCst) {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        release.store(true, Ordering::SeqCst);
        thread::sleep(Duration::from_millis(200));
        on_input("\u{4}"); // quit
    });

    runtime.block_on(mode.run_async());

    let writes = handles.writes.lock().unwrap().clone();
    assert!(
        writes.contains("Hello from the delayed provider"),
        "the turn must still render its streamed text, got: {writes:?}"
    );
    let full_redraws = mode.full_redraws();
    assert!(
        full_redraws <= 2,
        "a streamed turn should repaint through the line diff, not force \
         full-screen redraws; got {full_redraws} full redraws"
    );
}

#[test]
fn delayed_submit_cancel_aborts_before_release() {
    let terminal = Box::new(DriveTerminal::new());
    let handles = TerminalHandles::grab(&terminal);
    let session = Arc::new(DelayedSession::new());
    let runtime = make_runtime();
    let mut mode = pirust_coding_agent::interactive_mode::InteractiveMode::new(
        terminal,
        Arc::clone(&session) as Arc<dyn InteractiveSession>,
        runtime.handle().clone(),
    );

    let prompt_seen = Arc::clone(&session.prompt_seen);
    let on_input_slot = handles.input;
    thread::spawn(move || {
        let mut on_input = take_on_input(&on_input_slot);
        type_and_submit(&mut on_input);
        for _ in 0..200 {
            if prompt_seen.load(Ordering::SeqCst) {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        // Cancel with Ctrl+C while the prompt is still pending.
        on_input("\u{3}"); // ctrl+c
        thread::sleep(Duration::from_millis(100));
        on_input("\u{4}"); // quit
    });

    runtime.block_on(mode.run_async());
    assert!(
        session.prompt_seen.load(Ordering::SeqCst),
        "prompt should have been invoked"
    );
    // The provider was aborted before its stream ran, so the assistant text
    // must NOT appear in the terminal — only the cancellation notice.
    let writes = handles.writes.lock().unwrap().clone();
    assert!(
        !writes.contains("Hello from the delayed provider"),
        "cancelled turn should not render streamed content, got: {writes:?}"
    );
    assert!(
        writes.contains("cancelled"),
        "cancellation notice should render, got: {writes:?}"
    );
}

#[test]
fn delayed_submit_error_renders_notice() {
    let terminal = Box::new(DriveTerminal::new());
    let handles = TerminalHandles::grab(&terminal);
    let session = Arc::new(DelayedSession::new());
    session.fail_prompt.store(true, Ordering::SeqCst);
    let runtime = make_runtime();
    let mut mode = pirust_coding_agent::interactive_mode::InteractiveMode::new(
        terminal,
        Arc::clone(&session) as Arc<dyn InteractiveSession>,
        runtime.handle().clone(),
    );

    let prompt_seen = Arc::clone(&session.prompt_seen);
    let release = Arc::clone(&session.release);
    let on_input_slot = handles.input;
    thread::spawn(move || {
        let mut on_input = take_on_input(&on_input_slot);
        type_and_submit(&mut on_input);
        for _ in 0..200 {
            if prompt_seen.load(Ordering::SeqCst) {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        release.store(true, Ordering::SeqCst);
        thread::sleep(Duration::from_millis(100));
        on_input("\u{4}"); // quit
    });

    runtime.block_on(mode.run_async());
    assert!(
        session.prompt_seen.load(Ordering::SeqCst),
        "prompt should have been invoked"
    );
    // The provider error must be visible in the rendered terminal output.
    let writes = handles.writes.lock().unwrap().clone();
    assert!(
        writes.contains("provider exploded"),
        "provider error should render inline, got: {writes:?}"
    );
}

#[test]
fn tool_approval_prompt_renders_and_deny_blocks() {
    let terminal = Box::new(DriveTerminal::new());
    let handles = TerminalHandles::grab(&terminal);
    let session = Arc::new(DelayedSession::new());
    session.ask_approval.store(true, Ordering::SeqCst);
    let runtime = make_runtime();
    let mut mode = pirust_coding_agent::interactive_mode::InteractiveMode::new(
        terminal,
        Arc::clone(&session) as Arc<dyn InteractiveSession>,
        runtime.handle().clone(),
    );

    let prompt_seen = Arc::clone(&session.prompt_seen);
    let release = Arc::clone(&session.release);
    let writes = handles.writes.clone();
    let on_input_slot = handles.input;
    thread::spawn(move || {
        let mut on_input = take_on_input(&on_input_slot);
        type_and_submit(&mut on_input);
        // Wait until the approval prompt has rendered, then deny.
        for _ in 0..500 {
            if writes.lock().unwrap().contains("requires approval") {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        on_input("d"); // deny
                       // Let the turn finish, then quit.
        for _ in 0..200 {
            if prompt_seen.load(Ordering::SeqCst) {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        release.store(true, Ordering::SeqCst);
        thread::sleep(Duration::from_millis(100));
        on_input("\u{4}"); // quit
    });

    runtime.block_on(mode.run_async());
    // The denial must be the decision the prompt's request got.
    let decision = *session.last_decision.lock().unwrap();
    assert_eq!(
        decision,
        Some(ToolApprovalDecision::Deny),
        "deny key must resolve the approval to Deny"
    );
    // The approval prompt + denial notice must have rendered.
    let writes = handles.writes.lock().unwrap().clone();
    assert!(
        writes.contains("requires approval"),
        "approval prompt should render, got: {writes:?}"
    );
    assert!(
        writes.contains("denied"),
        "denial notice should render, got: {writes:?}"
    );
}

#[test]
fn resize_during_idle_is_picked_up() {
    let terminal = Box::new(DriveTerminal::new());
    let handles = TerminalHandles::grab(&terminal);
    let session = Arc::new(DelayedSession::new());
    let runtime = make_runtime();
    let mut mode = pirust_coding_agent::interactive_mode::InteractiveMode::new(
        terminal,
        Arc::clone(&session) as Arc<dyn InteractiveSession>,
        runtime.handle().clone(),
    );

    let on_input_slot = handles.input;
    thread::spawn(move || {
        let mut on_input = take_on_input(&on_input_slot);
        thread::sleep(Duration::from_millis(50));
        on_input("\u{4}"); // quit
    });

    mode.run();
    // Just proving the loop tolerates an idle run + quit without panic.
}

/// A prompt submitted while a turn is already running must be queued and then
/// run — not silently discarded.
///
/// The submit drain used to be guarded by `self.active_turn.is_none()`, with no
/// `else`. The editor clears itself on submit, so text entered during a turn
/// disappeared from the screen *and* from the queue, and nothing told the user.
/// `docs/tui-design-samples.html` §7 names `queued` as a response state the UI
/// must distinguish, which is only meaningful if the queue exists.
///
/// This asserts both halves: the queue notice is rendered (so the user is told),
/// and `prompt` is entered twice (so the queued text actually ran).
#[test]
fn a_prompt_submitted_mid_turn_is_queued_then_runs() {
    let terminal = Box::new(DriveTerminal::new());
    let handles = TerminalHandles::grab(&terminal);
    let session = Arc::new(DelayedSession::new());
    let runtime = make_runtime();
    let mut mode = pirust_coding_agent::interactive_mode::InteractiveMode::new(
        terminal,
        Arc::clone(&session) as Arc<dyn InteractiveSession>,
        runtime.handle().clone(),
    );

    let prompt_seen = Arc::clone(&session.prompt_seen);
    let prompt_count = Arc::clone(&session.prompt_count);
    let release = Arc::clone(&session.release);
    let on_input_slot = handles.input;
    thread::spawn(move || {
        let mut on_input = take_on_input(&on_input_slot);
        // First prompt: starts the turn, which then blocks on `release`.
        type_and_submit(&mut on_input);
        for _ in 0..200 {
            if prompt_seen.load(Ordering::SeqCst) {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        // Second prompt, typed while the first turn is still in flight.
        for ch in ["q", "u", "e", "u", "e", "d"] {
            on_input(ch);
            thread::sleep(Duration::from_millis(10));
        }
        on_input("\r");
        thread::sleep(Duration::from_millis(80));
        // Let the first turn finish; the queued one must start on its own.
        release.store(true, Ordering::SeqCst);
        for _ in 0..300 {
            if prompt_count.load(Ordering::SeqCst) >= 2 {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        thread::sleep(Duration::from_millis(60));
        on_input("\u{4}"); // quit
    });

    runtime.block_on(mode.run_async());

    let writes = handles.writes.lock().unwrap().clone();
    assert!(
        writes.contains("queued #1"),
        "the queued prompt should be announced, got: {writes:?}"
    );
    assert_eq!(
        session.prompt_count.load(Ordering::SeqCst),
        2,
        "the queued prompt should have run after the first turn finished"
    );
}

/// `/compact` runs end to end: it reaches the session exactly once and both
/// the start and success notices render in the transcript.
#[test]
fn slash_compact_runs_and_shows_start_and_finish_notices() {
    let terminal = Box::new(DriveTerminal::new());
    let handles = TerminalHandles::grab(&terminal);
    let session = Arc::new(DelayedSession::new());
    let runtime = make_runtime();
    let mut mode = pirust_coding_agent::interactive_mode::InteractiveMode::new(
        terminal,
        Arc::clone(&session) as Arc<dyn InteractiveSession>,
        runtime.handle().clone(),
    );

    let compact_count = Arc::clone(&session.compact_count);
    let on_input_slot = handles.input;
    thread::spawn(move || {
        let mut on_input = take_on_input(&on_input_slot);
        on_input("/compact");
        thread::sleep(Duration::from_millis(30));
        on_input("\r");
        for _ in 0..200 {
            if compact_count.load(Ordering::SeqCst) >= 1 {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        thread::sleep(Duration::from_millis(80));
        on_input("\u{4}"); // quit
    });

    runtime.block_on(mode.run_async());

    assert_eq!(
        session.compact_count.load(Ordering::SeqCst),
        1,
        "/compact should have reached the session exactly once"
    );
    let writes = handles.writes.lock().unwrap().clone();
    assert!(
        writes.contains("Compacting session"),
        "the start notice should render, got: {writes:?}"
    );
    assert!(
        writes.contains("Compaction finished"),
        "the success notice should render, got: {writes:?}"
    );
}

/// A failed compaction must say so, not silently claim success — the bug
/// `render_event`'s `CompactionEnd` arm used to have (it ignored `aborted`
/// entirely and always showed "Compaction finished").
#[test]
fn slash_compact_failure_shows_failure_notice_not_success() {
    let terminal = Box::new(DriveTerminal::new());
    let handles = TerminalHandles::grab(&terminal);
    let session = Arc::new(DelayedSession::new());
    session.fail_compact.store(true, Ordering::SeqCst);
    let runtime = make_runtime();
    let mut mode = pirust_coding_agent::interactive_mode::InteractiveMode::new(
        terminal,
        Arc::clone(&session) as Arc<dyn InteractiveSession>,
        runtime.handle().clone(),
    );

    let compact_count = Arc::clone(&session.compact_count);
    let on_input_slot = handles.input;
    thread::spawn(move || {
        let mut on_input = take_on_input(&on_input_slot);
        on_input("/compact");
        thread::sleep(Duration::from_millis(30));
        on_input("\r");
        for _ in 0..200 {
            if compact_count.load(Ordering::SeqCst) >= 1 {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        thread::sleep(Duration::from_millis(80));
        on_input("\u{4}"); // quit
    });

    runtime.block_on(mode.run_async());

    let writes = handles.writes.lock().unwrap().clone();
    assert!(
        writes.contains("Compaction failed"),
        "a failed compaction must say so, got: {writes:?}"
    );
    assert!(
        !writes.contains("Compaction finished"),
        "a failed compaction must not also claim success, got: {writes:?}"
    );
}

/// A prompt turn and a `/compact` must never run concurrently: both mutate
/// `Agent`'s message list, so `run_compact` guards on `active_turn` itself
/// rather than relying on the generic mid-turn slash-command dispatch.
#[test]
fn slash_compact_is_blocked_while_a_prompt_is_in_flight() {
    let terminal = Box::new(DriveTerminal::new());
    let handles = TerminalHandles::grab(&terminal);
    let session = Arc::new(DelayedSession::new());
    let runtime = make_runtime();
    let mut mode = pirust_coding_agent::interactive_mode::InteractiveMode::new(
        terminal,
        Arc::clone(&session) as Arc<dyn InteractiveSession>,
        runtime.handle().clone(),
    );

    let prompt_seen = Arc::clone(&session.prompt_seen);
    let release = Arc::clone(&session.release);
    let on_input_slot = handles.input;
    let writes_probe = Arc::clone(&handles.writes);
    thread::spawn(move || {
        let mut on_input = take_on_input(&on_input_slot);
        // Start a prompt turn, which blocks on `release`.
        type_and_submit(&mut on_input);
        for _ in 0..200 {
            if prompt_seen.load(Ordering::SeqCst) {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        // While the prompt is still in flight, try to compact.
        on_input("/compact");
        thread::sleep(Duration::from_millis(30));
        on_input("\r");
        for _ in 0..200 {
            if writes_probe
                .lock()
                .unwrap()
                .contains("Cannot compact while a request is in progress")
            {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        // Let the prompt finish, then quit.
        release.store(true, Ordering::SeqCst);
        thread::sleep(Duration::from_millis(100));
        on_input("\u{4}"); // quit
    });

    runtime.block_on(mode.run_async());

    let writes = handles.writes.lock().unwrap().clone();
    assert!(
        writes.contains("Cannot compact while a request is in progress"),
        "compaction must refuse to start mid-turn, got: {writes:?}"
    );
    assert_eq!(
        session.compact_count.load(Ordering::SeqCst),
        0,
        "the guard in run_compact should stop the call before it ever reaches the session"
    );
}

/// An idle TUI must not burn CPU.
///
/// The loop used to end every iteration with an unconditional
/// `tokio::time::sleep(10ms)`, so an idle process woke 100 times a second to
/// drain four empty channels and re-poll the terminal size. `wait_for_work`
/// replaced that with a park on the input channel plus a `Notify` permit, so an
/// idle second should cost a couple of resize-poll wakeups, not a hundred.
///
/// `InteractiveMode::loop_iterations` counts loop-body entries directly, and
/// `run_async` borrows `&mut self`, so the count is still readable after
/// `block_on` returns. The bound is deliberately loose — this is a regression
/// guard against returning to a busy-poll, not a benchmark.
#[test]
fn an_idle_loop_parks_instead_of_polling() {
    let terminal = Box::new(DriveTerminal::new());
    let handles = TerminalHandles::grab(&terminal);
    let session = Arc::new(DelayedSession::new());
    let runtime = make_runtime();
    let mut mode = pirust_coding_agent::interactive_mode::InteractiveMode::new(
        terminal,
        Arc::clone(&session) as Arc<dyn InteractiveSession>,
        runtime.handle().clone(),
    );

    let on_input_slot = handles.input;
    thread::spawn(move || {
        let mut on_input = take_on_input(&on_input_slot);
        // Sit completely idle for a second: no keys, no prompt, no events.
        thread::sleep(Duration::from_millis(1000));
        on_input("\u{4}"); // quit
    });

    runtime.block_on(mode.run_async());
    let _ = handles.writes;

    let iterations = mode.loop_iterations();
    // At a 500ms idle resize-poll, one idle second is ~2-3 iterations plus a
    // few for startup and shutdown. The old 10ms busy-poll would be ~100.
    assert!(
        iterations < 25,
        "an idle second should park, not spin: {iterations} loop iterations \
         (the 10ms busy-poll this replaced would be ~100)"
    );
}

/// Assistant text must go through the Markdown renderer, not be shown raw.
///
/// `pirust-tui` has shipped a 2,000-line Markdown renderer since the crate was
/// ported, and the chat never called it — `render_event` mounted a plain
/// `Text`, so `**bold**` and `# heading` reached the screen as literal
/// asterisks and hashes. This asserts the markers are consumed.
#[test]
fn assistant_markdown_is_rendered_not_shown_raw() {
    let terminal = Box::new(DriveTerminal::new());
    let handles = TerminalHandles::grab(&terminal);
    let session = Arc::new(DelayedSession::new());
    let runtime = make_runtime();
    let mut mode = pirust_coding_agent::interactive_mode::InteractiveMode::new(
        terminal,
        Arc::clone(&session) as Arc<dyn InteractiveSession>,
        runtime.handle().clone(),
    );

    let prompt_seen = Arc::clone(&session.prompt_seen);
    let release = Arc::clone(&session.release);
    let markdown = Arc::clone(&session.stream_text);
    *markdown.lock().unwrap() = "# Heading\n\nSome **bold** words.".to_string();
    let on_input_slot = handles.input;
    thread::spawn(move || {
        let mut on_input = take_on_input(&on_input_slot);
        type_and_submit(&mut on_input);
        for _ in 0..200 {
            if prompt_seen.load(Ordering::SeqCst) {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        release.store(true, Ordering::SeqCst);
        thread::sleep(Duration::from_millis(150));
        on_input("\u{4}");
    });

    runtime.block_on(mode.run_async());
    let writes = handles.writes.lock().unwrap().clone();

    assert!(
        writes.contains("Heading"),
        "heading text should be rendered, got: {writes:?}"
    );
    assert!(
        writes.contains("bold"),
        "emphasised text should be rendered, got: {writes:?}"
    );
    assert!(
        !writes.contains("**bold**"),
        "the ** markers should be consumed by the Markdown renderer, not printed: {writes:?}"
    );
    assert!(
        !writes.contains("# Heading"),
        "the # marker should be consumed by the Markdown renderer, not printed: {writes:?}"
    );
}

/// A model's reasoning must be shown, collapsed, and expandable with Ctrl+O.
///
/// `assistant_text` filtered assistant content blocks down to `type == "text"`
/// and dropped everything else, so thinking output was discarded entirely.
#[test]
fn thinking_is_shown_collapsed_and_expands_on_ctrl_o() {
    let terminal = Box::new(DriveTerminal::new());
    let handles = TerminalHandles::grab(&terminal);
    let session = Arc::new(DelayedSession::new());
    let runtime = make_runtime();
    let mut mode = pirust_coding_agent::interactive_mode::InteractiveMode::new(
        terminal,
        Arc::clone(&session) as Arc<dyn InteractiveSession>,
        runtime.handle().clone(),
    );

    let prompt_seen = Arc::clone(&session.prompt_seen);
    let release = Arc::clone(&session.release);
    *session.thinking_text.lock().unwrap() =
        "First I inspect the parser.\nThen I plan the smallest safe change.".to_string();
    let on_input_slot = handles.input;
    thread::spawn(move || {
        let mut on_input = take_on_input(&on_input_slot);
        type_and_submit(&mut on_input);
        for _ in 0..200 {
            if prompt_seen.load(Ordering::SeqCst) {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        release.store(true, Ordering::SeqCst);
        thread::sleep(Duration::from_millis(120));
        // Ctrl+O expands the reasoning block.
        on_input("\u{f}");
        thread::sleep(Duration::from_millis(120));
        on_input("\u{4}");
    });

    runtime.block_on(mode.run_async());
    let writes = handles.writes.lock().unwrap().clone();

    assert!(
        writes.contains("Thinking") || writes.contains("Thought"),
        "a reasoning block should be shown, got: {writes:?}"
    );
    assert!(
        writes.contains("smallest safe change"),
        "Ctrl+O should expand the reasoning text, got: {writes:?}"
    );
}

/// A `write` tool call must render a diff, not its raw args JSON.
#[test]
fn a_write_tool_call_renders_a_diff_not_raw_json() {
    let terminal = Box::new(DriveTerminal::new());
    let handles = TerminalHandles::grab(&terminal);
    let session = Arc::new(DelayedSession::new());
    let runtime = make_runtime();
    let mut mode = pirust_coding_agent::interactive_mode::InteractiveMode::new(
        terminal,
        Arc::clone(&session) as Arc<dyn InteractiveSession>,
        runtime.handle().clone(),
    );

    // A path that certainly does not exist, so the diff is a clean "new file".
    let path = "pirust_diff_probe_does_not_exist.txt";
    if let Some(listener) = session.listener.lock().unwrap().clone() {
        listener(&AgentSessionEvent::ToolExecutionStart {
            tool_call_id: "call_w".into(),
            tool_name: "write".into(),
            args: serde_json::json!({ "path": path, "content": "alpha\nbeta\n" }),
        });
    }

    let on_input_slot = handles.input;
    thread::spawn(move || {
        let mut on_input = take_on_input(&on_input_slot);
        thread::sleep(Duration::from_millis(120));
        on_input("\u{4}");
    });

    runtime.block_on(mode.run_async());
    let writes = handles.writes.lock().unwrap().clone();

    assert!(
        writes.contains("alpha"),
        "the written content should appear as diff lines, got: {writes:?}"
    );
    assert!(
        writes.contains(path),
        "the changed path should be identified, got: {writes:?}"
    );
    assert!(
        !writes.contains("\"content\""),
        "the raw args JSON should be suppressed in favour of the diff: {writes:?}"
    );
}
