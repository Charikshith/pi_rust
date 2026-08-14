//! Golden byte-compatibility tests for the `anthropic-messages` streaming adapter.
//!
//! Each scenario feeds a committed raw SSE fixture through the adapter via an injected
//! [`CannedTransport`] (the Rust equivalent of Pi's fake `Anthropic` client / `asResponse()`),
//! collects the emitted event tape plus the final `AssistantMessage`, and compares them against
//! goldens captured from Pi (`docs/analysis/06-anthropic-runtime-spec.md` §Oracle).
//!
//! ## Final message compared to Pi's LITERAL captured bytes
//!
//! The final `AssistantMessage` is compared BYTE-FOR-BYTE against `<name>.final.golden`, which
//! holds the exact compact `JSON.stringify(final)` bytes Pi's adapter produced (timestamps
//! normalized to 0). We deliberately do NOT deserialize `expected["final"]` into the Rust struct
//! and re-serialize it: that round-trip is self-referential — any field the struct silently
//! drops/reorders would be dropped/reordered on BOTH sides, letting a divergent port pass. Reading
//! the raw golden string pins float formatting (e.g. `7.750625`, `0.000037000000000000005`) and
//! key order (`…timestamp,responseId`; usage `…cost,cacheWrite1h`) against Pi's real output.
//! The tape type-sequence and stable-field checks still compare against the raw `expected` tape
//! `Value`s in `<name>.expected.json`.
//!
//! ## Why the mid-stream `partial` snapshot is NOT byte-compared
//!
//! Pi builds ONE mutable `output` object and pushes it *by reference* into every event, so its
//! captured tape has every `partial` field pointing at the SAME object — i.e. the FINAL mutated
//! state (JS reference aliasing). A faithful Rust port cannot alias like that; it emits an OWNED
//! incremental snapshot cloned at each emission, which correctly reflects the message state *at
//! that point in the stream* and therefore differs from Pi's retroactively-final snapshots. This
//! divergence is expected and documented in the spec (§4b) and the adapter module. Consequently
//! we assert the tape's event `type` sequence and the emission-stable payload fields
//! (`contentIndex`, `delta`, `content`, `toolCall`, `reason`) but skip `partial`. The TERMINAL
//! event's `done.message` / `error.error` IS byte-compared, since it equals the final message.

use std::path::PathBuf;

use futures::StreamExt;
use pirust_ai::api::anthropic_messages::stream;
use pirust_ai::api::AnthropicOptions;
use pirust_ai::http::CannedTransport;
use pirust_ai::types::event::AssistantMessageEvent;
use pirust_ai::types::message::{AssistantMessage, Context};
use pirust_ai::types::model::Model;
use serde_json::Value;

/// Absolute path to the vendored Pi fixtures for a given file name.
fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/pi/anthropic")
        .join(name)
}

/// Look up a `Model` from the vendored `models.corpus.jsonl` by (provider, id) — the same
/// catalog values Pi's `getModel` used, including cost rates.
fn lookup_model(provider: &str, model_id: &str) -> Model {
    let corpus = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/pi/models.corpus.jsonl");
    let text = std::fs::read_to_string(&corpus).expect("read models corpus");
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(line).expect("parse corpus line");
        let matches = value.get("provider").and_then(Value::as_str) == Some(provider)
            && value.get("id").and_then(Value::as_str) == Some(model_id);
        if matches {
            return serde_json::from_value(value).expect("deserialize Model");
        }
    }
    panic!("model not found in corpus: provider={provider} id={model_id}");
}

/// Serialize an `AssistantMessage` with a zeroed timestamp — the direct struct serialization
/// (via the `jsnum` `serialize_with` hook) is what pins JS-compatible float formatting and the
/// runtime key-insertion order. Round-tripping through `serde_json::Value` would reformat floats.
fn message_string(mut message: AssistantMessage) -> String {
    message.timestamp = 0;
    serde_json::to_string(&message).expect("serialize message")
}

/// Extract the terminal event's `AssistantMessage` (`done.message` / `error.error`).
fn terminal_message(event: &AssistantMessageEvent) -> AssistantMessage {
    match event {
        AssistantMessageEvent::Done { message, .. } => message.clone(),
        AssistantMessageEvent::Error { error, .. } => error.clone(),
        other => panic!("last tape event is not terminal: {other:?}"),
    }
}

/// Compare the emission-stable payload fields of one tape event against the golden.
fn assert_stable_fields(index: usize, actual: &Value, expected: &Value) {
    assert_eq!(
        actual.get("type"),
        expected.get("type"),
        "tape[{index}] type mismatch"
    );
    for field in ["contentIndex", "delta", "content", "toolCall", "reason"] {
        if let Some(expected_value) = expected.get(field) {
            assert_eq!(
                actual.get(field),
                Some(expected_value),
                "tape[{index}] field `{field}` mismatch"
            );
        }
    }
}

/// Drive one golden scenario end to end.
async fn run_scenario(name: &str) {
    // --- request: {provider, modelId, context, options} ---
    let request_text = std::fs::read_to_string(fixture_path(&format!("{name}.request.json")))
        .expect("read request");
    let request: Value = serde_json::from_str(&request_text).expect("parse request");
    let provider = request["provider"].as_str().expect("provider");
    let model_id = request["modelId"].as_str().expect("modelId");
    let model = lookup_model(provider, model_id);
    let context: Context =
        serde_json::from_value(request["context"].clone()).expect("deserialize Context");

    // --- canned SSE transport (the oracle double for options.client) ---
    let sse_bytes =
        std::fs::read_to_string(fixture_path(&format!("{name}.sse"))).expect("read sse");
    let options = AnthropicOptions::default().with_transport(CannedTransport::new(sse_bytes));

    // --- run the adapter, collecting the tape and the final message ---
    let mut event_stream = stream(&model, &context, Some(options));
    let mut tape: Vec<AssistantMessageEvent> = Vec::new();
    while let Some(event) = event_stream.next().await {
        tape.push(event);
    }
    let final_message = event_stream.result().await;

    // --- expected: {tape, final} ---
    let expected_text = std::fs::read_to_string(fixture_path(&format!("{name}.expected.json")))
        .expect("read expected");
    let expected: Value = serde_json::from_str(&expected_text).expect("parse expected");
    let expected_tape = expected["tape"].as_array().expect("expected tape array");

    // --- FINAL message: byte-identical to Pi's LITERAL captured bytes ---
    // Compare the adapter's serialized final message directly against the exact
    // bytes Pi's adapter emitted (`<name>.final.golden`), NOT a value we
    // deserialize-and-reserialize through the Rust struct. Round-tripping the
    // expected through `AssistantMessage` would silently mask any field the
    // struct drops/reorders (it would be dropped/reordered on both sides); the
    // raw-string comparison catches float-format, key-order, and dropped/extra
    // -field divergences against Pi's real output.
    let expected_final = std::fs::read_to_string(fixture_path(&format!("{name}.final.golden")))
        .expect("read final golden");
    let actual_final = message_string(final_message);
    assert_eq!(
        actual_final, expected_final,
        "[{name}] final message must be byte-identical to Pi's literal captured bytes"
    );

    // --- TAPE: same length, same type sequence, stable fields; skip `partial` (see module doc) ---
    assert_eq!(
        tape.len(),
        expected_tape.len(),
        "[{name}] tape length mismatch"
    );
    for (index, (event, expected_event)) in tape.iter().zip(expected_tape).enumerate() {
        let actual_value = serde_json::to_value(event).expect("event to value");
        assert_stable_fields(index, &actual_value, expected_event);
    }

    // --- TERMINAL event's message/error is byte-identical to Pi's literal final bytes ---
    let terminal = terminal_message(tape.last().expect("non-empty tape"));
    assert_eq!(
        message_string(terminal),
        expected_final,
        "[{name}] terminal event message must equal Pi's literal final bytes byte-for-byte"
    );
}

#[tokio::test]
async fn golden_text_basic() {
    run_scenario("text-basic").await;
}

#[tokio::test]
async fn golden_toolcall_repair() {
    run_scenario("toolcall-repair").await;
}

#[tokio::test]
async fn golden_refusal_error() {
    run_scenario("refusal-error").await;
}

#[tokio::test]
async fn golden_cache_write_1h() {
    run_scenario("cache-write-1h").await;
}

#[tokio::test]
async fn golden_no_usage_in_delta() {
    run_scenario("no-usage-in-delta").await;
}
