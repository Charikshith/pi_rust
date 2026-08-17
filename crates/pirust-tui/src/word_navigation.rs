//! Port of `packages/tui/src/word-navigation.ts` — pure cursor-offset word
//! navigation used by the line editor. See `docs/analysis/05-tui.md` §5/§9.
//! Internal helper — not part of `index.ts`'s public surface.
//!
//! ## Scope decisions (documented, not silent — AGENTS.md Correctness Bar)
//!
//! - **UTF-16 code-unit offsets, by design.** `cursor` in the TS is a UTF-16
//!   code-unit offset (`text.slice(0, cursor)`, `segment.length` are JS string
//!   semantics), NOT a byte offset and NOT a `char` count — `plan.md`'s Wave 6
//!   entry names this "the single biggest porting hazard" for `editor.rs`,
//!   which will call these exact functions with its own UTF-16 cursor state.
//!   [`find_word_backward`]/[`find_word_forward`] therefore take and return
//!   `usize` counted in UTF-16 units (an astral-plane char counts as 2), so
//!   Wave 6 needs zero adaptation. Implementation: decode the relevant slice
//!   via [`str::encode_utf16`]/`String::from_utf16_lossy`, do all length
//!   arithmetic in UTF-16-unit space, and only look at segment *content*
//!   (never position) for whitespace/punctuation/word-like classification.
//! - **`isWordLike` derivation.** `Intl.Segmenter(undefined, {granularity:
//!   "word"})` tags each segment with `isWordLike: boolean` (per UAX#29's
//!   word-break "word" categories). `unicode-segmentation`'s
//!   `split_word_bounds` gives the same boundaries but no such flag; this port
//!   derives it as "segment contains at least one alphanumeric `char`", the
//!   standard UAX#29 approximation. Verified against the oracle's emoji/
//!   punctuation-run/CJK-punctuation cases; no divergence found there.
//! - **Known gap: CJK dictionary segmentation.** V8's `Intl.Segmenter` (via
//!   ICU) applies a *dictionary-based* word breaker on top of plain UAX#29 for
//!   Chinese/Japanese text with no delimiters — it can group a run of Han
//!   ideographs into one recognized multi-character word (e.g. "日本語" as a
//!   single segment). `unicode-segmentation`'s `split_word_bounds` has no such
//!   dictionary and, per plain UAX#29, gives each Han ideograph its own
//!   word-break unit. Confirmed via the oracle: `find_word_forward` on
//!   "日本語のテキスト" from cursor 0 returns `1` here vs. real Pi's `3`
//!   (Pi's segmenter groups "日本語" as one word; this port sees "日" alone).
//!   Pure-katakana/hiragana runs are NOT affected (both algorithms treat
//!   `split_word_bounds`'s script-continuation rules the same way there,
//!   confirmed by `find_word_backward` on the same string landing on the
//!   identical boundary as Pi). No Rust crate in this dependency tree ships a
//!   CJK segmentation dictionary; implementing one is out of this wave's
//!   scope. `tests/word_navigation_golden.rs` documents and skips this one
//!   specific, understood case rather than asserting a guaranteed-false
//!   equality or silently dropping the oracle case.
//! - **`isWhitespaceChar` is NOT reused from `utils.rs`.** `utils::is_whitespace_char`
//!   mirrors a call site that always passes a single character and checks only
//!   the first `char` of its input. This file's TS source (`isWhitespaceChar`,
//!   utils.ts:826, `/\s/.test(char)`) is called with whole *segments*
//!   (potentially multi-character runs), and `/\s/.test(str)` without the
//!   global flag matches if the string contains a whitespace character
//!   *anywhere*, not just at the start. Reusing the first-char-only helper
//!   here would silently diverge for a hypothetical non-homogeneous segment.
//!   [`segment_is_whitespace`] instead matches the TS's literal "contains any
//!   whitespace" semantics directly, at the (trivial) cost of not sharing code
//!   with `utils.rs`. `PUNCTUATION_REGEX` membership IS reused as-is via
//!   `utils::PUNCTUATION_CHARS` (a plain fixed character set has no such
//!   shape mismatch).

use unicode_segmentation::UnicodeSegmentation;

use crate::utils::PUNCTUATION_CHARS;

/// A single word-boundary segment plus its Unicode-word-break classification —
/// the Rust shape of a JS `Intl.SegmentData` (`{ segment, isWordLike }`) as
/// consumed by `findWordBackward`/`findWordForward`.
#[derive(Debug, Clone)]
pub struct WordSegment {
    pub text: String,
    pub is_word_like: bool,
}

/// A custom segmenter override, returning word segments for the given text.
pub type SegmentFn<'a> = dyn Fn(&str) -> Vec<WordSegment> + 'a;
/// A predicate identifying atomic segments to treat as single units (e.g.
/// paste markers).
pub type AtomicSegmentFn<'a> = dyn Fn(&str) -> bool + 'a;

/// Options for word navigation (`WordNavigationOptions`, word-navigation.ts:9).
/// `segment`/`is_atomic_segment` mirror the TS's caller-injection seam so
/// `editor.rs` (Wave 6) can supply a paste-marker-aware segmenter/atomic-check
/// without a signature change here. Nothing calls these yet in this wave.
#[derive(Default)]
pub struct WordNavigationOptions<'a> {
    /// Custom segmenter returning word segments for the given text.
    pub segment: Option<&'a SegmentFn<'a>>,
    /// Predicate identifying atomic segments to treat as single units (e.g.
    /// paste markers).
    pub is_atomic_segment: Option<&'a AtomicSegmentFn<'a>>,
}

/// `/\s/.test(segment)` (utils.ts:826, called on a whole segment here — see
/// module docs for why this isn't `utils::is_whitespace_char`).
fn segment_is_whitespace(segment: &str) -> bool {
    segment.chars().any(char::is_whitespace)
}

fn is_word_like(segment: &str) -> bool {
    segment.chars().any(char::is_alphanumeric)
}

fn default_segments(text: &str) -> Vec<WordSegment> {
    text.split_word_bounds()
        .map(|s| WordSegment {
            text: s.to_string(),
            is_word_like: is_word_like(s),
        })
        .collect()
}

fn segments_for(text: &str, options: Option<&WordNavigationOptions>) -> Vec<WordSegment> {
    match options.and_then(|o| o.segment) {
        Some(seg_fn) => seg_fn(text),
        None => default_segments(text),
    }
}

fn is_atomic(options: Option<&WordNavigationOptions>, segment: &str) -> bool {
    options
        .and_then(|o| o.is_atomic_segment)
        .is_some_and(|f| f(segment))
}

fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// First match of `PUNCTUATION_REGEX` (`.exec(segment)?.index`, non-global —
/// first occurrence only), as a UTF-16 offset from the start of `segment`.
fn first_punctuation_utf16_index(segment: &str) -> Option<usize> {
    let mut utf16_idx = 0usize;
    for c in segment.chars() {
        if PUNCTUATION_CHARS.contains(&c) {
            return Some(utf16_idx);
        }
        utf16_idx += c.len_utf16();
    }
    None
}

/// UTF-16 offset immediately after the LAST `PUNCTUATION_REGEX` match
/// (`[...segment.matchAll(...)]`; `lastMatch.index + lastMatch[0].length`).
/// Every punctuation char in `PUNCTUATION_REGEX` is ASCII (1 UTF-16 unit), so
/// this is always `index_of_last_match + 1`.
fn last_punctuation_utf16_boundary(segment: &str) -> Option<usize> {
    let mut utf16_idx = 0usize;
    let mut last = None;
    for c in segment.chars() {
        let w = c.len_utf16();
        if PUNCTUATION_CHARS.contains(&c) {
            last = Some(utf16_idx + w);
        }
        utf16_idx += w;
    }
    last
}

/// Find the cursor position after moving one word backward from `cursor` in
/// `text` (`findWordBackward`, word-navigation.ts:22). `cursor` is a UTF-16
/// code-unit offset (see module docs). Pure function — does not mutate state.
pub fn find_word_backward(
    text: &str,
    cursor: usize,
    options: Option<&WordNavigationOptions>,
) -> usize {
    if cursor == 0 {
        return 0;
    }
    let units: Vec<u16> = text.encode_utf16().collect();
    // `text.slice(0, cursor)` clamps internally on an out-of-range `cursor`,
    // but the TS's `newCursor = cursor` baseline does NOT — an overshot
    // cursor is only visible in the slice, not in the return-value arithmetic.
    let slice_end = cursor.min(units.len());
    let text_before_cursor = String::from_utf16_lossy(&units[..slice_end]);

    let segments = segments_for(&text_before_cursor, options);
    let mut len = segments.len();
    let mut new_cursor = cursor;

    // Skip trailing whitespace.
    while len > 0 {
        let last = &segments[len - 1];
        if is_atomic(options, &last.text) || !segment_is_whitespace(&last.text) {
            break;
        }
        new_cursor -= utf16_len(&last.text);
        len -= 1;
    }

    if len == 0 {
        return new_cursor;
    }

    let last = &segments[len - 1];
    if is_atomic(options, &last.text) {
        // Skip one atomic segment.
        new_cursor -= utf16_len(&last.text);
    } else if last.is_word_like {
        // Skip inside one word-like segment, preserving ASCII punctuation boundaries.
        let seg_len = utf16_len(&last.text);
        match last_punctuation_utf16_boundary(&last.text) {
            Some(boundary) => new_cursor -= seg_len - boundary,
            None => new_cursor -= seg_len,
        }
    } else {
        // Skip non-word non-whitespace run (punctuation).
        while len > 0 {
            let s = &segments[len - 1];
            if is_atomic(options, &s.text) || s.is_word_like || segment_is_whitespace(&s.text) {
                break;
            }
            new_cursor -= utf16_len(&s.text);
            len -= 1;
        }
    }

    new_cursor
}

/// Find the cursor position after moving one word forward from `cursor` in
/// `text` (`findWordForward`, word-navigation.ts:78). `cursor` is a UTF-16
/// code-unit offset (see module docs). Pure function — does not mutate state.
pub fn find_word_forward(
    text: &str,
    cursor: usize,
    options: Option<&WordNavigationOptions>,
) -> usize {
    let units: Vec<u16> = text.encode_utf16().collect();
    if cursor >= units.len() {
        return units.len();
    }
    let text_after_cursor = String::from_utf16_lossy(&units[cursor..]);

    let segments = segments_for(&text_after_cursor, options);
    let mut idx = 0usize;
    let mut new_cursor = cursor;

    // Skip leading whitespace.
    while idx < segments.len() {
        let seg = &segments[idx];
        if is_atomic(options, &seg.text) || !segment_is_whitespace(&seg.text) {
            break;
        }
        new_cursor += utf16_len(&seg.text);
        idx += 1;
    }

    let Some(seg) = segments.get(idx) else {
        return new_cursor;
    };

    if is_atomic(options, &seg.text) {
        // Skip one atomic segment.
        new_cursor += utf16_len(&seg.text);
    } else if seg.is_word_like {
        // Skip inside one word-like segment, preserving ASCII punctuation boundaries.
        match first_punctuation_utf16_index(&seg.text) {
            Some(i) => new_cursor += i,
            None => new_cursor += utf16_len(&seg.text),
        }
    } else {
        // Skip non-word non-whitespace run (punctuation).
        while idx < segments.len() {
            let s = &segments[idx];
            if is_atomic(options, &s.text) || s.is_word_like || segment_is_whitespace(&s.text) {
                break;
            }
            new_cursor += utf16_len(&s.text);
            idx += 1;
        }
    }

    new_cursor
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backward_cursor_zero_short_circuits() {
        assert_eq!(find_word_backward("hello world", 0, None), 0);
    }

    #[test]
    fn forward_cursor_at_length_short_circuits() {
        assert_eq!(find_word_forward("hello world", 11, None), 11);
    }

    #[test]
    fn forward_cursor_beyond_length_clamps() {
        assert_eq!(find_word_forward("hi", 50, None), 2);
    }

    #[test]
    fn backward_skips_one_word() {
        assert_eq!(find_word_backward("hello world", 11, None), 6);
    }

    #[test]
    fn forward_skips_one_word() {
        assert_eq!(find_word_forward("hello world", 0, None), 5);
    }
}
