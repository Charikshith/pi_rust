//! feat-012 Wave 1 goldens: replay the RPC protocol tapes captured from REAL
//! `pi` (`runRpcMode`, stub runtime host) by `scripts/gen-rpc-oracle.mjs`.
//!
//! What each assertion proves:
//! - REQUESTS parse into our typed [`RpcCommand`] exactly as Pi's TS union
//!   would discriminate them (including the invalid-JSON and unknown-type
//!   rows, which pin the `parse`/`Unknown command:` error wording).
//! - RESPONSES rebuilt from our envelope types are BYTE-IDENTICAL to Pi's
//!   stdout lines (key order, omitted-undefined keys, error wording). Payload
//!   values are ours-by-construction — they mirror the oracle script's stub
//!   spec, so any byte difference is a serialization-order bug on our side,
//!   not a data mismatch.

use pirust_coding_agent::rpc::jsonl::JsonLineSplitter;
use pirust_coding_agent::rpc::types::{
    parse_input, ParsedInput, QueueMode, QueueModeSerde, RpcCommand, RpcResponse, RpcSessionState,
    StreamingBehavior, ThinkingLevel, ThinkingLevelSerde,
};

fn fixture(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/pi/rpc")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!("missing fixture {path:?}: {e} — run scripts/gen-rpc-oracle.mjs")
    })
}

fn captured_requests() -> Vec<String> {
    fixture("requests.corpus.jsonl")
        .lines()
        .map(str::to_string)
        .collect()
}

/// The inner JSON text of every captured stdout line, in capture order.
fn captured_responses() -> Vec<String> {
    let mut splitter = JsonLineSplitter::new();
    let mut out = Vec::new();
    for line in splitter.push(fixture("responses.corpus.jsonl").as_bytes()) {
        let v: serde_json::Value = serde_json::from_str(&line).expect("response record is JSON");
        out.push(v["line"].as_str().expect("record.line").to_string());
    }
    assert!(!out.is_empty(), "fixture must not be empty");
    out
}

fn faux_model_json() -> serde_json::Value {
    serde_json::json!({
        "id": "faux-1",
        "name": "Faux Model",
        "api": "faux:{FAUX_API}",
        "provider": "faux",
        "baseUrl": "http://localhost:0",
        "reasoning": false,
        "input": ["text", "image"],
        "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0},
        "contextWindow": 128000,
        "maxTokens": 16384,
    })
}

#[test]
fn every_captured_request_parses_as_expected() {
    // Expectations mirror the command script in gen-rpc-oracle.mjs buildSpec().
    use ParsedInput::*;
    use RpcCommand::*;
    let expected: Vec<(ParsedInput, Option<&str>)> = vec![
        (Unknown(None), None), // "{not json" -> JSON syntax error -> parse response
        (Command(GetState), Some("{\"type\":\"get_state\"}")),
        // extension_ui_response with unknown id: parsed, silently ignored.
        (
            ExtensionUiResponse(pirust_coding_agent::rpc::types::RpcExtensionUIResponse {
                id: "nonexistent".to_string(),
                payload: pirust_coding_agent::rpc::types::UiResponsePayload::Value {
                    value: "x".to_string(),
                },
            }),
            Some("{\"type\":\"extension_ui_response\",\"id\":\"nonexistent\",\"value\":\"x\"}"),
        ),
        (
            Command(SetThinkingLevel {
                level: ThinkingLevel::High,
            }),
            None,
        ),
        (Command(CycleThinkingLevel), None),
        (Command(GetAvailableThinkingLevels), None),
        (
            Command(SetSteeringMode {
                mode: QueueMode::OneAtATime,
            }),
            None,
        ),
        (
            Command(SetFollowUpMode {
                mode: QueueMode::All,
            }),
            None,
        ),
        (
            Command(Compact {
                custom_instructions: Some("focus on tests".into()),
            }),
            None,
        ),
        (
            Command(Compact {
                custom_instructions: None,
            }),
            None,
        ),
        (Command(SetAutoCompaction { enabled: false }), None),
        (Command(SetAutoRetry { enabled: true }), None),
        (Command(AbortRetry), None),
        (
            Command(Bash {
                command: "echo hi".into(),
                exclude_from_context: None,
            }),
            None,
        ),
        (Command(AbortBash), None),
        (Command(Abort), None),
        (
            Command(Steer {
                message: "mid-run change".into(),
                images: None,
            }),
            None,
        ),
        (
            Command(FollowUp {
                message: "after this".into(),
                images: None,
            }),
            None,
        ),
        (
            Command(NewSession {
                parent_session: None,
            }),
            None,
        ),
        (
            Command(SwitchSession {
                session_path: "{TMP}/other.session.jsonl".into(),
            }),
            None,
        ),
        (
            Command(Fork {
                entry_id: "e1".into(),
            }),
            None,
        ),
        (Command(Clone), None),
        (Command(GetForkMessages), None),
        (Command(GetEntries { since: None }), None),
        (
            Command(GetEntries {
                since: Some("nope".into()),
            }),
            None,
        ),
        (Command(GetTree), None),
        (Command(GetLastAssistantText), None),
        (Command(SetSessionName { name: "   ".into() }), None),
        (
            Command(SetSessionName {
                name: "  My Session  ".into(),
            }),
            None,
        ),
        (Command(GetMessages), None),
        (Command(GetCommands), None),
        (Command(GetSessionStats), None),
        (
            Command(SetModel {
                provider: "faux".into(),
                model_id: "missing-model".into(),
            }),
            None,
        ),
        (
            Command(SetModel {
                provider: "faux".into(),
                model_id: "faux-1".into(),
            }),
            None,
        ),
        (Command(CycleModel), None),
        (Command(GetAvailableModels), None),
        (Unknown(Some("bogus_command".into())), None),
        (
            Command(Prompt {
                message: "hello again".into(),
                images: None,
                streaming_behavior: Some(StreamingBehavior::FollowUp),
            }),
            None,
        ),
    ];

    let requests = captured_requests();
    assert_eq!(requests.len(), expected.len(), "request count drifted");
    for (i, line) in requests.iter().enumerate() {
        let (want, raw_check) = &expected[i];
        let got = parse_input(line);
        assert_eq!(&got, want, "request #{i} parsed wrong: {line}");
        if let Some(raw) = raw_check {
            assert_eq!(line, raw, "request #{i} fixture drift");
        }
    }
}

#[test]
fn unknown_type_error_wording_matches_pi() {
    // rpc-mode.ts:713 — `Unknown command: ${type}`; non-object JSON reports
    // the literal string "undefined".
    let got = parse_input("{\"type\":\"bogus_command\"}");
    assert_eq!(
        got.unknown_type_label().as_deref(),
        Some("Unknown command: bogus_command")
    );
    let got = parse_input("[1,2]");
    assert_eq!(
        got.unknown_type_label().as_deref(),
        Some("Unknown command: undefined")
    );
}

// -- Response byte-identity ---------------------------------------------------

fn assert_line_eq(actual: String, expected: &str, label: &str) {
    let line = actual.trim_end_matches('\n');
    assert_eq!(line, expected, "{label}: byte drift vs Pi capture");
}

fn ok(id: Option<&str>, command: &str) -> String {
    pirust_coding_agent::rpc::jsonl::serialize_json_line(&RpcResponse::success(
        id.map(str::to_string),
        command,
    ))
}

fn err(id: Option<&str>, command: &str, message: &str) -> String {
    pirust_coding_agent::rpc::jsonl::serialize_json_line(&RpcResponse::error(
        id.map(str::to_string),
        command,
        message,
    ))
}

fn ok_data(id: Option<&str>, command: &str, data: serde_json::Value) -> String {
    pirust_coding_agent::rpc::jsonl::serialize_json_line(&RpcResponse::success_with(
        id.map(str::to_string),
        command,
        data,
    ))
}

/// Rebuild EVERY captured response from our typed constructors and compare
/// bytes. Rows that depend on session payloads (entries/tree/messages/stats)
/// reconstruct those payloads verbatim from the stub spec.
#[test]
fn responses_rebuild_byte_identically() {
    let r = captured_responses();

    // 0: invalid JSON -> parse error (V8 message wording pinned verbatim)
    assert_line_eq(
        err(None, "parse", "Failed to parse command: Expected property name or '}' in JSON at position 1 (line 1 column 2)"),
        &r[0], "row0 parse-error");

    // 1: get_state — typed RpcSessionState; undefined keys OMITTED.
    let state = RpcSessionState {
        model: Some(faux_model_json()),
        thinking_level: ThinkingLevelSerde(ThinkingLevel::Off),
        is_streaming: false,
        is_compacting: false,
        steering_mode: QueueModeSerde(QueueMode::All),
        follow_up_mode: QueueModeSerde(QueueMode::All),
        session_file: None,
        session_id: "sess_rpc_test_0000".to_string(),
        session_name: None,
        auto_compaction_enabled: true,
        message_count: 2,
        pending_message_count: 0,
    };
    assert_line_eq(
        ok_data(None, "get_state", serde_json::to_value(&state).unwrap()),
        &r[1],
        "row1 get_state",
    );

    // 2..=11: envelopes and simple data payloads, in input order
    assert_line_eq(ok(Some("a"), "set_thinking_level"), &r[2], "row2");
    assert_line_eq(
        ok_data(
            Some("b"),
            "cycle_thinking_level",
            serde_json::json!({"level":"medium"}),
        ),
        &r[3],
        "row3 cycle_thinking_level",
    );
    assert_line_eq(
        ok_data(
            Some("c"),
            "get_available_thinking_levels",
            serde_json::json!({"levels":["off","minimal","low","medium","high"]}),
        ),
        &r[4],
        "row4 levels",
    );
    assert_line_eq(ok(Some("d"), "set_steering_mode"), &r[5], "row5");
    assert_line_eq(ok(Some("e"), "set_follow_up_mode"), &r[6], "row6");
    let compaction =
        serde_json::json!({"summary":"[stub summary]","firstKeptEntryId":"e2","tokensBefore":1234});
    assert_line_eq(
        ok_data(Some("f"), "compact", compaction.clone()),
        &r[7],
        "row7 compact f",
    );
    assert_line_eq(
        ok_data(None, "compact", compaction.clone()),
        &r[8],
        "row8 compact no-id",
    );
    assert_line_eq(ok(Some("g"), "set_auto_compaction"), &r[9], "row9");
    assert_line_eq(ok(Some("h"), "set_auto_retry"), &r[10], "row10");
    assert_line_eq(ok(Some("i"), "abort_retry"), &r[11], "row11");

    // 12..=16: bash + aborts + queues
    assert_line_eq(
        ok_data(
            Some("j"),
            "bash",
            serde_json::json!({"output":"hi\n","exitCode":0,"cancelled":false,"truncated":false,"__rpcId":"j"}),
        ),
        &r[12],
        "row12 bash",
    );
    assert_line_eq(ok(Some("k"), "abort_bash"), &r[13], "row13");
    assert_line_eq(ok(Some("l"), "abort"), &r[14], "row14");
    assert_line_eq(ok(Some("m"), "steer"), &r[15], "row15");
    assert_line_eq(ok(Some("n"), "follow_up"), &r[16], "row16");

    // 17..=20: session lifecycle
    assert_line_eq(
        ok_data(
            Some("o"),
            "new_session",
            serde_json::json!({"cancelled":false}),
        ),
        &r[17],
        "row17 new_session",
    );
    assert_line_eq(
        ok_data(
            Some("p"),
            "switch_session",
            serde_json::json!({"cancelled":false}),
        ),
        &r[18],
        "row18 switch_session",
    );
    assert_line_eq(
        ok_data(
            Some("q"),
            "fork",
            serde_json::json!({"text":"hello world","cancelled":false}),
        ),
        &r[19],
        "row19 fork",
    );
    assert_line_eq(
        ok_data(Some("r"), "clone", serde_json::json!({"cancelled":false})),
        &r[20],
        "row20 clone",
    );

    // 21: fork messages
    assert_line_eq(
        ok_data(
            Some("s"),
            "get_fork_messages",
            serde_json::json!({"messages":[{"entryId":"e1","text":"hello world"}]}),
        ),
        &r[21],
        "row21 fork messages",
    );

    // 22..=25: entries/tree/text
    let entries = serde_json::json!([
        {"id":"e1","parentId":null,"type":"message","message":{"role":"user"}},
        {"id":"e2","parentId":"e1","type":"message","message":{"role":"assistant"}},
    ]);
    assert_line_eq(
        ok_data(
            Some("t"),
            "get_entries",
            serde_json::json!({"entries": entries, "leafId":"e2"}),
        ),
        &r[22],
        "row22 get_entries",
    );
    assert_line_eq(
        err(Some("u"), "get_entries", "Entry not found: nope"),
        &r[23],
        "row23",
    );
    let tree = serde_json::json!([
        {"entry":{"id":"e1","parentId":null,"type":"message","message":{"role":"user"}},
         "children":[{"entry":{"id":"e2","parentId":"e1","type":"message","message":{"role":"assistant"}},
                      "children":[]}]},
    ]);
    assert_line_eq(
        ok_data(
            Some("v"),
            "get_tree",
            serde_json::json!({"tree": tree, "leafId":"e2"}),
        ),
        &r[24],
        "row24 get_tree",
    );
    assert_line_eq(
        ok_data(
            Some("w"),
            "get_last_assistant_text",
            serde_json::json!({"text":"hi there"}),
        ),
        &r[25],
        "row25 last text",
    );

    // 26..=27: session name
    assert_line_eq(
        err(
            Some("x"),
            "set_session_name",
            "Session name cannot be empty",
        ),
        &r[26],
        "row26 empty-name",
    );
    assert_line_eq(ok(Some("y"), "set_session_name"), &r[27], "row27 set name");

    // 28: get_messages — AgentMessage array passes through verbatim.
    let messages = serde_json::json!([
        {"role":"user","content":"hello world","timestamp":1700000000000i64},
        {"role":"assistant","content":[{"type":"text","text":"hi there"}],
         "api":"anthropic-messages","provider":"anthropic","model":"claude-opus-4-8",
         "usage":{"input":10,"output":5,"cacheRead":0,"cacheWrite":0,"totalTokens":15,
                  "cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},
         "stopReason":"stop","timestamp":1700000000000i64},
    ]);
    assert_line_eq(
        ok_data(
            Some("z"),
            "get_messages",
            serde_json::json!({"messages": messages}),
        ),
        &r[28],
        "row28 get_messages",
    );

    // 29: get_commands — no extensions/templates/skills registered.
    assert_line_eq(
        ok_data(
            Some("aa"),
            "get_commands",
            serde_json::json!({"commands":[]}),
        ),
        &r[29],
        "row29 commands",
    );

    // 30: get_session_stats — `sessionFile` is undefined in the stub, so the
    // key is OMITTED (first position of the TS interface, absent on the wire).
    assert_line_eq(
        ok_data(
            Some("ab"),
            "get_session_stats",
            serde_json::json!({
            "sessionId":"sess_rpc_test_0000","userMessages":1,"assistantMessages":1,
            "toolCalls":0,"toolResults":0,"totalMessages":2,
            "tokens":{"input":10,"output":5,"cacheRead":0,"cacheWrite":0,"total":15},
            "cost":0.000037}),
        ),
        &r[30],
        "row30 stats",
    );

    // 31..=34: models
    assert_line_eq(
        err(
            Some("ac"),
            "set_model",
            "Model not found: faux/missing-model",
        ),
        &r[31],
        "row31 model not found",
    );
    assert_line_eq(
        ok_data(Some("ad"), "set_model", faux_model_json()),
        &r[32],
        "row32 set_model returns Model",
    );
    assert_line_eq(
        ok_data(
            Some("ae"),
            "cycle_model",
            serde_json::json!({"model": faux_model_json(), "thinkingLevel":"medium", "isScoped":false}),
        ),
        &r[33],
        "row33 cycle_model",
    );
    assert_line_eq(
        ok_data(
            Some("af"),
            "get_available_models",
            serde_json::json!({"models":[faux_model_json()]}),
        ),
        &r[34],
        "row34 available models",
    );

    // 35: unknown command
    assert_line_eq(
        err(None, "bogus_command", "Unknown command: bogus_command"),
        &r[35],
        "row35 unknown",
    );

    // 36..=38: prompt success + passthrough events (toJsonEvent output).
    assert_line_eq(ok(Some("ag"), "prompt"), &r[36], "row36 prompt ack");
    assert_line_eq(
        "{\"type\":\"queue_update\",\"steering\":[],\"followUp\":[]}".to_string(),
        &r[37],
        "row37 queue_update",
    );
    assert_line_eq(
        "{\"type\":\"agent_settled\"}".to_string(),
        &r[38],
        "row38 agent_settled",
    );
}

#[test]
fn response_count_matches_capture() {
    assert_eq!(captured_responses().len(), 39);
    assert_eq!(captured_requests().len(), 38);
}
