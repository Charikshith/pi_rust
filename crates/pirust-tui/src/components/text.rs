//! Port of `packages/tui/src/components/text.ts` — a multi-line,
//! word-wrapped text component with optional background. See
//! `docs/analysis/05-tui.md` §6.

use crate::tui::Component;
use crate::utils::{apply_background_to_line, visible_width, wrap_text_with_ansi};

/// `Text` (text.ts:7).
pub struct Text {
    text: String,
    padding_x: usize,
    padding_y: usize,
    custom_bg_fn: Option<super::ColorFn>,
    cached_text: Option<String>,
    cached_width: Option<usize>,
    cached_lines: Option<Vec<String>>,
}

impl Text {
    /// `constructor` (text.ts:18) — `text=""`, `paddingX=1`, `paddingY=1`.
    pub fn new(text: impl Into<String>, padding_x: usize, padding_y: usize) -> Self {
        Self {
            text: text.into(),
            padding_x,
            padding_y,
            custom_bg_fn: None,
            cached_text: None,
            cached_width: None,
            cached_lines: None,
        }
    }

    pub fn with_bg_fn(
        text: impl Into<String>,
        padding_x: usize,
        padding_y: usize,
        custom_bg_fn: super::ColorFn,
    ) -> Self {
        Self {
            text: text.into(),
            padding_x,
            padding_y,
            custom_bg_fn: Some(custom_bg_fn),
            cached_text: None,
            cached_width: None,
            cached_lines: None,
        }
    }

    /// `setText` (text.ts:25).
    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.clear_cache();
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// `setCustomBgFn` (text.ts:32).
    pub fn set_custom_bg_fn(&mut self, custom_bg_fn: Option<super::ColorFn>) {
        self.custom_bg_fn = custom_bg_fn;
        self.clear_cache();
    }

    fn clear_cache(&mut self) {
        self.cached_text = None;
        self.cached_width = None;
        self.cached_lines = None;
    }
}

impl Component for Text {
    /// `render` (text.ts:45).
    fn render(&mut self, width: usize) -> Vec<String> {
        if let (Some(cached_lines), Some(cached_text), Some(cached_width)) =
            (&self.cached_lines, &self.cached_text, self.cached_width)
        {
            if cached_text == &self.text && cached_width == width {
                return cached_lines.clone();
            }
        }

        if self.text.trim().is_empty() {
            let result: Vec<String> = Vec::new();
            self.cached_text = Some(self.text.clone());
            self.cached_width = Some(width);
            self.cached_lines = Some(result.clone());
            return result;
        }

        let normalized_text = self.text.replace('\t', "   ");
        let content_width = (width.saturating_sub(self.padding_x * 2)).max(1);
        let wrapped_lines = wrap_text_with_ansi(&normalized_text, content_width);

        let left_margin = " ".repeat(self.padding_x);
        let right_margin = " ".repeat(self.padding_x);
        let mut content_lines: Vec<String> = Vec::new();
        for line in &wrapped_lines {
            let line_with_margins = format!("{left_margin}{line}{right_margin}");
            if let Some(bg_fn) = &self.custom_bg_fn {
                content_lines.push(apply_background_to_line(&line_with_margins, width, |s| {
                    bg_fn(s)
                }));
            } else {
                let visible_len = visible_width(&line_with_margins);
                let padding_needed = width.saturating_sub(visible_len);
                content_lines.push(format!("{line_with_margins}{}", " ".repeat(padding_needed)));
            }
        }

        let empty_line = " ".repeat(width);
        let mut empty_lines: Vec<String> = Vec::new();
        for _ in 0..self.padding_y {
            let line = if let Some(bg_fn) = &self.custom_bg_fn {
                apply_background_to_line(&empty_line, width, |s| bg_fn(s))
            } else {
                empty_line.clone()
            };
            empty_lines.push(line);
        }

        let mut result = empty_lines.clone();
        result.extend(content_lines);
        result.extend(empty_lines);

        self.cached_text = Some(self.text.clone());
        self.cached_width = Some(width);
        self.cached_lines = Some(result.clone());

        if result.is_empty() {
            vec![String::new()]
        } else {
            result
        }
    }

    fn invalidate(&mut self) {
        self.clear_cache();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_text_renders_nothing() {
        let mut t = Text::new("", 1, 1);
        assert_eq!(t.render(10), Vec::<String>::new());
    }

    #[test]
    fn simple_text_gets_padding() {
        let mut t = Text::new("hi", 1, 0);
        let lines = t.render(10);
        assert_eq!(lines, vec![" hi       ".to_string()]);
        assert_eq!(lines[0].chars().count(), 10);
    }

    #[test]
    fn cache_hits_on_same_text_and_width() {
        let mut t = Text::new("hi", 1, 0);
        let first = t.render(10);
        let second = t.render(10);
        assert_eq!(first, second);
    }
}
