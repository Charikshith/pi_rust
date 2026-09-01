//! P6 (`docs/tui-pending-action-plan.md`): "measure first, do not refactor on
//! a hunch." `Component::render(&mut self, width) -> Vec<String>` returns
//! owned data, so a cache *hit* (`text.rs`, `interactive_welcome.rs` x2,
//! `interactive_thinking.rs`, `interactive_debug.rs`) still clones a fresh
//! `Vec<String>` plus every `String` in it, rather than returning a borrow.
//!
//! This test builds the scenario the plan names — a long transcript at
//! 200x50 — and times the pure render-tree-walk-and-clone cost (every
//! mounted component hits its cache, since nothing changes between calls)
//! against the cost of one real frame's terminal write, so the two numbers
//! are directly comparable on the same machine, same run.
//!
//! `#[ignore]`d: a timing comparison is not a correctness assertion cargo
//! test's default run should gate on (machine-dependent, and the two
//! `eprintln!`s below are the actual deliverable — a human-readable number —
//! not a pass/fail). Run explicitly: `cargo test -p pirust-tui --test
//! render_cache_perf -- --ignored --nocapture`.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

use pirust_tui::{SharedComponent, Terminal, Text, TUI};

struct RecordingTerminal {
    writes: Rc<RefCell<String>>,
    columns: u16,
    rows: u16,
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
        self.columns
    }
    fn rows(&self) -> u16 {
        self.rows
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

/// A realistic long chat transcript: `count` multi-sentence messages, each
/// long enough to wrap across several lines at 200 columns — not the
/// single-short-line fixture `transcript_pruning.rs` uses, since that would
/// undercount `text.rs`'s per-line clone cost. A mutable `tail` message is
/// mounted last, so a caller can force one real, non-empty content change
/// (the differential renderer must have genuine work to do, not a no-op).
fn long_transcript(
    count: usize,
    columns: u16,
    rows: u16,
) -> (TUI, Rc<RefCell<Text>>, Rc<RefCell<String>>) {
    let writes = Rc::new(RefCell::new(String::new()));
    let terminal = Box::new(RecordingTerminal {
        writes: Rc::clone(&writes),
        columns,
        rows,
    });
    let mut tui = TUI::new(terminal, Some(false));

    let chat = Rc::new(RefCell::new(pirust_tui::tui::Container::new()));
    let body = "This is a chat message with enough words in it to wrap across \
                several lines once the terminal is only two hundred columns wide, \
                which is the width this scenario is measuring against.";
    for i in 0..count {
        let line: SharedComponent =
            Rc::new(RefCell::new(Text::new(format!("message {i}: {body}"), 1, 1)));
        chat.borrow_mut().add_child(line);
    }
    let tail = Rc::new(RefCell::new(Text::new("tail 0", 1, 1)));
    chat.borrow_mut()
        .add_child(Rc::clone(&tail) as SharedComponent);
    tui.add_child(chat as SharedComponent);

    tui.request_render(true);
    tui.poll();
    (tui, tail, writes)
}

#[test]
#[ignore]
fn render_cache_hit_cost_vs_one_frame_of_terminal_io() {
    const MESSAGE_COUNT: usize = 500;
    const COLUMNS: u16 = 200;
    const ROWS: u16 = 50;
    const CACHE_HIT_ITERATIONS: u32 = 2_000;

    let (mut tui, tail, writes) = long_transcript(MESSAGE_COUNT, COLUMNS, ROWS);

    // Every component just rendered once above (`request_render(true)` +
    // `poll()`), so every subsequent `render(COLUMNS)` call below is a pure
    // cache-hit walk: no text changed, no width changed, nothing to
    // recompute — exactly this test's scenario.
    let start = Instant::now();
    let mut total_lines = 0usize;
    for _ in 0..CACHE_HIT_ITERATIONS {
        total_lines += tui.render(COLUMNS as usize).len();
    }
    let cache_hit_elapsed = start.elapsed();
    let per_call = cache_hit_elapsed / CACHE_HIT_ITERATIONS;

    // One real frame's terminal write: change the tail row's text (so the
    // differential renderer has genuine, non-empty work to do — a real
    // streamed-token update, not a no-op), request a render, and time
    // `poll()` — the actual bytes-to-stdout cost this crate's diff renderer
    // already avoids paying redundantly, per its own docs.
    tail.borrow_mut().set_text("tail 1: a streamed token just arrived");
    writes.borrow_mut().clear();
    std::thread::sleep(std::time::Duration::from_millis(20));
    tui.request_render(false);
    let io_start = Instant::now();
    let rendered = tui.poll();
    let io_elapsed = io_start.elapsed();
    let io_bytes = writes.borrow().len();

    assert!(rendered, "a render was requested and should have happened");
    assert!(total_lines > 0, "sanity: the transcript rendered something");
    assert!(io_bytes > 0, "sanity: the tail change should have written something");

    eprintln!(
        "\n--- P6 render-cache measurement ({MESSAGE_COUNT} messages, {COLUMNS}x{ROWS}) ---"
    );
    eprintln!(
        "cache-hit render(): {CACHE_HIT_ITERATIONS} calls in {cache_hit_elapsed:?} \
         ({per_call:?}/call)"
    );
    eprintln!("one real frame's terminal write: {io_elapsed:?} ({io_bytes} bytes)");
    if io_elapsed > per_call {
        eprintln!(
            "-> one frame of terminal I/O costs {:.1}x a single cache-hit render() call",
            io_elapsed.as_secs_f64() / per_call.as_secs_f64().max(f64::MIN_POSITIVE)
        );
    }
    eprintln!("--- end P6 measurement ---\n");
}
