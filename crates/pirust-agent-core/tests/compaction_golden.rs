//! Deterministic-compaction oracle (§4.5, §11.B, §12).
//!
//! Every expected value here is cross-referenced to Pi's captured numbers in
//! `tests/fixtures/pi/agent/compaction.json`, which in turn come from
//! `packages/agent/test/harness/compaction.test.ts`. A divergence means the Rust
//! port drifted (UTF-16 counting, index math) — the fix is the port, never the
//! assertion.

use pirust_agent_core::harness::compaction::{
    calculate_context_tokens, estimate_context_tokens, estimate_tokens, find_cut_point,
    find_turn_start_index, prepare_compaction, CompactionSettings, DEFAULT_COMPACTION_SETTINGS,
};
use pirust_agent_core::harness::messages::{
    create_branch_summary_message, create_compaction_summary_message, create_custom_message,
    AgentMessage, BashExecutionMessage, BashExecutionRole,
};
use pirust_agent_core::harness::types::SessionTreeEntry;
use pirust_ai::types::{
    Api, AssistantContent, AssistantMessage, AssistantRole, Cost, ImageContent, ImageTag, Message,
    ProviderId, StopReason, TextContent, ThinkingContent, ThinkingTag, ToolCall, ToolCallTag,
    ToolResultMessage, ToolResultRole, Usage, UserContent, UserMessage, UserMessageContent,
    UserRole,
};

// --------------------------------------------------------------------------
// Message constructors (mirror compaction.test.ts:46-76).
// --------------------------------------------------------------------------

fn mock_usage(input: u64, output: u64, cache_read: u64, cache_write: u64) -> Usage {
    Usage {
        input,
        output,
        cache_read,
        cache_write,
        total_tokens: Some(input + output + cache_read + cache_write),
        cost: Cost {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
            total: 0.0,
        },
        cache_write1h: None,
        reasoning: None,
    }
}

/// `createUserMessage`: content is a single text block (compaction.test.ts:57-63).
fn user_msg(text: &str) -> AgentMessage {
    AgentMessage::Llm(Message::User(UserMessage {
        role: UserRole::User,
        content: UserMessageContent::Blocks(vec![UserContent::Text(TextContent::new(text))]),
        timestamp: 0,
    }))
}

fn assistant_raw(
    content: Vec<AssistantContent>,
    usage: Usage,
    stop: StopReason,
) -> AssistantMessage {
    AssistantMessage {
        role: AssistantRole::Assistant,
        content,
        api: Api::from("anthropic-messages"),
        provider: ProviderId::from("anthropic"),
        model: Some("claude-sonnet-4-5".into()),
        response_model: None,
        diagnostics: None,
        usage,
        stop_reason: stop,
        timestamp: 0,
        response_id: None,
        raw_stop_reason: None,
        error_message: None,
        end_turn: None,
    }
}

/// `createAssistantMessage` with a single text block (compaction.test.ts:65-76).
fn assistant_msg(text: &str, usage: Usage) -> AgentMessage {
    AgentMessage::Llm(Message::Assistant(assistant_raw(
        vec![AssistantContent::Text(TextContent::new(text))],
        usage,
        StopReason::Stop,
    )))
}

fn role_of(m: &AgentMessage) -> &'static str {
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

// --------------------------------------------------------------------------
// Entry constructors.
// --------------------------------------------------------------------------

fn iso() -> String {
    "2026-01-01T00:00:00.000Z".into()
}

fn message_entry(id: &str, parent: Option<&str>, message: AgentMessage) -> SessionTreeEntry {
    SessionTreeEntry::Message {
        id: id.into(),
        parent_id: parent.map(str::to_string),
        timestamp: iso(),
        message,
    }
}

fn thinking_entry(id: &str) -> SessionTreeEntry {
    SessionTreeEntry::ThinkingLevelChange {
        id: id.into(),
        parent_id: None,
        timestamp: iso(),
        thinking_level: "high".into(),
    }
}

fn model_change_entry(id: &str) -> SessionTreeEntry {
    SessionTreeEntry::ModelChange {
        id: id.into(),
        parent_id: None,
        timestamp: iso(),
        provider: "openai".into(),
        model_id: "gpt-4".into(),
    }
}

fn branch_summary_entry(id: &str) -> SessionTreeEntry {
    SessionTreeEntry::BranchSummary {
        id: id.into(),
        parent_id: None,
        timestamp: iso(),
        from_id: "branch".into(),
        summary: "branch summary".into(),
        details: None,
        from_hook: None,
    }
}

fn custom_message_entry(id: &str) -> SessionTreeEntry {
    SessionTreeEntry::CustomMessage {
        id: id.into(),
        parent_id: None,
        timestamp: iso(),
        custom_type: "note".into(),
        content: UserMessageContent::Text("custom content".into()),
        display: true,
        details: None,
    }
}

fn compaction_entry(id: &str, summary: &str, first_kept: &str) -> SessionTreeEntry {
    SessionTreeEntry::Compaction {
        id: id.into(),
        parent_id: None,
        timestamp: iso(),
        summary: summary.into(),
        first_kept_entry_id: first_kept.into(),
        tokens_before: 1234,
        details: None,
        from_hook: None,
    }
}

// ==========================================================================
// estimateTokens per role (fixture: estimateTokensByRole).
// ==========================================================================

#[test]
fn estimate_tokens_per_role() {
    // user "plain user" (string content) -> ceil(10/4) = 3.
    let user = AgentMessage::Llm(Message::User(UserMessage {
        role: UserRole::User,
        content: UserMessageContent::Text("plain user".into()),
        timestamp: 0,
    }));
    assert_eq!(estimate_tokens(&user), 3);

    // assistant: thinking "thinking"(8) + toolCall name "read"(4) +
    // JSON.stringify({"path":"file.ts"})(18) = 30 -> ceil(30/4) = 8.
    let mut args = serde_json::Map::new();
    args.insert("path".into(), serde_json::Value::String("file.ts".into()));
    let assistant = AgentMessage::Llm(Message::Assistant(assistant_raw(
        vec![
            AssistantContent::Thinking(ThinkingContent {
                kind: ThinkingTag::Thinking,
                thinking: "thinking".into(),
                thinking_signature: None,
                redacted: None,
            }),
            AssistantContent::ToolCall(ToolCall {
                kind: ToolCallTag::ToolCall,
                id: "call-1".into(),
                name: "read".into(),
                arguments: args,
                thought_signature: None,
                namespace: None,
                partial_json: None,
            }),
        ],
        mock_usage(100, 50, 0, 0),
        StopReason::Stop,
    )));
    assert_eq!(estimate_tokens(&assistant), 8);

    // custom string "custom text"(11) -> ceil(11/4) = 3.
    let custom = AgentMessage::Custom(create_custom_message(
        "note".into(),
        UserMessageContent::Text("custom text".into()),
        true,
        None,
        0,
    ));
    assert_eq!(estimate_tokens(&custom), 3);

    // toolResult text "tool text"(9) + image(4800) = 4809 -> ceil(4809/4) = 1203.
    let tool_result = AgentMessage::Llm(Message::ToolResult(ToolResultMessage {
        role: ToolResultRole::ToolResult,
        tool_call_id: "call-1".into(),
        tool_name: "read".into(),
        content: vec![
            UserContent::Text(TextContent::new("tool text")),
            UserContent::Image(ImageContent {
                kind: ImageTag::Image,
                data: "abc".into(),
                mime_type: "image/png".into(),
            }),
        ],
        details: None,
        added_tool_names: None,
        is_error: false,
        timestamp: 0,
    }));
    assert_eq!(estimate_tokens(&tool_result), 1203);

    // bashExecution "npm run check"(13) + "ok"(2) = 15 -> ceil(15/4) = 4.
    let bash = AgentMessage::BashExecution(BashExecutionMessage {
        role: BashExecutionRole::BashExecution,
        command: "npm run check".into(),
        output: "ok".into(),
        exit_code: Some(0),
        cancelled: false,
        truncated: false,
        full_output_path: None,
        timestamp: 0,
        exclude_from_context: None,
    });
    assert_eq!(estimate_tokens(&bash), 4);

    // branchSummary "branch"(6) -> ceil(6/4) = 2.
    let branch = AgentMessage::BranchSummary(create_branch_summary_message(
        "branch".into(),
        "x".into(),
        0,
    ));
    assert_eq!(estimate_tokens(&branch), 2);

    // compactionSummary "compact"(7) -> ceil(7/4) = 2.
    let compaction = AgentMessage::CompactionSummary(create_compaction_summary_message(
        "compact".into(),
        123,
        0,
    ));
    assert_eq!(estimate_tokens(&compaction), 2);

    // NOTE: the fixture's "unknown role -> 0" (compaction.test.ts:294) is a cast of
    // an out-of-domain object. Rust's closed `AgentMessage` enum makes such a role
    // unrepresentable, so the zero fallthrough is statically eliminated (see
    // estimate_tokens docs). All seven representable roles are covered above.
}

// ==========================================================================
// calculateContextTokens / shouldCompact (fixture: calculateContextTokens, shouldCompact).
// ==========================================================================

#[test]
fn calculate_context_tokens_matches() {
    assert_eq!(
        calculate_context_tokens(&mock_usage(1000, 500, 200, 100)),
        1800
    );
    assert_eq!(calculate_context_tokens(&mock_usage(0, 0, 0, 0)), 0);
}

#[test]
fn should_compact_matches() {
    let settings = CompactionSettings {
        enabled: true,
        reserve_tokens: 10000,
        keep_recent_tokens: 20000,
    };
    assert!(pirust_agent_core::harness::compaction::should_compact(
        95000, 100000, &settings
    ));
    assert!(!pirust_agent_core::harness::compaction::should_compact(
        89000, 100000, &settings
    ));
    let disabled = CompactionSettings {
        enabled: false,
        ..settings.clone()
    };
    assert!(!pirust_agent_core::harness::compaction::should_compact(
        95000, 100000, &disabled
    ));
}

// ==========================================================================
// estimateContextTokens (fixture: estimateContextTokens).
// ==========================================================================

#[test]
fn estimate_context_tokens_cases() {
    // No usage -> lastUsageIndex null.
    let est = estimate_context_tokens(&[user_msg("no usage")]);
    assert_eq!(est.last_usage_index, None);

    // [assistant(usage 20), user "tail"(1)] -> usage 20 / idx 0 / trailing 1 / tokens 21.
    let assistant = assistant_msg("assistant", mock_usage(10, 5, 3, 2));
    let est = estimate_context_tokens(&[assistant.clone(), user_msg("tail")]);
    assert_eq!(est.usage_tokens, 20);
    assert_eq!(est.last_usage_index, Some(0));
    assert_eq!(est.trailing_tokens, 1);
    assert_eq!(est.tokens, 21);

    // 4-msg: last assistant has usage(0,0) (skipped); anchor is idx 1 (usage 20).
    // trailing = user "continue"(8->2) + assistant "Partial thinking"(16->4) = 6.
    let est = estimate_context_tokens(&[
        user_msg("Hello"),
        assistant,
        user_msg("continue"),
        assistant_msg("Partial thinking", mock_usage(0, 0, 0, 0)),
    ]);
    assert_eq!(est.usage_tokens, 20);
    assert_eq!(est.last_usage_index, Some(1));
    assert_eq!(est.trailing_tokens, 6);
    assert_eq!(est.tokens, 26);
}

// ==========================================================================
// findTurnStartIndex / findCutPoint edge cases (fixture: cutPoints).
// ==========================================================================

#[test]
fn find_turn_start_index_cases() {
    let thinking = thinking_entry("t");
    let branch = branch_summary_entry("b");
    let custom = custom_message_entry("c");
    let model = model_change_entry("m");

    assert_eq!(find_turn_start_index(&[thinking.clone(), branch], 1, 0), 1);
    assert_eq!(find_turn_start_index(&[thinking.clone(), custom], 1, 0), 1);
    assert_eq!(find_turn_start_index(&[thinking, model], 1, 0), -1);
}

#[test]
fn find_cut_point_cases() {
    // [thinking, modelChange], 0..2, keep 1 -> no valid cut points.
    let cut = find_cut_point(&[thinking_entry("t"), model_change_entry("m")], 0, 2, 1);
    assert_eq!(cut.first_kept_entry_index, 0);
    assert_eq!(cut.turn_start_index, -1);
    assert!(!cut.is_split_turn);

    // [thinking, branchSummary, customMessage], 0..3, keep 1 -> firstKeptEntryIndex 0.
    let cut = find_cut_point(
        &[
            thinking_entry("t"),
            branch_summary_entry("b"),
            custom_message_entry("c"),
        ],
        0,
        3,
        1,
    );
    assert_eq!(cut.first_kept_entry_index, 0);

    // [toolResult], 0..1, keep 1 -> no valid cut points (toolResult excluded).
    let tool_result = message_entry(
        "tr",
        None,
        AgentMessage::Llm(Message::ToolResult(ToolResultMessage {
            role: ToolResultRole::ToolResult,
            tool_call_id: "call-1".into(),
            tool_name: "read".into(),
            content: vec![UserContent::Text(TextContent::new("tool output"))],
            details: None,
            added_tool_names: None,
            is_error: false,
            timestamp: 0,
        })),
    );
    let cut = find_cut_point(&[tool_result], 0, 1, 1);
    assert_eq!(cut.first_kept_entry_index, 0);
    assert_eq!(cut.turn_start_index, -1);
    assert!(!cut.is_split_turn);

    // [user, compaction, assistant], 0..3, keep 1 -> firstKeptEntryIndex 2.
    let entries = [
        message_entry("entry-0", None, user_msg("user")),
        compaction_entry("entry-1", "summary", "entry-0"),
        message_entry(
            "entry-2",
            Some("entry-1"),
            assistant_msg("assistant", mock_usage(100, 50, 0, 0)),
        ),
    ];
    let cut = find_cut_point(&entries, 0, 3, 1);
    assert_eq!(cut.first_kept_entry_index, 2);
}

// ==========================================================================
// prepareCompaction (fixture: prepareCompaction + noopCases).
// ==========================================================================

#[test]
fn prepare_compaction_previous_summary() {
    // Entries mirror compaction.test.ts:351-365 (ids entry-0..entry-6).
    let entries = [
        message_entry("entry-0", None, user_msg("user msg 1")),
        message_entry(
            "entry-1",
            Some("entry-0"),
            assistant_msg("assistant msg 1", mock_usage(100, 50, 0, 0)),
        ),
        message_entry("entry-2", Some("entry-1"), user_msg("user msg 2")),
        message_entry(
            "entry-3",
            Some("entry-2"),
            assistant_msg("assistant msg 2", mock_usage(5000, 1000, 0, 0)),
        ),
        compaction_entry("entry-4", "First summary", "entry-2"),
        message_entry("entry-5", Some("entry-4"), user_msg("user msg 3")),
        message_entry(
            "entry-6",
            Some("entry-5"),
            assistant_msg("assistant msg 3", mock_usage(8000, 2000, 0, 0)),
        ),
    ];

    let prep = prepare_compaction(&entries, &DEFAULT_COMPACTION_SETTINGS)
        .expect("prepare_compaction ok")
        .expect("preparation present");

    assert_eq!(prep.previous_summary.as_deref(), Some("First summary"));
    assert!(!prep.first_kept_entry_id.is_empty());
    assert_eq!(prep.first_kept_entry_id, "entry-2");
    assert!(!prep.is_split_turn);
    // tokensBefore = estimateContextTokens(buildSessionContext(...).messages).tokens.
    // Collapsed context ends at assistant msg 3 (usage 10000) -> tokens 10000.
    assert_eq!(prep.tokens_before, 10000);
    // historyEnd == firstKeptEntryIndex (boundaryStart) -> no history to summarize.
    let roles: Vec<&str> = prep.messages_to_summarize.iter().map(role_of).collect();
    assert!(roles.is_empty());
}

#[test]
fn prepare_compaction_split_turn_custom_and_branch() {
    // Mirrors compaction.test.ts:395-424: messagesToSummarize roles ["branchSummary","custom"].
    let entries = [
        branch_summary_entry("entry-0"),
        custom_message_entry("entry-1"),
        message_entry("entry-2", Some("entry-1"), user_msg("keep")),
        message_entry(
            "entry-3",
            Some("entry-2"),
            assistant_msg("assistant", mock_usage(100, 50, 0, 0)),
        ),
    ];
    let settings = CompactionSettings {
        enabled: true,
        reserve_tokens: 100,
        keep_recent_tokens: 1,
    };
    let prep = prepare_compaction(&entries, &settings)
        .expect("prepare_compaction ok")
        .expect("preparation present");

    assert!(prep.is_split_turn);
    let history: Vec<&str> = prep.messages_to_summarize.iter().map(role_of).collect();
    assert_eq!(history, vec!["branchSummary", "custom"]);
    let prefix: Vec<&str> = prep.turn_prefix_messages.iter().map(role_of).collect();
    assert_eq!(prefix, vec!["user"]);
}

#[test]
fn prepare_compaction_noop_cases() {
    // Last entry is a compaction -> None.
    let compaction = compaction_entry("entry-0", "already compacted", "entry-keep");
    let prep = prepare_compaction(&[compaction], &DEFAULT_COMPACTION_SETTINGS)
        .expect("prepare_compaction ok");
    assert!(prep.is_none());

    // Empty path -> None.
    let prep =
        prepare_compaction(&[], &DEFAULT_COMPACTION_SETTINGS).expect("prepare_compaction ok");
    assert!(prep.is_none());
}
