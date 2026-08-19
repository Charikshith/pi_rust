//! Port of `packages/tui/src/components/editor.ts` — the multi-line line
//! editor (2,333 TS lines). See `docs/analysis/05-tui.md` §5 and §9.
//!
//! ## UTF-16 code-unit cursor arithmetic (the central hazard)
//!
//! JS strings are UTF-16: `.length`, `.slice(col)`, `charCodeAt` all operate
//! on UTF-16 code units. Cursor columns (`cursorCol`) and all
//! `startIndex`/`endIndex` in `wordWrapLine`/`buildVisualLineMap` are UTF-16
//! offsets, **not** `char`/byte indices. This port follows `input.rs`'s
//! `encode_utf16`-based technique (`utf16_len`/`slice_utf16`) everywhere a
//! column is converted to a byte offset for `String` slicing. Grapheme
//! segmentation is done with `unicode-segmentation`, which is byte-indexed;
//! the editor's `segment()` wraps it into UTF-16-indexed segments exactly like
//! `word_navigation.rs` does.
//!
//! ## Scope decisions (documented, not silent — AGENTS.md Correctness Bar)
//!
//! - **Autocomplete is synchronous, not async.** The TS uses
//!   `AbortController`/`setTimeout`/a promise chain to debounce and cancel
//!   autocomplete requests. This port has no owned event loop (same
//!   caller-owns-the-timer story as Waves 2/4/5), so
//!   [`Editor::request_autocomplete`] runs the provider's `get_suggestions`
//!   synchronously (blocking) and skips the debounce timer entirely. The
//!   `ATTACHMENT_AUTOCOMPLETE_DEBOUNCE_MS=20` debounce, `AbortController`
//!   cancellation, and the promise-serialization are documented deferred
//!   residuals — `getAutocompleteDebounceMs` still returns the same value the
//!   TS would, so the *trigger* logic is faithful; only the delay is elided.
//! - **`tui.requestRender()` is a no-op.** The editor calls
//!   `this.tui.requestRender()` after autocomplete updates. `Editor` holds a
//!   `Rc<RefCell<TUI>>`-style reference only for `terminal.rows` and to fire
//!   the render request; in this wave the render request is elided (the
//!   caller-driven `TUI::poll()` model means a parent loop re-renders anyway).
//! - **`focused` drives the hardware-cursor marker** exactly like the TS's
//!   `emitCursorMarker = this.focused`.
//!
//! ## Autocomplete trigger patterns
//!
//! `buildTriggerPattern`/`buildDebouncePattern` are regexes built from
//! `escapeCharacterClass`; this port builds the same character classes as
//! fixed matchers over the UTF-16 slices.

use std::cell::RefCell;
use std::rc::Rc;

use unicode_segmentation::UnicodeSegmentation;

use crate::autocomplete::{AutocompleteProvider, AutocompleteSuggestions};
use crate::components::input::OnTextFnMut;
use crate::components::select_list::{
    SelectItem, SelectList, SelectListLayoutOptions, SelectListTheme,
};
use crate::keybindings::{get_keybindings, Keybinding};
use crate::keys::{decode_printable_key, matches_key};
use crate::kill_ring::KillRing;
use crate::tui::{Component, Focusable, CURSOR_MARKER, TUI};
use crate::undo_stack::UndoStack;
use crate::utils::{is_cjk_break_char, is_whitespace_char, slice_by_column, visible_width};
use crate::word_navigation::{find_word_backward, find_word_forward, WordNavigationOptions};

/// `ATTACHMENT_AUTOCOMPLETE_DEBOUNCE_MS` (editor.ts:243).
const ATTACHMENT_AUTOCOMPLETE_DEBOUNCE_MS: usize = 20;
/// `DEFAULT_AUTOCOMPLETE_TRIGGER_CHARACTERS` (editor.ts:244).
const DEFAULT_AUTOCOMPLETE_TRIGGER_CHARACTERS: [char; 2] = ['@', '#'];

/// `SLASH_COMMAND_SELECT_LIST_LAYOUT` (editor.ts:238).
const SLASH_COMMAND_SELECT_LIST_LAYOUT: SelectListLayoutOptions = SelectListLayoutOptions {
    min_primary_column_width: Some(12),
    max_primary_column_width: Some(32),
    truncate_primary: None,
};

/// A `[paste #N ...]` marker. The regexes are `g`-flagged in the TS; we scan
/// manually so a single function serves both the global and single-segment
/// tests.
fn parse_paste_marker(s: &str) -> Option<(usize, Option<String>)> {
    let rest = s.strip_prefix("[paste #")?;
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    let id: usize = digits.parse().ok()?;
    let after = &rest[digits.len()..];
    // Optional suffix: ` +123 lines` or ` 1234 chars`.
    let suffix = after.strip_prefix(' ').map(|s| s.to_string());
    Some((id, suffix))
}

/// `isPasteMarker` (editor.ts:27).
fn is_paste_marker(segment: &str) -> bool {
    segment.len() >= 10 && parse_paste_marker(segment).is_some()
}

/// `buildTriggerPattern` (editor.ts:250) as a predicate over the text before
/// the cursor: `(?:^|[\s])[triggers][^\s]*$`.
fn trigger_pattern_matches(text: &str, triggers: &[char]) -> bool {
    let Some(last) = text.chars().last() else {
        return false;
    };
    // [^\s]*$ — the tail is all non-whitespace.
    if text.chars().any(char::is_whitespace) {
        return false;
    }
    if !triggers.contains(&last) {
        return false;
    }
    // (?:^|[\s]) before the trigger — text is all non-whitespace, so the
    // trigger must be at index 0 (or preceded by whitespace, which the
    // non-whitespace check already ruled out — except the trigger itself).
    // The TS regex allows `^` OR `[\s]`; since the whole tail is non-space,
    // the only match is the trigger at the start.
    text.len() == 1
        || text
            .chars()
            .nth(text.chars().count() - 2)
            .is_some_and(char::is_whitespace)
}

/// `buildDebouncePattern` (editor.ts:254): the `@`-quoted form or any other
/// trigger char at a word boundary. Implemented as a predicate.
fn debounce_pattern_matches(text: &str, triggers: &[char]) -> bool {
    // `(?:^|[ \t])(?:@(?:"[^"]*|[^\s]*)|[triggers][^\s]*)$`
    let Some(last) = text.chars().last() else {
        return false;
    };
    if last == '@' {
        // @(...) form: `@"..."` or `@...` — the tail after @ is non-space.
        let rest = &text[..text.len() - 1];
        return rest.chars().all(|c| c != ' ' && c != '\t');
    }
    if triggers.contains(&last) {
        let rest = &text[..text.len() - 1];
        return rest.chars().all(|c| c != ' ' && c != '\t');
    }
    false
}

/// `createScrollBorder` (editor.ts:259).
fn create_scroll_border(direction: char, hidden_line_count: usize, width: usize) -> String {
    let available_width = width;
    let indicator = format!("─── {direction} {hidden_line_count} more ");
    let indicator_width = visible_width(&indicator);
    if available_width >= indicator_width {
        return indicator + &"─".repeat(available_width - indicator_width);
    }
    let ellipsis = "...";
    let ellipsis_width = visible_width(ellipsis).min(available_width);
    let indicator_width = available_width.saturating_sub(ellipsis_width);
    slice_by_column(&indicator, 0, indicator_width, true) + ellipsis
}

/// `TextChunk` (editor.ts:97).
#[derive(Debug, Clone)]
pub struct TextChunk {
    pub text: String,
    /// UTF-16 code-unit offset into the original line.
    pub start_index: usize,
    /// UTF-16 code-unit offset (exclusive) into the original line.
    pub end_index: usize,
}

/// `wordWrapLine` (editor.ts:114) — split a line into word-wrapped chunks at
/// `max_width` visible columns. Returns the same `TextChunk` list the TS does.
pub fn word_wrap_line(
    line: &str,
    max_width: usize,
    pre_segmented: Option<&[GraphemeSeg]>,
) -> Vec<TextChunk> {
    if line.is_empty() || max_width == 0 {
        return vec![TextChunk {
            text: String::new(),
            start_index: 0,
            end_index: 0,
        }];
    }
    let line_width = visible_width(line);
    if line_width <= max_width {
        return vec![TextChunk {
            text: line.to_string(),
            start_index: 0,
            end_index: utf16_len(line),
        }];
    }

    // UTF-16-indexed grapheme segments. `pre_segmented` is the
    // paste-marker-aware list; otherwise segment the whole line.
    let owned_segments;
    let segments: Vec<GraphemeSeg> = match pre_segmented {
        Some(s) => s.to_vec(),
        None => {
            owned_segments = segment_graphemes(line);
            owned_segments
        }
    };

    let mut chunks: Vec<TextChunk> = Vec::new();
    let mut current_width = 0usize;
    let mut chunk_start = 0usize; // UTF-16 offset

    // Wrap opportunity: position after the last whitespace before a
    // non-whitespace grapheme (UTF-16 offset where a break is allowed).
    let mut wrap_opp_index: isize = -1;
    let mut wrap_opp_width = 0usize;

    for i in 0..segments.len() {
        let seg = &segments[i];
        let grapheme = seg.text.as_str();
        let g_width = visible_width(grapheme);
        let char_index = seg.index; // UTF-16 offset of this grapheme
        let is_ws = !is_paste_marker(grapheme) && is_whitespace_char(grapheme);

        // Overflow check before advancing.
        if current_width + g_width > max_width {
            if wrap_opp_index >= 0 && current_width - wrap_opp_width + g_width <= max_width {
                // Backtrack to last wrap opportunity.
                chunks.push(TextChunk {
                    text: slice_utf16(line, chunk_start, wrap_opp_index as usize).to_string(),
                    start_index: chunk_start,
                    end_index: wrap_opp_index as usize,
                });
                chunk_start = wrap_opp_index as usize;
                current_width -= wrap_opp_width;
            } else if chunk_start < char_index {
                // Force-break at current position.
                chunks.push(TextChunk {
                    text: slice_utf16(line, chunk_start, char_index).to_string(),
                    start_index: chunk_start,
                    end_index: char_index,
                });
                chunk_start = char_index;
                current_width = 0;
            }
            wrap_opp_index = -1;
        }

        if g_width > max_width {
            // Single atomic segment wider than maxWidth (e.g. paste marker in
            // a narrow terminal). Re-wrap at grapheme granularity.
            let sub_chunks = word_wrap_line(grapheme, max_width, None);
            for sc in sub_chunks.iter().take(sub_chunks.len().saturating_sub(1)) {
                chunks.push(TextChunk {
                    text: sc.text.clone(),
                    start_index: char_index + sc.start_index,
                    end_index: char_index + sc.end_index,
                });
            }
            let last = sub_chunks.last().unwrap();
            chunk_start = char_index + last.start_index;
            current_width = visible_width(&last.text);
            wrap_opp_index = -1;
            continue;
        }

        // Advance.
        current_width += g_width;

        // Record wrap opportunity.
        let next = segments.get(i + 1);
        if is_ws && next.is_some_and(|n| is_paste_marker(&n.text) || !is_whitespace_char(&n.text)) {
            wrap_opp_index = next.unwrap().index as isize;
            wrap_opp_width = current_width;
        } else if !is_ws && next.is_some_and(|n| !is_whitespace_char(&n.text)) {
            let is_cjk = !is_paste_marker(grapheme) && grapheme.chars().any(is_cjk_break_char);
            let next_is_cjk = !is_paste_marker(&next.unwrap().text)
                && next.unwrap().text.chars().any(is_cjk_break_char);
            if is_cjk || next_is_cjk {
                wrap_opp_index = next.unwrap().index as isize;
                wrap_opp_width = current_width;
            }
        }
    }

    // Push final chunk.
    chunks.push(TextChunk {
        text: slice_utf16(line, chunk_start, utf16_len(line)).to_string(),
        start_index: chunk_start,
        end_index: utf16_len(line),
    });

    chunks
}

/// A single grapheme segment with its UTF-16 start offset — the Rust shape of
/// an `Intl.SegmentData` for the editor's `segment()`.
#[derive(Debug, Clone)]
pub struct GraphemeSeg {
    pub text: String,
    pub index: usize, // UTF-16 offset
}

/// UTF-16-indexed grapheme segmentation (`Intl.Segmenter` → `unicode-segmentation`).
pub fn segment_graphemes(text: &str) -> Vec<GraphemeSeg> {
    let mut out = Vec::new();
    let mut utf16_pos = 0usize;
    for g in text.graphemes(true) {
        out.push(GraphemeSeg {
            text: g.to_string(),
            index: utf16_pos,
        });
        utf16_pos += utf16_len(g);
    }
    out
}

/// UTF-16 code-unit length (`string.length`).
pub fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// UTF-16 code-unit offset → byte index (for `String` slicing).
pub fn utf16_to_byte_index(s: &str, utf16_idx: usize) -> usize {
    let mut utf16_pos = 0usize;
    for (byte_idx, ch) in s.char_indices() {
        if utf16_pos >= utf16_idx {
            return byte_idx;
        }
        utf16_pos += ch.len_utf16();
    }
    s.len()
}

/// `slice(utf16_start, utf16_end)` — UTF-16-slice of `s` (clamped).
pub fn slice_utf16(s: &str, start: usize, end: usize) -> &str {
    let start_b = utf16_to_byte_index(s, start);
    let end_b = utf16_to_byte_index(s, end.max(start));
    &s[start_b..end_b]
}

// =============================================================================
// Editor
// =============================================================================

/// `EditorState` (editor.ts:228).
#[derive(Debug, Clone, PartialEq)]
struct EditorState {
    lines: Vec<String>,
    cursor_line: usize,
    cursor_col: usize, // UTF-16 code units
}

/// `EditorSnapshot` (editor.ts:240) — undo snapshot.
#[derive(Debug, Clone)]
struct EditorSnapshot {
    state: EditorState,
    pastes: Vec<(usize, String)>,
    paste_counter: usize,
}

/// `LayoutLine` (editor.ts:244).
struct LayoutLine {
    text: String,
    has_cursor: bool,
    cursor_pos: Option<usize>, // UTF-16 offset into `text`
}

/// `EditorOptions` (editor.ts:233).
#[derive(Debug, Clone, Default)]
pub struct EditorOptions {
    pub padding_x: Option<usize>,
    pub autocomplete_max_visible: Option<usize>,
}

/// `Editor` (editor.ts:270) — the multi-line line editor.
///
/// Implements [`Component`] + [`Focusable`] (editor.ts:270) and the
/// [`crate::editor_component::EditorComponent`] seam.
pub struct Editor {
    state: EditorState,
    focused: bool,
    tui: Rc<RefCell<TUI>>,
    border_color: crate::components::ColorFn,
    padding_x: usize,
    last_width: usize,
    scroll_offset: usize,
    autocomplete_provider: Option<Box<dyn AutocompleteProvider>>,
    autocomplete_trigger_characters: Vec<char>,
    autocomplete_list: Option<SelectList>,
    autocomplete_state: Option<&'static str>, // "regular" | "force"
    autocomplete_prefix: String,
    autocomplete_max_visible: usize,
    pastes: Vec<(usize, String)>, // Vec as ordered map
    paste_counter: usize,
    paste_buffer: String,
    is_in_paste: bool,
    history: Vec<String>,
    history_index: isize, // -1 = not browsing
    history_draft: Option<EditorState>,
    kill_ring: KillRing,
    last_action: Option<&'static str>, // "kill" | "yank" | "type-word"
    jump_mode: Option<&'static str>,   // "forward" | "backward"
    preferred_visual_col: Option<usize>,
    snapped_from_cursor_col: Option<usize>,
    undo_stack: UndoStack<EditorSnapshot>,
    pub on_submit: Option<OnTextFnMut>,
    pub on_change: Option<OnTextFnMut>,
    pub disable_submit: bool,
    /// Cached `terminal_rows` for reads that happen mid-render (when the
    /// TUI that renders this editor is already mutably borrowed — a
    /// re-entrant `RefCell::borrow` would panic). See [`Editor::terminal_rows`].
    cached_terminal_rows: u16,
}

impl Editor {
    /// `constructor` (editor.ts:345).
    pub fn new(
        tui: Rc<RefCell<TUI>>,
        border_color: crate::components::ColorFn,
        options: EditorOptions,
    ) -> Self {
        let padding_x = options.padding_x.unwrap_or(0);
        let max_visible = options.autocomplete_max_visible.unwrap_or(5);
        Self {
            state: EditorState {
                lines: vec![String::new()],
                cursor_line: 0,
                cursor_col: 0,
            },
            focused: false,
            tui,
            border_color,
            padding_x,
            last_width: 80,
            scroll_offset: 0,
            autocomplete_provider: None,
            autocomplete_trigger_characters: DEFAULT_AUTOCOMPLETE_TRIGGER_CHARACTERS.to_vec(),
            autocomplete_list: None,
            autocomplete_state: None,
            autocomplete_prefix: String::new(),
            autocomplete_max_visible: max_visible.clamp(3, 20),
            pastes: Vec::new(),
            paste_counter: 0,
            paste_buffer: String::new(),
            is_in_paste: false,
            history: Vec::new(),
            history_index: -1,
            history_draft: None,
            kill_ring: KillRing::new(),
            last_action: None,
            jump_mode: None,
            preferred_visual_col: None,
            snapped_from_cursor_col: None,
            undo_stack: UndoStack::new(),
            on_submit: None,
            on_change: None,
            disable_submit: false,
            cached_terminal_rows: 24,
        }
    }

    /// `getPaddingX` (editor.ts:365).
    pub fn get_padding_x(&self) -> usize {
        self.padding_x
    }

    /// `setPaddingX` (editor.ts:369).
    pub fn set_padding_x(&mut self, padding: usize) {
        self.padding_x = padding;
    }

    /// `getAutocompleteMaxVisible` (editor.ts:377).
    pub fn get_autocomplete_max_visible(&self) -> usize {
        self.autocomplete_max_visible
    }

    /// `setAutocompleteMaxVisible` (editor.ts:381).
    pub fn set_autocomplete_max_visible(&mut self, max_visible: usize) {
        self.autocomplete_max_visible = max_visible.clamp(3, 20);
    }

    /// `setAutocompleteProvider` (editor.ts:389).
    pub fn set_autocomplete_provider(&mut self, provider: Option<Box<dyn AutocompleteProvider>>) {
        self.autocomplete_provider = provider;
    }

    /// `addToHistory` (editor.ts:399).
    pub fn add_to_history(&mut self, text: &str) {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return;
        }
        if self.history.first().is_some_and(|h| h == trimmed) {
            return;
        }
        self.history.insert(0, trimmed.to_string());
        if self.history.len() > 100 {
            self.history.pop();
        }
    }

    /// `invalidate` (editor.ts:478).
    fn invalidate(&mut self) {}

    // -- Internal helpers ------------------------------------------------------

    fn valid_paste_ids(&self) -> Vec<usize> {
        self.pastes.iter().map(|(id, _)| *id).collect()
    }

    /// `segment(text, mode)` (editor.ts:361) — grapheme or word segmentation
    /// with paste-marker awareness.
    fn segment(&self, text: &str, mode: &str) -> Vec<GraphemeSeg> {
        segment_with_markers(text, mode, &self.valid_paste_ids())
    }

    fn is_editor_empty(&self) -> bool {
        self.state.lines.len() == 1 && self.state.lines[0].is_empty()
    }

    fn is_on_first_visual_line(&self) -> bool {
        let visual = self.build_visual_line_map(self.last_width);
        self.find_current_visual_line(&visual) == 0
    }

    fn is_on_last_visual_line(&self) -> bool {
        let visual = self.build_visual_line_map(self.last_width);
        self.find_current_visual_line(&visual) == visual.len() - 1
    }

    fn navigate_history(&mut self, direction: i8) {
        self.last_action = None;
        if self.history.is_empty() {
            return;
        }
        let new_index = self.history_index - direction as isize;
        if new_index < -1 || new_index >= self.history.len() as isize {
            return;
        }
        if self.history_index == -1 && new_index >= 0 {
            self.push_undo_snapshot();
            self.history_draft = Some(self.state.clone());
        }
        self.history_index = new_index;
        if self.history_index == -1 {
            let draft = self.history_draft.take();
            if let Some(draft) = draft {
                self.state = draft;
                self.preferred_visual_col = None;
                self.snapped_from_cursor_col = None;
                self.scroll_offset = 0;
                self.fire_change();
            } else {
                self.set_text_internal("", "end");
            }
        } else {
            let text = self.history[self.history_index as usize].clone();
            self.set_text_internal(&text, if direction == -1 { "start" } else { "end" });
        }
    }

    fn exit_history_browsing(&mut self) {
        self.history_index = -1;
        self.history_draft = None;
    }

    fn set_text_internal(&mut self, text: &str, cursor_placement: &str) {
        let lines: Vec<String> = if text.is_empty() {
            vec![String::new()]
        } else {
            text.split('\n').map(str::to_string).collect()
        };
        self.state.lines = lines;
        self.state.cursor_line = if cursor_placement == "start" {
            0
        } else {
            self.state.lines.len() - 1
        };
        let line_len = self.state.lines[self.state.cursor_line].clone();
        self.set_cursor_col(if cursor_placement == "start" {
            0
        } else {
            utf16_len(&line_len)
        });
        self.scroll_offset = 0;
        self.fire_change();
    }

    fn fire_change(&mut self) {
        let text = self.get_text();
        if let Some(cb) = &mut self.on_change {
            cb(&text);
        }
    }

    fn fire_submit(&mut self, text: &str) {
        if let Some(cb) = &mut self.on_submit {
            cb(text);
        }
    }

    /// `getText` (editor.ts:993).
    /// `terminalRows()` (editor.ts:500,1871) — reads the terminal height
    /// through the TUI handle. The TUI that renders this editor holds a
    /// mutable borrow of itself during `render`; a plain `RefCell::borrow`
    /// here would panic re-entrantly. `try_borrow` with a cached fallback
    /// keeps the read working (rows don't change mid-render) without
    /// fighting the borrow checker.
    pub fn terminal_rows(&mut self) -> u16 {
        match self.tui.try_borrow() {
            Ok(tui) => {
                let rows = tui.terminal_rows();
                self.cached_terminal_rows = rows;
                rows
            }
            Err(_) => self.cached_terminal_rows,
        }
    }

    pub fn get_text(&self) -> String {
        self.state.lines.join("\n")
    }

    /// `getExpandedText` (editor.ts:1010).
    pub fn get_expanded_text(&self) -> String {
        self.expand_paste_markers(&self.state.lines.join("\n"))
    }

    /// `getLines` (editor.ts:1014).
    pub fn get_lines(&self) -> Vec<String> {
        self.state.lines.clone()
    }

    /// `getCursor` (editor.ts:1018).
    pub fn get_cursor(&self) -> (usize, usize) {
        (self.state.cursor_line, self.state.cursor_col)
    }

    /// `setText` (editor.ts:1022).
    pub fn set_text(&mut self, text: &str) {
        self.cancel_autocomplete();
        self.last_action = None;
        self.exit_history_browsing();
        let normalized = self.normalize_text(text);
        if self.get_text() != normalized {
            self.push_undo_snapshot();
        }
        self.pastes.clear();
        self.paste_counter = 0;
        self.set_text_internal(&normalized, "end");
    }

    /// `insertTextAtCursor` (editor.ts:1041).
    pub fn insert_text_at_cursor(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.cancel_autocomplete();
        self.push_undo_snapshot();
        self.last_action = None;
        self.exit_history_browsing();
        self.insert_text_at_cursor_internal(text);
    }

    fn normalize_text(&self, text: &str) -> String {
        text.replace("\r\n", "\n")
            .replace('\r', "\n")
            .replace('\t', "    ")
    }

    fn insert_text_at_cursor_internal(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let normalized = self.normalize_text(text);
        let inserted_lines: Vec<&str> = normalized.split('\n').collect();
        let current_line = self.state.lines[self.state.cursor_line].clone();
        let before_cursor = slice_utf16(&current_line, 0, self.state.cursor_col).to_string();
        let after_cursor = slice_utf16(
            &current_line,
            self.state.cursor_col,
            utf16_len(&current_line),
        )
        .to_string();

        if inserted_lines.len() == 1 {
            self.state.lines[self.state.cursor_line] = before_cursor + &normalized + &after_cursor;
            self.set_cursor_col(self.state.cursor_col + utf16_len(&normalized));
        } else {
            let mut new_lines: Vec<String> = Vec::new();
            new_lines.extend(self.state.lines[..self.state.cursor_line].iter().cloned());
            new_lines.push(before_cursor + inserted_lines[0]);
            new_lines.extend(
                inserted_lines[1..inserted_lines.len() - 1]
                    .iter()
                    .map(|s| s.to_string()),
            );
            new_lines.push(inserted_lines[inserted_lines.len() - 1].to_string() + &after_cursor);
            new_lines.extend(
                self.state.lines[self.state.cursor_line + 1..]
                    .iter()
                    .cloned(),
            );
            self.state.lines = new_lines;
            self.state.cursor_line += inserted_lines.len() - 1;
            let last_len = utf16_len(inserted_lines[inserted_lines.len() - 1]);
            self.set_cursor_col(last_len);
        }
        self.fire_change();
    }

    fn insert_character(&mut self, ch: &str, skip_undo_coalescing: Option<bool>) {
        self.exit_history_browsing();
        if skip_undo_coalescing != Some(true) {
            if is_whitespace_char(ch) || self.last_action != Some("type-word") {
                self.push_undo_snapshot();
            }
            self.last_action = Some("type-word");
        }
        let line = self.state.lines[self.state.cursor_line].clone();
        let before = slice_utf16(&line, 0, self.state.cursor_col).to_string();
        let after = slice_utf16(&line, self.state.cursor_col, utf16_len(&line)).to_string();
        self.state.lines[self.state.cursor_line] = before + ch + &after;
        self.set_cursor_col(self.state.cursor_col + utf16_len(ch));
        self.fire_change();

        // Autocomplete triggers (synchronous — see module docs).
        if self.autocomplete_state.is_none() {
            if ch == "/" && self.is_at_start_of_message() {
                self.try_trigger_autocomplete(false);
            } else if self
                .autocomplete_trigger_characters
                .contains(&ch.chars().next().unwrap_or('\0'))
            {
                let current_line = self.state.lines[self.state.cursor_line].clone();
                let text_before_cursor = slice_utf16(&current_line, 0, self.state.cursor_col);
                let char_before_symbol = slice_utf16(
                    text_before_cursor,
                    utf16_len(text_before_cursor).saturating_sub(2),
                    utf16_len(text_before_cursor).saturating_sub(1),
                );
                if utf16_len(text_before_cursor) == 1
                    || char_before_symbol == " "
                    || char_before_symbol == "\t"
                {
                    self.try_trigger_autocomplete(false);
                }
            } else if ch
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphanumeric() || ".-_".contains(c))
            {
                let current_line = self.state.lines[self.state.cursor_line].clone();
                let text_before_cursor = slice_utf16(&current_line, 0, self.state.cursor_col);
                if self.is_in_slash_command_context(text_before_cursor)
                    || self.autocomplete_trigger_pattern_matches(text_before_cursor)
                {
                    self.try_trigger_autocomplete(false);
                }
            }
        } else {
            self.update_autocomplete();
        }
    }

    fn autocomplete_trigger_pattern_matches(&self, text: &str) -> bool {
        trigger_pattern_matches(text, &self.autocomplete_trigger_characters)
    }

    fn autocomplete_debounce_pattern_matches(&self, text: &str) -> bool {
        debounce_pattern_matches(text, &self.autocomplete_trigger_characters)
    }

    fn handle_paste(&mut self, pasted_text: &str) {
        self.cancel_autocomplete();
        self.exit_history_browsing();
        self.last_action = None;
        self.push_undo_snapshot();

        // Decode tmux-re-encoded control bytes: `\x1b[<cp>;5u` → literal.
        let decoded_text = decode_csi_u_controls(pasted_text);
        let clean_text = self.normalize_text(&decoded_text);
        let mut filtered_text: String = clean_text
            .chars()
            .filter(|c| *c == '\n' || (*c as u32) >= 32)
            .collect();

        // If pasting a file path and char before cursor is a word char, prepend space.
        if filtered_text.starts_with('/')
            || filtered_text.starts_with('~')
            || filtered_text.starts_with('.')
        {
            let current_line = self.state.lines[self.state.cursor_line].clone();
            let char_before_cursor = if self.state.cursor_col > 0 {
                slice_utf16(
                    &current_line,
                    self.state.cursor_col - 1,
                    self.state.cursor_col,
                )
            } else {
                ""
            };
            if !char_before_cursor.is_empty()
                && char_before_cursor
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '_')
            {
                filtered_text = format!(" {filtered_text}");
            }
        }

        let pasted_lines: Vec<&str> = filtered_text.split('\n').collect();
        let total_chars = utf16_len(&filtered_text);
        if pasted_lines.len() > 10 || total_chars > 1000 {
            self.paste_counter += 1;
            let paste_id = self.paste_counter;
            self.pastes.retain(|(id, _)| *id != paste_id);
            self.pastes.push((paste_id, filtered_text.clone()));
            let marker = if pasted_lines.len() > 10 {
                format!("[paste #{paste_id} +{} lines]", pasted_lines.len())
            } else {
                format!("[paste #{paste_id} {total_chars} chars]")
            };
            self.insert_text_at_cursor_internal(&marker);
            return;
        }
        self.insert_text_at_cursor_internal(&filtered_text);
    }

    fn add_new_line(&mut self) {
        self.cancel_autocomplete();
        self.exit_history_browsing();
        self.last_action = None;
        self.push_undo_snapshot();
        let current_line = self.state.lines[self.state.cursor_line].clone();
        let before = slice_utf16(&current_line, 0, self.state.cursor_col).to_string();
        let after = slice_utf16(
            &current_line,
            self.state.cursor_col,
            utf16_len(&current_line),
        )
        .to_string();
        self.state.lines[self.state.cursor_line] = before;
        self.state.lines.insert(self.state.cursor_line + 1, after);
        self.state.cursor_line += 1;
        self.set_cursor_col(0);
        self.fire_change();
    }

    fn should_submit_on_backslash_enter(&self, data: &str) -> bool {
        if self.disable_submit {
            return false;
        }
        if !matches_key(data, "enter") {
            return false;
        }
        let kb = get_keybindings();
        let submit_keys = kb.get_keys(Keybinding::InputSubmit);
        let has_shift_enter = submit_keys
            .iter()
            .any(|k| k == "shift+enter" || k == "shift+return");
        if !has_shift_enter {
            return false;
        }
        let current_line = self.state.lines[self.state.cursor_line].clone();
        self.state.cursor_col > 0
            && slice_utf16(
                &current_line,
                self.state.cursor_col - 1,
                self.state.cursor_col,
            ) == "\\"
    }

    fn submit_value(&mut self) {
        self.cancel_autocomplete();
        let result = self
            .expand_paste_markers(&self.state.lines.join("\n"))
            .trim()
            .to_string();
        self.state = EditorState {
            lines: vec![String::new()],
            cursor_line: 0,
            cursor_col: 0,
        };
        self.pastes.clear();
        self.paste_counter = 0;
        self.exit_history_browsing();
        self.scroll_offset = 0;
        self.undo_stack.clear();
        self.last_action = None;
        self.fire_change();
        self.fire_submit(&result);
    }

    fn handle_backspace(&mut self) {
        self.exit_history_browsing();
        self.last_action = None;

        if self.state.cursor_col > 0 {
            self.push_undo_snapshot();
            let mut line = self.state.lines[self.state.cursor_line].clone();
            let before_cursor = slice_utf16(&line, 0, self.state.cursor_col);
            let graphemes = self.segment(before_cursor, "grapheme");
            let last_grapheme = graphemes.last().cloned();
            let grapheme_length = last_grapheme
                .as_ref()
                .map(|g| utf16_len(&g.text))
                .unwrap_or(1);
            let is_pasted_segmented = last_grapheme
                .as_ref()
                .and_then(|g| parse_paste_marker(&g.text));

            if let Some((target_id, _)) = is_pasted_segmented {
                self.pastes.retain(|(id, _)| *id != target_id);
                self.paste_counter = self.paste_counter.saturating_sub(1);
                // Shift registry entries down in ascending id order.
                let mut higher: Vec<usize> = self
                    .pastes
                    .iter()
                    .map(|(id, _)| *id)
                    .filter(|id| *id > target_id)
                    .collect();
                higher.sort_unstable();
                for id in higher {
                    let content = self
                        .pastes
                        .iter()
                        .find(|(i, _)| *i == id)
                        .map(|(_, c)| c.clone())
                        .unwrap_or_default();
                    self.pastes.retain(|(i, _)| *i != id);
                    self.pastes.push((id - 1, content));
                }
                // Renumber markers with ids greater than the removed one.
                let lines = self.state.lines.clone();
                self.state.lines = lines
                    .iter()
                    .map(|l| renumber_paste_markers(l, target_id))
                    .collect();
            }

            line = self.state.lines[self.state.cursor_line].clone();
            let before = slice_utf16(
                &line,
                0,
                self.state.cursor_col.saturating_sub(grapheme_length),
            )
            .to_string();
            let after = slice_utf16(&line, self.state.cursor_col, utf16_len(&line)).to_string();
            self.state.lines[self.state.cursor_line] = before + &after;
            self.set_cursor_col(self.state.cursor_col - grapheme_length);
        } else if self.state.cursor_line > 0 {
            self.push_undo_snapshot();
            let current_line = self.state.lines[self.state.cursor_line].clone();
            let previous_line = self.state.lines[self.state.cursor_line - 1].clone();
            let prev_len = utf16_len(&previous_line);
            self.state.lines[self.state.cursor_line - 1] = previous_line + &current_line;
            self.state.lines.remove(self.state.cursor_line);
            self.state.cursor_line -= 1;
            self.set_cursor_col(prev_len);
        }

        self.fire_change();
        if self.autocomplete_state.is_some() {
            self.update_autocomplete();
        } else {
            let current_line = self.state.lines[self.state.cursor_line].clone();
            let text_before_cursor = slice_utf16(&current_line, 0, self.state.cursor_col);
            if self.is_in_slash_command_context(text_before_cursor)
                || self.autocomplete_trigger_pattern_matches(text_before_cursor)
            {
                self.try_trigger_autocomplete(false);
            }
        }
    }

    fn set_cursor_col(&mut self, col: usize) {
        self.state.cursor_col = col;
        self.preferred_visual_col = None;
        self.snapped_from_cursor_col = None;
    }

    fn move_to_visual_line(
        &mut self,
        visual_lines: &[VisualLine],
        current_visual_line: usize,
        target_visual_line: usize,
    ) {
        let Some(current_vl) = visual_lines.get(current_visual_line) else {
            return;
        };
        let Some(target_vl) = visual_lines.get(target_visual_line) else {
            return;
        };

        let current_visual_col = if let Some(snapped) = self.snapped_from_cursor_col {
            let vl_index = self.find_visual_line_at(visual_lines, current_vl.logical_line, snapped);
            snapped.saturating_sub(visual_lines[vl_index].start_col)
        } else {
            self.state.cursor_col.saturating_sub(current_vl.start_col)
        };

        let is_last_source_segment = current_visual_line == visual_lines.len() - 1
            || visual_lines[current_visual_line + 1].logical_line != current_vl.logical_line;
        let source_max_visual_col = if is_last_source_segment {
            current_vl.length
        } else {
            current_vl.length.saturating_sub(1)
        };
        let is_last_target_segment = target_visual_line == visual_lines.len() - 1
            || visual_lines[target_visual_line + 1].logical_line != target_vl.logical_line;
        let target_max_visual_col = if is_last_target_segment {
            target_vl.length
        } else {
            target_vl.length.saturating_sub(1)
        };

        let move_to_visual_col = self.compute_vertical_move_column(
            current_visual_col,
            source_max_visual_col,
            target_max_visual_col,
        );

        self.state.cursor_line = target_vl.logical_line;
        let target_col = target_vl.start_col + move_to_visual_col;
        let logical_line = self.state.lines[target_vl.logical_line].clone();
        self.state.cursor_col = target_col.min(utf16_len(&logical_line));

        // Snap cursor to atomic segment boundary (e.g. paste markers).
        let segments = self.segment(&logical_line, "grapheme");
        for seg in &segments {
            if seg.index > self.state.cursor_col {
                break;
            }
            if utf16_len(&seg.text) <= 1 {
                continue;
            }
            if self.state.cursor_col < seg.index + utf16_len(&seg.text) {
                let is_continuation = seg.index < target_vl.start_col;
                let is_moving_down = target_visual_line > current_visual_line;
                if is_continuation && is_moving_down {
                    let seg_end = seg.index + utf16_len(&seg.text);
                    let mut next = target_visual_line + 1;
                    while next < visual_lines.len()
                        && visual_lines[next].logical_line == target_vl.logical_line
                        && visual_lines[next].start_col < seg_end
                    {
                        next += 1;
                    }
                    if next < visual_lines.len() {
                        self.move_to_visual_line(visual_lines, current_visual_line, next);
                        return;
                    }
                }
                self.snapped_from_cursor_col = Some(self.state.cursor_col);
                self.state.cursor_col = seg.index;
                return;
            }
        }
        self.snapped_from_cursor_col = None;
    }

    fn compute_vertical_move_column(
        &mut self,
        current_visual_col: usize,
        source_max_visual_col: usize,
        target_max_visual_col: usize,
    ) -> usize {
        let has_preferred = self.preferred_visual_col.is_some();
        let cursor_in_middle = current_visual_col < source_max_visual_col;
        let target_too_short = target_max_visual_col < current_visual_col;

        if !has_preferred || cursor_in_middle {
            if target_too_short {
                self.preferred_visual_col = Some(current_visual_col);
                return target_max_visual_col;
            }
            self.preferred_visual_col = None;
            return current_visual_col;
        }

        let target_cant_fit_preferred = target_max_visual_col < self.preferred_visual_col.unwrap();
        if target_too_short || target_cant_fit_preferred {
            return target_max_visual_col;
        }
        self.preferred_visual_col.take().unwrap()
    }

    fn move_to_line_start(&mut self) {
        self.last_action = None;
        self.set_cursor_col(0);
    }

    fn move_to_line_end(&mut self) {
        self.last_action = None;
        let current_line = self.state.lines[self.state.cursor_line].clone();
        self.set_cursor_col(utf16_len(&current_line));
    }

    fn delete_to_start_of_line(&mut self) {
        self.exit_history_browsing();
        let current_line = self.state.lines[self.state.cursor_line].clone();
        if self.state.cursor_col > 0 {
            self.push_undo_snapshot();
            let deleted_text = slice_utf16(&current_line, 0, self.state.cursor_col).to_string();
            self.kill_ring
                .push(&deleted_text, true, self.last_action == Some("kill"));
            self.last_action = Some("kill");
            self.state.lines[self.state.cursor_line] = slice_utf16(
                &current_line,
                self.state.cursor_col,
                utf16_len(&current_line),
            )
            .to_string();
            self.set_cursor_col(0);
        } else if self.state.cursor_line > 0 {
            self.push_undo_snapshot();
            self.kill_ring
                .push("\n", true, self.last_action == Some("kill"));
            self.last_action = Some("kill");
            let previous_line = self.state.lines[self.state.cursor_line - 1].clone();
            let prev_len = utf16_len(&previous_line);
            self.state.lines[self.state.cursor_line - 1] = previous_line + &current_line;
            self.state.lines.remove(self.state.cursor_line);
            self.state.cursor_line -= 1;
            self.set_cursor_col(prev_len);
        }
        self.fire_change();
    }

    fn delete_to_end_of_line(&mut self) {
        self.exit_history_browsing();
        let current_line = self.state.lines[self.state.cursor_line].clone();
        if self.state.cursor_col < utf16_len(&current_line) {
            self.push_undo_snapshot();
            let deleted_text = slice_utf16(
                &current_line,
                self.state.cursor_col,
                utf16_len(&current_line),
            )
            .to_string();
            self.kill_ring
                .push(&deleted_text, false, self.last_action == Some("kill"));
            self.last_action = Some("kill");
            self.state.lines[self.state.cursor_line] =
                slice_utf16(&current_line, 0, self.state.cursor_col).to_string();
        } else if self.state.cursor_line < self.state.lines.len() - 1 {
            self.push_undo_snapshot();
            self.kill_ring
                .push("\n", false, self.last_action == Some("kill"));
            self.last_action = Some("kill");
            let next_line = self.state.lines[self.state.cursor_line + 1].clone();
            self.state.lines[self.state.cursor_line] = current_line + &next_line;
            self.state.lines.remove(self.state.cursor_line + 1);
        }
        self.fire_change();
    }

    fn delete_word_backwards(&mut self) {
        self.exit_history_browsing();
        let current_line = self.state.lines[self.state.cursor_line].clone();
        if self.state.cursor_col == 0 {
            if self.state.cursor_line > 0 {
                self.push_undo_snapshot();
                self.kill_ring
                    .push("\n", true, self.last_action == Some("kill"));
                self.last_action = Some("kill");
                let previous_line = self.state.lines[self.state.cursor_line - 1].clone();
                let prev_len = utf16_len(&previous_line);
                self.state.lines[self.state.cursor_line - 1] = previous_line + &current_line;
                self.state.lines.remove(self.state.cursor_line);
                self.state.cursor_line -= 1;
                self.set_cursor_col(prev_len);
            }
        } else {
            self.push_undo_snapshot();
            let was_kill = self.last_action == Some("kill");
            let old_cursor_col = self.state.cursor_col;
            self.move_word_backwards();
            let delete_from = self.state.cursor_col;
            self.set_cursor_col(old_cursor_col);
            let deleted_text =
                slice_utf16(&current_line, delete_from, self.state.cursor_col).to_string();
            self.kill_ring.push(&deleted_text, true, was_kill);
            self.last_action = Some("kill");
            self.state.lines[self.state.cursor_line] = slice_utf16(&current_line, 0, delete_from)
                .to_string()
                + slice_utf16(
                    &current_line,
                    self.state.cursor_col,
                    utf16_len(&current_line),
                );
            self.set_cursor_col(delete_from);
        }
        self.fire_change();
    }

    fn delete_word_forward(&mut self) {
        self.exit_history_browsing();
        let current_line = self.state.lines[self.state.cursor_line].clone();
        let line_len = utf16_len(&current_line);
        if self.state.cursor_col >= line_len {
            if self.state.cursor_line < self.state.lines.len() - 1 {
                self.push_undo_snapshot();
                self.kill_ring
                    .push("\n", false, self.last_action == Some("kill"));
                self.last_action = Some("kill");
                let next_line = self.state.lines[self.state.cursor_line + 1].clone();
                self.state.lines[self.state.cursor_line] = current_line + &next_line;
                self.state.lines.remove(self.state.cursor_line + 1);
            }
        } else {
            self.push_undo_snapshot();
            let was_kill = self.last_action == Some("kill");
            let old_cursor_col = self.state.cursor_col;
            self.move_word_forwards();
            let delete_to = self.state.cursor_col;
            self.set_cursor_col(old_cursor_col);
            let deleted_text =
                slice_utf16(&current_line, self.state.cursor_col, delete_to).to_string();
            self.kill_ring.push(&deleted_text, false, was_kill);
            self.last_action = Some("kill");
            self.state.lines[self.state.cursor_line] =
                slice_utf16(&current_line, 0, self.state.cursor_col).to_string()
                    + slice_utf16(&current_line, delete_to, utf16_len(&current_line));
        }
        self.fire_change();
    }

    fn handle_forward_delete(&mut self) {
        self.exit_history_browsing();
        self.last_action = None;
        let current_line = self.state.lines[self.state.cursor_line].clone();
        if self.state.cursor_col < utf16_len(&current_line) {
            self.push_undo_snapshot();
            let after_cursor = slice_utf16(
                &current_line,
                self.state.cursor_col,
                utf16_len(&current_line),
            );
            let graphemes = self.segment(after_cursor, "grapheme");
            let first_grapheme = graphemes.first();
            let grapheme_length = first_grapheme.map(|g| utf16_len(&g.text)).unwrap_or(1);
            let before = slice_utf16(&current_line, 0, self.state.cursor_col).to_string();
            let after = slice_utf16(
                &current_line,
                self.state.cursor_col + grapheme_length,
                utf16_len(&current_line),
            )
            .to_string();
            self.state.lines[self.state.cursor_line] = before + &after;
        } else if self.state.cursor_line < self.state.lines.len() - 1 {
            self.push_undo_snapshot();
            let next_line = self.state.lines[self.state.cursor_line + 1].clone();
            self.state.lines[self.state.cursor_line] = current_line + &next_line;
            self.state.lines.remove(self.state.cursor_line + 1);
        }
        self.fire_change();
        if self.autocomplete_state.is_some() {
            self.update_autocomplete();
        } else {
            let current_line = self.state.lines[self.state.cursor_line].clone();
            let text_before_cursor = slice_utf16(&current_line, 0, self.state.cursor_col);
            if self.is_in_slash_command_context(text_before_cursor)
                || self.autocomplete_trigger_pattern_matches(text_before_cursor)
            {
                self.try_trigger_autocomplete(false);
            }
        }
    }

    fn build_visual_line_map(&self, width: usize) -> Vec<VisualLine> {
        let mut visual_lines: Vec<VisualLine> = Vec::new();
        for (i, line) in self.state.lines.iter().enumerate() {
            let line_vis_width = visible_width(line);
            if line.is_empty() {
                visual_lines.push(VisualLine {
                    logical_line: i,
                    start_col: 0,
                    length: 0,
                });
            } else if line_vis_width <= width {
                visual_lines.push(VisualLine {
                    logical_line: i,
                    start_col: 0,
                    length: utf16_len(line),
                });
            } else {
                let chunks = word_wrap_line(line, width, Some(&self.segment(line, "grapheme")));
                for chunk in &chunks {
                    visual_lines.push(VisualLine {
                        logical_line: i,
                        start_col: chunk.start_index,
                        length: chunk.end_index - chunk.start_index,
                    });
                }
            }
        }
        visual_lines
    }

    fn find_visual_line_at(&self, visual_lines: &[VisualLine], line: usize, col: usize) -> usize {
        for (i, vl) in visual_lines.iter().enumerate() {
            if vl.logical_line != line {
                continue;
            }
            let offset = col.saturating_sub(vl.start_col);
            let is_last_segment_of_line =
                i == visual_lines.len() - 1 || visual_lines[i + 1].logical_line != vl.logical_line;
            if offset < vl.length || (is_last_segment_of_line && offset == vl.length) {
                return i;
            }
        }
        visual_lines.len() - 1
    }

    fn find_current_visual_line(&self, visual_lines: &[VisualLine]) -> usize {
        self.find_visual_line_at(visual_lines, self.state.cursor_line, self.state.cursor_col)
    }

    fn move_cursor(&mut self, delta_line: i8, delta_col: i8) {
        self.last_action = None;
        let visual_lines = self.build_visual_line_map(self.last_width);
        let current_visual_line = self.find_current_visual_line(&visual_lines);

        if delta_line != 0 {
            let target = current_visual_line as isize + delta_line as isize;
            if target >= 0 && (target as usize) < visual_lines.len() {
                self.move_to_visual_line(&visual_lines, current_visual_line, target as usize);
            }
        }

        if delta_col != 0 {
            let current_line = self.state.lines[self.state.cursor_line].clone();
            let line_len = utf16_len(&current_line);
            if delta_col > 0 {
                if self.state.cursor_col < line_len {
                    let after_cursor = slice_utf16(&current_line, self.state.cursor_col, line_len);
                    let graphemes = self.segment(after_cursor, "grapheme");
                    let first_grapheme = graphemes.first();
                    self.set_cursor_col(
                        self.state.cursor_col
                            + first_grapheme.map(|g| utf16_len(&g.text)).unwrap_or(1),
                    );
                } else if self.state.cursor_line < self.state.lines.len() - 1 {
                    self.state.cursor_line += 1;
                    self.set_cursor_col(0);
                } else {
                    let current_vl = &visual_lines[current_visual_line];
                    self.preferred_visual_col =
                        Some(self.state.cursor_col.saturating_sub(current_vl.start_col));
                }
            } else {
                if self.state.cursor_col > 0 {
                    let before_cursor = slice_utf16(&current_line, 0, self.state.cursor_col);
                    let graphemes = self.segment(before_cursor, "grapheme");
                    let last_grapheme = graphemes.last();
                    self.set_cursor_col(
                        self.state.cursor_col
                            - last_grapheme.map(|g| utf16_len(&g.text)).unwrap_or(1),
                    );
                } else if self.state.cursor_line > 0 {
                    self.state.cursor_line -= 1;
                    let prev_line = self.state.lines[self.state.cursor_line].clone();
                    self.set_cursor_col(utf16_len(&prev_line));
                }
            }
        }

        if self.autocomplete_state.is_some() {
            self.update_autocomplete();
        }
    }

    fn page_scroll(&mut self, direction: i8) {
        self.last_action = None;
        let terminal_rows = self.terminal_rows();
        let page_size = std::cmp::max(5, (terminal_rows as f64 * 0.3).floor() as usize);
        let visual_lines = self.build_visual_line_map(self.last_width);
        let current_visual_line = self.find_current_visual_line(&visual_lines);
        let target = (current_visual_line as isize + direction as isize * page_size as isize)
            .clamp(0, visual_lines.len().saturating_sub(1) as isize) as usize;
        self.move_to_visual_line(&visual_lines, current_visual_line, target);
    }

    fn move_word_backwards(&mut self) {
        self.last_action = None;
        let current_line = self.state.lines[self.state.cursor_line].clone();
        if self.state.cursor_col == 0 {
            if self.state.cursor_line > 0 {
                self.state.cursor_line -= 1;
                let prev_line = self.state.lines[self.state.cursor_line].clone();
                self.set_cursor_col(utf16_len(&prev_line));
            }
            return;
        }
        let options = WordNavigationOptions {
            segment: None,
            is_atomic_segment: Some(&is_paste_marker),
        };
        let col = find_word_backward(&current_line, self.state.cursor_col, Some(&options));
        self.set_cursor_col(col);
    }

    fn yank(&mut self) {
        if self.kill_ring.is_empty() {
            return;
        }
        self.push_undo_snapshot();
        let text = self.kill_ring.peek().unwrap().to_string();
        self.insert_yanked_text(&text);
        self.last_action = Some("yank");
    }

    fn yank_pop(&mut self) {
        if self.last_action != Some("yank") || self.kill_ring.len() <= 1 {
            return;
        }
        self.push_undo_snapshot();
        self.delete_yanked_text();
        self.kill_ring.rotate();
        let text = self.kill_ring.peek().unwrap().to_string();
        self.insert_yanked_text(&text);
        self.last_action = Some("yank");
    }

    fn insert_yanked_text(&mut self, text: &str) {
        self.exit_history_browsing();
        let lines: Vec<&str> = text.split('\n').collect();
        if lines.len() == 1 {
            let current_line = self.state.lines[self.state.cursor_line].clone();
            let before = slice_utf16(&current_line, 0, self.state.cursor_col).to_string();
            let after = slice_utf16(
                &current_line,
                self.state.cursor_col,
                utf16_len(&current_line),
            )
            .to_string();
            self.state.lines[self.state.cursor_line] = before + text + &after;
            self.set_cursor_col(self.state.cursor_col + utf16_len(text));
        } else {
            let current_line = self.state.lines[self.state.cursor_line].clone();
            let before = slice_utf16(&current_line, 0, self.state.cursor_col).to_string();
            let after = slice_utf16(
                &current_line,
                self.state.cursor_col,
                utf16_len(&current_line),
            )
            .to_string();
            self.state.lines[self.state.cursor_line] = before + lines[0];
            for (i, mid) in lines[1..lines.len() - 1].iter().enumerate() {
                self.state
                    .lines
                    .insert(self.state.cursor_line + i + 1, mid.to_string());
            }
            let last_line_index = self.state.cursor_line + lines.len() - 1;
            self.state
                .lines
                .insert(last_line_index, lines[lines.len() - 1].to_string() + &after);
            self.state.cursor_line = last_line_index;
            self.set_cursor_col(utf16_len(lines[lines.len() - 1]));
        }
        self.fire_change();
    }

    fn delete_yanked_text(&mut self) {
        let Some(yanked_text) = self.kill_ring.peek().map(str::to_string) else {
            return;
        };
        let yank_lines: Vec<&str> = yanked_text.split('\n').collect();
        if yank_lines.len() == 1 {
            let current_line = self.state.lines[self.state.cursor_line].clone();
            let delete_len = utf16_len(&yanked_text);
            let before = slice_utf16(
                &current_line,
                0,
                self.state.cursor_col.saturating_sub(delete_len),
            )
            .to_string();
            let after = slice_utf16(
                &current_line,
                self.state.cursor_col,
                utf16_len(&current_line),
            )
            .to_string();
            self.state.lines[self.state.cursor_line] = before + &after;
            self.set_cursor_col(self.state.cursor_col - delete_len);
        } else {
            let start_line = self.state.cursor_line - (yank_lines.len() - 1);
            let start_col =
                utf16_len(&self.state.lines[start_line]).saturating_sub(utf16_len(yank_lines[0]));
            let after_cursor = slice_utf16(
                &self.state.lines[self.state.cursor_line],
                self.state.cursor_col,
                utf16_len(&self.state.lines[self.state.cursor_line]),
            )
            .to_string();
            let before_yank = slice_utf16(&self.state.lines[start_line], 0, start_col).to_string();
            self.state.lines.splice(
                start_line..start_line + yank_lines.len(),
                [before_yank + &after_cursor],
            );
            self.state.cursor_line = start_line;
            self.set_cursor_col(start_col);
        }
        self.fire_change();
    }

    fn push_undo_snapshot(&mut self) {
        self.undo_stack.push(&EditorSnapshot {
            state: self.state.clone(),
            pastes: self.pastes.clone(),
            paste_counter: self.paste_counter,
        });
    }

    fn undo(&mut self) {
        self.exit_history_browsing();
        let Some(snapshot) = self.undo_stack.pop() else {
            return;
        };
        self.state = snapshot.state;
        self.pastes = snapshot.pastes;
        self.paste_counter = snapshot.paste_counter;
        self.last_action = None;
        self.preferred_visual_col = None;
        self.fire_change();
    }

    fn jump_to_char(&mut self, ch: char, direction: &str) {
        self.last_action = None;
        let is_forward = direction == "forward";
        let lines = self.state.lines.clone();
        let end: isize = if is_forward { lines.len() as isize } else { -1 };
        let step: isize = if is_forward { 1 } else { -1 };
        let mut line_idx = self.state.cursor_line as isize;
        while line_idx != end {
            let line = &lines[line_idx as usize];
            let is_current_line = line_idx as usize == self.state.cursor_line;
            let search_from: Option<usize> = if is_current_line {
                if is_forward {
                    Some(self.state.cursor_col + 1)
                } else {
                    Some(self.state.cursor_col.saturating_sub(1))
                }
            } else {
                None
            };
            let idx = if is_forward {
                if let Some(from) = search_from {
                    utf16_char_index(line, ch, from)
                } else {
                    utf16_char_index(line, ch, 0)
                }
            } else {
                utf16_last_char_index(line, ch, search_from)
            };
            if let Some(idx) = idx {
                self.state.cursor_line = line_idx as usize;
                self.set_cursor_col(idx);
                return;
            }
            line_idx += step;
        }
    }

    fn move_word_forwards(&mut self) {
        self.last_action = None;
        let current_line = self.state.lines[self.state.cursor_line].clone();
        if self.state.cursor_col >= utf16_len(&current_line) {
            if self.state.cursor_line < self.state.lines.len() - 1 {
                self.state.cursor_line += 1;
                self.set_cursor_col(0);
            }
            return;
        }
        let options = WordNavigationOptions {
            segment: None,
            is_atomic_segment: Some(&is_paste_marker),
        };
        let col = find_word_forward(&current_line, self.state.cursor_col, Some(&options));
        self.set_cursor_col(col);
    }

    fn is_slash_menu_allowed(&self) -> bool {
        self.state.cursor_line == 0
    }

    fn is_at_start_of_message(&self) -> bool {
        if !self.is_slash_menu_allowed() {
            return false;
        }
        let current_line = self.state.lines[self.state.cursor_line].clone();
        let before_cursor = slice_utf16(&current_line, 0, self.state.cursor_col);
        let trimmed = before_cursor.trim();
        trimmed.is_empty() || trimmed == "/"
    }

    fn is_in_slash_command_context(&self, text_before_cursor: &str) -> bool {
        self.is_slash_menu_allowed() && text_before_cursor.trim_start().starts_with('/')
    }

    fn get_best_autocomplete_match_index(&self, items: &[SelectItem], prefix: &str) -> isize {
        if prefix.is_empty() {
            return -1;
        }
        let mut first_prefix_index = -1isize;
        for (i, item) in items.iter().enumerate() {
            if item.value == prefix {
                return i as isize;
            }
            if first_prefix_index == -1 && item.value.starts_with(prefix) {
                first_prefix_index = i as isize;
            }
        }
        first_prefix_index
    }

    fn create_autocomplete_list(&self, prefix: &str, items: Vec<SelectItem>) -> SelectList {
        let layout = if prefix.starts_with('/') {
            SLASH_COMMAND_SELECT_LIST_LAYOUT
        } else {
            SelectListLayoutOptions::default()
        };
        let theme = SelectListTheme {
            selected_prefix: Box::new(|s| s.to_string()),
            selected_text: Box::new(|s| s.to_string()),
            description: Box::new(|s| s.to_string()),
            scroll_info: Box::new(|s| s.to_string()),
            no_match: Box::new(|s| s.to_string()),
        };
        SelectList::new(items, self.autocomplete_max_visible, theme, layout)
    }

    fn try_trigger_autocomplete(&mut self, explicit_tab: bool) {
        self.request_autocomplete(false, explicit_tab);
    }

    fn handle_tab_completion(&mut self) {
        if self.autocomplete_provider.is_none() {
            return;
        }
        let current_line = self.state.lines[self.state.cursor_line].clone();
        let before_cursor = slice_utf16(&current_line, 0, self.state.cursor_col);
        if self.is_in_slash_command_context(before_cursor)
            && !before_cursor.trim_start().contains(' ')
        {
            self.request_autocomplete(false, true);
        } else {
            self.request_autocomplete(true, true);
        }
    }

    /// `requestAutocomplete` — synchronous (see module docs). The debounce
    /// timer is elided; `get_autocomplete_debounce_ms` still computes the
    /// same value the TS would so the *trigger* decision is faithful.
    fn request_autocomplete(&mut self, force: bool, explicit_tab: bool) {
        if self.autocomplete_provider.is_none() {
            return;
        }
        if force {
            let should_trigger = self
                .autocomplete_provider
                .as_ref()
                .map(|p| {
                    p.should_trigger_file_completion(&crate::autocomplete::CompletionContext {
                        lines: &self.state.lines,
                        cursor_line: self.state.cursor_line,
                        cursor_col: self.state.cursor_col,
                    })
                })
                .unwrap_or(true);
            if !should_trigger {
                return;
            }
        }
        let _debounce_ms = self.get_autocomplete_debounce_ms(force, explicit_tab);
        // Run the provider synchronously.
        self.run_autocomplete_request(force, explicit_tab);
    }

    fn get_autocomplete_debounce_ms(&self, force: bool, explicit_tab: bool) -> usize {
        if explicit_tab || force {
            return 0;
        }
        let current_line = self.state.lines[self.state.cursor_line].clone();
        let text_before_cursor = slice_utf16(&current_line, 0, self.state.cursor_col);
        if self.autocomplete_debounce_pattern_matches(text_before_cursor) {
            ATTACHMENT_AUTOCOMPLETE_DEBOUNCE_MS
        } else {
            0
        }
    }

    fn run_autocomplete_request(&mut self, force: bool, explicit_tab: bool) {
        // Compute everything through the provider first (dropping the borrow)
        // so we can mutate `self` (cancel/undo) afterwards.
        let (suggestions, single_result) = {
            let Some(provider) = &mut self.autocomplete_provider else {
                return;
            };
            let ctx = crate::autocomplete::CompletionContext {
                lines: &self.state.lines,
                cursor_line: self.state.cursor_line,
                cursor_col: self.state.cursor_col,
            };
            let suggestions = match provider.get_suggestions(&ctx, force) {
                Some(s) if !s.items.is_empty() => s,
                _ => return,
            };
            if force && explicit_tab && suggestions.items.len() == 1 {
                let item = suggestions.items[0].clone();
                let result = provider.apply_completion(&ctx, &item, &suggestions.prefix);
                (None, Some(result))
            } else {
                (Some(suggestions), None)
            }
        };
        if let Some(result) = single_result {
            self.push_undo_snapshot();
            self.last_action = None;
            self.state.lines = result.lines;
            self.state.cursor_line = result.cursor_line;
            self.set_cursor_col(result.cursor_col);
            self.fire_change();
            return;
        }
        if let Some(suggestions) = suggestions {
            self.apply_autocomplete_suggestions(
                suggestions,
                if force { "force" } else { "regular" },
            );
        } else {
            self.cancel_autocomplete();
        }
    }

    fn apply_autocomplete_suggestions(
        &mut self,
        suggestions: AutocompleteSuggestions,
        state: &str,
    ) {
        let items: Vec<SelectItem> = suggestions
            .items
            .iter()
            .map(|it| SelectItem {
                value: it.value.clone(),
                label: it.label.clone(),
                description: it.description.clone(),
            })
            .collect();
        self.autocomplete_prefix = suggestions.prefix.clone();
        let best = self.get_best_autocomplete_match_index(&items, &suggestions.prefix);
        self.autocomplete_list = Some(self.create_autocomplete_list(&suggestions.prefix, items));
        if best >= 0 {
            if let Some(list) = &mut self.autocomplete_list {
                list.set_selected_index(best as usize);
            }
        }
        self.autocomplete_state = Some(if state == "force" { "force" } else { "regular" });
    }

    fn cancel_autocomplete_request(&mut self) {
        // Synchronous: no timers or AbortControllers to cancel.
    }

    fn clear_autocomplete_ui(&mut self) {
        self.autocomplete_state = None;
        self.autocomplete_list = None;
        self.autocomplete_prefix.clear();
    }

    fn cancel_autocomplete(&mut self) {
        self.cancel_autocomplete_request();
        self.clear_autocomplete_ui();
    }

    pub fn is_showing_autocomplete(&self) -> bool {
        self.autocomplete_state.is_some()
    }

    fn update_autocomplete(&mut self) {
        if self.autocomplete_state.is_none() || self.autocomplete_provider.is_none() {
            return;
        }
        self.request_autocomplete(self.autocomplete_state == Some("force"), false);
    }

    fn expand_paste_markers(&self, text: &str) -> String {
        let mut result = text.to_string();
        for (paste_id, paste_content) in &self.pastes {
            let pattern = format!("[paste #{paste_id} ");
            // Replace marker spans with the paste content.
            let mut out = String::with_capacity(result.len());
            let mut rest = result.as_str();
            while let Some(idx) = rest.find(&pattern) {
                out.push_str(&rest[..idx]);
                let after = &rest[idx + pattern.len()..];
                // Suffix: `+N lines` or `N chars` then `]`.
                let suffix_end = after.find(']').unwrap_or(after.len());
                let suffix = &after[..suffix_end];
                if suffix.starts_with('+') || suffix.ends_with("chars") || suffix.ends_with("lines")
                {
                    out.push_str(paste_content);
                    rest = &after[suffix_end + 1..];
                } else {
                    // Not a valid marker (missing suffix) — keep literally.
                    out.push_str(&rest[idx..idx + pattern.len()]);
                    rest = after;
                }
            }
            out.push_str(rest);
            result = out;
        }
        result
    }

    // -- Component / Focusable -------------------------------------------------

    /// `render` (editor.ts:482).
    pub fn render(&mut self, width: usize) -> Vec<String> {
        let max_padding = (width.saturating_sub(1)) / 2;
        let padding_x = self.padding_x.min(max_padding);
        let content_width = std::cmp::max(1, width - padding_x * 2);
        let layout_width = std::cmp::max(1, content_width - if padding_x != 0 { 0 } else { 1 });
        self.last_width = layout_width;

        let horizontal = (self.border_color)("─");
        let layout_lines = self.layout_text(layout_width);

        let terminal_rows = self.terminal_rows();
        let max_visible_lines = std::cmp::max(5, (terminal_rows as f64 * 0.3).floor() as usize);

        let cursor_line_index = layout_lines.iter().position(|l| l.has_cursor).unwrap_or(0);
        if cursor_line_index < self.scroll_offset {
            self.scroll_offset = cursor_line_index;
        } else if cursor_line_index >= self.scroll_offset + max_visible_lines {
            self.scroll_offset = cursor_line_index - max_visible_lines + 1;
        }
        let max_scroll_offset = layout_lines.len().saturating_sub(max_visible_lines);
        self.scroll_offset = self.scroll_offset.min(max_scroll_offset);

        let visible_lines: Vec<LayoutLine> = layout_lines
            .into_iter()
            .skip(self.scroll_offset)
            .take(max_visible_lines)
            .collect();

        let mut result: Vec<String> = Vec::new();
        let left_padding = " ".repeat(padding_x);
        let right_padding = left_padding.clone();

        if self.scroll_offset > 0 {
            let border = create_scroll_border('↑', self.scroll_offset, width);
            result.push((self.border_color)(&border));
        } else {
            result.push(horizontal.repeat(width));
        }

        let emit_cursor_marker = self.focused;

        for layout_line in &visible_lines {
            let mut display_text = layout_line.text.clone();
            let mut line_visible_width = visible_width(&layout_line.text);
            let mut cursor_in_padding = false;

            if layout_line.has_cursor {
                if let Some(cursor_pos) = layout_line.cursor_pos {
                    let before = slice_utf16(&display_text, 0, cursor_pos).to_string();
                    let after = slice_utf16(&display_text, cursor_pos, utf16_len(&display_text))
                        .to_string();
                    let marker = if emit_cursor_marker {
                        CURSOR_MARKER
                    } else {
                        ""
                    };
                    if !after.is_empty() {
                        let after_graphemes = self.segment(&after, "grapheme");
                        let first_grapheme = after_graphemes
                            .first()
                            .map(|g| g.text.clone())
                            .unwrap_or_default();
                        let rest_after =
                            slice_utf16(&after, utf16_len(&first_grapheme), utf16_len(&after))
                                .to_string();
                        let cursor = format!("\x1b[7m{first_grapheme}\x1b[0m");
                        display_text = format!("{before}{marker}{cursor}{rest_after}");
                    } else {
                        let cursor = "\x1b[7m \x1b[0m";
                        display_text = format!("{before}{marker}{cursor}");
                        line_visible_width += 1;
                        if line_visible_width > content_width && padding_x > 0 {
                            cursor_in_padding = true;
                        }
                    }
                }
            }

            let padding = " ".repeat(content_width.saturating_sub(line_visible_width));
            let line_right_padding = if cursor_in_padding {
                &right_padding[1..]
            } else {
                &right_padding
            };
            result.push(format!(
                "{left_padding}{display_text}{padding}{line_right_padding}"
            ));
        }

        // `lines_below` = total layout lines - (scroll_offset + visible lines).
        let total_layout = self.layout_text(layout_width).len();
        let lines_below = total_layout.saturating_sub(self.scroll_offset + visible_lines.len());
        if lines_below > 0 {
            let border = create_scroll_border('↓', lines_below, width);
            result.push((self.border_color)(&border));
        } else {
            result.push(horizontal.repeat(width));
        }

        if self.autocomplete_state.is_some() {
            if let Some(list) = &mut self.autocomplete_list {
                let ac_result = list.render(content_width);
                for line in ac_result {
                    let line_width = visible_width(&line);
                    let line_padding = " ".repeat(content_width.saturating_sub(line_width));
                    result.push(format!("{left_padding}{line}{line_padding}{right_padding}"));
                }
            }
        }

        result
    }

    /// `layoutText` (editor.ts:905).
    fn layout_text(&self, content_width: usize) -> Vec<LayoutLine> {
        let mut layout_lines: Vec<LayoutLine> = Vec::new();
        if self.state.lines.is_empty()
            || (self.state.lines.len() == 1 && self.state.lines[0].is_empty())
        {
            layout_lines.push(LayoutLine {
                text: String::new(),
                has_cursor: true,
                cursor_pos: Some(0),
            });
            return layout_lines;
        }
        for (i, line) in self.state.lines.iter().enumerate() {
            let is_current_line = i == self.state.cursor_line;
            let line_visible_width = visible_width(line);
            if line_visible_width <= content_width {
                if is_current_line {
                    layout_lines.push(LayoutLine {
                        text: line.clone(),
                        has_cursor: true,
                        cursor_pos: Some(self.state.cursor_col),
                    });
                } else {
                    layout_lines.push(LayoutLine {
                        text: line.clone(),
                        has_cursor: false,
                        cursor_pos: None,
                    });
                }
            } else {
                let chunks =
                    word_wrap_line(line, content_width, Some(&self.segment(line, "grapheme")));
                for (chunk_index, chunk) in chunks.iter().enumerate() {
                    let cursor_pos = self.state.cursor_col;
                    let is_last_chunk = chunk_index == chunks.len() - 1;
                    let mut has_cursor_in_chunk = false;
                    let mut adjusted_cursor_pos = 0;
                    if is_current_line {
                        if is_last_chunk {
                            has_cursor_in_chunk = cursor_pos >= chunk.start_index;
                            adjusted_cursor_pos = cursor_pos - chunk.start_index;
                        } else {
                            has_cursor_in_chunk =
                                cursor_pos >= chunk.start_index && cursor_pos < chunk.end_index;
                            if has_cursor_in_chunk {
                                adjusted_cursor_pos = cursor_pos - chunk.start_index;
                                if adjusted_cursor_pos > utf16_len(&chunk.text) {
                                    adjusted_cursor_pos = utf16_len(&chunk.text);
                                }
                            }
                        }
                    }
                    if has_cursor_in_chunk {
                        layout_lines.push(LayoutLine {
                            text: chunk.text.clone(),
                            has_cursor: true,
                            cursor_pos: Some(adjusted_cursor_pos),
                        });
                    } else {
                        layout_lines.push(LayoutLine {
                            text: chunk.text.clone(),
                            has_cursor: false,
                            cursor_pos: None,
                        });
                    }
                }
            }
        }
        layout_lines
    }

    /// `handleInput` (editor.ts:603).
    pub fn handle_input(&mut self, data: &str) {
        let kb = get_keybindings();

        // Character jump mode.
        if let Some(jump_mode) = self.jump_mode {
            if kb.matches(data, Keybinding::EditorJumpForward)
                || kb.matches(data, Keybinding::EditorJumpBackward)
            {
                self.jump_mode = None;
                return;
            }
            let printable = decode_printable_key(data).or_else(|| {
                if data.chars().next().is_some_and(|c| (c as u32) >= 32) {
                    Some(data.to_string())
                } else {
                    None
                }
            });
            if let Some(printable) = printable {
                let direction = jump_mode;
                self.jump_mode = None;
                let ch = printable.chars().next().unwrap_or('\0');
                self.jump_to_char(ch, direction);
                return;
            }
            self.jump_mode = None;
        }

        // Bracketed paste.
        if data.contains("\x1b[200~") {
            self.is_in_paste = true;
            self.paste_buffer.clear();
            let stripped = data.replacen("\x1b[200~", "", 1);
            self.paste_buffer.push_str(&stripped);
            if let Some(end_idx) = self.paste_buffer.find("\x1b[201~") {
                let paste_content = self.paste_buffer[..end_idx].to_string();
                if !paste_content.is_empty() {
                    self.handle_paste(&paste_content);
                }
                self.is_in_paste = false;
                let remaining = self.paste_buffer[end_idx + 6..].to_string();
                self.paste_buffer.clear();
                if !remaining.is_empty() {
                    self.handle_input(&remaining);
                }
                return;
            }
            return;
        }
        if self.is_in_paste {
            self.paste_buffer.push_str(data);
            if let Some(end_idx) = self.paste_buffer.find("\x1b[201~") {
                let paste_content = self.paste_buffer[..end_idx].to_string();
                if !paste_content.is_empty() {
                    self.handle_paste(&paste_content);
                }
                self.is_in_paste = false;
                let remaining = self.paste_buffer[end_idx + 6..].to_string();
                self.paste_buffer.clear();
                if !remaining.is_empty() {
                    self.handle_input(&remaining);
                }
                return;
            }
            return;
        }

        // Ctrl+C — let parent handle.
        if kb.matches(data, Keybinding::InputCopy) {
            return;
        }
        // Undo.
        if kb.matches(data, Keybinding::EditorUndo) {
            self.undo();
            return;
        }

        // Autocomplete mode.
        if self.autocomplete_state.is_some() {
            if let Some(list) = &mut self.autocomplete_list {
                if kb.matches(data, Keybinding::SelectCancel) {
                    self.cancel_autocomplete();
                    return;
                }
                if kb.matches(data, Keybinding::SelectUp)
                    || kb.matches(data, Keybinding::SelectDown)
                {
                    list.handle_input(data);
                    return;
                }
                if kb.matches(data, Keybinding::InputTab) {
                    let selected = list.get_selected_item().cloned();
                    if let Some(selected) = selected {
                        let item = crate::autocomplete::AutocompleteItem {
                            value: selected.value.clone(),
                            label: selected.label.clone(),
                            description: selected.description.clone(),
                        };
                        // Compute the completion result before mutating self.
                        let result = self.autocomplete_provider.as_ref().map(|provider| {
                            let ctx = crate::autocomplete::CompletionContext {
                                lines: &self.state.lines,
                                cursor_line: self.state.cursor_line,
                                cursor_col: self.state.cursor_col,
                            };
                            provider.apply_completion(&ctx, &item, &self.autocomplete_prefix)
                        });
                        if let Some(result) = result {
                            self.push_undo_snapshot();
                            self.last_action = None;
                            self.state.lines = result.lines;
                            self.state.cursor_line = result.cursor_line;
                            self.set_cursor_col(result.cursor_col);
                            self.cancel_autocomplete();
                            self.fire_change();
                        }
                    }
                    return;
                }
                if kb.matches(data, Keybinding::SelectConfirm) {
                    let selected = list.get_selected_item().cloned();
                    if let Some(selected) = selected {
                        let item = crate::autocomplete::AutocompleteItem {
                            value: selected.value.clone(),
                            label: selected.label.clone(),
                            description: selected.description.clone(),
                        };
                        let result = self.autocomplete_provider.as_ref().map(|provider| {
                            let ctx = crate::autocomplete::CompletionContext {
                                lines: &self.state.lines,
                                cursor_line: self.state.cursor_line,
                                cursor_col: self.state.cursor_col,
                            };
                            provider.apply_completion(&ctx, &item, &self.autocomplete_prefix)
                        });
                        if let Some(result) = result {
                            self.push_undo_snapshot();
                            self.last_action = None;
                            self.state.lines = result.lines;
                            self.state.cursor_line = result.cursor_line;
                            self.set_cursor_col(result.cursor_col);
                            let was_slash = self.autocomplete_prefix.starts_with('/');
                            self.cancel_autocomplete();
                            if !was_slash {
                                self.fire_change();
                                return;
                            }
                        }
                    }
                }
            }
        }

        // Tab — trigger completion.
        if kb.matches(data, Keybinding::InputTab) && self.autocomplete_state.is_none() {
            self.handle_tab_completion();
            return;
        }

        // Deletion actions.
        if kb.matches(data, Keybinding::EditorDeleteToLineEnd) {
            self.delete_to_end_of_line();
            return;
        }
        if kb.matches(data, Keybinding::EditorDeleteToLineStart) {
            self.delete_to_start_of_line();
            return;
        }
        if kb.matches(data, Keybinding::EditorDeleteWordBackward) {
            self.delete_word_backwards();
            return;
        }
        if kb.matches(data, Keybinding::EditorDeleteWordForward) {
            self.delete_word_forward();
            return;
        }
        if kb.matches(data, Keybinding::EditorDeleteCharBackward)
            || matches_key(data, "shift+backspace")
        {
            self.handle_backspace();
            return;
        }
        if kb.matches(data, Keybinding::EditorDeleteCharForward)
            || matches_key(data, "shift+delete")
        {
            self.handle_forward_delete();
            return;
        }

        // Kill ring.
        if kb.matches(data, Keybinding::EditorYank) {
            self.yank();
            return;
        }
        if kb.matches(data, Keybinding::EditorYankPop) {
            self.yank_pop();
            return;
        }

        // Dedicated history.
        if kb.matches(data, Keybinding::EditorHistoryPrevious) {
            self.cancel_autocomplete();
            self.navigate_history(-1);
            return;
        }
        if kb.matches(data, Keybinding::EditorHistoryNext) {
            self.cancel_autocomplete();
            self.navigate_history(1);
            return;
        }

        // Cursor movement.
        if kb.matches(data, Keybinding::EditorCursorLineStart) {
            self.move_to_line_start();
            return;
        }
        if kb.matches(data, Keybinding::EditorCursorLineEnd) {
            self.move_to_line_end();
            return;
        }
        if kb.matches(data, Keybinding::EditorCursorWordLeft) {
            self.move_word_backwards();
            return;
        }
        if kb.matches(data, Keybinding::EditorCursorWordRight) {
            self.move_word_forwards();
            return;
        }

        // New line.
        let is_newline = kb.matches(data, Keybinding::InputNewLine)
            || (data.chars().next().is_some_and(|c| c as u32 == 10) && data.len() > 1)
            || data == "\x1b\r"
            || data == "\x1b[13;2~"
            || (data.len() > 1 && data.contains('\x1b') && data.contains('\r'))
            || (data == "\n" && data.len() == 1);
        if is_newline {
            if self.should_submit_on_backslash_enter(data) {
                self.handle_backspace();
                self.submit_value();
                return;
            }
            self.add_new_line();
            return;
        }

        // Submit.
        if kb.matches(data, Keybinding::InputSubmit) {
            if self.disable_submit {
                return;
            }
            let current_line = self.state.lines[self.state.cursor_line].clone();
            if self.state.cursor_col > 0
                && slice_utf16(
                    &current_line,
                    self.state.cursor_col - 1,
                    self.state.cursor_col,
                ) == "\\"
            {
                self.handle_backspace();
                self.add_new_line();
                return;
            }
            self.submit_value();
            return;
        }

        // Arrow keys with history.
        if kb.matches(data, Keybinding::EditorCursorUp) {
            if self.is_on_first_visual_line()
                && (self.is_editor_empty() || self.history_index > -1 || self.state.cursor_col == 0)
            {
                self.navigate_history(-1);
            } else if self.is_on_first_visual_line() {
                self.move_to_line_start();
            } else {
                self.move_cursor(-1, 0);
            }
            return;
        }
        if kb.matches(data, Keybinding::EditorCursorDown) {
            if self.history_index > -1 && self.is_on_last_visual_line() {
                self.navigate_history(1);
            } else if self.is_on_last_visual_line() {
                self.move_to_line_end();
            } else {
                self.move_cursor(1, 0);
            }
            return;
        }
        if kb.matches(data, Keybinding::EditorCursorRight) {
            self.move_cursor(0, 1);
            return;
        }
        if kb.matches(data, Keybinding::EditorCursorLeft) {
            self.move_cursor(0, -1);
            return;
        }

        // Page up/down.
        if kb.matches(data, Keybinding::EditorPageUp) {
            self.page_scroll(-1);
            return;
        }
        if kb.matches(data, Keybinding::EditorPageDown) {
            self.page_scroll(1);
            return;
        }

        // Character jump triggers.
        if kb.matches(data, Keybinding::EditorJumpForward) {
            self.jump_mode = Some("forward");
            return;
        }
        if kb.matches(data, Keybinding::EditorJumpBackward) {
            self.jump_mode = Some("backward");
            return;
        }

        // Shift+Space.
        if matches_key(data, "shift+space") {
            self.insert_character(" ", None);
            return;
        }

        if let Some(printable) = decode_printable_key(data) {
            self.insert_character(&printable, None);
            return;
        }

        if data.chars().next().is_some_and(|c| (c as u32) >= 32) {
            self.insert_character(data, None);
        }
    }
}

/// `VisualLine` — an element of `buildVisualLineMap`.
#[derive(Debug, Clone, Copy)]
struct VisualLine {
    logical_line: usize,
    start_col: usize,
    length: usize,
}

/// `segmentWithMarkers` (editor.ts:39) — grapheme/word segmentation with
/// paste-marker awareness.
fn segment_with_markers(text: &str, mode: &str, valid_ids: &[usize]) -> Vec<GraphemeSeg> {
    if valid_ids.is_empty() || !text.contains("[paste #") {
        return if mode == "word" {
            segment_words(text)
        } else {
            segment_graphemes(text)
        };
    }
    // Find marker spans with valid IDs.
    let mut markers: Vec<(usize, usize)> = Vec::new();
    let mut search_from = 0usize;
    while let Some(rel) = text[search_from..].find("[paste #") {
        let start = search_from + rel;
        let after = &text[start + 8..];
        let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
        if digits.is_empty() {
            search_from = start + 8;
            continue;
        }
        let id: usize = digits.parse().unwrap_or(0);
        let after_digits = &after[digits.len()..];
        // Optional suffix.
        let mut end = start + 8 + digits.len();
        if after_digits.starts_with(' ') {
            if let Some(bracket) = after_digits.find(']') {
                end = start + 8 + digits.len() + bracket + 1;
            }
        } else if after_digits.starts_with(']') {
            end = start + 8 + digits.len() + 1;
        }
        if valid_ids.contains(&id) {
            markers.push((start, end));
        }
        search_from = end.max(start + 8);
    }
    if markers.is_empty() {
        return if mode == "word" {
            segment_words(text)
        } else {
            segment_graphemes(text)
        };
    }
    let base = if mode == "word" {
        segment_words(text)
    } else {
        segment_graphemes(text)
    };
    let mut result: Vec<GraphemeSeg> = Vec::new();
    let mut marker_idx = 0usize;
    for seg in base {
        while marker_idx < markers.len() && markers[marker_idx].1 <= seg.index {
            marker_idx += 1;
        }
        let marker = markers.get(marker_idx).copied();
        if let Some((mstart, mend)) = marker {
            if seg.index >= mstart && seg.index < mend {
                if seg.index == mstart {
                    result.push(GraphemeSeg {
                        text: text[mstart..mend].to_string(),
                        index: mstart,
                    });
                }
                continue;
            }
        }
        result.push(seg);
    }
    result
}

/// Word segmentation (UTF-16 indexed) — the `Intl.Segmenter` word mode.
fn segment_words(text: &str) -> Vec<GraphemeSeg> {
    let mut out = Vec::new();
    let mut utf16_pos = 0usize;
    for w in text.split_word_bounds() {
        out.push(GraphemeSeg {
            text: w.to_string(),
            index: utf16_pos,
        });
        utf16_pos += utf16_len(w);
    }
    out
}

/// `decodeCsiUControls` — decode `\x1b[<cp>;5u` tmux re-encodings.
fn decode_csi_u_controls(text: &str) -> String {
    let mut out = String::new();
    let bytes = text.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
            // Find `;5u`.
            if let Some(semi) = bytes[i + 2..].iter().position(|b| *b == b';') {
                let num_start = i + 2;
                let num_end = i + 2 + semi;
                let after_semi = i + 2 + semi + 1;
                if bytes.get(after_semi) == Some(&b'5') && bytes.get(after_semi + 1) == Some(&b'u')
                {
                    let cp: usize = std::str::from_utf8(&bytes[num_start..num_end])
                        .ok()
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(0);
                    if (97..=122).contains(&cp) {
                        out.push(char::from_u32((cp - 96) as u32).unwrap_or('?'));
                        i = after_semi + 2;
                        continue;
                    }
                    if (65..=90).contains(&cp) {
                        out.push(char::from_u32((cp - 64) as u32).unwrap_or('?'));
                        i = after_semi + 2;
                        continue;
                    }
                }
            }
        }
        // Copy one UTF-8 char.
        let ch_len = utf8_char_len(bytes[i]);
        let ch = std::str::from_utf8(&bytes[i..i + ch_len]).unwrap_or("?");
        out.push_str(ch);
        i += ch_len;
    }
    out
}

fn utf8_char_len(b: u8) -> usize {
    if b < 0x80 {
        1
    } else if b >> 5 == 0b110 {
        2
    } else if b >> 4 == 0b1110 {
        3
    } else if b >> 3 == 0b11110 {
        4
    } else {
        1
    }
}

/// `renumberPasteMarkers` — shift marker ids > `removed` down by one.
fn renumber_paste_markers(line: &str, removed: usize) -> String {
    let mut out = String::new();
    let mut rest = line;
    while let Some(idx) = rest.find("[paste #") {
        out.push_str(&rest[..idx]);
        let after = &rest[idx + 8..];
        let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
        if digits.is_empty() {
            out.push_str(&rest[idx..idx + 8]);
            rest = after;
            continue;
        }
        let id: usize = digits.parse().unwrap_or(0);
        let after_digits = &after[digits.len()..];
        let mut end = idx + 8 + digits.len();
        if after_digits.starts_with(' ') {
            if let Some(bracket) = after_digits.find(']') {
                end = idx + 8 + digits.len() + bracket + 1;
            }
        } else if after_digits.starts_with(']') {
            end = idx + 8 + digits.len() + 1;
        }
        if id > removed {
            out.push_str(&format!("[paste #{}", id - 1));
            out.push_str(&rest[idx + 8 + digits.len()..end]);
        } else {
            out.push_str(&rest[idx..end]);
        }
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

/// UTF-16 index of the first `ch` at/after `from`.
fn utf16_char_index(s: &str, ch: char, from: usize) -> Option<usize> {
    let byte_from = utf16_to_byte_index(s, from);
    s[byte_from..]
        .find(ch)
        .map(|rel| from + utf16_len(&s[byte_from..byte_from + rel]))
}

/// UTF-16 index of the last `ch` at/before `from` (from = UTF-16 offset).
fn utf16_last_char_index(s: &str, ch: char, from: Option<usize>) -> Option<usize> {
    let byte_from = from.map(|f| utf16_to_byte_index(s, f)).unwrap_or(s.len());
    let head = &s[..byte_from];
    let rel_byte = head.rfind(ch)?;
    Some(utf16_len(&head[..rel_byte]))
}

impl Component for Editor {
    fn render(&mut self, width: usize) -> Vec<String> {
        Editor::render(self, width)
    }
    fn handle_input(&mut self, data: &str) {
        Editor::handle_input(self, data);
    }
    fn invalidate(&mut self) {
        Editor::invalidate(self);
    }
    fn as_focusable_mut(&mut self) -> Option<&mut dyn Focusable> {
        Some(self)
    }
}

impl Focusable for Editor {
    fn is_focused(&self) -> bool {
        self.focused
    }
    fn set_focused(&mut self, value: bool) {
        self.focused = value;
    }
}

impl crate::editor_component::EditorComponent for Editor {
    fn get_text(&self) -> String {
        Editor::get_text(self)
    }
    fn set_text(&mut self, text: &str) {
        Editor::set_text(self, text);
    }
    fn set_on_submit(&mut self, callback: Option<OnTextFnMut>) {
        self.on_submit = callback;
    }
    fn set_on_change(&mut self, callback: Option<OnTextFnMut>) {
        self.on_change = callback;
    }
    fn add_to_history(&mut self, text: &str) {
        Editor::add_to_history(self, text);
    }
    fn insert_text_at_cursor(&mut self, text: &str) {
        Editor::insert_text_at_cursor(self, text);
    }
    fn get_expanded_text(&self) -> String {
        Editor::get_expanded_text(self)
    }
    fn set_autocomplete_provider(&mut self, provider: Option<Box<dyn AutocompleteProvider>>) {
        Editor::set_autocomplete_provider(self, provider);
    }
    fn set_padding_x(&mut self, padding: usize) {
        Editor::set_padding_x(self, padding);
    }
    fn set_autocomplete_max_visible(&mut self, max_visible: usize) {
        Editor::set_autocomplete_max_visible(self, max_visible);
    }
}
