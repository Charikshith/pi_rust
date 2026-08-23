//! feat-009 Wave 1 goldens: replay the framing cases captured from REAL Pi
//! (`packages/protocol/src/framing.ts`) by `scripts/gen-orchestrator-oracle.mjs`.

use pirust_orchestrator::protocol::framing::{assert_complete_frame, encode_frame, FrameDecoder};
use serde_json::Value as Json;

fn fixture_lines() -> Vec<Json> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/pi/orchestrator")
        .join("framing.cases.jsonl");
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

fn max_frame_length(record: &Json) -> Option<u64> {
    record["maxFrameLength"].as_u64()
}

#[test]
fn framing_cases_match_real_pi() {
    let records = fixture_lines();
    assert!(!records.is_empty(), "fixture should not be empty");

    let mut counted = 0;
    for record in &records {
        let description = record["description"].as_str().unwrap();
        match record["kind"].as_str().unwrap() {
            "encode_frame" => {
                counted += 1;
                let payload = from_hex(record["payloadHex"].as_str().unwrap());
                let expected = record["frameHex"].as_str().unwrap();
                let frame = encode_frame(&payload).unwrap();
                assert_eq!(
                    to_hex(&frame),
                    expected,
                    "encode_frame mismatch for {description:?}"
                );
            }
            "assert_complete" => {
                counted += 1;
                let frame = from_hex(record["frameHex"].as_str().unwrap());
                let ok = record["ok"].as_bool().unwrap();
                let result = assert_complete_frame(&frame, max_frame_length(record));
                assert_eq!(
                    result.is_ok(),
                    ok,
                    "assert_complete_frame mismatch for {description:?}"
                );
            }
            "decoder_push" => {
                counted += 1;
                let chunks: Vec<Vec<u8>> = record["chunksHex"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|h| from_hex(h.as_str().unwrap()))
                    .collect();
                let mut decoder = FrameDecoder::new(max_frame_length(record));
                let mut frames = Vec::new();
                let mut push_failed = false;
                for chunk in &chunks {
                    match decoder.push(chunk) {
                        Ok(mut produced) => frames.append(&mut produced),
                        Err(_) => {
                            push_failed = true;
                            break;
                        }
                    }
                }
                let expected_push_throws = record["pushThrows"].as_bool().unwrap_or(false);
                assert_eq!(
                    push_failed, expected_push_throws,
                    "push-throw mismatch for {description:?}"
                );
                if !push_failed {
                    let expected_frames: Vec<String> = record["framesHex"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|h| h.as_str().unwrap().to_string())
                        .collect();
                    let got_frames: Vec<String> = frames.iter().map(|f| to_hex(f)).collect();
                    assert_eq!(
                        got_frames, expected_frames,
                        "frames mismatch for {description:?}"
                    );

                    let expected_end_throws = record["endThrows"].as_bool().unwrap_or(false);
                    let end_result = decoder.end();
                    assert_eq!(
                        end_result.is_err(),
                        expected_end_throws,
                        "end() mismatch for {description:?}"
                    );
                }
            }
            other => panic!("unknown record kind {other}"),
        }
    }

    assert!(
        counted >= 15,
        "expected a substantial framing battery, got {counted}"
    );
}

#[test]
fn oversized_declared_length_latches_a_permanently_failed_decoder() {
    let mut decoder = FrameDecoder::new(Some(3));
    assert!(decoder.push(&[0, 0, 0, 4]).is_err());
    // Second push must also fail — the decoder never recovers.
    assert!(decoder.push(&[1]).is_err());
}
