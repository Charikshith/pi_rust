//! Pruning a long transcript must be invisible on screen.
//!
//! `Container::drop_leading_children` + `TUI::forget_leading_lines` bound the
//! document the renderer carries, so a long session stops paying O(session)
//! per frame and stops growing `previous_lines` without limit. The whole thing
//! only holds together if the two stay in step: drop children without shifting
//! the renderer's diff state and every remaining row renumbers, so the next
//! frame finds everything changed and falls back to a full redraw with a
//! visibly duplicated transcript.
//!
//! These tests pin the alignment, not the speed.

use std::cell::RefCell;
use std::rc::Rc;

use pirust_tui::{SharedComponent, Terminal, Text, TUI};

struct RecordingTerminal {
    writes: Rc<RefCell<String>>,
}

impl Terminal for RecordingTerminal {
    fn start(
        &mut self,
        _on_input: Box<dyn FnMut(&str) + Send>,
        _on_resize: Box<dyn FnMut() + Send>,
    ) {
    }
    fn stop(&mut self) {}
    fn drain_input(&mut self, _max_ms: u64, _idle_ms: u64) {}
    fn write(&mut self, data: &str) {
        self.writes.borrow_mut().push_str(data);
    }
    fn columns(&self) -> u16 {
        80
    }
    fn rows(&self) -> u16 {
        24
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

/// A chat container with `count` one-line messages plus a mutable tail, all
/// mounted on a TUI that has already rendered one frame.
struct Fixture {
    tui: TUI,
    chat: Rc<RefCell<pirust_tui::tui::Container>>,
    tail: Rc<RefCell<Text>>,
    writes: Rc<RefCell<String>>,
}

fn transcript(count: usize) -> Fixture {
    let writes = Rc::new(RefCell::new(String::new()));
    let terminal = Box::new(RecordingTerminal {
        writes: Rc::clone(&writes),
    });
    let mut tui = TUI::new(terminal, Some(false));

    let chat = Rc::new(RefCell::new(pirust_tui::tui::Container::new()));
    for i in 0..count {
        let line: SharedComponent = Rc::new(RefCell::new(Text::new(format!("message {i}"), 0, 0)));
        chat.borrow_mut().add_child(line);
    }
    let tail = Rc::new(RefCell::new(Text::new("tail 0", 0, 0)));
    chat.borrow_mut()
        .add_child(Rc::clone(&tail) as SharedComponent);
    tui.add_child(Rc::clone(&chat) as SharedComponent);

    tui.request_render(true);
    tui.poll();
    Fixture {
        tui,
        chat,
        tail,
        writes,
    }
}

/// Force a render regardless of the throttle window.
fn render_now(tui: &mut TUI) {
    std::thread::sleep(std::time::Duration::from_millis(20));
    tui.request_render(false);
    assert!(
        tui.poll(),
        "a render was requested and should have happened"
    );
}

#[test]
fn pruning_scrolled_lines_costs_no_full_redraw_and_no_extra_output() {
    let Fixture {
        mut tui,
        chat,
        tail,
        writes,
    } = transcript(400);

    // A baseline update with no pruning: this is what one changed row costs.
    tail.borrow_mut().set_text("tail 1");
    writes.borrow_mut().clear();
    let redraws_before = tui.full_redraws();
    render_now(&mut tui);
    let baseline_bytes = writes.borrow().len();
    assert_eq!(
        tui.full_redraws(),
        redraws_before,
        "a one-row change must not full-redraw"
    );

    // Now prune, then make the same kind of one-row change.
    let budget = tui.lines_above_viewport();
    assert!(
        budget > 300,
        "a 400-line transcript on a 24-row terminal should have most of its \
         rows above the viewport, got {budget}"
    );
    let dropped = chat.borrow_mut().drop_leading_children(80, budget);
    assert!(
        dropped > 300,
        "expected to prune most of the transcript, got {dropped}"
    );
    tui.forget_leading_lines(dropped);

    tail.borrow_mut().set_text("tail 2");
    writes.borrow_mut().clear();
    let redraws_before = tui.full_redraws();
    render_now(&mut tui);
    let pruned_bytes = writes.borrow().len();

    assert_eq!(
        tui.full_redraws(),
        redraws_before,
        "pruning must not trigger a full redraw; the diff state was left \
         misaligned with the shortened document"
    );
    assert!(
        pruned_bytes <= baseline_bytes * 2,
        "a one-row change after pruning wrote {pruned_bytes} bytes vs \
         {baseline_bytes} before pruning — the renderer is repainting rows \
         that did not change"
    );
    // The row that changed is the one that reached the terminal.
    assert!(
        writes.borrow().contains("tail 2"),
        "the changed row should still render, got: {:?}",
        writes.borrow()
    );
}

#[test]
fn pruning_never_drops_a_visible_line() {
    let Fixture { mut tui, chat, .. } = transcript(400);

    // Ask for far more than is legal; the TUI must clamp to what has already
    // scrolled off, and the surviving document must still cover the screen.
    let budget = tui.lines_above_viewport();
    let dropped = chat.borrow_mut().drop_leading_children(80, budget);
    tui.forget_leading_lines(dropped);

    let remaining = tui.render(80).len();
    assert!(
        remaining >= 24,
        "pruning left {remaining} lines, fewer than the 24-row terminal shows"
    );
}

#[test]
fn forget_leading_lines_refuses_to_outrun_the_viewport() {
    let Fixture { mut tui, .. } = transcript(400);
    let above = tui.lines_above_viewport();
    // Asking to forget the entire document must clamp, not underflow.
    tui.forget_leading_lines(usize::MAX);
    assert_eq!(
        tui.lines_above_viewport(),
        0,
        "forgetting everything above the viewport should leave nothing above it"
    );
    assert!(above > 0, "the fixture should have had scrolled-off rows");
}

/// A short transcript has nothing above the viewport, so pruning is a no-op
/// rather than an eager discard of content the user can still see.
#[test]
fn short_transcript_prunes_nothing() {
    let Fixture { mut tui, chat, .. } = transcript(3);
    let budget = tui.lines_above_viewport();
    assert_eq!(
        budget, 0,
        "a 4-line document on 24 rows has scrolled nothing"
    );
    let dropped = chat.borrow_mut().drop_leading_children(80, budget);
    assert_eq!(dropped, 0, "nothing should be pruned");
    tui.forget_leading_lines(dropped);
    assert_eq!(tui.render(80).len(), 4);
}
