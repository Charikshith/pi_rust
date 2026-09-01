//! `latex.ts` (1380 lines) — render a basic LaTeX math expression as
//! terminal-friendly Unicode text. Ported 1:1 from the oracle `../pi`
//! checkout (`packages/tui/src/latex.ts`).
//!
//! The TS source is a symbol table + one `LatexParser` class. This port
//! keeps the same structure: `render_latex(source, options)` entry + a
//! `LatexParser` struct. Sentinel characters (`\u{f0000}`..`\u{f0005}`) are
//! the TS's private markers and never appear in real input.
//!
//! Correctness bar: oracle goldens (`tests/fixtures/pi/tui/markdown.cases.jsonl`,
//! which include inline + display LaTeX) + unit tests below, verified against
//! real Pi `renderLatex` output.

use std::collections::{HashMap, HashSet};

use crate::utils::visible_width;

const LAYOUT_MARKER_START: char = '\u{f0000}';
const LAYOUT_MARKER_END: char = '\u{f0001}';
const PROTECTED_SPACE: &str = "\u{f0002}";
const NEGATIVE_SPACE: char = '\u{0}';
const NAMED_OPERATOR_START: char = '\u{f0004}';
const NAMED_OPERATOR_END: char = '\u{f0005}';

/// `replaceCharacters` — map every char via `replacements`; None if any char missing.
fn replace_characters(value: &str, replacements: &HashMap<&str, &str>) -> Option<String> {
    let mut result = String::new();
    for character in value.chars() {
        let key = character.to_string();
        let repl = replacements.get(key.as_str())?;
        result.push_str(repl);
    }
    Some(result)
}

/// `formatScript` (latex.ts): `value.replace(/\s*([=+-])\s*/g, "$1")` then
/// map each char to superscript/subscript.
fn format_script(value: &str, kind: char) -> String {
    let value = value.trim();
    let replacements = if kind == 's' {
        subscripts()
    } else {
        superscripts()
    };

    // TS: value.replace(/\s*([=+-])\s*/g, "$1")
    let mut stripped = String::new();
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '=' || ch == '+' || ch == '-' {
            // strip surrounding whitespace
            stripped.push(ch);
            while let Some(&n) = chars.peek() {
                if n.is_whitespace() {
                    chars.next();
                } else {
                    break;
                }
            }
        } else {
            stripped.push(ch);
        }
    }

    if let Some(unicode) = replace_characters(&stripped, &replacements) {
        return unicode;
    }

    let prefix = if kind == 's' { "_" } else { "^" };
    let char_len = value.chars().count();
    if char_len == 1 || (kind == 's' && value.chars().all(|c| c.is_ascii_alphabetic())) {
        return format!("{prefix}{value}");
    }
    format!("{prefix}({value})")
}

/// `formatFraction`.
fn format_fraction(numerator: &str, denominator: &str) -> String {
    let numerator = numerator.trim();
    let denominator = denominator.trim();
    let simple_numerator = numerator.chars().all(|c| c.is_alphanumeric() || c == '.');
    let simple_denominator = denominator.chars().all(|c| c.is_alphanumeric() || c == '.')
        || denominator.chars().count() == 1;
    let num = if simple_numerator {
        numerator.to_string()
    } else {
        format!("({numerator})")
    };
    let den = if simple_denominator {
        denominator.to_string()
    } else {
        format!("({denominator})")
    };
    format!("{num}/{den}")
}

/// `formatRoot`.
fn format_root(value: &str, symbol: &str) -> String {
    let value = value.trim();
    if value.chars().all(|c| c.is_alphanumeric() || c == '.') {
        format!("{symbol}{value}")
    } else {
        format!("{symbol}({value})")
    }
}

fn pad_layout_line(line: &str, width: usize, centered: bool) -> String {
    let padding = width.saturating_sub(visible_width(line));
    let left = if centered { padding / 2 } else { 0 };
    format!("{}{}{}", " ".repeat(left), line, " ".repeat(padding - left))
}

#[derive(Clone)]
enum LayoutNode {
    Fraction {
        numerator: String,
        denominator: String,
    },
    Operator {
        operator: String,
        lower: Option<String>,
        upper: Option<String>,
    },
    Matrix {
        lines: Vec<String>,
        baseline: usize,
    },
}

struct Layout {
    lines: Vec<String>,
    width: usize,
    baseline: usize,
}

fn join_layouts(layouts: &[Layout]) -> Layout {
    if layouts.is_empty() {
        return Layout {
            lines: vec![String::new()],
            width: 0,
            baseline: 0,
        };
    }
    let baseline = layouts.iter().map(|l| l.baseline).max().unwrap_or(0);
    let below = layouts
        .iter()
        .map(|l| l.lines.len().saturating_sub(l.baseline + 1))
        .max()
        .unwrap_or(0);
    let mut lines: Vec<String> = Vec::new();
    for row in 0..=baseline + below {
        let mut line = String::new();
        for layout in layouts {
            let source_row = row as isize - baseline as isize + layout.baseline as isize;
            if source_row >= 0 && source_row < layout.lines.len() as isize {
                line.push_str(&pad_layout_line(
                    &layout.lines[source_row as usize],
                    layout.width,
                    false,
                ));
            } else {
                line.push_str(&" ".repeat(layout.width));
            }
        }
        lines.push(line.trim_end().to_string());
    }
    Layout {
        lines,
        width: layouts.iter().map(|l| l.width).sum(),
        baseline,
    }
}

/// `renderLayout` (latex.ts) — resolve `\u{f0000}N\u{f0001}` markers against
/// layout nodes, stacking fractions/operators/matrices vertically.
fn render_layout(source: &str, nodes: &[LayoutNode]) -> Layout {
    let mut rendered_lines: Vec<String> = Vec::new();
    let mut first_baseline = 0;
    for source_line in source.split('\n') {
        let mut layouts: Vec<Layout> = Vec::new();
        let mut position = 0;
        let mut previous_node: Option<&LayoutNode> = None;
        let chars: Vec<char> = source_line.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            // find next LAYOUT_MARKER_START + digits + LAYOUT_MARKER_END
            if chars[i] == LAYOUT_MARKER_START {
                let mut j = i + 1;
                while j < chars.len() && chars[j].is_ascii_digit() {
                    j += 1;
                }
                if j > i + 1 && j < chars.len() && chars[j] == LAYOUT_MARKER_END {
                    let digits: String = chars[i + 1..j].iter().collect();
                    let node_index: usize = digits.parse().unwrap_or(0);
                    let marker_len = j - i + 1;
                    if let Some(node) = nodes.get(node_index) {
                        if i > position {
                            let sliced: String = chars[position..i].iter().collect();
                            let trimmed = if previous_node.is_some() {
                                sliced.trim_start()
                            } else {
                                sliced.as_str()
                            }
                            .trim_end();
                            let preserve_leading =
                                matches!(previous_node, Some(LayoutNode::Matrix { .. }))
                                    && sliced.starts_with(' ');
                            let preserve_trailing =
                                matches!(node, LayoutNode::Matrix { .. }) && sliced.ends_with(' ');
                            let text = if !trimmed.is_empty() {
                                format!(
                                    "{}{}{}",
                                    if preserve_leading { " " } else { "" },
                                    trimmed,
                                    if preserve_trailing { " " } else { "" }
                                )
                            } else if preserve_leading || preserve_trailing {
                                " ".to_string()
                            } else {
                                String::new()
                            };
                            layouts.push(Layout {
                                lines: vec![text.clone()],
                                width: visible_width(&text),
                                baseline: 0,
                            });
                        }
                        match node {
                            LayoutNode::Fraction {
                                numerator,
                                denominator,
                            } => {
                                let numerator_layout = render_layout(numerator, nodes);
                                let denominator_layout = render_layout(denominator, nodes);
                                let content_width =
                                    numerator_layout.width.max(denominator_layout.width).max(1);
                                let width = content_width + 2;
                                let mut lines: Vec<String> = numerator_layout
                                    .lines
                                    .iter()
                                    .map(|line| pad_layout_line(line, width, true))
                                    .collect();
                                lines.push(format!(" {} ", "─".repeat(content_width)));
                                lines.extend(
                                    denominator_layout
                                        .lines
                                        .iter()
                                        .map(|line| pad_layout_line(line, width, true)),
                                );
                                layouts.push(Layout {
                                    lines,
                                    width,
                                    baseline: numerator_layout.lines.len(),
                                });
                            }
                            LayoutNode::Operator {
                                operator,
                                lower,
                                upper,
                            } => {
                                let content_width = visible_width(operator)
                                    .max(lower.as_ref().map(|l| visible_width(l)).unwrap_or(0))
                                    .max(upper.as_ref().map(|u| visible_width(u)).unwrap_or(0));
                                let mut lines: Vec<String> = Vec::new();
                                if let Some(u) = upper {
                                    lines.push(format!(
                                        "{} ",
                                        pad_layout_line(u, content_width, true)
                                    ));
                                }
                                lines.push(format!(
                                    "{} ",
                                    pad_layout_line(operator, content_width, true)
                                ));
                                if let Some(l) = lower {
                                    lines.push(format!(
                                        "{} ",
                                        pad_layout_line(l, content_width, true)
                                    ));
                                }
                                layouts.push(Layout {
                                    lines,
                                    width: content_width + 1,
                                    baseline: if upper.is_some() { 1 } else { 0 },
                                });
                            }
                            LayoutNode::Matrix { lines, baseline } => {
                                let width = lines
                                    .iter()
                                    .map(|line| visible_width(line))
                                    .max()
                                    .unwrap_or(0);
                                layouts.push(Layout {
                                    lines: lines
                                        .iter()
                                        .map(|line| pad_layout_line(line, width, false))
                                        .collect(),
                                    width,
                                    baseline: *baseline,
                                });
                            }
                        }
                        position = i + marker_len;
                        previous_node = Some(node);
                        i += marker_len;
                        continue;
                    }
                }
            }
            i += 1;
        }
        if position < chars.len() {
            let sliced: String = chars[position..].iter().collect();
            let trimmed = if previous_node.is_some() {
                sliced.trim_start()
            } else {
                sliced.as_str()
            };
            let text = if matches!(previous_node, Some(LayoutNode::Matrix { .. }))
                && sliced.starts_with(' ')
            {
                format!(" {trimmed}")
            } else {
                trimmed.to_string()
            };
            layouts.push(Layout {
                lines: vec![text.clone()],
                width: visible_width(&text),
                baseline: 0,
            });
        }
        let line_layout = join_layouts(&layouts);
        if rendered_lines.is_empty() {
            first_baseline = line_layout.baseline;
        }
        rendered_lines.extend(line_layout.lines);
    }
    let width = rendered_lines
        .iter()
        .map(|line| visible_width(line))
        .max()
        .unwrap_or(0);
    Layout {
        lines: rendered_lines,
        width,
        baseline: first_baseline,
    }
}

/// `LatexParser` (latex.ts).
struct LatexParser<'a> {
    source: &'a str,
    layout_nodes: Vec<LayoutNode>,
    display: bool,
    position: usize,
    supported: bool,
    stack_fractions: bool,
}

impl<'a> LatexParser<'a> {
    fn new(source: &'a str, display: bool) -> Self {
        Self {
            source,
            layout_nodes: Vec::new(),
            display,
            position: 0,
            supported: true,
            stack_fractions: true,
        }
    }

    fn render(&mut self) -> Option<String> {
        let rendered = self.parse_sequence(None);
        if !self.supported || self.position != self.source.len() {
            return None;
        }
        let layout_nodes = std::mem::take(&mut self.layout_nodes);
        let rendered = normalize_output(&rendered);
        if layout_nodes.is_empty() {
            return Some(rendered.replace(PROTECTED_SPACE, " "));
        }
        let layout = render_layout(&rendered, &layout_nodes);
        let indentation = layout
            .lines
            .iter()
            .filter(|line| !line.trim().is_empty())
            .map(|line| line.len() - line.trim_start().len())
            .min()
            .unwrap_or(0);
        Some(
            layout
                .lines
                .iter()
                .map(|line| line[indentation..].trim_end().to_string())
                .collect::<Vec<String>>()
                .join("\n")
                .trim_end()
                .to_string()
                .replace(PROTECTED_SPACE, " "),
        )
    }

    fn parse_sequence(&mut self, end_character: Option<char>) -> String {
        let mut result = String::new();
        while self.position < self.source.len() {
            let character = self.source[self.position..].chars().next().unwrap();
            if let Some(end) = end_character {
                if character == end {
                    self.position += character.len_utf8();
                    return result;
                }
            }
            match character {
                '}' => {
                    self.supported = false;
                    return result;
                }
                '{' => {
                    self.position += 1;
                    result.push_str(&self.parse_sequence(Some('}')));
                    continue;
                }
                '\\' => {
                    let command = self.parse_command();
                    if command == NEGATIVE_SPACE.to_string() {
                        result = result.trim_end().to_string();
                        if result.ends_with(NAMED_OPERATOR_END) {
                            result.truncate(result.len() - NAMED_OPERATOR_END.len_utf8());
                        }
                    } else {
                        result.push_str(&command);
                    }
                    continue;
                }
                '^' | '_' => {
                    self.position += 1;
                    result = result.trim_end().to_string();
                    let script = format_script(&self.parse_required_argument(false), character);
                    if result.ends_with(NAMED_OPERATOR_END) {
                        result.truncate(result.len() - NAMED_OPERATOR_END.len_utf8());
                        result.push_str(&script);
                        result.push(NAMED_OPERATOR_END);
                    } else {
                        result.push_str(&script);
                    }
                    continue;
                }
                _ if character.is_whitespace() => {
                    result.push_str(&self.parse_whitespace());
                    continue;
                }
                '=' | '<' | '>' => {
                    result = format!("{} {character} ", result.trim_end());
                    self.position += 1;
                    continue;
                }
                '&' => {
                    self.position += 1;
                    continue;
                }
                '~' => {
                    self.position += 1;
                    result.push(' ');
                    continue;
                }
                '.' => {
                    // TRAILING_LAYOUT_MARKER_PATTERN: matrix gets a trailing '.'
                    let marker = self.trailing_layout_marker(&result);
                    if let Some(node_index) = marker {
                        if let Some(LayoutNode::Matrix { lines, .. }) =
                            self.layout_nodes.get_mut(node_index)
                        {
                            let last = lines.len().saturating_sub(1);
                            lines[last].push(character);
                            self.position += 1;
                            continue;
                        }
                    }
                    result.push(character);
                    self.position += 1;
                    continue;
                }
                _ => {
                    result.push(character);
                    self.position += 1;
                }
            }
        }
        if end_character.is_some() {
            self.supported = false;
        }
        result
    }

    /// `TRAILING_LAYOUT_MARKER_PATTERN = /\u{f0000}(\d+)\u{f0001}$/u`
    fn trailing_layout_marker(&self, result: &str) -> Option<usize> {
        let chars: Vec<char> = result.chars().collect();
        if chars.last() != Some(&LAYOUT_MARKER_END) {
            return None;
        }
        let mut j = chars.len() - 1;
        while j > 0 && chars[j - 1].is_ascii_digit() {
            j -= 1;
        }
        if j == 0 || chars[j - 1] != LAYOUT_MARKER_START {
            return None;
        }
        let digits: String = chars[j..chars.len() - 1].iter().collect();
        digits.parse().ok()
    }

    fn parse_whitespace(&mut self) -> String {
        while self.position < self.source.len()
            && self.source[self.position..]
                .chars()
                .next()
                .unwrap()
                .is_whitespace()
        {
            self.position += 1;
        }
        " ".to_string()
    }

    fn parse_command(&mut self) -> String {
        self.position += 1;
        if self.position >= self.source.len() {
            self.supported = false;
            return String::new();
        }
        let mut command = String::new();
        let first = self.source[self.position..].chars().next().unwrap();
        if first == '\n' || first == '\r' {
            self.position += 1;
            if first == '\r' && self.source[self.position..].starts_with('\n') {
                self.position += 1;
            }
            return " ".to_string();
        }
        if first.is_ascii_alphabetic() {
            let start = self.position;
            while self.position < self.source.len()
                && self.source[self.position..]
                    .chars()
                    .next()
                    .unwrap()
                    .is_ascii_alphabetic()
            {
                self.position += 1;
            }
            command = self.source[start..self.position].to_string();
        } else {
            command.push(first);
            self.position += first.len_utf8();
        }

        let c = command.as_str();
        if c == "\\" {
            return "\n".to_string();
        }
        if spacing_commands().contains(c) {
            return " ".to_string();
        }
        if negative_spacing_commands().contains(c) {
            return NEGATIVE_SPACE.to_string();
        }
        if ignored_commands().contains(c) {
            return String::new();
        }
        if ["{", "}", "$", "%", "#", "_", "&"].contains(&c) {
            return command;
        }
        if c == "|" {
            return "‖".to_string();
        }
        if c == "not" {
            let value = self.parse_required_argument(false).trim().to_string();
            if let Some(n) = negated_symbols().get(value.as_str()) {
                return format!(" {n} ");
            }
            let characters: Vec<char> = value.chars().collect();
            if characters.is_empty() {
                self.supported = false;
                return String::new();
            }
            return format!(
                " {}{}{} ",
                characters[0],
                '\u{338}',
                characters[1..].iter().collect::<String>()
            );
        }
        if limit_operators().contains(c) {
            return self.parse_operator(c, "bracket", true, true);
        }
        if let Some(sym) = symbols().get(c) {
            if display_limit_symbols().contains(c) {
                return self.parse_operator(sym, "script", true, false);
            }
            if c == "cdot" || c == "times" || relation_commands().contains(c) {
                return format!(" {sym} ");
            }
            return sym.to_string();
        }
        if named_operators().contains(c) {
            return format!("{NAMED_OPERATOR_START}{command}{NAMED_OPERATOR_END}");
        }
        if size_commands().contains(c) {
            return String::new();
        }
        if ["left", "middle", "right"].contains(&c) {
            if self.source[self.position..].starts_with('.') {
                self.position += 1;
            }
            return String::new();
        }
        if ["frac", "dfrac", "tfrac"].contains(&c) {
            let should_stack = self.display && self.stack_fractions && c != "tfrac";
            let numerator = self.parse_required_argument(!should_stack);
            let denominator = self.parse_required_argument(!should_stack);
            if should_stack {
                let index = self.layout_nodes.len();
                self.layout_nodes.push(LayoutNode::Fraction {
                    numerator: normalize_output(&numerator),
                    denominator: normalize_output(&denominator),
                });
                return format!("{LAYOUT_MARKER_START}{index}{LAYOUT_MARKER_END}");
            }
            return format_fraction(&numerator, &denominator);
        }
        if c == "sqrt" {
            let degree = self.parse_optional_argument();
            let value = self.parse_required_argument(true);
            let degree = degree.map(|d| d.trim().to_string());
            if degree.is_none() || degree.as_deref() == Some("2") {
                return format_root(&value, "√");
            }
            if degree.as_deref() == Some("3") {
                return format_root(&value, "∛");
            }
            if degree.as_deref() == Some("4") {
                return format_root(&value, "∜");
            }
            return format!(
                "{}{}",
                format_script(degree.as_deref().unwrap_or(""), 'u'),
                format_root(&value, "√")
            );
        }
        if ["boxed", "fbox"].contains(&c) {
            return format!("[{}]", self.parse_required_argument(true).trim());
        }
        if ["binom", "dbinom", "tbinom"].contains(&c) {
            return format!(
                "({} choose {})",
                self.parse_required_argument(true),
                self.parse_required_argument(true)
            );
        }
        if let Some(accent) = accents().get(c) {
            let value = self.parse_required_argument(true);
            if value.chars().count() == 1 {
                return format!("{value}{accent}");
            }
            return format!("{c}({value})");
        }
        if c == "mathbb" {
            let value = self.parse_required_argument(true);
            return value
                .chars()
                .map(|ch| {
                    blackboard()
                        .get(ch.to_string().as_str())
                        .copied()
                        .unwrap_or(&ch.to_string())
                        .to_string()
                })
                .collect();
        }
        if c == "operatorname" {
            let starred = self.source[self.position..].starts_with('*');
            if starred {
                self.position += 1;
            }
            let operator = normalize_output(&self.parse_required_argument(true))
                .trim()
                .to_string();
            return self.parse_operator(&operator, "bracket", starred, true);
        }
        if c == "mod" || c == "bmod" {
            return " mod ".to_string();
        }
        if c == "pmod" || c == "pod" {
            let value = self.parse_required_argument(true).trim().to_string();
            return if c == "pmod" {
                format!(" (mod {value})")
            } else {
                format!(" ({value})")
            };
        }
        if c == "overset" || c == "stackrel" {
            let upper = self.parse_required_argument(true);
            let value = self.parse_required_argument(true).trim().to_string();
            return format!("{value}{}", format_script(&upper, 'u'));
        }
        if c == "underset" {
            let lower = self.parse_required_argument(true);
            let value = self.parse_required_argument(true).trim().to_string();
            return format!("{value}{}", format_script(&lower, 's'));
        }
        if plain_wrappers().contains(c) {
            let value = self.parse_required_argument(true);
            if c.starts_with("text") || c == "mbox" {
                return value;
            }
            return value.trim().to_string();
        }
        if c == "begin" {
            return self.parse_environment();
        }
        if c == "end" {
            self.supported = false;
            return String::new();
        }
        self.supported = false;
        format!("\\{command}")
    }

    fn parse_operator(
        &mut self,
        operator: &str,
        inline_lower_style: &str,
        display_limits: bool,
        spaced: bool,
    ) -> String {
        let mut use_display_limits = display_limits;
        let mut modifier_position = self.position;
        while modifier_position < self.source.len()
            && self.source[modifier_position..]
                .chars()
                .next()
                .unwrap()
                .is_ascii_whitespace()
        {
            modifier_position += 1;
        }
        let rest = &self.source[modifier_position..];
        let modifier = if rest.starts_with("\\limits") {
            Some("limits")
        } else if rest.starts_with("\\nolimits") {
            Some("nolimits")
        } else {
            None
        };
        if let Some(m) = modifier {
            use_display_limits = m == "limits";
            self.position = modifier_position + m.len() + 1;
        }

        let mut lower: Option<String> = None;
        let mut upper: Option<String> = None;
        loop {
            let mut script_position = self.position;
            while script_position < self.source.len()
                && self.source[script_position..]
                    .chars()
                    .next()
                    .unwrap()
                    .is_ascii_whitespace()
            {
                script_position += 1;
            }
            let kind = self.source[script_position..].chars().next();
            if kind != Some('_') && kind != Some('^') {
                break;
            }
            self.position = script_position + 1;
            let value = normalize_output(&self.parse_required_argument(false)).replace(' ', "");
            match kind {
                Some('_') => {
                    if lower.is_some() {
                        self.supported = false;
                    }
                    lower = Some(value);
                }
                _ => {
                    if upper.is_some() {
                        self.supported = false;
                    }
                    upper = Some(value);
                }
            }
        }

        if self.display && use_display_limits && (lower.is_some() || upper.is_some()) {
            let index = self.layout_nodes.len();
            self.layout_nodes.push(LayoutNode::Operator {
                operator: operator.to_string(),
                lower,
                upper,
            });
            return format!("{LAYOUT_MARKER_START}{index}{LAYOUT_MARKER_END}");
        }

        let mut rendered = operator.to_string();
        if let Some(l) = lower {
            let lower_str = if inline_lower_style == "bracket" {
                format!("[{l}]")
            } else {
                format_script(&l, 's')
            };
            rendered.push_str(&lower_str);
        }
        if let Some(u) = upper {
            rendered.push_str(&format_script(&u, 'u'));
        }
        if spaced {
            format!(" {rendered} ")
        } else {
            rendered
        }
    }

    fn parse_required_argument(&mut self, stack_fractions: bool) -> String {
        let previous = self.stack_fractions;
        self.stack_fractions = previous && stack_fractions;
        let value = self.parse_required_argument_value();
        self.stack_fractions = previous;
        value
    }

    fn parse_required_argument_value(&mut self) -> String {
        while self.position < self.source.len()
            && self.source[self.position..]
                .chars()
                .next()
                .unwrap()
                .is_whitespace()
        {
            self.position += 1;
        }
        if self.position >= self.source.len() {
            self.supported = false;
            return String::new();
        }
        let next = self.source[self.position..].chars().next().unwrap();
        if next == '{' {
            self.position += 1;
            return self.parse_sequence(Some('}'));
        }
        if next == '\\' {
            return self.parse_command();
        }
        let value = next.to_string();
        self.position += next.len_utf8();
        value
    }

    fn parse_optional_argument(&mut self) -> Option<String> {
        while self.position < self.source.len()
            && self.source[self.position..]
                .chars()
                .next()
                .unwrap()
                .is_ascii_whitespace()
        {
            self.position += 1;
        }
        if !self.source[self.position..].starts_with('[') {
            return None;
        }
        let end = self.source[self.position + 1..]
            .find(']')
            .map(|i| i + self.position + 1);
        let Some(end) = end else {
            self.supported = false;
            return None;
        };
        let value = self.source[self.position + 1..end].to_string();
        self.position = end + 1;
        Some(self.render_nested(&value))
    }

    fn read_raw_group(&mut self) -> Option<String> {
        while self.position < self.source.len()
            && self.source[self.position..]
                .chars()
                .next()
                .unwrap()
                .is_ascii_whitespace()
        {
            self.position += 1;
        }
        if !self.source[self.position..].starts_with('{') {
            self.supported = false;
            return None;
        }
        let start = self.position + 1;
        self.position = start;
        let mut depth = 1;
        while self.position < self.source.len() {
            let character = self.source[self.position..].chars().next().unwrap();
            if character == '\\' {
                self.position += 2;
                continue;
            }
            if character == '{' {
                depth += 1;
            }
            if character == '}' {
                depth -= 1;
            }
            if depth == 0 {
                let value = self.source[start..self.position].to_string();
                self.position += 1;
                return Some(value);
            }
            self.position += character.len_utf8();
        }
        self.supported = false;
        None
    }

    fn split_environment_rows(&self, body: &str) -> Vec<String> {
        // TS: body.split(/\\\\(?:\[[^\]\n]*\])?/)
        let mut rows = Vec::new();
        let mut current = String::new();
        let mut chars = body.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\\' && chars.peek() == Some(&'\\') {
                chars.next();
                if chars.peek() == Some(&'[') {
                    chars.next();
                    while let Some(&n) = chars.peek() {
                        if n == ']' {
                            chars.next();
                            break;
                        }
                        if n == '\n' {
                            break;
                        }
                        chars.next();
                    }
                }
                rows.push(std::mem::take(&mut current));
            } else {
                current.push(c);
            }
        }
        rows.push(current);
        rows
    }

    fn parse_environment(&mut self) -> String {
        let environment = self.read_raw_group();
        let Some(environment) = environment else {
            return String::new();
        };
        let end_marker = format!("\\end{{{environment}}}");
        let end = self.source[self.position..]
            .find(&end_marker)
            .map(|i| i + self.position);
        let Some(end) = end else {
            self.supported = false;
            return String::new();
        };
        let body = self.source[self.position..end].to_string();
        self.position = end + end_marker.len();

        if ["equation", "equation*", "displaymath"].contains(&environment.as_str()) {
            return self.render_nested(&body).trim().to_string();
        }
        if [
            "aligned",
            "align",
            "align*",
            "alignedat",
            "alignat",
            "alignat*",
            "gather",
            "gathered",
            "multline",
            "multline*",
            "split",
        ]
        .contains(&environment.as_str())
        {
            let aligned_at = ["alignedat", "alignat", "alignat*"].contains(&environment.as_str());
            let aligned_body = if aligned_at {
                let trimmed_start = body.trim_start();
                if trimmed_start.starts_with('{') {
                    if let Some(end) = trimmed_start.find('}') {
                        trimmed_start[end + 1..].to_string()
                    } else {
                        body.clone()
                    }
                } else {
                    body.clone()
                }
            } else {
                body.clone()
            };
            return self
                .split_environment_rows(&aligned_body)
                .iter()
                .map(|row| {
                    let cells: Vec<&str> = row.split('&').collect();
                    let source = if aligned_at {
                        (0..cells.len().div_ceil(2))
                            .map(|index| cells[index * 2..index * 2 + 2].join(""))
                            .collect::<Vec<String>>()
                            .join(" ")
                    } else {
                        cells.join("")
                    };
                    self.render_nested(&source).trim().to_string()
                })
                .filter(|s| !s.is_empty())
                .collect::<Vec<String>>()
                .join("\n");
        }
        if ["cases", "cases*"].contains(&environment.as_str()) {
            let rows: Vec<Vec<String>> = self
                .split_environment_rows(&body)
                .iter()
                .map(|row| {
                    row.split('&')
                        .map(|cell| self.render_nested(cell).trim().to_string())
                        .collect()
                })
                .filter(|row: &Vec<String>| row.iter().any(|c| !c.is_empty()))
                .collect();
            return rows
                .iter()
                .enumerate()
                .map(|(index, row)| {
                    let value = row
                        .first()
                        .cloned()
                        .unwrap_or_default()
                        .replace(",", "")
                        .trim_end()
                        .to_string();
                    let condition = row.get(1).cloned().unwrap_or_default();
                    let delimiter = if index == 0 {
                        "⎧"
                    } else if index == rows.len() - 1 {
                        "⎩"
                    } else {
                        "⎨"
                    };
                    let condition_prefix = if condition.starts_with("if")
                        || condition.starts_with("when")
                        || condition.starts_with("for")
                        || condition.starts_with("otherwise")
                    {
                        " "
                    } else {
                        " if "
                    };
                    let tail = if condition.is_empty() {
                        String::new()
                    } else {
                        format!("{condition_prefix}{condition}")
                    };
                    format!("{delimiter} {value}{tail}")
                })
                .collect::<Vec<String>>()
                .join("\n");
        }
        if [
            "array",
            "matrix",
            "smallmatrix",
            "pmatrix",
            "bmatrix",
            "Bmatrix",
            "vmatrix",
            "Vmatrix",
        ]
        .contains(&environment.as_str())
        {
            let matrix_body = if environment == "array" {
                let trimmed_start = body.trim_start();
                if trimmed_start.starts_with('{') {
                    if let Some(end) = trimmed_start.find('}') {
                        trimmed_start[end + 1..].to_string()
                    } else {
                        body.clone()
                    }
                } else {
                    body.clone()
                }
            } else {
                body.clone()
            };
            return self.render_matrix(&environment, &matrix_body);
        }
        self.supported = false;
        body
    }

    fn render_matrix(&mut self, environment: &str, body: &str) -> String {
        let matrix: Vec<Vec<String>> = self
            .split_environment_rows(body)
            .iter()
            .map(|row| {
                row.split('&')
                    .map(|cell| self.render_nested(cell).trim().to_string())
                    .collect()
            })
            .filter(|row: &Vec<String>| row.iter().any(|c| !c.is_empty()))
            .collect();
        let column_count = matrix.iter().map(|row| row.len()).max().unwrap_or(0);
        let column_widths: Vec<usize> = (0..column_count)
            .map(|column| {
                matrix
                    .iter()
                    .map(|row| visible_width(row.get(column).map(String::as_str).unwrap_or("")))
                    .max()
                    .unwrap_or(0)
            })
            .collect();
        let rows: Vec<String> = matrix
            .iter()
            .map(|row| {
                (0..column_count)
                    .map(|column| {
                        let cell = row.get(column).cloned().unwrap_or_default();
                        format!(
                            "{cell}{}",
                            PROTECTED_SPACE
                                .repeat(column_widths[column].saturating_sub(visible_width(&cell)))
                        )
                    })
                    .collect::<Vec<String>>()
                    .join(" │ ")
            })
            .collect();

        let lines: Vec<String>;
        if ["array", "matrix", "smallmatrix"].contains(&environment) {
            lines = rows;
        } else {
            let delimiter: [&str; 6] = match environment {
                "pmatrix" => ["⎛", "⎞", "⎜", "⎟", "⎝", "⎠"],
                "bmatrix" => ["⎡", "⎤", "⎢", "⎥", "⎣", "⎦"],
                "Bmatrix" => ["⎧", "⎫", "⎨", "⎬", "⎩", "⎭"],
                "vmatrix" => ["│", "│", "│", "│", "│", "│"],
                "Vmatrix" => ["║", "║", "║", "║", "║", "║"],
                _ => {
                    self.supported = false;
                    return rows.join("\n");
                }
            };
            lines = rows
                .iter()
                .enumerate()
                .map(|(index, row)| {
                    let (left, right) = if index == 0 {
                        (delimiter[0], delimiter[1])
                    } else if index == rows.len() - 1 {
                        (delimiter[4], delimiter[5])
                    } else {
                        (delimiter[2], delimiter[3])
                    };
                    format!("{left} {row} {right}")
                })
                .collect();
        }
        if lines.len() <= 1 {
            return lines.first().cloned().unwrap_or_default();
        }
        let index = self.layout_nodes.len();
        self.layout_nodes.push(LayoutNode::Matrix {
            lines: lines.clone(),
            baseline: 0,
        });
        format!("{LAYOUT_MARKER_START}{index}{LAYOUT_MARKER_END}")
    }

    fn render_nested(&mut self, source: &str) -> String {
        // Reuse the shared layout node pool so markers keep valid indices.
        let mut nested = LatexParser::new(source, self.display);
        nested.layout_nodes = self.layout_nodes.clone();
        let rendered = nested.render();
        match rendered {
            Some(r) => {
                self.layout_nodes = nested.layout_nodes;
                r
            }
            None => {
                self.supported = false;
                source.to_string()
            }
        }
    }
}

fn normalize_output(value: &str) -> String {
    let mut s = value.to_string();
    // LEFT spacing: (?<=[\p{L}\p{N})\]}\u{f0001}])\u{f0004} -> " "
    let mut out = String::new();
    let chars: Vec<char> = s.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if c == NAMED_OPERATOR_START && i > 0 {
            let prev = chars[i - 1];
            if prev.is_alphanumeric()
                || prev == ')'
                || prev == ']'
                || prev == '}'
                || prev == LAYOUT_MARKER_END
            {
                out.push(' ');
            }
        }
        out.push(c);
    }
    s = out;
    s = s.replace(NAMED_OPERATOR_START, "");
    // RIGHT spacing: \u{f0005}(?=[\p{L}\p{N}√\u{f0000}])
    let mut out2 = String::new();
    let chars: Vec<char> = s.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if c == NAMED_OPERATOR_END && i + 1 < chars.len() {
            let next = chars[i + 1];
            if next.is_alphanumeric() || next == '√' || next == LAYOUT_MARKER_START {
                out2.push(' ');
            }
        }
        out2.push(c);
    }
    s = out2.replace(NAMED_OPERATOR_END, "");
    let lines: Vec<String> = s
        .split('\n')
        .map(|line| {
            line.split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .trim()
                .to_string()
        })
        .collect();
    let filtered: Vec<String> = lines
        .iter()
        .enumerate()
        .filter(|(index, line)| !line.is_empty() || (*index > 0 && *index < lines.len() - 1))
        .map(|(_, line)| line.clone())
        .collect();
    filtered.join("\n").trim().to_string()
}

/// `RenderLatexOptions` (latex.ts).
#[derive(Default, Clone, Copy)]
pub struct RenderLatexOptions {
    /// Stack fractions and operator limits vertically for display math.
    pub display: bool,
}

/// `renderLatex` (latex.ts) — public entry point.
pub fn render_latex(source: &str, options: &RenderLatexOptions) -> Option<String> {
    let mut parser = LatexParser::new(source, options.display);
    parser.render()
}

// ---------------------------------------------------------------------------
// Symbol tables (latex.ts) — kept as module-level functions so they're only
// built once per call site (the TS builds them once at module scope).
// ---------------------------------------------------------------------------

fn symbols() -> HashMap<&'static str, &'static str> {
    let mut m = HashMap::new();
    for (k, v) in [
        ("alpha", "α"),
        ("beta", "β"),
        ("gamma", "γ"),
        ("delta", "δ"),
        ("epsilon", "ϵ"),
        ("varepsilon", "ε"),
        ("zeta", "ζ"),
        ("eta", "η"),
        ("theta", "θ"),
        ("vartheta", "ϑ"),
        ("iota", "ι"),
        ("kappa", "κ"),
        ("varkappa", "ϰ"),
        ("lambda", "λ"),
        ("mu", "μ"),
        ("nu", "ν"),
        ("xi", "ξ"),
        ("pi", "π"),
        ("varpi", "ϖ"),
        ("rho", "ρ"),
        ("varrho", "ϱ"),
        ("sigma", "σ"),
        ("varsigma", "ς"),
        ("tau", "τ"),
        ("upsilon", "υ"),
        ("phi", "ϕ"),
        ("varphi", "φ"),
        ("chi", "χ"),
        ("psi", "ψ"),
        ("omega", "ω"),
        ("Gamma", "Γ"),
        ("Delta", "Δ"),
        ("Theta", "Θ"),
        ("Lambda", "Λ"),
        ("Xi", "Ξ"),
        ("Pi", "Π"),
        ("Sigma", "Σ"),
        ("Upsilon", "Υ"),
        ("Phi", "Φ"),
        ("Psi", "Ψ"),
        ("Omega", "Ω"),
        ("pm", "±"),
        ("mp", "∓"),
        ("times", "×"),
        ("div", "÷"),
        ("cdot", "·"),
        ("ast", "∗"),
        ("star", "⋆"),
        ("circ", "∘"),
        ("bullet", "•"),
        ("oplus", "⊕"),
        ("ominus", "⊖"),
        ("otimes", "⊗"),
        ("oslash", "⊘"),
        ("odot", "⊙"),
        ("bigcirc", "○"),
        ("dagger", "†"),
        ("ddagger", "‡"),
        ("amalg", "⨿"),
        ("uplus", "⊎"),
        ("sqcap", "⊓"),
        ("sqcup", "⊔"),
        ("triangleleft", "◁"),
        ("triangleright", "▷"),
        ("wr", "≀"),
        ("cap", "∩"),
        ("cup", "∪"),
        ("bigcap", "⋂"),
        ("bigcup", "⋃"),
        ("bigwedge", "⋀"),
        ("bigvee", "⋁"),
        ("bigsqcup", "⨆"),
        ("biguplus", "⨄"),
        ("bigoplus", "⨁"),
        ("bigotimes", "⨂"),
        ("bigodot", "⨀"),
        ("setminus", "∖"),
        ("in", "∈"),
        ("notin", "∉"),
        ("ni", "∋"),
        ("subset", "⊂"),
        ("supset", "⊃"),
        ("subseteq", "⊆"),
        ("supseteq", "⊇"),
        ("sqsubset", "⊏"),
        ("sqsupset", "⊐"),
        ("sqsubseteq", "⊑"),
        ("sqsupseteq", "⊒"),
        ("prec", "≺"),
        ("preceq", "≼"),
        ("succ", "≻"),
        ("succeq", "≽"),
        ("ll", "≪"),
        ("gg", "≫"),
        ("le", "≤"),
        ("leq", "≤"),
        ("leqslant", "≤"),
        ("ge", "≥"),
        ("geq", "≥"),
        ("geqslant", "≥"),
        ("ne", "≠"),
        ("neq", "≠"),
        ("equiv", "≡"),
        ("approx", "≈"),
        ("sim", "∼"),
        ("simeq", "≃"),
        ("cong", "≅"),
        ("asymp", "≍"),
        ("doteq", "≐"),
        ("propto", "∝"),
        ("parallel", "∥"),
        ("perp", "⊥"),
        ("mid", "∣"),
        ("vdash", "⊢"),
        ("dashv", "⊣"),
        ("models", "⊨"),
        ("Vdash", "⊩"),
        ("Vvdash", "⊪"),
        ("nvdash", "⊬"),
        ("nvDash", "⊭"),
        ("forall", "∀"),
        ("exists", "∃"),
        ("nexists", "∄"),
        ("neg", "¬"),
        ("land", "∧"),
        ("wedge", "∧"),
        ("lor", "∨"),
        ("vee", "∨"),
        ("to", "→"),
        ("rightarrow", "→"),
        ("longrightarrow", "→"),
        ("leftarrow", "←"),
        ("longleftarrow", "←"),
        ("gets", "←"),
        ("leftrightarrow", "↔"),
        ("longleftrightarrow", "↔"),
        ("hookleftarrow", "↩"),
        ("hookrightarrow", "↪"),
        ("twoheadleftarrow", "↞"),
        ("twoheadrightarrow", "↠"),
        ("leftharpoonup", "↼"),
        ("leftharpoondown", "↽"),
        ("rightharpoonup", "⇀"),
        ("rightharpoondown", "⇁"),
        ("rightleftharpoons", "⇌"),
        ("leftrightharpoons", "⇋"),
        ("nearrow", "↗"),
        ("searrow", "↘"),
        ("swarrow", "↙"),
        ("nwarrow", "↖"),
        ("rightsquigarrow", "⇝"),
        ("leadsto", "⇝"),
        ("Rightarrow", "⇒"),
        ("Longrightarrow", "⇒"),
        ("Leftarrow", "⇐"),
        ("Longleftarrow", "⇐"),
        ("Leftrightarrow", "⇔"),
        ("Longleftrightarrow", "⇔"),
        ("implies", "⇒"),
        ("iff", "⇔"),
        ("mapsto", "↦"),
        ("longmapsto", "↦"),
        ("uparrow", "↑"),
        ("downarrow", "↓"),
        ("partial", "∂"),
        ("nabla", "∇"),
        ("int", "∫"),
        ("iint", "∬"),
        ("iiint", "∭"),
        ("oint", "∮"),
        ("sum", "∑"),
        ("prod", "∏"),
        ("coprod", "∐"),
        ("infty", "∞"),
        ("emptyset", "∅"),
        ("varnothing", "∅"),
        ("angle", "∠"),
        ("therefore", "∴"),
        ("because", "∵"),
        ("aleph", "ℵ"),
        ("beth", "ℶ"),
        ("gimel", "ℷ"),
        ("daleth", "ℸ"),
        ("top", "⊤"),
        ("bot", "⊥"),
        ("triangle", "△"),
        ("square", "□"),
        ("lozenge", "◊"),
        ("checkmark", "✓"),
        ("complement", "∁"),
        ("wp", "℘"),
        ("prime", "′"),
        ("ldots", "…"),
        ("dots", "…"),
        ("cdots", "⋯"),
        ("vdots", "⋮"),
        ("ddots", "⋱"),
        ("ell", "ℓ"),
        ("hbar", "ℏ"),
        ("Im", "ℑ"),
        ("Re", "ℜ"),
        ("langle", "⟨"),
        ("rangle", "⟩"),
        ("vert", "|"),
        ("lvert", "|"),
        ("rvert", "|"),
        ("Vert", "‖"),
        ("lVert", "‖"),
        ("rVert", "‖"),
        ("lbrace", "{"),
        ("rbrace", "}"),
        ("backslash", "\\"),
        ("lfloor", "⌊"),
        ("rfloor", "⌋"),
        ("lceil", "⌈"),
        ("rceil", "⌉"),
        ("colon", ":"),
    ] {
        m.insert(k, v);
    }
    m
}

fn named_operators() -> HashSet<&'static str> {
    [
        "arccos", "arcsin", "arctan", "arg", "cos", "cosh", "cot", "coth", "csc", "deg", "det",
        "dim", "exp", "gcd", "hom", "inf", "ker", "lg", "lim", "liminf", "limsup", "ln", "log",
        "max", "min", "Pr", "sec", "sin", "sinh", "sup", "tan", "tanh",
    ]
    .into_iter()
    .collect()
}

fn limit_operators() -> HashSet<&'static str> {
    [
        "argmax", "argmin", "inf", "injlim", "lim", "liminf", "limsup", "max", "min", "projlim",
        "sup",
    ]
    .into_iter()
    .collect()
}

fn display_limit_symbols() -> HashSet<&'static str> {
    [
        "bigcap",
        "bigcup",
        "bigodot",
        "bigoplus",
        "bigotimes",
        "bigsqcup",
        "biguplus",
        "bigvee",
        "bigwedge",
        "coprod",
        "int",
        "iint",
        "iiint",
        "oint",
        "prod",
        "sum",
    ]
    .into_iter()
    .collect()
}

fn relation_commands() -> HashSet<&'static str> {
    [
        "Leftarrow",
        "Leftrightarrow",
        "Longleftarrow",
        "Longleftrightarrow",
        "Longrightarrow",
        "Rightarrow",
        "Vdash",
        "Vvdash",
        "approx",
        "asymp",
        "cong",
        "dashv",
        "doteq",
        "downarrow",
        "equiv",
        "ge",
        "geq",
        "geqslant",
        "gets",
        "gg",
        "hookleftarrow",
        "hookrightarrow",
        "iff",
        "implies",
        "in",
        "leadsto",
        "le",
        "leftarrow",
        "leftharpoondown",
        "leftharpoonup",
        "leftrightarrow",
        "leftrightharpoons",
        "leq",
        "leqslant",
        "ll",
        "longleftarrow",
        "longleftrightarrow",
        "longmapsto",
        "longrightarrow",
        "mapsto",
        "mid",
        "models",
        "ne",
        "nearrow",
        "neq",
        "ni",
        "notin",
        "nvdash",
        "nvDash",
        "nwarrow",
        "parallel",
        "perp",
        "prec",
        "preceq",
        "propto",
        "rightharpoondown",
        "rightharpoonup",
        "rightleftharpoons",
        "rightarrow",
        "rightsquigarrow",
        "searrow",
        "sim",
        "simeq",
        "sqsubset",
        "sqsubseteq",
        "sqsupset",
        "sqsupseteq",
        "subset",
        "subseteq",
        "succ",
        "succeq",
        "supset",
        "supseteq",
        "swarrow",
        "to",
        "triangleleft",
        "triangleright",
        "twoheadleftarrow",
        "twoheadrightarrow",
        "uparrow",
        "vdash",
    ]
    .into_iter()
    .collect()
}

fn negated_symbols() -> HashMap<&'static str, &'static str> {
    [
        ("<", "≮"),
        (">", "≯"),
        ("=", "≠"),
        ("∈", "∉"),
        ("∋", "∌"),
        ("∣", "∤"),
        ("∥", "∦"),
        ("∼", "≁"),
        ("≃", "≄"),
        ("≅", "≇"),
        ("≈", "≉"),
        ("≡", "≢"),
        ("≤", "≰"),
        ("≥", "≱"),
        ("≺", "⊀"),
        ("≻", "⊁"),
        ("⊂", "⊄"),
        ("⊃", "⊅"),
        ("⊆", "⊈"),
        ("⊇", "⊉"),
        ("⊢", "⊬"),
        ("⊨", "⊭"),
        ("↔", "↮"),
        ("←", "↚"),
        ("→", "↛"),
        ("⇒", "⇏"),
        ("⇐", "⇍"),
        ("⇔", "⇎"),
        ("≼", "⋠"),
        ("≽", "⋡"),
    ]
    .into_iter()
    .collect()
}

fn blackboard() -> HashMap<&'static str, &'static str> {
    [
        ("C", "ℂ"),
        ("H", "ℍ"),
        ("N", "ℕ"),
        ("P", "ℙ"),
        ("Q", "ℚ"),
        ("R", "ℝ"),
        ("Z", "ℤ"),
    ]
    .into_iter()
    .collect()
}

fn superscripts() -> HashMap<&'static str, &'static str> {
    [
        ("0", "⁰"),
        ("1", "¹"),
        ("2", "²"),
        ("3", "³"),
        ("4", "⁴"),
        ("5", "⁵"),
        ("6", "⁶"),
        ("7", "⁷"),
        ("8", "⁸"),
        ("9", "⁹"),
        ("+", "⁺"),
        ("-", "⁻"),
        ("=", "⁼"),
        ("(", "⁽"),
        (")", "⁾"),
        ("a", "ᵃ"),
        ("b", "ᵇ"),
        ("c", "ᶜ"),
        ("d", "ᵈ"),
        ("e", "ᵉ"),
        ("f", "ᶠ"),
        ("g", "ᵍ"),
        ("h", "ʰ"),
        ("i", "ⁱ"),
        ("j", "ʲ"),
        ("k", "ᵏ"),
        ("l", "ˡ"),
        ("m", "ᵐ"),
        ("n", "ⁿ"),
        ("o", "ᵒ"),
        ("p", "ᵖ"),
        ("r", "ʳ"),
        ("s", "ˢ"),
        ("t", "ᵗ"),
        ("u", "ᵘ"),
        ("v", "ᵛ"),
        ("w", "ʷ"),
        ("x", "ˣ"),
        ("y", "ʸ"),
        ("z", "ᶻ"),
    ]
    .into_iter()
    .collect()
}

fn subscripts() -> HashMap<&'static str, &'static str> {
    [
        ("0", "₀"),
        ("1", "₁"),
        ("2", "₂"),
        ("3", "₃"),
        ("4", "₄"),
        ("5", "₅"),
        ("6", "₆"),
        ("7", "₇"),
        ("8", "₈"),
        ("9", "₉"),
        ("+", "₊"),
        ("-", "₋"),
        ("=", "₌"),
        ("(", "₍"),
        (")", "₎"),
        ("a", "ₐ"),
        ("e", "ₑ"),
        ("h", "ₕ"),
        ("i", "ᵢ"),
        ("j", "ⱼ"),
        ("k", "ₖ"),
        ("l", "ₗ"),
        ("m", "ₘ"),
        ("n", "ₙ"),
        ("o", "ₒ"),
        ("p", "ₚ"),
        ("r", "ᵣ"),
        ("s", "ₛ"),
        ("t", "ₜ"),
        ("u", "ᵤ"),
        ("v", "ᵥ"),
        ("x", "ₓ"),
    ]
    .into_iter()
    .collect()
}

fn spacing_commands() -> HashSet<&'static str> {
    [
        ",",
        ":",
        ";",
        " ",
        ">",
        "enspace",
        "enskip",
        "medspace",
        "quad",
        "qquad",
        "thickspace",
        "thinspace",
    ]
    .into_iter()
    .collect()
}

fn negative_spacing_commands() -> HashSet<&'static str> {
    ["!", "negmedspace", "negthickspace", "negthinspace"]
        .into_iter()
        .collect()
}

fn ignored_commands() -> HashSet<&'static str> {
    [
        "displaystyle",
        "limits",
        "nolimits",
        "scriptstyle",
        "scriptscriptstyle",
        "textstyle",
    ]
    .into_iter()
    .collect()
}

fn size_commands() -> HashSet<&'static str> {
    [
        "big", "Big", "bigg", "Bigg", "bigl", "Bigl", "biggl", "Biggl", "bigr", "Bigr", "biggr",
        "Biggr",
    ]
    .into_iter()
    .collect()
}

fn plain_wrappers() -> HashSet<&'static str> {
    [
        "emph",
        "mathcal",
        "mathbf",
        "mathfrak",
        "mathit",
        "mathrm",
        "mathnormal",
        "mathscr",
        "mathsf",
        "mathtt",
        "mathup",
        "mbox",
        "overbrace",
        "pmb",
        "smash",
        "substack",
        "text",
        "textbf",
        "textit",
        "textmd",
        "textnormal",
        "textrm",
        "textsc",
        "textsf",
        "textsl",
        "texttt",
        "textup",
        "underbrace",
        "bm",
        "boldsymbol",
    ]
    .into_iter()
    .collect()
}

fn accents() -> HashMap<&'static str, &'static str> {
    [
        ("acute", "\u{301}"),
        ("bar", "\u{305}"),
        ("breve", "\u{306}"),
        ("check", "\u{30c}"),
        ("ddot", "\u{308}"),
        ("dot", "\u{307}"),
        ("grave", "\u{300}"),
        ("hat", "\u{302}"),
        ("mathring", "\u{30a}"),
        ("overleftarrow", "\u{20d6}"),
        ("overleftrightarrow", "\u{20e1}"),
        ("overline", "\u{305}"),
        ("overrightarrow", "\u{20d7}"),
        ("tilde", "\u{303}"),
        ("underline", "\u{332}"),
        ("vec", "\u{20d7}"),
        ("widehat", "\u{302}"),
        ("widetilde", "\u{303}"),
    ]
    .into_iter()
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(src: &str) -> String {
        render_latex(src, &RenderLatexOptions::default()).unwrap_or_default()
    }

    #[test]
    fn basic_symbols() {
        assert_eq!(render(r"x^2 + 1"), "x² + 1");
        assert_eq!(render(r"\alpha + \beta"), "α + β");
    }

    #[test]
    fn fraction_inline() {
        assert_eq!(render(r"\frac{1}{2}"), "1/2");
    }

    #[test]
    fn sqrt_and_root() {
        assert_eq!(render(r"\sqrt{4}"), "√4");
        assert_eq!(render(r"\sqrt[3]{8}"), "∛8");
    }

    #[test]
    fn matrix_env() {
        assert_eq!(
            render(r"\begin{matrix}a & b \\ c & d\end{matrix}"),
            "a │ b\nc │ d"
        );
    }

    #[test]
    fn display_sum_stacks() {
        let r = render_latex(r"\sum_{i=1}^{n} i", &RenderLatexOptions { display: true }).unwrap();
        assert!(r.contains('∑'));
        assert!(r.contains('n'));
        assert!(r.contains("i=1"));
    }

    #[test]
    fn unsupported_returns_none() {
        assert!(render_latex(r"\unknowncommand", &RenderLatexOptions::default()).is_none());
    }
}
