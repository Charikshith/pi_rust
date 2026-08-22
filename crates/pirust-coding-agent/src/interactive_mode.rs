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
use crate::print_mode::{AgentSessionEvent, PrintModeSession};

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
    _event_tx: Sender<AgentSessionEvent>,
    /// Set when the user quits (Ctrl+D on an empty editor).
    quit: Arc<AtomicBool>,
    /// Set by Ctrl+C/Esc and consumed by the async turn loop.
    cancel_requested: Arc<AtomicBool>,
    /// The async session used by background turn tasks.
    session: Arc<dyn PrintModeSession>,
    runtime: tokio::runtime::Handle,
    active_turn: Option<JoinHandle<Result<(), crate::print_mode::ThrownValue>>>,
    subscription: Option<crate::print_mode::Subscription>,
    /// Chat container children: user messages, the streaming assistant
    /// message, and separators. The streaming message is always the last
    /// child while a turn is in flight.
    chat: Rc<RefCell<pirust_tui::tui::Container>>,
    status: Rc<RefCell<Text>>,
    last_size: Option<u16>,
    streaming_text: Option<Rc<RefCell<Text>>>,
    /// Tool executions in flight, keyed by tool call id
    /// (`pendingTools` map, interactive-mode.ts:468).
    pending_tools: HashMap<String, Rc<RefCell<ToolExecutionComponent>>>,
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

        let status = Rc::new(RefCell::new(Text::new(
            session_status(session.header().as_ref(), "ready"),
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
        {
            let quit = Arc::clone(&quit);
            let cancel_requested = Arc::clone(&cancel_requested);
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
                    None
                }));
        }

        // Subscribe to session events, bridging the agent thread → channel.
        let (event_tx, event_rx) = channel::<AgentSessionEvent>();
        let subscription = {
            let tx = event_tx.clone();
            let subscription = session.subscribe(Arc::new(move |event: &AgentSessionEvent| {
                let _ = tx.send(event.clone());
            }));
            // Keep the subscription alive for the mode's lifetime and
            // unsubscribe it deterministically when the mode is dropped.
            subscription
        };

        Self {
            tui,
            input_rx,
            submit_rx,
            _submit_tx: submit_tx,
            event_rx,
            _event_tx: event_tx,
            quit,
            cancel_requested,
            session,
            runtime,
            active_turn: None,
            subscription: Some(subscription),
            chat,
            status,
            last_size: None,
            streaming_text: None,
            pending_tools: HashMap::new(),
        }
    }

    /// Drive the TUI without blocking while a model turn runs.
    pub async fn run_async(&mut self) {
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
            if self.cancel_requested.swap(false, Ordering::Relaxed) {
                if let Some(turn) = self.active_turn.take() {
                    turn.abort();
                    self.show_error("Request cancelled");
                }
            }
            if self
                .active_turn
                .as_ref()
                .is_some_and(JoinHandle::is_finished)
            {
                match self.active_turn.take().unwrap().await {
                    Ok(Ok(())) => self.set_status("ready"),
                    Ok(Err(error)) => self.show_error(error.console_message()),
                    Err(error) => self.show_error(format!("turn task failed: {error}")),
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
                self.run_turn_sync(&text);
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

    fn dispatch_command(&mut self, text: &str) {
        let command = text
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .trim_start_matches('/')
            .to_ascii_lowercase();
        if command == "help" {
            let help = BUILTIN_SLASH_COMMANDS
                .iter()
                .map(|(name, description, _)| format!("/{name} — {description}"))
                .collect::<Vec<_>>()
                .join("\n");
            self.show_error(help);
        } else if BUILTIN_SLASH_COMMANDS
            .iter()
            .any(|(name, _, _)| *name == command)
        {
            self.show_error(format!("/{command} is not available in this session"));
        } else {
            self.show_error(format!("Unknown command: /{command}"));
        }
    }

    /// Start one turn as a task so input and streamed events remain responsive.
    fn start_turn(&mut self, text: String) {
        self.set_status("turn: running");
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

    fn set_status(&mut self, state: &str) {
        let header = self.session.header();
        self.status
            .borrow_mut()
            .set_text(session_status(header.as_ref(), state));
        self.tui.borrow_mut().request_render(true);
    }

    fn show_error(&mut self, message: impl Into<String>) {
        self.set_status("error");
        let error = Rc::new(RefCell::new(Text::new(
            format!("✗ {}", message.into()),
            0,
            0,
        )));
        self.chat.borrow_mut().add_child(error as SharedComponent);
        self.tui.borrow_mut().request_render(true);
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

fn session_status(
    header: Option<&pirust_agent_core::harness::types::SessionHeader>,
    state: &str,
) -> String {
    match header {
        Some(header) => format!(
            "cwd: {} · session: {} · connection: {}",
            header.cwd, header.id, state
        ),
        None => format!("session: unavailable · connection: {state}"),
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
    ("quit", "Quit pi", None),
];
