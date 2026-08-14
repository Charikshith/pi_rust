//! Partial port of `utils/tools-manager.ts` — locating the managed `rg` / `fd`
//! binaries that `grep` / `find` shell out to.
//!
//! RESOLUTION ONLY: the GitHub-releases downloader is deferred to feat-005.
//!
//! # Resolution order
//!
//! [`get_tool_path`] ports `getToolPath` (`tools-manager.ts:85-104`) and
//! [`ensure_tool`] ports `ensureTool` (`tools-manager.ts:326-369`):
//!
//! 1. **Managed binary** — `<tools_dir>/<binary_name>` plus `.exe` when
//!    `platform() === "win32"`; returned verbatim if it exists
//!    (`tools-manager.ts:90-93`). `tools_dir` is `TOOLS_DIR = getBinDir()`
//!    (`tools-manager.ts:10`) = `join(getAgentDir(), "bin")` (`config.ts:549-551`).
//! 2. **System PATH** — for each candidate command name, probe by *spawning*
//!    `<cmd> --version`; "spawned without an error" is success, whatever the
//!    exit status (`commandExists`, `tools-manager.ts:74-82` — it only inspects
//!    `result.error`, never `result.status`). On success Pi returns the **bare
//!    command name**, not an absolute path: it relies on PATH lookup happening
//!    again at spawn time (`tools-manager.ts:95-101`). `fd` probes
//!    `["fd", "fdfind"]` (`tools-manager.ts:34`); `rg` declares no
//!    `systemBinaryNames`, so it falls back to `[binaryName]` = `["rg"]`
//!    (`tools-manager.ts:96`).
//! 3. **Offline gate** — [`OFFLINE_ENV`] equal to `1`, or (after
//!    `toLowerCase()`) `true` / `yes`, means give up (`isOfflineModeEnabled`,
//!    `tools-manager.ts:14-18`; the gate itself at `tools-manager.ts:335-340`).
//! 4. **Android/Termux gate** — `platform() === "android"` means give up,
//!    because Bionic libc cannot run the released Linux binaries; Pi's message
//!    names `pkg install fd` / `pkg install ripgrep`
//!    (`TERMUX_PACKAGES`, `tools-manager.ts:319-322`; gate at `:342-350`).
//! 5. Otherwise Pi downloads. pirust stops here — see
//!    [`EnsureOutcome::DownloadDeferred`].
//!
//! Note the **order**: both gates sit *after* the whole of `getToolPath`, so an
//! offline (or Termux) agent still finds a managed or PATH binary, and still
//! pays for the `--version` probes. Only the *download* is gated. Likewise the
//! offline gate is checked before the android gate, so on Termux with the
//! offline flag set it is the offline message that is produced.
//!
//! `ensureTool` never throws for a missing binary — it resolves to `undefined`
//! (`tools-manager.ts:333`, `:339`, `:349`, `:367`) and the user-facing error
//! text is produced by the calling tool: `"ripgrep (rg) is not available and
//! could not be downloaded"` (`core/tools/grep.ts:175`) and `"fd is not
//! available and could not be downloaded"` (`core/tools/find.ts:221`). Those
//! strings deliberately do not live here.
//!
//! # Verified against real Pi
//!
//! Every rule above was observed by running Pi's own `getToolPath` /
//! `ensureTool` under `node --experimental-strip-types` with
//! `PI_CODING_AGENT_DIR` / `PI_OFFLINE` / `PATH` set up to force each branch,
//! rather than asserting self-authored expectations:
//!
//! | scenario | Pi's observed result |
//! |---|---|
//! | managed `rg.exe` present | `"C:\Users\<u>\.pi\agent\bin\rg.exe"` |
//! | managed dir empty, nothing on `PATH` | `null` |
//! | `rg.exe` on `PATH` | `"rg"` — the **bare name** |
//! | only `fdfind.exe` on `PATH` | `"fdfind"` |
//! | `PI_OFFLINE=1` **and** `rg` on `PATH` | `"rg"` — the gate does not suppress the probe |
//! | `PI_OFFLINE` ∈ `1`/`true`/`TRUE`/`yes`/`YES`/`Yes`, nothing found | `undefined` + `"<name> not found. Offline mode enabled, skipping download."` |
//! | `PI_OFFLINE` ∈ `""`/`0`/`no`/`" 1"`/`01`, nothing found | `"<name> not found. Downloading..."` → download |
//! | a *directory* named `rg.exe` in the tools dir | returned as if it were the binary |
//!
//! On the current dev machine the managed lookup misses (`rg.exe`/`fd.exe` live
//! in Pi's `~/.pi/agent/bin`, and pirust looks in `~/.pirust/agent/bin`) and the
//! `PATH` probe misses too, so [`ensure_tool`] correctly yields nothing. That is
//! the expected result: pirust must **not** grow a `~/.pi` fallback Pi lacks.
//!
//! # Deferred to feat-005: the downloader
//!
//! Everything reached *after* step 4 in Pi is out of scope here:
//! `getLatestVersion` (`tools-manager.ts:107-119`), `downloadFile`
//! (`:122-137`), `ToolConfig.getAssetName` (`:36-48`, `:55-69`), the
//! `fd` + `darwin` + `x64` version pin to `10.3.0` (`:250-252`), the unique
//! `extract_tmp_*` directory (`:271-277`), tar.gz/zip extraction (`:184-238`),
//! `findBinaryRecursively` (`:139-159`), the rename into place and the
//! `chmod 0o755` (`:299-308`). [`ToolSpec::repo`] / [`ToolSpec::tag_prefix`]
//! are the metadata that downloader needs, carried here so the record stays a
//! faithful port of `TOOLS` (`tools-manager.ts:29-71`).
//!
//! **The hook-in point is the single [`EnsureOutcome::DownloadDeferred`] arm of
//! [`ensure_tool_outcome`].** It reports "no binary, and no download was
//! attempted"; it never fabricates a path, so a caller can never mistake the
//! deferral for a successful install. feat-005 replaces that arm with the
//! download and restores Pi's three progress lines (`tools-manager.ts:354`,
//! `:360`, `:365`).
//!
//! # Divergences from Pi (documented, intentional)
//!
//! - **State-directory naming.** [`CONFIG_DIR_NAME`], [`ENV_AGENT_DIR`] and
//!   [`OFFLINE_ENV`] intentionally differ from Pi's `.pi` /
//!   `PI_CODING_AGENT_DIR` / `PI_OFFLINE`; see those constants.
//! - **`silent` parameter dropped.** Pi's `ensureTool(tool, silent = false)`
//!   uses `silent` only to suppress `console.log` (`tools-manager.ts:336`,
//!   `:346`, `:353`, `:359`, `:364`). This crate is UI-free, so the decision
//!   moves to the caller: [`EnsureOutcome::log_line`] returns the text Pi would
//!   have printed (minus `chalk` colour, which is a TUI concern) and the caller
//!   prints it or not. `grep`/`find` pass `silent = true`
//!   (`core/tools/grep.ts:172`, `core/tools/find.ts:214`), i.e. they discard it.
//! - **`if (!config) return null/undefined`** (`tools-manager.ts:87`, `:333`)
//!   is unrepresentable: [`ManagedTool`] is a closed enum, so there is no
//!   "unknown tool" path to port.
//! - **`file://` agent-dir overrides.** `getAgentDir` runs the override through
//!   `expandTildePath` → `normalizePath` (`config.ts:498-500`,
//!   `utils/paths.ts:57-80`), which also converts `file://` URLs via
//!   `fileURLToPath`. Only the `~` expansion is ported here; a `file://` value
//!   is passed through unchanged. This is the one observed behaviour this
//!   module does not reproduce: with real Pi,
//!   `PI_CODING_AGENT_DIR=file:///C:/tmp/x` yields `getAgentDir()` =
//!   `C:\tmp\x`, whereas [`BinaryEnv::agent_dir`] yields the literal
//!   `file:///C:/tmp/x`. `normalizePath` (including a `fileURLToPath`
//!   transcription) lands in [`crate::path_utils`]; `agent_dir` should delegate
//!   to it then and the gap closes. Every other `expandTildePath` case was
//!   checked against Pi: `~` → home, `~/rest` and (win32) `~\rest` →
//!   `join(home, rest)`, `/opt/~/x` unchanged.
//! - **`os.homedir()` fallback.** libuv checks `USERPROFILE` (win32) or `HOME`
//!   (POSIX) and only then consults the passwd database /
//!   `SHGetKnownFolderPath`. [`node_homedir`] ports the env-var half; when the
//!   variable is unset the result is `None`, which surfaces as
//!   [`HomeDirUnavailable`] instead of Pi's `os.homedir()` throw.
//! - **`spawnSync` → async spawn.** `commandExists` blocks Node's event loop;
//!   [`SpawnProbe`] awaits the child instead, so concurrent probes (Pi's
//!   `Promise.all([ensureTool("fd"), ensureTool("rg")])`,
//!   `modes/interactive/interactive-mode.ts:682`) can overlap. The boolean
//!   result is unaffected. `stdio: "pipe"` becomes `Stdio::null()`: Pi captures
//!   the child's output and then discards it.
//! - **No process-global reads inside the resolution functions.** All of
//!   Pi's ambient inputs (`process.env`, `os.platform()`, `os.homedir()`) are
//!   captured in a [`BinaryEnv`] value that the caller threads through, so
//!   tests never touch the real environment. [`BinaryEnv::from_process_env`] is
//!   the one place that reads it.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use async_trait::async_trait;

// =============================================================================
// pirust state-directory naming (intentional divergence from Pi)
// =============================================================================

/// Directory under the user's home that holds pirust state — the analogue of
/// Pi's `CONFIG_DIR_NAME` (`config.ts:491`, `".pi"`).
///
/// **Intentionally diverges from Pi.** pirust owns its own state directory, so
/// it must not read or write Pi's `~/.pi`; the *file formats* inside stay
/// byte-compatible with Pi's, only the location differs. Per an explicit user
/// directive the managed-binary directory is `~/.pirust/agent/bin`.
///
/// This constant plus [`ENV_AGENT_DIR`] and [`OFFLINE_ENV`] are the only place
/// the naming is decided; flipping them back to Pi's `.pi` / `PI_*` is a
/// three-line change.
pub const CONFIG_DIR_NAME: &str = ".pirust";

/// Env var overriding the agent directory — analogue of Pi's `ENV_AGENT_DIR`
/// (`config.ts:495`), which is built as `${APP_NAME.toUpperCase()}_CODING_AGENT_DIR`
/// and evaluates to `PI_CODING_AGENT_DIR` (`APP_NAME` = `"pi"`, `config.ts:489`).
///
/// **Intentionally diverges from Pi** — see [`CONFIG_DIR_NAME`].
pub const ENV_AGENT_DIR: &str = "PIRUST_CODING_AGENT_DIR";

/// Env var enabling offline mode — analogue of Pi's `PI_OFFLINE`
/// (`isOfflineModeEnabled`, `tools-manager.ts:15`).
///
/// **Intentionally diverges from Pi** — see [`CONFIG_DIR_NAME`].
pub const OFFLINE_ENV: &str = "PIRUST_OFFLINE";

/// Directory component appended to the home dir:
/// `join(homedir(), CONFIG_DIR_NAME, "agent")` (`getAgentDir`, `config.ts:515-521`).
pub const AGENT_DIR_NAME: &str = "agent";

/// Directory component holding the managed binaries: `join(getAgentDir(), "bin")`
/// (`getBinDir`, `config.ts:549-551`).
pub const BIN_DIR_NAME: &str = "bin";

/// Env var `os.homedir()` reads on POSIX (libuv `uv_os_homedir`).
pub const HOME_ENV_POSIX: &str = "HOME";

/// Env var `os.homedir()` reads on Windows (libuv `uv_os_homedir`); Node
/// resolves `~` to `%USERPROFILE%` there.
pub const HOME_ENV_WINDOWS: &str = "USERPROFILE";

// =============================================================================
// Platform (Node `os.platform()`)
// =============================================================================

/// The `os.platform()` values this module branches on (`tools-manager.ts:90`
/// win32, `:344` android).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Platform {
    /// Node `"win32"`.
    Win32,
    /// Node `"darwin"`.
    Darwin,
    /// Node `"linux"`.
    Linux,
    /// Node `"android"` — Termux.
    Android,
    /// Any other platform; behaves like `Linux` for every rule ported here.
    Other,
}

impl Platform {
    /// The platform this binary was built for.
    pub fn current() -> Self {
        Self::from_rust_os(std::env::consts::OS)
    }

    /// Maps a [`std::env::consts::OS`] value onto Node's `os.platform()` naming.
    pub fn from_rust_os(os: &str) -> Self {
        match os {
            "windows" => Self::Win32,
            "macos" => Self::Darwin,
            "linux" => Self::Linux,
            "android" => Self::Android,
            _ => Self::Other,
        }
    }

    /// The `os.platform()` string Pi would compare against, for messages/tests.
    pub fn as_node_str(self) -> &'static str {
        match self {
            Self::Win32 => "win32",
            Self::Darwin => "darwin",
            Self::Linux => "linux",
            Self::Android => "android",
            Self::Other => "other",
        }
    }

    /// `platform() === "win32" ? ".exe" : ""` (`tools-manager.ts:90`).
    pub fn exe_suffix(self) -> &'static str {
        if self == Self::Win32 {
            ".exe"
        } else {
            ""
        }
    }
}

// =============================================================================
// The TOOLS record (`tools-manager.ts:29-71`)
// =============================================================================

/// The two managed tools — Pi's `tool: "fd" | "rg"` union
/// (`tools-manager.ts:85`, `:326`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ManagedTool {
    /// `TOOLS.fd` — backend for the `find` tool.
    Fd,
    /// `TOOLS.rg` — backend for the `grep` tool.
    Rg,
}

/// One entry of Pi's `TOOLS` record (`tools-manager.ts:20-71`), minus
/// `getAssetName` (deferred to feat-005; see the module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolSpec {
    /// The `TOOLS` key, i.e. the value of Pi's `tool` argument: `"fd"` / `"rg"`.
    pub key: &'static str,
    /// `ToolConfig.name` — the display name used in Pi's log lines
    /// (`"fd"` / `"ripgrep"`, `tools-manager.ts:31`, `:51`).
    pub name: &'static str,
    /// `ToolConfig.binaryName` — the file name inside the tools dir
    /// (`tools-manager.ts:33`, `:53`).
    pub binary_name: &'static str,
    /// `ToolConfig.systemBinaryNames ?? [binaryName]` — the PATH candidates, in
    /// probe order (`tools-manager.ts:34`, `:96`).
    pub system_binary_names: &'static [&'static str],
    /// `TERMUX_PACKAGES[tool] ?? tool` — the `pkg install` argument
    /// (`tools-manager.ts:319-322`, `:345`).
    pub termux_package: &'static str,
    /// `ToolConfig.repo` (`tools-manager.ts:32`, `:52`). Unused until feat-005
    /// implements the downloader.
    pub repo: &'static str,
    /// `ToolConfig.tagPrefix` (`tools-manager.ts:35`, `:54`). Unused until
    /// feat-005 implements the downloader.
    pub tag_prefix: &'static str,
}

/// `TOOLS.fd` (`tools-manager.ts:30-49`).
static FD_SPEC: ToolSpec = ToolSpec {
    key: "fd",
    name: "fd",
    binary_name: "fd",
    system_binary_names: &["fd", "fdfind"],
    termux_package: "fd",
    repo: "sharkdp/fd",
    tag_prefix: "v",
};

/// `TOOLS.rg` (`tools-manager.ts:50-70`). No `systemBinaryNames`, so the PATH
/// candidates default to `[binaryName]` (`tools-manager.ts:96`).
static RG_SPEC: ToolSpec = ToolSpec {
    key: "rg",
    name: "ripgrep",
    binary_name: "rg",
    system_binary_names: &["rg"],
    termux_package: "ripgrep",
    repo: "BurntSushi/ripgrep",
    tag_prefix: "",
};

impl ManagedTool {
    /// The `TOOLS` key (`"fd"` / `"rg"`).
    pub fn key(self) -> &'static str {
        self.spec().key
    }

    /// This tool's [`ToolSpec`].
    pub fn spec(self) -> &'static ToolSpec {
        match self {
            Self::Fd => &FD_SPEC,
            Self::Rg => &RG_SPEC,
        }
    }

    /// Parses a `TOOLS` key. Pi's `TOOLS[tool]` lookup plus the
    /// `if (!config) return null` guard (`tools-manager.ts:86-87`) collapses to
    /// this `Option`.
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "fd" => Some(Self::Fd),
            "rg" => Some(Self::Rg),
            _ => None,
        }
    }
}

// =============================================================================
// Environment seam
// =============================================================================

/// `os.homedir()` failed *and* was needed: no [`ENV_AGENT_DIR`] override was
/// set, so the agent dir cannot be derived. Pi throws from `os.homedir()` in the
/// equivalent situation (see the module docs).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "cannot resolve the home directory: environment variable `{home_env}` is unset \
     and no `PIRUST_CODING_AGENT_DIR` override was given"
)]
pub struct HomeDirUnavailable {
    /// The env var that would have supplied the home dir on this platform:
    /// [`HOME_ENV_WINDOWS`] or [`HOME_ENV_POSIX`].
    pub home_env: &'static str,
}

/// The env var `os.homedir()` consults on `platform`.
pub fn home_env_var(platform: Platform) -> &'static str {
    if platform == Platform::Win32 {
        HOME_ENV_WINDOWS
    } else {
        HOME_ENV_POSIX
    }
}

/// `os.homedir()`, env-var half only: `%USERPROFILE%` on Windows, `$HOME`
/// elsewhere (libuv `uv_os_homedir`). The passwd-database fallback is not
/// ported — see the module docs.
///
/// An empty value is returned as `Some("")`, matching libuv (which distinguishes
/// only "set" from `UV_ENOENT`) and Node's subsequent `join("", ...)`.
pub fn node_homedir(platform: Platform) -> Option<PathBuf> {
    std::env::var(home_env_var(platform))
        .ok()
        .map(PathBuf::from)
}

/// Every ambient input Pi's resolution reads (`process.env`, `os.platform()`,
/// `os.homedir()`), captured so it can be threaded through explicitly instead of
/// being read from globals mid-resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinaryEnv {
    /// `os.platform()`.
    pub platform: Platform,
    /// `os.homedir()`, or `None` if it could not be determined.
    pub home_dir: Option<PathBuf>,
    /// `process.env[ENV_AGENT_DIR]` — the raw, unexpanded value.
    pub agent_dir_override: Option<String>,
    /// `process.env[OFFLINE_ENV]` — the raw value; interpreted by
    /// [`BinaryEnv::is_offline`].
    pub offline: Option<String>,
}

impl BinaryEnv {
    /// Snapshots the real process environment. The **only** function in this
    /// module that reads globals; call it once at startup and pass the result
    /// down (production callers) so tests can build a [`BinaryEnv`] literal.
    pub fn from_process_env() -> Self {
        let platform = Platform::current();
        Self {
            platform,
            home_dir: node_homedir(platform),
            agent_dir_override: std::env::var(ENV_AGENT_DIR).ok(),
            offline: std::env::var(OFFLINE_ENV).ok(),
        }
    }

    /// `getAgentDir()` (`config.ts:515-521`): the [`ENV_AGENT_DIR`] override
    /// (tilde-expanded) if set and non-empty, else
    /// `join(homedir(), CONFIG_DIR_NAME, "agent")`.
    ///
    /// An empty override is falsy in JS (`if (envDir)`, `config.ts:516`), so it
    /// is ignored exactly as an unset one is.
    pub fn agent_dir(&self) -> Result<PathBuf, HomeDirUnavailable> {
        if let Some(dir) = self.agent_dir_override.as_deref().filter(|d| !d.is_empty()) {
            return self.expand_tilde_path(dir);
        }
        Ok(self.home_dir()?.join(CONFIG_DIR_NAME).join(AGENT_DIR_NAME))
    }

    /// `TOOLS_DIR = getBinDir()` (`tools-manager.ts:10`, `config.ts:549-551`) —
    /// `join(getAgentDir(), "bin")`, i.e. `~/.pirust/agent/bin` by default.
    pub fn tools_dir(&self) -> Result<PathBuf, HomeDirUnavailable> {
        Ok(self.agent_dir()?.join(BIN_DIR_NAME))
    }

    /// The managed-binary path probed first by [`get_tool_path`]:
    /// `join(TOOLS_DIR, binaryName + (win32 ? ".exe" : ""))`
    /// (`tools-manager.ts:90`).
    pub fn managed_binary_path(&self, tool: ManagedTool) -> Result<PathBuf, HomeDirUnavailable> {
        let spec = tool.spec();
        let file_name = format!("{}{}", spec.binary_name, self.platform.exe_suffix());
        Ok(self.tools_dir()?.join(file_name))
    }

    /// `isOfflineModeEnabled()` (`tools-manager.ts:14-18`): unset or empty →
    /// `false`; otherwise `"1"` exactly, or `toLowerCase()` equal to `"true"` or
    /// `"yes"`. Anything else (`"0"`, `"no"`, `"on"`, `" 1"`, …) → `false`.
    pub fn is_offline(&self) -> bool {
        let Some(value) = self.offline.as_deref() else {
            return false;
        };
        // `if (!value) return false` — the empty string is falsy in JS.
        if value.is_empty() {
            return false;
        }
        let lowered = value.to_lowercase();
        value == "1" || lowered == "true" || lowered == "yes"
    }

    fn home_dir(&self) -> Result<&Path, HomeDirUnavailable> {
        self.home_dir.as_deref().ok_or(HomeDirUnavailable {
            home_env: home_env_var(self.platform),
        })
    }

    /// `expandTildePath` = `normalizePath(path)` with default options
    /// (`config.ts:498-500`, `utils/paths.ts:57-80`), restricted to the `~`
    /// expansion: `"~"` → home, `"~/rest"` (plus `"~\rest"` on win32) →
    /// `join(home, rest)`, anything else verbatim. `trim` and
    /// `normalizeUnicodeSpaces` default to off, so no other rewriting happens;
    /// the `file://` branch is not ported (see the module docs).
    fn expand_tilde_path(&self, input: &str) -> Result<PathBuf, HomeDirUnavailable> {
        if input == "~" {
            return Ok(self.home_dir()?.to_path_buf());
        }
        let tilde_prefixed = input.starts_with("~/")
            || (self.platform == Platform::Win32 && input.starts_with("~\\"));
        if tilde_prefixed {
            return Ok(self.home_dir()?.join(&input[2..]));
        }
        Ok(PathBuf::from(input))
    }
}

// =============================================================================
// commandExists (`tools-manager.ts:74-82`)
// =============================================================================

/// The `commandExists` seam: "can `<command> --version` be spawned at all?".
#[async_trait]
pub trait CommandProbe: Send + Sync {
    /// `commandExists(cmd)` (`tools-manager.ts:74-82`). `true` iff the spawn
    /// itself succeeded; the child's exit status is irrelevant, because Pi only
    /// inspects `result.error` (which is set for `ENOENT`).
    async fn command_exists(&self, command: &str) -> bool;
}

/// Production [`CommandProbe`]: actually spawns `<command> --version`.
///
/// Command lookup is left to the OS, as in Pi: `spawnSync` without `shell: true`
/// hands the bare name to `execvp` / `CreateProcess`, and neither consults
/// `PATHEXT`, so a `foo.cmd` on `PATH` is *not* discoverable. Rust's
/// [`std::process::Command`] behaves the same way (PATH search, `.exe` appended
/// on Windows, no `PATHEXT`).
#[derive(Debug, Clone, Copy, Default)]
pub struct SpawnProbe;

#[async_trait]
impl CommandProbe for SpawnProbe {
    async fn command_exists(&self, command: &str) -> bool {
        let spawned = tokio::process::Command::new(command)
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        match spawned {
            Ok(mut child) => {
                // Pi's `spawnSync` waits for the child; wait too, so probing
                // cannot leak processes. The status is deliberately ignored —
                // a command that exists but rejects `--version` still counts.
                let _ = child.wait().await;
                true
            }
            // `result.error` set (`ENOENT`, `EACCES`, bad exe format, …).
            Err(_) => false,
        }
    }
}

// =============================================================================
// getToolPath / ensureTool
// =============================================================================

/// `getToolPath(tool)` (`tools-manager.ts:85-104`).
///
/// Returns either an absolute path to the managed binary, **or a bare command
/// name** when the tool was found on `PATH` — Pi returns the command name and
/// lets the eventual spawn redo the `PATH` lookup (`tools-manager.ts:95-101`),
/// so callers must pass the value to a spawn that performs `PATH` resolution.
///
/// `Err` only when the tools dir itself is unresolvable ([`HomeDirUnavailable`]);
/// a missing binary is `Ok(None)`.
pub async fn get_tool_path(
    tool: ManagedTool,
    env: &BinaryEnv,
    probe: &dyn CommandProbe,
) -> Result<Option<String>, HomeDirUnavailable> {
    // Check our tools directory first (`tools-manager.ts:89-93`).
    let local_path = env.managed_binary_path(tool)?;
    if local_path.exists() {
        return Ok(Some(local_path.to_string_lossy().into_owned()));
    }

    // Check system PATH - if found, just return the command name
    // (`tools-manager.ts:95-101`).
    for name in tool.spec().system_binary_names {
        if probe.command_exists(name).await {
            return Ok(Some((*name).to_string()));
        }
    }

    Ok(None)
}

/// Why [`ensure_tool`] returned what it returned — which branch of `ensureTool`
/// (`tools-manager.ts:326-369`) was taken.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnsureOutcome {
    /// `getToolPath` succeeded (`tools-manager.ts:327-330`). Absolute path, or a
    /// bare `PATH` command name — see [`get_tool_path`].
    Found(String),
    /// Offline mode is on, so the download was skipped
    /// (`tools-manager.ts:335-340`). Pi resolves `undefined`.
    OfflineSkipped,
    /// Android/Termux: the released Linux binaries are Bionic-incompatible, so
    /// the user must `pkg install` it (`tools-manager.ts:342-350`). Pi resolves
    /// `undefined`.
    TermuxInstall,
    /// **feat-005 seam.** Pi would download from GitHub releases here
    /// (`tools-manager.ts:352-368`); this port does not, and reports that
    /// plainly rather than inventing a path. Treat exactly like a failed
    /// download: no binary is available.
    DownloadDeferred,
}

impl EnsureOutcome {
    /// The resolved path/command name, if any — Pi's `string | undefined`.
    pub fn path(&self) -> Option<&str> {
        match self {
            Self::Found(path) => Some(path),
            _ => None,
        }
    }

    /// [`EnsureOutcome::path`], by value.
    pub fn into_path(self) -> Option<String> {
        match self {
            Self::Found(path) => Some(path),
            _ => None,
        }
    }

    /// The line Pi prints for this branch when `silent` is false, without
    /// `chalk` colour (`tools-manager.ts:337`, `:347`).
    ///
    /// `None` for [`EnsureOutcome::Found`] (Pi prints nothing) and for
    /// [`EnsureOutcome::DownloadDeferred`] — Pi's `"<name> not found.
    /// Downloading..."` (`tools-manager.ts:354`) would be a lie here, so
    /// feat-005 restores it together with `"<name> installed to <path>"`
    /// (`:360`) and `"Failed to download <name>: <e>"` (`:365`).
    pub fn log_line(&self, tool: ManagedTool) -> Option<String> {
        let spec = tool.spec();
        match self {
            Self::Found(_) | Self::DownloadDeferred => None,
            Self::OfflineSkipped => Some(format!(
                "{} not found. Offline mode enabled, skipping download.",
                spec.name
            )),
            Self::TermuxInstall => Some(format!(
                "{} not found. Install with: pkg install {}",
                spec.name, spec.termux_package
            )),
        }
    }
}

/// `ensureTool(tool, silent)` (`tools-manager.ts:326-369`), reporting which
/// branch was taken so the caller can print Pi's message (see
/// [`EnsureOutcome::log_line`]) — the `silent` flag itself is not ported.
///
/// Order, exactly as Pi: [`get_tool_path`] (managed dir, then the `PATH`
/// probes), then the offline gate, then the android gate. Both gates guard only
/// the download, so neither can hide an already-installed binary.
pub async fn ensure_tool_outcome(
    tool: ManagedTool,
    env: &BinaryEnv,
    probe: &dyn CommandProbe,
) -> Result<EnsureOutcome, HomeDirUnavailable> {
    if let Some(existing_path) = get_tool_path(tool, env, probe).await? {
        return Ok(EnsureOutcome::Found(existing_path));
    }

    if env.is_offline() {
        return Ok(EnsureOutcome::OfflineSkipped);
    }

    // On Android/Termux, Linux binaries don't work due to Bionic libc
    // incompatibility. Users must install via pkg (`tools-manager.ts:342-350`).
    if env.platform == Platform::Android {
        return Ok(EnsureOutcome::TermuxInstall);
    }

    // Pi downloads here; deferred to feat-005 (see the module docs).
    Ok(EnsureOutcome::DownloadDeferred)
}

/// `ensureTool(tool, silent)` reduced to Pi's return value: the path to the
/// tool, or nothing.
///
/// Never errors for a missing binary — `Ok(None)` covers "offline", "Termux" and
/// "no download attempted" alike, matching Pi's `undefined`. The user-facing
/// message is the calling tool's job (`core/tools/grep.ts:175`,
/// `core/tools/find.ts:221`). Use [`ensure_tool_outcome`] when the reason
/// matters.
pub async fn ensure_tool(
    tool: ManagedTool,
    env: &BinaryEnv,
    probe: &dyn CommandProbe,
) -> Result<Option<String>, HomeDirUnavailable> {
    Ok(ensure_tool_outcome(tool, env, probe).await?.into_path())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_with_offline(offline: Option<&str>) -> BinaryEnv {
        BinaryEnv {
            platform: Platform::Linux,
            home_dir: Some(PathBuf::from("/home/u")),
            agent_dir_override: None,
            offline: offline.map(str::to_string),
        }
    }

    #[test]
    fn offline_truthy_values_match_pi() {
        // `value === "1" || value.toLowerCase() === "true" || ... === "yes"`
        // (tools-manager.ts:17).
        for value in ["1", "true", "TRUE", "True", "yes", "YES", "Yes", "yEs"] {
            assert!(
                env_with_offline(Some(value)).is_offline(),
                "expected {value:?} to enable offline mode"
            );
        }
    }

    #[test]
    fn offline_falsy_values_match_pi() {
        // Unset and "" hit `if (!value) return false` (tools-manager.ts:16); the
        // rest fail all three equality checks — note "1" is compared *without*
        // lowercasing or trimming, so " 1", "01" and "TRUE " are all false.
        assert!(!env_with_offline(None).is_offline());
        for value in [
            "", "0", "no", "NO", "false", "on", "off", " 1", "1 ", "01", "TRUE ", "y", "t",
        ] {
            assert!(
                !env_with_offline(Some(value)).is_offline(),
                "expected {value:?} to leave offline mode disabled"
            );
        }
    }

    #[test]
    fn tool_specs_match_the_tools_record() {
        // tools-manager.ts:29-71, plus :96 (rg's systemBinaryNames default) and
        // :319-322 (TERMUX_PACKAGES).
        let fd = ManagedTool::Fd.spec();
        assert_eq!(fd.key, "fd");
        assert_eq!(fd.name, "fd");
        assert_eq!(fd.binary_name, "fd");
        assert_eq!(fd.system_binary_names, ["fd", "fdfind"]);
        assert_eq!(fd.termux_package, "fd");
        assert_eq!(fd.repo, "sharkdp/fd");
        assert_eq!(fd.tag_prefix, "v");

        let rg = ManagedTool::Rg.spec();
        assert_eq!(rg.key, "rg");
        assert_eq!(rg.name, "ripgrep");
        assert_eq!(rg.binary_name, "rg");
        assert_eq!(rg.system_binary_names, ["rg"]);
        assert_eq!(rg.termux_package, "ripgrep");
        assert_eq!(rg.repo, "BurntSushi/ripgrep");
        assert_eq!(rg.tag_prefix, "");
    }

    #[test]
    fn tool_keys_round_trip() {
        assert_eq!(ManagedTool::from_key("fd"), Some(ManagedTool::Fd));
        assert_eq!(ManagedTool::from_key("rg"), Some(ManagedTool::Rg));
        assert_eq!(ManagedTool::from_key("ripgrep"), None);
        assert_eq!(ManagedTool::from_key(""), None);
        assert_eq!(ManagedTool::Fd.key(), "fd");
        assert_eq!(ManagedTool::Rg.key(), "rg");
    }

    #[test]
    fn platform_maps_rust_os_onto_node_platform() {
        assert_eq!(Platform::from_rust_os("windows"), Platform::Win32);
        assert_eq!(Platform::from_rust_os("macos"), Platform::Darwin);
        assert_eq!(Platform::from_rust_os("linux"), Platform::Linux);
        assert_eq!(Platform::from_rust_os("android"), Platform::Android);
        assert_eq!(Platform::from_rust_os("freebsd"), Platform::Other);
        assert_eq!(Platform::Win32.as_node_str(), "win32");
        assert_eq!(Platform::Win32.exe_suffix(), ".exe");
        assert_eq!(Platform::Linux.exe_suffix(), "");
        assert_eq!(Platform::Android.exe_suffix(), "");
    }

    #[test]
    fn home_env_var_is_userprofile_on_windows_and_home_elsewhere() {
        assert_eq!(home_env_var(Platform::Win32), HOME_ENV_WINDOWS);
        assert_eq!(home_env_var(Platform::Linux), HOME_ENV_POSIX);
        assert_eq!(home_env_var(Platform::Darwin), HOME_ENV_POSIX);
        assert_eq!(home_env_var(Platform::Android), HOME_ENV_POSIX);
    }
}
