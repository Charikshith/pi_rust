//! Port of `config.ts` — agent-dir resolution, all config paths, and app identity.
//!
//! Re-exports the `.pirust` naming constants from [`pirust_tools::binaries`] rather than
//! redeclaring them. Gated by `tests/fixtures/pi/cli/config_paths.json`.
//!
//! # What is ported
//!
//! | Pi | here |
//! |---|---|
//! | `expandTildePath` (`config.ts:498-500`) | [`ConfigEnv::expand_tilde_path`] |
//! | `getAgentDir` (`config.ts:515-521`) | [`ConfigEnv::agent_dir`] |
//! | `getCustomThemesDir` (`config.ts:524-526`) | [`ConfigEnv::custom_themes_dir`] |
//! | `getModelsPath` (`config.ts:529-531`) | [`ConfigEnv::models_path`] |
//! | `getAuthPath` (`config.ts:534-536`) | [`ConfigEnv::auth_path`] |
//! | `getSettingsPath` (`config.ts:539-541`) | [`ConfigEnv::settings_path`] |
//! | `getToolsDir` (`config.ts:544-546`) | [`ConfigEnv::tools_dir`] |
//! | `getBinDir` (`config.ts:549-551`) | [`ConfigEnv::bin_dir`] |
//! | `getPromptsDir` (`config.ts:554-556`) | [`ConfigEnv::prompts_dir`] |
//! | `getSessionsDir` (`config.ts:559-561`) | [`ConfigEnv::sessions_dir`] |
//! | `getDebugLogPath` (`config.ts:564-566`) | [`ConfigEnv::debug_log_path`] |
//! | `APP_NAME`/`CONFIG_DIR_NAME`/`VERSION`/`ENV_AGENT_DIR`/`ENV_SESSION_DIR` (`config.ts:489-496`) | [`AppIdentity`], [`PIRUST`] |
//!
//! Every accessor is `join(getAgentDir(), <leaf>)` and `getAgentDir()` re-reads the
//! environment on every call in Pi (no caching, `config.ts:516`). Here the environment is a
//! value — [`ConfigEnv`] — so "re-read per call" becomes "the caller decides when to
//! snapshot"; nothing is memoized inside this module either.
//!
//! # Composition — nothing here is a second implementation
//!
//! - **Naming constants.** [`CONFIG_DIR_NAME`], [`ENV_AGENT_DIR`], [`AGENT_DIR_NAME`],
//!   [`BIN_DIR_NAME`] and [`OFFLINE_ENV`] are *re-exports* of
//!   `pirust_tools::binaries` (`binaries.rs:154-175`), which owns them. Only [`APP_NAME`],
//!   [`ENV_SESSION_DIR`] and [`VERSION`] are declared here, because `binaries` has no need
//!   for them.
//! - **`os.homedir()`.** [`ConfigEnv::from_process_env_for`] calls
//!   [`pirust_tools::binaries::node_homedir`]; the `USERPROFILE`/`HOME` decision is not
//!   re-derived. [`HomeDirUnavailable`] is `binaries`' error type, re-exported.
//! - **Tilde expansion.** [`ConfigEnv::expand_tilde_path`] delegates to
//!   [`pirust_tools::path_utils::normalize_path`] with **default options** — which is
//!   literally what `expandTildePath` is (`config.ts:499` calls `normalizePath(path)` with
//!   no options). So `~`, `~/x`, win32 `~\x`, `file://…`, the no-trim rule, the
//!   unicode-space rule and the `@`-prefix rule all come from the module that was verified
//!   against 50 Pi cases. No tilde expander is written here.
//! - **`path.join`.** Also `path_utils` — see [`ConfigEnv::node_join`] for why the call
//!   looks the way it does. `std::path::PathBuf::join` is **not** used anywhere in this
//!   module: it does not reproduce Node's separator normalization (Node's
//!   `join("rel/agent", "models.json")` is `rel\agent\models.json` on win32, `PathBuf`'s is
//!   `rel/agent\models.json`), and the fixture's `env-set-relative` branch pins exactly
//!   that. For the same reason every accessor returns [`String`], as Pi does.
//! - **[`BinaryEnv`] interop.** [`ConfigEnv::from_binary_env`] /
//!   [`ConfigEnv::to_binary_env`] convert both ways so a process snapshots ambience once
//!   and the tools layer and the config layer cannot disagree. `ConfigEnv` stores
//!   `binaries::Platform` (all five `os.platform()` values) rather than
//!   `path_utils::Platform` (win32 vs. posix) precisely so that round trip is lossless —
//!   `binaries`' Android gate needs the distinction that `path_utils` collapses.
//!
//! One behaviour is **deliberately not** delegated to `BinaryEnv::agent_dir`: that method
//! hand-rolls the tilde rules and documents a `file://` gap (`binaries.rs:106-116`).
//! [`ConfigEnv::agent_dir`] goes through `normalize_path` instead, so
//! `PIRUST_CODING_AGENT_DIR=file:///C:/tmp/x` resolves to `C:\tmp\x` exactly as Pi does.
//! The two agree on every other input; when `binaries.rs` is next touched, its
//! `expand_tilde_path` should be deleted in favour of `path_utils` and the duplication
//! disappears.
//!
//! # pirust divergence (intentional, explicit)
//!
//! Pi derives its identity at runtime from the `piConfig` block of its own `package.json`
//! (`config.ts:470-496`), which makes the binary rebrandable — `piConfig.name = "tau"`
//! yields `~/.tau/agent` and `TAU_CODING_AGENT_DIR`. pirust drops that seam: identity is
//! the compile-time [`PIRUST`] constant, with [`VERSION`] from `CARGO_PKG_VERSION`
//! (Cargo's analogue of `pkg.version`, `config.ts:492`).
//!
//! | Pi | pirust |
//! |---|---|
//! | `~/.pi/agent` | **`~/.pirust/agent`** |
//! | `PI_CODING_AGENT_DIR` | **`PIRUST_CODING_AGENT_DIR`** |
//! | `PI_CODING_AGENT_SESSION_DIR` | **`PIRUST_CODING_AGENT_SESSION_DIR`** |
//! | `PI_OFFLINE` | **`PIRUST_OFFLINE`** |
//! | `pi-debug.log` | **`pirust-debug.log`** (`${APP_NAME}-debug.log`, `config.ts:565`) |
//!
//! pirust must not read or write Pi's `~/.pi`, and grows no fallback to it. Every file
//! *FORMAT* under the directory stays byte-compatible with Pi: `settings.json`,
//! `auth.json`, `models.json`, `models-store.json`, `trust.json`, `keybindings.json` and
//! the session `.jsonl` files keep Pi's schema, key order and
//! `JSON.stringify(x, null, 2)` formatting. Only the root directory differs.
//!
//! Because the fixture was captured from real Pi it carries Pi's identity (`.pi`,
//! `PI_CODING_AGENT_DIR`, `pi-debug.log`). [`PI`] exists so `tests/config_golden.rs` can
//! replay it against the *same* code path, proving the path composition rather than the
//! branding.
//!
//! # Not ported
//!
//! - **Install-method detection and self-update** (`config.ts:19-364`: `isBunBinary`,
//!   `detectInstallMethod`, `getSelfUpdateCommand`, `getUpdateInstruction`,
//!   `getGlobalPackageRoots`, …). feat-005 does not need any of it, and a stub would be
//!   worse than an absence: "which package manager installed this npm package" has no
//!   meaning for a Cargo-built binary. Nothing here pretends to answer it.
//! - **Package-asset paths** (`config.ts:367-464`: `getPackageDir`, `getThemesDir`,
//!   `getExportTemplateDir`, `getPackageJsonPath`, `getReadmePath`, `getDocsPath`,
//!   `getExamplesPath`, `getChangelogPath`, `getInteractiveAssetsDir`,
//!   `getBundledInteractiveAssetPath`). These resolve relative to Pi's *installed package
//!   directory* — the fixture's `packageAssetPaths` block is a property of the capture
//!   machine's checkout, not of Pi's logic (its own note says so). Spec §5.2 defers them;
//!   §14 specifies what pirust substitutes where the README/docs/examples paths leak into
//!   the system prompt.
//! - **`getShareViewerUrl`** (`config.ts:502-508`) — session sharing is unported, and spec
//!   §5.1 forbids introducing a `PIRUST_SHARE_VIEWER_URL` for it. The fixture's
//!   `getShareViewerUrl` block is therefore unclaimed by this module.
//! - **Session-dir precedence.** It does **not** live in `config.ts`: `main.ts:573-577`
//!   composes `--session-dir` > `$ENV_SESSION_DIR` > `settingsManager.getSessionDir()`,
//!   all three guarded by truthiness (so an empty string is skipped, not honoured), with
//!   `normalizePath` / `expandTildePath` — the same function — applied to the first two.
//!   Left to `main.rs`; this module contributes [`ENV_SESSION_DIR`] and
//!   [`ConfigEnv::expand_tilde_path`], which is all `main.rs` needs. The
//!   `getDefaultSessionDirPath` half of `tests/fixtures/pi/cli/session_dir.cases.jsonl`
//!   belongs to the session manager.

use std::path::PathBuf;

use pirust_tools::binaries;
use pirust_tools::path_utils::{self, PathEnv, PathInputOptions, Platform as PathPlatform};

pub use pirust_tools::binaries::{
    BinaryEnv, HomeDirUnavailable, Platform, AGENT_DIR_NAME, BIN_DIR_NAME, CONFIG_DIR_NAME,
    ENV_AGENT_DIR, HOME_ENV_POSIX, HOME_ENV_WINDOWS, OFFLINE_ENV,
};
pub use pirust_tools::path_utils::PathError;

// =============================================================================
// App identity (`config.ts:470-496`)
// =============================================================================

/// pirust's `APP_NAME` — Pi's is `piConfig?.name || "pi"` (`config.ts:489`).
///
/// Used for the debug-log file name (`config.ts:565`) and 27 times in `--help`
/// (spec §4.2). **Intentionally diverges from Pi** — see the module docs.
pub const APP_NAME: &str = "pirust";

/// Env var overriding the session directory — Pi builds it as
/// `${APP_NAME.toUpperCase()}_CODING_AGENT_SESSION_DIR` (`config.ts:496`).
///
/// Read by `main.ts:573`, not by anything in `config.ts`; declared here because this
/// module owns the naming. **Intentionally diverges from Pi** — see the module docs.
pub const ENV_SESSION_DIR: &str = "PIRUST_CODING_AGENT_SESSION_DIR";

/// `VERSION` (`config.ts:492`, `pkg.version || "0.0.0"`).
///
/// Cargo's `CARGO_PKG_VERSION` is the exact analogue of the `package.json` read Pi does at
/// module load — same value, resolved at compile time instead. Printed by `--version`
/// (`main.ts:522`); it does **not** appear in the help text (spec §4.2).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Leaf of `getCustomThemesDir` (`config.ts:525`).
pub const THEMES_DIR_NAME: &str = "themes";
/// Leaf of `getModelsPath` (`config.ts:530`).
pub const MODELS_FILE_NAME: &str = "models.json";
/// Leaf of `getAuthPath` (`config.ts:535`).
pub const AUTH_FILE_NAME: &str = "auth.json";
/// Leaf of `getSettingsPath` (`config.ts:540`), and of the per-project settings file
/// `join(cwd, CONFIG_DIR_NAME, "settings.json")` (`core/settings-manager.ts:196`).
pub const SETTINGS_FILE_NAME: &str = "settings.json";
/// Leaf of `getToolsDir` (`config.ts:545`).
pub const TOOLS_DIR_NAME: &str = "tools";
/// Leaf of `getPromptsDir` (`config.ts:555`).
pub const PROMPTS_DIR_NAME: &str = "prompts";
/// Leaf of `getSessionsDir` (`config.ts:560`).
pub const SESSIONS_DIR_NAME: &str = "sessions";
/// Suffix of `getDebugLogPath`: `` `${APP_NAME}-debug.log` `` (`config.ts:565`).
pub const DEBUG_LOG_SUFFIX: &str = "-debug.log";

/// The four identity strings that reach user-visible output, plus the version.
///
/// In Pi these are module-level `const`s derived from `package.json` (`config.ts:488-496`),
/// so every consumer reads globals. Here they are a value, which is what lets
/// `args::render_help` and this module's path composition be replayed under Pi's identity
/// in a golden test without forking the logic.
///
/// `Copy` and `&'static str` throughout: identity is fixed at compile time (see the
/// module docs on the dropped `piConfig` rebranding seam).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppIdentity {
    /// `APP_NAME` (`config.ts:489`) — the binary's own name.
    pub app_name: &'static str,
    /// `CONFIG_DIR_NAME` (`config.ts:491`) — the dot-directory under `$HOME`.
    pub config_dir_name: &'static str,
    /// `ENV_AGENT_DIR` (`config.ts:495`).
    pub env_agent_dir: &'static str,
    /// `ENV_SESSION_DIR` (`config.ts:496`).
    pub env_session_dir: &'static str,
    /// `VERSION` (`config.ts:492`).
    pub version: &'static str,
}

/// **The production identity.** Everything outside tests must use this one.
pub const PIRUST: AppIdentity = AppIdentity {
    app_name: APP_NAME,
    config_dir_name: CONFIG_DIR_NAME,
    env_agent_dir: ENV_AGENT_DIR,
    env_session_dir: ENV_SESSION_DIR,
    version: VERSION,
};

/// **Test-only: real Pi's identity, as captured by the fixtures.**
///
/// Not `#[cfg(test)]` because the golden suites are separate crates
/// (`tests/config_golden.rs`, `tests/help_golden.rs`) and could not see it otherwise.
/// Values verbatim from `tests/fixtures/pi/cli/config_paths.json` (`identity` block) and
/// `tests/fixtures/pi/cli/help.identity.json` (which additionally records
/// `VERSION = "0.80.10"`, the version of Pi the captures were taken from).
///
/// Passing this to [`ConfigEnv`] or to the help renderer makes pirust emit *Pi's* strings,
/// which is how the goldens are compared. Never use it in production: it would make pirust
/// read and write Pi's `~/.pi`.
pub const PI: AppIdentity = AppIdentity {
    app_name: "pi",
    config_dir_name: ".pi",
    env_agent_dir: "PI_CODING_AGENT_DIR",
    env_session_dir: "PI_CODING_AGENT_SESSION_DIR",
    version: "0.80.10",
};

// =============================================================================
// Errors
// =============================================================================

/// Why a config path could not be produced. Both variants correspond to a Pi `throw`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigPathError {
    /// `os.homedir()` was needed and unavailable — Pi throws from `homedir()`
    /// (`config.ts:520`). Reuses `binaries`' error rather than declaring a second one.
    #[error(transparent)]
    HomeDirUnavailable(#[from] HomeDirUnavailable),
    /// `fileURLToPath` threw inside `normalizePath` (`utils/paths.ts:74-76`) — e.g.
    /// `PIRUST_CODING_AGENT_DIR=file:///tmp/x` on win32, which Pi reports as
    /// `TypeError: File URL path must be absolute`. Pi does not catch it here.
    #[error(transparent)]
    Path(#[from] PathError),
}

// =============================================================================
// The environment seam
// =============================================================================

/// Every ambient input `config.ts`'s path accessors read: `os.platform()`,
/// `os.homedir()` and `process.env[ENV_AGENT_DIR]` — plus the [`AppIdentity`] that decides
/// *which* env var and *which* dot-directory those are.
///
/// Snapshot it once with [`ConfigEnv::from_process_env`] (the only function in this module
/// that touches globals) and thread it through. Tests build a literal instead, so they
/// never call `std::env::set_var` — which is process-global and would race under
/// `cargo test`'s parallel threads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigEnv {
    /// Which app this is. [`PIRUST`] in production.
    pub identity: AppIdentity,
    /// `os.platform()`. Only `win32` vs. not affects path composition (the `~\` form and
    /// the `node:path` flavour), but the full value is kept so [`ConfigEnv::to_binary_env`]
    /// round-trips.
    pub platform: Platform,
    /// `os.homedir()`, or `None` when it could not be determined (Pi throws in that case;
    /// here it surfaces as [`ConfigPathError::HomeDirUnavailable`], and only if some path
    /// actually needs it). `Some("")` is a *set but empty* home, which Node joins onto.
    pub home_dir: Option<String>,
    /// `process.env[identity.env_agent_dir]` — the raw, unexpanded value. `Some("")` is
    /// honoured as JS honours it: falsy, hence ignored (`config.ts:517`).
    pub agent_dir_override: Option<String>,
}

impl ConfigEnv {
    /// Snapshot the real process environment under the production identity [`PIRUST`].
    pub fn from_process_env() -> Self {
        Self::from_process_env_for(PIRUST)
    }

    /// [`ConfigEnv::from_process_env`] for an arbitrary identity — the env var actually
    /// read is `identity.env_agent_dir`, mirroring Pi, where `ENV_AGENT_DIR` is itself
    /// derived from `APP_NAME` (`config.ts:495`).
    ///
    /// `os.homedir()` comes from [`pirust_tools::binaries::node_homedir`]; on Windows a
    /// home path that is not valid UTF-8 is lossily converted, as `PathBuf` → `String`
    /// requires. Pi, holding UTF-16, would keep the original bytes.
    pub fn from_process_env_for(identity: AppIdentity) -> Self {
        let platform = Platform::current();
        Self {
            identity,
            platform,
            home_dir: binaries::node_homedir(platform)
                .map(|home| home.to_string_lossy().into_owned()),
            agent_dir_override: std::env::var(identity.env_agent_dir).ok(),
        }
    }

    /// Adopt the ambience the tools layer already captured, under identity [`PIRUST`].
    ///
    /// Lossless: [`BinaryEnv`] reads the same three inputs with the same
    /// `PIRUST_CODING_AGENT_DIR` (`binaries.rs:161`), so no value is re-derived. Only
    /// `BinaryEnv::offline` is dropped — nothing in `config.ts` reads it.
    pub fn from_binary_env(env: &BinaryEnv) -> Self {
        Self {
            identity: PIRUST,
            platform: env.platform,
            home_dir: env
                .home_dir
                .as_ref()
                .map(|home| home.to_string_lossy().into_owned()),
            agent_dir_override: env.agent_dir_override.clone(),
        }
    }

    /// The reverse of [`ConfigEnv::from_binary_env`], so one snapshot can drive both
    /// layers. `offline` is `process.env[OFFLINE_ENV]`, which this module never reads.
    ///
    /// Only meaningful for [`PIRUST`]: `BinaryEnv`'s own accessors compose the constants
    /// from `binaries` directly and have no [`AppIdentity`] to parameterize, so a
    /// `ConfigEnv` carrying [`PI`] would still yield `~/.pirust/agent/bin` here.
    pub fn to_binary_env(&self, offline: Option<String>) -> BinaryEnv {
        BinaryEnv {
            platform: self.platform,
            home_dir: self.home_dir.as_deref().map(PathBuf::from),
            agent_dir_override: self.agent_dir_override.clone(),
            offline,
        }
    }

    // -------------------------------------------------------------------------
    // config.ts:498-500 — expandTildePath
    // -------------------------------------------------------------------------

    /// `expandTildePath(path)` = `normalizePath(path)` with **no options**
    /// (`config.ts:498-500`, `utils/paths.ts:57-79`).
    ///
    /// Delegated wholesale to [`pirust_tools::path_utils::normalize_path`]; the rules are
    /// documented and fixture-verified there. In summary: `"~"` → home; `"~/rest"` (plus
    /// `"~\rest"` on win32 only) → `join(home, rest)`; a leading `file://` (case
    /// sensitive) → `fileURLToPath`, which can throw; anything else verbatim — *not*
    /// resolved, so a relative value stays relative and separators are untouched. Because
    /// no options are passed, `trim`, `normalizeUnicodeSpaces` and `stripAtPrefix` are all
    /// off, so `"  ~/x  "`, `"a\u{a0}b"` and `"@file.md"` come back unchanged.
    pub fn expand_tilde_path(&self, input: &str) -> Result<String, ConfigPathError> {
        // `normalize_path` takes the home dir as a plain `&str` (its own `os.homedir()`
        // fallback yields `""` when unset). Pi would instead throw from `homedir()`, but
        // only when the value actually needs expanding — hence this predicate. The
        // expansion itself is entirely `path_utils`'.
        let home = if self.tilde_expansion_needs_home(input) {
            self.require_home()?
        } else {
            ""
        };
        let env = self.path_env(home);
        Ok(path_utils::normalize_path(
            &env,
            input,
            &PathInputOptions::default(),
        )?)
    }

    // -------------------------------------------------------------------------
    // config.ts:515-566 — the path accessors
    // -------------------------------------------------------------------------

    /// `getAgentDir()` (`config.ts:515-521`): the [`AppIdentity::env_agent_dir`] override,
    /// tilde-expanded, else `join(homedir(), CONFIG_DIR_NAME, "agent")`.
    ///
    /// The override wins only when **truthy** (`if (envDir)`, `config.ts:517`), so
    /// `PIRUST_CODING_AGENT_DIR=""` behaves exactly like an unset variable. It is passed
    /// through `expandTildePath`, **not** `resolve`, so an absolute value is returned
    /// verbatim (no canonicalization) and a relative value stays relative — every
    /// downstream accessor then joins onto a relative base.
    pub fn agent_dir(&self) -> Result<String, ConfigPathError> {
        if let Some(env_dir) = self.agent_dir_override.as_deref().filter(|d| !d.is_empty()) {
            return self.expand_tilde_path(env_dir);
        }
        let home = self.require_home()?;
        Ok(self.node_join(home, &[self.identity.config_dir_name, AGENT_DIR_NAME]))
    }

    /// `getCustomThemesDir()` — `join(getAgentDir(), "themes")` (`config.ts:524-526`).
    pub fn custom_themes_dir(&self) -> Result<String, ConfigPathError> {
        self.agent_dir_join(THEMES_DIR_NAME)
    }

    /// `getModelsPath()` — `join(getAgentDir(), "models.json")` (`config.ts:529-531`).
    pub fn models_path(&self) -> Result<String, ConfigPathError> {
        self.agent_dir_join(MODELS_FILE_NAME)
    }

    /// `getAuthPath()` — `join(getAgentDir(), "auth.json")` (`config.ts:534-536`).
    pub fn auth_path(&self) -> Result<String, ConfigPathError> {
        self.agent_dir_join(AUTH_FILE_NAME)
    }

    /// `getSettingsPath()` — `join(getAgentDir(), "settings.json")` (`config.ts:539-541`).
    pub fn settings_path(&self) -> Result<String, ConfigPathError> {
        self.agent_dir_join(SETTINGS_FILE_NAME)
    }

    /// `getToolsDir()` — `join(getAgentDir(), "tools")` (`config.ts:544-546`).
    ///
    /// Note this is *not* where the managed `fd`/`rg` binaries live; that is
    /// [`ConfigEnv::bin_dir`] (`tools-manager.ts:10` aliases `TOOLS_DIR = getBinDir()`).
    pub fn tools_dir(&self) -> Result<String, ConfigPathError> {
        self.agent_dir_join(TOOLS_DIR_NAME)
    }

    /// `getBinDir()` — `join(getAgentDir(), "bin")` (`config.ts:549-551`). Same directory
    /// [`pirust_tools::binaries::BinaryEnv::tools_dir`] resolves.
    pub fn bin_dir(&self) -> Result<String, ConfigPathError> {
        self.agent_dir_join(BIN_DIR_NAME)
    }

    /// `getPromptsDir()` — `join(getAgentDir(), "prompts")` (`config.ts:554-556`).
    pub fn prompts_dir(&self) -> Result<String, ConfigPathError> {
        self.agent_dir_join(PROMPTS_DIR_NAME)
    }

    /// `getSessionsDir()` — `join(getAgentDir(), "sessions")` (`config.ts:559-561`).
    ///
    /// This is the *store root*. It is unrelated to the `--session-dir` override, whose
    /// precedence lives in `main.ts:573-577` (see the module docs).
    pub fn sessions_dir(&self) -> Result<String, ConfigPathError> {
        self.agent_dir_join(SESSIONS_DIR_NAME)
    }

    /// `getDebugLogPath()` — `` join(getAgentDir(), `${APP_NAME}-debug.log`) ``
    /// (`config.ts:564-566`). The only accessor whose leaf depends on the identity:
    /// `pirust-debug.log` here, `pi-debug.log` for Pi.
    pub fn debug_log_path(&self) -> Result<String, ConfigPathError> {
        let leaf = format!("{}{DEBUG_LOG_SUFFIX}", self.identity.app_name);
        self.agent_dir_join(&leaf)
    }

    // -------------------------------------------------------------------------
    // Internals
    // -------------------------------------------------------------------------

    /// `join(getAgentDir(), leaf)` — the shape of every accessor above.
    fn agent_dir_join(&self, leaf: &str) -> Result<String, ConfigPathError> {
        let agent_dir = self.agent_dir()?;
        Ok(self.node_join(&agent_dir, &[leaf]))
    }

    /// Node's `path.join(base, ...tail)` for this platform, borrowed from
    /// [`pirust_tools::path_utils`] instead of reimplemented.
    ///
    /// That module's transcription of `node:path` is private, but `normalizePath`'s tilde
    /// branch *is* `platform.join([home, rest])` (`utils/paths.ts:70`) — so passing
    /// `"~/<tail>"` with `home_dir = base` yields exactly `join(base, ...tail)` from the
    /// verified implementation. Joining `tail` with `/` first is safe: `node:path` builds
    /// its own string by concatenating the arguments with the platform separator and
    /// normalizing **once**, and win32 normalization treats `/` and `\` identically, so
    /// `join(base, "a", "b")` and `join(base, "a/b")` are the same string on both
    /// platforms. Every `tail` here is a separator-free literal.
    ///
    /// Infallible: the tilde branch `return`s before the `file://` branch, the only place
    /// `normalize_path` can fail.
    fn node_join(&self, base: &str, tail: &[&str]) -> String {
        let env = self.path_env("");
        let options = PathInputOptions {
            home_dir: Some(base),
            ..PathInputOptions::default()
        };
        path_utils::normalize_path(&env, &format!("~/{}", tail.join("/")), &options)
            .expect("normalizePath's tilde branch returns before the file:// branch")
    }

    /// The `PathEnv` `path_utils` wants: `process.platform` collapsed to win32-vs-posix,
    /// the home dir to expand `~` against, and `process.cwd()` — which `normalizePath`
    /// never reads (only `resolvePath` does), hence the empty string.
    fn path_env(&self, home_dir: &str) -> PathEnv {
        PathEnv {
            platform: if self.platform == Platform::Win32 {
                PathPlatform::Win32
            } else {
                PathPlatform::Posix
            },
            home_dir: home_dir.to_string(),
            cwd: String::new(),
        }
    }

    /// Would `normalizePath` reach `os.homedir()` for this input? (`utils/paths.ts:66-72`
    /// with default options: `expandTilde` defaults to true, so the check is just the
    /// three tilde forms.)
    fn tilde_expansion_needs_home(&self, input: &str) -> bool {
        input == "~"
            || input.starts_with("~/")
            || (self.platform == Platform::Win32 && input.starts_with("~\\"))
    }

    /// `os.homedir()` or Pi's throw (`config.ts:520`).
    fn require_home(&self) -> Result<&str, HomeDirUnavailable> {
        self.home_dir.as_deref().ok_or(HomeDirUnavailable {
            home_env: binaries::home_env_var(self.platform),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A win32 `ConfigEnv` under Pi's identity, i.e. the fixture's ambience.
    fn pi_win32(agent_dir_override: Option<&str>) -> ConfigEnv {
        ConfigEnv {
            identity: PI,
            platform: Platform::Win32,
            home_dir: Some("C:\\Users\\oracle".to_string()),
            agent_dir_override: agent_dir_override.map(str::to_string),
        }
    }

    #[test]
    fn identity_reuses_the_shared_constants() {
        // Re-exported from binaries.rs:154-175, not redeclared here.
        assert_eq!(PIRUST.config_dir_name, CONFIG_DIR_NAME);
        assert_eq!(PIRUST.env_agent_dir, ENV_AGENT_DIR);
        assert_eq!(CONFIG_DIR_NAME, ".pirust");
        assert_eq!(ENV_AGENT_DIR, "PIRUST_CODING_AGENT_DIR");
        assert_eq!(OFFLINE_ENV, "PIRUST_OFFLINE");
        assert_eq!(AGENT_DIR_NAME, "agent");
        assert_eq!(BIN_DIR_NAME, "bin");
        // Declared here because binaries.rs has no use for them.
        assert_eq!(PIRUST.app_name, "pirust");
        assert_eq!(PIRUST.env_session_dir, "PIRUST_CODING_AGENT_SESSION_DIR");
        assert_eq!(PIRUST.version, env!("CARGO_PKG_VERSION"));
        // `ENV_*` follow Pi's `${APP_NAME.toUpperCase()}_…` construction (config.ts:495).
        assert_eq!(
            PIRUST.env_agent_dir,
            format!("{}_CODING_AGENT_DIR", PIRUST.app_name.to_uppercase())
        );
        assert_eq!(
            PIRUST.env_session_dir,
            format!(
                "{}_CODING_AGENT_SESSION_DIR",
                PIRUST.app_name.to_uppercase()
            )
        );
        assert_eq!(
            PI.env_session_dir,
            format!("{}_CODING_AGENT_SESSION_DIR", PI.app_name.to_uppercase())
        );
    }

    #[test]
    fn production_identity_composes_the_pirust_directory() {
        let env = ConfigEnv {
            identity: PIRUST,
            ..pi_win32(None)
        };
        assert_eq!(
            env.agent_dir().unwrap(),
            "C:\\Users\\oracle\\.pirust\\agent"
        );
        // The debug log's leaf tracks APP_NAME (config.ts:565).
        assert_eq!(
            env.debug_log_path().unwrap(),
            "C:\\Users\\oracle\\.pirust\\agent\\pirust-debug.log"
        );
        assert_eq!(
            pi_win32(None).debug_log_path().unwrap(),
            "C:\\Users\\oracle\\.pi\\agent\\pi-debug.log"
        );
    }

    #[test]
    fn missing_home_is_only_an_error_when_it_is_needed() {
        let no_home = ConfigEnv {
            home_dir: None,
            ..pi_win32(None)
        };
        assert_eq!(
            no_home.agent_dir(),
            Err(ConfigPathError::HomeDirUnavailable(HomeDirUnavailable {
                home_env: HOME_ENV_WINDOWS,
            }))
        );
        // An absolute override never consults homedir(), so it still resolves.
        let with_override = ConfigEnv {
            agent_dir_override: Some("D:\\agent".to_string()),
            ..no_home.clone()
        };
        assert_eq!(
            with_override.settings_path().unwrap(),
            "D:\\agent\\settings.json"
        );
        // …but a tilde override does.
        let tilde = ConfigEnv {
            agent_dir_override: Some("~/x".to_string()),
            ..no_home
        };
        assert!(matches!(
            tilde.agent_dir(),
            Err(ConfigPathError::HomeDirUnavailable(_))
        ));
    }

    #[test]
    fn an_empty_home_is_joined_onto_like_node_does() {
        // libuv distinguishes "set" from "unset"; Node then joins onto "".
        let env = ConfigEnv {
            home_dir: Some(String::new()),
            ..pi_win32(None)
        };
        assert_eq!(env.agent_dir().unwrap(), ".pi\\agent");
    }

    #[test]
    fn file_url_override_propagates_the_typeerror() {
        // utils/paths.ts:74-76; Pi does not catch, so neither do we.
        let env = pi_win32(Some("file:///tmp/x"));
        assert_eq!(
            env.agent_dir().unwrap_err().to_string(),
            "File URL path must be absolute"
        );
        // A drive-lettered file URL converts (binaries.rs's documented gap, closed here).
        assert_eq!(
            pi_win32(Some("file:///C:/tmp/x")).agent_dir().unwrap(),
            "C:\\tmp\\x"
        );
    }

    #[test]
    fn binary_env_round_trips() {
        let config = ConfigEnv {
            identity: PIRUST,
            ..pi_win32(Some("~/custom"))
        };
        let binary = config.to_binary_env(Some("1".to_string()));
        assert!(binary.is_offline());
        assert_eq!(ConfigEnv::from_binary_env(&binary), config);
        // Both layers agree on the bin dir for the production identity.
        assert_eq!(
            binary.tools_dir().unwrap().to_string_lossy(),
            config.bin_dir().unwrap()
        );
    }

    #[test]
    fn node_join_normalizes_separators_where_pathbuf_would_not() {
        // The `env-set-relative` fixture branch in miniature: node's join rewrites the
        // base's `/` to `\` on win32, `PathBuf::join` would leave `rel/agent\models.json`.
        let env = pi_win32(Some("rel/agent"));
        assert_eq!(env.agent_dir().unwrap(), "rel/agent");
        assert_eq!(env.models_path().unwrap(), "rel\\agent\\models.json");
        // Multi-part joins agree with node's single-normalize semantics.
        assert_eq!(
            env.node_join("C:\\h", &[".pi", "agent"]),
            env.node_join(&env.node_join("C:\\h", &[".pi"]), &["agent"])
        );
    }

    #[test]
    fn win32_backslash_tilde_is_platform_gated() {
        // utils/paths.ts:69 — the `~\` form is win32-only.
        assert_eq!(
            pi_win32(Some("~\\custom")).agent_dir().unwrap(),
            "C:\\Users\\oracle\\custom"
        );
        let posix = ConfigEnv {
            platform: Platform::Linux,
            home_dir: Some("/home/oracle".to_string()),
            ..pi_win32(Some("~\\custom"))
        };
        assert_eq!(posix.agent_dir().unwrap(), "~\\custom");
        assert_eq!(
            ConfigEnv {
                agent_dir_override: Some("~/custom".to_string()),
                ..posix
            }
            .sessions_dir()
            .unwrap(),
            "/home/oracle/custom/sessions"
        );
    }

    #[test]
    fn from_process_env_reads_the_identity_s_own_variable() {
        // Cannot assert the value (the machine's env is whatever it is), but the identity
        // must be threaded through and the platform must match the build target.
        let env = ConfigEnv::from_process_env();
        assert_eq!(env.identity, PIRUST);
        assert_eq!(env.platform, Platform::current());
        assert_eq!(
            env.agent_dir_override,
            std::env::var(PIRUST.env_agent_dir).ok()
        );
        assert_eq!(
            ConfigEnv::from_process_env_for(PI).agent_dir_override,
            std::env::var(PI.env_agent_dir).ok()
        );
    }
}
