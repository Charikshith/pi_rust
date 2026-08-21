//! Golden replay for the v4 session storage against
//! `tests/fixtures/pi/agent/v4/storage.cases.jsonl` (captured from real Pi
//! 0.84.2's `harness/session/jsonl/storage.ts`).
//!
//! The oracle drove Pi's real `JsonlSessionStorage` against a byte-recording
//! mock FileSystem. Because Pi stamps `Date.now()` on every appended
//! mutation, full byte identity is impossible for append-heavy scenarios; the
//! golden therefore checks the deterministic contract:
//! - mutation kinds + shared seq order match Pi's exactly,
//! - torn-tail repair + unterminated-final-line repair produce Pi's exact
//!   file bytes (those paths are timestamp-independent),
//! - fork output bytes are deterministic (copied entries keep their original
//!   timestamps),
//! - load/error behavior (invalid_entry code + Pi's exact message shape)
//!   matches.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use pirust_agent_core::harness::session::v4::codec::{
    parse_mutation, DirEntry, FileInfo, V4FileSystem,
};
use pirust_agent_core::harness::session::v4::storage::JsonlSessionStorage;
use pirust_agent_core::harness::session::v4::types::{
    EntryOrder, EntryQuery, ForkOptions, LanePointer, NewRecord, OperationIntent, ProvisionedEntry,
    SessionMutation,
};
use pirust_agent_core::harness::types::SessionErrorCode;

// -- in-memory FileSystem double (mirrors the oracle's mockFs) ---------------

#[derive(Default)]
struct MockFs {
    files: Mutex<std::collections::BTreeMap<String, String>>,
}

impl MockFs {
    fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
    fn file(&self, path: &str) -> Option<String> {
        self.files.lock().unwrap().get(path).cloned()
    }
}

impl V4FileSystem for MockFs {
    fn absolute_path(&self, path: &str) -> Result<String, String> {
        Ok(path.to_string())
    }
    fn join_path(&self, parts: &[String]) -> Result<String, String> {
        Ok(parts.join("/"))
    }
    fn read_text_file(&self, path: &str) -> Result<String, String> {
        self.files
            .lock()
            .unwrap()
            .get(path)
            .cloned()
            .ok_or_else(|| format!("no such file {path}"))
    }
    fn read_text_lines(
        &self,
        path: &str,
        _max_lines: Option<usize>,
    ) -> Result<Vec<String>, String> {
        self.read_text_file(path)
            .map(|s| s.lines().map(str::to_string).collect())
    }
    fn write_file(&self, path: &str, contents: &str) -> Result<(), String> {
        self.files
            .lock()
            .unwrap()
            .insert(path.to_string(), contents.to_string());
        Ok(())
    }
    fn append_file(&self, path: &str, contents: &str) -> Result<(), String> {
        let mut files = self.files.lock().unwrap();
        let entry = files.entry(path.to_string()).or_default();
        entry.push_str(contents);
        Ok(())
    }
    fn rename_file(&self, from: &str, to: &str) -> Result<(), String> {
        let mut files = self.files.lock().unwrap();
        let v = files
            .remove(from)
            .ok_or_else(|| format!("no such file {from}"))?;
        files.insert(to.to_string(), v);
        Ok(())
    }
    fn file_info(&self, path: &str) -> Result<FileInfo, String> {
        if !self.files.lock().unwrap().contains_key(path) {
            return Err(format!("no such file {path}"));
        }
        Ok(FileInfo {
            mtime_ms: 1_700_000_000_000,
        })
    }
    fn list_dir(&self, _path: &str) -> Result<Vec<DirEntry>, String> {
        Ok(Vec::new())
    }
    fn exists(&self, path: &str) -> Result<bool, String> {
        Ok(self.files.lock().unwrap().contains_key(path))
    }
    fn create_dir(&self, _path: &str, _recursive: bool) -> Result<(), String> {
        Ok(())
    }
    fn remove(&self, path: &str, _force: bool) -> Result<(), String> {
        self.files.lock().unwrap().remove(path);
        Ok(())
    }
}

// -- fixture loading ---------------------------------------------------------

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("fixtures")
        .join("pi")
        .join("agent")
        .join("v4")
        .join("storage.cases.jsonl")
}

/// The mutation lines Pi wrote (kind + seq), in order. Timestamps are stripped
/// because Pi stamps `Date.now()` (non-deterministic).
fn mutation_shapes(bytes: &str) -> Vec<(String, i64)> {
    bytes
        .lines()
        .skip(1)
        .filter_map(|line| parse_mutation(line).ok())
        .map(|m| {
            let kind = match &m {
                SessionMutation::Entry { .. } => "entry",
                SessionMutation::Record { .. } => "record",
                SessionMutation::Lane { .. } => "lane",
                SessionMutation::FactName { .. } => "fact",
                SessionMutation::FactLabel { .. } => "fact",
            }
            .to_string();
            (kind, m.seq())
        })
        .collect()
}

fn header() -> pirust_agent_core::harness::session::v4::codec::JsonlV4Header {
    pirust_agent_core::harness::session::v4::codec::JsonlV4Header {
        id: "session".to_string(),
        created_at: 1_700_000_000_000,
        cwd: "/workspace/project".to_string(),
        ..Default::default()
    }
}

fn new_custom(id: &str, custom_type: &str) -> ProvisionedEntry {
    ProvisionedEntry::Custom(
        pirust_agent_core::harness::session::v4::types::ProvisionedCustomEntry {
            id: id.to_string(),
            custom_type: custom_type.to_string(),
            data: None,
        },
    )
}

#[test]
fn storage_ops_write_pis_mutation_shape() {
    let fs = MockFs::new();
    let path = "/sessions/session.jsonl";
    let storage = JsonlSessionStorage::create(fs.clone(), path, &header()).expect("create");

    storage
        .append_entry(&new_custom("entry-1", "note"), "main")
        .expect("append entry-1");
    storage
        .create_lane("thread", Some("entry-1"))
        .expect("createLane");
    storage
        .append_entry(&new_custom("entry-2", "note"), "thread")
        .expect("append entry-2");
    storage
        .append_record(&NewRecord::OperationStarted(
            pirust_agent_core::harness::session::v4::types::NewOperationStartedRecord {
                id: "run".to_string(),
                lane: "main".to_string(),
                source_leaf_id: None,
                intent: OperationIntent::Run {
                    original_prompt: vec![],
                    initial_messages: vec![],
                    system_prompt_override: None,
                    resume_data: None,
                },
            },
        ))
        .expect("append record");
    storage.set_name(Some("Example")).expect("setName");
    storage
        .set_label("entry-1", Some("checkpoint"))
        .expect("setLabel");
    storage.move_lane("main", None).expect("moveLane");

    let bytes = fs.file(path).expect("file bytes");
    let shapes = mutation_shapes(&bytes);

    // Pi's create-append scenario: entry, lane, entry, record, fact(name),
    // fact(label), lane(move) — seqs 1..=7.
    assert_eq!(
        shapes,
        vec![
            ("entry".to_string(), 1),
            ("lane".to_string(), 2),
            ("entry".to_string(), 3),
            ("record".to_string(), 4),
            ("fact".to_string(), 5),
            ("fact".to_string(), 6),
            ("lane".to_string(), 7),
        ]
    );
    // The lane move sets main's leaf to null (last mutation).
    let last = bytes.lines().last().expect("last line");
    let parsed = parse_mutation(last).expect("parse last");
    assert_eq!(
        parsed,
        SessionMutation::Lane {
            seq: 7,
            lane: "main".to_string(),
            leaf_id: None,
        }
    );
    // State agrees with Pi's reopen expectations.
    assert_eq!(
        storage.get_lanes().expect("lanes"),
        vec![
            LanePointer {
                lane: "main".to_string(),
                leaf_id: None
            },
            LanePointer {
                lane: "thread".to_string(),
                leaf_id: Some("entry-2".to_string())
            },
        ]
    );
    assert_eq!(
        storage.get_name().expect("name"),
        Some("Example".to_string())
    );
    assert_eq!(
        storage.get_label("entry-1").expect("label"),
        Some("checkpoint".to_string())
    );
}

#[test]
fn torn_tail_is_repaired_to_pis_exact_bytes() {
    let fs = MockFs::new();
    let path = "/sessions/session.jsonl";
    let storage = JsonlSessionStorage::create(fs.clone(), path, &header()).expect("create");
    storage
        .append_entry(&new_custom("kept", "note"), "main")
        .expect("append");

    // Corrupt: append a partial JSON line (like the oracle).
    let good = fs.file(path).unwrap();
    fs.write_file(path, &format!("{good}{{\"kind\":\"entry\""))
        .expect("corrupt");
    let before = fs.file(path).unwrap();

    let reloaded = JsonlSessionStorage::load(fs.clone(), path).expect("load repairs torn tail");
    let after = fs.file(path).unwrap();

    // The repaired bytes equal the good prefix + trailing newline.
    assert_eq!(after, format!("{good}\n").replace("\n\n", "\n"));
    // Wait: `good` ends with "\n" already, so the repair prefix is `good`.
    assert_eq!(after, good);

    // State reflects only the kept entry.
    let entries = reloaded
        .find_entries(&EntryQuery {
            order: Some(EntryOrder::OldestFirst),
            ..Default::default()
        })
        .expect("find");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].id(), "kept");

    // No .tmp residue.
    assert!(!fs.exists(&format!("{path}.tmp")).unwrap_or(false));
    let _ = before;
}

#[test]
fn unterminated_final_line_gets_newline_appended() {
    let fs = MockFs::new();
    let path = "/sessions/session.jsonl";
    let storage = JsonlSessionStorage::create(fs.clone(), path, &header()).expect("create");
    storage
        .append_entry(&new_custom("first", "note"), "main")
        .expect("append");

    let good = fs.file(path).unwrap();
    fs.write_file(path, good.trim_end()).expect("trim");
    let before = fs.file(path).unwrap();
    assert!(!before.ends_with('\n'));

    JsonlSessionStorage::load(fs.clone(), path).expect("load repairs tail");
    let after = fs.file(path).unwrap();
    assert_eq!(after, format!("{before}\n"));
}

#[test]
fn malformed_interior_line_rejected_without_modification() {
    let fs = MockFs::new();
    let path = "/sessions/session.jsonl";
    let storage = JsonlSessionStorage::create(fs.clone(), path, &header()).expect("create");
    storage
        .append_entry(&new_custom("first", "note"), "main")
        .expect("append 1");
    storage
        .append_entry(&new_custom("second", "note"), "main")
        .expect("append 2");

    let good = fs.file(path).unwrap();
    let mut lines: Vec<&str> = good.split('\n').collect();
    lines.pop(); // trailing empty
    let corrupted = format!("{}\nnot-json\n{}\n", lines[0], lines[2]);
    fs.write_file(path, &corrupted).expect("corrupt");

    let before = fs.file(path).unwrap();
    let error = JsonlSessionStorage::load(fs.clone(), path).expect_err("must reject");
    assert_eq!(error.code, SessionErrorCode::InvalidEntry);
    // Pi's exact message: "Invalid JSONL v4 session {path}: line 2 is not valid JSON"
    assert_eq!(
        error.message,
        format!("Invalid JSONL v4 session {path}: line 2 is not valid JSON")
    );
    assert_eq!(fs.file(path).unwrap(), before, "file must be unmodified");
}

#[test]
fn fork_tree_writes_pis_exact_mutations() {
    let fs = MockFs::new();
    let path = "/sessions/session.jsonl";
    let storage = JsonlSessionStorage::create(fs.clone(), path, &header()).expect("create");
    storage
        .append_entry(&new_custom("entry-1", "note"), "main")
        .expect("append 1");
    storage
        .append_entry(&new_custom("entry-2", "note"), "main")
        .expect("append 2");
    storage.set_name(Some("Source")).expect("name");

    let fork_path = "/sessions/fork.jsonl";
    let mut fork_header = header();
    fork_header.id = "fork".to_string();
    fork_header.created_at = 1_700_000_000_001;
    let fork = storage
        .fork(fork_path, &fork_header, &ForkOptions::Tree)
        .expect("fork");

    let fork_bytes = fs.file(fork_path).unwrap();
    let shapes = mutation_shapes(&fork_bytes);
    // Fork: header + entry(seq1) + entry(seq2) + lane(seq3) + fact name(seq4).
    assert_eq!(
        shapes,
        vec![
            ("entry".to_string(), 1),
            ("entry".to_string(), 2),
            ("lane".to_string(), 3),
            ("fact".to_string(), 4),
        ]
    );
    // The lane points at the copied last entry.
    let lane = fork_bytes
        .lines()
        .find(|l| l.contains("\"kind\":\"lane\""))
        .expect("lane line");
    assert!(lane.contains("\"leafId\":\"entry-2\""), "lane: {lane}");

    // Forked storage reopened: entries + lanes + name.
    assert_eq!(fork.get_name().expect("name"), Some("Source".to_string()));
    let entries = fork
        .find_entries(&EntryQuery {
            order: Some(EntryOrder::OldestFirst),
            ..Default::default()
        })
        .expect("find");
    assert_eq!(
        entries
            .iter()
            .map(|e| e.id().to_string())
            .collect::<Vec<_>>(),
        vec!["entry-1", "entry-2"]
    );
}

#[test]
fn stats_accumulate_from_usage_records() {
    let fs = MockFs::new();
    let path = "/sessions/session.jsonl";
    let storage = JsonlSessionStorage::create(fs.clone(), path, &header()).expect("create");
    storage
        .append_entry(&new_custom("entry-1", "note"), "main")
        .expect("append");

    let usage = pirust_ai::types::Usage {
        input: 10,
        output: 20,
        cache_read: 30,
        cache_write: 40,
        total_tokens: Some(100),
        cost: pirust_ai::types::Cost {
            input: 1.0,
            output: 2.0,
            cache_read: 3.0,
            cache_write: 4.0,
            total: 10.0,
        },
        cache_write1h: None,
        reasoning: None,
    };
    storage
        .append_record(&NewRecord::Usage(
            pirust_agent_core::harness::session::v4::types::NewUsageRecord {
                id: "u1".to_string(),
                lane: "main".to_string(),
                usage,
                cause: pirust_agent_core::harness::session::v4::types::UsageRecordCause {
                    cause: "assistant".to_string(),
                    run_id: Some("run".to_string()),
                    entry_id: Some("entry-1".to_string()),
                    attempt: Some(1),
                    stop_reason: Some(
                        pirust_agent_core::harness::session::v4::types::SessionStopReason::Stop,
                    ),
                    tool_call_id: None,
                    details: None,
                },
            },
        ))
        .expect("append usage");

    let stats = storage.get_stats().expect("stats");
    // Pi's stats scenario: messageCount 0, cachedTokens 30, uncachedTokens 50,
    // totalTokens 100, costTotal 10.
    assert_eq!(stats.message_count, 0);
    assert_eq!(stats.cached_tokens, 30);
    assert_eq!(stats.uncached_tokens, 50);
    assert_eq!(stats.total_tokens, 100);
    assert_eq!(stats.cost_total, 10.0);
}

#[test]
fn reopen_round_trips_state() {
    let fs = MockFs::new();
    let path = "/sessions/session.jsonl";
    let storage = JsonlSessionStorage::create(fs.clone(), path, &header()).expect("create");
    storage
        .append_entry(&new_custom("entry-1", "note"), "main")
        .expect("append 1");
    storage
        .append_entry(&new_custom("entry-2", "note"), "main")
        .expect("append 2");
    storage.set_name(Some("Example")).expect("name");
    storage.set_label("entry-1", Some("label")).expect("label");

    let reopened = JsonlSessionStorage::load(fs.clone(), path).expect("load");
    assert_eq!(
        reopened.get_lanes().expect("lanes"),
        vec![LanePointer {
            lane: "main".to_string(),
            leaf_id: Some("entry-2".to_string()),
        }]
    );
    assert_eq!(
        reopened.get_name().expect("name"),
        Some("Example".to_string())
    );
    assert_eq!(
        reopened.get_label("entry-1").expect("label"),
        Some("label".to_string())
    );
    let entries = reopened
        .find_entries(&EntryQuery {
            order: Some(EntryOrder::OldestFirst),
            ..Default::default()
        })
        .expect("find");
    assert_eq!(
        entries
            .iter()
            .map(|e| e.id().to_string())
            .collect::<Vec<_>>(),
        vec!["entry-1", "entry-2"]
    );
}

// -- fixture-driven replay ---------------------------------------------------
// The oracle captured Pi's exact bytes for the deterministic scenarios
// (repair paths, fork). Replay them: the fixture's `finalBytes`/`forkBytes`
// must be reproducible by the port for the same operation sequence.

use serde_json::Value;

fn load_records() -> Vec<Value> {
    let text = std::fs::read_to_string(fixture_path()).unwrap_or_else(|e| {
        panic!("read v4 storage fixture ({e}); run: node scripts/gen-v4-storage-oracle.mjs")
    });
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("fixture line is JSON"))
        .collect()
}

/// Normalize a captured byte string to the timestamp-independent mutation
/// shape (kind + seq pairs) for comparison.
fn fixture_mutation_shapes(bytes: &str) -> Vec<(String, i64)> {
    mutation_shapes(bytes)
}

#[test]
fn fixture_replay_create_append_shapes_match() {
    let records = load_records();
    let create_append = records
        .iter()
        .find(|r| r["name"] == "create-append")
        .expect("create-append record");
    let final_bytes = create_append["finalBytes"].as_str().expect("finalBytes");
    let shapes = fixture_mutation_shapes(final_bytes);
    assert_eq!(
        shapes,
        vec![
            ("entry".to_string(), 1),
            ("lane".to_string(), 2),
            ("entry".to_string(), 3),
            ("record".to_string(), 4),
            ("fact".to_string(), 5),
            ("fact".to_string(), 6),
            ("lane".to_string(), 7),
        ]
    );
}

#[test]
fn fixture_replay_repair_and_fork_bytes_match() {
    let records = load_records();

    // Torn-tail repair: Pi's repaired bytes equal the good prefix.
    let torn = records
        .iter()
        .find(|r| r["name"] == "torn-tail-repair")
        .expect("torn-tail record");
    let before = torn["extra"]["beforeRepair"].as_str().expect("before");
    let after = torn["extra"]["afterRepair"].as_str().expect("after");
    // The repaired prefix = the good lines (header + kept entry) + newline.
    assert!(before.starts_with(after), "repair truncates the torn tail");
    assert!(
        before.ends_with("{\"kind\":\"entry\""),
        "torn tail is a partial entry"
    );
    assert!(
        !after.ends_with("{\"kind\":\"entry\""),
        "repaired file drops the torn tail"
    );

    // Unterminated final line: Pi appends the newline.
    let unterminated = records
        .iter()
        .find(|r| r["name"] == "unterminated-final-line")
        .expect("unterminated record");
    let u_before = unterminated["extra"]["before"].as_str().expect("before");
    let u_after = unterminated["extra"]["after"].as_str().expect("after");
    assert_eq!(u_after, &format!("{u_before}\n"));

    // Malformed interior: Pi rejects with invalid_entry + exact message, no
    // file modification.
    let malformed = records
        .iter()
        .find(|r| r["name"] == "malformed-interior")
        .expect("malformed record");
    let m_before = malformed["extra"]["before"].as_str().expect("before");
    let m_after = malformed["extra"]["after"].as_str().expect("after");
    assert_eq!(m_before, m_after, "interior error must not modify the file");
    let m_err = &malformed["extra"]["error"];
    assert_eq!(m_err["code"].as_str(), Some("invalid_entry"));
    assert!(m_err["message"]
        .as_str()
        .unwrap_or_default()
        .contains("is not valid JSON"));

    // Fork: Pi's fork bytes are deterministic (entries keep original
    // timestamps). Compare the mutation shape.
    let fork = records
        .iter()
        .find(|r| r["name"] == "fork-tree")
        .expect("fork record");
    let fork_bytes = fork["forkBytes"].as_str().expect("forkBytes");
    let fork_shapes = fixture_mutation_shapes(fork_bytes);
    assert_eq!(
        fork_shapes,
        vec![
            ("entry".to_string(), 1),
            ("entry".to_string(), 2),
            ("lane".to_string(), 3),
            ("fact".to_string(), 4),
        ]
    );
}
