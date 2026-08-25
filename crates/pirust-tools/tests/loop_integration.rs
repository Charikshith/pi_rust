//! End-to-end proof that a registry tool actually runs inside the agent runtime.
//!
//! Every other test in this crate exercises a tool in isolation: build the
//! definition, call `execute`, compare against a captured row. That leaves one
//! seam unproven — the loop. This test closes it by driving
//! [`run_agent_loop`] with the scripted `faux` provider (the house pattern from
//! `crates/pirust-agent-core/tests/{loop_golden,harness_golden}.rs`) and the real
//! `read` tool taken from [`pirust_tools::create_tool`], and asserting three
//! things:
//!
//! 1. **The tool ran.** The tape carries `tool_execution_start` /
//!    `tool_execution_end`, and the returned messages carry a `toolResult` whose
//!    text is what Pi's `read` produces for that file.
//! 2. **The result flowed back to the provider.** Turn 2's request context
//!    contains the `toolResult` message, so the loop really fed it forward rather
//!    than only reporting it on the tape.
//! 3. **`description()` — not `label()` — is what reaches the provider.**
//!    `build_llm_tools` (`agent_loop.rs:1072-1083`) fills `Tool.description` from
//!    `AgentTool::description()`; `read`'s label is the bare string `"read"`,
//!    so a `label()`/`description()` mix-up is directly observable. Pinned
//!    against `tests/fixtures/pi/tools/strings/read.json`, which was captured
//!    from real Pi — this was a live bug during feat-004.
//!
//! The expected `read` output is not self-authored: row 1 of
//! `tests/fixtures/pi/tools/exec.corpus.jsonl` pins that a plain read of a
//! newline-terminated text file returns the file verbatim with `details: null`.
//!
//! Nothing outside a `tempfile` directory is touched, and no external binary is
//! involved (`read` needs neither `rg` nor `fd`).

use std::collections::VecDeque;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use pirust_agent_core::agent_loop::{run_agent_loop, AgentEventSink, StreamFn};
use pirust_agent_core::harness::messages::{convert_to_llm, AgentMessage};
use pirust_agent_core::types::{AgentContext, AgentEvent, AgentLoopConfig, ToolExecutionMode};
use pirust_ai::api::{ProviderStreams, SimpleStreamOptions};
use pirust_ai::providers::faux::{faux_assistant_message, faux_text_message, faux_tool_call, Faux};
use pirust_ai::types::{
    Context, Message, Model, StopReason, Tool, UserContent, UserMessage, UserMessageContent,
    UserRole,
};
use pirust_tools::{create_tool, ToolName};
use serde_json::{Map, Value};

/// The file the scripted assistant asks `read` for, and its exact bytes.
const TARGET_FILE: &str = "hello.txt";
const TARGET_CONTENTS: &str = "hello\nworld\n";

fn fixture(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/pi/tools")
        .join(relative)
}

fn read_fixture(relative: &str) -> String {
    let path = fixture(relative);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read fixture {}: {e}", path.display()))
}

fn read_args() -> Map<String, Value> {
    let mut args = Map::new();
    args.insert("path".to_string(), Value::String(TARGET_FILE.to_string()));
    args
}

/// What the provider was asked for, per `stream` call.
#[derive(Debug, Default)]
struct Observed {
    contexts: Vec<Context>,
}

/// A two-turn faux script plus a recorder for the request contexts.
///
/// Turn 1: the assistant calls `read` with `{ path: "hello.txt" }` (id
/// `call-1`). Turn 2: plain text, which ends the loop. A fresh [`Faux`] is built
/// per call because `Faux` is `!Send` and [`StreamFn`] must be `Send + Sync`; the
/// stream it returns is fully buffered, so it outlives the provider — the same
/// trick `loop_golden.rs:87-115` uses.
fn scripted_stream_fn(model: Model, observed: Arc<Mutex<Observed>>) -> StreamFn {
    let responses = Arc::new(Mutex::new(VecDeque::from([
        faux_assistant_message(
            vec![pirust_ai::types::AssistantContent::ToolCall(
                faux_tool_call("call-1", "read", read_args()),
            )],
            Default::default(),
        ),
        faux_text_message("read it"),
    ])));

    Arc::new(
        move |_model: Model, ctx: Context, opts: SimpleStreamOptions, _token| {
            observed.lock().unwrap().contexts.push(ctx.clone());
            let message = responses
                .lock()
                .unwrap()
                .pop_front()
                .expect("faux script exhausted");
            // min == max keeps delta chunking deterministic.
            let faux = Faux::new().with_token_size(1000, 1000);
            faux.set_responses(vec![message.into()]);
            faux.stream_simple(&model, &ctx, Some(opts))
        },
    )
}

fn tape_sink(tape: Arc<Mutex<Vec<AgentEvent>>>) -> AgentEventSink {
    Box::new(move |event: AgentEvent| {
        tape.lock().unwrap().push(event);
        Box::pin(async {}) as Pin<Box<dyn Future<Output = ()> + Send>>
    })
}

fn base_config(model: Model) -> AgentLoopConfig {
    AgentLoopConfig {
        model,
        api_key: None,
        tool_execution: None,
        reasoning: None,
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

/// Concatenate the text blocks of a tool-result / user content list.
fn text_of(content: &[UserContent]) -> String {
    content
        .iter()
        .filter_map(|block| match block {
            UserContent::Text(text) => Some(text.text.clone()),
            UserContent::Image(_) => None,
        })
        .collect()
}

#[tokio::test]
async fn read_tool_from_the_registry_runs_through_the_agent_loop() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join(TARGET_FILE), TARGET_CONTENTS).expect("write target file");
    let cwd = dir.path().to_string_lossy().to_string();

    // The tool under test comes from the registry, not from `read.rs` directly.
    let read_tool = create_tool(ToolName::Read, &cwd, None);
    assert_eq!(read_tool.name(), "read");
    assert_eq!(read_tool.label(), "read");
    assert_eq!(read_tool.execution_mode(), None::<ToolExecutionMode>);

    let model = Faux::new().get_model().clone();
    let observed = Arc::new(Mutex::new(Observed::default()));
    let stream_fn = scripted_stream_fn(model.clone(), Arc::clone(&observed));

    let context = AgentContext {
        system_prompt: "You are a test agent.".to_string(),
        messages: Vec::new(),
        tools: Some(vec![Arc::clone(&read_tool)]),
    };
    let prompts = vec![AgentMessage::Llm(Message::User(UserMessage {
        role: UserRole::User,
        content: UserMessageContent::Text(format!("please read {TARGET_FILE}")),
        timestamp: 0,
    }))];

    let tape = Arc::new(Mutex::new(Vec::new()));
    let mut sink = tape_sink(Arc::clone(&tape));

    let new_messages = run_agent_loop(
        prompts,
        context,
        base_config(model),
        &mut sink,
        None,
        Some(stream_fn),
    )
    .await;

    // --- 1. the tool ran -------------------------------------------------
    let types: Vec<&str> = {
        let events = tape.lock().unwrap();
        events.iter().map(event_type).collect()
    };
    assert!(
        types.contains(&"tool_execution_start") && types.contains(&"tool_execution_end"),
        "the loop must have executed the tool; tape was {types:?}"
    );

    let tool_result = new_messages
        .iter()
        .find_map(|message| match message {
            AgentMessage::Llm(Message::ToolResult(result)) => Some(result),
            _ => None,
        })
        .expect("the loop must produce a toolResult message");
    assert_eq!(tool_result.tool_name, "read");
    assert_eq!(tool_result.tool_call_id, "call-1");
    assert!(
        !tool_result.is_error,
        "read failed: {}",
        text_of(&tool_result.content)
    );
    // exec.corpus.jsonl row 1: a plain newline-terminated file comes back
    // verbatim, with `details: null`.
    assert_eq!(text_of(&tool_result.content), TARGET_CONTENTS);
    assert_eq!(tool_result.details, Some(Value::Null));

    // The loop ran two turns and stopped on the text response.
    let stop_reasons: Vec<StopReason> = new_messages
        .iter()
        .filter_map(|message| match message {
            AgentMessage::Llm(Message::Assistant(assistant)) => Some(assistant.stop_reason),
            _ => None,
        })
        .collect();
    assert_eq!(stop_reasons, vec![StopReason::Stop, StopReason::Stop]);

    // --- 2. the result flowed back into the provider request -------------
    let contexts = observed.lock().unwrap().contexts.clone();
    assert_eq!(
        contexts.len(),
        2,
        "the loop must have made two provider calls"
    );
    let fed_back = contexts[1]
        .messages
        .iter()
        .find_map(|message| match message {
            Message::ToolResult(result) => Some(result),
            _ => None,
        })
        .expect("turn 2's request context must carry the toolResult");
    assert_eq!(fed_back.tool_call_id, "call-1");
    assert_eq!(fed_back.tool_name, "read");
    assert_eq!(text_of(&fed_back.content), TARGET_CONTENTS);

    // --- 3. `description()`, not `label()`, reaches the provider ----------
    let strings: Value =
        serde_json::from_str(&read_fixture("strings/read.json")).expect("parse strings/read.json");
    let want_description = strings["description"]
        .as_str()
        .expect("captured read description");
    let want_label = strings["label"].as_str().expect("captured read label");
    assert_ne!(
        want_description, want_label,
        "the fixture must make description and label distinguishable for this test to bite"
    );

    let offered: &[Tool] = contexts[0]
        .tools
        .as_deref()
        .expect("the request context must offer the tool");
    assert_eq!(offered.len(), 1);
    assert_eq!(offered[0].name, "read");
    assert_eq!(
        offered[0].description, want_description,
        "the provider must receive AgentTool::description(), not label()"
    );
    assert_ne!(
        offered[0].description, want_label,
        "label() leaked into the provider request"
    );

    // The schema travels with it, byte-for-byte.
    let want_schema = read_fixture("schemas/read.json").trim_end().to_string();
    let got_schema = serde_json::to_string(&offered[0].parameters).expect("serialize parameters");
    assert_eq!(got_schema, want_schema);

    // Both turns offered the same tool list.
    assert_eq!(contexts[1].tools.as_deref(), Some(offered));
}
