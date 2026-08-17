//! Port of `packages/tui/src/utils.ts` — terminal visible-width, ANSI-code
//! extraction/tracking, and ANSI-aware wrapping/truncation/slicing. See
//! `docs/analysis/05-tui.md` §2/§9.
//!
//! ## Known porting gaps (documented, not silent — AGENTS.md Correctness Bar)
//!
//! - **RGI emoji matching** ([`is_probable_rgi_emoji`]): the TS source tests
//!   `/^\p{RGI_Emoji}$/v`, matching against Unicode's official curated list of
//!   thousands of "recommended for general interchange" emoji sequences. There is
//!   no Rust crate in this dependency tree with that exact table, so this port uses
//!   a heuristic (known emoji code-point blocks + known emoji combinators: ZWJ,
//!   VS15/16, skin-tone modifiers, combining keycap, emoji tag characters). This
//!   covers every case exercised by `tests/fixtures/pi/tui/utils.cases.jsonl`
//!   (plain emoji, ZWJ family sequences, skin-tone modifiers, flag pairs, VS16
//!   presentation selectors) but is not a byte-exact replica of V8's ICU-backed
//!   property match for the full Unicode emoji corpus.
//! - **`Default_Ignorable_Code_Point`** ([`is_non_printing`]): approximated as
//!   Unicode General_Category Control ∪ Format ∪ Mark, which covers every
//!   practical zero-width character (ZWJ, ZWNJ, ZWSP, soft hyphen, combining
//!   marks, variation selectors) but is not the full derived property (a few
//!   reserved/noncharacter code points are out of scope and untested).
//! - **`cjkBreakRegex`** ([`is_cjk_break_char`]): the TS source uses
//!   `Script_Extensions` (Han/Hiragana/Katakana/Hangul/Bopomofo), a nuanced
//!   property including characters shared across scripts. This port uses the
//!   standard block-range approximation instead, which is exact for the common
//!   case (CJK ideographs, kana, hangul syllables/jamo, bopomofo) but may differ
//!   at the margins for rare extension characters.
//! - **East Asian Width table**: `unicode-width` and `get-east-asian-width` are
//!   independently maintained implementations of the same Unicode
//!   `EastAsianWidth.txt` data; both default ambiguous-width characters to
//!   narrow (1), so they should agree, but this has not been diffed
//!   codepoint-by-codepoint against the npm package's table.
//! - The JS source's `widthCache` (a bounded FIFO memoization `Map`) is a
//!   performance-only mechanism with zero effect on any function's return value
//!   (same input always produces the same width) — intentionally not ported.

use unicode_segmentation::UnicodeSegmentation;

/// A parsed ANSI/OSC/APC escape sequence: the literal bytes and their length.
/// Mirrors `extractAnsiCode`'s `{ code, length }` return shape (utils.ts:311).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnsiCode {
    pub code: String,
    pub length: usize,
}

/// Mirrors `sliceWithWidth`'s `{ text, width }` return shape (utils.ts:1083).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SliceResult {
    pub text: String,
    pub width: usize,
}

/// Mirrors `extractSegments`'s return shape (utils.ts:1138).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedSegments {
    pub before: String,
    pub before_width: usize,
    pub after: String,
    pub after_width: usize,
}

/// `PUNCTUATION_REGEX` (utils.ts:821) as a fixed character set.
pub const PUNCTUATION_CHARS: &[char] = &[
    '(', ')', '{', '}', '[', ']', '<', '>', '.', ',', ';', ':', '\'', '"', '!', '?', '+', '-', '=',
    '*', '/', '\\', '|', '&', '%', '^', '$', '#', '@', '~', '`',
];

/// Check if a character is whitespace (`isWhitespaceChar`, utils.ts:826).
pub fn is_whitespace_char(ch: &str) -> bool {
    ch.chars().next().is_some_and(char::is_whitespace)
}

/// Check if a character is punctuation (`isPunctuationChar`, utils.ts:833).
pub fn is_punctuation_char(ch: &str) -> bool {
    ch.chars()
        .next()
        .is_some_and(|c| PUNCTUATION_CHARS.contains(&c))
}

/// `cjkBreakRegex` (utils.ts:48-49) approximated via Unicode block ranges for
/// Han / Hiragana / Katakana / Hangul / Bopomofo. See module docs for the gap.
fn is_cjk_break_char(c: char) -> bool {
    matches!(c as u32,
        0x4e00..=0x9fff | 0x3400..=0x4dbf | 0xf900..=0xfaff | 0x20000..=0x2fa1f
        | 0x3041..=0x3096 | 0x309d..=0x309f
        | 0x30a1..=0x30fa | 0x30fd..=0x30ff | 0x31f0..=0x31ff | 0xff66..=0xff9d
        | 0xac00..=0xd7a3 | 0x1100..=0x11ff | 0x3130..=0x318f | 0xa960..=0xa97f | 0xd7b0..=0xd7ff
        | 0x3105..=0x312f | 0x31a0..=0x31bf
    )
}

fn is_printable_ascii(s: &str) -> bool {
    s.bytes().all(|b| (0x20..=0x7e).contains(&b))
}

/// Approximates `\p{Default_Ignorable_Code_Point}|\p{Control}|\p{Mark}`. See module docs.
fn is_non_printing(c: char) -> bool {
    use unicode_properties::{GeneralCategory, UnicodeGeneralCategory};
    matches!(
        c.general_category(),
        GeneralCategory::Control
            | GeneralCategory::Format
            | GeneralCategory::NonspacingMark
            | GeneralCategory::SpacingMark
            | GeneralCategory::EnclosingMark
    )
}

/// `zeroWidthRegex.test(segment)` (utils.ts:40): true only if EVERY char in the
/// (non-empty) segment is non-printing.
fn is_zero_width_only(segment: &str) -> bool {
    !segment.is_empty() && segment.chars().all(is_non_printing)
}

/// `leadingNonPrintingRegex` strip (utils.ts:41): drop a leading run of
/// non-printing chars, matching `base = segment.replace(leadingNonPrintingRegex, "")`.
fn strip_leading_non_printing(segment: &str) -> &str {
    let mut idx = 0;
    for c in segment.chars() {
        if is_non_printing(c) {
            idx += c.len_utf8();
        } else {
            break;
        }
    }
    &segment[idx..]
}

/// `couldBeEmoji` (utils.ts:27-37) — fast heuristic pre-filter before the
/// expensive RGI-emoji test. `segment.length` is a UTF-16 code-unit count in
/// JS; replicated exactly via `encode_utf16().count()` since a bare single
/// supplementary-plane emoji (one grapheme, one codepoint) is 2 UTF-16 units
/// but should NOT alone satisfy "multi-codepoint sequence" — only truly
/// additional codepoints (ZWJ, modifiers) push the count past 2.
fn could_be_emoji(segment: &str) -> bool {
    let cp = segment.chars().next().map_or(0, |c| c as u32);
    (0x1f000..=0x1fbff).contains(&cp)
        || (0x2300..=0x23ff).contains(&cp)
        || (0x2600..=0x27bf).contains(&cp)
        || (0x2b50..=0x2b55).contains(&cp)
        || segment.contains('\u{FE0F}')
        || segment.encode_utf16().count() > 2
}

fn is_emoji_base_codepoint(cp: u32) -> bool {
    (0x1f000..=0x1fbff).contains(&cp)
        || (0x2300..=0x23ff).contains(&cp)
        || (0x2600..=0x27bf).contains(&cp)
        || (0x2b50..=0x2b55).contains(&cp)
        || cp == 0x203c
        || cp == 0x2049
}

fn is_emoji_combinator(cp: u32) -> bool {
    cp == 0x200d // ZWJ
        || cp == 0xfe0f // VS16 emoji presentation
        || cp == 0xfe0e // VS15 text presentation
        || (0x1f3fb..=0x1f3ff).contains(&cp) // skin tone modifiers
        || cp == 0x20e3 // combining enclosing keycap
        || (0xe0020..=0xe007f).contains(&cp) // emoji tag sequence
}

/// Heuristic approximation of `/^\p{RGI_Emoji}$/v`. See module docs' known gap.
fn is_probable_rgi_emoji(segment: &str) -> bool {
    let mut has_emoji_base = false;
    for c in segment.chars() {
        let cp = c as u32;
        if is_emoji_base_codepoint(cp) {
            has_emoji_base = true;
        } else if is_emoji_combinator(cp) || (c.is_ascii_digit() && cp != 0) {
            // ASCII digits are only valid here as a keycap sequence base (e.g. "1️⃣");
            // harmless to allow generally since a bare digit alone never reaches this function
            // (couldBeEmoji's pre-filter requires an emoji-range codepoint or FE0F or >2 UTF-16 units).
            continue;
        } else {
            return false;
        }
    }
    has_emoji_base
}

fn east_asian_width(cp: u32) -> usize {
    char::from_u32(cp)
        .and_then(unicode_width::UnicodeWidthChar::width)
        .unwrap_or(0)
}

/// `graphemeWidth` (utils.ts:167-211): the terminal column width of one grapheme cluster.
fn grapheme_width(segment: &str) -> usize {
    if segment == "\t" {
        return 3;
    }
    if is_zero_width_only(segment) {
        return 0;
    }
    if could_be_emoji(segment) && is_probable_rgi_emoji(segment) {
        return 2;
    }
    let base = strip_leading_non_printing(segment);
    let cp = match base.chars().next() {
        Some(c) => c as u32,
        None => return 0,
    };
    if (0x1f1e6..=0x1f1ff).contains(&cp) {
        return 2;
    }
    let mut width = east_asian_width(cp);
    if segment.chars().count() > 1 {
        for c in segment.chars().skip(1) {
            let c_cp = c as u32;
            if (0xff00..=0xffef).contains(&c_cp) {
                width += east_asian_width(c_cp);
            } else if c_cp == 0x0e33 || c_cp == 0x0eb3 {
                width += 1;
            }
        }
    }
    width
}

/// Returns the length in bytes of the ANSI/OSC/APC escape sequence starting at
/// byte offset `pos`, or `None` if `pos` isn't the start of one. Mirrors
/// `extractAnsiCode` (utils.ts:311-349). Safe on byte offsets: every delimiter
/// byte this function looks for (`ESC`, `[`, `]`, `_`, BEL, `\`, and the CSI
/// terminators `mGKHJ`) is ASCII and can never appear as a UTF-8 continuation byte.
pub fn extract_ansi_code(s: &str, pos: usize) -> Option<AnsiCode> {
    let bytes = s.as_bytes();
    if pos >= bytes.len() || bytes[pos] != 0x1b {
        return None;
    }
    match bytes.get(pos + 1) {
        Some(b'[') => {
            let mut j = pos + 2;
            while j < bytes.len() && !matches!(bytes[j], b'm' | b'G' | b'K' | b'H' | b'J') {
                j += 1;
            }
            if j < bytes.len() {
                Some(AnsiCode {
                    code: s[pos..=j].to_string(),
                    length: j + 1 - pos,
                })
            } else {
                None
            }
        }
        Some(b']') | Some(b'_') => {
            let mut j = pos + 2;
            loop {
                if j >= bytes.len() {
                    return None;
                }
                if bytes[j] == 0x07 {
                    return Some(AnsiCode {
                        code: s[pos..=j].to_string(),
                        length: j + 1 - pos,
                    });
                }
                if bytes[j] == 0x1b && bytes.get(j + 1) == Some(&b'\\') {
                    return Some(AnsiCode {
                        code: s[pos..j + 2].to_string(),
                        length: j + 2 - pos,
                    });
                }
                j += 1;
            }
        }
        _ => None,
    }
}

/// Calculate the visible width of a string in terminal columns (`visibleWidth`, utils.ts:216-271).
pub fn visible_width(s: &str) -> usize {
    if s.is_empty() {
        return 0;
    }
    if is_printable_ascii(s) {
        return s.len();
    }
    let mut clean = s.to_string();
    if clean.contains('\t') {
        clean = clean.replace('\t', "   ");
    }
    if clean.contains('\u{1b}') {
        let mut stripped = String::new();
        let bytes_len = clean.len();
        let mut i = 0;
        while i < bytes_len {
            if let Some(ansi) = extract_ansi_code(&clean, i) {
                i += ansi.length;
                continue;
            }
            let ch = clean[i..].chars().next().unwrap();
            stripped.push(ch);
            i += ch.len_utf8();
        }
        clean = stripped;
    }
    clean.graphemes(true).map(grapheme_width).sum()
}

const THAI_LAO_AM: [char; 2] = ['\u{0e33}', '\u{0eb3}'];

/// Normalize text for terminal output (`normalizeTerminalOutput`, utils.ts:284-306).
pub fn normalize_terminal_output(s: &str) -> String {
    let mut normalized = s.to_string();
    if normalized.contains(THAI_LAO_AM[0]) || normalized.contains(THAI_LAO_AM[1]) {
        normalized = normalized
            .replace('\u{0e33}', "\u{0e4d}\u{0e32}")
            .replace('\u{0eb3}', "\u{0ecd}\u{0eb2}");
    }
    if !normalized.contains('\t') {
        return normalized;
    }
    let mut result = String::new();
    let bytes_len = normalized.len();
    let mut i = 0;
    while i < bytes_len {
        if let Some(ansi) = extract_ansi_code(&normalized, i) {
            result.push_str(&ansi.code);
            i += ansi.length;
            continue;
        }
        let ch = normalized[i..].chars().next().unwrap();
        if ch == '\t' {
            result.push_str("   ");
        } else {
            result.push(ch);
        }
        i += ch.len_utf8();
    }
    result
}

fn update_tracker_from_text(text: &str, tracker: &mut AnsiCodeTracker) {
    let bytes_len = text.len();
    let mut i = 0;
    while i < bytes_len {
        if let Some(ansi) = extract_ansi_code(text, i) {
            tracker.process(&ansi.code);
            i += ansi.length;
        } else {
            let ch_len = text[i..].chars().next().map_or(1, |c| c.len_utf8());
            i += ch_len;
        }
    }
}

/// Split text into words while keeping ANSI codes attached (`splitIntoTokensWithAnsi`, utils.ts:628-702).
fn split_into_tokens_with_ansi(text: &str) -> Vec<String> {
    #[derive(PartialEq, Clone, Copy)]
    enum Kind {
        Space,
        Word,
    }
    let mut tokens: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut pending_ansi = String::new();
    let mut current_kind: Option<Kind> = None;
    let bytes_len = text.len();
    let mut i = 0;

    while i < bytes_len {
        if let Some(ansi) = extract_ansi_code(text, i) {
            pending_ansi.push_str(&ansi.code);
            i += ansi.length;
            continue;
        }
        let mut end = i;
        while end < bytes_len && extract_ansi_code(text, end).is_none() {
            end += 1;
        }
        for seg in text[i..end].graphemes(true) {
            let segment_is_space = seg == " ";
            if !segment_is_space && seg.chars().any(is_cjk_break_char) {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                    current_kind = None;
                }
                let token = format!("{pending_ansi}{seg}");
                pending_ansi.clear();
                tokens.push(token);
                continue;
            }
            let segment_kind = if segment_is_space {
                Kind::Space
            } else {
                Kind::Word
            };
            if !current.is_empty() && current_kind != Some(segment_kind) {
                tokens.push(std::mem::take(&mut current));
            }
            if !pending_ansi.is_empty() {
                current.push_str(&pending_ansi);
                pending_ansi.clear();
            }
            current_kind = Some(segment_kind);
            current.push_str(seg);
        }
        i = end;
    }

    if !pending_ansi.is_empty() {
        if !current.is_empty() {
            current.push_str(&pending_ansi);
        } else if !tokens.is_empty() {
            let last = tokens.len() - 1;
            tokens[last].push_str(&pending_ansi);
        } else {
            current.push_str(&pending_ansi);
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

/// `breakLongWord` (utils.ts:837-904): break a single too-long token character by character.
fn break_long_word(word: &str, width: usize, tracker: &mut AnsiCodeTracker) -> Vec<String> {
    enum Seg {
        Ansi(String),
        Grapheme(String),
    }
    let mut segments = Vec::new();
    let bytes_len = word.len();
    let mut i = 0;
    while i < bytes_len {
        if let Some(ansi) = extract_ansi_code(word, i) {
            segments.push(Seg::Ansi(ansi.code));
            i += ansi.length;
        } else {
            let mut end = i;
            while end < bytes_len && extract_ansi_code(word, end).is_none() {
                end += 1;
            }
            for g in word[i..end].graphemes(true) {
                segments.push(Seg::Grapheme(g.to_string()));
            }
            i = end;
        }
    }

    let mut lines = Vec::new();
    let mut current_line = tracker.get_active_codes();
    let mut current_width = 0usize;
    for seg in segments {
        match seg {
            Seg::Ansi(code) => {
                current_line.push_str(&code);
                tracker.process(&code);
            }
            Seg::Grapheme(g) => {
                if g.is_empty() {
                    continue;
                }
                let gw = visible_width(&g);
                if current_width + gw > width {
                    let reset = tracker.get_line_end_reset();
                    if !reset.is_empty() {
                        current_line.push_str(&reset);
                    }
                    lines.push(std::mem::replace(
                        &mut current_line,
                        tracker.get_active_codes(),
                    ));
                    current_width = 0;
                }
                current_line.push_str(&g);
                current_width += gw;
            }
        }
    }
    if !current_line.is_empty() {
        lines.push(current_line);
    }
    if lines.is_empty() {
        vec![String::new()]
    } else {
        lines
    }
}

/// `wrapSingleLine` (utils.ts:740-819): wrap one line (no embedded newlines) to `width` columns.
fn wrap_single_line(line: &str, width: usize) -> Vec<String> {
    if line.is_empty() {
        return vec![String::new()];
    }
    if visible_width(line) <= width {
        return vec![line.to_string()];
    }

    let mut wrapped: Vec<String> = Vec::new();
    let mut tracker = AnsiCodeTracker::new();
    let tokens = split_into_tokens_with_ansi(line);

    let mut current_line = String::new();
    let mut current_visible_length = 0usize;

    for token in tokens {
        let token_visible_length = visible_width(&token);
        let is_whitespace = token.trim().is_empty();

        if token_visible_length > width && !is_whitespace {
            if !current_line.is_empty() {
                let reset = tracker.get_line_end_reset();
                if !reset.is_empty() {
                    current_line.push_str(&reset);
                }
                wrapped.push(std::mem::take(&mut current_line));
            }
            let broken = break_long_word(&token, width, &mut tracker);
            let last_index = broken.len() - 1;
            for b in &broken[..last_index] {
                wrapped.push(b.clone());
            }
            current_line = broken[last_index].clone();
            current_visible_length = visible_width(&current_line);
            continue;
        }

        let total_needed = current_visible_length + token_visible_length;
        if total_needed > width && current_visible_length > 0 {
            let mut line_to_wrap = current_line.trim_end().to_string();
            let reset = tracker.get_line_end_reset();
            if !reset.is_empty() {
                line_to_wrap.push_str(&reset);
            }
            wrapped.push(line_to_wrap);
            if is_whitespace {
                current_line = tracker.get_active_codes();
                current_visible_length = 0;
            } else {
                current_line = format!("{}{token}", tracker.get_active_codes());
                current_visible_length = token_visible_length;
            }
        } else {
            current_line.push_str(&token);
            current_visible_length += token_visible_length;
        }
        update_tracker_from_text(&token, &mut tracker);
    }

    if !current_line.is_empty() {
        wrapped.push(current_line);
    }
    if wrapped.is_empty() {
        vec![String::new()]
    } else {
        wrapped
            .into_iter()
            .map(|l| l.trim_end().to_string())
            .collect()
    }
}

/// Splits on `\r\n` (as a unit), lone `\r`, or lone `\n` — matches JS's `split(/\r\n|\r|\n/)`.
fn split_lines_like_js(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let bytes = text.as_bytes();
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\r' {
            lines.push(&text[start..i]);
            i += if bytes.get(i + 1) == Some(&b'\n') {
                2
            } else {
                1
            };
            start = i;
        } else if bytes[i] == b'\n' {
            lines.push(&text[start..i]);
            i += 1;
            start = i;
        } else {
            i += 1;
        }
    }
    lines.push(&text[start..]);
    lines
}

/// Wrap text with ANSI codes preserved (`wrapTextWithAnsi`, utils.ts:715-738).
pub fn wrap_text_with_ansi(text: &str, width: usize) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }
    let input_lines = split_lines_like_js(text);
    let mut result: Vec<String> = Vec::new();
    let mut tracker = AnsiCodeTracker::new();

    for input_line in input_lines {
        let prefix = if !result.is_empty() {
            tracker.get_active_codes()
        } else {
            String::new()
        };
        let combined = format!("{prefix}{input_line}");
        result.extend(wrap_single_line(&combined, width));
        update_tracker_from_text(input_line, &mut tracker);
    }

    if result.is_empty() {
        vec![String::new()]
    } else {
        result
    }
}

/// Apply background color to a line, padding to full width (`applyBackgroundToLine`, utils.ts:914-923).
pub fn apply_background_to_line(
    line: &str,
    width: usize,
    bg_fn: impl Fn(&str) -> String,
) -> String {
    let visible_len = visible_width(line);
    let padding = " ".repeat(width.saturating_sub(visible_len));
    bg_fn(&format!("{line}{padding}"))
}

struct FragmentResult {
    text: String,
    width: usize,
}

/// `truncateFragmentToWidth` (utils.ts:61-139): used only to truncate the ellipsis itself
/// when the ellipsis is wider than `maxWidth`.
fn truncate_fragment_to_width(text: &str, max_width: usize) -> FragmentResult {
    if max_width == 0 || text.is_empty() {
        return FragmentResult {
            text: String::new(),
            width: 0,
        };
    }
    if is_printable_ascii(text) {
        let clipped = &text[..max_width.min(text.len())];
        return FragmentResult {
            text: clipped.to_string(),
            width: clipped.len(),
        };
    }
    let has_ansi = text.contains('\u{1b}');
    let has_tabs = text.contains('\t');
    if !has_ansi && !has_tabs {
        let mut result = String::new();
        let mut width = 0usize;
        for seg in text.graphemes(true) {
            let w = grapheme_width(seg);
            if width + w > max_width {
                break;
            }
            result.push_str(seg);
            width += w;
        }
        return FragmentResult {
            text: result,
            width,
        };
    }

    let mut result = String::new();
    let mut width = 0usize;
    let mut pending_ansi = String::new();
    let bytes_len = text.len();
    let mut i = 0;
    while i < bytes_len {
        if let Some(ansi) = extract_ansi_code(text, i) {
            pending_ansi.push_str(&ansi.code);
            i += ansi.length;
            continue;
        }
        if text.as_bytes()[i] == b'\t' {
            if width + 3 > max_width {
                return FragmentResult {
                    text: result,
                    width,
                };
            }
            if !pending_ansi.is_empty() {
                result.push_str(&pending_ansi);
                pending_ansi.clear();
            }
            result.push('\t');
            width += 3;
            i += 1;
            continue;
        }
        let mut end = i;
        while end < bytes_len
            && text.as_bytes()[end] != b'\t'
            && extract_ansi_code(text, end).is_none()
        {
            end += 1;
        }
        for seg in text[i..end].graphemes(true) {
            let w = grapheme_width(seg);
            if width + w > max_width {
                return FragmentResult {
                    text: result,
                    width,
                };
            }
            if !pending_ansi.is_empty() {
                result.push_str(&pending_ansi);
                pending_ansi.clear();
            }
            result.push_str(seg);
            width += w;
        }
        i = end;
    }
    FragmentResult {
        text: result,
        width,
    }
}

fn finalize_truncated_result(
    prefix: &str,
    prefix_width: usize,
    ellipsis: &str,
    ellipsis_width: usize,
    max_width: usize,
    pad: bool,
) -> String {
    const RESET: &str = "\x1b[0m";
    let visible_total = prefix_width + ellipsis_width;
    let mut result = if !ellipsis.is_empty() {
        format!("{prefix}{RESET}{ellipsis}{RESET}")
    } else {
        format!("{prefix}{RESET}")
    };
    if pad {
        result.push_str(&" ".repeat(max_width.saturating_sub(visible_total)));
    }
    result
}

/// Truncate text to fit within `max_width` visible columns, adding an ellipsis if needed
/// (`truncateToWidth`, utils.ts:936-1072). `ellipsis` defaults to `"..."` and `pad` to `false`
/// at TS call sites; Rust has no default arguments, so callers pass both explicitly.
pub fn truncate_to_width(text: &str, max_width: usize, ellipsis: &str, pad: bool) -> String {
    if max_width == 0 {
        return String::new();
    }
    if text.is_empty() {
        return if pad {
            " ".repeat(max_width)
        } else {
            String::new()
        };
    }

    let ellipsis_width = visible_width(ellipsis);
    if ellipsis_width >= max_width {
        let text_width = visible_width(text);
        if text_width <= max_width {
            return if pad {
                format!("{text}{}", " ".repeat(max_width - text_width))
            } else {
                text.to_string()
            };
        }
        let clipped = truncate_fragment_to_width(ellipsis, max_width);
        if clipped.width == 0 {
            return if pad {
                " ".repeat(max_width)
            } else {
                String::new()
            };
        }
        return finalize_truncated_result("", 0, &clipped.text, clipped.width, max_width, pad);
    }

    if is_printable_ascii(text) {
        if text.len() <= max_width {
            return if pad {
                format!("{text}{}", " ".repeat(max_width - text.len()))
            } else {
                text.to_string()
            };
        }
        let target_width = max_width - ellipsis_width;
        return finalize_truncated_result(
            &text[..target_width],
            target_width,
            ellipsis,
            ellipsis_width,
            max_width,
            pad,
        );
    }

    let target_width = max_width - ellipsis_width;
    let mut result = String::new();
    let mut pending_ansi = String::new();
    let mut visible_so_far = 0usize;
    let mut kept_width = 0usize;
    let mut keep_contiguous_prefix = true;
    let mut overflowed = false;
    let has_ansi = text.contains('\u{1b}');
    let has_tabs = text.contains('\t');
    let mut exhausted_input = true;

    if !has_ansi && !has_tabs {
        for seg in text.graphemes(true) {
            let w = grapheme_width(seg);
            if keep_contiguous_prefix && kept_width + w <= target_width {
                result.push_str(seg);
                kept_width += w;
            } else {
                keep_contiguous_prefix = false;
            }
            visible_so_far += w;
            if visible_so_far > max_width {
                overflowed = true;
                exhausted_input = false;
                break;
            }
        }
    } else {
        let bytes_len = text.len();
        let mut i = 0;
        'outer: while i < bytes_len {
            if let Some(ansi) = extract_ansi_code(text, i) {
                pending_ansi.push_str(&ansi.code);
                i += ansi.length;
                continue;
            }
            if text.as_bytes()[i] == b'\t' {
                if keep_contiguous_prefix && kept_width + 3 <= target_width {
                    if !pending_ansi.is_empty() {
                        result.push_str(&pending_ansi);
                        pending_ansi.clear();
                    }
                    result.push('\t');
                    kept_width += 3;
                } else {
                    keep_contiguous_prefix = false;
                    pending_ansi.clear();
                }
                visible_so_far += 3;
                if visible_so_far > max_width {
                    overflowed = true;
                    exhausted_input = false;
                    break 'outer;
                }
                i += 1;
                continue;
            }
            let mut end = i;
            while end < bytes_len
                && text.as_bytes()[end] != b'\t'
                && extract_ansi_code(text, end).is_none()
            {
                end += 1;
            }
            for seg in text[i..end].graphemes(true) {
                let w = grapheme_width(seg);
                if keep_contiguous_prefix && kept_width + w <= target_width {
                    if !pending_ansi.is_empty() {
                        result.push_str(&pending_ansi);
                        pending_ansi.clear();
                    }
                    result.push_str(seg);
                    kept_width += w;
                } else {
                    keep_contiguous_prefix = false;
                    pending_ansi.clear();
                }
                visible_so_far += w;
                if visible_so_far > max_width {
                    overflowed = true;
                    exhausted_input = false;
                    break 'outer;
                }
            }
            i = end;
        }
    }

    if !overflowed && exhausted_input {
        return if pad {
            format!(
                "{text}{}",
                " ".repeat(max_width.saturating_sub(visible_so_far))
            )
        } else {
            text.to_string()
        };
    }

    finalize_truncated_result(
        &result,
        kept_width,
        ellipsis,
        ellipsis_width,
        max_width,
        pad,
    )
}

/// Extract a range of visible columns from a line (`sliceWithWidth`, utils.ts:1083-1128).
/// `strict`: if true, exclude wide chars at the boundary that would extend past the range.
pub fn slice_with_width(line: &str, start_col: usize, length: usize, strict: bool) -> SliceResult {
    if length == 0 {
        return SliceResult {
            text: String::new(),
            width: 0,
        };
    }
    let end_col = start_col + length;
    let mut result = String::new();
    let mut result_width = 0usize;
    let mut current_col = 0usize;
    let mut pending_ansi = String::new();
    let bytes_len = line.len();
    let mut i = 0;

    while i < bytes_len {
        if let Some(ansi) = extract_ansi_code(line, i) {
            if current_col >= start_col && current_col < end_col {
                result.push_str(&ansi.code);
            } else if current_col < start_col {
                pending_ansi.push_str(&ansi.code);
            }
            i += ansi.length;
            continue;
        }
        let mut text_end = i;
        while text_end < bytes_len && extract_ansi_code(line, text_end).is_none() {
            text_end += 1;
        }
        let mut hit_end = false;
        for seg in line[i..text_end].graphemes(true) {
            let w = grapheme_width(seg);
            let in_range = current_col >= start_col && current_col < end_col;
            let fits = !strict || current_col + w <= end_col;
            if in_range && fits {
                if !pending_ansi.is_empty() {
                    result.push_str(&pending_ansi);
                    pending_ansi.clear();
                }
                result.push_str(seg);
                result_width += w;
            }
            current_col += w;
            if current_col >= end_col {
                hit_end = true;
                break;
            }
        }
        i = text_end;
        if hit_end {
            break;
        }
    }
    SliceResult {
        text: result,
        width: result_width,
    }
}

/// Like [`slice_with_width`] but only returns the text (`sliceByColumn`, utils.ts:1078-1080).
pub fn slice_by_column(line: &str, start_col: usize, length: usize, strict: bool) -> String {
    slice_with_width(line, start_col, length, strict).text
}

/// Extract "before" and "after" segments from a line in one pass (`extractSegments`, utils.ts:1138-1209).
/// Used for overlay compositing: `after` inherits active SGR styling from before the overlay.
pub fn extract_segments(
    line: &str,
    before_end: usize,
    after_start: usize,
    after_len: usize,
    strict_after: bool,
) -> ExtractedSegments {
    let mut before = String::new();
    let mut before_width = 0usize;
    let mut after = String::new();
    let mut after_width = 0usize;
    let mut current_col = 0usize;
    let mut pending_ansi_before = String::new();
    let mut after_started = false;
    let after_end = after_start + after_len;
    let mut tracker = AnsiCodeTracker::new();
    let bytes_len = line.len();
    let mut i = 0;

    let is_done = |col: usize| {
        if after_len == 0 {
            col >= before_end
        } else {
            col >= after_end
        }
    };

    while i < bytes_len {
        if let Some(ansi) = extract_ansi_code(line, i) {
            tracker.process(&ansi.code);
            if current_col < before_end {
                pending_ansi_before.push_str(&ansi.code);
            } else if current_col >= after_start && current_col < after_end && after_started {
                after.push_str(&ansi.code);
            }
            i += ansi.length;
            continue;
        }
        let mut text_end = i;
        while text_end < bytes_len && extract_ansi_code(line, text_end).is_none() {
            text_end += 1;
        }
        let mut done = false;
        for seg in line[i..text_end].graphemes(true) {
            let w = grapheme_width(seg);
            if current_col < before_end && current_col + w <= before_end {
                if !pending_ansi_before.is_empty() {
                    before.push_str(&pending_ansi_before);
                    pending_ansi_before.clear();
                }
                before.push_str(seg);
                before_width += w;
            } else if current_col >= after_start && current_col < after_end {
                let fits = !strict_after || current_col + w <= after_end;
                if fits {
                    if !after_started {
                        after.push_str(&tracker.get_active_codes());
                        after_started = true;
                    }
                    after.push_str(seg);
                    after_width += w;
                }
            }
            current_col += w;
            if is_done(current_col) {
                done = true;
                break;
            }
        }
        i = text_end;
        if done {
            break;
        }
    }

    ExtractedSegments {
        before,
        before_width,
        after,
        after_width,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Osc8Terminator {
    Bel,
    St,
}

impl Osc8Terminator {
    fn as_str(self) -> &'static str {
        match self {
            Osc8Terminator::Bel => "\x07",
            Osc8Terminator::St => "\x1b\\",
        }
    }
}

#[derive(Clone)]
struct ActiveHyperlink {
    params: String,
    url: String,
    terminator: Osc8Terminator,
}

/// `parseOsc8Hyperlink` (utils.ts:359-377). `None` = not an OSC 8 sequence at all
/// (mirrors TS `undefined`); `Some(None)` = a close (`mirrors TS `null`, empty url);
/// `Some(Some(hyperlink))` = an open.
fn parse_osc8_hyperlink(ansi_code: &str) -> Option<Option<ActiveHyperlink>> {
    if !ansi_code.starts_with("\x1b]8;") {
        return None;
    }
    let terminator = if ansi_code.ends_with('\x07') {
        Osc8Terminator::Bel
    } else {
        Osc8Terminator::St
    };
    let trim = if terminator == Osc8Terminator::Bel {
        1
    } else {
        2
    };
    let body = &ansi_code[4..ansi_code.len() - trim];
    let sep = body.find(';')?;
    let params = &body[..sep];
    let url = &body[sep + 1..];
    if url.is_empty() {
        return Some(None);
    }
    Some(Some(ActiveHyperlink {
        params: params.to_string(),
        url: url.to_string(),
        terminator,
    }))
}

fn format_osc8_hyperlink(hyperlink: &ActiveHyperlink) -> String {
    format!(
        "\x1b]8;{};{}{}",
        hyperlink.params,
        hyperlink.url,
        hyperlink.terminator.as_str()
    )
}

fn format_osc8_close(terminator: Osc8Terminator) -> String {
    format!("\x1b]8;;{}", terminator.as_str())
}

/// `AnsiCodeTracker` (utils.ts:390-610): tracks active SGR attributes and OSC 8
/// hyperlink state to preserve styling across line breaks.
struct AnsiCodeTracker {
    bold: bool,
    dim: bool,
    italic: bool,
    underline: bool,
    blink: bool,
    inverse: bool,
    hidden: bool,
    strikethrough: bool,
    fg_color: Option<String>,
    bg_color: Option<String>,
    active_hyperlink: Option<ActiveHyperlink>,
}

impl AnsiCodeTracker {
    fn new() -> Self {
        Self {
            bold: false,
            dim: false,
            italic: false,
            underline: false,
            blink: false,
            inverse: false,
            hidden: false,
            strikethrough: false,
            fg_color: None,
            bg_color: None,
            active_hyperlink: None,
        }
    }

    fn process(&mut self, ansi_code: &str) {
        if let Some(hyperlink_result) = parse_osc8_hyperlink(ansi_code) {
            self.active_hyperlink = hyperlink_result;
            return;
        }
        if !ansi_code.ends_with('m') {
            return;
        }
        let Some(rest) = ansi_code.strip_prefix("\x1b[") else {
            return;
        };
        let Some(params) = rest.strip_suffix('m') else {
            return;
        };
        if !params.chars().all(|c| c.is_ascii_digit() || c == ';') {
            return;
        }
        if params.is_empty() || params == "0" {
            self.reset();
            return;
        }

        let parts: Vec<&str> = params.split(';').collect();
        let mut i = 0;
        while i < parts.len() {
            let code: i32 = match parts[i].parse() {
                Ok(v) => v,
                Err(_) => {
                    i += 1;
                    continue;
                }
            };

            if code == 38 || code == 48 {
                if parts.get(i + 1) == Some(&"5") && parts.get(i + 2).is_some() {
                    let color_code = format!("{};{};{}", parts[i], parts[i + 1], parts[i + 2]);
                    if code == 38 {
                        self.fg_color = Some(color_code);
                    } else {
                        self.bg_color = Some(color_code);
                    }
                    i += 3;
                    continue;
                } else if parts.get(i + 1) == Some(&"2") && parts.get(i + 4).is_some() {
                    let color_code = format!(
                        "{};{};{};{};{}",
                        parts[i],
                        parts[i + 1],
                        parts[i + 2],
                        parts[i + 3],
                        parts[i + 4]
                    );
                    if code == 38 {
                        self.fg_color = Some(color_code);
                    } else {
                        self.bg_color = Some(color_code);
                    }
                    i += 5;
                    continue;
                }
            }

            match code {
                0 => self.reset(),
                1 => self.bold = true,
                2 => self.dim = true,
                3 => self.italic = true,
                4 => self.underline = true,
                5 => self.blink = true,
                7 => self.inverse = true,
                8 => self.hidden = true,
                9 => self.strikethrough = true,
                21 => self.bold = false,
                22 => {
                    self.bold = false;
                    self.dim = false;
                }
                23 => self.italic = false,
                24 => self.underline = false,
                25 => self.blink = false,
                27 => self.inverse = false,
                28 => self.hidden = false,
                29 => self.strikethrough = false,
                39 => self.fg_color = None,
                49 => self.bg_color = None,
                _ => {
                    if (30..=37).contains(&code) || (90..=97).contains(&code) {
                        self.fg_color = Some(code.to_string());
                    } else if (40..=47).contains(&code) || (100..=107).contains(&code) {
                        self.bg_color = Some(code.to_string());
                    }
                }
            }
            i += 1;
        }
    }

    fn reset(&mut self) {
        self.bold = false;
        self.dim = false;
        self.italic = false;
        self.underline = false;
        self.blink = false;
        self.inverse = false;
        self.hidden = false;
        self.strikethrough = false;
        self.fg_color = None;
        self.bg_color = None;
    }

    fn get_active_codes(&self) -> String {
        let mut codes: Vec<String> = Vec::new();
        if self.bold {
            codes.push("1".to_string());
        }
        if self.dim {
            codes.push("2".to_string());
        }
        if self.italic {
            codes.push("3".to_string());
        }
        if self.underline {
            codes.push("4".to_string());
        }
        if self.blink {
            codes.push("5".to_string());
        }
        if self.inverse {
            codes.push("7".to_string());
        }
        if self.hidden {
            codes.push("8".to_string());
        }
        if self.strikethrough {
            codes.push("9".to_string());
        }
        if let Some(fg) = &self.fg_color {
            codes.push(fg.clone());
        }
        if let Some(bg) = &self.bg_color {
            codes.push(bg.clone());
        }
        let mut result = if codes.is_empty() {
            String::new()
        } else {
            format!("\x1b[{}m", codes.join(";"))
        };
        if let Some(hyperlink) = &self.active_hyperlink {
            result.push_str(&format_osc8_hyperlink(hyperlink));
        }
        result
    }

    fn get_line_end_reset(&self) -> String {
        let mut result = String::new();
        if self.underline {
            result.push_str("\x1b[24m");
        }
        if let Some(hyperlink) = &self.active_hyperlink {
            result.push_str(&format_osc8_close(hyperlink.terminator));
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_whitespace_char_matches_common_cases() {
        assert!(is_whitespace_char(" "));
        assert!(is_whitespace_char("\t"));
        assert!(!is_whitespace_char("a"));
        assert!(!is_whitespace_char(""));
    }

    #[test]
    fn is_punctuation_char_matches_fixed_set() {
        assert!(is_punctuation_char("."));
        assert!(is_punctuation_char("("));
        assert!(!is_punctuation_char("a"));
        assert!(!is_punctuation_char(" "));
    }

    #[test]
    fn visible_width_ascii_fast_path() {
        assert_eq!(visible_width("hello"), 5);
        assert_eq!(visible_width(""), 0);
    }

    #[test]
    fn visible_width_strips_ansi() {
        assert_eq!(visible_width("\x1b[1mhi\x1b[0m"), 2);
    }

    #[test]
    fn extract_ansi_code_csi_sgr() {
        let code = extract_ansi_code("\x1b[1;31mtext", 0).unwrap();
        assert_eq!(code.code, "\x1b[1;31m");
        assert_eq!(code.length, 7);
    }

    #[test]
    fn extract_ansi_code_returns_none_for_plain_text() {
        assert!(extract_ansi_code("hello", 0).is_none());
    }
}
