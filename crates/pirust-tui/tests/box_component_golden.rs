//! Pi oracle for [`pirust_tui::BoxComponent`] (feat-006 Wave 5).
//!
//! Replays every record of `tests/fixtures/pi/tui/box.cases.jsonl` —
//! captured by executing real Pi's `packages/tui/src/components/box.ts` —
//! and asserts identical `render(width)` output for the same padding/
//! background/child configuration.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use pirust_tui::components::box_component::BoxComponent;
use pirust_tui::tui::{Component, SharedComponent};
use serde_json::Value;

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/pi/tui/box.cases.jsonl")
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

struct Fixed(Vec<String>);
impl Component for Fixed {
    fn render(&mut self, _width: usize) -> Vec<String> {
        self.0.clone()
    }
    fn invalidate(&mut self) {}
}

#[test]
fn every_box_record_matches_pi() {
    let records = load_records();
    assert_eq!(
        records.len(),
        5,
        "fixture record count changed — update this assertion deliberately"
    );

    let mut failures = Vec::new();
    for record in &records {
        let note = record["note"].as_str().unwrap();
        let padding_x = record["paddingX"].as_u64().unwrap() as usize;
        let padding_y = record["paddingY"].as_u64().unwrap() as usize;
        let use_bg = record["useBg"].as_bool().unwrap();
        let width = record["width"].as_u64().unwrap() as usize;
        let expected: Vec<String> = record["result"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();

        let mut b = if use_bg {
            BoxComponent::with_bg_fn(
                padding_x,
                padding_y,
                Box::new(|s: &str| format!("<bg>{s}</bg>")),
            )
        } else {
            BoxComponent::new(padding_x, padding_y)
        };

        for child in record["children"].as_array().unwrap() {
            let lines: Vec<String> = child
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap().to_string())
                .collect();
            let component: SharedComponent = Rc::new(RefCell::new(Fixed(lines)));
            b.add_child(component);
        }

        let actual = b.render(width);
        if actual != expected {
            failures.push(format!(
                "[{note}]\n  expected: {expected:?}\n  actual:   {actual:?}"
            ));
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));
}
