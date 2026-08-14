//! Port of `core/tools/truncate.ts` — shared output truncation.
//!
//! Gated by `tests/fixtures/pi/tools/truncate.cases.jsonl` (71 cases from real Pi).
//!
//! Truncation is driven by two independent limits — whichever is hit first wins
//! (TS `truncate.ts:1-9`): a line limit ([`DEFAULT_MAX_LINES`]) and a byte limit
//! ([`DEFAULT_MAX_BYTES`]). Output never contains a partial line, except
//! [`truncate_tail`]'s documented edge case.
//!
//! Byte accounting is UTF-8: Pi calls `Buffer.byteLength(s, "utf-8")`, which for a
//! Rust `&str` is exactly `s.len()`.
//!
//! Two behaviours captured from real Pi look like bugs but *are* the contract; the
//! fixture pins them and this port reproduces them verbatim:
//! - `truncate_head("aa\nbb\ncc\n", max_bytes = 8)` reports `truncatedBy = "lines"`
//!   although the line limit was never reached (fixture case 16). `total_bytes` is 9
//!   (the trailing newline counts) so `truncated = true`, but the collecting loop
//!   charges a newline only *between* lines, so nothing ever overflows the limit and
//!   the initialiser `truncatedBy = "lines"` survives (TS `truncate.ts:124`).
//! - `truncate_tail(content, max_bytes = 0)` returns `content = ""` yet reports
//!   `output_lines = 1` and `last_line_partial = true` (fixture case 42): the
//!   partial-line branch unshifts an empty string into the output array
//!   (TS `truncate.ts:207-212`).
//!
//! One deliberate, bounded divergence exists — see [`truncate_line`].

use std::borrow::Cow;

use pirust_ai::jsnum::js_number;
use serde::{Deserialize, Serialize};

/// Default maximum number of lines (TS `truncate.ts:11`).
pub const DEFAULT_MAX_LINES: u64 = 2000;

/// Default maximum number of bytes — 50KB (TS `truncate.ts:12`).
pub const DEFAULT_MAX_BYTES: u64 = 50 * 1024;

/// Maximum UTF-16 code units per grep match line (TS `truncate.ts:13`).
pub const GREP_MAX_LINE_LENGTH: usize = 500;

/// Suffix appended to a line cut by [`truncate_line`] (TS `truncate.ts:275`).
const TRUNCATED_SUFFIX: &str = "... [truncated]";

/// Which limit was hit (TS `truncate.ts:21`: `"lines" | "bytes"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TruncatedBy {
    Lines,
    Bytes,
}

/// Outcome of a head/tail truncation (TS `TruncationResult`, `truncate.ts:15-38`).
///
/// This value is persisted: tools put it in `details.truncation`, which lands in the
/// session JSONL. Field order and names therefore mirror Pi's object literals
/// (`truncate.ts:88-100`) so `serde_json` reproduces `JSON.stringify` byte for byte —
/// including `"truncatedBy":null`, which Pi always emits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TruncationResult {
    /// The truncated content (TS `truncate.ts:17`).
    pub content: String,
    /// Whether truncation occurred (TS `truncate.ts:19`).
    pub truncated: bool,
    /// Which limit was hit, or `None` if not truncated (TS `truncate.ts:21`).
    pub truncated_by: Option<TruncatedBy>,
    /// Total number of lines in the original content (TS `truncate.ts:23`).
    pub total_lines: u64,
    /// Total number of bytes in the original content (TS `truncate.ts:25`).
    pub total_bytes: u64,
    /// Number of complete lines in the truncated output (TS `truncate.ts:27`).
    pub output_lines: u64,
    /// Number of bytes in the truncated output (TS `truncate.ts:29`).
    pub output_bytes: u64,
    /// Whether the last line was partially truncated — tail truncation only
    /// (TS `truncate.ts:31`).
    pub last_line_partial: bool,
    /// Whether the first line exceeded the byte limit — head truncation only
    /// (TS `truncate.ts:33`).
    pub first_line_exceeds_limit: bool,
    /// The max lines limit that was applied (TS `truncate.ts:35`).
    pub max_lines: u64,
    /// The max bytes limit that was applied (TS `truncate.ts:37`).
    pub max_bytes: u64,
}

impl TruncationResult {
    /// The "no truncation needed" result, shared by [`truncate_head`]
    /// (TS `truncate.ts:87-101`) and [`truncate_tail`] (TS `truncate.ts:177-191`),
    /// which build the identical literal.
    fn untouched(
        content: &str,
        total_lines: u64,
        total_bytes: u64,
        max_lines: u64,
        max_bytes: u64,
    ) -> Self {
        Self {
            content: content.to_string(),
            truncated: false,
            truncated_by: None,
            total_lines,
            total_bytes,
            output_lines: total_lines,
            output_bytes: total_bytes,
            last_line_partial: false,
            first_line_exceeds_limit: false,
            max_lines,
            max_bytes,
        }
    }
}

/// Truncation limits (TS `TruncationOptions`, `truncate.ts:40-45`).
///
/// `None` means "apply Pi's `??` default": [`DEFAULT_MAX_LINES`] / [`DEFAULT_MAX_BYTES`]
/// (TS `truncate.ts:79-80`). `u64` rather than `usize` because Pi's callers pass
/// `Number.MAX_SAFE_INTEGER` as `maxLines` (e.g. `find.ts:189`, `ls.ts:182`,
/// `grep.ts:335`) and that value must round-trip through the persisted result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TruncationOptions {
    /// Maximum number of lines (TS `truncate.ts:42`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_lines: Option<u64>,
    /// Maximum number of bytes (TS `truncate.ts:44`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_bytes: Option<u64>,
}

/// Split content into countable lines (TS `splitLinesForCounting`,
/// `truncate.ts:47-56`).
///
/// Empty content has zero lines, and a trailing newline does not open a new one — so
/// `"a\nb\n"` and `"a\nb"` both count 2 lines (fixture cases 5-7).
fn split_lines_for_counting(content: &str) -> Vec<&str> {
    if content.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<&str> = content.split('\n').collect();
    if content.ends_with('\n') {
        lines.pop();
    }
    lines
}

/// ECMAScript `Number.prototype.toFixed(1)` (ECMA-262 §21.1.3.3), as used by
/// [`format_size`] (TS `truncate.ts:65`, `truncate.ts:67`).
///
/// Rust's `{:.1}` is not a drop-in replacement: it rounds halfway cases to even,
/// whereas `toFixed` picks *the larger* candidate ("If there are two such n, pick the
/// larger n"), i.e. half-away-from-zero on the double's exact decimal value. They
/// disagree for e.g. `1280 / 1024 == 1.25`, where Pi yields `"1.3"` and `{:.1}` yields
/// `"1.2"`.
///
/// The exact value is read off a 40-fractional-digit rendering: Rust's precision
/// formatting is exact-value based, and 40 digits is well past the ~17 significant
/// digits that separate adjacent doubles, so the digit driving the rounding decision is
/// the true one rather than an artefact of the printing.
fn to_fixed_1(v: f64) -> String {
    if !v.is_finite() || v.abs() >= 1e21 {
        // §21.1.3.3 step 8: fall back to `Number::toString`.
        return js_number(v);
    }
    let negative = v < 0.0;
    let magnitude = v.abs();
    let exact = format!("{magnitude:.40}");
    let (int_part, frac_part) = exact
        .split_once('.')
        .expect("precision formatting always emits a '.'");

    // Kept digits: the whole integer part plus one fractional digit.
    let mut digits: Vec<u8> = int_part
        .bytes()
        .chain(frac_part.bytes().take(1))
        .map(|b| b - b'0')
        .collect();

    // Everything past the kept digit is the remainder; `>= 5` in the first dropped
    // position means the remainder is >= half, which rounds up (ties included).
    if frac_part.as_bytes().get(1).copied().unwrap_or(b'0') >= b'5' {
        let mut idx = digits.len();
        loop {
            if idx == 0 {
                digits.insert(0, 1);
                break;
            }
            idx -= 1;
            if digits[idx] == 9 {
                digits[idx] = 0;
            } else {
                digits[idx] += 1;
                break;
            }
        }
    }

    let frac_digit = digits.pop().expect("one fractional digit was pushed");
    let mut out = String::with_capacity(digits.len() + 3);
    if negative {
        out.push('-');
    }
    for d in &digits {
        out.push(char::from(d + b'0'));
    }
    out.push('.');
    out.push(char::from(frac_digit + b'0'));
    out
}

/// Format bytes as human-readable size (TS `formatSize`, `truncate.ts:58-69`).
///
/// `< 1024` is reported as raw bytes, below 1MiB as KB with one decimal, otherwise MB
/// with one decimal — both decimals via [`to_fixed_1`], not `{:.1}`.
pub fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes}B")
    } else if bytes < 1024 * 1024 {
        format!("{}KB", to_fixed_1(bytes as f64 / 1024.0))
    } else {
        format!("{}MB", to_fixed_1(bytes as f64 / (1024.0 * 1024.0)))
    }
}

/// Truncate content from the head — keep the first N lines/bytes (TS `truncateHead`,
/// `truncate.ts:71-160`). Suitable for file reads, where the beginning matters.
///
/// Never returns a partial line: if the first line alone exceeds the byte limit the
/// content is empty and `first_line_exceeds_limit` is set (TS `truncate.ts:103-119`).
pub fn truncate_head(content: &str, options: TruncationOptions) -> TruncationResult {
    let max_lines = options.max_lines.unwrap_or(DEFAULT_MAX_LINES);
    let max_bytes = options.max_bytes.unwrap_or(DEFAULT_MAX_BYTES);

    let total_bytes = content.len() as u64;
    let lines = split_lines_for_counting(content);
    let total_lines = lines.len() as u64;

    // No truncation needed (TS `truncate.ts:87`).
    if total_lines <= max_lines && total_bytes <= max_bytes {
        return TruncationResult::untouched(
            content,
            total_lines,
            total_bytes,
            max_lines,
            max_bytes,
        );
    }

    // First line alone over the byte limit (TS `truncate.ts:104`). Pi indexes `lines[0]`
    // unguarded; `lines` is empty only for empty content, which always took the early
    // return above, so the `0` fallback is unreachable rather than a behaviour choice.
    let first_line_bytes = lines.first().map_or(0, |l| l.len() as u64);
    if first_line_bytes > max_bytes {
        return TruncationResult {
            content: String::new(),
            truncated: true,
            truncated_by: Some(TruncatedBy::Bytes),
            total_lines,
            total_bytes,
            output_lines: 0,
            output_bytes: 0,
            last_line_partial: false,
            first_line_exceeds_limit: true,
            max_lines,
            max_bytes,
        };
    }

    // Collect complete lines that fit (TS `truncate.ts:121-137`).
    let mut collected: Vec<&str> = Vec::new();
    let mut output_bytes_count: u64 = 0;
    let mut truncated_by = TruncatedBy::Lines;

    for (i, &line) in lines.iter().enumerate() {
        if i as u64 >= max_lines {
            break;
        }
        // +1 for the joining newline. NOTE: Pi keys this off the *source* index `i`
        // (TS `truncate.ts:128`), unlike `truncate_tail`, which keys off how many lines
        // have been collected. Kept as written.
        let line_bytes = line.len() as u64 + u64::from(i > 0);

        if output_bytes_count + line_bytes > max_bytes {
            truncated_by = TruncatedBy::Bytes;
            break;
        }

        collected.push(line);
        output_bytes_count += line_bytes;
    }

    // Exited due to the line limit (TS `truncate.ts:139-142`).
    if collected.len() as u64 >= max_lines && output_bytes_count <= max_bytes {
        truncated_by = TruncatedBy::Lines;
    }

    let output_content = collected.join("\n");
    let final_output_bytes = output_content.len() as u64;

    TruncationResult {
        content: output_content,
        truncated: true,
        truncated_by: Some(truncated_by),
        total_lines,
        total_bytes,
        output_lines: collected.len() as u64,
        output_bytes: final_output_bytes,
        last_line_partial: false,
        first_line_exceeds_limit: false,
        max_lines,
        max_bytes,
    }
}

/// Truncate content from the tail — keep the last N lines/bytes (TS `truncateTail`,
/// `truncate.ts:162-241`). Suitable for bash output, where the end matters.
///
/// May return a partial *first* line when the final line of the original content alone
/// exceeds the byte limit (TS `truncate.ts:205-212`); that is the only case where
/// `last_line_partial` is set.
pub fn truncate_tail(content: &str, options: TruncationOptions) -> TruncationResult {
    let max_lines = options.max_lines.unwrap_or(DEFAULT_MAX_LINES);
    let max_bytes = options.max_bytes.unwrap_or(DEFAULT_MAX_BYTES);

    let total_bytes = content.len() as u64;
    let lines = split_lines_for_counting(content);
    let total_lines = lines.len() as u64;

    // No truncation needed (TS `truncate.ts:177`).
    if total_lines <= max_lines && total_bytes <= max_bytes {
        return TruncationResult::untouched(
            content,
            total_lines,
            total_bytes,
            max_lines,
            max_bytes,
        );
    }

    // Work backwards from the end (TS `truncate.ts:193-218`). Pi `unshift`s onto the
    // output array; collecting in reverse and flipping once is equivalent, and
    // `collected.len()` matches Pi's `outputLinesArr.length` at every step.
    let mut collected: Vec<Cow<'_, str>> = Vec::new();
    let mut output_bytes_count: u64 = 0;
    let mut truncated_by = TruncatedBy::Lines;
    let mut last_line_partial = false;

    for &line in lines.iter().rev() {
        if collected.len() as u64 >= max_lines {
            break;
        }
        // +1 for the joining newline. NOTE: keyed off how many lines are already
        // collected (TS `truncate.ts:201`), unlike `truncate_head`'s source index.
        let line_bytes = line.len() as u64 + u64::from(!collected.is_empty());

        if output_bytes_count + line_bytes > max_bytes {
            truncated_by = TruncatedBy::Bytes;
            // Edge case: nothing collected yet and this line alone exceeds the limit,
            // so keep the end of it (TS `truncate.ts:205-212`). With `max_bytes = 0`
            // this pushes an empty string, which is why fixture case 42 reports
            // `output_lines = 1` for empty content.
            if collected.is_empty() {
                let truncated_line = truncate_string_to_bytes_from_end(line, max_bytes);
                output_bytes_count = truncated_line.len() as u64;
                collected.push(Cow::Borrowed(truncated_line));
                last_line_partial = true;
            }
            break;
        }

        collected.push(Cow::Borrowed(line));
        output_bytes_count += line_bytes;
    }

    collected.reverse();

    // Exited due to the line limit (TS `truncate.ts:220-223`).
    if collected.len() as u64 >= max_lines && output_bytes_count <= max_bytes {
        truncated_by = TruncatedBy::Lines;
    }

    let output_content = collected.join("\n");
    let final_output_bytes = output_content.len() as u64;

    TruncationResult {
        content: output_content,
        truncated: true,
        truncated_by: Some(truncated_by),
        total_lines,
        total_bytes,
        output_lines: collected.len() as u64,
        output_bytes: final_output_bytes,
        last_line_partial,
        first_line_exceeds_limit: false,
        max_lines,
        max_bytes,
    }
}

/// Truncate a string to fit within a byte limit, keeping the end
/// (TS `truncateStringToBytesFromEnd`, `truncate.ts:243-262`).
///
/// The cut is moved forward off UTF-8 continuation bytes (`b & 0xC0 == 0x80`), exactly
/// as Pi does; in valid UTF-8 the first non-continuation byte at or after the cut is a
/// char boundary, so the resulting slice is always well-formed (fixture case 37).
fn truncate_string_to_bytes_from_end(s: &str, max_bytes: u64) -> &str {
    let bytes = s.as_bytes();
    if bytes.len() as u64 <= max_bytes {
        return s;
    }

    // Start from the end, skip `max_bytes` back.
    let mut start = bytes.len() - max_bytes as usize;

    // Find a valid UTF-8 boundary (start of a character).
    while start < bytes.len() && (bytes[start] & 0xc0) == 0x80 {
        start += 1;
    }

    &s[start..]
}

/// A single-line truncation (TS `{ text, wasTruncated }`, `truncate.ts:271`), with
/// `text` as a Rust `String` — see [`truncate_line`] for the lossy edge case.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LineTruncation {
    pub text: String,
    pub was_truncated: bool,
}

/// A single-line truncation whose `text` is the raw UTF-16 code-unit sequence Pi
/// produces — including unpaired surrogates, which a `String` cannot hold.
///
/// Not `Serialize`: Pi's wire form is a JS string, not an array of units, so this type
/// exists purely for exactness (byte-identity tests, or a future consumer that needs
/// to re-encode Pi's escapes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineTruncationUtf16 {
    pub text: Vec<u16>,
    pub was_truncated: bool,
}

/// Truncate a single line to `max_chars` **UTF-16 code units**, appending
/// `... [truncated]` (TS `truncateLine`, `truncate.ts:264-276`). Used for grep match
/// lines (`grep.ts:262`, `grep.ts:324`).
///
/// `max_chars = None` applies Pi's default parameter, [`GREP_MAX_LINE_LENGTH`]
/// (TS `truncate.ts:270`).
///
/// # Divergence from Pi (deliberate and bounded)
///
/// Pi measures with `line.length` and cuts with `line.slice(0, maxChars)`. Both are
/// UTF-16 based and codepoint-unsafe, so a cut in the middle of a surrogate pair leaves
/// a **lone high surrogate** in the result. Fixture case 50 is exactly that:
/// `truncateLine("👍👍👍", 3)` returns `"👍\ud83d... [truncated]"`. A Rust `String` is
/// well-formed UTF-8 and cannot represent an unpaired surrogate, so this function
/// replaces each one with U+FFFD REPLACEMENT CHARACTER (`String::from_utf16_lossy`),
/// returning `"👍\u{FFFD}... [truncated]"`.
///
/// Rationale: U+FFFD is the *same* substitution Node itself performs the instant such a
/// string is encoded to UTF-8, and it has the same UTF-8 length (3 bytes) that Pi's own
/// `Buffer.byteLength(loneSurrogate, "utf-8")` reports. Every downstream byte
/// computation therefore agrees: the [`truncate_head`] pass over assembled grep output
/// sees identical byte counts and truncates at identical points. The only observable
/// difference is the escape written for that one character — `"\ud83d"` in Pi's session
/// JSONL versus `"�"` here — and the corresponding bytes sent to the model.
///
/// [`truncate_line_utf16`] returns Pi's exact code units for callers that need
/// byte-identity; `tests/truncate_golden.rs` asserts against those, and against this
/// function's documented U+FFFD output.
pub fn truncate_line(line: &str, max_chars: Option<usize>) -> LineTruncation {
    let limit = max_chars.unwrap_or(GREP_MAX_LINE_LENGTH);
    // A string's UTF-8 length is never below its UTF-16 length, so this is an exact
    // "definitely within the limit" test that skips the UTF-16 conversion.
    if line.len() <= limit {
        return LineTruncation {
            text: line.to_string(),
            was_truncated: false,
        };
    }

    let utf16 = truncate_line_utf16(line, max_chars);
    LineTruncation {
        // The only lossy step in this module; see the divergence note above.
        text: String::from_utf16_lossy(&utf16.text),
        was_truncated: utf16.was_truncated,
    }
}

/// [`truncate_line`] without the UTF-8 round trip: returns the exact UTF-16 code units
/// Pi's `line.slice(0, maxChars)` produces (TS `truncate.ts:272-275`), unpaired
/// surrogates included.
pub fn truncate_line_utf16(line: &str, max_chars: Option<usize>) -> LineTruncationUtf16 {
    let limit = max_chars.unwrap_or(GREP_MAX_LINE_LENGTH);
    let units: Vec<u16> = line.encode_utf16().collect();

    if units.len() <= limit {
        return LineTruncationUtf16 {
            text: units,
            was_truncated: false,
        };
    }

    let mut text = units[..limit].to_vec();
    text.extend(TRUNCATED_SUFFIX.encode_utf16());
    LineTruncationUtf16 {
        text,
        was_truncated: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pins the documented divergence itself (the fixture cannot: real Pi's answer
    /// contains a lone surrogate). Pi returns `"👍\ud83d... [truncated]"` for this
    /// input; `tests/truncate_golden.rs` checks that literal via
    /// [`truncate_line_utf16`]. Here we assert the `String` API's U+FFFD form.
    #[test]
    fn split_surrogate_pair_becomes_replacement_char() {
        let cut = truncate_line("👍👍👍", Some(3));
        assert!(cut.was_truncated);
        assert_eq!(cut.text, "👍\u{FFFD}... [truncated]");
        // The lossy character occupies the same 3 UTF-8 bytes Node reports for the lone
        // surrogate it replaces, so downstream byte accounting is unaffected.
        assert_eq!('\u{FFFD}'.len_utf8(), 3);

        // The exact code units are still available, unpaired high surrogate intact.
        let exact = truncate_line_utf16("👍👍👍", Some(3));
        assert_eq!(&exact.text[..3], &[0xD83D, 0xDC4D, 0xD83D]);
        assert!(String::from_utf16(&exact.text).is_err());
    }

    /// `to_fixed_1` must round halfway cases away from zero, where Rust's `{:.1}`
    /// rounds to even. Guards the helper against being "simplified" back to `{:.1}`.
    #[test]
    fn to_fixed_1_rounds_halfway_up_unlike_rust_formatting() {
        // 1280 / 1024 == 1.25 exactly; ECMAScript picks the larger candidate.
        assert_eq!(to_fixed_1(1.25), "1.3");
        assert_eq!(format!("{:.1}", 1.25_f64), "1.2");
        assert_eq!(to_fixed_1(-1.25), "-1.3");
        assert_eq!(to_fixed_1(0.0), "0.0");
        assert_eq!(to_fixed_1(-0.0), "0.0");
        assert_eq!(to_fixed_1(9.96), "10.0");
    }
}
