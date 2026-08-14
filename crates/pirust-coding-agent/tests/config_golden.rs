//! Path oracle for [`pirust_coding_agent::config`] against real Pi.
//!
//! Fixture: `tests/fixtures/pi/cli/config_paths.json`, captured by executing Pi's own
//! `config.ts` accessors on win32 — 7 `getAgentDir()` resolution branches × 10 path
//! accessors, plus 18 `expandTildePath` cases. Every expectation below is a literal from
//! that file; nothing is re-derived here, and a shrunken fixture fails the count
//! assertions rather than silently passing.
//!
//! # Why the test substitutes Pi's identity
//!
//! The fixture necessarily carries the identity Pi ran with — `.pi`,
//! `PI_CODING_AGENT_DIR`, `pi-debug.log` — while production pirust uses `.pirust`,
//! `PIRUST_CODING_AGENT_DIR`, `pirust-debug.log`. That branding difference is an
//! intentional divergence (`config.rs`'s module docs), so comparing pirust's *default*
//! output against the fixture would fail for a reason that is not a defect.
//!
//! Rather than fork the logic or hand-edit the fixture, identity is a **parameter**:
//! [`AppIdentity`] is a field of [`ConfigEnv`], the production path is
//! `ConfigEnv::from_process_env()` (identity [`PIRUST`]) and this test builds the same
//! struct with identity [`PI`]. One code path, two brandings — exactly the approach the
//! `--help` golden test takes with `render_help(&PI, …)`. `config.rs`'s unit tests pin the
//! `PIRUST` side, so a swap of the two constants cannot go unnoticed.
//!
//! # Why no `std::env::set_var`
//!
//! Pi reads `process.env[ENV_AGENT_DIR]`, `os.homedir()` and `os.platform()` on every
//! `getAgentDir()` call. All three are fields of [`ConfigEnv`], so each fixture branch is
//! reproduced by constructing a value — no mutation of the process environment (which is
//! global and would race with the other tests in this binary), and no dependency on the
//! host's real home directory or platform. The fixture's `{HOME}` / `{TMPROOT}`
//! placeholders become arbitrary literals below; every accessor is pure string
//! composition, so nothing needs to exist on disk.

use std::collections::BTreeSet;
use std::path::PathBuf;

use pirust_coding_agent::config::{AppIdentity, ConfigEnv, Platform, PI, PIRUST};
use serde_json::Value;

/// Stand-in for the capture machine's `os.homedir()` (`{HOME}` in the fixture).
const HOME: &str = "C:\\Users\\pi-oracle";
/// Stand-in for the capture machine's temp root (`{TMPROOT}` in the fixture).
const TMPROOT: &str = "C:\\Temp\\pi-oracle";
/// Stand-in for a captured temp agent dir, should the fixture ever use it.
const AGENTDIR: &str = "C:\\Temp\\pi-oracle\\agentdir";

/// The `agentDirBranches` keys, in fixture order.
const BRANCHES: [&str; 7] = [
    "env-set-absolute",
    "env-set-tilde-only",
    "env-set-tilde-slash",
    "env-set-tilde-backslash",
    "env-set-relative",
    "env-set-empty-string",
    "unset",
];

/// The `paths` keys of every branch, mapped onto the ported accessors. The names are the
/// fixture's (i.e. Pi's function names), so the mapping is unambiguous.
type Accessor = fn(&ConfigEnv) -> Result<String, pirust_coding_agent::config::ConfigPathError>;
const ACCESSORS: [(&str, Accessor); 10] = [
    ("getAgentDir", ConfigEnv::agent_dir),
    ("getCustomThemesDir", ConfigEnv::custom_themes_dir),
    ("getModelsPath", ConfigEnv::models_path),
    ("getAuthPath", ConfigEnv::auth_path),
    ("getSettingsPath", ConfigEnv::settings_path),
    ("getToolsDir", ConfigEnv::tools_dir),
    ("getBinDir", ConfigEnv::bin_dir),
    ("getPromptsDir", ConfigEnv::prompts_dir),
    ("getSessionsDir", ConfigEnv::sessions_dir),
    ("getDebugLogPath", ConfigEnv::debug_log_path),
];

fn fixture() -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/pi/cli/config_paths.json");
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read fixture {}: {e}", path.display()));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("parse fixture {}: {e}", path.display()))
}

/// Resolve the fixture's placeholders. Asserts none is left over, so a placeholder this
/// test does not know about cannot be compared as a literal and quietly "pass".
fn resolve(template: &str, case: &str) -> String {
    let out = template
        .replace("{HOME}", HOME)
        .replace("{TMPROOT}", TMPROOT)
        .replace("{AGENTDIR}", AGENTDIR);
    assert!(
        !out.contains('{'),
        "{case}: unresolved placeholder in fixture value {template:?}"
    );
    out
}

/// The fixture's `platform`, as a [`Platform`]. Pinning it (rather than using the host's)
/// is what makes the win32 expectations reproducible everywhere.
fn platform(fixture: &Value) -> Platform {
    match fixture["platform"].as_str().expect("platform") {
        "win32" => Platform::Win32,
        "darwin" => Platform::Darwin,
        "linux" => Platform::Linux,
        "android" => Platform::Android,
        other => panic!("unexpected fixture platform {other:?}"),
    }
}

#[test]
fn fixture_identity_is_pi_s_and_matches_the_pi_constant() {
    let fixture = fixture();
    // The capture is win32 with `\` separators; every expectation below assumes it.
    assert_eq!(fixture["platform"], "win32");
    assert_eq!(fixture["sep"], "\\");

    let identity = &fixture["identity"];
    assert_eq!(identity["APP_NAME"], PI.app_name);
    assert_eq!(identity["CONFIG_DIR_NAME"], PI.config_dir_name);
    assert_eq!(identity["ENV_AGENT_DIR"], PI.env_agent_dir);
    assert_eq!(identity["ENV_SESSION_DIR"], PI.env_session_dir);

    // …and pirust's own identity is the documented divergence, not an accident.
    assert_eq!(PIRUST.app_name, "pirust");
    assert_eq!(PIRUST.config_dir_name, ".pirust");
    assert_eq!(PIRUST.env_agent_dir, "PIRUST_CODING_AGENT_DIR");
    assert_eq!(PIRUST.env_session_dir, "PIRUST_CODING_AGENT_SESSION_DIR");
    assert_ne!(PIRUST, PI);
}

#[test]
fn every_agent_dir_branch_matches_pi() {
    let fixture = fixture();
    let platform = platform(&fixture);
    let branches = fixture["agentDirBranches"]
        .as_object()
        .expect("agentDirBranches");

    assert_eq!(
        branches.len(),
        BRANCHES.len(),
        "fixture should carry all {} resolution branches, found {:?}",
        BRANCHES.len(),
        branches.keys().collect::<Vec<_>>()
    );
    assert_eq!(
        branches.keys().map(String::as_str).collect::<BTreeSet<_>>(),
        BRANCHES.into_iter().collect::<BTreeSet<_>>(),
        "fixture branch names changed"
    );

    let mut compared = 0usize;
    for name in BRANCHES {
        let branch = &branches[name];

        // `envValue: null` is an unset variable; a string (possibly empty) is a set one.
        let env_value = match &branch["envValue"] {
            Value::Null => None,
            Value::String(raw) => Some(resolve(raw, name)),
            other => panic!("{name}: unexpected envValue {other:?}"),
        };
        let env = ConfigEnv {
            identity: PI,
            platform,
            home_dir: Some(HOME.to_string()),
            agent_dir_override: env_value,
        };

        let paths = branch["paths"].as_object().expect("paths");
        assert_eq!(
            paths.keys().map(String::as_str).collect::<BTreeSet<_>>(),
            ACCESSORS
                .iter()
                .map(|(key, _)| *key)
                .collect::<BTreeSet<_>>(),
            "{name}: fixture accessor set changed"
        );

        for (key, accessor) in ACCESSORS {
            let case = format!("{name}/{key}");
            let want = resolve(paths[key].as_str().expect("path string"), &case);
            let got = accessor(&env)
                .unwrap_or_else(|e| panic!("{case}: expected {want:?}, accessor failed: {e}"));
            assert_eq!(got, want, "{case}: expected {want:?}, got {got:?}");
            compared += 1;
        }
    }
    assert_eq!(compared, BRANCHES.len() * ACCESSORS.len(), "88 comparisons");
}

#[test]
fn every_expand_tilde_path_case_matches_pi() {
    let fixture = fixture();
    let platform = platform(&fixture);
    let cases = fixture["expandTildePath"]
        .as_array()
        .expect("expandTildePath");
    assert_eq!(
        cases.len(),
        18,
        "fixture should carry all 18 expandTildePath cases"
    );

    // `expandTildePath` reads only `os.homedir()` and `process.platform`; the agent-dir
    // override is irrelevant to it.
    let env = ConfigEnv {
        identity: PI,
        platform,
        home_dir: Some(HOME.to_string()),
        agent_dir_override: None,
    };

    let mut with_result = 0usize;
    let mut with_error = 0usize;
    for (index, case) in cases.iter().enumerate() {
        let input = case["input"].as_str().expect("input");
        let label = format!("expandTildePath[{index}] {input:?}");
        let got = env.expand_tilde_path(input);

        match (case.get("result"), case.get("error")) {
            (Some(result), None) => {
                let want = resolve(result.as_str().expect("result"), &label);
                let got =
                    got.unwrap_or_else(|e| panic!("{label}: expected {want:?}, but failed: {e}"));
                assert_eq!(got, want, "{label}: expected {want:?}, got {got:?}");
                with_result += 1;
            }
            (None, Some(error)) => {
                // Pi's capture records the thrown value including its JS class, e.g.
                // `TypeError: File URL path must be absolute`. `PathError`'s `Display` is
                // Node's `err.message`, so the class name is prepended for comparison.
                let want = error.as_str().expect("error");
                let err = got.expect_err(&format!("{label}: expected the throw {want:?}"));
                let (class, message) = want
                    .split_once(": ")
                    .unwrap_or_else(|| panic!("{label}: malformed error {want:?}"));
                assert_eq!(class, "TypeError", "{label}: unexpected error class");
                assert_eq!(
                    err.to_string(),
                    message,
                    "{label}: expected {message:?}, got {err}"
                );
                with_error += 1;
            }
            _ => panic!("{label}: case must have exactly one of `result` / `error`"),
        }
    }
    assert_eq!(with_result, 17, "17 of the 18 cases return a value");
    assert_eq!(with_error, 1, "1 of the 18 cases throws");
}

#[test]
fn the_pirust_side_of_the_same_code_path_is_the_documented_divergence() {
    // Same struct, same accessors, only `identity` swapped: proves the golden test above
    // exercises production's code path and not a test-only variant.
    let pi = ConfigEnv {
        identity: PI,
        platform: Platform::Win32,
        home_dir: Some(HOME.to_string()),
        agent_dir_override: None,
    };
    let pirust = ConfigEnv {
        identity: PIRUST,
        ..pi.clone()
    };
    assert_eq!(pi.agent_dir().unwrap(), format!("{HOME}\\.pi\\agent"));
    assert_eq!(
        pirust.agent_dir().unwrap(),
        format!("{HOME}\\.pirust\\agent")
    );
    assert_eq!(
        pi.debug_log_path().unwrap(),
        format!("{HOME}\\.pi\\agent\\pi-debug.log")
    );
    assert_eq!(
        pirust.debug_log_path().unwrap(),
        format!("{HOME}\\.pirust\\agent\\pirust-debug.log")
    );

    // The identity struct `args::render_help` is parameterized by (spec §4.3).
    let ids: [AppIdentity; 2] = [PI, PIRUST];
    for id in ids {
        assert!(!id.app_name.is_empty());
        assert!(id.config_dir_name.starts_with('.'));
        assert!(id.env_agent_dir.ends_with("_CODING_AGENT_DIR"));
        assert!(id.env_session_dir.ends_with("_CODING_AGENT_SESSION_DIR"));
        assert!(!id.version.is_empty());
    }
}
