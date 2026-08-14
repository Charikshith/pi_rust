//! Pi-as-oracle test for `pirust_tools::read`.
//!
//! Three fixtures, all produced by executing real Pi
//! (`scripts/gen-tools-oracle.mjs`), gate this port:
//!
//! * `tests/fixtures/pi/tools/schemas/read.json` — the exact `JSON.stringify`
//!   bytes of `readSchema`, compared byte for byte (TypeBox key order included).
//! * `tests/fixtures/pi/tools/strings/read.json` — `name`, `label`,
//!   `description`, `promptSnippet`, `promptGuidelines`, `executionMode`,
//!   `hasPrepareArguments`.
//! * `tests/fixtures/pi/tools/exec.corpus.jsonl` — 13 `"tool":"read"` records
//!   captured against the temp tree described by `exec.tree.json`. The tree is
//!   rebuilt here into a `tempfile` directory, then every record is replayed
//!   through the real tool and its `content` / `details` (or its `error`
//!   message) compared exactly.
//!
//! Nothing in this file is a hand-written expectation: a failure means the port
//! diverged from Pi. The only transformation applied to a captured value is the
//! `{TMPROOT}` placeholder substitution the generator documents
//! (`gen-tools-oracle.mjs:73-79`), plus — on a non-Windows host — rewriting the
//! `\` separators of the win32 capture in the one absolute path that appears
//! inside an error message.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use pirust_agent_core::types::{AgentTool, AgentToolUpdateCallback};
use pirust_tools::read::{create_read_tool_definition, ReadToolOptions};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

/// Every `"tool":"read"` record in `exec.corpus.jsonl` must be exercised; a
/// shrinking fixture must fail loudly instead of silently weakening the suite.
const EXPECTED_READ_ROWS: usize = 13;

/// The generator's placeholder for the temp tree root
/// (`gen-tools-oracle.mjs:1001`).
const TMPROOT: &str = "{TMPROOT}";

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

fn no_op_update() -> AgentToolUpdateCallback {
    Arc::new(|_| {})
}

/// `parameters` + every string `createReadToolDefinition` puts on the wire.
#[test]
fn read_definition_matches_pi_bytes() {
    let definition = create_read_tool_definition("C:\\oracle\\cwd", None);

    // The schema is shipped to the provider as `JSON.stringify(parameters)`, so
    // key order is part of the contract.
    let expected_schema = read_fixture("schemas/read.json");
    let actual_schema = serde_json::to_string(&definition.parameters).expect("serialize schema");
    assert_eq!(
        actual_schema,
        expected_schema.trim_end_matches('\n'),
        "read `parameters` must serialize byte-identically to Pi"
    );

    let strings: Value =
        serde_json::from_str(&read_fixture("strings/read.json")).expect("parse strings/read.json");
    assert_eq!(Value::from(definition.name.clone()), strings["name"]);
    assert_eq!(Value::from(definition.label.clone()), strings["label"]);
    assert_eq!(
        Value::from(definition.description.clone()),
        strings["description"],
        "description embeds DEFAULT_MAX_LINES and DEFAULT_MAX_BYTES / 1024"
    );
    assert_eq!(
        Value::from(definition.prompt_snippet.clone().expect("promptSnippet")),
        strings["promptSnippet"]
    );
    assert_eq!(
        Value::from(definition.prompt_guidelines.clone().expect("guidelines")),
        strings["promptGuidelines"]
    );
    // `executionMode: null` and `hasPrepareArguments: false` — read overrides
    // neither.
    assert_eq!(strings["executionMode"], Value::Null);
    assert!(definition.execution_mode.is_none());
    assert_eq!(strings["hasPrepareArguments"], Value::Bool(false));
    assert!(definition.prepare_arguments.is_none());
}

/// Rebuild `exec.tree.json`'s `dirs` + `files` verbatim (UTF-8, LF, no BOM) —
/// the state the captured read records ran against.
fn materialize_tree(root: &Path) {
    let tree: Value =
        serde_json::from_str(&read_fixture("exec.tree.json")).expect("parse exec.tree.json");

    for dir in tree["dirs"].as_array().expect("tree.dirs is an array") {
        let dir = dir.as_str().expect("tree.dirs entry is a string");
        std::fs::create_dir_all(root.join(dir)).unwrap_or_else(|e| panic!("mkdir {dir}: {e}"));
    }
    for (relative, contents) in tree["files"].as_object().expect("tree.files is an object") {
        let contents = contents
            .as_str()
            .unwrap_or_else(|| panic!("tree.files[{relative}] is not a string"));
        let path = root.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap_or_else(|e| panic!("mkdir for {relative}: {e}"));
        }
        std::fs::write(&path, contents.as_bytes())
            .unwrap_or_else(|e| panic!("write {relative}: {e}"));
    }
}

/// Substitute the generator's `{TMPROOT}` placeholder.
///
/// `exec.tree.json` records `"platform": "win32"`, so the captured absolute path
/// uses `\`. On another host the tool legitimately produces `/`
/// (`resolveToCwd` delegates to `node:path`, whose separators are platform
/// dependent by contract — `gen-tools-oracle.mjs:90-93`), so the expectation is
/// converted rather than the actual value.
fn resolve_placeholders(captured: &str, root: &str) -> String {
    if cfg!(windows) {
        captured.replace(TMPROOT, root)
    } else {
        captured.replace('\\', "/").replace(TMPROOT, root)
    }
}

#[tokio::test]
async fn read_exec_rows_match_pi_oracle() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().to_str().expect("utf-8 temp path").to_string();
    materialize_tree(temp.path());

    let definition = create_read_tool_definition(root.clone(), Some(ReadToolOptions::default()));

    let corpus = read_fixture("exec.corpus.jsonl");
    let rows: Vec<Value> = corpus
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str::<Value>(line).expect("malformed corpus row"))
        .filter(|row| row["tool"] == "read")
        .collect();

    assert_eq!(
        rows.len(),
        EXPECTED_READ_ROWS,
        "exec.corpus.jsonl must still hold all {EXPECTED_READ_ROWS} read records"
    );

    for (index, row) in rows.iter().enumerate() {
        let case = index + 1;
        let note = row["note"].as_str().unwrap_or("<no note>");
        let label = format!("read row {case} \"{note}\" args={}", row["args"]);
        assert_eq!(
            row["cwd"],
            Value::from(TMPROOT),
            "{label}: every read record ran with cwd = the tree root"
        );

        let result = definition
            .execute(
                &format!("oracle-read-{index}"),
                row["args"].clone(),
                CancellationToken::new(),
                no_op_update(),
            )
            .await;

        if row["ok"] == Value::Bool(true) {
            let result = match result {
                Ok(result) => result,
                Err(error) => panic!("{label}: expected success, got error {error}"),
            };

            let expected_content = serde_json::to_string(&row["content"]).expect("serialize");
            let actual_content = serde_json::to_string(&result.content).expect("serialize");
            assert_eq!(
                actual_content, expected_content,
                "{label}: content diverged from Pi"
            );

            let expected_details = serde_json::to_string(&row["details"]).expect("serialize");
            let actual_details = serde_json::to_string(&result.details).expect("serialize");
            assert_eq!(
                actual_details, expected_details,
                "{label}: details diverged from Pi"
            );
        } else {
            let expected_error = resolve_placeholders(
                row["error"].as_str().expect("error rows carry a message"),
                &root,
            );
            match result {
                Ok(result) => panic!(
                    "{label}: expected error {expected_error:?}, got content {}",
                    serde_json::to_string(&result.content).expect("serialize")
                ),
                Err(error) => assert_eq!(
                    error.to_string(),
                    expected_error,
                    "{label}: error message diverged from Pi"
                ),
            }
        }
    }
}

/// A cancelled token yields Pi's `Operation aborted` (`read.ts:226`) instead of
/// a file read.
#[tokio::test]
async fn cancelled_token_aborts_like_pi() {
    let definition = create_read_tool_definition("C:\\oracle\\cwd", None);
    let token = CancellationToken::new();
    token.cancel();

    let error = definition
        .execute(
            "abort",
            serde_json::json!({ "path": "whatever.txt" }),
            token,
            no_op_update(),
        )
        .await
        .expect_err("a cancelled token aborts");
    assert_eq!(error.to_string(), "Operation aborted");
}

/// The image branch fails **loudly** while `processImage` is unported: a PNG is
/// never quietly decoded as UTF-8 text. See the `read` module docs' "Gap" and
/// the `#[ignore]`d test below for what is missing.
#[tokio::test]
async fn image_read_errors_instead_of_returning_mojibake() {
    let temp = tempfile::tempdir().expect("tempdir");
    let png = still_png();
    std::fs::write(temp.path().join("pixel.png"), &png).expect("write png");

    let definition = create_read_tool_definition(
        temp.path().to_str().expect("utf-8 temp path"),
        Some(ReadToolOptions::default()),
    );
    let error = definition
        .execute(
            "image",
            serde_json::json!({ "path": "pixel.png" }),
            CancellationToken::new(),
            no_op_update(),
        )
        .await
        .expect_err("image processing is not ported");
    assert!(
        error.to_string().contains("image processing is not ported"),
        "expected the unported-seam error, got {error}"
    );
    // The MIME sniff itself works, which is why the text branch was not taken.
    assert!(
        error.to_string().contains("image/png"),
        "the detected MIME type must appear in the error, got {error}"
    );
}

/// **Known gap (feat-004).** What is missing to make this pass:
///
/// * `utils/image-process.ts` (`processImage`) — normalize to png/jpeg/gif/webp,
///   resize to the inline limit when `autoResizeImages` is set, base64-encode.
/// * `utils/image-resize.ts` / `image-resize-core.ts` — decode + resize +
///   re-encode, i.e. an image codec dependency (Pi uses Photon/WASM; the port
///   plan names the `image` crate, `docs/analysis/03-coding-agent.md:201`).
/// * `utils/image-convert.ts` (`convertImageBytesToPng`) for the bmp path.
/// * A Pi oracle: no captured fixture reads an image, so the expectation below
///   is derived from `read.ts:255-262` (note text, then an `ImageContent`
///   block), not from a capture. Add image rows to `gen-tools-oracle.mjs`
///   together with the implementation.
///
/// The `ctx?.model` vision check (`read.ts:246`) is a second, independent gap:
/// there is no `ExtensionContext` port, so `[Current model does not support
/// images…]` can never be appended today. `non_vision_image_note` is ported and
/// unit-tested; only its caller is missing the model.
#[tokio::test]
#[ignore = "needs utils/image-process.ts + an image codec dependency (feat-004 may not add one)"]
async fn image_branch_needs_image_processing_dep() {
    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::write(temp.path().join("pixel.png"), still_png()).expect("write png");

    let definition = create_read_tool_definition(
        temp.path().to_str().expect("utf-8 temp path"),
        Some(ReadToolOptions::default()),
    );
    let result = definition
        .execute(
            "image",
            serde_json::json!({ "path": "pixel.png" }),
            CancellationToken::new(),
            no_op_update(),
        )
        .await
        .expect("image reads must succeed once processing is ported");

    let content = serde_json::to_value(&result.content).expect("serialize");
    let blocks = content.as_array().expect("content array");
    assert_eq!(
        blocks[0]["text"],
        Value::from("Read image file [image/png]")
    );
    assert_eq!(blocks[1]["type"], Value::from("image"));
    assert_eq!(blocks[1]["mimeType"], Value::from("image/png"));
}

/// A 1x1 still PNG: signature, a 13-byte `IHDR`, then an `IDAT` header — the
/// shape `detectSupportedImageMimeType` accepts (`utils/mime.ts:10-12`).
fn still_png() -> Vec<u8> {
    let mut png = vec![0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
    png.extend_from_slice(&13u32.to_be_bytes());
    png.extend_from_slice(b"IHDR");
    png.extend_from_slice(&1u32.to_be_bytes());
    png.extend_from_slice(&1u32.to_be_bytes());
    png.extend_from_slice(&[8, 6, 0, 0, 0]);
    png.extend_from_slice(&[0, 0, 0, 0]); // CRC
    png.extend_from_slice(&0u32.to_be_bytes());
    png.extend_from_slice(b"IDAT");
    png
}
