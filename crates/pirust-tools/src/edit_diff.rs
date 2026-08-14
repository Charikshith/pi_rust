//! Port of `core/tools/edit-diff.ts` — the match/replace engine behind `edit`,
//! plus the display diff and unified patch it persists into `details`.
//!
//! Gated by `tests/fixtures/pi/tools/edit.diff.corpus.jsonl` (56 cases from real Pi).
//!
//! Everything this module emits is written verbatim into session JSONL
//! (`details.diff` / `details.patch` / `details.firstChangedLine`) or onto disk
//! (`newContent`), so every branch here is a byte-parity surface.
//!
//! # Ported symbols
//! - `detectLineEnding` (`edit-diff.ts:10-16`) → [`detect_line_ending`]
//! - `normalizeToLF` (`:18-20`) → [`normalize_to_lf`]
//! - `restoreLineEndings` (`:22-24`) → [`restore_line_endings`]
//! - `normalizeForFuzzyMatch` (`:33-54`) → [`normalize_for_fuzzy_match`]
//! - `splitLinesWithEndings` (`:56-58`) → [`split_lines_with_endings`]
//! - `getLineSpans` (`:74-81`) → [`get_line_spans`]
//! - `getReplacementLineRange` (`:83-108`) → [`get_replacement_line_range`]
//! - `applyReplacements` (`:110-119`) → [`apply_replacements`]
//! - `applyReplacementsPreservingUnchangedLines` (`:131-172`)
//!   → [`apply_replacements_preserving_unchanged_lines`]
//! - `fuzzyFindText` (`:206-244`) → [`fuzzy_find_text`]
//! - `stripBom` (`:247-249`) → [`strip_bom`]
//! - `countOccurrences` (`:251-255`) → [`count_occurrences`]
//! - `getNotFoundError` / `getDuplicateError` / `getEmptyOldTextError` /
//!   `getNoChangeError` (`:257-293`) → [`EditDiffError`]
//! - `applyEditsToNormalizedContent` (`:304-366`) → [`apply_edits_to_normalized_content`]
//! - `generateUnifiedPatch` (`:369-374`) → [`generate_unified_patch`]
//! - `generateDiffString` (`:380-503`) → [`generate_diff_string`]
//!
//! # Deliberately not ported
//! `computeEditsDiff` (`:518-547`) and `computeEditDiff` (`:553-560`) are TUI-only
//! preview helpers: they do `access` + `readFile` and then call the two functions
//! above, returning `{error}` instead of throwing. They land with the TUI
//! (feat-006/007) alongside `renderCall`, and the corpus does not cover them
//! (its two `lowLevel`-less error cases come from `edit.ts`'s own validation).
//!
//! # The diff itself
//! `generateDiffString`/`generateUnifiedPatch` are built on the npm `diff` package
//! (jsdiff **8.0.4**, a real dependency of `packages/coding-agent`). jsdiff's
//! tie-breaking, hunk grouping and trailing-newline attribution are all directly
//! observable in the persisted bytes, so [`jsdiff`] is a literal port of
//! `node_modules/diff/libesm/`:
//! - `Diff#diff` / `#diffWithOptionsObj` / `#addToPath` / `#extractCommon` /
//!   `#buildValues` (`diff/base.js:1-253`) → [`jsdiff::diff_lines`]
//! - `LineDiff` + `tokenize` (`diff/line.js:3-65`) → [`jsdiff::tokenize`]
//! - `structuredPatch` / `formatPatch` / `splitLines` (`patch/create.js:17-228`)
//!   → [`jsdiff::structured_patch`], [`jsdiff::format_patch`]
//!
//! No Rust diff crate is used: a different-but-valid diff produces different bytes.
//!
//! # Offsets
//! JS string offsets are UTF-16 code units; this port uses UTF-8 byte offsets.
//! That is safe because every offset produced here is only ever compared against,
//! or sliced out of, the *same* string it was measured in — and both encodings are
//! order-preserving with matching substring lengths for a given substring.

use std::borrow::Cow;

use unicode_normalization::UnicodeNormalization;

/// Default `contextLines` for [`generate_diff_string`] / [`generate_unified_patch`]
/// (TS default parameter, `edit-diff.ts:369` and `:383`).
pub const DEFAULT_CONTEXT_LINES: usize = 4;

/// UTF-8 BOM (`"﻿"`, `edit-diff.ts:248`).
const BOM: &str = "\u{feff}";

// ---------------------------------------------------------------------------
// Errors (TS `getNotFoundError` .. `getNoChangeError`, `edit-diff.ts:257-293`,
// plus the two internal invariants thrown at `:96`/`:104` and `:139`).
//
// Every message branches on whether the call carries exactly ONE edit or more;
// the literal strings are reproduced verbatim because they reach the model.
// ---------------------------------------------------------------------------

/// Every failure `applyEditsToNormalizedContent` can throw.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EditDiffError {
    /// `getEmptyOldTextError`, single-edit branch (`edit-diff.ts:281`).
    #[error("oldText must not be empty in {path}.")]
    EmptyOldTextSingle {
        /// The raw (unresolved) path as passed to the tool.
        path: String,
    },

    /// `getEmptyOldTextError`, multi-edit branch (`edit-diff.ts:283`).
    #[error("edits[{edit_index}].oldText must not be empty in {path}.")]
    EmptyOldText {
        /// Index into the caller's `edits` array.
        edit_index: usize,
        /// The raw (unresolved) path as passed to the tool.
        path: String,
    },

    /// `getNotFoundError`, single-edit branch (`edit-diff.ts:259-261`).
    #[error(
        "Could not find the exact text in {path}. The old text must match exactly \
         including all whitespace and newlines."
    )]
    NotFoundSingle {
        /// The raw (unresolved) path as passed to the tool.
        path: String,
    },

    /// `getNotFoundError`, multi-edit branch (`edit-diff.ts:263-265`).
    #[error(
        "Could not find edits[{edit_index}] in {path}. The oldText must match exactly \
         including all whitespace and newlines."
    )]
    NotFound {
        /// Index into the caller's `edits` array.
        edit_index: usize,
        /// The raw (unresolved) path as passed to the tool.
        path: String,
    },

    /// `getDuplicateError`, single-edit branch (`edit-diff.ts:270-272`).
    #[error(
        "Found {occurrences} occurrences of the text in {path}. The text must be unique. \
         Please provide more context to make it unique."
    )]
    DuplicateSingle {
        /// Occurrence count, always measured in fuzzy-normalized space.
        occurrences: usize,
        /// The raw (unresolved) path as passed to the tool.
        path: String,
    },

    /// `getDuplicateError`, multi-edit branch (`edit-diff.ts:274-276`).
    #[error(
        "Found {occurrences} occurrences of edits[{edit_index}] in {path}. Each oldText \
         must be unique. Please provide more context to make it unique."
    )]
    Duplicate {
        /// Occurrence count, always measured in fuzzy-normalized space.
        occurrences: usize,
        /// Index into the caller's `edits` array.
        edit_index: usize,
        /// The raw (unresolved) path as passed to the tool.
        path: String,
    },

    /// Overlap check (`edit-diff.ts:349-353`). The two indices are the *original*
    /// `edits` indices, but reported in match-position order.
    #[error(
        "edits[{first}] and edits[{second}] overlap in {path}. Merge them into one edit \
         or target disjoint regions."
    )]
    Overlap {
        /// Original index of the earlier-matching edit.
        first: usize,
        /// Original index of the later-matching edit.
        second: usize,
        /// The raw (unresolved) path as passed to the tool.
        path: String,
    },

    /// `getNoChangeError`, single-edit branch (`edit-diff.ts:288-290`).
    #[error(
        "No changes made to {path}. The replacement produced identical content. This might \
         indicate an issue with special characters or the text not existing as expected."
    )]
    NoChangeSingle {
        /// The raw (unresolved) path as passed to the tool.
        path: String,
    },

    /// `getNoChangeError`, multi-edit branch (`edit-diff.ts:292`).
    #[error("No changes made to {path}. The replacements produced identical content.")]
    NoChange {
        /// The raw (unresolved) path as passed to the tool.
        path: String,
    },

    /// Internal invariant of `applyReplacementsPreservingUnchangedLines` (`:139`).
    #[error(
        "Cannot preserve unchanged lines because the base content has a different line count."
    )]
    LineCountMismatch,

    /// Internal invariant of `getReplacementLineRange` (`:96` and `:104`).
    #[error("Replacement range is outside the base content.")]
    RangeOutsideBase,
}

impl EditDiffError {
    /// TS `getEmptyOldTextError` (`edit-diff.ts:279-284`).
    fn empty_old_text(path: &str, edit_index: usize, total_edits: usize) -> Self {
        if total_edits == 1 {
            Self::EmptyOldTextSingle {
                path: path.to_string(),
            }
        } else {
            Self::EmptyOldText {
                edit_index,
                path: path.to_string(),
            }
        }
    }

    /// TS `getNotFoundError` (`edit-diff.ts:257-266`).
    fn not_found(path: &str, edit_index: usize, total_edits: usize) -> Self {
        if total_edits == 1 {
            Self::NotFoundSingle {
                path: path.to_string(),
            }
        } else {
            Self::NotFound {
                edit_index,
                path: path.to_string(),
            }
        }
    }

    /// TS `getDuplicateError` (`edit-diff.ts:268-277`).
    fn duplicate(path: &str, edit_index: usize, total_edits: usize, occurrences: usize) -> Self {
        if total_edits == 1 {
            Self::DuplicateSingle {
                occurrences,
                path: path.to_string(),
            }
        } else {
            Self::Duplicate {
                occurrences,
                edit_index,
                path: path.to_string(),
            }
        }
    }

    /// TS `getNoChangeError` (`edit-diff.ts:286-293`).
    fn no_change(path: &str, total_edits: usize) -> Self {
        if total_edits == 1 {
            Self::NoChangeSingle {
                path: path.to_string(),
            }
        } else {
            Self::NoChange {
                path: path.to_string(),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Line-ending / BOM primitives
// ---------------------------------------------------------------------------

/// TS `detectLineEnding` (`edit-diff.ts:10-16`): `"\r\n"` only when the file's
/// first `\n` is part of a CRLF pair, otherwise `"\n"` (including "no newline").
pub fn detect_line_ending(content: &str) -> &'static str {
    let crlf_idx = content.find("\r\n");
    let lf_idx = content.find('\n');
    match (lf_idx, crlf_idx) {
        // `if (lfIdx === -1) return "\n"; if (crlfIdx === -1) return "\n";`
        (None, _) | (Some(_), None) => "\n",
        (Some(lf), Some(crlf)) => {
            if crlf < lf {
                "\r\n"
            } else {
                "\n"
            }
        }
    }
}

/// TS `normalizeToLF` (`edit-diff.ts:18-20`): CRLF → LF, then any bare CR → LF.
pub fn normalize_to_lf(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// TS `restoreLineEndings` (`edit-diff.ts:22-24`).
pub fn restore_line_endings(text: &str, ending: &str) -> String {
    if ending == "\r\n" {
        text.replace('\n', "\r\n")
    } else {
        text.to_string()
    }
}

/// TS `stripBom` (`edit-diff.ts:247-249`): returns `(bom, text)` where `bom` is
/// either `"\u{feff}"` or `""`.
pub fn strip_bom(content: &str) -> (&'static str, &str) {
    match content.strip_prefix(BOM) {
        Some(rest) => (BOM, rest),
        None => ("", content),
    }
}

/// True for the code points JS `String.prototype.trimEnd` strips, i.e. ECMA-262
/// `TrimString`'s `WhiteSpace` ∪ `LineTerminator`: TAB/VT/FF/SP, `Zs`
/// (U+0020, U+00A0, U+1680, U+2000–U+200A, U+202F, U+205F, U+3000), U+FEFF, and
/// LF/CR/U+2028/U+2029.
///
/// Not `char::is_whitespace` (= Unicode `White_Space`), which differs in exactly
/// two code points: it lacks U+FEFF and it includes U+0085 NEL.
fn is_js_trim_whitespace(c: char) -> bool {
    matches!(
        c,
        '\u{9}'
            | '\u{a}'
            | '\u{b}'
            | '\u{c}'
            | '\u{d}'
            | '\u{20}'
            | '\u{a0}'
            | '\u{1680}'
            | '\u{2000}'
            ..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
    )
}

/// TS `normalizeForFuzzyMatch` (`edit-diff.ts:33-54`). Progressive, in this exact
/// order:
/// 1. `normalize("NFKC")`
/// 2. `split("\n").map(trimEnd).join("\n")`
/// 3. `[‘’‚‛]` → `'`
/// 4. `[“”„‟]` → `"`
/// 5. `[‐‑‒–—―−]` → `-`
/// 6. `[  -   　]` → ` `
///
/// Steps 3–6 are fused into one pass: each produces an ASCII code point that is a
/// member of no later class, so a single left-to-right substitution is identical
/// to four sequential `String#replace` calls.
pub fn normalize_for_fuzzy_match(text: &str) -> String {
    let nfkc: String = text.nfkc().collect();

    // Strip trailing whitespace per line (`:38-40`).
    let mut trimmed = String::with_capacity(nfkc.len());
    for (i, line) in nfkc.split('\n').enumerate() {
        if i > 0 {
            trimmed.push('\n');
        }
        trimmed.push_str(line.trim_end_matches(is_js_trim_whitespace));
    }

    trimmed
        .chars()
        .map(|c| match c {
            // Smart single quotes → ' (`:42`)
            '\u{2018}' | '\u{2019}' | '\u{201a}' | '\u{201b}' => '\'',
            // Smart double quotes → " (`:44`)
            '\u{201c}' | '\u{201d}' | '\u{201e}' | '\u{201f}' => '"',
            // Various dashes/hyphens → - (`:48`)
            '\u{2010}'..='\u{2015}' | '\u{2212}' => '-',
            // Special spaces → regular space (`:52`). Note U+2000/U+2001 and
            // U+1680 are deliberately absent from the TS class.
            '\u{a0}' | '\u{2002}'..='\u{200a}' | '\u{202f}' | '\u{205f}' | '\u{3000}' => ' ',
            other => other,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Line spans / replacement plumbing
// ---------------------------------------------------------------------------

/// TS `splitLinesWithEndings` (`edit-diff.ts:56-58`), i.e. the regex
/// `/[^\n]*\n|[^\n]+/g`: every line keeps its own trailing `\n`; a final line
/// without one is still emitted; `""` yields no lines at all.
pub fn split_lines_with_endings(content: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0usize;
    for (i, byte) in content.bytes().enumerate() {
        if byte == b'\n' {
            out.push(&content[start..=i]);
            start = i + 1;
        }
    }
    if start < content.len() {
        out.push(&content[start..]);
    }
    out
}

/// TS `LineSpan` (`edit-diff.ts:60-63`): half-open byte range of one line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineSpan {
    /// Inclusive start offset.
    pub start: usize,
    /// Exclusive end offset (after the line's `\n`, when it has one).
    pub end: usize,
}

/// TS `getLineSpans` (`edit-diff.ts:74-81`).
pub fn get_line_spans(content: &str) -> Vec<LineSpan> {
    let mut offset = 0usize;
    split_lines_with_endings(content)
        .into_iter()
        .map(|line| {
            let span = LineSpan {
                start: offset,
                end: offset + line.len(),
            };
            offset = span.end;
            span
        })
        .collect()
}

/// TS `TextReplacement` (`edit-diff.ts:72`), a `Pick` of `MatchedEdit`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextReplacement {
    /// Offset of the match inside the *base* content it was matched against.
    pub match_index: usize,
    /// Byte length of the matched text.
    pub match_length: usize,
    /// LF-normalized replacement text.
    pub new_text: String,
}

/// TS `MatchedEdit` (`edit-diff.ts:65-70`).
#[derive(Debug, Clone, PartialEq, Eq)]
struct MatchedEdit {
    edit_index: usize,
    replacement: TextReplacement,
}

/// Half-open line range, TS `{ startLine, endLine }` (`edit-diff.ts:107`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LineRange {
    start_line: usize,
    end_line: usize,
}

/// TS `getReplacementLineRange` (`edit-diff.ts:83-108`).
fn get_replacement_line_range(
    lines: &[LineSpan],
    replacement: &TextReplacement,
) -> Result<LineRange, EditDiffError> {
    let replacement_start = replacement.match_index;
    let replacement_end = replacement.match_index + replacement.match_length;

    let mut start_line: Option<usize> = None;
    for (i, line) in lines.iter().enumerate() {
        if replacement_start >= line.start && replacement_start < line.end {
            start_line = Some(i);
            break;
        }
    }
    let start_line = start_line.ok_or(EditDiffError::RangeOutsideBase)?;

    let mut end_line = start_line;
    while end_line < lines.len() && lines[end_line].end < replacement_end {
        end_line += 1;
    }
    if end_line >= lines.len() {
        return Err(EditDiffError::RangeOutsideBase);
    }

    Ok(LineRange {
        start_line,
        end_line: end_line + 1,
    })
}

/// TS `applyReplacements` (`edit-diff.ts:110-119`): iterates **backwards** over
/// position-sorted replacements so earlier offsets stay valid. `offset` is the
/// absolute position `content` starts at inside the base content.
fn apply_replacements(content: &str, replacements: &[TextReplacement], offset: usize) -> String {
    let mut result = content.to_string();
    for replacement in replacements.iter().rev() {
        let match_index = replacement.match_index - offset;
        let mut next = String::with_capacity(result.len() + replacement.new_text.len());
        next.push_str(&result[..match_index]);
        next.push_str(&replacement.new_text);
        next.push_str(&result[match_index + replacement.match_length..]);
        result = next;
    }
    result
}

/// A merged run of line-adjacent replacements (TS `groups` element, `:142`).
#[derive(Debug)]
struct ReplacementGroup {
    start_line: usize,
    end_line: usize,
    replacements: Vec<TextReplacement>,
}

/// TS `applyReplacementsPreservingUnchangedLines` (`edit-diff.ts:131-172`).
///
/// Apply replacements matched against `base_content` (a normalized view) to
/// `original_content`: each replacement widens to the lines it touches, those
/// lines are rewritten from the normalized base, and every other line is copied
/// back byte-for-byte from the original.
pub fn apply_replacements_preserving_unchanged_lines(
    original_content: &str,
    base_content: &str,
    replacements: &[TextReplacement],
) -> Result<String, EditDiffError> {
    let original_lines = split_lines_with_endings(original_content);
    let base_lines = get_line_spans(base_content);
    if original_lines.len() != base_lines.len() {
        return Err(EditDiffError::LineCountMismatch);
    }

    let mut groups: Vec<ReplacementGroup> = Vec::new();
    // `[...replacements].sort((a, b) => a.matchIndex - b.matchIndex)` — JS sort is
    // stable, so `sort_by_key` matches on equal `matchIndex`.
    let mut sorted: Vec<&TextReplacement> = replacements.iter().collect();
    sorted.sort_by_key(|r| r.match_index);

    for replacement in sorted {
        let range = get_replacement_line_range(&base_lines, replacement)?;
        if let Some(current) = groups.last_mut() {
            // Overlapping or merely touching line ranges coalesce.
            if range.start_line < current.end_line {
                current.end_line = current.end_line.max(range.end_line);
                current.replacements.push(replacement.clone());
                continue;
            }
        }
        groups.push(ReplacementGroup {
            start_line: range.start_line,
            end_line: range.end_line,
            replacements: vec![replacement.clone()],
        });
    }

    let mut original_line_index = 0usize;
    let mut result = String::new();
    for group in &groups {
        result.push_str(&original_lines[original_line_index..group.start_line].concat());

        let group_start_offset = base_lines[group.start_line].start;
        let group_end_offset = base_lines[group.end_line - 1].end;
        result.push_str(&apply_replacements(
            &base_content[group_start_offset..group_end_offset],
            &group.replacements,
            group_start_offset,
        ));
        original_line_index = group.end_line;
    }
    result.push_str(&original_lines[original_line_index..].concat());

    Ok(result)
}

// ---------------------------------------------------------------------------
// Matching
// ---------------------------------------------------------------------------

/// TS `FuzzyMatchResult` (`edit-diff.ts:174-188`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuzzyMatchResult<'a> {
    /// Whether a match was found.
    pub found: bool,
    /// Match start offset inside [`Self::content_for_replacement`]; `None` for
    /// TS's `index: -1`.
    pub index: Option<usize>,
    /// Byte length of the matched text.
    pub match_length: usize,
    /// `false` = exact match.
    pub used_fuzzy_match: bool,
    /// The content the offsets refer to: the input on an exact match (or miss),
    /// the fuzzy-normalized input on a fuzzy match.
    pub content_for_replacement: Cow<'a, str>,
}

/// TS `Edit` (`edit-diff.ts:190-193`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Edit {
    /// Text to find.
    pub old_text: String,
    /// Text to substitute in.
    pub new_text: String,
}

/// TS `AppliedEditsResult` (`edit-diff.ts:195-198`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedEditsResult {
    /// Always the LF-normalized *original* content — even on the fuzzy path, so
    /// the diff/patch describe the true file, not the fuzzy view.
    pub base_content: String,
    /// Content after all replacements.
    pub new_content: String,
}

/// TS `fuzzyFindText` (`edit-diff.ts:206-244`): exact `indexOf` first; otherwise
/// search in fuzzy-normalized space and return offsets **in that space**.
pub fn fuzzy_find_text<'a>(content: &'a str, old_text: &str) -> FuzzyMatchResult<'a> {
    // Try exact match first (`:208-217`).
    if let Some(exact_index) = content.find(old_text) {
        return FuzzyMatchResult {
            found: true,
            index: Some(exact_index),
            match_length: old_text.len(),
            used_fuzzy_match: false,
            content_for_replacement: Cow::Borrowed(content),
        };
    }

    // Try fuzzy match — work entirely in normalized space (`:220-222`).
    let fuzzy_content = normalize_for_fuzzy_match(content);
    let fuzzy_old_text = normalize_for_fuzzy_match(old_text);
    let Some(fuzzy_index) = fuzzy_content.find(&fuzzy_old_text) else {
        return FuzzyMatchResult {
            found: false,
            index: None,
            match_length: 0,
            used_fuzzy_match: false,
            content_for_replacement: Cow::Borrowed(content),
        };
    };

    FuzzyMatchResult {
        found: true,
        index: Some(fuzzy_index),
        match_length: fuzzy_old_text.len(),
        used_fuzzy_match: true,
        content_for_replacement: Cow::Owned(fuzzy_content),
    }
}

/// TS `countOccurrences` (`edit-diff.ts:251-255`): **always** counted in
/// fuzzy-normalized space, even when the match itself was exact.
///
/// TS is `fuzzyContent.split(fuzzyOldText).length - 1`. When the normalized
/// needle is empty (e.g. `oldText` is all trailing whitespace) JS `split("")`
/// splits into UTF-16 code units, so the count is `utf16Length - 1` — and `-1`
/// for empty content, which `saturating_sub` renders as `0`. Both are `<= 1`, so
/// the duplicate branch behaves identically.
pub fn count_occurrences(content: &str, old_text: &str) -> usize {
    let fuzzy_content = normalize_for_fuzzy_match(content);
    let fuzzy_old_text = normalize_for_fuzzy_match(old_text);
    if fuzzy_old_text.is_empty() {
        return fuzzy_content.encode_utf16().count().saturating_sub(1);
    }
    fuzzy_content.split(&fuzzy_old_text).count() - 1
}

/// TS `applyEditsToNormalizedContent` (`edit-diff.ts:304-366`).
///
/// All edits are matched against the same content. If **any** edit needs fuzzy
/// matching the whole call switches into fuzzy-normalized space and finishes with
/// [`apply_replacements_preserving_unchanged_lines`] so untouched lines keep their
/// original bytes; otherwise plain [`apply_replacements`] runs.
pub fn apply_edits_to_normalized_content(
    normalized_content: &str,
    edits: &[Edit],
    path: &str,
) -> Result<AppliedEditsResult, EditDiffError> {
    let normalized_edits: Vec<Edit> = edits
        .iter()
        .map(|edit| Edit {
            old_text: normalize_to_lf(&edit.old_text),
            new_text: normalize_to_lf(&edit.new_text),
        })
        .collect();
    let total_edits = normalized_edits.len();

    for (i, edit) in normalized_edits.iter().enumerate() {
        if edit.old_text.is_empty() {
            return Err(EditDiffError::empty_old_text(path, i, total_edits));
        }
    }

    // `initialMatches.some(m => m.usedFuzzyMatch)` — `fuzzy_find_text` is pure, so
    // short-circuiting is equivalent to mapping then `.some`.
    let used_fuzzy_match = normalized_edits
        .iter()
        .any(|edit| fuzzy_find_text(normalized_content, &edit.old_text).used_fuzzy_match);
    let replacement_base_content: Cow<'_, str> = if used_fuzzy_match {
        Cow::Owned(normalize_for_fuzzy_match(normalized_content))
    } else {
        Cow::Borrowed(normalized_content)
    };

    let mut matched_edits: Vec<MatchedEdit> = Vec::with_capacity(total_edits);
    for (i, edit) in normalized_edits.iter().enumerate() {
        let match_result = fuzzy_find_text(&replacement_base_content, &edit.old_text);
        if !match_result.found {
            return Err(EditDiffError::not_found(path, i, total_edits));
        }

        let occurrences = count_occurrences(&replacement_base_content, &edit.old_text);
        if occurrences > 1 {
            return Err(EditDiffError::duplicate(path, i, total_edits, occurrences));
        }

        matched_edits.push(MatchedEdit {
            edit_index: i,
            replacement: TextReplacement {
                match_index: match_result.index.unwrap_or(0),
                match_length: match_result.match_length,
                new_text: edit.new_text.clone(),
            },
        });
    }

    matched_edits.sort_by_key(|m| m.replacement.match_index);
    for pair in matched_edits.windows(2) {
        let (previous, current) = (&pair[0], &pair[1]);
        if previous.replacement.match_index + previous.replacement.match_length
            > current.replacement.match_index
        {
            return Err(EditDiffError::Overlap {
                first: previous.edit_index,
                second: current.edit_index,
                path: path.to_string(),
            });
        }
    }

    let replacements: Vec<TextReplacement> =
        matched_edits.into_iter().map(|m| m.replacement).collect();

    let base_content = normalized_content.to_string();
    let new_content = if used_fuzzy_match {
        apply_replacements_preserving_unchanged_lines(
            normalized_content,
            &replacement_base_content,
            &replacements,
        )?
    } else {
        apply_replacements(&replacement_base_content, &replacements, 0)
    };

    if base_content == new_content {
        return Err(EditDiffError::no_change(path, total_edits));
    }

    Ok(AppliedEditsResult {
        base_content,
        new_content,
    })
}

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------

/// TS `EditDiffResult` (`edit-diff.ts:505-508`) — also the return of
/// `generateDiffString` (`:384`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditDiffResult {
    /// Display diff, `join("\n")` with **no** trailing newline.
    pub diff: String,
    /// Line number (in the new file) of the first added/removed hunk.
    pub first_changed_line: Option<usize>,
}

/// TS `generateUnifiedPatch` (`edit-diff.ts:369-374`) with the default
/// `contextLines = 4`.
pub fn generate_unified_patch(path: &str, old_content: &str, new_content: &str) -> String {
    generate_unified_patch_with_context(path, old_content, new_content, DEFAULT_CONTEXT_LINES)
}

/// TS `generateUnifiedPatch` (`edit-diff.ts:369-374`):
/// `Diff.createTwoFilesPatch(path, path, old, new, undefined, undefined, {context, headerOptions: FILE_HEADERS_ONLY})`.
///
/// Both file names are the same raw relative path; the `undefined` headers mean no
/// `\t<date>` suffix, and `FILE_HEADERS_ONLY` suppresses the `Index:` line and the
/// `===` underline.
pub fn generate_unified_patch_with_context(
    path: &str,
    old_content: &str,
    new_content: &str,
    context_lines: usize,
) -> String {
    let mut patch = jsdiff::structured_patch(path, path, old_content, new_content, context_lines);
    jsdiff::format_patch(&mut patch)
}

/// TS `generateDiffString` (`edit-diff.ts:380-503`) with the default
/// `contextLines = 4`.
pub fn generate_diff_string(old_content: &str, new_content: &str) -> EditDiffResult {
    generate_diff_string_with_context(old_content, new_content, DEFAULT_CONTEXT_LINES)
}

/// TS `generateDiffString` (`edit-diff.ts:380-503`).
///
/// Row formats, exactly: `+{num:>w} {line}`, `-{num:>w} {line}`, ` {num:>w} {line}`
/// and the elision ` {"":>w} ...`, where `w` is the decimal width of
/// `max(oldLines.length, newLines.length)` measured by `split("\n")` on each
/// **whole** document. Output is `join("\n")` with no trailing newline.
pub fn generate_diff_string_with_context(
    old_content: &str,
    new_content: &str,
    context_lines: usize,
) -> EditDiffResult {
    let parts = jsdiff::diff_lines(old_content, new_content);
    let mut output: Vec<String> = Vec::new();

    let old_lines = old_content.split('\n').count();
    let new_lines = new_content.split('\n').count();
    let max_line_num = old_lines.max(new_lines);
    let line_num_width = max_line_num.to_string().len();
    let blank = " ".repeat(line_num_width);

    let mut old_line_num = 1usize;
    let mut new_line_num = 1usize;
    let mut last_was_change = false;
    let mut first_changed_line: Option<usize> = None;

    for (i, part) in parts.iter().enumerate() {
        // `part.value.split("\n")`, dropping the final empty element (`:400-403`).
        let mut raw: Vec<&str> = part.value.split('\n').collect();
        if raw.last() == Some(&"") {
            raw.pop();
        }

        if part.added || part.removed {
            // Capture the first changed line (in the new file) (`:407-409`).
            if first_changed_line.is_none() {
                first_changed_line = Some(new_line_num);
            }

            for line in &raw {
                if part.added {
                    output.push(format!("+{new_line_num:>line_num_width$} {line}"));
                    new_line_num += 1;
                } else {
                    output.push(format!("-{old_line_num:>line_num_width$} {line}"));
                    old_line_num += 1;
                }
            }
            last_was_change = true;
        } else {
            // Context lines — only a few before/after changes (`:426-499`).
            let next_part_is_change =
                i < parts.len() - 1 && (parts[i + 1].added || parts[i + 1].removed);
            let has_leading_change = last_was_change;
            let has_trailing_change = next_part_is_change;

            if has_leading_change && has_trailing_change {
                if raw.len() <= context_lines * 2 {
                    for line in &raw {
                        output.push(format!(" {old_line_num:>line_num_width$} {line}"));
                        old_line_num += 1;
                        new_line_num += 1;
                    }
                } else {
                    let leading_lines = &raw[..context_lines];
                    let trailing_lines = &raw[raw.len() - context_lines..];
                    let skipped_lines = raw.len() - leading_lines.len() - trailing_lines.len();

                    for line in leading_lines {
                        output.push(format!(" {old_line_num:>line_num_width$} {line}"));
                        old_line_num += 1;
                        new_line_num += 1;
                    }

                    output.push(format!(" {blank} ..."));
                    old_line_num += skipped_lines;
                    new_line_num += skipped_lines;

                    for line in trailing_lines {
                        output.push(format!(" {old_line_num:>line_num_width$} {line}"));
                        old_line_num += 1;
                        new_line_num += 1;
                    }
                }
            } else if has_leading_change {
                let shown = context_lines.min(raw.len());
                let shown_lines = &raw[..shown];
                let skipped_lines = raw.len() - shown_lines.len();

                for line in shown_lines {
                    output.push(format!(" {old_line_num:>line_num_width$} {line}"));
                    old_line_num += 1;
                    new_line_num += 1;
                }

                if skipped_lines > 0 {
                    output.push(format!(" {blank} ..."));
                    old_line_num += skipped_lines;
                    new_line_num += skipped_lines;
                }
            } else if has_trailing_change {
                let skipped_lines = raw.len().saturating_sub(context_lines);
                if skipped_lines > 0 {
                    output.push(format!(" {blank} ..."));
                    old_line_num += skipped_lines;
                    new_line_num += skipped_lines;
                }

                for line in &raw[skipped_lines..] {
                    output.push(format!(" {old_line_num:>line_num_width$} {line}"));
                    old_line_num += 1;
                    new_line_num += 1;
                }
            } else {
                // Skip these context lines entirely, but still advance the counters.
                old_line_num += raw.len();
                new_line_num += raw.len();
            }

            last_was_change = false;
        }
    }

    EditDiffResult {
        diff: output.join("\n"),
        first_changed_line,
    }
}

// ---------------------------------------------------------------------------
// jsdiff 8.0.4 (npm `diff`) — literal port of the pieces Pi reaches.
// ---------------------------------------------------------------------------

/// Literal port of the parts of npm `diff` v8.0.4 that Pi's `edit-diff.ts` uses:
/// `diffLines` (`LineDiff` over `Diff`) and `createTwoFilesPatch`
/// (`structuredPatch` + `formatPatch`).
///
/// Only the option set Pi actually passes is modelled: `{}` for `diffLines`, and
/// `{context, headerOptions: FILE_HEADERS_ONLY}` for the patch. So
/// `oneChangePerToken`, `comparator`, `ignoreCase`, `ignoreWhitespace`,
/// `ignoreNewlineAtEof`, `newlineIsToken`, `stripTrailingCr`, `maxEditLength`,
/// `timeout` and `callback` are all absent/false, `useLongestToken` is `false` and
/// `postProcess` is the identity — each is noted where it collapses a branch.
mod jsdiff {
    /// One `ChangeObject` (`base.js#buildValues`, `diff/base.js:212-252`).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(super) struct Change {
        /// Text of this run: new tokens for context/added, old tokens for removed.
        pub value: String,
        /// Present only in the new document.
        pub added: bool,
        /// Present only in the old document.
        pub removed: bool,
    }

    /// `Component` of the reverse linked list built along a D-path
    /// (`diff/base.js:139-170`). Stored in an arena; `previous` is an arena index.
    #[derive(Debug, Clone, Copy)]
    struct Component {
        count: usize,
        added: bool,
        removed: bool,
        previous: Option<u32>,
    }

    /// One `bestPath` entry (`diff/base.js:40`).
    #[derive(Debug, Clone, Copy)]
    struct PathEntry {
        old_pos: i64,
        last_component: Option<u32>,
    }

    /// `line.js` `tokenize` (`diff/line.js:44-65`) with `options = {}`:
    /// `value.split(/(\n|\r\n)/)` then merge each separator into the preceding
    /// content token. `\r\n` wins over `\n` at a `\r` because the alternation is
    /// tried at each index in turn.
    ///
    /// Every emitted token is non-empty, so `Diff#removeEmpty`
    /// (`diff/base.js:180-188`) is a no-op here and is not ported.
    pub(super) fn tokenize(value: &str) -> Vec<&str> {
        let bytes = value.as_bytes();
        let mut out: Vec<&str> = Vec::new();
        let mut start = 0usize;
        let mut i = 0usize;
        while i < bytes.len() {
            if bytes[i] == b'\n' {
                out.push(&value[start..i + 1]);
                i += 1;
                start = i;
            } else if bytes[i] == b'\r' && i + 1 < bytes.len() && bytes[i + 1] == b'\n' {
                out.push(&value[start..i + 2]);
                i += 2;
                start = i;
            } else {
                i += 1;
            }
        }
        if start < value.len() {
            out.push(&value[start..]);
        }
        out
    }

    /// `Diff#addToPath` (`diff/base.js:139-153`). Consecutive same-flag edits
    /// merge into one component (`oneChangePerToken` is never set).
    fn add_to_path(
        path: PathEntry,
        added: bool,
        removed: bool,
        old_pos_inc: i64,
        arena: &mut Vec<Component>,
    ) -> PathEntry {
        if let Some(last_idx) = path.last_component {
            let last = arena[last_idx as usize];
            if last.added == added && last.removed == removed {
                arena.push(Component {
                    count: last.count + 1,
                    added,
                    removed,
                    previous: last.previous,
                });
                return PathEntry {
                    old_pos: path.old_pos + old_pos_inc,
                    last_component: Some((arena.len() - 1) as u32),
                };
            }
        }
        arena.push(Component {
            count: 1,
            added,
            removed,
            previous: path.last_component,
        });
        PathEntry {
            old_pos: path.old_pos + old_pos_inc,
            last_component: Some((arena.len() - 1) as u32),
        }
    }

    /// `Diff#extractCommon` (`diff/base.js:154-170`). `equals` collapses to token
    /// equality: `LineDiff#equals` (`diff/line.js:8-33`) only rewrites its inputs
    /// under `ignoreWhitespace`/`ignoreNewlineAtEof`, and `Diff#equals`
    /// (`:171-179`) to `left === right` without a comparator or `ignoreCase`.
    ///
    /// Mutates `base_path` in place, exactly as the JS does.
    fn extract_common(
        base_path: &mut PathEntry,
        new_tokens: &[&str],
        old_tokens: &[&str],
        diagonal_path: i64,
        arena: &mut Vec<Component>,
    ) -> i64 {
        let new_len = new_tokens.len() as i64;
        let old_len = old_tokens.len() as i64;
        let mut old_pos = base_path.old_pos;
        let mut new_pos = old_pos - diagonal_path;
        let mut common_count = 0usize;
        // The `>= 0` guards stand in for JS reading past index 0 as `undefined`
        // (which `equals` reports unequal); the D-path invariants keep both `>= -1`.
        while new_pos + 1 >= 0
            && new_pos + 1 < new_len
            && old_pos + 1 >= 0
            && old_pos + 1 < old_len
            && old_tokens[(old_pos + 1) as usize] == new_tokens[(new_pos + 1) as usize]
        {
            new_pos += 1;
            old_pos += 1;
            common_count += 1;
        }
        if common_count > 0 {
            arena.push(Component {
                count: common_count,
                added: false,
                removed: false,
                previous: base_path.last_component,
            });
            base_path.last_component = Some((arena.len() - 1) as u32);
        }
        base_path.old_pos = old_pos;
        new_pos
    }

    /// `Diff#buildValues` (`diff/base.js:212-252`). `useLongestToken` is `false`
    /// for `LineDiff`, so the `oldValue.length > value.length` branch is dead.
    fn build_values(
        last_component: Option<u32>,
        new_tokens: &[&str],
        old_tokens: &[&str],
        arena: &[Component],
    ) -> Vec<Change> {
        let mut order: Vec<u32> = Vec::new();
        let mut cursor = last_component;
        while let Some(idx) = cursor {
            order.push(idx);
            cursor = arena[idx as usize].previous;
        }
        order.reverse();

        let mut out: Vec<Change> = Vec::with_capacity(order.len());
        let mut new_pos = 0usize;
        let mut old_pos = 0usize;
        for idx in order {
            let component = arena[idx as usize];
            let value = if component.removed {
                let value = old_tokens[old_pos..old_pos + component.count].concat();
                old_pos += component.count;
                value
            } else {
                let value = new_tokens[new_pos..new_pos + component.count].concat();
                new_pos += component.count;
                if !component.added {
                    old_pos += component.count;
                }
                value
            };
            out.push(Change {
                value,
                added: component.added,
                removed: component.removed,
            });
        }
        out
    }

    /// `diffLines(oldStr, newStr)` (`diff/line.js:36-38`) → `Diff#diff` +
    /// `#diffWithOptionsObj` (`diff/base.js:2-138`), i.e. Myers with jsdiff's
    /// edge-pruning optimisation. `castInput` is the identity and `postProcess` is
    /// the identity for `LineDiff`, so both are elided.
    pub(super) fn diff_lines(old_str: &str, new_str: &str) -> Vec<Change> {
        let old_tokens = tokenize(old_str);
        let new_tokens = tokenize(new_str);
        let new_len = new_tokens.len() as i64;
        let old_len = old_tokens.len() as i64;

        let mut arena: Vec<Component> = Vec::new();

        // `maxEditLength = newLen + oldLen` (no `options.maxEditLength`), and
        // `maxExecutionTime = Infinity` so the timeout check always passes.
        let max_edit_length = new_len + old_len;

        // `bestPath` is a JS array indexed by the (possibly negative) diagonal.
        let offset = max_edit_length + 1;
        let mut best: Vec<Option<PathEntry>> = vec![None; (2 * offset + 3) as usize];
        let at = |d: i64| -> usize { (d + offset) as usize };

        // Seed editLength = 0, i.e. the content starts with the same values.
        let mut seed = PathEntry {
            old_pos: -1,
            last_component: None,
        };
        let mut new_pos = extract_common(&mut seed, &new_tokens, &old_tokens, 0, &mut arena);
        best[at(0)] = Some(seed);
        if seed.old_pos + 1 >= old_len && new_pos + 1 >= new_len {
            // Identity per the equality and tokenizer.
            return build_values(seed.last_component, &new_tokens, &old_tokens, &arena);
        }

        // `-Infinity` / `+Infinity` sentinels (`diff/base.js:64`).
        let mut min_diagonal_to_consider: Option<i64> = None;
        let mut max_diagonal_to_consider: Option<i64> = None;

        let mut edit_length: i64 = 1;
        while edit_length <= max_edit_length {
            let lo = match min_diagonal_to_consider {
                Some(m) => m.max(-edit_length),
                None => -edit_length,
            };
            let hi = match max_diagonal_to_consider {
                Some(m) => m.min(edit_length),
                None => edit_length,
            };

            let mut diagonal_path = lo;
            while diagonal_path <= hi {
                let remove_path = best[at(diagonal_path - 1)];
                let add_path = best[at(diagonal_path + 1)];
                if remove_path.is_some() {
                    // No one else is going to attempt to use this value, clear it.
                    best[at(diagonal_path - 1)] = None;
                }

                let mut can_add = false;
                if let Some(ap) = add_path {
                    // What newPos will be after we do an insertion:
                    let add_path_new_pos = ap.old_pos - diagonal_path;
                    can_add = (0..new_len).contains(&add_path_new_pos);
                }
                let can_remove = remove_path.is_some_and(|rp| rp.old_pos + 1 < old_len);

                if !can_add && !can_remove {
                    // If this path is a terminal then prune.
                    best[at(diagonal_path)] = None;
                    diagonal_path += 2;
                    continue;
                }

                // Select the diagonal to branch from: the prior path whose position
                // in the old string is farthest from the origin without passing the
                // bounds of the diff graph.
                let mut base_path = if !can_remove
                    || (can_add
                        && remove_path.expect("canRemove implies removePath").old_pos
                            < add_path.expect("canAdd implies addPath").old_pos)
                {
                    add_to_path(
                        add_path.expect("!canRemove implies canAdd"),
                        true,
                        false,
                        0,
                        &mut arena,
                    )
                } else {
                    add_to_path(
                        remove_path.expect("canRemove implies removePath"),
                        false,
                        true,
                        1,
                        &mut arena,
                    )
                };

                new_pos = extract_common(
                    &mut base_path,
                    &new_tokens,
                    &old_tokens,
                    diagonal_path,
                    &mut arena,
                );

                if base_path.old_pos + 1 >= old_len && new_pos + 1 >= new_len {
                    // If we have hit the end of both strings, then we are done.
                    return build_values(
                        base_path.last_component,
                        &new_tokens,
                        &old_tokens,
                        &arena,
                    );
                }
                best[at(diagonal_path)] = Some(base_path);
                if base_path.old_pos + 1 >= old_len {
                    max_diagonal_to_consider = Some(match max_diagonal_to_consider {
                        Some(m) => m.min(diagonal_path - 1),
                        None => diagonal_path - 1,
                    });
                }
                if new_pos + 1 >= new_len {
                    min_diagonal_to_consider = Some(match min_diagonal_to_consider {
                        Some(m) => m.max(diagonal_path + 1),
                        None => diagonal_path + 1,
                    });
                }

                diagonal_path += 2;
            }
            edit_length += 1;
        }

        // Unreachable without `options.maxEditLength`/`timeout`: Myers always
        // finishes within `oldLen + newLen` edits. JS returns `undefined` here.
        Vec::new()
    }

    /// One hunk of a `StructuredPatch` (`patch/create.js:105-111`).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(super) struct Hunk {
        old_start: i64,
        old_lines: i64,
        new_start: i64,
        new_lines: i64,
        lines: Vec<String>,
    }

    /// `StructuredPatch` (`patch/create.js:135-139`). `oldHeader`/`newHeader` are
    /// always `undefined` from Pi, so they are not modelled.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(super) struct StructuredPatch {
        old_file_name: String,
        new_file_name: String,
        hunks: Vec<Hunk>,
    }

    /// One entry of the working `diff` array inside `diffLinesResultToPatch`
    /// (`patch/create.js:50-140`), after `current.lines = lines`.
    struct PatchPart {
        added: bool,
        removed: bool,
        lines: Vec<String>,
    }

    /// `splitLines` (`patch/create.js:215-228`): lines keep their trailing `\n`;
    /// a final line without one keeps none.
    fn split_lines(text: &str) -> Vec<String> {
        let has_trailing_nl = text.ends_with('\n');
        let mut result: Vec<String> = text.split('\n').map(|line| format!("{line}\n")).collect();
        if has_trailing_nl {
            result.pop();
        } else {
            let last = result
                .pop()
                .expect("str::split always yields at least one part");
            result.push(last[..last.len() - 1].to_string());
        }
        result
    }

    /// `structuredPatch` + `diffLinesResultToPatch` (`patch/create.js:17-140`).
    pub(super) fn structured_patch(
        old_file_name: &str,
        new_file_name: &str,
        old_str: &str,
        new_str: &str,
        context: usize,
    ) -> StructuredPatch {
        let mut parts: Vec<PatchPart> = diff_lines(old_str, new_str)
            .into_iter()
            .map(|change| PatchPart {
                added: change.added,
                removed: change.removed,
                lines: split_lines(&change.value),
            })
            .collect();
        // `diff.push({ value: '', lines: [] })` — an empty value to make cleanup
        // easier. Its pre-set `lines: []` bypasses `splitLines('')`.
        parts.push(PatchPart {
            added: false,
            removed: false,
            lines: Vec::new(),
        });

        // STEP 1: build hunks with trailing newlines still attached.
        let mut hunks: Vec<Hunk> = Vec::new();
        let mut old_range_start: i64 = 0;
        let mut new_range_start: i64 = 0;
        let mut cur_range: Vec<String> = Vec::new();
        let mut old_line: i64 = 1;
        let mut new_line: i64 = 1;
        let part_count = parts.len() as i64;

        for (i, part) in parts.iter().enumerate() {
            let added = part.added;
            let removed = part.removed;
            let lines_len = part.lines.len() as i64;

            if added || removed {
                // If we have previous context, start with that.
                if old_range_start == 0 {
                    old_range_start = old_line;
                    new_range_start = new_line;
                    if i > 0 {
                        let prev = &parts[i - 1].lines;
                        cur_range = if context > 0 {
                            let start = prev.len().saturating_sub(context);
                            prev[start..].iter().map(|e| format!(" {e}")).collect()
                        } else {
                            Vec::new()
                        };
                        old_range_start -= cur_range.len() as i64;
                        new_range_start -= cur_range.len() as i64;
                    }
                }

                // Output our changes.
                let marker = if added { '+' } else { '-' };
                for line in &part.lines {
                    cur_range.push(format!("{marker}{line}"));
                }

                // Track the updated file position.
                if added {
                    new_line += lines_len;
                } else {
                    old_line += lines_len;
                }
            } else {
                // Identical context lines. Track line changes.
                if old_range_start != 0 {
                    if lines_len <= (context as i64) * 2 && (i as i64) < part_count - 2 {
                        // Overlapping.
                        for line in &part.lines {
                            cur_range.push(format!(" {line}"));
                        }
                    } else {
                        // End the range and output.
                        let context_size = lines_len.min(context as i64);
                        for line in &part.lines[..context_size as usize] {
                            cur_range.push(format!(" {line}"));
                        }
                        hunks.push(Hunk {
                            old_start: old_range_start,
                            old_lines: old_line - old_range_start + context_size,
                            new_start: new_range_start,
                            new_lines: new_line - new_range_start + context_size,
                            lines: std::mem::take(&mut cur_range),
                        });
                        old_range_start = 0;
                        new_range_start = 0;
                    }
                }
                old_line += lines_len;
                new_line += lines_len;
            }
        }

        // STEP 2: drop each line's trailing `\n`, inserting the no-newline marker
        // where there was none (`patch/create.js:124-134`).
        for hunk in &mut hunks {
            let mut i = 0usize;
            while i < hunk.lines.len() {
                if hunk.lines[i].ends_with('\n') {
                    hunk.lines[i].pop();
                } else {
                    hunk.lines
                        .insert(i + 1, "\\ No newline at end of file".to_string());
                    i += 1; // Skip the line we just added, then continue iterating.
                }
                i += 1;
            }
        }

        StructuredPatch {
            old_file_name: old_file_name.to_string(),
            new_file_name: new_file_name.to_string(),
            hunks,
        }
    }

    /// `formatPatch` (`patch/create.js:146-188`) with `FILE_HEADERS_ONLY`
    /// (`:7-11`): no `Index:` line, no `===` underline, and no `\t<header>` suffix
    /// because Pi passes `undefined` for both dates.
    ///
    /// Mutates `patch` for the zero-length-hunk `start -= 1` quirk, as the JS does.
    pub(super) fn format_patch(patch: &mut StructuredPatch) -> String {
        let mut ret: Vec<String> = Vec::new();
        ret.push(format!("--- {}", patch.old_file_name));
        ret.push(format!("+++ {}", patch.new_file_name));

        for hunk in &mut patch.hunks {
            // Unified Diff Format quirk: if the chunk size is 0, the first number
            // is one lower than one would expect.
            if hunk.old_lines == 0 {
                hunk.old_start -= 1;
            }
            if hunk.new_lines == 0 {
                hunk.new_start -= 1;
            }
            ret.push(format!(
                "@@ -{},{} +{},{} @@",
                hunk.old_start, hunk.old_lines, hunk.new_start, hunk.new_lines
            ));
            for line in &hunk.lines {
                ret.push(line.clone());
            }
        }

        let mut out = ret.join("\n");
        out.push('\n');
        out
    }
}
