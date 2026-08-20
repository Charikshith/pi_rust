//! Byte oracle for [`pirust_coding_agent::print_mode`] against real Pi.
//!
//! Fixtures (`tests/fixtures/pi/printmode/`, all captured by driving Pi's **real**
//! `runPrintMode` in a child process with separate stdout/stderr pipes — see
//! `events.provenance.json`):
//!
//! | file | rows | what it pins |
//! |---|---|---|
//! | `text_mode.cases.jsonl` | 23 | `--mode text` stdout/stderr/exit for every scenario |
//! | `json_mode.cases.jsonl` | 23 | the same 23 scenarios under `--mode json` |
//! | `output_guard.cases.jsonl` | 12 | 11 step scripts + the 48-row takeover decision table |
//! | `exit_codes.json` | 18 | every terminal outcome, plus the same decision table |
//!
//! Every expectation below is a literal from those files; nothing is re-derived. Record
//! counts are asserted so a shrunken fixture fails loudly, and each test collects **all**
//! failures before panicking, reporting the first differing byte rather than dumping the
//! whole payload.
//!
//! # What the replay supplies, and what it must not
//!
//! `runPrintMode` reads only `sessionManager.getHeader`, `bindExtensions`, `subscribe`,
//! `prompt` and `state` off the session (provenance `seam.phase2_capture`), so the stub
//! here implements exactly those and replays the record's canned `events` /
//! `finalStateMessages`. The *only* test-authored inputs are:
//!
//! - the placeholder substitutions (§ [`resolve`]), chosen to contain no character that
//!   `JSON.stringify` would escape, so substituting into the raw fixture line leaves both
//!   the event objects and the expected stdout consistent;
//! - the thrown values for the two `prompt-throws-*` records ([`THROWN_VALUES`]), which
//!   are the oracle's own stub inputs (`eventSource: "n/a (throw before any event)"`);
//! - the SIGTERM delivery point, which the oracle triggered with `process.emit("SIGTERM")`
//!   from inside `session.prompt()`.
//!
//! All events are emitted during the **first** `prompt` call. Pi's subscription is
//! installed once (before any prompt) and the json branch has no per-turn state, so which
//! prompt emits a given event cannot change a byte of stdout; `emittedEventCount` is
//! asserted as a total.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use futures::future::BoxFuture;
use pirust_agent_core::harness::messages::AgentMessage;
use pirust_agent_core::harness::types::SessionHeader;
use pirust_coding_agent::args::parse_args;
use pirust_coding_agent::config::Platform;
use pirust_coding_agent::print_mode::{
    is_plain_runtime_metadata_command, process_exit_code, registered_signals, resolve_app_mode,
    run_print_mode, should_take_over_stdout, to_print_output_mode, AgentSessionEvent, AppMode,
    Cancelled, ExtensionBindMode, ExtensionBinding, NavigateTreeOptions, OutputGuard, PrintModeEnv,
    PrintModeOptions, PrintModeSession, PrintOutputMode, PromptOptions, RebindSessionFn,
    SessionEventListener, SessionStateView, Signal, SignalHandler, SignalRegistry, SignalToken,
    StdoutWriteArgs, Subscription, ThrownValue, COMMAND_CONTEXT_ACTION_KEYS,
};
use serde_json::Value;

// ============================================================================
// Fixture loading
// ============================================================================

/// Stand-ins for the capture machine's paths / ids. Every one is free of `"`, `\` and
/// control characters, so substituting it into the *raw* JSONL text yields the same bytes
/// as substituting it into a parsed string and re-serializing — which is what makes the
/// events and the expected stdout stay consistent.
const PLACEHOLDERS: [(&str, &str); 7] = [
    ("{TMPROOT}", "/tmp/pi-oracle"),
    ("{PROJECTDIR}", "/tmp/pi-oracle/project"),
    ("{AGENTDIR}", "/tmp/pi-oracle/agent"),
    ("{SESSIONDIR}", "/tmp/pi-oracle/sessions"),
    ("{SESSIONID}", "01234567-89ab-7cde-8f01-23456789abcd"),
    ("{HOME}", "/home/pi-oracle"),
    ("{PIPKG}", "/opt/pi/packages/coding-agent"),
];

fn fixture(name: &str) -> String {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/pi/printmode/")
        .join(name);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read fixture {}: {e}", path.display()))
}

/// Resolve the fixture's placeholders, then reject any that this test does not know
/// about — an unresolved `{PLACEHOLDER}` compared as a literal would quietly "pass".
fn resolve(raw: &str, case: &str) -> String {
    let mut out = raw.to_string();
    for (token, value) in PLACEHOLDERS {
        out = out.replace(token, value);
    }
    if let Some(leftover) = find_placeholder(&out) {
        panic!("{case}: unresolved placeholder {leftover:?} in fixture text");
    }
    out
}

/// The first `{ALL_CAPS}` token in `text`, if any. Hand-rolled because JSON is full of
/// `{` and the crate has no regex dependency.
fn find_placeholder(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'{' {
            let mut j = i + 1;
            while j < bytes.len() && (bytes[j].is_ascii_uppercase() || bytes[j] == b'_') {
                j += 1;
            }
            if j > i + 1 && j < bytes.len() && bytes[j] == b'}' {
                return Some(text[i..=j].to_string());
            }
        }
        i += 1;
    }
    None
}

/// Report the first byte position at which `got` diverges from `want` — house style
/// (`crates/pirust-agent-core/tests/session_golden.rs`).
fn first_diff(want: &str, got: &str) -> String {
    if want == got {
        return "identical".to_string();
    }
    let (wb, gb) = (want.as_bytes(), got.as_bytes());
    let n = wb.len().min(gb.len());
    let mut i = 0;
    while i < n && wb[i] == gb[i] {
        i += 1;
    }
    while !want.is_char_boundary(i) || !got.is_char_boundary(i) {
        i -= 1;
    }
    format!(
        "first diff at byte {i} (want {} bytes, got {} bytes):\n  want …{:?}\n  got  …{:?}",
        wb.len(),
        gb.len(),
        &want[i..want.len().min(i + 60)],
        &got[i..got.len().min(i + 60)],
    )
}

/// Collects failures so one run reports every broken case, not just the first.
#[derive(Default)]
struct Failures(Vec<String>);

impl Failures {
    fn check(&mut self, ok: bool, message: impl FnOnce() -> String) {
        if !ok {
            self.0.push(message());
        }
    }

    fn eq_str(&mut self, case: &str, field: &str, want: &str, got: &str) {
        self.check(want == got, || {
            format!("{case}: {field} differs\n{}", first_diff(want, got))
        });
    }

    fn eq_dbg<T: PartialEq + std::fmt::Debug>(&mut self, case: &str, field: &str, want: T, got: T) {
        self.check(want == got, || {
            format!("{case}: {field}: want {want:?}, got {got:?}")
        });
    }

    fn finish(self, what: &str) {
        if !self.0.is_empty() {
            panic!(
                "{} of {what} failed:\n\n{}\n",
                self.0.len(),
                self.0.join("\n\n")
            );
        }
    }
}

// ============================================================================
// The case record
// ============================================================================

/// One line of `text_mode.cases.jsonl` / `json_mode.cases.jsonl`.
#[derive(Clone)]
struct Case {
    name: String,
    mode: PrintOutputMode,
    header: Option<SessionHeader>,
    initial_message: Option<String>,
    messages: Vec<String>,
    events: Vec<AgentSessionEvent>,
    final_state_messages: Vec<AgentMessage>,
    stdout: String,
    stderr: String,
    exit_code: i32,
    process_exit_code: Option<i32>,
    runtime: Option<Value>,
}

fn load_cases(file: &str, expect_mode: PrintOutputMode) -> Vec<Case> {
    let raw = fixture(file);
    let lines: Vec<&str> = raw.lines().filter(|l| !l.trim().is_empty()).collect();
    lines
        .iter()
        .enumerate()
        .map(|(index, line)| {
            let label = format!("{file}[{index}]");
            let resolved = resolve(line, &label);
            let record: Value =
                serde_json::from_str(&resolved).unwrap_or_else(|e| panic!("{label}: parse: {e}"));
            let name = record["name"].as_str().expect("name").to_string();
            let case = format!("{file} {name}");

            let mode = match record["mode"].as_str().expect("mode") {
                "text" => PrintOutputMode::Text,
                "json" => PrintOutputMode::Json,
                other => panic!("{case}: unexpected mode {other:?}"),
            };
            assert_eq!(mode, expect_mode, "{case}: record is in the wrong file");

            let header = match &record["header"] {
                Value::Null => None,
                value => Some(
                    serde_json::from_value(value.clone())
                        .unwrap_or_else(|e| panic!("{case}: header: {e}")),
                ),
            };
            let initial_message = match &record["initialMessage"] {
                Value::Null => None,
                value => Some(value.as_str().expect("initialMessage").to_string()),
            };
            let messages = record["messages"]
                .as_array()
                .expect("messages")
                .iter()
                .map(|m| m.as_str().expect("message").to_string())
                .collect();
            let events: Vec<AgentSessionEvent> = record["events"]
                .as_array()
                .expect("events")
                .iter()
                .enumerate()
                .map(|(i, event)| {
                    serde_json::from_value(event.clone())
                        .unwrap_or_else(|e| panic!("{case}: events[{i}]: {e}\n  {event}"))
                })
                .collect();
            let final_state_messages: Vec<AgentMessage> = record["finalStateMessages"]
                .as_array()
                .expect("finalStateMessages")
                .iter()
                .enumerate()
                .map(|(i, message)| {
                    serde_json::from_value(message.clone())
                        .unwrap_or_else(|e| panic!("{case}: finalStateMessages[{i}]: {e}"))
                })
                .collect();

            Case {
                name,
                mode,
                header,
                initial_message,
                messages,
                events,
                final_state_messages,
                stdout: record["stdout"].as_str().expect("stdout").to_string(),
                stderr: record["stderr"].as_str().expect("stderr").to_string(),
                exit_code: record["exitCode"].as_i64().expect("exitCode") as i32,
                process_exit_code: record["processExitCode"].as_i64().map(|c| c as i32),
                runtime: match &record["runtime"] {
                    Value::Null => None,
                    value => Some(value.clone()),
                },
            }
        })
        .collect()
}

/// The two records whose `session.prompt()` rejects. The values are the oracle's stub
/// inputs (`String(error)` has already been applied to the non-Error one — there is no JS
/// runtime here to apply it).
const THROWN_VALUES: [(&str, &str, bool); 2] = [
    ("prompt-throws-error", "session is not ready", true),
    ("prompt-throws-non-error", "[object Object]", false),
];

fn thrown_value_for(name: &str) -> Option<ThrownValue> {
    THROWN_VALUES
        .iter()
        .find(|(case, _, _)| *case == name)
        .map(|(_, message, is_error)| {
            if *is_error {
                ThrownValue::Error(message.to_string())
            } else {
                ThrownValue::NonError(message.to_string())
            }
        })
}

// ============================================================================
// Stubs: the session, the runtime host, the signal registry, the sinks
// ============================================================================

/// A `Write` over a shared buffer, so the test can read back what landed where.
#[derive(Clone)]
struct SharedBuffer(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for SharedBuffer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct Sinks {
    stdout: Arc<Mutex<Vec<u8>>>,
    stderr: Arc<Mutex<Vec<u8>>>,
}

impl Sinks {
    fn new() -> (Arc<OutputGuard>, Sinks) {
        let stdout = Arc::new(Mutex::new(Vec::new()));
        let stderr = Arc::new(Mutex::new(Vec::new()));
        let guard = Arc::new(OutputGuard::new(
            Box::new(SharedBuffer(Arc::clone(&stdout))),
            Box::new(SharedBuffer(Arc::clone(&stderr))),
        ));
        (guard, Sinks { stdout, stderr })
    }

    fn stdout(&self) -> String {
        decode(&self.stdout)
    }

    fn stderr(&self) -> String {
        decode(&self.stderr)
    }
}

fn decode(buffer: &Arc<Mutex<Vec<u8>>>) -> String {
    String::from_utf8(buffer.lock().unwrap().clone()).expect("output is utf-8")
}

/// What the stub recorded — the fixture's `runtime` block.
#[derive(Default)]
struct Recorder {
    bind_extensions_calls: Vec<Value>,
    subscribe_count: usize,
    unsubscribe_count: usize,
    prompt_calls: Vec<Value>,
    emitted_event_count: usize,
    shutdown_events: Vec<Value>,
    session_disposed: bool,
    stdout_taken_over_during_run: bool,
    /// Snapshot of `(stdout, stderr)` taken when `process.exit` was called.
    exits: Vec<(i32, String, String)>,
}

type Shared<T> = Arc<Mutex<T>>;

fn shared<T>(value: T) -> Shared<T> {
    Arc::new(Mutex::new(value))
}

/// `process.on` / `process.off` / `process.exit`, recording listener counts so the
/// fixture's `sigterm*/sighup*` deltas can be asserted.
struct RecordingSignals {
    listeners: Shared<Vec<(Signal, SignalToken)>>,
    handlers: Shared<Vec<(SignalToken, SignalHandler)>>,
    next_token: AtomicU64,
    killed_detached_children: Shared<usize>,
    recorder: Shared<Recorder>,
    sinks: Arc<Sinks>,
}

impl RecordingSignals {
    fn new(recorder: Shared<Recorder>, sinks: Arc<Sinks>) -> Self {
        Self {
            listeners: shared(Vec::new()),
            handlers: shared(Vec::new()),
            next_token: AtomicU64::new(1),
            killed_detached_children: shared(0),
            recorder,
            sinks,
        }
    }

    fn listener_count(&self, signal: Signal) -> usize {
        self.listeners
            .lock()
            .unwrap()
            .iter()
            .filter(|(s, _)| *s == signal)
            .count()
    }

    fn handler(&self, signal: Signal) -> Option<SignalHandler> {
        let listeners = self.listeners.lock().unwrap();
        let token = listeners
            .iter()
            .find(|(s, _)| *s == signal)
            .map(|(_, t)| *t)?;
        let handlers = self.handlers.lock().unwrap();
        handlers
            .iter()
            .find(|(t, _)| *t == token)
            .map(|(_, h)| Arc::clone(h))
    }
}

impl SignalRegistry for RecordingSignals {
    fn on(&self, signal: Signal, handler: SignalHandler) -> SignalToken {
        let token = SignalToken(self.next_token.fetch_add(1, Ordering::SeqCst));
        self.listeners.lock().unwrap().push((signal, token));
        self.handlers.lock().unwrap().push((token, handler));
        token
    }

    fn off(&self, signal: Signal, token: SignalToken) {
        self.listeners
            .lock()
            .unwrap()
            .retain(|(s, t)| !(*s == signal && *t == token));
    }

    fn kill_tracked_detached_children(&self) {
        *self.killed_detached_children.lock().unwrap() += 1;
    }

    fn exit(&self, code: i32) {
        self.recorder
            .lock()
            .unwrap()
            .exits
            .push((code, self.sinks.stdout(), self.sinks.stderr()));
    }
}

/// Fired from inside `prompt` — the seam standing in for the oracle's
/// `process.emit("SIGTERM")`.
type PromptSideEffect = Arc<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>;

/// The `AgentSession` stub: exactly the members `runPrintMode` touches.
struct StubSession {
    header: Option<SessionHeader>,
    events: Vec<AgentSessionEvent>,
    state: SessionStateView,
    throws: Option<ThrownValue>,
    listener: Shared<Option<SessionEventListener>>,
    emitted: Shared<bool>,
    recorder: Shared<Recorder>,
    guard: Arc<OutputGuard>,
    /// Fired from inside the first `prompt` — the oracle's `process.emit("SIGTERM")` seam.
    on_prompt: Shared<Option<PromptSideEffect>>,
}

#[async_trait::async_trait]
impl PrintModeSession for StubSession {
    fn header(&self) -> Option<SessionHeader> {
        self.header.clone()
    }

    async fn bind_extensions(&self, binding: ExtensionBinding) -> Result<(), ThrownValue> {
        let mut recorder = self.recorder.lock().unwrap();
        recorder.stdout_taken_over_during_run |= self.guard.is_stdout_taken_over();
        recorder.bind_extensions_calls.push(serde_json::json!({
            "mode": match binding.mode {
                ExtensionBindMode::Print => "print",
                ExtensionBindMode::Json => "json",
                ExtensionBindMode::Tui => "tui",
            },
            "hasCommandContextActions": true,
            "commandContextActionKeys": COMMAND_CONTEXT_ACTION_KEYS,
            "hasOnError": true,
        }));
        // Keep the six closures alive long enough to prove they are constructible; print
        // mode itself never invokes them.
        drop(binding);
        Ok(())
    }

    fn subscribe(&self, listener: SessionEventListener) -> Subscription {
        let mut recorder = self.recorder.lock().unwrap();
        recorder.subscribe_count += 1;
        drop(recorder);
        *self.listener.lock().unwrap() = Some(listener);
        let recorder = Arc::clone(&self.recorder);
        let slot = Arc::clone(&self.listener);
        Subscription::new(move || {
            recorder.lock().unwrap().unsubscribe_count += 1;
            *slot.lock().unwrap() = None;
        })
    }

    async fn prompt(&self, text: &str, options: Option<PromptOptions>) -> Result<(), ThrownValue> {
        {
            let mut recorder = self.recorder.lock().unwrap();
            recorder.stdout_taken_over_during_run |= self.guard.is_stdout_taken_over();
            recorder.prompt_calls.push(serde_json::json!({
                "text": text,
                "options": options.map(|options| serde_json::json!({
                    // `undefined` images serialize as `null` in the oracle's capture.
                    "images": options.images,
                })),
            }));
        }

        let hook = self.on_prompt.lock().unwrap().clone();
        if let Some(hook) = hook {
            hook().await;
        }

        if let Some(thrown) = &self.throws {
            return Err(thrown.clone());
        }

        // Emit the canned stream once, on the first prompt (see the module docs).
        let already = std::mem::replace(&mut *self.emitted.lock().unwrap(), true);
        if !already {
            let listener = self.listener.lock().unwrap().clone();
            if let Some(listener) = listener {
                for event in &self.events {
                    listener(event);
                    self.recorder.lock().unwrap().emitted_event_count += 1;
                }
            }
        }
        Ok(())
    }

    fn state(&self) -> SessionStateView {
        self.state.clone()
    }

    async fn wait_for_idle(&self) {}

    async fn navigate_tree(
        &self,
        _target_id: &str,
        _options: Option<NavigateTreeOptions>,
    ) -> Cancelled {
        Cancelled { cancelled: false }
    }

    async fn reload(&self) {}
}

/// The `AgentSessionRuntime` stub. `dispose()` reproduces
/// `agent-session-runtime.ts:395-402`: emit `session_shutdown{reason:"quit"}`, then
/// dispose the session. What print mode contributes — and what the fixture's
/// `shutdownEvents` / `sessionDisposed` therefore pin — is that this happens **exactly
/// once** per run, however the run ended.
struct StubHost {
    session: Arc<dyn PrintModeSession>,
    recorder: Shared<Recorder>,
    rebind: Shared<Option<RebindSessionFn>>,
}

impl pirust_coding_agent::print_mode::AgentSessionRuntimeHost for StubHost {
    fn session(&self) -> Arc<dyn PrintModeSession> {
        Arc::clone(&self.session)
    }

    fn set_rebind_session(&self, rebind: RebindSessionFn) {
        *self.rebind.lock().unwrap() = Some(rebind);
    }

    fn dispose(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            let mut recorder = self.recorder.lock().unwrap();
            recorder
                .shutdown_events
                .push(serde_json::json!({"type": "session_shutdown", "reason": "quit"}));
            recorder.session_disposed = true;
        })
    }

    fn new_session(&self, options: Value) -> BoxFuture<'_, Value> {
        Box::pin(async move { options })
    }

    fn fork(&self, _entry_id: String, _options: Value) -> BoxFuture<'_, Cancelled> {
        Box::pin(async { Cancelled { cancelled: false } })
    }

    fn switch_session(&self, _path: String, options: Value) -> BoxFuture<'_, Value> {
        Box::pin(async move { options })
    }
}

/// What one replay produced.
struct Replay {
    exit_code: i32,
    stdout: String,
    stderr: String,
    write_failure: Option<String>,
    recorder: Shared<Recorder>,
    signals: Arc<RecordingSignals>,
    sigterm_during_run: usize,
    sighup_during_run: usize,
}

/// Drive `run_print_mode` over one record, mirroring `main.ts:540-544,855`: the child
/// took stdout over before the run and restored it after.
async fn replay(case: &Case, platform: Platform, prompt_hook: PromptHook) -> Replay {
    let (guard, sinks) = Sinks::new();
    let sinks = Arc::new(sinks);
    let recorder = shared(Recorder::default());
    let signals = Arc::new(RecordingSignals::new(
        Arc::clone(&recorder),
        Arc::clone(&sinks),
    ));

    let session = Arc::new(StubSession {
        header: case.header.clone(),
        events: case.events.clone(),
        state: SessionStateView {
            messages: case.final_state_messages.clone(),
        },
        throws: thrown_value_for(&case.name),
        listener: shared(None),
        emitted: shared(false),
        recorder: Arc::clone(&recorder),
        guard: Arc::clone(&guard),
        on_prompt: shared(None),
    });
    let session_dyn: Arc<dyn PrintModeSession> = session.clone();
    let signals_dyn: Arc<dyn SignalRegistry> = signals.clone();
    let host = Arc::new(StubHost {
        session: session_dyn,
        recorder: Arc::clone(&recorder),
        rebind: shared(None),
    });

    guard.take_over_stdout();

    // Observed from inside the run, so the counts are the deltas print mode caused.
    let during = shared((0usize, 0usize));
    if let Some(hook) = prompt_hook {
        let signals_for_hook = Arc::clone(&signals);
        let during = Arc::clone(&during);
        *session.on_prompt.lock().unwrap() = Some(Arc::new(move || {
            *during.lock().unwrap() = (
                signals_for_hook.listener_count(Signal::Sigterm),
                signals_for_hook.listener_count(Signal::Sighup),
            );
            hook(Arc::clone(&signals_for_hook))
        }));
    } else {
        let signals_for_hook = Arc::clone(&signals);
        let during = Arc::clone(&during);
        *session.on_prompt.lock().unwrap() = Some(Arc::new(move || {
            *during.lock().unwrap() = (
                signals_for_hook.listener_count(Signal::Sigterm),
                signals_for_hook.listener_count(Signal::Sighup),
            );
            Box::pin(async {})
        }));
    }

    let exit_code = run_print_mode(
        host,
        PrintModeOptions {
            mode: case.mode,
            messages: case.messages.clone(),
            initial_message: case.initial_message.clone(),
            initial_images: None,
        },
        PrintModeEnv {
            guard: Arc::clone(&guard),
            signals: signals_dyn,
            platform,
        },
    )
    .await;

    guard.restore_stdout();

    let (sigterm_during_run, sighup_during_run) = *during.lock().unwrap();
    Replay {
        exit_code,
        stdout: sinks.stdout(),
        stderr: sinks.stderr(),
        write_failure: guard.write_failure(),
        recorder,
        signals,
        sigterm_during_run,
        sighup_during_run,
    }
}

type PromptHook =
    Option<Arc<dyn Fn(Arc<RecordingSignals>) -> BoxFuture<'static, ()> + Send + Sync>>;

// ============================================================================
// The 46 case records
// ============================================================================

async fn check_cases(file: &str, mode: PrintOutputMode) {
    let cases = load_cases(file, mode);
    assert_eq!(cases.len(), 23, "{file} should carry all 23 records");

    let mut failures = Failures::default();
    for case in &cases {
        let label = format!("{file} {}", case.name);
        let replay = replay(case, Platform::Win32, None).await;

        failures.eq_str(&label, "stdout", &case.stdout, &replay.stdout);
        failures.eq_str(&label, "stderr", &case.stderr, &replay.stderr);
        failures.eq_dbg(&label, "exitCode", case.exit_code, replay.exit_code);
        failures.eq_dbg(
            &label,
            "processExitCode",
            case.process_exit_code,
            Some(process_exit_code(replay.exit_code).unwrap_or(0)),
        );

        // No write may have failed, and the `finally` must have removed every listener.
        failures.eq_dbg(&label, "write_failure", None, replay.write_failure.clone());
        failures.eq_dbg(
            &label,
            "SIGTERM listeners after cleanup",
            0,
            replay.signals.listener_count(Signal::Sigterm),
        );

        if let Some(runtime) = &case.runtime {
            check_runtime(&mut failures, &label, runtime, &replay);
        }
    }
    failures.finish(&format!("{file} records"));
}

/// Compare every key the record's `runtime` block carries (some records carry only a
/// subset), so a missing key is a fixture fact this test simply has nothing to say about
/// — never a silently skipped assertion for a key that IS present.
fn check_runtime(failures: &mut Failures, label: &str, runtime: &Value, replay: &Replay) {
    let recorder = replay.recorder.lock().unwrap();
    let object = runtime.as_object().expect("runtime is an object");
    for (key, want) in object {
        let case = format!("{label} runtime.{key}");
        match key.as_str() {
            "bindExtensionsCalls" => failures.eq_str(
                &case,
                "value",
                &want.to_string(),
                &Value::from(recorder.bind_extensions_calls.clone()).to_string(),
            ),
            "subscribeCount" => failures.eq_dbg(
                &case,
                "value",
                want.as_u64(),
                Some(recorder.subscribe_count as u64),
            ),
            "unsubscribeCount" => failures.eq_dbg(
                &case,
                "value",
                want.as_u64(),
                Some(recorder.unsubscribe_count as u64),
            ),
            "promptCalls" => failures.eq_str(
                &case,
                "value",
                &want.to_string(),
                &Value::from(recorder.prompt_calls.clone()).to_string(),
            ),
            "emittedEventCount" => failures.eq_dbg(
                &case,
                "value",
                want.as_u64(),
                Some(recorder.emitted_event_count as u64),
            ),
            "shutdownEvents" => failures.eq_str(
                &case,
                "value",
                &want.to_string(),
                &Value::from(recorder.shutdown_events.clone()).to_string(),
            ),
            "sessionDisposed" => failures.eq_dbg(
                &case,
                "value",
                want.as_bool(),
                Some(recorder.session_disposed),
            ),
            "stdoutTakenOverDuringRun" => failures.eq_dbg(
                &case,
                "value",
                want.as_bool(),
                Some(recorder.stdout_taken_over_during_run),
            ),
            // The oracle's child already had one listener of each signal before
            // `runPrintMode`, so the pinned fact is the DELTA: after cleanup the counts
            // are back to the baseline. Here the baseline is 0.
            "sigtermListenersBeforeRun" | "sighupListenersBeforeRun" => {
                let after_key = key.replace("BeforeRun", "AfterCleanup");
                failures.eq_dbg(
                    &case,
                    "fixture invariant before == after",
                    want.as_u64(),
                    runtime.get(&after_key).and_then(Value::as_u64),
                );
            }
            "sigtermListenersAfterCleanup" => failures.eq_dbg(
                &case,
                "listeners removed",
                0,
                replay.signals.listener_count(Signal::Sigterm),
            ),
            "sighupListenersAfterCleanup" => failures.eq_dbg(
                &case,
                "listeners removed",
                0,
                replay.signals.listener_count(Signal::Sighup),
            ),
            other => panic!("{case}: unrecognized runtime key {other:?} — teach the test about it"),
        }
    }
}

#[tokio::test]
async fn every_text_mode_record_matches_pi() {
    check_cases("text_mode.cases.jsonl", PrintOutputMode::Text).await;
}

#[tokio::test]
async fn every_json_mode_record_matches_pi() {
    check_cases("json_mode.cases.jsonl", PrintOutputMode::Json).await;
}

/// The one asymmetry that is easiest to "fix" by accident: the same failed turn exits 1
/// in text mode and **0** in json mode, because `if (mode === "text")` gates the whole
/// error block (`print-mode.ts:129`).
#[test]
fn a_failed_turn_exits_1_in_text_mode_and_0_in_json_mode() {
    let text = load_cases("text_mode.cases.jsonl", PrintOutputMode::Text);
    let json = load_cases("json_mode.cases.jsonl", PrintOutputMode::Json);
    assert_eq!(text.len(), json.len());

    let mut asymmetric = 0usize;
    for (text_case, json_case) in text.iter().zip(json.iter()) {
        assert_eq!(text_case.name, json_case.name, "the files must be aligned");
        if text_case.exit_code == 1 && !text_case.name.starts_with("prompt-throws") {
            assert_eq!(
                json_case.exit_code, 0,
                "{}: json mode must not inherit the text-mode failure exit",
                json_case.name
            );
            assert_eq!(
                json_case.stderr, "",
                "{}: json mode is silent",
                json_case.name
            );
            asymmetric += 1;
        }
    }
    assert_eq!(
        asymmetric, 6,
        "6 scenarios exit 1 in text mode and 0 in json mode"
    );

    // …and a THROWN error exits 1 in both.
    for name in ["prompt-throws-error", "prompt-throws-non-error"] {
        let text_case = text.iter().find(|c| c.name == name).expect(name);
        let json_case = json.iter().find(|c| c.name == name).expect(name);
        assert_eq!(text_case.exit_code, 1);
        assert_eq!(json_case.exit_code, 1);
    }
}

// ============================================================================
// output_guard.cases.jsonl
// ============================================================================

/// Interpret one `steps` script against the guard, mirroring the oracle's own harness.
fn run_guard_steps(
    failures: &mut Failures,
    label: &str,
    steps: &[String],
    observations: &[Value],
    guard: &OutputGuard,
) {
    for (index, step) in steps.iter().enumerate() {
        let observation = observations
            .get(index)
            .unwrap_or_else(|| panic!("{label}: no observation for step {index} ({step})"));
        assert_eq!(
            observation["step"].as_str(),
            Some(step.as_str()),
            "{label}: observation {index} is out of step"
        );
        let case = format!("{label} step[{index}] {step}");
        let (kind, payload) = step.split_once(':').unwrap_or((step.as_str(), ""));

        match kind {
            "isTakenOver" => {}
            "takeover" => guard.take_over_stdout(),
            "restore" => guard.restore_stdout(),
            "log" => guard.log(payload),
            "error" => guard.error(payload),
            "raw" => guard.write_raw_stdout(&format!("{payload}\n")),
            "rawEmpty" => guard.write_raw_stdout(""),
            "flush" => guard.flush_raw_stdout(),
            "backpressure" => guard.wait_for_raw_stdout_backpressure(),
            "stdoutWrite" => {
                let returned =
                    guard.process_stdout_write(&format!("{payload}\n"), StdoutWriteArgs::Plain);
                failures.eq_dbg(
                    &case,
                    "returned",
                    observation["returned"].as_bool(),
                    Some(returned),
                );
            }
            "stdoutWriteCb2" => {
                let mut invoked = false;
                let mut callback = |_error: Option<&std::io::Error>| invoked = true;
                guard.process_stdout_write(
                    &format!("{payload}\n"),
                    StdoutWriteArgs::Callback(&mut callback),
                );
                failures.eq_dbg(
                    &case,
                    "callbackInvoked",
                    observation["callbackInvoked"].as_bool(),
                    Some(invoked),
                );
            }
            "stdoutWriteCb3" => {
                let mut invoked = false;
                let mut callback = |_error: Option<&std::io::Error>| invoked = true;
                guard.process_stdout_write(
                    &format!("{payload}\n"),
                    StdoutWriteArgs::EncodingAndCallback("utf8", &mut callback),
                );
                failures.eq_dbg(
                    &case,
                    "callbackInvoked",
                    observation["callbackInvoked"].as_bool(),
                    Some(invoked),
                );
            }
            "stderrWrite" => {
                let returned = guard.process_stderr_write(&format!("{payload}\n"));
                failures.eq_dbg(
                    &case,
                    "returned",
                    observation["returned"].as_bool(),
                    Some(returned),
                );
            }
            other => panic!("{case}: unrecognized step kind {other:?}"),
        }

        if let Some(want) = observation["isStdoutTakenOver"].as_bool() {
            failures.eq_dbg(
                &case,
                "isStdoutTakenOver",
                want,
                guard.is_stdout_taken_over(),
            );
        }
    }
}

#[test]
fn every_output_guard_script_lands_on_the_stream_pi_recorded() {
    let raw = fixture("output_guard.cases.jsonl");
    let lines: Vec<&str> = raw.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len(), 12, "fixture should carry all 12 records");

    let mut failures = Failures::default();
    let mut scripts = 0usize;
    let mut decision_tables = 0usize;

    for (index, line) in lines.iter().enumerate() {
        let record: Value = serde_json::from_str(&resolve(line, &format!("guard[{index}]")))
            .unwrap_or_else(|e| panic!("guard[{index}]: parse: {e}"));
        let name = record["name"].as_str().expect("name").to_string();

        let Some(steps) = record["steps"].as_array() else {
            // Record 12 has no script: it is the 48-row takeover decision table.
            check_decision_table(
                &mut failures,
                &name,
                record["decisionTable"].as_array().expect("decisionTable"),
            );
            decision_tables += 1;
            continue;
        };
        scripts += 1;

        let steps: Vec<String> = steps
            .iter()
            .map(|s| s.as_str().expect("step").to_string())
            .collect();
        let observations = record["observations"].as_array().expect("observations");

        let stdout = Arc::new(Mutex::new(Vec::new()));
        let stderr = Arc::new(Mutex::new(Vec::new()));
        let guard = OutputGuard::new(
            Box::new(SharedBuffer(Arc::clone(&stdout))),
            Box::new(SharedBuffer(Arc::clone(&stderr))),
        );
        run_guard_steps(&mut failures, &name, &steps, observations, &guard);

        let got_stdout = decode(&stdout);
        let got_stderr = decode(&stderr);
        failures.eq_str(
            &name,
            "stdout",
            record["stdout"].as_str().expect("stdout"),
            &got_stdout,
        );
        failures.eq_str(
            &name,
            "stderr",
            record["stderr"].as_str().expect("stderr"),
            &got_stderr,
        );

        // The `landedOn` map — the whole point of the guard port.
        let landed_on = record["landedOn"].as_object().expect("landedOn");
        assert!(!landed_on.is_empty(), "{name}: landedOn must not be empty");
        for (marker, want) in landed_on {
            let case = format!("{name} landedOn[{marker}]");
            let in_stdout = got_stdout.contains(marker.as_str());
            let in_stderr = got_stderr.contains(marker.as_str());
            let got = match (in_stdout, in_stderr) {
                (true, false) => "stdout",
                (false, true) => "stderr",
                (true, true) => "BOTH",
                (false, false) => "NEITHER",
            };
            failures.eq_dbg(&case, "stream", want.as_str(), Some(got));
        }
        // …and the two marker lists must agree with it.
        for (key, stream) in [("stdoutMarkers", "stdout"), ("stderrMarkers", "stderr")] {
            for marker in record[key].as_array().expect(key) {
                let marker = marker.as_str().expect("marker");
                failures.eq_dbg(
                    &format!("{name} {key}[{marker}]"),
                    "landedOn agrees",
                    Some(stream),
                    landed_on.get(marker).and_then(Value::as_str),
                );
            }
        }
    }

    assert_eq!(scripts, 11, "11 of the 12 records are step scripts");
    assert_eq!(
        decision_tables, 1,
        "1 of the 12 records is the decision table"
    );
    failures.finish("output-guard records");
}

/// The 48-row `shouldTakeOverStdout` table (`main.ts:540-544` + `:117-119`).
fn check_decision_table(failures: &mut Failures, label: &str, rows: &[Value]) {
    assert_eq!(
        rows.len(),
        48,
        "{label}: the table should carry all 48 rows"
    );
    for (index, row) in rows.iter().enumerate() {
        let argv: Vec<String> = row["argv"]
            .as_array()
            .expect("argv")
            .iter()
            .map(|a| a.as_str().expect("arg").to_string())
            .collect();
        let case = format!(
            "{label}[{index}] {argv:?} stdin={} stdout={}",
            row["stdinIsTTY"], row["stdoutIsTTY"]
        );
        let parsed = parse_args(&argv);
        let stdin_is_tty = row["stdinIsTTY"].as_bool().expect("stdinIsTTY");
        let stdout_is_tty = row["stdoutIsTTY"].as_bool().expect("stdoutIsTTY");

        let app_mode = resolve_app_mode(&parsed, stdin_is_tty, stdout_is_tty);
        let app_mode_wire = match app_mode {
            AppMode::Interactive => "interactive",
            AppMode::Print => "print",
            AppMode::Json => "json",
            AppMode::Rpc => "rpc",
        };
        failures.eq_dbg(
            &case,
            "appMode",
            row["appMode"].as_str(),
            Some(app_mode_wire),
        );
        failures.eq_dbg(
            &case,
            "isPlainRuntimeMetadataCommand",
            row["isPlainRuntimeMetadataCommand"].as_bool(),
            Some(is_plain_runtime_metadata_command(&parsed)),
        );
        failures.eq_dbg(
            &case,
            "shouldTakeOverStdout",
            row["shouldTakeOverStdout"].as_bool(),
            Some(should_take_over_stdout(app_mode, &parsed)),
        );
        // `printOutputMode: null` marks the rpc rows, where main.ts never calls
        // `toPrintOutputMode` (it would return "text").
        match row["printOutputMode"].as_str() {
            None => failures.eq_dbg(&case, "printOutputMode null ⇒ rpc", AppMode::Rpc, app_mode),
            Some(want) => failures.eq_dbg(
                &case,
                "printOutputMode",
                want,
                match to_print_output_mode(app_mode) {
                    PrintOutputMode::Text => "text",
                    PrintOutputMode::Json => "json",
                },
            ),
        }
    }
}

// ============================================================================
// exit_codes.json
// ============================================================================

#[tokio::test]
async fn every_exit_code_row_matches_pi() {
    let doc: Value = serde_json::from_str(&resolve(&fixture("exit_codes.json"), "exit_codes.json"))
        .expect("parse exit_codes.json");
    assert_eq!(doc["platform"], "win32");
    let rows = doc["rows"].as_array().expect("rows");
    assert_eq!(rows.len(), 18, "fixture should carry all 18 rows");

    // Every outcome print mode itself can produce, keyed by what the process observed.
    let mut produced: Vec<(PrintOutputMode, String, String, i32, Option<i32>)> = Vec::new();
    for (file, mode) in [
        ("text_mode.cases.jsonl", PrintOutputMode::Text),
        ("json_mode.cases.jsonl", PrintOutputMode::Json),
    ] {
        for case in load_cases(file, mode) {
            let replay = replay(&case, Platform::Win32, None).await;
            produced.push((
                mode,
                replay.stdout,
                replay.stderr,
                replay.exit_code,
                Some(process_exit_code(replay.exit_code).unwrap_or(0)),
            ));
        }
    }

    let mut failures = Failures::default();
    let mut replayed = 0usize;
    let mut out_of_scope = 0usize;
    let mut signal_rows = 0usize;

    for row in rows {
        let outcome = row["outcome"].as_str().expect("outcome").to_string();
        let source = row["source"].as_str().expect("source");

        // The SIGTERM row: `exitCode` is null because `runPrintMode` never returned.
        if row["exitCode"].is_null() {
            check_sigterm_row(&mut failures, &outcome, row).await;
            signal_rows += 1;
            continue;
        }

        // `main.ts:800-803` exits before `runPrintMode` is ever reached.
        if source.contains("main.ts:800-803") {
            failures.eq_dbg(
                &outcome,
                "exitCode",
                Some(1),
                row["exitCode"].as_i64().map(|c| c as i32),
            );
            failures.eq_str(
                &outcome,
                "stderr is console.error(message + \"\\n\")",
                &format!(
                    "{}\n",
                    doc["messages"]["noModelsAvailable"]
                        .as_str()
                        .expect("message")
                ),
                row["stderr"].as_str().expect("stderr"),
            );
            failures.eq_dbg(&outcome, "mode", Some("text|json"), row["mode"].as_str());
            out_of_scope += 1;
            continue;
        }

        let mode = match row["mode"].as_str().expect("mode") {
            "text" => PrintOutputMode::Text,
            "json" => PrintOutputMode::Json,
            other => panic!("{outcome}: unexpected mode {other:?}"),
        };
        let want = (
            mode,
            row["stdout"].as_str().expect("stdout").to_string(),
            row["stderr"].as_str().expect("stderr").to_string(),
            row["exitCode"].as_i64().expect("exitCode") as i32,
            row["processExitCode"].as_i64().map(|c| c as i32),
        );
        failures.check(produced.contains(&want), || {
            let near = produced
                .iter()
                .filter(|p| p.0 == want.0 && p.3 == want.3)
                .map(|p| format!("    stderr={:?} stdout[..40]={:?}", p.2, &p.1[..p.1.len().min(40)]))
                .collect::<Vec<_>>()
                .join("\n");
            format!(
                "{outcome}: no replayed case produced this outcome\n  want exit={} stderr={:?} stdout={} bytes\n  same-mode/same-exit candidates:\n{near}",
                want.3, want.2, want.1.len()
            )
        });
        replayed += 1;
    }

    assert_eq!(replayed, 16, "16 rows are reproducible by run_print_mode");
    assert_eq!(signal_rows, 1, "1 row is the SIGTERM handler");
    assert_eq!(
        out_of_scope, 1,
        "1 row exits before run_print_mode (main.ts:800-803)"
    );

    // The table is duplicated in this file; assert both copies.
    check_decision_table(
        &mut failures,
        "exit_codes.appModeAndStdoutTakeover",
        doc["appModeAndStdoutTakeover"]["rows"]
            .as_array()
            .expect("rows"),
    );
    failures.finish("exit_codes rows");
}

/// The SIGTERM row: delivered from inside `session.prompt()`, exactly as the oracle's
/// `process.emit("SIGTERM")` was.
async fn check_sigterm_row(failures: &mut Failures, label: &str, row: &Value) {
    let listeners = &row["signalListeners"];
    assert_eq!(
        row["platform"], "win32",
        "{label}: the row is a win32 capture"
    );

    // A record with no prompts would never reach the delivery point, so drive the
    // simplest scenario that does — and give it an empty state so the text branch writes
    // nothing, matching the row's empty stdout/stderr.
    let cases = load_cases("text_mode.cases.jsonl", PrintOutputMode::Text);
    let mut case = cases
        .iter()
        .find(|c| c.name == "empty-state-messages")
        .expect("empty-state-messages")
        .clone();
    case.name = "sigterm-during-the-run".to_string();
    case.events = Vec::new();
    case.runtime = None;

    let hook: PromptHook = Some(Arc::new(|signals: Arc<RecordingSignals>| {
        Box::pin(async move {
            let handler = signals
                .handler(Signal::Sigterm)
                .expect("print mode installed a SIGTERM listener");
            handler().await;
        })
    }));
    let replay = replay(&case, Platform::Win32, hook).await;

    // `sigtermAddedByPrintMode: 1` / `sighupAddedByPrintMode: 0` — the win32 gate.
    failures.eq_dbg(
        label,
        "sigtermAddedByPrintMode",
        listeners["sigtermAddedByPrintMode"].as_u64(),
        Some(replay.sigterm_during_run as u64),
    );
    failures.eq_dbg(
        label,
        "sighupAddedByPrintMode",
        listeners["sighupAddedByPrintMode"].as_u64(),
        Some(replay.sighup_during_run as u64),
    );
    // The fixture's own before/during deltas must say the same thing.
    failures.eq_dbg(
        label,
        "sigtermDuringRun - sigtermBeforeRunPrintMode",
        listeners["sigtermAddedByPrintMode"].as_u64(),
        Some(
            listeners["sigtermDuringRun"].as_u64().expect("during")
                - listeners["sigtermBeforeRunPrintMode"]
                    .as_u64()
                    .expect("before"),
        ),
    );
    failures.eq_dbg(
        label,
        "sighupDuringRun - sighupBeforeRunPrintMode",
        listeners["sighupAddedByPrintMode"].as_u64(),
        Some(
            listeners["sighupDuringRun"].as_u64().expect("during")
                - listeners["sighupBeforeRunPrintMode"]
                    .as_u64()
                    .expect("before"),
        ),
    );

    // Read everything out under one lock: `std::sync::Mutex` is not reentrant.
    let (exits, session_disposed, shutdown_events) = {
        let recorder = replay.recorder.lock().unwrap();
        (
            recorder.exits.clone(),
            recorder.session_disposed,
            recorder.shutdown_events.len(),
        )
    };

    // `processExitCode: 143`, and the streams were still empty at that moment.
    failures.eq_dbg(label, "process.exit call count", 1, exits.len());
    if let Some((code, stdout_at_exit, stderr_at_exit)) = exits.first() {
        failures.eq_dbg(
            label,
            "processExitCode",
            row["processExitCode"].as_i64().map(|c| c as i32),
            Some(*code),
        );
        failures.eq_str(
            label,
            "stdout at exit",
            row["stdout"].as_str().unwrap_or(""),
            stdout_at_exit,
        );
        failures.eq_str(
            label,
            "stderr at exit",
            row["stderr"].as_str().unwrap_or(""),
            stderr_at_exit,
        );
    }
    // The handler ran `killTrackedDetachedChildren()` and disposed the runtime…
    failures.eq_dbg(
        label,
        "killTrackedDetachedChildren",
        1,
        *replay.signals.killed_detached_children.lock().unwrap(),
    );
    failures.eq_dbg(label, "sessionDisposed", true, session_disposed);
    // …exactly once: the `finally`'s own dispose is latched out by `disposed`.
    failures.eq_dbg(label, "shutdownEvents", 1, shutdown_events);
}

// ============================================================================
// Structural facts the byte comparison depends on
// ============================================================================

/// json mode is newline-delimited, not a document: no wrapper array, no indentation, and
/// every non-empty line is itself a complete compact JSON object.
#[test]
fn json_mode_is_one_compact_object_per_line() {
    let cases = load_cases("json_mode.cases.jsonl", PrintOutputMode::Json);
    let mut lines_seen = 0usize;
    for case in &cases {
        assert!(
            !case.stdout.starts_with('[') && !case.stdout.contains("\n  "),
            "{}: json mode must not be a document or indented",
            case.name
        );
        let expected_lines = case.events.len() + usize::from(case.header.is_some());
        let lines: Vec<&str> = case.stdout.lines().collect();
        assert_eq!(
            lines.len(),
            expected_lines,
            "{}: one line per event, header first",
            case.name
        );
        for (index, line) in lines.iter().enumerate() {
            let value: Value = serde_json::from_str(line)
                .unwrap_or_else(|e| panic!("{} line {index}: not compact JSON: {e}", case.name));
            assert_eq!(
                serde_json::to_string(&value).unwrap(),
                *line,
                "{} line {index}: not the compact form",
                case.name
            );
            lines_seen += 1;
        }
        if let (Some(header), Some(first)) = (case.header.as_ref(), lines.first()) {
            assert_eq!(
                *first,
                serde_json::to_string(header).unwrap(),
                "{}: the header line comes first",
                case.name
            );
        }
    }
    // 258 events + 22 headers (only `no-session-header` has none).
    assert_eq!(
        lines_seen, 280,
        "the json corpus is 258 event lines + 22 header lines"
    );
}

/// Every event in the corpus round-trips byte-identically through
/// [`AgentSessionEvent`] — the guarantee json mode's stdout rests on.
#[test]
fn every_event_roundtrips_byte_identical() {
    let mut failures = Failures::default();
    let mut seen = 0usize;
    let mut kinds = std::collections::BTreeSet::new();
    for file in ["text_mode.cases.jsonl", "json_mode.cases.jsonl"] {
        for (index, line) in fixture(file)
            .lines()
            .filter(|l| !l.trim().is_empty())
            .enumerate()
        {
            let record: Value =
                serde_json::from_str(&resolve(line, &format!("{file}[{index}]"))).unwrap();
            for (i, raw) in record["events"].as_array().unwrap().iter().enumerate() {
                let label = format!("{file}[{index}] events[{i}]");
                kinds.insert(raw["type"].as_str().unwrap().to_string());
                let want = serde_json::to_string(raw).unwrap();
                let event: AgentSessionEvent = serde_json::from_value(raw.clone())
                    .unwrap_or_else(|e| panic!("{label}: deserialize: {e}\n  {want}"));
                let got = serde_json::to_string(&event).unwrap();
                failures.eq_str(&label, "re-serialized", &want, &got);
                seen += 1;
            }
        }
    }
    assert_eq!(seen, 516, "258 events in each of the two files");
    // The 12 variants the fixture actually exercises (provenance `notCaptured` lists the
    // rest).
    assert_eq!(
        kinds.iter().map(String::as_str).collect::<Vec<_>>(),
        [
            "agent_end",
            "agent_settled",
            "agent_start",
            "auto_retry_end",
            "auto_retry_start",
            "message_end",
            "message_start",
            "message_update",
            "tool_execution_end",
            "tool_execution_start",
            "turn_end",
            "turn_start",
        ],
        "the captured event set changed"
    );
    failures.finish("event round-trips");
}

/// `auto_retry_start` / `auto_retry_end` are real (a retryable `503`), and text mode
/// reports the **second** error, not the first.
#[test]
fn the_retry_scenarios_report_the_final_error() {
    let cases = load_cases("text_mode.cases.jsonl", PrintOutputMode::Text);
    let exhausted = cases
        .iter()
        .find(|c| c.name == "provider-error-retried-then-exhausted")
        .expect("provider-error-retried-then-exhausted");
    assert!(
        exhausted
            .events
            .iter()
            .any(|e| matches!(e, AgentSessionEvent::AutoRetryStart { .. }))
            && exhausted
                .events
                .iter()
                .any(|e| matches!(e, AgentSessionEvent::AutoRetryEnd { .. })),
        "the scenario must carry a real auto_retry pair"
    );
    // The FIRST error was `provider exploded: 503 upstream`; the reported one is the
    // second.
    assert_eq!(exhausted.stderr, "No more faux responses queued\n");
    assert!(
        exhausted.events.iter().any(|e| matches!(
            e,
            AgentSessionEvent::AutoRetryStart { error_message, .. }
                if error_message == "provider exploded: 503 upstream"
        )),
        "the first (retried) error must still be in the stream"
    );

    let recovered = cases
        .iter()
        .find(|c| c.name == "provider-error-retried-then-succeeds")
        .expect("provider-error-retried-then-succeeds");
    assert_eq!(recovered.exit_code, 0, "a recovered retry exits 0");
    assert_eq!(recovered.stdout, "Recovered.\n");
    assert_eq!(recovered.stderr, "");
}

/// Print mode emits no ANSI at all (`print-mode.ts` imports no chalk).
#[test]
fn no_fixture_output_contains_an_escape_sequence() {
    for file in ["text_mode.cases.jsonl", "json_mode.cases.jsonl"] {
        for case in load_cases(
            file,
            if file.starts_with("text") {
                PrintOutputMode::Text
            } else {
                PrintOutputMode::Json
            },
        ) {
            assert!(
                !case.stdout.contains('\u{1b}') && !case.stderr.contains('\u{1b}'),
                "{file} {}: print mode must emit no ANSI",
                case.name
            );
        }
    }
}

/// The win32 SIGHUP gate, independent of any fixture replay.
#[test]
fn registered_signals_follow_the_platform_gate() {
    assert_eq!(registered_signals(Platform::Win32), vec![Signal::Sigterm]);
    for platform in [Platform::Linux, Platform::Darwin] {
        assert_eq!(
            registered_signals(platform),
            vec![Signal::Sigterm, Signal::Sighup],
            "{platform:?} registers SIGHUP too"
        );
    }
    assert_eq!(Signal::Sigterm.exit_code(), 143);
    assert_eq!(Signal::Sighup.exit_code(), 129);
    assert_eq!(Signal::Sigterm.as_str(), "SIGTERM");
    assert_eq!(Signal::Sighup.as_str(), "SIGHUP");
}
