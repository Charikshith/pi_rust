//! Interactive mode (feat-007 Wave 1) — the `pi`-style TUI loop.
//!
//! Port of `packages/coding-agent/src/modes/interactive/interactive-mode.ts`
//! (6,008 lines) — this wave is the **scaffold**: launch the TUI, show an
//! Editor prompt, run a turn on submit, loop. Full streaming/tool rendering,
//! slash commands, model switcher, trust prompts etc. are later waves.
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
//! ```text
//! terminal reader thread ──channel──▶ main loop ──▶ tui.handle_input ──▶ editor
//!                                                    │
//!                                                    └──▶ on_submit ──▶ session.prompt ──▶ render
//! ```
//!
//! The turn itself is `async` (`PrintModeSession::prompt`), so the loop
//! blocks on it via a `tokio::runtime::Handle` passed in — matching how
//! `main.rs` already owns the runtime.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;

use pirust_tui::editor::Editor;
use pirust_tui::tui::{SharedComponent, TUI};

/// The interactive session runner.
pub struct InteractiveMode {
    tui: Rc<RefCell<TUI>>,
    /// Raw input sequences from the terminal reader thread.
    input_rx: Receiver<String>,
    /// Submitted prompts from the editor's `on_submit`.
    submit_rx: Receiver<String>,
    _submit_tx: Sender<String>,
    /// Set when the user quits (Ctrl+D on an empty editor).
    quit: Arc<AtomicBool>,
}

impl InteractiveMode {
    /// Build the TUI + editor, then start the terminal reader thread.
    ///
    /// `terminal` is started with an `on_input` callback that forwards raw
    /// sequences into a channel this struct drains in [`Self::run`] — the
    /// `!Send` TUI never touches the background thread.
    pub fn new(terminal: Box<dyn pirust_tui::terminal::Terminal>) -> Self {
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

        // The editor is mounted in the same TUI it holds a handle to.
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

        Self {
            tui,
            input_rx,
            submit_rx,
            _submit_tx: submit_tx,
            quit,
        }
    }

    /// Drive the loop until quit. `prompt` runs one turn for the user's
    /// text; it should block on the async session turn (via a runtime
    /// handle) and render output into the TUI's container.
    pub fn run<F>(&mut self, mut prompt: F)
    where
        F: FnMut(String),
    {
        loop {
            if self.quit.load(Ordering::Relaxed) {
                break;
            }
            // Drain raw input from the terminal reader thread.
            while let Ok(data) = self.input_rx.try_recv() {
                self.tui.borrow_mut().handle_input(&data);
            }
            // Drain submitted prompts — collect first, then run OUTSIDE the
            // TUI borrow (a turn renders into the TUI, which borrows it
            // again; re-entrant borrow panics).
            let mut pending = Vec::new();
            while let Ok(text) = self.submit_rx.try_recv() {
                if !text.is_empty() {
                    pending.push(text);
                }
            }
            // Render any pending frame before the turn (editor already
            // cleared itself on submit).
            self.tui.borrow_mut().poll();
            for text in pending {
                prompt(text);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Feed a raw sequence into the TUI directly (tests, or when the
    /// terminal isn't a real reader).
    pub fn handle_input(&mut self, data: &str) {
        self.tui.borrow_mut().handle_input(data);
    }
}

impl Drop for InteractiveMode {
    fn drop(&mut self) {
        self.tui.borrow_mut().stop();
    }
}
