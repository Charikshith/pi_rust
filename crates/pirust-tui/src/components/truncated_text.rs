//! Port of `packages/tui/src/components/truncated-text.ts` — a single-line
//! text component truncated to fit the viewport width. See
//! `docs/analysis/05-tui.md` §6.

use crate::tui::Component;
use crate::utils::{truncate_to_width, visible_width};

/// `TruncatedText` (truncated-text.ts:7).
pub struct TruncatedText {
    text: String,
    padding_x: usize,
    padding_y: usize,
}

impl TruncatedText {
    /// `constructor` (truncated-text.ts:12) — `paddingX=0`, `paddingY=0`.
    pub fn new(text: impl Into<String>, padding_x: usize, padding_y: usize) -> Self {
        Self {
            text: text.into(),
            padding_x,
            padding_y,
        }
    }

    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
    }
}

impl Component for TruncatedText {
    /// `render` (truncated-text.ts:22).
    fn render(&mut self, width: usize) -> Vec<String> {
        let mut result = Vec::new();
        let empty_line = " ".repeat(width);

        for _ in 0..self.padding_y {
            result.push(empty_line.clone());
        }

        let available_width = (width.saturating_sub(self.padding_x * 2)).max(1);

        let single_line_text = match self.text.find('\n') {
            Some(idx) => &self.text[..idx],
            None => self.text.as_str(),
        };

        let display_text = truncate_to_width(single_line_text, available_width, "...", false);

        let left_padding = " ".repeat(self.padding_x);
        let right_padding = " ".repeat(self.padding_x);
        let line_with_padding = format!("{left_padding}{display_text}{right_padding}");

        let line_visible_width = visible_width(&line_with_padding);
        let padding_needed = width.saturating_sub(line_visible_width);
        let final_line = format!("{line_with_padding}{}", " ".repeat(padding_needed));

        result.push(final_line);

        for _ in 0..self.padding_y {
            result.push(empty_line.clone());
        }

        result
    }

    fn invalidate(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stops_at_first_newline() {
        let mut t = TruncatedText::new("first\nsecond", 0, 0);
        let lines = t.render(20);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].starts_with("first"));
    }

    #[test]
    fn pads_to_exact_width() {
        let mut t = TruncatedText::new("hi", 0, 0);
        let lines = t.render(10);
        assert_eq!(visible_width(&lines[0]), 10);
    }

    #[test]
    fn vertical_padding_adds_empty_lines() {
        let mut t = TruncatedText::new("hi", 0, 2);
        let lines = t.render(5);
        assert_eq!(lines.len(), 5);
    }
}
