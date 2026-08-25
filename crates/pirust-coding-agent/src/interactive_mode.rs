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
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};
use tokio::sync::Notify;
use tokio::task::JoinHandle;

use pirust_tui::components::text::Text;
use pirust_tui::editor::Editor;
use pirust_tui::tui::{Component, SharedComponent, TUI};

use crate::interactive_a11y::glyph;
use crate::interactive_commands::CommandOutcome;
use crate::interactive_debug::{DebugLog, DebugPanel, RequestId, TurnTimings};
use crate::interactive_diff::{parse_file_change, DiffPreview};
use crate::interactive_markdown::MarkdownText;
use crate::interactive_pickers::{
    ModelEntry, ModelPicker as PickerModelPicker, PickerAction, SessionPicker,
};
use crate::interactive_theme::{self, dark};
use crate::interactive_thinking::{thinking_text, ThinkingComponent, ThinkingRegistry};
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

// REMOVED (user directive): a hand-rolled slash-command palette used to
// live here — its own separate filter box, key routing, and rendering,
// entirely independent of `pirust-tui`'s real `Editor` autocomplete. It
// intercepted `/` globally before the editor ever saw it, so the editor's
// OWN already-wired autocomplete (`set_autocomplete_provider` below, built
// on the exact same `BUILTIN_SLASH_COMMANDS` list) never got a chance to
// run — the real one renders text typed directly in the input box with a
// dropdown underneath (matching a real reference screenshot); the removed
// one rendered a disconnected "Command palette (filter: X)" notice with no
// connection to the input box at all. Letting `/` flow to the editor as a
// normal character is the fix — no replacement code needed here.

// REMOVED: two hand-rolled pickers used to live here — a `ModelPicker` over a
// `Vec<(String, String)>` and a `ResumePicker` over a `Vec<(String, String,
// String)>`. Both were fakes in every sense that mattered:
//
//   * their *data* was a one-element list built from the current status, so
//     `/model` offered only the model already in use and `/resume` only the
//     session already open, and Enter on either answered "not available in
//     this session";
//   * `visible()` rebuilt a `Vec<&(String, String)>` on every keystroke and
//     filtered with `to_ascii_lowercase().contains()`, not the fuzzy matcher
//     `pirust-tui` already ships;
//   * navigation was `selected += 1` with **no upper bound** — held-down
//     Down-arrow walked the index past the end of the list, and only the
//     `.min(len - 1)` in the renderer hid it;
//   * there was no viewport, so a real provider list would have rendered
//     every model into the band and pushed the editor off-screen.
//
// They are replaced by `crate::interactive_pickers::{ModelPicker,
// SessionPicker}`: real data off the session seam, fuzzy filtering over a
// reused index buffer, clamped navigation, a scrolling viewport, and columns
// that degrade at narrow widths.

/// How many lines of a tool result to preview before truncating with
/// `... (N more lines)` — `FALLBACK_PREVIEW_LINES` (tool-execution.ts:15).
const FALLBACK_PREVIEW_LINES: usize = 10;

/// The interactive session runner.
pub struct InteractiveMode {
    tui: Rc<RefCell<TUI>>,
    /// Same editor the TUI renders; kept here too so `run_async` can check
    /// whether the "/" command dropdown is open before treating Esc as
    /// "cancel the active turn."
    editor: Rc<RefCell<Editor>>,
    /// Raw input sequences from the terminal reader thread.
    ///
    /// A **tokio** unbounded channel rather than [`std::sync::mpsc`] so
    /// `run_async` can `select!` on it and park until a key actually arrives
    /// (see [`Self::wait_for_work`]). `UnboundedSender::send` is a plain
    /// non-blocking, non-async call that needs no runtime context, so the
    /// terminal's reader thread still feeds it exactly as before.
    input_rx: UnboundedReceiver<String>,
    /// Submitted prompts from the editor's `on_submit`.
    submit_rx: Receiver<String>,
    _submit_tx: Sender<String>,
    /// Session events from the agent loop thread (subscription listener).
    ///
    /// Deliberately still a **bounded** [`std::sync::mpsc::SyncSender`]: the
    /// bound is the backpressure that stops a fast provider from growing the
    /// queue without limit, and `tokio::sync::mpsc::Sender::blocking_send`
    /// cannot replace it — the agent loop calls this listener from *inside* the
    /// runtime, where `blocking_send` panics ("Cannot block the current thread
    /// from within a runtime"), which is the exact class of bug this TUI was
    /// first bitten by. The loop is woken instead by [`Self::wake`].
    event_rx: Receiver<AgentSessionEvent>,
    _event_tx: std::sync::mpsc::SyncSender<AgentSessionEvent>,
    /// Keystrokes [`Self::wait_for_work`] pulled off `input_rx` to end its
    /// wait, handed back so the single drain site in `run_async` still sees
    /// them in order. A tokio receiver has no push-back, and dropping the
    /// value would silently eat the key that woke us.
    ///
    /// Never holds more than one element in practice — `select!` returns one
    /// value per wait — so this costs nothing; `VecDeque` is used only because
    /// it makes the drain's ordering obvious.
    replay_input: VecDeque<String>,
    /// Wakes `run_async` when a producer that is *not* the input channel has
    /// something for it: a session event or a tool-approval request, both of
    /// which arrive on sync channels from the agent thread.
    ///
    /// [`Notify::notify_one`] is sync, allocation-free, safe from any thread
    /// with or without a runtime, and — critically — *stores* a permit, so a
    /// notification sent between two `notified()` calls is not lost. That
    /// permit is what lets the loop sleep indefinitely instead of polling.
    wake: Arc<Notify>,
    /// How many times `run_async`'s body has run. The regression seam for
    /// [`Self::wait_for_work`]: an idle loop must park, so this must stay
    /// near-flat while nothing is happening. A plain `u64` is enough — the
    /// loop is single-threaded and nothing else writes it.
    loop_iterations: u64,
    /// Prompts submitted while a turn was already in flight.
    ///
    /// These used to be dropped on the floor: the submit drain ran only
    /// `if ... self.active_turn.is_none()`, so anything typed and entered
    /// during a turn vanished with no message at all. The design spec's
    /// response-state list names `queued` as a state the UI must distinguish,
    /// so they are now held here and started in order as turns finish.
    pending_prompts: VecDeque<String>,
    /// Set when the user quits (Ctrl+D on an empty editor).
    quit: Arc<AtomicBool>,
    /// Set by Ctrl+C/Esc and consumed by the async turn loop.
    cancel_requested: Arc<AtomicBool>,
    /// When Ctrl+C was last pressed, shared with the TUI's global input
    /// listener so the "second press within 500ms quits" window is one window
    /// whichever path saw the key (`handle_ctrl_c`).
    last_ctrl_c: Arc<Mutex<Option<Instant>>>,
    // (former `palette_requested` field removed — see the removal note near
    // the top of this file, right after ApprovalMessage.)
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
    /// The live assistant message.
    ///
    /// A [`MarkdownText`], not a plain `Text`: `pirust-tui` has carried a full
    /// 2,000-line Markdown renderer since the TUI crate was ported and the
    /// chat never called it, so every fenced code block, list and heading a
    /// model produced was shown as literal `**` and backticks. The component
    /// no-ops an unchanged `set_text`, which matters because this is written
    /// once per streamed token.
    streaming_text: Option<Rc<RefCell<MarkdownText>>>,
    /// The live reasoning block for the current assistant message, mounted
    /// above it. `None` until the model actually emits thinking, so a
    /// non-reasoning model costs nothing.
    streaming_thinking: Option<Rc<RefCell<ThinkingComponent>>>,
    /// Every thinking block in the transcript, so a global Ctrl+O can expand
    /// or collapse them without the chat container knowing what it holds.
    thinking: ThinkingRegistry,
    /// Diff previews for file-editing tool calls, keyed by tool call id and
    /// mounted directly under their tool box.
    pending_diffs: HashMap<String, Rc<RefCell<DiffPreview>>>,
    /// Instrumentation for the turn in flight: request id, time-to-first-token,
    /// total duration, per-tool durations. `None` between turns.
    timings: Option<TurnTimings>,
    /// The bounded event log behind the debug panel and the panic report.
    ///
    /// `Arc<Mutex<_>>` rather than the `Rc<RefCell<_>>` everything else in this
    /// file uses, because the panic hook is `Send + Sync + 'static` and cannot
    /// hold `Rc`. It is the one piece of TUI state that must outlive the TUI.
    debug_log: Arc<Mutex<DebugLog>>,
    /// The debug panel, part of the bottom band and hidden by default.
    debug_panel: Rc<RefCell<DebugPanel>>,
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
    /// The model picker, `Some` while it is open.
    model_picker: Option<Rc<RefCell<PickerModelPicker>>>,
    /// The session picker, `Some` while it is open.
    resume_picker: Option<Rc<RefCell<SessionPicker>>>,
    /// The first-run block, dismissed on the first submitted prompt so it
    /// stops occupying rows once there is a conversation to read.
    welcome: Rc<RefCell<crate::interactive_welcome::WelcomeScreen>>,
    /// The selectable model list, supplied by `main.rs` through
    /// [`Self::set_model_entries`].
    ///
    /// Not read off the session seam like the session list is, because there
    /// is nothing to read it from: `SingleTurnSession` holds an `Agent`, and
    /// `Agent::model()` is the *one* model in use — the composed provider list
    /// lives in the `ModelRuntime` that `main.rs` builds and never hands to
    /// the session. Pushing it in from there beats widening
    /// `InteractiveMode::new`, whose signature every test rig calls.
    model_entries: Vec<ModelEntry>,
    /// The text any open modal (palette/model-picker/resume-picker) renders
    /// into — empty when none is open. NOT its own overlay: it renders as
    /// part of the SAME `BottomLeft` overlay as the editor/status band (see
    /// `EditorStatusBand`), directly above the editor, rather than floating
    /// as an independent centered overlay disconnected from the input box.
    modal_text: Rc<RefCell<Modal>>,
}

/// `ToolExecutionComponent` (tool-execution.ts) — a simplified but faithful
/// port: a `Box` (pending/success/error bg) with the tool name title + args
/// JSON, then the streaming result text (truncated preview).
pub struct ToolExecutionComponent {
    tool_name: String,
    args: serde_json::Value,
    /// Suppress the pretty-printed args JSON.
    ///
    /// Set when a sibling [`DiffPreview`] is mounted directly beneath this box
    /// (a `write` or `edit` call). Printing `{"path": …, "content": "…"}` above
    /// a rendered diff of that same content shows the file twice, once
    /// unreadably — the diff *is* the argument display for those tools.
    suppress_args: bool,
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
    fn new(tool_name: String, args: serde_json::Value, suppress_args: bool) -> Self {
        Self {
            tool_name,
            args,
            suppress_args,
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
        if !self.suppress_args {
            let content = serde_json::to_string_pretty(&self.args).unwrap_or_default();
            if !content.is_empty() && content != "{}" && content != "null" {
                text.push_str("\n\n");
                text.push_str(&content);
            }
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

/// Bottom-anchored modal + editor + status band (user directive — a
/// deliberate deviation from Pi's own top-down, no-padding document flow):
/// the open modal's text (palette / model picker / resume picker, empty
/// when none is open), the editor, and the status line are combined into
/// ONE component and mounted as a single `BottomLeft`-anchored overlay (see
/// `InteractiveMode::new`), reusing the same overlay-compositing machinery
/// Pi's own popups rely on to pin content to the terminal's last rows
/// regardless of how short the chat transcript is — rather than a
/// hand-rolled blank-line filler, which panicked with a `RefCell`
/// double-borrow (this component would need to read `TUI::terminal_rows()`
/// through the very `Rc<RefCell<TUI>>>` that is already mutably borrowed
/// for the render pass calling it).
///
/// The modal used to be a SEPARATE `Center`-anchored overlay: fixed the
/// duplicate-copies-in-chat bug (see `show_modal`'s doc comment) but left it
/// floating disconnected from the input box, with blank space on all sides
/// — not what a dropdown attached to its input should look like. Folding it
/// into this same band, rendered first (so it appears directly above the
/// editor), fixes that: one cohesive, bottom-anchored block, no gap.
struct EditorStatusBand {
    /// The debug panel. Renders zero rows while hidden, which is its default,
    /// so it costs nothing until `/debug` turns it on.
    debug: Rc<RefCell<DebugPanel>>,
    modal: Rc<RefCell<Modal>>,
    editor: Rc<RefCell<Editor>>,
    status: Rc<RefCell<Text>>,
}

/// What currently occupies the band's modal row(s).
///
/// This used to be a bare `Text`, which forced every modal to pre-render
/// itself to a `String` before it knew the terminal width. The real pickers
/// lay out aligned columns and a scrolling viewport, both of which need the
/// width, and their output is already ANSI-coloured — pushing that through
/// `Text`'s wrapper would re-wrap and mangle it. So the slot now holds either
/// plain text (short notices, which genuinely do not care about width) or a
/// full [`Component`](pirust_tui::tui::Component) that renders itself.
enum Modal {
    /// Nothing open.
    None,
    /// A short pre-rendered notice.
    Text(Text),
    /// A width-aware component: the model or session picker.
    Component(SharedComponent),
}

impl Modal {
    fn render(&mut self, width: usize) -> Vec<String> {
        match self {
            Modal::None => Vec::new(),
            Modal::Text(text) => text.render(width),
            Modal::Component(component) => component.borrow_mut().render(width),
        }
    }

    fn invalidate(&mut self) {
        match self {
            Modal::None => {}
            Modal::Text(text) => text.invalidate(),
            Modal::Component(component) => component.borrow_mut().invalidate(),
        }
    }
}

impl pirust_tui::tui::Component for EditorStatusBand {
    fn render(&mut self, width: usize) -> Vec<String> {
        // Debug output sits at the top of the band — furthest from the input
        // box, because it is the least likely thing the user is looking at.
        let mut lines = self.debug.borrow_mut().render(width);
        lines.extend(self.modal.borrow_mut().render(width));
        lines.extend(self.editor.borrow_mut().render(width));
        lines.extend(self.status.borrow_mut().render(width));
        lines
    }

    fn invalidate(&mut self) {
        self.debug.borrow_mut().invalidate();
        self.modal.borrow_mut().invalidate();
        self.editor.borrow_mut().invalidate();
        self.status.borrow_mut().invalidate();
    }
}

impl InteractiveMode {
    /// Build the TUI + editor + chat container, start the terminal reader
    /// thread, and subscribe to the session's events.
    pub fn new(
        terminal: Box<dyn pirust_tui::terminal::Terminal>,
        session: Arc<dyn InteractiveSession>,
        runtime: tokio::runtime::Handle,
    ) -> Self {
        // Accessibility first: everything built below asks
        // `interactive_a11y::active()` whether it may emit colour, box-drawing
        // glyphs or animation, so the detection has to happen before the first
        // component exists. `NO_COLOR`, `TERM=dumb` and a non-TTY stdout all
        // land here, and the Markdown theme is switched to its identity
        // variant so a `NO_COLOR` run emits no escape bytes at all rather than
        // colouring text and hoping the terminal ignores it.
        let a11y = crate::interactive_a11y::detect();
        crate::interactive_a11y::set_active(a11y);
        crate::interactive_markdown::set_color_enabled(
            a11y.color != crate::interactive_a11y::ColorMode::None,
        );

        // The bounded event log. Built before anything can fail so the panic
        // hook has somewhere to write from the earliest possible moment.
        let debug_log = Arc::new(Mutex::new(DebugLog::new()));
        crate::interactive_debug::install_panic_hook(Arc::clone(&debug_log));

        let (input_tx, input_rx) = unbounded_channel::<String>();

        // Start the terminal reader thread, feeding the channel. `send` on a
        // tokio unbounded sender is a lock-free push with no `.await` and no
        // runtime requirement, so this stays a plain callback from a plain
        // thread — the reader is unchanged, only the loop's *wait* changes.
        let mut terminal = terminal;
        terminal.start(
            Box::new(move |data: &str| {
                let _ = input_tx.send(data.to_string());
            }),
            Box::new(|| {}),
        );

        // The loop's wakeup source for everything that is not a keystroke.
        let wake = Arc::new(Notify::new());

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

        // Chat container: a normal top-level child, same as Pi's own layout
        // — it scrolls/grows top-down like any inline-diff terminal output.
        let chat = Rc::new(RefCell::new(pirust_tui::tui::Container::new()));
        tui.borrow_mut()
            .add_child(Rc::clone(&chat) as SharedComponent);

        // The first-run block (`docs/tui-design-samples.html` §1): cwd, the
        // model actually selected, the provider, and the tool names. It is a
        // normal chat child, so it simply scrolls away as the conversation
        // grows instead of needing to be explicitly dismissed.
        //
        // Tool names come off the session seam's `tool_names()`, which
        // defaults to empty — `TuiRuntimeStatus` carries only `tools_enabled`,
        // and the real enabled-tool set is assembled in `sdk.rs` and was never
        // threaded back to the TUI. An empty list renders no tool row rather
        // than inventing one.
        let welcome = {
            let cwd = session
                .header()
                .map(|h| h.cwd)
                .unwrap_or_else(|| String::from("."));
            let tool_names = session.tool_names();
            let borrowed: Vec<&str> = tool_names.iter().map(String::as_str).collect();
            let welcome = Rc::new(RefCell::new(
                crate::interactive_welcome::WelcomeScreen::from_status(
                    &cwd,
                    &runtime_status,
                    &borrowed,
                ),
            ));
            chat.borrow_mut()
                .add_child(Rc::clone(&welcome) as SharedComponent);
            welcome
        };

        let editor = Rc::new(RefCell::new(Editor::new(
            tui.borrow().terminal_rows_handle(),
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

        // Modal text + editor + status line: a `BottomLeft`-anchored overlay
        // (user directive), so this band is always pinned to the terminal's
        // last rows — chat scrolls above it, status sits below the editor
        // instead of above it, and an open palette/picker renders directly
        // above the editor instead of floating separately. See
        // `EditorStatusBand`'s doc comment.
        let modal_text = Rc::new(RefCell::new(Modal::None));
        let debug_panel = Rc::new(RefCell::new(DebugPanel::new(Arc::clone(&debug_log))));
        let band = Rc::new(RefCell::new(EditorStatusBand {
            debug: Rc::clone(&debug_panel),
            modal: Rc::clone(&modal_text),
            editor: Rc::clone(&editor),
            status: Rc::clone(&status),
        }));
        tui.borrow_mut().show_overlay(
            band as SharedComponent,
            Some(pirust_tui::tui::OverlayOptions {
                anchor: Some(pirust_tui::tui::OverlayAnchor::BottomLeft),
                width: Some(pirust_tui::tui::SizeValue::Percent(100.0)),
                ..Default::default()
            }),
        );
        tui.borrow_mut()
            .set_focus(Some(Rc::clone(&editor) as SharedComponent));

        // Slash-command autocomplete (`createBaseAutocompleteProvider`,
        // interactive-mode.ts:670): the BUILTIN_SLASH_COMMANDS list mapped
        // to `SlashCommand`.
        //
        // Commands `dispatch_command` cannot actually run say so in their
        // description (audit #22). Most of the registered list is not wired to
        // a handler yet, and offering `/export` or `/fork` as if they worked —
        // only to answer "is not available in this session" once the user picks
        // one — is exactly the drift the audit calls out.
        {
            let commands: Vec<pirust_tui::autocomplete::CommandOrItem> =
                crate::interactive_mode::BUILTIN_SLASH_COMMANDS
                    .iter()
                    .map(|(name, description, hint)| {
                        let description = if slash_command_available(name) {
                            description.to_string()
                        } else {
                            format!("{description} (unavailable in this session)")
                        };
                        pirust_tui::autocomplete::CommandOrItem::Command(
                            pirust_tui::autocomplete::SlashCommand {
                                name: name.to_string(),
                                description: Some(description),
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
        // by the editor being empty). Ctrl+C is always global (Pi's own
        // `handleCtrlC`: a second press within 500ms quits, matching
        // interactive-mode.ts's `lastSigintTime` window exactly) — a lone
        // press requests cancellation of the active turn. Esc is
        // deliberately NOT consumed here: it must reach an open modal
        // (palette/model-picker/resume-picker) so that modal's own Esc
        // handler can close it; see `run_async`'s fallback for the
        // no-modal-open case.
        let quit = Arc::new(AtomicBool::new(false));
        let cancel_requested = Arc::new(AtomicBool::new(false));
        let last_ctrl_c: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None));
        {
            let quit = Arc::clone(&quit);
            let cancel_requested = Arc::clone(&cancel_requested);
            let editor = Rc::clone(&editor);
            let last_ctrl_c = Arc::clone(&last_ctrl_c);
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
                    if pirust_tui::keys::matches_key(data, "ctrl+c") {
                        let now = Instant::now();
                        let mut last = last_ctrl_c.lock().unwrap();
                        if last.is_some_and(|t| now.duration_since(t) < Duration::from_millis(500))
                        {
                            quit.store(true, Ordering::Relaxed);
                        } else {
                            cancel_requested.store(true, Ordering::Relaxed);
                            *last = Some(now);
                        }
                        return Some(pirust_tui::tui::InputListenerResult {
                            consume: true,
                            data: None,
                        });
                    }
                    // `/` is deliberately NOT special-cased here anymore (user
                    // directive): it now flows to the editor like any other
                    // character, so the editor's own native autocomplete
                    // (`set_autocomplete_provider` below) sees it and opens its
                    // dropdown — the correct, already-built mechanism this custom
                    // listener used to preempt.
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
            let wake = Arc::clone(&wake);
            let subscription = session.subscribe(Arc::new(move |event: &AgentSessionEvent| {
                let _ = tx.send(event.clone());
                // Hand the loop a permit *after* the event is queued, so it
                // cannot wake to an empty queue and go straight back to sleep
                // holding an unconsumed event.
                wake.notify_one();
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
            let wake = Arc::clone(&wake);
            session.set_tool_approval_decider(Arc::new(move |request: ToolApprovalRequest| {
                let (respond_tx, respond_rx) = tokio::sync::oneshot::channel();
                let _ = tx.send(ApprovalMessage {
                    request,
                    respond: respond_tx,
                });
                // Without this the agent loop would park on the oneshot while
                // the UI loop slept, and the approval prompt would not appear
                // until the idle timeout fired.
                wake.notify_one();
                // Await the user's decision from the UI loop. The oneshot is
                // cancelled if the turn task is aborted (Ctrl+C), unblocking
                // the agent loop.
                Box::pin(async move { respond_rx.await.unwrap_or(ToolApprovalDecision::Deny) })
            }));
        }

        Self {
            tui,
            editor: Rc::clone(&editor),
            input_rx,
            submit_rx,
            _submit_tx: submit_tx,
            event_rx,
            _event_tx: event_tx,
            replay_input: VecDeque::new(),
            wake,
            loop_iterations: 0,
            pending_prompts: VecDeque::new(),
            quit,
            cancel_requested,
            last_ctrl_c,
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
            streaming_thinking: None,
            thinking: ThinkingRegistry::new(),
            pending_diffs: HashMap::new(),
            timings: None,
            debug_log,
            debug_panel,
            pending_tools: HashMap::new(),
            _approval_tx: approval_tx,
            approval_rx,
            pending_approval: None,
            model_picker: None,
            resume_picker: None,
            welcome,
            model_entries: Vec::new(),
            modal_text,
        }
    }

    /// Drive the TUI without blocking while a model turn runs.
    ///
    /// # Why this loop parks instead of polling
    ///
    /// It used to end every iteration with an unconditional
    /// `tokio::time::sleep(10ms)`, so an idle pirust woke, took four `RefCell`
    /// borrows, drained four empty channels, polled the terminal size and
    /// re-rendered **one hundred times a second** while the user was reading
    /// the screen or thinking. On a laptop that is a measurable battery cost
    /// for literally no work, and it put a 10ms floor under input latency for
    /// free.
    ///
    /// Now every producer can wake the loop — keystrokes through the tokio
    /// input channel, session events and approval requests through
    /// [`Self::wake`] — so [`Self::wait_for_work`] parks the task until there
    /// is genuinely something to do. The timeout that remains is a safety net
    /// for the one input the loop cannot be notified about (a terminal
    /// resize, which is polled), and it is chosen by whether a turn is live.
    pub async fn run_async(&mut self) {
        loop {
            if self.quit.load(Ordering::Relaxed) {
                break;
            }
            self.loop_iterations += 1;
            // Whether this iteration found anything at all. When it did, the
            // loop goes straight round again rather than parking: a burst of
            // input or events is drained back-to-back at full speed, and only
            // a genuinely quiet iteration pays for a wait.
            let mut did_work = false;
            // Replayed keys (the one that ended the last wait) come first so
            // ordering is preserved, then whatever else has queued up.
            loop {
                let data = match self.replay_input.pop_front() {
                    Some(data) => data,
                    None => match self.input_rx.try_recv() {
                        Ok(data) => data,
                        Err(_) => break,
                    },
                };
                did_work = true;
                // Ctrl+C is global and must outrank every modal. The branches
                // below never reach `TUI::handle_input`, so the global input
                // listener that normally sees Ctrl+C never runs while a picker
                // or an approval prompt is open — which used to leave the user
                // with no way to cancel the turn or quit from inside one.
                if (self.model_picker.is_some()
                    || self.resume_picker.is_some()
                    || self.pending_approval.is_some())
                    && pirust_tui::keys::matches_key(&data, "ctrl+c")
                {
                    self.handle_ctrl_c();
                    continue;
                }
                // An open modal owns input until dismissed.
                if self.model_picker.is_some() {
                    self.handle_model_picker_key(&data);
                } else if self.resume_picker.is_some() {
                    self.handle_resume_picker_key(&data);
                } else if self.pending_approval.is_some() {
                    // While a tool awaits approval, a single decision key routes to
                    // the approval instead of the editor/loop.
                    self.handle_approval_key(&data);
                } else if pirust_tui::keys::matches_key(&data, "ctrl+o") {
                    // Expand/collapse the most recent reasoning block. The
                    // design spec names Ctrl+O for exactly this, and it is
                    // handled here rather than in the global input listener so
                    // it cannot fire while a modal owns the keyboard.
                    self.thinking.toggle_latest();
                    self.repaint();
                } else if pirust_tui::keys::matches_key(&data, "escape")
                    && !self.editor.borrow().is_showing_autocomplete()
                {
                    // No modal or autocomplete dropdown (e.g. the "/" command
                    // list) is open here — this is the plain "cancel the
                    // active turn" case the global listener used to swallow
                    // before it could ever reach a modal.
                    self.cancel_requested.store(true, Ordering::Relaxed);
                } else {
                    self.tui.borrow_mut().handle_input(&data);
                }
            }
            // Coalesce stream updates: only the freshest MessageUpdate is
            // rendered this iteration (plan.md step 2 backpressure).
            let mut latest_update: Option<AgentSessionEvent> = None;
            while let Ok(event) = self.event_rx.try_recv() {
                did_work = true;
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
                did_work = true;
                self.show_approval(approval);
            }
            if self.cancel_requested.swap(false, Ordering::Relaxed) {
                did_work = true;
                if self.turn_state == TurnState::Running
                    || self.turn_state == TurnState::AwaitingApproval
                {
                    self.turn_state = TurnState::Cancelling;
                    // B2: cancel cooperatively instead of `JoinHandle::abort()`.
                    // A hard abort drops the turn future mid-await, so
                    // `finish_run()` never runs and every later prompt fails
                    // `BusyPrompt` forever. `Agent::abort()` cancels the run's
                    // token; the streaming loop and the approval decider both
                    // now race against it and unwind on their own, so the
                    // task below finishes normally and `finish_turn` runs.
                    self.session.abort();
                }
                self.repaint();
            }
            if self
                .active_turn
                .as_ref()
                .is_some_and(JoinHandle::is_finished)
            {
                did_work = true;
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
                if text.is_empty() {
                    continue;
                }
                did_work = true;
                // Slash commands are UI-local and instant, so they run even
                // mid-turn — `/hotkeys` or `/session` while the model thinks
                // is exactly when a user wants them. Only prompts queue.
                if text.starts_with('/') {
                    self.dispatch_command(&text);
                } else if self.active_turn.is_none() {
                    self.start_turn(text);
                } else {
                    self.enqueue_prompt(text);
                }
            }
            // A turn just ended and something is waiting: start it now rather
            // than after the next wakeup, so a queued prompt does not sit
            // visibly idle for a whole timeout.
            if self.active_turn.is_none() {
                if let Some(next) = self.pending_prompts.pop_front() {
                    did_work = true;
                    self.refresh_status();
                    self.start_turn(next);
                }
            }
            // Detect a terminal resize (the TUI's own resize callback is not
            // wired this wave; the loop polls size directly and re-renders).
            let size = self.tui.borrow().terminal_rows();
            if Some(size) != self.last_size {
                did_work = true;
                self.last_size = Some(size);
                // A real invalidation: every cached line is the wrong width or
                // in the wrong row, so the diff cache must go. This is the one
                // place the force flag belongs (see `repaint`).
                self.tui.borrow_mut().request_render(true);
                self.tui.borrow_mut().invalidate();
            }
            self.prune_scrollback();
            self.tui.borrow_mut().poll();
            if !did_work {
                self.wait_for_work().await;
            }
        }
    }

    /// Park until a producer has something, or the resize poll is due.
    ///
    /// Three things can end the wait:
    ///
    /// * a keystroke — the tokio input channel is awaited directly, so this is
    ///   woken by the terminal reader thread's `send` with no timer involved;
    /// * a session event or approval request — [`Self::wake`]'s stored permit,
    ///   posted by the agent thread;
    /// * the timeout — the safety net for terminal resize, which nothing
    ///   notifies us about because `TUI`'s own resize callback is not wired.
    ///
    /// The timeout is the only thing that still costs idle wakeups, so it is
    /// deliberately asymmetric. `RESIZE_POLL_ACTIVE` keeps a live turn's
    /// layout honest while output is streaming; `RESIZE_POLL_IDLE` is 25×
    /// longer, because a resize while the user is idle only has to be noticed
    /// before they type — and a keystroke ends the wait instantly anyway.
    ///
    /// A recovered keystroke is pushed back into the queue rather than handled
    /// here: `select!` hands us the value, and dropping it would eat the key.
    /// Re-queueing keeps the single drain site in `run_async` authoritative.
    async fn wait_for_work(&mut self) {
        /// Resize-poll period while a turn is streaming.
        const RESIZE_POLL_ACTIVE: Duration = Duration::from_millis(20);
        /// Resize-poll period while nothing is running. 500ms is ~50× fewer
        /// wakeups than the old unconditional 10ms tick, and is invisible:
        /// input, events and approvals all wake the loop on their own.
        const RESIZE_POLL_IDLE: Duration = Duration::from_millis(500);

        let live = matches!(
            self.turn_state,
            TurnState::Running | TurnState::AwaitingApproval | TurnState::Cancelling
        ) || self.active_turn.is_some();
        let timeout = if live {
            RESIZE_POLL_ACTIVE
        } else {
            RESIZE_POLL_IDLE
        };

        tokio::select! {
            biased;
            data = self.input_rx.recv() => {
                if let Some(data) = data {
                    self.replay_input.push_back(data);
                }
            }
            _ = self.wake.notified() => {}
            _ = tokio::time::sleep(timeout) => {}
        }
    }

    /// Drop chat entries that have scrolled far enough above the viewport that
    /// the renderer can no longer address them, so a long session stops paying
    /// for its whole history on every frame.
    ///
    /// Every user message, assistant message, tool box, notice and separator
    /// used to stay in the chat container for the life of the process. Nothing
    /// was ever removed, and `Container::render` walks every child on every
    /// frame, so both the per-frame cost and the retained memory grew without
    /// limit — measured at roughly half a microsecond per entry per frame, so
    /// a 5,000-entry session spent milliseconds per frame rebuilding text that
    /// is nowhere near the screen.
    ///
    /// Only entries the differential renderer has already given up on are
    /// dropped: `lines_above_viewport` is the renderer's own count of rows it
    /// would have to full-redraw to touch, and `RETAINED_SCREENS` holds back
    /// several screens more on top of that, so what is dropped is well past
    /// the top of the terminal. Those rows still exist in the terminal's own
    /// scrollback — this drops pirust's *copy* of them, not the user's.
    ///
    /// `forget_leading_lines` is what makes it invisible: it shifts the
    /// renderer's diff state by the same number of rows, so the next frame
    /// still diffs like-for-like instead of seeing the document renumber.
    fn prune_scrollback(&mut self) {
        /// Screens of already-scrolled-off history to keep anyway. Generous on
        /// purpose: a resize full-redraws from the retained document, so this
        /// is the window that survives one.
        const RETAINED_SCREENS: usize = 10;

        let (budget, width) = {
            let tui = self.tui.borrow();
            let keep = RETAINED_SCREENS * tui.terminal_rows() as usize;
            (
                tui.lines_above_viewport().saturating_sub(keep),
                tui.terminal_columns() as usize,
            )
        };
        if budget == 0 {
            return;
        }
        let dropped = self.chat.borrow_mut().drop_leading_children(width, budget);
        if dropped > 0 {
            self.tui.borrow_mut().forget_leading_lines(dropped);
        }
    }

    /// Finish the active turn: clear the streaming message and approval, drop
    /// stale pending tools, and refresh the status line.
    fn finish_turn(&mut self, outcome: TurnState) {
        // A cancelled turn can leave a thinking block mid-stream. Closing it
        // flips its header from "Thinking…" to a settled summary, so the
        // transcript does not keep claiming work is in progress.
        if let Some(th) = self.streaming_thinking.take() {
            th.borrow_mut().finish();
        }
        self.streaming_text = None;
        self.streaming_turn = None;
        self.pending_tools.clear();
        self.pending_diffs.clear();
        self.pending_approval = None;
        self.active_turn = None;
        self.turn_state = outcome;
        // Stop the clock and put the timing summary in the transcript. This is
        // the design spec's "elapsed time" and "request id" requirement, and
        // it is the only place they are cheap to show: once per turn, not per
        // frame.
        if let Some(timings) = &mut self.timings {
            timings.mark_end();
            let summary = timings.summary();
            self.log(crate::interactive_debug::LogLevel::Info, &summary);
            if self.debug_panel.borrow().is_visible() {
                self.show_notice(summary);
            }
        }
        self.refresh_status();
        self.repaint();
    }

    /// Append one line to the bounded debug log.
    ///
    /// Swallows a poisoned mutex rather than propagating it: the log is
    /// instrumentation, and a panic in the *logger* while reporting a panic is
    /// how a TUI ends up wedged with the terminal in raw mode.
    fn log(&self, level: crate::interactive_debug::LogLevel, message: &str) {
        if let Ok(mut log) = self.debug_log.lock() {
            match level {
                crate::interactive_debug::LogLevel::Error => log.error(message),
                crate::interactive_debug::LogLevel::Warn => log.warn(message),
                crate::interactive_debug::LogLevel::Info => log.info(message),
                crate::interactive_debug::LogLevel::Debug => log.debug(message),
            }
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
        // Same bookkeeping `start_turn` does, so `render_event`'s live-turn
        // guard means the same thing on both paths.
        self.turn_state = TurnState::Running;
        self.turn_id += 1;
        let user_text = Rc::new(RefCell::new(Text::new(
            format!("{} {text}", glyph("▶", ">")),
            0,
            0,
        )));
        self.chat
            .borrow_mut()
            .add_child(user_text as SharedComponent);
        // Forced, unlike everywhere else (see `repaint`): the very next line
        // blocks the thread until the turn ends, so this frame must go out now
        // rather than wait for the throttle window in a later `poll()`.
        self.tui.borrow_mut().request_render(true);
        self.tui.borrow_mut().poll();
        let session = Arc::clone(&self.session);
        let text = text.to_string();
        let runtime = self.runtime.clone();
        let _ = runtime.block_on(async move { session.prompt(&text, None).await });
        // The state stays `Running` on purpose: `run()` drains this turn's
        // events on the NEXT loop iteration, after `prompt` has already
        // returned, so marking the turn finished here would make the live-turn
        // guard in `render_event` drop the very stream it just produced.
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
            "debug" => self.toggle_debug_panel(),
            "thinking" => self.toggle_thinking(arg),
            "export" => self.run_export(arg),
            "copy" => self.run_copy(),
            "trust" => self.run_trust(arg),
            "changelog" => self.run_changelog(arg),
            "quit" => {
                self.quit.store(true, Ordering::Relaxed);
            }
            _ => {
                if BUILTIN_SLASH_COMMANDS
                    .iter()
                    .any(|(name, _, _)| *name == command)
                {
                    // A precise reason beats a flat "not available": it names
                    // the missing seam, so the answer is actionable instead of
                    // just a refusal.
                    match crate::interactive_commands::unavailable_reason(&command) {
                        Some(reason) => self.show_error(format!("/{command}: {reason}")),
                        None => {
                            self.show_error(format!("/{command} is not available in this session"))
                        }
                    }
                } else {
                    self.show_error(format!("Unknown command: /{command}"));
                }
            }
        }
    }

    /// `/help` — the registered command list, with the same availability
    /// marking the autocomplete dropdown uses (audit #22).
    fn show_help(&mut self) {
        // Grouped by category with aligned columns, rather than the flat
        // 28-line dump this used to print.
        let help = crate::interactive_commands::command_help_lines(&slash_command_available);
        self.show_notice(help);
    }

    /// `/hotkeys` — the keyboard shortcuts the TUI implements.
    fn show_hotkeys(&mut self) {
        self.show_notice(
            "Ctrl+D    quit (empty editor)\n\
             Ctrl+C    cancel the active turn (twice within 500ms quits)\n\
             Esc       cancel the active turn, or close an open picker\n\
             Ctrl+O    expand/collapse the latest reasoning block\n\
             r / a / d resolve a tool-approval prompt (run once / always / deny)\n\
             /         open the command palette\n\
             ↑ / ↓     move in a picker · Enter select · Esc dismiss\n\
             /debug    show the debug panel · /thinking [on|off] all reasoning",
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
        self.show_notice(lines.join("\n"));
    }

    /// `/name <name>` — set the session display name.
    ///
    /// Reports honestly that nothing was renamed. This used to answer
    /// "Session renamed to: X" for any argument, but there is no rename on
    /// `PrintModeSession`, so nothing was written anywhere and the next
    /// `/session` still showed the old name. Claiming a write that did not
    /// happen is worse than saying the command is not wired yet, so
    /// `slash_command_available` reports it unavailable too and both `/help`
    /// and the autocomplete dropdown mark it as such.
    fn set_session_name(&mut self, arg: Option<&str>) {
        let name = arg.map(str::trim).filter(|n| !n.is_empty());
        match name {
            None => self.show_error("Usage: /name <name>"),
            // Echoes the name the store actually wrote, not the argument:
            // `append_session_info` collapses newline runs to single spaces
            // and trims, so the two can differ.
            Some(name) => match self.session.set_session_name(name) {
                Ok(written) => self.show_notice(format!("Session renamed to: {written}")),
                Err(error) => self.show_error(format!("Could not rename the session: {error}")),
            },
        }
    }

    /// `/models` — the active provider's model + the configured list headline.
    fn show_models_list(&mut self) {
        match &self.runtime_status {
            Some(status) => {
                self.show_notice(format!(
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
        self.show_notice("Model list refreshed");
    }

    /// Render a [`CommandOutcome`] from [`crate::interactive_commands`] into
    /// the chat. One place, so every wired command reports consistently.
    fn apply_outcome(&mut self, outcome: CommandOutcome) {
        match outcome {
            CommandOutcome::Notice(text) => self.show_notice(text),
            CommandOutcome::Error(text) => self.show_error(text),
            CommandOutcome::Quit => self.quit.store(true, Ordering::Relaxed),
            CommandOutcome::OpenModelPicker => self.open_model_picker(),
            CommandOutcome::OpenSessionPicker => self.open_resume_picker(),
            CommandOutcome::OpenSettings => {
                self.show_error("/settings needs the SettingsManager, which the TUI does not hold")
            }
            CommandOutcome::ToggleDebug => self.toggle_debug_panel(),
            // OSC 52: written straight to the terminal, not into the
            // transcript — it is a control sequence, not text to display.
            CommandOutcome::CopyToClipboard(sequence) => {
                self.tui.borrow_mut().write_raw(&sequence);
                self.show_notice("Copied the last assistant message to the clipboard");
            }
        }
    }

    /// `/export [path]` — write the transcript as JSONL (default) or HTML.
    fn run_export(&mut self, arg: Option<&str>) {
        let state = self.session.state();
        let session_id = self.session.header().map(|h| h.id);
        let outcome = crate::interactive_commands::export_session(
            &state.messages,
            session_id.as_deref(),
            arg,
        );
        self.apply_outcome(outcome);
    }

    /// `/copy` — put the last assistant message on the clipboard via OSC 52,
    /// which works over SSH and needs no platform clipboard binding.
    fn run_copy(&mut self) {
        let state = self.session.state();
        let outcome = crate::interactive_commands::copy_last_message(&state.messages);
        self.apply_outcome(outcome);
    }

    /// `/trust [on|off]` — record this project's trust decision.
    fn run_trust(&mut self, arg: Option<&str>) {
        let trusted = match arg.map(str::to_ascii_lowercase).as_deref() {
            None | Some("on") | Some("yes") | Some("trust") => true,
            Some("off") | Some("no") | Some("revoke") => false,
            Some(other) => {
                self.show_error(format!("Usage: /trust [on|off] (got {other:?})"));
                return;
            }
        };
        let cwd = match self.session.header().map(|h| h.cwd) {
            Some(cwd) => cwd,
            None => {
                self.show_error("/trust needs a session cwd, and this session reports none");
                return;
            }
        };
        let config = crate::config::ConfigEnv::from_process_env();
        let agent_dir = match config.agent_dir() {
            Ok(dir) => dir,
            Err(error) => {
                self.show_error(format!(
                    "/trust could not resolve the agent directory: {error}"
                ));
                return;
            }
        };
        let path = crate::interactive_commands::trust_store_path(&agent_dir);
        let outcome = crate::interactive_commands::set_project_trust(&path, &cwd, trusted);
        self.apply_outcome(outcome);
    }

    /// `/changelog [path]` — pirust ships no `CHANGELOG.md`, so the path is
    /// required rather than guessed. Saying so beats reading a stale file from
    /// whatever directory the binary happens to sit in.
    fn run_changelog(&mut self, arg: Option<&str>) {
        /// How many entries to show — enough to be useful, short enough not to
        /// bury the transcript.
        const MAX_ENTRIES: usize = 5;

        let Some(path) = arg else {
            self.show_error(
                "Usage: /changelog <path-to-CHANGELOG.md> — pirust does not ship one, \
                 so there is no default to read",
            );
            return;
        };
        let outcome =
            crate::interactive_commands::changelog_text(std::path::Path::new(path), MAX_ENTRIES);
        self.apply_outcome(outcome);
    }

    /// `/debug` — show or hide the debug panel.
    ///
    /// The panel reads the same bounded ring buffer the panic report does, so
    /// turning it on costs nothing extra: the events were already being
    /// recorded, they just were not on screen.
    fn toggle_debug_panel(&mut self) {
        self.debug_panel.borrow_mut().toggle();
        let visible = self.debug_panel.borrow().is_visible();
        let path = std::env::var(crate::interactive_debug::DEBUG_LOG_ENV).ok();
        if visible {
            let sink = match &path {
                Some(path) => format!(" · also logging to {path}"),
                None => format!(
                    " · set {}=<path> to also write a log file",
                    crate::interactive_debug::DEBUG_LOG_ENV
                ),
            };
            self.show_notice(format!("Debug panel on{sink}"));
        } else {
            self.show_notice("Debug panel off");
        }
        self.repaint();
    }

    /// `/thinking [on|off]` — expand or collapse reasoning blocks.
    ///
    /// Ctrl+O toggles the most recent block; this sets *all* of them at once,
    /// which is what you want after the fact when reading back a long session.
    fn toggle_thinking(&mut self, arg: Option<&str>) {
        let expanded = match arg.map(str::to_ascii_lowercase).as_deref() {
            Some("on") | Some("expand") | Some("show") => true,
            Some("off") | Some("collapse") | Some("hide") => false,
            None => true,
            Some(other) => {
                self.show_error(format!("Usage: /thinking [on|off] (got {other:?})"));
                return;
            }
        };
        self.thinking.toggle_all(expanded);
        self.show_notice(if expanded {
            "Reasoning blocks expanded (Ctrl+O toggles the latest)"
        } else {
            "Reasoning blocks collapsed (Ctrl+O toggles the latest)"
        });
        self.repaint();
    }

    /// `/reload-extensions` (Wave 5) — rescan `<agent_dir>/extensions/*.wasm`
    /// for extensions not already loaded, without restarting `pirust`.
    fn reload_extensions(&mut self) {
        match self.session.reload_wasm_extensions() {
            Ok(0) => self.show_notice("No new extensions found"),
            Ok(count) => self.show_notice(format!("Loaded {count} new extension(s)")),
            Err(error) => self.show_error(format!("Could not reload extensions: {error}")),
        }
    }

    /// Hold a prompt typed while a turn was already running.
    ///
    /// The old code dropped these silently, which is the worst possible
    /// outcome: the editor clears on submit, so the text was gone from the
    /// screen *and* gone from the queue, and nothing said so. Queueing it and
    /// echoing it dimmed keeps the promise the cleared editor implies.
    ///
    /// The queue is bounded. An unbounded one is a memory leak with a user
    /// holding Enter on it, and a hundred stacked prompts is never what
    /// somebody meant.
    fn enqueue_prompt(&mut self, text: String) {
        /// How many prompts may wait behind the running turn.
        const MAX_QUEUED_PROMPTS: usize = 16;

        if self.pending_prompts.len() >= MAX_QUEUED_PROMPTS {
            self.show_error(format!(
                "Queue is full ({MAX_QUEUED_PROMPTS} prompts waiting) — this one was not accepted"
            ));
            return;
        }
        let position = self.pending_prompts.len() + 1;
        self.pending_prompts.push_back(text.clone());
        self.show_notice(format!(
            "{} queued #{position}: {text}",
            glyph("⋯", "[queued]")
        ));
        self.refresh_status();
    }

    /// How many prompts are waiting behind the active turn — the regression
    /// seam for the queueing behaviour, and what the status line reports.
    pub fn queued_prompts(&self) -> usize {
        self.pending_prompts.len()
    }

    /// How many times the async loop body has run — the regression seam for
    /// "an idle TUI parks instead of polling".
    pub fn loop_iterations(&self) -> u64 {
        self.loop_iterations
    }

    /// Start one turn as a task so input and streamed events remain responsive.
    fn start_turn(&mut self, text: String) {
        self.turn_state = TurnState::Running;
        self.turn_id += 1;
        // Fresh instrumentation per turn: a new request id to quote in an
        // error report, and the clock that produces the time-to-first-token
        // figure the status line shows once the turn ends.
        self.timings = Some(TurnTimings::new(RequestId::next()));
        // The first-run block has done its job — free its rows for the
        // transcript. Idempotent, so no need to track whether this is the
        // first turn.
        self.welcome.borrow_mut().dismiss();
        self.refresh_status();
        // User message line (Pi adds the user message on `message_start`
        // with role user; we mirror that by appending it before the turn).
        // Boxed in `userMessageBg` (real Pi's own theme color), the same way
        // `ToolExecutionComponent` boxes tool calls — so typed input is
        // visually distinct from both plain assistant text and tool output.
        let user_line = format!("{} {text}", glyph("▶", ">"));
        let user_text = Rc::new(RefCell::new(Text::with_bg_fn(
            user_line,
            1,
            1,
            interactive_theme::bg(dark::USER_MESSAGE_BG),
        )));
        self.chat
            .borrow_mut()
            .add_child(user_text as SharedComponent);
        self.repaint();
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
            self.pending_prompts.len(),
        ));
        self.repaint();
    }

    /// Set the status line to a specific turn-state word.
    fn set_status(&mut self, state: TurnState) {
        self.turn_state = state;
        self.refresh_status();
    }

    /// Ask for a repaint after a content change.
    ///
    /// Deliberately NOT `request_render(true)`: the force flag clears
    /// `previous_lines`/`previous_width`, which is the TUI's whole line-diff
    /// cache, so the next `poll()` takes `do_render`'s `full_render` path and
    /// repaints every row of the terminal. That is only correct for a real
    /// invalidation (a resize). Appending a chat line or updating streamed
    /// assistant text changes a handful of rows, and the differential renderer
    /// exists precisely to rewrite only those — forcing here made every stream
    /// delta a full-screen repaint (`docs/tui-design-audit.md`: "Rust does not
    /// guarantee speed if every stream update causes a full terminal redraw").
    /// Components clear their own caches on mutation (`Text::set_text` →
    /// `clear_cache`), so the diff sees fresh lines without the force flag.
    fn repaint(&self) {
        self.tui.borrow_mut().request_render(false);
    }

    /// Full-screen repaints performed so far — the regression seam for the
    /// "streaming must not force full redraws" test.
    pub fn full_redraws(&self) -> u64 {
        self.tui.borrow().full_redraws()
    }

    /// How many entries the chat container still holds — the regression seam
    /// for `prune_scrollback`, which has to keep this bounded no matter how
    /// long the session runs.
    pub fn chat_entries(&self) -> usize {
        self.chat.borrow().len()
    }

    /// Append an informational line to the chat (command output, confirmations).
    fn show_notice(&mut self, message: impl Into<String>) {
        let notice = Rc::new(RefCell::new(Text::new(message.into(), 0, 0)));
        self.chat.borrow_mut().add_child(notice as SharedComponent);
        self.repaint();
    }

    fn show_error(&mut self, message: impl Into<String>) {
        let error = Rc::new(RefCell::new(Text::new(
            // `[error]` rather than a red ✗ when glyphs are off: the spec's
            // accessibility bar is "no colour-only meaning", so the state has
            // to survive both a monochrome and an ASCII-only terminal.
            format!("{} {}", glyph("✗", "[error]"), message.into()),
            0,
            0,
        )));
        self.chat.borrow_mut().add_child(error as SharedComponent);
        self.repaint();
    }

    /// Render a pending tool-approval prompt and wait for a decision key.
    fn show_approval(&mut self, approval: ApprovalMessage) {
        let request = &approval.request;
        let args = serde_json::to_string(&request.args).unwrap_or_default();
        // Risk warning for destructive tools (plan.md step 7): the exact
        // command, the cwd it would run in, and a warning for bash.
        let mut lines = vec![format!(
            "{} Tool execution requires approval: {}",
            glyph("⚠", "[approval]"),
            request.tool_name
        )];
        if !args.is_empty() && args != "null" {
            lines.push(args.clone());
        }
        if request.tool_name == "bash" {
            if let Some(cwd) = self.session.header().map(|h| h.cwd) {
                lines.push(format!("cwd: {cwd}"));
            }
            lines.push(format!(
                "{} This command runs on your machine — review it carefully",
                glyph("⚠", "[warning]")
            ));
        }
        lines.push("[r]un once · [a]lways allow · [d]eny".to_string());
        self.set_status(TurnState::AwaitingApproval);
        let notice = Rc::new(RefCell::new(Text::new(lines.join("\n"), 0, 0)));
        self.chat.borrow_mut().add_child(notice as SharedComponent);
        self.repaint();
        self.pending_approval = Some(approval);
    }

    /// Ctrl+C, from either the TUI's global input listener or the modal
    /// bypass in `run_async`. A lone press cancels the active turn; a second
    /// press within 500ms quits (Pi's `handleCtrlC`/`lastSigintTime` window).
    fn handle_ctrl_c(&mut self) {
        let now = Instant::now();
        let mut last = self.last_ctrl_c.lock().unwrap();
        if last.is_some_and(|t| now.duration_since(t) < Duration::from_millis(500)) {
            self.quit.store(true, Ordering::Relaxed);
        } else {
            self.cancel_requested.store(true, Ordering::Relaxed);
            *last = Some(now);
        }
    }

    /// Resolve a pending approval from a decision key (`r`/`a`/`d`), or close
    /// it with Esc. Esc denies rather than leaving the prompt up: the agent
    /// loop is parked on the oneshot, so the prompt must resolve to something.
    fn handle_approval_key(&mut self, data: &str) {
        if pirust_tui::keys::matches_key(data, "escape") {
            self.resolve_approval(ToolApprovalDecision::Deny);
            return;
        }
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
            self.resolve_approval(decision);
        }
    }

    /// Answer the parked approval oneshot and note the outcome in the chat.
    fn resolve_approval(&mut self, decision: ToolApprovalDecision) {
        let Some(approval) = self.pending_approval.take() else {
            return;
        };
        let _ = approval.respond.send(decision);
        let verb = match decision {
            ToolApprovalDecision::RunOnce => "allowed (once)",
            ToolApprovalDecision::AlwaysAllow => "allowed (always)",
            ToolApprovalDecision::Deny => "denied",
        };
        self.show_notice(format!(
            "{} {} {verb}",
            glyph("✓", "[ok]"),
            approval.request.tool_name
        ));
        self.set_status(TurnState::Running);
    }

    /// Render one session event into the chat container.
    fn render_event(&mut self, event: &AgentSessionEvent) {
        // A stale event from a previous turn must not bleed into the current
        // one (plan.md step 2, audit #16). `AgentSessionEvent` carries no turn
        // id of its own, so the guard is built from what the loop does know:
        //
        //   * `turn_live` — the turn task is aborted on cancel, but the agent
        //     thread can still be mid-listener-call and deliver one more event
        //     afterwards. Opening a streaming component for a turn that is
        //     already Cancelled/Failed/Completed leaves a zombie message in the
        //     chat that nothing ever fills in or removes.
        //   * `streaming_turn` — the turn id that owns the live streaming
        //     component, so an update can only ever write into the component
        //     the *same* turn opened. `finish_turn` clears it.
        //
        // This field pair used to be written and never read; the comment here
        // claimed a guard that did not exist.
        // Log every event, including the ten variants the match below ignores.
        // Those used to vanish into a silent `_ => {}`, which is exactly what
        // made a misbehaving turn impossible to explain after the fact.
        // `record_event` logs the variant name plus small scalars only — never
        // the message payloads — so this is bounded and cheap.
        if let Ok(mut log) = self.debug_log.lock() {
            log.record_event(event);
        }

        // Any session activity makes the first-run block stale — not just a
        // prompt the user typed. Tool calls and messages can arrive from a
        // resumed session or an extension, and in every case the transcript
        // now has content worth the rows. `dismiss` early-returns once set.
        self.welcome.borrow_mut().dismiss();

        let turn_live = matches!(
            self.turn_state,
            TurnState::Running | TurnState::AwaitingApproval
        );
        match event {
            AgentSessionEvent::MessageStart { message } => {
                let role = message.get("role").and_then(|r| r.as_str());
                if role == Some("user") {
                    // User messages also arrive as events; `run_turn` already
                    // added them, so skip duplicates (Pi adds them on
                    // message_start; we did it in run_turn).
                } else if role == Some("assistant") && turn_live {
                    // Reasoning is mounted *before* the answer so it reads
                    // top-down the way the model produced it. It renders
                    // nothing at all until thinking text actually arrives, so
                    // a non-reasoning model pays one `Rc` and no rows.
                    let thinking = Rc::new(RefCell::new(ThinkingComponent::new()));
                    self.thinking.register(&thinking);
                    self.chat
                        .borrow_mut()
                        .add_child(Rc::clone(&thinking) as SharedComponent);
                    self.streaming_thinking = Some(thinking);

                    // Begin streaming the answer itself, through the Markdown
                    // renderer rather than as literal text.
                    let text = Rc::new(RefCell::new(MarkdownText::new("", 0, 0)));
                    self.chat
                        .borrow_mut()
                        .add_child(Rc::clone(&text) as SharedComponent);
                    self.streaming_text = Some(text);
                    self.streaming_turn = Some(self.turn_id);
                }
            }
            AgentSessionEvent::MessageUpdate { message, .. } => {
                if self.streaming_turn != Some(self.turn_id) {
                    return;
                }
                // First visible token of the turn — the latency number that
                // actually describes how fast pirust feels.
                if let Some(timings) = &mut self.timings {
                    timings.mark_first_token();
                }
                // Reasoning first: the thinking component stays silent while
                // `thinking_text` is empty, so this costs a scan of the
                // content array and nothing else for a non-reasoning model.
                if let Some(th) = &self.streaming_thinking {
                    let reasoning = thinking_text(message);
                    if !reasoning.is_empty() {
                        th.borrow_mut().set_text(&reasoning);
                    }
                }
                if let Some(st) = &self.streaming_text {
                    st.borrow_mut().set_text(&assistant_text(message));
                }
                self.repaint();
            }
            AgentSessionEvent::MessageEnd { message } => {
                if self.streaming_turn != Some(self.turn_id) {
                    return;
                }
                if let Some(th) = self.streaming_thinking.take() {
                    let reasoning = thinking_text(message);
                    if !reasoning.is_empty() {
                        th.borrow_mut().set_text(&reasoning);
                    }
                    // Flips the collapsed summary from "Thinking…" to
                    // "Thought for N lines" and stops the live tail.
                    th.borrow_mut().finish();
                }
                if let Some(st) = self.streaming_text.take() {
                    st.borrow_mut().set_text(&assistant_text(message));
                    self.streaming_turn = None;
                }
                self.repaint();
            }
            AgentSessionEvent::AgentEnd { .. } => {
                // Turn over: a blank separator line.
                let sep = Rc::new(RefCell::new(pirust_tui::components::spacer::Spacer::new(1)));
                self.chat.borrow_mut().add_child(sep as SharedComponent);
                self.repaint();
            }
            AgentSessionEvent::ToolExecutionStart {
                tool_call_id,
                tool_name,
                args,
            } => {
                // `handleEvent` case "tool_execution_start"
                // (interactive-mode.ts:3263-3285): add a ToolExecutionComponent
                // if not already present, then mark execution started.
                if !self.pending_tools.contains_key(tool_call_id) {
                    // `write`/`edit` get a real diff instead of their args
                    // JSON — the design spec's file-safety requirement is
                    // "preview diffs before writes, identify changed paths".
                    // This runs on `tool_execution_start`, i.e. *before* the
                    // tool has written anything, which is what makes the
                    // on-disk read below the true "before" side.
                    let change = parse_file_change(tool_name, args);
                    let component = Rc::new(RefCell::new(ToolExecutionComponent::new(
                        tool_name.clone(),
                        args.clone(),
                        change.is_some(),
                    )));
                    self.chat
                        .borrow_mut()
                        .add_child(Rc::clone(&component) as SharedComponent);
                    if let Some(change) = change {
                        // Read the current file so the preview is a genuine
                        // before/after. A failure here is not an error: a
                        // brand-new file legitimately has no old content, and
                        // the renderer already labels that case "new file".
                        let change = match crate::interactive_diff::read_old_content_from_disk(
                            std::path::Path::new(&change.path),
                        ) {
                            Ok(old) => change.with_old_content(old),
                            Err(_) => change,
                        };
                        let preview = Rc::new(RefCell::new(DiffPreview::new(change)));
                        self.chat
                            .borrow_mut()
                            .add_child(Rc::clone(&preview) as SharedComponent);
                        self.pending_diffs.insert(tool_call_id.clone(), preview);
                    }
                    self.pending_tools.insert(tool_call_id.clone(), component);
                }
                if let Some(tool) = self.pending_tools.get(tool_call_id) {
                    tool.borrow_mut().set_expanded(false); // Pi: this.toolOutputExpanded
                    tool.borrow_mut().mark_execution_started();
                }
                if let Some(timings) = &mut self.timings {
                    timings.mark_tool_start(tool_call_id.clone(), tool_name.clone());
                }
                self.repaint();
            }
            AgentSessionEvent::ToolExecutionUpdate {
                tool_call_id,
                partial_result,
                ..
            } => {
                if let Some(tool) = self.pending_tools.get(tool_call_id) {
                    let content = result_content(partial_result);
                    tool.borrow_mut().update_result(content, false, true);
                    self.repaint();
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
                }
                // The diff preview stays mounted in the transcript — it is the
                // record of what changed — but drop this map's handle so a
                // long session does not accumulate one entry per edit.
                self.pending_diffs.remove(tool_call_id);
                if let Some(timings) = &mut self.timings {
                    timings.mark_tool_end(tool_call_id);
                }
                self.repaint();
            }
            AgentSessionEvent::CompactionStart { .. } => {
                self.set_status(TurnState::Running);
                self.show_notice(format!(
                    "{} Compacting session…",
                    glyph("♻", "[compacting]")
                ));
            }
            AgentSessionEvent::CompactionEnd { .. } => {
                self.show_notice(format!("{} Compaction finished", glyph("♻", "[compacted]")));
            }
            AgentSessionEvent::AutoRetryStart { attempt, .. } => {
                self.show_notice(format!(
                    "{} Retrying request (attempt {attempt})…",
                    glyph("⟳", "[retry]")
                ));
            }
            AgentSessionEvent::AgentSettled => {
                self.refresh_status();
            }
            _ => {}
        }
    }

    /// Set the modal band's text (see `EditorStatusBand`), updating it in
    /// place. Every modal keystroke goes through here, so a redraw must
    /// never append a new component.
    #[allow(dead_code)]
    fn show_modal(&mut self, body: String) {
        *self.modal_text.borrow_mut() = Modal::Text(Text::new(body, 1, 0));
        self.repaint();
    }

    /// Put a width-aware component in the modal slot — the pickers, which lay
    /// out columns and a viewport and so cannot be pre-rendered to a `String`.
    fn show_modal_component(&mut self, component: SharedComponent) {
        *self.modal_text.borrow_mut() = Modal::Component(component);
        self.repaint();
    }

    /// Clear the modal slot. Idempotent, so every modal-close path can call it
    /// unconditionally next to its `self.<modal> = None`.
    fn hide_modal(&mut self) {
        *self.modal_text.borrow_mut() = Modal::None;
        self.repaint();
    }

    /// The selectable model list. `main.rs` calls this after building the
    /// model runtime; without it `/model` honestly reports an empty catalog
    /// rather than showing one fabricated row.
    pub fn set_model_entries(&mut self, entries: Vec<ModelEntry>) {
        self.model_entries = entries;
    }

    /// Open the `/model` picker.
    fn open_model_picker(&mut self) {
        let picker = Rc::new(RefCell::new(PickerModelPicker::new(
            self.model_entries.clone(),
            self.picker_viewport_rows(),
        )));
        self.model_picker = Some(Rc::clone(&picker));
        self.show_modal_component(picker as SharedComponent);
    }

    /// Route a key to the model picker.
    ///
    /// All the navigation, fuzzy filtering and clamping now lives in the
    /// picker itself; this only has to act on the [`PickerAction`] it reports.
    fn handle_model_picker_key(&mut self, data: &str) {
        let Some(picker) = self.model_picker.clone() else {
            return;
        };
        let action = picker.borrow_mut().handle_key(data);
        match action {
            PickerAction::None => self.repaint(),
            PickerAction::Dismissed => {
                self.model_picker = None;
                self.hide_modal();
            }
            PickerAction::Selected(index) => {
                let chosen = self
                    .model_entries
                    .get(index)
                    .map(|entry| format!("{} / {}", entry.provider, entry.model_id));
                self.model_picker = None;
                self.hide_modal();
                match chosen {
                    // Switching the live model means rebuilding the `Agent`'s
                    // provider adapter, which only `main.rs` can do — the
                    // session seam exposes no setter. So the choice is
                    // reported with the concrete flag that applies it, rather
                    // than pretending the switch happened.
                    Some(model) => self.show_notice(format!(
                        "Selected {model}. Switching the live model is not wired to the running \
                         agent yet — start pirust with `--model {model}` to use it."
                    )),
                    None => self.show_error("No model is available to select"),
                }
                self.refresh_status();
            }
        }
    }

    /// Open the `/resume` picker over the real session store.
    fn open_resume_picker(&mut self) {
        let entries = self.session.session_entries();
        let picker = Rc::new(RefCell::new(SessionPicker::new(
            entries,
            self.picker_viewport_rows(),
        )));
        self.resume_picker = Some(Rc::clone(&picker));
        self.show_modal_component(picker as SharedComponent);
    }

    /// How many list rows a picker may occupy.
    ///
    /// The pickers render inside the bottom band, above the editor, so every
    /// row they take is a row of transcript pushed off-screen. `RESERVED`
    /// covers the picker's own header and hint lines plus the editor and
    /// status line beneath it; the clamp keeps the list usable at 80×24 (the
    /// spec's floor) without letting a 200-model list swallow a tall terminal.
    fn picker_viewport_rows(&self) -> usize {
        /// Rows the band needs for everything that is not list content.
        const RESERVED: usize = 8;
        /// Never show fewer than this, even on a very short terminal.
        const MIN_ROWS: usize = 3;
        /// Never show more than this, however tall the terminal.
        const MAX_ROWS: usize = 15;

        let rows = self.tui.borrow().terminal_rows() as usize;
        rows.saturating_sub(RESERVED).clamp(MIN_ROWS, MAX_ROWS)
    }

    /// Route a key to the session picker.
    fn handle_resume_picker_key(&mut self, data: &str) {
        let Some(picker) = self.resume_picker.clone() else {
            return;
        };
        let action = picker.borrow_mut().handle_key(data);
        match action {
            PickerAction::None => self.repaint(),
            PickerAction::Dismissed => {
                self.resume_picker = None;
                self.hide_modal();
            }
            PickerAction::Selected(_) => {
                let chosen = picker
                    .borrow()
                    .selected_entry()
                    .map(|entry| (entry.id.clone(), entry.cwd.clone()));
                self.resume_picker = None;
                self.hide_modal();
                match chosen {
                    // Resuming replaces the whole agent + session manager,
                    // which is `main.rs`'s job — `PrintModeSession` has no
                    // swap-session method. The id is echoed with the exact
                    // command that resumes it, which is genuinely useful,
                    // unlike the old flat "not available".
                    Some((id, cwd)) => self.show_notice(format!(
                        "Session {id} ({cwd}). Resuming in-place is not wired to the running \
                         agent yet — run `pirust --resume {id}` to open it."
                    )),
                    None => self.show_error("No session is available to resume"),
                }
            }
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
    queued: usize,
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
    // `queued` is the design spec's "queued" response state. It is appended
    // rather than replacing `state_word` because both are true at once: the
    // current turn is still running *and* N prompts are waiting behind it.
    let queue = if queued > 0 {
        format!(" · +{queued} queued")
    } else {
        String::new()
    };
    match header {
        Some(header) => format!(
            "cwd: {} · session: {} · {} · {} · {} · {state_word}{queue}",
            header.cwd, header.id, model, context, tools
        ),
        None => format!("session: unavailable · {model} · {context} · {state_word}{queue}"),
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

/// Whether `dispatch_command` has a working handler for `name` in this
/// session — the availability half of audit #22 ("a command visible to the
/// user must have a capability/handler or be clearly marked unavailable").
///
/// This list must stay in step with `dispatch_command`'s match arms;
/// `every_dispatched_command_is_registered` in `tests/tui_commands_status.rs`
/// fails if either side gains a name the other does not have.
pub fn slash_command_available(name: &str) -> bool {
    matches!(
        name,
        "help"
            | "hotkeys"
            | "session"
            | "model"
            | "models"
            | "resume"
            | "refresh-model-list"
            | "reload-extensions"
            | "debug"
            | "thinking"
            | "name"
            | "export"
            | "copy"
            | "trust"
            | "changelog"
            | "quit"
    )
}

/// `BUILTIN_SLASH_COMMANDS` (slash-commands.ts:19) — name, description,
/// optional argument hint.
pub const BUILTIN_SLASH_COMMANDS: &[(&str, &str, Option<&str>)] = &[
    ("help", "Show all commands", None),
    ("settings", "Open settings menu", None),
    (
        "model",
        "Select model (opens selector UI)",
        Some("<provider/model>"),
    ),
    ("models", "List available models", None),
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
    ("restart", "Restart the agent process", None),
    (
        "refresh-model-list",
        "Re-read the provider model list",
        None,
    ),
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
    // pirust-only additions, not in Pi's `BUILTIN_SLASH_COMMANDS`: the design
    // spec requires an optional debug panel and expandable reasoning, and both
    // need a discoverable way in besides a key nobody guesses.
    (
        "debug",
        "Show/hide the debug panel (recent events, timings, request id)",
        None,
    ),
    (
        "thinking",
        "Expand or collapse all reasoning blocks (Ctrl+O toggles the latest)",
        Some("[on|off]"),
    ),
];
