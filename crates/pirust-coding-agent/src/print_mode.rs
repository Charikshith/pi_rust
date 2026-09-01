//! Port of `modes/print-mode.ts` — the one-shot / piped run mode (text and json).
//!
//! Also carries `core/output-guard.ts`'s `takeOverStdout` / `writeRawStdout` semantics:
//! in non-interactive modes incidental stdout is redirected to stderr so stdout carries
//! only the payload.
//!
//! Spec: `docs/analysis/09-cli-config-spec.md` §13 (both output shapes, streams, exit
//! codes) and §16 hazard 11/12. Gated by `tests/fixtures/pi/printmode/` —
//! `text_mode.cases.jsonl` (23), `json_mode.cases.jsonl` (23),
//! `output_guard.cases.jsonl` (12), `exit_codes.json` (18 rows) and
//! `events.provenance.json` (how the events were harvested and what is *not* pinned).
//!
//! # What is ported
//!
//! | Pi | here |
//! |---|---|
//! | `PrintModeOptions` (`print-mode.ts:17-26`) | [`PrintModeOptions`] |
//! | `runPrintMode` (`:32-159`) | [`run_print_mode`] |
//! | `disposeRuntime` (`:40-45`) | private `dispose_runtime` |
//! | `registerSignalHandlers` (`:47-63`) | [`registered_signals`] + [`Signal::exit_code`] |
//! | `rebindSession` (`:71-109`) | private `rebind_session` |
//! | `AgentSessionEvent` (`core/agent-session.ts:136-162`) | [`AgentSessionEvent`] |
//! | `takeOverStdout` (`output-guard.ts:45-70`) | [`OutputGuard::take_over_stdout`] |
//! | `restoreStdout` (`:72-79`) | [`OutputGuard::restore_stdout`] |
//! | `isStdoutTakenOver` (`:81-83`) | [`OutputGuard::is_stdout_taken_over`] |
//! | `writeRawStdout` (`:85-93`) | [`OutputGuard::write_raw_stdout`] |
//! | `waitForRawStdoutBackpressure` (`:95-103`) | [`OutputGuard::wait_for_raw_stdout_backpressure`] |
//! | `flushRawStdout` (`:105-108`) | [`OutputGuard::flush_raw_stdout`] |
//! | `resolveAppMode` (`main.ts:100-111`) | [`resolve_app_mode`] |
//! | `toPrintOutputMode` (`main.ts:113-115`) | [`to_print_output_mode`] |
//! | `isPlainRuntimeMetadataCommand` (`main.ts:117-119`) | [`is_plain_runtime_metadata_command`] |
//! | `shouldTakeOverStdout` (`main.ts:541`) | [`should_take_over_stdout`] |
//! | `main.ts:856-857` (`process.exitCode` when non-zero) | [`process_exit_code`] |
//!
//! The four `main.ts` helpers live here rather than in `main.rs` because they *select*
//! print mode's output shape and gate the output guard — `main.rs` (wave 5) calls them.
//! They are pinned by `output_guard.cases.jsonl`'s 48-row `decisionTable` and
//! `exit_codes.json`'s identical `appModeAndStdoutTakeover.rows`.
//!
//! # The output contract, in one place
//!
//! **json mode** is compact `JSON.stringify`, *one object per line* — not a document,
//! no wrapper array, no indentation. The header line comes first and only if
//! `sessionManager.getHeader()` is truthy; then one line per event, produced by
//! `writeRawStdout(`${JSON.stringify(event)}\n`)` for **every** event with no switch on
//! `type` (`print-mode.ts:104-108`).
//!
//! **json mode never sets a non-zero exit for a failed turn.** `if (mode === "text")`
//! (`:129`) gates the whole error block, so a provider error or an abort exits **0** in
//! json mode and **1** in text mode. Only a *thrown* error exits 1 in both (`:149-151`).
//!
//! **Text mode** drops thinking and toolCall blocks (`:139-143` only looks at
//! `type === "text"`), gives each text block its own `\n`, and therefore emits a
//! **second** `\n` after a text that already ends in one. Error text is
//! `` errorMessage || `Request ${stopReason}` `` — a JS `||`, so an *empty* `errorMessage`
//! also falls back.
//!
//! `initialMessage: ""` is falsy (`:121`) so it is never prompted, but an empty string
//! inside `options.messages` **is** prompted (`:125-127` has no truthiness guard).
//!
//! Print mode emits **no ANSI at all**: `print-mode.ts` imports no chalk, so there is no
//! colour variant to model (`events.provenance.json` `determinism.ansi`).
//!
//! # The three seams Pi does not have
//!
//! - **The monkey-patched `process.stdout.write` → [`OutputGuard`].** Pi's guard is a
//!   module-global that *replaces* `process.stdout.write` with a function writing to
//!   stderr; Rust has nothing to monkey-patch. Here the guard is an explicit **writer
//!   pair** ([`OutputGuard::new`]) that the whole module writes through:
//!   [`OutputGuard::process_stdout_write`] (= the patched `process.stdout.write`, hence
//!   also `console.log`) routes to stderr while the takeover is engaged, and
//!   [`OutputGuard::write_raw_stdout`] always reaches the real stdout. This is a
//!   structural divergence with identical observable behaviour — which stream each write
//!   lands on, and in what order — and that is exactly what
//!   `output_guard.cases.jsonl`'s `landedOn` map pins.
//!   Pi's `rawStdoutWriteTail` promise chain exists to make async writes strictly
//!   ordered and non-interleaved; here a single `Mutex` around both writers gives the
//!   same guarantee without the chain, and the `ENOBUFS`/`EAGAIN`/`EWOULDBLOCK` retry
//!   loop (`output-guard.ts:34-41`) becomes a [`std::io::ErrorKind::WouldBlock`] /
//!   `Interrupted` retry with the same 10 ms delay ([`RAW_STDOUT_RETRY_DELAY_MS`]).
//! - **`process.on("SIGTERM", …)` → [`SignalRegistry`].** A library must not install
//!   process-wide handlers (and a test cannot deliver a real signal portably), so
//!   registration, removal, `killTrackedDetachedChildren()` and `process.exit` are trait
//!   methods. [`registered_signals`] reproduces the platform gate verbatim: SIGTERM
//!   always, SIGHUP **only** when `process.platform !== "win32"` (`print-mode.ts:48-51`)
//!   — which is why `exit_codes.json`'s SIGTERM row records `sighupAddedByPrintMode: 0`.
//! - **`process.exit(exitCode)` → a returned `i32`.** [`run_print_mode`] *returns* the
//!   code so tests can assert it; `main.rs` (wave 5) does the exiting, assigning
//!   `process.exitCode` only when non-zero ([`process_exit_code`]).
//!
//! # Why event payloads are [`serde_json::Value`]
//!
//! [`AgentSessionEvent`] is typed down to its own fields (the tag, and each event's own
//! key order — note `message_update` puts `assistantMessageEvent` **before** `message`,
//! matching the construction site at `agent-loop.ts:340-344` rather than the declaration
//! at `types.ts:425`). The *message* payloads, however, are carried as `Value` with
//! `preserve_order`, because the assistant-message key order is **provider-dependent**:
//! the faux provider that produced the fixture emits
//! `role, content, api, provider, model, usage, stopReason, errorMessage, timestamp`,
//! whereas [`pirust_ai::types::AssistantMessage`] is pinned to the Anthropic adapter's
//! *runtime* insertion order, which appends `errorMessage` **after** `timestamp`. Both
//! are correct for their own producer; re-serializing a faux message through the typed
//! struct would move the key and break the byte contract, and the fix does not belong in
//! a sibling crate. `Value` reproduces `JSON.stringify` exactly for any producer.
//!
//! Reading the final state does *not* have that problem (deserialization is
//! order-independent), so the text branch works on a typed
//! [`pirust_agent_core::types::AgentMessage`].
//!
//! # Divergences
//!
//! - Pi's `writeRawStdout` calls `process.exit(1)` from inside the promise chain when a
//!   write fails non-retryably (`output-guard.ts:90-92`). A library cannot exit, so the
//!   failure is recorded ([`OutputGuard::write_failure`]) for the binary to act on.
//! - `killTrackedDetachedChildren()` (`utils/shell.ts`) is not ported in feat-005;
//!   [`SignalRegistry::kill_tracked_detached_children`] defaults to a no-op so the call
//!   site stays visible.
//! - `AgentSessionEvent`'s `queue_update`, `compaction_start`, `compaction_end`,
//!   `entry_appended`, `session_info_changed`, `thinking_level_changed` and
//!   `tool_execution_update` variants are **not** in the fixture
//!   (`events.provenance.json` `notCaptured`); their key order follows the union
//!   declaration and is therefore *unverified*. Everything else here is byte-pinned.

use std::io::Write;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use futures::future::BoxFuture;
use pirust_agent_core::harness::messages::AgentMessage;
use pirust_agent_core::harness::types::SessionHeader;
use pirust_ai::types::{AssistantContent, ImageContent, Message, StopReason};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::Platform;

// ============================================================================
// output-guard.ts
// ============================================================================

/// `RAW_STDOUT_RETRY_DELAY_MS` (`output-guard.ts:9`).
pub const RAW_STDOUT_RETRY_DELAY_MS: Duration = Duration::from_millis(10);

/// Which real stream a write reached. Mirrors `output_guard.cases.jsonl`'s `landedOn`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Stream {
    Stdout,
    Stderr,
}

/// The argument shapes the patched `process.stdout.write` accepts
/// (`output-guard.ts:54-63`). The encoding is **dropped** (`String(chunk)` is used) and
/// the callback is forwarded whether it arrived as arg 2 or arg 3.
pub enum StdoutWriteArgs<'a> {
    /// `process.stdout.write(chunk)`.
    Plain,
    /// `process.stdout.write(chunk, callback)` — `typeof encodingOrCallback === "function"`.
    Callback(&'a mut dyn FnMut(Option<&std::io::Error>)),
    /// `process.stdout.write(chunk, encoding, callback)` — encoding ignored.
    EncodingAndCallback(&'a str, &'a mut dyn FnMut(Option<&std::io::Error>)),
}

/// Present exactly while `stdoutTakeoverState !== undefined` (`output-guard.ts:7`).
///
/// Pi's state also caches `rawStdoutWrite`, `rawStderrWrite` and
/// `originalStdoutWrite`; here the two writers are owned by [`OutputGuard`] for the whole
/// process lifetime, so only the *flag* is state. A second `takeOverStdout()` early-returns
/// (`:46-48`), which is why **one** `restoreStdout()` fully restores — modelling this as a
/// nesting depth would be a defect.
struct Takeover;

struct GuardInner {
    /// The bound `process.stdout.write` captured before any patching — the only path to
    /// the real stdout while taken over.
    raw_stdout: Box<dyn Write + Send>,
    /// `process.stderr.write`, which `takeOverStdout` never touches.
    stderr: Box<dyn Write + Send>,
    takeover: Option<Takeover>,
    write_failure: Option<String>,
}

/// `core/output-guard.ts` as an explicit writer pair.
///
/// One `Mutex` covers both writers, so writes are strictly ordered and never interleaved
/// — the guarantee Pi buys with its `rawStdoutWriteTail` promise chain
/// (`output-guard.ts:11,89`). The relative order *across* the two streams is not
/// observable (the fixture's own note), only the order within each.
pub struct OutputGuard {
    inner: Mutex<GuardInner>,
}

impl OutputGuard {
    /// Build a guard over an explicit writer pair.
    pub fn new(stdout: Box<dyn Write + Send>, stderr: Box<dyn Write + Send>) -> Self {
        Self {
            inner: Mutex::new(GuardInner {
                raw_stdout: stdout,
                stderr,
                takeover: None,
                write_failure: None,
            }),
        }
    }

    /// The production guard: the process's real stdout / stderr.
    pub fn from_process() -> Self {
        Self::new(Box::new(std::io::stdout()), Box::new(std::io::stderr()))
    }

    /// `takeOverStdout()` (`output-guard.ts:45-70`) — **idempotent**. After this,
    /// `process.stdout.write` (and therefore every `console.log`) writes to stderr, while
    /// [`write_raw_stdout`](Self::write_raw_stdout) still reaches the real stdout.
    pub fn take_over_stdout(&self) {
        let mut inner = self.lock();
        if inner.takeover.is_some() {
            // `output-guard.ts:46-48`: the early return is what makes ONE
            // `restoreStdout()` enough.
            return;
        }
        inner.takeover = Some(Takeover);
    }

    /// `restoreStdout()` (`output-guard.ts:72-79`) — a no-op when nothing was taken over.
    pub fn restore_stdout(&self) {
        let mut inner = self.lock();
        if inner.takeover.is_none() {
            return;
        }
        inner.takeover = None;
    }

    /// `isStdoutTakenOver()` (`output-guard.ts:81-83`).
    pub fn is_stdout_taken_over(&self) -> bool {
        self.lock().takeover.is_some()
    }

    /// `writeRawStdout(text)` (`output-guard.ts:85-93`) — the only way to reach the real
    /// stdout while taken over. An empty string is an early return: **nothing** is
    /// written and no drain barrier is queued.
    pub fn write_raw_stdout(&self, text: &str) {
        if text.is_empty() {
            // `output-guard.ts:86-88`.
            return;
        }
        let mut inner = self.lock();
        let result = write_retrying(&mut *inner.raw_stdout, text.as_bytes());
        Self::record(&mut inner, result);
    }

    /// `waitForRawStdoutBackpressure()` (`output-guard.ts:95-103`).
    ///
    /// Pi awaits the promise tail until it stops changing. Writes here complete
    /// synchronously under the mutex, so once this call acquires the lock there is
    /// nothing outstanding.
    pub fn wait_for_raw_stdout_backpressure(&self) {
        drop(self.lock());
    }

    /// `flushRawStdout()` (`output-guard.ts:105-108`) — drain, then the zero-byte
    /// `write("")` barrier. Note the empty-string early return lives in
    /// `writeRawStdout`, **not** in `writeRawStdoutChunk`, so this really does hit the
    /// stream.
    pub fn flush_raw_stdout(&self) {
        let mut inner = self.lock();
        let result = inner.raw_stdout.flush();
        Self::record(&mut inner, result);
    }

    /// `console.log(text)`: `process.stdout.write(`${text}\n`)`, therefore subject to the
    /// takeover (spec §13.2 — every bootstrap `console.log` lands on stderr in print mode).
    pub fn log(&self, text: &str) {
        self.process_stdout_write(&format!("{text}\n"), StdoutWriteArgs::Plain);
    }

    /// `console.error(text)`: `process.stderr.write(`${text}\n`)`, never redirected.
    pub fn error(&self, text: &str) {
        self.process_stderr_write(&format!("{text}\n"));
    }

    /// `process.stdout.write(...)` — the patched function while taken over
    /// (`output-guard.ts:54-63`), the bound original otherwise. Returns Node's `boolean`.
    pub fn process_stdout_write(&self, chunk: &str, args: StdoutWriteArgs<'_>) -> bool {
        let mut inner = self.lock();
        let result = if inner.takeover.is_some() {
            write_retrying(&mut *inner.stderr, chunk.as_bytes())
        } else {
            write_retrying(&mut *inner.raw_stdout, chunk.as_bytes())
        };
        let err = result.as_ref().err();
        match args {
            StdoutWriteArgs::Plain => {}
            StdoutWriteArgs::Callback(callback) => callback(err),
            StdoutWriteArgs::EncodingAndCallback(_encoding, callback) => callback(err),
        }
        Self::record(&mut inner, result);
        true
    }

    /// `process.stderr.write(chunk)` — untouched by `takeOverStdout`. Returns Node's
    /// `boolean`.
    pub fn process_stderr_write(&self, chunk: &str) -> bool {
        let mut inner = self.lock();
        let result = write_retrying(&mut *inner.stderr, chunk.as_bytes());
        Self::record(&mut inner, result);
        true
    }

    /// The first non-retryable write error, if any. Pi's `writeRawStdout` reacts with
    /// `process.exit(1)` (`output-guard.ts:90-92`); a library records it instead.
    pub fn write_failure(&self) -> Option<String> {
        self.lock().write_failure.clone()
    }

    fn record(inner: &mut GuardInner, result: std::io::Result<()>) {
        if let Err(error) = result {
            if inner.write_failure.is_none() {
                inner.write_failure = Some(error.to_string());
            }
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, GuardInner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// `writeRawStdoutChunk` (`output-guard.ts:20-43`): retry forever on
/// `ENOBUFS`/`EAGAIN`/`EWOULDBLOCK` with a 10 ms delay, rethrow anything else.
fn write_retrying(writer: &mut (dyn Write + Send), bytes: &[u8]) -> std::io::Result<()> {
    loop {
        match writer.write_all(bytes) {
            Ok(()) => return writer.flush(),
            Err(error) if is_retryable(&error) => std::thread::sleep(RAW_STDOUT_RETRY_DELAY_MS),
            Err(error) => return Err(error),
        }
    }
}

/// `code !== "ENOBUFS" && code !== "EAGAIN" && code !== "EWOULDBLOCK"`
/// (`output-guard.ts:37`). `EAGAIN` and `EWOULDBLOCK` are both
/// [`std::io::ErrorKind::WouldBlock`]; `ENOBUFS` has no `ErrorKind`, so it is matched by
/// raw errno (105 on Linux, 55 on macOS; unused on win32).
fn is_retryable(error: &std::io::Error) -> bool {
    if error.kind() == std::io::ErrorKind::WouldBlock
        || error.kind() == std::io::ErrorKind::Interrupted
    {
        return true;
    }
    matches!(error.raw_os_error(), Some(105) | Some(55))
}

// ============================================================================
// Signals (print-mode.ts:47-63)
// ============================================================================

/// The signals print mode may register (`print-mode.ts:48`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Signal {
    #[serde(rename = "SIGTERM")]
    Sigterm,
    #[serde(rename = "SIGHUP")]
    Sighup,
}

impl Signal {
    /// The Node signal name.
    pub fn as_str(self) -> &'static str {
        match self {
            Signal::Sigterm => "SIGTERM",
            Signal::Sighup => "SIGHUP",
        }
    }

    /// `signal === "SIGHUP" ? 129 : 143` (`print-mode.ts:57`).
    pub fn exit_code(self) -> i32 {
        match self {
            Signal::Sigterm => 143,
            Signal::Sighup => 129,
        }
    }
}

/// `const signals: NodeJS.Signals[] = ["SIGTERM"]; if (process.platform !== "win32")
/// signals.push("SIGHUP")` (`print-mode.ts:48-51`).
///
/// The gate is why `exit_codes.json`'s SIGTERM row records
/// `sigtermAddedByPrintMode: 1` and `sighupAddedByPrintMode: 0` on win32.
pub fn registered_signals(platform: Platform) -> Vec<Signal> {
    let mut signals = vec![Signal::Sigterm];
    if platform != Platform::Win32 {
        signals.push(Signal::Sighup);
    }
    signals
}

/// Opaque handle standing in for the `handler` reference `process.off` needs
/// (`print-mode.ts:61`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignalToken(pub u64);

/// The handler body print mode installs (`print-mode.ts:54-59`): it runs
/// `killTrackedDetachedChildren()` synchronously, then kicks off the async dispose whose
/// `finally` exits. The returned future is what `void …finally(…)` schedules.
pub type SignalHandler = Arc<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>;

/// `process.on` / `process.off` / `process.exit`, as a seam.
pub trait SignalRegistry: Send + Sync {
    /// `process.on(signal, handler)` (`print-mode.ts:60`).
    fn on(&self, signal: Signal, handler: SignalHandler) -> SignalToken;

    /// `process.off(signal, handler)` (`print-mode.ts:61`, run from the `finally`).
    fn off(&self, signal: Signal, token: SignalToken);

    /// `killTrackedDetachedChildren()` (`print-mode.ts:55`). `utils/shell.ts`'s tracker
    /// is not ported in feat-005, so the default is a no-op.
    fn kill_tracked_detached_children(&self) {}

    /// `process.exit(code)` (`print-mode.ts:57`).
    fn exit(&self, code: i32);
}

/// A registry that installs nothing and never exits — for callers that do not model
/// signals (and for `--help`-style paths that never reach a run).
pub struct NoSignals;

impl SignalRegistry for NoSignals {
    fn on(&self, _signal: Signal, _handler: SignalHandler) -> SignalToken {
        SignalToken(0)
    }
    fn off(&self, _signal: Signal, _token: SignalToken) {}
    fn exit(&self, _code: i32) {}
}

// ============================================================================
// AgentSessionEvent (core/agent-session.ts:136-162)
// ============================================================================

/// `compaction_start` / `compaction_end`'s `reason` (`agent-session.ts:149,155`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CompactionReason {
    Manual,
    Threshold,
    Overflow,
}

/// coding-agent's `AgentSessionEvent` (`core/agent-session.ts:136-162`): every loop
/// [`pirust_agent_core::types::AgentEvent`] **except** `agent_end`, plus a widened
/// `agent_end` and nine session-level additions.
///
/// # Why not reuse [`pirust_agent_core::types::AgentEvent`]
///
/// Two reasons, both byte-level:
/// 1. `agent_end` is *replaced* here (`Exclude<AgentEvent, {type:"agent_end"}>` + a
///    `{type, messages, willRetry}` arm), so the loop enum cannot express the union.
/// 2. Agent-core's `AgentEvent::MessageUpdate` declares `message` before
///    `assistantMessageEvent` (following `types.ts:425`), but the construction site
///    emits `assistantMessageEvent` first (`agent-loop.ts:340-344`) — and
///    `JSON.stringify` follows the construction site. The fixture's every
///    `message_update` line is `{"type":…,"assistantMessageEvent":…,"message":…}`.
///    Serializing agent-core's enum would emit the two keys the other way round. That is
///    a latent defect in the loop type, not something to fix from here.
///
/// Message payloads are [`Value`]; see the module docs for why.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentSessionEvent {
    /// Run started (`types.ts:417`).
    AgentStart,
    /// A turn started (`types.ts:420`).
    TurnStart,
    /// A turn completed (`types.ts:421`).
    TurnEnd {
        message: Value,
        #[serde(rename = "toolResults")]
        tool_results: Vec<Value>,
    },
    /// A user / assistant / toolResult message started (`types.ts:423`).
    MessageStart { message: Value },
    /// Assistant streaming delta (`types.ts:425`). Key order is the construction
    /// site's: `assistantMessageEvent` **then** `message` (`agent-loop.ts:340-344`).
    MessageUpdate {
        #[serde(rename = "assistantMessageEvent")]
        assistant_message_event: Value,
        message: Value,
    },
    /// A message finished (`types.ts:426`).
    MessageEnd { message: Value },
    /// A tool began executing (`types.ts:428`).
    ToolExecutionStart {
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        #[serde(rename = "toolName")]
        tool_name: String,
        args: Value,
    },
    /// A tool streamed a partial update (`types.ts:429`). Not in the fixture.
    ToolExecutionUpdate {
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        #[serde(rename = "toolName")]
        tool_name: String,
        args: Value,
        #[serde(rename = "partialResult")]
        partial_result: Value,
    },
    /// A tool finished executing (`types.ts:430`).
    ToolExecutionEnd {
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        #[serde(rename = "toolName")]
        tool_name: String,
        result: Value,
        #[serde(rename = "isError")]
        is_error: bool,
    },
    /// The **widened** `agent_end` (`agent-session.ts:138-142`): the loop's
    /// `{type, messages}` plus `willRetry`.
    AgentEnd {
        messages: Vec<Value>,
        #[serde(rename = "willRetry")]
        will_retry: bool,
    },
    /// The run and everything it queued has settled (`agent-session.ts:143`). Emitted
    /// once per prompt, *after* the last `agent_end`.
    AgentSettled,
    /// Steering / follow-up queue changed (`agent-session.ts:144-148`). Not in the
    /// fixture — key order unverified.
    QueueUpdate {
        steering: Vec<String>,
        #[serde(rename = "followUp")]
        follow_up: Vec<String>,
    },
    /// Compaction started (`agent-session.ts:149`). Not in the fixture.
    CompactionStart { reason: CompactionReason },
    /// A session entry was appended by the extension API (`agent-session.ts:150`). Not
    /// in the fixture — needs a loaded extension (feat-007).
    EntryAppended { entry: Value },
    /// `/name` changed the session label (`agent-session.ts:151`). Not in the fixture.
    SessionInfoChanged {
        /// `string | undefined`: `undefined` is omitted by `JSON.stringify`.
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    /// `/thinking` changed the reasoning level (`agent-session.ts:152`). Not in the
    /// fixture.
    ThinkingLevelChanged { level: String },
    /// Compaction finished (`agent-session.ts:153-160`). Not in the fixture.
    CompactionEnd {
        reason: CompactionReason,
        /// `CompactionResult | undefined` — the key is always set, but `undefined` is
        /// omitted by `JSON.stringify`.
        #[serde(skip_serializing_if = "Option::is_none")]
        result: Option<Value>,
        aborted: bool,
        #[serde(rename = "willRetry")]
        will_retry: bool,
        #[serde(rename = "errorMessage", skip_serializing_if = "Option::is_none")]
        error_message: Option<String>,
    },
    /// A retryable provider error is about to be retried (`agent-session.ts:161`).
    /// Fires for real on a `503` (`ai/src/utils/retry.ts`).
    AutoRetryStart {
        attempt: u32,
        #[serde(rename = "maxAttempts")]
        max_attempts: u32,
        #[serde(rename = "delayMs")]
        delay_ms: u64,
        #[serde(rename = "errorMessage")]
        error_message: String,
    },
    /// The retry finished (`agent-session.ts:162`). `finalError` is present only on
    /// failure.
    AutoRetryEnd {
        success: bool,
        attempt: u32,
        #[serde(rename = "finalError", skip_serializing_if = "Option::is_none")]
        final_error: Option<String>,
    },
}

// ============================================================================
// The session / runtime seam
// ============================================================================

/// A thrown JS value, as `print-mode.ts:150` renders it:
/// `error instanceof Error ? error.message : String(error)`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ThrownValue {
    /// An `Error`; carries `error.message`.
    #[error("{0}")]
    Error(String),
    /// Anything else; carries the already-`String()`-converted value (there is no JS
    /// runtime here to run the conversion — e.g. a plain object is `"[object Object]"`).
    #[error("{0}")]
    NonError(String),
}

impl ThrownValue {
    /// The string `console.error` receives (`print-mode.ts:150`).
    pub fn console_message(&self) -> &str {
        match self {
            ThrownValue::Error(message) | ThrownValue::NonError(message) => message,
        }
    }
}

/// `{ cancelled: boolean }` — the narrowed result print mode returns from `fork` and
/// `navigateTree` (`print-mode.ts:79,89`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cancelled {
    pub cancelled: bool,
}

/// The four fields print mode explicitly re-picks when forwarding `navigateTree`
/// (`print-mode.ts:83-88`) — a pass-through would also carry the caller's other keys.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NavigateTreeOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summarize: Option<bool>,
    #[serde(rename = "customInstructions", skip_serializing_if = "Option::is_none")]
    pub custom_instructions: Option<String>,
    #[serde(
        rename = "replaceInstructions",
        skip_serializing_if = "Option::is_none"
    )]
    pub replace_instructions: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// `session.state` — print mode reads **only** `messages` (`print-mode.ts:130-131`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionStateView {
    pub messages: Vec<AgentMessage>,
}

/// A tool-call awaiting the user's approval, surfaced to the interactive
/// layer as a prompt (tool-approval.ts `pendingApproval`).
#[derive(Debug, Clone, PartialEq)]
pub struct ToolApprovalRequest {
    /// The tool name, e.g. `"bash"`.
    pub tool_name: String,
    /// The tool's arguments JSON.
    pub args: serde_json::Value,
}

/// The user's decision on a [`ToolApprovalRequest`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolApprovalDecision {
    /// Run this call once only.
    RunOnce,
    /// Allow this tool without asking again.
    AlwaysAllow,
    /// Block this call.
    Deny,
}

/// The interactive layer's tool-approval decider: given a tool call about to
/// execute, returns the user's decision. Runs on the agent loop thread, so it
/// returns a future the hook awaits.
pub type ToolApprovalDecider =
    Arc<dyn Fn(ToolApprovalRequest) -> BoxFuture<'static, ToolApprovalDecision> + Send + Sync>;

/// The runtime identity/status the TUI's persistent status line shows
/// (plan.md step 3). `AgentHarness`/the session owns the state; this is a
/// read-only projection, so the TUI stays UI-agnostic-free.
#[derive(Debug, Clone, PartialEq)]
pub struct TuiRuntimeStatus {
    /// Provider id of the active model, e.g. `"anthropic"`.
    pub provider: String,
    /// Model id, e.g. `"claude-sonnet-4-5"`.
    pub model: String,
    /// Human model name, e.g. `"Claude Sonnet 4.5"`.
    pub model_name: String,
    /// Context window in tokens.
    pub context_window: u64,
    /// Whether the model advertises reasoning support.
    pub reasoning_supported: bool,
    /// Active reasoning level (`thinking_level_as_str`).
    pub thinking_level: String,
    /// Context usage: input + output tokens of the current transcript.
    pub context_tokens: u64,
    /// Total cost of the current transcript in USD.
    pub cost: f64,
    /// Whether tools are enabled for the current session.
    pub tools_enabled: bool,
}

/// The slice of the runtime the TUI reads for status rendering. `SingleTurnSession`
/// implements it from the real `Agent`; test stubs implement it directly.
pub trait TuiRuntimeInfo: Send + Sync {
    /// A snapshot of the runtime identity/status for the status line.
    fn runtime_status(&self) -> TuiRuntimeStatus;

    /// The names of the tools this session has enabled, for the first-run
    /// welcome block (`docs/tui-design-samples.html` §1 lists them).
    ///
    /// A **defaulted** method rather than a [`TuiRuntimeStatus`] field:
    /// `TuiRuntimeStatus` is constructed as a struct literal by every golden
    /// stub in the test suites, so adding a field there would break all of
    /// them to carry data they have nothing to put in. The real set is
    /// assembled in `sdk.rs` (`initial_active_tool_names`) and has never been
    /// threaded back to the TUI; until it is, the default empty list makes the
    /// welcome block render no tool row, which is honest. A fabricated list
    /// would not be.
    fn tool_names(&self) -> Vec<String> {
        Vec::new()
    }

    /// The resumable sessions on disk, newest first, for `/resume`.
    ///
    /// Defaulted for the same reason as [`Self::tool_names`]: the golden stubs
    /// have no session store. `SingleTurnSession` overrides it from the
    /// `SessionManager` it already holds, so the real picker is real — the
    /// version this replaced listed only the *current* session and answered
    /// "Session resume is not available" on Enter.
    fn session_entries(&self) -> Vec<crate::interactive_pickers::SessionEntry> {
        Vec::new()
    }

    /// `/model` (live switch) — replace the running model in place, taking effect on
    /// the very next turn.
    ///
    /// Defaulted, not required, for the same reason as [`Self::tool_names`] and
    /// [`Self::session_entries`]: three suites hand-implement `TuiRuntimeInfo`
    /// directly (`tests/print_mode_golden.rs`, `tests/tui_commands_status.rs`,
    /// `tests/interactive_mode_smoke.rs`) and have no model to switch, so a
    /// required method would break all three for a capability they cannot honor.
    ///
    /// This is honest to default, unlike a cosmetic stub, because the real
    /// implementer's job is genuinely simple: `Agent::set_model(&self, model:
    /// &Model)` (`crates/pirust-agent-core/src/agent.rs:370`) already exists and
    /// mutates `AgentInner`'s `Mutex<MutableAgentState>` in place — copy-on-assign,
    /// mirroring `agent.ts:82-84`. `Agent` is `#[derive(Clone)]` over an
    /// `Arc<AgentInner>` (`agent.rs:295-298`), so every clone of the `Agent` the
    /// session holds observes the mutation immediately; there is nothing to
    /// rebuild or swap. In particular `build_stream_fn` (`sdk.rs:246-314`) takes
    /// `model` as a **call-time argument** to its returned closure and dispatches
    /// on `model.api.0.as_str()` fresh on every call (`sdk.rs:295`) — there is no
    /// cached per-model provider adapter anywhere to invalidate — so flipping
    /// `AgentInner`'s `model` field is sufficient by itself; no session
    /// reconstruction, no adapter rebuild. The default reports the switch
    /// unavailable, for sessions with no underlying `Agent` to set it on.
    fn set_model(&self, _model: &pirust_ai::types::Model) -> Result<(), String> {
        Err("model switching is not wired to this session".to_string())
    }

    /// `/model` (live switch), selected from
    /// [`crate::interactive_pickers::ModelPicker`] — resolve a `provider`/`model_id`
    /// string pair into a real `pirust_ai::types::Model` and apply it via
    /// [`Self::set_model`].
    ///
    /// A second seam rather than widening [`Self::set_model`]'s signature or
    /// `ModelEntry` itself: `ModelEntry` (`interactive_pickers.rs`) is deliberately
    /// string-only (see its own doc comment) — the real `Model` value lives in the
    /// `ModelRuntime` `main.rs` composes and never hands to the session, the same gap
    /// [`Self::set_model`]'s doc comment describes. The alternative — carrying the real
    /// `Model` alongside every `ModelEntry` — would change
    /// `InteractiveMode::set_model_entries`'s signature from `Vec<ModelEntry>` to
    /// something `Model`-shaped, and `main.rs` is the only caller of that setter and is
    /// off limits this wave. Resolving by name here instead keeps `main.rs` untouched:
    /// whichever implementer holds (or can reach) the provider list can look
    /// `provider`/`model_id` up and delegate to [`Self::set_model`]. The default reports
    /// this unavailable, for sessions with nothing to resolve against — same as every
    /// other capability-gated default in this trait.
    fn set_model_by_name(&self, _provider: &str, _model_id: &str) -> Result<(), String> {
        Err("model switching is not wired to this session".to_string())
    }

    /// `/tree` — the session's branch tree, pre-order, for
    /// [`crate::interactive_pickers::BranchPicker`].
    ///
    /// Defaulted to empty for the same reason as [`Self::session_entries`]: the golden
    /// stubs (`tests/print_mode_golden.rs`, `tests/tui_commands_status.rs`,
    /// `tests/interactive_mode_smoke.rs`) have no session store to walk. The real
    /// implementer is expected to call
    /// `crate::interactive_pickers::load_branch_entries(&session_manager.list_branches())`
    /// over the `SessionManager` it already holds (`session.rs::SessionManager::list_branches`,
    /// a pre-order walk — see `load_branch_entries`'s own doc comment on why that order
    /// must not be re-sorted). An empty result renders "no branches" honestly in the TUI
    /// rather than opening a picker with nothing in it.
    fn branch_entries(&self) -> Vec<crate::interactive_pickers::BranchEntry> {
        Vec::new()
    }
}

/// `session.subscribe`'s return value: the unsubscribe thunk.
pub struct Subscription {
    unsubscribe: Box<dyn FnOnce() + Send>,
}

impl Subscription {
    /// Wrap an unsubscribe thunk.
    pub fn new(unsubscribe: impl FnOnce() + Send + 'static) -> Self {
        Self {
            unsubscribe: Box::new(unsubscribe),
        }
    }

    /// `unsubscribe()` (`print-mode.ts:43,103`).
    pub fn unsubscribe(self) {
        (self.unsubscribe)();
    }
}

/// The listener passed to `session.subscribe` (`print-mode.ts:104-108`).
pub type SessionEventListener = Arc<dyn Fn(&AgentSessionEvent) + Send + Sync>;

/// `bindExtensions`'s `mode` — `mode === "json" ? "json" : "print"`
/// (`print-mode.ts:74`) for print mode; the interactive TUI binds `"tui"`
/// (interactive-mode.ts:1862). Note text print mode binds as **`print`**, not
/// `text`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExtensionBindMode {
    Print,
    Json,
    /// Interactive TUI — `has_ui: true` (dialog-capable).
    Tui,
}

/// An extension error reported through `bindExtensions`'s `onError`
/// (`print-mode.ts:98-100`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionError {
    pub extension_path: String,
    pub error: String,
}

/// `onError` (`print-mode.ts:98-100`).
pub type ExtensionErrorHandler = Arc<dyn Fn(&ExtensionError) + Send + Sync>;

type WaitForIdleFn = Arc<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>;
type NewSessionFn = Arc<dyn Fn(Value) -> BoxFuture<'static, Value> + Send + Sync>;
type ForkFn = Arc<dyn Fn(String, Value) -> BoxFuture<'static, Cancelled> + Send + Sync>;
type NavigateTreeFn =
    Arc<dyn Fn(String, Option<NavigateTreeOptions>) -> BoxFuture<'static, Cancelled> + Send + Sync>;
type SwitchSessionFn = Arc<dyn Fn(String, Value) -> BoxFuture<'static, Value> + Send + Sync>;
type ReloadFn = Arc<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>;

/// The six `commandContextActions` closures (`print-mode.ts:76-97`), in object-literal
/// order — which is the order `Object.keys` reports and the fixture records as
/// `commandContextActionKeys`.
pub struct CommandContextActions {
    pub wait_for_idle: WaitForIdleFn,
    pub new_session: NewSessionFn,
    pub fork: ForkFn,
    pub navigate_tree: NavigateTreeFn,
    pub switch_session: SwitchSessionFn,
    pub reload: ReloadFn,
}

impl CommandContextActions {
    /// All no-op actions — used by tests that only exercise the extension
    /// binding, not the session-control commands.
    pub fn placeholder() -> Self {
        Self {
            wait_for_idle: Arc::new(|| Box::pin(async {})),
            new_session: Arc::new(|_| Box::pin(async { Value::Null })),
            fork: Arc::new(|_, _| Box::pin(async { Cancelled { cancelled: false } })),
            navigate_tree: Arc::new(|_, _| Box::pin(async { Cancelled { cancelled: false } })),
            switch_session: Arc::new(|_, _| Box::pin(async { Value::Null })),
            reload: Arc::new(|| Box::pin(async {})),
        }
    }
}

/// `Object.keys(commandContextActions)` (`print-mode.ts:76-97`).
pub const COMMAND_CONTEXT_ACTION_KEYS: [&str; 6] = [
    "waitForIdle",
    "newSession",
    "fork",
    "navigateTree",
    "switchSession",
    "reload",
];

/// `commandContextActions` (`print-mode.ts:76-97`) — builds the six extension-facing
/// closures over a given `host`/`session` pair. Shared by [`PrintModeRun`]'s own
/// `command_context_actions` (the RPC/print-mode path) and `main.rs`'s interactive arm
/// (P5, `docs/tui-pending-action-plan.md`), so the wiring exists in exactly one place —
/// `PrintModeRun::command_context_actions` used to hold this body directly, with `self.host`
/// in place of the `host` parameter here.
pub fn build_command_context_actions(
    host: &Arc<dyn AgentSessionRuntimeHost>,
    session: &Arc<dyn PrintModeSession>,
) -> CommandContextActions {
    let wait_session = Arc::clone(session);
    let navigate_session = Arc::clone(session);
    let reload_session = Arc::clone(session);
    let new_session_host = Arc::clone(host);
    let fork_host = Arc::clone(host);
    let switch_host = Arc::clone(host);

    CommandContextActions {
        // `:77`
        wait_for_idle: Arc::new(move || {
            let session = Arc::clone(&wait_session);
            Box::pin(async move { session.wait_for_idle().await })
        }),
        // `:78`
        new_session: Arc::new(move |options| {
            let host = Arc::clone(&new_session_host);
            Box::pin(async move { host.new_session(options).await })
        }),
        // `:79-82` — the result is narrowed to `{ cancelled }`.
        fork: Arc::new(move |entry_id, options| {
            let host = Arc::clone(&fork_host);
            Box::pin(async move {
                let result = host.fork(entry_id, options).await;
                Cancelled {
                    cancelled: result.cancelled,
                }
            })
        }),
        // `:83-90` — the four options are re-picked, then the result narrowed.
        navigate_tree: Arc::new(move |target_id, options| {
            let session = Arc::clone(&navigate_session);
            Box::pin(async move {
                let repicked = options.map(|options| NavigateTreeOptions {
                    summarize: options.summarize,
                    custom_instructions: options.custom_instructions,
                    replace_instructions: options.replace_instructions,
                    label: options.label,
                });
                let result = session.navigate_tree(&target_id, repicked).await;
                Cancelled {
                    cancelled: result.cancelled,
                }
            })
        }),
        // `:91-93`
        switch_session: Arc::new(move |session_path, options| {
            let host = Arc::clone(&switch_host);
            Box::pin(async move { host.switch_session(session_path, options).await })
        }),
        // `:94-96`
        reload: Arc::new(move || {
            let session = Arc::clone(&reload_session);
            Box::pin(async move { session.reload().await })
        }),
    }
}

/// The argument object print mode passes to `session.bindExtensions`
/// (`print-mode.ts:73-101`).
pub struct ExtensionBinding {
    pub mode: ExtensionBindMode,
    pub command_context_actions: CommandContextActions,
    pub on_error: ExtensionErrorHandler,
}

/// The slice of `AgentSession` print mode touches (`agent-session-runtime-events.test.ts`
/// and Pi's own `print-mode.test.ts` stub the same six members).
#[async_trait::async_trait]
pub trait PrintModeSession: Send + Sync {
    /// `session.sessionManager.getHeader()` (`print-mode.ts:113`). `None` is the falsy
    /// case that suppresses the header line entirely.
    fn header(&self) -> Option<SessionHeader>;

    /// `session.bindExtensions(…)` (`print-mode.ts:73-101`).
    async fn bind_extensions(&self, binding: ExtensionBinding) -> Result<(), ThrownValue>;

    /// `session.subscribe(listener)` (`print-mode.ts:104`).
    fn subscribe(&self, listener: SessionEventListener) -> Subscription;

    /// `session.prompt(text, options)` (`print-mode.ts:122,126`).
    async fn prompt(&self, text: &str, options: Option<PromptOptions>) -> Result<(), ThrownValue>;

    /// `session.state` (`print-mode.ts:130`).
    fn state(&self) -> SessionStateView;

    /// `session.waitForIdle()` (`print-mode.ts:77`).
    async fn wait_for_idle(&self);

    /// `session.navigateTree(targetId, …)` (`print-mode.ts:83-88`).
    async fn navigate_tree(
        &self,
        target_id: &str,
        options: Option<NavigateTreeOptions>,
    ) -> Cancelled;

    /// `session.reload()` (`print-mode.ts:95`).
    async fn reload(&self);

    /// Cooperatively cancel the in-flight run (`Agent::abort`, agent.rs:437).
    /// Cancels the run's token and lets the prompt future observe it and
    /// unwind normally, so `finish_run()` still runs (B2: a hard
    /// `JoinHandle::abort()` on the caller's task instead used to skip
    /// `finish_run()` and wedge every later prompt on `BusyPrompt`). The
    /// default is a no-op for sessions with no underlying `Agent` to cancel.
    fn abort(&self) {}

    /// Register the interactive layer's tool-approval decider. The session
    /// consults it (awaiting its returned future) from its `before_tool_call`
    /// hook and blocks a tool when it returns a non-allow decision. The
    /// default lets every tool run, matching Pi's default allow behaviour for
    /// the headless/trusted path; the TUI supplies the real decider.
    fn set_tool_approval_decider(&self, _decider: ToolApprovalDecider) {}

    /// `/reload-extensions` (Wave 5, pirust-only — no Pi TS counterpart, and
    /// distinct from `reload()` above, which is Pi's full skills/prompts/
    /// themes/context-files reload and remains unwired). Rescans
    /// `<agent_dir>/extensions/*.wasm` for files not already loaded and adds
    /// them to the running extension runner. Returns how many were added.
    /// The default covers a build without wasm-extensions support, or a
    /// session on which extensions were never bound.
    fn reload_wasm_extensions(&self) -> Result<usize, String> {
        Err("wasm extensions are not supported in this build".to_string())
    }

    /// `/name <name>` — persist the session's display label, returning the
    /// sanitized name that was written.
    ///
    /// This makes `/name` genuinely work rather than reporting itself
    /// unavailable. The store side always existed —
    /// `SessionManager::append_session_info` (`session-manager.ts:1065-1076`)
    /// appends the `session_info` entry and collapses newlines to spaces — it
    /// simply had no route out to the interactive layer, so the TUI answered
    /// "renaming is not wired to the session store" for every argument.
    ///
    /// The default still reports that, for sessions with no store behind them.
    fn set_session_name(&self, _name: &str) -> Result<String, String> {
        Err("renaming is not wired to the session store".to_string())
    }

    /// `/compact` — manually summarize and shrink the conversation history,
    /// making `/compact` genuinely work rather than reporting itself
    /// unavailable.
    ///
    /// The deterministic half of compaction (boundary detection, cut-point
    /// selection, token-budget accounting) already existed and was fully
    /// working — `harness::compaction::v4::prepare_compaction`
    /// (`harness/compaction/v4.rs:216`) — but it consumes a v4 session-tree
    /// `Entry` path, and `SingleTurnSession` (the TUI's only
    /// `PrintModeSession` implementer with a real `Agent` behind it,
    /// `runtime_host.rs`) has no `Entry` tree: it holds conversation state as
    /// a flat `Vec<AgentMessage>` on `Agent` (`agent.rs:345,360`). The
    /// missing piece this method's implementer is expected to use is
    /// `harness::compaction::v4::prepare_compaction_from_messages`
    /// (`harness/compaction/v4.rs`), which synthesizes a minimal `Entry`
    /// path from that flat list so the existing cut-point logic can run
    /// unmodified — no v4 tree state is added anywhere.
    ///
    /// An implementer that performs a real compaction is expected to emit
    /// `AgentSessionEvent::CompactionStart { reason }` before starting and
    /// `AgentSessionEvent::CompactionEnd { reason, aborted, .. }` after
    /// finishing (successfully or not) through its own listener — mirroring
    /// how `prompt()` emits synthetic `AgentSettled` — so the TUI's existing
    /// `render_event` handling for those two variants (which predates this
    /// method and was previously unreachable dead code) has something to
    /// react to. `Err` returned here is a delivery failure (e.g. compaction
    /// genuinely found nothing to compact); it does not by itself imply no
    /// `CompactionEnd` was emitted.
    ///
    /// The default reports compaction unavailable, matching every other
    /// capability-gated default in this trait, for sessions with no
    /// underlying `Agent`/store to compact.
    async fn compact(&self, _reason: CompactionReason) -> Result<(), String> {
        Err("manual compaction is not wired to this session".to_string())
    }

    /// `/new` — discard the current transcript and start a fresh, empty session in
    /// place, as if the process had just been launched.
    ///
    /// Mutates the live session directly: nothing is swapped out from under the
    /// caller, so any already-held reference (e.g. this same `Arc<dyn
    /// PrintModeSession>`) keeps working and now reflects the fresh session.
    ///
    /// Defaulted, not required, for the same reason as [`Self::set_session_name`]
    /// and [`Self::compact`]: four suites hand-implement `PrintModeSession`
    /// directly (`tests/print_mode_golden.rs`'s `StubSession`,
    /// `tests/tui_commands_status.rs`'s `StatusSession`,
    /// `tests/interactive_mode_smoke.rs`'s `StubSession`,
    /// `tests/tui_delayed_provider.rs`'s `DelayedSession`) and have no session
    /// store to reset — a required method would break all four for a capability
    /// they cannot honor.
    ///
    /// The real implementer is expected to call `SessionManager::new_session`
    /// (`session.rs:2165`, `&mut self, options: Option<&NewSessionOptions>) ->
    /// Result<Option<String>, SessionError>`) with `None` to reset the store's
    /// buffer/index/leaf and (in persisting mode) compute the fresh file path, and
    /// `Agent::set_messages(&self, messages: &[AgentMessage])`
    /// (`agent.rs:360`) with an empty slice so the in-memory transcript the
    /// `Agent` actually replays from (`agent.rs:345`) matches the now-empty store.
    /// Note this is unrelated to the `interactive_commands.rs::unavailable_reason`
    /// entry for `"new"`, which currently (wrongly) says it needs
    /// `CommandContextActions::new_session` — the TUI reaches session control
    /// through `PrintModeSession` directly, exactly like `/name` and `/compact`
    /// already do, never through `CommandContextActions`.
    ///
    /// The default reports this unavailable, for sessions with no underlying
    /// store to reset.
    fn start_new_session(&self) -> Result<(), String> {
        Err("starting a new session is not wired to this session".to_string())
    }

    /// `/clone` — duplicate the session at its current position (the current leaf,
    /// not the whole tree) into a new session file, and switch the live session to
    /// it. `Ok(Some(path))` carries the new session file's path; `Ok(None)` is the
    /// in-memory (non-persisting) case, matching
    /// `SessionManager::create_branched_session`'s own `None`-on-no-file contract.
    ///
    /// Defaulted, not required, for the same reason as [`Self::start_new_session`]:
    /// the same four hand-implemented test-suite stubs have no session store to
    /// clone.
    ///
    /// The real implementer is expected to call
    /// `SessionManager::create_branched_session(&mut self, leaf_id: &str) ->
    /// Result<Option<String>, SessionError>` (`session.rs:2775`) with the store's
    /// *current* leaf id (branching from "now" rather than an earlier point is what
    /// distinguishes `/clone` from `/fork`, below), then rebuild the `Agent`'s
    /// transcript for the new file with `crate::session::entries_to_agent_messages
    /// (entries: &[&FileEntry]) -> Vec<AgentMessage>` (`session.rs:1595`, `pub(crate)`)
    /// over the branched path's entries, and hand the result to
    /// `Agent::set_messages` (`agent.rs:360`). Per the note on
    /// [`Self::start_new_session`], `interactive_commands.rs::unavailable_reason`'s
    /// `"clone"` entry (citing `CommandContextActions::fork`/`::new_session`) is
    /// now stale for the same reason — this method is the real seam.
    ///
    /// The default reports this unavailable, for sessions with no underlying store
    /// to clone.
    fn clone_session(&self) -> Result<Option<String>, String> {
        Err("cloning is not wired to this session".to_string())
    }

    /// `/fork <entry_id>` — branch a new session off an *earlier* point in this
    /// session's history (`entry_id`, not the current leaf — that is
    /// [`Self::clone_session`]'s job) and switch the live session to it.
    /// `Ok(Some(path))` carries the new session file's path; `Ok(None)` is the
    /// in-memory (non-persisting) case, matching
    /// `SessionManager::create_branched_session`'s own `None`-on-no-file contract.
    ///
    /// Defaulted, not required, for the same reason as [`Self::start_new_session`]:
    /// the same four hand-implemented test-suite stubs have no session store to
    /// fork from.
    ///
    /// The real implementer is expected to call the same
    /// `SessionManager::create_branched_session(&mut self, leaf_id: &str) ->
    /// Result<Option<String>, SessionError>` (`session.rs:2775`) used by
    /// [`Self::clone_session`], but passing the caller-supplied `entry_id` instead
    /// of the store's current leaf — that one argument is the entire difference
    /// between `/fork` and `/clone` at this layer. As with `clone_session`, the
    /// transcript rebuild for the branched entries is
    /// `crate::session::entries_to_agent_messages` (`session.rs:1595`) followed by
    /// `Agent::set_messages` (`agent.rs:360`). Per the note on
    /// [`Self::start_new_session`], `interactive_commands.rs::unavailable_reason`'s
    /// `"fork"` entry (citing `CommandContextActions::fork`) is now stale for the
    /// same reason — this method is the real seam.
    ///
    /// The default reports this unavailable, for sessions with no underlying store
    /// to fork from.
    fn fork_from(&self, _entry_id: &str) -> Result<Option<String>, String> {
        Err("forking is not wired to this session".to_string())
    }

    /// Load a different session file into the *live* session, replacing its
    /// transcript and store state in place.
    ///
    /// This one seam is deliberately shared by two distinct commands so a second,
    /// near-duplicate method never needs to exist:
    /// - `/import <path>` — an arbitrary, caller-supplied file path.
    /// - `/resume` (in-place variant) — a path drawn from
    ///   [`TuiRuntimeInfo::session_entries`]'s own listing.
    ///
    /// Both are "point the live session at this file on disk" and nothing more;
    /// the only difference is where the path string came from, which is the
    /// caller's concern, not this method's.
    ///
    /// Defaulted, not required, for the same reason as [`Self::start_new_session`]:
    /// the same four hand-implemented test-suite stubs have no session store to
    /// repoint.
    ///
    /// The real implementer is expected to call
    /// `SessionManager::set_session_file(&mut self, session_file: &str) ->
    /// Result<(), SessionError>` (`session.rs:2113`) with `path`, then rebuild the
    /// `Agent`'s transcript from the newly loaded entries via
    /// `crate::session::entries_to_agent_messages` (`session.rs:1595`) and
    /// `Agent::set_messages` (`agent.rs:360`), the same two-step pattern as
    /// [`Self::clone_session`] and [`Self::fork_from`]. Per the note on
    /// [`Self::start_new_session`], `interactive_commands.rs::unavailable_reason`'s
    /// `"import"` entry (citing `CommandContextActions::new_session`) is now stale
    /// for the same reason — this method is the real seam, exactly like `/name`.
    ///
    /// The default reports this unavailable, for sessions with no underlying store
    /// to repoint.
    fn switch_to_session_file(&self, _path: &str) -> Result<(), String> {
        Err("switching session files is not wired to this session".to_string())
    }
}

/// `session.prompt`'s options (`print-mode.ts:122`). The initial prompt passes
/// `{ images: initialImages }`; every follow-up passes **no** options at all
/// (`print-mode.ts:126`), which the fixture records as `options: null`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PromptOptions {
    pub images: Option<Vec<ImageContent>>,
}

/// The slice of `AgentSessionRuntime` print mode touches
/// (`core/agent-session-runtime.ts`).
pub trait AgentSessionRuntimeHost: Send + Sync {
    /// `runtimeHost.session` (`print-mode.ts:35,72`) — re-read on every rebind, because
    /// the runtime may have swapped the session underneath.
    fn session(&self) -> Arc<dyn PrintModeSession>;

    /// `runtimeHost.setRebindSession(cb)` (`print-mode.ts:67-69`).
    fn set_rebind_session(&self, rebind: RebindSessionFn);

    /// `runtimeHost.dispose()` (`print-mode.ts:44`) — emits
    /// `session_shutdown{reason:"quit"}` then disposes the session
    /// (`agent-session-runtime.ts:395-402`).
    fn dispose(&self) -> BoxFuture<'_, ()>;

    /// `runtimeHost.newSession(options)` (`print-mode.ts:77`).
    fn new_session(&self, options: Value) -> BoxFuture<'_, Value>;

    /// `runtimeHost.fork(entryId, options)` (`print-mode.ts:79`).
    fn fork(&self, entry_id: String, options: Value) -> BoxFuture<'_, Cancelled>;

    /// `runtimeHost.switchSession(path, options)` (`print-mode.ts:92`).
    fn switch_session(&self, session_path: String, options: Value) -> BoxFuture<'_, Value>;
}

/// The callback handed to `setRebindSession` (`print-mode.ts:67-69`).
pub type RebindSessionFn =
    Arc<dyn Fn() -> BoxFuture<'static, Result<(), ThrownValue>> + Send + Sync>;

// ============================================================================
// print-mode.ts
// ============================================================================

/// `mode: "text" | "json"` (`print-mode.ts:19`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PrintOutputMode {
    /// Final assistant response only.
    #[default]
    Text,
    /// Newline-delimited JSON of the session header + every event.
    Json,
}

/// `AppMode` (`main.ts:100-111`) — which of the four run modes the process entered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AppMode {
    Interactive,
    Print,
    Json,
    Rpc,
}

/// `resolveAppMode` (`main.ts:100-111`).
///
/// Note `--mode text` matches **neither** early branch, so it falls through to the TTY
/// logic (§16 hazard 10): `pi --mode text` on a TTY without `-p` is *interactive*. And a
/// non-TTY on **either** stream forces print mode (§16 hazard 11).
pub fn resolve_app_mode(
    args: &crate::args::Args,
    stdin_is_tty: bool,
    stdout_is_tty: bool,
) -> AppMode {
    if args.mode == Some(crate::args::Mode::Rpc) {
        return AppMode::Rpc;
    }
    if args.mode == Some(crate::args::Mode::Json) {
        return AppMode::Json;
    }
    if args.print.unwrap_or(false) || !stdin_is_tty || !stdout_is_tty {
        return AppMode::Print;
    }
    AppMode::Interactive
}

/// `toPrintOutputMode` (`main.ts:113-115`) — `appMode === "json" ? "json" : "text"`.
///
/// Everything that is not `json` maps to `text`, **including `rpc`**; main.ts simply
/// never calls it on the rpc path, which is why the fixture records `printOutputMode:
/// null` for those rows rather than `"text"`.
pub fn to_print_output_mode(app_mode: AppMode) -> PrintOutputMode {
    if app_mode == AppMode::Json {
        PrintOutputMode::Json
    } else {
        PrintOutputMode::Text
    }
}

/// `isPlainRuntimeMetadataCommand` (`main.ts:117-119`) — the **only** exemption from
/// [`OutputGuard::take_over_stdout`].
///
/// §16 hazard 12: `--help`/`--list-models` keep real stdout only while `!parsed.print &&
/// parsed.mode === undefined`, so `pi -p --help` and `pi --mode json --list-models`
/// print their output to **stderr**, and even `pi --mode text --help` loses the
/// exemption because `mode` is *defined*.
pub fn is_plain_runtime_metadata_command(args: &crate::args::Args) -> bool {
    !args.print.unwrap_or(false)
        && args.mode.is_none()
        && (args.help == Some(true) || args.list_models.is_some())
}

/// `shouldTakeOverStdout` (`main.ts:541`).
pub fn should_take_over_stdout(app_mode: AppMode, args: &crate::args::Args) -> bool {
    app_mode != AppMode::Interactive && !is_plain_runtime_metadata_command(args)
}

/// `PrintModeOptions` (`print-mode.ts:17-26`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PrintModeOptions {
    /// `mode` — required in TS; [`Default`] here is [`PrintOutputMode::Text`].
    pub mode: PrintOutputMode,
    /// `messages = []` (`print-mode.ts:33`) — the follow-up prompts.
    pub messages: Vec<String>,
    /// `initialMessage` — `undefined` **and** `""` are both falsy, so neither is
    /// prompted (`print-mode.ts:121`).
    pub initial_message: Option<String>,
    /// `initialImages`, attached to the initial prompt only.
    pub initial_images: Option<Vec<ImageContent>>,
}

/// The process-level seams `runPrintMode` reaches for. See the module docs.
pub struct PrintModeEnv {
    pub guard: Arc<OutputGuard>,
    pub signals: Arc<dyn SignalRegistry>,
    /// `process.platform`, for the SIGHUP gate (`print-mode.ts:49`).
    pub platform: Platform,
}

/// `main.ts:856-857` — `process.exitCode` is assigned **only** when the returned code is
/// non-zero, so a zero-exit run leaves Node's default in place. The fixtures record both
/// numbers (`exitCode` / `processExitCode`).
pub fn process_exit_code(exit_code: i32) -> Option<i32> {
    if exit_code != 0 {
        Some(exit_code)
    } else {
        None
    }
}

/// `Request ${stopReason}`'s interpolation (`print-mode.ts:136`) — the wire string, not
/// the Rust variant name.
fn stop_reason_wire(stop_reason: StopReason) -> &'static str {
    match stop_reason {
        StopReason::Stop => "stop",
        StopReason::Length => "length",
        StopReason::ToolUse => "toolUse",
        StopReason::Error => "error",
        StopReason::Aborted => "aborted",
    }
}

/// Per-run state shared by `disposeRuntime`, `rebindSession` and the signal handlers
/// (`print-mode.ts:34-38`).
struct RunState {
    /// `let session = runtimeHost.session` (`:35`).
    session: Arc<dyn PrintModeSession>,
    /// `let unsubscribe: (() => void) | undefined` (`:36`).
    unsubscribe: Option<Subscription>,
    /// `let disposed = false` (`:37`).
    disposed: bool,
}

struct PrintModeRun {
    host: Arc<dyn AgentSessionRuntimeHost>,
    guard: Arc<OutputGuard>,
    mode: PrintOutputMode,
    state: Mutex<RunState>,
}

impl PrintModeRun {
    fn lock(&self) -> std::sync::MutexGuard<'_, RunState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// `disposeRuntime` (`print-mode.ts:40-45`) — latched, so a signal and the `finally`
    /// cannot dispose twice.
    async fn dispose_runtime(&self) {
        let unsubscribe = {
            let mut state = self.lock();
            if state.disposed {
                return;
            }
            state.disposed = true;
            state.unsubscribe.take()
        };
        if let Some(unsubscribe) = unsubscribe {
            unsubscribe.unsubscribe();
        }
        self.host.dispose().await;
    }

    /// `rebindSession` (`print-mode.ts:71-109`).
    async fn rebind_session(&self) -> Result<(), ThrownValue> {
        // `:72` — re-read the (possibly replaced) session.
        let session = self.host.session();
        self.lock().session = Arc::clone(&session);

        let binding = ExtensionBinding {
            // `:74` — text mode binds as "print".
            mode: match self.mode {
                PrintOutputMode::Json => ExtensionBindMode::Json,
                PrintOutputMode::Text => ExtensionBindMode::Print,
            },
            command_context_actions: self.command_context_actions(&session),
            on_error: self.extension_error_handler(),
        };
        session.bind_extensions(binding).await?;

        // `:103-108` — drop the previous subscription, then install the new one.
        let previous = self.lock().unsubscribe.take();
        if let Some(previous) = previous {
            previous.unsubscribe();
        }

        let guard = Arc::clone(&self.guard);
        let mode = self.mode;
        let subscription = session.subscribe(Arc::new(move |event: &AgentSessionEvent| {
            if mode == PrintOutputMode::Json {
                // `:106` — one compact `JSON.stringify` per event, no switch on `type`.
                match serde_json::to_string(event) {
                    Ok(line) => guard.write_raw_stdout(&format!("{line}\n")),
                    Err(error) => guard.error(&error.to_string()),
                }
            }
        }));
        self.lock().unsubscribe = Some(subscription);
        Ok(())
    }

    /// `commandContextActions` (`print-mode.ts:76-97`).
    fn command_context_actions(
        &self,
        session: &Arc<dyn PrintModeSession>,
    ) -> CommandContextActions {
        build_command_context_actions(&self.host, session)
    }

    /// `onError` (`print-mode.ts:98-100`) — always stderr, via `console.error`.
    fn extension_error_handler(&self) -> ExtensionErrorHandler {
        let guard = Arc::clone(&self.guard);
        Arc::new(move |error: &ExtensionError| {
            guard.error(&format!(
                "Extension error ({}): {}",
                error.extension_path, error.error
            ));
        })
    }

    /// The text-mode tail (`print-mode.ts:129-146`).
    fn write_text_mode_result(&self) -> i32 {
        let state = self.lock().session.state();
        // `:131` — `state.messages[state.messages.length - 1]`, `undefined` when empty.
        let Some(last_message) = state.messages.last() else {
            return 0;
        };
        // `:133` — `lastMessage?.role === "assistant"`; anything else writes nothing.
        let AgentMessage::Llm(Message::Assistant(assistant)) = last_message else {
            return 0;
        };

        if assistant.stop_reason == StopReason::Error
            || assistant.stop_reason == StopReason::Aborted
        {
            // `:136` — `errorMessage || `Request ${stopReason}``. A JS `||`, so an empty
            // string falls back too.
            let message = assistant
                .error_message
                .as_deref()
                .filter(|text| !text.is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| format!("Request {}", stop_reason_wire(assistant.stop_reason)));
            self.guard.error(&message);
            // `:137`
            return 1;
        }

        // `:139-143` — text blocks only, in block order, each with its own `\n`
        // (so a text already ending in `\n` gets a second one).
        for content in &assistant.content {
            if let AssistantContent::Text(text) = content {
                self.guard.write_raw_stdout(&format!("{}\n", text.text));
            }
        }
        0
    }
}

/// `runPrintMode` (`print-mode.ts:32-159`).
///
/// Returns the exit code instead of setting `process.exitCode`; see [`process_exit_code`].
pub async fn run_print_mode(
    runtime_host: Arc<dyn AgentSessionRuntimeHost>,
    options: PrintModeOptions,
    env: PrintModeEnv,
) -> i32 {
    let PrintModeOptions {
        mode,
        messages,
        initial_message,
        initial_images,
    } = options;
    // `:34` — `let exitCode = 0`.
    let mut exit_code = 0;

    let run = Arc::new(PrintModeRun {
        host: Arc::clone(&runtime_host),
        guard: Arc::clone(&env.guard),
        mode,
        state: Mutex::new(RunState {
            // `:35`
            session: runtime_host.session(),
            unsubscribe: None,
            disposed: false,
        }),
    });

    // `:47-65` — registerSignalHandlers(). SIGTERM always; SIGHUP only off win32.
    let mut signal_cleanup_handlers: Vec<(Signal, SignalToken)> = Vec::new();
    for signal in registered_signals(env.platform) {
        let weak: Weak<PrintModeRun> = Arc::downgrade(&run);
        let signals = Arc::clone(&env.signals);
        let handler: SignalHandler = Arc::new(move || {
            let weak = weak.clone();
            let signals = Arc::clone(&signals);
            Box::pin(async move {
                // `:55`
                signals.kill_tracked_detached_children();
                // `:56` — `void disposeRuntime().finally(…)`.
                if let Some(run) = weak.upgrade() {
                    run.dispose_runtime().await;
                }
                // `:57`
                signals.exit(signal.exit_code());
            })
        });
        let token = env.signals.on(signal, handler);
        // `:61` — remembered so the `finally` can `process.off`.
        signal_cleanup_handlers.push((signal, token));
    }

    // `:67-69` — the rebind callback the runtime calls when it swaps sessions. Declared
    // before `rebindSession` in TS (a TDZ that is safe because the body runs later).
    {
        let weak: Weak<PrintModeRun> = Arc::downgrade(&run);
        runtime_host.set_rebind_session(Arc::new(move || {
            let weak = weak.clone();
            Box::pin(async move {
                match weak.upgrade() {
                    Some(run) => run.rebind_session().await,
                    None => Ok(()),
                }
            })
        }));
    }

    // `:111-151` — the try/catch. Every `?`-style early exit lands in the catch arm.
    let outcome: Result<i32, ThrownValue> = async {
        // `:112-117` — the header line, before any event and only when truthy.
        if mode == PrintOutputMode::Json {
            if let Some(header) = run.lock().session.header() {
                let line = serde_json::to_string(&header)
                    .map_err(|error| ThrownValue::Error(error.to_string()))?;
                env.guard.write_raw_stdout(&format!("{line}\n"));
            }
        }

        // `:119`
        run.rebind_session().await?;

        // `:121-123` — `if (initialMessage)`: JS truthiness, so `""` is skipped.
        if let Some(initial_message) = initial_message.as_deref().filter(|text| !text.is_empty()) {
            let session = Arc::clone(&run.lock().session);
            session
                .prompt(
                    initial_message,
                    Some(PromptOptions {
                        images: initial_images.clone(),
                    }),
                )
                .await?;
        }

        // `:125-127` — no truthiness guard here: an empty string IS prompted.
        for message in &messages {
            let session = Arc::clone(&run.lock().session);
            session.prompt(message, None).await?;
        }

        // `:129-146` — the whole block is text-mode only, which is why a failed turn
        // never changes json mode's exit code.
        if mode == PrintOutputMode::Text {
            exit_code = run.write_text_mode_result();
        }

        // `:148`
        Ok(exit_code)
    }
    .await;

    let result = match outcome {
        Ok(code) => code,
        Err(thrown) => {
            // `:149-151`
            env.guard.error(thrown.console_message());
            1
        }
    };

    // `:152-158` — the finally, in order.
    for (signal, token) in signal_cleanup_handlers {
        env.signals.off(signal, token);
    }
    run.dispose_runtime().await;
    env.guard.flush_raw_stdout();

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `Write` over a shared buffer, so a test can read back what landed where.
    #[derive(Clone)]
    struct Shared(Arc<Mutex<Vec<u8>>>);

    impl Write for Shared {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// A guard plus the stdout / stderr buffers behind it.
    type Rig = (OutputGuard, Arc<Mutex<Vec<u8>>>, Arc<Mutex<Vec<u8>>>);

    fn guard() -> Rig {
        let out = Arc::new(Mutex::new(Vec::new()));
        let err = Arc::new(Mutex::new(Vec::new()));
        let guard = OutputGuard::new(
            Box::new(Shared(Arc::clone(&out))),
            Box::new(Shared(Arc::clone(&err))),
        );
        (guard, out, err)
    }

    fn text(buffer: &Arc<Mutex<Vec<u8>>>) -> String {
        String::from_utf8(buffer.lock().unwrap().clone()).unwrap()
    }

    #[test]
    fn takeover_redirects_console_log_but_not_raw_stdout() {
        let (guard, out, err) = guard();
        guard.log("BEFORE");
        guard.take_over_stdout();
        guard.log("AFTER");
        guard.write_raw_stdout("RAW\n");
        guard.error("ERR");
        assert_eq!(text(&out), "BEFORE\nRAW\n");
        assert_eq!(text(&err), "AFTER\nERR\n");
    }

    #[test]
    fn second_takeover_is_a_noop_so_one_restore_restores() {
        let (guard, out, err) = guard();
        guard.take_over_stdout();
        guard.take_over_stdout();
        assert!(guard.is_stdout_taken_over());
        guard.restore_stdout();
        assert!(!guard.is_stdout_taken_over());
        guard.log("BACK");
        assert_eq!(text(&out), "BACK\n");
        assert_eq!(text(&err), "");
    }

    #[test]
    fn empty_raw_write_is_a_noop() {
        let (guard, out, _err) = guard();
        guard.write_raw_stdout("");
        assert_eq!(text(&out), "");
    }

    #[test]
    fn sighup_is_registered_only_off_win32() {
        assert_eq!(registered_signals(Platform::Win32), vec![Signal::Sigterm]);
        assert_eq!(
            registered_signals(Platform::Linux),
            vec![Signal::Sigterm, Signal::Sighup]
        );
        assert_eq!(Signal::Sigterm.exit_code(), 143);
        assert_eq!(Signal::Sighup.exit_code(), 129);
    }

    #[test]
    fn message_update_puts_the_assistant_event_before_the_message() {
        let event = AgentSessionEvent::MessageUpdate {
            assistant_message_event: Value::Null,
            message: Value::Null,
        };
        assert_eq!(
            serde_json::to_string(&event).unwrap(),
            r#"{"type":"message_update","assistantMessageEvent":null,"message":null}"#
        );
    }

    #[test]
    fn process_exit_code_is_set_only_when_non_zero() {
        assert_eq!(process_exit_code(0), None);
        assert_eq!(process_exit_code(1), Some(1));
        assert_eq!(process_exit_code(143), Some(143));
    }
}
