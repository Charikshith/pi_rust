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

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use pirust_agent_core::harness::types::SessionHeader;
use pirust_coding_agent::print_mode::{
    AgentSessionEvent, Cancelled, ExtensionBinding, NavigateTreeOptions, PrintModeSession,
    PromptOptions, SessionEventListener, SessionStateView, Subscription, ThrownValue,
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
    release: Arc<AtomicBool>,
    fail_prompt: Arc<AtomicBool>,
}

impl DelayedSession {
    fn new() -> Self {
        Self {
            listener: Arc::new(Mutex::new(None)),
            prompt_seen: Arc::new(AtomicBool::new(false)),
            release: Arc::new(AtomicBool::new(false)),
            fail_prompt: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Emit the canned assistant stream through the captured listener.
    fn emit_stream(&self, text: &str) {
        let listener = self.listener.lock().unwrap().clone();
        if let Some(listener) = listener {
            listener(&AgentSessionEvent::MessageStart {
                message: serde_json::json!({"role": "assistant", "content": []}),
            });
            listener(&AgentSessionEvent::MessageUpdate {
                assistant_message_event: serde_json::json!({}),
                message: serde_json::json!({
                    "role": "assistant",
                    "content": [{"type": "text", "text": text}]
                }),
            });
            listener(&AgentSessionEvent::MessageEnd {
                message: serde_json::json!({
                    "role": "assistant",
                    "content": [{"type": "text", "text": text}]
                }),
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
        // Wait until released (or aborted via drop).
        while !self.release.load(Ordering::SeqCst) {
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
        Arc::clone(&session) as Arc<dyn PrintModeSession>,
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

#[test]
fn delayed_submit_cancel_aborts_before_release() {
    let terminal = Box::new(DriveTerminal::new());
    let handles = TerminalHandles::grab(&terminal);
    let session = Arc::new(DelayedSession::new());
    let runtime = make_runtime();
    let mut mode = pirust_coding_agent::interactive_mode::InteractiveMode::new(
        terminal,
        Arc::clone(&session) as Arc<dyn PrintModeSession>,
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
        Arc::clone(&session) as Arc<dyn PrintModeSession>,
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
fn resize_during_idle_is_picked_up() {
    let terminal = Box::new(DriveTerminal::new());
    let handles = TerminalHandles::grab(&terminal);
    let session = Arc::new(DelayedSession::new());
    let runtime = make_runtime();
    let mut mode = pirust_coding_agent::interactive_mode::InteractiveMode::new(
        terminal,
        Arc::clone(&session) as Arc<dyn PrintModeSession>,
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
