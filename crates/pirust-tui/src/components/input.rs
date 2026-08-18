//! Port of `packages/tui/src/components/input.ts` — a single-line focusable
//! text input with horizontal scrolling, Emacs-style kill/yank, and undo.
//! See `docs/analysis/05-tui.md` §6/§9.
//!
//! ## Scope decisions (documented, not silent — AGENTS.md Correctness Bar)
//!
//! - **UTF-16 code-unit cursor offsets — same family as `word_navigation.rs`
//!   (Wave 3).** `this.value`/`this.cursor` in the TS are JS-string/UTF-16
//!   based throughout (`.slice()`, `.length`, grapheme walks over
//!   substrings). This port keeps `value: String` (UTF-8, like every other
//!   Rust string) but does all cursor *arithmetic* in UTF-16-unit space via
//!   the same `encode_utf16`-based technique `word_navigation.rs` already
//!   uses, so [`find_word_backward`]/[`find_word_forward`] can be called
//!   directly with zero adaptation (they already expect/return UTF-16
//!   offsets). [`utf16_to_byte_index`] converts a UTF-16 offset to the UTF-8
//!   byte index needed for `String` slicing/splicing.
//! - **Optional TS callback properties → `Option<Box<dyn FnMut(...)>>`.**
//!   Same idiom `CancellableLoader::on_abort` (this wave) established;
//!   `on_submit`/`on_escape` here follow it for consistency.

use unicode_segmentation::UnicodeSegmentation;

use crate::keybindings::{get_keybindings, Keybinding};
use crate::keys::decode_kitty_printable;
use crate::kill_ring::KillRing;
use crate::tui::{Component, Focusable, CURSOR_MARKER};
use crate::undo_stack::UndoStack;
use crate::utils::{slice_by_column, visible_width};
use crate::word_navigation::{find_word_backward, find_word_forward};

#[derive(Debug, Clone, PartialEq)]
struct InputState {
    value: String,
    cursor: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LastAction {
    Kill,
    Yank,
    TypeWord,
}

/// UTF-16 code-unit length of a `&str` — `string.length` in the TS.
fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// Convert a UTF-16 code-unit offset into `s` to a UTF-8 byte index, for
/// `String` slicing — see module docs.
fn utf16_to_byte_index(s: &str, utf16_idx: usize) -> usize {
    let mut utf16_pos = 0usize;
    for (byte_idx, ch) in s.char_indices() {
        if utf16_pos >= utf16_idx {
            return byte_idx;
        }
        utf16_pos += ch.len_utf16();
    }
    s.len()
}

fn slice_utf16(s: &str, start: usize, end: usize) -> &str {
    let start_b = utf16_to_byte_index(s, start);
    let end_b = utf16_to_byte_index(s, end.max(start));
    &s[start_b..end_b]
}

/// The grapheme cluster (and its UTF-16 length) ending at `cursor`, if any.
fn grapheme_before(value: &str, cursor: usize) -> Option<(String, usize)> {
    let before = slice_utf16(value, 0, cursor);
    let g = before.graphemes(true).next_back()?;
    Some((g.to_string(), utf16_len(g)))
}

/// The grapheme cluster (and its UTF-16 length) starting at `cursor`, if any.
fn grapheme_after(value: &str, cursor: usize) -> Option<(String, usize)> {
    let after = slice_utf16(value, cursor, utf16_len(value));
    let g = after.graphemes(true).next()?;
    Some((g.to_string(), utf16_len(g)))
}

/// A single-`&str`-argument `FnMut` callback (`(value: string) => void`),
/// factored into a `type` alias per `clippy::type_complexity`.
pub type OnTextFnMut = Box<dyn FnMut(&str)>;

fn has_control_chars(data: &str) -> bool {
    data.chars().any(|ch| {
        let code = ch as u32;
        code < 32 || code == 0x7f || (0x80..=0x9f).contains(&code)
    })
}

/// `Input` (input.ts:19).
pub struct Input {
    value: String,
    cursor: usize,
    pub on_submit: Option<OnTextFnMut>,
    pub on_escape: Option<Box<dyn FnMut()>>,
    focused: bool,
    paste_buffer: String,
    is_in_paste: bool,
    kill_ring: KillRing,
    last_action: Option<LastAction>,
    undo_stack: UndoStack<InputState>,
}

impl Default for Input {
    fn default() -> Self {
        Self::new()
    }
}

impl Input {
    pub fn new() -> Self {
        Self {
            value: String::new(),
            cursor: 0,
            on_submit: None,
            on_escape: None,
            focused: false,
            paste_buffer: String::new(),
            is_in_paste: false,
            kill_ring: KillRing::new(),
            last_action: None,
            undo_stack: UndoStack::new(),
        }
    }

    /// `getValue` (input.ts:39).
    pub fn get_value(&self) -> &str {
        &self.value
    }

    /// `setValue` (input.ts:43).
    pub fn set_value(&mut self, value: impl Into<String>) {
        self.value = value.into();
        self.cursor = self.cursor.min(utf16_len(&self.value));
    }

    fn push_undo(&mut self) {
        self.undo_stack.push(&InputState {
            value: self.value.clone(),
            cursor: self.cursor,
        });
    }

    fn undo(&mut self) {
        if let Some(snapshot) = self.undo_stack.pop() {
            self.value = snapshot.value;
            self.cursor = snapshot.cursor;
            self.last_action = None;
        }
    }

    fn insert_character(&mut self, ch: &str) {
        let is_ws = ch.chars().all(char::is_whitespace);
        if is_ws || self.last_action != Some(LastAction::TypeWord) {
            self.push_undo();
        }
        self.last_action = Some(LastAction::TypeWord);
        let byte_idx = utf16_to_byte_index(&self.value, self.cursor);
        self.value.insert_str(byte_idx, ch);
        self.cursor += utf16_len(ch);
    }

    fn handle_backspace(&mut self) {
        self.last_action = None;
        if self.cursor == 0 {
            return;
        }
        if let Some((_, len)) = grapheme_before(&self.value, self.cursor) {
            self.push_undo();
            let start = utf16_to_byte_index(&self.value, self.cursor - len);
            let end = utf16_to_byte_index(&self.value, self.cursor);
            self.value.replace_range(start..end, "");
            self.cursor -= len;
        }
    }

    fn handle_forward_delete(&mut self) {
        self.last_action = None;
        if self.cursor >= utf16_len(&self.value) {
            return;
        }
        if let Some((_, len)) = grapheme_after(&self.value, self.cursor) {
            self.push_undo();
            let start = utf16_to_byte_index(&self.value, self.cursor);
            let end = utf16_to_byte_index(&self.value, self.cursor + len);
            self.value.replace_range(start..end, "");
        }
    }

    fn delete_to_line_start(&mut self) {
        if self.cursor == 0 {
            return;
        }
        self.push_undo();
        let deleted = slice_utf16(&self.value, 0, self.cursor).to_string();
        self.kill_ring
            .push(&deleted, true, self.last_action == Some(LastAction::Kill));
        self.last_action = Some(LastAction::Kill);
        let end = utf16_to_byte_index(&self.value, self.cursor);
        self.value.replace_range(0..end, "");
        self.cursor = 0;
    }

    fn delete_to_line_end(&mut self) {
        let len = utf16_len(&self.value);
        if self.cursor >= len {
            return;
        }
        self.push_undo();
        let deleted = slice_utf16(&self.value, self.cursor, len).to_string();
        self.kill_ring
            .push(&deleted, false, self.last_action == Some(LastAction::Kill));
        self.last_action = Some(LastAction::Kill);
        let start = utf16_to_byte_index(&self.value, self.cursor);
        self.value.truncate(start);
    }

    fn move_word_backwards(&mut self) {
        if self.cursor == 0 {
            return;
        }
        self.last_action = None;
        self.cursor = find_word_backward(&self.value, self.cursor, None);
    }

    fn move_word_forwards(&mut self) {
        let len = utf16_len(&self.value);
        if self.cursor >= len {
            return;
        }
        self.last_action = None;
        self.cursor = find_word_forward(&self.value, self.cursor, None);
    }

    fn delete_word_backwards(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let was_kill = self.last_action == Some(LastAction::Kill);
        self.push_undo();
        let old_cursor = self.cursor;
        self.move_word_backwards();
        let delete_from = self.cursor;
        self.cursor = old_cursor;

        let deleted = slice_utf16(&self.value, delete_from, self.cursor).to_string();
        self.kill_ring.push(&deleted, true, was_kill);
        self.last_action = Some(LastAction::Kill);

        let start = utf16_to_byte_index(&self.value, delete_from);
        let end = utf16_to_byte_index(&self.value, self.cursor);
        self.value.replace_range(start..end, "");
        self.cursor = delete_from;
    }

    fn delete_word_forward(&mut self) {
        let len = utf16_len(&self.value);
        if self.cursor >= len {
            return;
        }
        let was_kill = self.last_action == Some(LastAction::Kill);
        self.push_undo();
        let old_cursor = self.cursor;
        self.move_word_forwards();
        let delete_to = self.cursor;
        self.cursor = old_cursor;

        let deleted = slice_utf16(&self.value, self.cursor, delete_to).to_string();
        self.kill_ring.push(&deleted, false, was_kill);
        self.last_action = Some(LastAction::Kill);

        let start = utf16_to_byte_index(&self.value, self.cursor);
        let end = utf16_to_byte_index(&self.value, delete_to);
        self.value.replace_range(start..end, "");
    }

    fn yank(&mut self) {
        let Some(text) = self.kill_ring.peek().map(str::to_string) else {
            return;
        };
        self.push_undo();
        let byte_idx = utf16_to_byte_index(&self.value, self.cursor);
        self.value.insert_str(byte_idx, &text);
        self.cursor += utf16_len(&text);
        self.last_action = Some(LastAction::Yank);
    }

    fn yank_pop(&mut self) {
        if self.last_action != Some(LastAction::Yank) || self.kill_ring.len() <= 1 {
            return;
        }
        self.push_undo();
        let prev_text = self.kill_ring.peek().unwrap_or("").to_string();
        let prev_len = utf16_len(&prev_text);
        let start = utf16_to_byte_index(&self.value, self.cursor - prev_len);
        let end = utf16_to_byte_index(&self.value, self.cursor);
        self.value.replace_range(start..end, "");
        self.cursor -= prev_len;

        self.kill_ring.rotate();
        let text = self.kill_ring.peek().unwrap_or("").to_string();
        let byte_idx = utf16_to_byte_index(&self.value, self.cursor);
        self.value.insert_str(byte_idx, &text);
        self.cursor += utf16_len(&text);
        self.last_action = Some(LastAction::Yank);
    }

    fn handle_paste(&mut self, pasted_text: &str) {
        self.last_action = None;
        self.push_undo();
        let clean: String = pasted_text
            .replace("\r\n", "")
            .replace(['\r', '\n'], "")
            .replace('\t', "    ");
        let byte_idx = utf16_to_byte_index(&self.value, self.cursor);
        self.value.insert_str(byte_idx, &clean);
        self.cursor += utf16_len(&clean);
    }
}

impl Focusable for Input {
    fn is_focused(&self) -> bool {
        self.focused
    }
    fn set_focused(&mut self, value: bool) {
        self.focused = value;
    }
}

impl Component for Input {
    fn as_focusable_mut(&mut self) -> Option<&mut dyn Focusable> {
        Some(self)
    }

    fn invalidate(&mut self) {}

    /// `handleInput` (input.ts:48).
    fn handle_input(&mut self, data: &str) {
        let mut data = data.to_string();

        if data.contains("\x1b[200~") {
            self.is_in_paste = true;
            self.paste_buffer.clear();
            data = data.replace("\x1b[200~", "");
        }

        if self.is_in_paste {
            self.paste_buffer.push_str(&data);
            if let Some(end_idx) = self.paste_buffer.find("\x1b[201~") {
                let paste_content = self.paste_buffer[..end_idx].to_string();
                let remaining = self.paste_buffer[end_idx + "\x1b[201~".len()..].to_string();
                self.is_in_paste = false;
                self.paste_buffer.clear();
                self.handle_paste(&paste_content);
                if !remaining.is_empty() {
                    self.handle_input(&remaining);
                }
            }
            return;
        }

        let kb = get_keybindings();

        if kb.matches(&data, Keybinding::SelectCancel) {
            drop(kb);
            if let Some(on_escape) = &mut self.on_escape {
                on_escape();
            }
            return;
        }
        if kb.matches(&data, Keybinding::EditorUndo) {
            drop(kb);
            self.undo();
            return;
        }
        if kb.matches(&data, Keybinding::InputSubmit) || data == "\n" {
            drop(kb);
            if let Some(on_submit) = &mut self.on_submit {
                let value = self.value.clone();
                on_submit(&value);
            }
            return;
        }
        if kb.matches(&data, Keybinding::EditorDeleteCharBackward) {
            drop(kb);
            self.handle_backspace();
            return;
        }
        if kb.matches(&data, Keybinding::EditorDeleteCharForward) {
            drop(kb);
            self.handle_forward_delete();
            return;
        }
        if kb.matches(&data, Keybinding::EditorDeleteWordBackward) {
            drop(kb);
            self.delete_word_backwards();
            return;
        }
        if kb.matches(&data, Keybinding::EditorDeleteWordForward) {
            drop(kb);
            self.delete_word_forward();
            return;
        }
        if kb.matches(&data, Keybinding::EditorDeleteToLineStart) {
            drop(kb);
            self.delete_to_line_start();
            return;
        }
        if kb.matches(&data, Keybinding::EditorDeleteToLineEnd) {
            drop(kb);
            self.delete_to_line_end();
            return;
        }
        if kb.matches(&data, Keybinding::EditorYank) {
            drop(kb);
            self.yank();
            return;
        }
        if kb.matches(&data, Keybinding::EditorYankPop) {
            drop(kb);
            self.yank_pop();
            return;
        }
        if kb.matches(&data, Keybinding::EditorCursorLeft) {
            drop(kb);
            self.last_action = None;
            if self.cursor > 0 {
                if let Some((_, len)) = grapheme_before(&self.value, self.cursor) {
                    self.cursor -= len;
                } else {
                    self.cursor -= 1;
                }
            }
            return;
        }
        if kb.matches(&data, Keybinding::EditorCursorRight) {
            drop(kb);
            self.last_action = None;
            let len = utf16_len(&self.value);
            if self.cursor < len {
                if let Some((_, glen)) = grapheme_after(&self.value, self.cursor) {
                    self.cursor += glen;
                } else {
                    self.cursor += 1;
                }
            }
            return;
        }
        if kb.matches(&data, Keybinding::EditorCursorLineStart) {
            drop(kb);
            self.last_action = None;
            self.cursor = 0;
            return;
        }
        if kb.matches(&data, Keybinding::EditorCursorLineEnd) {
            drop(kb);
            self.last_action = None;
            self.cursor = utf16_len(&self.value);
            return;
        }
        if kb.matches(&data, Keybinding::EditorCursorWordLeft) {
            drop(kb);
            self.move_word_backwards();
            return;
        }
        if kb.matches(&data, Keybinding::EditorCursorWordRight) {
            drop(kb);
            self.move_word_forwards();
            return;
        }
        drop(kb);

        if let Some(printable) = decode_kitty_printable(&data) {
            self.insert_character(&printable);
            return;
        }

        if !has_control_chars(&data) {
            self.insert_character(&data);
        }
    }

    /// `render` (input.ts:378).
    fn render(&mut self, width: usize) -> Vec<String> {
        let prompt = "> ";
        let available_width = width as i64 - visible_width(prompt) as i64;
        if available_width <= 0 {
            return vec![prompt.to_string()];
        }
        let available_width = available_width as usize;

        let total_width = visible_width(&self.value);
        let value_len = utf16_len(&self.value);

        let (visible_text, cursor_display): (String, usize);
        if total_width < available_width {
            visible_text = self.value.clone();
            cursor_display = self.cursor;
        } else {
            let scroll_width = if self.cursor == value_len {
                available_width.saturating_sub(1)
            } else {
                available_width
            };
            let cursor_col = visible_width(slice_utf16(&self.value, 0, self.cursor));

            if scroll_width > 0 {
                let half_width = scroll_width / 2;
                let start_col = if cursor_col < half_width {
                    0
                } else if cursor_col > total_width.saturating_sub(half_width) {
                    total_width.saturating_sub(scroll_width)
                } else {
                    cursor_col.saturating_sub(half_width)
                };

                visible_text = slice_by_column(&self.value, start_col, scroll_width, true);
                let before_cursor = slice_by_column(
                    &self.value,
                    start_col,
                    cursor_col.saturating_sub(start_col),
                    true,
                );
                // `beforeCursor.length` (input.ts:417) is a UTF-16 length, not
                // a byte length — `cursorDisplay` is a UTF-16 offset into
                // `visibleText` in both branches (see module docs' UTF-16 note).
                cursor_display = utf16_len(&before_cursor);
            } else {
                visible_text = String::new();
                cursor_display = 0;
            }
        }

        let visible_len = utf16_len(&visible_text);
        let cursor_grapheme =
            slice_utf16(&visible_text, cursor_display.min(visible_len), visible_len)
                .graphemes(true)
                .next()
                .map(str::to_string);

        let before_cursor = slice_utf16(&visible_text, 0, cursor_display.min(visible_len));
        let at_cursor = cursor_grapheme.as_deref().unwrap_or(" ");
        let after_cursor_start = (cursor_display + utf16_len(at_cursor)).min(visible_len);
        let after_cursor = slice_utf16(&visible_text, after_cursor_start, visible_len);

        let marker = if self.focused { CURSOR_MARKER } else { "" };
        let cursor_char = format!("\x1b[7m{at_cursor}\x1b[27m");
        let text_with_cursor = format!("{before_cursor}{marker}{cursor_char}{after_cursor}");

        let visual_length = visible_width(&text_with_cursor);
        let padding = " ".repeat(available_width.saturating_sub(visual_length));
        vec![format!("{prompt}{text_with_cursor}{padding}")]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typing_appends_and_moves_cursor() {
        let mut input = Input::new();
        input.handle_input("h");
        input.handle_input("i");
        assert_eq!(input.get_value(), "hi");
        assert_eq!(input.cursor, 2);
    }

    #[test]
    fn backspace_removes_last_grapheme() {
        let mut input = Input::new();
        input.set_value("hi");
        input.cursor = 2;
        input.handle_backspace();
        assert_eq!(input.get_value(), "h");
    }

    #[test]
    fn undo_restores_previous_snapshot() {
        // Consecutive word characters coalesce into ONE undo unit (see
        // `insert_character`'s doc-cited coalescing rule) — "a" then " "
        // then "b" pushes only 2 snapshots (before "a", before " "; "b"
        // coalesces with the space since last_action is already TypeWord),
        // so one undo removes " b" as a unit, landing back on "a".
        let mut input = Input::new();
        input.handle_input("a");
        input.handle_input(" ");
        input.handle_input("b");
        input.undo();
        assert_eq!(input.get_value(), "a");
    }

    #[test]
    fn kill_to_line_start_then_yank_roundtrips() {
        let mut input = Input::new();
        input.set_value("hello world");
        input.cursor = 5;
        input.delete_to_line_start();
        assert_eq!(input.get_value(), " world");
        input.cursor = 0;
        input.yank();
        assert_eq!(input.get_value(), "hello world");
    }

    #[test]
    fn word_backward_uses_word_navigation() {
        let mut input = Input::new();
        input.set_value("hello world");
        input.cursor = 11;
        input.move_word_backwards();
        assert_eq!(input.cursor, 6);
    }

    #[test]
    fn astral_plane_grapheme_backspace_removes_full_codepoint() {
        let mut input = Input::new();
        input.set_value("a😀b");
        input.cursor = utf16_len("a😀b"); // end
        input.handle_backspace();
        assert_eq!(input.get_value(), "a😀");
        input.handle_backspace();
        assert_eq!(input.get_value(), "a");
    }

    #[test]
    fn bracketed_paste_strips_newlines_and_tabs() {
        let mut input = Input::new();
        input.handle_input("\x1b[200~line1\nline2\t\x1b[201~");
        assert_eq!(input.get_value(), "line1line2    ");
    }

    #[test]
    fn render_shows_prompt_and_pads_to_width() {
        let mut input = Input::new();
        input.set_value("hi");
        let lines = input.render(20);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].starts_with("> "));
        assert_eq!(visible_width(&lines[0]), 20);
    }
}
