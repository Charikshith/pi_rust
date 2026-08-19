//! Pi oracle for [`pirust_tui::SettingsList`] (feat-006 Wave 5).
//!
//! Replays every record of `tests/fixtures/pi/tui/settings-list.cases.jsonl`
//! — captured by executing real Pi's
//! `packages/tui/src/components/settings-list.ts` against IDENTITY theme
//! functions — and asserts identical ordered `render`/`onChange`/`onCancel`
//! results for the same operation script.
//!
//! Submenu open/close is NOT covered here: a `SettingItem.submenu` factory
//! returns a live `Component`/closure, which has no JSON-serializable
//! representation to carry across the Node-oracle/Rust boundary. The
//! value-cycling, cancel, search-filter, and description-render paths
//! (everything this fixture DOES cover) exercise the rest of the file's
//! logic; submenu wiring itself is a documented, not silent, gap — a
//! same-process Rust-only test could still cover it later if needed.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use pirust_tui::components::settings_list::{
    SettingItem, SettingsList, SettingsListOptions, SettingsListTheme,
};
use pirust_tui::tui::Component;
use serde_json::Value;

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/pi/tui/settings-list.cases.jsonl")
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

fn identity_theme() -> SettingsListTheme {
    SettingsListTheme {
        label: Box::new(|s, _selected| s.to_string()),
        value: Box::new(|s, _selected| s.to_string()),
        description: Box::new(|s| s.to_string()),
        cursor: "> ".to_string(),
        hint: Box::new(|s| s.to_string()),
    }
}

fn parse_items(v: &Value) -> Vec<SettingItem> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|item| SettingItem {
            id: item["id"].as_str().unwrap().to_string(),
            label: item["label"].as_str().unwrap().to_string(),
            description: item["description"].as_str().map(str::to_string),
            current_value: item["currentValue"].as_str().unwrap().to_string(),
            values: item["values"]
                .as_array()
                .map(|a| a.iter().map(|v| v.as_str().unwrap().to_string()).collect()),
            submenu: None,
        })
        .collect()
}

#[test]
fn every_settings_list_record_matches_pi() {
    let records = load_records();
    assert_eq!(
        records.len(),
        6,
        "fixture record count changed — update this assertion deliberately"
    );

    let mut failures = Vec::new();
    for record in &records {
        let note = record["note"].as_str().unwrap();
        let items = parse_items(&record["items"]);
        let max_visible = record["maxVisible"].as_u64().unwrap() as usize;
        let enable_search = record["enableSearch"].as_bool().unwrap();
        let default_width = record["width"].as_u64().unwrap() as usize;
        let ops = record["ops"].as_array().unwrap();
        let expected_events = record["events"].as_array().unwrap();

        let changes: Rc<RefCell<Vec<(String, String)>>> = Rc::new(RefCell::new(Vec::new()));
        let changes_clone = changes.clone();
        let cancelled = Rc::new(RefCell::new(false));
        let cancelled_clone = cancelled.clone();

        let mut list = SettingsList::new(
            items,
            max_visible,
            identity_theme(),
            Box::new(move |id, val| {
                changes_clone
                    .borrow_mut()
                    .push((id.to_string(), val.to_string()))
            }),
            Box::new(move || *cancelled_clone.borrow_mut() = true),
            SettingsListOptions { enable_search },
        );

        for (op, expected) in ops.iter().zip(expected_events.iter()) {
            match op["op"].as_str().unwrap() {
                "render" => {
                    let width = op["width"]
                        .as_u64()
                        .map(|w| w as usize)
                        .unwrap_or(default_width);
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
                    let expected_cancelled = expected["cancelled"].as_bool().unwrap();
                    let actual_cancelled = *cancelled.borrow();
                    if actual_cancelled != expected_cancelled {
                        failures.push(format!(
                            "[{note}] handleInput cancelled\n  expected: {expected_cancelled}\n  actual:   {actual_cancelled}"
                        ));
                    }
                    let expected_changes: Vec<(String, String)> = expected["changes"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|c| {
                            (
                                c["id"].as_str().unwrap().to_string(),
                                c["newValue"].as_str().unwrap().to_string(),
                            )
                        })
                        .collect();
                    let actual_changes = changes.borrow().clone();
                    if actual_changes != expected_changes {
                        failures.push(format!(
                            "[{note}] handleInput changes\n  expected: {expected_changes:?}\n  actual:   {actual_changes:?}"
                        ));
                    }
                }
                other => panic!("no Rust dispatch wired for oracle op {other:?}"),
            }
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));
}
