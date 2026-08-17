//! Pi oracle for [`pirust_tui::fuzzy`] (feat-006 Wave 3).
//!
//! Replays every record of `tests/fixtures/pi/tui/fuzzy.cases.jsonl` —
//! captured by executing real Pi's `packages/tui/src/fuzzy.ts` — and asserts
//! identical match/score/filter-order results.

use std::path::PathBuf;

use pirust_tui::fuzzy::{fuzzy_filter, fuzzy_match};
use serde_json::Value;

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/pi/tui/fuzzy.cases.jsonl")
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

// Both sides are IEEE-754 doubles produced by the same simple arithmetic
// (additions/subtractions of small multiples); a tight epsilon guards
// against a JSON-round-trip formatting difference without masking a real
// scoring-logic divergence.
const EPS: f64 = 1e-9;

#[test]
fn every_fuzzy_record_matches_pi() {
    let records = load_records();
    assert_eq!(
        records.len(),
        19,
        "fixture record count changed — update this assertion deliberately"
    );

    let mut failures = Vec::new();
    for record in &records {
        let note = record["note"].as_str().unwrap();
        let fn_name = record["fn"].as_str().unwrap();

        if fn_name == "fuzzyMatch" {
            let query = record["query"].as_str().unwrap();
            let text = record["text"].as_str().unwrap();
            let expected_matches = record["result"]["matches"].as_bool().unwrap();
            let expected_score = record["result"]["score"].as_f64().unwrap();

            let actual = fuzzy_match(query, text);
            if actual.matches != expected_matches
                || (actual.matches && (actual.score - expected_score).abs() > EPS)
            {
                failures.push(format!(
                    "[{note}] fuzzyMatch({query:?}, {text:?})\n  expected: matches={expected_matches} score={expected_score}\n  actual:   matches={} score={}",
                    actual.matches, actual.score
                ));
            }
        } else if fn_name == "fuzzyFilter" {
            let items: Vec<String> = record["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap().to_string())
                .collect();
            let query = record["query"].as_str().unwrap();
            let expected: Vec<String> = record["result"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap().to_string())
                .collect();

            let actual: Vec<String> = fuzzy_filter(&items, query, |s| s.clone())
                .into_iter()
                .cloned()
                .collect();
            if actual != expected {
                failures.push(format!(
                    "[{note}] fuzzyFilter({items:?}, {query:?})\n  expected: {expected:?}\n  actual:   {actual:?}"
                ));
            }
        } else {
            panic!("no Rust dispatch wired for oracle fn {fn_name:?}");
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));
}
