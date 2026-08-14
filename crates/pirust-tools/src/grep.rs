//! Port of `core/tools/grep.ts` (UI-free half) — the `grep` tool.
//!
//! Shells out to the managed `rg` binary (see [`crate::binaries`]).
//! Gated by `tests/fixtures/pi/tools/{schemas,strings}/grep.json` and the `grep`
//! rows of `exec.corpus.jsonl`.
//!
//! `grep` is a thin, opinionated driver around `rg --json`: it streams ripgrep's
//! JSON Lines, keeps at most [`DEFAULT_LIMIT`] matches, and renders each match as
//! `path:line: text` (`grep.ts:326`) with optional `path-line- text` context rows
//! (`grep.ts:265`). Three independent caps apply: the match limit, the
//! [`DEFAULT_MAX_BYTES`] byte cap, and [`GREP_MAX_LINE_LENGTH`] per line.
//!
//! Ported items:
//!
//! | TS (`grep.ts`) | here |
//! | --- | --- |
//! | `grepSchema` (:24-36) | the `object_schema` in [`create_grep_tool_definition`] |
//! | `DEFAULT_LIMIT` (:39) | [`DEFAULT_LIMIT`] |
//! | `GrepToolDetails` (:41-45) | [`GrepToolDetails`] |
//! | `GrepOperations` (:51-56) | [`GrepOperations`] |
//! | `defaultGrepOperations` (:58-61) | [`LocalGrepOperations`] |
//! | `GrepToolOptions` (:63-66) | [`GrepToolOptions`] (plus one pirust-only seam, [`GrepToolOptions::rg_path`]) |
//! | `createGrepToolDefinition` (:123-381) | [`create_grep_tool_definition`] |
//! | `createGrepTool` (:383-385) | [`create_grep_tool`] |
//! | `formatGrepCall` (:68-86), `formatGrepResult` (:88-121), `renderCall` (:370-374), `renderResult` (:375-379) | **omitted** — TUI only (feat-006/007) |
//!
//! # `operations` does not replace ripgrep
//!
//! Unlike `ls`/`read`, the [`GrepOperations`] seam is *not* the whole side-effect
//! surface: `rg` always runs (`grep.ts:221`), and `operations` only covers the
//! `isDirectory` probe (`grep.ts:182`) and the `readFile` used to fetch context
//! rows (`grep.ts:205`). An SSH-backed `GrepOperations` therefore still searches
//! the *local* filesystem — that is Pi's behaviour, reproduced here.
//!
//! # Four behaviours that look like bugs but are the contract
//!
//! - **A malformed `match` event still consumes match budget.** `matchCount++`
//!   happens unconditionally (`grep.ts:281`), *before* the
//!   `filePath && typeof lineNumber === "number"` guard that decides whether the
//!   row is kept (`grep.ts:285`). So an event that ripgrep emits without a usable
//!   `path.text` / `line_number` shrinks the number of rows the user gets, and can
//!   even trip the match-limit notice on its own. **Not gated by a test**: real
//!   ripgrep always fills both fields, so no corpus row can distinguish the two
//!   placements of `matchCount++`, and manufacturing one would need a stub `rg`
//!   executable replayed through real Pi to capture the oracle bytes.
//! - **The limit check sits at the top of the line handler** (`grep.ts:273`), so
//!   once `matchCount >= effectiveLimit` every remaining buffered line is dropped
//!   without being parsed — including non-`match` events.
//! - **A line that fails `JSON.parse` is silently skipped** (`grep.ts:277-279`):
//!   no error, no budget consumed.
//! - **`limit` *is* clamped**: `Math.max(1, limit ?? DEFAULT_LIMIT)`
//!   (`grep.ts:189`), unlike `ls.ts:125` / `find.ts` which apply `?? DEFAULT` with
//!   no clamp. `limit: 0` therefore behaves as `limit: 1` here.
//!
//! # Environment note (pirust)
//!
//! `ensure_tool(ManagedTool::Rg, …)` looks in `~/.pirust/agent/bin` and then on
//! `PATH` (see [`crate::binaries`]); on a machine where only Pi's `~/.pi/agent/bin`
//! holds `rg.exe` it correctly finds nothing, and this tool then produces
//! [`RG_UNAVAILABLE`]. Adding a `~/.pi` fallback would be a divergence, so instead
//! [`GrepToolOptions::rg_path`] lets a caller (notably `tests/grep_golden.rs`)
//! inject the binary it located itself.

use std::collections::HashMap;
use std::fmt;
use std::process::Stdio;
use std::sync::Arc;

use async_trait::async_trait;
use pirust_agent_core::types::{AgentTool, AgentToolResult, AgentToolUpdateCallback, ToolError};
use pirust_ai::jsnum::{self, js_number};
use pirust_ai::types::{TextContent, UserContent};
use serde::{Deserialize, Serialize, Serializer};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

use crate::binaries::{ensure_tool, BinaryEnv, ManagedTool, SpawnProbe};
use crate::definition::schema::{
    boolean_prop, number_prop, object_schema, optional, required, string_prop,
};
use crate::definition::PirustToolDefinition;
use crate::path_utils;
use crate::truncate::{
    format_size, truncate_head, truncate_line, TruncationOptions, TruncationResult,
    DEFAULT_MAX_BYTES, GREP_MAX_LINE_LENGTH,
};

/// Default maximum number of matches returned (TS `grep.ts:39`).
pub const DEFAULT_LIMIT: f64 = 100.0;

/// `Number.MAX_SAFE_INTEGER`, passed as `maxLines` so only the byte cap is live
/// (TS `grep.ts:335`: "There is no line limit here because the match limit already
/// capped rows").
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// `grep.ts:131` — the `description` template literal with its three
/// substitutions already performed: `${DEFAULT_LIMIT}` → `100`,
/// `${DEFAULT_MAX_BYTES / 1024}` → `50` and `${GREP_MAX_LINE_LENGTH}` → `500`.
///
/// The byte cap appears here as the bare `"50KB"` of an integer division, *not* as
/// [`format_size`]'s `"50.0KB"` — both literals exist in this module (the latter in
/// the truncation notice, `grep.ts:347`) and they are not interchangeable.
/// `description_matches_the_template_literal` pins the substitution.
pub const GREP_DESCRIPTION: &str = "Search file contents for a pattern. Returns matching lines with file paths and line numbers. Respects .gitignore. Output is truncated to 100 matches or 50KB (whichever is hit first). Long lines are truncated to 500 chars.";

/// `grep.ts:132` — `promptSnippet`.
pub const GREP_PROMPT_SNIPPET: &str = "Search file contents for patterns (respects .gitignore)";

/// The message Pi rejects with when `ensureTool("rg")` yields nothing
/// (`grep.ts:174`). Note that [`ensure_tool`] never *errors* for a missing binary,
/// exactly like Pi's `ensureTool`; producing this text is the tool's job.
pub const RG_UNAVAILABLE: &str = "ripgrep (rg) is not available and could not be downloaded";

/// The message of the `Error` Pi rejects with on abort, both from the up-front
/// `signal.aborted` check (`grep.ts:159`) and from the `close` handler after the
/// `abort` listener killed the child (`grep.ts:301`).
const ABORT_MESSAGE: &str = "Operation aborted";

/// Output when ripgrep reported no matches at all (TS `grep.ts:311`).
const NO_MATCHES: &str = "No matches found";

/// Row emitted in place of a context block whose file could not be read
/// (TS `grep.ts:253`).
const UNABLE_TO_READ: &str = "(unable to read file)";

// ===========================================================================
// Operations seam
// ===========================================================================

/// Pluggable operations for the `grep` tool (TS `GrepOperations`, `grep.ts:51-56`).
///
/// Override to delegate the two *filesystem* touches to a remote system (for
/// example SSH) — but see the module docs: ripgrep still runs locally.
///
/// Both methods return [`ToolError`] because Pi only ever uses the rejection as a
/// boolean: `isDirectory`'s failure becomes `Path not found: ${searchPath}`
/// (`grep.ts:184`) and `readFile`'s becomes an empty line list, i.e. the
/// [`UNABLE_TO_READ`] row (`grep.ts:207-209`). The message itself is discarded in
/// both cases.
#[async_trait]
pub trait GrepOperations: Send + Sync {
    /// Check if path is a directory; `Err` if the path does not exist
    /// (TS `grep.ts:53`).
    async fn is_directory(&self, absolute_path: &str) -> Result<bool, ToolError>;

    /// Read file contents for context lines (TS `grep.ts:55`).
    async fn read_file(&self, absolute_path: &str) -> Result<String, ToolError>;
}

/// `defaultGrepOperations` (TS `grep.ts:58-61`): `fs/promises.stat` plus
/// `fs/promises.readFile(p, "utf-8")`.
#[derive(Debug, Clone, Copy, Default)]
pub struct LocalGrepOperations;

#[async_trait]
impl GrepOperations for LocalGrepOperations {
    /// TS `(await fsStat(p)).isDirectory()` (`grep.ts:59`) — Node's `stat`, which
    /// follows symlinks, as does `tokio::fs::metadata`.
    async fn is_directory(&self, absolute_path: &str) -> Result<bool, ToolError> {
        Ok(tokio::fs::metadata(absolute_path).await?.is_dir())
    }

    /// TS `fsReadFile(p, "utf-8")` (`grep.ts:60`).
    ///
    /// Node's `"utf-8"` decode is lossy: invalid byte sequences become U+FFFD
    /// rather than failing, so this reads bytes and converts with
    /// [`String::from_utf8_lossy`] instead of `fs::read_to_string` (which would
    /// error and turn the block into an [`UNABLE_TO_READ`] row Pi never emits).
    async fn read_file(&self, absolute_path: &str) -> Result<String, ToolError> {
        let bytes = tokio::fs::read(absolute_path).await?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }
}

/// `GrepToolOptions` (TS `grep.ts:63-66`), plus one pirust-only field.
#[derive(Clone, Default)]
pub struct GrepToolOptions {
    /// Custom operations for grep. `None` → [`LocalGrepOperations`]
    /// (TS `grep.ts:65`, resolved per call at `grep.ts:179`).
    pub operations: Option<Arc<dyn GrepOperations>>,
    /// **Not in Pi.** Overrides the `ensureTool("rg", true)` lookup
    /// (`grep.ts:172`) with an explicit path or command name.
    ///
    /// Pi has no such option because on a Pi install the binary is always in
    /// `~/.pi/agent/bin` (or gets downloaded). pirust's managed directory is
    /// `~/.pirust/agent/bin` and the downloader is deferred to feat-005, so
    /// without this seam every `grep` call on a fresh machine would end at
    /// [`RG_UNAVAILABLE`] and the golden corpus could not be replayed. When
    /// `Some`, [`ensure_tool`] is not consulted at all; when `None`, resolution is
    /// byte-for-byte Pi's.
    pub rg_path: Option<String>,
}

impl fmt::Debug for GrepToolOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GrepToolOptions")
            .field("operations", &self.operations.as_ref().map(|_| "<dyn>"))
            .field("rg_path", &self.rg_path)
            .finish()
    }
}

// ===========================================================================
// Details
// ===========================================================================

/// `GrepToolDetails` (TS `grep.ts:41-45`) — the `details` payload, persisted into
/// the session JSONL.
///
/// **Field order is deliberately not the TS interface's.** The interface declares
/// `truncation`, `matchLimitReached`, `linesTruncated` (`grep.ts:42-44`), but the
/// object is built empty and filled `matchLimitReached` first (`grep.ts:344`),
/// `truncation` second (`grep.ts:348`), `linesTruncated` third (`grep.ts:354`) —
/// and `JSON.stringify` follows *insertion* order. Since this value is compared
/// byte-for-byte against captured Pi output, the Rust declaration order matches
/// the insertion order instead. (No captured row sets two of the three at once, so
/// the corpus cannot tell; `ls.rs` makes the same choice for the same reason.)
///
/// Every field is omitted when absent and the whole object collapses to `null`
/// when none is set (`grep.ts:360`) — see [`GrepToolDetails::is_empty`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrepToolDetails {
    /// The `effectiveLimit` that was reached, set only when the match cap cut the
    /// output (TS `grep.ts:344`). `f64` because `limit` is a schema `number`.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_js_number"
    )]
    pub match_limit_reached: Option<f64>,
    /// The byte-truncation result, set only when the byte cap cut the output
    /// (TS `grep.ts:348`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncation: Option<TruncationResult>,
    /// Set (to `true`, never `false`) when at least one row was cut to
    /// [`GREP_MAX_LINE_LENGTH`] (TS `grep.ts:354`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lines_truncated: Option<bool>,
}

impl GrepToolDetails {
    /// TS `Object.keys(details).length === 0` (`grep.ts:360`).
    pub fn is_empty(&self) -> bool {
        self.match_limit_reached.is_none()
            && self.truncation.is_none()
            && self.lines_truncated.is_none()
    }

    /// TS `details` as resolved into the tool result: the object, or `undefined`
    /// (→ JSON `null`) when it never got a key.
    fn into_value(self) -> Value {
        if self.is_empty() {
            Value::Null
        } else {
            serde_json::to_value(self).expect("GrepToolDetails is always representable as JSON")
        }
    }
}

/// Emit an `f64` the way `JSON.stringify` would — `3`, not `3.0` — by delegating to
/// the landed [`js_number`] serializer. `skip_serializing_if` guarantees `Some`.
fn serialize_js_number<S: Serializer>(
    value: &Option<f64>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    let value = value.as_ref().expect("skip_serializing_if guarantees Some");
    jsnum::serialize_f64(value, serializer)
}

// ===========================================================================
// Tool definition
// ===========================================================================

/// `createGrepToolDefinition` (TS `grep.ts:123-381`), minus `renderCall` /
/// `renderResult`.
///
/// `options.operations` is captured, not resolved: Pi re-evaluates
/// `customOps ?? defaultGrepOperations` on every call (`grep.ts:179`), unlike
/// `ls`, which resolves once at definition time.
pub fn create_grep_tool_definition(
    cwd: impl Into<String>,
    options: GrepToolOptions,
) -> PirustToolDefinition {
    let cwd: Arc<str> = Arc::from(cwd.into());
    let custom_ops = options.operations;
    let rg_path: Option<Arc<str>> = options.rg_path.map(Arc::from);

    PirustToolDefinition::new(
        "grep",
        "grep",
        GREP_DESCRIPTION,
        // TS `grepSchema` (`grep.ts:24-36`): `pattern` required, the other six
        // `Type.Optional`, in declaration order.
        object_schema([
            required(
                "pattern",
                string_prop("Search pattern (regex or literal string)"),
            ),
            optional(
                "path",
                string_prop("Directory or file to search (default: current directory)"),
            ),
            optional(
                "glob",
                string_prop("Filter files by glob pattern, e.g. '*.ts' or '**/*.spec.ts'"),
            ),
            optional(
                "ignoreCase",
                boolean_prop("Case-insensitive search (default: false)"),
            ),
            optional(
                "literal",
                boolean_prop("Treat pattern as literal string instead of regex (default: false)"),
            ),
            optional(
                "context",
                number_prop("Number of lines to show before and after each match (default: 0)"),
            ),
            optional(
                "limit",
                number_prop("Maximum number of matches to return (default: 100)"),
            ),
        ]),
        move |_tool_call_id: String,
              args: Value,
              token: CancellationToken,
              _on_update: AgentToolUpdateCallback| {
            let cwd = Arc::clone(&cwd);
            let custom_ops = custom_ops.clone();
            let rg_path = rg_path.clone();
            async move {
                // TS `grep.ts:179`: `customOps ?? defaultGrepOperations`.
                let ops: Arc<dyn GrepOperations> =
                    custom_ops.unwrap_or_else(|| Arc::new(LocalGrepOperations));
                execute_grep(&cwd, ops.as_ref(), rg_path.as_deref(), &args, &token).await
            }
        },
    )
    .with_prompt_snippet(GREP_PROMPT_SNIPPET)
}

/// `createGrepTool` (TS `grep.ts:383-385`).
///
/// `wrapToolDefinition` has no separate type here — `PirustToolDefinition` *is*
/// the [`AgentTool`] — so this is [`create_grep_tool_definition`] behind the trait
/// object.
pub fn create_grep_tool(cwd: impl Into<String>, options: GrepToolOptions) -> Arc<dyn AgentTool> {
    Arc::new(create_grep_tool_definition(cwd, options))
}

// ===========================================================================
// execute
// ===========================================================================

/// One kept `match` event (TS `matches`, `grep.ts:271`).
#[derive(Debug, Clone)]
struct GrepMatch {
    /// `event.data.path.text` (`grep.ts:282`).
    file_path: String,
    /// `event.data.line_number` (`grep.ts:283`); `f64` because the guard is
    /// `typeof lineNumber === "number"`, not an integer check.
    line_number: f64,
    /// `event.data.lines.text` (`grep.ts:284`) — `None` is TS `undefined`, which
    /// forces the `formatBlock` path even at `context: 0` (`grep.ts:318`).
    line_text: Option<String>,
}

/// TS `execute` (`grep.ts:134-369`).
///
/// Pi hand-rolls a `Promise` with a `settled` latch and an `abort` listener that is
/// removed by `cleanup()` at the top of the `close` handler (`grep.ts:231-234`,
/// `:298`). That shape matters: an abort arriving *after* ripgrep closed — i.e.
/// during the formatting pass — is ignored, because the listener is already gone.
/// So cancellation is observed only while streaming (see [`grep_body`]) and never
/// wrapped around the whole body.
async fn execute_grep(
    cwd: &str,
    ops: &dyn GrepOperations,
    rg_path_override: Option<&str>,
    args: &Value,
    token: &CancellationToken,
) -> Result<AgentToolResult, ToolError> {
    // TS `grep.ts:158-161`: the synchronous pre-check, before `ensureTool`.
    if token.is_cancelled() {
        return Err(ABORT_MESSAGE.into());
    }
    grep_body(cwd, ops, rg_path_override, args, token).await
}

/// The `async` IIFE inside Pi's promise (TS `grep.ts:170-367`).
async fn grep_body(
    cwd: &str,
    ops: &dyn GrepOperations,
    rg_path_override: Option<&str>,
    args: &Value,
    token: &CancellationToken,
) -> Result<AgentToolResult, ToolError> {
    // TS `grep.ts:172-176`. `ensure_tool` resolves to `None` for "missing", never
    // an error; `Err` here is only the unresolvable-home-dir case, which Pi
    // surfaces as an `os.homedir()` throw through the same outer `catch`.
    let rg_path = match rg_path_override {
        Some(path) => path.to_string(),
        None => {
            let env = BinaryEnv::from_process_env();
            match ensure_tool(ManagedTool::Rg, &env, &SpawnProbe).await? {
                Some(path) => path,
                None => return Err(RG_UNAVAILABLE.into()),
            }
        }
    };

    // TS `grep.ts:178`: `searchDir || "."`, so `undefined`, `null` *and* `""` all
    // fall back to the cwd — `??` would keep `""`.
    let search_dir = args
        .get("path")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
        .unwrap_or(".");
    let search_path = path_utils::resolve_to_cwd(search_dir, cwd)?;

    // TS `grep.ts:180-186`: any rejection becomes `Path not found`, message
    // discarded.
    let is_directory = ops
        .is_directory(&search_path)
        .await
        .map_err(|_| -> ToolError { format!("Path not found: {search_path}").into() })?;

    // TS `grep.ts:188`: `context && context > 0 ? context : 0` — `0` and `NaN` are
    // falsy, and a *negative* context is normalized to `0` rather than kept.
    let context_value = args
        .get("context")
        .and_then(Value::as_f64)
        .filter(|context| *context > 0.0)
        .unwrap_or(0.0);
    // TS `grep.ts:189`: `Math.max(1, limit ?? DEFAULT_LIMIT)` — grep *does* clamp.
    let effective_limit = js_max_1(args.get("limit").and_then(Value::as_f64));

    // TS `grep.ts:215-219`. Order is load-bearing and the flag set is exactly
    // this: no `-C`/context flag (context rows are read from the file by the tool
    // itself), no `--max-count` (the limit is enforced by killing the child), no
    // `--no-ignore` (so rg's default `.gitignore` handling applies, which is what
    // the description promises), no `--smart-case`, no `--type`.
    let mut rg_args: Vec<String> = ["--json", "--line-number", "--color=never", "--hidden"]
        .iter()
        .map(|flag| (*flag).to_string())
        .collect();
    if js_truthy(args.get("ignoreCase")) {
        rg_args.push("--ignore-case".to_string());
    }
    if js_truthy(args.get("literal")) {
        rg_args.push("--fixed-strings".to_string());
    }
    // `if (glob)` — a truthy non-string `glob` would make Node's `spawn` throw
    // `ERR_INVALID_ARG_TYPE`; schema validation makes that unreachable, so a
    // non-string is treated as absent here.
    if let Some(glob) = args
        .get("glob")
        .and_then(Value::as_str)
        .filter(|glob| !glob.is_empty())
    {
        rg_args.push("--glob".to_string());
        rg_args.push(glob.to_string());
    }
    rg_args.push("--".to_string());
    // Likewise, a missing `pattern` cannot survive schema validation; Pi would
    // throw from `spawn` rather than search for the empty pattern.
    rg_args.push(
        args.get("pattern")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
    );
    rg_args.push(search_path.clone());

    // TS `grep.ts:221`: `stdio: ["ignore", "pipe", "pipe"]`.
    //
    // Pi surfaces a spawn failure through `child.on("error")` as
    // `Failed to run ripgrep: ${error.message}` (`grep.ts:296`). Node reports the
    // failure asynchronously and its message reads `spawn rg ENOENT`; Rust fails
    // synchronously here with the OS text, so the prefix matches but the tail does
    // not. No captured row exercises it.
    let mut child = Command::new(&rg_path)
        .args(&rg_args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Safety net only: every path below reaches `wait()`.
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| -> ToolError { format!("Failed to run ripgrep: {error}").into() })?;

    let stdout = child.stdout.take().expect("stdout was piped");
    let mut stderr = child.stderr.take().expect("stderr was piped");
    // TS `grep.ts:246-248`: `stderr += chunk.toString()`, i.e. a lossy UTF-8
    // decode, accumulated concurrently with stdout.
    let stderr_task = tokio::spawn(async move {
        let mut buffer = Vec::new();
        let _ = stderr.read_to_end(&mut buffer).await;
        String::from_utf8_lossy(&buffer).into_owned()
    });

    // TS `createInterface({ input: child.stdout })` (`grep.ts:222`). Node's
    // readline splits on `\r?\n`; `tokio`'s `lines()` splits on `\n` and strips a
    // trailing `\r`. ripgrep's JSON Lines use `\n` only.
    let mut stdout_lines = BufReader::new(stdout).lines();

    let mut match_count = 0.0_f64;
    let mut match_limit_reached = false;
    let mut lines_truncated = false;
    let mut aborted = false;
    let mut killed = false;
    let mut killed_due_to_limit = false;
    let mut matches: Vec<GrepMatch> = Vec::new();

    loop {
        // TS `grep.ts:241-245`: the `abort` listener sets `aborted` and kills the
        // child, then lets `close` do the rejecting. `biased` polls the stream
        // first so an already-buffered line is never lost to a late abort, and the
        // guard stops re-selecting on an already-cancelled token.
        let next = if aborted {
            stdout_lines.next_line().await
        } else {
            tokio::select! {
                biased;
                line = stdout_lines.next_line() => line,
                () = token.cancelled() => {
                    aborted = true;
                    // TS `stopChild()` (`grep.ts:235-240`) with `dueToLimit` false.
                    if !killed {
                        killed = true;
                        killed_due_to_limit = false;
                        let _ = child.start_kill();
                    }
                    continue;
                }
            }
        };
        let line = match next {
            Ok(Some(line)) => line,
            // EOF, or the pipe broke because we killed the child: readline would
            // simply stop emitting `line` events and let `close` fire.
            Ok(None) | Err(_) => break,
        };

        // TS `grep.ts:273`. `str::trim` is not exactly JS `String.prototype.trim`
        // (it keeps U+FEFF, which JS drops), which cannot matter for JSON Lines.
        if line.trim().is_empty() || match_count >= effective_limit {
            continue;
        }
        // TS `grep.ts:274-279`: an unparseable line is silently skipped.
        let Ok(event) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if event.get("type").and_then(Value::as_str) != Some("match") {
            continue;
        }

        // TS `grep.ts:281`: unconditional, so a malformed event still costs budget.
        match_count += 1.0;
        let data = event.get("data");
        let file_path = data
            .and_then(|data| data.get("path"))
            .and_then(|path| path.get("text"))
            .and_then(Value::as_str)
            // `if (filePath && …)`: the empty string is falsy.
            .filter(|path| !path.is_empty());
        let line_number = data
            .and_then(|data| data.get("line_number"))
            .and_then(Value::as_f64);
        let line_text = data
            .and_then(|data| data.get("lines"))
            .and_then(|lines| lines.get("text"))
            .and_then(Value::as_str);
        // TS `grep.ts:285-286`.
        if let (Some(file_path), Some(line_number)) = (file_path, line_number) {
            matches.push(GrepMatch {
                file_path: file_path.to_string(),
                line_number,
                line_text: line_text.map(str::to_string),
            });
        }
        // TS `grep.ts:287-290`: `stopChild(true)`.
        if match_count >= effective_limit {
            match_limit_reached = true;
            if !killed {
                killed = true;
                killed_due_to_limit = true;
                let _ = child.start_kill();
            }
        }
    }

    // TS `child.on("close")` (`grep.ts:298`), which Node fires once the process
    // exited *and* both pipes closed.
    let status = child.wait().await;
    let stderr_text = stderr_task.await.unwrap_or_default();

    // TS `grep.ts:300-303`.
    if aborted {
        return Err(ABORT_MESSAGE.into());
    }
    // TS `grep.ts:304-308`. `code` is `null` when the child died from a signal,
    // and `null !== 0 && null !== 1`, so that too is an error — unless *we* killed
    // it for the match limit.
    let code = status
        .as_ref()
        .ok()
        .and_then(std::process::ExitStatus::code);
    if !killed_due_to_limit && code != Some(0) && code != Some(1) {
        let trimmed = stderr_text.trim();
        return Err(if trimmed.is_empty() {
            // `${code}` on `null` interpolates the four characters `null`.
            let code = code.map_or_else(|| "null".to_string(), |code| code.to_string());
            format!("ripgrep exited with code {code}")
        } else {
            trimmed.to_string()
        }
        .into());
    }
    // TS `grep.ts:309-314`: `details` stays `undefined` even if a malformed event
    // pushed `matchCount` — but then `matchCount` is not 0, so this is exactly
    // "ripgrep emitted no match event at all".
    if match_count == 0.0 {
        return Ok(text_result(NO_MATCHES, Value::Null));
    }

    // TS `grep.ts:316-331`: formatting runs *after* streaming so a custom async
    // `readFile()` can be awaited.
    let mut file_cache: HashMap<String, Vec<String>> = HashMap::new();
    let mut output_lines: Vec<String> = Vec::new();
    for grep_match in &matches {
        let relative_path = format_path(&search_path, &grep_match.file_path, is_directory);
        match (context_value == 0.0, grep_match.line_text.as_deref()) {
            // TS `grep.ts:318-326`.
            (true, Some(line_text)) => {
                let sanitized = sanitize_match_line(line_text);
                let cut = truncate_line(&sanitized, None);
                if cut.was_truncated {
                    lines_truncated = true;
                }
                output_lines.push(format!(
                    "{relative_path}:{}: {}",
                    js_number(grep_match.line_number),
                    cut.text
                ));
            }
            // TS `grep.ts:328-329` → `formatBlock` (`grep.ts:250-268`).
            _ => {
                let lines = file_lines(ops, &mut file_cache, &grep_match.file_path).await;
                if lines.is_empty() {
                    output_lines.push(format!(
                        "{relative_path}:{}: {UNABLE_TO_READ}",
                        js_number(grep_match.line_number)
                    ));
                    continue;
                }
                let line_number = grep_match.line_number;
                let (start, end) = if context_value > 0.0 {
                    (
                        f64::max(1.0, line_number - context_value),
                        f64::min(lines.len() as f64, line_number + context_value),
                    )
                } else {
                    (line_number, line_number)
                };
                let mut current = start;
                while current <= end {
                    // TS `lines[current - 1] ?? ""` (`grep.ts:258`). A fractional
                    // `context` makes `current` fractional too, and JS array
                    // indexing by a non-integer always yields `undefined`.
                    let line_text = js_index(lines, current - 1.0);
                    // Only `\r` is stripped here: `file_lines` already normalized
                    // the line endings.
                    let sanitized = line_text.replace('\r', "");
                    let cut = truncate_line(&sanitized, None);
                    if cut.was_truncated {
                        lines_truncated = true;
                    }
                    let number = js_number(current);
                    output_lines.push(if current == line_number {
                        format!("{relative_path}:{number}: {}", cut.text)
                    } else {
                        // The `-N-` separator is what tells a context row from a
                        // match row; `:N:` here would be a silent contract break.
                        format!("{relative_path}-{number}- {}", cut.text)
                    });
                    current += 1.0;
                }
            }
        }
    }

    let raw_output = output_lines.join("\n");
    // TS `grep.ts:335`: byte cap only.
    let truncation = truncate_head(
        &raw_output,
        TruncationOptions {
            max_lines: Some(MAX_SAFE_INTEGER),
            max_bytes: None,
        },
    );
    let mut output = truncation.content.clone();
    let mut details = GrepToolDetails::default();

    // TS `grep.ts:339-355`.
    let mut notices: Vec<String> = Vec::new();
    if match_limit_reached {
        notices.push(match_limit_notice(effective_limit));
        details.match_limit_reached = Some(effective_limit);
    }
    if truncation.truncated {
        notices.push(byte_limit_notice());
        details.truncation = Some(truncation);
    }
    if lines_truncated {
        notices.push(lines_truncated_notice());
        details.lines_truncated = Some(true);
    }
    // TS `grep.ts:356`.
    if !notices.is_empty() {
        output += &format!("\n\n[{}]", notices.join(". "));
    }

    Ok(text_result(&output, details.into_value()))
}

/// TS `grep.ts:341-343` — the match-limit notice. Gated end-to-end by the
/// `limit: 3` corpus row.
fn match_limit_notice(effective_limit: f64) -> String {
    format!(
        "{} matches limit reached. Use limit={} for more, or refine pattern",
        js_number(effective_limit),
        js_number(effective_limit * 2.0)
    )
}

/// TS `grep.ts:347` — the byte-limit notice.
///
/// Built with [`format_size`], so it reads `50.0KB`, **not** the `50KB` that
/// [`GREP_DESCRIPTION`]'s integer division produces. This is a named function
/// purely so a unit test can pin it: no captured `grep` corpus row reaches the byte
/// cap (the largest is three rows long), so the call site would otherwise be
/// ungated. `formatSize(51200) === "50.0KB"` is itself real-Pi output — see
/// `tests/fixtures/pi/tools/truncate.cases.jsonl`.
fn byte_limit_notice() -> String {
    format!("{} limit reached", format_size(DEFAULT_MAX_BYTES))
}

/// TS `grep.ts:351-353` — the long-line notice. Gated end-to-end by the
/// `src/long.txt` corpus rows.
fn lines_truncated_notice() -> String {
    format!("Some lines truncated to {GREP_MAX_LINE_LENGTH} chars. Use read tool to see full lines")
}

/// TS `{ content: [{ type: "text", text }], details }` (`grep.ts:311`,
/// `grep.ts:358-361`).
fn text_result(text: &str, details: Value) -> AgentToolResult {
    AgentToolResult {
        content: vec![UserContent::Text(TextContent::new(text))],
        details,
        added_tool_names: None,
        terminate: None,
    }
}

/// TS `Math.max(1, limit ?? DEFAULT_LIMIT)` (`grep.ts:189`).
///
/// `Math.max` propagates `NaN`, where Rust's `f64::max` would return the other
/// operand — reachable only if the schema ever admitted a non-number, and a `NaN`
/// limit makes every `matchCount >= effectiveLimit` comparison false, i.e. no
/// limit at all.
fn js_max_1(limit: Option<f64>) -> f64 {
    let limit = limit.unwrap_or(DEFAULT_LIMIT);
    if limit.is_nan() {
        f64::NAN
    } else if limit > 1.0 {
        limit
    } else {
        1.0
    }
}

/// JS truthiness, for `if (ignoreCase)` / `if (literal)` (`grep.ts:216-217`). An
/// absent argument is `undefined`, i.e. falsy.
fn js_truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(flag)) => *flag,
        Some(Value::Number(number)) => number
            .as_f64()
            .is_some_and(|number| number != 0.0 && !number.is_nan()),
        Some(Value::String(text)) => !text.is_empty(),
        // Every object and array — including `[]` and `{}` — is truthy.
        Some(Value::Array(_) | Value::Object(_)) => true,
    }
}

/// JS `array[index] ?? ""` where `index` is a `number`: a non-integral or
/// out-of-range index yields `undefined`, hence `""`.
fn js_index(lines: &[String], index: f64) -> &str {
    if index.fract() != 0.0 || index < 0.0 {
        return "";
    }
    lines.get(index as usize).map_or("", String::as_str)
}

/// TS `grep.ts:320-323` — the match-line sanitizer:
/// `.replace(/\r\n/g, "\n").replace(/\r/g, "").replace(/\n$/, "")`.
///
/// ripgrep's `data.lines.text` includes the line's own terminator, which is what
/// the final (non-global, `$`-anchored) replace strips — **one** trailing `\n`, not
/// all of them.
fn sanitize_match_line(text: &str) -> String {
    let mut sanitized = text.replace("\r\n", "\n").replace('\r', "");
    if sanitized.ends_with('\n') {
        sanitized.pop();
    }
    sanitized
}

/// TS `getFileLines` (`grep.ts:201-213`): read once per file, normalize `\r\n` and
/// then bare `\r` to `\n`, split on `\n`, and cache — *including* the empty vector
/// a read failure produces, so a broken file is not re-read per match.
///
/// Note the second replacement differs from [`sanitize_match_line`]'s: here a lone
/// `\r` becomes a newline (old-Mac line endings), there it is deleted.
async fn file_lines<'cache>(
    ops: &dyn GrepOperations,
    cache: &'cache mut HashMap<String, Vec<String>>,
    file_path: &str,
) -> &'cache [String] {
    if !cache.contains_key(file_path) {
        let lines = match ops.read_file(file_path).await {
            Ok(content) => content
                .replace("\r\n", "\n")
                .replace('\r', "\n")
                .split('\n')
                .map(str::to_string)
                .collect(),
            // TS `grep.ts:207-209`: any failure becomes the empty list, which the
            // caller renders as `(unable to read file)`.
            Err(_) => Vec::new(),
        };
        cache.insert(file_path.to_string(), lines);
    }
    &cache[file_path]
}

/// TS `formatPath` (`grep.ts:190-198`).
///
/// When the search path is a directory, a match is reported relative to it (with
/// `\` rewritten to `/`, so output is stable across platforms); when it is a single
/// file — or the relative path escapes the search root — the bare basename is used.
fn format_path(search_path: &str, file_path: &str, is_directory: bool) -> String {
    if is_directory {
        let relative = node_relative(search_path, file_path);
        if !relative.is_empty() && !relative.starts_with("..") {
            return relative.replace('\\', "/");
        }
    }
    node_basename(file_path)
}

// ===========================================================================
// node:path transcriptions
// ===========================================================================
//
// `crate::path_utils` ports `path.resolve` / `path.normalize` / `path.join` but
// keeps them private to that module, and `grep.ts` needs two more of the family
// (`path.relative` at `grep.ts:192` and `path.basename` at `grep.ts:197`). They are
// transcribed here from `node:path` rather than reimplemented; the unit tests pin
// them against output taken from a real `node`.
//
// One documented simplification in both `relative` flavours: Node runs each
// argument through `path.resolve` first, which this does not. Both inputs are
// already resolved in this tool — `search_path` comes out of
// `path_utils::resolve_to_cwd`, and `file_path` is `search_path` plus the
// components ripgrep walked — so the call would be a no-op.

/// `path.relative(from, to)` for the running platform.
fn node_relative(from: &str, to: &str) -> String {
    match path_utils::Platform::current() {
        path_utils::Platform::Win32 => win32_relative(from, to),
        path_utils::Platform::Posix => posix_relative(from, to),
    }
}

/// `path.win32.relative` (Node `lib/path.js`).
///
/// Comparison is case-insensitive, as on the real filesystem. **Bounded
/// divergence**: Node lowercases with full-Unicode `toLowerCase()`, this uses
/// ASCII-only case folding. Node then indexes the *original* strings with offsets
/// computed from the lowercased ones, so its own behaviour is undefined for the
/// characters where the two differ in length (`İ`); ASCII folding cannot desync.
/// The observable difference is confined to two paths that are equal except for the
/// case of a non-ASCII letter, where this returns an absolute path (`to`) instead
/// of a relative one.
fn win32_relative(from_orig: &str, to_orig: &str) -> String {
    if from_orig == to_orig {
        return String::new();
    }
    let from = from_orig.to_ascii_lowercase();
    let to = to_orig.to_ascii_lowercase();
    if from == to {
        return String::new();
    }
    let from_bytes = from.as_bytes();
    let to_bytes = to.as_bytes();

    // Trim leading, then trailing, backslashes (the latter matters for UNC paths).
    let mut from_start = 0;
    while from_start < from_bytes.len() && from_bytes[from_start] == b'\\' {
        from_start += 1;
    }
    let mut from_end = from_bytes.len();
    while from_end > from_start + 1 && from_bytes[from_end - 1] == b'\\' {
        from_end -= 1;
    }
    let from_len = from_end - from_start;

    let mut to_start = 0;
    while to_start < to_bytes.len() && to_bytes[to_start] == b'\\' {
        to_start += 1;
    }
    let mut to_end = to_bytes.len();
    while to_end > to_start + 1 && to_bytes[to_end - 1] == b'\\' {
        to_end -= 1;
    }
    let to_len = to_end - to_start;

    let length = from_len.min(to_len);
    // Node's `-1` sentinel is load-bearing (it distinguishes "no common component"
    // from "the root is common"), so it is kept as a signed value.
    let mut last_common_sep: i64 = -1;
    let mut i = 0;
    while i < length {
        let byte = from_bytes[from_start + i];
        if byte != to_bytes[to_start + i] {
            break;
        }
        if byte == b'\\' {
            last_common_sep = i as i64;
        }
        i += 1;
    }
    if i != length {
        if last_common_sep == -1 {
            return to_orig.to_string();
        }
    } else {
        if to_len > length {
            if to_bytes[to_start + i] == b'\\' {
                // `from` is a direct ancestor of `to`.
                return byte_slice(to_orig, to_start + i + 1, to_orig.len()).to_string();
            }
            if i == 2 {
                // `from` is the drive root (`C:`).
                return byte_slice(to_orig, to_start + i, to_orig.len()).to_string();
            }
        }
        if from_len > length {
            if from_bytes[from_start + i] == b'\\' {
                last_common_sep = i as i64;
            } else if i == 2 {
                last_common_sep = 3;
            }
        }
        if last_common_sep == -1 {
            last_common_sep = 0;
        }
    }

    let mut out = String::new();
    let mut index = from_start + last_common_sep as usize + 1;
    while index <= from_end {
        if index == from_end || from_bytes[index] == b'\\' {
            out.push_str(if out.is_empty() { ".." } else { "\\.." });
        }
        index += 1;
    }
    let mut tail_start = to_start + last_common_sep as usize;
    if !out.is_empty() {
        out.push_str(byte_slice(to_orig, tail_start, to_end));
        return out;
    }
    if to_bytes.get(tail_start) == Some(&b'\\') {
        tail_start += 1;
    }
    byte_slice(to_orig, tail_start, to_end).to_string()
}

/// `path.posix.relative` (Node `lib/path.js`). Both arguments are absolute after
/// Node's `resolve`, which is why `fromStart` / `toStart` are the constant `1`.
fn posix_relative(from: &str, to: &str) -> String {
    if from == to {
        return String::new();
    }
    let from_bytes = from.as_bytes();
    let to_bytes = to.as_bytes();

    let from_start = 1;
    let from_end = from_bytes.len();
    let from_len = from_end.saturating_sub(from_start);
    let to_start = 1;
    let to_len = to_bytes.len().saturating_sub(to_start);

    let length = from_len.min(to_len);
    let mut last_common_sep: i64 = -1;
    let mut i = 0;
    while i < length {
        let byte = from_bytes[from_start + i];
        if byte != to_bytes[to_start + i] {
            break;
        }
        if byte == b'/' {
            last_common_sep = i as i64;
        }
        i += 1;
    }
    // Note the shape: unlike win32, the `fromLen > length` arm is an `else if` on
    // `toLen > length`, and there is no drive-letter case.
    if i == length {
        if to_len > length {
            if to_bytes[to_start + i] == b'/' {
                return byte_slice(to, to_start + i + 1, to_bytes.len()).to_string();
            }
            if i == 0 {
                return byte_slice(to, to_start + i, to_bytes.len()).to_string();
            }
        } else if from_len > length {
            if from_bytes[from_start + i] == b'/' {
                last_common_sep = i as i64;
            } else if i == 0 {
                last_common_sep = 0;
            }
        }
    }

    let mut out = String::new();
    let mut index = from_start + (last_common_sep + 1) as usize;
    while index <= from_end {
        if index == from_end || from_bytes[index] == b'/' {
            out.push_str(if out.is_empty() { ".." } else { "/.." });
        }
        index += 1;
    }
    // `to.slice(toStart + lastCommonSep)`, where `lastCommonSep` may still be the
    // `-1` sentinel — hence the signed arithmetic: `1 + -1` is `slice(0)`, i.e. the
    // whole (absolute) path, which is what makes `relative("/a", "/b")` = `"../b"`.
    let tail_start = (to_start as i64 + last_common_sep).max(0) as usize;
    out.push_str(byte_slice(to, tail_start, to_bytes.len()));
    out
}

/// `path.basename(p)` for the running platform (no `ext` argument: `grep.ts:197`
/// passes none).
fn node_basename(path: &str) -> String {
    let win32 = path_utils::Platform::current() == path_utils::Platform::Win32;
    let bytes = path.as_bytes();
    let is_separator = |byte: u8| byte == b'/' || (win32 && byte == b'\\');

    // win32 skips a `C:` prefix so that the separator after it is not mistaken for
    // a trailing one.
    let mut start =
        if win32 && bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
            2
        } else {
            0
        };
    let mut end: Option<usize> = None;
    let mut matched_slash = true;
    let mut index = bytes.len();
    while index > start {
        index -= 1;
        if is_separator(bytes[index]) {
            if !matched_slash {
                start = index + 1;
                break;
            }
        } else if end.is_none() {
            matched_slash = false;
            end = Some(index + 1);
        }
    }
    match end {
        None => String::new(),
        Some(end) => byte_slice(path, start, end).to_string(),
    }
}

/// `String.prototype.slice` on byte offsets.
///
/// Every offset the two `relative` transcriptions and [`node_basename`] produce
/// sits at `0`, at a length, or immediately after an ASCII separator — always a
/// UTF-8 boundary — so `get` never falls back in practice; it is used instead of
/// `&s[a..b]` so that a hypothetical mid-character offset degrades instead of
/// panicking inside a tool call.
fn byte_slice(text: &str, start: usize, end: usize) -> &str {
    text.get(start..end).unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// [`GREP_DESCRIPTION`] is a flattened template literal; this reproduces the
    /// interpolation so the three constants it depends on cannot drift.
    #[test]
    fn description_matches_the_template_literal() {
        let composed = format!(
            "Search file contents for a pattern. Returns matching lines with file paths and line \
             numbers. Respects .gitignore. Output is truncated to {} matches or {}KB (whichever is \
             hit first). Long lines are truncated to {GREP_MAX_LINE_LENGTH} chars.",
            js_number(DEFAULT_LIMIT),
            DEFAULT_MAX_BYTES / 1024
        );
        assert_eq!(GREP_DESCRIPTION, composed);
        // The description's `50KB` and the notice's `50.0KB` are different strings
        // and must not be conflated.
        assert_eq!(format_size(DEFAULT_MAX_BYTES), "50.0KB");
        assert!(!GREP_DESCRIPTION.contains("50.0KB"));
    }

    /// The three truncation notices, verbatim.
    ///
    /// The match-limit and long-line texts are also gated end-to-end by corpus
    /// rows 11-13; the byte-limit one is not reachable from any captured row, so
    /// this is the only thing standing between it and a silent `50KB` (the
    /// description's spelling) creeping into the notice.
    #[test]
    fn notices_match_pi() {
        assert_eq!(
            match_limit_notice(3.0),
            "3 matches limit reached. Use limit=6 for more, or refine pattern"
        );
        assert_eq!(
            match_limit_notice(DEFAULT_LIMIT),
            "100 matches limit reached. Use limit=200 for more, or refine pattern"
        );
        // `formatSize(51200)` — captured from real Pi in truncate.cases.jsonl.
        assert_eq!(byte_limit_notice(), "50.0KB limit reached");
        assert_ne!(byte_limit_notice(), "50KB limit reached");
        assert_eq!(
            lines_truncated_notice(),
            "Some lines truncated to 500 chars. Use read tool to see full lines"
        );
    }

    /// `Math.max(1, limit ?? 100)`: grep clamps where `ls`/`find` do not.
    #[test]
    fn limit_is_clamped_to_at_least_one() {
        assert_eq!(js_max_1(None), 100.0);
        assert_eq!(js_max_1(Some(3.0)), 3.0);
        assert_eq!(js_max_1(Some(0.0)), 1.0);
        assert_eq!(js_max_1(Some(-5.0)), 1.0);
        assert_eq!(js_max_1(Some(1.5)), 1.5);
        assert!(js_max_1(Some(f64::NAN)).is_nan());
    }

    /// The two sanitizers are deliberately different: the match-line one deletes
    /// `\r` and strips exactly one trailing `\n`, the file one (inside
    /// [`file_lines`]) turns a lone `\r` into a newline.
    #[test]
    fn match_line_sanitizer_strips_one_trailing_newline() {
        assert_eq!(sanitize_match_line("a\r\nb\n"), "a\nb");
        assert_eq!(sanitize_match_line("a\rb"), "ab");
        assert_eq!(sanitize_match_line("a\n\n"), "a\n");
        assert_eq!(sanitize_match_line("a"), "a");
    }

    /// `details` collapses to `null` when empty, keeps insertion order otherwise,
    /// and writes `matchLimitReached` as a JS integer.
    #[test]
    fn details_serializes_like_json_stringify() {
        assert_eq!(GrepToolDetails::default().into_value(), Value::Null);

        let details = GrepToolDetails {
            match_limit_reached: Some(3.0),
            truncation: None,
            lines_truncated: Some(true),
        };
        assert_eq!(
            serde_json::to_string(&details.into_value()).unwrap(),
            r#"{"matchLimitReached":3,"linesTruncated":true}"#
        );
    }

    /// Pinned against `node -e 'console.log(require("path").win32.relative(a, b))'`
    /// on the same `node` the oracle script runs under.
    #[test]
    fn win32_relative_matches_node() {
        for (from, to, want) in [
            ("C:\\t\\src", "C:\\t\\src\\a.ts", "a.ts"),
            ("C:\\t\\src", "C:\\t\\src\\nested\\c.txt", "nested\\c.txt"),
            ("C:\\t\\src", "C:\\t\\other\\x.txt", "..\\other\\x.txt"),
            ("C:\\t\\src", "C:\\t\\src", ""),
            // Different drive: no relative path exists, so `to` comes back whole.
            ("C:\\t\\src", "D:\\t\\src\\a.ts", "D:\\t\\src\\a.ts"),
            // Case-insensitive comparison.
            ("C:\\T\\SRC", "C:\\t\\src\\a.ts", "a.ts"),
            // A shared *prefix* is not a shared component.
            ("C:\\t\\src\\a", "C:\\t\\src\\ab", "..\\ab"),
            // Trailing separators are trimmed.
            ("C:\\t\\src\\\\", "C:\\t\\src\\a.ts", "a.ts"),
            // The drive root is the `i === 2` special case.
            ("C:\\", "C:\\a.txt", "a.txt"),
        ] {
            assert_eq!(
                win32_relative(from, to),
                want,
                "win32.relative({from:?}, {to:?})"
            );
        }
    }

    /// Pinned against `require("path").posix.relative`.
    #[test]
    fn posix_relative_matches_node() {
        for (from, to, want) in [
            ("/t/src", "/t/src/a.ts", "a.ts"),
            ("/t/src", "/t/src/nested/c.txt", "nested/c.txt"),
            ("/t/src", "/t/other/x.txt", "../other/x.txt"),
            ("/t/src", "/t/src", ""),
            ("/t/src/a", "/t/src/ab", "../ab"),
            ("/", "/a.txt", "a.txt"),
            // `("/t/src/", "/t/src/a.ts")` is deliberately absent: Node answers
            // `"a.ts"` there only because its `relative` runs both arguments
            // through `resolve` first, which drops the trailing slash. This
            // transcription skips that step (see the section comment above) and so
            // returns `"../a.ts"`. Unreachable here — `resolve_to_cwd` never
            // returns a trailing separator. win32 is unaffected: its `relative`
            // trims trailing separators itself.
        ] {
            assert_eq!(
                posix_relative(from, to),
                want,
                "posix.relative({from:?}, {to:?})"
            );
        }
    }

    /// Pinned against `require("path").{win32,posix}.basename`; only the flavour
    /// for the running platform is exercised, since [`node_basename`] branches on
    /// it.
    #[test]
    fn basename_matches_node() {
        let win32 = path_utils::Platform::current() == path_utils::Platform::Win32;
        let cases: &[(&str, &str)] = if win32 {
            &[
                ("C:\\t\\src\\a.ts", "a.ts"),
                ("C:\\t\\src\\\\", "src"),
                ("C:\\\\", ""),
                ("a.ts", "a.ts"),
                ("", ""),
                ("C:\\t\\src/a.ts", "a.ts"),
                ("/t/src/a.ts", "a.ts"),
                ("C:a.ts", "a.ts"),
            ]
        } else {
            &[
                ("/t/src/a.ts", "a.ts"),
                ("/t/src/", "src"),
                ("/", ""),
                ("a.ts", "a.ts"),
                ("", ""),
                // A backslash is an ordinary character on posix.
                ("C:\\t\\a.ts", "C:\\t\\a.ts"),
            ]
        };
        for (path, want) in cases {
            assert_eq!(node_basename(path), *want, "basename({path:?})");
        }
    }

    /// `formatPath` reports relative to a directory search root and by basename
    /// otherwise — including when the match escapes the root.
    #[test]
    fn format_path_relativizes_only_under_a_directory_root() {
        if path_utils::Platform::current() != path_utils::Platform::Win32 {
            return;
        }
        assert_eq!(
            format_path("C:\\t\\src", "C:\\t\\src\\nested\\c.txt", true),
            "nested/c.txt"
        );
        // A single-file search root: basename, not "".
        assert_eq!(
            format_path("C:\\t\\src\\long.txt", "C:\\t\\src\\long.txt", false),
            "long.txt"
        );
        // Outside the root: the `..` guard falls through to the basename.
        assert_eq!(
            format_path("C:\\t\\src", "C:\\t\\other\\x.txt", true),
            "x.txt"
        );
    }

    /// JS array indexing by a fractional or negative number is `undefined`.
    #[test]
    fn js_index_mirrors_js_array_access() {
        let lines = vec!["a".to_string(), "b".to_string()];
        assert_eq!(js_index(&lines, 0.0), "a");
        assert_eq!(js_index(&lines, 1.0), "b");
        assert_eq!(js_index(&lines, 2.0), "");
        assert_eq!(js_index(&lines, 0.5), "");
        assert_eq!(js_index(&lines, -1.0), "");
    }

    /// The two boolean flags are read with JS truthiness, not a strict `=== true`.
    #[test]
    fn js_truthy_matches_javascript() {
        use serde_json::json;
        assert!(!js_truthy(None));
        assert!(!js_truthy(Some(&Value::Null)));
        assert!(!js_truthy(Some(&json!(false))));
        assert!(!js_truthy(Some(&json!(0))));
        assert!(!js_truthy(Some(&json!(""))));
        assert!(js_truthy(Some(&json!(true))));
        assert!(js_truthy(Some(&json!(1))));
        assert!(js_truthy(Some(&json!("false"))));
        assert!(js_truthy(Some(&json!([]))));
        assert!(js_truthy(Some(&json!({}))));
    }
}
