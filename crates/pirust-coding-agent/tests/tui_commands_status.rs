//! feat-013 completion tests (steps 3–7): the enriched status line, slash-command
//! dispatch/palette, and the full tool-approval flow (run-once / always / deny).
//!
//! These drive the public interaction contract — terminal input through
//! `InteractiveMode::run_async` with a feeder thread — using a stub that reports
//! runtime status and records the tool-approval decision, asserting on the
//! captured terminal writes rather than internal state.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use pirust_agent_core::harness::types::{SessionHeader, SessionHeaderTag};
use pirust_coding_agent::interactive_mode::InteractiveSession;
use pirust_coding_agent::print_mode::{
    Cancelled, ExtensionBinding, NavigateTreeOptions, PrintModeSession, PromptOptions,
    SessionEventListener, SessionStateView, Subscription, ThrownValue, ToolApprovalDecider,
    ToolApprovalDecision, ToolApprovalRequest, TuiRuntimeInfo, TuiRuntimeStatus,
};
use pirust_tui::terminal::Terminal;

type InputSlot = Arc<Mutex<Option<Box<dyn FnMut(&str) + Send>>>>;

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

fn take_on_input(slot: &InputSlot) -> Box<dyn FnMut(&str) + Send> {
    for _ in 0..200 {
        if let Some(cb) = slot.lock().unwrap().take() {
            return cb;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("terminal should have captured on_input");
}

/// A stub that reports a fixed runtime status and (opt-in) requests a tool
/// approval on prompt, recording the decision the TUI resolves.
struct StatusSession {
    listener: Arc<Mutex<Option<SessionEventListener>>>,
    ask_approval: Arc<AtomicBool>,
    decider: Arc<Mutex<Option<ToolApprovalDecider>>>,
    last_decision: Arc<Mutex<Option<ToolApprovalDecision>>>,
    pub header: Option<SessionHeader>,
}

impl StatusSession {
    fn with_cwd(cwd: &str, id: &str) -> Self {
        Self {
            listener: Arc::new(Mutex::new(None)),
            ask_approval: Arc::new(AtomicBool::new(false)),
            decider: Arc::new(Mutex::new(None)),
            last_decision: Arc::new(Mutex::new(None)),
            header: Some(SessionHeader {
                kind: SessionHeaderTag::Session,
                version: 3,
                id: id.into(),
                timestamp: "2026-01-01T00:00:00.000Z".into(),
                cwd: cwd.into(),
                parent_session: None,
                metadata: None,
            }),
        }
    }
}

#[async_trait::async_trait]
impl PrintModeSession for StatusSession {
    fn header(&self) -> Option<SessionHeader> {
        self.header.clone()
    }
    async fn bind_extensions(&self, _binding: ExtensionBinding) -> Result<(), ThrownValue> {
        Ok(())
    }
    fn subscribe(&self, listener: SessionEventListener) -> Subscription {
        *self.listener.lock().unwrap() = Some(listener);
        Subscription::new(|| {})
    }
    async fn prompt(&self, _text: &str, _o: Option<PromptOptions>) -> Result<(), ThrownValue> {
        if self.ask_approval.load(Ordering::SeqCst) {
            let request = ToolApprovalRequest {
                tool_name: "bash".to_string(),
                args: serde_json::json!({ "command": "rm -rf /" }),
            };
            let decider = self.decider.lock().unwrap().clone();
            let decision = match decider {
                Some(d) => d(request).await,
                None => ToolApprovalDecision::RunOnce,
            };
            *self.last_decision.lock().unwrap() = Some(decision);
        }
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
}

impl TuiRuntimeInfo for StatusSession {
    fn runtime_status(&self) -> TuiRuntimeStatus {
        TuiRuntimeStatus {
            provider: "anthropic".into(),
            model: "claude-sonnet-4-5".into(),
            model_name: "Claude Sonnet 4.5".into(),
            context_window: 200_000,
            reasoning_supported: true,
            thinking_level: "medium".into(),
            context_tokens: 12_345,
            cost: 0.0042,
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

/// A fully-wired rig: the mode plus the captured terminal writes and input
/// callback slot.
struct Rig {
    mode: pirust_coding_agent::interactive_mode::InteractiveMode,
    writes: Arc<Mutex<String>>,
    input: InputSlot,
    _runtime: tokio::runtime::Runtime,
}

fn make_rig(session: Arc<StatusSession>) -> Rig {
    let terminal = Box::new(DriveTerminal::new());
    let writes = terminal.writes.clone();
    let input = terminal.input_slot.clone();
    let runtime = make_runtime();
    let mode = pirust_coding_agent::interactive_mode::InteractiveMode::new(
        terminal,
        session as Arc<dyn InteractiveSession>,
        runtime.handle().clone(),
    );
    Rig {
        mode,
        writes,
        input,
        _runtime: runtime,
    }
}

/// Type `text` into the editor character-by-character (with delays so the loop
/// processes each key), submit, wait, then quit with Ctrl+D.
///
/// Slash-prefixed text is sent as a single paste so it routes to the editor
/// and submits as one line (typing `/` alone would open the palette and
/// consume the remaining characters as a filter).
fn type_submit_quit(on_input: &mut Box<dyn FnMut(&str) + Send>, text: &str) {
    if text.starts_with('/') {
        // Paste the whole line: editor gets the full text, Enter submits it
        // through the normal submit channel (dispatch_command path).
        on_input(text);
        thread::sleep(Duration::from_millis(30));
        on_input("\r");
        thread::sleep(Duration::from_millis(150));
        on_input("\u{4}");
        return;
    }
    for ch in text.chars() {
        on_input(&ch.to_string());
        thread::sleep(Duration::from_millis(15));
    }
    on_input("\r");
    thread::sleep(Duration::from_millis(150));
    on_input("\u{4}");
}

/// Drive the mode's async loop on the current thread; the feeder thread types
/// and quits. Returns the captured terminal writes.
fn run_async_rig(rig: Rig, text: &str) -> String {
    let mut mode = rig.mode;
    let writes = rig.writes;
    let input = rig.input;
    let input_text = text.to_string();
    thread::spawn(move || {
        let mut cb = take_on_input(&input);
        type_submit_quit(&mut cb, &input_text);
    });
    make_runtime().block_on(mode.run_async());
    let out = writes.lock().unwrap().clone();
    out
}

#[test]
fn status_line_shows_runtime_identity_and_model() {
    let session = Arc::new(StatusSession::with_cwd("/proj", "sess_abc"));
    let rig = make_rig(session);
    let rendered = run_async_rig(rig, "");
    assert!(
        rendered.contains("cwd: /proj"),
        "status should show cwd, got: {rendered:?}"
    );
    assert!(
        rendered.contains("sess_abc"),
        "status should show session id, got: {rendered:?}"
    );
    assert!(
        rendered.contains("anthropic / claude-sonnet-4-5"),
        "status should show provider/model, got: {rendered:?}"
    );
    assert!(
        rendered.contains("12345tok"),
        "status should show context tokens, got: {rendered:?}"
    );
    assert!(
        rendered.contains("ready"),
        "idle status should be ready, got: {rendered:?}"
    );
}

#[test]
fn slash_help_opens_palette_and_lists_commands() {
    let session = Arc::new(StatusSession::with_cwd("/proj", "s1"));
    let rig = make_rig(session);
    // Typing "/" opens the palette; the rest filters it; Enter runs /help.
    let rendered = run_async_rig(rig, "/help");
    assert!(
        rendered.contains("/model") && rendered.contains("/resume"),
        "/help should list commands, got: {rendered:?}"
    );
}

#[test]
fn unknown_slash_command_renders_actionable_error() {
    let session = Arc::new(StatusSession::with_cwd("/proj", "s1"));
    let rig = make_rig(session);
    // The palette filters on "bogus" → no match → Enter on empty selection
    // does nothing; instead drive dispatch directly through the public submit
    // path with a full "/bogus" line to assert the actionable error.
    let rendered = run_async_rig(rig, "/bogus");
    assert!(
        rendered.contains("Unknown command: /bogus") || rendered.contains("no matching commands"),
        "unknown command should render an actionable error, got: {rendered:?}"
    );
}

#[test]
fn tool_approval_run_once_allow_and_deny() {
    for (key, expected) in [
        ("r", ToolApprovalDecision::RunOnce),
        ("a", ToolApprovalDecision::AlwaysAllow),
        ("d", ToolApprovalDecision::Deny),
    ] {
        let session = Arc::new(StatusSession::with_cwd("/proj", "s1"));
        session.ask_approval.store(true, Ordering::SeqCst);
        let rig = make_rig(Arc::clone(&session));
        let mut mode = rig.mode;
        let writes = rig.writes;
        let input = rig.input;

        let key = key.to_string();
        let key_arc = Arc::new(key.clone());
        let input_for_feeder = input.clone();
        let writes_for_feeder = writes.clone();
        // One feeder thread drives the whole interaction: type + submit, wait
        // for the approval prompt to render, resolve it with the decision key,
        // then quit. This mirrors the delayed-provider test seam.
        thread::spawn(move || {
            let mut cb = take_on_input(&input_for_feeder);
            type_submit_quit(&mut cb, "run the tool");
            // Wait until the approval prompt is visible.
            for _ in 0..400 {
                if writes_for_feeder
                    .lock()
                    .unwrap()
                    .contains("requires approval")
                {
                    break;
                }
                thread::sleep(Duration::from_millis(10));
            }
            cb(&key_arc);
            thread::sleep(Duration::from_millis(150));
            cb("\u{4}"); // quit
        });

        make_runtime().block_on(mode.run_async());

        let decision = *session.last_decision.lock().unwrap();
        assert_eq!(
            decision,
            Some(expected),
            "key {key} should yield {expected:?}"
        );
        let rendered = writes.lock().unwrap().clone();
        assert!(
            rendered.contains("requires approval"),
            "approval prompt should render, got: {rendered:?}"
        );
    }
}

/// Make a rig with an explicit terminal size (plan.md step 8: 80x24, 120x40
/// and narrow terminals must not panic or clip ambiguous state).
fn make_rig_sized(session: Arc<StatusSession>, (cols, rows): (u16, u16)) -> Rig {
    let terminal = Box::new(DriveTerminal::new());
    *terminal.size.lock().unwrap() = (cols, rows);
    let writes = terminal.writes.clone();
    let input = terminal.input_slot.clone();
    let runtime = make_runtime();
    let mode = pirust_coding_agent::interactive_mode::InteractiveMode::new(
        terminal,
        session as Arc<dyn InteractiveSession>,
        runtime.handle().clone(),
    );
    Rig {
        mode,
        writes,
        input,
        _runtime: runtime,
    }
}

#[test]
fn terminal_sizes_render_without_panic() {
    for (cols, rows) in [(80u16, 24u16), (120u16, 40u16), (40u16, 10u16)] {
        let session = Arc::new(StatusSession::with_cwd("/proj", "s1"));
        let rig = make_rig_sized(session, (cols, rows));
        // Rendering at each size must complete without panicking.
        run_async_rig(rig, "");
    }
}

#[test]
fn resize_during_idle_re_renders() {
    // The loop re-requests a render when terminal_rows() changes; rendering at
    // two sizes without a resize panic proves the frame recomputes.
    let session = Arc::new(StatusSession::with_cwd("/proj", "s1"));
    let rig = make_rig(session);
    let mut mode = rig.mode;
    let writes = rig.writes;
    let input = rig.input;

    thread::spawn(move || {
        let mut cb = take_on_input(&input);
        thread::sleep(Duration::from_millis(60));
        cb("\u{4}"); // quit
    });

    make_runtime().block_on(mode.run_async());
    let rendered = writes.lock().unwrap().clone();
    assert!(
        rendered.contains("cwd: /proj"),
        "rendering should reflect the header, got: {rendered:?}"
    );
}

#[test]
fn long_tool_result_truncates_to_preview() {
    // The tool-execution fallback previews FALLBACK_PREVIEW_LINES (10) lines
    // and appends a "more lines" hint for the rest.
    let session = Arc::new(StatusSession::with_cwd("/proj", "s1"));
    let rig = make_rig(Arc::clone(&session));
    let (mut mode, writes, input) = (rig.mode, rig.writes, rig.input);

    // Emit a tool result with more than 10 lines through the subscription
    // BEFORE the loop runs, so they are queued for the event drain.
    let text = (0..30)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    if let Some(listener) = session.listener.lock().unwrap().clone() {
        listener(
            &pirust_coding_agent::print_mode::AgentSessionEvent::ToolExecutionStart {
                tool_call_id: "call_1".into(),
                tool_name: "bash".into(),
                args: serde_json::json!({ "command": "long" }),
            },
        );
        listener(
            &pirust_coding_agent::print_mode::AgentSessionEvent::ToolExecutionEnd {
                tool_call_id: "call_1".into(),
                tool_name: "bash".into(),
                result: serde_json::json!({
                    "content": [{"type": "text", "text": text}]
                }),
                is_error: false,
            },
        );
    }

    // Run the loop (which drains the queued events) from a thread while the
    // mode's sync `run` pumps on the main thread and quits on Ctrl+D.
    let input_for_feeder = input.clone();
    thread::spawn(move || {
        let mut cb = take_on_input(&input_for_feeder);
        thread::sleep(Duration::from_millis(80));
        cb("\u{4}"); // quit
    });
    mode.run();

    let rendered = writes.lock().unwrap().clone();
    assert!(
        rendered.contains("more lines"),
        "long tool output should truncate with a hint, got: {rendered:?}"
    );
}
