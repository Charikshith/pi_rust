//! feat-012 Wave 2 — behavioral tests for [`pirust_coding_agent::rpc::mode::handle_command`]
//! over a real [`AgentHarness`] + [`RpcRuntimeHost`]. Not an oracle byte-replay (see
//! `plan.md`'s Wave 2 section for why): these assert our own dispatch logic is internally
//! consistent and matches the Wave-1-pinned wire shapes (`RpcResponse`/`RpcSessionState`),
//! using a scripted `Faux` provider — the same pattern `harness_golden.rs` uses.
//!
//! One test (`live_prompt_against_local_llama_server`) drives a REAL end-to-end turn
//! against a local `llama-server` (`ggml-org/Qwen3.5-0.8B-GGUF` at `127.0.0.1:8080`,
//! Anthropic-compatible `/v1/messages`) when one is reachable; it skips (not fails) when
//! not, matching `scripts/gen-rpc-live-oracle.mjs`'s own convention.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pirust_agent_core::agent_loop::StreamFn;
use pirust_agent_core::harness::session::v4::memory::InMemorySessionStorage as V4MemoryStorage;
use pirust_agent_core::harness::session::v4::session::Session as V4Session;
use pirust_agent_core::harness::session::v4::types::SessionMetadata;
use pirust_agent_core::harness::{AgentHarness, AgentHarnessOptions, AgentHarnessPhase};
use pirust_ai::api::{ProviderStreams, SimpleStreamOptions};
use pirust_ai::providers::faux::{faux_text_message, Faux};
use pirust_ai::types::{Api, Modality, Model, ModelCost, ModelCostRates, ProviderId};
use pirust_coding_agent::models::StaticModelSource;
use pirust_coding_agent::rpc::host::RpcRuntimeHost;
use pirust_coding_agent::rpc::mode::{handle_command, RpcOutputFn};
use pirust_coding_agent::rpc::types::{QueueMode, RpcCommand, RpcResponse, ThinkingLevel};

type TestStorage = V4MemoryStorage;

fn test_model(id: &str, reasoning: bool) -> Model {
    Model {
        id: id.to_string(),
        name: id.to_string(),
        api: Api::from("faux"),
        provider: ProviderId::from("faux"),
        base_url: "http://localhost:0".into(),
        reasoning,
        thinking_level_map: None,
        input: vec![Modality::Text],
        cost: ModelCost {
            rates: ModelCostRates {
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
            },
            tiers: None,
        },
        context_window: 128_000,
        max_tokens: 16_384,
        headers: None,
        compat: None,
    }
}

/// A scripted single-reply faux provider: every call returns the same text.
fn scripted_stream_fn(text: &'static str) -> StreamFn {
    Arc::new(move |model, ctx, opts: SimpleStreamOptions, _token| {
        let faux = Faux::new().with_token_size(1000, 1000);
        faux.set_responses(vec![faux_text_message(text).into()]);
        faux.stream_simple(&model, &ctx, Some(opts))
    })
}

fn test_harness(model: Model, stream_fn: StreamFn) -> Arc<AgentHarness<TestStorage>> {
    let storage = Arc::new(V4MemoryStorage::new(SessionMetadata {
        id: "sess-rpc-test".to_string(),
        created_at: 0,
        parent_session_id: None,
    }));
    let session = V4Session::new(storage);
    let options = AgentHarnessOptions::new(stream_fn, model, session);
    Arc::new(AgentHarness::new(options))
}

fn test_host(model: Model, other_models: Vec<Model>) -> Arc<RpcRuntimeHost<TestStorage>> {
    let stream_fn = scripted_stream_fn("done");
    let harness = test_harness(model.clone(), stream_fn);
    let mut all = vec![model];
    all.extend(other_models);
    let providers: BTreeSet<String> = all.iter().map(|m| m.provider.0.clone()).collect();
    let source = Arc::new(StaticModelSource::new(all, providers));
    Arc::new(RpcRuntimeHost::new(harness, source))
}

fn collector() -> (RpcOutputFn, Arc<Mutex<Vec<RpcResponse>>>) {
    let responses: Arc<Mutex<Vec<RpcResponse>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&responses);
    let output: RpcOutputFn = Arc::new(move |r| sink.lock().unwrap().push(r));
    (output, responses)
}

fn data_of(response: &RpcResponse) -> serde_json::Value {
    match &response.outcome {
        pirust_coding_agent::rpc::types::Outcome::Success { data } => {
            data.clone().unwrap_or(serde_json::Value::Null)
        }
        pirust_coding_agent::rpc::types::Outcome::Error { .. } => {
            panic!("expected success, got error: {response:?}")
        }
    }
}

fn error_of(response: &RpcResponse) -> String {
    match &response.outcome {
        pirust_coding_agent::rpc::types::Outcome::Error { error } => error.clone(),
        pirust_coding_agent::rpc::types::Outcome::Success { .. } => {
            panic!("expected error, got success: {response:?}")
        }
    }
}

#[tokio::test]
async fn get_state_matches_oracle_defaults() {
    let host = test_host(test_model("faux-1", false), vec![]);
    let (output, responses) = collector();
    handle_command(&host, None, RpcCommand::GetState, output).await;
    let data = data_of(&responses.lock().unwrap()[0]);
    // Defaults pinned by `tests/fixtures/pi/rpc/responses.corpus.jsonl`'s
    // `get_state` record: steeringMode/followUpMode "all", autoCompactionEnabled true.
    assert_eq!(data["steeringMode"], "all");
    assert_eq!(data["followUpMode"], "all");
    assert_eq!(data["autoCompactionEnabled"], true);
    assert_eq!(data["isStreaming"], false);
    assert_eq!(data["isCompacting"], false);
    assert_eq!(data["messageCount"], 0);
    assert_eq!(data["pendingMessageCount"], 0);
    assert_eq!(data["thinkingLevel"], "off");
}

#[tokio::test]
async fn set_model_success_and_not_found() {
    let m2 = test_model("faux-2", false);
    let host = test_host(test_model("faux-1", false), vec![m2.clone()]);

    let (output, responses) = collector();
    handle_command(
        &host,
        Some("1".into()),
        RpcCommand::SetModel {
            provider: "faux".into(),
            model_id: "faux-2".into(),
        },
        output,
    )
    .await;
    assert_eq!(data_of(&responses.lock().unwrap()[0])["id"], "faux-2");
    assert_eq!(host.harness.model().id, "faux-2");

    let (output, responses) = collector();
    handle_command(
        &host,
        Some("2".into()),
        RpcCommand::SetModel {
            provider: "faux".into(),
            model_id: "nope".into(),
        },
        output,
    )
    .await;
    assert_eq!(
        error_of(&responses.lock().unwrap()[0]),
        "Model not found: faux/nope"
    );
}

#[tokio::test]
async fn cycle_model_wraps_and_null_when_alone() {
    let m1 = test_model("faux-1", false);
    let m2 = test_model("faux-2", false);
    let host = test_host(m1.clone(), vec![m2.clone()]);

    let (output, responses) = collector();
    handle_command(&host, None, RpcCommand::CycleModel, output).await;
    assert_eq!(data_of(&responses.lock().unwrap()[0])["id"], "faux-2");

    let (output, responses) = collector();
    handle_command(&host, None, RpcCommand::CycleModel, output).await;
    assert_eq!(data_of(&responses.lock().unwrap()[0])["id"], "faux-1");

    // Alone in the catalog: cycle returns explicit JSON `null` (not an omitted key).
    let solo_host = test_host(test_model("faux-solo", false), vec![]);
    let (output, responses) = collector();
    handle_command(&solo_host, None, RpcCommand::CycleModel, output).await;
    let response = &responses.lock().unwrap()[0];
    match &response.outcome {
        pirust_coding_agent::rpc::types::Outcome::Success { data } => {
            assert_eq!(*data, Some(serde_json::Value::Null))
        }
        other => panic!("expected success(null), got {other:?}"),
    }
}

#[tokio::test]
async fn thinking_levels_follow_model_reasoning_capability() {
    let host = test_host(test_model("faux-1", true), vec![]);

    let (output, responses) = collector();
    handle_command(&host, None, RpcCommand::GetAvailableThinkingLevels, output).await;
    let levels = data_of(&responses.lock().unwrap()[0])["levels"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(levels.len(), 5);

    let (output, responses) = collector();
    handle_command(&host, None, RpcCommand::CycleThinkingLevel, output).await;
    assert_eq!(data_of(&responses.lock().unwrap()[0])["level"], "minimal");

    // A non-reasoning model only offers "off"; cycling has nothing to move to.
    let flat_host = test_host(test_model("faux-flat", false), vec![]);
    let (output, responses) = collector();
    handle_command(&flat_host, None, RpcCommand::CycleThinkingLevel, output).await;
    let is_null = matches!(
        responses.lock().unwrap()[0].outcome,
        pirust_coding_agent::rpc::types::Outcome::Success {
            data: Some(serde_json::Value::Null)
        }
    );
    assert!(is_null, "expected success(null)");

    let (output, _) = collector();
    handle_command(
        &host,
        None,
        RpcCommand::SetThinkingLevel {
            level: ThinkingLevel::High,
        },
        output,
    )
    .await;
    assert_eq!(
        host.harness.thinking_level(),
        pirust_agent_core::types::ThinkingLevel::High
    );
}

#[tokio::test]
async fn queue_modes_and_pending_count() {
    let host = test_host(test_model("faux-1", false), vec![]);

    let (output, _) = collector();
    handle_command(
        &host,
        None,
        RpcCommand::SetSteeringMode {
            mode: QueueMode::OneAtATime,
        },
        output,
    )
    .await;
    assert_eq!(host.steering_mode(), QueueMode::OneAtATime);

    let (output, _) = collector();
    handle_command(
        &host,
        None,
        RpcCommand::Steer {
            message: "steer msg".into(),
            images: None,
        },
        output,
    )
    .await;
    let (output, _) = collector();
    handle_command(
        &host,
        None,
        RpcCommand::FollowUp {
            message: "follow msg".into(),
            images: None,
        },
        output,
    )
    .await;

    let (output, responses) = collector();
    handle_command(&host, None, RpcCommand::GetState, output).await;
    assert_eq!(
        data_of(&responses.lock().unwrap()[0])["pendingMessageCount"],
        2
    );
}

#[tokio::test]
async fn session_name_rejects_empty_accepts_value() {
    let host = test_host(test_model("faux-1", false), vec![]);

    let (output, responses) = collector();
    handle_command(
        &host,
        None,
        RpcCommand::SetSessionName { name: "  ".into() },
        output,
    )
    .await;
    assert_eq!(
        error_of(&responses.lock().unwrap()[0]),
        "Session name cannot be empty"
    );

    let (output, _) = collector();
    handle_command(
        &host,
        None,
        RpcCommand::SetSessionName {
            name: "my session".into(),
        },
        output,
    )
    .await;
    assert_eq!(
        host.harness.session().get_name().unwrap(),
        Some("my session".to_string())
    );
}

#[tokio::test]
async fn get_entries_since_unknown_id_errors() {
    let host = test_host(test_model("faux-1", false), vec![]);
    let (output, responses) = collector();
    handle_command(
        &host,
        None,
        RpcCommand::GetEntries {
            since: Some("nope".into()),
        },
        output,
    )
    .await;
    assert_eq!(
        error_of(&responses.lock().unwrap()[0]),
        "Entry not found: nope"
    );
}

#[tokio::test]
async fn unsupported_commands_return_named_error_not_unknown_command() {
    let host = test_host(test_model("faux-1", false), vec![]);
    let (output, responses) = collector();
    handle_command(
        &host,
        None,
        RpcCommand::Bash {
            command: "echo hi".into(),
            exclude_from_context: None,
        },
        output,
    )
    .await;
    let message = error_of(&responses.lock().unwrap()[0]);
    assert!(message.contains("bash is not supported yet"));
    assert!(!message.starts_with("Unknown command"));
}

#[tokio::test]
async fn prompt_runs_end_to_end_and_is_queryable() {
    let host = test_host(test_model("faux-1", false), vec![]);
    let (output, responses) = collector();
    handle_command(
        &host,
        Some("p1".into()),
        RpcCommand::Prompt {
            message: "hello".into(),
            images: None,
            streaming_behavior: None,
        },
        output,
    )
    .await;
    // Immediate ack, matching Pi's preflight-success shape.
    assert!(matches!(
        responses.lock().unwrap()[0].outcome,
        pirust_coding_agent::rpc::types::Outcome::Success { data: None }
    ));

    // Wait for the spawned turn to settle.
    for _ in 0..200 {
        if host.harness.phase() == AgentHarnessPhase::Idle && !host.harness.messages().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let (output, responses) = collector();
    handle_command(&host, None, RpcCommand::GetLastAssistantText, output).await;
    assert_eq!(data_of(&responses.lock().unwrap()[0])["text"], "done");

    let (output, responses) = collector();
    handle_command(&host, None, RpcCommand::GetMessages, output).await;
    let messages = data_of(&responses.lock().unwrap()[0])["messages"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(messages.len(), 2); // user + assistant
}

/// The model id this `llama-server` will accept in a `/v1/messages` request,
/// or `None` when nothing is listening.
///
/// This test used to hardcode `"local"`, which worked only against
/// single-model `llama-server` builds — those ignore the request's `model`
/// field entirely. Router-mode builds (what `--hf-repo` starts now) resolve it
/// against the loaded model list and reject anything else with
/// `400 model 'local' not found` in ~3ms. That arrived here not as an obvious
/// connection error but as a *successful* prompt command followed by an
/// assistant message with zero content blocks, so the failure looked like a
/// model quirk rather than a bad request. Ask the server for the id instead of
/// guessing it.
///
/// Hand-rolled HTTP because the alternative is a `reqwest` dev-dependency for
/// one unauthenticated localhost GET.
fn live_model_id(addr: &std::net::SocketAddr) -> Option<String> {
    use std::io::{Read, Write};
    let mut stream = std::net::TcpStream::connect_timeout(addr, Duration::from_millis(500)).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    // No `Accept-Encoding`, so the body comes back uncompressed.
    stream
        .write_all(
            b"GET /v1/models HTTP/1.1\r\n\
              Host: 127.0.0.1\r\n\
              Accept: application/json\r\n\
              Connection: close\r\n\r\n",
        )
        .ok()?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).ok()?;
    let body = String::from_utf8_lossy(&raw);
    // The first `"id":"…"` in the `data` array is the loaded model.
    let start = body.find("\"id\":\"")? + "\"id\":\"".len();
    let rest = body.get(start..)?;
    let end = rest.find('"')?;
    let id = &rest[..end];
    if id.is_empty() {
        return None;
    }
    Some(id.to_string())
}

/// LIVE — drives a real turn against `ggml-org/Qwen3.5-0.8B-GGUF` at
/// `127.0.0.1:8080` (Anthropic-compatible `/v1/messages`) through the SAME
/// dispatch loop `pirust --mode rpc` will use once Wave 3 wires it into
/// `main.rs`. Skips (does not fail) when no server is reachable — the same
/// convention `scripts/gen-rpc-live-oracle.mjs` uses.
#[tokio::test]
async fn live_prompt_against_local_llama_server() {
    let addr: std::net::SocketAddr = "127.0.0.1:8080".parse().unwrap();
    let Some(model_id) = live_model_id(&addr) else {
        eprintln!("no server on 127.0.0.1:8080; skipping live RPC test (not a failure)");
        return;
    };
    eprintln!("live server model id: {model_id}");

    let model = Model {
        // Must be the id the server reports, not a placeholder — see `live_model_id`.
        id: model_id.clone(),
        name: model_id,
        api: Api::from("anthropic-messages"),
        provider: ProviderId::from("anthropic"),
        base_url: "http://127.0.0.1:8080".into(),
        reasoning: false,
        thinking_level_map: None,
        input: vec![Modality::Text],
        cost: ModelCost {
            rates: ModelCostRates {
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
            },
            tiers: None,
        },
        context_window: 32_000,
        max_tokens: 4096,
        headers: None,
        compat: None,
    };

    let stream_fn: StreamFn = Arc::new(move |model, ctx, mut opts: SimpleStreamOptions, _token| {
        // llama-server does not check this key, but our own client refuses to send a
        // request with none configured (matches Pi's real credential-resolution
        // behavior) — same placeholder `scripts/gen-rpc-live-oracle.mjs` uses.
        opts.base.api_key = Some("test-key-not-used-by-llama-server".to_string());
        pirust_ai::api::anthropic_messages::stream_simple(&model, &ctx, Some(opts))
    });

    let harness = test_harness(model.clone(), stream_fn);
    let source = Arc::new(StaticModelSource::new(
        vec![model],
        BTreeSet::from(["anthropic".to_string()]),
    ));
    let host = Arc::new(RpcRuntimeHost::new(harness, source));

    let (output, responses) = collector();
    handle_command(
        &host,
        Some("live-1".into()),
        RpcCommand::Prompt {
            message: "Reply with exactly the word PONG and nothing else.".into(),
            images: None,
            streaming_behavior: None,
        },
        output,
    )
    .await;
    assert!(matches!(
        responses.lock().unwrap()[0].outcome,
        pirust_coding_agent::rpc::types::Outcome::Success { .. }
    ));

    for _ in 0..3000 {
        if host.harness.phase() == AgentHarnessPhase::Idle && !host.harness.messages().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(host.harness.phase(), AgentHarnessPhase::Idle);

    // This tiny reasoning model (confirmed by manual `/v1/messages` probing) spends its
    // entire token budget on a `thinking` block for a trivial prompt and never reaches
    // visible text, even with `max_tokens` raised well past what a real answer would need
    // and `thinking: {type: "disabled"}` sent — a model/server characteristic, not a
    // pirust bug (`get_last_assistant_text` correctly returns "" when there is genuinely
    // no text block). The meaningful live assertions are that the round trip happened at
    // all: a real HTTP turn against a real server produced a real persisted message.
    let (output, responses) = collector();
    handle_command(&host, None, RpcCommand::GetLastAssistantText, output).await;
    let text = data_of(&responses.lock().unwrap()[0])["text"]
        .as_str()
        .unwrap()
        .to_string();
    eprintln!(
        "live model replied (text blocks only, may be empty for this reasoning model): {text:?}"
    );

    let (output, responses) = collector();
    handle_command(&host, None, RpcCommand::GetMessages, output).await;
    let messages = data_of(&responses.lock().unwrap()[0])["messages"]
        .as_array()
        .unwrap()
        .clone();
    eprintln!(
        "live model full messages: {}",
        serde_json::to_string_pretty(&messages).unwrap()
    );
    assert_eq!(
        messages.len(),
        2,
        "expected a persisted user + assistant message"
    );
    assert_eq!(messages[1]["role"], "assistant");
    assert!(
        !messages[1]["content"].as_array().unwrap().is_empty(),
        "expected the real model's assistant message to carry at least one content block"
    );
}
