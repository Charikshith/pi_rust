//! Pi oracle for [`pirust_tui::tui`] (feat-006 Wave 4).
//!
//! Replays every record of `tests/fixtures/pi/tui/tui.cases.jsonl` —
//! captured by driving a real Pi `TUI` against a JS-side fake `Terminal` —
//! and asserts identical `write()` byte sequences (and, where recorded, the
//! same focus/overlay observable state). Modest coverage by design — see
//! `pirust_tui::tui`'s module docs and `plan.md`'s Wave 4 entry: this is not
//! an exhaustive `doRender` branch enumeration.
//!
//! Timing note: `poll()` mirrors the oracle's `await sleep(20)` — both sides
//! wait past the 16ms render-throttle before a `write()` snapshot is taken.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::thread::sleep;
use std::time::Duration;

use pirust_tui::tui::{Component, Focusable, OverlayOptions, SharedComponent, TUI};
use serde_json::Value;

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/pi/tui/tui.cases.jsonl")
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

struct MockTerminal {
    columns: Rc<Cell<u16>>,
    rows: Rc<Cell<u16>>,
    writes: Rc<RefCell<Vec<String>>>,
}

type MockTerminalHandles = (
    MockTerminal,
    Rc<Cell<u16>>,
    Rc<Cell<u16>>,
    Rc<RefCell<Vec<String>>>,
);

fn make_mock_terminal(columns: u16, rows: u16) -> MockTerminalHandles {
    let columns = Rc::new(Cell::new(columns));
    let rows = Rc::new(Cell::new(rows));
    let writes = Rc::new(RefCell::new(Vec::new()));
    (
        MockTerminal {
            columns: columns.clone(),
            rows: rows.clone(),
            writes: writes.clone(),
        },
        columns,
        rows,
        writes,
    )
}

impl pirust_tui::terminal::Terminal for MockTerminal {
    fn start(
        &mut self,
        _on_input: Box<dyn FnMut(&str) + Send>,
        _on_resize: Box<dyn FnMut() + Send>,
    ) {
    }
    fn stop(&mut self) {}
    fn drain_input(&mut self, _max_ms: u64, _idle_ms: u64) {}
    fn write(&mut self, data: &str) {
        self.writes.borrow_mut().push(data.to_string());
    }
    fn columns(&self) -> u16 {
        self.columns.get()
    }
    fn rows(&self) -> u16 {
        self.rows.get()
    }
    fn kitty_protocol_active(&self) -> bool {
        false
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

struct FakeComponent {
    lines: Rc<RefCell<Vec<String>>>,
    focused: bool,
}

impl Component for FakeComponent {
    fn render(&mut self, _width: usize) -> Vec<String> {
        self.lines.borrow().clone()
    }
    fn invalidate(&mut self) {}
    fn as_focusable_mut(&mut self) -> Option<&mut dyn Focusable> {
        Some(self)
    }
}

impl Focusable for FakeComponent {
    fn is_focused(&self) -> bool {
        self.focused
    }
    fn set_focused(&mut self, value: bool) {
        self.focused = value;
    }
}

fn fake_component(lines: Vec<String>) -> (SharedComponent, Rc<RefCell<Vec<String>>>) {
    let content = Rc::new(RefCell::new(lines));
    let component: SharedComponent = Rc::new(RefCell::new(FakeComponent {
        lines: content.clone(),
        focused: false,
    }));
    (component, content)
}

fn wait_for_throttle() {
    sleep(Duration::from_millis(20));
}

#[test]
fn tui_cases_match_pi() {
    let records = load_records();
    assert_eq!(
        records.len(),
        7,
        "fixture record count changed — update this assertion deliberately"
    );

    for record in &records {
        let note = record["note"].as_str().unwrap();
        let columns = record["columns"].as_u64().unwrap() as u16;
        let rows = record["rows"].as_u64().unwrap() as u16;
        let expected_writes: Vec<String> = record["writes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();

        let (terminal, columns_handle, _rows_handle, writes) = make_mock_terminal(columns, rows);
        let mut tui = TUI::new(Box::new(terminal), Some(false));

        match note {
            "first-render-then-noop" => {
                let (component, _) = fake_component(vec!["hello".to_string(), "world".to_string()]);
                tui.add_child(component);
                tui.request_render(true);
                writes.borrow_mut().clear();
                tui.request_render(false);
                wait_for_throttle();
                tui.poll();
            }
            "single-line-diff" => {
                let (component, content) =
                    fake_component(vec!["hello".to_string(), "world".to_string()]);
                tui.add_child(component);
                tui.request_render(true);
                wait_for_throttle();
                tui.poll();
                writes.borrow_mut().clear();
                content.borrow_mut()[1] = "WORLD!".to_string();
                tui.request_render(false);
                wait_for_throttle();
                tui.poll();
            }
            "append-growth" => {
                let (component, content) = fake_component(vec!["hello".to_string()]);
                tui.add_child(component);
                tui.request_render(true);
                wait_for_throttle();
                tui.poll();
                writes.borrow_mut().clear();
                content.borrow_mut().push("more".to_string());
                tui.request_render(false);
                wait_for_throttle();
                tui.poll();
            }
            "width-change-forces-full-redraw" => {
                let (component, _) = fake_component(vec!["hello".to_string(), "world".to_string()]);
                tui.add_child(component);
                tui.request_render(true);
                wait_for_throttle();
                tui.poll();
                writes.borrow_mut().clear();
                columns_handle.set(30);
                tui.request_render(false);
                wait_for_throttle();
                tui.poll();
            }
            "overlay-show-focus-hide-restores-prior-focus" => {
                let (base, _) = fake_component(vec!["base-line".to_string()]);
                tui.add_child(base.clone());
                tui.set_focus(Some(base.clone()));
                tui.request_render(true);
                wait_for_throttle();
                tui.poll();
                writes.borrow_mut().clear();

                let (overlay, _) = fake_component(vec!["overlay-line".to_string()]);
                let id = tui.show_overlay(overlay.clone(), Some(OverlayOptions::default()));
                let focused_is_overlay = tui_focus_is(&tui, &overlay);
                let has_overlay = tui.has_overlay();
                wait_for_throttle();
                tui.poll();

                tui.hide_overlay_by_id(id);
                let focused_is_base = tui_focus_is(&tui, &base);
                let has_overlay_after = tui.has_overlay();
                wait_for_throttle();
                tui.poll();

                let expected_events = &record["events"];
                assert_eq!(
                    expected_events[0]["afterShow_focusedIsOverlay"], focused_is_overlay,
                    "[{note}] afterShow_focusedIsOverlay"
                );
                assert_eq!(
                    expected_events[0]["hasOverlay"], has_overlay,
                    "[{note}] hasOverlay-after-show"
                );
                assert_eq!(
                    expected_events[1]["afterHide_focusedIsBase"], focused_is_base,
                    "[{note}] afterHide_focusedIsBase"
                );
                assert_eq!(
                    expected_events[1]["hasOverlay"], has_overlay_after,
                    "[{note}] hasOverlay-after-hide"
                );
            }
            "two-overlays-hide-non-topmost-does-not-move-focus" => {
                tui.request_render(true);
                wait_for_throttle();
                tui.poll();
                let (overlay_a, _) = fake_component(vec!["a".to_string()]);
                let (overlay_b, _) = fake_component(vec!["b".to_string()]);
                let id_a = tui.show_overlay(overlay_a.clone(), Some(OverlayOptions::default()));
                wait_for_throttle();
                tui.poll();
                let id_b = tui.show_overlay(overlay_b.clone(), Some(OverlayOptions::default()));
                wait_for_throttle();
                tui.poll();
                let focused_is_b = tui_focus_is(&tui, &overlay_b);

                tui.hide_overlay_by_id(id_a);
                wait_for_throttle();
                tui.poll();
                let still_focused_is_b = tui_focus_is(&tui, &overlay_b);
                let has_overlay = tui.has_overlay();

                let expected_events = &record["events"];
                assert_eq!(
                    expected_events[0]["focusedIsB"], focused_is_b,
                    "[{note}] focusedIsB"
                );
                assert_eq!(
                    expected_events[1]["stillFocusedIsB"], still_focused_is_b,
                    "[{note}] stillFocusedIsB"
                );
                assert_eq!(
                    expected_events[1]["hasOverlay"], has_overlay,
                    "[{note}] hasOverlay"
                );
                tui.hide_overlay_by_id(id_b);
                // The oracle's fixture-record `writes` field captures the fake
                // terminal's array BY REFERENCE, not a snapshot — a render
                // this final `hide()` schedules (non-force, so it's throttled,
                // not immediate) lands in the array before the whole script
                // finishes and JSON.stringify runs, even without an explicit
                // trailing `sleep()` in that one case's own script body. Match
                // it here with an explicit wait + poll.
                wait_for_throttle();
                tui.poll();
            }
            "cursor-marker-extracted-and-stripped" => {
                const CURSOR_MARKER: &str = "\x1b_pi:c\x07";
                let (component, _) = fake_component(vec![format!("abc{CURSOR_MARKER}def")]);
                tui.set_focus(Some(component.clone()));
                tui.add_child(component);
                tui.request_render(true);
                wait_for_throttle();
                tui.poll();
            }
            other => panic!("unhandled case {other}"),
        }

        let actual_writes = writes.borrow().clone();
        assert_eq!(
            actual_writes, expected_writes,
            "[{note}] write() sequence mismatch"
        );
    }
}

fn tui_focus_is(tui: &TUI, component: &SharedComponent) -> bool {
    tui.is_focused_component(component)
}
