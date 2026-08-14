//! Behavioural golden test for the agent loop (feat-003).
//!
//! Drives [`run_agent_loop`] with a scripted `fauxProvider` (2 turns) + a fake
//! `echo` tool and compares the emitted `AgentEvent` tape against the Pi fixture
//! `tests/fixtures/pi/agent/loop-echo.json`.
//!
//! The fixture's `tapeTypes` is the *harness* tape. This crate's loop emits the
//! `AgentEvent` union only, so per the fixture's own rules the comparison
//! - REMOVES the harness-own events `after_provider_response`, `save_point`,
//!   `settled` (added by the Wave-F harness), and
//! - COLLAPSES consecutive `message_update` runs to one (delta chunk count is
//!   runtime-dependent per the fixture `determinism` note).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::future::BoxFuture;
use pirust_ai::api::{ProviderStreams, SimpleStreamOptions};
use pirust_ai::providers::faux::{faux_assistant_message, faux_text_message, faux_tool_call, Faux};
use pirust_ai::types::{
    AssistantContent, Context, Message, Model, StopReason, TextContent, UserContent, UserMessage,
    UserMessageContent, UserRole,
};
use serde_json::{Map, Value};
use tokio_util::sync::CancellationToken;

use pirust_agent_core::agent_loop::{run_agent_loop, AgentEventSink, StreamFn};
use pirust_agent_core::harness::messages::{convert_to_llm, AgentMessage};
use pirust_agent_core::types::{
    AgentContext, AgentEvent, AgentLoopConfig, AgentTool, AgentToolResult, AgentToolUpdateCallback,
    ToolError,
};

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

/// Build a `StreamFn` that replays the 2-turn faux script. A fresh `Faux` is
/// created per call (faux is `!Send`; the closure only captures the `Send` queued
/// messages + model), and the fully-buffered stream it produces outlives it.
fn scripted_stream_fn(model: Model) -> StreamFn {
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
        move |_model: Model, ctx: Context, opts: SimpleStreamOptions, _token| {
            let message = responses
                .lock()
                .unwrap()
                .pop_front()
                .expect("faux script exhausted");
            // min == max => deterministic single-ish delta chunking.
            let faux = Faux::new().with_token_size(1000, 1000);
            faux.set_responses(vec![message.into()]);
            faux.stream_simple(&model, &ctx, Some(opts))
        },
    )
}

fn tape_sink(tape: Arc<Mutex<Vec<AgentEvent>>>) -> AgentEventSink {
    Box::new(move |event: AgentEvent| {
        tape.lock().unwrap().push(event);
        Box::pin(async {}) as BoxFuture<'static, ()>
    })
}

fn base_config(model: Model) -> AgentLoopConfig {
    AgentLoopConfig {
        model,
        api_key: None,
        tool_execution: None,
        convert_to_llm: Box::new(|msgs| Box::pin(async move { convert_to_llm(&msgs) })),
        transform_context: None,
        get_api_key: None,
        should_stop_after_turn: None,
        prepare_next_turn: None,
        get_steering_messages: None,
        get_follow_up_messages: None,
        before_tool_call: None,
        after_tool_call: None,
    }
}

fn event_type(event: &AgentEvent) -> &'static str {
    match event {
        AgentEvent::AgentStart => "agent_start",
        AgentEvent::AgentEnd { .. } => "agent_end",
        AgentEvent::TurnStart => "turn_start",
        AgentEvent::TurnEnd { .. } => "turn_end",
        AgentEvent::MessageStart { .. } => "message_start",
        AgentEvent::MessageUpdate { .. } => "message_update",
        AgentEvent::MessageEnd { .. } => "message_end",
        AgentEvent::ToolExecutionStart { .. } => "tool_execution_start",
        AgentEvent::ToolExecutionUpdate { .. } => "tool_execution_update",
        AgentEvent::ToolExecutionEnd { .. } => "tool_execution_end",
    }
}

/// Collapse consecutive `message_update` runs to a single entry.
fn collapse_updates(types: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for &t in types {
        if t == "message_update" && out.last().map(String::as_str) == Some("message_update") {
            continue;
        }
        out.push(t.to_string());
    }
    out
}

#[tokio::test]
async fn loop_tape_matches_pi_fixture() {
    let model = Faux::new().get_model().clone();
    let stream_fn = scripted_stream_fn(model.clone());

    let context = AgentContext {
        system_prompt: "You are a test agent.".to_string(),
        messages: Vec::new(),
        tools: Some(vec![Arc::new(EchoTool) as Arc<dyn AgentTool>]),
    };
    let prompts = vec![AgentMessage::Llm(Message::User(UserMessage {
        role: UserRole::User,
        content: UserMessageContent::Text("please echo hi".to_string()),
        timestamp: 0,
    }))];

    let tape = Arc::new(Mutex::new(Vec::new()));
    let mut sink = tape_sink(tape.clone());

    let new_messages = run_agent_loop(
        prompts,
        context,
        base_config(model),
        &mut sink,
        None,
        Some(stream_fn),
    )
    .await;

    // 1. Tape (with consecutive message_update collapsed) equals the filtered
    //    Pi fixture sequence (harness-own events removed, updates collapsed).
    let raw: Vec<&str> = {
        let events = tape.lock().unwrap();
        events.iter().map(event_type).collect()
    };
    let actual = collapse_updates(&raw);
    let expected = vec![
        "agent_start",
        "turn_start",
        "message_start",
        "message_end",
        "message_start",
        "message_update",
        "message_end",
        "tool_execution_start",
        "tool_execution_end",
        "message_start",
        "message_end",
        "turn_end",
        "turn_start",
        "message_start",
        "message_update",
        "message_end",
        "turn_end",
        "agent_end",
    ];
    assert_eq!(
        actual, expected,
        "collapsed loop tape must match the filtered Pi fixture"
    );

    // 2. stableFields from the returned newMessages.
    //    Order: [user prompt, assistant(toolCall), toolResult, assistant("done")].
    assert_eq!(new_messages.len(), 4);

    // tool name "echo" + toolResult text "hi".
    let tool_result = new_messages
        .iter()
        .find_map(|m| match m {
            AgentMessage::Llm(Message::ToolResult(tr)) => Some(tr),
            _ => None,
        })
        .expect("a toolResult message");
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

    // assistant final text "done" + stopReasons [stop, stop].
    let assistants: Vec<_> = new_messages
        .iter()
        .filter_map(|m| match m {
            AgentMessage::Llm(Message::Assistant(a)) => Some(a),
            _ => None,
        })
        .collect();
    assert_eq!(assistants.len(), 2);
    let stop_reasons: Vec<StopReason> = assistants.iter().map(|a| a.stop_reason).collect();
    assert_eq!(stop_reasons, vec![StopReason::Stop, StopReason::Stop]);

    let final_text: String = assistants
        .last()
        .unwrap()
        .content
        .iter()
        .filter_map(|c| match c {
            AssistantContent::Text(t) => Some(t.text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(final_text, "done");
}
