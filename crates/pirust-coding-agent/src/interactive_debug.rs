//! Observability for the interactive TUI: request ids, turn timing, a bounded
//! debug log with an optional file sink, a debug panel component, and a
//! panic hook that turns a crash into a copyable report instead of a raw
//! backtrace dump.
//!
//! `docs/tui-design-samples.html` promises "Observability — Optional debug
//! panel/log file, request id, elapsed time, and copyable error details —
//! never a raw panic by default." None of that existed anywhere in the
//! crate; this module is all of it. Wiring `interactive_mode.rs`'s
//! `render_event`/key handling up to [`DebugLog::record_event`] and
//! [`DebugPanel`] is left to the caller of this task, per the task boundary
//! (no edits to `interactive_mode.rs`).
//!
//! # What was already there, and what was reused
//!
//! - **No existing log writer.** `rg 'PIRUST_LOG|log_file|tracing|env_logger|debug_log'`
//!   across `crates/` turns up exactly one hit outside this file and its own
//!   tests: [`crate::config::ConfigEnv::debug_log_path`] (`config.rs:473-479`),
//!   a *path accessor* porting Pi's `getDebugLogPath()` — `` join(getAgentDir(),
//!   `${APP_NAME}-debug.log`) `` — with zero call sites beyond its own
//!   golden test. Nothing in the crate ever opens or writes that path. There
//!   is no `tracing`/`log`/`env_logger` dependency in this crate at all, so
//!   there was nothing to build on top of or duplicate: this is the first
//!   actual log *writer*.
//! - **Why `ConfigEnv::debug_log_path()` is not called directly here.**
//!   Constructing a `ConfigEnv` requires a process environment snapshot
//!   (`ConfigEnv::from_process_env_for`, fallible when `HOME`/`USERPROFILE`
//!   is unavailable) that this self-contained module has no reason to
//!   depend on. Instead [`DebugLog::with_sink_path`] accepts any path
//!   explicitly, so a future wiring pass in `interactive_mode.rs` can feed
//!   it `ConfigEnv::debug_log_path()`'s result directly — same directory
//!   Pi would use, no logic duplicated here.
//! - **The `PIRUST_*` env convention.** Every existing var is `PIRUST_` +
//!   `SCREAMING_SNAKE_CASE`: `PIRUST_CODING_AGENT_DIR`/`PIRUST_CODING_AGENT_SESSION_DIR`
//!   (`config.rs:145,174`), `PIRUST_OFFLINE` (`config.rs:174`, `main.rs:153`),
//!   `PIRUST_TELEMETRY` (`provider_attribution.rs:28`), `PIRUST_PACKAGE_DIR`
//!   (`auth_guidance.rs:47`), `PIRUST_REDUCED_MOTION`/`PIRUST_ASCII`/`PIRUST_A11Y`
//!   (`interactive_a11y.rs`). [`DEBUG_LOG_ENV`] (`PIRUST_TUI_LOG`) follows
//!   the same shape.
//! - **`std::env::set_var` is `unsafe` in this toolchain** (noted at
//!   `auth_guidance.rs:47-48`) and this crate is `#![forbid(unsafe_code)]`,
//!   so this module's tests cannot exercise the env-var branch in-process.
//!   [`DebugLog::with_sink_path`] exists specifically so the *file-writing*
//!   behavior the env var enables is still fully testable without mutating
//!   process-global state.
//!
//! # What `render_event` (`interactive_mode.rs:1209-1334`) currently shows
//! the user, for contrast
//!
//! It streams `MessageStart`/`MessageUpdate`/`MessageEnd`, draws a separator
//! on `AgentEnd`, tracks `ToolExecutionStart`/`Update`/`End`, shows a notice
//! for `CompactionStart`/`CompactionEnd`/`AutoRetryStart`, and refreshes the
//! status line on `AgentSettled` — everything else (`AgentStart`, `TurnStart`,
//! `TurnEnd`, `QueueUpdate`, `EntryAppended`, `SessionInfoChanged`,
//! `ThinkingLevelChanged`, `AutoRetryEnd`) falls through its `_ => {}` and is
//! invisible to the user today. [`DebugLog::record_event`] logs *every*
//! variant of [`crate::print_mode::AgentSessionEvent`] (19 total,
//! `print_mode.rs:468-583`), which is exactly the point of a debug log: it
//! sees what the chat transcript deliberately does not render.
//! `session_status()` (`interactive_mode.rs:1501-1533`)'s
//! `" · "`-joined, single-line status format is the precedent
//! [`TurnTimings::summary`] follows for the same reason — it is meant to
//! live on that same status line.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::LineWriter;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use pirust_tui::tui::Component;
use pirust_tui::utils::visible_width;
use serde_json::Value;

use crate::interactive_theme::{bg, dark, fg};
use crate::print_mode::AgentSessionEvent;

// ============================================================================
// Request ids
// ============================================================================

/// A cheap, `Copy`, monotonically-allocated id for one turn/request. Deliberately
/// not a UUID: a UUID needs a dependency this task may not add and 128 bits of
/// randomness nobody will ever type back at support — a process-local counter is
/// smaller, faster to allocate (`AtomicU64::fetch_add`, no RNG), and just as
/// useful for "which turn does this log line belong to," which is the only job
/// it has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RequestId(u64);

static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

impl RequestId {
    /// Allocate the next id in process-wide, monotonically increasing order.
    /// `Relaxed` ordering is enough: callers only ever compare/display the
    /// value, never use it to synchronize access to other memory.
    pub fn next() -> Self {
        RequestId(NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed))
    }

    /// Reconstruct a `RequestId` from a raw value. Mainly for tests and for
    /// re-parsing an id back out of a persisted log line; real ids should
    /// come from [`RequestId::next`].
    pub fn from_raw(id: u64) -> Self {
        RequestId(id)
    }

    pub fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for RequestId {
    /// `req_00000017` — fixed-width, unambiguous to select with a double
    /// click in any terminal, and greppable in a log file without a regex.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "req_{:08}", self.0)
    }
}

// ============================================================================
// Turn timing
// ============================================================================

/// One tool call's wall-clock duration, recorded once the call finishes.
#[derive(Debug, Clone)]
pub struct ToolTiming {
    pub name: String,
    pub duration: Duration,
}

/// Per-turn wall-clock instrumentation. Built on [`Instant`], never
/// [`std::time::SystemTime`]: `SystemTime` can jump backwards (NTP step, user
/// changes the clock) and its subtraction is fallible for exactly that
/// reason — the wrong primitive for "how long did this take," which must be
/// monotonic by construction.
pub struct TurnTimings {
    request_id: RequestId,
    turn_start: Instant,
    first_token: Option<Instant>,
    turn_end: Option<Instant>,
    input_tokens: u64,
    output_tokens: u64,
    /// Tool calls still in flight, keyed by `toolCallId`: name plus start
    /// time, removed and turned into a [`ToolTiming`] on
    /// [`TurnTimings::mark_tool_end`].
    tool_open: HashMap<String, (String, Instant)>,
    tool_durations: Vec<ToolTiming>,
}

impl TurnTimings {
    pub fn new(request_id: RequestId) -> Self {
        Self {
            request_id,
            turn_start: Instant::now(),
            first_token: None,
            turn_end: None,
            input_tokens: 0,
            output_tokens: 0,
            tool_open: HashMap::new(),
            tool_durations: Vec::new(),
        }
    }

    pub fn request_id(&self) -> RequestId {
        self.request_id
    }

    /// Mark the first assistant token/delta of the turn. Idempotent: only the
    /// first call has any effect, so a caller can invoke this on every
    /// `MessageUpdate` without needing to track "have I already marked this."
    /// This is the number that actually matters for perceived speed —
    /// time-to-first-byte, not total duration.
    pub fn mark_first_token(&mut self) {
        if self.first_token.is_none() {
            self.first_token = Some(Instant::now());
        }
    }

    /// Mark the turn as finished. Idempotent in the same way as
    /// [`TurnTimings::mark_first_token`] — a later call does not push the
    /// end time forward.
    pub fn mark_end(&mut self) {
        if self.turn_end.is_none() {
            self.turn_end = Some(Instant::now());
        }
    }

    pub fn add_tokens(&mut self, input: u64, output: u64) {
        self.input_tokens += input;
        self.output_tokens += output;
    }

    pub fn input_tokens(&self) -> u64 {
        self.input_tokens
    }

    pub fn output_tokens(&self) -> u64 {
        self.output_tokens
    }

    /// Record a tool call starting. `tool_call_id` matches
    /// [`AgentSessionEvent::ToolExecutionStart`]'s field of the same name.
    pub fn mark_tool_start(
        &mut self,
        tool_call_id: impl Into<String>,
        tool_name: impl Into<String>,
    ) {
        self.tool_open
            .insert(tool_call_id.into(), (tool_name.into(), Instant::now()));
    }

    /// Record a tool call finishing. A no-op if `tool_call_id` was never
    /// opened (e.g. the id was already evicted, or timing started after the
    /// call began) — timing is best-effort instrumentation, not a
    /// correctness invariant, so it degrades silently rather than panicking.
    pub fn mark_tool_end(&mut self, tool_call_id: &str) {
        if let Some((name, start)) = self.tool_open.remove(tool_call_id) {
            self.tool_durations.push(ToolTiming {
                name,
                duration: start.elapsed(),
            });
        }
    }

    pub fn tool_durations(&self) -> &[ToolTiming] {
        &self.tool_durations
    }

    pub fn tool_count(&self) -> usize {
        self.tool_durations.len()
    }

    /// Time from turn start to the first token, or `None` if no token has
    /// arrived yet.
    pub fn first_token_latency(&self) -> Option<Duration> {
        self.first_token
            .map(|t| t.saturating_duration_since(self.turn_start))
    }

    /// Total turn duration: from start to [`TurnTimings::mark_end`], or to
    /// "now" if the turn has not finished yet (so a status line can show a
    /// live, growing elapsed time).
    pub fn total_duration(&self) -> Duration {
        self.turn_end
            .unwrap_or_else(Instant::now)
            .saturating_duration_since(self.turn_start)
    }

    /// One status-bar line, e.g. `req_00000017 · 1.2s to first token · 8.4s
    /// total · 3 tools`. All duration formatting happens here, lazily, on
    /// demand — the hot path (`mark_first_token`/`mark_tool_end`) only ever
    /// does integer `Instant` arithmetic.
    pub fn summary(&self) -> String {
        use std::fmt::Write as _;
        let mut out = String::new();
        let _ = write!(out, "{} · ", self.request_id);
        match self.first_token_latency() {
            Some(d) => {
                let _ = write!(out, "{:.1}s to first token · ", d.as_secs_f64());
            }
            None => out.push_str("no first token yet · "),
        }
        let _ = write!(
            out,
            "{:.1}s total · {} tool{}",
            self.total_duration().as_secs_f64(),
            self.tool_count(),
            if self.tool_count() == 1 { "" } else { "s" }
        );
        out
    }
}

// ============================================================================
// Bounded debug log
// ============================================================================

/// Ring-buffer capacity, in entries.
///
/// Sizing: an [`Entry`] is a `u64` seq (8 B) + `Duration` (16 B) + [`LogLevel`]
/// (1 B, a fieldless enum) + a `String` message. `record_event`'s messages are
/// one short line — a variant tag, a tool name, an id, a couple of counts —
/// typically well under 80 bytes of content plus `String`'s 24-byte header on a
/// 64-bit target. That puts one entry at roughly 24 + 80 ≈ 104 bytes, so 512
/// entries is a **~53 KiB ceiling**: enough history to cover a multi-minute
/// turn's tool calls, retries and compaction events — the things that matter
/// for "what just happened" — while being small enough that no session's
/// memory profile will ever notice it. [`VecDeque::with_capacity`] pre-allocates
/// that ceiling once, so steady-state pushes (`pop_front` + `push_back` once
/// full) never reallocate or shift the buffer; both are O(1) on `VecDeque`,
/// unlike `Vec::remove(0)`, which is the footgun this type exists to avoid.
pub const CAPACITY: usize = 512;

/// How many of the most recent entries a panic report includes. Deliberately
/// smaller than [`CAPACITY`]: a panic report is meant to be pasted into an
/// issue or a chat message, not to reproduce the entire ring buffer.
pub const PANIC_REPORT_LINES: usize = 40;

/// `PIRUST_TUI_LOG=<path>` — when set to a non-empty value, [`DebugLog::new`]
/// opens that path in append mode as the log's file sink. Unset (the
/// default) means in-memory only: the ring buffer still works, nothing is
/// ever written to disk.
pub const DEBUG_LOG_ENV: &str = "PIRUST_TUI_LOG";

/// Severity of a [`DebugLog`] entry. A fieldless, `Copy` enum rather than a
/// `String` — the whole point of a bounded log is that entries stay small.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

impl LogLevel {
    fn as_str(self) -> &'static str {
        match self {
            LogLevel::Debug => "DBG",
            LogLevel::Info => "INF",
            LogLevel::Warn => "WRN",
            LogLevel::Error => "ERR",
        }
    }
}

impl fmt::Display for LogLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One ring-buffer slot. Private: callers only ever see formatted output via
/// [`DebugLog::recent`], never this struct directly.
#[derive(Debug, Clone)]
struct Entry {
    seq: u64,
    elapsed: Duration,
    level: LogLevel,
    message: String,
}

impl Entry {
    /// Render this entry as one display line. Elapsed-time formatting
    /// (`{:.3}`) is done here, lazily, only when a line is actually about to
    /// be shown — never at push time.
    fn format_line(&self) -> String {
        format!(
            "[{:06}] {:>8.3}s {} {}",
            self.seq,
            self.elapsed.as_secs_f64(),
            self.level,
            self.message
        )
    }
}

/// A file sink: an already-open handle, written to append-only and
/// line-buffered so every complete line reaches disk promptly without a
/// `flush()` after every write. Never reopened; if the underlying write ever
/// fails, [`DebugLog`] drops the sink (see [`DebugLog::write_to_sink`]) —
/// this module never panics on log I/O.
struct FileSink {
    writer: LineWriter<File>,
}

/// A bounded, in-memory-first debug log: a fixed-capacity ring buffer of
/// recent events, plus an optional file sink. See [`CAPACITY`] for the
/// memory ceiling and [`DEBUG_LOG_ENV`] for how the file sink is enabled.
pub struct DebugLog {
    start: Instant,
    entries: VecDeque<Entry>,
    next_seq: u64,
    /// Reused across pushes: [`DebugLog::write_event_summary`] and the
    /// `info`/`warn`/`error`/`debug` helpers write into this buffer, and
    /// [`DebugLog::take_message_slot`] either clones it into a fresh
    /// `String` (only during the first [`CAPACITY`] pushes, before the ring
    /// is full) or copies it into a popped entry's already-allocated
    /// `String` (every push after that) — the steady-state, indefinitely
    /// long-running case never allocates a new string for the message.
    scratch: String,
    sink: Option<FileSink>,
}

impl Default for DebugLog {
    fn default() -> Self {
        Self::new()
    }
}

impl DebugLog {
    /// Create a log with no file sink, then enable one from
    /// [`DEBUG_LOG_ENV`] if it is set to a non-empty value. Opening a bad
    /// path never panics or fails construction — it just logs one `Warn`
    /// entry and leaves the sink disabled.
    pub fn new() -> Self {
        let mut log = Self::new_without_sink();
        if let Ok(path) = std::env::var(DEBUG_LOG_ENV) {
            if !path.is_empty() {
                log.enable_sink(&path);
            }
        }
        log
    }

    /// Create a log with no file sink and no env lookup. Used by tests (this
    /// crate is `#![forbid(unsafe_code)]`, and `std::env::set_var` is
    /// `unsafe`, so [`DEBUG_LOG_ENV`] itself cannot be exercised in-process
    /// here) and by any caller that wants a purely in-memory log.
    pub fn new_without_sink() -> Self {
        Self {
            start: Instant::now(),
            entries: VecDeque::with_capacity(CAPACITY),
            next_seq: 0,
            scratch: String::new(),
            sink: None,
        }
    }

    /// Create a log with the file sink pointed explicitly at `path`,
    /// bypassing [`DEBUG_LOG_ENV`]. This is what a caller wants once the log
    /// path is derived from [`crate::config::ConfigEnv::debug_log_path`]
    /// rather than the environment (see the module docs), and it is how
    /// this module's own tests exercise sink behavior without mutating
    /// process-global env state.
    pub fn with_sink_path(path: impl AsRef<std::path::Path>) -> Self {
        let mut log = Self::new_without_sink();
        log.enable_sink(&path.as_ref().to_string_lossy());
        log
    }

    fn enable_sink(&mut self, path: &str) {
        match OpenOptions::new().create(true).append(true).open(path) {
            Ok(file) => {
                self.sink = Some(FileSink {
                    writer: LineWriter::new(file),
                });
            }
            Err(err) => {
                self.log_str(LogLevel::Warn, &format!("debug log sink disabled: {err}"));
            }
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Total number of entries ever pushed (not just the ones still in the
    /// ring). Monotonically increasing, so a consumer (e.g. [`DebugPanel`])
    /// can tell "did anything change since I last rendered" with one cheap
    /// integer compare instead of comparing buffer contents.
    pub fn sequence(&self) -> u64 {
        self.next_seq
    }

    pub fn info(&mut self, message: impl AsRef<str>) {
        self.log_str(LogLevel::Info, message.as_ref());
    }

    pub fn warn(&mut self, message: impl AsRef<str>) {
        self.log_str(LogLevel::Warn, message.as_ref());
    }

    pub fn error(&mut self, message: impl AsRef<str>) {
        self.log_str(LogLevel::Error, message.as_ref());
    }

    pub fn debug(&mut self, message: impl AsRef<str>) {
        self.log_str(LogLevel::Debug, message.as_ref());
    }

    fn log_str(&mut self, level: LogLevel, message: &str) {
        self.scratch.clear();
        self.scratch.push_str(message);
        self.push_message(level);
    }

    /// Summarise `event` into one short log line. **Never** serializes the
    /// event's `Value` payloads (`message`, `args`, `partial_result`,
    /// `result`, `entry`) — only small scalars already sitting in the
    /// event's own struct fields: variant name, tool name, tool call id,
    /// attempt/retry counters, `is_error`, and shallow size hints (element
    /// counts, not recursive byte counts) for the payloads themselves. That
    /// is the entire reason this type can afford a bounded ring buffer:
    /// entries stay a fixed, small size regardless of how large the actual
    /// message content is.
    pub fn record_event(&mut self, event: &AgentSessionEvent) {
        self.scratch.clear();
        let level = self.write_event_summary(event);
        self.push_message(level);
    }

    fn write_event_summary(&mut self, event: &AgentSessionEvent) -> LogLevel {
        use std::fmt::Write as _;
        match event {
            AgentSessionEvent::AgentStart => {
                self.scratch.push_str("agent_start");
                LogLevel::Info
            }
            AgentSessionEvent::TurnStart => {
                self.scratch.push_str("turn_start");
                LogLevel::Info
            }
            AgentSessionEvent::TurnEnd { tool_results, .. } => {
                let _ = write!(self.scratch, "turn_end tool_results={}", tool_results.len());
                LogLevel::Info
            }
            AgentSessionEvent::MessageStart { message } => {
                let role = message.get("role").and_then(Value::as_str).unwrap_or("?");
                let _ = write!(self.scratch, "message_start role={role}");
                LogLevel::Debug
            }
            AgentSessionEvent::MessageUpdate { .. } => {
                // High-frequency (one per streaming delta): fixed text, zero
                // computation, so this is cheap even at token-per-event rates.
                self.scratch.push_str("message_update");
                LogLevel::Debug
            }
            AgentSessionEvent::MessageEnd { message } => {
                let role = message.get("role").and_then(Value::as_str).unwrap_or("?");
                let _ = write!(self.scratch, "message_end role={role}");
                LogLevel::Debug
            }
            AgentSessionEvent::ToolExecutionStart {
                tool_call_id,
                tool_name,
                ..
            } => {
                let _ = write!(self.scratch, "tool_start {tool_name} id={tool_call_id}");
                LogLevel::Info
            }
            AgentSessionEvent::ToolExecutionUpdate {
                tool_call_id,
                tool_name,
                partial_result,
                ..
            } => {
                let _ = write!(
                    self.scratch,
                    "tool_update {tool_name} id={tool_call_id} size~{}",
                    value_size_hint(partial_result)
                );
                LogLevel::Debug
            }
            AgentSessionEvent::ToolExecutionEnd {
                tool_call_id,
                tool_name,
                result,
                is_error,
            } => {
                let _ = write!(
                    self.scratch,
                    "tool_end {tool_name} id={tool_call_id} is_error={is_error} size~{}",
                    value_size_hint(result)
                );
                if *is_error {
                    LogLevel::Error
                } else {
                    LogLevel::Info
                }
            }
            AgentSessionEvent::AgentEnd {
                messages,
                will_retry,
            } => {
                let _ = write!(
                    self.scratch,
                    "agent_end messages={} will_retry={}",
                    messages.len(),
                    will_retry
                );
                LogLevel::Info
            }
            AgentSessionEvent::AgentSettled => {
                self.scratch.push_str("agent_settled");
                LogLevel::Info
            }
            AgentSessionEvent::QueueUpdate {
                steering,
                follow_up,
            } => {
                let _ = write!(
                    self.scratch,
                    "queue_update steering={} follow_up={}",
                    steering.len(),
                    follow_up.len()
                );
                LogLevel::Debug
            }
            AgentSessionEvent::CompactionStart { reason } => {
                let _ = write!(self.scratch, "compaction_start reason={reason:?}");
                LogLevel::Warn
            }
            AgentSessionEvent::EntryAppended { entry } => {
                let _ = write!(
                    self.scratch,
                    "entry_appended size~{}",
                    value_size_hint(entry)
                );
                LogLevel::Debug
            }
            AgentSessionEvent::SessionInfoChanged { name } => {
                let _ = write!(
                    self.scratch,
                    "session_info_changed name={}",
                    name.as_deref().unwrap_or("-")
                );
                LogLevel::Info
            }
            AgentSessionEvent::ThinkingLevelChanged { level } => {
                let _ = write!(self.scratch, "thinking_level_changed level={level}");
                LogLevel::Info
            }
            AgentSessionEvent::CompactionEnd {
                reason,
                result,
                aborted,
                will_retry,
                error_message,
            } => {
                let _ = write!(
                    self.scratch,
                    "compaction_end reason={reason:?} aborted={aborted} will_retry={will_retry} \
                     has_result={} err={}",
                    result.is_some(),
                    error_message
                        .as_deref()
                        .map(|m| truncate_for_log(m, 96))
                        .unwrap_or("-")
                );
                LogLevel::Warn
            }
            AgentSessionEvent::AutoRetryStart {
                attempt,
                max_attempts,
                delay_ms,
                error_message,
            } => {
                let _ = write!(
                    self.scratch,
                    "auto_retry_start attempt={attempt}/{max_attempts} delay_ms={delay_ms} err={}",
                    truncate_for_log(error_message, 96)
                );
                LogLevel::Warn
            }
            AgentSessionEvent::AutoRetryEnd {
                success,
                attempt,
                final_error,
            } => {
                let _ = write!(
                    self.scratch,
                    "auto_retry_end success={success} attempt={attempt} err={}",
                    final_error
                        .as_deref()
                        .map(|m| truncate_for_log(m, 96))
                        .unwrap_or("-")
                );
                if *success {
                    LogLevel::Info
                } else {
                    LogLevel::Error
                }
            }
        }
    }

    /// Push `self.scratch` (already filled by the caller) as a new entry:
    /// evict-and-reuse if the ring is full, write to the file sink if one is
    /// open, then clear `self.scratch` for the next call. O(1) amortised:
    /// `VecDeque::pop_front`/`push_back` are both O(1), and the file write is
    /// a single line write to an already-open, line-buffered handle.
    fn push_message(&mut self, level: LogLevel) {
        let elapsed = self.start.elapsed();
        let seq = self.next_seq;
        self.next_seq += 1;
        let message = self.take_message_slot();
        self.write_to_sink(seq, elapsed, level, &message);
        self.entries.push_back(Entry {
            seq,
            elapsed,
            level,
            message,
        });
        self.scratch.clear();
    }

    /// Produce the owned `String` for the entry about to be pushed. Once the
    /// ring is full this reuses the popped-front entry's `String` allocation
    /// (`clear` + `push_str`) instead of allocating a new one — the ring
    /// buffer's steady-state memory footprint never grows past [`CAPACITY`]
    /// entries' worth of string storage, no matter how long the process runs.
    fn take_message_slot(&mut self) -> String {
        if self.entries.len() >= CAPACITY {
            let mut reused = self
                .entries
                .pop_front()
                .expect("len() >= CAPACITY > 0 guarantees a front element");
            reused.message.clear();
            reused.message.push_str(&self.scratch);
            reused.message
        } else {
            self.scratch.clone()
        }
    }

    /// Best-effort file write. **Never panics on I/O failure**: a failed
    /// write disables the sink (so later calls stop paying for a doomed
    /// write attempt) and records a `Warn` entry directly, in-memory, so the
    /// operator can still see *that* logging degraded even though the file
    /// sink is gone.
    fn write_to_sink(&mut self, seq: u64, elapsed: Duration, level: LogLevel, message: &str) {
        use std::io::Write as _;
        let Some(sink) = self.sink.as_mut() else {
            return;
        };
        let result = writeln!(
            sink.writer,
            "[{seq:06}] {:>8.3}s {level} {message}",
            elapsed.as_secs_f64()
        );
        if let Err(err) = result {
            self.sink = None;
            let warn_seq = self.next_seq;
            self.next_seq += 1;
            if self.entries.len() >= CAPACITY {
                self.entries.pop_front();
            }
            self.entries.push_back(Entry {
                seq: warn_seq,
                elapsed: self.start.elapsed(),
                level: LogLevel::Warn,
                message: format!("debug log sink disabled: {err}"),
            });
        }
    }

    /// The most recent `max` entries (oldest first), each pre-formatted as a
    /// display line via [`Entry::format_line`]. Allocates — call this only
    /// when something is actually about to be shown (a render, a panic
    /// report), not on every event.
    pub fn recent(&self, max: usize) -> Vec<(LogLevel, String)> {
        let start = self.entries.len().saturating_sub(max);
        self.entries
            .iter()
            .skip(start)
            .map(|e| (e.level, e.format_line()))
            .collect()
    }
}

/// A cheap, shallow "how big is this JSON value" hint: element/char count for
/// strings, arrays and objects, `1` for scalars, `0` for null. Deliberately
/// **not** a recursive byte count (that would mean walking, and often
/// re-serializing, the whole payload — exactly what this module exists to
/// avoid) — just enough signal to tell "this tool result was empty" from
/// "this tool result was huge" at a glance.
fn value_size_hint(value: &Value) -> usize {
    match value {
        Value::Null => 0,
        Value::Bool(_) | Value::Number(_) => 1,
        Value::String(s) => s.len(),
        Value::Array(a) => a.len(),
        Value::Object(o) => o.len(),
    }
}

/// Truncate `s` to at most `max_chars` `char`s, appending an ellipsis if it
/// was cut. Char-boundary safe (unlike naive byte slicing), used to cap
/// error-message fields that can otherwise be arbitrarily long (a provider's
/// full error body) before they go into a log entry.
fn truncate_for_log(s: &str, max_chars: usize) -> &str {
    match s.char_indices().nth(max_chars) {
        Some((idx, _)) => &s[..idx],
        None => s,
    }
}

// ============================================================================
// Panic capture and copyable error reports
// ============================================================================

/// Build the message + location portion of a panic report from `info`. Pure
/// and standalone (no [`DebugLog`] access) so it is trivially unit-testable
/// by installing a temporary hook and triggering a real panic — see the
/// tests below.
pub fn panic_report(info: &std::panic::PanicHookInfo<'_>) -> String {
    use std::fmt::Write as _;
    let message = panic_payload_message(info);
    let location = info
        .location()
        .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
        .unwrap_or_else(|| "<unknown location>".to_string());
    let mut out = String::new();
    let _ = writeln!(out, "==== pirust panic (copy everything below) ====");
    let _ = writeln!(out, "message:  {message}");
    let _ = writeln!(out, "location: {location}");
    out
}

fn panic_payload_message(info: &std::panic::PanicHookInfo<'_>) -> String {
    if let Some(s) = info.payload().downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = info.payload().downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic payload>".to_string()
    }
}

/// Install a process-wide panic hook that turns a panic into a copyable
/// report (payload message, source location, and the last
/// [`PANIC_REPORT_LINES`] debug-log lines) instead of Rust's default raw
/// backtrace dump, printed to stderr and — if the sink is still open — also
/// written to the debug log's file.
///
/// # Why `Arc<Mutex<DebugLog>>` and not a lock-free channel
///
/// The hook needs read access to the *same* log the live TUI is writing to
/// (so a crash's report includes the events that led up to it, not a
/// separate copy). Three reasons this is a `Mutex`, not a channel:
///
/// 1. [`DebugLog`] already needs interior mutability to be updated as events
///    arrive from the render loop — a `Mutex` is the natural shape for
///    "one writer at a time, occasional readers," not a new design.
/// 2. A panic is rare and exceptional. The hook only contends for the lock
///    in the vanishingly unlikely case it fires *while* another call is
///    mid-`record_event`. It always uses `try_lock`, never a blocking
///    `lock` — so it can never deadlock or block the crashing thread, and
///    degrades to "recent activity unavailable" if it loses the race.
/// 3. A channel would need a consumer thread running somewhere to drain it
///    into the ring buffer, which is another thread and another shutdown
///    path to get the exact same "never block, never panic" guarantee
///    `try_lock` already gives for free.
///
/// The hook itself never panics: every fallible step (`try_lock`, the
/// eventual file write inside [`DebugLog::write_to_sink`]) already degrades
/// gracefully rather than propagating an error, so a panic *while handling a
/// panic* — which would abort the process — cannot happen from this code.
pub fn install_panic_hook(log: Arc<Mutex<DebugLog>>) {
    std::panic::set_hook(Box::new(move |info| {
        use std::fmt::Write as _;
        let mut report = panic_report(info);
        match log.try_lock() {
            Ok(mut guard) => {
                let recent = guard.recent(PANIC_REPORT_LINES);
                let _ = writeln!(report, "-- recent activity ({} lines) --", recent.len());
                for (_, line) in &recent {
                    let _ = writeln!(report, "{line}");
                }
                let _ = writeln!(report, "===============================================");
                // Record the crash itself into the log (and, transitively,
                // its file sink) as a single multi-line entry. This is the
                // one deliberate exception to "keep entries small": a panic
                // is rare enough that the extra size is worth the detail.
                guard.error(&report);
            }
            Err(_) => {
                let _ = writeln!(report, "-- recent activity unavailable (log busy) --");
                let _ = writeln!(report, "===============================================");
            }
        }
        eprintln!("{report}");
    }));
}

/// A copyable, terminal-safe "error details" block: what failed, which
/// request it belongs to (if any), and that request's timing summary (if
/// any). This is the non-panic counterpart to [`panic_report`] — surfaced
/// for ordinary tool/provider errors, not just crashes.
pub fn copyable_error(
    message: &str,
    request_id: Option<RequestId>,
    timings: Option<&TurnTimings>,
) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(out, "---- error (copy everything below) ----");
    if let Some(id) = request_id {
        let _ = writeln!(out, "request:  {id}");
    }
    if let Some(t) = timings {
        let _ = writeln!(out, "timing:   {}", t.summary());
    }
    let _ = writeln!(out, "message:  {message}");
    let _ = write!(out, "----------------------------------------");
    out
}

// ============================================================================
// Debug panel component
// ============================================================================

/// A `Component` that renders the tail of a [`DebugLog`] when visible and
/// nothing at all when hidden. Shares its log with [`install_panic_hook`] via
/// the same `Arc<Mutex<DebugLog>>` — one log, two readers.
pub struct DebugPanel {
    log: Arc<Mutex<DebugLog>>,
    visible: bool,
    max_lines: usize,
    cached_width: Option<usize>,
    /// The log's [`DebugLog::sequence`] value the cache was built from.
    /// Comparing this one integer is how [`DebugPanel::render`] knows
    /// whether the log changed since the last render, without ever
    /// comparing buffer contents.
    cached_seq: Option<u64>,
    cached_lines: Vec<String>,
}

impl DebugPanel {
    /// Default number of trailing log lines shown — enough to see a tool
    /// call and its result without eating the whole screen.
    pub const DEFAULT_MAX_LINES: usize = 12;

    pub fn new(log: Arc<Mutex<DebugLog>>) -> Self {
        Self {
            log,
            visible: false,
            max_lines: Self::DEFAULT_MAX_LINES,
            cached_width: None,
            cached_seq: None,
            cached_lines: Vec::new(),
        }
    }

    pub fn toggle(&mut self) {
        self.set_visible(!self.visible);
    }

    pub fn set_visible(&mut self, visible: bool) {
        if self.visible != visible {
            self.visible = visible;
            self.invalidate();
        }
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }
}

impl Component for DebugPanel {
    fn render(&mut self, width: usize) -> Vec<String> {
        // Hidden: no lock, no formatting, no allocation — the whole point of
        // a debug panel is that it costs nothing when nobody asked for it.
        if !self.visible {
            return Vec::new();
        }

        let (seq, entries) = {
            let log = self
                .log
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            (log.sequence(), log.recent(self.max_lines))
        };

        if self.cached_seq == Some(seq) && self.cached_width == Some(width) {
            // Matches `Text::render`'s own cached-clone precedent
            // (`pirust-tui/src/components/text.rs:79-82`): the `Component`
            // trait's `Vec<String>` return type forces an owned copy out no
            // matter what, so "zero allocation when unchanged" means "skip
            // the lock, the formatting and the color/width work," not "skip
            // the unavoidable final clone."
            return self.cached_lines.clone();
        }

        let mut rendered = Vec::with_capacity(entries.len());
        for (level, line) in &entries {
            let fitted = fit_to_width(line, width);
            let colored = match level {
                LogLevel::Error => bg(dark::TOOL_ERROR_BG)(&fitted),
                LogLevel::Debug => fg(dark::GRAY)(&fitted),
                LogLevel::Info | LogLevel::Warn => fg(dark::TEXT)(&fitted),
            };
            rendered.push(colored);
        }

        self.cached_width = Some(width);
        self.cached_seq = Some(seq);
        self.cached_lines = rendered.clone();
        rendered
    }

    fn invalidate(&mut self) {
        self.cached_width = None;
        self.cached_seq = None;
    }
}

/// Pad `line` to exactly `width` visible columns, or truncate with a
/// trailing ellipsis if it is longer. Debug-log lines are plain text (no
/// embedded ANSI), so this is a simpler pass than `pirust_tui`'s
/// ANSI-aware wrapping utilities need to be — [`visible_width`] is reused
/// rather than reimplemented, but the truncation itself is a plain `chars()`
/// walk.
fn fit_to_width(line: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let vw = visible_width(line);
    if vw <= width {
        return format!("{line}{}", " ".repeat(width - vw));
    }
    let truncated: String = line.chars().take(width.saturating_sub(1)).collect();
    format!("{truncated}\u{2026}")
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- RequestId -------------------------------------------------------

    #[test]
    fn request_id_display_is_zero_padded() {
        assert_eq!(RequestId::from_raw(17).to_string(), "req_00000017");
        assert_eq!(RequestId::from_raw(0).to_string(), "req_00000000");
    }

    #[test]
    fn request_id_next_is_monotonic_and_copy() {
        let a = RequestId::next();
        let b = RequestId::next();
        let a_copy = a; // Copy, not a move
        assert!(b.get() > a.get());
        assert_eq!(a, a_copy);
    }

    // ---- TurnTimings -------------------------------------------------------

    #[test]
    fn turn_timings_first_token_latency_is_none_until_marked() {
        let t = TurnTimings::new(RequestId::from_raw(1));
        assert!(t.first_token_latency().is_none());
    }

    #[test]
    fn turn_timings_summary_reports_id_and_tool_count() {
        let id = RequestId::from_raw(42);
        let mut t = TurnTimings::new(id);
        t.mark_first_token();
        t.mark_tool_start("call-1", "read_file");
        t.mark_tool_end("call-1");
        t.mark_end();

        assert_eq!(t.tool_count(), 1);
        assert_eq!(t.tool_durations()[0].name, "read_file");

        let summary = t.summary();
        assert!(summary.starts_with("req_00000042"));
        assert!(summary.contains("to first token"));
        assert!(summary.contains("total"));
        assert!(summary.contains("1 tool"));
    }

    #[test]
    fn turn_timings_mark_tool_end_without_start_is_a_harmless_no_op() {
        let mut t = TurnTimings::new(RequestId::from_raw(1));
        t.mark_tool_end("never-started");
        assert_eq!(t.tool_count(), 0);
    }

    // ---- DebugLog: ring buffer ---------------------------------------------

    #[test]
    fn ring_buffer_caps_length_and_evicts_oldest() {
        let mut log = DebugLog::new_without_sink();
        for i in 0..(CAPACITY + 25) {
            log.info(format!("msg-{i}"));
        }
        assert_eq!(log.len(), CAPACITY);
        assert_eq!(log.sequence(), (CAPACITY + 25) as u64);

        let newest = log.recent(1);
        assert_eq!(newest.len(), 1);
        assert!(newest[0].1.contains(&format!("msg-{}", CAPACITY + 24)));

        // The first 25 pushes must have been evicted; the oldest survivor is msg-25.
        let all = log.recent(CAPACITY);
        assert!(
            all[0].1.contains("msg-25"),
            "unexpected oldest line: {}",
            all[0].1
        );
        assert!(
            !all[0].1.contains("msg-24 "),
            "msg-24 should have been evicted"
        );
    }

    #[test]
    fn record_event_does_not_serialize_large_payloads() {
        let mut log = DebugLog::new_without_sink();
        let huge = "x".repeat(50_000);
        let event = AgentSessionEvent::MessageUpdate {
            assistant_message_event: serde_json::json!({"text": huge}),
            message: serde_json::json!({
                "role": "assistant",
                "content": [{"type": "text", "text": huge}],
            }),
        };
        log.record_event(&event);
        let recent = log.recent(1);
        assert_eq!(recent.len(), 1);
        assert!(
            recent[0].1.len() < 200,
            "log entry must summarise, not embed, the payload: got {} bytes",
            recent[0].1.len()
        );
    }

    #[test]
    fn record_event_summarises_tool_execution_end_with_error_level() {
        let mut log = DebugLog::new_without_sink();
        let event = AgentSessionEvent::ToolExecutionEnd {
            tool_call_id: "call-9".to_string(),
            tool_name: "bash".to_string(),
            result: serde_json::json!("boom"),
            is_error: true,
        };
        log.record_event(&event);
        let recent = log.recent(1);
        assert_eq!(recent[0].0, LogLevel::Error);
        assert!(recent[0].1.contains("bash"));
        assert!(recent[0].1.contains("call-9"));
        assert!(recent[0].1.contains("is_error=true"));
    }

    #[test]
    fn record_event_covers_every_variant_without_panicking() {
        // Exercises the match in `write_event_summary` for every
        // `AgentSessionEvent` variant, proving the port is exhaustive and
        // that none of them can panic while formatting.
        let mut log = DebugLog::new_without_sink();
        let events = vec![
            AgentSessionEvent::AgentStart,
            AgentSessionEvent::TurnStart,
            AgentSessionEvent::TurnEnd {
                message: serde_json::json!({}),
                tool_results: vec![],
            },
            AgentSessionEvent::MessageStart {
                message: serde_json::json!({"role": "user"}),
            },
            AgentSessionEvent::MessageEnd {
                message: serde_json::json!({"role": "assistant"}),
            },
            AgentSessionEvent::ToolExecutionStart {
                tool_call_id: "c1".into(),
                tool_name: "grep".into(),
                args: serde_json::json!({}),
            },
            AgentSessionEvent::ToolExecutionUpdate {
                tool_call_id: "c1".into(),
                tool_name: "grep".into(),
                args: serde_json::json!({}),
                partial_result: serde_json::json!("partial"),
            },
            AgentSessionEvent::AgentEnd {
                messages: vec![],
                will_retry: false,
            },
            AgentSessionEvent::AgentSettled,
            AgentSessionEvent::QueueUpdate {
                steering: vec![],
                follow_up: vec![],
            },
            AgentSessionEvent::CompactionStart {
                reason: crate::print_mode::CompactionReason::Manual,
            },
            AgentSessionEvent::EntryAppended {
                entry: serde_json::json!({}),
            },
            AgentSessionEvent::SessionInfoChanged { name: None },
            AgentSessionEvent::ThinkingLevelChanged {
                level: "high".into(),
            },
            AgentSessionEvent::CompactionEnd {
                reason: crate::print_mode::CompactionReason::Threshold,
                result: None,
                aborted: false,
                will_retry: false,
                error_message: None,
            },
            AgentSessionEvent::AutoRetryStart {
                attempt: 1,
                max_attempts: 3,
                delay_ms: 500,
                error_message: "503".into(),
            },
            AgentSessionEvent::AutoRetryEnd {
                success: true,
                attempt: 1,
                final_error: None,
            },
        ];
        let count = events.len();
        for event in &events {
            log.record_event(event);
        }
        assert_eq!(log.len(), count);
    }

    // ---- DebugLog: file sink -------------------------------------------------

    #[test]
    fn file_sink_writes_lines_without_panicking() {
        let path = std::env::temp_dir().join(format!(
            "pirust_interactive_debug_test_{}_{}.log",
            std::process::id(),
            RequestId::next().get()
        ));
        let mut log = DebugLog::with_sink_path(&path);
        log.info("sink-line-one");
        log.warn("sink-line-two");

        let contents = std::fs::read_to_string(&path).expect("sink file should have been written");
        assert!(contents.contains("sink-line-one"));
        assert!(contents.contains("sink-line-two"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn file_sink_open_failure_degrades_to_in_memory_only() {
        // A directory cannot be opened in append mode: this must record a
        // warning and keep working, never panic.
        let dir = std::env::temp_dir();
        let mut log = DebugLog::with_sink_path(&dir);
        log.info("still-works-without-a-sink");
        let recent = log.recent(4);
        assert!(recent
            .iter()
            .any(|(_, l)| l.contains("still-works-without-a-sink")));
    }

    // ---- Panic report / copyable error --------------------------------------

    #[test]
    fn panic_report_contains_message_and_location() {
        let captured: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let captured_clone = Arc::clone(&captured);
        std::panic::set_hook(Box::new(move |info| {
            *captured_clone.lock().unwrap() = Some(panic_report(info));
        }));

        let result = std::panic::catch_unwind(|| {
            panic!("interactive-debug-test-marker");
        });
        assert!(result.is_err());

        // Restore a normal hook so later panics in this test binary behave
        // normally.
        let _ = std::panic::take_hook();

        let report = captured
            .lock()
            .unwrap()
            .take()
            .expect("hook should have run");
        assert!(report.contains("interactive-debug-test-marker"));
        assert!(report.contains("location:"));
    }

    #[test]
    fn copyable_error_includes_request_id_and_timing_when_given() {
        let id = RequestId::from_raw(7);
        let mut timings = TurnTimings::new(id);
        timings.mark_first_token();
        timings.mark_end();

        let with_context = copyable_error("boom", Some(id), Some(&timings));
        assert!(with_context.contains("req_00000007"));
        assert!(with_context.contains("boom"));
        assert!(with_context.contains("timing:"));

        let bare = copyable_error("boom", None, None);
        assert!(bare.contains("boom"));
        assert!(!bare.contains("request:"));
        assert!(!bare.contains("timing:"));
    }

    // ---- DebugPanel ----------------------------------------------------------

    #[test]
    fn debug_panel_hidden_renders_nothing() {
        let log = Arc::new(Mutex::new(DebugLog::new_without_sink()));
        let mut panel = DebugPanel::new(log);
        assert!(!panel.is_visible());
        assert!(panel.render(80).is_empty());
    }

    #[test]
    fn debug_panel_visible_renders_recent_lines_padded_to_width() {
        let log = Arc::new(Mutex::new(DebugLog::new_without_sink()));
        log.lock().unwrap().info("hello-panel");

        let mut panel = DebugPanel::new(Arc::clone(&log));
        panel.set_visible(true);
        let lines = panel.render(40);
        assert!(!lines.is_empty());
        assert!(lines.iter().any(|l| l.contains("hello-panel")));
        for l in &lines {
            assert_eq!(visible_width(l), 40);
        }
    }

    #[test]
    fn debug_panel_toggle_flips_visibility() {
        let log = Arc::new(Mutex::new(DebugLog::new_without_sink()));
        let mut panel = DebugPanel::new(log);
        assert!(!panel.is_visible());
        panel.toggle();
        assert!(panel.is_visible());
        panel.toggle();
        assert!(!panel.is_visible());
    }

    #[test]
    fn debug_panel_cache_is_reused_until_the_log_changes() {
        let log = Arc::new(Mutex::new(DebugLog::new_without_sink()));
        log.lock().unwrap().info("first");
        let mut panel = DebugPanel::new(Arc::clone(&log));
        panel.set_visible(true);

        let first_render = panel.render(30);
        assert!(panel.cached_seq.is_some());

        // Same width, same log sequence: must return the cached content.
        let second_render = panel.render(30);
        assert_eq!(first_render, second_render);

        // New event bumps the sequence: the cache must refresh.
        log.lock().unwrap().info("second");
        let third_render = panel.render(30);
        assert!(third_render.iter().any(|l| l.contains("second")));
    }

    #[test]
    fn fit_to_width_pads_short_lines_and_truncates_long_ones() {
        assert_eq!(fit_to_width("abc", 5), "abc  ");
        let truncated = fit_to_width("abcdefghij", 5);
        assert_eq!(visible_width(&truncated), 5);
        assert!(truncated.ends_with('\u{2026}'));
    }
}
