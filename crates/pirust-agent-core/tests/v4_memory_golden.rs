//! Golden replay for the v4 in-memory session layer against
//! `tests/fixtures/pi/agent/v4/memory.cases.jsonl` (captured from real Pi
//! 0.84.2's `memory.ts` + `context.ts` + `session.ts`).
//!
//! The oracle drove Pi's real `InMemorySessionStorage`/`InMemorySessionRepo`/
//! `Session` and the context builders directly (no FileSystem, no bytes) and
//! recorded the mutation/replay contract + context projection. Timestamps and
//! `createdAt` are wall-clock in Pi (normalized to 0 in the fixture) and
//! fork leaf ids are uuidv7 (normalized to `<UUID>`). The Rust port must
//! reproduce the same names, seqs, errors, and context messages.

use std::sync::Arc;

use pirust_agent_core::harness::messages::AgentMessage;
use pirust_agent_core::harness::session::v4::context::{
    build_session_context, SessionContextBuildOptions,
};
use pirust_agent_core::harness::session::v4::memory::{
    InMemorySessionRepo, InMemorySessionStorage,
};
use pirust_agent_core::harness::session::v4::session::Session;
use pirust_agent_core::harness::session::v4::types::{
    Entry, ForkOptions, NewOperationStartedRecord, NewRecord, OperationIntent,
    SessionCreateOptions, SessionMetadata, SessionRepo, SessionStorage,
};
use pirust_agent_core::harness::types::SessionErrorCode;

// -- deterministic id generator (mirrors the oracle's SeqIds) ---------------
struct SeqIds {
    n: std::sync::atomic::AtomicI64,
}
impl Default for SeqIds {
    fn default() -> Self {
        Self {
            n: std::sync::atomic::AtomicI64::new(0),
        }
    }
}
impl pirust_agent_core::harness::session::v4::session::IdGenerator for SeqIds {
    fn next(&self) -> String {
        let id = format!(
            "id-{}",
            self.n.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        id
    }
}

fn user(text: &str) -> AgentMessage {
    serde_json::from_value(serde_json::json!({
        "role": "user",
        "content": [{ "type": "text", "text": text }],
        "timestamp": 0,
    }))
    .unwrap()
}

/// Normalize a serde value: timestamps/createdAt → 0, uuidv7 → `<UUID>`.
fn normalize(v: &serde_json::Value) -> serde_json::Value {
    fn walk(v: &serde_json::Value) -> serde_json::Value {
        match v {
            serde_json::Value::Number(n) => serde_json::Value::Number(n.clone()),
            serde_json::Value::String(s) => {
                let is_uuid = s.len() == 36
                    && s.as_bytes()[8] == b'-'
                    && s.as_bytes()[13] == b'-'
                    && s.as_bytes()[14] == b'7'
                    && s.as_bytes()[18] == b'-'
                    && s.as_bytes()[23] == b'-';
                if is_uuid {
                    serde_json::Value::String("<UUID>".to_string())
                } else {
                    serde_json::Value::String(s.clone())
                }
            }
            serde_json::Value::Array(items) => {
                serde_json::Value::Array(items.iter().map(walk).collect())
            }
            serde_json::Value::Object(map) => {
                let mut out = serde_json::Map::new();
                for (k, val) in map {
                    let v = walk(val);
                    let v = if (k == "timestamp" || k == "createdAt") && v.is_number() {
                        serde_json::Value::Number(0.into())
                    } else {
                        v
                    };
                    out.insert(k.clone(), v);
                }
                serde_json::Value::Object(out)
            }
            other => other.clone(),
        }
    }
    walk(v)
}

fn session_with_storage() -> Session<InMemorySessionStorage> {
    let storage = InMemorySessionStorage::new(SessionMetadata {
        id: "mem".to_string(),
        created_at: 1000,
        parent_session_id: Some("parent".to_string()),
    });
    Session::with_id_generator(Arc::new(storage), Arc::new(SeqIds::default()))
}

// ---------------------------------------------------------------------------

#[test]
fn memory_storage_mutation_replay_contract() {
    let session = session_with_storage();
    let out = session.storage();

    // lanes0 — `main` seeded with null leaf.
    assert_eq!(
        out.get_lanes().unwrap(),
        vec![
            pirust_agent_core::harness::session::v4::types::LanePointer {
                lane: "main".to_string(),
                leaf_id: None,
            }
        ]
    );

    out.create_lane("thread", None).unwrap();
    let msg_id = session.view("main").append_message(user("hi")).unwrap();
    assert_eq!(msg_id, "id-0");

    // lanes: main → id-0, thread → null
    let lanes = out.get_lanes().unwrap();
    assert_eq!(lanes[0].lane, "main");
    assert_eq!(lanes[0].leaf_id.as_deref(), Some("id-0"));
    assert_eq!(lanes[1].lane, "thread");
    assert_eq!(lanes[1].leaf_id, None);

    // entry: parentId null, seq 2, timestamp normalized to 0
    let entry = out.get_entry(&msg_id).unwrap().unwrap();
    let entry_json = normalize(&serde_json::to_value(entry).unwrap());
    let expected_entry = serde_json::json!({
        "type": "message",
        "id": "id-0",
        "message": { "role": "user", "content": [{ "type": "text", "text": "hi" }], "timestamp": 0 },
        "parentId": null,
        "seq": 2,
        "timestamp": 0,
    });
    assert_eq!(entry_json, expected_entry);

    // record
    let record = out
        .append_record(&NewRecord::OperationStarted(NewOperationStartedRecord {
            id: "run".to_string(),
            lane: "thread".to_string(),
            source_leaf_id: None,
            intent: OperationIntent::Run {
                original_prompt: vec![],
                initial_messages: vec![],
                system_prompt_override: None,
                resume_data: None,
            },
        }))
        .unwrap();
    assert_eq!(record.id(), "run");
    assert_eq!(record.seq(), 3);

    let records = out
        .find_records(
            &pirust_agent_core::harness::session::v4::types::RecordQuery {
                record_type: Some("operation_started".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].id(), "run");

    // global facts
    out.set_name(Some("Example")).unwrap();
    assert_eq!(out.get_name().unwrap().as_deref(), Some("Example"));
    out.set_label(&msg_id, Some("checkpoint")).unwrap();
    assert_eq!(
        out.get_label(&msg_id).unwrap().as_deref(),
        Some("checkpoint")
    );

    // log: lane(1), entry(2), record(3), fact name(4), fact label(5)
    let log = out.get_log(&Default::default()).unwrap();
    use pirust_agent_core::harness::session::v4::types::LogItem;
    let log_entries: Vec<(String, i64)> = log
        .iter()
        .map(|item| match item {
            LogItem::Lane { seq, .. } => ("lane".to_string(), *seq),
            LogItem::Entry { seq, .. } => ("entry".to_string(), *seq),
            LogItem::Record { seq, .. } => ("record".to_string(), *seq),
            LogItem::FactName { seq, .. } => ("fact".to_string(), *seq),
            LogItem::FactLabel { seq, .. } => ("fact".to_string(), *seq),
        })
        .collect();
    assert_eq!(
        log_entries,
        vec![
            ("lane".to_string(), 1),
            ("entry".to_string(), 2),
            ("record".to_string(), 3),
            ("fact".to_string(), 4),
            ("fact".to_string(), 5),
        ]
    );
    // fact kinds + target
    let facts: Vec<&LogItem> = log
        .iter()
        .filter(|item| matches!(item, LogItem::FactName { .. } | LogItem::FactLabel { .. }))
        .collect();
    assert_eq!(facts.len(), 2);
    if let LogItem::FactName { name, .. } = facts[0] {
        assert_eq!(name.as_deref(), Some("Example"));
    }
    if let LogItem::FactLabel {
        target_id, label, ..
    } = facts[1]
    {
        assert_eq!(target_id, "id-0");
        assert_eq!(label.as_deref(), Some("checkpoint"));
    }

    // stats
    let stats = out.get_stats().unwrap();
    assert_eq!(stats.message_count, 1);
    assert_eq!(stats.cached_tokens, 0);
    assert_eq!(stats.uncached_tokens, 0);
    assert_eq!(stats.total_tokens, 0);
    assert_eq!(stats.cost_total, 0.0);
}

#[test]
fn memory_repo_create_open_list_delete() {
    let repo = InMemorySessionRepo::new();
    let created = repo
        .create(SessionCreateOptions {
            id: Some("a".to_string()),
            ..Default::default()
        })
        .unwrap();
    let meta = created.get_metadata().unwrap();
    assert_eq!(meta.id, "a");
    // oracle normalizes createdAt to 0; Rust uses now_ms — check the id only.
    created.storage().set_name(Some("A")).unwrap();

    // duplicate
    let err = repo
        .create(SessionCreateOptions {
            id: Some("a".to_string()),
            ..Default::default()
        })
        .unwrap_err();
    assert_eq!(err.code, SessionErrorCode::AlreadyExists);
    assert_eq!(err.message, "Session already exists: a");

    let list = repo.list(None).unwrap();
    assert_eq!(
        list.iter().map(|m| m.id.clone()).collect::<Vec<_>>(),
        vec!["a"]
    );

    let opened = repo.open(list[0].clone()).unwrap();
    assert_eq!(opened.storage().get_name().unwrap().as_deref(), Some("A"));

    repo.delete(list[0].clone()).unwrap();
    assert!(repo.list(None).unwrap().is_empty());

    let err = repo
        .open(SessionMetadata {
            id: "a".to_string(),
            created_at: 0,
            parent_session_id: None,
        })
        .unwrap_err();
    assert_eq!(err.code, SessionErrorCode::NotFound);
    assert_eq!(err.message, "Session not found: a");
}

#[test]
fn memory_repo_fork_tree_keeps_entries() {
    let repo = InMemorySessionRepo::new();
    let source = repo
        .create(SessionCreateOptions {
            id: Some("source".to_string()),
            ..Default::default()
        })
        .unwrap();
    source.append_message(user("one")).unwrap();
    source.append_message(user("two")).unwrap();
    let source_meta = source.get_metadata().unwrap();

    let fork = repo
        .fork(
            source_meta,
            pirust_agent_core::harness::session::v4::types::MemoryForkOptions {
                fork: Some(ForkOptions::Tree),
                id: Some("forked".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
    let f_meta = fork.get_metadata().unwrap();
    assert_eq!(f_meta.id, "forked");
    assert_eq!(f_meta.parent_session_id.as_deref(), Some("source"));
    let entries = fork.storage().find_entries(&Default::default()).unwrap();
    assert_eq!(entries.len(), 2);
    let lanes = fork.storage().get_lanes().unwrap();
    assert_eq!(lanes.len(), 1);
    assert_eq!(lanes[0].lane, "main");
    assert!(lanes[0].leaf_id.is_some());
}

#[test]
fn context_path_derives_state_and_collapses_compaction() {
    use pirust_agent_core::harness::session::v4::context::SessionContextBuildOptions;
    let path = vec![
        entry(
            "message",
            "n1",
            1,
            None,
            serde_json::json!({"message": user("hi")}),
        ),
        entry(
            "thinking_level_change",
            "t1",
            2,
            Some("n1"),
            serde_json::json!({"thinkingLevel": "high"}),
        ),
        entry(
            "model_change",
            "m1",
            3,
            Some("t1"),
            serde_json::json!({"provider": "anthropic", "modelId": "claude-3-5"}),
        ),
        entry(
            "active_tools_change",
            "at1",
            4,
            Some("m1"),
            serde_json::json!({"activeToolNames": ["bash", "read"]}),
        ),
        entry(
            "message",
            "n2",
            5,
            Some("at1"),
            serde_json::json!({"message": user("interim")}),
        ), // replaced by compaction
        entry(
            "compaction",
            "c1",
            6,
            Some("n2"),
            serde_json::json!({"summary": "sum", "retainedTail": [user("tail")], "tokensBefore": 10}),
        ),
        entry(
            "message",
            "n3",
            7,
            Some("c1"),
            serde_json::json!({"message": user("after")}),
        ),
        entry(
            "custom",
            "cu1",
            8,
            Some("n3"),
            serde_json::json!({"customType": "note", "data": {"v": 1}}),
        ),
    ];

    let ctx = build_session_context(&path, &SessionContextBuildOptions::default());
    assert_eq!(ctx.thinking_level, "high");
    assert_eq!(ctx.model.as_ref().unwrap().provider, "anthropic");
    assert_eq!(ctx.model.as_ref().unwrap().model_id, "claude-3-5");
    assert_eq!(
        ctx.active_tool_names.as_ref().unwrap(),
        &vec!["bash".to_string(), "read".to_string()]
    );
    // messages: compaction summary + retainedTail + after (hi + assistant dropped)
    let messages = ctx.messages;
    let msgs = normalize(&serde_json::to_value(&messages).unwrap());
    let msgs = msgs.as_array().unwrap();
    assert_eq!(msgs.len(), 3);
    assert_eq!(msgs[0]["role"], "compactionSummary");
    assert_eq!(msgs[0]["summary"], "sum");
    assert_eq!(msgs[0]["tokensBefore"], 10);
    assert_eq!(msgs[1]["content"][0]["text"], "tail");
    assert_eq!(msgs[2]["content"][0]["text"], "after");
}

fn entry(
    entry_type: &str,
    id: &str,
    seq: i64,
    parent: Option<&str>,
    payload: serde_json::Value,
) -> Entry {
    // Build a typed Entry from a JSON fragment (mirrors the oracle's path array).
    let mut map = serde_json::Map::new();
    map.insert(
        "type".to_string(),
        serde_json::Value::String(entry_type.to_string()),
    );
    map.insert("id".to_string(), serde_json::Value::String(id.to_string()));
    map.insert("seq".to_string(), serde_json::Value::Number(seq.into()));
    map.insert(
        "parentId".to_string(),
        parent
            .map(|p| serde_json::Value::String(p.to_string()))
            .unwrap_or(serde_json::Value::Null),
    );
    map.insert("timestamp".to_string(), serde_json::Value::Number(0.into()));
    if let Some(obj) = payload.as_object() {
        for (k, v) in obj {
            map.insert(k.clone(), v.clone());
        }
    }
    serde_json::from_value(serde_json::Value::Object(map)).unwrap()
}

#[test]
fn context_deferred_assistant_message_is_dropped() {
    // A deferred assistant stop reason produces no context message. The Rust
    // `StopReason` enum lacks "deferred" (wire value deferred to adapter); the
    // port checks `rawStopReason == "deferred"`.
    let path = vec![entry(
        "message",
        "d1",
        1,
        None,
        serde_json::json!({
            "message": {
                "role": "assistant",
                "content": [{ "type": "text", "text": "ok" }],
                "api": "anthropic",
                "provider": "x",
                "model": "m",
                "usage": { "input": 1, "output": 1, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 2, "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 } },
                "stopReason": "stop",
                "rawStopReason": "deferred",
                "timestamp": 0,
            }
        }),
    )];
    let ctx = build_session_context(&path, &SessionContextBuildOptions::default());
    assert!(ctx.messages.is_empty());
}
