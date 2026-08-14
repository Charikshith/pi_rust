//! Filesystem oracle for [`pirust_coding_agent::migrations`] against real Pi.
//!
//! Fixtures, both captured by executing Pi's own `migrations.ts` on win32:
//!
//! - `tests/fixtures/pi/cli/migrations.cases.jsonl` — **46 records**: 10 × M1, 6 × M2,
//!   8 × M3, 8 × M4, 10 × M5, 3 × `runMigrations` (one of which pins the *order* of effects
//!   through the console stream) and 1 × `showDeprecationWarnings`.
//! - `tests/fixtures/pi/cli/session_dir.cases.jsonl` — the **15**
//!   `migrateSessionsFromAgentRoot` records (the 9 `getDefaultSessionDirPath` ones belong to
//!   the session manager and are counted, not run).
//!
//! Each record carries a `before` tree, an `after` tree (a path-sorted list of relative
//! paths with contents, directories marked by a trailing `/`), the value Pi returned and the
//! `console` lines it wrote. Every expectation below is a literal from those files; the
//! record and per-function counts are asserted so a shrunken fixture fails loudly rather
//! than silently passing.
//!
//! # How a record is replayed
//!
//! The `before` tree is materialized into a fresh [`TempDir`] — `<tmp>/agent` for the agent
//! dir and `<tmp>/project` for the cwd — and [`ConfigEnv`] is pointed at it with identity
//! [`PI`], the branding the capture necessarily carries (so M5's project dir is `.pi`, as in
//! the fixture). Nothing touches the real `~/.pirust` or `~/.pi`, no environment variable is
//! mutated, and `TempDir`'s `Drop` removes the sandbox on unwind as well as on success.
//! Failures are collected and reported together, with a tree diff per case rather than a
//! dump.
//!
//! # Platform caveats (the fixtures' own, not this suite's)
//!
//! - **`platformDependent: "m2-filename-split"`.** On win32 M2's basename expression yields
//!   an absolute path, so the encoded `sessions/<enc>/` directory is created but the rename
//!   fails and the `.jsonl` stays in the agent root (`fileWasMoved: false`). On a POSIX host
//!   the same code — correctly — moves the file, so those `after` trees do not describe
//!   Pi-on-Linux either. Those records are compared on Windows and **skipped with a named
//!   reason elsewhere**, with the skip count asserted.
//! - **`M3:case-sensitivity-of-the-binary-names`** is platform-dependent too, though the
//!   fixture does not mark it: its `after` tree shows `tools/RG.EXE` arriving as
//!   `bin/rg.exe`, which only happens because NTFS answers `existsSync("tools/rg.exe")` with
//!   yes. Same treatment, keyed by name.
//! - **`modeMeaningful: false`** on every record: the captured `mode: "0666"` is a win32
//!   artefact. The mode comparison is therefore skipped on Windows, and on unix the one mode
//!   Pi actually sets is asserted instead — `auth.json` = `0600` (`migrations.ts:69`). **The
//!   fixtures should be re-derived on Linux/macOS** to make the column real.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use pirust_coding_agent::config::{ConfigEnv, Platform, PI};
use pirust_coding_agent::migrations::{
    encode_session_dir_name, migrate_auth_to_auth_json, migrate_extension_system,
    migrate_keybindings_config_file, migrate_sessions_from_agent_root, migrate_tools_to_bin,
    run_migrations, show_deprecation_warnings, CapturingConsole,
};
use serde_json::{json, Value};
use tempfile::TempDir;

/// Every record of `migrations.cases.jsonl`, by `fn`, in fixture order.
const MIGRATIONS_BREAKDOWN: [(&str, usize); 7] = [
    ("migrateAuthToAuthJson", 10),
    ("migrateSessionsFromAgentRoot", 6),
    ("migrateToolsToBin", 8),
    ("migrateKeybindingsConfigFile", 8),
    ("migrateExtensionSystem", 10),
    ("runMigrations", 3),
    ("showDeprecationWarnings", 1),
];

/// `session_dir.cases.jsonl` is shared with the session manager (spec §7.7).
const SESSION_DIR_BREAKDOWN: [(&str, usize); 2] = [
    ("migrateSessionsFromAgentRoot", 15),
    ("getDefaultSessionDirPath", 9),
];

/// The one record whose `after` tree depends on a case-insensitive filesystem rather than on
/// Pi's logic — see the module docs.
const WIN32_FS_CASE_INSENSITIVE: [&str; 1] = ["M3:case-sensitivity-of-the-binary-names"];

/// The fixture's `cwd` placeholder for the sandbox project directory.
const PROJECT_DIR_PLACEHOLDER: &str = "{PROJECTDIR}";

// =============================================================================
// Fixture loading
// =============================================================================

fn cases(file: &str) -> Vec<Value> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/pi/cli")
        .join(file);
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read fixture {}: {e}", path.display()));
    raw.lines()
        .filter(|line| !line.trim().is_empty())
        .enumerate()
        .map(|(index, line)| {
            serde_json::from_str(line)
                .unwrap_or_else(|e| panic!("{file} line {}: parse failed: {e}", index + 1))
        })
        .collect()
}

fn field<'a>(case: &'a Value, key: &str) -> &'a str {
    case[key]
        .as_str()
        .unwrap_or_else(|| panic!("record is missing a string `{key}`: {case}"))
}

fn entries<'a>(case: &'a Value, key: &str) -> &'a [Value] {
    case[key]
        .as_array()
        .unwrap_or_else(|| panic!("record's `{key}` is not an array: {case}"))
}

// =============================================================================
// Trees
// =============================================================================

/// One entry of a fixture tree.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Node {
    /// A directory (fixture path ends with `/`; no `mode`/`content`).
    Dir,
    /// A file and its exact bytes.
    File(String),
}

/// A whole tree, keyed by the fixture's relative path (directories keep their `/`).
type Tree = BTreeMap<String, Node>;

fn expected_tree(entries: &[Value]) -> Tree {
    entries
        .iter()
        .map(|entry| {
            let path = field(entry, "path");
            match path.strip_suffix('/') {
                Some(dir) => (format!("{dir}/"), Node::Dir),
                None => (
                    path.to_string(),
                    Node::File(field(entry, "content").to_string()),
                ),
            }
        })
        .collect()
}

fn actual_tree(root: &Path) -> Tree {
    let mut tree = Tree::new();
    collect(root, "", &mut tree);
    tree
}

fn collect(dir: &Path, prefix: &str, tree: &mut Tree) {
    let Ok(read_dir) = fs::read_dir(dir) else {
        return;
    };
    for entry in read_dir.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        let relative = format!("{prefix}{name}");
        if path.is_dir() {
            tree.insert(format!("{relative}/"), Node::Dir);
            collect(&path, &format!("{relative}/"), tree);
        } else {
            let bytes = fs::read(&path)
                .unwrap_or_else(|e| panic!("read produced file {}: {e}", path.display()));
            tree.insert(
                relative,
                Node::File(String::from_utf8_lossy(&bytes).into_owned()),
            );
        }
    }
}

fn materialize(root: &Path, entries: &[Value]) {
    fs::create_dir_all(root).unwrap_or_else(|e| panic!("create {}: {e}", root.display()));
    for entry in entries {
        let path = field(entry, "path");
        match path.strip_suffix('/') {
            Some(dir) => {
                let target = root.join(dir);
                fs::create_dir_all(&target)
                    .unwrap_or_else(|e| panic!("create {}: {e}", target.display()));
            }
            None => {
                let target = root.join(path);
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent)
                        .unwrap_or_else(|e| panic!("create {}: {e}", parent.display()));
                }
                fs::write(&target, field(entry, "content"))
                    .unwrap_or_else(|e| panic!("write {}: {e}", target.display()));
            }
        }
    }
}

/// `want` vs `got`, one line per difference. Contents are elided past 60 chars so a diff
/// stays readable; the comparison itself is byte-exact.
fn tree_diff(label: &str, want: &Tree, got: &Tree) -> Vec<String> {
    let mut lines = Vec::new();
    for (path, node) in want {
        match got.get(path) {
            None => lines.push(format!("  {label}: missing    {path}  {}", describe(node))),
            Some(actual) if actual != node => lines.push(format!(
                "  {label}: differs    {path}\n      want {}\n      got  {}",
                describe(node),
                describe(actual)
            )),
            Some(_) => {}
        }
    }
    for (path, node) in got {
        if !want.contains_key(path) {
            lines.push(format!("  {label}: unexpected {path}  {}", describe(node)));
        }
    }
    lines
}

fn describe(node: &Node) -> String {
    match node {
        Node::Dir => "<dir>".to_string(),
        Node::File(content) if content.chars().count() > 60 => {
            let head: String = content.chars().take(60).collect();
            format!("{head:?}… ({} bytes)", content.len())
        }
        Node::File(content) => format!("{content:?}"),
    }
}

// =============================================================================
// Modes (`migrations.ts:69`, spec §7.6)
// =============================================================================

/// What this platform can actually verify about the produced file modes.
///
/// `modeMeaningful` is the fixture's own admission that its `mode` column was captured
/// where `chmod` is a no-op. When it is false we do not compare against `0666`; on unix we
/// assert the single mode Pi sets deliberately instead.
#[cfg(unix)]
fn mode_problems(
    mode_meaningful: bool,
    root: &Path,
    after_entries: &[Value],
    before: &Tree,
) -> Vec<String> {
    use std::os::unix::fs::PermissionsExt;

    let mut problems = Vec::new();
    for entry in after_entries {
        let path = field(entry, "path");
        if path.ends_with('/') {
            continue;
        }
        let Ok(metadata) = fs::metadata(root.join(path)) else {
            continue; // absence is already reported by the tree diff
        };
        let got = format!("0{:o}", metadata.permissions().mode() & 0o7777);
        if mode_meaningful {
            let want = field(entry, "mode");
            if want != got {
                problems.push(format!("  mode: {path}: want {want}, got {got}"));
            }
        } else if path == "auth.json" && !before.contains_key("auth.json") {
            // The one mode this port sets: writeFileSync(..., { mode: 0o600 }).
            if got != "0600" {
                problems.push(format!(
                    "  mode: auth.json: expected 0600 (migrations.ts:69), got {got}"
                ));
            }
        }
    }
    problems
}

#[cfg(not(unix))]
fn mode_problems(
    mode_meaningful: bool,
    _root: &Path,
    _after_entries: &[Value],
    _before: &Tree,
) -> Vec<String> {
    if mode_meaningful {
        return vec![
            "  mode: the fixture claims modeMeaningful, but modes are unverifiable here — \
             re-derive the fixture on Linux/macOS"
                .to_string(),
        ];
    }
    Vec::new()
}

// =============================================================================
// The sandbox
// =============================================================================

struct Sandbox {
    /// Held for its `Drop`: removes the tree on success and on unwind.
    _tmp: TempDir,
    agent: PathBuf,
    project: PathBuf,
    env: ConfigEnv,
}

/// Materialize a record's `before` tree. `project_entries` is `None` for the records that
/// have no project scope (`"project": null`), in which case no project dir is created.
fn sandbox(agent_entries: &[Value], project_entries: Option<&[Value]>) -> Sandbox {
    let tmp = TempDir::new().expect("create tempdir");
    let agent = tmp.path().join("agent");
    materialize(&agent, agent_entries);
    let project = tmp.path().join("project");
    if let Some(project_entries) = project_entries {
        materialize(&project, project_entries);
    }
    let env = ConfigEnv {
        identity: PI,
        // The host's platform, because these cases hit the real filesystem: the fixture's
        // win32 `after` trees are only reproducible on win32 (see the module docs).
        platform: Platform::current(),
        home_dir: Some(path_str(tmp.path())),
        // `getAgentDir()` reads this env var; passing it as a value is what keeps the suite
        // out of the real `~/.pirust` without mutating the process environment.
        agent_dir_override: Some(path_str(&agent)),
    };
    Sandbox {
        _tmp: tmp,
        agent,
        project,
        env,
    }
}

fn path_str(path: &Path) -> String {
    path.to_str()
        .unwrap_or_else(|| panic!("non-UTF-8 temp path {}", path.display()))
        .to_string()
}

// =============================================================================
// migrations.cases.jsonl
// =============================================================================

#[test]
fn every_migrations_case_matches_pi() {
    let cases = cases("migrations.cases.jsonl");
    assert_eq!(
        cases.len(),
        46,
        "the fixture must carry all 46 captured records"
    );
    assert_breakdown(&cases, &MIGRATIONS_BREAKDOWN);
    for case in &cases {
        assert_eq!(
            field(case, "platform"),
            "win32",
            "{}: the whole suite compares against a win32 capture",
            field(case, "name")
        );
    }

    let mut failures: Vec<String> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    let mut compared = 0usize;

    for case in &cases {
        let name = field(case, "name");
        if let Some(reason) = skip_reason(case) {
            skipped.push(format!("{name} ({reason})"));
            continue;
        }
        let problems = match field(case, "fn") {
            "showDeprecationWarnings" => run_show_deprecation_warnings(case),
            _ => run_filesystem_case(case),
        };
        if problems.is_empty() {
            compared += 1;
        } else {
            failures.push(format!("{name}\n{}", problems.join("\n")));
        }
    }

    assert!(
        failures.is_empty(),
        "{} of {} records diverge from Pi:\n\n{}\n",
        failures.len(),
        cases.len(),
        failures.join("\n\n")
    );
    assert_eq!(
        compared + skipped.len(),
        cases.len(),
        "every record must be either compared or skipped for a named reason"
    );
    assert_platform_skips(&skipped, 6);
}

/// `session_dir.cases.jsonl`'s `migrateSessionsFromAgentRoot` half — the cwd→dir encoding.
#[test]
fn every_session_dir_migration_case_matches_pi() {
    let cases = cases("session_dir.cases.jsonl");
    assert_eq!(cases.len(), 24, "the fixture must carry all 24 records");
    assert_breakdown(&cases, &SESSION_DIR_BREAKDOWN);

    let mine: Vec<&Value> = cases
        .iter()
        .filter(|case| field(case, "fn") == "migrateSessionsFromAgentRoot")
        .collect();

    let mut failures: Vec<String> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    let mut compared = 0usize;

    for case in mine {
        let name = field(case, "name");
        let mut problems = Vec::new();

        // The encoding itself, pinned independently of the filesystem: `encodedDirName` is
        // present (as a string) whenever Pi reached the encoding step.
        if let (Some(cwd), Some(want)) = (case["cwd"].as_str(), case["encodedDirName"].as_str()) {
            let got = encode_session_dir_name(cwd);
            if got != want {
                problems.push(format!("  encode({cwd:?}): want {want:?}, got {got:?}"));
            }
        }

        if let Some(reason) = skip_reason(case) {
            skipped.push(format!("{name} ({reason})"));
            // The encoding check above is platform-independent, so a failure still counts.
            if !problems.is_empty() {
                failures.push(format!("{name}\n{}", problems.join("\n")));
            }
            continue;
        }

        let before = entries(case, "before");
        let sandbox = sandbox(before, None);
        let outcome = migrate_sessions_from_agent_root(&sandbox.env);
        if let Err(error) = outcome {
            problems.push(format!(
                "  migrate_sessions_from_agent_root failed: {error}"
            ));
        }
        let want = expected_tree(entries(case, "after"));
        problems.extend(tree_diff("agent", &want, &actual_tree(&sandbox.agent)));
        problems.extend(mode_problems(
            case["modeMeaningful"].as_bool().unwrap_or(false),
            &sandbox.agent,
            entries(case, "after"),
            &expected_tree(before),
        ));
        problems.extend(console_diff(case, &CapturingConsole::default()));

        if problems.is_empty() {
            compared += 1;
        } else {
            failures.push(format!("{name}\n{}", problems.join("\n")));
        }
    }

    assert!(
        failures.is_empty(),
        "{} records diverge from Pi:\n\n{}\n",
        failures.len(),
        failures.join("\n\n")
    );
    assert_eq!(
        compared + skipped.len(),
        15,
        "all 15 migrateSessionsFromAgentRoot records must be accounted for"
    );
    assert_platform_skips(&skipped, 15);
}

/// The `getDefaultSessionDirPath` half is another wave's (spec §7.7); this suite only
/// proves it is still there, and that the two halves genuinely differ — resolution happens
/// in `session-manager.ts:473`, *before* the shared encoding, which is why
/// [`encode_session_dir_name`] takes an already-resolved path.
#[test]
fn the_session_manager_half_of_the_fixture_is_left_alone() {
    let cases = cases("session_dir.cases.jsonl");
    let theirs: Vec<&Value> = cases
        .iter()
        .filter(|case| field(case, "fn") == "getDefaultSessionDirPath")
        .collect();
    assert_eq!(theirs.len(), 9, "9 getDefaultSessionDirPath records");

    for case in theirs {
        // Their `result` is win32-resolved first (`/home/user/project` →
        // `C:\home\user\project`), so the encoding alone cannot produce it…
        let cwd = field(case, "cwd");
        let result = field(case, "result");
        let unresolved = encode_session_dir_name(cwd);
        // …but it does produce the tail once the caller has resolved: every `result` ends in
        // the encoding of *some* absolute path, and only the posix-rooted inputs differ from
        // the raw encoding.
        assert!(
            result.contains("\\sessions\\--"),
            "{}: unexpected shape {result:?}",
            field(case, "name")
        );
        if cwd.starts_with("C:") {
            assert!(
                result.ends_with(&unresolved),
                "{}: a drive-lettered cwd needs no resolution, so the tail must be {unresolved:?}: {result:?}",
                field(case, "name")
            );
        }
    }
}

// =============================================================================
// Case runners
// =============================================================================

/// Everything except `showDeprecationWarnings`: materialize, run, compare trees + return
/// value + console.
fn run_filesystem_case(case: &Value) -> Vec<String> {
    let function = field(case, "fn");
    let before_agent = entries(&case["before"], "agent");
    let before_project = case["before"]["project"].as_array().map(Vec::as_slice);
    let sandbox = sandbox(before_agent, before_project);
    let mut console = CapturingConsole::default();
    let mut problems = Vec::new();

    let cwd = case["cwd"].as_str().map(|cwd| {
        assert_eq!(
            cwd,
            PROJECT_DIR_PLACEHOLDER,
            "{}: unknown cwd placeholder {cwd:?}",
            field(case, "name")
        );
        path_str(&sandbox.project)
    });

    let returned: Value = match function {
        "migrateAuthToAuthJson" => match migrate_auth_to_auth_json(&sandbox.env) {
            Ok(providers) => json!(providers),
            Err(error) => {
                problems.push(format!("  migrate_auth_to_auth_json failed: {error}"));
                Value::Null
            }
        },
        "migrateSessionsFromAgentRoot" => {
            if let Err(error) = migrate_sessions_from_agent_root(&sandbox.env) {
                problems.push(format!(
                    "  migrate_sessions_from_agent_root failed: {error}"
                ));
            }
            Value::Null
        }
        "migrateToolsToBin" => {
            if let Err(error) = migrate_tools_to_bin(&sandbox.env, &mut console) {
                problems.push(format!("  migrate_tools_to_bin failed: {error}"));
            }
            Value::Null
        }
        "migrateKeybindingsConfigFile" => {
            if let Err(error) = migrate_keybindings_config_file(&sandbox.env) {
                problems.push(format!("  migrate_keybindings_config_file failed: {error}"));
            }
            Value::Null
        }
        "migrateExtensionSystem" => {
            let cwd = cwd.as_deref().expect("migrateExtensionSystem needs a cwd");
            match migrate_extension_system(&sandbox.env, cwd, &mut console) {
                Ok(warnings) => json!(warnings),
                Err(error) => {
                    problems.push(format!("  migrate_extension_system failed: {error}"));
                    Value::Null
                }
            }
        }
        "runMigrations" => {
            let cwd = cwd.as_deref().expect("runMigrations needs a cwd");
            match run_migrations(&sandbox.env, cwd, &mut console) {
                Ok(results) => json!({
                    "migratedAuthProviders": results.migrated_auth_providers,
                    "deprecationWarnings": results.deprecation_warnings,
                }),
                Err(error) => {
                    problems.push(format!("  run_migrations failed: {error}"));
                    Value::Null
                }
            }
        }
        other => panic!("unknown fixture fn {other:?}"),
    };

    if returned != case["returned"] {
        problems.push(format!(
            "  returned: want {}, got {}",
            case["returned"], returned
        ));
    }

    let want_agent = expected_tree(entries(&case["after"], "agent"));
    problems.extend(tree_diff(
        "agent",
        &want_agent,
        &actual_tree(&sandbox.agent),
    ));
    if let Some(after_project) = case["after"]["project"].as_array() {
        let want_project = expected_tree(after_project);
        problems.extend(tree_diff(
            "project",
            &want_project,
            &actual_tree(&sandbox.project),
        ));
    }

    problems.extend(mode_problems(
        case["modeMeaningful"].as_bool().unwrap_or(false),
        &sandbox.agent,
        entries(&case["after"], "agent"),
        &expected_tree(before_agent),
    ));
    problems.extend(console_diff(case, &console));
    problems
}

/// The child-process capture: `input` warnings in, `stdout` out (`stderr` empty).
fn run_show_deprecation_warnings(case: &Value) -> Vec<String> {
    let warnings: Vec<String> = entries(case, "input")
        .iter()
        .map(|warning| {
            warning
                .as_str()
                .expect("input warnings are strings")
                .to_string()
        })
        .collect();
    let mut console = CapturingConsole::default();
    show_deprecation_warnings(&mut console, &warnings);

    let mut problems = Vec::new();
    let want = field(case, "stdout");
    let got = console.stdout();
    if got != want {
        problems.push(format!("  stdout: want {want:?}, got {got:?}"));
    }
    assert_eq!(
        field(case, "stderr"),
        "",
        "the capture wrote nothing to stderr"
    );
    problems
}

/// The fixture's `console` array vs. what the sink recorded. Pi writes every line with
/// `console.log`, i.e. to stdout.
fn console_diff(case: &Value, console: &CapturingConsole) -> Vec<String> {
    let want: Vec<&str> = entries(case, "console")
        .iter()
        .map(|line| {
            assert_eq!(
                field(line, "stream"),
                "stdout",
                "{}: migrations only use console.log",
                field(case, "name")
            );
            field(line, "text")
        })
        .collect();
    let got = console.texts();
    if got == want {
        return Vec::new();
    }
    vec![format!("  console: want {want:?}, got {got:?}")]
}

// =============================================================================
// Bookkeeping
// =============================================================================

fn assert_breakdown(cases: &[Value], breakdown: &[(&str, usize)]) {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for case in cases {
        *counts.entry(field(case, "fn")).or_default() += 1;
    }
    for (function, want) in breakdown {
        assert_eq!(
            counts.get(function).copied().unwrap_or(0),
            *want,
            "fixture should carry {want} {function} records, found {:?}",
            counts.get(function)
        );
    }
    assert_eq!(
        counts.len(),
        breakdown.len(),
        "fixture gained or lost a function: {:?}",
        counts.keys().collect::<Vec<_>>()
    );
}

/// Why a record cannot be replayed on this host — `None` means "compare it".
fn skip_reason(case: &Value) -> Option<&'static str> {
    if cfg!(windows) {
        return None;
    }
    let name = field(case, "name");
    if case["platformDependent"].as_str() == Some("m2-filename-split") {
        return Some("fixture marks it platformDependent: m2-filename-split");
    }
    if WIN32_FS_CASE_INSENSITIVE.contains(&name) {
        return Some("after tree depends on a case-insensitive filesystem");
    }
    None
}

/// A skip must never be silent: on win32 nothing may be skipped, elsewhere exactly the
/// documented number, so neither a shrunken fixture nor a lost marker can hide a case.
fn assert_platform_skips(skipped: &[String], expected_off_windows: usize) {
    if cfg!(windows) {
        assert!(
            skipped.is_empty(),
            "on win32 every record is comparable, but skipped: {skipped:?}"
        );
    } else {
        assert_eq!(
            skipped.len(),
            expected_off_windows,
            "expected exactly {expected_off_windows} platform-gated records, got {skipped:?}"
        );
        eprintln!(
            "note: {} win32-only records skipped on this platform: {}",
            skipped.len(),
            skipped.join(", ")
        );
    }
}
