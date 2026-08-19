//! Pi oracle for [`pirust_tui::editor::Editor`] (feat-006 Wave 6).
//!
//! Replays every record of `tests/fixtures/pi/tui/editor.cases.jsonl` —
//! captured by executing real Pi's `packages/tui/src/components/editor.ts`
//! against a fake `TUI` — and asserts identical final text, cursor, events,
//! and rendered output. This is the crown-jewel wave: the UTF-16 code-unit
//! cursor arithmetic, paste-marker segmentation, kill-ring/undo/history,
//! char-jump, and word-wrap layout are all pinned against Pi's literal
//! behavior.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use pirust_tui::editor::Editor;

use pirust_tui::TUI;
use serde_json::Value;

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/pi/tui/editor.cases.jsonl")
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

/// A minimal fake Terminal matching the oracle's `makeFakeTerminal`.
struct FakeTerminal {
    columns: u16,
    rows: u16,
    writes: RefCell<Vec<String>>,
}

impl pirust_tui::terminal::Terminal for FakeTerminal {
    fn start(
        &mut self,
        _on_input: Box<dyn FnMut(&str) + Send>,
        _on_resize: Box<dyn FnMut() + Send>,
    ) {
    }
    fn stop(&mut self) {}
    fn drain_input(&mut self, _max_ms: u64, _idle_ms: u64) {}
    fn columns(&self) -> u16 {
        self.columns
    }
    fn rows(&self) -> u16 {
        self.rows
    }
    fn kitty_protocol_active(&self) -> bool {
        false
    }
    fn write(&mut self, data: &str) {
        self.writes.borrow_mut().push(data.to_string());
    }
    fn move_by(&mut self, _lines: i32) {}
    fn hide_cursor(&mut self) {}
    fn show_cursor(&mut self) {}
    fn clear_line(&mut self) {}
    fn clear_from_cursor(&mut self) {}
    fn clear_screen(&mut self) {}
    fn set_title(&mut self, _title: &str) {}
    fn set_progress(&mut self, _active: bool) {}
}

fn make_editor(width: u16, rows: u16) -> Editor {
    let terminal = Box::new(FakeTerminal {
        columns: width,
        rows,
        writes: RefCell::new(Vec::new()),
    });
    let tui = Rc::new(RefCell::new(TUI::new(terminal, Some(false))));
    Editor::new(
        tui,
        Box::new(|s| s.to_string()),
        pirust_tui::editor::EditorOptions::default(),
    )
}

#[test]
fn every_editor_record_matches_pi() {
    let records = load_records();
    assert_eq!(
        records.len(),
        25,
        "fixture record count changed — update this assertion deliberately"
    );

    let mut failures = Vec::new();
    for record in &records {
        let note = record["note"].as_str().unwrap();
        let ops = record["ops"].as_array().unwrap();
        let width = record["width"].as_u64().unwrap() as u16;
        let rows = record["rows"].as_u64().unwrap() as u16;
        let expected_text = record["text"].as_str().unwrap();
        let expected_cursor = record["cursor"].as_object().unwrap();
        let expected_events = record["events"].as_array().unwrap();

        let mut editor = make_editor(width, rows);
        let events_rc: Rc<RefCell<Vec<Value>>> = Rc::new(RefCell::new(Vec::new()));
        {
            let events = events_rc.clone();
            editor.on_change = Some(Box::new(move |text: &str| {
                events
                    .borrow_mut()
                    .push(serde_json::json!({ "change": text }));
            }));
        }
        {
            let events = events_rc.clone();
            editor.on_submit = Some(Box::new(move |text: &str| {
                events
                    .borrow_mut()
                    .push(serde_json::json!({ "submit": text }));
            }));
        }

        for op in ops {
            match op["op"].as_str().unwrap() {
                "handleInput" => {
                    let data = op["data"].as_str().unwrap();
                    editor.handle_input(data);
                }
                "setText" => {
                    let value = op["value"].as_str().unwrap();
                    editor.set_text(value);
                }
                "addToHistory" => {
                    let value = op["value"].as_str().unwrap();
                    editor.add_to_history(value);
                }
                "setPaddingX" => {
                    let value = op["value"].as_u64().unwrap() as usize;
                    editor.set_padding_x(value);
                }
                other => panic!("unknown op {other} in record {note}"),
            }
        }

        let actual_text = editor.get_text();
        let (actual_line, actual_col) = editor.get_cursor();
        let actual_cursor: Value = serde_json::json!({ "line": actual_line, "col": actual_col });
        let actual_render = editor.render(width as usize);

        let mut ok = true;
        let mut problems = Vec::new();
        if actual_text != expected_text {
            ok = false;
            problems.push(format!(
                "text: expected {:?}, got {:?}",
                expected_text, actual_text
            ));
        }
        if actual_cursor.as_object() != Some(expected_cursor) {
            ok = false;
            problems.push(format!(
                "cursor: expected {expected_cursor:?}, got {actual_cursor:?}"
            ));
        }
        if events_rc.borrow().as_slice() != *expected_events {
            ok = false;
            problems.push(format!(
                "events: expected {:?}, got {:?}",
                expected_events,
                events_rc.borrow()
            ));
        }
        let expected_render = record["render"].as_array().unwrap();
        if actual_render != *expected_render {
            ok = false;
            problems.push(format!(
                "render: expected {} lines, got {} lines — {:?} vs {:?}",
                expected_render.len(),
                actual_render.len(),
                expected_render,
                actual_render
            ));
        }

        if !ok {
            failures.push(format!("\n[{note}] {}", problems.join(" | ")));
        }
    }

    assert!(
        failures.is_empty(),
        "{} editor record(s) diverged from Pi:{}\n",
        failures.len(),
        failures.join("\n")
    );
}
