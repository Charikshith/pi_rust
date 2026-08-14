//! Pi-as-oracle test for `pirust_tools::edit`.
//!
//! Four fixtures, all captured by executing real Pi (`scripts/gen-tools-oracle.mjs`):
//!
//! * `tests/fixtures/pi/tools/schemas/edit.json` — the exact `JSON.stringify`
//!   bytes of `editSchema` (`edit.ts:33-53`), including the two orderings only
//!   TypeBox produces: the nested `items` object's `required` before its
//!   `properties`, and the array's `description` *after* `items`.
//! * `tests/fixtures/pi/tools/strings/edit.json` — `name` / `label` /
//!   `description` / `promptSnippet` / the four `promptGuidelines` bullets /
//!   `executionMode` / `hasPrepareArguments` (`edit.ts:293-307`).
//! * `tests/fixtures/pi/tools/edit.prepare.cases.jsonl` — 10 captured
//!   `prepareEditArguments` calls (`edit.ts:94-118`), input and output verbatim.
//! * `tests/fixtures/pi/tools/edit.diff.corpus.jsonl` — all 56 captured cases,
//!   replayed through the **whole tool** rather than the diff engine
//!   (`crates/pirust-tools/tests/edit_diff_golden.rs` drives the 54 with a
//!   `lowLevel` block at engine level; the two without one —
//!   `err-empty-edits-array` and `err-file-missing` — originate in `edit.ts` and
//!   only this suite can produce them).
//!
//! Nothing here is computed by hand. Every success row is compared on four axes:
//! the `content` array's serialized bytes, `details`' serialized bytes (key order
//! included, since `details` is persisted into the session JSONL), each `details`
//! field individually with a first-divergence report, and — the real proof — the
//! bytes that landed on disk (`writtenContent`, which carries the BOM and the
//! CRLFs `details.diff` does not). Every error row is compared on its exact
//! message.
//!
//! The replay runs against real files in a `tempfile` tree with the default
//! [`LocalEditOperations`]: that is what makes `err-file-missing` genuine — its
//! `Error code: ENOENT.` comes from a real failed `access`, not from a
//! test-fabricated error.

use std::fs;
use std::path::PathBuf;

use pirust_agent_core::types::{AgentTool, AgentToolUpdateCallback};
use pirust_tools::edit::create_edit_tool_definition;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// A shrinking fixture has to fail loudly instead of silently weakening the suite.
const EXPECTED_CORPUS_ROWS: usize = 56;
const EXPECTED_OK_ROWS: usize = 41;
const EXPECTED_ERROR_ROWS: usize = 15;
const EXPECTED_PREPARE_CASES: usize = 10;

/// The two rows whose errors come from `edit.ts` itself (`validateEditInput` and
/// the `ops.access` catch) rather than from the diff engine. They are the reason
/// this suite exists on top of `edit_diff_golden.rs`.
const EDIT_TS_LEVEL_CASES: [&str; 2] = ["err-empty-edits-array", "err-file-missing"];

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/pi/tools")
}

fn read_fixture(relative: &str) -> String {
    let path = fixtures_dir().join(relative);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read fixture {}: {e}", path.display()))
}

// ---------------------------------------------------------------------------
// Fixture shapes
// ---------------------------------------------------------------------------

/// `strings/edit.json`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FixtureStrings {
    name: String,
    label: String,
    description: String,
    prompt_snippet: String,
    prompt_guidelines: Vec<String>,
    execution_mode: Option<Value>,
    has_prepare_arguments: bool,
}

/// One line of `edit.prepare.cases.jsonl`.
#[derive(Debug, Deserialize)]
struct PrepareCase {
    #[serde(rename = "fn")]
    function: String,
    tool: String,
    note: String,
    input: Value,
    ok: bool,
    output: Value,
}

/// One line of `edit.diff.corpus.jsonl`, as the *tool* sees it: `lowLevel` is the
/// engine-level capture and belongs to `edit_diff_golden.rs`, so it is only read
/// here to classify the two `edit.ts`-level rows.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Case {
    name: String,
    note: String,
    /// The **raw** path argument — what every message and the patch header use.
    path: String,
    /// File contents before the edit; `null` means the file does not exist.
    original: Option<String>,
    /// Passed through to the tool verbatim, so a malformed `edits` stays malformed.
    edits: Value,
    ok: bool,
    /// Pi's `AgentToolResult.content`, verbatim JSON.
    content: Option<Value>,
    /// Pi's `AgentToolResult.details`, verbatim JSON (key order included).
    details: Option<Value>,
    /// The thrown `Error.message`.
    error: Option<String>,
    /// Exactly what the file held afterwards.
    written_content: Option<String>,
    low_level: Option<Value>,
}

fn load_prepare_cases() -> Vec<PrepareCase> {
    let raw = read_fixture("edit.prepare.cases.jsonl");
    raw.lines()
        .filter(|line| !line.trim().is_empty())
        .enumerate()
        .map(|(i, line)| {
            serde_json::from_str::<PrepareCase>(line)
                .unwrap_or_else(|e| panic!("prepare case line {}: deserialize failed: {e}", i + 1))
        })
        .collect()
}

fn load_corpus() -> Vec<Case> {
    let raw = read_fixture("edit.diff.corpus.jsonl");
    raw.lines()
        .filter(|line| !line.trim().is_empty())
        .enumerate()
        .map(|(i, line)| {
            serde_json::from_str::<Case>(line)
                .unwrap_or_else(|e| panic!("corpus line {}: deserialize failed: {e}", i + 1))
        })
        .collect()
}

fn noop_update() -> AgentToolUpdateCallback {
    Arc::new(|_| {})
}

// ---------------------------------------------------------------------------
// Failure reporting (same shape as `edit_diff_golden.rs`)
// ---------------------------------------------------------------------------

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
    let line_no = want[..byte.min(want.len())].matches('\n').count() + 1;
    let want_lines: Vec<&str> = want.split('\n').collect();
    let got_lines: Vec<&str> = got.split('\n').collect();
    let show = |lines: &[&str]| -> String {
        lines
            .get(line_no - 1)
            .map(|line| escape(line))
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

// ---------------------------------------------------------------------------
// 1. Static tool data
// ---------------------------------------------------------------------------

/// `parameters` must serialize byte-identically to Pi's captured
/// `JSON.stringify(editSchema)` — every nested key order included.
#[test]
fn parameters_match_pi_bytes() {
    let want = read_fixture("schemas/edit.json")
        .trim_end_matches(['\n', '\r'])
        .to_string();

    let definition = create_edit_tool_definition("/oracle/cwd", None);
    let got = serde_json::to_string(&AgentTool::parameters(&definition)).expect("serialize");

    assert_eq!(
        got,
        want,
        "edit parameters bytes diverged\n{}",
        first_diff(&want, &got)
    );
}

/// Every string Pi's `createEditToolDefinition` sets (`edit.ts:293-304`), plus the
/// two facts the capture records about the fields around them: no `executionMode`
/// (`edit.ts` sets none) and `hasPrepareArguments: true` — `edit` is the only
/// built-in with a shim.
#[test]
fn strings_match_pi() {
    let raw = read_fixture("strings/edit.json");
    let want: FixtureStrings =
        serde_json::from_str(&raw).unwrap_or_else(|e| panic!("parse strings/edit.json: {e}"));

    let definition = create_edit_tool_definition("/oracle/cwd", None);

    assert_eq!(definition.name, want.name, "name diverged");
    assert_eq!(definition.label, want.label, "label diverged");
    assert_bytes(
        "strings",
        "description",
        &want.description,
        &definition.description,
    );
    assert_bytes(
        "strings",
        "promptSnippet",
        &want.prompt_snippet,
        definition.prompt_snippet.as_deref().unwrap_or_default(),
    );

    let got_guidelines = definition
        .prompt_guidelines
        .as_deref()
        .expect("edit must set promptGuidelines");
    assert_eq!(
        want.prompt_guidelines.len(),
        4,
        "fixture changed: edit should carry four promptGuidelines bullets"
    );
    assert_eq!(
        got_guidelines.len(),
        want.prompt_guidelines.len(),
        "promptGuidelines count diverged"
    );
    for (index, (want_line, got_line)) in want
        .prompt_guidelines
        .iter()
        .zip(got_guidelines.iter())
        .enumerate()
    {
        assert_bytes(
            "strings",
            &format!("promptGuidelines[{index}]"),
            want_line,
            got_line,
        );
    }

    assert!(
        want.execution_mode.is_none(),
        "fixture changed: edit now records an executionMode ({:?})",
        want.execution_mode
    );
    assert!(
        definition.execution_mode.is_none(),
        "edit must not set executionMode"
    );
    assert!(
        want.has_prepare_arguments,
        "fixture changed: edit should have a prepareArguments shim"
    );
    assert!(
        definition.prepare_arguments.is_some(),
        "edit must set prepareArguments"
    );
}

// ---------------------------------------------------------------------------
// 2. prepareArguments (edit.ts:94-118)
// ---------------------------------------------------------------------------

/// All 10 captured `prepareEditArguments` calls, driven through the `AgentTool`
/// bridge (which is what the loop calls).
///
/// Compared twice: as `Value` (content) and as serialized bytes (key order — the
/// legacy shim rebuilds the object, and `{ ...rest, edits }` puts `edits` where
/// the rest-spread left it).
#[test]
fn prepare_arguments_cases_match_pi() {
    let cases = load_prepare_cases();
    assert_eq!(
        cases.len(),
        EXPECTED_PREPARE_CASES,
        "expected {EXPECTED_PREPARE_CASES} captured prepareArguments cases"
    );

    let definition = create_edit_tool_definition("/oracle/cwd", None);

    for (index, case) in cases.iter().enumerate() {
        let label = format!("prepare case {index} ({})", case.note);
        assert_eq!(case.function, "prepareArguments", "{label}: wrong fn");
        assert_eq!(case.tool, "edit", "{label}: wrong tool");
        assert!(
            case.ok,
            "{label}: fixture records a failure, not handled here"
        );

        let got = AgentTool::prepare_arguments(&definition, case.input.clone());
        assert_eq!(
            got, case.output,
            "{label}: output diverged\n  input:    {}\n  expected: {}\n  actual:   {}",
            case.input, case.output, got
        );
        // Value equality ignores key order; the bytes do not.
        let want_bytes = serde_json::to_string(&case.output).expect("serialize");
        let got_bytes = serde_json::to_string(&got).expect("serialize");
        assert_bytes(&label, "output bytes", &want_bytes, &got_bytes);
    }
}

// ---------------------------------------------------------------------------
// 3. The corpus, through the whole tool
// ---------------------------------------------------------------------------

/// The corpus must keep covering what this port depends on. Asserted separately
/// so a fixture edit that drops a case names the loss instead of quietly reducing
/// coverage.
#[test]
fn corpus_shape_is_intact() {
    let cases = load_corpus();
    assert_eq!(
        cases.len(),
        EXPECTED_CORPUS_ROWS,
        "corpus must carry all {EXPECTED_CORPUS_ROWS} captured cases"
    );
    assert_eq!(
        cases.iter().filter(|case| case.ok).count(),
        EXPECTED_OK_ROWS,
        "corpus must carry all {EXPECTED_OK_ROWS} success cases"
    );
    assert_eq!(
        cases.iter().filter(|case| !case.ok).count(),
        EXPECTED_ERROR_ROWS,
        "corpus must carry all {EXPECTED_ERROR_ROWS} error cases"
    );

    for case in &cases {
        if case.ok {
            assert!(
                case.content.is_some() && case.details.is_some() && case.written_content.is_some(),
                "case `{}`: success case missing content/details/writtenContent",
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

    // The two rows only this suite can reproduce, pinned by name.
    let edit_ts_level: Vec<&str> = cases
        .iter()
        .filter(|case| case.low_level.is_none())
        .map(|case| case.name.as_str())
        .collect();
    assert_eq!(
        edit_ts_level, EDIT_TS_LEVEL_CASES,
        "unexpected set of `edit.ts`-level cases"
    );
    // `err-file-missing` is the only row with no file on disk, and the row that
    // pins the raw (relative) path inside the `access` failure message.
    let missing_file: Vec<&str> = cases
        .iter()
        .filter(|case| case.original.is_none())
        .map(|case| case.name.as_str())
        .collect();
    assert_eq!(missing_file, ["err-file-missing"]);
    // And one row proves the patch header uses the raw path, not `file.txt`.
    assert!(
        cases.iter().any(|case| case.path.contains('/')),
        "lost the nested-raw-path row"
    );
}

/// Replay every corpus row through the real tool against real files.
#[tokio::test]
async fn corpus_rows_match_pi() {
    let cases = load_corpus();
    assert_eq!(cases.len(), EXPECTED_CORPUS_ROWS);

    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();
    let cwd = root.to_str().expect("temp dir path must be UTF-8");
    // Default (local filesystem) operations: `err-file-missing`'s ENOENT has to
    // come from a real `access`, not from a fabricated error.
    let definition = create_edit_tool_definition(cwd, None);

    let mut driven_ok = 0usize;
    let mut driven_err = 0usize;

    for (index, case) in cases.iter().enumerate() {
        let label = format!("row {index} `{}` ({})", case.name, case.note);
        let target = root.join(&case.path);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .unwrap_or_else(|e| panic!("{label}: mkdir {}: {e}", parent.display()));
        }
        match &case.original {
            // Byte-exact: `original` carries BOMs and CRLFs.
            Some(original) => fs::write(&target, original.as_bytes())
                .unwrap_or_else(|e| panic!("{label}: write {}: {e}", target.display())),
            None => {
                if target.exists() {
                    fs::remove_file(&target)
                        .unwrap_or_else(|e| panic!("{label}: rm {}: {e}", target.display()));
                }
            }
        }

        // The loop's own path: `prepareArguments` then `execute`. It is the
        // identity for every canonical row (`prepare_arguments_cases_match_pi`
        // pins that), so this only widens what the replay covers.
        let args = AgentTool::prepare_arguments(
            &definition,
            json!({ "path": case.path, "edits": case.edits }),
        );
        let result = AgentTool::execute(
            &definition,
            &format!("call_{index}"),
            args,
            CancellationToken::new(),
            noop_update(),
        )
        .await;

        if !case.ok {
            driven_err += 1;
            let want = case.error.as_deref().expect("checked by corpus_shape");
            match result {
                Ok(ok) => panic!(
                    "{label}: expected error `{want}`, got Ok({})",
                    serde_json::to_string(&ok.content).unwrap()
                ),
                Err(error) => assert_bytes(&case.name, "error message", want, &error.to_string()),
            }
            // Structural, from `edit.ts:343-347`: the write happens only after
            // the replacements succeed, so a failed edit leaves the file alone.
            if let Some(original) = case.original.as_deref() {
                let after = fs::read(&target).expect("file should still be there");
                assert_eq!(
                    after,
                    original.as_bytes(),
                    "{label}: a failed edit must not touch the file"
                );
            }
            continue;
        }

        driven_ok += 1;
        let result = result.unwrap_or_else(|error| panic!("{label}: execute failed: {error}"));

        // 1. The content array, as bytes on the wire (the success message, with
        //    the post-prepareArguments edit count and the raw path).
        let want_content = serde_json::to_string(case.content.as_ref().unwrap()).unwrap();
        let got_content = serde_json::to_string(&result.content).unwrap();
        assert_bytes(&case.name, "content", &want_content, &got_content);

        // 2. `details`, whole: key order matters because it is persisted into the
        //    session JSONL.
        let want_details = case.details.as_ref().unwrap();
        let want_details_bytes = serde_json::to_string(want_details).unwrap();
        let got_details_bytes = serde_json::to_string(&result.details).unwrap();
        assert_bytes(
            &case.name,
            "details",
            &want_details_bytes,
            &got_details_bytes,
        );

        // 3. …and field by field, so a divergence reports where.
        assert_bytes(
            &case.name,
            "details.diff",
            want_details["diff"].as_str().expect("captured diff"),
            result.details["diff"].as_str().expect("emitted diff"),
        );
        assert_bytes(
            &case.name,
            "details.patch",
            want_details["patch"].as_str().expect("captured patch"),
            result.details["patch"].as_str().expect("emitted patch"),
        );
        assert_eq!(
            want_details["firstChangedLine"], result.details["firstChangedLine"],
            "case `{}`: details.firstChangedLine diverged",
            case.name
        );

        // 4. The real proof: the bytes on disk, with the BOM and the dominant
        //    line ending restored (`edit.ts:346-347`).
        let written = fs::read(&target)
            .unwrap_or_else(|e| panic!("{label}: read back {}: {e}", target.display()));
        let want_written = case.written_content.as_deref().unwrap();
        assert_bytes(
            &case.name,
            "writtenContent",
            want_written,
            &String::from_utf8_lossy(&written),
        );
        assert_eq!(
            written,
            want_written.as_bytes(),
            "case `{}`: bytes on disk diverged",
            case.name
        );
    }

    assert_eq!(driven_ok, EXPECTED_OK_ROWS, "must drive every success row");
    assert_eq!(
        driven_err, EXPECTED_ERROR_ROWS,
        "must drive every error row"
    );
    assert_eq!(
        driven_ok + driven_err,
        EXPECTED_CORPUS_ROWS,
        "must drive every row"
    );
}
