//! Interactive mode (feat-007 Wave 1) — scaffold tests.
//!
//! Proves the full path: terminal → channel → TUI.handle_input → editor →
//! on_submit → prompt. `InteractiveMode` is `!Send` (Rc-based TUI), so the
//! mode runs on the main test thread and a feeder thread drives the captured
//! `on_input` callback; Ctrl+D on the empty editor makes `run()` return.

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use pirust_coding_agent::interactive_mode::InteractiveMode;
use pirust_tui::terminal::Terminal;

/// A terminal whose `start` captures the `on_input` callback so a test
/// thread can feed input.
type InputSlot = Arc<Mutex<Option<Box<dyn FnMut(&str) + Send>>>>;

struct DriveTerminal {
    input_slot: InputSlot,
}

impl Terminal for DriveTerminal {
    fn start(
        &mut self,
        on_input: Box<dyn FnMut(&str) + Send>,
        _on_resize: Box<dyn FnMut() + Send>,
    ) {
        *self.input_slot.lock().unwrap() = Some(on_input);
    }
    fn stop(&mut self) {}
    fn drain_input(&mut self, _max_ms: u64, _idle_ms: u64) {}
    fn write(&mut self, _data: &str) {}
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

/// Grab the captured `on_input` callback from the terminal.
fn take_on_input(input_slot: &InputSlot) -> Box<dyn FnMut(&str) + Send> {
    for _ in 0..200 {
        if let Some(cb) = input_slot.lock().unwrap().take() {
            return cb;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("terminal should have captured on_input");
}

#[test]
fn submit_routes_through_editor_to_prompt() {
    let input_slot = Arc::new(Mutex::new(None));
    let terminal = Box::new(DriveTerminal {
        input_slot: Arc::clone(&input_slot),
    });
    let mut mode = InteractiveMode::new(terminal);

    let submitted = Arc::new(Mutex::new(Vec::new()));

    // Feeder: type "hello", Enter, wait for the prompt, then Ctrl+D to quit.
    {
        let input_slot = Arc::clone(&input_slot);
        let submitted = Arc::clone(&submitted);
        thread::spawn(move || {
            let mut on_input = take_on_input(&input_slot);
            for ch in ["h", "e", "l", "l", "o"] {
                on_input(ch);
                thread::sleep(Duration::from_millis(10));
            }
            on_input("\r"); // submit
                            // Wait until the prompt fires (submitted non-empty), then quit.
            for _ in 0..200 {
                if !submitted.lock().unwrap().is_empty() {
                    break;
                }
                thread::sleep(Duration::from_millis(10));
            }
            on_input("\u{4}"); // ctrl+d on empty editor -> quit
        });
    }

    // Drive the loop on the main thread. `prompt` records submissions.
    let prompts: Arc<Mutex<Vec<String>>> = Arc::clone(&submitted);
    mode.run(move |text: String| {
        prompts.lock().unwrap().push(text);
    });

    // run() returned (ctrl+d quit). Assert the prompt fired.
    let submitted = submitted.lock().unwrap();
    assert!(
        submitted.iter().any(|s| s.contains("hello")),
        "prompt should receive typed text, got: {submitted:?}"
    );
}

#[test]
fn ctrl_d_on_empty_editor_quits_without_prompt() {
    let input_slot = Arc::new(Mutex::new(None));
    let terminal = Box::new(DriveTerminal {
        input_slot: Arc::clone(&input_slot),
    });
    let mut mode = InteractiveMode::new(terminal);

    let submitted = Arc::new(Mutex::new(Vec::new()));
    {
        let input_slot = Arc::clone(&input_slot);
        thread::spawn(move || {
            let mut on_input = take_on_input(&input_slot);
            thread::sleep(Duration::from_millis(50));
            on_input("\u{4}"); // ctrl+d on empty editor -> quit
        });
    }

    let prompts = Arc::clone(&submitted);
    mode.run(move |text: String| {
        prompts.lock().unwrap().push(text);
    });

    assert!(
        submitted.lock().unwrap().is_empty(),
        "no prompt should fire on ctrl+d alone"
    );
}
