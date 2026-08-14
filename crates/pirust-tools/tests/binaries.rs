//! Resolution tests for `binaries.rs` — the ported half of
//! `packages/coding-agent/src/utils/tools-manager.ts`.
//!
//! Every ambient input is threaded through a [`BinaryEnv`] value and a fake
//! [`CommandProbe`], so:
//! - no test reads or writes the real `HOME` / `USERPROFILE` / `PIRUST_*` env
//!   (`std::env::set_var` is process-global and races parallel tests), and
//! - no test depends on this machine actually having `rg` / `fd` installed.
//!
//! The managed-binary directory is a `tempfile::TempDir` handed to
//! `BinaryEnv::agent_dir_override`, i.e. exactly what setting
//! `PIRUST_CODING_AGENT_DIR` would produce.
//!
//! The expected values are **observed Pi behaviour**, not invented: each branch
//! was forced in real Pi (`node --experimental-strip-types`, `PI_CODING_AGENT_DIR`
//! / `PI_OFFLINE` / `PATH` set per scenario) and the returned value / printed
//! line copied here. The observation table is in the `binaries` module docs.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use async_trait::async_trait;
use pirust_tools::binaries::{
    ensure_tool, ensure_tool_outcome, get_tool_path, home_env_var, BinaryEnv, CommandProbe,
    EnsureOutcome, ManagedTool, Platform, SpawnProbe, AGENT_DIR_NAME, BIN_DIR_NAME,
    CONFIG_DIR_NAME, ENV_AGENT_DIR, HOME_ENV_POSIX, HOME_ENV_WINDOWS, OFFLINE_ENV,
};
use tempfile::TempDir;

// -----------------------------------------------------------------------------
// Test doubles
// -----------------------------------------------------------------------------

/// `commandExists` (`tools-manager.ts:74-82`) without spawning anything, and
/// recording the probe order so tests can assert *which* candidate names Pi
/// would have tried and in what order.
#[derive(Debug, Default)]
struct FakeProbe {
    existing: Vec<String>,
    calls: Mutex<Vec<String>>,
}

impl FakeProbe {
    fn with(existing: &[&str]) -> Self {
        Self {
            existing: existing.iter().map(|c| (*c).to_string()).collect(),
            calls: Mutex::new(Vec::new()),
        }
    }

    fn none() -> Self {
        Self::with(&[])
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().expect("probe mutex").clone()
    }
}

#[async_trait]
impl CommandProbe for FakeProbe {
    async fn command_exists(&self, command: &str) -> bool {
        self.calls
            .lock()
            .expect("probe mutex")
            .push(command.to_string());
        self.existing.iter().any(|c| c == command)
    }
}

/// A [`BinaryEnv`] whose agent dir is the `PIRUST_CODING_AGENT_DIR` override
/// (so `home_dir` is irrelevant), online, on `platform`.
fn env_with_override(agent_dir: &Path, platform: Platform) -> BinaryEnv {
    BinaryEnv {
        platform,
        home_dir: None,
        agent_dir_override: Some(agent_dir.to_string_lossy().into_owned()),
        offline: None,
    }
}

/// A [`BinaryEnv`] with no override, resolving the agent dir from `home`
/// (`join(homedir(), ".pirust", "agent")`).
fn env_with_home(home: &Path, platform: Platform) -> BinaryEnv {
    BinaryEnv {
        platform,
        home_dir: Some(home.to_path_buf()),
        agent_dir_override: None,
        offline: None,
    }
}

/// Creates `<agent_dir>/bin/<name>` as an empty file and returns its path.
fn install_managed(agent_dir: &Path, name: &str) -> PathBuf {
    let bin = agent_dir.join(BIN_DIR_NAME);
    fs::create_dir_all(&bin).expect("create bin dir");
    let path = bin.join(name);
    fs::write(&path, b"").expect("create fake binary");
    path
}

/// The file name `getToolPath` looks for on `platform` (`tools-manager.ts:90`).
fn managed_file_name(tool: ManagedTool, platform: Platform) -> String {
    format!("{}{}", tool.spec().binary_name, platform.exe_suffix())
}

// -----------------------------------------------------------------------------
// The pirust naming constants (intentional divergence from Pi)
// -----------------------------------------------------------------------------

#[test]
fn pirust_naming_constants_are_the_single_source_of_truth() {
    // Deliberately NOT Pi's `.pi` / `PI_CODING_AGENT_DIR` / `PI_OFFLINE`
    // (config.ts:489-495, tools-manager.ts:15): pirust owns its own state dir.
    assert_eq!(CONFIG_DIR_NAME, ".pirust");
    assert_eq!(ENV_AGENT_DIR, "PIRUST_CODING_AGENT_DIR");
    assert_eq!(OFFLINE_ENV, "PIRUST_OFFLINE");
    // Layout components stay identical to Pi's (config.ts:515-521, :549-551).
    assert_eq!(AGENT_DIR_NAME, "agent");
    assert_eq!(BIN_DIR_NAME, "bin");
    // os.homedir()'s env-var half (libuv uv_os_homedir).
    assert_eq!(HOME_ENV_WINDOWS, "USERPROFILE");
    assert_eq!(HOME_ENV_POSIX, "HOME");
    assert_eq!(home_env_var(Platform::Win32), HOME_ENV_WINDOWS);
    assert_eq!(home_env_var(Platform::Linux), HOME_ENV_POSIX);
}

#[test]
fn default_tools_dir_is_home_dot_pirust_agent_bin() {
    let home = TempDir::new().expect("tempdir");
    let env = env_with_home(home.path(), Platform::Linux);

    assert_eq!(
        env.agent_dir().expect("agent dir"),
        home.path().join(".pirust").join("agent")
    );
    assert_eq!(
        env.tools_dir().expect("tools dir"),
        home.path().join(".pirust").join("agent").join("bin")
    );
    assert_eq!(
        env.managed_binary_path(ManagedTool::Rg).expect("rg path"),
        home.path()
            .join(".pirust")
            .join("agent")
            .join("bin")
            .join("rg")
    );
    assert_eq!(
        env.managed_binary_path(ManagedTool::Fd).expect("fd path"),
        home.path()
            .join(".pirust")
            .join("agent")
            .join("bin")
            .join("fd")
    );
}

#[tokio::test]
async fn pis_dot_pi_directory_is_never_consulted() {
    // Regression guard: pirust must NOT gain a `~/.pi` fallback that Pi does not
    // have. On the dev machine `~/.pi/agent/bin` really does hold rg/fd while
    // `~/.pirust/agent/bin` is empty, and the correct behaviour is a miss.
    let home = TempDir::new().expect("tempdir");
    let pi_bin = home.path().join(".pi").join("agent").join("bin");
    fs::create_dir_all(&pi_bin).expect("create ~/.pi/agent/bin");
    for name in ["rg", "rg.exe", "fd", "fd.exe", "fdfind"] {
        fs::write(pi_bin.join(name), b"").expect("create fake binary");
    }

    let env = env_with_home(home.path(), Platform::current());
    let probe = FakeProbe::none();

    assert_eq!(
        get_tool_path(ManagedTool::Rg, &env, &probe).await,
        Ok(None),
        "~/.pi must not be searched"
    );
    assert_eq!(get_tool_path(ManagedTool::Fd, &env, &probe).await, Ok(None));
}

// -----------------------------------------------------------------------------
// 1. Managed binary in the tools dir (tools-manager.ts:89-93)
// -----------------------------------------------------------------------------

#[tokio::test]
async fn managed_binary_is_returned_as_a_full_path_without_probing() {
    for platform in [Platform::Win32, Platform::Linux, Platform::Darwin] {
        for tool in [ManagedTool::Rg, ManagedTool::Fd] {
            let agent = TempDir::new().expect("tempdir");
            let expected = install_managed(agent.path(), &managed_file_name(tool, platform));
            let env = env_with_override(agent.path(), platform);
            let probe = FakeProbe::with(&["rg", "fd", "fdfind"]);

            let got = get_tool_path(tool, &env, &probe).await.expect("resolve");
            assert_eq!(got.as_deref(), Some(expected.to_string_lossy().as_ref()));
            // The managed hit returns before the PATH probe (`return localPath`).
            assert!(
                probe.calls().is_empty(),
                "managed hit must short-circuit the PATH probe"
            );
        }
    }
}

#[tokio::test]
async fn managed_binary_found_via_the_home_dir_layout() {
    let home = TempDir::new().expect("tempdir");
    let platform = Platform::current();
    let agent = home.path().join(CONFIG_DIR_NAME).join(AGENT_DIR_NAME);
    let expected = install_managed(&agent, &managed_file_name(ManagedTool::Rg, platform));

    let env = env_with_home(home.path(), platform);
    let probe = FakeProbe::with(&["rg"]);

    assert_eq!(
        get_tool_path(ManagedTool::Rg, &env, &probe).await,
        Ok(Some(expected.to_string_lossy().into_owned()))
    );
    assert!(probe.calls().is_empty());
}

#[tokio::test]
async fn managed_lookup_uses_existssync_semantics_so_a_directory_counts() {
    // `existsSync` (tools-manager.ts:91) is true for directories too, and Pi
    // returns the path regardless of type. Ported as-is — observed in Pi: a
    // *directory* named rg.exe in the tools dir is returned as the binary path.

    let agent = TempDir::new().expect("tempdir");
    let platform = Platform::current();
    let bin = agent.path().join(BIN_DIR_NAME);
    let as_dir = bin.join(managed_file_name(ManagedTool::Fd, platform));
    fs::create_dir_all(&as_dir).expect("create dir named like the binary");

    let env = env_with_override(agent.path(), platform);
    let probe = FakeProbe::with(&["fd"]);

    assert_eq!(
        get_tool_path(ManagedTool::Fd, &env, &probe).await,
        Ok(Some(as_dir.to_string_lossy().into_owned()))
    );
    assert!(probe.calls().is_empty());
}

#[tokio::test]
async fn exe_suffix_is_appended_only_on_win32() {
    // `binaryName + (platform() === "win32" ? ".exe" : "")` (tools-manager.ts:90).
    let agent = TempDir::new().expect("tempdir");
    let with_exe = install_managed(agent.path(), "rg.exe");

    let win = env_with_override(agent.path(), Platform::Win32);
    let probe = FakeProbe::none();
    assert_eq!(
        get_tool_path(ManagedTool::Rg, &win, &probe).await,
        Ok(Some(with_exe.to_string_lossy().into_owned())),
        "win32 must look for rg.exe"
    );
    assert!(probe.calls().is_empty());

    // Same directory, non-win32 platform: `rg.exe` is not the name it looks for.
    for platform in [Platform::Linux, Platform::Darwin, Platform::Android] {
        let posix = env_with_override(agent.path(), platform);
        let probe = FakeProbe::none();
        assert_eq!(
            get_tool_path(ManagedTool::Rg, &posix, &probe).await,
            Ok(None),
            "{} must look for bare `rg`",
            platform.as_node_str()
        );
        assert_eq!(probe.calls(), ["rg"], "and must fall through to the probe");
    }
}

#[tokio::test]
async fn extensionless_binary_is_not_found_on_win32() {
    let agent = TempDir::new().expect("tempdir");
    install_managed(agent.path(), "rg");

    let win = env_with_override(agent.path(), Platform::Win32);
    let probe = FakeProbe::none();
    assert_eq!(get_tool_path(ManagedTool::Rg, &win, &probe).await, Ok(None));
    assert_eq!(probe.calls(), ["rg"]);
}

// -----------------------------------------------------------------------------
// 2. System PATH probe (tools-manager.ts:95-101)
// -----------------------------------------------------------------------------

#[tokio::test]
async fn missing_managed_binary_falls_through_to_the_path_probe() {
    let agent = TempDir::new().expect("tempdir");
    fs::create_dir_all(agent.path().join(BIN_DIR_NAME)).expect("empty bin dir");
    let env = env_with_override(agent.path(), Platform::current());
    let probe = FakeProbe::with(&["rg"]);

    // Pi returns the *bare command name*, not an absolute path: it relies on
    // PATH lookup happening again at spawn time (tools-manager.ts:98-100).
    // Observed in Pi: with an empty PI_CODING_AGENT_DIR and rg.exe on PATH,
    // getToolPath("rg") === "rg".

    assert_eq!(
        get_tool_path(ManagedTool::Rg, &env, &probe).await,
        Ok(Some("rg".to_string()))
    );
    assert_eq!(probe.calls(), ["rg"]);
}

#[tokio::test]
async fn a_missing_tools_dir_is_not_an_error_it_just_probes() {
    // The tools dir need not exist at all (Pi only `existsSync`es the file).
    let agent = TempDir::new().expect("tempdir");
    let never_created = agent.path().join("does-not-exist");
    let env = env_with_override(&never_created, Platform::current());
    let probe = FakeProbe::with(&["fdfind"]);

    assert_eq!(
        get_tool_path(ManagedTool::Fd, &env, &probe).await,
        Ok(Some("fdfind".to_string()))
    );
    assert_eq!(probe.calls(), ["fd", "fdfind"]);
}

#[tokio::test]
async fn fd_probes_fd_then_fdfind() {
    // `systemBinaryNames: ["fd", "fdfind"]` (tools-manager.ts:34), in order.
    // Observed in Pi: with only fdfind.exe on PATH, getToolPath("fd") === "fdfind".

    let agent = TempDir::new().expect("tempdir");
    let platform = Platform::current();

    // Both present → the first candidate wins and `fdfind` is never probed.
    let env = env_with_override(agent.path(), platform);
    let probe = FakeProbe::with(&["fd", "fdfind"]);
    assert_eq!(
        get_tool_path(ManagedTool::Fd, &env, &probe).await,
        Ok(Some("fd".to_string()))
    );
    assert_eq!(probe.calls(), ["fd"]);

    // Only the Debian/Ubuntu name present → both are probed, `fdfind` returned.
    let probe = FakeProbe::with(&["fdfind"]);
    assert_eq!(
        get_tool_path(ManagedTool::Fd, &env, &probe).await,
        Ok(Some("fdfind".to_string()))
    );
    assert_eq!(probe.calls(), ["fd", "fdfind"]);

    // Neither present → both probed, nothing found.
    let probe = FakeProbe::none();
    assert_eq!(get_tool_path(ManagedTool::Fd, &env, &probe).await, Ok(None));
    assert_eq!(probe.calls(), ["fd", "fdfind"]);
}

#[tokio::test]
async fn rg_probes_only_rg() {
    // `TOOLS.rg` declares no `systemBinaryNames`, so the candidates default to
    // `[binaryName]` (tools-manager.ts:96) — `ripgrep` is NEVER probed.
    let agent = TempDir::new().expect("tempdir");
    let env = env_with_override(agent.path(), Platform::current());

    let probe = FakeProbe::with(&["ripgrep"]);
    assert_eq!(get_tool_path(ManagedTool::Rg, &env, &probe).await, Ok(None));
    assert_eq!(probe.calls(), ["rg"]);
}

// -----------------------------------------------------------------------------
// 3. Offline gate (tools-manager.ts:14-18, :335-340)
// -----------------------------------------------------------------------------

#[tokio::test]
async fn offline_flag_gives_up_instead_of_downloading() {
    let agent = TempDir::new().expect("tempdir");
    for value in ["1", "true", "TRUE", "True", "yes", "YES", "yEs"] {
        for tool in [ManagedTool::Rg, ManagedTool::Fd] {
            let env = BinaryEnv {
                offline: Some(value.to_string()),
                ..env_with_override(agent.path(), Platform::current())
            };
            let probe = FakeProbe::none();

            assert_eq!(
                ensure_tool_outcome(tool, &env, &probe).await,
                Ok(EnsureOutcome::OfflineSkipped),
                "{OFFLINE_ENV}={value:?} must skip the download"
            );
            // Never throws for a missing binary: it just returns nothing.
            assert_eq!(ensure_tool(tool, &env, &probe).await, Ok(None));
        }
    }
}

#[tokio::test]
async fn offline_flag_does_not_suppress_the_path_probe() {
    // Pi checks `isOfflineModeEnabled()` *after* the whole of `getToolPath`
    // (tools-manager.ts:327 then :335), so offline mode gates only the download.
    // An offline agent still finds a PATH binary, and still pays for the probes.
    // Observed in Pi: PI_OFFLINE=1 with fdfind.exe on PATH still returns "fdfind".

    let agent = TempDir::new().expect("tempdir");
    let env = BinaryEnv {
        offline: Some("1".to_string()),
        ..env_with_override(agent.path(), Platform::current())
    };

    let probe = FakeProbe::with(&["fdfind"]);
    assert_eq!(
        ensure_tool_outcome(ManagedTool::Fd, &env, &probe).await,
        Ok(EnsureOutcome::Found("fdfind".to_string()))
    );
    assert_eq!(probe.calls(), ["fd", "fdfind"]);

    let probe = FakeProbe::none();
    assert_eq!(
        ensure_tool_outcome(ManagedTool::Fd, &env, &probe).await,
        Ok(EnsureOutcome::OfflineSkipped)
    );
    assert_eq!(
        probe.calls(),
        ["fd", "fdfind"],
        "offline mode must not skip the probes"
    );
}

#[tokio::test]
async fn offline_flag_does_not_suppress_the_managed_binary() {
    let agent = TempDir::new().expect("tempdir");
    let platform = Platform::current();
    let expected = install_managed(agent.path(), &managed_file_name(ManagedTool::Rg, platform));
    let env = BinaryEnv {
        offline: Some("yes".to_string()),
        ..env_with_override(agent.path(), platform)
    };
    let probe = FakeProbe::none();

    assert_eq!(
        ensure_tool(ManagedTool::Rg, &env, &probe).await,
        Ok(Some(expected.to_string_lossy().into_owned()))
    );
}

#[tokio::test]
async fn non_truthy_offline_values_leave_the_download_path_open() {
    let agent = TempDir::new().expect("tempdir");
    // Observed in Pi: PI_OFFLINE in {"", 0, no, " 1", 01} all print
    // "ripgrep not found. Downloading..." and proceed to download.
    for value in ["", "0", "no", "false", "off", " 1", "01", "y"] {
        let env = BinaryEnv {
            offline: Some(value.to_string()),
            ..env_with_override(agent.path(), Platform::Linux)
        };
        let probe = FakeProbe::none();
        assert_eq!(
            ensure_tool_outcome(ManagedTool::Rg, &env, &probe).await,
            Ok(EnsureOutcome::DownloadDeferred),
            "{OFFLINE_ENV}={value:?} must not enable offline mode"
        );
    }
}

#[tokio::test]
async fn offline_message_matches_pi() {
    // chalk.yellow(`${config.name} not found. Offline mode enabled, skipping download.`)
    // (tools-manager.ts:337). Copied from Pi's real stdout for PI_OFFLINE in
    // {1, true, TRUE, yes, YES, Yes} with nothing installed.

    assert_eq!(
        EnsureOutcome::OfflineSkipped.log_line(ManagedTool::Rg),
        Some("ripgrep not found. Offline mode enabled, skipping download.".to_string())
    );
    assert_eq!(
        EnsureOutcome::OfflineSkipped.log_line(ManagedTool::Fd),
        Some("fd not found. Offline mode enabled, skipping download.".to_string())
    );
}

// -----------------------------------------------------------------------------
// 4. Android/Termux gate (tools-manager.ts:342-350)
// -----------------------------------------------------------------------------

#[tokio::test]
async fn android_gives_up_and_names_the_termux_package() {
    let agent = TempDir::new().expect("tempdir");
    let env = env_with_override(agent.path(), Platform::Android);

    for (tool, expected) in [
        (
            ManagedTool::Fd,
            "fd not found. Install with: pkg install fd",
        ),
        (
            ManagedTool::Rg,
            "ripgrep not found. Install with: pkg install ripgrep",
        ),
    ] {
        let probe = FakeProbe::none();
        let outcome = ensure_tool_outcome(tool, &env, &probe).await.expect("gate");
        assert_eq!(outcome, EnsureOutcome::TermuxInstall);
        assert_eq!(outcome.log_line(tool).as_deref(), Some(expected));
        assert_eq!(ensure_tool(tool, &env, &probe).await, Ok(None));
    }
}

#[tokio::test]
async fn android_still_uses_a_system_or_managed_binary_if_present() {
    let agent = TempDir::new().expect("tempdir");
    let env = env_with_override(agent.path(), Platform::Android);
    let probe = FakeProbe::with(&["rg"]);

    // `pkg install ripgrep` puts rg on PATH; the gate must not hide it.
    assert_eq!(
        ensure_tool(ManagedTool::Rg, &env, &probe).await,
        Ok(Some("rg".to_string()))
    );
}

#[tokio::test]
async fn offline_gate_is_checked_before_the_android_gate() {
    // tools-manager.ts:335 precedes :344.
    let agent = TempDir::new().expect("tempdir");
    let env = BinaryEnv {
        offline: Some("true".to_string()),
        ..env_with_override(agent.path(), Platform::Android)
    };
    let probe = FakeProbe::none();

    assert_eq!(
        ensure_tool_outcome(ManagedTool::Fd, &env, &probe).await,
        Ok(EnsureOutcome::OfflineSkipped)
    );
}

// -----------------------------------------------------------------------------
// 5. Not found / the deferred downloader
// -----------------------------------------------------------------------------

#[tokio::test]
async fn nothing_available_and_online_reports_the_deferred_download() {
    // feat-005 seam: this must NOT pretend to succeed.
    let agent = TempDir::new().expect("tempdir");
    let env = env_with_override(agent.path(), Platform::Linux);

    for tool in [ManagedTool::Rg, ManagedTool::Fd] {
        let probe = FakeProbe::none();
        let outcome = ensure_tool_outcome(tool, &env, &probe)
            .await
            .expect("resolve");
        assert_eq!(outcome, EnsureOutcome::DownloadDeferred);
        assert_eq!(outcome.path(), None);
        assert_eq!(outcome.log_line(tool), None);
        assert_eq!(ensure_tool(tool, &env, &probe).await, Ok(None));
    }
}

#[tokio::test]
async fn found_outcome_logs_nothing() {
    assert_eq!(
        EnsureOutcome::Found("rg".to_string()).log_line(ManagedTool::Rg),
        None
    );
    assert_eq!(EnsureOutcome::Found("rg".to_string()).path(), Some("rg"));
}

// -----------------------------------------------------------------------------
// The agent-dir override / home-dir seam (config.ts:515-521)
// -----------------------------------------------------------------------------

#[tokio::test]
async fn agent_dir_override_wins_over_the_home_dir() {
    let home = TempDir::new().expect("tempdir");
    let over = TempDir::new().expect("tempdir");
    let platform = Platform::current();
    let file_name = managed_file_name(ManagedTool::Rg, platform);

    // Present under the *home* layout only — must be ignored.
    install_managed(
        &home.path().join(CONFIG_DIR_NAME).join(AGENT_DIR_NAME),
        &file_name,
    );

    let env = BinaryEnv {
        platform,
        home_dir: Some(home.path().to_path_buf()),
        agent_dir_override: Some(over.path().to_string_lossy().into_owned()),
        offline: None,
    };
    let probe = FakeProbe::none();
    assert_eq!(get_tool_path(ManagedTool::Rg, &env, &probe).await, Ok(None));
    assert_eq!(probe.calls(), ["rg"]);

    // Now install under the override → found there.
    let expected = install_managed(over.path(), &file_name);
    assert_eq!(
        get_tool_path(ManagedTool::Rg, &env, &probe).await,
        Ok(Some(expected.to_string_lossy().into_owned()))
    );
}

#[test]
fn an_empty_override_is_falsy_and_falls_back_to_the_home_layout() {
    // `if (envDir)` (config.ts:516) — "" is falsy in JS.
    let home = TempDir::new().expect("tempdir");
    let env = BinaryEnv {
        platform: Platform::Linux,
        home_dir: Some(home.path().to_path_buf()),
        agent_dir_override: Some(String::new()),
        offline: None,
    };
    assert_eq!(
        env.agent_dir().expect("agent dir"),
        home.path().join(".pirust").join("agent")
    );
}

#[test]
fn tilde_overrides_expand_against_the_injected_home() {
    // expandTildePath → normalizePath (config.ts:498-500, utils/paths.ts:66-73).
    let home = TempDir::new().expect("tempdir");
    let base = BinaryEnv {
        platform: Platform::Linux,
        home_dir: Some(home.path().to_path_buf()),
        agent_dir_override: None,
        offline: None,
    };

    let bare_tilde = BinaryEnv {
        agent_dir_override: Some("~".to_string()),
        ..base.clone()
    };
    assert_eq!(bare_tilde.agent_dir().expect("agent dir"), home.path());
    assert_eq!(
        bare_tilde.tools_dir().expect("tools dir"),
        home.path().join("bin")
    );

    let sub = BinaryEnv {
        agent_dir_override: Some("~/custom/agent".to_string()),
        ..base.clone()
    };
    assert_eq!(
        sub.agent_dir().expect("agent dir"),
        home.path().join("custom/agent")
    );

    // Backslash form is win32-only (`process.platform === "win32"` guard).
    let posix_backslash = BinaryEnv {
        agent_dir_override: Some("~\\custom".to_string()),
        ..base.clone()
    };
    assert_eq!(
        posix_backslash.agent_dir().expect("agent dir"),
        PathBuf::from("~\\custom")
    );

    let win_backslash = BinaryEnv {
        platform: Platform::Win32,
        agent_dir_override: Some("~\\custom".to_string()),
        ..base.clone()
    };
    assert_eq!(
        win_backslash.agent_dir().expect("agent dir"),
        home.path().join("custom")
    );

    // A `~` that is not a path prefix is left alone.
    let embedded = BinaryEnv {
        agent_dir_override: Some("/opt/~/x".to_string()),
        ..base
    };
    assert_eq!(
        embedded.agent_dir().expect("agent dir"),
        PathBuf::from("/opt/~/x")
    );
}

#[tokio::test]
async fn an_unresolvable_home_dir_is_an_error_not_a_silent_miss() {
    // Pi throws out of `os.homedir()` here; a missing binary must never be
    // confused with a broken environment.
    let env = BinaryEnv {
        platform: Platform::Win32,
        home_dir: None,
        agent_dir_override: None,
        offline: None,
    };
    let probe = FakeProbe::with(&["rg"]);

    let err = env.tools_dir().expect_err("must fail");
    assert_eq!(err.home_env, HOME_ENV_WINDOWS);
    assert!(
        err.to_string().contains("USERPROFILE"),
        "message names the env var: {err}"
    );
    assert_eq!(
        get_tool_path(ManagedTool::Rg, &env, &probe).await.err(),
        Some(err.clone())
    );
    assert_eq!(
        ensure_tool(ManagedTool::Rg, &env, &probe).await.err(),
        Some(err)
    );
    assert!(probe.calls().is_empty(), "resolution aborts before probing");

    let posix = BinaryEnv {
        platform: Platform::Linux,
        ..env
    };
    assert_eq!(
        posix.tools_dir().expect_err("must fail").home_env,
        HOME_ENV_POSIX
    );

    // With an absolute override the home dir is never consulted, so no error.
    let overridden = BinaryEnv {
        platform: Platform::Linux,
        home_dir: None,
        agent_dir_override: Some("/opt/pirust/agent".to_string()),
        offline: None,
    };
    assert_eq!(
        overridden.tools_dir(),
        Ok(PathBuf::from("/opt/pirust/agent").join("bin"))
    );
}

#[test]
fn from_process_env_reads_the_real_environment_without_mutating_it() {
    // Read-only: safe under parallel tests. Asserts the wiring, not the machine.
    let env = BinaryEnv::from_process_env();
    assert_eq!(env.platform, Platform::current());
    assert_eq!(
        env.home_dir,
        std::env::var_os(home_env_var(env.platform)).map(PathBuf::from)
    );
    assert_eq!(env.agent_dir_override, std::env::var(ENV_AGENT_DIR).ok());
    assert_eq!(env.offline, std::env::var(OFFLINE_ENV).ok());
}

// -----------------------------------------------------------------------------
// The real spawn-based probe (tools-manager.ts:74-82)
// -----------------------------------------------------------------------------

#[tokio::test]
async fn spawn_probe_reports_false_when_the_command_does_not_exist() {
    // Pi's ENOENT branch: `result.error` is set, so `commandExists` is false.
    let probe = SpawnProbe;
    assert!(
        !probe
            .command_exists("pirust-definitely-not-a-real-binary-9c1f")
            .await
    );
}

#[tokio::test]
async fn spawn_probe_reports_true_for_a_command_that_spawns() {
    // `cargo` is guaranteed present while these tests run; CARGO is the absolute
    // path cargo passed to rustc. Skipped if the test is run outside cargo.
    let Some(cargo) = option_env!("CARGO") else {
        return;
    };
    let probe = SpawnProbe;
    assert!(probe.command_exists(cargo).await);
}
