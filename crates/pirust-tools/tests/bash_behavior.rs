//! Behaviour oracle for `pirust_tools::bash`.
//!
//! `bash` is the one built-in whose `execute` the Pi oracle could not capture —
//! driving it spawns a real shell and pulls pids, timings and temp-file names into
//! the fixture. Two of its three surfaces *are* captured and are asserted here
//! byte-for-byte:
//!
//! * `tests/fixtures/pi/tools/schemas/bash.json` — the exact `JSON.stringify`
//!   bytes of `bashSchema` (`bash.ts:40-43`);
//! * `tests/fixtures/pi/tools/strings/bash.json` — `name` / `label` /
//!   `description` / `promptSnippet`, plus the three "absent" facts (no
//!   `promptGuidelines`, no `executionMode`, no `prepareArguments`).
//!
//! Everything else is pinned **structurally against the TS source**, with every
//! user-visible string quoted from it. The vehicle is a fake `BashOperations`:
//! `execute` drives command execution entirely through that trait
//! (`bash.ts:295`, `:400-405`), so a fake that replays canned chunks and a canned
//! outcome exercises the whole streaming/format/status pipeline with no process,
//! no shell, no clock and no filesystem — which is what makes the assertions here
//! deterministic on any host.
//!
//! Three things the fake cannot reach are covered separately:
//! * the two `LocalBashOperations` rejections that fire *before* anything is
//!   spawned (`resolveTimeoutMs`, then the abort check) — asserted against the
//!   real local operations, still without a shell;
//! * the shell-resolution ladder — asserted against a fake `ShellEnvironment`, so
//!   the Windows-only branches and the "no bash shell found" message are checked
//!   on any platform;
//! * the real spawn path — a handful of tests at the bottom, each of which
//!   **skips** (loudly) when no bash is available, so a bash-less machine cannot
//!   weaken the deterministic suite above.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pirust_agent_core::types::{AgentTool, AgentToolResult, AgentToolUpdateCallback, ToolError};
use pirust_ai::types::UserContent;
use pirust_tools::bash::{
    append_status, bash_parameters, create_bash_tool_definition, format_bash_output,
    get_shell_config, get_shell_env, resolve_shell_config, resolve_timeout_ms, BashExecOptions,
    BashExecResult, BashOperations, BashSpawnContext, BashToolOptions, CommandTransport,
    ShellConfig, ShellConfigError, ShellEnvironment,
};
use pirust_tools::binaries::{BinaryEnv, Platform};
use pirust_tools::output_accumulator::OutputSnapshot;
use pirust_tools::truncate::{TruncatedBy, TruncationResult};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/pi/tools")
}

// ---------------------------------------------------------------------------
// Static tool data (oracle-captured)
// ---------------------------------------------------------------------------

/// `strings/bash.json`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FixtureStrings {
    name: String,
    label: String,
    description: String,
    prompt_snippet: String,
    prompt_guidelines: Option<Value>,
    execution_mode: Option<Value>,
    has_prepare_arguments: bool,
}

/// `parameters` must serialize byte-identically to Pi's captured
/// `JSON.stringify(bashSchema)` — key order included (`required` before
/// `properties`).
#[test]
fn parameters_match_pi_bytes() {
    let path = fixtures_dir().join("schemas/bash.json");
    let expected = fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
        .trim_end_matches(['\n', '\r'])
        .to_string();

    let definition = create_bash_tool_definition("/oracle/cwd", None);
    let actual = serde_json::to_string(&AgentTool::parameters(&definition)).unwrap();
    assert_eq!(
        actual, expected,
        "bash parameters bytes diverged\n  expected: {expected}\n  actual:   {actual}"
    );

    // The free builder and the definition must agree, so a future caller of
    // `bash_parameters` cannot drift from the tool.
    assert_eq!(bash_parameters(), AgentTool::parameters(&definition));
}

/// Every string `createBashToolDefinition` sets (`bash.ts:299-303`), plus the
/// three "absent" facts the capture records.
#[test]
fn strings_match_pi() {
    let path = fixtures_dir().join("strings/bash.json");
    let raw = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let expected: FixtureStrings =
        serde_json::from_str(&raw).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()));

    let definition = create_bash_tool_definition("/oracle/cwd", None);

    assert_eq!(definition.name, expected.name, "name diverged");
    assert_eq!(definition.label, expected.label, "label diverged");
    assert_eq!(
        definition.description, expected.description,
        "description diverged\n  expected: {:?}\n  actual:   {:?}",
        expected.description, definition.description
    );
    // The description is a template literal over DEFAULT_MAX_LINES and
    // DEFAULT_MAX_BYTES / 1024 (`bash.ts:301`), so the captured bytes also pin
    // those two constants' rendering.
    assert!(
        expected
            .description
            .contains("truncated to last 2000 lines or 50KB"),
        "fixture changed: the description no longer names the two limits"
    );
    assert_eq!(
        definition.prompt_snippet.as_deref(),
        Some(expected.prompt_snippet.as_str()),
        "promptSnippet diverged"
    );
    assert!(
        expected.prompt_guidelines.is_none(),
        "fixture changed: bash now records promptGuidelines ({:?})",
        expected.prompt_guidelines
    );
    assert!(
        definition.prompt_guidelines.is_none(),
        "bash must not set promptGuidelines"
    );
    assert!(
        expected.execution_mode.is_none(),
        "fixture changed: bash now records an executionMode ({:?})",
        expected.execution_mode
    );
    assert!(
        definition.execution_mode.is_none(),
        "bash must not set executionMode"
    );
    assert!(
        !expected.has_prepare_arguments,
        "fixture changed: bash now has a prepareArguments shim"
    );
    assert!(
        definition.prepare_arguments.is_none(),
        "bash must not set prepareArguments"
    );
}

// ---------------------------------------------------------------------------
// The fake BashOperations
// ---------------------------------------------------------------------------

/// What `ops.exec` did with its arguments — the only way to observe
/// `commandPrefix` (`bash.ts:311`) and the spawn hook (`bash.ts:158-161`).
#[derive(Debug, Clone, PartialEq)]
struct RecordedExec {
    command: String,
    cwd: String,
    timeout: Option<f64>,
    env: BTreeMap<String, String>,
}

/// Replays canned output chunks through `onData`, then returns a canned outcome.
///
/// `outcome` is `Ok(exitCode)` or `Err(message)`; the message *is* the error
/// identity, exactly as in Pi (`bash.ts:410`, `:413` match on `err.message`).
struct FakeBashOperations {
    chunks: Vec<Vec<u8>>,
    outcome: Result<Option<i32>, String>,
    calls: Mutex<Vec<RecordedExec>>,
}

#[async_trait]
impl BashOperations for FakeBashOperations {
    async fn exec(
        &self,
        command: &str,
        cwd: &str,
        options: BashExecOptions,
    ) -> Result<BashExecResult, ToolError> {
        self.calls.lock().unwrap().push(RecordedExec {
            command: command.to_string(),
            cwd: cwd.to_string(),
            timeout: options.timeout,
            env: options.env.clone().unwrap_or_default(),
        });
        for chunk in &self.chunks {
            (options.on_data)(chunk);
        }
        match &self.outcome {
            Ok(exit_code) => Ok(BashExecResult {
                exit_code: *exit_code,
            }),
            Err(message) => Err(message.clone().into()),
        }
    }
}

/// One `execute` call plus everything it emitted.
struct Run {
    result: Result<AgentToolResult, ToolError>,
    updates: Vec<AgentToolResult>,
    calls: Vec<RecordedExec>,
}

impl Run {
    fn ok(&self) -> &AgentToolResult {
        self.result
            .as_ref()
            .unwrap_or_else(|e| panic!("expected success, got error {e:?}"))
    }

    /// The thrown message — the only thing that survives an error path.
    fn error_message(&self) -> String {
        self.result
            .as_ref()
            .err()
            .map(ToString::to_string)
            .unwrap_or_else(|| panic!("expected an error, got {:?}", self.result.as_ref().ok()))
    }

    fn text(&self) -> String {
        text_of(self.ok())
    }

    fn details(&self) -> &Value {
        &self.ok().details
    }
}

fn text_of(result: &AgentToolResult) -> String {
    result
        .content
        .iter()
        .map(|block| match block {
            UserContent::Text(text) => text.text.clone(),
            UserContent::Image(_) => panic!("bash never returns image content"),
        })
        .collect()
}

async fn run_with(
    chunks: Vec<Vec<u8>>,
    outcome: Result<Option<i32>, String>,
    args: Value,
    options: BashToolOptions,
) -> Run {
    let ops = Arc::new(FakeBashOperations {
        chunks,
        outcome,
        calls: Mutex::new(Vec::new()),
    });
    let definition = create_bash_tool_definition(
        "/oracle/cwd",
        Some(BashToolOptions {
            operations: Some(Arc::clone(&ops) as Arc<dyn BashOperations>),
            ..options
        }),
    );

    let updates: Arc<Mutex<Vec<AgentToolResult>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&updates);
    let on_update: AgentToolUpdateCallback =
        Arc::new(move |update| sink.lock().unwrap().push(update));

    let result = AgentTool::execute(
        &definition,
        "call_bash",
        args,
        CancellationToken::new(),
        on_update,
    )
    .await;

    let emitted = updates.lock().unwrap().clone();
    let calls = ops.calls.lock().unwrap().clone();
    Run {
        result,
        updates: emitted,
        calls,
    }
}

/// The common case: one canned outcome, no chunks beyond what is passed.
async fn run_fake(chunks: Vec<&str>, outcome: Result<Option<i32>, String>, args: Value) -> Run {
    run_with(
        chunks.into_iter().map(|c| c.as_bytes().to_vec()).collect(),
        outcome,
        args,
        BashToolOptions::default(),
    )
    .await
}

fn full_output_path(details: &Value) -> String {
    details
        .get("fullOutputPath")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("details has no fullOutputPath: {details}"))
        .to_string()
}

/// Deletes a spill file the run created, so a green suite leaves nothing behind.
fn remove_spill(path: &str) {
    let _ = fs::remove_file(path);
}

// ---------------------------------------------------------------------------
// execute: the success path
// ---------------------------------------------------------------------------

/// `formatOutput`'s default `emptyText` (`bash.ts:375`): a command that printed
/// nothing and exited 0 reports `(no output)`, with `details` absent.
///
/// Also pins the empty first update (`bash.ts:355-357`): `content: []`,
/// `details: undefined`.
#[tokio::test]
async fn no_output_success_reports_no_output_after_one_empty_update() {
    let run = run_fake(vec![], Ok(Some(0)), json!({ "command": "true" })).await;

    assert_eq!(run.text(), "(no output)");
    assert_eq!(*run.details(), Value::Null, "details must stay undefined");

    let first = run
        .updates
        .first()
        .expect("execute must emit one update before running anything");
    assert!(
        first.content.is_empty(),
        "the first update carries no content: {:?}",
        first.content
    );
    assert_eq!(first.details, Value::Null);
    // Nothing was ever appended, so the emitter stayed clean: no further updates.
    assert_eq!(run.updates.len(), 1, "{:?}", run.updates);
}

/// `exitCode !== 0 && exitCode !== null` (`bash.ts:422`): a signalled child
/// (`null` / `None`) is a **success**, not a failure.
#[tokio::test]
async fn a_null_exit_code_is_a_success() {
    let run = run_fake(vec!["partial\n"], Ok(None), json!({ "command": "sleep 1" })).await;
    assert_eq!(run.text(), "partial\n");
    assert_eq!(*run.details(), Value::Null);
}

/// `Command exited with code ${exitCode}` (`bash.ts:423`), joined by
/// `appendStatus`. Note the *three* newlines: the output keeps its own trailing
/// newline and `appendStatus` adds a blank line on top of it.
#[tokio::test]
async fn a_nonzero_exit_code_appends_the_status_to_the_output() {
    let run = run_fake(vec!["hi\n"], Ok(Some(3)), json!({ "command": "false" })).await;
    assert_eq!(run.error_message(), "hi\n\n\nCommand exited with code 3");

    // A nonzero exit is formatted on the SUCCESS path (`bash.ts:421`), i.e. with
    // the default `emptyText`, so a silent failure *does* say `(no output)` —
    // unlike an abort or a timeout, which format with an empty override.
    let run = run_fake(vec![], Ok(Some(127)), json!({ "command": "nope" })).await;
    assert_eq!(
        run.error_message(),
        "(no output)\n\nCommand exited with code 127"
    );
}

/// `appendStatus` (`bash.ts:395`) in isolation.
#[test]
fn append_status_joins_with_one_blank_line() {
    assert_eq!(append_status("", "Command aborted"), "Command aborted");
    assert_eq!(append_status("out", "S"), "out\n\nS");
    // A trailing newline in the text is preserved, not collapsed.
    assert_eq!(append_status("out\n", "S"), "out\n\n\nS");
}

/// `commandPrefix` (`bash.ts:311`) is joined with a bare newline, and the raw
/// `timeout` argument reaches `ops.exec` untouched (`bash.ts:403`).
#[tokio::test]
async fn the_command_prefix_is_prepended_with_a_newline() {
    let run = run_with(
        Vec::new(),
        Ok(Some(0)),
        json!({ "command": "ls", "timeout": 1.5 }),
        BashToolOptions {
            command_prefix: Some("set -euo pipefail".to_string()),
            ..BashToolOptions::default()
        },
    )
    .await;

    let call = run.calls.first().expect("ops.exec must have been called");
    assert_eq!(call.command, "set -euo pipefail\nls");
    assert_eq!(call.cwd, "/oracle/cwd");
    assert_eq!(call.timeout, Some(1.5));
    assert!(
        !call.env.is_empty(),
        "the spawn context must carry a real environment"
    );
}

/// `resolveSpawnContext` (`bash.ts:158-161`): the hook sees the already-prefixed
/// command and may replace command, cwd and env wholesale.
#[tokio::test]
async fn the_spawn_hook_can_rewrite_command_cwd_and_env() {
    let hook = Arc::new(|context: BashSpawnContext| BashSpawnContext {
        command: format!("ssh host {:?}", context.command),
        cwd: "/remote".to_string(),
        env: BTreeMap::from([("ONLY".to_string(), "1".to_string())]),
    });

    let run = run_with(
        Vec::new(),
        Ok(Some(0)),
        json!({ "command": "ls" }),
        BashToolOptions {
            command_prefix: Some("cd /tmp".to_string()),
            spawn_hook: Some(hook),
            ..BashToolOptions::default()
        },
    )
    .await;

    let call = run.calls.first().expect("ops.exec must have been called");
    assert_eq!(call.command, "ssh host \"cd /tmp\\nls\"");
    assert_eq!(call.cwd, "/remote");
    assert_eq!(
        call.env,
        BTreeMap::from([("ONLY".to_string(), "1".to_string())])
    );
}

// ---------------------------------------------------------------------------
// execute: the three error paths
// ---------------------------------------------------------------------------

/// `Command aborted` (`bash.ts:411`), and — because the error path passes an
/// **empty** `emptyText` (`bash.ts:409`) — no `(no output)` anywhere.
#[tokio::test]
async fn an_aborted_command_reports_command_aborted_without_no_output() {
    let run = run_fake(
        vec![],
        Err("aborted".to_string()),
        json!({ "command": "x" }),
    )
    .await;
    assert_eq!(run.error_message(), "Command aborted");
    assert!(
        !run.error_message().contains("(no output)"),
        "the error path must not substitute (no output)"
    );

    // Whatever had already been printed is kept, then the status is appended.
    let run = run_fake(
        vec!["half a line"],
        Err("aborted".to_string()),
        json!({ "command": "x" }),
    )
    .await;
    assert_eq!(run.error_message(), "half a line\n\nCommand aborted");
}

/// `Command timed out after ${timeoutSecs} seconds` (`bash.ts:415`), where
/// `timeoutSecs` is `err.message.split(":")[1]` — the raw value as JS rendered
/// it, so a fractional timeout stays fractional.
#[tokio::test]
async fn a_timeout_echoes_the_seconds_verbatim() {
    for (message, expected) in [
        ("timeout:1.5", "Command timed out after 1.5 seconds"),
        ("timeout:2", "Command timed out after 2 seconds"),
        ("timeout:0.25", "Command timed out after 0.25 seconds"),
        // Not a number at all: Pi does no parsing, it just splits.
        ("timeout:", "Command timed out after  seconds"),
    ] {
        let run = run_fake(
            vec![],
            Err(message.to_string()),
            json!({ "command": "sleep 9" }),
        )
        .await;
        assert_eq!(run.error_message(), expected, "for {message:?}");
    }

    // And with output, joined by appendStatus.
    let run = run_fake(
        vec!["tick\n"],
        Err("timeout:1.5".to_string()),
        json!({ "command": "sleep 9", "timeout": 1.5 }),
    )
    .await;
    assert_eq!(
        run.error_message(),
        "tick\n\n\nCommand timed out after 1.5 seconds"
    );
}

/// `throw err` (`bash.ts:417`): anything that is neither `aborted` nor
/// `timeout:*` reaches the model unchanged — which is how `Invalid timeout: …`,
/// `Working directory does not exist: …` and the "no bash shell found" message
/// get there. Any output already collected is **discarded** on this path.
#[tokio::test]
async fn other_errors_propagate_verbatim_and_drop_the_output() {
    for message in [
        "Invalid timeout: must be a finite number of seconds",
        "Invalid timeout: maximum is 2147483.647 seconds",
        "Working directory does not exist: /nope\nCannot execute bash commands.",
        "Custom shell path not found: /nope/bash",
        "spawn /bin/bash ENOENT",
    ] {
        let run = run_fake(
            vec!["printed before the failure\n"],
            Err(message.to_string()),
            json!({ "command": "x" }),
        )
        .await;
        assert_eq!(run.error_message(), message);
        assert!(
            !run.error_message().contains("printed before the failure"),
            "the verbatim path must not splice the output in"
        );
    }
}

// ---------------------------------------------------------------------------
// The three truncation footers
// ---------------------------------------------------------------------------

/// `truncatedBy === "lines"` (`bash.ts:386-387`). 2500 short lines: the line
/// limit binds, the byte limit does not.
#[tokio::test]
async fn the_line_limit_footer_names_the_kept_range() {
    let run = run_fake(
        vec![&"x\n".repeat(2500)],
        Ok(Some(0)),
        json!({ "command": "seq 2500" }),
    )
    .await;

    let path = full_output_path(run.details());
    let text = run.text();
    assert!(
        text.ends_with(&format!(
            "\n\n[Showing lines 501-2500 of 2500. Full output: {path}]"
        )),
        "unexpected footer in {:?}",
        &text[text.len().saturating_sub(120)..]
    );
    // The kept window is the LAST 2000 lines, joined without a trailing newline.
    assert!(text.starts_with("x\nx\n"));

    let truncation = &run.details()["truncation"];
    assert_eq!(truncation["truncated"], json!(true));
    assert_eq!(truncation["truncatedBy"], json!("lines"));
    assert_eq!(truncation["totalLines"], json!(2500));
    assert_eq!(truncation["outputLines"], json!(2000));
    assert_eq!(truncation["totalBytes"], json!(5000));
    remove_spill(&path);
}

/// The `else` branch (`bash.ts:388-389`): the byte limit binds while the line
/// limit does not. 100 lines of 1000 bytes = 100_100 bytes, so the tail keeps 51
/// whole lines.
#[tokio::test]
async fn the_byte_limit_footer_reports_50kb_and_the_kept_range() {
    let chunk = format!("{}\n", "a".repeat(1000)).repeat(100);
    let run = run_fake(vec![&chunk], Ok(Some(0)), json!({ "command": "cat big" })).await;

    let path = full_output_path(run.details());
    let text = run.text();
    assert!(
        text.ends_with(&format!(
            "\n\n[Showing lines 50-100 of 100 (50.0KB limit). Full output: {path}]"
        )),
        "unexpected footer in {:?}",
        &text[text.len().saturating_sub(120)..]
    );

    let truncation = &run.details()["truncation"];
    assert_eq!(truncation["truncatedBy"], json!("bytes"));
    assert_eq!(truncation["totalLines"], json!(100));
    assert_eq!(truncation["outputLines"], json!(51));
    assert_eq!(truncation["totalBytes"], json!(100_100));
    assert_eq!(truncation["outputBytes"], json!(51_050));
    assert_eq!(truncation["lastLinePartial"], json!(false));
    remove_spill(&path);
}

/// `lastLinePartial` (`bash.ts:383-385`): a single 60_000-byte line with no
/// newline. The footer reports the kept *size* and the whole line's size, not a
/// line range.
#[tokio::test]
async fn the_partial_line_footer_reports_both_sizes() {
    let run = run_fake(
        vec![&"a".repeat(60_000)],
        Ok(Some(0)),
        json!({ "command": "printf ..." }),
    )
    .await;

    let path = full_output_path(run.details());
    let text = run.text();
    // 51_200 B = 50.0KB kept; the line itself is 60_000 B = 58.6KB.
    assert!(
        text.ends_with(&format!(
            "\n\n[Showing last 50.0KB of line 1 (line is 58.6KB). Full output: {path}]"
        )),
        "unexpected footer in {:?}",
        &text[text.len().saturating_sub(120)..]
    );

    let truncation = &run.details()["truncation"];
    assert_eq!(truncation["lastLinePartial"], json!(true));
    assert_eq!(truncation["totalLines"], json!(1));
    assert_eq!(truncation["outputLines"], json!(1));
    assert_eq!(truncation["totalBytes"], json!(60_000));
    assert_eq!(truncation["outputBytes"], json!(51_200));
    remove_spill(&path);
}

/// The byte-limit footer hardcodes `formatSize(DEFAULT_MAX_BYTES)`
/// (`bash.ts:389`) — it says `50.0KB` even when the snapshot's own `maxBytes` is
/// something else. `execute` always builds the accumulator with Pi's defaults, so
/// this is only reachable by calling `formatOutput` directly.
#[test]
fn the_byte_limit_footer_ignores_the_snapshots_own_max_bytes() {
    let snapshot = snapshot_with(
        "kept",
        TruncationResult {
            content: "kept".to_string(),
            truncated: true,
            truncated_by: Some(TruncatedBy::Bytes),
            total_lines: 10,
            total_bytes: 99,
            output_lines: 4,
            output_bytes: 20,
            last_line_partial: false,
            first_line_exceeds_limit: false,
            max_lines: 2000,
            max_bytes: 8,
        },
        Some("/tmp/pirust-bash-0.log"),
    );

    let formatted = format_bash_output(&snapshot, 0, "(no output)");
    assert_eq!(
        formatted.text,
        "kept\n\n[Showing lines 7-10 of 10 (50.0KB limit). Full output: /tmp/pirust-bash-0.log]",
        "the limit in the footer is DEFAULT_MAX_BYTES, not truncation.maxBytes (8B)"
    );
    let details = formatted.details.expect("truncated output carries details");
    assert_eq!(
        details.full_output_path.as_deref(),
        Some("/tmp/pirust-bash-0.log")
    );
    assert_eq!(details.truncation.map(|t| t.max_bytes), Some(8));
}

/// `formatOutput`'s `emptyText` parameter, both values (`bash.ts:375`, `:409`),
/// and the fact that an untruncated snapshot carries no `details` at all.
#[test]
fn format_output_substitutes_empty_text_and_omits_details_when_untouched() {
    let untouched = TruncationResult {
        content: String::new(),
        truncated: false,
        truncated_by: None,
        total_lines: 0,
        total_bytes: 0,
        output_lines: 0,
        output_bytes: 0,
        last_line_partial: false,
        first_line_exceeds_limit: false,
        max_lines: 2000,
        max_bytes: 51_200,
    };
    let snapshot = snapshot_with("", untouched, None);

    let success = format_bash_output(&snapshot, 0, "(no output)");
    assert_eq!(success.text, "(no output)");
    assert!(success.details.is_none());

    let failure = format_bash_output(&snapshot, 0, "");
    assert_eq!(failure.text, "");
    assert!(failure.details.is_none());
}

fn snapshot_with(
    content: &str,
    truncation: TruncationResult,
    full_output_path: Option<&str>,
) -> OutputSnapshot {
    OutputSnapshot {
        content: content.to_string(),
        truncation,
        full_output_path: full_output_path.map(PathBuf::from),
    }
}

// ---------------------------------------------------------------------------
// Streaming updates
// ---------------------------------------------------------------------------

/// Partial updates carry `details` (so the UI can show a truncation warning) but
/// **never** the footer text: the emitter sends `snapshot.content` raw
/// (`bash.ts:325`) and only the terminal `formatOutput` appends a footer.
#[tokio::test]
async fn partial_updates_carry_details_but_never_the_footer() {
    let chunk = format!("{}\n", "a".repeat(1000)).repeat(100);
    let run = run_fake(vec![&chunk], Ok(Some(0)), json!({ "command": "cat big" })).await;
    let path = full_output_path(run.details());

    assert!(
        run.updates.len() >= 2,
        "expected the empty update plus at least one streamed update, got {}",
        run.updates.len()
    );
    for (index, update) in run.updates.iter().enumerate() {
        let text = text_of(update);
        assert!(
            !text.contains("[Showing"),
            "update {index} leaked a truncation footer: {:?}",
            &text[text.len().saturating_sub(120)..]
        );
        assert!(
            !text.contains("(no output)"),
            "update {index} leaked the empty-output placeholder"
        );
    }

    // The streamed update does report the truncation as structure.
    let streamed = &run.updates[1];
    assert_eq!(
        streamed.details["truncation"]["truncatedBy"],
        json!("bytes"),
        "{:?}",
        streamed.details
    );
    assert_eq!(streamed.details["fullOutputPath"], json!(path));
    // Only the final result carries the footer.
    assert!(run
        .text()
        .contains("[Showing lines 50-100 of 100 (50.0KB limit)."));
    remove_spill(&path);
}

/// The truncation record and the spill path never reach the model as *structure*
/// on an error path — `execute` destructures only `text` (`bash.ts:409`) and then
/// throws, and Rust's `ToolError` has nowhere to put `details` anyway. What does
/// survive is the footer, baked into the message.
#[tokio::test]
async fn details_are_dropped_on_every_error_path_but_the_footer_survives() {
    let chunk = format!("{}\n", "a".repeat(1000)).repeat(100);

    for (outcome, expected_status) in [
        (Err("aborted".to_string()), Some("Command aborted")),
        (
            Err("timeout:1.5".to_string()),
            Some("Command timed out after 1.5 seconds"),
        ),
        (Ok(Some(9)), Some("Command exited with code 9")),
        (Err("kaboom".to_string()), None),
    ] {
        let run = run_fake(vec![&chunk], outcome.clone(), json!({ "command": "x" })).await;
        let message = run.error_message();
        assert!(run.result.is_err(), "expected an error for {outcome:?}");

        // The spill path is only discoverable from the last streaming update now.
        let path = run
            .updates
            .last()
            .and_then(|update| update.details.get("fullOutputPath"))
            .and_then(Value::as_str)
            .expect("the streamed update carries the spill path")
            .to_string();

        match expected_status {
            Some(status) => {
                assert!(
                    message.contains(&format!(
                        "[Showing lines 50-100 of 100 (50.0KB limit). Full output: {path}]"
                    )),
                    "the footer must survive into the message: {message:?}"
                );
                assert!(
                    message.ends_with(&format!("\n\n{status}")),
                    "expected {status:?} at the end of {message:?}"
                );
            }
            // The verbatim path keeps neither the output nor the footer.
            None => assert_eq!(message, "kaboom"),
        }
        remove_spill(&path);
    }
}

// ---------------------------------------------------------------------------
// resolveTimeoutMs (bash.ts:27-38)
// ---------------------------------------------------------------------------

/// All three outcomes, including the float spelling of `MAX_TIMEOUT_SECONDS`.
#[test]
fn resolve_timeout_ms_matches_pi() {
    // 1. `undefined` passes through — there is NO default timeout.
    assert_eq!(resolve_timeout_ms(None), Ok(None));

    // 2. Seconds become milliseconds, fractions included.
    assert_eq!(resolve_timeout_ms(Some(1.0)), Ok(Some(1000.0)));
    assert_eq!(resolve_timeout_ms(Some(1.5)), Ok(Some(1500.0)));
    assert_eq!(resolve_timeout_ms(Some(0.0015)), Ok(Some(1.5)));
    assert_eq!(resolve_timeout_ms(Some(2_000_000.0)), Ok(Some(2e9)));

    // 3. Non-finite or non-positive.
    for value in [0.0, -0.0, -1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let error = resolve_timeout_ms(Some(value)).expect_err("must be rejected");
        assert_eq!(
            error.to_string(),
            "Invalid timeout: must be a finite number of seconds",
            "for {value}"
        );
    }

    // 4. Above `setTimeout`'s 32-bit ceiling. The message prints
    //    MAX_TIMEOUT_MS / 1000 the way JS would: `2147483.647`.
    for value in [2_147_484.0, 1e9, f64::MAX] {
        let error = resolve_timeout_ms(Some(value)).expect_err("must be rejected");
        assert_eq!(
            error.to_string(),
            "Invalid timeout: maximum is 2147483.647 seconds",
            "for {value}"
        );
    }
    // Just under the ceiling is fine.
    assert!(resolve_timeout_ms(Some(2_147_483.0)).is_ok());
}

// ---------------------------------------------------------------------------
// getShellConfig (shell.ts:67-120)
// ---------------------------------------------------------------------------

/// A scripted `ShellEnvironment`, so every branch of the ladder is reachable on
/// any host.
struct FakeShellEnvironment {
    platform: Platform,
    vars: BTreeMap<String, String>,
    existing: Vec<String>,
    bash_on_path: Option<String>,
    probes: Mutex<u32>,
}

impl FakeShellEnvironment {
    fn new(platform: Platform) -> Self {
        Self {
            platform,
            vars: BTreeMap::new(),
            existing: Vec::new(),
            bash_on_path: None,
            probes: Mutex::new(0),
        }
    }

    fn with_var(mut self, key: &str, value: &str) -> Self {
        self.vars.insert(key.to_string(), value.to_string());
        self
    }

    fn with_existing(mut self, path: &str) -> Self {
        self.existing.push(path.to_string());
        self
    }

    fn with_bash_on_path(mut self, path: &str) -> Self {
        self.bash_on_path = Some(path.to_string());
        self
    }

    /// The two Windows Git Bash candidates, in Pi's order.
    fn with_program_files(self) -> Self {
        self.with_var("ProgramFiles", r"C:\Program Files")
            .with_var("ProgramFiles(x86)", r"C:\Program Files (x86)")
    }

    fn probe_count(&self) -> u32 {
        *self.probes.lock().unwrap()
    }
}

#[async_trait]
impl ShellEnvironment for FakeShellEnvironment {
    fn platform(&self) -> Platform {
        self.platform
    }

    fn env_var(&self, key: &str) -> Option<String> {
        self.vars.get(key).cloned()
    }

    fn exists(&self, path: &str) -> bool {
        self.existing.iter().any(|candidate| candidate == path)
    }

    async fn find_bash_on_path(&self) -> Option<String> {
        *self.probes.lock().unwrap() += 1;
        self.bash_on_path.clone()
    }
}

/// Step 1 (`shell.ts:69-74`): an explicit `shellPath` wins outright, and a
/// missing one is a hard error rather than a fallback.
#[tokio::test]
async fn an_explicit_shell_path_wins_and_must_exist() {
    let env = FakeShellEnvironment::new(Platform::Linux)
        .with_existing("/opt/custom/bash")
        .with_existing("/bin/bash");
    let config = resolve_shell_config(&env, Some("/opt/custom/bash"))
        .await
        .expect("the custom path exists");
    assert_eq!(config.shell, "/opt/custom/bash");
    assert_eq!(config.args, ["-c"]);
    assert_eq!(env.probe_count(), 0, "no PATH probe when a path is given");

    let error = resolve_shell_config(&env, Some("/opt/missing/bash"))
        .await
        .expect_err("a missing custom path must fail");
    assert_eq!(
        error,
        ShellConfigError::CustomShellPathNotFound("/opt/missing/bash".to_string())
    );
    assert_eq!(
        error.to_string(),
        "Custom shell path not found: /opt/missing/bash"
    );
}

/// `getBashShellConfig` (`shell.ts:20-22`): the legacy WSL shim gets `-s` and the
/// command on stdin; everything else gets `-c` on argv.
#[tokio::test]
async fn the_legacy_wsl_shim_takes_the_command_on_stdin() {
    let wsl = r"C:\Windows\System32\bash.exe";
    let env = FakeShellEnvironment::new(Platform::Win32).with_existing(wsl);
    let config = resolve_shell_config(&env, Some(wsl)).await.unwrap();
    assert_eq!(config.args, ["-s"]);
    assert_eq!(config.command_transport, CommandTransport::Stdin);

    let git_bash = r"C:\Program Files\Git\bin\bash.exe";
    let env = FakeShellEnvironment::new(Platform::Win32).with_existing(git_bash);
    let config = resolve_shell_config(&env, Some(git_bash)).await.unwrap();
    assert_eq!(config.args, ["-c"]);
    assert_eq!(config.command_transport, CommandTransport::Argv);
}

/// JS truthiness, twice over: `if (customShellPath)` (`shell.ts:69`) and
/// `if (programFiles)` (`shell.ts:80`, `:84`) both treat an **empty** string as
/// absent, so neither becomes a candidate — and neither fails the call.
#[tokio::test]
async fn empty_strings_are_treated_as_absent_not_as_paths() {
    // An empty `shellPath` falls through to the platform ladder.
    let env = FakeShellEnvironment::new(Platform::Linux).with_existing("/bin/bash");
    let config = resolve_shell_config(&env, Some("")).await.unwrap();
    assert_eq!(config.shell, "/bin/bash");

    // An empty `%ProgramFiles%` contributes no candidate and no searched line.
    let env = FakeShellEnvironment::new(Platform::Win32)
        .with_var("ProgramFiles", "")
        .with_var("ProgramFiles(x86)", r"C:\Program Files (x86)");
    let error = resolve_shell_config(&env, None).await.unwrap_err();
    assert!(
        error
            .to_string()
            .ends_with("Searched Git Bash in:\n  C:\\Program Files (x86)\\Git\\bin\\bash.exe"),
        "{error}"
    );
}

/// Step 2 (`shell.ts:77-98`), all three Windows rungs in order.
#[tokio::test]
async fn windows_prefers_git_bash_then_falls_back_to_path() {
    // 64-bit Git Bash first.
    let env = FakeShellEnvironment::new(Platform::Win32)
        .with_program_files()
        .with_existing(r"C:\Program Files\Git\bin\bash.exe")
        .with_existing(r"C:\Program Files (x86)\Git\bin\bash.exe");
    let config = resolve_shell_config(&env, None).await.unwrap();
    assert_eq!(config.shell, r"C:\Program Files\Git\bin\bash.exe");
    assert_eq!(env.probe_count(), 0, "a Git Bash hit skips the PATH probe");

    // Then the 32-bit location.
    let env = FakeShellEnvironment::new(Platform::Win32)
        .with_program_files()
        .with_existing(r"C:\Program Files (x86)\Git\bin\bash.exe");
    let config = resolve_shell_config(&env, None).await.unwrap();
    assert_eq!(config.shell, r"C:\Program Files (x86)\Git\bin\bash.exe");

    // Then whatever `where bash.exe` finds — Cygwin, MSYS2, WSL, …
    let env = FakeShellEnvironment::new(Platform::Win32)
        .with_program_files()
        .with_bash_on_path(r"C:\msys64\usr\bin\bash.exe");
    let config = resolve_shell_config(&env, None).await.unwrap();
    assert_eq!(config.shell, r"C:\msys64\usr\bin\bash.exe");
    assert_eq!(env.probe_count(), 1);
}

/// `shell.ts:100-106`: there is no `cmd.exe` and no PowerShell fallback — the
/// call fails with the three-option message and the searched-paths list.
#[tokio::test]
async fn windows_without_any_bash_reports_the_full_message() {
    let env = FakeShellEnvironment::new(Platform::Win32).with_program_files();
    let error = resolve_shell_config(&env, None)
        .await
        .expect_err("no bash anywhere");
    assert_eq!(
        error.to_string(),
        concat!(
            "No bash shell found. Options:\n",
            "  1. Install Git for Windows: https://git-scm.com/download/win\n",
            "  2. Add your bash to PATH (Cygwin, MSYS2, etc.)\n",
            "  3. Set shellPath in settings.json\n",
            "\n",
            "Searched Git Bash in:\n",
            "  C:\\Program Files\\Git\\bin\\bash.exe\n",
            "  C:\\Program Files (x86)\\Git\\bin\\bash.exe",
        )
    );

    // With neither ProgramFiles variable set the list is empty, and JS's
    // `[].join("\n")` leaves the message ending in a bare newline.
    let env = FakeShellEnvironment::new(Platform::Win32);
    let error = resolve_shell_config(&env, None).await.unwrap_err();
    assert!(
        error.to_string().ends_with("Searched Git Bash in:\n"),
        "{error}"
    );
}

/// Step 3 (`shell.ts:110-119`): `/bin/bash`, then `which bash`, then a bare
/// `sh -c` — which is the only resolved shell that skips `existsSync`.
#[tokio::test]
async fn unix_prefers_bin_bash_then_path_then_sh() {
    let env = FakeShellEnvironment::new(Platform::Linux)
        .with_existing("/bin/bash")
        .with_bash_on_path("/usr/local/bin/bash");
    let config = resolve_shell_config(&env, None).await.unwrap();
    assert_eq!(config.shell, "/bin/bash");
    assert_eq!(env.probe_count(), 0);

    let env = FakeShellEnvironment::new(Platform::Darwin).with_bash_on_path("/opt/brew/bin/bash");
    let config = resolve_shell_config(&env, None).await.unwrap();
    assert_eq!(config.shell, "/opt/brew/bin/bash");
    assert_eq!(env.probe_count(), 1);

    let env = FakeShellEnvironment::new(Platform::Android);
    let config = resolve_shell_config(&env, None).await.unwrap();
    assert_eq!(
        config,
        ShellConfig {
            shell: "sh".to_string(),
            args: vec!["-c".to_string()],
            command_transport: CommandTransport::Argv,
        }
    );
}

/// Whatever rung is taken, the shell is invoked one-shot: `-l` (login) and `-i`
/// (interactive) are never passed, so no user rc file can rewrite the command's
/// environment.
#[tokio::test]
async fn no_resolved_shell_is_ever_a_login_or_interactive_shell() {
    let candidates: Vec<(FakeShellEnvironment, Option<&str>)> = vec![
        (
            FakeShellEnvironment::new(Platform::Linux).with_existing("/bin/bash"),
            None,
        ),
        (
            FakeShellEnvironment::new(Platform::Linux).with_bash_on_path("/usr/bin/bash"),
            None,
        ),
        (FakeShellEnvironment::new(Platform::Linux), None),
        (
            FakeShellEnvironment::new(Platform::Win32)
                .with_program_files()
                .with_existing(r"C:\Program Files\Git\bin\bash.exe"),
            None,
        ),
        (
            FakeShellEnvironment::new(Platform::Win32).with_existing(r"C:\w\System32\bash.exe"),
            Some(r"C:\w\System32\bash.exe"),
        ),
    ];

    for (env, custom) in candidates {
        let config = resolve_shell_config(&env, custom).await.unwrap();
        assert!(
            config.args == ["-c"] || config.args == ["-s"],
            "unexpected shell args {:?} for {}",
            config.args,
            config.shell
        );
        for argument in &config.args {
            assert_ne!(argument, "-l", "a login shell must never be requested");
            assert_ne!(
                argument, "-i",
                "an interactive shell must never be requested"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// getShellEnv (shell.ts:122-134)
// ---------------------------------------------------------------------------

/// The managed bin dir is prepended to the existing `PATH` (under whatever
/// casing that variable already uses), and only once.
#[test]
fn the_shell_env_prepends_the_managed_bin_dir_to_path() {
    let Ok(bin_dir) = BinaryEnv::from_process_env().tools_dir() else {
        eprintln!("skipped: the home directory is not resolvable on this machine");
        return;
    };
    let bin_dir = bin_dir.to_string_lossy().into_owned();
    let env = get_shell_env().expect("the bin dir resolved, so getShellEnv must succeed");

    let (key, value) = env
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("path"))
        .expect("a PATH entry must exist");
    let delimiter = if cfg!(windows) { ';' } else { ':' };
    let entries: Vec<&str> = value.split(delimiter).filter(|e| !e.is_empty()).collect();

    assert_eq!(
        entries.first().copied(),
        Some(bin_dir.as_str()),
        "{key} must start with the managed bin dir"
    );
    assert_eq!(
        entries.iter().filter(|entry| **entry == bin_dir).count(),
        1,
        "the bin dir must not be added twice"
    );
    // Every other variable is carried over untouched.
    for (key, value) in std::env::vars() {
        if key.eq_ignore_ascii_case("path") {
            continue;
        }
        assert_eq!(env.get(&key), Some(&value), "{key} was not carried over");
    }
}

// ---------------------------------------------------------------------------
// LocalBashOperations: the rejections that need no shell
// ---------------------------------------------------------------------------

fn noop_update() -> AgentToolUpdateCallback {
    Arc::new(|_| {})
}

/// `resolveTimeoutMs` runs first in `createLocalBashOperations.exec`
/// (`bash.ts:85`), before the abort check and before any shell resolution — so
/// these two messages reach the model on every machine, bash or no bash, and they
/// travel the "propagate verbatim" path (`bash.ts:417`).
#[tokio::test]
async fn the_local_operations_reject_a_bad_timeout_before_spawning() {
    let definition = create_bash_tool_definition("/definitely/not/a/directory", None);

    for (timeout, expected) in [
        (
            json!(0),
            "Invalid timeout: must be a finite number of seconds",
        ),
        (
            json!(-1),
            "Invalid timeout: must be a finite number of seconds",
        ),
        (
            json!(1e9),
            "Invalid timeout: maximum is 2147483.647 seconds",
        ),
    ] {
        let error = AgentTool::execute(
            &definition,
            "call_timeout",
            json!({ "command": "echo hi", "timeout": timeout }),
            CancellationToken::new(),
            noop_update(),
        )
        .await
        .expect_err("an invalid timeout must fail");
        assert_eq!(error.to_string(), expected, "for timeout {timeout}");
    }
}

/// `if (signal?.aborted) throw new Error("aborted")` (`bash.ts:86-88`) also runs
/// before the shell is resolved, so an already-cancelled call is rejected without
/// touching the filesystem — and `execute` turns it into `Command aborted` with
/// no `(no output)`.
#[tokio::test]
async fn the_local_operations_reject_an_already_aborted_call_before_spawning() {
    let token = CancellationToken::new();
    token.cancel();
    let error = AgentTool::execute(
        &create_bash_tool_definition("/definitely/not/a/directory", None),
        "call_aborted",
        json!({ "command": "echo hi" }),
        token,
        noop_update(),
    )
    .await
    .expect_err("an aborted call must fail");
    assert_eq!(error.to_string(), "Command aborted");
}

// ---------------------------------------------------------------------------
// The real spawn path — skipped when this machine has no bash
// ---------------------------------------------------------------------------

/// `true` when `getShellConfig` can resolve a shell here. Every test below is a
/// no-op otherwise, so the suite never becomes machine-dependent.
async fn real_bash_available() -> bool {
    match get_shell_config(None).await {
        Ok(_) => true,
        Err(error) => {
            eprintln!("skipping the real-shell tests: {error}");
            false
        }
    }
}

/// End to end through `LocalBashOperations`: stdout is streamed, decoded and
/// returned, and a clean exit carries no `details`.
#[tokio::test]
async fn real_bash_streams_stdout_and_stderr_into_one_result() {
    if !real_bash_available().await {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let result = AgentTool::execute(
        &create_bash_tool_definition(dir.path().to_str().unwrap(), None),
        "call_real_echo",
        json!({ "command": "printf 'out\\n'; printf 'err\\n' 1>&2" }),
        CancellationToken::new(),
        noop_update(),
    )
    .await
    .expect("printf must succeed");

    let text = text_of(&result);
    // Both streams share one callback, so both appear; their relative order is
    // arrival order and is not asserted.
    assert!(text.contains("out\n"), "{text:?}");
    assert!(text.contains("err\n"), "{text:?}");
    assert_eq!(result.details, Value::Null);
}

/// A nonzero exit really does reach `appendStatus` through the local path.
#[tokio::test]
async fn real_bash_reports_a_nonzero_exit_code() {
    if !real_bash_available().await {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let error = AgentTool::execute(
        &create_bash_tool_definition(dir.path().to_str().unwrap(), None),
        "call_real_exit",
        json!({ "command": "printf 'bye\\n'; exit 3" }),
        CancellationToken::new(),
        noop_update(),
    )
    .await
    .expect_err("exit 3 must fail");
    assert_eq!(error.to_string(), "bye\n\n\nCommand exited with code 3");
}

/// The cwd check (`bash.ts:91-94`) — note it runs *after* shell resolution, so it
/// needs a real shell to be reachable at all.
#[tokio::test]
async fn real_bash_rejects_a_missing_working_directory() {
    if !real_bash_available().await {
        return;
    }
    let missing = if cfg!(windows) {
        r"C:\pirust\definitely\not\here"
    } else {
        "/pirust/definitely/not/here"
    };
    let error = AgentTool::execute(
        &create_bash_tool_definition(missing, None),
        "call_real_missing_cwd",
        json!({ "command": "pwd" }),
        CancellationToken::new(),
        noop_update(),
    )
    .await
    .expect_err("a missing cwd must fail");
    assert_eq!(
        error.to_string(),
        format!("Working directory does not exist: {missing}\nCannot execute bash commands.")
    );
}

/// The real timeout + `killProcessTree` path: `sleep 30` must be killed and
/// reported with the raw seconds echoed back.
#[tokio::test]
async fn real_bash_kills_a_command_that_times_out() {
    if !real_bash_available().await {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let error = AgentTool::execute(
        &create_bash_tool_definition(dir.path().to_str().unwrap(), None),
        "call_real_timeout",
        json!({ "command": "sleep 30", "timeout": 0.3 }),
        CancellationToken::new(),
        noop_update(),
    )
    .await
    .expect_err("the command must be killed");
    assert_eq!(error.to_string(), "Command timed out after 0.3 seconds");
}

/// Cancelling mid-flight kills the tree and reports `Command aborted`.
#[tokio::test]
async fn real_bash_aborts_a_running_command() {
    if !real_bash_available().await {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let token = CancellationToken::new();
    let canceller = token.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        canceller.cancel();
    });

    let error = AgentTool::execute(
        &create_bash_tool_definition(dir.path().to_str().unwrap(), None),
        "call_real_abort",
        json!({ "command": "sleep 30" }),
        token,
        noop_update(),
    )
    .await
    .expect_err("the command must be aborted");
    assert_eq!(error.to_string(), "Command aborted");
}
