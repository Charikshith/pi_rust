//! Interactive mode (feat-007 Wave 2) — streaming turn display.
//!
//! Port of `packages/coding-agent/src/modes/interactive/interactive-mode.ts`
//! (6,008 lines) — this wave adds the **streaming turn display**: on submit,
//! `session.prompt` runs (blocking the loop, exactly like Pi's
//! `await this.session.prompt`); the session's event subscription forwards
//! `message_start`/`message_update`/`message_end`/`agent_end` to the TUI's
//! chat container. Assistant text streams into a live `Text` component;
//! user messages and the final assistant message are committed as lines.
//! Full Markdown/thinking/tool rendering, slash commands, model switcher
//! are later waves.
//!
//! ## Design: synchronous TUI + async turns
//!
//! `pirust-tui` is `!Send` (`Rc`-based, mirroring JS object identity) and its
//! `ProcessTerminal` reader thread calls `on_input` from a background thread.
//! The loop therefore mirrors Pi's event-loop structure: the `on_input`
//! callback (must be `Send`) pushes raw sequences into a
//! [`std::sync::mpsc`] channel; the main thread drains the channel and feeds
//! [`TUI::handle_input`] — the same caller-owns-the-loop adaptation the TUI
//! crate documents for its own deferred timers.
//!
//! Session events take the same bridge: the agent loop thread (which calls
//! the subscription listener) pushes into an event channel; the main loop
//! drains it and renders into the chat container. The turn itself is `async`
//! (`PrintModeSession::prompt`), so the loop blocks on it via a
//! `tokio::runtime::Handle` passed in — matching how `main.rs` already owns
//! the runtime.
//!
//! ```text
//! terminal reader thread ──input──▶ main loop ──▶ tui.handle_input ──▶ editor
//! agent loop thread ──events──▶ main loop ──▶ chat container render
//!                                        └──▶ submit ──▶ block_on(prompt)
//! ```

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;

use pirust_tui::components::text::Text;
use pirust_tui::editor::Editor;
use pirust_tui::tui::{SharedComponent, TUI};

use crate::print_mode::{AgentSessionEvent, PrintModeSession};

/// The interactive session runner.
pub struct InteractiveMode {
    tui: Rc<RefCell<TUI>>,
    /// Raw input sequences from the terminal reader thread.
    input_rx: Receiver<String>,
    /// Submitted prompts from the editor's `on_submit`.
    submit_rx: Receiver<String>,
    _submit_tx: Sender<String>,
    /// Session events from the agent loop thread (subscription listener).
    event_rx: Receiver<AgentSessionEvent>,
    _event_tx: Sender<AgentSessionEvent>,
    /// Set when the user quits (Ctrl+D on an empty editor).
    quit: Arc<AtomicBool>,
    /// The async session (driven via `block_on` on the runtime handle).
    session: Arc<dyn PrintModeSession>,
    runtime: tokio::runtime::Handle,
    /// Chat container children: user messages, the streaming assistant
    /// message, and separators. The streaming message is always the last
    /// child while a turn is in flight.
    chat: Rc<RefCell<pirust_tui::tui::Container>>,
    streaming_text: Option<Rc<RefCell<Text>>>,
}

impl InteractiveMode {
    /// Build the TUI + editor + chat container, start the terminal reader
    /// thread, and subscribe to the session's events.
    pub fn new(
        terminal: Box<dyn pirust_tui::terminal::Terminal>,
        session: Arc<dyn PrintModeSession>,
        runtime: tokio::runtime::Handle,
    ) -> Self {
        let (input_tx, input_rx) = channel::<String>();

        // Start the terminal reader thread, feeding the channel.
        let mut terminal = terminal;
        terminal.start(
            Box::new(move |data: &str| {
                let _ = input_tx.send(data.to_string());
            }),
            Box::new(|| {}),
        );

        let tui = Rc::new(RefCell::new(TUI::new(terminal, Some(false))));
        tui.borrow_mut().start();

        // Chat container + editor, mounted in the document root.
        let chat = Rc::new(RefCell::new(pirust_tui::tui::Container::new()));
        let chat_shared: SharedComponent = Rc::clone(&chat) as SharedComponent;
        tui.borrow_mut().add_child(chat_shared);

        let editor = Rc::new(RefCell::new(Editor::new(
            Rc::clone(&tui),
            Box::new(|s| s.to_string()),
            Default::default(),
        )));
        let (submit_tx, submit_rx) = channel::<String>();
        {
            let tx = submit_tx.clone();
            let mut editor_ref = editor.borrow_mut();
            editor_ref.on_submit = Some(Box::new(move |text: &str| {
                let _ = tx.send(text.to_string());
            }));
        }
        let editor_shared: SharedComponent = Rc::clone(&editor) as SharedComponent;
        tui.borrow_mut().add_child(editor_shared);
        tui.borrow_mut()
            .set_focus(Some(Rc::clone(&editor) as SharedComponent));

        // Ctrl+D quits when the editor is empty (Pi's `handleCtrlD`, enforced
        // by the editor being empty). The input listener runs before the
        // focused component; it consumes only when empty, so a non-empty
        // editor still gets delete-forward.
        let quit = Arc::new(AtomicBool::new(false));
        {
            let quit = Arc::clone(&quit);
            let editor = Rc::clone(&editor);
            tui.borrow_mut()
                .add_input_listener(Box::new(move |data: &str| {
                    if pirust_tui::keys::matches_key(data, "ctrl+d")
                        && editor.borrow().get_text().is_empty()
                    {
                        quit.store(true, Ordering::Relaxed);
                        return Some(pirust_tui::tui::InputListenerResult {
                            consume: true,
                            data: None,
                        });
                    }
                    None
                }));
        }

        // Subscribe to session events, bridging the agent thread → channel.
        let (event_tx, event_rx) = channel::<AgentSessionEvent>();
        {
            let tx = event_tx.clone();
            let subscription = session.subscribe(Arc::new(move |event: &AgentSessionEvent| {
                let _ = tx.send(event.clone());
            }));
            // Keep the subscription alive for the mode's lifetime: the
            // session's `subscribe` returns an unsubscribe thunk we must hold.
            // (`SingleTurnSession::subscribe`'s thunk is a documented no-op,
            // but the API requires it be kept.)
            std::mem::forget(subscription);
        }

        Self {
            tui,
            input_rx,
            submit_rx,
            _submit_tx: submit_tx,
            event_rx,
            _event_tx: event_tx,
            quit,
            session,
            runtime,
            chat,
            streaming_text: None,
        }
    }

    /// Drive the loop until quit. `prompt` runs one turn for the user's
    /// text (blocking on the async session via the runtime handle), while
    /// session events stream into the chat container.
    pub fn run(&mut self) {
        loop {
            if self.quit.load(Ordering::Relaxed) {
                break;
            }
            // Drain raw input from the terminal reader thread.
            while let Ok(data) = self.input_rx.try_recv() {
                self.tui.borrow_mut().handle_input(&data);
            }
            // Drain session events, rendering into the chat container.
            let mut pending_turns = Vec::new();
            while let Ok(event) = self.event_rx.try_recv() {
                self.render_event(&event);
            }
            // Drain submitted prompts — collect first, then run OUTSIDE the
            // TUI borrow (a turn renders into the TUI, which borrows it
            // again; re-entrant borrow panics).
            while let Ok(text) = self.submit_rx.try_recv() {
                if !text.is_empty() {
                    pending_turns.push(text);
                }
            }
            // Render any pending frame before the turn (editor already
            // cleared itself on submit).
            self.tui.borrow_mut().poll();
            for text in pending_turns {
                self.run_turn(&text);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Run one user turn: add the user's text to the chat, then block on
    /// `session.prompt` (events stream in via the subscription during the
    /// turn and are rendered by [`Self::render_event`]).
    fn run_turn(&mut self, text: &str) {
        // User message line (Pi adds the user message on `message_start`
        // with role user; we mirror that by appending it before the turn).
        let user_line = format!("▶ {text}");
        let user_text = Rc::new(RefCell::new(Text::new(user_line, 0, 0)));
        self.chat
            .borrow_mut()
            .add_child(user_text as SharedComponent);
        self.tui.borrow_mut().request_render(true);
        self.tui.borrow_mut().poll();

        // Run the turn (blocks the loop, like Pi's `await prompt`).
        let session = Arc::clone(&self.session);
        let text = text.to_string();
        let _ = self
            .runtime
            .block_on(async move { session.prompt(&text, None).await });
    }

    /// Render one session event into the chat container.
    fn render_event(&mut self, event: &AgentSessionEvent) {
        match event {
            AgentSessionEvent::MessageStart { message } => {
                let role = message.get("role").and_then(|r| r.as_str());
                if role == Some("user") {
                    // User messages also arrive as events; `run_turn` already
                    // added them, so skip duplicates (Pi adds them on
                    // message_start; we did it in run_turn).
                } else if role == Some("assistant") {
                    // Begin streaming: a fresh Text that updates on
                    // message_update.
                    self.streaming_text = Some(Rc::new(RefCell::new(Text::new("", 0, 0))));
                    let st = self.streaming_text.as_ref().unwrap().clone();
                    self.chat.borrow_mut().add_child(st as SharedComponent);
                }
            }
            AgentSessionEvent::MessageUpdate { message, .. } => {
                if let Some(st) = &self.streaming_text {
                    let text = assistant_text(message);
                    st.borrow_mut().set_text(text);
                    self.tui.borrow_mut().request_render(true);
                }
            }
            AgentSessionEvent::MessageEnd { message } => {
                if let Some(st) = self.streaming_text.take() {
                    let text = assistant_text(message);
                    st.borrow_mut().set_text(text);
                    self.tui.borrow_mut().request_render(true);
                }
            }
            AgentSessionEvent::AgentEnd { .. } => {
                // Turn over: a blank separator line.
                let sep = Rc::new(RefCell::new(pirust_tui::components::spacer::Spacer::new(1)));
                self.chat.borrow_mut().add_child(sep as SharedComponent);
                self.tui.borrow_mut().request_render(true);
            }
            _ => {}
        }
    }

    /// Feed a raw sequence into the TUI directly (tests, or when the
    /// terminal isn't a real reader).
    pub fn handle_input(&mut self, data: &str) {
        self.tui.borrow_mut().handle_input(data);
    }

    /// Render the current frame (tests).
    pub fn poll(&mut self) {
        self.tui.borrow_mut().poll();
    }
}

impl Drop for InteractiveMode {
    fn drop(&mut self) {
        self.tui.borrow_mut().stop();
    }
}

/// Extract the assistant's streaming text from a message `Value`:
/// `content[].text` joined, matching the assistant-message component's
/// text-block handling (trimmed). Tool calls / thinking are ignored this
/// wave.
fn assistant_text(message: &serde_json::Value) -> String {
    let Some(content) = message.get("content").and_then(|c| c.as_array()) else {
        return String::new();
    };
    content
        .iter()
        .filter_map(|block| {
            if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                block.get("text").and_then(|t| t.as_str())
            } else {
                None
            }
        })
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}
