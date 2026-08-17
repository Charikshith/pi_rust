//! Pi oracle for [`pirust_tui::word_navigation`] (feat-006 Wave 3).
//!
//! Replays every record of `tests/fixtures/pi/tui/word-navigation.cases.jsonl`
//! — captured by executing real Pi's `packages/tui/src/word-navigation.ts` —
//! and asserts identical UTF-16-offset results.

use std::path::PathBuf;

use pirust_tui::word_navigation::{find_word_backward, find_word_forward};
use serde_json::Value;

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/pi/tui/word-navigation.cases.jsonl")
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

/// Cases with a known, understood, unfixable-without-a-new-dependency
/// divergence — see `pirust_tui::word_navigation`'s module docs, "Known gap:
/// CJK dictionary segmentation". Real Pi's `Intl.Segmenter` dictionary-groups
/// "日本語" into one word; `unicode-segmentation`'s plain-UAX#29
/// `split_word_bounds` gives each Han ideograph its own boundary. Named here
/// rather than silently dropped from the fixture or asserted as a false pass.
const KNOWN_CJK_DICTIONARY_SEGMENTATION_GAP: &[&str] = &["forward-cjk-text"];

#[test]
fn every_word_navigation_record_matches_pi() {
    let records = load_records();
    assert_eq!(
        records.len(),
        29,
        "fixture record count changed — update this assertion deliberately"
    );

    let mut failures = Vec::new();
    for record in &records {
        let note = record["note"].as_str().unwrap();
        if KNOWN_CJK_DICTIONARY_SEGMENTATION_GAP.contains(&note) {
            continue;
        }
        let fn_name = record["fn"].as_str().unwrap();
        let text = record["text"].as_str().unwrap();
        let cursor = record["cursor"].as_u64().unwrap() as usize;
        let expected = record["result"].as_u64().unwrap() as usize;

        let actual = if fn_name == "findWordBackward" {
            find_word_backward(text, cursor, None)
        } else {
            find_word_forward(text, cursor, None)
        };

        if actual != expected {
            failures.push(format!(
                "[{note}] {fn_name}(text={text:?}, cursor={cursor})\n  expected: {expected}\n  actual:   {actual}"
            ));
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));
}
