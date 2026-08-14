//! Acceptance tests for the session-tree layer (spec §1.4, §4.2, §4.4, §11.A).
//!
//! Three oracles:
//! 1. Header byte-identity: `JsonlSessionStorage::create` reproduces the
//!    `header.golden` / `header.withmeta.golden` fixtures byte-for-byte.
//! 2. `build_session_context`: the compaction collapse yields the role sequence
//!    from `tests/fixtures/pi/agent/compaction.json`.
//! 3. Structural: parentId linking, leaf advance, path-to-root order, and
//!    branch/fork leaf-repointing over an INJECTED deterministic uuid source
//!    (ids are ours, not the oracle's mock-RNG artifacts).

use std::sync::atomic::{AtomicU8, Ordering};

use pirust_agent_core::harness::messages::AgentMessage;
use pirust_agent_core::harness::session::jsonl_storage::{JsonlCreateOptions, JsonlSessionStorage};
use pirust_agent_core::harness::session::memory_storage::InMemorySessionStorage;
use pirust_agent_core::harness::session::uuid::Uuidv7Source;
use pirust_agent_core::harness::session::{
    build_session_context, BranchSummaryInput, FixedClock, Session,
};
use pirust_agent_core::harness::types::{SessionMetadata, SessionStorage, SessionTreeEntry};

/// Workspace-root fixtures dir (`CARGO_MANIFEST_DIR` = crates/pirust-agent-core).
fn fixture(path: &str) -> String {
    let root = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/pi/agent/"
    );
    std::fs::read_to_string(format!("{root}{path}"))
        .unwrap_or_else(|e| panic!("read fixture {path}: {e}"))
}

fn fixed_clock(iso: &str) -> Box<FixedClock> {
    Box::new(FixedClock(iso.to_string()))
}

/// Deterministic uuid source: every `fill_random` writes a distinct constant
/// byte pattern (a per-call counter), so `uuidv7().slice(-8)` — which lives in
/// the random tail — differs between successive entry-id allocations instead of
/// colliding on a fixed value.
#[derive(Default)]
struct SeqSource {
    counter: AtomicU8,
}

impl Uuidv7Source for SeqSource {
    fn now_ms(&self) -> u64 {
        1_700_000_000_000
    }
    fn fill_random(&self, buf: &mut [u8; 16]) {
        let n = self.counter.fetch_add(1, Ordering::Relaxed);
        for (i, b) in buf.iter_mut().enumerate() {
            *b = n.wrapping_add(i as u8);
        }
    }
}

// ---------------------------------------------------------------------------
// 1. Header byte-identity
// ---------------------------------------------------------------------------

fn golden_line(name: &str) -> String {
    fixture(name)
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap()
        .to_string()
}

fn read_header_line(path: &std::path::Path) -> String {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .next()
        .unwrap()
        .to_string()
}

#[test]
fn create_header_is_byte_identical_to_golden() {
    let dir = std::env::temp_dir().join(format!("pirust-sess-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("plain.jsonl");

    JsonlSessionStorage::create(
        path.clone(),
        JsonlCreateOptions {
            cwd: "/oracle/cwd".to_string(),
            session_id: "session-oracle".to_string(),
            parent_session_path: None,
            metadata: None,
        },
        fixed_clock("2023-11-14T22:13:20.000Z"),
    )
    .unwrap();

    assert_eq!(read_header_line(&path), golden_line("header.golden"));
}

#[test]
fn create_header_with_meta_is_byte_identical_to_golden() {
    let dir = std::env::temp_dir().join(format!("pirust-sess-meta-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("withmeta.jsonl");

    let metadata = serde_json::json!({ "foo": "bar", "n": 1 });
    JsonlSessionStorage::create(
        path.clone(),
        JsonlCreateOptions {
            cwd: "/oracle/cwd".to_string(),
            session_id: "session-oracle".to_string(),
            parent_session_path: Some("/parent/session.jsonl".to_string()),
            metadata: Some(metadata),
        },
        fixed_clock("2023-11-14T22:13:20.000Z"),
    )
    .unwrap();

    assert_eq!(
        read_header_line(&path),
        golden_line("header.withmeta.golden")
    );
}

// ---------------------------------------------------------------------------
// 2. buildSessionContext compaction collapse
// ---------------------------------------------------------------------------

/// Build a `message` entry from a JSON message body (reuses the byte-verified
/// corpus format), with the given id/parentId.
fn msg_entry(id: &str, parent: Option<&str>, message_json: serde_json::Value) -> SessionTreeEntry {
    let entry = serde_json::json!({
        "type": "message",
        "id": id,
        "parentId": parent,
        "timestamp": "2023-11-14T22:13:20.000Z",
        "message": message_json,
    });
    serde_json::from_value(entry).unwrap()
}

fn user_msg(text: &str) -> serde_json::Value {
    serde_json::json!({
        "role": "user",
        "content": [{ "type": "text", "text": text }],
        "timestamp": 1_700_000_000_000_i64,
    })
}

fn assistant_msg(text: &str) -> serde_json::Value {
    serde_json::json!({
        "role": "assistant",
        "content": [{ "type": "text", "text": text }],
        "api": "anthropic-messages",
        "provider": "anthropic",
        "model": "claude-sonnet-4-5",
        "usage": { "input": 10, "output": 5, "cacheRead": 0, "cacheWrite": 0,
                   "totalTokens": 15, "cost": { "input": 0, "output": 0,
                   "cacheRead": 0, "cacheWrite": 0, "total": 0 } },
        "stopReason": "stop",
        "timestamp": 1_700_000_000_001_i64,
    })
}

fn role_of(m: &AgentMessage) -> &'static str {
    use pirust_ai::types::Message;
    match m {
        AgentMessage::Llm(Message::User(_)) => "user",
        AgentMessage::Llm(Message::Assistant(_)) => "assistant",
        AgentMessage::Llm(Message::ToolResult(_)) => "toolResult",
        AgentMessage::BashExecution(_) => "bashExecution",
        AgentMessage::Custom(_) => "custom",
        AgentMessage::BranchSummary(_) => "branchSummary",
        AgentMessage::CompactionSummary(_) => "compactionSummary",
    }
}

#[test]
fn build_session_context_collapses_at_compaction() {
    // Mirrors compaction.test.ts:328-339: [u1, a1, u2, a2, compaction(keep=u2), u3, a3].
    let u1 = msg_entry("u1", None, user_msg("1"));
    let a1 = msg_entry("a1", Some("u1"), assistant_msg("a"));
    let u2 = msg_entry("u2", Some("a1"), user_msg("2"));
    let a2 = msg_entry("a2", Some("u2"), assistant_msg("b"));
    let compaction: SessionTreeEntry = serde_json::from_value(serde_json::json!({
        "type": "compaction",
        "id": "comp",
        "parentId": "a2",
        "timestamp": "2023-11-14T22:13:20.000Z",
        "summary": "Summary of 1,a,2,b",
        "firstKeptEntryId": "u2",
        "tokensBefore": 4321,
    }))
    .unwrap();
    let u3 = msg_entry("u3", Some("comp"), user_msg("3"));
    let a3 = msg_entry("a3", Some("u3"), assistant_msg("c"));

    let path = [u1, a1, u2, a2, compaction, u3, a3];
    let context = build_session_context(&path);

    let roles: Vec<&str> = context.messages.iter().map(role_of).collect();

    // Cross-check against the Pi-authentic structure in compaction.json.
    let oracle = fixture("compaction.json");
    let oracle: serde_json::Value = serde_json::from_str(&oracle).unwrap();
    let expected_roles: Vec<String> = oracle["buildSessionContext"]["roles"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();

    assert_eq!(context.messages.len(), 5, "length must match oracle");
    assert_eq!(
        roles,
        [
            "compactionSummary",
            "user",
            "assistant",
            "user",
            "assistant"
        ]
    );
    assert_eq!(roles, expected_roles);
    // Model derived from assistant messages in the raw path.
    assert_eq!(
        context.model.as_ref().unwrap().model_id,
        "claude-sonnet-4-5"
    );
}

// ---------------------------------------------------------------------------
// 3. Structural: parentId linking, leaf advance, path order, repointing
// ---------------------------------------------------------------------------

fn in_memory_session() -> Session<InMemorySessionStorage<SeqSource>> {
    let storage = InMemorySessionStorage::from_options(
        SeqSource::default(),
        fixed_clock("2023-11-14T22:13:20.000Z"),
        Vec::new(),
        SessionMetadata {
            id: "sess-1".to_string(),
            created_at: "2026-01-01T00:00:00.000Z".to_string(),
        },
    )
    .unwrap();
    Session::with_clock(storage, fixed_clock("2023-11-14T22:13:20.000Z"))
}

#[tokio::test]
async fn append_chain_links_parent_ids_and_advances_leaf() {
    let session = in_memory_session();

    let id1 = session
        .append_thinking_level_change("high".to_string())
        .await
        .unwrap();
    // Root entry has no parent.
    assert_eq!(session.get_leaf_id().await.unwrap(), Some(id1.clone()));

    let id2 = session
        .append_model_change("anthropic".to_string(), "claude-sonnet-4-5".to_string())
        .await
        .unwrap();
    let id3 = session
        .append_active_tools_change(vec!["read".to_string()])
        .await
        .unwrap();

    // Leaf tracks the most recent append.
    assert_eq!(session.get_leaf_id().await.unwrap(), Some(id3.clone()));

    // parentId of each entry == prior leaf.
    let e1 = session.get_entry(&id1).await.unwrap().unwrap();
    let e2 = session.get_entry(&id2).await.unwrap().unwrap();
    let e3 = session.get_entry(&id3).await.unwrap().unwrap();
    assert_eq!(parent(&e1), None);
    assert_eq!(parent(&e2).as_deref(), Some(id1.as_str()));
    assert_eq!(parent(&e3).as_deref(), Some(id2.as_str()));

    // getPathToRoot is root-first.
    let path = session.get_branch(None).await.unwrap();
    let ids: Vec<String> = path.iter().map(|e| id_of(e).to_string()).collect();
    assert_eq!(ids, vec![id1, id2, id3]);
}

#[tokio::test]
async fn move_to_repoints_leaf_and_appends_branch_summary() {
    let session = in_memory_session();

    let root = session
        .append_thinking_level_change("low".to_string())
        .await
        .unwrap();
    let mid = session
        .append_model_change("anthropic".to_string(), "m".to_string())
        .await
        .unwrap();
    let _tip = session.append_active_tools_change(vec![]).await.unwrap();

    // Fork/navigate: repoint the leaf back to `root` with a branch summary.
    let branch_id = session
        .move_to(
            Some(root.clone()),
            Some(BranchSummaryInput {
                summary: "went back".to_string(),
                details: None,
                from_hook: Some(false),
            }),
        )
        .await
        .unwrap()
        .unwrap();

    // The branch_summary is now the leaf; its parent is the repoint target.
    assert_eq!(
        session.get_leaf_id().await.unwrap(),
        Some(branch_id.clone())
    );
    let bs = session.get_entry(&branch_id).await.unwrap().unwrap();
    assert_eq!(parent(&bs).as_deref(), Some(root.as_str()));

    // The new branch omits `mid`/`tip`: path is root -> branch_summary.
    let path = session.get_branch(None).await.unwrap();
    let ids: Vec<String> = path.iter().map(|e| id_of(e).to_string()).collect();
    assert_eq!(ids, vec![root.clone(), branch_id]);
    assert!(!ids.contains(&mid));
}

#[tokio::test]
async fn move_to_without_summary_only_repoints() {
    let session = in_memory_session();
    let root = session
        .append_thinking_level_change("low".to_string())
        .await
        .unwrap();
    let _tip = session
        .append_model_change("p".to_string(), "m".to_string())
        .await
        .unwrap();

    let ret = session.move_to(Some(root.clone()), None).await.unwrap();
    assert_eq!(ret, None, "no branch summary id when summary is None");
    assert_eq!(session.get_leaf_id().await.unwrap(), Some(root));
}

#[tokio::test]
async fn labels_and_session_name_round_trip() {
    let session = in_memory_session();
    let target = session
        .append_thinking_level_change("low".to_string())
        .await
        .unwrap();
    session
        .append_label(target.clone(), Some("  my-label  ".to_string()))
        .await
        .unwrap();
    assert_eq!(
        session.get_label(&target).await.unwrap().as_deref(),
        Some("my-label")
    );

    session
        .append_session_name("Line one\nLine two")
        .await
        .unwrap();
    assert_eq!(
        session.get_session_name().await.unwrap().as_deref(),
        Some("Line one Line two")
    );

    // Labeling a missing entry errors.
    assert!(session
        .append_label("nope".to_string(), Some("x".to_string()))
        .await
        .is_err());
}

// ---------------------------------------------------------------------------
// v1 legacy detection (spec §11.A)
// ---------------------------------------------------------------------------

#[test]
fn open_rejects_v1_legacy_header() {
    let dir = std::env::temp_dir().join(format!("pirust-sess-v1-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("v1.jsonl");
    // v1 header: no `version`, extra provider/modelId (spec §11.A).
    std::fs::write(
        &path,
        "{\"type\":\"session\",\"id\":\"s\",\"timestamp\":\"2023-11-14T22:13:20.000Z\",\"cwd\":\"/x\",\"provider\":\"anthropic\",\"modelId\":\"m\"}\n",
    )
    .unwrap();

    let err = match JsonlSessionStorage::open(path, fixed_clock("2023-11-14T22:13:20.000Z")) {
        Ok(_) => panic!("expected v1 rejection"),
        Err(e) => e,
    };
    assert!(
        err.message.contains("unsupported session version"),
        "got: {}",
        err.message
    );
}

#[tokio::test]
async fn jsonl_round_trips_appended_entries_to_disk() {
    let dir = std::env::temp_dir().join(format!("pirust-sess-rt-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("rt.jsonl");

    let storage = JsonlSessionStorage::create_with_source(
        path.clone(),
        JsonlCreateOptions {
            cwd: "/x".to_string(),
            session_id: "s".to_string(),
            parent_session_path: None,
            metadata: None,
        },
        fixed_clock("2023-11-14T22:13:20.000Z"),
        SeqSource::default(),
    )
    .unwrap();
    let session = Session::with_clock(storage, fixed_clock("2023-11-14T22:13:20.000Z"));
    let a = session
        .append_thinking_level_change("high".to_string())
        .await
        .unwrap();
    let b = session
        .append_model_change("anthropic".to_string(), "m".to_string())
        .await
        .unwrap();

    // Reopen the file and confirm the tree replays with the same leaf + links.
    let reopened =
        JsonlSessionStorage::open(path, fixed_clock("2023-11-14T22:13:20.000Z")).unwrap();
    assert_eq!(reopened.get_leaf_id().await.unwrap(), Some(b.clone()));
    let path_entries = reopened.get_path_to_root(Some(b.clone())).await.unwrap();
    let ids: Vec<String> = path_entries.iter().map(|e| id_of(e).to_string()).collect();
    assert_eq!(ids, vec![a, b]);
}

// --- helpers over the opaque entry enum (mirrors the crate-internal accessors) ---

fn id_of(entry: &SessionTreeEntry) -> &str {
    match entry {
        SessionTreeEntry::Message { id, .. }
        | SessionTreeEntry::ThinkingLevelChange { id, .. }
        | SessionTreeEntry::ModelChange { id, .. }
        | SessionTreeEntry::ActiveToolsChange { id, .. }
        | SessionTreeEntry::Compaction { id, .. }
        | SessionTreeEntry::BranchSummary { id, .. }
        | SessionTreeEntry::Custom { id, .. }
        | SessionTreeEntry::CustomMessage { id, .. }
        | SessionTreeEntry::Label { id, .. }
        | SessionTreeEntry::SessionInfo { id, .. }
        | SessionTreeEntry::Leaf { id, .. } => id,
    }
}

fn parent(entry: &SessionTreeEntry) -> Option<String> {
    match entry {
        SessionTreeEntry::Message { parent_id, .. }
        | SessionTreeEntry::ThinkingLevelChange { parent_id, .. }
        | SessionTreeEntry::ModelChange { parent_id, .. }
        | SessionTreeEntry::ActiveToolsChange { parent_id, .. }
        | SessionTreeEntry::Compaction { parent_id, .. }
        | SessionTreeEntry::BranchSummary { parent_id, .. }
        | SessionTreeEntry::Custom { parent_id, .. }
        | SessionTreeEntry::CustomMessage { parent_id, .. }
        | SessionTreeEntry::Label { parent_id, .. }
        | SessionTreeEntry::SessionInfo { parent_id, .. }
        | SessionTreeEntry::Leaf { parent_id, .. } => parent_id.clone(),
    }
}
