//! Pi oracle for [`pirust_tui::Input`] (feat-006 Wave 5).
//!
//! Replays every record of `tests/fixtures/pi/tui/input.cases.jsonl` —
//! captured by executing real Pi's `packages/tui/src/components/input.ts`
//! — and asserts identical final value + rendered output, including the
//! UTF-16-cursor-arithmetic hazard cases (astral-plane grapheme backspace)
//! named in `input.rs`'s module docs.

use std::path::PathBuf;

use pirust_tui::tui::Component;
use pirust_tui::Input;
use serde_json::Value;

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/pi/tui/input.cases.jsonl")
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

#[test]
fn every_input_record_matches_pi() {
    let records = load_records();
    assert_eq!(
        records.len(),
        9,
        "fixture record count changed — update this assertion deliberately"
    );

    let mut failures = Vec::new();
    for record in &records {
        let note = record["note"].as_str().unwrap();
        let ops = record["ops"].as_array().unwrap();
        let width = record["width"].as_u64().unwrap() as usize;

        let mut input = Input::new();
        for op in ops {
            match op["op"].as_str().unwrap() {
                "handleInput" => input.handle_input(op["data"].as_str().unwrap()),
                "setValue" => input.set_value(op["value"].as_str().unwrap()),
                other => panic!("no Rust dispatch wired for oracle op {other:?}"),
            }
        }

        let expected_event = &record["events"][0];
        let expected_value = expected_event["value"].as_str().unwrap();
        let expected_render: Vec<String> = expected_event["render"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();

        let actual_value = input.get_value().to_string();
        if actual_value != expected_value {
            failures.push(format!(
                "[{note}] value\n  expected: {expected_value:?}\n  actual:   {actual_value:?}"
            ));
        }
        let actual_render = input.render(width);
        if actual_render != expected_render {
            failures.push(format!(
                "[{note}] render\n  expected: {expected_render:?}\n  actual:   {actual_render:?}"
            ));
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));
}
