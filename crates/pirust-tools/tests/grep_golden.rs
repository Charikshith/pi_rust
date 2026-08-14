//! Oracle for `core/tools/grep.ts`.
//!
//! Three independent gates, all against bytes captured from real Pi by
//! `scripts/gen-tools-oracle.mjs`:
//!
//! 1. `tests/fixtures/pi/tools/schemas/grep.json` — `parameters` must serialize
//!    byte-identically. `grep` is the schema that pins TypeBox rule 5: `required`
//!    and `Type.Optional` properties interleave in declaration order.
//! 2. `tests/fixtures/pi/tools/strings/grep.json` — name / label / description /
//!    promptSnippet, plus the three `null`/`false` fields.
//! 3. The fifteen `"tool":"grep"` rows of
//!    `tests/fixtures/pi/tools/exec.corpus.jsonl`, replayed against a `tempfile`
//!    rebuild of `exec.tree.json` with the tool's `cwd` set to the tree root.
//!    `details` is compared as exact JSON text; error rows are compared as exact
//!    `Error.message`; `content` is compared after the *same* order normalization
//!    the capture applied (see [`sort_grep_by_file`]).
//!
//! A failure means the Rust port diverged from Pi; the fix is the port, never the
//! assertion.
//!
//! # This suite needs a real `rg`
//!
//! `grep` shells out to ripgrep, so the corpus rows cannot be replayed without the
//! binary. pirust's managed directory is `~/.pirust/agent/bin`, which on a machine
//! that only has Pi installed is empty — so the test locates `rg` itself (see
//! [`locate_rg`]) and injects it through `GrepToolOptions::rg_path`. If no `rg` can
//! be found the corpus test **fails loudly** rather than skipping: a silent skip
//! would let the whole port rot unnoticed.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

use pirust_agent_core::types::{AgentTool, AgentToolResult, AgentToolUpdateCallback};
use pirust_tools::definition::PirustToolDefinition;
use pirust_tools::grep::{create_grep_tool_definition, GrepToolOptions};
use serde_json::{Map, Value};
use tokio_util::sync::CancellationToken;

/// Number of `"tool":"grep"` rows in the captured corpus. Asserted so a truncated
/// fixture cannot silently weaken the suite.
const GREP_ROW_COUNT: usize = 15;

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

fn definition_for(cwd: &str, rg_path: Option<String>) -> PirustToolDefinition {
    create_grep_tool_definition(
        cwd,
        GrepToolOptions {
            operations: None,
            rg_path,
        },
    )
}

// ===========================================================================
// 1. Schema bytes
// ===========================================================================

#[test]
fn parameters_match_the_captured_schema_bytes() {
    let want = read_fixture("schemas/grep.json").trim_end().to_string();
    let definition = definition_for("C:\\anywhere", None);
    let got = serde_json::to_string(&definition.parameters).expect("serialize parameters");

    assert_eq!(
        got, want,
        "grep parameters must be byte-identical to schemas/grep.json\n  expected: {want}\n  \
         actual:   {got}"
    );
    // The point of this particular schema: one required property, and the six
    // optional ones stay interleaved in declaration order rather than being moved
    // after it.
    assert!(want.starts_with(r#"{"type":"object","required":["pattern"],"properties":{"pattern""#));
}

// ===========================================================================
// 2. Prompt strings
// ===========================================================================

#[test]
fn strings_match_the_captured_metadata() {
    let raw = read_fixture("strings/grep.json");
    let want: Value = serde_json::from_str(&raw).expect("parse strings/grep.json");
    let definition = create_grep_tool_definition("C:\\anywhere", GrepToolOptions::default());

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
        "grep has no promptGuidelines"
    );
    assert!(
        want["executionMode"].is_null() && definition.execution_mode.is_none(),
        "grep has no executionMode override"
    );
    assert_eq!(
        want["hasPrepareArguments"].as_bool(),
        Some(false),
        "the fixture says grep has no prepareArguments"
    );
    assert!(definition.prepare_arguments.is_none());
}

// ===========================================================================
// 3. Execution corpus
// ===========================================================================

/// Rebuild `exec.tree.json` verbatim: all directories, then all files as UTF-8
/// with LF newlines and no BOM (`fs::write` writes the JSON string's bytes
/// unchanged). `outAfterWrites` is deliberately ignored — it describes the state
/// *after* the `write`-tool rows, which were captured last.
fn build_fixture_tree(root: &Path) {
    let tree: Value = serde_json::from_str(&read_fixture("exec.tree.json")).expect("parse tree");

    assert_eq!(
        tree["rgAvailable"].as_bool(),
        Some(true),
        "the captured grep rows are only real output if the oracle found rg"
    );
    // `rg` honours `.gitignore` only inside a git repository, so an ancestor `.git`
    // would change which files are searched. The capture recorded that it had none.
    if tree["insideGitRepo"].as_bool() == Some(false) {
        let mut cursor = Some(root);
        while let Some(dir) = cursor {
            assert!(
                !dir.join(".git").exists(),
                "the corpus was captured outside a git repo, but the temp tree has an ancestor \
                 .git at {}; rg's ignore behaviour would differ",
                dir.display()
            );
            cursor = dir.parent();
        }
    }

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

/// Every `"tool":"grep"` row of the corpus, in file order.
fn grep_rows() -> Vec<Map<String, Value>> {
    read_fixture("exec.corpus.jsonl")
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str::<Map<String, Value>>(line)
                .unwrap_or_else(|e| panic!("deserialize corpus row failed: {e}\n  {line}"))
        })
        .filter(|row| row.get("tool").and_then(Value::as_str) == Some("grep"))
        .collect()
}

/// Find a real `rg`, or panic with instructions.
///
/// Order: `PIRUST_TEST_RG`, then Pi's own managed directory (`~/.pi/agent/bin`,
/// which is where the oracle script found it), then `PATH`. pirust's own
/// `~/.pirust/agent/bin` is covered by the `PATH`-less first branch of
/// `binaries::get_tool_path`, which the tool itself exercises when no override is
/// given; this helper deliberately does **not** copy anything into it.
fn locate_rg() -> String {
    let exe = if cfg!(windows) { "rg.exe" } else { "rg" };

    if let Ok(path) = std::env::var("PIRUST_TEST_RG") {
        if !path.is_empty() {
            assert!(
                Path::new(&path).exists(),
                "PIRUST_TEST_RG is set to {path:?}, which does not exist"
            );
            return path;
        }
    }

    let home_env = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    if let Ok(home) = std::env::var(home_env) {
        let candidate = Path::new(&home)
            .join(".pi")
            .join("agent")
            .join("bin")
            .join(exe);
        if candidate.exists() {
            return candidate.to_string_lossy().into_owned();
        }
    }

    // `binaries::SpawnProbe`'s test: "can `rg --version` be spawned at all?".
    let on_path = Command::new("rg")
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok();
    if on_path {
        return "rg".to_string();
    }

    panic!(
        "no ripgrep binary found, so the captured grep corpus cannot be replayed. Looked at \
         $PIRUST_TEST_RG, ${home_env}/.pi/agent/bin/{exe} and `rg` on PATH. Install ripgrep or \
         set PIRUST_TEST_RG=<path to rg>. This test fails rather than skips on purpose."
    );
}

// --- rg emission-order normalization ---------------------------------------
//
// Transcribed from `scripts/gen-tools-oracle.mjs:223-249`, which explains why it
// exists: `rg` walks directories in parallel, so the order in which *files* are
// emitted is not reproducible (the script measured two distinct orderings for
// `grep "export"` over a 6-file tree in 12 runs). Pi never sorts, so the order is
// external-binary noise, and every captured grep row carries
// `"orderNormalized": true`.
//
// The rule, which must be mirrored exactly: groups of *consecutive* rows belonging
// to the same file are sorted by file path, each file's own row order is PRESERVED
// (that intra-file ordering and the `path:N: ` / `path-N- ` prefixes are contract),
// and the trailing `"\n\n[...notice...]"` is never reordered.

/// `GREP_MATCH_ROW = /^(.*?):(\d+): /` then `GREP_CONTEXT_ROW = /^(.*?)-(\d+)- /`,
/// else the whole line (`gen-tools-oracle.mjs:234-236`).
fn grep_path_token(line: &str) -> &str {
    row_prefix(line, b':', ": ")
        .or_else(|| row_prefix(line, b'-', "- "))
        .unwrap_or(line)
}

/// The lazy `^(.*?)` + separator + `(\d+)` + `"<sep> "` match, hand-rolled: scan
/// left to right for the first separator that is followed by at least one digit and
/// then the two-character tail. Lazy quantification is exactly this
/// leftmost-separator scan, and the greedy `\d+` cannot need backtracking because a
/// digit never matches the separator that must follow it.
fn row_prefix<'a>(line: &'a str, separator: u8, tail: &str) -> Option<&'a str> {
    let bytes = line.as_bytes();
    for index in 0..bytes.len() {
        if bytes[index] != separator {
            continue;
        }
        let mut cursor = index + 1;
        while cursor < bytes.len() && bytes[cursor].is_ascii_digit() {
            cursor += 1;
        }
        if cursor == index + 1 {
            continue;
        }
        if line[cursor..].starts_with(tail) {
            return Some(&line[..index]);
        }
    }
    None
}

/// `sortGrepByFile` (`gen-tools-oracle.mjs:238-249`).
fn sort_grep_by_file(text: &str) -> String {
    let mut groups: Vec<(&str, Vec<&str>)> = Vec::new();
    for line in text.split('\n') {
        let token = grep_path_token(line);
        match groups.last_mut() {
            Some((last_token, lines)) if *last_token == token => lines.push(line),
            _ => groups.push((token, vec![line])),
        }
    }
    // `Array#sort` is stable, so equal tokens keep their relative order. JS compares
    // strings by UTF-16 code unit and Rust by UTF-8 byte; the two orders differ only
    // for astral characters versus U+E000..U+FFFF, which no fixture path contains.
    groups.sort_by(|a, b| a.0.cmp(b.0));
    groups
        .into_iter()
        .flat_map(|(_, lines)| lines)
        .collect::<Vec<_>>()
        .join("\n")
}

/// The capture's per-content-block normalization (`gen-tools-oracle.mjs:1338-1346`):
/// canonicalize only the rows above the first `"\n\n["`, leave the notice where Pi
/// put it.
fn normalize_grep_text(text: &str) -> String {
    match text.find("\n\n[") {
        None => sort_grep_by_file(text),
        Some(index) => format!("{}{}", sort_grep_by_file(&text[..index]), &text[index..]),
    }
}

/// [`normalize_grep_text`] applied to every `{"type":"text"}` block of a serialized
/// `content` array, exactly as the capture did.
fn normalize_content(content: &Value) -> Value {
    let Some(blocks) = content.as_array() else {
        return content.clone();
    };
    Value::Array(
        blocks
            .iter()
            .map(|block| {
                let is_text = block.get("type").and_then(Value::as_str) == Some("text");
                match (is_text, block.get("text").and_then(Value::as_str)) {
                    (true, Some(text)) => {
                        let mut block = block.clone();
                        block["text"] = Value::String(normalize_grep_text(text));
                        block
                    }
                    _ => block.clone(),
                }
            })
            .collect(),
    )
}

#[tokio::test]
async fn exec_corpus_rows_match_pi() {
    let rows = grep_rows();
    assert_eq!(
        rows.len(),
        GREP_ROW_COUNT,
        "exec.corpus.jsonl should hold all {GREP_ROW_COUNT} captured grep rows"
    );

    let rg_path = locate_rg();
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

        let definition = definition_for(&cwd, Some(rg_path.clone()));
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
                    "case {case} grep ({note})\n  args:     {args}\n  \
                     expected: content {}\n  actual:   Err({e})",
                    row["content"]
                )
            });

            // Both sides are order-normalized the same way; the fixture already is.
            assert_eq!(
                row["orderNormalized"].as_bool(),
                Some(true),
                "case {case} grep ({note}): every captured grep row is order-normalized"
            );
            let want_content =
                serde_json::to_string(&normalize_content(&row["content"])).expect("want content");
            let got = serde_json::to_value(&result.content).expect("serialize content");
            let got_content = serde_json::to_string(&normalize_content(&got)).expect("got content");
            assert_eq!(
                got_content, want_content,
                "case {case} grep ({note}): content diverged\n  args:     {args}\n  \
                 expected: {want_content}\n  actual:   {got_content}"
            );

            // `details: null` in the corpus is Pi's `details: undefined`.
            let want_details = serde_json::to_string(&row["details"]).expect("stringify details");
            let got_details = serde_json::to_string(&result.details).expect("serialize details");
            assert_eq!(
                got_details, want_details,
                "case {case} grep ({note}): details diverged\n  args:     {args}\n  \
                 expected: {want_details}\n  actual:   {got_details}"
            );
        } else {
            let want = row["error"]
                .as_str()
                .expect("!ok row without an error message")
                .replace("{TMPROOT}", &root);
            let err = actual.err().unwrap_or_else(|| {
                panic!("case {case} grep ({note}): Pi threw {want:?}, port returned Ok")
            });
            assert_eq!(
                err.to_string(),
                want,
                "case {case} grep ({note}): error message diverged\n  args:     {args}\n  \
                 expected: Err({want})\n  actual:   Err({err})"
            );
        }
    }
}

/// The normalizer is part of the assertion, so it gets its own guard: it must group
/// per file and sort the *groups*, never sort individual lines, and never touch the
/// notice.
#[test]
fn order_normalization_matches_the_capture_script() {
    // Groups are sorted by path; intra-file order (here descending line numbers,
    // and a context row before its match row) is preserved verbatim.
    let input = "b.ts:9: nine\nb.ts:2: two\na.ts-1- ctx\na.ts:2: hit";
    assert_eq!(
        sort_grep_by_file(input),
        "a.ts-1- ctx\na.ts:2: hit\nb.ts:9: nine\nb.ts:2: two"
    );

    // A plain line sort would produce a different answer; make sure we did not.
    let mut line_sorted: Vec<&str> = input.split('\n').collect();
    line_sorted.sort_unstable();
    assert_ne!(sort_grep_by_file(input), line_sorted.join("\n"));

    // The trailing notice stays put even though `[` sorts before any letter.
    let with_notice = "b.ts:1: b\na.ts:1: a\n\n[3 matches limit reached. Use limit=6 for more]";
    assert_eq!(
        normalize_grep_text(with_notice),
        "a.ts:1: a\nb.ts:1: b\n\n[3 matches limit reached. Use limit=6 for more]"
    );

    // Token extraction: lazy prefix, digits required, and the two row shapes.
    assert_eq!(
        grep_path_token("nested/c.txt:3: MATCH here"),
        "nested/c.txt"
    );
    assert_eq!(grep_path_token("nested/c.txt-4- tail"), "nested/c.txt");
    assert_eq!(grep_path_token("a:1: x:2: y"), "a");
    assert_eq!(grep_path_token("a:1:2: y"), "a:1");
    assert_eq!(grep_path_token("no row shape here"), "no row shape here");
}
