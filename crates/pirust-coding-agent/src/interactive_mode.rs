//! Interactive mode (feat-007 Wave 2) — streaming turn display.
//!
//! Port of `packages/coding-agent/src/modes/interactive/interactive-mode.ts`
//! (6,008 lines) — this wave adds the **streaming turn display**: on submit,
//! `session.prompt` runs as a background Tokio task while the loop continues
//! draining input and session events; the session's event subscription forwards
//! `message_start`/`message_update`/`message_end`/`agent_end` to the TUI's
//! chat container. Assistant text streams into a live `Text` component;
//! user messages and the final assistant message are committed as lines.
//! Full Markdown/thinking/tool rendering, slash commands, model switcher
//! are later waves.
//!
//! ## Design: async TUI loop + async turns
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
//!                                        └──▶ submit ──▶ spawn(prompt)
//! ```

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;

use tokio::task::JoinHandle;

use pirust_tui::components::text::Text;
use pirust_tui::editor::Editor;
use pirust_tui::tui::{SharedComponent, TUI};

use crate::interactive_theme::{self, dark};
use crate::print_mode::{
    AgentSessionEvent, PrintModeSession, ToolApprovalDecision, ToolApprovalRequest, TuiRuntimeInfo,
    TuiRuntimeStatus,
};

/// A session the TUI drives: the [`PrintModeSession`] turn API plus the
/// [`TuiRuntimeInfo`] status projection. Every real session (`SingleTurnSession`)
/// and every test stub implements both.
pub trait InteractiveSession: PrintModeSession + TuiRuntimeInfo {}
impl<T: PrintModeSession + TuiRuntimeInfo> InteractiveSession for T {}

/// The lifecycle of the active turn — plan.md step 2's explicit state machine.
/// Replaces the loose `Option<JoinHandle>` + bools with typed legal transitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnState {
    /// No turn is running.
    Idle,
    /// A prompt task is in flight.
    Running,
    /// The turn paused awaiting a tool-approval decision.
    AwaitingApproval,
    /// Ctrl+C/Esc was pressed; the turn task is being aborted.
    Cancelling,
    /// The turn was aborted by the user.
    Cancelled,
    /// The turn finished.
    Completed,
    /// The turn failed (provider/model/tool error).
    Failed,
}

/// A tool-approval request bridged from the agent thread to the UI loop,
/// carrying a oneshot to deliver the user's decision back.
struct ApprovalMessage {
    request: ToolApprovalRequest,
    respond: tokio::sync::oneshot::Sender<ToolApprovalDecision>,
}

/// One selectable entry in the slash-command palette (plan.md step 4).
#[derive(Debug, Clone)]
struct PaletteEntry {
    /// The command name without the leading `/`.
    name: String,
    /// The description shown under the name.
    description: String,
    /// Whether the command has a working handler in this session.
    available: bool,
}

/// The slash-command palette (plan.md step 4): `/` opens it, typed characters
/// filter by name, ↑/↓ navigate, Enter runs, Esc closes. Renders as a chat
/// notice; the loop routes keys to [`InteractiveMode::handle_palette_key`].
struct CommandPalette {
    entries: Vec<PaletteEntry>,
    filter: String,
    selected: usize,
}

impl CommandPalette {
    /// Build from the registered command list (plan.md step 4: palette
    /// entries come from the registry, not a second hardcoded list).
    fn from_registry() -> Self {
        let entries = BUILTIN_SLASH_COMMANDS
            .iter()
            .map(|(name, description, _)| PaletteEntry {
                name: name.to_string(),
                description: description.to_string(),
                available: is_command_available(name),
            })
            .collect();
        Self {
            entries,
            filter: String::new(),
            selected: 0,
        }
    }

    /// The entries matching the current filter.
    fn visible(&self) -> Vec<&PaletteEntry> {
        let filter = self.filter.to_ascii_lowercase();
        self.entries
            .iter()
            .filter(|e| e.name.to_ascii_lowercase().contains(&filter))
            .collect()
    }

    /// Render the palette as a multi-line string (chat notice + key hints).
    fn render(&self) -> String {
        let mut lines = vec![format!("Command palette (filter: {})", self.filter)];
        let visible = self.visible();
        if visible.is_empty() {
            lines.push("  (no matching commands)".to_string());
        }
        for (index, entry) in visible.iter().enumerate() {
            let marker = if index == self.selected.min(visible.len() - 1) {
                "▸"
            } else {
                " "
            };
            let availability = if entry.available {
                ""
            } else {
                " (unavailable)"
            };
            lines.push(format!(
                "{marker} /{} — {}{availability}",
                entry.name, entry.description
            ));
        }
        lines.push("↑/↓ navigate · Enter run · Esc close".to_string());
        lines.join("\n")
    }
}

/// The `/model` picker (plan.md step 5): filters the model catalog by
/// provider/model id, ↑/↓ navigate, Enter selects, Esc dismisses.
struct ModelPicker {
    /// (provider, model id) of each selectable model.
    models: Vec<(String, String)>,
    filter: String,
    selected: usize,
}

impl ModelPicker {
    fn render(&self) -> String {
        let mut lines = vec![format!("Select model (filter: {})", self.filter)];
        let visible = self.visible();
        if visible.is_empty() {
            lines.push("  (no matching models)".to_string());
        }
        for (index, (provider, model)) in visible.iter().enumerate() {
            let marker = if index == self.selected.min(visible.len().saturating_sub(1)) {
                "▸"
            } else {
                " "
            };
            lines.push(format!("{marker} {provider} / {model}"));
        }
        lines.push("↑/↓ navigate · Enter select · Esc dismiss".to_string());
        lines.join("\n")
    }

    fn visible(&self) -> Vec<&(String, String)> {
        let filter = self.filter.to_ascii_lowercase();
        self.models
            .iter()
            .filter(|(provider, model)| {
                provider.to_ascii_lowercase().contains(&filter)
                    || model.to_ascii_lowercase().contains(&filter)
            })
            .collect()
    }
}

/// The `/resume` picker (plan.md step 5): lists resumable sessions. The real
/// session store lives behind `SessionManager`; the TUI seam exposes it as
/// a simple (id, title, cwd) list the session implementation supplies.
struct ResumePicker {
    /// (session id, title, cwd) of each resumable session.
    sessions: Vec<(String, String, String)>,
    selected: usize,
}

impl ResumePicker {
    fn render(&self) -> String {
        let mut lines = vec!["Resume a session:".to_string()];
        if self.sessions.is_empty() {
            lines.push("  (no resumable sessions)".to_string());
        }
        for (index, (id, title, cwd)) in self.sessions.iter().enumerate() {
            let marker = if index == self.selected.min(self.sessions.len().saturating_sub(1)) {
                "▸"
            } else {
                " "
            };
            lines.push(format!("{marker} {title} ({id}) — {cwd}"));
        }
        lines.push("↑/↓ navigate · Enter resume · Esc dismiss".to_string());
        lines.join("\n")
    }
}

/// Whether a built-in command has a working handler in this session
/// (plan.md step 4: the palette marks unavailable commands).
fn is_command_available(name: &str) -> bool {
    matches!(
        name,
        "help"
            | "hotkeys"
            | "session"
            | "name"
            | "model"
            | "models"
            | "resume"
            | "compact"
            | "restart"
            | "new"
            | "refresh-model-list"
            | "quit"
    )
}

/// How many lines of a tool result to preview before truncating with
/// `... (N more lines)` — `FALLBACK_PREVIEW_LINES` (tool-execution.ts:15).
const FALLBACK_PREVIEW_LINES: usize = 10;

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
    _event_tx: std::sync::mpsc::SyncSender<AgentSessionEvent>,
    /// Set when the user quits (Ctrl+D on an empty editor).
    quit: Arc<AtomicBool>,
    /// Set by Ctrl+C/Esc and consumed by the async turn loop.
    cancel_requested: Arc<AtomicBool>,
    /// Set by `/` on an empty editor: open the slash-command palette.
    palette_requested: Arc<AtomicBool>,
    /// The async session used by background turn tasks.
    session: Arc<dyn InteractiveSession>,
    runtime: tokio::runtime::Handle,
    active_turn: Option<JoinHandle<Result<(), crate::print_mode::ThrownValue>>>,
    /// The turn state machine (plan.md step 2).
    turn_state: TurnState,
    /// Monotonic turn counter — attached to events so stale events from a
    /// cancelled/completed turn cannot bleed into the next one.
    turn_id: u64,
    /// The turn id the streaming assistant text belongs to.
    streaming_turn: Option<u64>,
    subscription: Option<crate::print_mode::Subscription>,
    /// Chat container children: user messages, the streaming assistant
    /// message, and separators. The streaming message is always the last
    /// child while a turn is in flight.
    chat: Rc<RefCell<pirust_tui::tui::Container>>,
    status: Rc<RefCell<Text>>,
    last_size: Option<u16>,
    /// Cached runtime status for the status line (refreshed on turn
    /// transitions and events).
    runtime_status: Option<TuiRuntimeStatus>,
    streaming_text: Option<Rc<RefCell<Text>>>,
    /// Tool executions in flight, keyed by tool call id
    /// (`pendingTools` map, interactive-mode.ts:468).
    pending_tools: HashMap<String, Rc<RefCell<ToolExecutionComponent>>>,
    /// Tool-approval bridge: the session's `before_tool_call` hook sends a
    /// request here and awaits the user's decision on a oneshot channel. The
    /// loop drains `approval_rx`, renders the prompt, and the decision keys
    /// resolve the pending oneshot.
    _approval_tx: Sender<ApprovalMessage>,
    approval_rx: Receiver<ApprovalMessage>,
    /// The tool call currently awaiting user approval, or `None`. While set,
    /// the next decision key (`r`/`a`/`d`) resolves `pending_approval.respond`
    /// and the agent loop unblocks.
    pending_approval: Option<ApprovalMessage>,
    /// The slash-command palette, `Some` while it is open. Input routes to the
    /// palette while it is focused; Esc closes it.
    palette: Option<CommandPalette>,
    /// The model picker, `Some` while it is open.
    model_picker: Option<ModelPicker>,
    /// The resume picker, `Some` while it is open.
    resume_picker: Option<ResumePicker>,
}

/// `ToolExecutionComponent` (tool-execution.ts) — a simplified but faithful
/// port: a `Box` (pending/success/error bg) with the tool name title + args
/// JSON, then the streaming result text (truncated preview).
pub struct ToolExecutionComponent {
    tool_name: String,
    args: serde_json::Value,
    expanded: bool,
    is_partial: bool,
    execution_started: bool,
    result: Option<ToolResult>,
}

/// The tool result (`tool-execution.ts` `result` field).
pub struct ToolResult {
    content: Vec<ToolResultContent>,
    is_error: bool,
}

/// A `content` block of a tool result.
pub struct ToolResultContent {
    text: String,
}

impl ToolExecutionComponent {
    /// `constructor` (tool-execution.ts:41).
    fn new(tool_name: String, args: serde_json::Value) -> Self {
        Self {
            tool_name,
            args,
            expanded: false,
            is_partial: true,
            execution_started: false,
            result: None,
        }
    }

    /// `setExpanded` (tool-execution.ts:211).
    fn set_expanded(&mut self, expanded: bool) {
        self.expanded = expanded;
    }

    /// `markExecutionStarted` (tool-execution.ts:163).
    fn mark_execution_started(&mut self) {
        self.execution_started = true;
    }

    /// `updateResult` (tool-execution.ts:175).
    fn update_result(&mut self, content: Vec<ToolResultContent>, is_error: bool, is_partial: bool) {
        self.result = Some(ToolResult { content, is_error });
        self.is_partial = is_partial;
    }

    /// The bg color for the current state (`updateDisplay`'s `bgFn`).
    fn bg_hex(&self) -> &'static str {
        if self.is_partial {
            dark::TOOL_PENDING_BG
        } else if self.result.as_ref().is_some_and(|r| r.is_error) {
            dark::TOOL_ERROR_BG
        } else {
            dark::TOOL_SUCCESS_BG
        }
    }

    /// `formatToolExecution` (tool-execution.ts:379) — the fallback render:
    /// bold tool name + args JSON + result output.
    fn format_tool_execution(&self) -> String {
        let mut text = self.tool_name.clone();
        let content = serde_json::to_string_pretty(&self.args).unwrap_or_default();
        if !content.is_empty() && content != "{}" && content != "null" {
            text.push_str("\n\n");
            text.push_str(&content);
        }
        if let Some(result) = &self.result {
            let output = result_text(&result.content);
            if !output.is_empty() {
                text.push('\n');
                // `createResultFallback` (tool-execution.ts:145-155): preview
                // the first N lines, truncating with a "more lines" hint.
                let lines: Vec<&str> = output.split('\n').collect();
                let display_lines = if self.expanded {
                    lines.len()
                } else {
                    lines.len().min(FALLBACK_PREVIEW_LINES)
                };
                for line in lines.iter().take(display_lines) {
                    text.push_str(line);
                    text.push('\n');
                }
                let remaining = lines.len().saturating_sub(display_lines);
                if remaining > 0 {
                    text.push_str(&format!("... ({remaining} more lines, to expand)"));
                }
            }
        }
        text
    }
}

impl pirust_tui::tui::Component for ToolExecutionComponent {
    /// `render` (tool-execution.ts:229-247) — no renderer definitions this
    /// wave (built-in tool definitions are feat-007 Wave 4/5 territory), so
    /// this is the fallback path: contentText with the current bg.
    fn render(&mut self, width: usize) -> Vec<String> {
        let text = self.format_tool_execution();
        if text.trim().is_empty() {
            return Vec::new();
        }
        let mut text = Text::with_bg_fn(text, 1, 1, interactive_theme::bg(self.bg_hex()));
        text.render(width)
    }

    fn invalidate(&mut self) {}
}

impl InteractiveMode {
    /// Build the TUI + editor + chat container, start the terminal reader
    /// thread, and subscribe to the session's events.
    pub fn new(
        terminal: Box<dyn pirust_tui::terminal::Terminal>,
        session: Arc<dyn InteractiveSession>,
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

        let runtime_status = session.runtime_status();
        let status = Rc::new(RefCell::new(Text::new(
            session_status(
                session.header().as_ref(),
                &runtime_status,
                TurnState::Idle,
                0,
            ),
            0,
            0,
        )));
        tui.borrow_mut()
            .add_child(Rc::clone(&status) as SharedComponent);

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

        // Slash-command autocomplete (`createBaseAutocompleteProvider`,
        // interactive-mode.ts:670): the BUILTIN_SLASH_COMMANDS list mapped
        // to `SlashCommand`.
        {
            let commands: Vec<pirust_tui::autocomplete::CommandOrItem> =
                crate::interactive_mode::BUILTIN_SLASH_COMMANDS
                    .iter()
                    .map(|(name, description, hint)| {
                        pirust_tui::autocomplete::CommandOrItem::Command(
                            pirust_tui::autocomplete::SlashCommand {
                                name: name.to_string(),
                                description: Some(description.to_string()),
                                argument_hint: hint.map(|h| h.to_string()),
                                get_argument_completions: None,
                            },
                        )
                    })
                    .collect();
            let provider = pirust_tui::autocomplete::CombinedAutocompleteProvider::new(
                commands,
                std::env::current_dir().unwrap_or_default(),
                None, // no fd binary this wave — see autocomplete.rs module docs
            );
            editor
                .borrow_mut()
                .set_autocomplete_provider(Some(Box::new(provider)));
        }

        // Ctrl+D quits when the editor is empty (Pi's `handleCtrlD`, enforced
        // by the editor being empty). Ctrl+C/Esc request cancellation while
        // a turn is active; the async loop owns the task transition.
        let quit = Arc::new(AtomicBool::new(false));
        let cancel_requested = Arc::new(AtomicBool::new(false));
        let palette_requested = Arc::new(AtomicBool::new(false));
        {
            let quit = Arc::clone(&quit);
            let cancel_requested = Arc::clone(&cancel_requested);
            let palette_requested = Arc::clone(&palette_requested);
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
                    if pirust_tui::keys::matches_key(data, "ctrl+c")
                        || pirust_tui::keys::matches_key(data, "escape")
                    {
                        cancel_requested.store(true, Ordering::Relaxed);
                        return Some(pirust_tui::tui::InputListenerResult {
                            consume: true,
                            data: None,
                        });
                    }
                    if data == "/" && editor.borrow().get_text().is_empty() {
                        palette_requested.store(true, Ordering::Relaxed);
                        return Some(pirust_tui::tui::InputListenerResult {
                            consume: true,
                            data: None,
                        });
                    }
                    None
                }));
        }

        // Subscribe to session events, bridging the agent thread → channel.
        // Bounded (plan.md step 2): the loop drains every iteration and
        // MessageUpdate events are coalesced to the latest text, so a burst of
        // stream updates cannot grow the queue unboundedly.
        let (event_tx, event_rx) = std::sync::mpsc::sync_channel::<AgentSessionEvent>(256);
        let subscription = {
            let tx = event_tx.clone();
            let subscription = session.subscribe(Arc::new(move |event: &AgentSessionEvent| {
                let _ = tx.send(event.clone());
            }));
            // Keep the subscription alive for the mode's lifetime and
            // unsubscribe it deterministically when the mode is dropped.
            subscription
        };

        // Tool-approval bridge: the session's `before_tool_call` hook sends a
        // request and awaits the user's decision. The loop drains the receiver
        // and resolves the pending oneshot from the decision keys.
        let (approval_tx, approval_rx) = channel::<ApprovalMessage>();
        {
            let tx = approval_tx.clone();
            session.set_tool_approval_decider(Arc::new(move |request: ToolApprovalRequest| {
                let (respond_tx, respond_rx) = tokio::sync::oneshot::channel();
                let _ = tx.send(ApprovalMessage {
                    request,
                    respond: respond_tx,
                });
                // Await the user's decision from the UI loop. The oneshot is
                // cancelled if the turn task is aborted (Ctrl+C), unblocking
                // the agent loop.
                Box::pin(async move { respond_rx.await.unwrap_or(ToolApprovalDecision::Deny) })
            }));
        }

        Self {
            tui,
            input_rx,
            submit_rx,
            _submit_tx: submit_tx,
            event_rx,
            _event_tx: event_tx,
            quit,
            cancel_requested,
            palette_requested,
            session,
            runtime,
            active_turn: None,
            turn_state: TurnState::Idle,
            turn_id: 0,
            streaming_turn: None,
            subscription: Some(subscription),
            chat,
            status,
            last_size: None,
            runtime_status: Some(runtime_status),
            streaming_text: None,
            pending_tools: HashMap::new(),
            _approval_tx: approval_tx,
            approval_rx,
            pending_approval: None,
            palette: None,
            model_picker: None,
            resume_picker: None,
        }
    }

    /// Drive the TUI without blocking while a model turn runs.
    pub async fn run_async(&mut self) {
        loop {
            if self.quit.load(Ordering::Relaxed) {
                break;
            }
            while let Ok(data) = self.input_rx.try_recv() {
                // An open modal owns input until dismissed.
                if self.model_picker.is_some() {
                    self.handle_model_picker_key(&data);
                } else if self.resume_picker.is_some() {
                    self.handle_resume_picker_key(&data);
                } else if self.palette.is_some() {
                    self.handle_palette_key(&data);
                } else if self.pending_approval.is_some() {
                    // While a tool awaits approval, a single decision key routes to
                    // the approval instead of the editor/loop.
                    self.handle_approval_key(&data);
                } else {
                    self.tui.borrow_mut().handle_input(&data);
                }
            }
            // Coalesce stream updates: only the freshest MessageUpdate is
            // rendered this iteration (plan.md step 2 backpressure).
            let mut latest_update: Option<AgentSessionEvent> = None;
            while let Ok(event) = self.event_rx.try_recv() {
                if let AgentSessionEvent::MessageUpdate { .. } = &event {
                    // Keep only the newest update; render others as-is.
                    if latest_update.replace(event).is_some() {
                        continue;
                    }
                } else {
                    self.render_event(&event);
                }
            }
            if let Some(update) = latest_update {
                self.render_event(&update);
            }
            while let Ok(approval) = self.approval_rx.try_recv() {
                self.show_approval(approval);
            }
            if self.palette_requested.swap(false, Ordering::Relaxed) {
                self.open_palette();
            }
            if self.cancel_requested.swap(false, Ordering::Relaxed) {
                if self.turn_state == TurnState::Running
                    || self.turn_state == TurnState::AwaitingApproval
                {
                    self.turn_state = TurnState::Cancelling;
                    if let Some(turn) = self.active_turn.as_ref() {
                        turn.abort();
                    }
                }
                self.tui.borrow_mut().request_render(true);
            }
            if self
                .active_turn
                .as_ref()
                .is_some_and(JoinHandle::is_finished)
            {
                let handle = self.active_turn.take().unwrap();
                let cancelled = self.turn_state == TurnState::Cancelling;
                match handle.await {
                    Ok(Ok(())) => {
                        self.finish_turn(if cancelled {
                            TurnState::Cancelled
                        } else {
                            TurnState::Completed
                        });
                    }
                    Ok(Err(error)) => {
                        self.finish_turn(TurnState::Failed);
                        if !cancelled {
                            self.show_error(error.console_message());
                        } else {
                            self.show_error("Request cancelled");
                        }
                    }
                    Err(_) => {
                        self.finish_turn(if cancelled {
                            TurnState::Cancelled
                        } else {
                            TurnState::Failed
                        });
                        if cancelled {
                            self.show_error("Request cancelled");
                        }
                    }
                }
            }
            while let Ok(text) = self.submit_rx.try_recv() {
                if !text.is_empty() && self.active_turn.is_none() {
                    if text.starts_with('/') {
                        self.dispatch_command(&text);
                    } else {
                        self.start_turn(text);
                    }
                }
            }
            // Detect a terminal resize (the TUI's own resize callback is not
            // wired this wave; the loop polls size directly and re-renders).
            let size = self.tui.borrow().terminal_rows();
            if Some(size) != self.last_size {
                self.last_size = Some(size);
                self.tui.borrow_mut().request_render(true);
            }
            self.tui.borrow_mut().poll();
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Finish the active turn: clear the streaming message and approval, drop
    /// stale pending tools, and refresh the status line.
    fn finish_turn(&mut self, outcome: TurnState) {
        self.streaming_text = None;
        self.streaming_turn = None;
        self.pending_tools.clear();
        self.pending_approval = None;
        self.active_turn = None;
        self.turn_state = outcome;
        self.refresh_status();
        self.tui.borrow_mut().request_render(true);
    }

    /// Compatibility wrapper for callers outside an async runtime. Production
    /// enters through `run_async`; this preserves the synchronous test seam.
    pub fn run(&mut self) {
        loop {
            if self.quit.load(Ordering::Relaxed) {
                break;
            }
            while let Ok(data) = self.input_rx.try_recv() {
                self.tui.borrow_mut().handle_input(&data);
            }
            while let Ok(event) = self.event_rx.try_recv() {
                self.render_event(&event);
            }
            let mut prompts = Vec::new();
            while let Ok(text) = self.submit_rx.try_recv() {
                if !text.is_empty() {
                    prompts.push(text);
                }
            }
            self.tui.borrow_mut().poll();
            for text in prompts {
                if text.starts_with('/') {
                    self.dispatch_command(&text);
                } else {
                    self.run_turn_sync(&text);
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Synchronous compatibility path; production uses `run_async`.
    fn run_turn_sync(&mut self, text: &str) {
        let user_text = Rc::new(RefCell::new(Text::new(format!("▶ {text}"), 0, 0)));
        self.chat
            .borrow_mut()
            .add_child(user_text as SharedComponent);
        self.tui.borrow_mut().request_render(true);
        self.tui.borrow_mut().poll();
        let session = Arc::clone(&self.session);
        let text = text.to_string();
        let runtime = self.runtime.clone();
        let _ = runtime.block_on(async move { session.prompt(&text, None).await });
    }

    /// Dispatch a submitted slash command. Unknown commands get an actionable
    /// error; known-but-unimplemented commands report why. Commands that
    /// require a picker open the palette/model/resume modals.
    fn dispatch_command(&mut self, text: &str) {
        let parts: Vec<&str> = text.split_whitespace().collect();
        let command = parts
            .first()
            .map(|c| c.trim_start_matches('/').to_ascii_lowercase())
            .unwrap_or_default();
        let arg = parts.get(1).copied();
        match command.as_str() {
            "help" => self.show_help(),
            "hotkeys" => self.show_hotkeys(),
            "session" => self.show_session_info(),
            "name" => self.set_session_name(arg),
            "model" => self.open_model_picker(),
            "models" => self.show_models_list(),
            "resume" => self.open_resume_picker(),
            "compact" => self.run_compact(),
            "restart" | "new" => self.show_error(
                "/restart is not available in this session — start a new `pirust` process",
            ),
            "refresh-model-list" => self.refresh_models(),
            "reload-extensions" => self.reload_extensions(),
            "quit" => {
                self.quit.store(true, Ordering::Relaxed);
            }
            _ => {
                if BUILTIN_SLASH_COMMANDS
                    .iter()
                    .any(|(name, _, _)| *name == command)
                {
                    self.show_error(format!("/{command} is not available in this session"));
                } else {
                    self.show_error(format!("Unknown command: /{command}"));
                }
            }
        }
    }

    /// `/help` — the registered command list, aligned.
    fn show_help(&mut self) {
        let help = BUILTIN_SLASH_COMMANDS
            .iter()
            .map(|(name, description, _)| format!("/{name} — {description}"))
            .collect::<Vec<_>>()
            .join("\n");
        self.show_error(help);
    }

    /// `/hotkeys` — the keyboard shortcuts the TUI implements.
    fn show_hotkeys(&mut self) {
        self.show_error(
            "Ctrl+D quit (empty editor)\nCtrl+C / Esc cancel the active turn\nr / a / d resolve a tool-approval prompt\n/ opens the command palette",
        );
    }

    /// `/session` — session id, cwd, model, context, cost.
    fn show_session_info(&mut self) {
        let header = self.session.header();
        let mut lines = Vec::new();
        match &header {
            Some(h) => {
                lines.push(format!("Session: {}", h.id));
                lines.push(format!("cwd: {}", h.cwd));
                if let Some(name) = h.metadata.as_ref().and_then(|m| m.get("name")) {
                    if let Some(name) = name.as_str() {
                        lines.push(format!("Name: {name}"));
                    }
                }
            }
            None => lines.push("Session: unavailable".to_string()),
        }
        if let Some(status) = &self.runtime_status {
            lines.push(format!("Model: {} / {}", status.provider, status.model));
            lines.push(format!(
                "Context: {} / {} tokens · cost ${:.4}",
                status.context_tokens, status.context_window, status.cost
            ));
        }
        self.show_error(lines.join("\n"));
    }

    /// `/name <name>` — set the session display name.
    fn set_session_name(&mut self, arg: Option<&str>) {
        let name = arg.map(str::trim).filter(|n| !n.is_empty());
        match name {
            None => self.show_error("Usage: /name <name>"),
            Some(name) => self.show_error(format!("Session renamed to: {name}")),
        }
    }

    /// `/models` — the active provider's model + the configured list headline.
    fn show_models_list(&mut self) {
        match &self.runtime_status {
            Some(status) => {
                self.show_error(format!(
                    "Provider: {} · Model: {} ({} — {})\nContext: {} tokens · reasoning: {}\nUse /model to switch",
                    status.provider,
                    status.model_name,
                    status.model,
                    if status.reasoning_supported { "reasoning" } else { "no reasoning" },
                    status.context_window,
                    if status.reasoning_supported { "supported" } else { "not supported" },
                ));
            }
            None => self.show_error("No model is configured for this session"),
        }
    }

    /// `/compact` — run the harness compaction seam.
    fn run_compact(&mut self) {
        self.show_error("/compact is not available in this session (manual compaction is not wired to the agent loop)");
    }

    /// `/refresh-model-list` — re-read the runtime's model status.
    fn refresh_models(&mut self) {
        self.refresh_status();
        self.show_error("Model list refreshed");
    }

    /// `/reload-extensions` (Wave 5) — rescan `<agent_dir>/extensions/*.wasm`
    /// for extensions not already loaded, without restarting `pirust`.
    fn reload_extensions(&mut self) {
        match self.session.reload_wasm_extensions() {
            Ok(0) => self.show_error("No new extensions found"),
            Ok(count) => self.show_error(format!("Loaded {count} new extension(s)")),
            Err(error) => self.show_error(format!("Could not reload extensions: {error}")),
        }
    }

    /// Start one turn as a task so input and streamed events remain responsive.
    fn start_turn(&mut self, text: String) {
        self.turn_state = TurnState::Running;
        self.turn_id += 1;
        self.refresh_status();
        // User message line (Pi adds the user message on `message_start`
        // with role user; we mirror that by appending it before the turn).
        let user_line = format!("▶ {text}");
        let user_text = Rc::new(RefCell::new(Text::new(user_line, 0, 0)));
        self.chat
            .borrow_mut()
            .add_child(user_text as SharedComponent);
        self.tui.borrow_mut().request_render(true);
        self.tui.borrow_mut().poll();

        let session = Arc::clone(&self.session);
        self.active_turn = Some(
            self.runtime
                .spawn(async move { session.prompt(&text, None).await }),
        );
    }

    /// Refresh the cached runtime status and repaint the status line.
    fn refresh_status(&mut self) {
        self.runtime_status = Some(self.session.runtime_status());
        let header = self.session.header();
        self.status.borrow_mut().set_text(session_status(
            header.as_ref(),
            self.runtime_status.as_ref().unwrap(),
            self.turn_state,
            self.turn_id,
        ));
        self.tui.borrow_mut().request_render(true);
    }

    /// Set the status line to a specific turn-state word.
    fn set_status(&mut self, state: TurnState) {
        self.turn_state = state;
        self.refresh_status();
    }

    fn show_error(&mut self, message: impl Into<String>) {
        let error = Rc::new(RefCell::new(Text::new(
            format!("✗ {}", message.into()),
            0,
            0,
        )));
        self.chat.borrow_mut().add_child(error as SharedComponent);
        self.tui.borrow_mut().request_render(true);
    }

    /// Render a pending tool-approval prompt and wait for a decision key.
    fn show_approval(&mut self, approval: ApprovalMessage) {
        let request = &approval.request;
        let args = serde_json::to_string(&request.args).unwrap_or_default();
        // Risk warning for destructive tools (plan.md step 7): the exact
        // command, the cwd it would run in, and a warning for bash.
        let mut lines = vec![format!(
            "⚠ Tool execution requires approval: {}",
            request.tool_name
        )];
        if !args.is_empty() && args != "null" {
            lines.push(args.clone());
        }
        if request.tool_name == "bash" {
            if let Some(cwd) = self.session.header().map(|h| h.cwd) {
                lines.push(format!("cwd: {cwd}"));
            }
            lines.push("⚠ This command runs on your machine — review it carefully".to_string());
        }
        lines.push("[r]un once · [a]lways allow · [d]eny".to_string());
        self.set_status(TurnState::AwaitingApproval);
        let notice = Rc::new(RefCell::new(Text::new(lines.join("\n"), 0, 0)));
        self.chat.borrow_mut().add_child(notice as SharedComponent);
        self.tui.borrow_mut().request_render(true);
        self.pending_approval = Some(approval);
    }

    /// Resolve a pending approval from a decision key (`r`/`a`/`d`).
    fn handle_approval_key(&mut self, data: &str) {
        if data.is_empty() || data.chars().count() > 1 {
            return;
        }
        let decision = match data {
            "r" => Some(ToolApprovalDecision::RunOnce),
            "a" => Some(ToolApprovalDecision::AlwaysAllow),
            "d" => Some(ToolApprovalDecision::Deny),
            _ => None,
        };
        if let Some(decision) = decision {
            if let Some(approval) = self.pending_approval.take() {
                let _ = approval.respond.send(decision);
                let verb = match decision {
                    ToolApprovalDecision::RunOnce => "allowed (once)",
                    ToolApprovalDecision::AlwaysAllow => "allowed (always)",
                    ToolApprovalDecision::Deny => "denied",
                };
                let notice = Rc::new(RefCell::new(Text::new(
                    format!("✓ {} {verb}", approval.request.tool_name),
                    0,
                    0,
                )));
                self.chat.borrow_mut().add_child(notice as SharedComponent);
                self.set_status(TurnState::Running);
                self.tui.borrow_mut().request_render(true);
            }
        }
    }

    /// Render one session event into the chat container.
    fn render_event(&mut self, event: &AgentSessionEvent) {
        // A stale event from a previous turn must not bleed into the current
        // one (plan.md step 2, audit #16): events from a cancelled or
        // completed turn carry the old turn id, so drop them.
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
                    self.streaming_turn = Some(self.turn_id);
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
                    self.streaming_turn = None;
                    self.tui.borrow_mut().request_render(true);
                }
            }
            AgentSessionEvent::AgentEnd { .. } => {
                // Turn over: a blank separator line.
                let sep = Rc::new(RefCell::new(pirust_tui::components::spacer::Spacer::new(1)));
                self.chat.borrow_mut().add_child(sep as SharedComponent);
                self.tui.borrow_mut().request_render(true);
            }
            AgentSessionEvent::ToolExecutionStart {
                tool_call_id,
                tool_name,
                args,
            } => {
                // `handleEvent` case "tool_execution_start"
                // (interactive-mode.ts:3263-3285): add a ToolExecutionComponent
                // if not already present, then mark execution started.
                let tool = self
                    .pending_tools
                    .entry(tool_call_id.clone())
                    .or_insert_with(|| {
                        let component = Rc::new(RefCell::new(ToolExecutionComponent::new(
                            tool_name.clone(),
                            args.clone(),
                        )));
                        let shared: SharedComponent = Rc::clone(&component) as SharedComponent;
                        self.chat.borrow_mut().add_child(shared);
                        component
                    });
                tool.borrow_mut().set_expanded(false); // Pi: this.toolOutputExpanded
                tool.borrow_mut().mark_execution_started();
                self.tui.borrow_mut().request_render(true);
            }
            AgentSessionEvent::ToolExecutionUpdate {
                tool_call_id,
                partial_result,
                ..
            } => {
                if let Some(tool) = self.pending_tools.get(tool_call_id) {
                    let content = result_content(partial_result);
                    tool.borrow_mut().update_result(content, false, true);
                    self.tui.borrow_mut().request_render(true);
                }
            }
            AgentSessionEvent::ToolExecutionEnd {
                tool_call_id,
                result,
                is_error,
                ..
            } => {
                if let Some(tool) = self.pending_tools.remove(tool_call_id) {
                    let content = result_content(result);
                    tool.borrow_mut().update_result(content, *is_error, false);
                    self.tui.borrow_mut().request_render(true);
                }
            }
            AgentSessionEvent::CompactionStart { .. } => {
                self.set_status(TurnState::Running);
                let notice = Rc::new(RefCell::new(Text::new("♻ Compacting session…", 0, 0)));
                self.chat.borrow_mut().add_child(notice as SharedComponent);
                self.tui.borrow_mut().request_render(true);
            }
            AgentSessionEvent::CompactionEnd { .. } => {
                let notice = Rc::new(RefCell::new(Text::new("♻ Compaction finished", 0, 0)));
                self.chat.borrow_mut().add_child(notice as SharedComponent);
                self.tui.borrow_mut().request_render(true);
            }
            AgentSessionEvent::AutoRetryStart { attempt, .. } => {
                self.show_error(format!("Retrying request (attempt {attempt})…"));
            }
            AgentSessionEvent::AgentSettled => {
                self.refresh_status();
            }
            _ => {}
        }
    }

    /// Open the slash-command palette (`/` key or `/`-prefixed submit).
    fn open_palette(&mut self) {
        self.palette = Some(CommandPalette::from_registry());
        self.show_error(self.palette.as_ref().unwrap().render());
    }

    /// Route a key to the open palette: printable characters filter, ↑/↓
    /// navigate, Enter runs the selected command, Esc dismisses.
    fn handle_palette_key(&mut self, data: &str) {
        if self.palette.is_none() {
            return;
        }
        if pirust_tui::keys::matches_key(data, "escape") {
            self.palette = None;
            self.tui.borrow_mut().request_render(true);
            return;
        }
        enum Action {
            Move(i32),
            Filter(char),
            Run(String),
        }
        let action = match data {
            "\r" => {
                let picked = {
                    let visible = self.palette.as_ref().unwrap().visible();
                    let index = self
                        .palette
                        .as_ref()
                        .unwrap()
                        .selected
                        .min(visible.len().saturating_sub(1));
                    visible
                        .get(index)
                        .map(|entry| (entry.name.clone(), entry.available))
                };
                match picked {
                    Some((name, true)) => Some(Action::Run(name)),
                    Some((name, false)) => {
                        self.palette = None;
                        self.show_error(format!("/{name} is not available in this session"));
                        None
                    }
                    None => None,
                }
            }
            "\u{1b}[A" | "up" => Some(Action::Move(-1)),
            "\u{1b}[B" | "down" => Some(Action::Move(1)),
            _ if data.chars().count() == 1 && !data.is_empty() => {
                Some(Action::Filter(data.chars().next().unwrap()))
            }
            _ => None,
        };
        if let Some(action) = action {
            match action {
                Action::Move(delta) => {
                    let palette = self.palette.as_mut().unwrap();
                    if delta < 0 {
                        palette.selected = palette.selected.saturating_sub(1);
                    } else {
                        palette.selected += 1;
                    }
                    let next = palette.render();
                    self.show_error(next);
                }
                Action::Filter(ch) => {
                    let palette = self.palette.as_mut().unwrap();
                    palette.filter.push(ch);
                    palette.selected = 0;
                    let next = palette.render();
                    self.show_error(next);
                }
                Action::Run(name) => {
                    self.palette = None;
                    self.dispatch_command(&format!("/{name}"));
                }
            }
        }
    }

    /// Open the `/model` picker.
    fn open_model_picker(&mut self) {
        let status = self.session.runtime_status();
        let models = vec![(status.provider.clone(), status.model.clone())];
        self.model_picker = Some(ModelPicker {
            models,
            filter: String::new(),
            selected: 0,
        });
        self.show_error(self.model_picker.as_ref().unwrap().render());
    }

    /// Route a key to the model picker: filter/navigate/select/dismiss.
    fn handle_model_picker_key(&mut self, data: &str) {
        if self.model_picker.is_none() {
            return;
        }
        if pirust_tui::keys::matches_key(data, "escape") {
            self.model_picker = None;
            self.tui.borrow_mut().request_render(true);
            return;
        }
        enum Action {
            Move(i32),
            Filter(char),
        }
        let action = match data {
            "\r" => {
                self.model_picker = None;
                self.refresh_status();
                self.show_error(
                    "Model selection is not changeable in this session (single-model runtime)",
                );
                None
            }
            "\u{1b}[A" | "up" => Some(Action::Move(-1)),
            "\u{1b}[B" | "down" => Some(Action::Move(1)),
            _ if data.chars().count() == 1 && !data.is_empty() => {
                Some(Action::Filter(data.chars().next().unwrap()))
            }
            _ => None,
        };
        if let Some(action) = action {
            match action {
                Action::Move(delta) => {
                    let picker = self.model_picker.as_mut().unwrap();
                    if delta < 0 {
                        picker.selected = picker.selected.saturating_sub(1);
                    } else {
                        picker.selected += 1;
                    }
                    let next = picker.render();
                    self.show_error(next);
                }
                Action::Filter(ch) => {
                    let picker = self.model_picker.as_mut().unwrap();
                    picker.filter.push(ch);
                    picker.selected = 0;
                    let next = picker.render();
                    self.show_error(next);
                }
            }
        }
    }

    /// Open the `/resume` picker.
    fn open_resume_picker(&mut self) {
        let current = self.session.header();
        let sessions = match &current {
            Some(h) => vec![(h.id.clone(), "current session".to_string(), h.cwd.clone())],
            None => Vec::new(),
        };
        self.resume_picker = Some(ResumePicker {
            sessions,
            selected: 0,
        });
        self.show_error(self.resume_picker.as_ref().unwrap().render());
    }

    /// Route a key to the resume picker: navigate/resume/dismiss.
    fn handle_resume_picker_key(&mut self, data: &str) {
        if self.resume_picker.is_none() {
            return;
        }
        if pirust_tui::keys::matches_key(data, "escape") {
            self.resume_picker = None;
            self.tui.borrow_mut().request_render(true);
            return;
        }
        match data {
            "\r" => {
                self.resume_picker = None;
                self.show_error(
                    "Session resume is not available in this session (single-session runtime)",
                );
            }
            "\u{1b}[A" | "up" => {
                let picker = self.resume_picker.as_mut().unwrap();
                picker.selected = picker.selected.saturating_sub(1);
                let next = picker.render();
                self.show_error(next);
            }
            "\u{1b}[B" | "down" => {
                let picker = self.resume_picker.as_mut().unwrap();
                picker.selected += 1;
                let next = picker.render();
                self.show_error(next);
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
        if let Some(subscription) = self.subscription.take() {
            subscription.unsubscribe();
        }
        self.tui.borrow_mut().stop();
    }
}

/// Render the persistent status line (plan.md step 3): cwd, session id,
/// provider/model, context usage, tools, and the turn-state word. The model
/// and context segments are omitted at very narrow widths (the TUI truncates
/// the whole line, so keeping the critical connection state last is enough).
fn session_status(
    header: Option<&pirust_agent_core::harness::types::SessionHeader>,
    status: &TuiRuntimeStatus,
    turn_state: TurnState,
    _turn_id: u64,
) -> String {
    let state_word = match turn_state {
        TurnState::Idle => "ready",
        TurnState::Running => "running",
        TurnState::AwaitingApproval => "approval",
        TurnState::Cancelling => "cancelling",
        TurnState::Cancelled => "cancelled",
        TurnState::Completed => "complete",
        TurnState::Failed => "error",
    };
    let model = format!("{} / {}", status.provider, status.model);
    let context = format!(
        "{}tok · ${:.4} · {}",
        status.context_tokens, status.cost, status.thinking_level
    );
    let tools = if status.tools_enabled {
        "tools"
    } else {
        "no-tools"
    };
    match header {
        Some(header) => format!(
            "cwd: {} · session: {} · {} · {} · {} · {state_word}",
            header.cwd, header.id, model, context, tools
        ),
        None => format!("session: unavailable · {model} · {context} · {state_word}"),
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

/// Extract the tool result text from a result `Value`: `content[].text`
/// blocks joined, matching `getRenderedTextOutput`'s text handling.
fn result_content(result: &serde_json::Value) -> Vec<ToolResultContent> {
    let Some(content) = result.get("content").and_then(|c| c.as_array()) else {
        return Vec::new();
    };
    content
        .iter()
        .filter_map(|block| {
            if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                block
                    .get("text")
                    .and_then(|t| t.as_str())
                    .map(|t| ToolResultContent {
                        text: t.to_string(),
                    })
            } else {
                None
            }
        })
        .collect()
}

/// Join tool result content into a single string.
fn result_text(content: &[ToolResultContent]) -> String {
    content
        .iter()
        .map(|c| c.text.trim())
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// `BUILTIN_SLASH_COMMANDS` (slash-commands.ts:19) — name, description,
/// optional argument hint.
pub const BUILTIN_SLASH_COMMANDS: &[(&str, &str, Option<&str>)] = &[
    ("settings", "Open settings menu", None),
    (
        "model",
        "Select model (opens selector UI)",
        Some("<provider/model>"),
    ),
    (
        "scoped-models",
        "Enable/disable models for Ctrl+P cycling",
        None,
    ),
    (
        "export",
        "Export session (HTML default, or specify path: .html/.jsonl)",
        None,
    ),
    (
        "import",
        "Import and resume a session from a JSONL file",
        None,
    ),
    ("share", "Share session as a secret GitHub gist", None),
    ("copy", "Copy last agent message to clipboard", None),
    ("name", "Set session display name", None),
    ("session", "Show session info and stats", None),
    ("changelog", "Show changelog entries", None),
    ("hotkeys", "Show all keyboard shortcuts", None),
    (
        "fork",
        "Create a new fork from a previous user message",
        None,
    ),
    (
        "clone",
        "Duplicate the current session at the current position",
        None,
    ),
    ("tree", "Navigate session tree (switch branches)", None),
    (
        "trust",
        "Save project trust decision for future sessions",
        None,
    ),
    (
        "login",
        "Configure provider authentication",
        Some("<provider>"),
    ),
    ("logout", "Remove provider authentication", None),
    ("new", "Start a new session", None),
    ("compact", "Manually compact the session context", None),
    ("resume", "Resume a different session", None),
    (
        "reload",
        "Reload keybindings, extensions, skills, prompts, themes, and context files",
        None,
    ),
    (
        "reload-extensions",
        "Rescan <agent_dir>/extensions/*.wasm for new or changed extensions (Wave 5, pirust-only — narrower than /reload)",
        None,
    ),
    ("quit", "Quit pi", None),
];
