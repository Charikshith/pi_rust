//! Pi oracle for the pure helpers in [`pirust_tui::terminal`] (feat-006
//! Wave 4). `ProcessTerminal`'s live-I/O plumbing has no oracle (see that
//! module's docs) — this covers `parseKeyboardProtocolNegotiationSequence`
//! and `normalizeAppleTerminalInput` only, replaying
//! `tests/fixtures/pi/tui/terminal.cases.jsonl`.

use std::path::PathBuf;

use pirust_tui::terminal::{
    normalize_apple_terminal_input, parse_keyboard_protocol_negotiation_sequence,
    KeyboardProtocolNegotiationSequence,
};
use serde_json::Value;

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/pi/tui/terminal.cases.jsonl")
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

fn negotiation_to_value(seq: Option<KeyboardProtocolNegotiationSequence>) -> Value {
    match seq {
        Some(KeyboardProtocolNegotiationSequence::KittyFlags(flags)) => {
            serde_json::json!({ "type": "kitty-flags", "flags": flags })
        }
        Some(KeyboardProtocolNegotiationSequence::DeviceAttributes) => {
            serde_json::json!({ "type": "device-attributes" })
        }
        None => Value::Null,
    }
}

#[test]
fn every_terminal_record_matches_pi() {
    let records = load_records();
    assert_eq!(
        records.len(),
        9,
        "fixture record count changed — update this assertion deliberately"
    );

    let mut failures = Vec::new();
    for record in &records {
        let note = record["note"].as_str().unwrap();
        let fn_name = record["fn"].as_str().unwrap();
        let args = record["args"].as_array().unwrap();
        let expected = &record["result"];

        let actual: Value = match fn_name {
            "parseKeyboardProtocolNegotiationSequence" => {
                let data = args[0].as_str().unwrap();
                negotiation_to_value(parse_keyboard_protocol_negotiation_sequence(data))
            }
            "normalizeAppleTerminalInput" => {
                let data = args[0].as_str().unwrap();
                let is_apple = args[1].as_bool().unwrap();
                let is_shift = args[2].as_bool().unwrap();
                Value::String(normalize_apple_terminal_input(data, is_apple, is_shift))
            }
            other => panic!("unknown fn {other}"),
        };

        if &actual != expected {
            failures.push(format!(
                "[{note}] {fn_name}\n  expected: {expected}\n  actual:   {actual}"
            ));
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));
}
