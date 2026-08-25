//! Regression test for a real bug: `Editor::render`'s `max_visible_lines`
//! must track the terminal's *current* row count, even though it is only
//! ever called from inside `TUI::do_render`'s render pass (see
//! `Editor::terminal_rows`'s doc comment for why a naive `Rc<RefCell<TUI>>`
//! re-borrow there always fails and would silently freeze this number at a
//! stale/default value instead of tracking a live resize).

use std::cell::RefCell;
use std::rc::Rc;

use pirust_tui::editor::Editor;
use pirust_tui::tui::{SharedComponent, TUI};

struct FakeTerminal {
    columns: u16,
    rows: Rc<std::cell::Cell<u16>>,
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
    fn write(&mut self, data: &str) {
        self.writes.borrow_mut().push(data.to_string());
    }
    fn columns(&self) -> u16 {
        self.columns
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

/// Mirrors `editor.rs`'s own `maxVisibleLines` formula
/// (`max(5, floor(rows * 0.3))`).
fn expected_max_visible_lines(rows: u16) -> usize {
    std::cmp::max(5, (rows as f64 * 0.3).floor() as usize)
}

#[test]
fn editor_box_height_tracks_a_live_resize_through_a_real_tui_render_pass() {
    let rows = Rc::new(std::cell::Cell::new(10u16));
    let terminal = Box::new(FakeTerminal {
        columns: 40,
        rows: rows.clone(),
        writes: RefCell::new(Vec::new()),
    });
    let tui = Rc::new(RefCell::new(TUI::new(terminal, Some(false))));

    let editor = Rc::new(RefCell::new(Editor::new(
        tui.borrow().terminal_rows_handle(),
        Box::new(|s| s.to_string()),
        Default::default(),
    )));
    // 20 short lines so the box always has more content than any candidate
    // `max_visible_lines` value in this test, and the rendered box height
    // (top border + visible lines + bottom border) exactly reveals it.
    let long_text = (0..20)
        .map(|i| format!("line{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    editor.borrow_mut().set_text(&long_text);

    tui.borrow_mut().add_child(editor.clone() as SharedComponent);

    // Render at rows=10 through the real TUI render pass — the same
    // `Rc<RefCell<TUI>>`-mutably-borrowed-for-the-whole-pass path
    // `InteractiveMode` uses, which is exactly where the old
    // `try_borrow`-with-stale-cache approach always fell back to its
    // hardcoded default and got this number wrong.
    tui.borrow_mut().request_render(true);
    tui.borrow_mut().poll();
    let box_height_at_10 = editor.borrow_mut().render(40).len();
    assert_eq!(
        box_height_at_10,
        expected_max_visible_lines(10) + 2,
        "box height at rows=10 should reflect the real height, not a stale/default value"
    );

    // Expand.
    rows.set(40);
    tui.borrow_mut().request_render(true);
    tui.borrow_mut().poll();
    let box_height_at_40 = editor.borrow_mut().render(40).len();
    assert_eq!(
        box_height_at_40,
        expected_max_visible_lines(40) + 2,
        "box height at rows=40 should grow with the resize, not stay pinned to the rows=10 value"
    );
    assert!(
        box_height_at_40 > box_height_at_10,
        "expanding the terminal should show more of the editor, not the same or fewer lines"
    );

    // Contract.
    rows.set(6);
    tui.borrow_mut().request_render(true);
    tui.borrow_mut().poll();
    let box_height_at_6 = editor.borrow_mut().render(40).len();
    assert_eq!(
        box_height_at_6,
        expected_max_visible_lines(6) + 2,
        "box height at rows=6 should shrink with the resize, not stay pinned to an earlier value"
    );
}
