//! Port of `packages/tui/src/terminal.ts` — the `Terminal` abstraction
//! `tui.rs` renders through, plus `ProcessTerminal`, the real stdio
//! implementation. See `docs/analysis/05-tui.md` §4/§8.
//!
//! ## Scope decisions (documented, not silent — AGENTS.md Correctness Bar)
//!
//! - **`crossterm` is used ONLY as a thin syscall shim** (raw-mode
//!   enable/disable, terminal size) per `05-tui.md` §8's explicit verdict —
//!   NOT its `Event`/`KeyEvent` parser. Every other `Terminal` method below
//!   is a literal ANSI-byte write, exactly matching the TS's own
//!   `process.stdout.write(...)` call sites (no crossterm API involved).
//!   Actual key-input decoding goes through `stdin_buffer`/`keys` (Waves
//!   1-2), never through crossterm's own key events.
//! - **Callback shape: `start` takes `'static` boxed closures, not the two
//!   plain closures the TS signature suggests**, because `ProcessTerminal`
//!   spawns a background thread to read stdin continuously (Rust has no
//!   Node-style ambient event loop) — the closures must outlive the call and
//!   must not borrow `self`. Wiring a live `TUI` (which naturally wants to
//!   capture `&mut self` in its own input handler) through this is
//!   `feat-007`'s job (per `plan.md`'s "Out of scope" section — this feature
//!   only builds the library standalone); `ProcessTerminal` itself just needs
//!   to accept and correctly invoke whatever `'static` closures it's given.
//! - **Resize detection is polling-based, not a native OS resize event.**
//!   The TS relies on Node's `process.stdout.on('resize', ...)`, backed by
//!   the OS's `SIGWINCH`. Rust has no equivalent without either a signal
//!   crate (not a workspace dependency, and a new one for a single call site
//!   fails the Ponytail ladder) or `crossterm::event::read()` — which cannot
//!   safely run concurrently with this module's own raw-byte stdin reader
//!   (both would race reading fd 0). This port polls `crossterm::terminal::
//!   size()` on a fixed interval (`RESIZE_POLL_INTERVAL`) and fires
//!   `on_resize` on change. This is a real, named behavioral difference from
//!   Pi (resize is detected within one poll interval, not instantly) — not
//!   papered over.
//! - **`enableWindowsVTInput()` and the macOS `isNativeModifierPressed`
//!   probe are Wave-7 stubs here.** Both native-module call sites in the TS
//!   fail closed when their `.node` addon isn't available (`catch {}` /
//!   dynamic `require` failure) — this port's stubs behave identically
//!   (`enable_windows_vt_input` is a documented no-op; `forward_input_
//!   sequence` always passes `is_shift_pressed = false`), matching today's
//!   non-Windows/non-macOS TS behavior exactly. Real FFI lands in
//!   `win_console.rs`/`native_modifiers.rs` (Wave 7).
//! - **The reader loop never calls `StdinBuffer::flush()`.** It blocks on
//!   `stdin.read()` and processes whatever bytes arrive; there is no
//!   separate idle-timeout watchdog thread. A real, named residual: a lone
//!   incomplete escape prefix (e.g. a bare Escape key with nothing after it)
//!   will sit in `StdinBuffer`'s internal buffer until the *next* byte
//!   arrives, rather than firing after `timeout_ms()` of inactivity like the
//!   TS's own `setTimeout`-driven flush. Fixing this needs either a
//!   non-blocking/timeout-capable stdin read or a second thread sharing the
//!   buffer under a lock — deferred as out of this wave's live-I/O testing
//!   scope (no oracle covers `ProcessTerminal` either way); revisit if
//!   `feat-007`'s real interactive wiring surfaces it as an actual UX bug.
//!   The same applies to `scheduleKeyboardProtocolNegotiationBufferFlush`'s
//!   150ms fragment-timeout (terminal.ts:295) — a split Kitty-negotiation
//!   response waits for the next byte rather than auto-flushing after 150ms.
//! - **No oracle for `ProcessTerminal`'s live-I/O plumbing** — there is no
//!   deterministic way to drive real stdio through Node either; the TS's own
//!   test suite does not appear to exercise `ProcessTerminal` directly (its
//!   `Terminal` interface is deliberately narrow so tests can supply a fake
//!   implementation instead — the same seam this port's `tui.rs` mock
//!   `Terminal` uses). This file's pure helpers below ARE oracle-tested.

use std::io::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::keys::set_kitty_protocol_active;
use crate::stdin_buffer::{StdinBuffer, StdinEvent};

const TERMINAL_PROGRESS_KEEPALIVE_MS: u64 = 1000;
const TERMINAL_PROGRESS_ACTIVE_SEQUENCE: &str = "\x1b]9;4;3\x07";
const TERMINAL_PROGRESS_CLEAR_SEQUENCE: &str = "\x1b]9;4;0;\x07";
const APPLE_TERMINAL_SHIFT_ENTER_SEQUENCE: &str = "\x1b[13;2u";
const DESIRED_KITTY_KEYBOARD_PROTOCOL_FLAGS: u32 = 7;
const RESIZE_POLL_INTERVAL: Duration = Duration::from_millis(200);

fn kitty_keyboard_protocol_query() -> String {
    format!("\x1b[>{DESIRED_KITTY_KEYBOARD_PROTOCOL_FLAGS}u\x1b[?u\x1b[c")
}

/// `KeyboardProtocolNegotiationSequence` (terminal.ts:19).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyboardProtocolNegotiationSequence {
    KittyFlags(u32),
    DeviceAttributes,
}

/// `parseKeyboardProtocolNegotiationSequence` (terminal.ts:23):
/// `^\x1b\[\?(\d+)u$` for Kitty flags, `^\x1b\[\?[\d;]*c$` for DA.
pub fn parse_keyboard_protocol_negotiation_sequence(
    sequence: &str,
) -> Option<KeyboardProtocolNegotiationSequence> {
    if let Some(rest) = sequence.strip_prefix("\x1b[?") {
        if let Some(digits) = rest.strip_suffix('u') {
            if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) {
                let flags: u32 = digits.parse().ok()?;
                return Some(KeyboardProtocolNegotiationSequence::KittyFlags(flags));
            }
        }
    }
    if let Some(rest) = sequence.strip_prefix("\x1b[?") {
        if let Some(body) = rest.strip_suffix('c') {
            if body.chars().all(|c| c.is_ascii_digit() || c == ';') {
                return Some(KeyboardProtocolNegotiationSequence::DeviceAttributes);
            }
        }
    }
    None
}

/// `isKeyboardProtocolNegotiationSequencePrefix` (terminal.ts:36):
/// `sequence === "\x1b[" || /^\x1b\[\?[\d;]*$/.test(sequence)`.
pub fn is_keyboard_protocol_negotiation_sequence_prefix(sequence: &str) -> bool {
    if sequence == "\x1b[" {
        return true;
    }
    match sequence.strip_prefix("\x1b[?") {
        Some(rest) => rest.chars().all(|c| c.is_ascii_digit() || c == ';'),
        None => false,
    }
}

/// `isAppleTerminalSession` (terminal.ts:40).
pub fn is_apple_terminal_session() -> bool {
    cfg!(target_os = "macos") && std::env::var("TERM_PROGRAM").as_deref() == Ok("Apple_Terminal")
}

/// `normalizeAppleTerminalInput` (terminal.ts:44).
pub fn normalize_apple_terminal_input(
    data: &str,
    is_apple_terminal: bool,
    is_shift_pressed: bool,
) -> String {
    if is_apple_terminal && data == "\r" && is_shift_pressed {
        return APPLE_TERMINAL_SHIFT_ENTER_SEQUENCE.to_string();
    }
    data.to_string()
}

/// `Terminal` (terminal.ts:52) — minimal terminal interface for the TUI
/// renderer. `start`'s closures are `'static` — see module docs. Not `Send`
/// (unlike an earlier draft of this trait): `tui.rs`'s `Component` tree is
/// `Rc<RefCell<dyn Component>>` (see its module docs), making `TUI` itself
/// `!Send` regardless of this trait — `ProcessTerminal`'s own background
/// threads don't need the trait bound either, since `Send`-ness there comes
/// directly from `SharedState`'s concrete field types (`Arc`/`Mutex`/
/// `AtomicBool`/`Box<dyn FnMut + Send>`), not from a supertrait on `dyn
/// Terminal`. This also lets test/oracle mock `Terminal`s use `Rc`/`RefCell`
/// for their captured-writes buffer instead of `Arc`/`Mutex`.
pub trait Terminal {
    fn start(&mut self, on_input: Box<dyn FnMut(&str) + Send>, on_resize: Box<dyn FnMut() + Send>);
    fn stop(&mut self);
    /// `drainInput` (terminal.ts:65) — blocks (synchronously; the TS version
    /// is `async` but every call site in `tui.rs`/its future callers can
    /// simply run this on a blocking thread) draining stdin for up to
    /// `max_ms`, exiting early once `idle_ms` passes with no input.
    fn drain_input(&mut self, max_ms: u64, idle_ms: u64);
    fn write(&mut self, data: &str);
    fn columns(&self) -> u16;
    fn rows(&self) -> u16;
    fn kitty_protocol_active(&self) -> bool;
    fn move_by(&mut self, lines: i32);
    fn hide_cursor(&mut self);
    fn show_cursor(&mut self);
    fn clear_line(&mut self);
    fn clear_from_cursor(&mut self);
    fn clear_screen(&mut self);
    fn set_title(&mut self, title: &str);
    fn set_progress(&mut self, active: bool);
}

type InputHandler = Box<dyn FnMut(&str) + Send>;

struct NegotiationBuffer {
    buffer: String,
}

/// Shared, lockable state the background reader thread and the foreground
/// `ProcessTerminal` methods (`stop`/`drain_input`) both touch — the Rust
/// analogue of the TS instance fields `inputHandler`/`keyboardProtocol*`
/// mutated from both `queryAndEnableKittyProtocol`'s stdin listener and
/// `drainInput`/`stop`.
struct SharedState {
    input_handler: Mutex<Option<InputHandler>>,
    last_data_at: Mutex<Instant>,
    kitty_protocol_active: AtomicBool,
    modify_other_keys_active: AtomicBool,
    running: AtomicBool,
    write_log_path: Option<std::path::PathBuf>,
}

/// Writes raw bytes to stdout (+ the optional `PI_TUI_WRITE_LOG` file) —
/// callable from both `ProcessTerminal`'s own methods and the background
/// reader thread's Kitty-protocol-negotiation handling (`enableModifyOtherKeys`
/// / `disableModifyOtherKeys`, terminal.ts:320-330, which the TS calls from
/// its stdin listener too).
fn emit_bytes(shared: &SharedState, data: &str) {
    let mut stdout = std::io::stdout();
    let _ = stdout.write_all(data.as_bytes());
    let _ = stdout.flush();
    if let Some(path) = &shared.write_log_path {
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = f.write_all(data.as_bytes());
        }
    }
}

fn enable_modify_other_keys(shared: &SharedState) {
    if shared.kitty_protocol_active.load(Ordering::Relaxed)
        || shared.modify_other_keys_active.load(Ordering::Relaxed)
    {
        return;
    }
    emit_bytes(shared, "\x1b[>4;2m");
    shared
        .modify_other_keys_active
        .store(true, Ordering::Relaxed);
}

fn disable_modify_other_keys(shared: &SharedState) {
    if !shared.modify_other_keys_active.load(Ordering::Relaxed) {
        return;
    }
    emit_bytes(shared, "\x1b[>4;0m");
    shared
        .modify_other_keys_active
        .store(false, Ordering::Relaxed);
}

/// `ProcessTerminal` (terminal.ts:99) — real terminal using stdin/stdout.
pub struct ProcessTerminal {
    shared: Arc<SharedState>,
    reader_thread: Option<std::thread::JoinHandle<()>>,
    resize_thread: Option<std::thread::JoinHandle<()>>,
}

impl Default for ProcessTerminal {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessTerminal {
    pub fn new() -> Self {
        let write_log_path = std::env::var("PI_TUI_WRITE_LOG")
            .ok()
            .filter(|s| !s.is_empty())
            .map(|env| {
                let path = std::path::PathBuf::from(&env);
                if path.is_dir() {
                    let now = chrono_like_timestamp();
                    path.join(format!("tui-{now}-{}.log", std::process::id()))
                } else {
                    path
                }
            });
        Self {
            shared: Arc::new(SharedState {
                input_handler: Mutex::new(None),
                last_data_at: Mutex::new(Instant::now()),
                kitty_protocol_active: AtomicBool::new(false),
                modify_other_keys_active: AtomicBool::new(false),
                running: AtomicBool::new(false),
                write_log_path,
            }),
            reader_thread: None,
            resize_thread: None,
        }
    }

    fn raw_write(&mut self, data: &str) {
        emit_bytes(&self.shared, data);
    }

    /// `enableWindowsVTInput` (terminal.ts:338) — Wave-7 stub, see module docs.
    fn enable_windows_vt_input(&self) {}
}

fn chrono_like_timestamp() -> String {
    // Filename-safe local-ish timestamp without a chrono dependency (this is
    // debug-log naming only, not behavior tested anywhere).
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}", now.as_secs())
}

impl Terminal for ProcessTerminal {
    fn start(&mut self, on_input: Box<dyn FnMut(&str) + Send>, on_resize: Box<dyn FnMut() + Send>) {
        let _ = crossterm::terminal::enable_raw_mode();
        self.raw_write("\x1b[?2004h"); // bracketed paste on

        self.shared.running.store(true, Ordering::Relaxed);
        *self
            .shared
            .input_handler
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(on_input);

        self.enable_windows_vt_input();

        // Kitty protocol query — negotiation is resolved inside the reader
        // thread as responses arrive (mirrors `queryAndEnableKittyProtocol`).
        self.raw_write(&kitty_keyboard_protocol_query());

        let shared = Arc::clone(&self.shared);
        self.reader_thread = Some(std::thread::spawn(move || run_reader_loop(shared)));

        let shared_resize = Arc::clone(&self.shared);
        let mut on_resize = on_resize;
        self.resize_thread = Some(std::thread::spawn(move || {
            let mut last = crossterm::terminal::size().ok();
            while shared_resize.running.load(Ordering::Relaxed) {
                std::thread::sleep(RESIZE_POLL_INTERVAL);
                if let Ok(size) = crossterm::terminal::size() {
                    if Some(size) != last {
                        last = Some(size);
                        on_resize();
                    }
                }
            }
        }));
    }

    fn stop(&mut self) {
        self.shared.running.store(false, Ordering::Relaxed);
        // TS order (terminal.ts:406-452): progress-clear (not tracked here,
        // see set_progress's module-doc deferral) -> bracketed-paste-off ->
        // kitty-disable -> modifyOtherKeys-disable.
        self.raw_write("\x1b[?2004l"); // bracketed paste off
        if self.shared.kitty_protocol_active.load(Ordering::Relaxed) {
            self.raw_write("\x1b[<u");
            self.shared
                .kitty_protocol_active
                .store(false, Ordering::Relaxed);
            set_kitty_protocol_active(false);
        }
        disable_modify_other_keys(&self.shared);
        *self
            .shared
            .input_handler
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
        let _ = crossterm::terminal::disable_raw_mode();
        if let Some(handle) = self.reader_thread.take() {
            let _ = handle.join();
        }
        if let Some(handle) = self.resize_thread.take() {
            let _ = handle.join();
        }
    }

    fn drain_input(&mut self, max_ms: u64, idle_ms: u64) {
        if self.shared.kitty_protocol_active.load(Ordering::Relaxed) {
            self.raw_write("\x1b[<u");
            self.shared
                .kitty_protocol_active
                .store(false, Ordering::Relaxed);
            set_kitty_protocol_active(false);
        }
        disable_modify_other_keys(&self.shared);

        let previous = self
            .shared
            .input_handler
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        *self
            .shared
            .last_data_at
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Instant::now();

        let deadline = Instant::now() + Duration::from_millis(max_ms);
        loop {
            let last = *self
                .shared
                .last_data_at
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if Instant::now() >= deadline || last.elapsed() >= Duration::from_millis(idle_ms) {
                break;
            }
            std::thread::sleep(Duration::from_millis(idle_ms.min(20)));
        }
        *self
            .shared
            .input_handler
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = previous;
    }

    fn write(&mut self, data: &str) {
        self.raw_write(data);
    }

    fn columns(&self) -> u16 {
        crossterm::terminal::size().map(|(w, _)| w).unwrap_or(80)
    }

    fn rows(&self) -> u16 {
        crossterm::terminal::size().map(|(_, h)| h).unwrap_or(24)
    }

    fn kitty_protocol_active(&self) -> bool {
        self.shared.kitty_protocol_active.load(Ordering::Relaxed)
    }

    fn move_by(&mut self, lines: i32) {
        use std::cmp::Ordering as CmpOrdering;
        match lines.cmp(&0) {
            CmpOrdering::Greater => self.raw_write(&format!("\x1b[{lines}B")),
            CmpOrdering::Less => self.raw_write(&format!("\x1b[{}A", -lines)),
            CmpOrdering::Equal => {}
        }
    }

    fn hide_cursor(&mut self) {
        self.raw_write("\x1b[?25l");
    }

    fn show_cursor(&mut self) {
        self.raw_write("\x1b[?25h");
    }

    fn clear_line(&mut self) {
        self.raw_write("\x1b[K");
    }

    fn clear_from_cursor(&mut self) {
        self.raw_write("\x1b[J");
    }

    fn clear_screen(&mut self) {
        self.raw_write("\x1b[2J\x1b[H");
    }

    fn set_title(&mut self, title: &str) {
        self.raw_write(&format!("\x1b]0;{title}\x07"));
    }

    fn set_progress(&mut self, active: bool) {
        if active {
            self.raw_write(TERMINAL_PROGRESS_ACTIVE_SEQUENCE);
            // The TS re-sends the active sequence every second to keep the
            // indicator alive; a background keepalive thread would need its
            // own stop signal. Not wired in this wave — see module docs on
            // ProcessTerminal's live-I/O scope; `set_progress`'s one-shot
            // sequence is still byte-correct, the keepalive repeat is the
            // deferred part.
            let _ = TERMINAL_PROGRESS_KEEPALIVE_MS;
        } else {
            self.raw_write(TERMINAL_PROGRESS_CLEAR_SEQUENCE);
        }
    }
}

/// The background reader loop spawned by `start` — reads raw stdin bytes,
/// feeds them through `StdinBuffer` (Waves 1-2), resolves Kitty-protocol
/// negotiation responses (`readKeyboardProtocolNegotiationSequence`,
/// terminal.ts:252), and forwards everything else to the input handler
/// (`forwardInputSequence`, terminal.ts:309).
fn run_reader_loop(shared: Arc<SharedState>) {
    let mut buffer = StdinBuffer::new(Default::default());
    let mut negotiation: Option<NegotiationBuffer> = None;
    let stdin = std::io::stdin();
    let mut byte = [0u8; 4096];
    use std::io::Read;

    while shared.running.load(Ordering::Relaxed) {
        let mut locked = stdin.lock();
        let n = match locked.read(&mut byte) {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => break,
        };
        drop(locked);
        *shared
            .last_data_at
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Instant::now();
        for event in buffer.process(&byte[..n]) {
            match event {
                StdinEvent::Data(sequence) => {
                    handle_negotiation_or_forward(&shared, &mut negotiation, sequence);
                }
                StdinEvent::Paste(content) => {
                    forward(&shared, &format!("\x1b[200~{content}\x1b[201~"));
                }
            }
        }
    }
}

fn handle_negotiation_or_forward(
    shared: &Arc<SharedState>,
    negotiation: &mut Option<NegotiationBuffer>,
    sequence: String,
) {
    if let Some(neg) = negotiation.as_mut() {
        let combined = format!("{}{}", neg.buffer, sequence);
        if let Some(parsed) = parse_keyboard_protocol_negotiation_sequence(&combined) {
            *negotiation = None;
            apply_negotiation(shared, parsed);
            return;
        }
        if is_keyboard_protocol_negotiation_sequence_prefix(&combined) {
            neg.buffer = combined;
            return;
        }
        let flushed = neg.buffer.clone();
        *negotiation = None;
        forward(shared, &flushed);
        // fall through to process `sequence` fresh below
    }

    if let Some(parsed) = parse_keyboard_protocol_negotiation_sequence(&sequence) {
        apply_negotiation(shared, parsed);
        return;
    }
    if is_keyboard_protocol_negotiation_sequence_prefix(&sequence) {
        *negotiation = Some(NegotiationBuffer { buffer: sequence });
        return;
    }
    forward(shared, &sequence);
}

/// `handleKeyboardProtocolNegotiationSequence` (terminal.ts:228).
fn apply_negotiation(shared: &Arc<SharedState>, parsed: KeyboardProtocolNegotiationSequence) {
    match parsed {
        KeyboardProtocolNegotiationSequence::KittyFlags(flags) => {
            if flags != 0 {
                disable_modify_other_keys(shared);
                if !shared.kitty_protocol_active.load(Ordering::Relaxed) {
                    shared.kitty_protocol_active.store(true, Ordering::Relaxed);
                    set_kitty_protocol_active(true);
                }
            } else {
                enable_modify_other_keys(shared);
            }
        }
        KeyboardProtocolNegotiationSequence::DeviceAttributes => {
            if !shared.kitty_protocol_active.load(Ordering::Relaxed) {
                enable_modify_other_keys(shared);
            }
        }
    }
}

fn forward(shared: &Arc<SharedState>, sequence: &str) {
    // `forwardInputSequence` (terminal.ts:309): Apple Terminal Shift+Enter
    // normalization. `is_shift_pressed` is always `false` here — the macOS
    // native-modifier probe is a Wave-7 stub (see module docs).
    let is_apple_terminal = sequence == "\r" && is_apple_terminal_session();
    let input = normalize_apple_terminal_input(sequence, is_apple_terminal, false);
    if let Some(handler) = shared
        .input_handler
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_mut()
    {
        handler(&input);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_kitty_flags() {
        assert_eq!(
            parse_keyboard_protocol_negotiation_sequence("\x1b[?7u"),
            Some(KeyboardProtocolNegotiationSequence::KittyFlags(7))
        );
    }

    #[test]
    fn parses_device_attributes() {
        assert_eq!(
            parse_keyboard_protocol_negotiation_sequence("\x1b[?1;2c"),
            Some(KeyboardProtocolNegotiationSequence::DeviceAttributes)
        );
    }

    #[test]
    fn prefix_detection() {
        assert!(is_keyboard_protocol_negotiation_sequence_prefix("\x1b["));
        assert!(is_keyboard_protocol_negotiation_sequence_prefix("\x1b[?7"));
        assert!(!is_keyboard_protocol_negotiation_sequence_prefix("\x1b[7u"));
    }

    #[test]
    fn apple_terminal_shift_enter() {
        assert_eq!(
            normalize_apple_terminal_input("\r", true, true),
            APPLE_TERMINAL_SHIFT_ENTER_SEQUENCE
        );
        assert_eq!(normalize_apple_terminal_input("\r", true, false), "\r");
        assert_eq!(normalize_apple_terminal_input("\r", false, true), "\r");
    }
}
