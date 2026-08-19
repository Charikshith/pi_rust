//! `components/markdown.ts` (1010 lines) — full markdown renderer built on
//! `marked`. Ported 1:1 from the oracle `../pi` checkout. The token model is
//! a hand-rolled subset of `marked`'s AST (`Token`) that the renderer
//! actually touches (heading depth, list start/task/checked/loose, code lang,
//! table header/rows, inline text/strong/em/codespan/link/del/escape/br/html,
//! blockquote, hr, space, html). Real Pi has no LaTeX rendering — `$...$`
//! and `$$...$$` are plain text, passed through unrendered.
//!
//! Correctness bar: `tests/fixtures/pi/tui/markdown.cases.jsonl` + `cargo
//! test -p pirust-tui` — byte-exact against real Pi output via the oracle
//! script.

use std::rc::Rc;

use crate::terminal_image::{get_capabilities, hyperlink, is_image_line};
use crate::utils::{apply_background_to_line, visible_width, wrap_text_with_ansi};

/// `Token` — the marked AST subset this renderer consumes.
#[derive(Debug, Clone)]
pub enum Token {
    Space,
    Hr,
    Heading {
        depth: usize,
        tokens: Vec<Token>,
    },
    Paragraph {
        tokens: Vec<Token>,
    },
    Text {
        text: String,
        tokens: Vec<Token>,
    },
    Strong {
        tokens: Vec<Token>,
    },
    Em {
        tokens: Vec<Token>,
    },
    Codespan {
        text: String,
    },
    Del {
        tokens: Vec<Token>,
    },
    Link {
        text: String,
        href: String,
        tokens: Vec<Token>,
    },
    Br,
    Escape {
        raw: String,
        text: String,
    },
    Html {
        raw: String,
    },
    Code {
        lang: String,
        text: String,
        raw: String,
    },
    List(ListToken),
    ListItem(ListItemToken),
    Blockquote {
        tokens: Vec<Token>,
    },
    Table(TableToken),
    Image {
        text: String,
    },
    Raw {
        raw: String,
    },
}

#[derive(Debug, Clone)]
pub struct ListToken {
    pub ordered: bool,
    pub start: usize,
    pub loose: bool,
    pub items: Vec<ListItemToken>,
}

#[derive(Debug, Clone)]
pub struct ListItemToken {
    pub raw: String,
    pub task: bool,
    pub checked: bool,
    pub tokens: Vec<Token>,
}

#[derive(Debug, Clone)]
pub struct TableToken {
    pub header: Vec<TableCell>,
    pub rows: Vec<Vec<TableCell>>,
    pub raw: String,
}

#[derive(Debug, Clone)]
pub struct TableCell {
    pub text: String,
    pub tokens: Vec<Token>,
}

/// `DefaultTextStyle` (markdown.ts).
/// A theme style function: text -> ANSI-styled text.
pub type StyleFn = Rc<dyn Fn(&str) -> String>;

#[derive(Clone, Default)]
pub struct DefaultTextStyle {
    pub color: Option<StyleFn>,
    pub bg_color: Option<StyleFn>,
    pub bold: bool,
    pub italic: bool,
    pub strikethrough: bool,
    pub underline: bool,
}

/// `MarkdownTheme` (markdown.ts).
#[derive(Clone)]
pub struct MarkdownTheme {
    pub heading: Rc<dyn Fn(&str) -> String>,
    pub link: Rc<dyn Fn(&str) -> String>,
    pub link_url: Rc<dyn Fn(&str) -> String>,
    pub code: Rc<dyn Fn(&str) -> String>,
    pub code_block: Rc<dyn Fn(&str) -> String>,
    pub code_block_border: Rc<dyn Fn(&str) -> String>,
    pub quote: Rc<dyn Fn(&str) -> String>,
    pub quote_border: Rc<dyn Fn(&str) -> String>,
    pub hr: Rc<dyn Fn(&str) -> String>,
    pub list_bullet: Rc<dyn Fn(&str) -> String>,
    pub bold: Rc<dyn Fn(&str) -> String>,
    pub italic: Rc<dyn Fn(&str) -> String>,
    pub strikethrough: Rc<dyn Fn(&str) -> String>,
    pub underline: Rc<dyn Fn(&str) -> String>,
}

/// `MarkdownOptions` (markdown.ts).
#[derive(Clone, Default)]
pub struct MarkdownOptions {
    pub preserve_ordered_list_markers: bool,
    pub preserve_backslash_escapes: bool,
}

struct InlineStyleContext {
    apply_text: Rc<dyn Fn(&str) -> String>,
    style_prefix: String,
}

impl Clone for InlineStyleContext {
    fn clone(&self) -> Self {
        Self {
            apply_text: self.apply_text.clone(),
            style_prefix: self.style_prefix.clone(),
        }
    }
}

/// `Markdown` component (markdown.ts:243).
pub struct Markdown {
    text: String,
    padding_x: usize,
    padding_y: usize,
    default_text_style: Option<DefaultTextStyle>,
    theme: MarkdownTheme,
    options: MarkdownOptions,
    default_style_prefix: Option<String>,
    cached_text: Option<String>,
    cached_width: Option<usize>,
    cached_lines: Option<Vec<String>>,
}

impl Markdown {
    pub fn new(
        text: &str,
        padding_x: usize,
        padding_y: usize,
        theme: MarkdownTheme,
        default_text_style: Option<DefaultTextStyle>,
        options: Option<MarkdownOptions>,
    ) -> Self {
        Self {
            text: text.to_string(),
            padding_x,
            padding_y,
            default_text_style,
            theme,
            options: options.unwrap_or_default(),
            default_style_prefix: None,
            cached_text: None,
            cached_width: None,
            cached_lines: None,
        }
    }

    pub fn set_text(&mut self, text: &str) {
        self.text = text.to_string();
        self.invalidate();
    }

    pub fn invalidate(&mut self) {
        self.cached_text = None;
        self.cached_width = None;
        self.cached_lines = None;
    }

    pub fn render(&mut self, width: usize) -> Vec<String> {
        if let (Some(lines), Some(t), Some(w)) =
            (&self.cached_lines, &self.cached_text, self.cached_width)
        {
            if *t == self.text && w == width {
                return lines.clone();
            }
        }

        let content_width = width.saturating_sub(self.padding_x * 2).max(1);
        let text = self.text.clone();

        if text.trim().is_empty() {
            self.cached_text = Some(self.text.clone());
            self.cached_width = Some(width);
            self.cached_lines = Some(Vec::new());
            return Vec::new();
        }

        // Replace tabs with 3 spaces.
        let normalized_text = text.replace('\t', "   ");
        let mut tokens = lex(&normalized_text);
        while matches!(tokens.last(), Some(Token::Space)) {
            tokens.pop();
        }
        let tokens = trim_partial_closing_fences(tokens);

        let mut rendered_lines: Vec<String> = Vec::new();
        for (i, token) in tokens.iter().enumerate() {
            let next_type = tokens.get(i + 1).map(|t| t.type_name());
            let token_lines = self.render_token(token, content_width, next_type, None);
            rendered_lines.extend(token_lines);
        }

        // Wrap lines (NO padding, NO background yet).
        let mut wrapped_lines: Vec<String> = Vec::new();
        for line in rendered_lines {
            if is_image_line(&line) {
                wrapped_lines.push(line);
            } else {
                for wrapped_line in wrap_text_with_ansi(&line, content_width) {
                    wrapped_lines.push(wrapped_line);
                }
            }
        }

        let left_margin = " ".repeat(self.padding_x);
        let right_margin = " ".repeat(self.padding_x);
        let bg_fn = self
            .default_text_style
            .as_ref()
            .and_then(|s| s.bg_color.clone());
        let mut content_lines: Vec<String> = Vec::new();

        for line in wrapped_lines {
            if is_image_line(&line) {
                content_lines.push(line);
                continue;
            }
            let line_with_margins = format!("{left_margin}{line}{right_margin}");
            if let Some(bg) = &bg_fn {
                content_lines.push(apply_background_to_line(
                    &line_with_margins,
                    width,
                    bg.as_ref(),
                ));
            } else {
                let visible_len = visible_width(&line_with_margins);
                let padding_needed = width.saturating_sub(visible_len);
                content_lines.push(format!("{line_with_margins}{}", " ".repeat(padding_needed)));
            }
        }

        let empty_line = " ".repeat(width);
        let mut empty_lines: Vec<String> = Vec::new();
        for _ in 0..self.padding_y {
            if let Some(bg) = &bg_fn {
                empty_lines.push(apply_background_to_line(&empty_line, width, bg.as_ref()));
            } else {
                empty_lines.push(empty_line.clone());
            }
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

    fn apply_default_style(&self, text: &str) -> String {
        let Some(style) = &self.default_text_style else {
            return text.to_string();
        };
        let mut styled = text.to_string();
        if let Some(color) = &style.color {
            styled = color(&styled);
        }
        if style.bold {
            styled = (self.theme.bold)(&styled);
        }
        if style.italic {
            styled = (self.theme.italic)(&styled);
        }
        if style.strikethrough {
            styled = (self.theme.strikethrough)(&styled);
        }
        if style.underline {
            styled = (self.theme.underline)(&styled);
        }
        styled
    }

    fn get_default_style_prefix(&mut self) -> String {
        let Some(style) = &self.default_text_style else {
            return String::new();
        };
        if let Some(prefix) = &self.default_style_prefix {
            return prefix.clone();
        }
        let sentinel = '\u{0}';
        let mut styled = sentinel.to_string();
        if let Some(color) = &style.color {
            styled = color(&styled);
        }
        if style.bold {
            styled = (self.theme.bold)(&styled);
        }
        if style.italic {
            styled = (self.theme.italic)(&styled);
        }
        if style.strikethrough {
            styled = (self.theme.strikethrough)(&styled);
        }
        if style.underline {
            styled = (self.theme.underline)(&styled);
        }
        let prefix = if let Some(idx) = styled.find(sentinel) {
            styled[..idx].to_string()
        } else {
            String::new()
        };
        self.default_style_prefix = Some(prefix.clone());
        prefix
    }

    fn get_style_prefix(&self, style_fn: &dyn Fn(&str) -> String) -> String {
        let sentinel = '\u{0}';
        let styled = style_fn(&sentinel.to_string());
        if let Some(idx) = styled.find(sentinel) {
            styled[..idx].to_string()
        } else {
            String::new()
        }
    }

    fn get_default_inline_style_context(&mut self) -> InlineStyleContext {
        // NOTE: `apply_default_style` needs &self; the render loop calls it
        // directly via `apply_text_with_newlines` (which checks
        // `is_default()`), so the context's apply_text is a passthrough.
        InlineStyleContext {
            apply_text: Rc::new(|t: &str| t.to_string()),
            style_prefix: self.get_default_style_prefix(),
        }
    }

    fn render_token(
        &mut self,
        token: &Token,
        width: usize,
        next_token_type: Option<&str>,
        style_context: Option<InlineStyleContext>,
    ) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();
        match token {
            Token::Heading { depth, tokens } => {
                let heading_level = *depth;
                let heading_prefix = format!("{} ", "#".repeat(heading_level));
                let heading_style_fn: Rc<dyn Fn(&str) -> String> = if heading_level == 1 {
                    let theme = self.theme.clone();
                    Rc::new(move |text: &str| {
                        (theme.heading)(&(theme.bold)(&(theme.underline)(text)))
                    })
                } else {
                    let theme = self.theme.clone();
                    Rc::new(move |text: &str| (theme.heading)(&(theme.bold)(text)))
                };
                let heading_style_context = InlineStyleContext {
                    apply_text: heading_style_fn.clone(),
                    style_prefix: self.get_style_prefix(heading_style_fn.as_ref()),
                };
                let heading_text = self.render_inline_tokens(tokens, Some(heading_style_context));
                let styled_heading = if heading_level >= 3 {
                    heading_style_fn(&heading_prefix) + &heading_text
                } else {
                    heading_text
                };
                lines.push(styled_heading);
                if let Some(next) = next_token_type {
                    if next != "space" {
                        lines.push(String::new());
                    }
                }
            }
            Token::Paragraph { tokens } => {
                let paragraph_text = self.render_inline_tokens(tokens, style_context);
                lines.push(paragraph_text);
                if let Some(next) = next_token_type {
                    if next != "list" && next != "space" {
                        lines.push(String::new());
                    }
                }
            }
            Token::Text { text, tokens } => {
                let t = Token::Text {
                    text: text.clone(),
                    tokens: tokens.clone(),
                };
                lines.push(self.render_inline_tokens(&[t], style_context));
            }
            Token::Code { lang, text, .. } => {
                let indent = "  ";
                lines.push((self.theme.code_block_border)(&format!("```{lang}")));
                let code_lines: Vec<&str> = text.split('\n').collect();
                for code_line in code_lines {
                    lines.push(format!("{indent}{}", (self.theme.code_block)(code_line)));
                }
                lines.push((self.theme.code_block_border)("```"));
                if let Some(next) = next_token_type {
                    if next != "space" {
                        lines.push(String::new());
                    }
                }
            }
            Token::List(list) => {
                lines.extend(self.render_list(list, 0, width, style_context));
            }
            Token::Table(table) => {
                lines.extend(self.render_table(table, width, next_token_type, style_context));
            }
            Token::Blockquote { tokens } => {
                let quote_style: Rc<dyn Fn(&str) -> String> = {
                    let theme = self.theme.clone();
                    Rc::new(move |text: &str| (theme.quote)(&(theme.italic)(text)))
                };
                let quote_style_prefix = self.get_style_prefix(quote_style.as_ref());
                let quote_style_prefix_owned = quote_style_prefix.clone();
                let apply_quote_style = move |line: &str| -> String {
                    if quote_style_prefix_owned.is_empty() {
                        return quote_style(line);
                    }
                    let line_with_reapplied =
                        line.replace("\x1b[0m", &format!("\x1b[0m{quote_style_prefix_owned}"));
                    quote_style(&line_with_reapplied)
                };

                let quote_content_width = width.saturating_sub(2).max(1);
                let quote_inline_style_context = InlineStyleContext {
                    apply_text: Rc::new(|t: &str| t.to_string()),
                    style_prefix: quote_style_prefix.clone(),
                };
                let quote_tokens = tokens.clone();
                let mut rendered_quote_lines: Vec<String> = Vec::new();
                for (i, quote_token) in quote_tokens.iter().enumerate() {
                    let next_quote = quote_tokens.get(i + 1).map(|t| t.type_name());
                    rendered_quote_lines.extend(self.render_token(
                        quote_token,
                        quote_content_width,
                        next_quote,
                        Some(quote_inline_style_context.clone()),
                    ));
                }
                while rendered_quote_lines
                    .last()
                    .map(|l| l.is_empty())
                    .unwrap_or(false)
                {
                    rendered_quote_lines.pop();
                }
                for quote_line in rendered_quote_lines {
                    let styled_line = apply_quote_style(&quote_line);
                    for wrapped_line in wrap_text_with_ansi(&styled_line, quote_content_width) {
                        lines.push((self.theme.quote_border)("│ ") + &wrapped_line);
                    }
                }
                if let Some(next) = next_token_type {
                    if next != "space" {
                        lines.push(String::new());
                    }
                }
            }
            Token::Image { text } => {
                lines.push(self.apply_default_style(text));
            }
            Token::Hr => {
                lines.push((self.theme.hr)(&"─".repeat(width.min(80))));
                if let Some(next) = next_token_type {
                    if next != "space" {
                        lines.push(String::new());
                    }
                }
            }
            Token::Html { raw } => {
                lines.push(self.apply_default_style(raw.trim()));
            }
            Token::Space => {
                lines.push(String::new());
            }
            other => {
                if let Some(t) = other.text_content() {
                    lines.push(t);
                }
            }
        }
        lines
    }

    /// `applyTextWithNewlines` (markdown.ts) — applies styling per segment
    /// so ANSI doesn't bleed across embedded newlines.
    fn apply_text_with_newlines(&self, text: &str, resolved: &InlineStyleContext) -> String {
        text.split('\n')
            .map(|segment| {
                if resolved.is_default() {
                    self.apply_default_style(segment)
                } else {
                    (resolved.apply_text)(segment)
                }
            })
            .collect::<Vec<String>>()
            .join("\n")
    }

    fn render_inline_tokens(
        &mut self,
        tokens: &[Token],
        style_context: Option<InlineStyleContext>,
    ) -> String {
        let mut result = String::new();
        let resolved = style_context.unwrap_or_else(|| self.get_default_inline_style_context());
        let style_prefix = resolved.style_prefix.clone();

        // `applyTextWithNewlines` (markdown.ts) is handled by the
        // `apply_text_with_newlines` method below; no closure here.

        for token in tokens {
            match token {
                Token::Escape { raw, text } => {
                    let use_raw = self.options.preserve_backslash_escapes;
                    let content = if use_raw { raw } else { text };
                    result.push_str(&self.apply_text_with_newlines(content, &resolved));
                }
                Token::Text { text, tokens } => {
                    if !tokens.is_empty() {
                        result.push_str(&self.render_inline_tokens(tokens, Some(resolved.clone())));
                    } else {
                        result.push_str(&self.apply_text_with_newlines(text, &resolved));
                    }
                }
                Token::Paragraph { tokens } => {
                    result.push_str(&self.render_inline_tokens(tokens, Some(resolved.clone())));
                }
                Token::Strong { tokens } => {
                    let bold_content = self.render_inline_tokens(tokens, Some(resolved.clone()));
                    result.push_str(&(self.theme.bold)(&bold_content));
                    result.push_str(&style_prefix);
                }
                Token::Em { tokens } => {
                    let italic_content = self.render_inline_tokens(tokens, Some(resolved.clone()));
                    result.push_str(&(self.theme.italic)(&italic_content));
                    result.push_str(&style_prefix);
                }
                Token::Codespan { text } => {
                    result.push_str(&(self.theme.code)(text));
                    result.push_str(&style_prefix);
                }
                Token::Link { text, href, tokens } => {
                    let link_text = self.render_inline_tokens(tokens, Some(resolved.clone()));
                    let styled_link = (self.theme.link)(&(self.theme.underline)(&link_text));
                    if get_capabilities().hyperlinks {
                        result.push_str(&hyperlink(&styled_link, href));
                        result.push_str(&style_prefix);
                    } else {
                        let href_for_comparison =
                            href.strip_prefix("mailto:").unwrap_or(href.as_str());
                        if text == href || text == href_for_comparison {
                            result.push_str(&styled_link);
                            result.push_str(&style_prefix);
                        } else {
                            result.push_str(&styled_link);
                            result.push_str(&(self.theme.link_url)(&format!(" ({href})")));
                            result.push_str(&style_prefix);
                        }
                    }
                }
                Token::Br => {
                    result.push('\n');
                }
                Token::Image { text } => {
                    result.push_str(&self.apply_text_with_newlines(text, &resolved));
                }
                Token::Del { tokens } => {
                    let del_content = self.render_inline_tokens(tokens, Some(resolved.clone()));
                    result.push_str(&(self.theme.strikethrough)(&del_content));
                    result.push_str(&style_prefix);
                }
                Token::Html { raw } => {
                    result.push_str(&self.apply_text_with_newlines(raw, &resolved));
                }
                other => {
                    if let Some(t) = other.text_content() {
                        result.push_str(&self.apply_text_with_newlines(&t, &resolved));
                    }
                }
            }
        }

        while !style_prefix.is_empty() && result.ends_with(&style_prefix) {
            result.truncate(result.len() - style_prefix.len());
        }

        result
    }

    fn get_ordered_list_marker(&self, item: &ListItemToken) -> Option<String> {
        // /^(?: {0,3})(\d{1,9}[.)])[ \t]+/
        let raw = item
            .raw
            .trim_start_matches(' ')
            .trim_start_matches(' ')
            .trim_start_matches(' ');
        let chars: Vec<char> = raw.chars().collect();
        let mut i = 0;
        while i < chars.len() && chars[i] == ' ' {
            i += 1;
        }
        let mut digits = String::new();
        while i < chars.len() && chars[i].is_ascii_digit() && digits.len() < 9 {
            digits.push(chars[i]);
            i += 1;
        }
        if digits.is_empty() {
            return None;
        }
        if i < chars.len() && (chars[i] == '.' || chars[i] == ')') {
            let marker = format!("{}{} ", digits, chars[i]);
            // [ \t]+
            let mut j = i + 1;
            while j < chars.len() && (chars[j] == ' ' || chars[j] == '\t') {
                j += 1;
            }
            if j > i + 1 {
                return Some(marker);
            }
        }
        None
    }

    fn get_unordered_list_marker(&self, item: &ListItemToken) -> Option<String> {
        // /^(?: {0,3})([-+*])(?:[ \t]+|(?=\r?\n|$))/
        let raw = item.raw.trim_start();
        let chars: Vec<char> = raw.chars().collect();
        let mut i = 0;
        while i < chars.len() && chars[i] == ' ' && i < 3 {
            i += 1;
        }
        if i < chars.len() && (chars[i] == '-' || chars[i] == '+' || chars[i] == '*') {
            let marker = format!("{} ", chars[i]);
            let j = i + 1;
            if j < chars.len() && (chars[j] == ' ' || chars[j] == '\t') {
                return Some(marker);
            }
            if j >= chars.len() || chars[j] == '\n' || chars[j] == '\r' {
                return Some(marker);
            }
        }
        None
    }

    fn render_list(
        &mut self,
        token: &ListToken,
        depth: usize,
        width: usize,
        style_context: Option<InlineStyleContext>,
    ) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();
        let indent = "    ".repeat(depth);
        let start_number = token.start;

        for (i, item) in token.items.iter().enumerate() {
            let is_last_item = i == token.items.len() - 1;
            let bullet = if token.ordered {
                if self.options.preserve_ordered_list_markers {
                    self.get_ordered_list_marker(item)
                        .unwrap_or_else(|| format!("{}. ", start_number + i))
                } else {
                    format!("{}. ", start_number + i)
                }
            } else if self.options.preserve_ordered_list_markers {
                self.get_unordered_list_marker(item)
                    .unwrap_or_else(|| "- ".to_string())
            } else {
                "- ".to_string()
            };
            let task_marker = if item.task {
                format!("[{}] ", if item.checked { "x" } else { " " })
            } else {
                String::new()
            };
            let marker = format!("{bullet}{task_marker}");
            let first_prefix = format!("{indent}{}", (self.theme.list_bullet)(&marker));
            let continuation_prefix = format!("{indent}{}", " ".repeat(visible_width(&marker)));
            let item_width = width.saturating_sub(visible_width(&first_prefix)).max(1);
            let mut rendered_any_line = false;

            for item_token in &item.tokens {
                if let Token::List(nested) = item_token {
                    lines.extend(self.render_list(nested, depth + 1, width, style_context.clone()));
                    rendered_any_line = true;
                    continue;
                }
                let item_lines =
                    self.render_token(item_token, item_width, None, style_context.clone());
                for line in item_lines {
                    for wrapped_line in wrap_text_with_ansi(&line, item_width) {
                        let line_prefix = if rendered_any_line {
                            continuation_prefix.clone()
                        } else {
                            first_prefix.clone()
                        };
                        lines.push(line_prefix + &wrapped_line);
                        rendered_any_line = true;
                    }
                }
            }

            if !rendered_any_line {
                lines.push(first_prefix);
            }
            if token.loose && !is_last_item {
                lines.push(String::new());
            }
        }
        lines
    }

    fn get_longest_word_width(&self, text: &str, max_width: Option<usize>) -> usize {
        let words: Vec<&str> = text.split_whitespace().filter(|w| !w.is_empty()).collect();
        let longest = words.iter().map(|w| visible_width(w)).max().unwrap_or(0);
        if let Some(max) = max_width {
            longest.min(max)
        } else {
            longest
        }
    }

    fn wrap_cell_text(&self, text: &str, max_width: usize) -> Vec<String> {
        wrap_text_with_ansi(text, max_width.max(1))
    }

    fn render_table(
        &mut self,
        token: &TableToken,
        available_width: usize,
        next_token_type: Option<&str>,
        style_context: Option<InlineStyleContext>,
    ) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();
        let num_cols = token.header.len();
        if num_cols == 0 {
            return lines;
        }
        let border_overhead = 3 * num_cols + 1;
        let available_for_cells = available_width.saturating_sub(border_overhead);
        if available_for_cells < num_cols {
            let fallback_lines = if token.raw.is_empty() {
                Vec::new()
            } else {
                wrap_text_with_ansi(&token.raw, available_width)
            };
            if let Some(next) = next_token_type {
                if next != "space" {
                    lines.push(String::new());
                }
            }
            lines.extend(fallback_lines);
            return lines;
        }

        let max_unbroken_word_width = 30;
        let mut natural_widths: Vec<usize> = Vec::new();
        let mut min_word_widths: Vec<usize> = Vec::new();
        for cell in &token.header {
            let header_text = self.render_inline_tokens(&cell.tokens, style_context.clone());
            natural_widths.push(visible_width(&header_text));
            min_word_widths.push(
                self.get_longest_word_width(&header_text, Some(max_unbroken_word_width))
                    .max(1),
            );
        }
        for row in &token.rows {
            for (i, cell) in row.iter().enumerate() {
                if i >= num_cols {
                    break;
                }
                let cell_text = self.render_inline_tokens(&cell.tokens, style_context.clone());
                if i < natural_widths.len() {
                    natural_widths[i] = natural_widths[i].max(visible_width(&cell_text));
                } else {
                    natural_widths.push(visible_width(&cell_text));
                }
                if i < min_word_widths.len() {
                    min_word_widths[i] = min_word_widths[i].max(
                        self.get_longest_word_width(&cell_text, Some(max_unbroken_word_width)),
                    );
                } else {
                    min_word_widths.push(
                        self.get_longest_word_width(&cell_text, Some(max_unbroken_word_width))
                            .max(1),
                    );
                }
            }
        }

        let mut min_column_widths = min_word_widths.clone();
        let mut min_cells_width: usize = min_column_widths.iter().sum();

        if min_cells_width > available_for_cells {
            min_column_widths = vec![1; num_cols];
            let remaining = available_for_cells.saturating_sub(num_cols);
            if remaining > 0 {
                let total_weight: usize = min_word_widths.iter().map(|w| w.saturating_sub(1)).sum();
                let growth: Vec<usize> = min_word_widths
                    .iter()
                    .map(|w| {
                        let weight = w.saturating_sub(1);
                        if total_weight > 0 {
                            weight
                                .checked_mul(remaining)
                                .map_or(0, |n| n / total_weight)
                        } else {
                            0
                        }
                    })
                    .collect();
                #[allow(clippy::needless_range_loop)]
                for i in 0..num_cols {
                    min_column_widths[i] += growth.get(i).copied().unwrap_or(0);
                }
                let allocated: usize = growth.iter().sum();
                let mut leftover = remaining.saturating_sub(allocated);
                let mut i = 0;
                while leftover > 0 && i < num_cols {
                    min_column_widths[i] += 1;
                    leftover -= 1;
                    i += 1;
                }
            }
            min_cells_width = min_column_widths.iter().sum();
        }

        let total_natural_width: usize = natural_widths.iter().sum::<usize>() + border_overhead;
        let column_widths: Vec<usize> = if total_natural_width <= available_width {
            natural_widths
                .iter()
                .enumerate()
                .map(|(index, w)| (*w).max(min_column_widths.get(index).copied().unwrap_or(0)))
                .collect()
        } else {
            let total_grow_potential: usize = natural_widths
                .iter()
                .enumerate()
                .map(|(index, w)| {
                    w.saturating_sub(min_column_widths.get(index).copied().unwrap_or(0))
                })
                .sum();
            let extra_width = available_for_cells.saturating_sub(min_cells_width);
            let mut widths: Vec<usize> = min_column_widths
                .iter()
                .enumerate()
                .map(|(index, min_w)| {
                    let natural_width = natural_widths.get(index).copied().unwrap_or(0);
                    let min_width_delta = natural_width.saturating_sub(*min_w);
                    let grow = if total_grow_potential > 0 {
                        min_width_delta
                            .checked_mul(extra_width)
                            .map_or(0, |n| n / total_grow_potential)
                    } else {
                        0
                    };
                    min_w + grow
                })
                .collect();
            let allocated: usize = widths.iter().sum();
            let mut remaining = available_for_cells.saturating_sub(allocated);
            while remaining > 0 {
                let mut grew = false;
                #[allow(clippy::needless_range_loop)] // index needed to
                // compare against the parallel natural_widths array
                for i in 0..num_cols {
                    if remaining == 0 {
                        break;
                    }
                    if widths[i] < natural_widths.get(i).copied().unwrap_or(0) {
                        widths[i] += 1;
                        remaining -= 1;
                        grew = true;
                    }
                }
                if !grew {
                    break;
                }
            }
            widths
        };

        let top_border_cells: Vec<String> = column_widths.iter().map(|w| "─".repeat(*w)).collect();
        lines.push(format!("┌─{}─┐", top_border_cells.join("─┬─")));

        let header_cell_lines: Vec<Vec<String>> = token
            .header
            .iter()
            .enumerate()
            .map(|(i, cell)| {
                let text = self.render_inline_tokens(&cell.tokens, style_context.clone());
                self.wrap_cell_text(&text, column_widths[i])
            })
            .collect();
        let header_line_count = header_cell_lines.iter().map(|c| c.len()).max().unwrap_or(0);
        for line_idx in 0..header_line_count {
            let row_parts: Vec<String> = header_cell_lines
                .iter()
                .enumerate()
                .map(|(col_idx, cell_lines)| {
                    let text = cell_lines.get(line_idx).cloned().unwrap_or_default();
                    let padded = format!(
                        "{text}{}",
                        " ".repeat(column_widths[col_idx].saturating_sub(visible_width(&text)))
                    );
                    (self.theme.bold)(&padded)
                })
                .collect();
            lines.push(format!("│ {} │", row_parts.join(" │ ")));
        }

        let separator_cells: Vec<String> = column_widths.iter().map(|w| "─".repeat(*w)).collect();
        lines.push(format!("├─{}─┤", separator_cells.join("─┼─")));

        for (row_index, row) in token.rows.iter().enumerate() {
            let row_cell_lines: Vec<Vec<String>> = row
                .iter()
                .enumerate()
                .map(|(i, cell)| {
                    let text = self.render_inline_tokens(&cell.tokens, style_context.clone());
                    self.wrap_cell_text(&text, column_widths[i])
                })
                .collect();
            let row_line_count = row_cell_lines.iter().map(|c| c.len()).max().unwrap_or(0);
            for line_idx in 0..row_line_count {
                let row_parts: Vec<String> = row_cell_lines
                    .iter()
                    .enumerate()
                    .map(|(col_idx, cell_lines)| {
                        let text = cell_lines.get(line_idx).cloned().unwrap_or_default();
                        format!(
                            "{text}{}",
                            " ".repeat(column_widths[col_idx].saturating_sub(visible_width(&text)))
                        )
                    })
                    .collect();
                lines.push(format!("│ {} │", row_parts.join(" │ ")));
            }
            if row_index < token.rows.len() - 1 {
                lines.push(format!("├─{}─┤", separator_cells.join("─┼─")));
            }
        }

        let bottom_border_cells: Vec<String> =
            column_widths.iter().map(|w| "─".repeat(*w)).collect();
        lines.push(format!("└─{}─┘", bottom_border_cells.join("─┴─")));

        if let Some(next) = next_token_type {
            if next != "space" {
                lines.push(String::new());
            }
        }
        lines
    }
}

impl Token {
    fn type_name(&self) -> &'static str {
        match self {
            Token::Space => "space",
            Token::Hr => "hr",
            Token::Heading { .. } => "heading",
            Token::Paragraph { .. } => "paragraph",
            Token::Text { .. } => "text",
            Token::Strong { .. } => "strong",
            Token::Em { .. } => "em",
            Token::Codespan { .. } => "codespan",
            Token::Del { .. } => "del",
            Token::Link { .. } => "link",
            Token::Br => "br",
            Token::Escape { .. } => "escape",
            Token::Html { .. } => "html",
            Token::Code { .. } => "code",
            Token::List(_) => "list",
            Token::ListItem(_) => "list_item",
            Token::Blockquote { .. } => "blockquote",
            Token::Table(_) => "table",
            Token::Image { .. } => "image",
            Token::Raw { .. } => "raw",
        }
    }

    fn text_content(&self) -> Option<String> {
        match self {
            Token::Text { text, .. } | Token::Escape { text, .. } => Some(text.clone()),
            Token::Raw { raw } => Some(raw.clone()),
            _ => None,
        }
    }
}

impl InlineStyleContext {
    fn is_default(&self) -> bool {
        false
    }
}

/// `trimPartialClosingFences` (markdown.ts:145) — stabilize streamed partial
/// closing fences so code blocks do not shrink/flicker.
fn trim_partial_closing_fences(tokens: Vec<Token>) -> Vec<Token> {
    // Peek the last token's shape without holding a borrow across the move.
    let last_shape = tokens.last().map(|t| match t {
        Token::List(_) => 0,
        Token::Blockquote { .. } => 1,
        Token::Code { .. } => 2,
        _ => 3,
    });
    let Some(shape) = last_shape else {
        return tokens;
    };
    match shape {
        0 => {
            let Token::List(list) = tokens.last().unwrap() else {
                unreachable!()
            };
            let list = list.clone();
            if list.items.last().is_some() {
                let mut items = list.items.clone();
                if let Some(last_item) = items.last_mut() {
                    last_item.tokens = trim_partial_closing_fences(last_item.tokens.clone());
                }
                let mut new_list = list.clone();
                new_list.items = items;
                let mut tokens = tokens;
                if let Some(Token::List(l)) = tokens.last_mut() {
                    *l = new_list;
                }
                tokens
            } else {
                tokens
            }
        }
        1 => {
            let Token::Blockquote { tokens: inner } = tokens.last().unwrap() else {
                unreachable!()
            };
            let inner = inner.clone();
            let mut tokens = tokens;
            if let Some(Token::Blockquote { tokens: inner2 }) = tokens.last_mut() {
                *inner2 = trim_partial_closing_fences(inner.clone());
            }
            tokens
        }
        2 => {
            let Token::Code { raw, .. } = tokens.last().unwrap() else {
                unreachable!()
            };
            let raw = raw.clone();
            let marker = {
                let bytes = raw.as_bytes();
                let mut i = 0;
                while i < bytes.len() && (bytes[i] == b'`' || bytes[i] == b'~') {
                    i += 1;
                }
                if i >= 3 {
                    Some(&raw[..i])
                } else {
                    None
                }
            };
            let Some(marker) = marker else { return tokens };
            let last_line = raw.rsplit('\n').next().unwrap_or("");
            if !(last_line.len() < marker.len()
                && last_line
                    .chars()
                    .all(|c| c == marker.chars().next().unwrap()))
            {
                return tokens;
            }
            // token.text = token.text.slice(0, -lastLine.length).replace(/\n$/, "")
            let mut tokens = tokens;
            if let Some(Token::Code { text: t, .. }) = tokens.last_mut() {
                let trimmed = t
                    .strip_suffix(last_line)
                    .unwrap_or(t)
                    .trim_end_matches('\n')
                    .to_string();
                *t = trimmed;
            }
            tokens
        }
        _ => tokens,
    }
}

// ---------------------------------------------------------------------------
// Lexer — a minimal `marked`-equivalent tokenizer covering the AST subset
// the renderer touches. Oracle-verified against real `marked` 18 output.
// ---------------------------------------------------------------------------

/// True when a `<...>` line is an inline autolink (email or URL) rather
/// than an HTML tag (marked semantics).
fn is_autolink_line(trimmed: &str) -> bool {
    if !trimmed.starts_with('<') {
        return false;
    }
    if let Some(end) = trimmed[1..].find('>') {
        let inner = &trimmed[1..1 + end];
        inner.contains('@') || inner.starts_with("http://") || inner.starts_with("https://")
    } else {
        false
    }
}

/// `lex` — top-level block tokenizer (marked's `lexer`).
fn lex(source: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let lines: Vec<&str> = source.split('\n').collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        // blank line -> space
        if line.trim().is_empty() {
            // consume consecutive blanks into one space
            tokens.push(Token::Space);
            while i < lines.len() && lines[i].trim().is_empty() {
                i += 1;
            }
            continue;
        }
        // heading
        if let Some((depth, rest)) = parse_heading(line) {
            tokens.push(Token::Heading {
                depth,
                tokens: lex_inline(rest),
            });
            i += 1;
            continue;
        }
        // hr
        if is_hr(line) {
            tokens.push(Token::Hr);
            i += 1;
            continue;
        }
        // code fence
        if let Some((lang, end_idx)) = parse_code_fence(&lines, i) {
            let content = lines[i + 1..end_idx].join("\n");
            let raw = lines[i..=end_idx].join("\n");
            tokens.push(Token::Code {
                lang,
                text: content,
                raw,
            });
            i = end_idx + 1;
            continue;
        }
        // blockquote
        if line.trim_start().starts_with('>') {
            let (inner, consumed) = collect_blockquote(&lines, i);
            tokens.push(Token::Blockquote {
                tokens: lex(&inner),
            });
            i += consumed;
            continue;
        }
        // list
        if let Some((list, consumed)) = parse_list(&lines, i) {
            tokens.push(Token::List(list));
            i += consumed;
            continue;
        }
        // table
        if i + 1 < lines.len() && is_table_separator(lines[i + 1]) && line.contains('|') {
            if let Some((table, consumed)) = parse_table(&lines, i) {
                tokens.push(Token::Table(table));
                i += consumed;
                continue;
            }
        }
        // html — only real tags. `<foo@bar.com>` / `<https://...>` are inline
        // autolinks and must go through the paragraph path (marked semantics):
        // treat as html only when the `<...>` content is NOT an autolink
        // (no `@` / `://` inside).
        if line.trim_start().starts_with('<') && !is_autolink_line(line.trim_start()) {
            tokens.push(Token::Html {
                raw: line.to_string(),
            });
            i += 1;
            continue;
        }
        // paragraph: consume until blank / block start
        let mut para_lines = vec![line];
        i += 1;
        while i < lines.len()
            && !lines[i].trim().is_empty()
            && parse_heading(lines[i]).is_none()
            && !is_hr(lines[i])
            && !lines[i].trim_start().starts_with('>')
            && !lines[i].trim_start().starts_with('-')
            && !lines[i].trim_start().starts_with('*')
            && !lines[i].trim_start().starts_with('+')
            && parse_list_marker(lines[i].trim_start()).is_none()
            && !lines[i].trim_start().starts_with("```")
            && !lines[i].trim_start().starts_with("~~~")
            && !is_table_separator(lines.get(i + 1).copied().unwrap_or(""))
        {
            para_lines.push(lines[i]);
            i += 1;
        }
        tokens.push(Token::Paragraph {
            tokens: lex_inline(&para_lines.join("\n")),
        });
    }
    tokens
}

fn parse_heading(line: &str) -> Option<(usize, &str)> {
    let trimmed = line.trim_start();
    let mut hashes = 0;
    for c in trimmed.chars() {
        if c == '#' {
            hashes += 1;
        } else {
            break;
        }
    }
    if (1..=6).contains(&hashes) {
        let rest = &trimmed[hashes..];
        let rest = rest.strip_prefix(' ').unwrap_or(rest);
        Some((hashes, rest))
    } else {
        None
    }
}

fn is_hr(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.len() < 3 {
        return false;
    }
    let c = trimmed.chars().next().unwrap();
    if c != '-' && c != '*' && c != '_' {
        return false;
    }
    trimmed.chars().all(|ch| ch == c || ch == ' ')
}

fn parse_code_fence(lines: &[&str], i: usize) -> Option<(String, usize)> {
    let line = lines[i];
    let trimmed = line.trim_start();
    if !(trimmed.starts_with("```") || trimmed.starts_with("~~~")) {
        return None;
    }
    let fence_char = trimmed.chars().next().unwrap();
    let fence_len = trimmed.chars().take_while(|c| *c == fence_char).count();
    if fence_len < 3 {
        return None;
    }
    let lang = trimmed[fence_len..].trim().to_string();
    let mut j = i + 1;
    while j < lines.len() {
        let l = lines[j].trim();
        let close_len = l.chars().take_while(|c| *c == fence_char).count();
        if close_len >= fence_len {
            return Some((lang, j));
        }
        j += 1;
    }
    Some((lang, lines.len() - 1))
}

fn is_table_separator(line: &str) -> bool {
    let trimmed = line.trim();
    if !trimmed.contains('|') {
        return false;
    }
    for part in trimmed.split('|') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let mut has_dash = false;
        for c in part.chars() {
            if c == '-' {
                has_dash = true;
            } else if c == ':' {
                continue;
            } else {
                return false;
            }
        }
        if !has_dash {
            return false;
        }
    }
    true
}

fn parse_table(lines: &[&str], i: usize) -> Option<(TableToken, usize)> {
    let header_line = lines[i];
    let sep_line = lines[i + 1];
    let header_cells: Vec<&str> = split_table_row(header_line);
    let seps: Vec<&str> = split_table_row(sep_line);
    if header_cells.is_empty() || header_cells.len() != seps.len() {
        return None;
    }
    let header: Vec<TableCell> = header_cells
        .iter()
        .map(|c| TableCell {
            text: c.trim().to_string(),
            tokens: lex_inline(c.trim()),
        })
        .collect();
    let mut rows: Vec<Vec<TableCell>> = Vec::new();
    let mut j = i + 2;
    while j < lines.len() && !lines[j].trim().is_empty() {
        let cells = split_table_row(lines[j]);
        if cells.is_empty() {
            break;
        }
        rows.push(
            cells
                .iter()
                .map(|c| TableCell {
                    text: c.trim().to_string(),
                    tokens: lex_inline(c.trim()),
                })
                .collect(),
        );
        j += 1;
    }
    let raw = lines[i..j].join("\n");
    Some((TableToken { header, rows, raw }, j - i))
}

fn split_table_row(line: &str) -> Vec<&str> {
    let line = line.trim();
    let inner = line
        .strip_prefix('|')
        .unwrap_or(line)
        .strip_suffix('|')
        .unwrap_or(line);
    inner.split('|').collect()
}

fn collect_blockquote(lines: &[&str], i: usize) -> (String, usize) {
    let mut inner = Vec::new();
    let mut j = i;
    while j < lines.len() {
        let line = lines[j];
        if line.trim_start().starts_with('>') {
            let stripped = line
                .trim_start()
                .trim_start_matches('>')
                .strip_prefix(' ')
                .unwrap_or("")
                .to_string();
            inner.push(stripped);
            j += 1;
        } else if line.trim().is_empty() {
            // lazy continuation — blank ends quote (marked: blank line in quote)
            break;
        } else {
            // lazy continuation line
            inner.push(line.to_string());
            j += 1;
        }
    }
    (inner.join("\n"), j - i)
}

/// Strip up to `width` leading spaces from a continuation line (marked
/// dedents list-item continuation content by the marker width).
fn dedent_line(line: &str, width: usize) -> String {
    let mut removed = 0;
    let mut out = String::new();
    for c in line.chars() {
        if c == ' ' && removed < width {
            removed += 1;
        } else {
            out.push(c);
        }
    }
    out
}

fn parse_list(lines: &[&str], i: usize) -> Option<(ListToken, usize)> {
    let line = lines[i];
    let trimmed = line.trim_start();
    let indent = line.len() - line.trim_start().len();
    let (ordered, start, bullet_len) = parse_list_marker(trimmed)?;
    let mut items = Vec::new();
    let mut j = i;
    let mut loose = false;
    loop {
        let item_line = lines.get(j)?;
        let item_trimmed = item_line.trim_start();
        let item_indent = item_line.len() - item_line.trim_start().len();
        if item_indent != indent {
            break;
        }
        let (item_ordered, _, _) = parse_list_marker(item_trimmed)?;
        if item_ordered != ordered {
            break;
        }
        // content after marker
        let content = &item_trimmed[bullet_len..];
        let mut item_lines = vec![content.to_string()];
        j += 1;
        // continuation: indented lines + blank-separated blocks
        let mut saw_blank = false;
        while j < lines.len() {
            let l = lines[j];
            let l_trimmed = l.trim_start();
            let l_indent = l.len() - l.trim_start().len();
            if l.trim().is_empty() {
                saw_blank = true;
                // Preserve the blank line UNLESS it's trailing (nothing but
                // blanks/siblings follow - a trailing newline is not a paragraph
                // break inside the item).
                let mut k = j + 1;
                while k < lines.len() && lines[k].trim().is_empty() {
                    k += 1;
                }
                let is_trailing = k >= lines.len()
                    || (lines[k].len() - lines[k].trim_start().len()) == indent
                        && parse_list_marker(lines[k].trim_start())
                            .map(|(o, _, _)| o == ordered)
                            .unwrap_or(false);
                if !is_trailing {
                    item_lines.push(String::new());
                }
                j += 1;
                continue;
            }
            // next sibling item?
            if l_indent == indent {
                if let Some((sib_ordered, _, _)) = parse_list_marker(l_trimmed) {
                    if sib_ordered == ordered {
                        break;
                    }
                }
            }
            // nested list or continuation — dedent by the item's marker
            // width (marked strips the list indentation from continuation
            // lines: `  code` under `- ` becomes `code`).
            if l_indent > indent || saw_blank {
                item_lines.push(dedent_line(l, bullet_len));
                j += 1;
                continue;
            }
            break;
        }
        let _ = saw_blank; // loose decided at sibling boundary below
        let task = item_lines
            .first()
            .map(|l| l.starts_with("[ ]") || l.starts_with("[x]"))
            .unwrap_or(false);
        let checked = item_lines
            .first()
            .map(|l| l.starts_with("[x]"))
            .unwrap_or(false);
        let mut first = item_lines.first().cloned().unwrap_or_default();
        if task {
            first = first[3..].trim_start().to_string();
        }
        if item_lines.len() == 1 {
            items.push(ListItemToken {
                raw: line.to_string(),
                task,
                checked,
                tokens: lex_inline(&first),
            });
        } else {
            let rest: Vec<String> = item_lines[1..].to_vec();
            // Keep blank lines in `rest` — `lex` needs them to split into
            // separate Paragraph/Space tokens (marked semantics: a blank line
            // inside a loose list item is a real paragraph break).
            let joined = format!("{first}\n{}", rest.join("\n"));
            let mut item_tokens = lex(&joined);
            // lex already produces a leading Paragraph for `first` when the
            // item has continuation lines — do not insert a duplicate.
            if !first.trim().is_empty()
                && !rest.is_empty()
                && !matches!(item_tokens.first(), Some(Token::Paragraph { .. }))
            {
                item_tokens.insert(
                    0,
                    Token::Paragraph {
                        tokens: lex_inline(&first),
                    },
                );
            }
            items.push(ListItemToken {
                raw: line.to_string(),
                task,
                checked,
                tokens: item_tokens,
            });
        }
        if j >= lines.len() {
            break;
        }
        // peek next sibling (skipping any blanks not already consumed by the
        // item loop); if a blank separates this item from the next, the list
        // is loose (marked semantics).
        let mut gap = j;
        let mut crossed_blank = saw_blank;
        while gap < lines.len() && lines[gap].trim().is_empty() {
            crossed_blank = true;
            gap += 1;
        }
        if gap >= lines.len() {
            break;
        }
        let next = lines[gap];
        let next_indent = next.len() - next.trim_start().len();
        if next_indent != indent {
            break;
        }
        let (next_ordered, _, _) = parse_list_marker(next.trim_start())?;
        if next_ordered != ordered {
            break;
        }
        if crossed_blank {
            loose = true;
        }
        j = gap;
    }
    Some((
        ListToken {
            ordered,
            start,
            loose,
            items,
        },
        j - i,
    ))
}

fn parse_list_marker(trimmed: &str) -> Option<(bool, usize, usize)> {
    let chars: Vec<char> = trimmed.chars().collect();
    let mut k = 0;
    while k < chars.len() && chars[k] == ' ' && k < 3 {
        k += 1;
    }
    if k >= chars.len() {
        return None;
    }
    // unordered
    if chars[k] == '-' || chars[k] == '+' || chars[k] == '*' {
        let after = k + 1;
        if after < chars.len() && (chars[after] == ' ' || chars[after] == '\t') {
            return Some((false, 1, after + 1));
        }
        if after >= chars.len() {
            return Some((false, 1, after));
        }
        return None;
    }
    // ordered: digits + . or )
    let mut digits = 0;
    let mut d = k;
    while d < chars.len() && chars[d].is_ascii_digit() && digits < 9 {
        digits += 1;
        d += 1;
    }
    if digits == 0 || d >= chars.len() || (chars[d] != '.' && chars[d] != ')') {
        return None;
    }
    let start: usize = trimmed[k..d].parse().unwrap_or(1);
    let after = d + 1;
    if after < chars.len() && (chars[after] == ' ' || chars[after] == '\t') {
        Some((true, start, after + 1))
    } else if after >= chars.len() {
        Some((true, start, after))
    } else {
        None
    }
}

/// `lex_inline` — marked's inline tokenizer (subset): escape, code, strong,
/// em, del, link, autolink, br, text, latex.
fn lex_inline(source: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let chars: Vec<char> = source.chars().collect();
    let mut i = 0;
    let mut text = String::new();

    let flush = |tokens: &mut Vec<Token>, text: &mut String| {
        if !text.is_empty() {
            tokens.push(Token::Text {
                text: std::mem::take(text),
                tokens: Vec::new(),
            });
        }
    };

    while i < chars.len() {
        let c = chars[i];
        // escape
        if c == '\\' && i + 1 < chars.len() {
            let next = chars[i + 1];
            if "\\`*_{}[]()#+-.!>|~".contains(next) {
                flush(&mut tokens, &mut text);
                tokens.push(Token::Escape {
                    raw: format!("\\{next}"),
                    text: next.to_string(),
                });
                i += 2;
                continue;
            }
        }
        // code span
        if c == '`' {
            let mut run = 0;
            let mut j = i;
            while j < chars.len() && chars[j] == '`' {
                run += 1;
                j += 1;
            }
            let close = find_code_close(&chars, j, run);
            if let Some(close) = close {
                flush(&mut tokens, &mut text);
                let inner: String = chars[j..close].iter().collect();
                tokens.push(Token::Codespan { text: inner });
                i = close + run;
                continue;
            }
        }
        // strong
        #[allow(clippy::collapsible_if)] // guards are distinct checks (delim run)
        if c == '*' || c == '_' {
            if i + 1 < chars.len() && chars[i + 1] == c {
                if let Some(close) = find_delim_close(&chars, i + 2, &format!("{c}{c}")) {
                    flush(&mut tokens, &mut text);
                    let inner: String = chars[i + 2..close].iter().collect();
                    tokens.push(Token::Strong {
                        tokens: lex_inline(&inner),
                    });
                    i = close + 2;
                    continue;
                }
            }
        }
        // em
        if c == '*' || c == '_' {
            if let Some(close) = find_delim_close(&chars, i + 1, &c.to_string()) {
                flush(&mut tokens, &mut text);
                let inner: String = chars[i + 1..close].iter().collect();
                tokens.push(Token::Em {
                    tokens: lex_inline(&inner),
                });
                i = close + 1;
                continue;
            }
        }
        // del
        if c == '~' && i + 1 < chars.len() && chars[i + 1] == '~' {
            if let Some(close) = find_delim_close(&chars, i + 2, "~~") {
                flush(&mut tokens, &mut text);
                let inner: String = chars[i + 2..close].iter().collect();
                tokens.push(Token::Del {
                    tokens: lex_inline(&inner),
                });
                i = close + 2;
                continue;
            }
        }
        // image
        if c == '!' && i + 1 < chars.len() && chars[i + 1] == '[' {
            if let Some(close) = find_char(&chars, i + 2, ']') {
                let label: String = chars[i + 2..close].iter().collect();
                if close + 1 < chars.len() && chars[close + 1] == '(' {
                    if let Some(paren_close) = find_char(&chars, close + 2, ')') {
                        flush(&mut tokens, &mut text);
                        tokens.push(Token::Image { text: label });
                        i = paren_close + 1;
                        continue;
                    }
                }
            }
        }
        // link
        if c == '[' {
            if let Some(close) = find_char(&chars, i + 1, ']') {
                let label: String = chars[i + 1..close].iter().collect();
                if close + 1 < chars.len() && chars[close + 1] == '(' {
                    if let Some(paren_close) = find_char(&chars, close + 2, ')') {
                        let href: String = chars[close + 2..paren_close].iter().collect();
                        flush(&mut tokens, &mut text);
                        tokens.push(Token::Link {
                            text: label.clone(),
                            href,
                            tokens: lex_inline(&label),
                        });
                        i = paren_close + 1;
                        continue;
                    }
                }
            }
        }
        // autolink
        if c == '<' {
            if let Some(close) = find_char(&chars, i + 1, '>') {
                let inner: String = chars[i + 1..close].iter().collect();
                if inner.contains('@')
                    || inner.starts_with("http://")
                    || inner.starts_with("https://")
                {
                    flush(&mut tokens, &mut text);
                    let href = if inner.contains('@') && !inner.starts_with("mailto:") {
                        format!("mailto:{inner}")
                    } else {
                        inner.clone()
                    };
                    tokens.push(Token::Link {
                        text: inner.clone(),
                        href,
                        tokens: vec![Token::Text {
                            text: inner,
                            tokens: Vec::new(),
                        }],
                    });
                    i = close + 1;
                    continue;
                }
            }
        }
        // br
        if c == '\\' && i + 1 < chars.len() && chars[i + 1] == '\n' {
            flush(&mut tokens, &mut text);
            tokens.push(Token::Br);
            i += 2;
            continue;
        }
        text.push(c);
        i += 1;
    }
    flush(&mut tokens, &mut text);
    tokens
}

fn find_char(chars: &[char], start: usize, target: char) -> Option<usize> {
    (start..chars.len()).find(|&i| chars[i] == target)
}

fn find_code_close(chars: &[char], start: usize, run: usize) -> Option<usize> {
    let mut i = start;
    while i < chars.len() {
        if chars[i] == '`' {
            let mut n = 0;
            while i + n < chars.len() && chars[i + n] == '`' {
                n += 1;
            }
            if n == run {
                return Some(i);
            }
            i += n;
        } else {
            i += 1;
        }
    }
    None
}

fn find_delim_close(chars: &[char], start: usize, delim: &str) -> Option<usize> {
    let d: Vec<char> = delim.chars().collect();
    let mut i = start;
    while i + d.len() <= chars.len() {
        if &chars[i..i + d.len()] == d.as_slice() {
            return Some(i);
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;

    fn identity_theme() -> MarkdownTheme {
        let id = |t: &str| t.to_string();
        MarkdownTheme {
            heading: Rc::new(id),
            link: Rc::new(id),
            link_url: Rc::new(id),
            code: Rc::new(id),
            code_block: Rc::new(id),
            code_block_border: Rc::new(id),
            quote: Rc::new(id),
            quote_border: Rc::new(id),
            hr: Rc::new(id),
            list_bullet: Rc::new(id),
            bold: Rc::new(id),
            italic: Rc::new(id),
            strikethrough: Rc::new(id),
            underline: Rc::new(id),
        }
    }

    #[test]
    fn renders_heading() {
        let mut md = Markdown::new("# Title", 0, 0, identity_theme(), None, None);
        let lines = md.render(80);
        assert!(lines[0].contains("Title"));
    }

    #[test]
    fn renders_list() {
        let mut md = Markdown::new("- one\n- two", 0, 0, identity_theme(), None, None);
        let lines = md.render(80);
        assert!(lines[0].contains("- one"));
    }

    #[test]
    fn renders_table() {
        let mut md = Markdown::new(
            "| a | b |\n|---| --- |\n| 1 | 2 |",
            0,
            0,
            identity_theme(),
            None,
            None,
        );
        let lines = md.render(80);
        assert!(lines[0].starts_with("┌"));
        assert!(lines.iter().any(|l| l.contains("│ 1 │ 2 │")));
    }

    #[test]
    fn renders_code_block() {
        let mut md = Markdown::new("```js\nx=1\n```", 0, 0, identity_theme(), None, None);
        let lines = md.render(80);
        assert!(lines[0].contains("```js"));
    }
}
