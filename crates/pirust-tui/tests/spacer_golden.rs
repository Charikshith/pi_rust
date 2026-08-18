//! Pi oracle for [`pirust_tui::Spacer`] (feat-006 Wave 5).
//!
//! Replays every record of `tests/fixtures/pi/tui/spacer.cases.jsonl` —
//! captured by executing real Pi's `packages/tui/src/components/spacer.ts`
//! — and asserts identical rendered output.

use std::path::PathBuf;

use pirust_tui::tui::Component;
use pirust_tui::Spacer;
use serde_json::Value;

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/pi/tui/spacer.cases.jsonl")
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
fn every_spacer_record_matches_pi() {
    let records = load_records();
    assert_eq!(
        records.len(),
        3,
        "fixture record count changed — update this assertion deliberately"
    );

    let mut failures = Vec::new();
    for record in &records {
        let note = record["note"].as_str().unwrap();
        let lines = record["lines"].as_u64().unwrap() as usize;
        let width = record["width"].as_u64().unwrap() as usize;
        let expected: Vec<String> = record["result"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();

        let mut s = Spacer::new(lines);
        let actual = s.render(width);
        if actual != expected {
            failures.push(format!("[{note}]\n  expected: {expected:?}\n  actual:   {actual:?}"));
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));
}
