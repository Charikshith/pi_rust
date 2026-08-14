//! Port of `core/tools/find.ts` (UI-free half) — the `find` tool.
//!
//! Shells out to the managed `fd` binary (see [`crate::binaries`]).
//! Gated by `tests/fixtures/pi/tools/{schemas,strings}/find.json` and the `find`
//! rows of `exec.corpus.jsonl`.
//!
//! `find` is Pi's glob-based file finder: `fd --glob`, `.gitignore`-aware, hidden
//! files included, paths reported **relative to the search root** with a `/`
//! appended to directories. Two independent caps apply, whichever is hit first:
//! [`DEFAULT_LIMIT`] results and [`DEFAULT_MAX_BYTES`] bytes (`find.ts:322-340`).
//!
//! Ported items:
//!
//! | TS (`find.ts`) | here |
//! | --- | --- |
//! | `toPosixPath` (:16-18) | [`to_posix_path`] |
//! | `findSchema` (:20-26) | the `object_schema` in [`create_find_tool_definition`] |
//! | `DEFAULT_LIMIT` (:30) | [`DEFAULT_LIMIT`] |
//! | `FindToolDetails` (:32-35) | [`FindToolDetails`] |
//! | `FindOperations` (:41-46) | [`FindOperations`] + [`GlobOptions`] |
//! | `defaultFindOperations` (:48-52) | [`DefaultFindOperations`] |
//! | `FindToolOptions` (:54-57) | [`FindToolOptions`] |
//! | `createFindToolDefinition` (:109-370) | [`create_find_tool_definition`] |
//! | `createFindTool` (:372-374) | [`create_find_tool`] |
//! | `formatFindCall` (:59-74), `formatFindResult` (:76-107), `renderCall` (:359-363), `renderResult` (:364-368) | **omitted** — TUI only (feat-006/007) |
//!
//! # Two `execute` bodies, not one
//!
//! `execute` branches on `if (customOps?.glob)` (`find.ts:155`). Pi's
//! `defaultFindOperations.glob` is a **placeholder that returns `[]`**
//! (`find.ts:50-51`) and is never called: the real `fd` invocation lives inline in
//! `execute` and runs only when no custom operations were supplied. So there are
//! two distinct code paths with *different* observable contracts, both reproduced:
//!
//! * **Branch A** ([`glob_branch`], `find.ts:155-211`) — checks `ops.exists` first
//!   (`Path not found: …`) and its result-limit notice is the bare
//!   `"${effectiveLimit} results limit reached"`, with **no** `. Use limit=… for
//!   more, or refine pattern` suffix.
//! * **Branch B** ([`fd_branch`], `find.ts:213-347`) — no `exists` check at all
//!   (a missing directory surfaces as `fd`'s own stderr), and its notice *does*
//!   carry the `Use limit=… for more, or refine pattern` suffix.
//!
//! Handing [`DefaultFindOperations`] to [`FindToolOptions::operations`] explicitly
//! therefore takes Branch A and always yields `No files found matching pattern`,
//! exactly as Pi does — `customOps?.glob` is truthy for it.
//!
//! # `limit` is not clamped
//!
//! `effectiveLimit = limit ?? DEFAULT_LIMIT` (`find.ts:151`) — `??`, so an explicit
//! `0` survives, and there is no `Math.max(1, …)` guard of the kind `grep` applies.
//! With `limit: 0`, `fd --max-results 0` is asked for and `relativized.length >= 0`
//! holds for the empty result too — but an empty `output` takes the
//! `No files found matching pattern` branch first (`find.ts:297`), so the notice is
//! dropped, just as in `ls`.
//!
//! # A win32 behaviour that is *not* a bug to fix
//!
//! For a pattern containing `/`, Pi passes `--full-path` and rewrites the pattern
//! to `**/<pattern>` (`find.ts:243-252`). `fd`'s globber treats only `/` as a
//! separator, while the Windows candidate path it matches against uses `\`, so a
//! path-shaped pattern like `nested/*.txt` **matches nothing on win32** — captured
//! as such in the corpus, with a note. No separator translation is added here; the
//! port reproduces Pi.
//!
//! # `fd`'s output separator depends on the environment
//!
//! `fd` writes `/`-separated paths when `MSYSTEM` is set in its environment (Git
//! Bash / MSYS2) and native `\`-separated paths otherwise. Pi spawns `fd` with the
//! inherited `process.env`, so which one happens is ambient — and it is observable:
//! with `/` output the `line.startsWith(searchPath)` fast path (`find.ts:313`)
//! misses and relativization falls through to `path.relative` (`find.ts:316`),
//! which normalizes separators *and* drops a trailing one. The captured corpus was
//! recorded with `MSYSTEM` set; see the header of
//! `crates/pirust-tools/tests/find_golden.rs`. Both paths are ported verbatim.

use std::fmt;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;

use async_trait::async_trait;
use pirust_agent_core::types::{AgentTool, AgentToolResult, AgentToolUpdateCallback, ToolError};
use pirust_ai::jsnum::js_number;
use pirust_ai::types::{TextContent, UserContent};
use serde::{Deserialize, Serialize, Serializer};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::binaries::{ensure_tool, BinaryEnv, ManagedTool, SpawnProbe};
use crate::definition::schema::{number_prop, object_schema, optional, required, string_prop};
use crate::definition::PirustToolDefinition;
use crate::path_utils::{self, Platform};
use crate::truncate::{
    format_size, truncate_head, TruncationOptions, TruncationResult, DEFAULT_MAX_BYTES,
};

/// Default maximum number of results returned (TS `find.ts:30`).
pub const DEFAULT_LIMIT: f64 = 1000.0;

/// `Number.MAX_SAFE_INTEGER`, passed as `maxLines` so that only the byte cap is
/// live (TS `find.ts:189`, `find.ts:324`): the result count is already capped by
/// `--max-results`.
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// `find.ts:117` — the `description` template literal with its two substitutions
/// already performed: `${DEFAULT_LIMIT}` → `1000` and `${DEFAULT_MAX_BYTES / 1024}`
/// → `50`.
///
/// The byte cap appears here as the bare `"50KB"` of an integer division, *not* as
/// [`format_size`]'s `"50.0KB"` — both literals exist in this module (the latter in
/// the truncation notice, `find.ts:198`/`find.ts:335`) and they are not
/// interchangeable.
pub const FIND_DESCRIPTION: &str = "Search for files by glob pattern. Returns matching file paths relative to the search directory. Respects .gitignore. Output is truncated to 1000 results or 50KB (whichever is hit first).";

/// `find.ts:118` — `promptSnippet`.
pub const FIND_PROMPT_SNIPPET: &str = "Find files by glob pattern (respects .gitignore)";

/// The message of the `Error` Pi rejects with on abort, from the up-front
/// `signal.aborted` check (`find.ts:129`), the `abort` listener (`find.ts:144`) and
/// the four mid-flight re-checks (`find.ts:161`, `:169`, `:216`, `:286`).
const ABORT_MESSAGE: &str = "Operation aborted";

/// Output when nothing matched (TS `find.ts:175`, `find.ts:300`).
const NO_MATCHES: &str = "No files found matching pattern";

/// TS `ops.glob`'s `ignore` argument (`find.ts:165`). Branch A only — `fd` gets no
/// equivalent flag, it relies on `.gitignore` plus its own defaults.
pub const GLOB_IGNORE: [&str; 2] = ["**/node_modules/**", "**/.git/**"];

/// TS `toPosixPath` (`find.ts:16-18`): `value.split(path.sep).join("/")`.
///
/// [`std::path::MAIN_SEPARATOR`] is Node's `path.sep` for the platform this binary
/// was built for — `\` on win32, `/` elsewhere, where the replacement is the
/// identity.
pub fn to_posix_path(value: &str) -> String {
    value.replace(std::path::MAIN_SEPARATOR, "/")
}

// ===========================================================================
// Operations seam
// ===========================================================================

/// TS `ops.glob`'s options object (`find.ts:45`, built at `find.ts:164-167`).
#[derive(Debug, Clone, PartialEq)]
pub struct GlobOptions<'a> {
    /// Glob patterns to skip — always [`GLOB_IGNORE`] from the built-in caller.
    pub ignore: &'a [&'a str],
    /// `effectiveLimit`, i.e. `limit ?? DEFAULT_LIMIT` (unclamped).
    pub limit: f64,
}

/// Pluggable operations for the `find` tool (TS `FindOperations`, `find.ts:41-46`).
///
/// Override to delegate file search to a remote system (for example SSH).
/// Supplying *any* implementation switches `execute` to Branch A — see the module
/// docs.
#[async_trait]
pub trait FindOperations: Send + Sync {
    /// Check if path exists (TS `find.ts:43`).
    async fn exists(&self, absolute_path: &str) -> bool;
    /// Find files matching a glob pattern; returns relative or absolute paths
    /// (TS `find.ts:45`).
    async fn glob(&self, pattern: &str, cwd: &str, options: GlobOptions<'_>) -> Vec<String>;
}

/// `defaultFindOperations` (TS `find.ts:48-52`).
///
/// [`DefaultFindOperations::glob`] is Pi's **placeholder returning `[]`**, not a
/// filesystem walk: `execute` only ever reaches it when a caller passes this value
/// in explicitly, because the built-in path takes the `fd` branch instead
/// (`find.ts:50-51`).
#[derive(Debug, Clone, Copy, Default)]
pub struct DefaultFindOperations;

#[async_trait]
impl FindOperations for DefaultFindOperations {
    /// TS `pathExists` (`find.ts:49`), i.e. `path-utils.ts:31-38`.
    async fn exists(&self, absolute_path: &str) -> bool {
        path_utils::path_exists(absolute_path).await
    }

    /// TS `glob: () => []` (`find.ts:51`) — the placeholder, verbatim.
    async fn glob(&self, _pattern: &str, _cwd: &str, _options: GlobOptions<'_>) -> Vec<String> {
        Vec::new()
    }
}

/// How Branch B finds and runs the `fd` binary.
///
/// **Not in Pi.** Pi calls `ensureTool("fd", true)` (`find.ts:214`) and spawns with
/// the inherited `process.env`; both defaults are reproduced by
/// [`FdSpawn::default`]. The two overrides exist because those inputs are ambient
/// and the golden test must pin them:
///
/// * `binary_path` skips [`ensure_tool`] entirely. pirust's managed directory is
///   `~/.pirust/agent/bin` (see [`crate::binaries::CONFIG_DIR_NAME`]) and must not
///   grow a `~/.pi` fallback, so on a machine where only Pi has downloaded `fd`
///   there is no other way to reach a real binary.
/// * `extra_env` is merged on top of the inherited environment. `fd` consults
///   `MSYSTEM` (win32 only) to decide whether to print `/`- or `\`-separated
///   paths, which changes which relativization branch Pi takes; the captured
///   corpus was recorded with it set.
///
/// Production callers use [`FdSpawn::default`], which is byte-for-byte Pi.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FdSpawn {
    /// Use this binary instead of asking [`ensure_tool`] for one.
    pub binary_path: Option<String>,
    /// Environment variables set on the `fd` child, on top of the inherited ones.
    pub extra_env: Vec<(String, String)>,
}

/// `FindToolOptions` (TS `find.ts:54-57`), plus the [`FdSpawn`] test seam.
#[derive(Clone, Default)]
pub struct FindToolOptions {
    /// Custom operations for find. `None` → the `fd` branch (TS `find.ts:56`,
    /// resolved at `find.ts:115`/`find.ts:155`).
    pub operations: Option<Arc<dyn FindOperations>>,
    /// How to locate and run `fd`. Not part of Pi — see [`FdSpawn`].
    pub fd: FdSpawn,
}

impl fmt::Debug for FindToolOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FindToolOptions")
            .field("operations", &self.operations.as_ref().map(|_| "<dyn>"))
            .field("fd", &self.fd)
            .finish()
    }
}

// ===========================================================================
// Details
// ===========================================================================

/// `FindToolDetails` (TS `find.ts:32-35`) — the `details` payload, persisted into
/// the session JSONL.
///
/// **Field order is deliberately the reverse of the TS interface.** The interface
/// declares `truncation` then `resultLimitReached` (`find.ts:33-34`), but the
/// object is built empty and filled `resultLimitReached` first
/// (`find.ts:195`/`:332`), `truncation` second (`find.ts:199`/`:336`) — and
/// `JSON.stringify` follows *insertion* order. Since this value is compared
/// byte-for-byte against captured Pi output, the Rust declaration order matches the
/// insertion order instead.
///
/// Both fields are omitted when absent, and the whole object collapses to `null`
/// when neither is set (`find.ts:207`/`:344`: `Object.keys(details).length > 0 ?
/// details : undefined`) — see [`FindToolDetails::is_empty`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FindToolDetails {
    /// The `effectiveLimit` that was reached, set only when the result cap cut the
    /// listing (TS `find.ts:195`, `find.ts:332`). `f64` because `limit` is a schema
    /// `number`; see [`serialize_js_number`] for how it reaches JSON.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_js_number"
    )]
    pub result_limit_reached: Option<f64>,
    /// The byte-truncation result, set only when the byte cap cut the output
    /// (TS `find.ts:199`, `find.ts:336`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncation: Option<TruncationResult>,
}

impl FindToolDetails {
    /// TS `Object.keys(details).length === 0` (`find.ts:207`, `find.ts:344`).
    pub fn is_empty(&self) -> bool {
        self.result_limit_reached.is_none() && self.truncation.is_none()
    }

    /// TS `details` as resolved into the tool result: the object, or `undefined`
    /// (→ JSON `null`) when it never got a key.
    fn into_value(self) -> Value {
        if self.is_empty() {
            Value::Null
        } else {
            serde_json::to_value(self).expect("FindToolDetails is always representable as JSON")
        }
    }
}

/// Emit an `f64` the way `JSON.stringify` would: as an integer literal whenever the
/// value is integral, since every JS number that happens to be a whole number
/// stringifies without a `.0`.
///
/// `serde_json` would otherwise write `1.0` for the `f64` `1.0`, and the captured
/// `details` reads `{"resultLimitReached":1}`. Mirrors `ls.rs`'s helper of the same
/// name; both exist because each tool's `details` is its own persisted shape.
fn serialize_js_number<S: Serializer>(
    value: &Option<f64>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    let value = value.expect("skip_serializing_if guarantees Some");
    // 2^53: above it every f64 is integral but no longer i64-exact in JS terms.
    if value.fract() == 0.0 && value.abs() < 9_007_199_254_740_992.0 {
        serializer.serialize_i64(value as i64)
    } else {
        serializer.serialize_f64(value)
    }
}

// ===========================================================================
// Tool definition
// ===========================================================================

/// `createFindToolDefinition` (TS `find.ts:109-370`), minus `renderCall` /
/// `renderResult`.
///
/// `options.operations` is read **once, here** (`find.ts:114`) and captured by
/// `execute`, so swapping it afterwards has no effect — that is the contract.
pub fn create_find_tool_definition(
    cwd: impl Into<String>,
    options: FindToolOptions,
) -> PirustToolDefinition {
    let cwd: Arc<str> = Arc::from(cwd.into());
    // TS `const customOps = options?.operations` (`find.ts:114`).
    let custom_ops = options.operations;
    let fd = Arc::new(options.fd);

    PirustToolDefinition::new(
        "find",
        "find",
        FIND_DESCRIPTION,
        // TS `findSchema` (`find.ts:20-26`).
        object_schema([
            required(
                "pattern",
                string_prop(
                    "Glob pattern to match files, e.g. '*.ts', '**/*.json', or 'src/**/*.spec.ts'",
                ),
            ),
            optional(
                "path",
                string_prop("Directory to search in (default: current directory)"),
            ),
            optional(
                "limit",
                number_prop("Maximum number of results (default: 1000)"),
            ),
        ]),
        move |_tool_call_id: String,
              args: Value,
              token: CancellationToken,
              _on_update: AgentToolUpdateCallback| {
            let cwd = Arc::clone(&cwd);
            let custom_ops = custom_ops.clone();
            let fd = Arc::clone(&fd);
            async move { execute_find(&cwd, custom_ops.as_deref(), &fd, &args, &token).await }
        },
    )
    .with_prompt_snippet(FIND_PROMPT_SNIPPET)
}

/// `createFindTool` (TS `find.ts:372-374`).
///
/// `wrapToolDefinition` has no separate type here — [`PirustToolDefinition`] *is*
/// the [`AgentTool`] — so this is [`create_find_tool_definition`] behind the trait
/// object.
pub fn create_find_tool(cwd: impl Into<String>, options: FindToolOptions) -> Arc<dyn AgentTool> {
    Arc::new(create_find_tool_definition(cwd, options))
}

/// TS `execute` (`find.ts:120-358`).
///
/// Pi hand-rolls a `Promise` so that an `abort` arriving mid-search rejects
/// immediately and kills the `fd` child (`find.ts:127-146`, `stopChild` at
/// `:260-264`), and removes the listener before resolving (`find.ts:138`) so a late
/// abort cannot clobber a finished result. The `select!` below is that shape:
/// `biased` polls the body first, so a body that is already `Ready` wins — the Rust
/// equivalent of unregistering the listener — while a body that is `Pending` yields
/// to the cancellation branch. Dropping the body drops the [`tokio::process::Child`],
/// which is spawned with `kill_on_drop`, reproducing `stopChild`.
async fn execute_find(
    cwd: &str,
    custom_ops: Option<&dyn FindOperations>,
    fd: &FdSpawn,
    args: &Value,
    token: &CancellationToken,
) -> Result<AgentToolResult, ToolError> {
    // TS `find.ts:128-131`: the synchronous pre-check, before any I/O.
    if token.is_cancelled() {
        return Err(ABORT_MESSAGE.into());
    }

    tokio::select! {
        biased;
        result = find_body(cwd, custom_ops, fd, args) => result,
        () = token.cancelled() => Err(ABORT_MESSAGE.into()),
    }
}

/// The `async` IIFE inside Pi's promise (TS `find.ts:148-356`), up to the point
/// where it forks into the two branches.
async fn find_body(
    cwd: &str,
    custom_ops: Option<&dyn FindOperations>,
    fd: &FdSpawn,
    args: &Value,
) -> Result<AgentToolResult, ToolError> {
    let pattern = args
        .get("pattern")
        .and_then(Value::as_str)
        .unwrap_or_default();
    // TS `find.ts:150`: `searchDir || "."`, so `undefined`, `null` *and* `""` all
    // fall back to the cwd — `??` would keep `""`.
    let search_dir = args
        .get("path")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
        .unwrap_or(".");
    let search_path = path_utils::resolve_to_cwd(search_dir, cwd)?;
    // TS `find.ts:151`: `limit ?? DEFAULT_LIMIT` — no `Math.max(1, …)` clamp.
    let effective_limit = args
        .get("limit")
        .and_then(Value::as_f64)
        .unwrap_or(DEFAULT_LIMIT);

    // TS `find.ts:155`: `if (customOps?.glob)`.
    match custom_ops {
        Some(ops) => glob_branch(ops, pattern, &search_path, effective_limit).await,
        None => fd_branch(fd, pattern, &search_path, effective_limit).await,
    }
}

// ===========================================================================
// Branch A — a custom `operations.glob` was supplied
// ===========================================================================

/// TS `find.ts:155-211`.
///
/// Note the two things that make this *not* interchangeable with [`fd_branch`]: the
/// up-front `ops.exists` check (`find.ts:156-159`), and a result-limit notice
/// without the `. Use limit=… for more, or refine pattern` suffix
/// (`find.ts:194`).
async fn glob_branch(
    ops: &dyn FindOperations,
    pattern: &str,
    search_path: &str,
    effective_limit: f64,
) -> Result<AgentToolResult, ToolError> {
    // TS `find.ts:156-159`.
    if !ops.exists(search_path).await {
        return Err(format!("Path not found: {search_path}").into());
    }

    let results = ops
        .glob(
            pattern,
            search_path,
            GlobOptions {
                ignore: &GLOB_IGNORE,
                limit: effective_limit,
            },
        )
        .await;

    // TS `find.ts:172-180`.
    if results.is_empty() {
        return Ok(text_result(NO_MATCHES, Value::Null));
    }

    // TS `find.ts:183-186`: relativize paths against the search root for stable
    // output. No trailing-separator handling here — that is Branch B's, because
    // only `fd` marks directories that way.
    let relativized: Vec<String> = results
        .iter()
        .map(|p| to_posix_path(&relative_to_search_path(p, search_path)))
        .collect();

    Ok(finish(
        relativized,
        effective_limit,
        NoticeStyle::PlainLimit,
    ))
}

// ===========================================================================
// Branch B — the default `fd` path
// ===========================================================================

/// TS `find.ts:213-347`.
///
/// There is **no `exists` check** in this branch: a missing directory reaches `fd`,
/// which exits non-zero with its own message, and that message is what the user
/// sees (captured in the corpus).
async fn fd_branch(
    fd: &FdSpawn,
    pattern: &str,
    search_path: &str,
    effective_limit: f64,
) -> Result<AgentToolResult, ToolError> {
    // TS `find.ts:214`: `ensureTool("fd", true)`. `silent` is not ported (see
    // `binaries.rs`); `Ok(None)` is Pi's `undefined`.
    let fd_path = match &fd.binary_path {
        Some(path) => path.clone(),
        None => ensure_tool(ManagedTool::Fd, &BinaryEnv::from_process_env(), &SpawnProbe)
            .await
            .map_err(|e| -> ToolError { e.to_string().into() })?
            // TS `find.ts:219-222`.
            .ok_or_else(|| -> ToolError {
                "fd is not available and could not be downloaded".into()
            })?,
    };

    let args = fd_args(pattern, search_path, effective_limit).await;

    // TS `find.ts:255`: `spawn(fdPath, args, { stdio: ["ignore", "pipe", "pipe"] })`.
    let mut command = tokio::process::Command::new(&fd_path);
    command
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // TS `stopChild` (`find.ts:260-264`): an abort kills the child. Here the
        // abort drops the future, hence the `Child`, hence the process.
        .kill_on_drop(true);
    for (key, value) in &fd.extra_env {
        command.env(key, value);
    }

    // TS `child.on("error", …)` (`find.ts:278-281`).
    //
    // # Known gap
    //
    // Node's `error.message` for a missing binary is `spawn <cmd> ENOENT`;
    // `std::io::Error`'s `Display` is the OS text plus `(os error N)`. The
    // surrounding `Failed to run fd: ` is exact, the tail is not. No captured row
    // exercises it (`ensureTool` returned a real path in every one).
    let output = match command.spawn() {
        Ok(child) => child
            .wait_with_output()
            .await
            .map_err(|e| -> ToolError { format!("Failed to run fd: {e}").into() })?,
        Err(e) => return Err(format!("Failed to run fd: {e}").into()),
    };

    // TS `createInterface({ input: child.stdout })` + `rl.on("line", …)`
    // (`find.ts:256`, `find.ts:274-276`). Non-UTF-8 bytes are replaced, matching
    // Node, which hands JS a string with U+FFFD for invalid sequences.
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines = readline_lines(&stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    // TS `find.ts:283-305`.
    let joined = lines.join("\n");
    if !output.status.success() {
        // TS `${code}`: `null` when the child died from a signal, which Windows
        // never reports.
        let code = output
            .status
            .code()
            .map_or_else(|| "null".to_string(), |code| code.to_string());
        let error_msg = {
            let trimmed = stderr.trim();
            if trimmed.is_empty() {
                format!("fd exited with code {code}")
            } else {
                trimmed.to_string()
            }
        };
        // A non-zero exit *with* output is tolerated (`find.ts:292`): `--max-results`
        // makes `fd` stop early, which it reports as a failure.
        if joined.is_empty() {
            return Err(error_msg.into());
        }
    }
    if joined.is_empty() {
        return Ok(text_result(NO_MATCHES, Value::Null));
    }

    // TS `find.ts:307-320`.
    let mut relativized: Vec<String> = Vec::new();
    for raw_line in &lines {
        // TS `rawLine.replace(/\r$/, "").trim()`.
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line).trim();
        if line.is_empty() {
            continue;
        }
        // `fd` appends a path separator to directories, so this is how a directory
        // keeps its `/` through relativization — `path.relative` would drop it.
        let had_trailing_slash = line.ends_with('/') || line.ends_with('\\');
        let mut relative_path = relative_to_search_path(line, search_path);
        // NOTE the order: the `endsWith("/")` test runs on the *pre*-posix string
        // (`find.ts:318`), so a native `nested\` fails it and gains a second
        // separator — `to_posix_path` then yields `nested//`. That is Pi's output
        // whenever `fd` prints `\`-separated paths; with `/`-separated output the
        // `path.relative` branch has already stripped the separator and the result
        // is `nested/`. Both are reachable and neither is "fixed" here.
        if had_trailing_slash && !relative_path.ends_with('/') {
            relative_path.push('/');
        }
        relativized.push(to_posix_path(&relative_path));
    }

    Ok(finish(
        relativized,
        effective_limit,
        NoticeStyle::LimitWithHint,
    ))
}

/// The `fd` argument vector (TS `find.ts:224-253`), in exact order.
async fn fd_args(pattern: &str, search_path: &str, effective_limit: f64) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "--glob".to_string(),
        "--color=never".to_string(),
        "--hidden".to_string(),
    ];

    // TS `find.ts:226-240`. `fd` normally ignores `.gitignore` outside git repos, so
    // keep `--no-require-git` there. Inside repos, use `fd`'s default git-aware
    // behaviour so parent `.gitignore` rules stop at nested repo boundaries:
    // https://github.com/earendil-works/pi/issues/5960
    if !inside_git_repo(search_path).await {
        args.push("--no-require-git".to_string());
    }
    args.push("--max-results".to_string());
    // TS `String(effectiveLimit)` (`find.ts:241`).
    args.push(js_number(effective_limit));

    // TS `find.ts:243-252`. `fd --glob` matches against the basename unless
    // `--full-path` is set; in `--full-path` mode it matches against the absolute
    // candidate path, so a path-containing pattern like `src/**/*.spec.ts` needs a
    // leading `**/` to match anything.
    let mut effective_pattern = pattern.to_string();
    if pattern.contains('/') {
        args.push("--full-path".to_string());
        // NOTE: `pattern !== "**"` (`find.ts:249`) is unreachable — `"**"` contains
        // no `/`, so it never enters this block at all, and therefore never gets
        // `--full-path` either. Ported as written.
        if !pattern.starts_with('/') && !pattern.starts_with("**/") && pattern != "**" {
            effective_pattern = format!("**/{pattern}");
        }
    }

    args.push("--".to_string());
    args.push(effective_pattern);
    args.push(search_path.to_string());
    args
}

/// TS `find.ts:230-239` — walk up from `searchPath` looking for a `.git` entry.
///
/// The loop ends when `path.dirname(current) === current`, i.e. at a filesystem
/// root. [`std::path::Path::parent`] returns `None` in exactly those cases
/// (`C:\`, `\\server\share`, `/`), so the two agree. It differs from `dirname` for a
/// path with a *trailing* separator, which `path.resolve` never produces and
/// `search_path` therefore never has.
async fn inside_git_repo(search_path: &str) -> bool {
    let mut current = PathBuf::from(search_path);
    loop {
        let git = current.join(".git");
        if path_utils::path_exists(&git.to_string_lossy()).await {
            return true;
        }
        match current.parent() {
            Some(parent) if parent != current => current = parent.to_path_buf(),
            _ => return false,
        }
    }
}

/// Node's `readline` line splitting for a complete stdout buffer
/// (TS `find.ts:256`, `find.ts:274-276`).
///
/// Node's interface terminates a line on `\r\n`, `\n`, or a `\r` **not** followed by
/// `\n`, and flushes any trailing partial line at `close` — so `"a\nb\n"` and
/// `"a\nb"` both yield `["a", "b"]` and `""` yields `[]`. `fd` emits `\n` only; the
/// `\r` handling is here because Pi's own `replace(/\r$/, "")` (`find.ts:309`)
/// implies it can appear.
fn readline_lines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let bytes = text.as_bytes();
    let mut start = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'\n' => {
                lines.push(&text[start..i]);
                i += 1;
                start = i;
            }
            b'\r' => {
                lines.push(&text[start..i]);
                // `\r\n` is one terminator.
                i += if bytes.get(i + 1) == Some(&b'\n') {
                    2
                } else {
                    1
                };
                start = i;
            }
            _ => i += 1,
        }
    }
    if start < bytes.len() {
        lines.push(&text[start..]);
    }
    lines
}

// ===========================================================================
// Shared tail
// ===========================================================================

/// Which wording the result-limit notice uses — the one observable difference
/// between the two branches' otherwise identical tails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NoticeStyle {
    /// Branch A (TS `find.ts:194`): `"<n> results limit reached"`, full stop.
    PlainLimit,
    /// Branch B (TS `find.ts:330`): plus `". Use limit=<2n> for more, or refine
    /// pattern"`.
    LimitWithHint,
}

/// TS `find.ts:187-209` / `find.ts:322-346` — identical apart from
/// [`NoticeStyle`].
fn finish(relativized: Vec<String>, effective_limit: f64, style: NoticeStyle) -> AgentToolResult {
    let result_limit_reached = relativized.len() as f64 >= effective_limit;
    let raw_output = relativized.join("\n");
    // Only the byte cap is live; the result count is capped by `--max-results`.
    let truncation = truncate_head(
        &raw_output,
        TruncationOptions {
            max_lines: Some(MAX_SAFE_INTEGER),
            max_bytes: None,
        },
    );
    let mut output = truncation.content.clone();
    let mut details = FindToolDetails::default();

    let mut notices: Vec<String> = Vec::new();
    if result_limit_reached {
        notices.push(match style {
            NoticeStyle::PlainLimit => {
                format!("{} results limit reached", js_number(effective_limit))
            }
            NoticeStyle::LimitWithHint => format!(
                "{} results limit reached. Use limit={} for more, or refine pattern",
                js_number(effective_limit),
                js_number(effective_limit * 2.0)
            ),
        });
        details.result_limit_reached = Some(effective_limit);
    }
    if truncation.truncated {
        // `formatSize`, hence `50.0KB` — not the `50KB` of the description.
        notices.push(format!("{} limit reached", format_size(DEFAULT_MAX_BYTES)));
        details.truncation = Some(truncation);
    }
    if !notices.is_empty() {
        output += &format!("\n\n[{}]", notices.join(". "));
    }

    text_result(&output, details.into_value())
}

/// TS `{ content: [{ type: "text", text }], details }` (`find.ts:175`,
/// `find.ts:205-208`, `find.ts:300`, `find.ts:342-345`).
fn text_result(text: &str, details: Value) -> AgentToolResult {
    AgentToolResult {
        content: vec![UserContent::Text(TextContent::new(text))],
        details,
        added_tool_names: None,
        terminate: None,
    }
}

/// TS `find.ts:183-186` / `find.ts:312-317` — the shared relativization step,
/// **without** the [`to_posix_path`] call.
///
/// The fast path is a literal prefix test (`p.startsWith(searchPath)`), which is
/// case-sensitive and separator-sensitive even on win32; when it misses, Node's
/// `path.relative` does the work. Posix-ification is deliberately left to the
/// callers: Branch A applies it immediately (`find.ts:184-185`) while Branch B
/// applies it *after* its trailing-separator fixup (`find.ts:318-319`), and that
/// ordering is observable.
fn relative_to_search_path(path: &str, search_path: &str) -> String {
    // TS `p.slice(searchPath.length + 1)`: drop the prefix plus one more unit, which
    // is the separator. Past the end of the string JS `slice` yields `""`.
    //
    // Pi counts UTF-16 code units; this drops one *character*. The two differ only
    // when the character right after the prefix is astral, where Pi keeps a lone
    // surrogate — and that character is a path separator in every reachable case.
    match path.strip_prefix(search_path) {
        Some(rest) => {
            let mut chars = rest.chars();
            chars.next();
            chars.as_str().to_string()
        }
        None => node_relative(search_path, path),
    }
}

// ===========================================================================
// node:path — relative
// ===========================================================================

/// Node's `path.relative(from, to)` (`find.ts:185`, `find.ts:316`), dispatched on
/// [`Platform::current`] exactly as Node picks its `win32`/`posix` flavour.
///
/// Transcribed from Node's `lib/path.js`. The two `path.resolve` calls it opens with
/// delegate to [`path_utils::resolve_to_cwd`], which is `path.resolve` preceded by
/// `normalizePath`; for the absolute paths this function is ever handed, the only
/// part of `normalizePath` that can fire is unicode-space folding (a leading `@`,
/// `~` or `file://` is impossible on an absolute path), so a path containing
/// U+00A0/U+2000-200A/U+202F/U+205F/U+3000 is the one documented divergence.
///
/// Byte offsets stand in for Node's UTF-16 offsets. They agree wherever
/// lowercasing is length-preserving (all ASCII, and every fixture path); where it
/// is not, Node's own index arithmetic is already inconsistent between the
/// lowercased copy it scans and the original it slices, so neither runtime is
/// "right" there.
fn node_relative(from: &str, to: &str) -> String {
    match Platform::current() {
        Platform::Win32 => win32_relative(from, to),
        Platform::Posix => posix_relative(from, to),
    }
}

/// `path.resolve(p)` — a single argument, so `process.cwd()` is the only base.
///
/// `Err` is unreachable for the callers here (only a `file://` input can fail, and
/// `path.relative`'s arguments are filesystem paths), so the input is returned
/// unchanged rather than inventing an error path Node does not have.
fn node_resolve1(path: &str) -> String {
    let cwd = path_utils::cwd();
    path_utils::resolve_to_cwd(path, &cwd).unwrap_or_else(|_| path.to_string())
}

/// Byte slice that cannot panic: offsets are floored onto char boundaries.
///
/// Only reachable when lowercasing changed a string's byte length — see
/// [`node_relative`]'s note. JS string slicing never throws, so neither does this.
fn safe_slice(value: &str, start: usize, end: usize) -> &str {
    let floor = |mut i: usize| {
        i = i.min(value.len());
        while i > 0 && !value.is_char_boundary(i) {
            i -= 1;
        }
        i
    };
    let start = floor(start);
    let end = floor(end).max(start);
    &value[start..end]
}

/// Node `win32.relative`.
fn win32_relative(from: &str, to: &str) -> String {
    if from == to {
        return String::new();
    }
    let from_orig = node_resolve1(from);
    let to_orig = node_resolve1(to);
    if from_orig == to_orig {
        return String::new();
    }
    let from_lower = from_orig.to_lowercase();
    let to_lower = to_orig.to_lowercase();
    if from_lower == to_lower {
        return String::new();
    }

    let f = from_lower.as_bytes();
    let t = to_lower.as_bytes();

    // Trim any leading backslashes.
    let mut from_start = 0usize;
    while from_start < f.len() && f[from_start] == b'\\' {
        from_start += 1;
    }
    // Trim trailing backslashes (applicable to UNC paths only).
    let mut from_end = f.len();
    while from_end > from_start + 1 && f[from_end - 1] == b'\\' {
        from_end -= 1;
    }
    let from_len = from_end - from_start;

    let mut to_start = 0usize;
    while to_start < t.len() && t[to_start] == b'\\' {
        to_start += 1;
    }
    let mut to_end = t.len();
    while to_end > to_start + 1 && t[to_end - 1] == b'\\' {
        to_end -= 1;
    }
    let to_len = to_end - to_start;

    // Compare paths to find the longest common path from root.
    let length = from_len.min(to_len);
    let mut last_common_sep: isize = -1;
    let mut i = 0usize;
    while i < length {
        let from_code = f[from_start + i];
        if from_code != t[to_start + i] {
            break;
        }
        if from_code == b'\\' {
            last_common_sep = i as isize;
        }
        i += 1;
    }

    if i != length {
        // A mismatch before the first common separator: `to` is unrelated to `from`.
        if last_common_sep == -1 {
            return to_orig;
        }
    } else {
        if to_len > length {
            if t.get(to_start + i) == Some(&b'\\') {
                // `from` is the exact base path for `to`.
                return safe_slice(&to_orig, to_start + i + 1, to_orig.len()).to_string();
            }
            if i == 2 {
                // `from` is the device root.
                return safe_slice(&to_orig, to_start + i, to_orig.len()).to_string();
            }
        }
        if from_len > length {
            if f.get(from_start + i) == Some(&b'\\') {
                last_common_sep = i as isize;
            } else if i == 2 {
                last_common_sep = 3;
            }
        }
        if last_common_sep == -1 {
            last_common_sep = 0;
        }
    }

    // Generate the relative path based on the difference between `to` and `from`.
    let mut out = String::new();
    let mut j = from_start + last_common_sep as usize + 1;
    while j <= from_end {
        if j == from_end || f.get(j) == Some(&b'\\') {
            out.push_str(if out.is_empty() { ".." } else { "\\.." });
        }
        j += 1;
    }

    let mut to_start = to_start + last_common_sep as usize;
    if !out.is_empty() {
        return format!("{out}{}", safe_slice(&to_orig, to_start, to_end));
    }
    if to_orig.as_bytes().get(to_start) == Some(&b'\\') {
        to_start += 1;
    }
    safe_slice(&to_orig, to_start, to_end).to_string()
}

/// Node `posix.relative`.
fn posix_relative(from: &str, to: &str) -> String {
    if from == to {
        return String::new();
    }
    let from_resolved = node_resolve1(from);
    let to_resolved = node_resolve1(to);
    if from_resolved == to_resolved {
        return String::new();
    }

    let f = from_resolved.as_bytes();
    let t = to_resolved.as_bytes();

    // `posix.resolve` always returns an absolute path, so the roots are known.
    let from_start = 1usize;
    let from_end = f.len();
    let from_len = from_end - from_start;
    let to_start = 1usize;
    let to_len = t.len() - to_start;

    let length = from_len.min(to_len);
    let mut last_common_sep: isize = -1;
    let mut i = 0usize;
    while i < length {
        let from_code = f[from_start + i];
        if from_code != t[to_start + i] {
            break;
        }
        if from_code == b'/' {
            last_common_sep = i as isize;
        }
        i += 1;
    }

    // NOTE: unlike `win32.relative`, the two size tests are `else if`-chained here,
    // and there is no `lastCommonSep === -1 → 0` fixup. Ported as written.
    if i == length {
        if to_len > length {
            if t.get(to_start + i) == Some(&b'/') {
                // `from` is the exact base path for `to`.
                return safe_slice(&to_resolved, to_start + i + 1, to_resolved.len()).to_string();
            }
            if i == 0 {
                // `from` is the root.
                return safe_slice(&to_resolved, to_start + i, to_resolved.len()).to_string();
            }
        } else if from_len > length {
            if f.get(from_start + i) == Some(&b'/') {
                last_common_sep = i as isize;
            } else if i == 0 {
                last_common_sep = 0;
            }
        }
    }

    let mut out = String::new();
    let mut j = from_start + (last_common_sep + 1) as usize;
    while j <= from_end {
        if j == from_end || f.get(j) == Some(&b'/') {
            out.push_str(if out.is_empty() { ".." } else { "/.." });
        }
        j += 1;
    }

    let tail_start = (to_start as isize + last_common_sep) as usize;
    format!(
        "{out}{}",
        safe_slice(&to_resolved, tail_start, to_resolved.len())
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// [`FIND_DESCRIPTION`] is a flattened template literal; this reproduces the
    /// interpolation so the two constants it depends on cannot drift.
    #[test]
    fn description_matches_the_template_literal() {
        let composed = format!(
            "Search for files by glob pattern. Returns matching file paths relative to the search \
             directory. Respects .gitignore. Output is truncated to {} results or {}KB (whichever \
             is hit first).",
            js_number(DEFAULT_LIMIT),
            DEFAULT_MAX_BYTES / 1024
        );
        assert_eq!(FIND_DESCRIPTION, composed);
        // The description's `50KB` and the notices' `50.0KB` are different strings
        // and must not be conflated.
        assert_eq!(format_size(DEFAULT_MAX_BYTES), "50.0KB");
        assert!(!FIND_DESCRIPTION.contains("50.0KB"));
    }

    /// The `--full-path` / `**/` rewrite table (TS `find.ts:243-252`), including
    /// the unreachable `pattern !== "**"` guard.
    #[tokio::test]
    async fn fd_argv_matches_the_ts_order() {
        // A tempdir has no `.git` ancestor, so `--no-require-git` is present.
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().to_string_lossy().into_owned();

        let args = fd_args("*.ts", &root, 1000.0).await;
        assert_eq!(
            args,
            vec![
                "--glob",
                "--color=never",
                "--hidden",
                "--no-require-git",
                "--max-results",
                "1000",
                "--",
                "*.ts",
                &root,
            ]
        );

        // A `/` in the pattern adds `--full-path` *and* the `**/` prefix.
        let args = fd_args("nested/*.txt", &root, 1000.0).await;
        assert_eq!(args[6], "--full-path");
        assert_eq!(args[8], "**/nested/*.txt");

        // Already `**/`-prefixed: not prefixed again.
        let args = fd_args("**/*.txt", &root, 1000.0).await;
        assert_eq!(args[6], "--full-path");
        assert_eq!(args[8], "**/*.txt");

        // Absolute pattern: `--full-path`, no prefix.
        let args = fd_args("/abs/*.txt", &root, 1000.0).await;
        assert_eq!(args[6], "--full-path");
        assert_eq!(args[8], "/abs/*.txt");

        // `**` holds no `/`, so it never enters the block: no `--full-path` at all,
        // which is why the `pattern !== "**"` guard is dead code.
        let args = fd_args("**", &root, 1000.0).await;
        assert!(!args.iter().any(|a| a == "--full-path"));
        assert_eq!(args[6], "--");
        assert_eq!(args[7], "**");
    }

    /// `--no-require-git` is emitted **only** outside a git repo (TS
    /// `find.ts:226-240`, pi issue #5960): inside one, `fd`'s default git-aware
    /// behaviour must stay on so a parent `.gitignore` stops at nested repo
    /// boundaries. The captured corpus ran outside a repo, so this is the only
    /// assertion that pins the flag being conditional at all.
    #[tokio::test]
    async fn no_require_git_is_dropped_inside_a_repo() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let search = tmp.path().join("src");
        std::fs::create_dir_all(&search).expect("mkdir");
        let search = search.to_string_lossy().into_owned();

        let args = fd_args("*.ts", &search, 1000.0).await;
        assert!(
            args.iter().any(|a| a == "--no-require-git"),
            "outside a repo the flag is present: {args:?}"
        );

        std::fs::create_dir_all(tmp.path().join(".git")).expect("mkdir .git");
        let args = fd_args("*.ts", &search, 1000.0).await;
        assert!(
            !args.iter().any(|a| a == "--no-require-git"),
            "inside a repo the flag must be gone: {args:?}"
        );
        // ...and nothing else shifted: the flag is simply absent.
        assert_eq!(args[3], "--max-results");
    }

    /// A directory *is* found by the ancestor walk, and its `.git` need not be a
    /// directory — `pathExists` is all Pi checks.
    #[tokio::test]
    async fn git_ancestor_walk_finds_a_dot_git_above_the_search_path() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path().join("repo");
        let deep = repo.join("a").join("b");
        std::fs::create_dir_all(&deep).expect("mkdir");
        assert!(!inside_git_repo(&deep.to_string_lossy()).await);
        std::fs::write(repo.join(".git"), b"gitdir: elsewhere").expect("write .git file");
        assert!(inside_git_repo(&deep.to_string_lossy()).await);
    }

    /// Node's readline contract: no empty trailing line, `\r\n` is one terminator.
    #[test]
    fn readline_lines_matches_node() {
        assert_eq!(readline_lines(""), Vec::<&str>::new());
        assert_eq!(readline_lines("a\nb\n"), vec!["a", "b"]);
        assert_eq!(readline_lines("a\nb"), vec!["a", "b"]);
        assert_eq!(readline_lines("a\r\nb\r\n"), vec!["a", "b"]);
        assert_eq!(readline_lines("a\rb"), vec!["a", "b"]);
        assert_eq!(readline_lines("\n"), vec![""]);
    }

    /// `path.relative` is what makes `fd`'s `/`-separated win32 output relativize
    /// correctly — and what drops the trailing separator the caller re-adds.
    #[test]
    fn relative_handles_the_separator_mismatch() {
        if Platform::current() != Platform::Win32 {
            return;
        }
        assert_eq!(win32_relative("C:\\t\\src", "C:/t/src/nested/"), "nested");
        assert_eq!(
            win32_relative("C:\\t\\src", "C:/t/src/nested/c.txt"),
            "nested\\c.txt"
        );
        // The fast path is separator-sensitive, so this is the branch actually taken
        // for `/`-separated `fd` output.
        assert!(!"C:/t/src/nested/".starts_with("C:\\t\\src"));
        // Sibling directories still produce `..` segments.
        assert_eq!(win32_relative("C:\\t\\src", "C:\\t\\out\\x"), "..\\out\\x");
        assert_eq!(win32_relative("C:\\t\\src", "C:\\t\\src"), "");
    }

    /// The posix flavour, which is the whole of `path.relative` on Linux/macOS.
    #[test]
    fn posix_relative_matches_node() {
        if Platform::current() != Platform::Posix {
            return;
        }
        assert_eq!(
            posix_relative("/t/src", "/t/src/nested/c.txt"),
            "nested/c.txt"
        );
        assert_eq!(posix_relative("/t/src", "/t/src/nested/"), "nested");
        assert_eq!(posix_relative("/t/src", "/t/out/x"), "../out/x");
        assert_eq!(posix_relative("/t/src", "/t/src"), "");
    }

    /// The fast path drops exactly the prefix plus one separator and does **not**
    /// posix-ify or touch the trailing separator — both are the callers' job, and
    /// Branch B's ordering of the two is what the corpus pins.
    #[test]
    fn relative_to_search_path_is_a_raw_prefix_strip() {
        let sep = std::path::MAIN_SEPARATOR;
        let search = format!("C:{sep}t{sep}src");
        assert_eq!(
            relative_to_search_path(&format!("{search}{sep}nested{sep}"), &search),
            format!("nested{sep}")
        );
        assert_eq!(
            relative_to_search_path(&format!("{search}{sep}a.ts"), &search),
            "a.ts"
        );
        // `p.slice(searchPath.length + 1)` past the end of the string is `""`.
        assert_eq!(relative_to_search_path(&search, &search), "");
    }

    /// Trailing-separator preservation is what turns `fd`'s directory marker into
    /// the `nested/` of the captured corpus, and the fixup runs *before*
    /// posix-ification — so native-separator output doubles the slash.
    #[test]
    fn trailing_separator_fixup_runs_before_posixification() {
        let apply = |line: &str, search: &str| {
            let had = line.ends_with('/') || line.ends_with('\\');
            let mut relative = relative_to_search_path(line, search);
            if had && !relative.ends_with('/') {
                relative.push('/');
            }
            to_posix_path(&relative)
        };
        // `/`-separated `fd` output on win32 takes the `path.relative` branch, which
        // strips the separator, so exactly one is re-added.
        if Platform::current() == Platform::Win32 {
            assert_eq!(apply("C:/t/src/nested/", "C:\\t\\src"), "nested/");
            // `\`-separated output takes the fast path and ends up doubled.
            assert_eq!(apply("C:\\t\\src\\nested\\", "C:\\t\\src"), "nested//");
        } else {
            assert_eq!(apply("/t/src/nested/", "/t/src"), "nested/");
        }
    }

    /// `details` collapses to `null` when empty and keeps insertion order
    /// otherwise.
    #[test]
    fn details_serializes_like_json_stringify() {
        assert_eq!(FindToolDetails::default().into_value(), Value::Null);

        let details = FindToolDetails {
            result_limit_reached: Some(1.0),
            truncation: None,
        };
        assert_eq!(
            serde_json::to_string(&details.into_value()).unwrap(),
            r#"{"resultLimitReached":1}"#
        );
    }

    /// The default operations' `glob` is Pi's placeholder, not a real walk.
    #[tokio::test]
    async fn default_glob_is_the_placeholder() {
        let ops = DefaultFindOperations;
        let results = ops
            .glob(
                "*",
                ".",
                GlobOptions {
                    ignore: &GLOB_IGNORE,
                    limit: 1000.0,
                },
            )
            .await;
        assert!(results.is_empty());
        assert_eq!(GLOB_IGNORE, ["**/node_modules/**", "**/.git/**"]);
    }
}
