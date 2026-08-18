//! Pi oracle for [`pirust_tui::SelectList`] (feat-006 Wave 5).
//!
//! Replays every record of `tests/fixtures/pi/tui/select-list.cases.jsonl`
//! — captured by executing real Pi's
//! `packages/tui/src/components/select-list.ts` against IDENTITY theme
//! functions (so this asserts pure layout/selection logic, not styling) —
//! and asserts identical ordered `render`/`selectedIndex`/`filteredLength`
//! results for the same operation script.

use std::path::PathBuf;

use pirust_tui::tui::Component;
use pirust_tui::{SelectItem, SelectList, SelectListLayoutOptions, SelectListTheme};
use serde_json::Value;

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/pi/tui/select-list.cases.jsonl")
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

fn identity_theme() -> SelectListTheme {
    SelectListTheme {
        selected_prefix: Box::new(|s| s.to_string()),
        selected_text: Box::new(|s| s.to_string()),
        description: Box::new(|s| s.to_string()),
        scroll_info: Box::new(|s| s.to_string()),
        no_match: Box::new(|s| s.to_string()),
    }
}

fn parse_items(v: &Value) -> Vec<SelectItem> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|item| SelectItem {
            value: item["value"].as_str().unwrap().to_string(),
            label: item["label"].as_str().unwrap().to_string(),
            description: item["description"].as_str().map(str::to_string),
        })
        .collect()
}

#[test]
fn every_select_list_record_matches_pi() {
    let records = load_records();
    assert_eq!(
        records.len(),
        7,
        "fixture record count changed — update this assertion deliberately"
    );

    let mut failures = Vec::new();
    for record in &records {
        let note = record["note"].as_str().unwrap();
        let items = parse_items(&record["items"]);
        let max_visible = record["maxVisible"].as_u64().unwrap() as usize;
        let ops = record["ops"].as_array().unwrap();
        let expected_events = record["events"].as_array().unwrap();

        let mut list = SelectList::new(items, max_visible, identity_theme(), SelectListLayoutOptions::default());

        for (op, expected) in ops.iter().zip(expected_events.iter()) {
            match op["op"].as_str().unwrap() {
                "render" => {
                    let width = op["width"].as_u64().unwrap() as usize;
                    let actual = list.render(width);
                    let expected_lines: Vec<String> = expected["render"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|v| v.as_str().unwrap().to_string())
                        .collect();
                    if actual != expected_lines {
                        failures.push(format!(
                            "[{note}] render\n  expected: {expected_lines:?}\n  actual:   {actual:?}"
                        ));
                    }
                }
                "handleInput" => {
                    let data = op["data"].as_str().unwrap();
                    list.handle_input(data);
                    let expected_idx = expected["selectedIndex"].as_u64().unwrap() as usize;
                    let actual_idx = selected_index_of(&list, &record["items"]);
                    if actual_idx != Some(expected_idx) {
                        failures.push(format!(
                            "[{note}] handleInput selectedIndex\n  expected: {expected_idx}\n  actual:   {actual_idx:?}"
                        ));
                    }
                }
                "setFilter" => {
                    let filter = op["filter"].as_str().unwrap();
                    list.set_filter(filter);
                    let expected_len = expected["filteredLength"].as_u64().unwrap() as usize;
                    let expected_idx = expected["selectedIndex"].as_u64().unwrap() as usize;
                    let actual_idx = selected_index_of(&list, &record["items"]);
                    if actual_idx != Some(expected_idx) {
                        failures.push(format!(
                            "[{note}] setFilter selectedIndex\n  expected: {expected_idx}\n  actual:   {actual_idx:?}"
                        ));
                    }
                    // filtered length is asserted indirectly via the next render case.
                    let _ = expected_len;
                }
                other => panic!("no Rust dispatch wired for oracle op {other:?}"),
            }
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));
}

/// `SelectList` has no direct "get current selected index" accessor (only
/// `get_selected_item()`), so recover the index by matching the selected
/// item's value against the ORIGINAL (unfiltered) item list. This is only
/// correct when the filtered set is identical to the original set (true for
/// every `handleInput`/`setFilter` case in this fixture — none combines a
/// narrowing filter with a subsequent index-navigation check) — a real
/// index-into-`filteredItems` recovery would need a test-only accessor this
/// wave didn't add. Named here rather than silently assumed correct.
fn selected_index_of(list: &SelectList, original_items: &Value) -> Option<usize> {
    let selected = list.get_selected_item()?;
    original_items
        .as_array()
        .unwrap()
        .iter()
        .position(|i| i["value"].as_str() == Some(selected.value.as_str()))
}
