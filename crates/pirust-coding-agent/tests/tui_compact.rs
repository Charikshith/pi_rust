//! Algorithm-level proof that `/compact` actually shrinks history, with no UI and no
//! network call involved — the counterpart to `tui_delayed_provider.rs`'s three
//! `slash_compact_*` tests, which prove the UI *shows* compaction happening but stub
//! out `PrintModeSession::compact` entirely (`DelayedSession::compact` never touches
//! an `Agent`). This file drives the REAL seam:
//! `PrintModeSession::compact` -> `SingleTurnSession::compact_inner`
//! (`runtime_host.rs:256`) -> `prepare_compaction_from_messages`
//! (`pirust-agent-core/src/harness/compaction/v4.rs`) -> `agent.set_messages()`
//! (`agent.rs:360`) — and asserts on the real `Agent` message list and the real
//! persisted session entries afterward.
//!
//! Modeled on `wave6_binding.rs`'s `make_session()` (a real `SingleTurnSession` +
//! `Agent` bound with no network call, seeded via `AgentOptions.messages`) and
//! `interactive_commands.rs`'s test-module `assistant_text_message`/
//! `user_text_message` helpers (the minimal-field construction template for
//! `AssistantMessage`/`UserMessage`).

use std::sync::Arc;

use pirust_agent_core::agent::{Agent, AgentOptions};
use pirust_agent_core::harness::messages::AgentMessage;
use pirust_agent_core::types::ThinkingLevel;
use pirust_ai::providers::faux::Faux;
use pirust_ai::types::{
    AssistantContent, AssistantMessage, Message, TextContent, UserMessage, UserMessageContent,
    UserRole,
};
use pirust_coding_agent::print_mode::{CompactionReason, PrintModeSession};
use pirust_coding_agent::runtime_host::SingleTurnSession;
use pirust_coding_agent::session::SessionEnv;
use pirust_tools::create_all_tools;

fn dummy_model() -> pirust_ai::types::Model {
    // A model identity — never actually streamed in this test.
    Faux::new().get_model().clone()
}

/// Same minimal-field construction `interactive_commands.rs`'s test module uses —
/// only the fields `estimate_tokens` (`compaction/mod.rs:135-162`) and the compaction
/// machinery actually read are meaningful; everything else is a cheap default.
fn assistant_text_message(text: &str) -> AgentMessage {
    AgentMessage::Llm(Message::Assistant(AssistantMessage {
        role: Default::default(),
        content: vec![AssistantContent::Text(TextContent::new(text))],
        api: "anthropic-messages".into(),
        provider: "anthropic".into(),
        model: None,
        response_model: None,
        diagnostics: None,
        usage: pirust_ai::types::Usage {
            input: 0,
            output: 0,
            cache_read: 0,
            cache_write: 0,
            cache_write1h: None,
            reasoning: None,
            total_tokens: None,
            cost: pirust_ai::types::Cost {
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
                total: 0.0,
            },
        },
        stop_reason: pirust_ai::types::StopReason::Stop,
        timestamp: 0,
        response_id: None,
        raw_stop_reason: None,
        error_message: None,
        end_turn: None,
    }))
}

fn user_text_message(text: &str) -> AgentMessage {
    AgentMessage::Llm(Message::User(UserMessage {
        role: UserRole::User,
        content: UserMessageContent::Text(text.to_string()),
        timestamp: 0,
    }))
}

/// `n` messages alternating user/assistant, starting with user, each carrying
/// `chars_each` ASCII characters of text. `estimate_tokens` is `ceil(chars / 4)`
/// (`compaction/mod.rs:161`), so `chars_each` directly controls the per-message
/// token cost the cut-point walk in `find_cut_point` (`v4.rs:119`) accumulates.
fn alternating_messages(n: usize, chars_each: usize) -> Vec<AgentMessage> {
    let body = "x".repeat(chars_each);
    (0..n)
        .map(|i| {
            if i % 2 == 0 {
                user_text_message(&body)
            } else {
                assistant_text_message(&body)
            }
        })
        .collect()
}

fn make_session(messages: Vec<AgentMessage>) -> (Arc<SingleTurnSession>, Agent) {
    let tool_registry = create_all_tools("/proj", None);
    let agent = Agent::new(AgentOptions {
        system_prompt: "test".into(),
        model: dummy_model(),
        thinking_level: ThinkingLevel::Off,
        tools: tool_registry
            .iter()
            .map(|(_, t)| t.clone())
            .collect::<Vec<_>>(),
        messages,
        convert_to_llm: None,
        transform_context: None,
        stream_fn: None,
        get_api_key: None,
        before_tool_call: None,
        after_tool_call: None,
        steering_mode: pirust_agent_core::types::QueueMode::OneAtATime,
        follow_up_mode: pirust_agent_core::types::QueueMode::OneAtATime,
        session_id: None,
        tool_execution: pirust_agent_core::types::ToolExecutionMode::Parallel,
    });
    let env = SessionEnv::new(
        pirust_coding_agent::config::ConfigEnv::from_process_env(),
        "/proj",
    );
    let manager = env.in_memory(Some("/proj"), None).unwrap();
    let session = SingleTurnSession::new(agent.clone(), manager, tool_registry);
    (session, agent)
}

/// End-to-end: 11 messages (6 user, 5 assistant, alternating, starting on user) each
/// carrying 28,000 ASCII characters (7,000 estimated tokens). Walking backward from
/// the end against `DEFAULT_COMPACTION_SETTINGS.keep_recent_tokens == 20_000`
/// (`compaction/mod.rs:73-77`), the accumulated-token budget is exceeded exactly at
/// index 8 (7,000 + 7,000 + 7,000 == 21,000 >= 20,000, contributed by indices 10, 9,
/// 8) — and index 8 is a **user** message (even index), so `find_cut_point` reports
/// `is_split_turn == false` and the cut is clean: no turn-prefix message is silently
/// dropped (see this file's module doc — `turn_prefix_messages` is intentionally
/// unused by both `SingleTurnSession::compact_inner` and the RPC harness's own
/// `AgentHarness::compact_inner`, so a split-turn cut would lose that message's
/// content entirely; this test deliberately avoids that case to keep the assertions
/// unambiguous).
#[tokio::test]
async fn compact_shrinks_history_and_persists_a_compaction_entry() {
    let seed = alternating_messages(11, 28_000);
    let (session, agent) = make_session(seed.clone());

    assert_eq!(
        agent.messages().len(),
        11,
        "sanity: all 11 seeded messages present"
    );

    session
        .compact(CompactionReason::Manual)
        .await
        .expect("compaction should succeed with a real cut point");

    let after = agent.messages();
    // 1 synthesized `CompactionSummary` + retained tail (indices 8, 9, 10 == 3 messages).
    assert_eq!(
        after.len(),
        4,
        "history shrank to summary + 3-message retained tail"
    );
    assert!(
        matches!(after[0], AgentMessage::CompactionSummary(_)),
        "first message after compaction is the synthesized summary, got {:?}",
        after[0]
    );
    // The retained tail is a value-for-value suffix of the original messages.
    assert_eq!(after[1..], seed[8..]);

    // The compaction was actually persisted, not just applied in-memory.
    let entries = session.entries_for_test();
    let compaction_entries: Vec<_> = entries
        .iter()
        .filter(|e| e.get("type").and_then(|t| t.as_str()) == Some("compaction"))
        .collect();
    assert_eq!(
        compaction_entries.len(),
        1,
        "exactly one compaction entry persisted"
    );

    // `firstKeptEntryId` must point at the on-disk id of the message that is now
    // `after[1]` (the first retained-tail message) — the 9th persisted "message"
    // entry (0-indexed: index 8).
    let message_entries: Vec<_> = entries
        .iter()
        .filter(|e| e.get("type").and_then(|t| t.as_str()) == Some("message"))
        .collect();
    assert_eq!(
        message_entries.len(),
        11,
        "all 11 original messages were persisted before compaction cut them"
    );
    let expected_id = message_entries[8]
        .get("id")
        .and_then(|v| v.as_str())
        .expect("message entry has an id");
    let first_kept_entry_id = compaction_entries[0]
        .get("firstKeptEntryId")
        .and_then(|v| v.as_str())
        .expect("compaction entry has firstKeptEntryId");
    assert_eq!(first_kept_entry_id, expected_id);
}

/// `prepare_compaction`/`prepare_compaction_from_messages` return `Ok(None)` when
/// `path_entries.is_empty()` (`v4.rs:220`) — an agent with no history has nothing to
/// summarize. `compact_inner` maps that `None` to a plain error, not a silent no-op
/// success, so a caller (the TUI's `run_compact`/`finish_compaction`,
/// `interactive_mode.rs`) can distinguish "nothing to do" from "it worked."
#[tokio::test]
async fn compact_reports_an_error_when_there_is_nothing_to_compact() {
    let (session, agent) = make_session(Vec::new());
    assert_eq!(agent.messages().len(), 0);

    let result = session.compact(CompactionReason::Manual).await;
    assert_eq!(result, Err("Nothing to compact".to_string()));
    assert_eq!(agent.messages().len(), 0, "no messages were touched");
    assert!(
        session
            .entries_for_test()
            .iter()
            .all(|e| e.get("type").and_then(|t| t.as_str()) != Some("compaction")),
        "no compaction entry was persisted on failure"
    );
}
