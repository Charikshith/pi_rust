//! Port of `packages/tui/src/stdin-buffer.ts` — buffers raw stdin bytes and emits
//! only complete escape sequences, so partial chunks (a mouse SGR sequence
//! arriving as `\x1b`, then `[<35`, then `;20;5m`) are never misinterpreted as a
//! regular keypress. See `docs/analysis/05-tui.md` §4/§9.
//!
//! ## Design decisions (documented, not silent — AGENTS.md Correctness Bar)
//!
//! - **No `EventEmitter`.** The TS `StdinBuffer` extends Node's `EventEmitter` to
//!   fire synchronous `'data'`/`'paste'` events from inside `process()` (and from
//!   its `setTimeout` callback). This crate has no async-runtime/event-loop
//!   dependency and shouldn't gain one just for this file (05-tui.md §8's own
//!   suggested Rust equivalent: "plain callbacks/closures... no crate needed").
//!   [`StdinBuffer::process`] instead *returns* the ordered list of events that
//!   the TS would have emitted synchronously, as `Vec<StdinEvent>`.
//! - **No owned timer.** The TS `process()` starts a `timeoutMs`-delayed
//!   `setTimeout` whenever bytes are left buffered after extraction, whose
//!   callback calls `flush()` and emits each returned sequence as a `data`
//!   event. Scheduling that timer is the event loop's job, not this buffer's —
//!   this crate has no event loop yet (that lands in Wave 4's `tui.rs`).
//!   [`StdinBuffer::flush`] has the exact same signature/semantics as the TS
//!   method; the caller is responsible for invoking it after
//!   [`StdinBuffer::timeout_ms`] have elapsed with no further `process()` call,
//!   and for emitting/handling the returned sequences exactly as the TS
//!   `setTimeout` callback does.
//! - **UTF-8, not UTF-16.** `process()` takes `&[u8]` (covering the TS `Buffer`
//!   input path, including its single-byte high-byte-to-ESC-meta conversion) and
//!   decodes via `String::from_utf8_lossy` (the TS's `Buffer#toString()` default
//!   is also UTF-8). Internal sequence-splitting logic (`extract_complete_sequences`
//!   et al.) walks `char` (Unicode scalar values), not UTF-16 code units like the
//!   TS's `.length`/string indexing — identical for every sequence this module
//!   actually parses (all escape-sequence structural bytes are ASCII), and only
//!   theoretically diverges for a raw non-escape astral-plane (>U+FFFF) input
//!   character, which `.length` would count as 2 code units — an edge case with
//!   no escape-sequence-splitting consequence.
//! - **Redundant mouse-SGR check collapsed.** The TS source's
//!   `isCompleteCsiSequence` tests the strict regex `^<\d+;\d+;\d+[Mm]$` first,
//!   and on failure falls back to a *second*, semantically identical manual
//!   check (strip `<`/trailing `M`/`m`, split on `;`, require 3 all-digit parts).
//!   Both checks accept exactly the same inputs, so this port implements the
//!   check once ([`is_mouse_sgr_complete`]) rather than duplicating dead
//!   fallback logic.

const BRACKETED_PASTE_START: &str = "\x1b[200~";
const BRACKETED_PASTE_END: &str = "\x1b[201~";

/// One event `StdinBuffer::process`/`flush` would have emitted (`data`/`paste`
/// in the TS `EventEmitter`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StdinEvent {
    Data(String),
    Paste(String),
}

/// `StdinBufferOptions` (stdin-buffer.ts:257).
#[derive(Debug, Clone, Copy, Default)]
pub struct StdinBufferOptions {
    /// Maximum time to wait for sequence completion (default: 10ms). See the
    /// module docs' "No owned timer" decision — the caller schedules this.
    pub timeout: Option<u64>,
}

fn parse_digits(s: &str) -> Option<i64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SequenceStatus {
    Complete,
    Incomplete,
    NotEscape,
}

/// `isCompleteSequence` (stdin-buffer.ts:29).
fn is_complete_sequence(data: &[char]) -> SequenceStatus {
    if data.first() != Some(&'\x1b') {
        return SequenceStatus::NotEscape;
    }
    if data.len() == 1 {
        return SequenceStatus::Incomplete;
    }
    let after_esc = &data[1..];
    match after_esc[0] {
        '[' => {
            if after_esc.len() >= 2 && after_esc[1] == 'M' {
                // Old-style mouse sequence: ESC[M + 3 bytes = 6 total.
                return if data.len() >= 6 {
                    SequenceStatus::Complete
                } else {
                    SequenceStatus::Incomplete
                };
            }
            is_complete_csi_sequence(data)
        }
        ']' => is_complete_osc_sequence(data),
        'P' => is_complete_dcs_sequence(data),
        '_' => is_complete_apc_sequence(data),
        'O' => {
            if after_esc.len() >= 2 {
                SequenceStatus::Complete
            } else {
                SequenceStatus::Incomplete
            }
        }
        // Meta key sequence (ESC + single char) or unknown escape: both "complete".
        _ => SequenceStatus::Complete,
    }
}

fn is_mouse_sgr_complete(payload: &[char]) -> bool {
    // `^<\d+;\d+;\d+[Mm]$` (stdin-buffer.ts:106, and the semantically identical
    // manual fallback at :113 — see module docs).
    if payload.len() < 2 || payload[0] != '<' {
        return false;
    }
    let last = *payload.last().unwrap();
    if last != 'M' && last != 'm' {
        return false;
    }
    let body: String = payload[1..payload.len() - 1].iter().collect();
    let parts: Vec<&str> = body.split(';').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

/// `isCompleteCsiSequence` (stdin-buffer.ts:84).
fn is_complete_csi_sequence(data: &[char]) -> SequenceStatus {
    if !(data.len() >= 2 && data[0] == '\x1b' && data[1] == '[') {
        return SequenceStatus::Complete;
    }
    if data.len() < 3 {
        return SequenceStatus::Incomplete;
    }
    let payload = &data[2..];
    let last_char_code = *payload.last().unwrap() as u32;
    if (0x40..=0x7e).contains(&last_char_code) {
        if payload[0] == '<' {
            return if is_mouse_sgr_complete(payload) {
                SequenceStatus::Complete
            } else {
                SequenceStatus::Incomplete
            };
        }
        return SequenceStatus::Complete;
    }
    SequenceStatus::Incomplete
}

fn ends_with_st(data: &[char]) -> bool {
    data.len() >= 2 && data[data.len() - 2] == '\x1b' && data[data.len() - 1] == '\\'
}

/// `isCompleteOscSequence` (stdin-buffer.ts:132).
fn is_complete_osc_sequence(data: &[char]) -> SequenceStatus {
    if !(data.len() >= 2 && data[0] == '\x1b' && data[1] == ']') {
        return SequenceStatus::Complete;
    }
    if ends_with_st(data) || data.last() == Some(&'\x07') {
        SequenceStatus::Complete
    } else {
        SequenceStatus::Incomplete
    }
}

/// `isCompleteDcsSequence` (stdin-buffer.ts:150).
fn is_complete_dcs_sequence(data: &[char]) -> SequenceStatus {
    if !(data.len() >= 2 && data[0] == '\x1b' && data[1] == 'P') {
        return SequenceStatus::Complete;
    }
    if ends_with_st(data) {
        SequenceStatus::Complete
    } else {
        SequenceStatus::Incomplete
    }
}

/// `isCompleteApcSequence` (stdin-buffer.ts:168).
fn is_complete_apc_sequence(data: &[char]) -> SequenceStatus {
    if !(data.len() >= 2 && data[0] == '\x1b' && data[1] == '_') {
        return SequenceStatus::Complete;
    }
    if ends_with_st(data) {
        SequenceStatus::Complete
    } else {
        SequenceStatus::Incomplete
    }
}

/// `parseUnmodifiedKittyPrintableCodepoint` (stdin-buffer.ts:184) — deliberately
/// stricter than `keys.rs`'s `parse_csi_u`: rejects ANY `;<mod>` segment, since
/// this only recognizes a bare, unmodified Kitty CSI-u printable push.
fn parse_unmodified_kitty_printable_codepoint(sequence: &str) -> Option<i64> {
    let inner = sequence.strip_prefix("\x1b[")?.strip_suffix('u')?;
    if inner.contains(';') {
        return None;
    }
    let mut segs = inner.split(':');
    let codepoint = parse_digits(segs.next()?)?;
    for seg in segs {
        if !seg.is_empty() && !seg.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
    }
    if codepoint >= 32 {
        Some(codepoint)
    } else {
        None
    }
}

/// `extractCompleteSequences` (stdin-buffer.ts:192): split `buffer` into
/// complete escape/plain-char sequences, returning the leftover (incomplete)
/// remainder. Includes the WezTerm double-escape split (stdin-buffer.ts:208-230).
fn extract_complete_sequences(buffer: &str) -> (Vec<String>, String) {
    let chars: Vec<char> = buffer.chars().collect();
    let mut sequences = Vec::new();
    let mut pos = 0usize;

    while pos < chars.len() {
        if chars[pos] == '\x1b' {
            let mut seq_end = 1usize;
            let mut matched = false;
            while pos + seq_end <= chars.len() {
                let candidate = &chars[pos..pos + seq_end];
                match is_complete_sequence(candidate) {
                    SequenceStatus::Complete => {
                        if candidate.len() == 2 && candidate[1] == '\x1b' {
                            if let Some(&next_char) = chars.get(pos + seq_end) {
                                if matches!(next_char, '[' | ']' | 'O' | 'P' | '_') {
                                    sequences.push("\x1b".to_string());
                                    pos += 1;
                                    matched = true;
                                    break;
                                }
                            }
                        }
                        sequences.push(candidate.iter().collect());
                        pos += seq_end;
                        matched = true;
                        break;
                    }
                    SequenceStatus::Incomplete => seq_end += 1,
                    SequenceStatus::NotEscape => {
                        // Should not happen when starting with ESC.
                        sequences.push(candidate.iter().collect());
                        pos += seq_end;
                        matched = true;
                        break;
                    }
                }
            }
            if !matched {
                let remainder: String = chars[pos..].iter().collect();
                return (sequences, remainder);
            }
        } else {
            sequences.push(chars[pos].to_string());
            pos += 1;
        }
    }

    (sequences, String::new())
}

fn high_byte_or_utf8(data: &[u8]) -> String {
    if data.len() == 1 && data[0] > 127 {
        let byte = data[0] - 128;
        format!("\x1b{}", byte as char)
    } else {
        String::from_utf8_lossy(data).into_owned()
    }
}

/// `StdinBuffer` (stdin-buffer.ts:274) — see module docs for the `EventEmitter`
/// / timer design decisions.
#[derive(Debug, Default)]
pub struct StdinBuffer {
    buffer: String,
    timeout_ms: u64,
    paste_mode: bool,
    paste_buffer: String,
    pending_kitty_printable_codepoint: Option<u32>,
}

impl StdinBuffer {
    pub fn new(options: StdinBufferOptions) -> Self {
        Self {
            buffer: String::new(),
            timeout_ms: options.timeout.unwrap_or(10),
            paste_mode: false,
            paste_buffer: String::new(),
            pending_kitty_printable_codepoint: None,
        }
    }

    /// The configured flush timeout — see module docs' "No owned timer" decision.
    pub fn timeout_ms(&self) -> u64 {
        self.timeout_ms
    }

    /// `process(data: string | Buffer)` (stdin-buffer.ts:287). Returns the events
    /// that would have been emitted synchronously.
    pub fn process(&mut self, data: &[u8]) -> Vec<StdinEvent> {
        let mut events = Vec::new();
        let str_data = high_byte_or_utf8(data);
        self.process_str(&str_data, &mut events);
        events
    }

    fn process_str(&mut self, str_data: &str, events: &mut Vec<StdinEvent>) {
        if str_data.is_empty() && self.buffer.is_empty() {
            self.emit_data_sequence(String::new(), events);
            return;
        }

        self.buffer.push_str(str_data);

        if self.paste_mode {
            self.paste_buffer.push_str(&self.buffer);
            self.buffer.clear();
            self.try_complete_paste(events);
            return;
        }

        if let Some(start_index) = self.buffer.find(BRACKETED_PASTE_START) {
            if start_index > 0 {
                let before_paste = self.buffer[..start_index].to_string();
                let (sequences, _) = extract_complete_sequences(&before_paste);
                for seq in sequences {
                    self.emit_data_sequence(seq, events);
                }
            }

            self.pending_kitty_printable_codepoint = None;
            let after_start = self.buffer[start_index + BRACKETED_PASTE_START.len()..].to_string();
            self.buffer.clear();
            self.paste_mode = true;
            self.paste_buffer = after_start;

            self.try_complete_paste(events);
            return;
        }

        let (sequences, remainder) = extract_complete_sequences(&self.buffer);
        self.buffer = remainder;
        for seq in sequences {
            self.emit_data_sequence(seq, events);
        }
        // If `self.buffer` is non-empty here, the TS starts a `timeout_ms`-delayed
        // timer to call `flush()` — the caller's job now, see module docs.
    }

    fn try_complete_paste(&mut self, events: &mut Vec<StdinEvent>) {
        let Some(end_index) = self.paste_buffer.find(BRACKETED_PASTE_END) else {
            return;
        };
        let pasted_content = self.paste_buffer[..end_index].to_string();
        let remaining = self.paste_buffer[end_index + BRACKETED_PASTE_END.len()..].to_string();

        self.paste_mode = false;
        self.paste_buffer.clear();
        self.pending_kitty_printable_codepoint = None;

        events.push(StdinEvent::Paste(pasted_content));

        if !remaining.is_empty() {
            self.process_str(&remaining, events);
        }
    }

    fn emit_data_sequence(&mut self, sequence: String, events: &mut Vec<StdinEvent>) {
        let raw_codepoint = if sequence.chars().count() == 1 {
            sequence.chars().next().map(|c| c as u32)
        } else {
            None
        };
        if let Some(rc) = raw_codepoint {
            if Some(rc) == self.pending_kitty_printable_codepoint {
                self.pending_kitty_printable_codepoint = None;
                return;
            }
        }
        self.pending_kitty_printable_codepoint =
            parse_unmodified_kitty_printable_codepoint(&sequence).map(|c| c as u32);
        events.push(StdinEvent::Data(sequence));
    }

    /// `flush()` (stdin-buffer.ts:400) — the caller invokes this after
    /// `timeout_ms()` elapses with no further `process()` call, exactly mirroring
    /// what the TS's own `setTimeout` callback does (see module docs).
    pub fn flush(&mut self) -> Vec<String> {
        if self.buffer.is_empty() {
            return Vec::new();
        }
        vec![std::mem::take(&mut self.buffer)]
    }

    /// `clear()` (stdin-buffer.ts:416).
    pub fn clear(&mut self) {
        self.buffer.clear();
        self.paste_mode = false;
        self.paste_buffer.clear();
        self.pending_kitty_printable_codepoint = None;
    }

    /// `getBuffer()` (stdin-buffer.ts:427).
    pub fn get_buffer(&self) -> &str {
        &self.buffer
    }

    /// `destroy()` (stdin-buffer.ts:431).
    pub fn destroy(&mut self) {
        self.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_mouse_sgr_across_three_chunks() {
        let mut buf = StdinBuffer::new(StdinBufferOptions::default());
        assert_eq!(buf.process(b"\x1b"), vec![]);
        assert_eq!(buf.process(b"[<35"), vec![]);
        assert_eq!(
            buf.process(b";20;5m"),
            vec![StdinEvent::Data("\x1b[<35;20;5m".to_string())]
        );
    }

    #[test]
    fn bracketed_paste_whole() {
        let mut buf = StdinBuffer::new(StdinBufferOptions::default());
        let events = buf.process(b"\x1b[200~hello world\x1b[201~");
        assert_eq!(events, vec![StdinEvent::Paste("hello world".to_string())]);
    }

    #[test]
    fn flush_returns_incomplete_buffer() {
        let mut buf = StdinBuffer::new(StdinBufferOptions::default());
        assert_eq!(buf.process(b"\x1b["), vec![]);
        assert_eq!(buf.flush(), vec!["\x1b[".to_string()]);
        assert_eq!(buf.get_buffer(), "");
    }
}
