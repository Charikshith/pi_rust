//! Session oracle for [`pirust_coding_agent::session`] against real Pi.
//!
//! Two halves, matching the two halves of the module:
//!
//! 1. **`getDefaultSessionDirPath`** — the 9 records of
//!    `tests/fixtures/pi/cli/session_dir.cases.jsonl` whose `fn` is
//!    `getDefaultSessionDirPath`, replayed with exact string comparison. The same file's
//!    other 15 records belong to `migrations.rs`' `migrateSessionsFromAgentRoot` and are
//!    asserted to be *present and still not ours*, so a reshuffled or shrunken fixture fails
//!    loudly instead of silently reducing coverage.
//! 2. **lifecycle + resolution** — `tempfile` directories only. Nothing here reads or writes
//!    the real `~/.pirust`, `~/.pi`, or `$PIRUST_CODING_AGENT_*`: [`SessionEnv`] carries the
//!    agent dir, the platform, the clock and the id source as **values**, so no test mutates
//!    the process environment (which is global and would race under `cargo test`).
//!
//! # Why the platform comes from the fixture
//!
//! The capture is win32 (`"platform": "win32"` on every record) and its expectations contain
//! `\` separators and drive letters. [`Platform`] is a field of [`ConfigEnv`], so the replay
//! pins the fixture's platform rather than the host's and the assertions hold on Linux and
//! macOS too — the same approach `config_golden.rs` takes.
//!
//! # The one thing the capture does not record: `process.cwd()`
//!
//! `getDefaultSessionDirPath` calls `resolvePath(cwd)` **before** encoding
//! (`session-manager.ts:473-475`), and on win32 `path.resolve("/home/user/project")` takes
//! its **drive letter from `process.cwd()`**. `scripts/gen-cli-oracle.mjs:894-911` notes it
//! only fed absolute cwds "because resolvePath() would resolve a relative value against
//! process.cwd(), which is not reproducible" — but a *rooted, driveless* posix path is
//! `path.win32.isAbsolute`, so those records are drive-dependent all the same. The drive is
//! recoverable from the records themselves (`/` → `C:\` → `--C----`), and
//! [`assert_capture_drive`] checks that recovery before any comparison, so `PROCESS_CWD`
//! below is pinned by the fixture rather than guessed. If the oracle is ever re-run from
//! another drive, that assertion is what will say so.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use pirust_agent_core::harness::session::FixedClock;
use pirust_coding_agent::config::{ConfigEnv, Platform, PI};
use pirust_coding_agent::migrations::encode_session_dir_name;
use pirust_coding_agent::session::{
    assert_valid_session_id, build_context_entries, create_session_manager, generate_id, is_header,
    parse_session_entries, resolve_session_path, validate_fork_flags, validate_session_id_flags,
    CapturingConsole, HeadlessPrompts, LeafId, NewSessionOptions, PickerUnavailable,
    ResolvedSession, SessionEnv, SessionError, SessionExit, SessionIdSource, SessionIo,
    SessionLoaders, SessionPrompts, SessionStream, SessionStyle,
};
use serde_json::Value;

/// A `process.cwd()` on the drive the capture ran from — see the module docs.
const PROCESS_CWD: &str = "C:\\oracle";

/// The 9 `getDefaultSessionDirPath` record names, in fixture order.
const DEFAULT_SESSION_DIR_CASES: [&str; 9] = [
    "posix-path",
    "windows-path-with-drive-letter",
    "windows-path-forward-slashes",
    "path-with-spaces",
    "path-with-non-ascii",
    "unc-path",
    "filesystem-root-posix",
    "filesystem-root-windows-drive",
    "trailing-slash",
];

fn fixture() -> Vec<Value> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/pi/cli/session_dir.cases.jsonl");
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read fixture {}: {e}", path.display()));
    raw.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str(line)
                .unwrap_or_else(|e| panic!("parse fixture {}: {e}", path.display()))
        })
        .collect()
}

fn records_for(fixture: &[Value], name: &str) -> Vec<Value> {
    fixture
        .iter()
        .filter(|record| record["fn"] == name)
        .cloned()
        .collect()
}

/// The fixture's `platform`, as a [`Platform`]. Pinning it is what makes the win32
/// expectations reproducible on every host.
fn platform(record: &Value) -> Platform {
    match record["platform"].as_str().expect("platform") {
        "win32" => Platform::Win32,
        "darwin" => Platform::Darwin,
        "linux" => Platform::Linux,
        "android" => Platform::Android,
        other => panic!("unexpected fixture platform {other:?}"),
    }
}

/// Recover the drive `process.cwd()` was on during the capture, and check [`PROCESS_CWD`]
/// agrees. Non-circular: it reads the `filesystem-root-posix` record, whose cwd is `/`, so
/// its whole answer past `sessions\` *is* `--<drive>----`.
fn assert_capture_drive(fixture: &[Value]) {
    let root = fixture
        .iter()
        .find(|record| {
            record["fn"] == "getDefaultSessionDirPath" && record["name"] == "filesystem-root-posix"
        })
        .expect("the filesystem-root-posix record pins the capture's drive");
    let result = root["result"].as_str().expect("result");
    let encoded = result
        .rsplit_once('\\')
        .expect("a win32 result has separators")
        .1;
    // `/` -> resolvePath -> `<drive>:\` -> encode -> `--<drive>----`
    let drive = encoded
        .strip_prefix("--")
        .and_then(|rest| rest.strip_suffix("----"))
        .unwrap_or_else(|| panic!("unexpected encoded root {encoded:?}"));
    assert_eq!(
        drive.len(),
        1,
        "expected a single-letter drive, got {drive:?}"
    );
    assert!(
        PROCESS_CWD.starts_with(&format!("{drive}:")),
        "the capture ran on drive {drive}:, but PROCESS_CWD is {PROCESS_CWD:?} — re-derive the \
         fixture or fix PROCESS_CWD"
    );
    // And the agent dir is on that same drive, as gen-cli-oracle.mjs:899 hard-codes.
    assert!(root["agentDir"]
        .as_str()
        .expect("agentDir")
        .starts_with(&format!("{drive}:")));
}

/// A [`SessionEnv`] reproducing the capture's ambience: Pi's identity, the fixture's
/// platform, and a `process.cwd()` on the captured drive.
fn capture_env(fixture_platform: Platform) -> SessionEnv {
    SessionEnv::new(
        ConfigEnv {
            identity: PI,
            platform: fixture_platform,
            home_dir: Some("C:\\Users\\pi-oracle".to_string()),
            // Every record passes `agentDir` explicitly, so this is never consulted; a
            // value is supplied anyway so a stray `getAgentDir()` cannot reach a real home.
            agent_dir_override: Some("C:\\oracle\\agent".to_string()),
        },
        PROCESS_CWD,
    )
}

#[test]
fn fixture_accounting_pins_both_halves() {
    let fixture = fixture();
    assert_eq!(
        fixture.len(),
        24,
        "session_dir.cases.jsonl should carry all 24 records"
    );

    let mine = records_for(&fixture, "getDefaultSessionDirPath");
    let migrations = records_for(&fixture, "migrateSessionsFromAgentRoot");
    assert_eq!(mine.len(), 9, "9 records belong to session.rs");
    assert_eq!(migrations.len(), 15, "15 records belong to migrations.rs");
    assert_eq!(
        mine.len() + migrations.len(),
        fixture.len(),
        "the fixture gained a third `fn`: {:?}",
        fixture
            .iter()
            .map(|record| record["fn"].as_str().unwrap_or("?"))
            .collect::<Vec<_>>()
    );

    // Mine are pure: a `result`, no filesystem tree.
    let names: Vec<&str> = mine
        .iter()
        .map(|record| record["name"].as_str().expect("name"))
        .collect();
    assert_eq!(
        names, DEFAULT_SESSION_DIR_CASES,
        "record order/names changed"
    );
    for record in &mine {
        assert!(record["result"].is_string(), "{record:?} lost its result");
        assert!(
            record.get("before").is_none(),
            "{record:?} is migration-shaped"
        );
        assert!(
            record.get("after").is_none(),
            "{record:?} is migration-shaped"
        );
    }

    // The migration half is still migration-shaped, and still NOT ours.
    for record in &migrations {
        assert!(record.get("result").is_none(), "{record:?} is now pure");
        assert!(
            record["before"].is_array(),
            "{record:?} lost its `before` tree"
        );
        assert!(
            record["after"].is_array(),
            "{record:?} lost its `after` tree"
        );
        assert_eq!(record["platformDependent"], "m2-filename-split");
        // migrations.rs' contract: the RAW header cwd is encoded, with no resolution.
        if let (Some(cwd), Some(encoded)) = (
            record["cwd"].as_str().filter(|cwd| !cwd.is_empty()),
            record["encodedDirName"].as_str(),
        ) {
            assert_eq!(
                encode_session_dir_name(cwd),
                encoded,
                "{}: the M2 encoding is the unresolved one",
                record["name"]
            );
        }
    }
}

#[test]
fn every_default_session_dir_path_record_matches_pi() {
    let fixture = fixture();
    assert_capture_drive(&fixture);
    let mine = records_for(&fixture, "getDefaultSessionDirPath");
    let env = capture_env(platform(&mine[0]));

    let mut compared = 0usize;
    for record in &mine {
        let name = record["name"].as_str().expect("name");
        assert_eq!(
            platform(record),
            env.config.platform,
            "{name}: mixed platforms in one fixture"
        );
        let cwd = record["cwd"].as_str().expect("cwd");
        let agent_dir = record["agentDir"].as_str().expect("agentDir");
        let want = record["result"].as_str().expect("result");

        let got = env
            .default_session_dir_path(cwd, Some(agent_dir))
            .unwrap_or_else(|e| panic!("{name}: expected {want:?}, but failed: {e}"));
        assert_eq!(got, want, "{name}: cwd {cwd:?}");
        compared += 1;
    }
    assert_eq!(compared, 9, "all 9 records compared");
}

#[test]
fn the_encoding_is_applied_after_resolution_not_before() {
    // Mutation guard (a): encoding the RAW cwd — migrations.rs' contract — is a DIFFERENT
    // string for six of the nine records, and the fixture's two halves prove it: the same
    // input `/home/user/project` is `--home-user-project--` for M2 and
    // `--C--home-user-project--` here.
    let fixture = fixture();
    let mine = records_for(&fixture, "getDefaultSessionDirPath");
    let env = capture_env(platform(&mine[0]));

    let mut differ: Vec<&str> = Vec::new();
    for record in &mine {
        let cwd = record["cwd"].as_str().expect("cwd");
        let want = record["result"].as_str().expect("result");
        let unresolved = encode_session_dir_name(cwd);
        let resolved = want
            .rsplit_once('\\')
            .expect("a win32 result has separators")
            .1;
        if unresolved != resolved {
            differ.push(record["name"].as_str().expect("name"));
        }
    }
    // The five whose cwd is rooted-but-driveless (so `resolve` prepends a drive) or has a
    // trailing separator (which `resolve` drops). The other four already carry a drive, or
    // are UNC, and encode identically either way — which is why the count matters: a port
    // that skipped `resolvePath` would still pass 4 of the 9 records.
    assert_eq!(
        differ,
        [
            "posix-path",
            "path-with-spaces",
            "path-with-non-ascii",
            "filesystem-root-posix",
            "trailing-slash"
        ],
        "which records distinguish resolve-then-encode from encode-only"
    );

    // The M2 half of the fixture is the other side of that same coin.
    let m2 = records_for(&fixture, "migrateSessionsFromAgentRoot");
    let posix = m2
        .iter()
        .find(|record| record["name"] == "posix-path")
        .expect("the shared `posix-path` case");
    assert_eq!(posix["cwd"], "/home/user/project");
    assert_eq!(posix["encodedDirName"], "--home-user-project--");
    assert_eq!(
        env.default_session_dir_path("/home/user/project", Some("C:\\oracle\\agent"))
            .unwrap(),
        "C:\\oracle\\agent\\sessions\\--C--home-user-project--"
    );

    // …and the drive really does come from `process.cwd()`, which is why the capture's
    // drive had to be recovered rather than assumed.
    let other_drive = SessionEnv::new(
        ConfigEnv {
            platform: env.config.platform,
            ..env.config.clone()
        },
        "D:\\elsewhere",
    );
    assert_eq!(
        other_drive
            .default_session_dir_path("/home/user/project", Some("C:\\oracle\\agent"))
            .unwrap(),
        "C:\\oracle\\agent\\sessions\\--D--home-user-project--"
    );
}

// ===========================================================================
// Lifecycle — tempfile dirs only, deterministic clock and ids
// ===========================================================================

/// The one timestamp every entry and file name in these tests carries.
const FIXED_TIME: &str = "2025-01-01T00:00:00.000Z";
/// `FIXED_TIME` after `replace(/[:.]/g, "-")` (`session-manager.ts:883`).
const FIXED_FILE_TIME: &str = "2025-01-01T00-00-00-000Z";

/// Sequential stand-ins for `uuidv7()` and `randomUUID()`, so file names and entry ids are
/// reproducible. `random_uuid` keeps `randomUUID`'s shape because `generateId` slices its
/// first 8 characters.
#[derive(Debug, Default)]
struct SeqIds {
    session: AtomicUsize,
    entry: AtomicUsize,
}

impl SessionIdSource for SeqIds {
    fn session_id(&self) -> String {
        format!("s{}", self.session.fetch_add(1, Ordering::Relaxed) + 1)
    }

    fn random_uuid(&self) -> String {
        let n = self.entry.fetch_add(1, Ordering::Relaxed) + 1;
        format!("e{n:07}-0000-4000-8000-000000000000")
    }
}

/// A store rooted entirely inside `root`: the agent dir, the "home" (so a stray `~` cannot
/// escape) and `process.cwd()` all point into the temp directory.
fn test_env(root: &std::path::Path) -> SessionEnv {
    let mut env = SessionEnv::new(
        ConfigEnv {
            identity: pirust_coding_agent::config::PIRUST,
            platform: Platform::current(),
            home_dir: Some(root.join("home").to_string_lossy().into_owned()),
            agent_dir_override: Some(root.join("agent").to_string_lossy().into_owned()),
        },
        root.join("project").to_string_lossy().into_owned(),
    );
    env.clock = Arc::new(FixedClock(FIXED_TIME.to_string()));
    env.ids = Arc::new(SeqIds::default());
    env
}

/// The project cwd `test_env` points at, created on disk.
fn project_cwd(root: &std::path::Path) -> String {
    let cwd = root.join("project");
    std::fs::create_dir_all(&cwd).expect("mkdir project");
    cwd.to_string_lossy().into_owned()
}

fn read(path: &str) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

fn user_message(text: &str) -> Value {
    serde_json::json!({"role": "user", "content": [{"type": "text", "text": text}]})
}

fn assistant_message(text: &str) -> Value {
    serde_json::json!({
        "role": "assistant",
        "content": [{"type": "text", "text": text}],
        "api": "anthropic-messages",
        "provider": "anthropic",
        "model": "claude-sonnet-4-5"
    })
}

#[test]
fn no_session_file_exists_until_the_first_assistant_message() {
    // spec §11.2 — the rule the feat-005 live differential depends on.
    let tmp = tempfile::tempdir().expect("tempdir");
    let env = test_env(tmp.path());
    let cwd = project_cwd(tmp.path());

    let mut manager = env.create(&cwd, None, None).expect("create");
    let file = manager
        .get_session_file()
        .expect("a persisting session has a file")
        .to_string();

    // The name is `${timestamp.replace(/[:.]/g,"-")}_${sessionId}.jsonl` (`:883-884`), in
    // the encoded default dir…
    assert!(
        file.ends_with(&format!("{FIXED_FILE_TIME}_s1.jsonl")),
        "unexpected session file name {file}"
    );
    assert_eq!(
        manager.get_session_dir(),
        env.default_session_dir_path(&cwd, None).unwrap()
    );
    assert!(manager.uses_default_session_dir().unwrap());
    // …the directory is created eagerly (`:814-816`), but the file is NOT.
    assert!(std::path::Path::new(manager.get_session_dir()).is_dir());
    assert!(!std::path::Path::new(&file).exists(), "no file yet");

    // The first three entries of a fresh run (spec §10.3 step 11) — still nothing on disk.
    manager
        .append_model_change("anthropic", "claude-sonnet-4-5")
        .expect("model_change");
    manager
        .append_thinking_level_change("off")
        .expect("thinking_level_change");
    manager
        .append_message(&user_message("hello"))
        .expect("user message");
    assert!(
        !std::path::Path::new(&file).exists(),
        "three buffered entries must still leave no file"
    );
    assert_eq!(
        manager.file_entries().len(),
        4,
        "header + 3 entries buffered"
    );

    // The assistant reply flushes the whole buffer, header first, with flag "wx".
    manager
        .append_message(&assistant_message("hi"))
        .expect("assistant message");
    let flushed = read(&file);
    let expected = format!(
        concat!(
            r#"{{"type":"session","version":3,"id":"s1","timestamp":"{time}","cwd":"{cwd}"}}"#,
            "\n",
            r#"{{"type":"model_change","id":"e0000001","parentId":null,"timestamp":"{time}","provider":"anthropic","modelId":"claude-sonnet-4-5"}}"#,
            "\n",
            r#"{{"type":"thinking_level_change","id":"e0000002","parentId":"e0000001","timestamp":"{time}","thinkingLevel":"off"}}"#,
            "\n",
            r#"{{"type":"message","id":"e0000003","parentId":"e0000002","timestamp":"{time}","message":{{"role":"user","content":[{{"type":"text","text":"hello"}}]}}}}"#,
            "\n",
            r#"{{"type":"message","id":"e0000004","parentId":"e0000003","timestamp":"{time}","message":{{"role":"assistant","content":[{{"type":"text","text":"hi"}}],"api":"anthropic-messages","provider":"anthropic","model":"claude-sonnet-4-5"}}}}"#,
            "\n"
        ),
        time = FIXED_TIME,
        cwd = cwd.replace('\\', "\\\\")
    );
    assert_eq!(flushed, expected, "flushed bytes");

    // Afterwards every entry is appended individually, not rewritten.
    manager
        .append_message(&user_message("again"))
        .expect("second user message");
    let appended = read(&file);
    assert!(
        appended.starts_with(&expected),
        "the prefix must be untouched"
    );
    assert_eq!(appended.lines().count(), 6);
    assert_eq!(
        appended.lines().last().unwrap(),
        format!(
            r#"{{"type":"message","id":"e0000005","parentId":"e0000004","timestamp":"{FIXED_TIME}","message":{{"role":"user","content":[{{"type":"text","text":"again"}}]}}}}"#
        )
    );
}

#[test]
fn an_opened_session_appends_directly_because_it_is_already_flushed() {
    // `_persist`'s `!hasAssistant && flushed` branch (`:951-952`): an existing file with no
    // assistant message still grows one line at a time.
    let tmp = tempfile::tempdir().expect("tempdir");
    let env = test_env(tmp.path());
    let cwd = project_cwd(tmp.path());
    let file = tmp.path().join("existing.jsonl");
    let header = format!(
        r#"{{"type":"session","version":3,"id":"kept","timestamp":"{FIXED_TIME}","cwd":"{}"}}"#,
        cwd.replace('\\', "\\\\")
    );
    std::fs::write(&file, format!("{header}\n")).expect("write header");

    let mut manager = env.open(&file.to_string_lossy(), None, None).expect("open");
    assert_eq!(manager.get_session_id(), "kept", "header id is adopted");
    // The session dir defaults to the file's parent (`:1459`), not the encoded default.
    assert_eq!(
        manager.get_session_dir(),
        tmp.path().to_string_lossy(),
        "sessionDir defaults to resolve(path, '..')"
    );

    manager.append_message(&user_message("q")).expect("append");
    let content = read(&file.to_string_lossy());
    assert_eq!(content.lines().count(), 2, "appended, not rewritten");
    assert!(content.starts_with(&header));
}

#[test]
fn a_zero_byte_session_file_is_initialized_in_place() {
    // `:833-843` — an empty explicit file gets a fresh header written immediately
    // (`_rewriteFile`), which is the one exception to the no-file rule.
    let tmp = tempfile::tempdir().expect("tempdir");
    let env = test_env(tmp.path());
    let file = tmp.path().join("empty.jsonl");
    std::fs::write(&file, "").expect("touch");

    let manager = env.open(&file.to_string_lossy(), None, None).expect("open");
    let content = read(&file.to_string_lossy());
    assert_eq!(content.lines().count(), 1, "just the new header");
    assert!(content.contains(r#""type":"session","version":3,"id":"s1""#));
    assert_eq!(manager.get_session_file().unwrap(), file.to_string_lossy());
    // cwd falls back to `process.cwd()` because the file had no header (`:1457`).
    assert_eq!(manager.get_cwd(), env.process_cwd);
}

#[test]
fn a_non_session_file_is_rejected_with_pis_message_and_left_alone() {
    // `:834-837` — spec §3.6's `Session file is not a valid pi session: ${path}`.
    let tmp = tempfile::tempdir().expect("tempdir");
    let env = test_env(tmp.path());
    let file = tmp.path().join("notes.jsonl");
    std::fs::write(&file, "just some text\n").expect("write");
    let resolved = env.resolve_path(&file.to_string_lossy()).unwrap();

    let error = env
        .open(&file.to_string_lossy(), None, None)
        .expect_err("must reject");
    assert_eq!(
        error.to_string(),
        format!("Session file is not a valid pi session: {resolved}")
    );
    assert_eq!(read(&file.to_string_lossy()), "just some text\n");
}

#[test]
fn an_explicit_missing_session_path_stays_unwritten() {
    // `:854-858` — a fresh session is started but the explicit path is preserved, so
    // `--session <new-file>` also writes nothing before the first assistant message.
    let tmp = tempfile::tempdir().expect("tempdir");
    let env = test_env(tmp.path());
    let file = tmp.path().join("nested").join("new.jsonl");

    let mut manager = env.open(&file.to_string_lossy(), None, None).expect("open");
    assert_eq!(manager.get_session_file().unwrap(), file.to_string_lossy());
    assert!(!file.exists());
    manager.append_message(&user_message("hi")).expect("append");
    assert!(!file.exists(), "still nothing without an assistant message");
    manager
        .append_message(&assistant_message("yo"))
        .expect("append");
    assert!(file.exists());
    assert_eq!(read(&file.to_string_lossy()).lines().count(), 3);
}

#[test]
fn in_memory_sessions_never_touch_the_disk() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let env = test_env(tmp.path());
    let cwd = project_cwd(tmp.path());

    let mut manager = env
        .in_memory(Some(&cwd), Some(&NewSessionOptions::with_id("abc.def-1")))
        .expect("in_memory");
    assert!(!manager.is_persisted());
    assert_eq!(manager.get_session_dir(), "");
    assert_eq!(manager.get_session_file(), None);
    assert_eq!(manager.get_session_id(), "abc.def-1");
    manager
        .append_message(&assistant_message("hi"))
        .expect("append");
    assert_eq!(manager.file_entries().len(), 2);
    // Nothing was created anywhere under the agent dir.
    assert!(!tmp.path().join("agent").exists());

    // An invalid explicit id throws from `newSession` (`:862-864`).
    let error = env
        .in_memory(Some(&cwd), Some(&NewSessionOptions::with_id("-bad")))
        .expect_err("invalid id");
    assert!(matches!(error, SessionError::InvalidSessionId));
    assert_eq!(
        error.to_string(),
        "Session id must be non-empty, contain only alphanumeric characters, '-', '_', and \
         '.', and start and end with an alphanumeric character"
    );
}

#[test]
fn fork_from_copies_every_entry_and_records_the_parent() {
    // `:1490-1541` — the forked file exists immediately (flag "wx" on the header).
    let tmp = tempfile::tempdir().expect("tempdir");
    let env = test_env(tmp.path());
    let cwd = project_cwd(tmp.path());
    let source = tmp.path().join("source.jsonl");
    let entries = [
        r#"{"type":"message","id":"aa","parentId":null,"timestamp":"2024-01-01T00:00:00.000Z","message":{"role":"user","content":"hi"}}"#,
        r#"{"type":"message","id":"bb","parentId":"aa","timestamp":"2024-01-01T00:00:01.000Z","message":{"role":"assistant","content":"yo"}}"#,
    ];
    std::fs::write(
        &source,
        format!(
            "{{\"type\":\"session\",\"version\":3,\"id\":\"src\",\"timestamp\":\"2024-01-01T00:00:00.000Z\",\"cwd\":\"/elsewhere\"}}\n{}\n{}\n",
            entries[0], entries[1]
        ),
    )
    .expect("write source");
    let resolved_source = env.resolve_path(&source.to_string_lossy()).unwrap();

    let manager = env
        .fork_from(&source.to_string_lossy(), &cwd, None, None)
        .expect("fork");
    let forked = manager.get_session_file().expect("file").to_string();
    assert!(
        std::path::Path::new(&forked).exists(),
        "forks write eagerly"
    );

    let lines: Vec<String> = read(&forked).lines().map(str::to_string).collect();
    assert_eq!(lines.len(), 3);
    let header: Value = serde_json::from_str(&lines[0]).unwrap();
    assert_eq!(header["id"], "s1");
    assert_eq!(header["cwd"], env.resolve_path(&cwd).unwrap());
    assert_eq!(header["parentSession"], resolved_source);
    assert_eq!(
        header.as_object().unwrap().keys().collect::<Vec<_>>(),
        ["type", "version", "id", "timestamp", "cwd", "parentSession"],
        "header key order (`:1523-1530`)"
    );
    // Entries are copied verbatim — ids, parents and timestamps untouched.
    assert_eq!(lines[1], entries[0]);
    assert_eq!(lines[2], entries[1]);
    assert_eq!(manager.get_cwd(), env.resolve_path(&cwd).unwrap());

    // An empty or headerless source is rejected with Pi's messages (spec §3.6).
    let empty = tmp.path().join("empty2.jsonl");
    std::fs::write(&empty, "").expect("touch");
    let error = env
        .fork_from(&empty.to_string_lossy(), &cwd, None, None)
        .expect_err("empty source");
    assert_eq!(
        error.to_string(),
        format!(
            "Cannot fork: source session file is empty or invalid: {}",
            env.resolve_path(&empty.to_string_lossy()).unwrap()
        )
    );
}

// ===========================================================================
// Listing + resolution
// ===========================================================================

/// Write a ready-made session file into `dir` with the given id, cwd and header time.
fn seed_session(dir: &std::path::Path, id: &str, cwd: &str, time: &str, text: &str) -> String {
    std::fs::create_dir_all(dir).expect("mkdir");
    let path = dir.join(format!("{}_{id}.jsonl", time.replace([':', '.'], "-")));
    let header = serde_json::json!({
        "type": "session", "version": 3, "id": id, "timestamp": time, "cwd": cwd
    });
    let message = serde_json::json!({
        "type": "message", "id": "m1", "parentId": null, "timestamp": time,
        "message": {"role": "user", "content": [{"type": "text", "text": text}]}
    });
    std::fs::write(
        &path,
        format!(
            "{}\n{}\n",
            serde_json::to_string(&header).unwrap(),
            serde_json::to_string(&message).unwrap()
        ),
    )
    .expect("write session");
    path.to_string_lossy().into_owned()
}

#[test]
fn listing_sorts_by_modified_descending_and_reports_progress() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let env = test_env(tmp.path());
    let cwd = project_cwd(tmp.path());
    let dir = env.default_session_dir(&cwd, None).expect("default dir");
    let dir = std::path::Path::new(&dir);
    let resolved_cwd = env.resolve_path(&cwd).unwrap();

    seed_session(
        dir,
        "old",
        &resolved_cwd,
        "2024-01-01T00:00:00.000Z",
        "first",
    );
    seed_session(
        dir,
        "new",
        &resolved_cwd,
        "2025-06-01T00:00:00.000Z",
        "second",
    );

    let mut progress: Vec<(usize, usize)> = Vec::new();
    let sessions = env
        .list(
            &cwd,
            None,
            Some(&mut |loaded, total| progress.push((loaded, total))),
        )
        .expect("list");
    assert_eq!(
        sessions.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
        ["new", "old"],
        "modified descending"
    );
    assert_eq!(progress, [(1, 2), (2, 2)], "one callback per file");
    assert_eq!(sessions[0].first_message, "second");
    assert_eq!(sessions[0].message_count, 1);
    assert_eq!(sessions[0].cwd, resolved_cwd);
    assert_eq!(sessions[0].name, None);

    // `findMostRecentSession` with a cwd filter keeps only matching headers (`:580-584`).
    seed_session(
        dir,
        "foreign",
        "/somewhere/else",
        "2026-01-01T00:00:00.000Z",
        "x",
    );
    let matched = env
        .find_most_recent_session(&dir.to_string_lossy(), Some(&cwd))
        .expect("find")
        .expect("a match");
    assert!(
        !matched.contains("foreign"),
        "the foreign-cwd session must be filtered out, got {matched}"
    );
}

#[test]
fn session_arguments_resolve_by_exact_then_prefix_match() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let env = test_env(tmp.path());
    let cwd = project_cwd(tmp.path());
    let dir = env.default_session_dir(&cwd, None).expect("default dir");
    let dir = std::path::Path::new(&dir);
    let resolved_cwd = env.resolve_path(&cwd).unwrap();

    // `0198c0de` is a prefix of both, and is ALSO the exact id of the older one.
    let older = seed_session(
        dir,
        "0198c0de",
        &resolved_cwd,
        "2024-01-01T00:00:00.000Z",
        "a",
    );
    let newer = seed_session(
        dir,
        "0198c0de-aaaa-4000-8000-000000000001",
        &resolved_cwd,
        "2025-01-01T00:00:00.000Z",
        "b",
    );

    // A partial UUID matches by prefix — the mutation guard for "require an exact match".
    assert_eq!(
        resolve_session_path(&env, "0198c0de-aaaa", &cwd, None).unwrap(),
        ResolvedSession::Local(newer.clone())
    );
    // …but an exact match always wins over a prefix, even a more recent one (`main.ts:172`).
    assert_eq!(
        resolve_session_path(&env, "0198c0de", &cwd, None).unwrap(),
        ResolvedSession::Local(older)
    );
    // `findLocalSessionByExactId` never prefix-matches (`main.ts:153-161`).
    assert_eq!(
        pirust_coding_agent::session::find_local_session_by_exact_id(
            &env,
            "0198c0de-aaaa",
            &cwd,
            None
        )
        .unwrap(),
        None
    );
    assert_eq!(
        pirust_coding_agent::session::find_local_session_by_exact_id(
            &env,
            "0198c0de-aaaa-4000-8000-000000000001",
            &cwd,
            None
        )
        .unwrap(),
        Some(newer)
    );

    // Path-shaped arguments skip id matching entirely (`main.ts:165-167`), resolved against
    // the CWD rather than `process.cwd()`.
    assert_eq!(
        resolve_session_path(&env, "sub/other.jsonl", &cwd, None).unwrap(),
        ResolvedSession::Path(env.resolve_path_in("sub/other.jsonl", &cwd).unwrap())
    );
    assert_eq!(
        resolve_session_path(&env, "bare.jsonl", &cwd, None).unwrap(),
        ResolvedSession::Path(env.resolve_path_in("bare.jsonl", &cwd).unwrap())
    );
    // Nothing anywhere.
    assert_eq!(
        resolve_session_path(&env, "zzzz", &cwd, None).unwrap(),
        ResolvedSession::NotFound("zzzz".to_string())
    );
}

// ===========================================================================
// createSessionManager + the injected prompt seam
// ===========================================================================

/// A scripted [`SessionPrompts`]: what `confirm` answers, and what the picker returns.
struct ScriptedPrompts {
    confirm: bool,
    confirmed_with: Vec<String>,
    picked: Result<Option<String>, PickerUnavailable>,
    /// How many sessions the picker's loaders reported — proves the loaders are usable.
    listed: Option<(usize, usize)>,
}

impl ScriptedPrompts {
    fn confirming() -> Self {
        Self {
            confirm: true,
            confirmed_with: Vec::new(),
            picked: Ok(None),
            listed: None,
        }
    }

    fn picking(path: Option<&str>) -> Self {
        Self {
            confirm: false,
            confirmed_with: Vec::new(),
            picked: Ok(path.map(str::to_string)),
            listed: None,
        }
    }
}

impl SessionPrompts for ScriptedPrompts {
    fn confirm(&mut self, message: &str) -> bool {
        self.confirmed_with.push(message.to_string());
        self.confirm
    }

    fn select_session(
        &mut self,
        loaders: &SessionLoaders<'_>,
    ) -> Result<Option<String>, PickerUnavailable> {
        // Exactly what `SessionSelectorComponent` does with its two callbacks.
        let current = loaders.current(None).expect("current loader");
        let all = loaders.all(None).expect("all loader");
        self.listed = Some((current.len(), all.len()));
        self.picked.clone()
    }
}

fn args(argv: &[&str]) -> pirust_coding_agent::args::Args {
    let argv: Vec<String> = argv.iter().map(|a| a.to_string()).collect();
    let parsed = pirust_coding_agent::args::parse_args(&argv);
    assert!(
        parsed.diagnostics.is_empty(),
        "unexpected parse diagnostics for {argv:?}: {:?}",
        parsed.diagnostics
    );
    parsed
}

/// Seed a session belonging to a *different* project, inside the same agent dir, so
/// `listAll` finds it but `list` does not.
fn seed_foreign_session(env: &SessionEnv, id: &str) -> String {
    let sessions_root = env.config.sessions_dir().expect("sessions dir");
    let dir = std::path::Path::new(&sessions_root).join("--other-project--");
    seed_session(
        &dir,
        id,
        "/other/project",
        "2025-01-01T00:00:00.000Z",
        "foreign",
    )
}

#[test]
fn the_session_global_branch_prompts_and_a_headless_caller_declines() {
    // spec §17.2 — the hazard. Declining is exit 0 with `Aborted.`, which is also what Pi
    // produces on a piped stdin (EOF -> "" -> not "y").
    let tmp = tempfile::tempdir().expect("tempdir");
    let env = test_env(tmp.path());
    let cwd = project_cwd(tmp.path());
    seed_foreign_session(&env, "ffff1111");

    let parsed = args(&["--session", "ffff1111"]);
    let mut console = CapturingConsole::default();
    let mut prompts = HeadlessPrompts;
    let mut io = SessionIo {
        console: &mut console,
        prompts: &mut prompts,
    };
    let exit =
        create_session_manager(&env, &parsed, &cwd, None, &mut io).expect_err("declined -> exit 0");
    assert_eq!(exit, SessionExit::SUCCESS);
    assert_eq!(
        console.texts(),
        [
            "Session found in different project: /other/project",
            "Aborted."
        ]
    );
    assert_eq!(console.lines[0].stream, SessionStream::Stdout);
    assert_eq!(console.lines[0].style, SessionStyle::Yellow);
    assert_eq!(console.lines[1].style, SessionStyle::Dim);
    // Nothing was forked into this project.
    let local = env.default_session_dir(&cwd, None).unwrap();
    assert_eq!(std::fs::read_dir(&local).unwrap().count(), 0);
}

#[test]
fn the_session_global_branch_forks_when_confirmed() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let env = test_env(tmp.path());
    let cwd = project_cwd(tmp.path());
    let foreign = seed_foreign_session(&env, "ffff2222");

    let parsed = args(&["--session", "ffff2222"]);
    let mut console = CapturingConsole::default();
    let mut prompts = ScriptedPrompts::confirming();
    let mut io = SessionIo {
        console: &mut console,
        prompts: &mut prompts,
    };
    let manager = create_session_manager(&env, &parsed, &cwd, None, &mut io).expect("forked");
    assert_eq!(
        prompts.confirmed_with,
        ["Fork this session into current directory?"],
        "the `[y/N] ` suffix belongs to the seam, not the caller"
    );
    assert_eq!(
        console.texts(),
        ["Session found in different project: /other/project"]
    );
    // The fork landed in THIS project, with the foreign file as its parent.
    let forked = manager.get_session_file().expect("file");
    assert!(forked.starts_with(&env.default_session_dir_path(&cwd, None).unwrap()));
    let header = manager.get_header().expect("header");
    assert_eq!(header["cwd"], env.resolve_path(&cwd).unwrap());
    assert_eq!(header["parentSession"], env.resolve_path(&foreign).unwrap());
}

#[test]
fn resume_fails_fast_without_a_terminal_and_uses_the_picker_when_present() {
    // spec §17.1 — the documented divergence: Pi builds the TUI unconditionally and hangs.
    let tmp = tempfile::tempdir().expect("tempdir");
    let env = test_env(tmp.path());
    let cwd = project_cwd(tmp.path());
    let dir = env.default_session_dir(&cwd, None).unwrap();
    let seeded = seed_session(
        std::path::Path::new(&dir),
        "aaaa1111",
        &env.resolve_path(&cwd).unwrap(),
        "2025-01-01T00:00:00.000Z",
        "hi",
    );
    seed_foreign_session(&env, "bbbb2222");
    let parsed = args(&["--resume"]);

    let mut console = CapturingConsole::default();
    let mut headless = HeadlessPrompts;
    let mut io = SessionIo {
        console: &mut console,
        prompts: &mut headless,
    };
    let exit = create_session_manager(&env, &parsed, &cwd, None, &mut io)
        .expect_err("no terminal -> exit 1");
    assert_eq!(exit, SessionExit::FAILURE);
    assert_eq!(
        console.texts(),
        ["Error: --resume requires an interactive terminal"]
    );
    assert_eq!(console.lines[0].stream, SessionStream::Stderr);

    // Cancelling the picker is Pi's `null`: `No session selected` on stdout, exit 0.
    let mut console = CapturingConsole::default();
    let mut cancelling = ScriptedPrompts::picking(None);
    let mut io = SessionIo {
        console: &mut console,
        prompts: &mut cancelling,
    };
    let exit = create_session_manager(&env, &parsed, &cwd, None, &mut io)
        .expect_err("cancelled -> exit 0");
    assert_eq!(exit, SessionExit::SUCCESS);
    assert_eq!(console.texts(), ["No session selected"]);
    assert_eq!(console.lines[0].style, SessionStyle::Dim);
    // Both loaders were reachable from inside the seam: 1 local, 2 across all projects.
    assert_eq!(cancelling.listed, Some((1, 2)));

    // A selection is opened.
    let mut console = CapturingConsole::default();
    let mut picking = ScriptedPrompts::picking(Some(&seeded));
    let mut io = SessionIo {
        console: &mut console,
        prompts: &mut picking,
    };
    let manager = create_session_manager(&env, &parsed, &cwd, None, &mut io).expect("opened");
    assert_eq!(manager.get_session_id(), "aaaa1111");
    assert!(console.lines.is_empty());
}

#[test]
fn flag_conflicts_report_pis_exact_strings_and_exit_codes() {
    // spec §3.6, `main.ts:216` / `:231` / `:239`.
    let cases: [(&[&str], &str); 5] = [
        (
            &["--fork", "x", "--session", "y"],
            "Error: --fork cannot be combined with --session",
        ),
        (
            &[
                "--fork",
                "x",
                "--session",
                "y",
                "--continue",
                "--resume",
                "--no-session",
            ],
            "Error: --fork cannot be combined with --session, --continue, --resume, --no-session",
        ),
        (
            &["--fork", "x", "--resume"],
            "Error: --fork cannot be combined with --resume",
        ),
        (
            &["--fork", "x", "--continue", "--no-session"],
            "Error: --fork cannot be combined with --continue, --no-session",
        ),
        (
            &["--fork", "x", "--no-session"],
            "Error: --fork cannot be combined with --no-session",
        ),
    ];
    for (argv, want) in cases {
        let parsed = args(argv);
        let mut console = CapturingConsole::default();
        let exit = validate_fork_flags(&parsed, &mut console).expect_err("must exit");
        assert_eq!(exit, SessionExit::FAILURE, "{argv:?}");
        assert_eq!(console.texts(), [want], "{argv:?}");
        assert_eq!(console.lines[0].stream, SessionStream::Stderr);
        assert_eq!(console.lines[0].style, SessionStyle::Red);
    }

    // --fork alone is fine, and --session-id is NOT a --fork conflict.
    for argv in [
        vec!["--fork", "x"],
        vec!["--fork", "x", "--session-id", "abc"],
    ] {
        let parsed = args(&argv);
        let mut console = CapturingConsole::default();
        validate_fork_flags(&parsed, &mut console).expect("no conflict");
        assert!(console.lines.is_empty(), "{argv:?}");
    }

    // --session-id's own conflicts: --session, --continue, --resume (NOT --no-session).
    let cases: [(&[&str], &str); 3] = [
        (
            &["--session-id", "a", "--continue"],
            "Error: --session-id cannot be combined with --continue",
        ),
        (
            &["--session-id", "a", "--session", "b", "--resume"],
            "Error: --session-id cannot be combined with --session, --resume",
        ),
        (
            &["--session-id", "-bad"],
            "Error: Session id must be non-empty, contain only alphanumeric characters, '-', \
             '_', and '.', and start and end with an alphanumeric character",
        ),
    ];
    for (argv, want) in cases {
        let parsed = args(argv);
        let mut console = CapturingConsole::default();
        let exit = validate_session_id_flags(&parsed, &mut console).expect_err("must exit");
        assert_eq!(exit, SessionExit::FAILURE, "{argv:?}");
        assert_eq!(console.texts(), [want], "{argv:?}");
    }
    let parsed = args(&["--session-id", "ok-1", "--no-session"]);
    let mut console = CapturingConsole::default();
    validate_session_id_flags(&parsed, &mut console).expect("--no-session is not a conflict");
    assert!(console.lines.is_empty());
}

#[test]
fn session_id_opens_an_existing_session_or_warns_and_creates_one() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let env = test_env(tmp.path());
    let cwd = project_cwd(tmp.path());
    let dir = env.default_session_dir(&cwd, None).unwrap();

    // No session with that id yet: a yellow warning on stderr, then a fresh session.
    let parsed = args(&["--session-id", "my-id"]);
    let mut console = CapturingConsole::default();
    let mut prompts = HeadlessPrompts;
    let mut io = SessionIo {
        console: &mut console,
        prompts: &mut prompts,
    };
    let manager = create_session_manager(&env, &parsed, &cwd, None, &mut io).expect("created");
    assert_eq!(
        console.texts(),
        ["Warning: No project session found with id 'my-id'; creating a new session with that id."]
    );
    assert_eq!(console.lines[0].stream, SessionStream::Stderr);
    assert_eq!(console.lines[0].style, SessionStyle::Yellow);
    assert_eq!(manager.get_session_id(), "my-id");
    assert!(manager
        .get_session_file()
        .unwrap()
        .ends_with(&format!("{FIXED_FILE_TIME}_my-id.jsonl")));
    // …and still no file (no assistant message).
    assert!(!std::path::Path::new(manager.get_session_file().unwrap()).exists());

    // Now that one exists on disk, the same flag opens it with no warning.
    seed_session(
        std::path::Path::new(&dir),
        "my-id",
        &env.resolve_path(&cwd).unwrap(),
        "2025-02-02T00:00:00.000Z",
        "existing",
    );
    let mut console = CapturingConsole::default();
    let mut prompts = HeadlessPrompts;
    let mut io = SessionIo {
        console: &mut console,
        prompts: &mut prompts,
    };
    let manager = create_session_manager(&env, &parsed, &cwd, None, &mut io).expect("opened");
    assert!(console.lines.is_empty());
    assert_eq!(manager.get_session_id(), "my-id");
    assert_eq!(
        manager.get_entries().len(),
        1,
        "the seeded message is loaded"
    );
}

#[test]
fn no_session_help_and_list_models_all_go_in_memory() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let env = test_env(tmp.path());
    let cwd = project_cwd(tmp.path());

    for argv in [
        vec!["--no-session"],
        vec!["--help"],
        vec!["--list-models"],
        // `--no-session` wins over every session flag (`main.ts:270`), including --continue.
        vec!["--no-session", "--continue"],
    ] {
        let parsed = args(&argv);
        let mut console = CapturingConsole::default();
        let mut prompts = HeadlessPrompts;
        let mut io = SessionIo {
            console: &mut console,
            prompts: &mut prompts,
        };
        let manager = create_session_manager(&env, &parsed, &cwd, None, &mut io)
            .unwrap_or_else(|e| panic!("{argv:?}: {e}"));
        assert!(!manager.is_persisted(), "{argv:?}");
        assert_eq!(manager.get_session_file(), None, "{argv:?}");
        assert!(console.lines.is_empty(), "{argv:?}");
    }
    // Nothing was created under the agent dir by any of them.
    assert!(!tmp.path().join("agent").exists());
}

#[test]
fn continue_picks_up_the_most_recent_session() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let env = test_env(tmp.path());
    let cwd = project_cwd(tmp.path());
    let dir = env.default_session_dir(&cwd, None).unwrap();
    seed_session(
        std::path::Path::new(&dir),
        "only",
        &env.resolve_path(&cwd).unwrap(),
        "2025-01-01T00:00:00.000Z",
        "hi",
    );

    let parsed = args(&["--continue"]);
    let mut console = CapturingConsole::default();
    let mut prompts = HeadlessPrompts;
    let mut io = SessionIo {
        console: &mut console,
        prompts: &mut prompts,
    };
    let manager = create_session_manager(&env, &parsed, &cwd, None, &mut io).expect("continued");
    assert_eq!(manager.get_session_id(), "only");
    assert!(console.lines.is_empty());

    // With nothing to continue, a fresh session (`:1475`).
    let tmp2 = tempfile::tempdir().expect("tempdir");
    let env2 = test_env(tmp2.path());
    let cwd2 = project_cwd(tmp2.path());
    let mut console = CapturingConsole::default();
    let mut prompts = HeadlessPrompts;
    let mut io = SessionIo {
        console: &mut console,
        prompts: &mut prompts,
    };
    let manager = create_session_manager(&env2, &parsed, &cwd2, None, &mut io).expect("fresh");
    assert_eq!(manager.get_session_id(), "s1");
    assert_eq!(manager.get_entries().len(), 0);
}

// ===========================================================================
// Byte-level checks against Pi-generated fixtures
// ===========================================================================

fn agent_fixture(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/pi/agent")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

#[test]
fn a_pi_written_session_round_trips_byte_for_byte() {
    // `loadEntriesFromFile` + `_rewriteFile` must be an identity on Pi's own bytes: every
    // key order, every unknown entry type (`active_tools_change`, `leaf` — which
    // coding-agent never writes but happily carries) and every number survives.
    let tmp = tempfile::tempdir().expect("tempdir");
    let env = test_env(tmp.path());
    let header = agent_fixture("header.golden");
    let entries = agent_fixture("entries.corpus.jsonl");
    let original = format!("{header}{entries}");
    let path = tmp.path().join("corpus.jsonl");
    std::fs::write(&path, &original).expect("write corpus");

    let loaded = env
        .load_entries_from_file(&path.to_string_lossy())
        .expect("load");
    let want_lines: Vec<&str> = original.lines().collect();
    assert_eq!(loaded.len(), want_lines.len(), "17 entries + 1 header");
    for (entry, want) in loaded.iter().zip(&want_lines) {
        assert_eq!(&serde_json::to_string(entry).unwrap(), want);
    }
    assert!(is_header(&loaded[0]));

    // The header's key order is what `newSession` builds (`:867-874`).
    let mut manager = env
        .create(&project_cwd(tmp.path()), None, None)
        .expect("create");
    let built = manager.get_header().expect("header").clone();
    let pi_header: Value = serde_json::from_str(header.trim()).unwrap();
    assert_eq!(
        built.as_object().unwrap().keys().collect::<Vec<_>>(),
        pi_header.as_object().unwrap().keys().collect::<Vec<_>>(),
        "header key order must match Pi's captured header line"
    );

    // …and `custom`/`custom_message` do NOT follow agent-core's order. The corpus is
    // agent-core's own capture, so this pins both sides of the divergence.
    let corpus_custom: Value = entries
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|entry| entry["type"] == "custom")
        .expect("a custom entry in the corpus");
    assert_eq!(
        corpus_custom
            .as_object()
            .unwrap()
            .keys()
            .collect::<Vec<_>>(),
        ["type", "id", "parentId", "timestamp", "customType", "data"],
        "agent-core writes the usual prefix"
    );
    manager
        .append_custom_entry("mytype", Some(serde_json::json!({"note": "data"})))
        .expect("custom");
    let appended = manager.file_entries().last().unwrap();
    assert_eq!(
        appended.as_object().unwrap().keys().collect::<Vec<_>>(),
        ["type", "customType", "data", "id", "parentId", "timestamp"],
        "coding-agent's literal order (`session-manager.ts:1051-1059`) differs"
    );
    manager
        .append_custom_message_entry("note", Value::String("hello".into()), true, None)
        .expect("custom_message");
    assert_eq!(
        manager
            .file_entries()
            .last()
            .unwrap()
            .as_object()
            .unwrap()
            .keys()
            .collect::<Vec<_>>(),
        [
            "type",
            "customType",
            "content",
            "display",
            "id",
            "parentId",
            "timestamp"
        ],
        "`:1106-1115`, with `details` omitted when undefined"
    );
}

#[test]
fn v1_sessions_migrate_to_v3_when_opened() {
    // NOT oracle-verified: spec §11.4's v1 fixtures
    // (`packages/coding-agent/test/fixtures/{before-compaction,large-session}.jsonl`) are not
    // vendored under `tests/fixtures/pi/`, so this asserts the structure the transcription
    // must produce, not captured bytes. Vendoring them (with an injected id source, since
    // `generateId` is random) is the way to close this.
    let tmp = tempfile::tempdir().expect("tempdir");
    let env = test_env(tmp.path());
    let path = tmp.path().join("v1.jsonl");
    let v1 = concat!(
        // A v1 header: no `version`, and `cwd` before nothing else.
        r#"{"type":"session","id":"v1sess","timestamp":"2024-01-01T00:00:00.000Z","cwd":"/p"}"#,
        "\n",
        r#"{"type":"message","timestamp":"2024-01-01T00:00:01.000Z","message":{"role":"user","content":"a"}}"#,
        "\n",
        r#"{"type":"message","timestamp":"2024-01-01T00:00:02.000Z","message":{"role":"hookMessage","content":"h"}}"#,
        "\n",
        r#"{"type":"compaction","timestamp":"2024-01-01T00:00:03.000Z","summary":"s","tokensBefore":9,"firstKeptEntryIndex":1}"#,
        "\n"
    );
    std::fs::write(&path, v1).expect("write v1");

    let manager = env.open(&path.to_string_lossy(), None, None).expect("open");
    // The file was rewritten in place because the migration reported a change.
    let lines: Vec<Value> = read(&path.to_string_lossy())
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(lines.len(), 4);

    // v1->v2 adds `version` as a NEW key, so JS appends it LAST; v2->v3 then overwrites it
    // in place, keeping that position.
    assert_eq!(
        lines[0].as_object().unwrap().keys().collect::<Vec<_>>(),
        ["type", "id", "timestamp", "cwd", "version"]
    );
    assert_eq!(lines[0]["version"], 3);

    // Entries gained `id`/`parentId` (appended, in that order) forming a linear chain.
    assert_eq!(
        lines[1].as_object().unwrap().keys().collect::<Vec<_>>(),
        ["type", "timestamp", "message", "id", "parentId"]
    );
    assert_eq!(lines[1]["parentId"], Value::Null);
    assert_eq!(lines[2]["parentId"], lines[1]["id"]);
    assert_eq!(lines[3]["parentId"], lines[2]["id"]);
    assert_eq!(lines[1]["id"], "e0000001");

    // `hookMessage` became `custom`.
    assert_eq!(lines[2]["message"]["role"], "custom");

    // `firstKeptEntryIndex` -> `firstKeptEntryId`, resolved against the ALREADY-assigned id
    // of `entries[1]`, and the old key is gone.
    assert_eq!(lines[3]["firstKeptEntryId"], lines[1]["id"]);
    assert!(lines[3].get("firstKeptEntryIndex").is_none());

    // The manager is consistent with the rewritten file.
    assert_eq!(manager.get_session_id(), "v1sess");
    assert_eq!(manager.get_leaf_id(), Some("e0000003"));
    assert_eq!(manager.get_branch(None).len(), 3, "a linear chain");
    let entries = manager.get_entries();
    let context = build_context_entries(&entries, LeafId::Id("e0000003"));
    assert_eq!(
        context
            .iter()
            .map(|entry| entry["type"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["compaction", "message", "message"],
        "the compaction leads, then everything from firstKeptEntryId on"
    );

    // A v3 file is left byte-identical (the migration returns false, so no rewrite).
    let before = read(&path.to_string_lossy());
    drop(
        env.open(&path.to_string_lossy(), None, None)
            .expect("reopen"),
    );
    assert_eq!(read(&path.to_string_lossy()), before);
}

#[test]
fn the_public_helpers_behave_as_pi_does() {
    // Small surfaces that the resolution paths depend on but no fixture record covers.
    assert!(assert_valid_session_id("a").is_ok());
    assert!(assert_valid_session_id("").is_err());
    assert_eq!(parse_session_entries("\n\n").len(), 0);
    assert_eq!(
        parse_session_entries("{\"a\":1}\nnope\n{\"b\":2}\n").len(),
        2
    );
    // `generateId` never returns a taken id.
    struct Ids;
    impl SessionIdSource for Ids {
        fn session_id(&self) -> String {
            "s".into()
        }
        fn random_uuid(&self) -> String {
            "abcdefab-0000-4000-8000-000000000000".into()
        }
    }
    assert_eq!(generate_id(&Ids, &|_| false), "abcdefab");
    assert_eq!(
        generate_id(&Ids, &|id| id == "abcdefab"),
        "abcdefab-0000-4000-8000-000000000000"
    );
}
