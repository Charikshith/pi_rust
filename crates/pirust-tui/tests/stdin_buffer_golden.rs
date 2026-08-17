//! Pi oracle for [`pirust_tui::stdin_buffer`] (feat-006 Wave 2).
//!
//! Replays every record of `tests/fixtures/pi/tui/stdin-buffer.cases.jsonl` —
//! captured by driving real Pi's `packages/tui/src/stdin-buffer.ts`
//! `StdinBuffer` class — and asserts the same ordered event sequence.

use std::path::PathBuf;

use pirust_tui::stdin_buffer::{StdinBuffer, StdinBufferOptions, StdinEvent};
use serde_json::Value;

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/pi/tui/stdin-buffer.cases.jsonl")
}

fn load_records() -> Vec<Value> {
    let path = fixture_path();
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read fixture {}: {error}", path.display()));
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .enumerate()
        .map(|(index, line)| {
            serde_json::from_str(line)
                .unwrap_or_else(|error| panic!("line {}: {error}\n  {line}", index + 1))
        })
        .collect()
}

fn call_bytes(data: &Value) -> Vec<u8> {
    match data {
        Value::String(s) => s.as_bytes().to_vec(),
        Value::Array(items) => items.iter().map(|v| v.as_u64().unwrap() as u8).collect(),
        other => panic!("unexpected call.data shape: {other}"),
    }
}

fn run_record(calls: &[Value]) -> Vec<(String, String)> {
    let mut buf = StdinBuffer::new(StdinBufferOptions::default());
    let mut events = Vec::new();
    for call in calls {
        let op = call["op"].as_str().unwrap();
        match op {
            "process" => {
                let bytes = call_bytes(&call["data"]);
                for event in buf.process(&bytes) {
                    match event {
                        StdinEvent::Data(v) => events.push(("data".to_string(), v)),
                        StdinEvent::Paste(v) => events.push(("paste".to_string(), v)),
                    }
                }
            }
            "flush" => {
                for seq in buf.flush() {
                    events.push(("data".to_string(), seq));
                }
            }
            other => panic!("unknown call.op {other:?}"),
        }
    }
    events
}

#[test]
fn every_stdin_buffer_record_matches_pi() {
    let records = load_records();
    assert_eq!(
        records.len(),
        23,
        "fixture record count changed — update this assertion deliberately"
    );

    let mut failures = Vec::new();
    for record in &records {
        let note = record["note"].as_str().unwrap();
        let calls = record["calls"].as_array().unwrap();
        let expected: Vec<(String, String)> = record["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| {
                (
                    e["type"].as_str().unwrap().to_string(),
                    e["value"].as_str().unwrap().to_string(),
                )
            })
            .collect();

        let actual = run_record(calls);
        if actual != expected {
            failures.push(format!(
                "[{note}]\n  expected: {expected:?}\n  actual:   {actual:?}"
            ));
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));
}
