//! Pi-as-oracle test for `pirust_tools::write`.
//!
//! Three fixtures, all captured by executing real Pi (`scripts/gen-tools-oracle.mjs`):
//!
//! * `tests/fixtures/pi/tools/schemas/write.json` — the exact `JSON.stringify`
//!   bytes of `writeSchema` (`write.ts:14-17`).
//! * `tests/fixtures/pi/tools/strings/write.json` — `name` / `label` /
//!   `description` / `promptSnippet` / `promptGuidelines` / `executionMode` /
//!   `hasPrepareArguments` (`write.ts:187-193`).
//! * `tests/fixtures/pi/tools/exec.corpus.jsonl` — the six `"tool":"write"`
//!   records, replayed in file order against a rebuild of
//!   `exec.tree.json`. Rows 1 and 2 target the same path, so replay order is what
//!   makes row 2 an overwrite; the tree is therefore shared and not reset between
//!   rows.
//!
//! Nothing here is computed by hand. Each row is compared on three axes: the
//! `content` array's serialized bytes, `details`, and — the real proof — the bytes
//! that landed on disk (`writtenContent` / `writtenBytes`). That last axis is what
//! catches the tool's central trap: `out/utf8.txt` reports "15 bytes" while
//! `writtenBytes` is 19, because Pi's message counts UTF-16 code units. After
//! every row the whole `out/` subtree is checked against `outAfterWrites`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use pirust_agent_core::types::{AgentTool, AgentToolUpdateCallback};
use pirust_tools::write::create_write_tool_definition;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

/// Every `"tool":"write"` record must be exercised; a shrinking fixture has to
/// fail loudly instead of silently weakening the suite.
const EXPECTED_WRITE_ROWS: usize = 6;

/// Placeholder the capture script wrote in place of the fixture tree root.
const TMPROOT: &str = "{TMPROOT}";

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/pi/tools")
}

// ---------------------------------------------------------------------------
// Fixture shapes
// ---------------------------------------------------------------------------

/// `exec.tree.json`. `files` maps a tree-relative path to its exact contents;
/// `out_after_writes` maps an `out/`-relative path to its contents after the
/// write records ran, with `null` marking a directory.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FixtureTree {
    dirs: Vec<String>,
    files: BTreeMap<String, String>,
    out_after_writes: BTreeMap<String, Option<String>>,
}

/// One `"tool":"write"` record of `exec.corpus.jsonl`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WriteRow {
    args: WriteArgs,
    cwd: String,
    note: String,
    ok: bool,
    /// Pi's `AgentToolResult.content`, verbatim JSON.
    content: Value,
    /// Pi's `AgentToolResult.details` — `undefined`, captured as `null`.
    details: Value,
    /// Exactly what the file held afterwards.
    written_content: String,
    /// Its UTF-8 byte length — deliberately *not* the number in `content`.
    written_bytes: u64,
}

#[derive(Debug, Deserialize)]
struct WriteArgs {
    path: String,
    content: String,
}

/// `strings/write.json`.
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

fn load_tree() -> FixtureTree {
    let path = fixtures_dir().join("exec.tree.json");
    let raw = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

fn load_write_rows() -> Vec<WriteRow> {
    let path = fixtures_dir().join("exec.corpus.jsonl");
    let raw = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    raw.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .filter_map(|(index, line)| {
            let record: Value = serde_json::from_str(line)
                .unwrap_or_else(|e| panic!("parse {} line {}: {e}", path.display(), index + 1));
            if record.get("tool").and_then(Value::as_str) != Some("write") {
                return None;
            }
            Some(
                serde_json::from_value::<WriteRow>(record).unwrap_or_else(|e| {
                    panic!(
                        "parse write row at {} line {}: {e}",
                        path.display(),
                        index + 1
                    )
                }),
            )
        })
        .collect()
}

/// Recreate the captured tree verbatim: all files UTF-8, LF newlines, no BOM
/// (`exec.tree.json`'s `note`).
fn rebuild_tree(tree: &FixtureTree, root: &Path) {
    for dir in &tree.dirs {
        let path = root.join(dir);
        fs::create_dir_all(&path).unwrap_or_else(|e| panic!("mkdir {}: {e}", path.display()));
    }
    for (relative, contents) in &tree.files {
        let path = root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .unwrap_or_else(|e| panic!("mkdir {}: {e}", parent.display()));
        }
        fs::write(&path, contents.as_bytes())
            .unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
    }
}

fn noop_update() -> AgentToolUpdateCallback {
    Arc::new(|_| {})
}

/// Byte slices rendered for a failure message without hiding non-UTF-8 or
/// invisible differences.
fn show(bytes: &[u8]) -> String {
    format!("{:?}", String::from_utf8_lossy(bytes))
}

// ---------------------------------------------------------------------------
// Static tool data
// ---------------------------------------------------------------------------

/// `parameters` must serialize byte-identically to Pi's captured
/// `JSON.stringify(writeSchema)` — key order included (`required` before
/// `properties`).
#[test]
fn parameters_match_pi_bytes() {
    let path = fixtures_dir().join("schemas/write.json");
    let expected = fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
        .trim_end_matches(['\n', '\r'])
        .to_string();

    let definition = create_write_tool_definition("/oracle/cwd", None);
    let actual = serde_json::to_string(&AgentTool::parameters(&definition)).unwrap();

    assert_eq!(
        actual, expected,
        "write parameters bytes diverged\n  expected: {expected}\n  actual:   {actual}"
    );
}

/// Every string Pi's `createWriteToolDefinition` sets (`write.ts:187-193`), plus
/// the two "absent" facts the capture records: no `executionMode`, no
/// `prepareArguments`.
#[test]
fn strings_match_pi() {
    let path = fixtures_dir().join("strings/write.json");
    let raw = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let expected: FixtureStrings =
        serde_json::from_str(&raw).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()));

    let definition = create_write_tool_definition("/oracle/cwd", None);

    assert_eq!(definition.name, expected.name, "name diverged");
    assert_eq!(definition.label, expected.label, "label diverged");
    assert_eq!(
        definition.description, expected.description,
        "description diverged\n  expected: {:?}\n  actual:   {:?}",
        expected.description, definition.description
    );
    assert_eq!(
        definition.prompt_snippet.as_deref(),
        Some(expected.prompt_snippet.as_str()),
        "promptSnippet diverged"
    );
    assert_eq!(
        definition.prompt_guidelines.as_deref(),
        Some(expected.prompt_guidelines.as_slice()),
        "promptGuidelines diverged"
    );
    assert!(
        expected.execution_mode.is_none(),
        "fixture changed: write now records an executionMode ({:?})",
        expected.execution_mode
    );
    assert!(
        definition.execution_mode.is_none(),
        "write must not set executionMode"
    );
    assert!(
        !expected.has_prepare_arguments,
        "fixture changed: write now has a prepareArguments shim"
    );
    assert!(
        definition.prepare_arguments.is_none(),
        "write must not set prepareArguments"
    );
}

// ---------------------------------------------------------------------------
// Execution corpus
// ---------------------------------------------------------------------------

/// The corpus must keep covering the cases this port depends on. Asserted
/// separately so a fixture edit that drops one of them names the loss instead of
/// quietly reducing coverage.
#[test]
fn corpus_still_covers_the_load_bearing_cases() {
    let rows = load_write_rows();
    assert_eq!(
        rows.len(),
        EXPECTED_WRITE_ROWS,
        "expected {EXPECTED_WRITE_ROWS} write rows, found {}",
        rows.len()
    );

    // Nested-parent-dir creation: a target whose parent chain does not exist in
    // the rebuilt tree.
    assert!(
        rows.iter().any(|row| row.args.path == "out/deep/a/b/c.txt"),
        "lost the nested-parent-directory row"
    );
    // Overwrite-existing: the same path written twice, in order.
    assert_eq!(
        rows.iter()
            .filter(|row| row.args.path == "out/new.txt")
            .count(),
        2,
        "lost the overwrite-existing pair"
    );
    // The UTF-16-vs-UTF-8 row: reported length differs from the bytes on disk.
    assert!(
        rows.iter()
            .any(|row| row.written_bytes != row.args.content.encode_utf16().count() as u64),
        "lost the non-ASCII row whose reported length is not its byte length"
    );
    // Empty content, and content that must be written verbatim (CRLF).
    assert!(
        rows.iter().any(|row| row.args.content.is_empty()),
        "lost the empty-content row"
    );
    assert!(
        rows.iter().any(|row| row.args.content.contains("\r\n")),
        "lost the CRLF row"
    );
}

/// Replay every `"tool":"write"` record against a rebuild of the captured tree.
#[tokio::test]
async fn write_rows_match_pi() {
    let tree = load_tree();
    let rows = load_write_rows();
    assert_eq!(
        rows.len(),
        EXPECTED_WRITE_ROWS,
        "expected {EXPECTED_WRITE_ROWS} write rows, found {}",
        rows.len()
    );

    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();
    rebuild_tree(&tree, root);
    let cwd = root.to_str().expect("temp dir path must be UTF-8");

    let definition = create_write_tool_definition(cwd, None);

    for (index, row) in rows.iter().enumerate() {
        let case = format!("row {index} ({}) path={}", row.note, row.args.path);
        assert_eq!(row.cwd, TMPROOT, "{case}: unexpected captured cwd");
        assert!(
            row.ok,
            "{case}: fixture records a failure, not handled here"
        );

        let result = AgentTool::execute(
            &definition,
            &format!("call_{index}"),
            json!({ "path": row.args.path, "content": row.args.content }),
            CancellationToken::new(),
            noop_update(),
        )
        .await
        .unwrap_or_else(|error| panic!("{case}: execute failed: {error}"));

        // 1. The content array, as bytes on the wire.
        let actual_content = serde_json::to_string(&result.content).unwrap();
        let expected_content = serde_json::to_string(&row.content).unwrap();
        assert_eq!(
            actual_content, expected_content,
            "{case}: content diverged\n  expected: {expected_content}\n  actual:   {actual_content}"
        );

        // 2. `details` — `undefined` in Pi, captured as `null`.
        assert_eq!(
            result.details, row.details,
            "{case}: details diverged\n  expected: {}\n  actual:   {}",
            row.details, result.details
        );
        assert_eq!(
            serde_json::to_string(&result.details).unwrap(),
            "null",
            "{case}: details must serialize as null"
        );

        // 3. The real proof: what landed on disk.
        let written_path = root.join(&row.args.path);
        let written = fs::read(&written_path)
            .unwrap_or_else(|e| panic!("{case}: read back {}: {e}", written_path.display()));
        assert_eq!(
            written,
            row.written_content.as_bytes(),
            "{case}: file contents diverged\n  expected: {}\n  actual:   {}",
            show(row.written_content.as_bytes()),
            show(&written)
        );
        assert_eq!(
            written.len() as u64,
            row.written_bytes,
            "{case}: byte length on disk diverged\n  expected: {}\n  actual:   {}",
            row.written_bytes,
            written.len()
        );
    }

    // The whole `out/` subtree, after all six rows.
    for (relative, expected) in &tree.out_after_writes {
        let path = root.join("out").join(relative.trim_end_matches('/'));
        match expected {
            None => assert!(
                path.is_dir(),
                "outAfterWrites: {relative} should be a directory ({})",
                path.display()
            ),
            Some(contents) => {
                let actual = fs::read(&path)
                    .unwrap_or_else(|e| panic!("outAfterWrites: read {}: {e}", path.display()));
                assert_eq!(
                    actual,
                    contents.as_bytes(),
                    "outAfterWrites: {relative} diverged\n  expected: {}\n  actual:   {}",
                    show(contents.as_bytes()),
                    show(&actual)
                );
            }
        }
    }
}
