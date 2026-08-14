//! Byte oracle for `core/tools/edit-diff.ts` (`crates/pirust-tools/src/edit_diff.rs`).
//!
//! Every expectation in here is a literal byte string captured by executing Pi's
//! real functions (`tests/fixtures/pi/tools/edit.diff.corpus.jsonl`, 56 cases: 41
//! success + 15 error). Nothing is recomputed locally: a failure means the Rust
//! port diverged from Pi, and the fix is the port, never the assertion.
//!
//! Per success case this pins:
//! - `lowLevel.baseContent` / `lowLevel.newContent` — `applyEditsToNormalizedContent`
//! - `details.diff` == `lowLevel.diff` — `generateDiffString().diff`
//! - `details.firstChangedLine` == `lowLevel.firstChangedLine`
//! - `details.patch` == `lowLevel.patch` — `generateUnifiedPatch()` (jsdiff 8.0.4)
//! - `lowLevel.detectedLineEnding` / `lowLevel.bom` — `detectLineEnding` / `stripBom`
//! - `writtenContent` — `bom + restoreLineEndings(newContent, ending)` (`edit.ts:346`)
//!
//! Per error case it pins the thrown `Error.message` verbatim.

use std::path::PathBuf;

use pirust_tools::edit_diff::{
    apply_edits_to_normalized_content, detect_line_ending, generate_diff_string,
    generate_unified_patch, normalize_to_lf, restore_line_endings, strip_bom, Edit,
};
use serde::Deserialize;

/// The two corpus cases that are *not* `edit-diff.ts` surface: their errors come
/// from `edit.ts`'s own `validateEditInput` / `ops.access` and they carry no
/// `lowLevel` block. Pinned by name so a corpus change cannot silently grow this
/// exemption.
const NON_EDIT_DIFF_CASES: [&str; 2] = ["err-empty-edits-array", "err-file-missing"];

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Case {
    name: String,
    path: String,
    original: Option<String>,
    edits: Vec<Edit>,
    ok: bool,
    details: Option<Details>,
    error: Option<String>,
    written_content: Option<String>,
    low_level: Option<LowLevel>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Details {
    diff: String,
    patch: String,
    first_changed_line: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LowLevel {
    detected_line_ending: Option<String>,
    bom: Option<bool>,
    base_content: Option<String>,
    new_content: Option<String>,
    diff: Option<String>,
    first_changed_line: Option<usize>,
    patch: Option<String>,
    error: Option<String>,
}

/// Workspace-root fixtures dir (`CARGO_MANIFEST_DIR` = `crates/pirust-tools`).
fn corpus_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/pi/tools/edit.diff.corpus.jsonl")
}

/// Render a control character visibly so a one-byte divergence is readable.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{feff}' => out.push_str("\\uFEFF"),
            other => out.push(other),
        }
    }
    out
}

/// First-divergence report: the differing line (1-based, split on `\n`) with both
/// sides escaped, plus the absolute byte offset.
fn first_diff(want: &str, got: &str) -> String {
    let wb = want.as_bytes();
    let gb = got.as_bytes();
    let common = wb.len().min(gb.len());
    let mut byte = 0usize;
    while byte < common && wb[byte] == gb[byte] {
        byte += 1;
    }
    if byte == wb.len() && byte == gb.len() {
        return "strings are equal (len differs? no)".to_string();
    }
    let line_no = want[..byte].matches('\n').count() + 1;
    let want_lines: Vec<&str> = want.split('\n').collect();
    let got_lines: Vec<&str> = got.split('\n').collect();
    let show = |lines: &[&str]| -> String {
        lines
            .get(line_no - 1)
            .map(|l| escape(l))
            .unwrap_or_else(|| "<missing line>".to_string())
    };
    format!(
        "first diff at byte {byte}, line {line_no} \
         (want {} lines / {} bytes, got {} lines / {} bytes)\n  \
         want: {}\n  got:  {}",
        want_lines.len(),
        want.len(),
        got_lines.len(),
        got.len(),
        show(&want_lines),
        show(&got_lines),
    )
}

/// Assert byte identity with a first-divergence report on failure.
fn assert_bytes(case: &str, what: &str, want: &str, got: &str) {
    assert!(
        want == got,
        "case `{case}`: {what} is not byte-identical to Pi\n{}",
        first_diff(want, got)
    );
}

fn load_cases() -> Vec<Case> {
    let path = corpus_path();
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read corpus {}: {e}", path.display()));
    raw.lines()
        .filter(|line| !line.trim().is_empty())
        .enumerate()
        .map(|(i, line)| {
            serde_json::from_str::<Case>(line)
                .unwrap_or_else(|e| panic!("corpus line {}: deserialize failed: {e}", i + 1))
        })
        .collect()
}

#[test]
fn corpus_shape_is_intact() {
    let cases = load_cases();
    assert_eq!(cases.len(), 56, "corpus must carry all 56 captured cases");

    let ok = cases.iter().filter(|c| c.ok).count();
    let err = cases.iter().filter(|c| !c.ok).count();
    assert_eq!(ok, 41, "corpus must carry all 41 success cases");
    assert_eq!(err, 15, "corpus must carry all 15 error cases");

    // Every success case must carry the full byte expectations, and every error
    // case an `error` string — otherwise the assertions below go vacuous.
    for case in &cases {
        if case.ok {
            assert!(
                case.details.is_some() && case.written_content.is_some(),
                "case `{}`: success case missing details/writtenContent",
                case.name
            );
            let low = case
                .low_level
                .as_ref()
                .unwrap_or_else(|| panic!("case `{}`: missing lowLevel", case.name));
            assert!(
                low.base_content.is_some()
                    && low.new_content.is_some()
                    && low.diff.is_some()
                    && low.patch.is_some(),
                "case `{}`: lowLevel missing captured values",
                case.name
            );
        } else {
            assert!(
                case.error.is_some(),
                "case `{}`: error case missing error",
                case.name
            );
        }
    }

    // The only cases without a `lowLevel` block are the two upstream `edit.ts`
    // validation failures that this module cannot produce.
    let skipped: Vec<&str> = cases
        .iter()
        .filter(|c| c.low_level.is_none())
        .map(|c| c.name.as_str())
        .collect();
    assert_eq!(
        skipped, NON_EDIT_DIFF_CASES,
        "unexpected set of non-edit-diff cases"
    );
}

#[test]
fn corpus_matches_pi_byte_for_byte() {
    let cases = load_cases();
    let mut driven = 0usize;
    let mut driven_ok = 0usize;
    let mut driven_err = 0usize;

    for case in &cases {
        let Some(low) = case.low_level.as_ref() else {
            assert!(
                NON_EDIT_DIFF_CASES.contains(&case.name.as_str()),
                "case `{}`: no lowLevel but not a known upstream case",
                case.name
            );
            continue;
        };
        driven += 1;

        let raw_content = case
            .original
            .as_ref()
            .unwrap_or_else(|| panic!("case `{}`: missing original", case.name));

        // `edit.ts:340-342`: strip BOM, detect the ending on the stripped text,
        // then normalize to LF before any matching happens.
        let (bom, content) = strip_bom(raw_content);
        let ending = detect_line_ending(content);
        let normalized = normalize_to_lf(content);

        let result = apply_edits_to_normalized_content(&normalized, &case.edits, &case.path);

        if !case.ok {
            driven_err += 1;
            let want = low
                .error
                .as_ref()
                .unwrap_or_else(|| panic!("case `{}`: lowLevel.error missing", case.name));
            assert_eq!(
                case.error.as_deref(),
                Some(want.as_str()),
                "case `{}`: fixture disagrees with itself on the error",
                case.name
            );
            match result {
                Ok(applied) => panic!(
                    "case `{}`: expected error `{want}`, got Ok(newContent = {:?})",
                    case.name, applied.new_content
                ),
                Err(err) => assert_bytes(&case.name, "error message", want, &err.to_string()),
            }
            continue;
        }

        driven_ok += 1;
        let applied = match result {
            Ok(applied) => applied,
            Err(err) => panic!("case `{}`: expected success, got error `{err}`", case.name),
        };

        // `detectLineEnding` / `stripBom`.
        let ending_name = if ending == "\r\n" { "CRLF" } else { "LF" };
        if let Some(want) = low.detected_line_ending.as_deref() {
            assert_eq!(
                want, ending_name,
                "case `{}`: detectLineEnding mismatch",
                case.name
            );
        }
        if let Some(want) = low.bom {
            assert_eq!(
                want,
                !bom.is_empty(),
                "case `{}`: stripBom mismatch",
                case.name
            );
        }

        // `applyEditsToNormalizedContent`.
        assert_bytes(
            &case.name,
            "lowLevel.baseContent",
            low.base_content.as_deref().expect("checked above"),
            &applied.base_content,
        );
        assert_bytes(
            &case.name,
            "lowLevel.newContent",
            low.new_content.as_deref().expect("checked above"),
            &applied.new_content,
        );

        // `bom + restoreLineEndings(newContent, originalEnding)` (`edit.ts:346`).
        if let Some(want) = case.written_content.as_deref() {
            let written = format!(
                "{bom}{}",
                restore_line_endings(&applied.new_content, ending)
            );
            assert_bytes(&case.name, "writtenContent", want, &written);
        }

        let details = case.details.as_ref().expect("checked above");

        // `generateDiffString(baseContent, newContent)`.
        let diff = generate_diff_string(&applied.base_content, &applied.new_content);
        assert_bytes(&case.name, "details.diff", &details.diff, &diff.diff);
        assert_bytes(
            &case.name,
            "lowLevel.diff",
            low.diff.as_deref().expect("checked above"),
            &diff.diff,
        );
        assert_eq!(
            details.first_changed_line, diff.first_changed_line,
            "case `{}`: details.firstChangedLine mismatch",
            case.name
        );
        assert_eq!(
            low.first_changed_line, diff.first_changed_line,
            "case `{}`: lowLevel.firstChangedLine mismatch",
            case.name
        );

        // `generateUnifiedPatch(path, baseContent, newContent)`.
        let patch = generate_unified_patch(&case.path, &applied.base_content, &applied.new_content);
        assert_bytes(&case.name, "details.patch", &details.patch, &patch);
        assert_bytes(
            &case.name,
            "lowLevel.patch",
            low.patch.as_deref().expect("checked above"),
            &patch,
        );
    }

    assert_eq!(
        driven, 54,
        "must drive every case that has a lowLevel block"
    );
    assert_eq!(driven_ok, 41, "must drive all 41 success cases");
    assert_eq!(driven_err, 13, "must drive all 13 edit-diff error cases");
}
