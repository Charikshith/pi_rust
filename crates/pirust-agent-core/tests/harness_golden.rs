//! Behavioural golden test for the `AgentHarness` (feat-003 acceptance oracle).
//!
//! Builds an [`AgentHarness`] with a scripted `fauxProvider` (the SAME 2-turn run
//! as `tests/fixtures/pi/agent/loop-echo.json`), a fake `echo` tool, and an
//! in-memory session, then asserts the FULL harness tape (WITH the harness-own
//! events `after_provider_response`, `save_point`, `settled`), the resulting
//! session `entryTypes`, and the fixture's `stableFields`.
//!
//! Per the fixture `determinism` note, only consecutive `message_update` runs are
//! collapsed (delta chunk count is runtime-dependent); every other event —
//! including the harness-own events and their exact positions — must match.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::future::BoxFuture;
use pirust_ai::api::{ProviderStreams, SimpleStreamOptions};
use pirust_ai::providers::faux::{faux_assistant_message, faux_text_message, faux_tool_call, Faux};
use pirust_ai::types::{
    AssistantContent, Context, Message, Model, StopReason, TextContent, UserContent,
};
use serde_json::{Map, Value};
use tokio_util::sync::CancellationToken;

use pirust_agent_core::agent_loop::StreamFn;
use pirust_agent_core::harness::messages::AgentMessage;
use pirust_agent_core::harness::session::memory_storage::InMemorySessionStorage;
use pirust_agent_core::harness::session::Session;
use pirust_agent_core::harness::types::SessionTreeEntry;
use pirust_agent_core::harness::{
    AgentHarness, AgentHarnessOptions, HarnessEvent, HarnessListener,
};
use pirust_agent_core::types::{AgentTool, AgentToolResult, AgentToolUpdateCallback, ToolError};

/// The fake `echo` tool: returns `{ content: [text <args.text>], details: {} }`.
struct EchoTool;

#[async_trait]
impl AgentTool for EchoTool {
    fn name(&self) -> &str {
        "echo"
    }
    fn label(&self) -> &str {
        "Echo"
    }
    fn description(&self) -> &str {
        "Echo"
    }
    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": { "text": { "type": "string" } },
            "required": ["text"],
            "additionalProperties": true
        })
    }
    async fn execute(
        &self,
        _id: &str,
        args: Value,
        _token: CancellationToken,
        _on_update: AgentToolUpdateCallback,
    ) -> Result<AgentToolResult, ToolError> {
        let text = args
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        Ok(AgentToolResult {
            content: vec![UserContent::Text(TextContent::new(text))],
            details: Value::Object(Map::new()),
            added_tool_names: None,
            terminate: None,
        })
    }
}

fn echo_args() -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("text".to_string(), Value::String("hi".to_string()));
    m
}

/// The scripted 2-turn faux provider stream function (matches loop-echo.json).
fn scripted_stream_fn() -> StreamFn {
    let responses = Arc::new(Mutex::new(VecDeque::from([
        // turn 1: assistant calls `echo` with { text: "hi" }, id "call-1".
        faux_assistant_message(
            vec![AssistantContent::ToolCall(faux_tool_call(
                "call-1",
                "echo",
                echo_args(),
            ))],
            Default::default(),
        ),
        // turn 2: assistant text "done".
        faux_text_message("done"),
    ])));

    Arc::new(
        move |model: Model, ctx: Context, opts: SimpleStreamOptions, _token| {
            let message = responses
                .lock()
                .unwrap()
                .pop_front()
                .expect("faux script exhausted");
            let faux = Faux::new().with_token_size(1000, 1000);
            faux.set_responses(vec![message.into()]);
            faux.stream_simple(&model, &ctx, Some(opts))
        },
    )
}

/// Collapse consecutive `message_update` runs to a single entry.
fn collapse_updates(types: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in types {
        if t == "message_update" && out.last().map(String::as_str) == Some("message_update") {
            continue;
        }
        out.push(t.clone());
    }
    out
}

/// Map a session entry to the fixture's `entryTypes` form (message entries only).
fn entry_type(entry: &SessionTreeEntry) -> Option<String> {
    match entry {
        SessionTreeEntry::Message { message, .. } => {
            let role = match message {
                AgentMessage::Llm(Message::User(_)) => "user",
                AgentMessage::Llm(Message::Assistant(_)) => "assistant",
                AgentMessage::Llm(Message::ToolResult(_)) => "toolResult",
                AgentMessage::BashExecution(_) => "bashExecution",
                AgentMessage::Custom(_) => "custom",
                AgentMessage::BranchSummary(_) => "branchSummary",
                AgentMessage::CompactionSummary(_) => "compactionSummary",
            };
            Some(format!("message:{role}"))
        }
        _ => None,
    }
}

#[tokio::test]
async fn harness_tape_and_session_match_pi_fixture() {
    let model = Faux::new().get_model().clone();
    let session = Session::new(InMemorySessionStorage::new());

    let mut options = AgentHarnessOptions::new(scripted_stream_fn(), model, session);
    options.tools = vec![Arc::new(EchoTool) as Arc<dyn AgentTool>];
    options.system_prompt = "You are a test agent.".to_string();

    let harness = AgentHarness::new(options);

    // Subscribe a wildcard listener collecting `event.type`.
    let tape: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    {
        let tape = Arc::clone(&tape);
        let listener: HarnessListener = Arc::new(move |event: HarnessEvent| {
            let tape = Arc::clone(&tape);
            let ty = event.event_type().to_string();
            Box::pin(async move {
                tape.lock().unwrap().push(ty);
            }) as BoxFuture<'static, ()>
        });
        harness.subscribe(listener);
    }

    let final_message = harness.prompt("please echo hi").await.expect("prompt ok");

    // 1. Full harness tape (with harness-own events), collapsing only consecutive
    //    message_update runs, equals the fixture's tapeTypes.
    let raw = tape.lock().unwrap().clone();
    let actual = collapse_updates(&raw);
    let expected = vec![
        "agent_start",
        "turn_start",
        "message_start",
        "message_end",
        "after_provider_response",
        "message_start",
        "message_update",
        "message_end",
        "tool_execution_start",
        "tool_execution_end",
        "message_start",
        "message_end",
        "turn_end",
        "save_point",
        "turn_start",
        "after_provider_response",
        "message_start",
        "message_update",
        "message_end",
        "turn_end",
        "save_point",
        "agent_end",
        "settled",
    ];
    assert_eq!(
        actual, expected,
        "full harness tape (updates collapsed) must match loop-echo.json tapeTypes"
    );

    // 2. Resulting session entryTypes.
    let entries = harness.session().get_entries().await.expect("entries");
    let entry_types: Vec<String> = entries.iter().filter_map(entry_type).collect();
    assert_eq!(
        entry_types,
        vec![
            "message:user",
            "message:assistant",
            "message:toolResult",
            "message:assistant",
        ],
        "session entryTypes must match loop-echo.json"
    );

    // 3. stableFields.
    // toolResult: name "echo", id "call-1", text "hi", not error.
    let tool_result = entries
        .iter()
        .find_map(|e| match e {
            SessionTreeEntry::Message {
                message: AgentMessage::Llm(Message::ToolResult(tr)),
                ..
            } => Some(tr),
            _ => None,
        })
        .expect("toolResult entry");
    assert_eq!(tool_result.tool_name, "echo");
    assert_eq!(tool_result.tool_call_id, "call-1");
    assert!(!tool_result.is_error);
    let tr_text: String = tool_result
        .content
        .iter()
        .filter_map(|c| match c {
            UserContent::Text(t) => Some(t.text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(tr_text, "hi");

    // stopReasons [stop, stop] across the two assistant entries.
    let stop_reasons: Vec<StopReason> = entries
        .iter()
        .filter_map(|e| match e {
            SessionTreeEntry::Message {
                message: AgentMessage::Llm(Message::Assistant(a)),
                ..
            } => Some(a.stop_reason),
            _ => None,
        })
        .collect();
    assert_eq!(stop_reasons, vec![StopReason::Stop, StopReason::Stop]);

    // final assistant text "done" (from the returned message).
    let final_text: String = final_message
        .content
        .iter()
        .filter_map(|c| match c {
            AssistantContent::Text(t) => Some(t.text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(final_text, "done");
    assert_eq!(final_message.stop_reason, StopReason::Stop);
}
