//! Port of `core/tools/bash.ts` (UI-free half) — the `bash` tool.
//!
//! Spawns a one-shot bash via [`crate::output_accumulator`] for streaming output.
//! Gated by `tests/fixtures/pi/tools/{schemas,strings}/bash.json`; `execute` is not
//! oracle-captured (it would spawn a real shell and pull PIDs/timing into the
//! fixture), so its behaviour is pinned structurally against the TS source.
//!
//! # Ported items
//!
//! | TS | here |
//! | --- | --- |
//! | `MAX_TIMEOUT_MS` / `MAX_TIMEOUT_SECONDS` (`bash.ts:24-25`) | [`MAX_TIMEOUT_MS`] / [`MAX_TIMEOUT_SECONDS`] |
//! | `resolveTimeoutMs` (`bash.ts:27-38`) | [`resolve_timeout_ms`] |
//! | `bashSchema` (`bash.ts:40-43`) | [`bash_parameters`] |
//! | `BashToolInput` (`bash.ts:45`) | [`BashToolInput`] |
//! | `BashToolDetails` (`bash.ts:47-50`) | [`BashToolDetails`] |
//! | `BashOperations` (`bash.ts:56-74`) | [`BashOperations`] + [`BashExecOptions`] / [`BashExecResult`] |
//! | `createLocalBashOperations` (`bash.ts:82-148`) | [`create_local_bash_operations`] / [`LocalBashOperations`] |
//! | `BashSpawnContext` (`bash.ts:150-154`) | [`BashSpawnContext`] |
//! | `BashSpawnHook` (`bash.ts:156`) | [`BashSpawnHook`] |
//! | `resolveSpawnContext` (`bash.ts:158-161`) | [`resolve_spawn_context`] |
//! | `BashToolOptions` (`bash.ts:163-172`) | [`BashToolOptions`] |
//! | `BASH_UPDATE_THROTTLE_MS` (`bash.ts:175`) | [`BASH_UPDATE_THROTTLE_MS`] |
//! | `createBashToolDefinition` data + `execute` (`bash.ts:291-429`) | [`create_bash_tool_definition`] |
//! | `formatOutput` (`bash.ts:375-393`) | [`format_bash_output`] |
//! | `appendStatus` (`bash.ts:395`) | [`append_status`] |
//! | `createBashTool` (`bash.ts:468-470`) | [`create_bash_tool`] |
//! | `getShellConfig` / `getBashShellConfig` / `findBashOnPath` (`shell.ts:20-120`) | [`resolve_shell_config`] / [`get_bash_shell_config`] / [`ShellEnvironment::find_bash_on_path`] |
//! | `getShellEnv` (`shell.ts:122-134`) | [`get_shell_env`] |
//! | `trackDetachedChildPid` … (`shell.ts:180-195`) | [`track_detached_child_pid`], [`untrack_detached_child_pid`], [`kill_tracked_detached_children`] |
//! | `killProcessTree` (`shell.ts:200-225`) | [`kill_process_tree`] |
//! | `EXIT_STDIO_GRACE_MS` / `waitForChildProcess` (`child-process.ts:16`, `:49-137`) | [`EXIT_STDIO_GRACE_MS`] / [`wait_for_child_process`] |
//!
//! **Skipped — TUI only** (feat-006/007): `BASH_PREVIEW_LINES` (`bash.ts:174`),
//! `BashRenderState` / `BashResultRenderState` / `BashResultRenderComponent`
//! (`bash.ts:177-195`), `formatDuration` (`:197-199`), `formatBashCall`
//! (`:201-207`), `rebuildBashResultRenderComponent` (`:209-289`), `renderCall`
//! (`:430-439`) and `renderResult` (`:440-464`). `sanitizeBinaryOutput`
//! (`shell.ts:144-174`) is not referenced by the bash path at all.
//!
//! # The three truncation footers
//!
//! Exactly one of the three is appended, and only when the snapshot reports
//! truncation ([`format_bash_output`], TS `:379-391`). Two details are load-bearing
//! and easy to "fix" by accident:
//!
//! * The byte-limit footer hardcodes `formatSize(DEFAULT_MAX_BYTES)` (TS `:389`) —
//!   it prints `50.0KB` even if the snapshot's own `maxBytes` is something else.
//!   Unobservable through `execute` (which always builds the accumulator with Pi's
//!   defaults) but reproduced verbatim, and pinned by a direct
//!   [`format_bash_output`] test.
//! * Partial updates never carry a footer: the streaming emitter sends
//!   `snapshot.content` raw (TS `:325`) and only the final format appends one. So
//!   the footer appears exactly once, in the terminal result.
//!
//! # `details` is dropped on every error path
//!
//! The three error paths destructure only `text` from `formatOutput`
//! (TS `:409`, `:422-423`) and then `throw`. The truncation record and the temp-file
//! path therefore never reach the model as structure — only as the footer *text*
//! baked into the message. Rust's [`ToolError`] cannot carry `details` at all, so
//! the loss is structural here rather than incidental.
//!
//! # `exit_code == None` is a success
//!
//! `if (exitCode !== 0 && exitCode !== null)` (TS `:422`): a process that died from a
//! signal (`ExitStatus::code() == None`) is *not* reported as a failure. The only
//! failure signals are a nonzero code, an abort, or a timeout.
//!
//! # Deliberate divergences
//!
//! * **Temp-file prefix.** Pi passes `"pi-bash"` (TS `:313`); this port passes
//!   `"pirust-bash"`. The spill filename is not a wire format — nothing parses it,
//!   only the presence of a path is contractual — so the rename keeps pirust's
//!   files distinguishable from a concurrently running Pi's. Same rationale as
//!   [`crate::output_accumulator`]'s prefix note.
//! * **`onUpdate` is not optional.** Pi guards every streaming step with
//!   `if (!onUpdate)` (TS `:320`, `:341`, `:355`). pirust's
//!   [`AgentToolUpdateCallback`] is a plain `Arc<dyn Fn…>`, so the callback always
//!   exists and the empty first update (TS `:355-357`) always fires.
//! * **`AbortSignal` → [`CancellationToken`].** Pi's `signal?` may be `undefined`;
//!   a never-cancelled token is observationally identical for both uses
//!   (`signal?.aborted` and the `"abort"` listener).
//! * **Error identity is the message, exactly as in Pi.** `execute` classifies
//!   failures by `err.message` (TS `:410`, `:413`), so [`BashOperations`]
//!   implementations signal an abort/timeout by producing an error whose `Display`
//!   is `aborted` / `timeout:<secs>` — see [`AbortedError`] / [`TimeoutError`].
//! * **`process.kill` → a `kill(1)` spawn.** `libc` is not in this crate's
//!   dependency set and the crate `#![forbid(unsafe_code)]`, so the Unix branch of
//!   [`kill_process_tree`] shells out instead of issuing the syscall; see that
//!   function.
//! * **`detached: true` → `process_group(0)`.** Node's `detached` calls `setsid()`
//!   (new session *and* group); this port only creates a new process group, which
//!   is the half `kill(-pid)` depends on. See [`LocalBashOperations::exec`].
//! * **Spawn failure.** Node reports `ENOENT` through the child's `"error"` event,
//!   which `waitForChildProcess` turns into a rejection; here `Command::spawn`
//!   fails directly. Same control flow (the error propagates verbatim out of
//!   `execute`), different message text.

use std::collections::{BTreeMap, BTreeSet};
use std::future::pending;
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use pirust_agent_core::types::{AgentTool, AgentToolResult, AgentToolUpdateCallback, ToolError};
use pirust_ai::jsnum::js_number;
use pirust_ai::types::{TextContent, UserContent};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::binaries::{BinaryEnv, HomeDirUnavailable, Platform};
use crate::definition::schema::{number_prop, object_schema, optional, required, string_prop};
use crate::definition::PirustToolDefinition;
use crate::output_accumulator::{
    OutputAccumulator, OutputAccumulatorOptions, OutputSnapshot, SnapshotOptions,
};
use crate::truncate::{
    format_size, TruncatedBy, TruncationResult, DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES,
};

// ===========================================================================
// Static tool data (bash.ts:40-43, bash.ts:299-303)
// ===========================================================================

/// `name` / `label` (`bash.ts:299-300`) — the same string for this tool.
const BASH_NAME: &str = "bash";

/// `promptSnippet` (`bash.ts:302`).
const BASH_PROMPT_SNIPPET: &str = "Execute bash commands (ls, grep, find, etc.)";

/// Temp-file prefix handed to the accumulator. Pi's is `"pi-bash"` (`bash.ts:313`)
/// — see the module docs for why this port renames it.
const BASH_TEMP_FILE_PREFIX: &str = "pirust-bash";

/// `description` (`bash.ts:301`), a template literal over the two shared
/// truncation constants. `DEFAULT_MAX_BYTES / 1024` is exactly `50` in JS too (a
/// whole number formats without a decimal point), so the rendered string is
/// `… last 2000 lines or 50KB …`.
pub fn bash_description() -> String {
    format!(
        "Execute a bash command in the current working directory. Returns stdout and stderr. \
         Output is truncated to last {DEFAULT_MAX_LINES} lines or {}KB (whichever is hit first). \
         If truncated, full output is saved to a temp file. Optionally provide a timeout in seconds.",
        DEFAULT_MAX_BYTES / 1024
    )
}

/// `bashSchema` (`bash.ts:40-43`), built through the TypeBox-key-order helpers so
/// the bytes match `tests/fixtures/pi/tools/schemas/bash.json`.
pub fn bash_parameters() -> Value {
    object_schema([
        required("command", string_prop("Bash command to execute")),
        optional(
            "timeout",
            number_prop("Timeout in seconds (optional, no default timeout)"),
        ),
    ])
}

/// `Static<typeof bashSchema>` (`bash.ts:45`).
///
/// `timeout` is `f64` because Pi's is a JS `number`: fractional timeouts are legal
/// and are echoed back verbatim in the timeout message (`1.5` prints `1.5`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct BashToolInput {
    /// Bash command to execute.
    pub command: String,
    /// Timeout in seconds (optional, no default timeout).
    #[serde(default)]
    pub timeout: Option<f64>,
}

/// `BashToolDetails` (`bash.ts:47-50`) — the `details` payload of a *successful*,
/// truncated call, and of every streaming update.
///
/// Both fields are `Option` and skipped when absent, so `JSON.stringify` parity
/// holds: Pi emits `{}` for the update that precedes any truncation (both
/// properties `undefined`, `bash.ts:326-329`).
///
/// `full_output_path` is a `String` rather than a `PathBuf` because Pi's is a JS
/// string and a `PathBuf` cannot serialize when the platform path is not UTF-8;
/// the conversion is [`Path::to_string_lossy`], which is what Node does to the
/// bytes from `os.tmpdir()`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct BashToolDetails {
    /// The truncation record, present only when `truncation.truncated` (`bash.ts:48`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncation: Option<TruncationResult>,
    /// Path of the spill file holding the complete output (`bash.ts:49`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub full_output_path: Option<String>,
}

// ===========================================================================
// Timeout resolution (bash.ts:24-38)
// ===========================================================================

/// `MAX_TIMEOUT_MS` (`bash.ts:24`) — `setTimeout`'s 32-bit signed ceiling.
pub const MAX_TIMEOUT_MS: f64 = 2_147_483_647.0;

/// `MAX_TIMEOUT_SECONDS` (`bash.ts:25`) — `MAX_TIMEOUT_MS / 1000`, i.e.
/// `2147483.647`. That float spelling is what reaches the model, so the constant
/// is *formatted* with [`js_number`] rather than hand-written.
pub const MAX_TIMEOUT_SECONDS: f64 = MAX_TIMEOUT_MS / 1000.0;

/// The two `resolveTimeoutMs` rejections (`bash.ts:30`, `bash.ts:35`), verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum InvalidTimeout {
    /// `!Number.isFinite(timeout) || timeout <= 0` (`bash.ts:29-31`).
    #[error("Invalid timeout: must be a finite number of seconds")]
    NotAFinitePositiveNumber,
    /// `timeout * 1000 > MAX_TIMEOUT_MS` (`bash.ts:34-36`).
    #[error(
        "Invalid timeout: maximum is {} seconds",
        js_number(MAX_TIMEOUT_SECONDS)
    )]
    AboveMaximum,
}

/// `resolveTimeoutMs(timeout)` (`bash.ts:27-38`): seconds → milliseconds.
///
/// `None` passes straight through — **there is no default timeout**, which is why
/// the schema's description says so. `NaN`, `Infinity` and anything `<= 0` are
/// rejected; so is a value whose millisecond form exceeds [`MAX_TIMEOUT_MS`].
///
/// Milliseconds stay `f64` because Pi's do: `1.5` seconds is `1500`, and
/// `0.0015` seconds is `1.5` ms, which `setTimeout` accepts.
pub fn resolve_timeout_ms(timeout: Option<f64>) -> Result<Option<f64>, InvalidTimeout> {
    let Some(timeout) = timeout else {
        return Ok(None);
    };
    if !timeout.is_finite() || timeout <= 0.0 {
        return Err(InvalidTimeout::NotAFinitePositiveNumber);
    }

    let timeout_ms = timeout * 1000.0;
    if timeout_ms > MAX_TIMEOUT_MS {
        return Err(InvalidTimeout::AboveMaximum);
    }
    Ok(Some(timeout_ms))
}

// ===========================================================================
// Shell resolution (shell.ts:6-120)
// ===========================================================================

/// How the command reaches the shell (`ShellConfig.commandTransport`,
/// `shell.ts:9`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CommandTransport {
    /// The command is the final `argv` entry, after [`ShellConfig::args`]. Pi
    /// leaves `commandTransport` `undefined` for this case (`shell.ts:21`).
    #[default]
    Argv,
    /// The command is written to the shell's stdin — the legacy-WSL `bash -s`
    /// case (`shell.ts:21`).
    Stdin,
}

/// `ShellConfig` (`shell.ts:6-10`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellConfig {
    /// Executable to spawn.
    pub shell: String,
    /// Fixed arguments. **Always** `["-c"]` or `["-s"]` — a login (`-l`) or
    /// interactive (`-i`) shell is never requested.
    pub args: Vec<String>,
    /// Where the command text goes.
    pub command_transport: CommandTransport,
}

/// The two `getShellConfig` rejections (`shell.ts:73`, `shell.ts:100-106`).
///
/// [`Self::NoBashShellFound`] is the multi-line message verbatim, including the
/// `Searched Git Bash in:` list. When neither `%ProgramFiles%` nor
/// `%ProgramFiles(x86)%` is set the list is empty and JS's `[].join("\n")` yields
/// `""`, so the message ends with a bare newline — reproduced.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ShellConfigError {
    /// An explicit `shellPath` that does not exist (`shell.ts:73`).
    #[error("Custom shell path not found: {0}")]
    CustomShellPathNotFound(String),
    /// Windows only: no Git Bash, and nothing named `bash.exe` on `PATH`
    /// (`shell.ts:100-106`).
    #[error(
        "No bash shell found. Options:\n  \
         1. Install Git for Windows: https://git-scm.com/download/win\n  \
         2. Add your bash to PATH (Cygwin, MSYS2, etc.)\n  \
         3. Set shellPath in settings.json\n\n\
         Searched Git Bash in:\n{searched}"
    )]
    NoBashShellFound {
        /// `paths.map((p) => `  ${p}`).join("\n")` (`shell.ts:105`).
        searched: String,
    },
}

/// `isLegacyWslBashPath(path)` (`shell.ts:15-18`).
///
/// Pi's regex is `^[a-z]:\\windows\\(?:system32|sysnative)\\bash\.exe$` applied
/// after `replace(/\//g, "\\")` + `toLowerCase()`. Hand-rolled here because no
/// regex crate is in this crate's dependency set; the character-class check is
/// `is_ascii_lowercase` *after* lowercasing, i.e. exactly `[a-z]`.
///
/// `to_lowercase` (Unicode, like JS's `toLowerCase`) rather than
/// `to_ascii_lowercase`: the comparison operands are ASCII, so the only way the
/// two differ is a non-ASCII character that lowercases into ASCII — and both
/// implementations then agree.
pub fn is_legacy_wsl_bash_path(path: &str) -> bool {
    let normalized = path.replace('/', "\\").to_lowercase();
    let Some(rest) = normalized.strip_prefix(|c: char| c.is_ascii_lowercase()) else {
        return false;
    };
    let Some(rest) = rest.strip_prefix(":\\windows\\") else {
        return false;
    };
    let Some(rest) = rest
        .strip_prefix("system32\\")
        .or_else(|| rest.strip_prefix("sysnative\\"))
    else {
        return false;
    };
    rest == "bash.exe"
}

/// `getBashShellConfig(shell)` (`shell.ts:20-22`): every resolved shell is
/// bash-family and one-shot. The legacy-WSL `bash.exe` shim mangles `-c`
/// arguments, so it gets `-s` and the command on stdin instead.
pub fn get_bash_shell_config(shell: &str) -> ShellConfig {
    if is_legacy_wsl_bash_path(shell) {
        ShellConfig {
            shell: shell.to_string(),
            args: vec!["-s".to_string()],
            command_transport: CommandTransport::Stdin,
        }
    } else {
        ShellConfig {
            shell: shell.to_string(),
            args: vec!["-c".to_string()],
            command_transport: CommandTransport::Argv,
        }
    }
}

/// Everything `getShellConfig` reads from the outside world: `process.platform`,
/// `process.env`, `existsSync` and the `where`/`which` probe.
///
/// Injectable so the whole resolution ladder — including the Windows-only
/// branches and the "no bash anywhere" message — is testable on any host without
/// the real binaries.
#[async_trait]
pub trait ShellEnvironment: Send + Sync {
    /// `process.platform` (`shell.ts:26`, `:76`).
    fn platform(&self) -> Platform;

    /// `process.env[key]` (`shell.ts:79`, `:83`).
    fn env_var(&self, key: &str) -> Option<String>;

    /// `existsSync(path)` (`shell.ts:35`, `:70`, `:89`, `:110`).
    fn exists(&self, path: &str) -> bool;

    /// `findBashOnPath()` (`shell.ts:24-58`).
    async fn find_bash_on_path(&self) -> Option<String>;
}

/// The real environment: `os`/`fs`/`spawnSync` as Pi uses them.
#[derive(Debug, Clone, Copy, Default)]
pub struct LocalShellEnvironment;

#[async_trait]
impl ShellEnvironment for LocalShellEnvironment {
    fn platform(&self) -> Platform {
        Platform::current()
    }

    fn env_var(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }

    fn exists(&self, path: &str) -> bool {
        Path::new(path).exists()
    }

    /// `findBashOnPath()` (`shell.ts:24-58`).
    ///
    /// Windows runs `where bash.exe` and *re-verifies* the first hit with
    /// `existsSync`, because `where` can report stale entries (`shell.ts:33-37`);
    /// Unix runs `which bash` and trusts the answer, which is what makes Termux
    /// and other special filesystems work (`shell.ts:45-52`).
    ///
    /// Pi blocks the event loop with `spawnSync(…, { timeout: 5000 })`; this
    /// awaits the child under the same 5 s cap.
    async fn find_bash_on_path(&self) -> Option<String> {
        let windows = self.platform() == Platform::Win32;
        let (program, argument) = if windows {
            ("where", "bash.exe")
        } else {
            ("which", "bash")
        };

        // `Command::output` captures stdout and stderr and gives the child a null
        // stdin, which is what `spawnSync(…, { encoding: "utf-8" })` does too.
        let mut command = Command::new(program);
        command.arg(argument);
        if windows {
            hide_window(&mut command);
        }
        let output = tokio::time::timeout(Duration::from_secs(5), command.output())
            .await
            .ok()?
            .ok()?;
        // `if (result.status === 0 && result.stdout)`.
        if !output.status.success() {
            return None;
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        // `result.stdout.trim().split(/\r?\n/)[0]`.
        let first_match = stdout.trim().lines().next().unwrap_or_default();
        if first_match.is_empty() {
            return None;
        }
        if windows && !self.exists(first_match) {
            return None;
        }
        Some(first_match.to_string())
    }
}

/// `getShellConfig(customShellPath)` (`shell.ts:67-120`), against an injected
/// [`ShellEnvironment`].
///
/// Resolution order, exactly as Pi:
/// 1. an explicit `shellPath` (must exist). Pi's guard is `if (customShellPath)`,
///    so an *empty* string is falsy and falls through to the platform ladder
///    rather than failing;
/// 2. on Windows: `%ProgramFiles%\Git\bin\bash.exe`, then
///    `%ProgramFiles(x86)%\Git\bin\bash.exe`, then `bash.exe` on `PATH`; no
///    `cmd.exe` and no PowerShell fallback ever — the call fails instead;
/// 3. on Unix: `/bin/bash`, then `bash` on `PATH`, then a bare `sh -c`.
///
/// Note that the Unix `sh` fallback goes through neither `existsSync` nor
/// [`get_bash_shell_config`]: Pi returns the literal `{ shell: "sh", args: ["-c"] }`
/// (`shell.ts:119`).
pub async fn resolve_shell_config(
    env: &dyn ShellEnvironment,
    custom_shell_path: Option<&str>,
) -> Result<ShellConfig, ShellConfigError> {
    // 1. Check user-specified shell path (`shell.ts:69-74`).
    if let Some(custom) = custom_shell_path.filter(|path| !path.is_empty()) {
        if env.exists(custom) {
            return Ok(get_bash_shell_config(custom));
        }
        return Err(ShellConfigError::CustomShellPathNotFound(
            custom.to_string(),
        ));
    }

    if env.platform() == Platform::Win32 {
        // 2. Try Git Bash in known locations (`shell.ts:77-92`).
        let mut paths: Vec<String> = Vec::new();
        // `if (programFiles)` — JS truthiness, so an *empty* variable contributes
        // no candidate (and no line to the "Searched Git Bash in:" list).
        if let Some(program_files) = env.env_var("ProgramFiles").filter(|v| !v.is_empty()) {
            paths.push(format!("{program_files}\\Git\\bin\\bash.exe"));
        }
        if let Some(program_files_x86) = env.env_var("ProgramFiles(x86)").filter(|v| !v.is_empty())
        {
            paths.push(format!("{program_files_x86}\\Git\\bin\\bash.exe"));
        }
        for path in &paths {
            if env.exists(path) {
                return Ok(get_bash_shell_config(path));
            }
        }

        // 3. Fallback: search bash.exe on PATH (Cygwin, MSYS2, WSL, …)
        // (`shell.ts:95-98`).
        if let Some(bash_on_path) = env.find_bash_on_path().await {
            return Ok(get_bash_shell_config(&bash_on_path));
        }

        return Err(ShellConfigError::NoBashShellFound {
            searched: paths
                .iter()
                .map(|path| format!("  {path}"))
                .collect::<Vec<_>>()
                .join("\n"),
        });
    }

    // Unix: try /bin/bash, then bash on PATH, then fallback to sh
    // (`shell.ts:110-119`).
    if env.exists("/bin/bash") {
        return Ok(get_bash_shell_config("/bin/bash"));
    }
    if let Some(bash_on_path) = env.find_bash_on_path().await {
        return Ok(get_bash_shell_config(&bash_on_path));
    }
    Ok(ShellConfig {
        shell: "sh".to_string(),
        args: vec!["-c".to_string()],
        command_transport: CommandTransport::Argv,
    })
}

/// [`resolve_shell_config`] against the real environment (`shell.ts:67`).
pub async fn get_shell_config(
    custom_shell_path: Option<&str>,
) -> Result<ShellConfig, ShellConfigError> {
    resolve_shell_config(&LocalShellEnvironment, custom_shell_path).await
}

// ===========================================================================
// Shell environment (shell.ts:122-134)
// ===========================================================================

/// `NodeJS.ProcessEnv` — the environment handed to the shell.
///
/// A `BTreeMap` rather than an insertion-ordered map: variable order is
/// unobservable to `Command::envs`, and a deterministic order keeps tests stable.
/// The one place Pi's ordering could matter is the case-insensitive `PATH` lookup
/// below, which picks the first matching key; with two differently-cased `path`
/// keys (impossible on Windows, where the OS folds them) the choice could differ.
pub type EnvMap = BTreeMap<String, String>;

/// `path.delimiter` — `;` on Windows, `:` elsewhere (`shell.ts:2`, `:126`).
fn path_delimiter() -> char {
    if Platform::current() == Platform::Win32 {
        ';'
    } else {
        ':'
    }
}

/// `getShellEnv()` (`shell.ts:122-134`): the process environment with the managed
/// bin dir prepended to `PATH`, unless it is already an entry.
///
/// The `PATH` key is found case-insensitively and *reused*, so a Windows `Path`
/// stays `Path` (`shell.ts:124`); with no path variable at all the literal `PATH`
/// is created.
///
/// The bin dir is [`BinaryEnv::tools_dir`] — pirust's analogue of Pi's
/// `getBinDir()` (`config.ts:549-551`), i.e. `~/.pirust/agent/bin`. Pi's
/// `getBinDir()` throws when the home directory cannot be resolved; that becomes
/// the [`HomeDirUnavailable`] error here, and it propagates out of `execute` the
/// same way.
pub fn get_shell_env() -> Result<EnvMap, HomeDirUnavailable> {
    let bin_dir = BinaryEnv::from_process_env().tools_dir()?;
    let bin_dir = bin_dir.to_string_lossy().into_owned();

    // `vars_os` + lossy, not `vars`: `std::env::vars` panics on a non-UTF-8
    // variable, whereas Node lossily decodes `environ` into JS strings.
    let mut env: EnvMap = std::env::vars_os()
        .map(|(key, value)| {
            (
                key.to_string_lossy().into_owned(),
                value.to_string_lossy().into_owned(),
            )
        })
        .collect();

    let path_key = env
        .keys()
        .find(|key| key.eq_ignore_ascii_case("path"))
        .cloned()
        .unwrap_or_else(|| "PATH".to_string());
    let current_path = env.get(&path_key).cloned().unwrap_or_default();

    let delimiter = path_delimiter();
    // `currentPath.split(delimiter).filter(Boolean)`, then an exact-string
    // `includes` — no normalization, no case folding.
    let has_bin_dir = current_path
        .split(delimiter)
        .filter(|entry| !entry.is_empty())
        .any(|entry| entry == bin_dir);
    // `[binDir, currentPath].filter(Boolean).join(delimiter)`.
    let updated_path = if has_bin_dir {
        current_path
    } else if current_path.is_empty() {
        bin_dir
    } else {
        format!("{bin_dir}{delimiter}{current_path}")
    };

    env.insert(path_key, updated_path);
    Ok(env)
}

// ===========================================================================
// Process-tree bookkeeping (shell.ts:176-225)
// ===========================================================================

/// Detached child processes must be tracked so they can be killed on parent
/// shutdown signals (SIGHUP/SIGTERM) — `trackedDetachedChildPids`
/// (`shell.ts:180`).
static TRACKED_DETACHED_CHILD_PIDS: Mutex<BTreeSet<u32>> = Mutex::new(BTreeSet::new());

/// `trackDetachedChildPid(pid)` (`shell.ts:182-184`).
pub fn track_detached_child_pid(pid: u32) {
    lock(&TRACKED_DETACHED_CHILD_PIDS).insert(pid);
}

/// `untrackDetachedChildPid(pid)` (`shell.ts:186-188`).
pub fn untrack_detached_child_pid(pid: u32) {
    lock(&TRACKED_DETACHED_CHILD_PIDS).remove(&pid);
}

/// `killTrackedDetachedChildren()` (`shell.ts:190-195`).
pub fn kill_tracked_detached_children() {
    let pids: Vec<u32> = std::mem::take(&mut *lock(&TRACKED_DETACHED_CHILD_PIDS))
        .into_iter()
        .collect();
    for pid in pids {
        kill_process_tree(pid);
    }
}

/// Untracks a pid on every exit path — Pi's `finally` block (`bash.ts:141-145`).
struct TrackedPid(Option<u32>);

impl Drop for TrackedPid {
    fn drop(&mut self) {
        if let Some(pid) = self.0 {
            untrack_detached_child_pid(pid);
        }
    }
}

/// `killProcessTree(pid)` (`shell.ts:200-225`): kill a process and all its
/// children, swallowing every failure.
///
/// * Windows: `taskkill /F /T /PID <pid>`, spawned detached with stdio ignored
///   and the console window hidden (`shell.ts:203-208`).
/// * Unix: `process.kill(-pid, "SIGKILL")`, falling back to `process.kill(pid,
///   "SIGKILL")` when the process-group kill fails (`shell.ts:214-223`).
///
/// **Divergence.** Pi issues the Unix kills as syscalls. `libc` is not in this
/// crate's dependency set and the crate is `#![forbid(unsafe_code)]`, so this
/// port spawns `sh -c 'kill -9 -<pid> || kill -9 <pid>'`: one process instead of
/// a syscall, but the same two attempts in the same order with the same signal,
/// and the shell's `||` reproduces the fallback that a Rust-level spawn could not
/// (a failing `kill` reports through its exit status, not through the spawn).
pub fn kill_process_tree(pid: u32) {
    let mut command = if Platform::current() == Platform::Win32 {
        let mut command = std::process::Command::new("taskkill");
        command.args(["/F", "/T", "/PID", &pid.to_string()]);
        hide_window_std(&mut command);
        command
    } else {
        let mut command = std::process::Command::new("sh");
        command.arg("-c");
        command.arg(format!("kill -9 -{pid} || kill -9 {pid}"));
        command
    };
    // `stdio: "ignore"`, `detached: true` — fire and forget, and never inherit
    // this process's streams.
    let _ = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

/// `windowsHide: true` for a [`tokio::process::Command`] (`shell.ts:31`, `:207`,
/// `bash.ts:102`).
///
/// Node's `windowsHide` maps to libuv's `UV_PROCESS_WINDOWS_HIDE`, which sets
/// `CREATE_NO_WINDOW` for console applications; that flag is what this reproduces.
/// A no-op elsewhere, because the option itself is Windows-only in Node.
fn hide_window(command: &mut Command) {
    #[cfg(windows)]
    {
        // `CREATE_NO_WINDOW` (winbase.h).
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    {
        let _ = command;
    }
}

/// [`hide_window`] for a blocking [`std::process::Command`].
fn hide_window_std(command: &mut std::process::Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // `CREATE_NO_WINDOW` (winbase.h).
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    {
        let _ = command;
    }
}

// ===========================================================================
// waitForChildProcess (child-process.ts:16, :38-137)
// ===========================================================================

/// `EXIT_STDIO_GRACE_MS` (`child-process.ts:16`).
pub const EXIT_STDIO_GRACE_MS: u64 = 100;

/// One event from a child's stdout or stderr, in arrival order.
enum StdioEvent {
    /// A `"data"` event.
    Data(Vec<u8>),
    /// An `"end"` event.
    End,
}

/// Pumps one pipe into the event channel. Node's stream does this itself; here a
/// task per pipe is what makes both pipes' `"data"` events interleave in arrival
/// order on the single consumer.
///
/// A read *error* is reported as `End`: Node would emit `"error"` (which
/// `waitForChildProcess` does not listen for) and then rely on `"close"` or the
/// idle timer, so ending the pipe here reaches the same resolution one grace
/// period earlier.
fn spawn_stdio_reader<R>(mut reader: R, tx: UnboundedSender<StdioEvent>) -> JoinHandle<()>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut buffer = vec![0u8; 64 * 1024];
        loop {
            match reader.read(&mut buffer).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx.send(StdioEvent::Data(buffer[..n].to_vec())).is_err() {
                        return;
                    }
                }
            }
        }
        let _ = tx.send(StdioEvent::End);
    })
}

/// Aborts the pipe pumps when [`wait_for_child_process`] resolves — Pi's
/// `child.stdout?.destroy()` (`child-process.ts:76-77`). Without it a detached
/// descendant holding the pipe open would keep the readers (and `onData`) alive
/// forever.
struct ReaderTasks(Vec<JoinHandle<()>>);

impl Drop for ReaderTasks {
    fn drop(&mut self) {
        for handle in &self.0 {
            handle.abort();
        }
    }
}

/// `sleep_until` for an optional deadline. The deadline is *absolute*, so
/// re-creating this future inside a `select!` loop re-arms nothing — only an
/// explicit new deadline moves it.
async fn sleep_until_optional(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => pending().await,
    }
}

/// `waitForChildProcess(child)` (`child-process.ts:38-137`): wait for a child to
/// terminate without hanging on inherited stdio handles.
///
/// A short-lived child can exit while a detached descendant keeps its
/// stdout/stderr pipe open. Resolving on a fixed deadline measured from exit
/// would silently lose output still being written (earendil-works/pi#5303), so
/// after exit this waits for the pipes to fall *idle*: the
/// [`EXIT_STDIO_GRACE_MS`] timer is re-armed on every chunk, so an actively
/// writing descendant keeps us reading while a quiet inherited handle still
/// releases us.
///
/// Resolution, in Pi's terms: `close` (both pipes ended *and* the process
/// exited), or the re-armable post-exit idle timer. `Err` corresponds to the
/// child's `"error"` event.
///
/// Both pipes share the single `on_data` callback, so stdout and stderr interleave
/// in arrival order with no tagging (`bash.ts:124-125`).
pub async fn wait_for_child_process(
    child: &mut Child,
    on_data: &BashOnData,
) -> std::io::Result<Option<i32>> {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut tasks = Vec::new();
    let mut open_pipes = 0usize;
    if let Some(stdout) = child.stdout.take() {
        tasks.push(spawn_stdio_reader(stdout, tx.clone()));
        open_pipes += 1;
    }
    if let Some(stderr) = child.stderr.take() {
        tasks.push(spawn_stdio_reader(stderr, tx.clone()));
        open_pipes += 1;
    }
    drop(tx);
    let _readers = ReaderTasks(tasks);

    let mut exited = false;
    let mut exit_code: Option<i32> = None;
    let mut ended_pipes = 0usize;
    let mut idle_deadline: Option<Instant> = None;
    // A closed channel yields `Ready(None)` forever, so the branch must be
    // *disabled* once drained — with `biased` it would otherwise starve every
    // later branch. (`select!` can never end up with all branches disabled:
    // whenever `exited` is set without returning, an idle deadline is armed.)
    let mut draining = true;

    loop {
        tokio::select! {
            // Drain output first: a queued chunk must never lose a race to the
            // idle timer, which in Node can only fire on an idle event loop.
            biased;

            event = rx.recv(), if draining => match event {
                Some(StdioEvent::Data(data)) => {
                    on_data(&data);
                    // Output is still arriving after exit; defer finalizing so we
                    // don't destroy the stream mid-write and truncate the tail.
                    if exited {
                        idle_deadline = Some(Instant::now() + Duration::from_millis(EXIT_STDIO_GRACE_MS));
                    }
                }
                Some(StdioEvent::End) => {
                    ended_pipes += 1;
                    // `maybeFinalizeAfterExit` — this is Pi's `close`.
                    if exited && ended_pipes == open_pipes {
                        return Ok(exit_code);
                    }
                }
                // Every reader is gone; nothing more can arrive.
                None => {
                    draining = false;
                    if exited {
                        return Ok(exit_code);
                    }
                }
            },
            status = child.wait(), if !exited => {
                exited = true;
                // `code` is `None` for a signalled child, exactly Node's `null`.
                exit_code = status?.code();
                if ended_pipes == open_pipes {
                    return Ok(exit_code);
                }
                idle_deadline = Some(Instant::now() + Duration::from_millis(EXIT_STDIO_GRACE_MS));
            },
            () = sleep_until_optional(idle_deadline), if idle_deadline.is_some() => {
                return Ok(exit_code);
            },
        }
    }
}

// ===========================================================================
// BashOperations (bash.ts:52-148)
// ===========================================================================

/// The shared stdout/stderr sink. Pi's `onData: (data: Buffer) => void`
/// (`bash.ts:68`).
///
/// `Arc` rather than a borrow so a [`BashOperations`] implementation can hand it
/// to spawned tasks. It is invoked from within the Tokio runtime, which the
/// throttled emitter relies on (it arms timers with [`tokio::spawn`]) — the same
/// requirement Node's `setTimeout` places on the event loop.
pub type BashOnData = Arc<dyn Fn(&[u8]) + Send + Sync>;

/// `BashOperations.exec`'s options object (`bash.ts:66-72`).
#[derive(Clone)]
pub struct BashExecOptions {
    /// Sink for every stdout and stderr chunk.
    pub on_data: BashOnData,
    /// Pi's `signal?: AbortSignal`. Always present here; a token that is never
    /// cancelled behaves like `undefined`.
    pub token: CancellationToken,
    /// Timeout in **seconds**, raw from the tool arguments (`bash.ts:71`).
    pub timeout: Option<f64>,
    /// Environment for the child; `None` means "use [`get_shell_env`]"
    /// (`bash.ts:100`).
    pub env: Option<EnvMap>,
}

impl std::fmt::Debug for BashExecOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BashExecOptions")
            .field("on_data", &"<fn>")
            .field("token", &self.token)
            .field("timeout", &self.timeout)
            .field("env", &self.env.as_ref().map(|env| env.len()))
            .finish()
    }
}

/// `Promise<{ exitCode: number | null }>` (`bash.ts:73`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BashExecResult {
    /// Exit code, or `None` when the process was killed by a signal. **`None` is
    /// a success** — see the module docs.
    pub exit_code: Option<i32>,
}

/// `new Error("aborted")` (`bash.ts:87`, `:135`) — recognised by `execute` by its
/// message, so a custom [`BashOperations`] can produce any error type whose
/// `Display` is `aborted`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("aborted")]
pub struct AbortedError;

/// `new Error(`timeout:${timeout}`)` (`bash.ts:138`). The payload is the raw
/// `timeout` argument rendered by JS number-to-string, which is what `execute`
/// echoes back — so `1.5` stays `1.5`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("timeout:{0}")]
pub struct TimeoutError(pub String);

/// Pluggable operations for the bash tool (`bash.ts:52-74`).
///
/// Override these to delegate command execution to remote systems (for example
/// SSH). This is also the seam that makes `execute` testable without a real
/// shell.
#[async_trait]
pub trait BashOperations: Send + Sync {
    /// Execute a command and stream output (`bash.ts:64-73`).
    ///
    /// Returns the exit code (`None` if killed). Aborts and timeouts are reported
    /// as errors whose messages are `aborted` and `timeout:<secs>`; anything else
    /// propagates verbatim to the model.
    async fn exec(
        &self,
        command: &str,
        cwd: &str,
        options: BashExecOptions,
    ) -> Result<BashExecResult, ToolError>;
}

/// pi's built-in local shell execution backend (`createLocalBashOperations`,
/// `bash.ts:82-148`).
#[derive(Debug, Clone, Default)]
pub struct LocalBashOperations {
    /// Optional explicit shell path from settings (`bash.ts:82`).
    pub shell_path: Option<String>,
}

/// `createLocalBashOperations(options)` (`bash.ts:82-148`).
///
/// Useful for extensions that intercept `user_bash` and still want pi's standard
/// local shell behavior while wrapping or rewriting commands.
pub fn create_local_bash_operations(shell_path: Option<String>) -> Arc<dyn BashOperations> {
    Arc::new(LocalBashOperations { shell_path })
}

#[async_trait]
impl BashOperations for LocalBashOperations {
    /// `bash.ts:84-146`, in Pi's exact order: resolve the timeout, check the
    /// abort signal, resolve the shell (**before** the cwd check, so a machine
    /// with no bash reports that rather than a bad cwd), then verify the cwd.
    async fn exec(
        &self,
        command: &str,
        cwd: &str,
        options: BashExecOptions,
    ) -> Result<BashExecResult, ToolError> {
        let timeout_ms = resolve_timeout_ms(options.timeout)?;
        if options.token.is_cancelled() {
            return Err(AbortedError.into());
        }
        let shell_config = get_shell_config(self.shell_path.as_deref()).await?;
        // `fsAccess(cwd, constants.F_OK)` (`bash.ts:91-94`).
        if tokio::fs::metadata(cwd).await.is_err() {
            return Err(format!(
                "Working directory does not exist: {cwd}\nCannot execute bash commands."
            )
            .into());
        }

        let command_from_stdin = shell_config.command_transport == CommandTransport::Stdin;
        let mut spawn = Command::new(&shell_config.shell);
        spawn.args(&shell_config.args);
        if !command_from_stdin {
            spawn.arg(command);
        }
        spawn.current_dir(cwd);
        // `stdio: [commandFromStdin ? "pipe" : "ignore", "pipe", "pipe"]`
        // (`bash.ts:101`) — stdin is *never* inherited, so a command that reads
        // stdin sees EOF instead of stealing the user's terminal.
        spawn.stdin(if command_from_stdin {
            Stdio::piped()
        } else {
            Stdio::null()
        });
        spawn.stdout(Stdio::piped()).stderr(Stdio::piped());
        // `env: env ?? getShellEnv()` (`bash.ts:100`). Node replaces the whole
        // environment when `env` is given, hence `env_clear`.
        spawn.env_clear().envs(match options.env {
            Some(env) => env,
            None => get_shell_env()?,
        });
        // `detached: process.platform !== "win32"` (`bash.ts:99`) — see the
        // module docs for the setsid-vs-process-group divergence.
        #[cfg(unix)]
        spawn.process_group(0);
        hide_window(&mut spawn);

        // Node surfaces a spawn failure through the child's `"error"` event; here
        // it is returned directly. Either way it propagates verbatim.
        let mut child = spawn.spawn()?;

        if command_from_stdin {
            // `child.stdin?.on("error", () => {}); child.stdin?.end(command)`
            // (`bash.ts:104-107`) — fire and forget, errors ignored.
            if let Some(mut stdin) = child.stdin.take() {
                let command = command.to_string();
                tokio::spawn(async move {
                    let _ = stdin.write_all(command.as_bytes()).await;
                    let _ = stdin.shutdown().await;
                });
            }
        }

        let pid = child.id();
        if let Some(pid) = pid {
            track_detached_child_pid(pid);
        }
        // Pi's `finally` (`bash.ts:141-145`).
        let _tracked = TrackedPid(pid);

        let mut timed_out = false;
        let mut abort_handled = false;
        // Absolute deadline: the branch's future is re-created on every loop
        // iteration, so a relative sleep would restart the clock.
        let timeout_deadline =
            timeout_ms.map(|ms| Instant::now() + Duration::from_secs_f64(ms / 1000.0));

        let exit_code = {
            let mut wait = std::pin::pin!(wait_for_child_process(&mut child, &options.on_data));
            loop {
                tokio::select! {
                    result = &mut wait => break result?,
                    // `setTimeout(() => { timedOut = true; killProcessTree(pid) })`
                    // (`bash.ts:118-121`).
                    () = sleep_until_optional(timeout_deadline), if !timed_out && timeout_deadline.is_some() => {
                        timed_out = true;
                        if let Some(pid) = pid {
                            kill_process_tree(pid);
                        }
                    }
                    // `signal.addEventListener("abort", onAbort, { once: true })`
                    // (`bash.ts:111-113`, `:127-130`).
                    () = options.token.cancelled(), if !abort_handled => {
                        abort_handled = true;
                        if let Some(pid) = pid {
                            kill_process_tree(pid);
                        }
                    }
                }
            }
        };

        // `bash.ts:134-139` — the abort check precedes the timeout check, so an
        // abort that races a timeout reports `aborted`.
        if options.token.is_cancelled() {
            return Err(AbortedError.into());
        }
        if timed_out {
            return Err(TimeoutError(js_number(options.timeout.unwrap_or_default())).into());
        }
        Ok(BashExecResult { exit_code })
    }
}

// ===========================================================================
// Spawn context + tool options (bash.ts:150-172)
// ===========================================================================

/// `BashSpawnContext` (`bash.ts:150-154`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BashSpawnContext {
    /// The command as it will be executed — already prefixed by
    /// [`BashToolOptions::command_prefix`].
    pub command: String,
    /// Working directory.
    pub cwd: String,
    /// Environment for the child.
    pub env: EnvMap,
}

/// `BashSpawnHook` (`bash.ts:156`): a last chance to rewrite the command, cwd or
/// environment. Synchronous and total, like Pi's.
pub type BashSpawnHook = Arc<dyn Fn(BashSpawnContext) -> BashSpawnContext + Send + Sync>;

/// `resolveSpawnContext(command, cwd, spawnHook)` (`bash.ts:158-161`).
///
/// The base environment is always a fresh [`get_shell_env`] copy (Pi's
/// `{ ...getShellEnv() }`), so a hook that mutates it cannot leak into the next
/// call.
pub fn resolve_spawn_context(
    command: String,
    cwd: &str,
    spawn_hook: Option<&BashSpawnHook>,
) -> Result<BashSpawnContext, HomeDirUnavailable> {
    let base_context = BashSpawnContext {
        command,
        cwd: cwd.to_string(),
        env: get_shell_env()?,
    };
    Ok(match spawn_hook {
        Some(hook) => hook(base_context),
        None => base_context,
    })
}

/// `BashToolOptions` (`bash.ts:163-172`).
#[derive(Clone, Default)]
pub struct BashToolOptions {
    /// Custom operations for command execution. Default: local shell
    /// ([`LocalBashOperations`]).
    pub operations: Option<Arc<dyn BashOperations>>,
    /// Command prefix prepended to every command (for example shell setup
    /// commands).
    pub command_prefix: Option<String>,
    /// Optional explicit shell path from settings.
    pub shell_path: Option<String>,
    /// Hook to adjust command, cwd, or env before execution.
    pub spawn_hook: Option<BashSpawnHook>,
}

impl std::fmt::Debug for BashToolOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BashToolOptions")
            .field("operations", &self.operations.as_ref().map(|_| "<ops>"))
            .field("command_prefix", &self.command_prefix)
            .field("shell_path", &self.shell_path)
            .field("spawn_hook", &self.spawn_hook.as_ref().map(|_| "<fn>"))
            .finish()
    }
}

// ===========================================================================
// Output formatting (bash.ts:375-395)
// ===========================================================================

/// `formatOutput`'s return value (`bash.ts:392`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BashFormattedOutput {
    /// The text sent to the model, truncation footer included.
    pub text: String,
    /// `details`, present **only** when the snapshot is truncated.
    pub details: Option<BashToolDetails>,
}

/// `${path}` for a possibly-absent path: JS interpolates `undefined` as the
/// literal `"undefined"`. Unreachable in practice — every truncated snapshot is
/// taken with `persistIfTruncated: true`, which creates the file — but kept exact.
fn interpolate_path(path: Option<&String>) -> &str {
    match path {
        Some(path) => path.as_str(),
        None => "undefined",
    }
}

/// `formatOutput(snapshot, emptyText)` (`bash.ts:375-393`).
///
/// Hoisted out of `execute` (where Pi declares it as a closure) so the footer
/// contract is directly testable; `last_line_bytes` is the one value Pi's closure
/// read from its environment (`output.getLastLineBytes()`, `bash.ts:384`).
///
/// `empty_text` is `"(no output)"` on the success path and `""` on every error
/// path (`bash.ts:409`), which is why a failed command with no output produces a
/// bare status line rather than `(no output)`.
///
/// Exactly one of three footers is appended when truncated:
/// * a partial last line (`bash.ts:383-385`),
/// * the line limit (`bash.ts:386-387`),
/// * the byte limit (`bash.ts:388-389`) — which hardcodes
///   `formatSize(DEFAULT_MAX_BYTES)`, i.e. `50.0KB`, and ignores
///   `truncation.max_bytes`.
pub fn format_bash_output(
    snapshot: &OutputSnapshot,
    last_line_bytes: u64,
    empty_text: &str,
) -> BashFormattedOutput {
    let truncation = &snapshot.truncation;
    let full_output_path = snapshot
        .full_output_path
        .as_ref()
        .map(|path| path.to_string_lossy().into_owned());

    let mut text = if snapshot.content.is_empty() {
        empty_text.to_string()
    } else {
        snapshot.content.clone()
    };
    let mut details = None;

    if truncation.truncated {
        details = Some(BashToolDetails {
            truncation: Some(truncation.clone()),
            full_output_path: full_output_path.clone(),
        });
        // `totalLines - outputLines + 1`. `saturating_sub` guards an underflow JS
        // would have expressed as a negative line number; the tail window is a
        // suffix of the whole output, so `outputLines <= totalLines` always holds.
        let start_line = truncation
            .total_lines
            .saturating_sub(truncation.output_lines)
            + 1;
        let end_line = truncation.total_lines;
        let path = interpolate_path(full_output_path.as_ref());

        if truncation.last_line_partial {
            let last_line_size = format_size(last_line_bytes);
            text += &format!(
                "\n\n[Showing last {} of line {end_line} (line is {last_line_size}). Full output: {path}]",
                format_size(truncation.output_bytes)
            );
        } else if truncation.truncated_by == Some(TruncatedBy::Lines) {
            text += &format!(
                "\n\n[Showing lines {start_line}-{end_line} of {}. Full output: {path}]",
                truncation.total_lines
            );
        } else {
            text += &format!(
                "\n\n[Showing lines {start_line}-{end_line} of {} ({} limit). Full output: {path}]",
                truncation.total_lines,
                // Deliberately NOT `truncation.max_bytes`.
                format_size(DEFAULT_MAX_BYTES)
            );
        }
    }

    BashFormattedOutput { text, details }
}

/// `appendStatus(text, status)` (`bash.ts:395`): a blank line between output and
/// status, and no leading blank lines when there was no output.
///
/// Note that `text` keeps its own trailing newline, so ordinary command output
/// ending in `\n` produces *three* consecutive newlines before the status.
pub fn append_status(text: &str, status: &str) -> String {
    if text.is_empty() {
        status.to_string()
    } else {
        format!("{text}\n\n{status}")
    }
}

// ===========================================================================
// Throttled streaming (bash.ts:313-373)
// ===========================================================================

/// `BASH_UPDATE_THROTTLE_MS` (`bash.ts:175`).
pub const BASH_UPDATE_THROTTLE_MS: u64 = 100;

/// A `Mutex` lock that survives poisoning: a panic inside one tool call must not
/// turn every later `lock()` into a second panic.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The mutable state `execute`'s closures share (`bash.ts:313-317`).
struct BashStream {
    /// `output`. `None` after [`finish_bash_output`] has taken ownership to run
    /// the async `close_temp_file`; a late `handle_data` then finds nothing,
    /// which `accepting_output` already prevents.
    output: Option<OutputAccumulator>,
    /// `acceptingOutput`.
    accepting_output: bool,
    /// `updateDirty`.
    update_dirty: bool,
    /// `lastUpdateAt`. Pi initialises it to `0` — an epoch timestamp, so the
    /// first `scheduleOutputUpdate` always computes a negative delay and flushes
    /// immediately. `None` reproduces that "infinitely long ago".
    last_update_at: Option<Instant>,
    /// `updateTimer`.
    update_timer: Option<JoinHandle<()>>,
}

impl BashStream {
    fn new() -> Self {
        Self {
            output: Some(OutputAccumulator::new(OutputAccumulatorOptions {
                temp_file_prefix: Some(BASH_TEMP_FILE_PREFIX.to_string()),
                ..OutputAccumulatorOptions::default()
            })),
            accepting_output: true,
            update_dirty: false,
            last_update_at: None,
            update_timer: None,
        }
    }

    /// `clearUpdateTimer` (`bash.ts:333-338`).
    fn clear_update_timer(&mut self) {
        if let Some(timer) = self.update_timer.take() {
            timer.abort();
        }
    }

    /// `BASH_UPDATE_THROTTLE_MS - (Date.now() - lastUpdateAt)` (`bash.ts:343`),
    /// as "how long to wait": `None` means "the delay is `<= 0`, flush now".
    fn remaining_throttle(&self) -> Option<Duration> {
        let elapsed = self.last_update_at?.elapsed();
        Duration::from_millis(BASH_UPDATE_THROTTLE_MS).checked_sub(elapsed)
    }
}

/// `emitOutputUpdate` (`bash.ts:319-331`): a no-op unless something is dirty.
///
/// Every emission carries `snapshot.content` **raw** — no truncation footer,
/// which only the final format appends — plus `details` with `truncation` present
/// only when the snapshot is truncated.
fn emit_output_update(shared: &Arc<Mutex<BashStream>>, on_update: &AgentToolUpdateCallback) {
    let update = {
        let mut state = lock(shared);
        if !state.update_dirty {
            return;
        }
        state.update_dirty = false;
        state.last_update_at = Some(Instant::now());
        let Some(output) = state.output.as_mut() else {
            return;
        };
        let snapshot = output.snapshot(SnapshotOptions {
            persist_if_truncated: true,
        });
        let truncated = snapshot.truncation.truncated;
        AgentToolResult {
            content: vec![UserContent::Text(TextContent::new(snapshot.content))],
            details: details_value(Some(&BashToolDetails {
                truncation: truncated.then_some(snapshot.truncation),
                full_output_path: snapshot
                    .full_output_path
                    .map(|path| path.to_string_lossy().into_owned()),
            })),
            added_tool_names: None,
            terminate: None,
        }
    };
    // Outside the lock: `on_update` is caller-supplied code.
    on_update(update);
}

/// `scheduleOutputUpdate` (`bash.ts:340-353`): flush now when the throttle window
/// has already elapsed, otherwise arm a *single* timer for the remainder.
fn schedule_output_update(shared: &Arc<Mutex<BashStream>>, on_update: &AgentToolUpdateCallback) {
    let flush_now = {
        let mut state = lock(shared);
        state.update_dirty = true;
        match state.remaining_throttle() {
            None => {
                state.clear_update_timer();
                true
            }
            // `updateTimer ??= setTimeout(...)`: an armed timer is left alone, so
            // a burst of chunks produces one emission per window.
            Some(delay) => {
                if state.update_timer.is_none() {
                    let shared = Arc::clone(shared);
                    let on_update = Arc::clone(on_update);
                    state.update_timer = Some(tokio::spawn(async move {
                        tokio::time::sleep(delay).await;
                        lock(&shared).update_timer = None;
                        emit_output_update(&shared, &on_update);
                    }));
                }
                false
            }
        }
    };
    if flush_now {
        emit_output_update(shared, on_update);
    }
}

/// `handleData` (`bash.ts:359-363`).
fn handle_bash_data(
    shared: &Arc<Mutex<BashStream>>,
    on_update: &AgentToolUpdateCallback,
    data: &[u8],
) {
    {
        let mut state = lock(shared);
        if !state.accepting_output {
            return;
        }
        if let Some(output) = state.output.as_mut() {
            // `append` only rejects after `finish`, which `accepting_output`
            // already gates.
            let _ = output.append(data);
        }
    }
    schedule_output_update(shared, on_update);
}

/// `finishOutput` (`bash.ts:365-373`): stop accepting output, flush the decoder,
/// cancel any pending timer, emit one last partial update, then take the final
/// snapshot and close the spill file.
///
/// Also returns `output.getLastLineBytes()` (`bash.ts:384`), which
/// [`format_bash_output`] needs and which cannot change after `finish`.
async fn finish_bash_output(
    shared: &Arc<Mutex<BashStream>>,
    on_update: &AgentToolUpdateCallback,
) -> Result<(OutputSnapshot, u64), ToolError> {
    {
        let mut state = lock(shared);
        state.accepting_output = false;
        if let Some(output) = state.output.as_mut() {
            output.finish();
        }
        state.clear_update_timer();
    }
    emit_output_update(shared, on_update);

    // Taking ownership is what lets the async `close_temp_file` run without
    // holding the lock across an await. `finishOutput` runs exactly once per
    // `execute`, so the `unwrap_or_default` arm is unreachable; an empty
    // accumulator is the harmless answer if it ever were reached.
    let mut output = lock(shared).output.take().unwrap_or_default();
    let snapshot = output.snapshot(SnapshotOptions {
        persist_if_truncated: true,
    });
    let last_line_bytes = output.get_last_line_bytes();
    output.close_temp_file().await?;
    Ok((snapshot, last_line_bytes))
}

/// `details` as the loop's `Value`: Pi's `undefined` is `null`.
fn details_value(details: Option<&BashToolDetails>) -> Value {
    match details {
        Some(details) => serde_json::to_value(details)
            .expect("BashToolDetails contains only strings, numbers and booleans"),
        None => Value::Null,
    }
}

// ===========================================================================
// execute (bash.ts:304-429)
// ===========================================================================

/// Everything `createBashToolDefinition` closes over (`bash.ts:292-297`) —
/// Pi's `cwd` parameter plus the three resolved options.
struct BashRuntime {
    cwd: String,
    ops: Arc<dyn BashOperations>,
    command_prefix: Option<String>,
    spawn_hook: Option<BashSpawnHook>,
}

/// `execute` (`bash.ts:304-429`), minus the `finally` (see [`execute_bash`]).
async fn execute_bash_inner(
    runtime: &BashRuntime,
    shared: &Arc<Mutex<BashStream>>,
    args: Value,
    token: CancellationToken,
    on_update: &AgentToolUpdateCallback,
) -> Result<AgentToolResult, ToolError> {
    let BashToolInput { command, timeout } = serde_json::from_value(args)?;

    // `bash.ts:311-312`.
    let resolved_command = match &runtime.command_prefix {
        Some(prefix) => format!("{prefix}\n{command}"),
        None => command,
    };
    let spawn_context =
        resolve_spawn_context(resolved_command, &runtime.cwd, runtime.spawn_hook.as_ref())?;

    // `bash.ts:355-357` — one empty update, before anything can have run, so the
    // UI can switch to "executing" immediately.
    on_update(AgentToolResult {
        content: Vec::new(),
        details: Value::Null,
        added_tool_names: None,
        terminate: None,
    });

    let on_data: BashOnData = {
        let shared = Arc::clone(shared);
        let on_update = Arc::clone(on_update);
        Arc::new(move |data: &[u8]| handle_bash_data(&shared, &on_update, data))
    };

    // `bash.ts:399-418`.
    let exec_result = runtime
        .ops
        .exec(
            &spawn_context.command,
            &spawn_context.cwd,
            BashExecOptions {
                on_data,
                token,
                timeout,
                env: Some(spawn_context.env),
            },
        )
        .await;

    let exit_code = match exec_result {
        Ok(result) => result.exit_code,
        Err(err) => {
            let (snapshot, last_line_bytes) = finish_bash_output(shared, on_update).await?;
            // Note the empty `emptyText`: an error with no output must not say
            // "(no output)". `details` is discarded — only `text` survives.
            let text = format_bash_output(&snapshot, last_line_bytes, "").text;
            let message = err.to_string();
            if message == "aborted" {
                return Err(append_status(&text, "Command aborted").into());
            }
            if let Some(timeout_secs) = message.strip_prefix("timeout:") {
                // `err.message.split(":")[1]` — the segment between the first and
                // second colon. Timeout renderings never contain a colon, so this
                // is the whole remainder.
                let timeout_secs = timeout_secs.split(':').next().unwrap_or_default();
                return Err(append_status(
                    &text,
                    &format!("Command timed out after {timeout_secs} seconds"),
                )
                .into());
            }
            // Everything else propagates verbatim: this is how `Invalid timeout:
            // …`, `Working directory does not exist: …` and the "no bash shell"
            // message reach the model.
            return Err(err);
        }
    };

    // `bash.ts:420-425`.
    let (snapshot, last_line_bytes) = finish_bash_output(shared, on_update).await?;
    let formatted = format_bash_output(&snapshot, last_line_bytes, "(no output)");
    // `exitCode !== 0 && exitCode !== null` — a signalled child (`None`) is a
    // success.
    if let Some(code) = exit_code.filter(|code| *code != 0) {
        return Err(
            append_status(&formatted.text, &format!("Command exited with code {code}")).into(),
        );
    }
    Ok(AgentToolResult {
        content: vec![UserContent::Text(TextContent::new(formatted.text))],
        details: details_value(formatted.details.as_ref()),
        added_tool_names: None,
        terminate: None,
    })
}

/// [`execute_bash_inner`] plus Pi's `finally { clearUpdateTimer(); }`
/// (`bash.ts:426-428`), so no armed timer can fire after the call has settled.
async fn execute_bash(
    runtime: &BashRuntime,
    args: Value,
    token: CancellationToken,
    on_update: AgentToolUpdateCallback,
) -> Result<AgentToolResult, ToolError> {
    let shared = Arc::new(Mutex::new(BashStream::new()));
    let result = execute_bash_inner(runtime, &shared, args, token, &on_update).await;
    lock(&shared).clear_update_timer();
    result
}

// ===========================================================================
// Factories (bash.ts:291-303, bash.ts:468-470)
// ===========================================================================

/// `createBashToolDefinition(cwd, options)` (`bash.ts:291-466`), minus the
/// renderers.
///
/// `options.operations ?? createLocalBashOperations({ shellPath })`
/// (`bash.ts:295`): `shell_path` is consumed *only* by the default operations, so
/// a custom [`BashOperations`] silently ignores it — exactly as in Pi.
///
/// No `promptGuidelines`, no `executionMode` and no `prepareArguments`, per
/// `tests/fixtures/pi/tools/strings/bash.json`.
pub fn create_bash_tool_definition(
    cwd: &str,
    options: Option<BashToolOptions>,
) -> PirustToolDefinition {
    let options = options.unwrap_or_default();
    let runtime = Arc::new(BashRuntime {
        cwd: cwd.to_string(),
        ops: options
            .operations
            .unwrap_or_else(|| create_local_bash_operations(options.shell_path)),
        command_prefix: options.command_prefix,
        spawn_hook: options.spawn_hook,
    });

    PirustToolDefinition::new(
        BASH_NAME,
        BASH_NAME,
        bash_description(),
        bash_parameters(),
        move |_tool_call_id: String,
              args: Value,
              token: CancellationToken,
              on_update: AgentToolUpdateCallback| {
            let runtime = Arc::clone(&runtime);
            async move { execute_bash(&runtime, args, token, on_update).await }
        },
    )
    .with_prompt_snippet(BASH_PROMPT_SNIPPET)
}

/// `createBashTool(cwd, options)` (`bash.ts:468-470`).
///
/// Pi calls `wrapToolDefinition`; here [`PirustToolDefinition`] *is* the wrapper
/// (see [`crate::definition`]), so this is only the erasure to
/// `Arc<dyn AgentTool>`.
pub fn create_bash_tool(cwd: &str, options: Option<BashToolOptions>) -> Arc<dyn AgentTool> {
    Arc::new(create_bash_tool_definition(cwd, options))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `isLegacyWslBashPath` (`shell.ts:15-18`) drives the only place a resolved
    /// shell is *not* given `-c`, so both the accepts and the rejects matter.
    #[test]
    fn legacy_wsl_bash_paths_are_recognised() {
        for path in [
            r"C:\Windows\System32\bash.exe",
            r"c:\windows\system32\bash.exe",
            r"C:\Windows\Sysnative\bash.exe",
            "C:/Windows/System32/bash.exe",
            "D:/WINDOWS/SysNative/BASH.EXE",
        ] {
            assert!(is_legacy_wsl_bash_path(path), "{path} should be legacy WSL");
            let config = get_bash_shell_config(path);
            assert_eq!(config.args, ["-s"]);
            assert_eq!(config.command_transport, CommandTransport::Stdin);
        }

        for path in [
            r"C:\Program Files\Git\bin\bash.exe",
            r"C:\Windows\System32\wsl.exe",
            r"C:\Windows\bash.exe",
            r"\\server\share\Windows\System32\bash.exe",
            r"C:\Windows\System32\subdir\bash.exe",
            "/bin/bash",
            "",
        ] {
            assert!(
                !is_legacy_wsl_bash_path(path),
                "{path:?} should not be legacy WSL"
            );
            let config = get_bash_shell_config(path);
            assert_eq!(config.args, ["-c"]);
            assert_eq!(config.command_transport, CommandTransport::Argv);
        }
    }

    /// `lastUpdateAt = 0` (`bash.ts:317`) is an epoch timestamp, so the very
    /// first chunk always flushes synchronously instead of waiting out a window.
    #[test]
    fn the_first_scheduled_update_is_never_throttled() {
        let mut state = BashStream::new();
        assert!(state.remaining_throttle().is_none(), "flush immediately");
        state.last_update_at = Some(Instant::now());
        assert!(
            state.remaining_throttle().is_some(),
            "a fresh emission arms the window"
        );
    }
}
