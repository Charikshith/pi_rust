//! Pi oracle for [`pirust_tui::TruncatedText`] (feat-006 Wave 5).
//!
//! Replays every record of `tests/fixtures/pi/tui/truncated-text.cases.jsonl`
//! — captured by executing real Pi's
//! `packages/tui/src/components/truncated-text.ts` — and asserts identical
//! rendered output.

use std::path::PathBuf;

use pirust_tui::tui::Component;
use pirust_tui::TruncatedText;
use serde_json::Value;

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/pi/tui/truncated-text.cases.jsonl")
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
fn every_truncated_text_record_matches_pi() {
    let records = load_records();
    assert_eq!(
        records.len(),
        6,
        "fixture record count changed — update this assertion deliberately"
    );

    let mut failures = Vec::new();
    for record in &records {
        let note = record["note"].as_str().unwrap();
        let text = record["text"].as_str().unwrap();
        let padding_x = record["paddingX"].as_u64().unwrap() as usize;
        let padding_y = record["paddingY"].as_u64().unwrap() as usize;
        let width = record["width"].as_u64().unwrap() as usize;
        let expected: Vec<String> = record["result"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();

        let mut t = TruncatedText::new(text, padding_x, padding_y);
        let actual = t.render(width);
        if actual != expected {
            failures.push(format!(
                "[{note}]\n  expected: {expected:?}\n  actual:   {actual:?}"
            ));
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));
}
