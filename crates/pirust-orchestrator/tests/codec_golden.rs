//! feat-009 Wave 2/4 goldens: replay the codec/envelope cases captured from
//! REAL Pi (`packages/protocol/src/codec.ts`) by
//! `scripts/gen-orchestrator-oracle.mjs`. Wave 2 captured 4 records tagged
//! `"scope":"deferred"` for Pi's deep `Command`/`CommandResult`/
//! `ServerEvent`/`SessionMetadata` shape validation, which Wave 2 didn't yet
//! implement — Wave 4 built that typing (see `schemas.rs`), so those
//! records are now asserted normally like everything else, not skipped.

use pirust_orchestrator::protocol::codec::{
    encode_client_message, encode_server_message, is_supported_protocol_version,
    ClientMessageDecoder, ServerMessageDecoder,
};
use pirust_orchestrator::protocol::schemas::{ClientMessage, ProtocolJson, ServerMessage};
use serde_json::Value as Json;

fn fixture_lines() -> Vec<Json> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/pi/orchestrator")
        .join("codec.cases.jsonl");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!("missing fixture {path:?}: {e} — run scripts/gen-orchestrator-oracle.mjs")
    });
    text.lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn from_hex(hex: &str) -> Vec<u8> {
    assert_eq!(hex.len() % 2, 0, "odd hex length: {hex}");
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect()
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Converts a `serde_json::Value` into [`ProtocolJson`] (the oracle script's
/// plain-JSON fixtures never contain a byte string, so this conversion is
/// infallible in practice — this test would rather panic loudly on a
/// surprising fixture shape than silently misinterpret one).
fn json_to_protocol(value: &Json) -> ProtocolJson {
    match value {
        Json::Null => ProtocolJson::Null,
        Json::Bool(b) => ProtocolJson::Bool(*b),
        Json::Number(n) => {
            ProtocolJson::Number(n.as_f64().expect("fixture numbers are always finite"))
        }
        Json::String(s) => ProtocolJson::Text(s.clone()),
        Json::Array(items) => ProtocolJson::Array(items.iter().map(json_to_protocol).collect()),
        Json::Object(map) => ProtocolJson::Map(
            map.iter()
                .map(|(k, v)| (k.clone(), json_to_protocol(v)))
                .collect(),
        ),
    }
}

#[test]
fn codec_cases_match_real_pi() {
    let records = fixture_lines();
    assert!(!records.is_empty(), "fixture should not be empty");

    let mut checked = 0;

    for record in &records {
        let description = record["description"].as_str().unwrap();
        match record["kind"].as_str().unwrap() {
            "is_supported_version" => {
                checked += 1;
                assert!(is_supported_protocol_version(1.0), "{description}");
                assert!(!is_supported_protocol_version(2.0), "{description}");
                assert!(!is_supported_protocol_version(2.5), "{description}");
            }
            "client_parse_ok" => {
                checked += 1;
                let json = json_to_protocol(&record["message"]);
                let message =
                    ClientMessage::parse(&json).unwrap_or_else(|e| panic!("{description}: {e}"));
                let expected_hex = record["hex"].as_str().unwrap();
                let encoded = encode_client_message(&message, None)
                    .unwrap_or_else(|e| panic!("{description}: {e}"));
                assert_eq!(to_hex(&encoded), expected_hex, "{description}");
            }
            "client_parse_reject" => {
                checked += 1;
                let json = json_to_protocol(&record["message"]);
                assert!(
                    ClientMessage::parse(&json).is_err(),
                    "expected rejection: {description}"
                );
            }
            "server_parse_ok" => {
                checked += 1;
                let json = json_to_protocol(&record["message"]);
                let message =
                    ServerMessage::parse(&json).unwrap_or_else(|e| panic!("{description}: {e}"));
                let expected_hex = record["hex"].as_str().unwrap();
                let encoded = encode_server_message(&message, None)
                    .unwrap_or_else(|e| panic!("{description}: {e}"));
                assert_eq!(to_hex(&encoded), expected_hex, "{description}");
            }
            "server_parse_reject" => {
                checked += 1;
                let json = json_to_protocol(&record["message"]);
                assert!(
                    ServerMessage::parse(&json).is_err(),
                    "expected rejection: {description}"
                );
            }
            "outbound_frame_limit" => {
                checked += 1;
                let json = json_to_protocol(&record["message"]);
                let max = record["maxFrameLength"].as_u64();
                let result = if record["side"] == "client" {
                    encode_client_message(&ClientMessage::parse(&json).unwrap(), max).map(|_| ())
                } else {
                    encode_server_message(&ServerMessage::parse(&json).unwrap(), max).map(|_| ())
                };
                assert!(
                    result.is_err(),
                    "expected outbound frame-limit rejection: {description}"
                );
            }
            "incremental_client_decode" => {
                checked += 1;
                let wire = from_hex(record["wireHex"].as_str().unwrap());
                let expected: Vec<ClientMessage> = record["expectedMessages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|m| ClientMessage::parse(&json_to_protocol(m)).unwrap())
                    .collect();
                for split in record["splits"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|s| s.as_u64().unwrap() as usize)
                {
                    let mut decoder = ClientMessageDecoder::new(None);
                    let mut messages = decoder.push(&wire[..split]).unwrap();
                    messages.extend(decoder.push(&wire[split..]).unwrap());
                    decoder.end().unwrap();
                    assert_eq!(messages, expected, "{description} at split {split}");
                }
            }
            "framed_client_reject" => {
                checked += 1;
                let frame = from_hex(record["frameHex"].as_str().unwrap());
                let mut decoder = ClientMessageDecoder::new(None);
                assert!(
                    decoder.push(&frame).is_err(),
                    "expected rejection: {description}"
                );
                let second = decoder.push(&[]);
                assert!(
                    matches!(&second, Err(e) if e.0.to_lowercase().contains("failed")),
                    "expected a latched 'failed' error on the next push: {description} (got {second:?})"
                );
            }
            "server_decoder_truncated" => {
                checked += 1;
                let chunk = from_hex(record["chunkHex"].as_str().unwrap());
                let mut decoder = ServerMessageDecoder::new(None);
                let pushed = decoder.push(&chunk).unwrap();
                assert!(pushed.is_empty(), "{description}");
                assert!(
                    decoder.end().is_err(),
                    "expected end() rejection: {description}"
                );
            }
            "client_decoder_oversized" => {
                checked += 1;
                let chunk = from_hex(record["chunkHex"].as_str().unwrap());
                let max = record["maxFrameLength"].as_u64();
                let mut decoder = ClientMessageDecoder::new(max);
                assert!(
                    decoder.push(&chunk).is_err(),
                    "expected rejection: {description}"
                );
            }
            other => panic!("unknown record kind {other}"),
        }
    }

    assert!(
        checked >= 24,
        "expected a substantial wave-2+4 battery, got {checked}"
    );
}

#[test]
fn client_message_decoder_latches_after_end_failure() {
    let mut decoder = ClientMessageDecoder::new(None);
    // Truncated header: push succeeds with no messages, end() fails and latches.
    decoder.push(&[0, 0, 0]).unwrap();
    assert!(decoder.end().is_err());
    assert!(decoder.push(&[]).is_err());
    assert!(decoder.end().is_err());
}
