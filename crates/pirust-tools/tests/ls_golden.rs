//! Oracle for `core/tools/ls.ts`.
//!
//! Three independent gates, all against bytes captured from real Pi by
//! `scripts/gen-tools-oracle.mjs`:
//!
//! 1. `tests/fixtures/pi/tools/schemas/ls.json` — `parameters` must serialize
//!    byte-identically. `ls` is the one built-in whose schema carries **no**
//!    `required` key (both properties are `Type.Optional`), so this row is also
//!    what pins `object_schema`'s omit-when-empty rule.
//! 2. `tests/fixtures/pi/tools/strings/ls.json` — name / label / description /
//!    promptSnippet, plus the three `null`/`false` fields.
//! 3. The nine `"tool":"ls"` rows of `tests/fixtures/pi/tools/exec.corpus.jsonl`,
//!    replayed against a `tempfile` rebuild of `exec.tree.json` with the tool's
//!    `cwd` set to the tree root. `content` and `details` are compared as exact
//!    JSON text; error rows are compared as exact `Error.message`.
//!
//! A failure means the Rust port diverged from Pi; the fix is the port, never the
//! assertion.

use std::path::{Path, PathBuf};

use pirust_agent_core::types::{AgentTool, AgentToolResult, AgentToolUpdateCallback};
use pirust_tools::ls::{create_ls_tool_definition, LsToolOptions};
use serde_json::{Map, Value};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Number of `"tool":"ls"` rows in the captured corpus. Asserted so a truncated
/// fixture cannot silently weaken the suite.
const LS_ROW_COUNT: usize = 9;

fn fixture(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/pi/tools")
        .join(relative)
}

fn read_fixture(relative: &str) -> String {
    let path = fixture(relative);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read fixture {}: {e}", path.display()))
}

fn noop_update() -> AgentToolUpdateCallback {
    Arc::new(|_| {})
}

// ===========================================================================
// 1. Schema bytes
// ===========================================================================

#[test]
fn parameters_match_the_captured_schema_bytes() {
    let want = read_fixture("schemas/ls.json").trim_end().to_string();
    let definition = create_ls_tool_definition("C:\\anywhere", LsToolOptions::default());
    let got = serde_json::to_string(&definition.parameters).expect("serialize parameters");

    assert_eq!(
        got, want,
        "ls parameters must be byte-identical to schemas/ls.json\n  expected: {want}\n  \
         actual:   {got}"
    );
    // The point of this particular schema: no `required` key at all.
    assert!(
        !want.contains("\"required\""),
        "schemas/ls.json is supposed to be the no-`required` case; the fixture changed"
    );
    assert!(definition.parameters.get("required").is_none());
}

// ===========================================================================
// 2. Prompt strings
// ===========================================================================

#[test]
fn strings_match_the_captured_metadata() {
    let raw = read_fixture("strings/ls.json");
    let want: Value = serde_json::from_str(&raw).expect("parse strings/ls.json");
    let definition = create_ls_tool_definition("C:\\anywhere", LsToolOptions::default());

    for (field, expected, actual) in [
        (
            "name",
            want["name"].as_str(),
            Some(definition.name.as_str()),
        ),
        (
            "label",
            want["label"].as_str(),
            Some(definition.label.as_str()),
        ),
        (
            "description",
            want["description"].as_str(),
            Some(definition.description.as_str()),
        ),
        (
            "promptSnippet",
            want["promptSnippet"].as_str(),
            definition.prompt_snippet.as_deref(),
        ),
    ] {
        assert_eq!(
            actual, expected,
            "{field} diverged\n  expected: {expected:?}\n  actual:   {actual:?}"
        );
    }

    assert!(
        want["promptGuidelines"].is_null() && definition.prompt_guidelines.is_none(),
        "ls has no promptGuidelines"
    );
    assert!(
        want["executionMode"].is_null() && definition.execution_mode.is_none(),
        "ls has no executionMode override"
    );
    assert_eq!(
        want["hasPrepareArguments"].as_bool(),
        Some(false),
        "the fixture says ls has no prepareArguments"
    );
    assert!(definition.prepare_arguments.is_none());
}

// ===========================================================================
// 3. Execution corpus
// ===========================================================================

/// Rebuild `exec.tree.json` verbatim: all directories, then all files as UTF-8
/// with LF newlines and no BOM (`fs::write` writes the JSON string's bytes
/// unchanged). `outAfterWrites` is deliberately ignored — it describes the state
/// *after* the `write`-tool rows, and the `ls` rows were captured before them
/// (the root listing expects `out/` to exist and be empty).
fn build_fixture_tree(root: &Path) {
    let tree: Value = serde_json::from_str(&read_fixture("exec.tree.json")).expect("parse tree");

    for dir in tree["dirs"].as_array().expect("tree.dirs") {
        let relative = dir.as_str().expect("dir entry is a string");
        std::fs::create_dir_all(root.join(relative))
            .unwrap_or_else(|e| panic!("create dir {relative}: {e}"));
    }

    for (relative, content) in tree["files"].as_object().expect("tree.files") {
        let target = root.join(relative);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).expect("create parent dir");
        }
        let text = content
            .as_str()
            .unwrap_or_else(|| panic!("file {relative} content is not a string"));
        std::fs::write(&target, text.as_bytes())
            .unwrap_or_else(|e| panic!("write file {relative}: {e}"));
    }
}

/// Every `"tool":"ls"` row of the corpus, in file order.
fn ls_rows() -> Vec<Map<String, Value>> {
    read_fixture("exec.corpus.jsonl")
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str::<Map<String, Value>>(line)
                .unwrap_or_else(|e| panic!("deserialize corpus row failed: {e}\n  {line}"))
        })
        .filter(|row| row.get("tool").and_then(Value::as_str) == Some("ls"))
        .collect()
}

#[tokio::test]
async fn exec_corpus_rows_match_pi() {
    let rows = ls_rows();
    assert_eq!(
        rows.len(),
        LS_ROW_COUNT,
        "exec.corpus.jsonl should hold all {LS_ROW_COUNT} captured ls rows"
    );

    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_string_lossy().into_owned();
    build_fixture_tree(tmp.path());

    for (index, row) in rows.iter().enumerate() {
        let case = index + 1;
        let note = row["note"].as_str().unwrap_or("");
        let args = row["args"].clone();
        // Every captured row ran with `cwd` = the tree root, recorded as the
        // placeholder `{TMPROOT}`.
        let cwd = row["cwd"]
            .as_str()
            .expect("cwd")
            .replace("{TMPROOT}", &root);
        assert_eq!(
            cwd, root,
            "case {case}: unexpected captured cwd {:?}",
            row["cwd"]
        );

        let definition = create_ls_tool_definition(cwd.clone(), LsToolOptions::default());
        let actual = definition
            .execute(
                &format!("call_{case}"),
                args.clone(),
                CancellationToken::new(),
                noop_update(),
            )
            .await;

        let ok = row["ok"].as_bool().expect("ok");
        if ok {
            let result: AgentToolResult = actual.unwrap_or_else(|e| {
                panic!(
                    "case {case} ls ({note})\n  args:     {args}\n  \
                     expected: content {}\n  actual:   Err({e})",
                    row["content"]
                )
            });

            let want_content = serde_json::to_string(&row["content"]).expect("stringify content");
            let got_content = serde_json::to_string(&result.content).expect("serialize content");
            assert_eq!(
                got_content, want_content,
                "case {case} ls ({note}): content diverged\n  args:     {args}\n  \
                 expected: {want_content}\n  actual:   {got_content}"
            );

            // `details: null` in the corpus is Pi's `details: undefined`.
            let want_details = serde_json::to_string(&row["details"]).expect("stringify details");
            let got_details = serde_json::to_string(&result.details).expect("serialize details");
            assert_eq!(
                got_details, want_details,
                "case {case} ls ({note}): details diverged\n  args:     {args}\n  \
                 expected: {want_details}\n  actual:   {got_details}"
            );
        } else {
            let want = row["error"]
                .as_str()
                .expect("!ok row without an error message")
                .replace("{TMPROOT}", &root);
            let err = actual.err().unwrap_or_else(|| {
                panic!("case {case} ls ({note}): Pi threw {want:?}, port returned Ok")
            });
            assert_eq!(
                err.to_string(),
                want,
                "case {case} ls ({note}): error message diverged\n  args:     {args}\n  \
                 expected: Err({want})\n  actual:   Err({err})"
            );
        }
    }
}

/// The `mixed` row is the only one that can tell ICU root collation apart from a
/// codepoint sort, so it gets its own named guard: if it is ever dropped from the
/// corpus, the collation port would stop being tested at all.
#[test]
fn the_collation_row_is_still_in_the_corpus() {
    let row = ls_rows()
        .into_iter()
        .find(|row| row["args"]["path"].as_str() == Some("mixed") && row["args"]["limit"].is_null())
        .expect("the unlimited `mixed` row pins the collation order");

    assert_eq!(
        row["content"][0]["text"].as_str(),
        Some(concat!(
            ".dotfile\nApple.txt\napple2.txt\nbanana.txt\n",
            "\u{00C9}clair.txt\nSub/\n\u{00FC}ber.txt\nzebra.txt\nZulu.txt"
        )),
        "the captured collation order changed"
    );
}

/// The two rows that pin Pi's **unclamped** `limit ?? DEFAULT_LIMIT` (`ls.ts:125`).
///
/// `grep`/`find` clamp with `Math.max(1, …)`; `ls` does not, so
/// `results.length >= effectiveLimit` is already true on the first iteration, the
/// listing stays empty, and the empty-`results` early return (`ls.ts:175-178`) wins
/// — which throws away *both* the entry-limit notice and `details.entryLimitReached`
/// even though `entryLimitReached` was set. The `limit: 3` row cannot see this
/// (clamping to `max(1, 3)` is a no-op), so without these two rows adding a clamp
/// would be invisible.
///
/// The expected bytes are the captured ones; `exec_corpus_rows_match_pi` is what
/// replays them against the port.
#[test]
fn the_unclamped_limit_rows_are_still_in_the_corpus() {
    let rows = ls_rows();
    for limit in [0.0_f64, -1.0] {
        let row = rows
            .iter()
            .find(|row| {
                row["args"]["path"].as_str() == Some("mixed")
                    && row["args"]["limit"].as_f64() == Some(limit)
            })
            .unwrap_or_else(|| {
                panic!(
                    "the `mixed` row with limit={limit} pins that ls does NOT clamp `limit` to \
                     at least 1; without it a Math.max(1, …) clamp would pass the suite"
                )
            });
        assert_eq!(
            row["content"][0]["text"].as_str(),
            Some("(empty directory)"),
            "limit={limit} must take the (empty directory) branch"
        );
        assert!(
            row["details"].is_null(),
            "limit={limit} must lose details.entryLimitReached entirely"
        );
        assert!(
            !row["content"][0]["text"]
                .as_str()
                .expect("text")
                .contains("entries limit reached"),
            "limit={limit} must lose the entry-limit notice too"
        );
    }
}
