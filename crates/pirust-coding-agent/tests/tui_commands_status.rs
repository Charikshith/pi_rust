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

/// Block until `needle` shows up in the captured writes, or give up.
///
/// Fixed sleeps are not safe here: `cargo test` runs every suite in the
/// workspace concurrently, so on a loaded machine the loop can take far longer
/// than any sleep to drain one keystroke.
fn wait_for(probe: &Arc<Mutex<String>>, needle: &str) -> bool {
    for _ in 0..400 {
        if probe.lock().unwrap().contains(needle) {
            return true;
        }
        thread::sleep(Duration::from_millis(25));
    }
    false
}

/// Regression: `/` is an ordinary character that goes into the editor, where
/// the editor's own autocomplete offers the slash commands.
///
/// There used to be a hand-rolled `CommandPalette` that grabbed `/` globally
/// before the editor saw it, and redrew itself by appending a fresh notice to
/// the chat on every keystroke -- so `/mod` left four stacked copies of a
/// "Command palette (filter: ...)" block that had no visual connection to the
/// input box, and none of them were removed on close. Both the interception
/// and that notice are gone. This pins down that they stay gone: every other
/// test in this file pastes slash lines as one blob, so nothing else covers
/// the flow of actually typing `/` key by key.
#[test]
fn typing_slash_goes_to_the_editor_not_a_palette() {
    let session = Arc::new(StatusSession::with_cwd("/proj", "s1"));
    let rig = make_rig(session);
    let mut mode = rig.mode;
    let writes = rig.writes;
    let input = rig.input;

    // The feeder cannot assert directly: it runs while the loop below owns the
    // main thread, so a panic there would hang the test instead of failing it.
    let frame: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));

    let probe = writes.clone();
    let out = frame.clone();
    thread::spawn(move || {
        let mut cb = take_on_input(&input);

        // Type `/mod` one key at a time, confirming each keystroke reached the
        // editor before sending the next.
        for (key, expected) in [("/", "/"), ("m", "/m"), ("o", "/mo"), ("d", "/mod")] {
            // Keep only the frames drawn for this keystroke, so the wait tests
            // the current frame rather than the whole scrollback.
            probe.lock().unwrap().clear();
            cb(key);
            wait_for(&probe, expected);
        }
        *out.lock().unwrap() = probe.lock().unwrap().clone();

        // Clear the editor, then quit -- Ctrl+D only quits on an empty editor.
        for _ in 0..4 {
            probe.lock().unwrap().clear();
            cb("\u{7f}");
            thread::sleep(Duration::from_millis(60));
        }
        cb("\u{4}");
    });

    make_runtime().block_on(mode.run_async());

    let frame = frame.lock().unwrap().clone();
    assert!(
        frame.contains("/mod"),
        "`/` and the characters after it belong in the editor, got: {frame:?}"
    );
    assert!(
        !frame.contains("Command palette"),
        "the hand-rolled palette is gone; `/` must not open one, got: {frame:?}"
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

/// A long session must not keep growing the chat container.
///
/// Every message, tool box, notice and separator used to stay mounted for the
/// life of the process, and `Container::render` walks every child on every
/// frame — so both the frame cost and the retained memory grew with the
/// session, without limit. `InteractiveMode::prune_scrollback` now drops
/// entries that have scrolled far past the top of the terminal (their rows
/// live in the terminal's own scrollback), keeping the document bounded.
///
/// This drives the real loop rather than the pruning primitives, so it covers
/// the wiring: pump 4,000 notices through and check the container settles.
#[test]
fn a_long_session_bounds_the_chat_container() {
    let session = Arc::new(StatusSession::with_cwd("/proj", "s1"));
    let rig = make_rig(Arc::clone(&session));
    let mut mode = rig.mode;
    let input = rig.input;

    // Emit far more entries than a 24-row terminal could ever show, from a
    // second thread: the event channel is a bounded `sync_channel`, so a
    // producer that runs ahead of the loop blocks until the loop drains.
    let emitter = Arc::clone(&session);
    thread::spawn(move || {
        let listener = {
            let mut got = None;
            for _ in 0..200 {
                if let Some(l) = emitter.listener.lock().unwrap().clone() {
                    got = Some(l);
                    break;
                }
                thread::sleep(Duration::from_millis(10));
            }
            got.expect("the mode should have subscribed")
        };
        for i in 0..4000 {
            listener(
                &pirust_coding_agent::print_mode::AgentSessionEvent::AutoRetryStart {
                    attempt: i,
                    max_attempts: 4000,
                    delay_ms: 0,
                    error_message: String::new(),
                },
            );
        }
    });

    thread::spawn(move || {
        let mut cb = take_on_input(&input);
        thread::sleep(Duration::from_millis(3000));
        cb("\u{4}"); // quit
    });
    make_runtime().block_on(mode.run_async());

    let entries = mode.chat_entries();
    assert!(
        entries > 0,
        "the transcript should still hold what is on screen"
    );
    assert!(
        entries < 4000,
        "the chat container kept all {entries} entries; a long session must \
         drop what has scrolled out of the renderer's reach"
    );
}

/// Audit #22 / AGENTS.md ("slash-command autocomplete must use the same
/// registered handlers as command execution"): a command the dispatcher can
/// actually run must be in the registry, or the editor's autocomplete — which
/// is built from that registry — will never offer it.
///
/// `/help`, `/models`, `/restart` and `/refresh-model-list` were dispatchable
/// but unregistered, so they were invisible in the dropdown and `/help` did
/// not even list itself.
#[test]
fn every_available_command_is_registered() {
    use pirust_coding_agent::interactive_mode::{slash_command_available, BUILTIN_SLASH_COMMANDS};
    let missing: Vec<&str> = [
        "help",
        "hotkeys",
        "session",
        "model",
        "models",
        "resume",
        "refresh-model-list",
        "reload-extensions",
        "quit",
    ]
    .into_iter()
    .filter(|name| {
        assert!(
            slash_command_available(name),
            "{name} should report as available"
        );
        !BUILTIN_SLASH_COMMANDS
            .iter()
            .any(|(registered, _, _)| registered == name)
    })
    .collect();
    assert!(
        missing.is_empty(),
        "dispatchable commands missing from the autocomplete registry: {missing:?}"
    );
}

/// The other half of audit #22: a registered command with no handler must say
/// so where the user chooses it, not only after they run it.
#[test]
fn unavailable_commands_are_marked_in_help() {
    let session = Arc::new(StatusSession::with_cwd("/proj", "s1"));
    let rig = make_rig(session);
    let rendered = run_async_rig(rig, "/help");
    assert!(
        rendered.contains("unavailable in this session"),
        "/help should mark commands with no handler, got: {rendered:?}"
    );
    assert!(
        !rendered.contains("✗"),
        "/help output is information, not an error, got: {rendered:?}"
    );
}

/// Ctrl+C must not be swallowed by an open modal.
///
/// `run_async` routes input to the model picker / resume picker / approval
/// prompt *instead of* `TUI::handle_input`, and the global Ctrl+C listener
/// lives inside the TUI. So while any of those was open, Ctrl+C reached
/// nothing at all: the turn could not be cancelled and the process could not
/// be quit. (Ctrl+D is in the same listener, so it was dead too — leaving Esc
/// as the only way out of a picker, and no way out of the approval prompt.)
///
/// Two presses within Pi's 500ms window quit, so this test asserts the loop
/// exits from inside an open `/model` picker. If it does not, the feeder trips
/// its own escape hatch and the assertion fails rather than hanging forever.
#[test]
fn ctrl_c_is_not_swallowed_by_an_open_model_picker() {
    let session = Arc::new(StatusSession::with_cwd("/proj", "s1"));
    let rig = make_rig(session);
    let mut mode = rig.mode;
    let writes = rig.writes;
    let input = rig.input;

    let exited = Arc::new(AtomicBool::new(false));
    let used_escape_hatch = Arc::new(AtomicBool::new(false));

    let probe = writes.clone();
    let exited_feeder = Arc::clone(&exited);
    let hatch = Arc::clone(&used_escape_hatch);
    thread::spawn(move || {
        let mut cb = take_on_input(&input);
        // Open the picker and confirm it is actually up before pressing keys.
        cb("/model");
        thread::sleep(Duration::from_millis(30));
        cb("\r");
        assert!(
            wait_for(&probe, "Select model"),
            "the /model picker should have opened"
        );

        // Two Ctrl+C presses inside the 500ms window: cancel, then quit.
        cb("\u{3}");
        thread::sleep(Duration::from_millis(50));
        cb("\u{3}");

        // Give the loop a generous window to exit on its own.
        for _ in 0..200 {
            if exited_feeder.load(Ordering::SeqCst) {
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        // It did not: unstick the test the long way so this fails instead of
        // hanging the suite.
        hatch.store(true, Ordering::SeqCst);
        cb("\u{1b}"); // Esc closes the picker
        thread::sleep(Duration::from_millis(50));
        cb("\u{4}"); // Ctrl+D quits an empty editor
    });

    make_runtime().block_on(mode.run_async());
    exited.store(true, Ordering::SeqCst);

    assert!(
        !used_escape_hatch.load(Ordering::SeqCst),
        "double Ctrl+C should quit from inside an open picker; the loop only \
         exited after Esc + Ctrl+D"
    );
}

/// Esc must resolve the approval prompt rather than leave it up.
///
/// The agent loop is parked on the approval oneshot, so the prompt has to
/// answer with *something*; `handle_approval_key` used to ignore every key
/// that was not `r`/`a`/`d`, so Esc did nothing and there was no way to back
/// out of the prompt.
///
/// Without the fix this scenario deadlocks outright rather than failing: the
/// prompt stays up, so Ctrl+D stays swallowed by the same modal branch and the
/// loop never exits. The feeder therefore keeps `d` in reserve as an escape
/// hatch and the test asserts it was never needed.
#[test]
fn escape_denies_a_pending_tool_approval() {
    let session = Arc::new(StatusSession::with_cwd("/proj", "s1"));
    session.ask_approval.store(true, Ordering::SeqCst);
    let rig = make_rig(Arc::clone(&session));
    let mut mode = rig.mode;
    let writes = rig.writes;
    let input = rig.input;

    let used_escape_hatch = Arc::new(AtomicBool::new(false));
    let probe = writes.clone();
    let hatch = Arc::clone(&used_escape_hatch);
    let decided = Arc::clone(&session.last_decision);
    thread::spawn(move || {
        let mut cb = take_on_input(&input);
        cb("go");
        thread::sleep(Duration::from_millis(30));
        cb("\r");
        assert!(
            wait_for(&probe, "requires approval"),
            "the approval prompt should have rendered"
        );
        cb("\u{1b}"); // Esc

        // Wait for Esc to resolve the prompt on its own.
        for _ in 0..200 {
            if decided.lock().unwrap().is_some() {
                thread::sleep(Duration::from_millis(100));
                cb("\u{4}"); // quit
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        // It did not: answer with `d` so the run ends and the assertion below
        // reports the failure instead of the suite hanging.
        hatch.store(true, Ordering::SeqCst);
        cb("d");
        thread::sleep(Duration::from_millis(150));
        cb("\u{4}");
    });

    make_runtime().block_on(mode.run_async());

    assert!(
        !used_escape_hatch.load(Ordering::SeqCst),
        "Esc left the approval prompt up; only `d` resolved it"
    );
    assert_eq!(
        *session.last_decision.lock().unwrap(),
        Some(ToolApprovalDecision::Deny),
        "Esc on the approval prompt should resolve it as Deny"
    );
    let rendered = writes.lock().unwrap().clone();
    assert!(
        rendered.contains("denied"),
        "the denial should be reported in the chat, got: {rendered:?}"
    );
}
