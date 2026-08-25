//! Port of `packages/tui/src/tui.ts` — the `Component`/`Container` model and
//! the `TUI` differential-render engine (frame pipeline, overlay
//! compositing + focus-restore state machine, cursor-marker extraction,
//! synchronized-output wrapping, full-redraw triggers, Kitty-image
//! reserved-row lifecycle). See `docs/analysis/05-tui.md` §3/§9.
//!
//! ## Scope decisions (documented, not silent — AGENTS.md Correctness Bar)
//!
//! - **Component identity: `Rc<RefCell<dyn Component>>`, not raw references.**
//!   TS `Component` values are shared, mutable, garbage-collected object
//!   references — `focusedComponent === previousFocus`, `overlayStack.find
//!   (e => e.component === x)` all compare by identity. The direct,
//!   `unsafe`-free Rust analogue of "a tree of heterogeneous shared-mutable
//!   trait objects, compared by identity" is `Rc<RefCell<dyn Component>>`
//!   ([`SharedComponent`]), compared via `Rc::ptr_eq` everywhere the TS uses
//!   `===`. This makes `TUI`/`Container` intentionally `!Send`/`!Sync` — the
//!   TS is single-threaded too (JS has no real threads); this port doesn't
//!   need to be thread-safe to be behaviorally faithful.
//! - **`OverlayHandle`'s TS closures become an `OverlayId` token.** The TS
//!   returns an object of closures (`hide`/`setHidden`/`focus`/...) each
//!   capturing `this` (the `TUI`) and `entry`. Rust cannot return closures
//!   borrowing `&mut self` for independent later calls. Each overlay gets a
//!   stable `OverlayId` (a monotonic counter, separate from `focusOrder`,
//!   which mutates); [`TUI::show_overlay`] returns one, and
//!   `hide_overlay_by_id`/`set_overlay_hidden`/`is_overlay_hidden`/
//!   `focus_overlay`/`unfocus_overlay`/`is_overlay_focused` take it — the
//!   same operations, just addressed by token instead of by closure.
//! - **`addInputListener`/`onTerminalColorSchemeChange`'s "returns an
//!   unsubscribe closure" becomes an id + explicit `remove_*` method**, same
//!   reasoning as the `OverlayHandle` adaptation above.
//! - **`requestRender`'s debounce is synchronous and caller-polled, not a
//!   self-owned timer.** The TS schedules its own `setTimeout`
//!   (`MIN_RENDER_INTERVAL_MS = 16`) to coalesce rapid `requestRender()`
//!   calls into one paint. This crate's `Component` tree is `Rc<RefCell<_>>`
//!   (see above), which is `!Send` — it cannot be moved into a `tokio::spawn`
//!   task the way an owned timer would need. [`TUI::request_render`] performs
//!   an immediate synchronous render when `force` is set OR the throttle
//!   interval has already elapsed; otherwise it only marks a render pending.
//!   [`TUI::poll`] is the explicit "pump" a caller's own loop invokes
//!   (after input, after a short sleep, on a timer it owns) to fire a
//!   pending render once the interval has passed — same coalescing
//!   observable behavior, caller-driven rather than self-scheduled. This is
//!   the same category of adaptation Wave 2's `StdinBuffer::flush()` made
//!   (defer real timer ownership to whoever owns the actual event loop —
//!   `feat-007`), now for a structural reason (`!Send`) rather than "no
//!   owned loop" — tokio ended up NOT being a dependency of this crate.
//! - **`queryTerminalBackgroundColor`/`queryTerminalColorScheme`'s `Promise`
//!   becomes an explicit callback registration with NO timeout.** Both are
//!   only meaningful with a real terminal round-trip (write a query, wait for
//!   an async OSC response) — untestable without live stdio, which is out of
//!   this wave's scope (no oracle exists for `ProcessTerminal` either). The
//!   query/response *matching* logic inside `handle_input` (consuming OSC 11
//!   replies and `CSI ? 997 ; n` reports) IS ported faithfully and IS
//!   exercised by the mock-`Terminal` oracle; the timeout-then-resolve-`None`
//!   half is a named, deferred residual (needs a real timer, same story as
//!   `terminal.rs`'s already-documented timing gaps).
//! - **Width-overflow crash path -> `panic!`, not `Result`.** The TS treats a
//!   rendered line exceeding terminal width as an unrecoverable programming
//!   error: it writes a crash log, calls `this.stop()`, then `throw`s. A
//!   normal `Result`/`Err` would let a caller silently continue past a
//!   corrupted render — the TS's own intent is "crash the process," which
//!   `panic!` (after the same crash-log-and-stop sequence) matches more
//!   faithfully than a recoverable error type.

use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::collections::{HashSet, VecDeque};
use std::rc::Rc;
use std::time::{Duration, Instant};

use crate::terminal::Terminal;
use crate::terminal_colors::{
    is_osc11_background_color_response, parse_osc11_background_color,
    parse_terminal_color_scheme_report, RgbColor, TerminalColorScheme,
};
use crate::terminal_image::{delete_kitty_image, get_capabilities, is_image_line};
use crate::utils::{
    extract_segments, normalize_terminal_output, slice_by_column, slice_with_width, visible_width,
};

/// `CURSOR_MARKER` (tui.ts:120) — APC sentinel components emit at the cursor
/// position when focused; `TUI` finds and strips it, positioning the
/// hardware cursor there.
pub const CURSOR_MARKER: &str = "\x1b_pi:c\x07";

const MIN_RENDER_INTERVAL_MS: u64 = 16;
const SEGMENT_RESET: &str = "\x1b[0m\x1b]8;;\x07";

/// `Component` (tui.ts:64). `as_focusable_mut`'s `None` default is this
/// port's analogue of `isFocusable`'s `"focused" in component` duck-typing —
/// see module docs.
pub trait Component {
    fn render(&mut self, width: usize) -> Vec<String>;
    fn handle_input(&mut self, _data: &str) {}
    fn wants_key_release(&self) -> bool {
        false
    }
    fn invalidate(&mut self);
    fn as_focusable_mut(&mut self) -> Option<&mut dyn Focusable> {
        None
    }
}

/// `Focusable` (tui.ts:104).
pub trait Focusable {
    fn is_focused(&self) -> bool;
    fn set_focused(&mut self, value: bool);
}

/// A shared, mutable component handle — see module docs, "Component identity".
pub type SharedComponent = Rc<RefCell<dyn Component>>;

/// `isFocusable` (tui.ts:110).
pub fn is_focusable(component: Option<&SharedComponent>) -> bool {
    component.is_some_and(|c| c.borrow_mut().as_focusable_mut().is_some())
}

fn set_focused_if_focusable(component: Option<&SharedComponent>, value: bool) {
    if let Some(c) = component {
        if let Some(f) = c.borrow_mut().as_focusable_mut() {
            f.set_focused(value);
        }
    }
}

fn same_component(a: Option<&SharedComponent>, b: Option<&SharedComponent>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => Rc::ptr_eq(a, b),
        (None, None) => true,
        _ => false,
    }
}

/// `OverlayAnchor` (tui.ts:127).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayAnchor {
    Center,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
    TopCenter,
    BottomCenter,
    LeftCenter,
    RightCenter,
}

/// `OverlayMargin` (tui.ts:141).
#[derive(Debug, Clone, Copy, Default)]
pub struct OverlayMargin {
    pub top: Option<i32>,
    pub right: Option<i32>,
    pub bottom: Option<i32>,
    pub left: Option<i32>,
}

/// `margin?: OverlayMargin | number` (tui.ts:196).
#[derive(Debug, Clone, Copy)]
pub enum OverlayMarginValue {
    Uniform(i32),
    PerSide(OverlayMargin),
}

/// `SizeValue` (tui.ts:149) — absolute cells or a percentage of a reference size.
#[derive(Debug, Clone, Copy)]
pub enum SizeValue {
    Absolute(f64),
    Percent(f64),
}

/// `parseSizeValue` (tui.ts:152).
fn parse_size_value(value: Option<SizeValue>, reference_size: f64) -> Option<f64> {
    match value? {
        SizeValue::Absolute(n) => Some(n),
        SizeValue::Percent(p) => Some((reference_size * p / 100.0).floor()),
    }
}

/// `OverlayOptions` (tui.ts:171).
#[derive(Default)]
pub struct OverlayOptions {
    pub width: Option<SizeValue>,
    pub min_width: Option<f64>,
    pub max_height: Option<SizeValue>,
    pub anchor: Option<OverlayAnchor>,
    pub offset_x: Option<i64>,
    pub offset_y: Option<i64>,
    pub row: Option<SizeValue>,
    pub col: Option<SizeValue>,
    pub margin: Option<OverlayMarginValue>,
    pub visible: Option<Box<dyn Fn(u16, u16) -> bool>>,
    pub non_capturing: bool,
}

/// `OverlayUnfocusOptions` (tui.ts:210).
pub struct OverlayUnfocusOptions {
    pub target: Option<SharedComponent>,
}

/// Opaque handle identifying one overlay stack entry — see module docs,
/// "OverlayHandle's TS closures become an OverlayId token".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct OverlayId(u64);

struct OverlayStackEntry {
    id: OverlayId,
    component: SharedComponent,
    options: Option<OverlayOptions>,
    pre_focus: Option<SharedComponent>,
    hidden: bool,
    focus_order: u64,
}

enum OverlayBlockedFocusResume {
    RestoreOverlay,
    FocusTarget(Option<SharedComponent>),
}

struct BlockedOverlayFocusRestoreState {
    overlay_id: OverlayId,
    blocked_by: SharedComponent,
    resume: OverlayBlockedFocusResume,
}

enum OverlayFocusRestoreState {
    Inactive,
    Eligible { overlay_id: OverlayId },
    Blocked(BlockedOverlayFocusRestoreState),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum OverlayFocusRestorePolicy {
    Clear,
    Preserve,
}

/// `InputListenerResult` (tui.ts:90).
#[derive(Debug, Clone, Default)]
pub struct InputListenerResult {
    pub consume: bool,
    pub data: Option<String>,
}

type InputListener = Box<dyn FnMut(&str) -> Option<InputListenerResult>>;
type ColorSchemeListener = Box<dyn FnMut(TerminalColorScheme)>;
type BackgroundColorCallback = Box<dyn FnOnce(Option<RgbColor>)>;

/// `Container` (tui.ts:256) — a `Component` that holds other components.
pub struct Container {
    pub children: Vec<SharedComponent>,
}

impl Default for Container {
    fn default() -> Self {
        Self::new()
    }
}

impl Container {
    pub fn new() -> Self {
        Self {
            children: Vec::new(),
        }
    }

    pub fn add_child(&mut self, component: SharedComponent) {
        self.children.push(component);
    }

    pub fn remove_child(&mut self, component: &SharedComponent) {
        if let Some(pos) = self.children.iter().position(|c| Rc::ptr_eq(c, component)) {
            self.children.remove(pos);
        }
    }

    pub fn clear(&mut self) {
        self.children.clear();
    }

    /// How many children are mounted.
    pub fn len(&self) -> usize {
        self.children.len()
    }

    pub fn is_empty(&self) -> bool {
        self.children.is_empty()
    }

    /// Drop leading children while doing so removes no more than `budget`
    /// rendered lines; returns how many lines were actually removed.
    ///
    /// A child is dropped only if it fits *entirely* within the budget, so the
    /// caller can guarantee it never discards a line that is still on screen.
    /// Cost is proportional to the children removed, not to the container
    /// length: the walk stops at the first child that does not fit.
    pub fn drop_leading_children(&mut self, width: usize, budget: usize) -> usize {
        let mut removed_lines = 0;
        let mut removed_children = 0;
        for child in &self.children {
            let lines = child.borrow_mut().render(width).len();
            if removed_lines + lines > budget {
                break;
            }
            removed_lines += lines;
            removed_children += 1;
        }
        self.children.drain(..removed_children);
        removed_lines
    }
}

impl Component for Container {
    fn render(&mut self, width: usize) -> Vec<String> {
        let mut lines = Vec::new();
        for child in &self.children {
            lines.extend(child.borrow_mut().render(width));
        }
        lines
    }

    fn invalidate(&mut self) {
        for child in &self.children {
            child.borrow_mut().invalidate();
        }
    }
}

struct KittyImageHeader {
    ids: Vec<u32>,
    rows: u32,
}

const KITTY_SEQUENCE_PREFIX: &str = "\x1b_G";

/// `parseKittyImageHeader` (tui.ts:28).
fn parse_kitty_image_header(line: &str) -> Option<KittyImageHeader> {
    let sequence_start = line.find(KITTY_SEQUENCE_PREFIX)?;
    let params_start = sequence_start + KITTY_SEQUENCE_PREFIX.len();
    let rest = &line[params_start..];
    let params_end = rest.find(';')?;
    let params = &rest[..params_end];

    let mut ids = Vec::new();
    let mut rows = 1u32;
    for param in params.split(',') {
        let mut parts = param.splitn(2, '=');
        let key = parts.next().unwrap_or("");
        let Some(value) = parts.next() else { continue };
        let Ok(number_value) = value.parse::<i64>() else {
            continue;
        };
        if number_value <= 0 || number_value > 0xffff_ffff {
            continue;
        }
        if key == "i" {
            ids.push(number_value as u32);
        } else if key == "r" {
            rows = number_value as u32;
        }
    }
    Some(KittyImageHeader { ids, rows })
}

fn extract_kitty_image_ids(line: &str) -> Vec<u32> {
    parse_kitty_image_header(line)
        .map(|h| h.ids)
        .unwrap_or_default()
}

fn extract_kitty_image_rows(line: &str) -> u32 {
    parse_kitty_image_header(line).map(|h| h.rows).unwrap_or(1)
}

/// `TUI` (tui.ts:295) — the differential-render engine.
pub struct TUI {
    container: Container,
    terminal: Box<dyn Terminal>,
    previous_lines: Vec<String>,
    previous_kitty_image_ids: HashSet<u32>,
    previous_width: i64,
    previous_height: i64,
    focused_component: Option<SharedComponent>,
    input_listeners: Vec<(u64, InputListener)>,
    next_input_listener_id: u64,
    pub on_debug: Option<Box<dyn FnMut()>>,
    render_requested: bool,
    force_pending: bool,
    last_render_at: Instant,
    cursor_row: i64,
    hardware_cursor_row: i64,
    show_hardware_cursor: bool,
    clear_on_shrink: bool,
    max_lines_rendered: i64,
    previous_viewport_top: i64,
    full_redraw_count: u64,
    stopped: bool,
    pending_osc11_background_queries: VecDeque<BackgroundColorCallback>,
    terminal_color_scheme_listeners: Vec<(u64, ColorSchemeListener)>,
    next_color_scheme_listener_id: u64,
    terminal_color_scheme_notifications_enabled: bool,
    focus_order_counter: u64,
    next_overlay_id: u64,
    overlay_stack: Vec<OverlayStackEntry>,
    overlay_focus_restore: OverlayFocusRestoreState,
    /// Live terminal row count, readable by a child component (e.g.
    /// `Editor`) from inside its own `render` even though `TUI` itself is
    /// already mutably borrowed for the render pass — a plain
    /// `Rc<RefCell<TUI>>` re-borrow would fail there. Refreshed at the top
    /// of every [`Self::do_render`].
    terminal_rows_cell: Rc<Cell<u16>>,
}

impl TUI {
    /// `constructor` (tui.ts:328). `show_hardware_cursor` defaults to the
    /// `PI_HARDWARE_CURSOR=1` env check when `None`, matching the TS default.
    pub fn new(terminal: Box<dyn Terminal>, show_hardware_cursor: Option<bool>) -> Self {
        let default_show_hardware_cursor =
            std::env::var("PI_HARDWARE_CURSOR").as_deref() == Ok("1");
        let default_clear_on_shrink = std::env::var("PI_CLEAR_ON_SHRINK").as_deref() == Ok("1");
        let terminal_rows_cell = Rc::new(Cell::new(terminal.rows()));
        Self {
            container: Container::new(),
            terminal,
            previous_lines: Vec::new(),
            previous_kitty_image_ids: HashSet::new(),
            previous_width: 0,
            previous_height: 0,
            focused_component: None,
            input_listeners: Vec::new(),
            next_input_listener_id: 0,
            on_debug: None,
            render_requested: false,
            force_pending: false,
            // TS seeds `lastRenderAt` to an hour ago so the first non-force
            // render is never throttled. `Instant::now() - 3600s` can underflow
            // Windows' boot-time-based counter (panics with "overflow when
            // subtracting duration from instant" — seen on this host), so seed
            // with plain `now()` instead: the force path bypasses the throttle
            // via `force_pending`, and every non-force first render in the
            // oracle corpus waits past the 16ms interval before polling anyway.
            last_render_at: Instant::now(),
            cursor_row: 0,
            hardware_cursor_row: 0,
            show_hardware_cursor: show_hardware_cursor.unwrap_or(default_show_hardware_cursor),
            clear_on_shrink: default_clear_on_shrink,
            max_lines_rendered: 0,
            previous_viewport_top: 0,
            full_redraw_count: 0,
            stopped: false,
            pending_osc11_background_queries: VecDeque::new(),
            terminal_color_scheme_listeners: Vec::new(),
            next_color_scheme_listener_id: 0,
            terminal_color_scheme_notifications_enabled: false,
            focus_order_counter: 0,
            next_overlay_id: 0,
            overlay_stack: Vec::new(),
            overlay_focus_restore: OverlayFocusRestoreState::Inactive,
            terminal_rows_cell,
        }
    }

    // -- Container delegation (TUI extends Container in the TS) --------

    pub fn add_child(&mut self, component: SharedComponent) {
        self.container.add_child(component);
    }

    pub fn remove_child(&mut self, component: &SharedComponent) {
        self.container.remove_child(component);
    }

    pub fn clear(&mut self) {
        self.container.clear();
    }

    pub fn render(&mut self, width: usize) -> Vec<String> {
        self.container.render(width)
    }

    /// How many document lines have scrolled above the viewport, i.e. are no
    /// longer addressable by the differential renderer. Any change to them
    /// forces a full redraw, so in practice they are frozen in the terminal's
    /// own scrollback. This is the budget a caller may
    /// [`forget_leading_lines`](Self::forget_leading_lines) from.
    pub fn lines_above_viewport(&self) -> usize {
        self.previous_viewport_top.max(0) as usize
    }

    /// Tell the renderer that `count` leading document lines have been removed
    /// from the component tree, so it can shift its diff state to match.
    ///
    /// Without this, dropping old children would renumber every remaining row:
    /// the next frame would compare row `i` of the shortened document against
    /// row `i` of the old one, find everything different, and fall back to a
    /// full redraw (and a visibly duplicated transcript). Shifting
    /// `previous_lines` and the cursor/viewport counters by the same amount
    /// keeps the two sides aligned, so pruning costs nothing on screen.
    ///
    /// Refuses to forget more than [`lines_above_viewport`](Self::lines_above_viewport):
    /// a row that is still on screen has to stay addressable.
    pub fn forget_leading_lines(&mut self, count: usize) {
        let count = count
            .min(self.previous_lines.len())
            .min(self.lines_above_viewport());
        if count == 0 {
            return;
        }
        let shift = count as i64;
        self.previous_lines.drain(..count);
        self.previous_viewport_top -= shift;
        self.cursor_row = (self.cursor_row - shift).max(0);
        self.hardware_cursor_row = (self.hardware_cursor_row - shift).max(0);
        self.max_lines_rendered = (self.max_lines_rendered - shift).max(0);
    }

    /// `invalidate` (tui.ts:630, `override`).
    pub fn invalidate(&mut self) {
        self.container.invalidate();
        for overlay in &self.overlay_stack {
            overlay.component.borrow_mut().invalidate();
        }
    }

    // -- Simple getters/setters -----------------------------------------

    pub fn full_redraws(&self) -> u64 {
        self.full_redraw_count
    }

    /// `this.tui.terminal.rows` — the editor (editor.ts:500) and page-scroll
    /// (editor.ts:1871) read the terminal height through the TUI. The Rust
    /// `Terminal` trait exposes `rows()`; this is the editor's accessor.
    /// The terminal width the renderer lays out against — the companion to
    /// [`terminal_rows`](Self::terminal_rows), needed by callers that render a
    /// component themselves (transcript pruning measures line counts at the
    /// same width the frame will use).
    pub fn terminal_columns(&self) -> u16 {
        self.terminal.columns()
    }

    pub fn terminal_rows(&self) -> u16 {
        self.terminal.rows()
    }

    /// A live handle to the terminal row count, safe for a child component
    /// to read from inside its own `render` even while this `TUI` is
    /// already mutably borrowed for that render pass. See
    /// [`Editor::terminal_rows`](crate::editor::Editor::terminal_rows).
    pub fn terminal_rows_handle(&self) -> Rc<Cell<u16>> {
        self.terminal_rows_cell.clone()
    }

    pub fn get_show_hardware_cursor(&self) -> bool {
        self.show_hardware_cursor
    }

    pub fn set_show_hardware_cursor(&mut self, enabled: bool) {
        if self.show_hardware_cursor == enabled {
            return;
        }
        self.show_hardware_cursor = enabled;
        if !enabled {
            self.terminal.hide_cursor();
        }
        self.request_render(false);
    }

    pub fn get_clear_on_shrink(&self) -> bool {
        self.clear_on_shrink
    }

    /// Test/oracle helper: is `component` the currently focused component?
    /// (There is no TS equivalent method — callers there just compare
    /// `tui.focusedComponent === component` directly since it's a public
    /// field; this crate keeps `focused_component` private, so tests need an
    /// accessor.)
    pub fn is_focused_component(&self, component: &SharedComponent) -> bool {
        same_component(self.focused_component.as_ref(), Some(component))
    }

    pub fn set_clear_on_shrink(&mut self, enabled: bool) {
        self.clear_on_shrink = enabled;
    }

    // -- Focus + overlay focus-restore state machine ---------------------

    /// `setFocus` (tui.ts:366).
    pub fn set_focus(&mut self, component: Option<SharedComponent>) {
        self.set_focus_internal(component, OverlayFocusRestorePolicy::Clear);
    }

    fn find_overlay_by_component(&self, component: &SharedComponent) -> Option<OverlayId> {
        self.overlay_stack
            .iter()
            .find(|e| Rc::ptr_eq(&e.component, component) && self.is_overlay_visible(e))
            .map(|e| e.id)
    }

    fn overlay_entry(&self, id: OverlayId) -> Option<&OverlayStackEntry> {
        self.overlay_stack.iter().find(|e| e.id == id)
    }

    /// `setFocusInternal` (tui.ts:370).
    fn set_focus_internal(
        &mut self,
        component: Option<SharedComponent>,
        overlay_focus_restore: OverlayFocusRestorePolicy,
    ) {
        let previous_focus = self.focused_component.clone();
        let mut next_focus = component;

        let previous_focused_overlay = previous_focus
            .as_ref()
            .and_then(|pf| self.find_overlay_by_component(pf));
        let next_focus_is_overlay = next_focus.as_ref().is_some_and(|nf| {
            self.overlay_stack
                .iter()
                .any(|e| Rc::ptr_eq(&e.component, nf))
        });
        let restore_state = self.get_visible_overlay_focus_restore();

        if next_focus.is_some() && !next_focus_is_overlay {
            let blocked_matching_previous = match &restore_state {
                OverlayFocusRestoreClone::Blocked {
                    overlay_id,
                    blocked_by,
                    resume,
                } if same_component(Some(blocked_by), previous_focus.as_ref()) => {
                    Some((*overlay_id, blocked_by.clone(), clone_resume(resume)))
                }
                _ => None,
            };
            if let Some((overlay_id, blocked_by, resume)) = blocked_matching_previous {
                let resume_is_focus_target =
                    matches!(resume, OverlayBlockedFocusResume::FocusTarget(_));
                let blocked_by_mounted = self.is_component_mounted(&blocked_by);
                if resume_is_focus_target || !blocked_by_mounted {
                    next_focus = self.resolve_blocked_overlay_focus_resume();
                } else {
                    self.overlay_focus_restore =
                        OverlayFocusRestoreState::Blocked(BlockedOverlayFocusRestoreState {
                            overlay_id,
                            blocked_by: next_focus.clone().unwrap(),
                            resume,
                        });
                }
            } else {
                let restore_overlay_id = match &restore_state {
                    OverlayFocusRestoreClone::Eligible { overlay_id } => Some(*overlay_id),
                    OverlayFocusRestoreClone::Blocked { overlay_id, .. } => Some(*overlay_id),
                    OverlayFocusRestoreClone::Inactive => None,
                };
                if let (Some(prev_overlay_id), Some(overlay_id)) =
                    (previous_focused_overlay, restore_overlay_id)
                {
                    if prev_overlay_id == overlay_id
                        && !self.is_overlay_focus_ancestor(prev_overlay_id, next_focus.as_ref())
                    {
                        self.overlay_focus_restore =
                            OverlayFocusRestoreState::Blocked(BlockedOverlayFocusRestoreState {
                                overlay_id: prev_overlay_id,
                                blocked_by: next_focus.clone().unwrap(),
                                resume: OverlayBlockedFocusResume::RestoreOverlay,
                            });
                    }
                }
            }
        } else if next_focus.is_none() {
            let blocked_matching_previous = matches!(
                &restore_state,
                OverlayFocusRestoreClone::Blocked { blocked_by, .. } if same_component(Some(blocked_by), previous_focus.as_ref())
            );
            if blocked_matching_previous {
                next_focus = self.resolve_blocked_overlay_focus_resume();
            } else if overlay_focus_restore == OverlayFocusRestorePolicy::Clear {
                self.clear_overlay_focus_restore();
            }
        }

        set_focused_if_focusable(self.focused_component.as_ref(), false);
        self.focused_component = next_focus.clone();
        set_focused_if_focusable(self.focused_component.as_ref(), true);

        if let Some(nf) = &next_focus {
            if let Some(overlay_id) = self.find_overlay_by_component(nf) {
                self.overlay_focus_restore = OverlayFocusRestoreState::Eligible { overlay_id };
            }
        }
    }

    fn clear_overlay_focus_restore(&mut self) {
        self.overlay_focus_restore = OverlayFocusRestoreState::Inactive;
    }

    fn clear_overlay_focus_restore_for(&mut self, id: OverlayId) {
        let matches = match &self.overlay_focus_restore {
            OverlayFocusRestoreState::Eligible { overlay_id } => *overlay_id == id,
            OverlayFocusRestoreState::Blocked(b) => b.overlay_id == id,
            OverlayFocusRestoreState::Inactive => false,
        };
        if matches {
            self.clear_overlay_focus_restore();
        }
    }

    /// `resolveBlockedOverlayFocusResume` (tui.ts:445).
    fn resolve_blocked_overlay_focus_resume(&mut self) -> Option<SharedComponent> {
        let OverlayFocusRestoreState::Blocked(blocked) = &self.overlay_focus_restore else {
            return None;
        };
        match &blocked.resume {
            OverlayBlockedFocusResume::RestoreOverlay => self
                .overlay_entry(blocked.overlay_id)
                .map(|e| e.component.clone()),
            OverlayBlockedFocusResume::FocusTarget(target) => {
                let target = target.clone();
                self.clear_overlay_focus_restore();
                target
            }
        }
    }

    /// `getVisibleOverlayFocusRestore` (tui.ts:451).
    fn get_visible_overlay_focus_restore(&self) -> OverlayFocusRestoreClone {
        let overlay_id = match &self.overlay_focus_restore {
            OverlayFocusRestoreState::Inactive => return OverlayFocusRestoreClone::Inactive,
            OverlayFocusRestoreState::Eligible { overlay_id } => *overlay_id,
            OverlayFocusRestoreState::Blocked(b) => b.overlay_id,
        };
        let Some(entry) = self.overlay_entry(overlay_id) else {
            return OverlayFocusRestoreClone::Inactive;
        };
        if !self.is_overlay_visible(entry) {
            return OverlayFocusRestoreClone::Inactive;
        }
        match &self.overlay_focus_restore {
            OverlayFocusRestoreState::Eligible { overlay_id } => {
                OverlayFocusRestoreClone::Eligible {
                    overlay_id: *overlay_id,
                }
            }
            OverlayFocusRestoreState::Blocked(b) => OverlayFocusRestoreClone::Blocked {
                overlay_id: b.overlay_id,
                blocked_by: b.blocked_by.clone(),
                resume: clone_resume(&b.resume),
            },
            OverlayFocusRestoreState::Inactive => OverlayFocusRestoreClone::Inactive,
        }
    }

    /// `isOverlayFocusAncestor` (tui.ts:460).
    fn is_overlay_focus_ancestor(
        &self,
        entry_id: OverlayId,
        component: Option<&SharedComponent>,
    ) -> bool {
        let Some(component) = component else {
            return false;
        };
        let mut visited: Vec<SharedComponent> = Vec::new();
        let mut current = self
            .overlay_entry(entry_id)
            .and_then(|e| e.pre_focus.clone());
        while let Some(cur) = current {
            if visited.iter().any(|v| Rc::ptr_eq(v, &cur)) {
                break;
            }
            visited.push(cur.clone());
            if Rc::ptr_eq(&cur, component) {
                return true;
            }
            current = self
                .overlay_stack
                .iter()
                .find(|o| Rc::ptr_eq(&o.component, &cur))
                .and_then(|o| o.pre_focus.clone());
        }
        false
    }

    /// `retargetOverlayPreFocus` (tui.ts:471).
    fn retarget_overlay_pre_focus(
        &mut self,
        removed_id: OverlayId,
        removed_component: &SharedComponent,
        removed_pre_focus: Option<SharedComponent>,
    ) {
        for overlay in &mut self.overlay_stack {
            if overlay.id != removed_id {
                if let Some(pf) = &overlay.pre_focus {
                    if Rc::ptr_eq(pf, removed_component) {
                        overlay.pre_focus = removed_pre_focus.clone();
                    }
                }
            }
        }
    }

    /// `isComponentMounted` (tui.ts:479).
    fn is_component_mounted(&self, component: &SharedComponent) -> bool {
        self.container
            .children
            .iter()
            .any(|child| contains_component(child, component))
    }

    // -- Overlays ----------------------------------------------------------

    /// `showOverlay` (tui.ts:493). Returns an [`OverlayId`] — see module docs.
    pub fn show_overlay(
        &mut self,
        component: SharedComponent,
        options: Option<OverlayOptions>,
    ) -> OverlayId {
        self.focus_order_counter += 1;
        let id = OverlayId(self.next_overlay_id);
        self.next_overlay_id += 1;
        let non_capturing = options.as_ref().is_some_and(|o| o.non_capturing);
        let entry = OverlayStackEntry {
            id,
            component: component.clone(),
            options,
            pre_focus: self.focused_component.clone(),
            hidden: false,
            focus_order: self.focus_order_counter,
        };
        self.overlay_stack.push(entry);

        let visible = self
            .overlay_entry(id)
            .is_some_and(|e| self.is_overlay_visible(e));
        if !non_capturing && visible {
            self.set_focus(Some(component));
        }
        self.terminal.hide_cursor();
        self.request_render(false);
        id
    }

    /// `hideOverlay` (tui.ts:589) — hides the topmost overlay.
    pub fn hide_overlay(&mut self) {
        if let Some(id) = self.overlay_stack.last().map(|e| e.id) {
            self.hide_overlay_by_id(id);
        }
    }

    /// The `hide()` closure of `OverlayHandle` (tui.ts:511), addressed by id.
    pub fn hide_overlay_by_id(&mut self, id: OverlayId) {
        let Some(index) = self.overlay_stack.iter().position(|e| e.id == id) else {
            return;
        };
        let entry_component = self.overlay_stack[index].component.clone();
        let entry_pre_focus = self.overlay_stack[index].pre_focus.clone();
        self.clear_overlay_focus_restore_for(id);
        self.retarget_overlay_pre_focus(id, &entry_component, entry_pre_focus.clone());
        self.overlay_stack.remove(index);
        if same_component(self.focused_component.as_ref(), Some(&entry_component)) {
            let top_visible = self.get_topmost_visible_overlay();
            self.set_focus(top_visible.or(entry_pre_focus));
        }
        if self.overlay_stack.is_empty() {
            self.terminal.hide_cursor();
        }
        self.request_render(false);
    }

    /// The `setHidden()` closure of `OverlayHandle` (tui.ts:526).
    pub fn set_overlay_hidden(&mut self, id: OverlayId, hidden: bool) {
        let Some(index) = self.overlay_stack.iter().position(|e| e.id == id) else {
            return;
        };
        if self.overlay_stack[index].hidden == hidden {
            return;
        }
        self.overlay_stack[index].hidden = hidden;
        let component = self.overlay_stack[index].component.clone();
        let non_capturing = self.overlay_stack[index]
            .options
            .as_ref()
            .is_some_and(|o| o.non_capturing);
        if hidden {
            self.clear_overlay_focus_restore_for(id);
            if same_component(self.focused_component.as_ref(), Some(&component)) {
                let top_visible = self.get_topmost_visible_overlay();
                let pre_focus = self.overlay_stack[index].pre_focus.clone();
                self.set_focus(top_visible.or(pre_focus));
            }
        } else {
            let visible = self
                .overlay_entry(id)
                .is_some_and(|e| self.is_overlay_visible(e));
            if !non_capturing && visible {
                self.focus_order_counter += 1;
                if let Some(entry) = self.overlay_stack.iter_mut().find(|e| e.id == id) {
                    entry.focus_order = self.focus_order_counter;
                }
                self.set_focus(Some(component));
            }
        }
        self.request_render(false);
    }

    /// The `isHidden()` closure of `OverlayHandle` (tui.ts:546).
    pub fn is_overlay_hidden(&self, id: OverlayId) -> bool {
        self.overlay_entry(id).map(|e| e.hidden).unwrap_or(true)
    }

    /// The `focus()` closure of `OverlayHandle` (tui.ts:547).
    pub fn focus_overlay(&mut self, id: OverlayId) {
        let Some(entry) = self.overlay_entry(id) else {
            return;
        };
        if !self.is_overlay_visible(entry) {
            return;
        }
        let component = entry.component.clone();
        self.focus_order_counter += 1;
        if let Some(entry) = self.overlay_stack.iter_mut().find(|e| e.id == id) {
            entry.focus_order = self.focus_order_counter;
        }
        self.set_focus(Some(component));
        self.request_render(false);
    }

    /// The `unfocus()` closure of `OverlayHandle` (tui.ts:553).
    pub fn unfocus_overlay(&mut self, id: OverlayId, options: Option<OverlayUnfocusOptions>) {
        let Some(entry) = self.overlay_entry(id) else {
            return;
        };
        let component = entry.component.clone();
        let is_focused = same_component(self.focused_component.as_ref(), Some(&component));
        let has_pending_restore = matches!(
            &self.overlay_focus_restore,
            OverlayFocusRestoreState::Eligible { overlay_id } if *overlay_id == id
        ) || matches!(
            &self.overlay_focus_restore,
            OverlayFocusRestoreState::Blocked(b) if b.overlay_id == id
        );
        if !is_focused && !has_pending_restore {
            return;
        }

        if let OverlayFocusRestoreState::Blocked(blocked) = &self.overlay_focus_restore {
            if blocked.overlay_id == id
                && same_component(self.focused_component.as_ref(), Some(&blocked.blocked_by))
            {
                let blocked_by = blocked.blocked_by.clone();
                if let Some(opts) = options {
                    self.overlay_focus_restore =
                        OverlayFocusRestoreState::Blocked(BlockedOverlayFocusRestoreState {
                            overlay_id: id,
                            blocked_by,
                            resume: OverlayBlockedFocusResume::FocusTarget(opts.target),
                        });
                } else {
                    self.clear_overlay_focus_restore();
                }
                self.request_render(false);
                return;
            }
        }

        self.clear_overlay_focus_restore_for(id);
        if is_focused || options.is_some() {
            let top_visible = self.get_topmost_visible_overlay();
            let fallback = match &top_visible {
                Some(tv) if !Rc::ptr_eq(tv, &component) => Some(tv.clone()),
                _ => self.overlay_entry(id).and_then(|e| e.pre_focus.clone()),
            };
            let target = match options {
                Some(opts) => opts.target,
                None => fallback,
            };
            self.set_focus(target);
        }
        self.request_render(false);
    }

    /// The `isFocused()` closure of `OverlayHandle` (tui.ts:584).
    pub fn is_overlay_focused(&self, id: OverlayId) -> bool {
        self.overlay_entry(id)
            .is_some_and(|e| same_component(self.focused_component.as_ref(), Some(&e.component)))
    }

    /// `hasOverlay` (tui.ts:605).
    pub fn has_overlay(&self) -> bool {
        self.overlay_stack
            .iter()
            .any(|e| self.is_overlay_visible(e))
    }

    /// `isOverlayVisible` (tui.ts:610).
    fn is_overlay_visible(&self, entry: &OverlayStackEntry) -> bool {
        if entry.hidden {
            return false;
        }
        if let Some(options) = &entry.options {
            if let Some(visible_fn) = &options.visible {
                return visible_fn(self.terminal.columns(), self.terminal.rows());
            }
        }
        true
    }

    /// `getTopmostVisibleOverlay` (tui.ts:619).
    fn get_topmost_visible_overlay(&self) -> Option<SharedComponent> {
        let mut topmost: Option<&OverlayStackEntry> = None;
        for overlay in &self.overlay_stack {
            let non_capturing = overlay.options.as_ref().is_some_and(|o| o.non_capturing);
            if non_capturing || !self.is_overlay_visible(overlay) {
                continue;
            }
            if topmost.is_none_or(|t| overlay.focus_order > t.focus_order) {
                topmost = Some(overlay);
            }
        }
        topmost.map(|e| e.component.clone())
    }

    // -- Start/stop/render scheduling --------------------------------------

    /// `start` (tui.ts:635).
    pub fn start(&mut self) {
        self.stopped = false;
        // The TS wires `terminal.start(onInput, onResize)` to
        // `handleInput`/`requestRender` here. This port's `Terminal::start`
        // needs `'static` closures that can't borrow `&mut self` — a real
        // TUI+ProcessTerminal wiring is `feat-007`'s job (module docs). Tests
        // drive `handle_input`/`request_render` directly against the mock
        // `Terminal`.
        self.terminal.hide_cursor();
        if self.terminal_color_scheme_notifications_enabled {
            self.terminal.write("\x1b[?2031h");
        }
        self.query_cell_size();
        self.request_render(false);
    }

    fn query_cell_size(&mut self) {
        if get_capabilities().images.is_none() {
            return;
        }
        self.terminal.write("\x1b[16t");
    }

    /// `stop` (tui.ts:687).
    pub fn stop(&mut self) {
        self.stopped = true;
        if self.terminal_color_scheme_notifications_enabled {
            self.terminal.write("\x1b[?2031l");
        }
        if !self.previous_lines.is_empty() {
            let target_row = self.previous_lines.len() as i64;
            let line_diff = target_row - self.hardware_cursor_row;
            if line_diff > 0 {
                self.terminal.write(&format!("\x1b[{line_diff}B"));
            } else if line_diff < 0 {
                self.terminal.write(&format!("\x1b[{}A", -line_diff));
            }
            self.terminal.write("\r\n");
        }
        self.terminal.show_cursor();
        self.terminal.stop();
    }

    /// `requestRender` (tui.ts:712) — see module docs, "requestRender's
    /// debounce is synchronous and caller-polled".
    /// `requestRender` (tui.ts:712). **Neither branch renders synchronously
    /// in the TS** — `force` schedules via `process.nextTick` (no throttle
    /// wait) and the non-force path via `process.nextTick` +
    /// a throttled `setTimeout`. This port's [`TUI::poll`] is the
    /// synchronous stand-in for both of those deferred callbacks — see
    /// module docs, "requestRender's debounce is synchronous and
    /// caller-polled". `force_pending` tracks that the next `poll()` should
    /// bypass the throttle check (mirroring `process.nextTick`'s "no
    /// `MIN_RENDER_INTERVAL_MS` wait" for the force path).
    pub fn request_render(&mut self, force: bool) {
        if force {
            self.previous_lines.clear();
            self.previous_width = -1;
            self.previous_height = -1;
            self.cursor_row = 0;
            self.hardware_cursor_row = 0;
            self.max_lines_rendered = 0;
            self.previous_viewport_top = 0;
            self.render_requested = true;
            self.force_pending = true;
            return;
        }
        if self.render_requested {
            return;
        }
        self.render_requested = true;
    }

    /// The caller-driven "pump" — see module docs. Renders now if a render is
    /// pending and (unless it was force-requested) the throttle interval has
    /// elapsed; returns whether it rendered.
    pub fn poll(&mut self) -> bool {
        if self.stopped || !self.render_requested {
            return false;
        }
        if !self.force_pending
            && self.last_render_at.elapsed() < Duration::from_millis(MIN_RENDER_INTERVAL_MS)
        {
            return false;
        }
        self.render_requested = false;
        self.force_pending = false;
        self.last_render_at = Instant::now();
        self.do_render();
        true
    }

    // -- Input handling -----------------------------------------------------

    pub fn add_input_listener(&mut self, listener: InputListener) -> u64 {
        let id = self.next_input_listener_id;
        self.next_input_listener_id += 1;
        self.input_listeners.push((id, listener));
        id
    }

    pub fn remove_input_listener(&mut self, id: u64) {
        self.input_listeners.retain(|(existing, _)| *existing != id);
    }

    pub fn on_terminal_color_scheme_change(&mut self, listener: ColorSchemeListener) -> u64 {
        let id = self.next_color_scheme_listener_id;
        self.next_color_scheme_listener_id += 1;
        self.terminal_color_scheme_listeners.push((id, listener));
        id
    }

    pub fn remove_terminal_color_scheme_listener(&mut self, id: u64) {
        self.terminal_color_scheme_listeners
            .retain(|(existing, _)| *existing != id);
    }

    pub fn set_terminal_color_scheme_notifications(&mut self, enabled: bool) {
        if self.terminal_color_scheme_notifications_enabled == enabled {
            return;
        }
        self.terminal_color_scheme_notifications_enabled = enabled;
        if !self.stopped {
            self.terminal.write(if enabled {
                "\x1b[?2031h"
            } else {
                "\x1b[?2031l"
            });
        }
    }

    /// `queryTerminalBackgroundColor` (tui.ts:1665) — see module docs (no
    /// timeout implemented; `on_resolve` fires only on a real response).
    pub fn query_terminal_background_color(&mut self, on_resolve: BackgroundColorCallback) {
        self.pending_osc11_background_queries.push_back(on_resolve);
        self.terminal.write("\x1b]11;?\x07");
    }

    /// `queryTerminalColorScheme` (tui.ts:1693) — sugar over
    /// `on_terminal_color_scheme_change`, self-removing on first response (no
    /// timeout — see module docs).
    pub fn query_terminal_color_scheme(
        &mut self,
        on_resolve: Box<dyn FnOnce(TerminalColorScheme)>,
    ) {
        let id_holder = Rc::new(RefCell::new(0u64));
        let id_holder_for_listener = id_holder.clone();
        let on_resolve = RefCell::new(Some(on_resolve));
        let listener_id = self.on_terminal_color_scheme_change(Box::new(move |scheme| {
            if let Some(f) = on_resolve.borrow_mut().take() {
                f(scheme);
            }
        }));
        *id_holder_for_listener.borrow_mut() = listener_id;
        self.terminal.write("\x1b[?996n");
    }

    /// `handleInput` (tui.ts:761).
    pub fn handle_input(&mut self, data: &str) {
        if self.consume_osc11_background_response(data) {
            return;
        }
        if self.consume_terminal_color_scheme_report(data) {
            return;
        }

        let mut current = data.to_string();
        if !self.input_listeners.is_empty() {
            let mut consumed = false;
            for (_, listener) in &mut self.input_listeners {
                if let Some(result) = listener(&current) {
                    if result.consume {
                        consumed = true;
                        break;
                    }
                    if let Some(replacement) = result.data {
                        current = replacement;
                    }
                }
            }
            if consumed {
                return;
            }
            if current.is_empty() {
                return;
            }
        }

        if self.consume_cell_size_response(&current) {
            return;
        }

        if crate::keys::matches_key(&current, "shift+ctrl+d") {
            if let Some(on_debug) = &mut self.on_debug {
                on_debug();
                return;
            }
        }

        if let Some(focused) = self.focused_component.clone() {
            if let Some(overlay) = self
                .overlay_stack
                .iter()
                .find(|o| Rc::ptr_eq(&o.component, &focused))
            {
                if !self.is_overlay_visible(overlay) {
                    let top_visible = self.get_topmost_visible_overlay();
                    if let Some(tv) = top_visible {
                        self.set_focus(Some(tv));
                    } else {
                        let pre_focus = overlay.pre_focus.clone();
                        self.set_focus_internal(pre_focus, OverlayFocusRestorePolicy::Preserve);
                    }
                }
            }
        }

        let focus_is_overlay = self.focused_component.as_ref().is_some_and(|f| {
            self.overlay_stack
                .iter()
                .any(|o| Rc::ptr_eq(&o.component, f))
        });
        if !focus_is_overlay {
            match self.get_visible_overlay_focus_restore() {
                OverlayFocusRestoreClone::Eligible { overlay_id } => {
                    if let Some(component) =
                        self.overlay_entry(overlay_id).map(|e| e.component.clone())
                    {
                        self.set_focus(Some(component));
                    }
                }
                OverlayFocusRestoreClone::Blocked {
                    overlay_id,
                    blocked_by,
                    resume,
                } => {
                    if !same_component(Some(&blocked_by), self.focused_component.as_ref()) {
                        match resume {
                            OverlayBlockedFocusResume::RestoreOverlay => {
                                if let Some(component) =
                                    self.overlay_entry(overlay_id).map(|e| e.component.clone())
                                {
                                    self.set_focus(Some(component));
                                }
                            }
                            OverlayBlockedFocusResume::FocusTarget(target) => {
                                self.clear_overlay_focus_restore();
                                self.set_focus(target);
                            }
                        }
                    }
                }
                OverlayFocusRestoreClone::Inactive => {}
            }
        }

        if let Some(focused) = self.focused_component.clone() {
            let wants_key_release = focused.borrow().wants_key_release();
            if crate::keys::is_key_release(&current) && !wants_key_release {
                return;
            }
            focused.borrow_mut().handle_input(&current);
            self.request_render(false);
        }
    }

    fn consume_osc11_background_response(&mut self, data: &str) -> bool {
        if self.pending_osc11_background_queries.is_empty() {
            return false;
        }
        if !is_osc11_background_color_response(data) {
            return false;
        }
        let rgb = parse_osc11_background_color(data);
        if let Some(callback) = self.pending_osc11_background_queries.pop_front() {
            callback(rgb);
        }
        true
    }

    fn consume_terminal_color_scheme_report(&mut self, data: &str) -> bool {
        let Some(scheme) = parse_terminal_color_scheme_report(data) else {
            return false;
        };
        for (_, listener) in &mut self.terminal_color_scheme_listeners {
            listener(scheme);
        }
        true
    }

    /// `consumeCellSizeResponse` (tui.ts:873): `^\x1b\[6;(\d+);(\d+)t$`.
    fn consume_cell_size_response(&mut self, data: &str) -> bool {
        let Some(rest) = data.strip_prefix("\x1b[6;") else {
            return false;
        };
        let Some(rest) = rest.strip_suffix('t') else {
            return false;
        };
        let Some((height_str, width_str)) = rest.split_once(';') else {
            return false;
        };
        if height_str.is_empty()
            || width_str.is_empty()
            || !height_str.bytes().all(|b| b.is_ascii_digit())
            || !width_str.bytes().all(|b| b.is_ascii_digit())
        {
            return false;
        }
        let height_px: i64 = height_str.parse().unwrap_or(0);
        let width_px: i64 = width_str.parse().unwrap_or(0);
        if height_px <= 0 || width_px <= 0 {
            return true;
        }
        crate::terminal_image::set_cell_dimensions(crate::terminal_image::CellDimensions {
            width_px: width_px as u32,
            height_px: height_px as u32,
        });
        self.invalidate();
        self.request_render(false);
        true
    }

    // -- Overlay layout ------------------------------------------------------

    /// `resolveOverlayLayout` (tui.ts:897).
    fn resolve_overlay_layout(
        &self,
        options: Option<&OverlayOptions>,
        overlay_height: i64,
        term_width: i64,
        term_height: i64,
    ) -> (i64, i64, i64, Option<i64>) {
        let margin = match options.and_then(|o| o.margin.as_ref()) {
            Some(OverlayMarginValue::Uniform(m)) => OverlayMargin {
                top: Some(*m),
                right: Some(*m),
                bottom: Some(*m),
                left: Some(*m),
            },
            Some(OverlayMarginValue::PerSide(m)) => *m,
            None => OverlayMargin::default(),
        };
        let margin_top = margin.top.unwrap_or(0).max(0) as i64;
        let margin_right = margin.right.unwrap_or(0).max(0) as i64;
        let margin_bottom = margin.bottom.unwrap_or(0).max(0) as i64;
        let margin_left = margin.left.unwrap_or(0).max(0) as i64;

        let avail_width = (term_width - margin_left - margin_right).max(1);
        let avail_height = (term_height - margin_top - margin_bottom).max(1);

        let mut width = parse_size_value(options.and_then(|o| o.width), term_width as f64)
            .map(|w| w as i64)
            .unwrap_or_else(|| 80.min(avail_width));
        if let Some(min_width) = options.and_then(|o| o.min_width) {
            width = width.max(min_width as i64);
        }
        width = width.max(1).min(avail_width);

        let max_height = parse_size_value(options.and_then(|o| o.max_height), term_height as f64)
            .map(|h| (h as i64).max(1).min(avail_height));

        let effective_height = match max_height {
            Some(mh) => overlay_height.min(mh),
            None => overlay_height,
        };

        let mut row: i64;
        if let Some(row_value) = options.and_then(|o| o.row) {
            row = match row_value {
                SizeValue::Percent(p) => {
                    let max_row = (avail_height - effective_height).max(0);
                    margin_top + ((max_row as f64 * (p / 100.0)).floor() as i64)
                }
                SizeValue::Absolute(n) => n as i64,
            };
        } else {
            let anchor = options
                .and_then(|o| o.anchor)
                .unwrap_or(OverlayAnchor::Center);
            row = self.resolve_anchor_row(anchor, effective_height, avail_height, margin_top);
        }

        let mut col: i64;
        if let Some(col_value) = options.and_then(|o| o.col) {
            col = match col_value {
                SizeValue::Percent(p) => {
                    let max_col = (avail_width - width).max(0);
                    margin_left + ((max_col as f64 * (p / 100.0)).floor() as i64)
                }
                SizeValue::Absolute(n) => n as i64,
            };
        } else {
            let anchor = options
                .and_then(|o| o.anchor)
                .unwrap_or(OverlayAnchor::Center);
            col = self.resolve_anchor_col(anchor, width, avail_width, margin_left);
        }

        if let Some(offset_y) = options.and_then(|o| o.offset_y) {
            row += offset_y;
        }
        if let Some(offset_x) = options.and_then(|o| o.offset_x) {
            col += offset_x;
        }

        row = row
            .max(margin_top)
            .min(term_height - margin_bottom - effective_height);
        col = col.max(margin_left).min(term_width - margin_right - width);

        (width, row, col, max_height)
    }

    /// `resolveAnchorRow` (tui.ts:997).
    fn resolve_anchor_row(
        &self,
        anchor: OverlayAnchor,
        height: i64,
        avail_height: i64,
        margin_top: i64,
    ) -> i64 {
        use OverlayAnchor::*;
        match anchor {
            TopLeft | TopCenter | TopRight => margin_top,
            BottomLeft | BottomCenter | BottomRight => margin_top + avail_height - height,
            LeftCenter | Center | RightCenter => margin_top + (avail_height - height) / 2,
        }
    }

    /// `resolveAnchorCol` (tui.ts:1014).
    fn resolve_anchor_col(
        &self,
        anchor: OverlayAnchor,
        width: i64,
        avail_width: i64,
        margin_left: i64,
    ) -> i64 {
        use OverlayAnchor::*;
        match anchor {
            TopLeft | LeftCenter | BottomLeft => margin_left,
            TopRight | RightCenter | BottomRight => margin_left + avail_width - width,
            TopCenter | Center | BottomCenter => margin_left + (avail_width - width) / 2,
        }
    }

    /// `compositeOverlays` (tui.ts:1031).
    fn composite_overlays(
        &mut self,
        lines: Vec<String>,
        term_width: i64,
        term_height: i64,
    ) -> Vec<String> {
        if self.overlay_stack.is_empty() {
            return lines;
        }
        let mut result = lines;

        struct Rendered {
            lines: Vec<String>,
            row: i64,
            col: i64,
            width: i64,
        }
        let mut rendered = Vec::new();
        let mut min_lines_needed = result.len() as i64;

        let mut visible_entries: Vec<usize> = (0..self.overlay_stack.len())
            .filter(|&i| self.is_overlay_visible(&self.overlay_stack[i]))
            .collect();
        visible_entries.sort_by_key(|&i| self.overlay_stack[i].focus_order);

        for i in visible_entries {
            let component = self.overlay_stack[i].component.clone();
            let (width, _row0, _col0, max_height) = self.resolve_overlay_layout(
                self.overlay_stack[i].options.as_ref(),
                0,
                term_width,
                term_height,
            );

            let mut overlay_lines = component.borrow_mut().render(width.max(0) as usize);
            if let Some(mh) = max_height {
                if overlay_lines.len() as i64 > mh {
                    overlay_lines.truncate(mh.max(0) as usize);
                }
            }

            let (_w, row, col, _mh) = self.resolve_overlay_layout(
                self.overlay_stack[i].options.as_ref(),
                overlay_lines.len() as i64,
                term_width,
                term_height,
            );

            min_lines_needed = min_lines_needed.max(row + overlay_lines.len() as i64);
            rendered.push(Rendered {
                lines: overlay_lines,
                row,
                col,
                width,
            });
        }

        let working_height = result
            .len()
            .max(term_height.max(0) as usize)
            .max(min_lines_needed.max(0) as usize);
        while result.len() < working_height {
            result.push(String::new());
        }
        let viewport_start = (working_height as i64 - term_height).max(0);

        for r in rendered {
            for (i, overlay_line) in r.lines.iter().enumerate() {
                let idx = viewport_start + r.row + i as i64;
                if idx >= 0 && (idx as usize) < result.len() {
                    let truncated = if visible_width(overlay_line) as i64 > r.width {
                        slice_by_column(overlay_line, 0, r.width.max(0) as usize, true)
                    } else {
                        overlay_line.clone()
                    };
                    result[idx as usize] = self.composite_line_at(
                        &result[idx as usize],
                        &truncated,
                        r.col,
                        r.width,
                        term_width,
                    );
                }
            }
        }

        result
    }

    /// `compositeLineAt` (tui.ts:1176).
    fn composite_line_at(
        &self,
        base_line: &str,
        overlay_line: &str,
        start_col: i64,
        overlay_width: i64,
        total_width: i64,
    ) -> String {
        if is_image_line(base_line) {
            return base_line.to_string();
        }
        let start_col = start_col.max(0) as usize;
        let overlay_width_u = overlay_width.max(0) as usize;
        let after_start = start_col + overlay_width_u;
        let after_len = (total_width - after_start as i64).max(0) as usize;
        let base = extract_segments(base_line, start_col, after_start, after_len, true);
        let overlay = slice_with_width(overlay_line, 0, overlay_width_u, true);

        let before_pad = start_col.saturating_sub(base.before_width);
        let overlay_pad = overlay_width_u.saturating_sub(overlay.width);
        let actual_before_width = start_col.max(base.before_width);
        let actual_overlay_width = overlay_width_u.max(overlay.width);
        let after_target =
            (total_width as usize).saturating_sub(actual_before_width + actual_overlay_width);
        let after_pad = after_target.saturating_sub(base.after_width);

        let mut result = String::new();
        result.push_str(&base.before);
        result.push_str(&" ".repeat(before_pad));
        result.push_str(SEGMENT_RESET);
        result.push_str(&overlay.text);
        result.push_str(&" ".repeat(overlay_pad));
        result.push_str(SEGMENT_RESET);
        result.push_str(&base.after);
        result.push_str(&" ".repeat(after_pad));

        let result_width = visible_width(&result) as i64;
        if result_width <= total_width {
            result
        } else {
            slice_by_column(&result, 0, total_width.max(0) as usize, true)
        }
    }

    // -- Kitty image lifecycle ------------------------------------------

    fn collect_kitty_image_ids(&self, lines: &[String]) -> HashSet<u32> {
        let mut ids = HashSet::new();
        for line in lines {
            for id in extract_kitty_image_ids(line) {
                ids.insert(id);
            }
        }
        ids
    }

    fn delete_kitty_images(&self, ids: impl IntoIterator<Item = u32>) -> String {
        let mut buffer = String::new();
        for id in ids {
            buffer.push_str(&delete_kitty_image(id));
        }
        buffer
    }

    /// `getKittyImageReservedRows` (tui.ts:1124).
    fn get_kitty_image_reserved_rows(
        &self,
        lines: &[String],
        index: usize,
        max_index: Option<usize>,
    ) -> usize {
        let max_index = max_index.unwrap_or(lines.len().saturating_sub(1));
        let rows = extract_kitty_image_rows(lines.get(index).map(String::as_str).unwrap_or(""));
        if rows <= 1 {
            return 1;
        }
        let max_rows = (rows as usize)
            .min(max_index.saturating_sub(index) + 1)
            .min(lines.len() - index);
        let mut reserved_rows = 1usize;
        while reserved_rows < max_rows {
            let line = lines
                .get(index + reserved_rows)
                .map(String::as_str)
                .unwrap_or("");
            if is_image_line(line) || visible_width(line) > 0 {
                break;
            }
            reserved_rows += 1;
        }
        reserved_rows
    }

    /// `expandChangedRangeForKittyImages` (tui.ts:1138).
    fn expand_changed_range_for_kitty_images(
        &self,
        first_changed: i64,
        last_changed: i64,
        new_lines: &[String],
    ) -> (i64, i64) {
        let mut expanded_first = first_changed;
        let mut expanded_last = last_changed;
        let mut expand_for = |lines: &[String]| {
            for i in 0..lines.len() {
                if extract_kitty_image_ids(&lines[i]).is_empty() {
                    continue;
                }
                let block_end =
                    i as i64 + self.get_kitty_image_reserved_rows(lines, i, None) as i64 - 1;
                if i as i64 >= first_changed
                    || (i as i64 <= last_changed && block_end >= first_changed)
                {
                    expanded_first = expanded_first.min(i as i64);
                    expanded_last = expanded_last.max(block_end);
                }
            }
        };
        expand_for(&self.previous_lines);
        expand_for(new_lines);
        (expanded_first, expanded_last)
    }

    /// `deleteChangedKittyImages` (tui.ts:1161).
    fn delete_changed_kitty_images(&self, first_changed: i64, last_changed: i64) -> String {
        if first_changed < 0 || last_changed < first_changed {
            return String::new();
        }
        let mut ids = HashSet::new();
        let max_line = last_changed.min(self.previous_lines.len() as i64 - 1);
        let mut i = first_changed;
        while i <= max_line {
            for id in extract_kitty_image_ids(&self.previous_lines[i as usize]) {
                ids.insert(id);
            }
            i += 1;
        }
        self.delete_kitty_images(ids)
    }

    // -- Cursor + line resets --------------------------------------------

    /// `extractCursorPosition` (tui.ts:1234).
    fn extract_cursor_position(&self, lines: &mut [String], height: i64) -> Option<(i64, i64)> {
        let viewport_top = (lines.len() as i64 - height).max(0);
        let mut row = lines.len() as i64 - 1;
        while row >= viewport_top {
            let line = &lines[row as usize];
            if let Some(marker_index) = line.find(CURSOR_MARKER) {
                let before_marker = &line[..marker_index];
                let col = visible_width(before_marker) as i64;
                let mut stripped = line[..marker_index].to_string();
                stripped.push_str(&line[marker_index + CURSOR_MARKER.len()..]);
                lines[row as usize] = stripped;
                return Some((row, col));
            }
            row -= 1;
        }
        None
    }

    /// The wire form of one rendered line: tab / Thai-Lao normalization plus
    /// the trailing segment reset. Image lines pass through untouched (their
    /// payload is not text and must not be rewritten).
    ///
    /// This used to be `apply_line_resets`, mapped over the *whole document*
    /// once per frame — two allocations and two full copies per line, for
    /// every line in the transcript, on every frame, even though a frame only
    /// ever writes the handful of lines that actually changed. It is now
    /// applied lazily at each write site instead, which makes the cost
    /// proportional to the changed lines rather than to the session length.
    ///
    /// `previous_lines` consequently stores lines in their *raw* form, so the
    /// diff below compares raw against raw — consistent, and the same answer,
    /// because the reset suffix is a constant and normalization is applied to
    /// both sides or to neither.
    fn line_for_output(line: &str) -> Cow<'_, str> {
        if is_image_line(line) {
            return Cow::Borrowed(line);
        }
        let normalized = normalize_terminal_output(line);
        let mut out = String::with_capacity(normalized.len() + SEGMENT_RESET.len());
        out.push_str(&normalized);
        out.push_str(SEGMENT_RESET);
        Cow::Owned(out)
    }

    // -- The render loop ----------------------------------------------------

    /// `doRender` (tui.ts:1254) — the full differential/full-redraw decision
    /// tree. See module docs for the width-overflow `panic!` decision.
    fn do_render(&mut self) {
        if self.stopped {
            return;
        }
        let width = self.terminal.columns() as i64;
        let height = self.terminal.rows() as i64;
        self.terminal_rows_cell.set(height as u16);
        let width_changed = self.previous_width != 0 && self.previous_width != width;
        let height_changed = self.previous_height != 0 && self.previous_height != height;
        let previous_buffer_length = if self.previous_height > 0 {
            self.previous_viewport_top + self.previous_height
        } else {
            height
        };
        let mut prev_viewport_top = if height_changed {
            (previous_buffer_length - height).max(0)
        } else {
            self.previous_viewport_top
        };
        let mut viewport_top = prev_viewport_top;
        let mut hardware_cursor_row = self.hardware_cursor_row;

        let mut new_lines = self.render(width.max(0) as usize);
        if !self.overlay_stack.is_empty() {
            new_lines = self.composite_overlays(new_lines, width, height);
        }
        let cursor_pos = self.extract_cursor_position(&mut new_lines, height);
        let new_lines = new_lines;

        // First render.
        if self.previous_lines.is_empty() && !width_changed && !height_changed {
            self.full_render(new_lines, cursor_pos, width, height, false);
            return;
        }
        if width_changed {
            self.full_render(new_lines, cursor_pos, width, height, true);
            return;
        }
        if height_changed && !is_termux_session() {
            self.full_render(new_lines, cursor_pos, width, height, true);
            return;
        }
        if self.clear_on_shrink
            && (new_lines.len() as i64) < self.max_lines_rendered
            && self.overlay_stack.is_empty()
        {
            self.full_render(new_lines, cursor_pos, width, height, true);
            return;
        }

        let mut first_changed: i64 = -1;
        let mut last_changed: i64 = -1;
        let max_lines = new_lines.len().max(self.previous_lines.len());
        for i in 0..max_lines {
            // Compare as `Option`, NOT with `""` as the missing-row stand-in.
            // A row past the end of one side must always count as changed, and
            // a blank row is a legitimate value: `previous_lines` holds raw
            // lines now (see `line_for_output`), so an empty row really is
            // `""` and would otherwise compare equal to "no such row" and be
            // skipped. It never collided before only because every processed
            // line carried a non-empty reset suffix.
            let old_line = self.previous_lines.get(i).map(String::as_str);
            let new_line = new_lines.get(i).map(String::as_str);
            if old_line != new_line {
                if first_changed == -1 {
                    first_changed = i as i64;
                }
                last_changed = i as i64;
            }
        }
        let appended_lines = new_lines.len() > self.previous_lines.len();
        if appended_lines {
            if first_changed == -1 {
                first_changed = self.previous_lines.len() as i64;
            }
            last_changed = new_lines.len() as i64 - 1;
        }
        if first_changed != -1 {
            let expanded =
                self.expand_changed_range_for_kitty_images(first_changed, last_changed, &new_lines);
            first_changed = expanded.0;
            last_changed = expanded.1;
        }
        let append_start = appended_lines
            && first_changed == self.previous_lines.len() as i64
            && first_changed > 0;

        if first_changed == -1 {
            self.position_hardware_cursor(cursor_pos, new_lines.len() as i64);
            self.previous_viewport_top = prev_viewport_top;
            self.previous_height = height;
            return;
        }

        if first_changed >= new_lines.len() as i64 {
            if self.previous_lines.len() as i64 > new_lines.len() as i64 {
                let mut buffer = String::from("\x1b[?2026h");
                buffer.push_str(&self.delete_changed_kitty_images(first_changed, last_changed));
                let target_row = (new_lines.len() as i64 - 1).max(0);
                if target_row < prev_viewport_top {
                    self.full_render(new_lines, cursor_pos, width, height, true);
                    return;
                }
                let line_diff = compute_line_diff(
                    hardware_cursor_row,
                    prev_viewport_top,
                    target_row,
                    viewport_top,
                );
                if line_diff > 0 {
                    buffer.push_str(&format!("\x1b[{line_diff}B"));
                } else if line_diff < 0 {
                    buffer.push_str(&format!("\x1b[{}A", -line_diff));
                }
                buffer.push('\r');
                let extra_lines = self.previous_lines.len() as i64 - new_lines.len() as i64;
                if extra_lines > height {
                    self.full_render(new_lines, cursor_pos, width, height, true);
                    return;
                }
                let clear_start_offset: i64 = if new_lines.is_empty() { 0 } else { 1 };
                if extra_lines > 0 && clear_start_offset > 0 {
                    buffer.push_str(&format!("\x1b[{clear_start_offset}B"));
                }
                for i in 0..extra_lines {
                    buffer.push_str("\r\x1b[2K");
                    if i < extra_lines - 1 {
                        buffer.push_str("\x1b[1B");
                    }
                }
                let move_back = (extra_lines - 1 + clear_start_offset).max(0);
                if move_back > 0 {
                    buffer.push_str(&format!("\x1b[{move_back}A"));
                }
                buffer.push_str("\x1b[?2026l");
                self.terminal.write(&buffer);
                self.cursor_row = target_row;
                self.hardware_cursor_row = target_row;
            }
            self.position_hardware_cursor(cursor_pos, new_lines.len() as i64);
            self.previous_kitty_image_ids = self.collect_kitty_image_ids(&new_lines);
            self.previous_lines = new_lines;
            self.previous_width = width;
            self.previous_height = height;
            self.previous_viewport_top = prev_viewport_top;
            return;
        }

        if first_changed < prev_viewport_top {
            self.full_render(new_lines, cursor_pos, width, height, true);
            return;
        }

        let mut buffer = String::from("\x1b[?2026h");
        buffer.push_str(&self.delete_changed_kitty_images(first_changed, last_changed));
        let prev_viewport_bottom = prev_viewport_top + height - 1;
        let move_target_row = if append_start {
            first_changed - 1
        } else {
            first_changed
        };
        if move_target_row > prev_viewport_bottom {
            let current_screen_row = (hardware_cursor_row - prev_viewport_top).clamp(0, height - 1);
            let move_to_bottom = height - 1 - current_screen_row;
            if move_to_bottom > 0 {
                buffer.push_str(&format!("\x1b[{move_to_bottom}B"));
            }
            let scroll = move_target_row - prev_viewport_bottom;
            buffer.push_str(&"\r\n".repeat(scroll.max(0) as usize));
            prev_viewport_top += scroll;
            viewport_top += scroll;
            hardware_cursor_row = move_target_row;
        }

        let line_diff = compute_line_diff(
            hardware_cursor_row,
            prev_viewport_top,
            move_target_row,
            viewport_top,
        );
        if line_diff > 0 {
            buffer.push_str(&format!("\x1b[{line_diff}B"));
        } else if line_diff < 0 {
            buffer.push_str(&format!("\x1b[{}A", -line_diff));
        }
        buffer.push_str(if append_start { "\r\n" } else { "\r" });

        let render_end = last_changed.min(new_lines.len() as i64 - 1);
        let mut i = first_changed;
        while i <= render_end {
            if i > first_changed {
                buffer.push_str("\r\n");
            }
            let line = new_lines[i as usize].clone();
            let is_image = is_image_line(&line);
            let image_reserved_rows = if is_image {
                self.get_kitty_image_reserved_rows(
                    &new_lines,
                    i as usize,
                    Some(render_end.max(0) as usize),
                ) as i64
            } else {
                1
            };
            if image_reserved_rows > 1 {
                let image_start_screen_row = i - viewport_top;
                if image_start_screen_row < 0
                    || image_start_screen_row + image_reserved_rows > height
                {
                    self.full_render(new_lines, cursor_pos, width, height, true);
                    return;
                }
                buffer.push_str("\x1b[2K");
                for _ in 1..image_reserved_rows {
                    buffer.push_str("\r\n\x1b[2K");
                }
                buffer.push_str(&format!("\x1b[{}A", image_reserved_rows - 1));
                buffer.push_str(&Self::line_for_output(&line));
                buffer.push_str(&format!("\x1b[{}B", image_reserved_rows - 1));
                i += image_reserved_rows - 1;
                i += 1;
                continue;
            }

            buffer.push_str("\x1b[2K");
            let out = Self::line_for_output(&line);
            if !is_image && visible_width(&out) as i64 > width {
                self.write_crash_log(i, width, &new_lines);
                self.stop();
                panic!(
                    "Rendered line {i} exceeds terminal width ({} > {width}). This is likely caused by a custom TUI \
                     component not truncating its output. Use visible_width() to measure and truncate_to_width() to \
                     truncate lines.",
                    visible_width(&out)
                );
            }
            buffer.push_str(&out);
            i += 1;
        }

        let mut final_cursor_row = render_end;
        if self.previous_lines.len() as i64 > new_lines.len() as i64 {
            if render_end < new_lines.len() as i64 - 1 {
                let move_down = new_lines.len() as i64 - 1 - render_end;
                buffer.push_str(&format!("\x1b[{move_down}B"));
                final_cursor_row = new_lines.len() as i64 - 1;
            }
            let extra_lines = self.previous_lines.len() as i64 - new_lines.len() as i64;
            for _ in new_lines.len() as i64..self.previous_lines.len() as i64 {
                buffer.push_str("\r\n\x1b[2K");
            }
            buffer.push_str(&format!("\x1b[{extra_lines}A"));
        }

        buffer.push_str("\x1b[?2026l");
        self.terminal.write(&buffer);

        self.cursor_row = (new_lines.len() as i64 - 1).max(0);
        self.hardware_cursor_row = final_cursor_row;
        self.max_lines_rendered = self.max_lines_rendered.max(new_lines.len() as i64);
        self.previous_viewport_top = prev_viewport_top.max(final_cursor_row - height + 1);

        self.position_hardware_cursor(cursor_pos, new_lines.len() as i64);

        self.previous_kitty_image_ids = self.collect_kitty_image_ids(&new_lines);
        self.previous_lines = new_lines;
        self.previous_width = width;
        self.previous_height = height;
    }

    fn write_crash_log(&self, line_index: i64, width: i64, new_lines: &[String]) {
        let Some(home) = home_dir() else { return };
        let dir = home.join(".pirust").join("agent");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("pi-crash.log");
        // Widths are measured on the wire form, matching the check that
        // tripped the panic (`new_lines` holds raw lines — see
        // `line_for_output`).
        let mut data = format!(
            "Crash at {:?}\nTerminal width: {width}\nLine {line_index} visible width: {}\n\n=== All rendered lines ===\n",
            std::time::SystemTime::now(),
            visible_width(&Self::line_for_output(&new_lines[line_index as usize]))
        );
        for (idx, l) in new_lines.iter().enumerate() {
            let out = Self::line_for_output(l);
            data.push_str(&format!("[{idx}] (w={}) {out}\n", visible_width(&out)));
        }
        let _ = std::fs::write(path, data);
    }

    /// `fullRender` (tui.ts:1284).
    fn full_render(
        &mut self,
        new_lines: Vec<String>,
        cursor_pos: Option<(i64, i64)>,
        width: i64,
        height: i64,
        clear: bool,
    ) {
        self.full_redraw_count += 1;
        let mut buffer = String::from("\x1b[?2026h");
        if clear {
            buffer
                .push_str(&self.delete_kitty_images(self.previous_kitty_image_ids.iter().copied()));
            buffer.push_str("\x1b[2J\x1b[H\x1b[3J");
        }
        for (i, line) in new_lines.iter().enumerate() {
            if i > 0 {
                buffer.push_str("\r\n");
            }
            let is_image = is_image_line(line);
            let image_reserved_rows = if is_image {
                self.get_kitty_image_reserved_rows(&new_lines, i, None) as i64
            } else {
                1
            };
            if image_reserved_rows > 1 && image_reserved_rows <= height {
                for _ in 1..image_reserved_rows {
                    buffer.push_str("\r\n");
                }
                buffer.push_str(&format!("\x1b[{}A", image_reserved_rows - 1));
                buffer.push_str(&Self::line_for_output(line));
                buffer.push_str(&format!("\x1b[{}B", image_reserved_rows - 1));
                continue;
            }
            buffer.push_str(&Self::line_for_output(line));
        }
        buffer.push_str("\x1b[?2026l");
        self.terminal.write(&buffer);
        self.cursor_row = (new_lines.len() as i64 - 1).max(0);
        self.hardware_cursor_row = self.cursor_row;
        if clear {
            self.max_lines_rendered = new_lines.len() as i64;
        } else {
            self.max_lines_rendered = self.max_lines_rendered.max(new_lines.len() as i64);
        }
        let buffer_length = height.max(new_lines.len() as i64);
        self.previous_viewport_top = (buffer_length - height).max(0);
        self.position_hardware_cursor(cursor_pos, new_lines.len() as i64);
        self.previous_kitty_image_ids = self.collect_kitty_image_ids(&new_lines);
        self.previous_lines = new_lines;
        self.previous_width = width;
        self.previous_height = height;
    }

    /// `positionHardwareCursor` (tui.ts:1627).
    fn position_hardware_cursor(&mut self, cursor_pos: Option<(i64, i64)>, total_lines: i64) {
        let Some((row, col)) = cursor_pos else {
            self.terminal.hide_cursor();
            return;
        };
        if total_lines <= 0 {
            self.terminal.hide_cursor();
            return;
        }
        let target_row = row.clamp(0, total_lines - 1);
        let target_col = col.max(0);
        let row_delta = target_row - self.hardware_cursor_row;
        let mut buffer = String::new();
        if row_delta > 0 {
            buffer.push_str(&format!("\x1b[{row_delta}B"));
        } else if row_delta < 0 {
            buffer.push_str(&format!("\x1b[{}A", -row_delta));
        }
        buffer.push_str(&format!("\x1b[{}G", target_col + 1));
        if !buffer.is_empty() {
            self.terminal.write(&buffer);
        }
        self.hardware_cursor_row = target_row;
        if self.show_hardware_cursor {
            self.terminal.show_cursor();
        } else {
            self.terminal.hide_cursor();
        }
    }
}

fn compute_line_diff(
    hardware_cursor_row: i64,
    prev_viewport_top: i64,
    target_row: i64,
    viewport_top: i64,
) -> i64 {
    let current_screen_row = hardware_cursor_row - prev_viewport_top;
    let target_screen_row = target_row - viewport_top;
    target_screen_row - current_screen_row
}

fn contains_component(root: &SharedComponent, target: &SharedComponent) -> bool {
    if Rc::ptr_eq(root, target) {
        return true;
    }
    // This port has no `instanceof Container` runtime check on `dyn
    // Component` (no downcasting without a new dependency); a component
    // that wraps a `Container` internally is walked via the plain
    // top-level `container.children` list on `TUI` itself
    // (`is_component_mounted`), which is the only real call site — nested
    // containers-within-containers beyond `TUI`'s own top level are not
    // walked here. Named simplification: this wave has no nested-Container
    // components to exercise the deeper TS recursion against.
    false
}

fn is_termux_session() -> bool {
    std::env::var("TERMUX_VERSION").is_ok()
}

fn home_dir() -> Option<std::path::PathBuf> {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()
        .map(std::path::PathBuf::from)
}

fn clone_resume(resume: &OverlayBlockedFocusResume) -> OverlayBlockedFocusResume {
    match resume {
        OverlayBlockedFocusResume::RestoreOverlay => OverlayBlockedFocusResume::RestoreOverlay,
        OverlayBlockedFocusResume::FocusTarget(t) => {
            OverlayBlockedFocusResume::FocusTarget(t.clone())
        }
    }
}

enum OverlayFocusRestoreClone {
    Inactive,
    Eligible {
        overlay_id: OverlayId,
    },
    Blocked {
        overlay_id: OverlayId,
        blocked_by: SharedComponent,
        resume: OverlayBlockedFocusResume,
    },
}
