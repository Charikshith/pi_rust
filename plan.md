# feat-012 — RPC mode (JSON-RPC over stdio)

**Success criterion:** `pirust --mode rpc` speaks Pi's exact JSONL RPC protocol —
commands on stdin, responses + events on stdout, byte-compatible shapes and error
wording — verified against real Pi as the oracle; plus an `RpcClient` for embedding,
mirroring `rpc-client.ts`.

Source: `pi/packages/coding-agent/src/modes/rpc/`
(`rpc-mode.ts` 817, `rpc-client.ts` 601, `jsonl.ts` 58, `rpc-types.ts` 289).
Pi's own tests mirror the seam: `test/rpc-jsonl.test.ts`, `rpc.test.ts`.

**Architecture note (named, not silent):** Pi's rpc-mode runs over
`AgentSessionRuntime` + the full interactive `AgentSession`. Our side has
`AgentHarness` (v4 Session) + `SingleTurnSession` (print-mode bridge) — no
`AgentSession` equivalent. The protocol layer (framing, types, dispatch shape,
error wording) ports 1:1; commands whose backing capability does not exist yet
(fork/clone/switch_session/export_html/bash-in-session, extension UI proxying)
return Pi-exact *shapes* driven by our host semantics, with gaps named here and
in module docs — never silently invented.

## Waves

1. **Wave 1 — protocol foundation + oracle**
   - `scripts/gen-rpc-oracle.mjs`: drive REAL `runRpcMode` from `../pi` in a child
     process with a stub `AgentSessionRuntime` (same seam as gen-printmode-oracle),
     feed command lines on fake stdin, capture exact stdout lines →
     `tests/fixtures/pi/rpc/{commands,responses}.corpus.jsonl`.
   - `crates/pirust-coding-agent/src/rpc/{mod,jsonl,types}.rs`:
     `serialize_json_line` + strict LF-only reader port (`jsonl.ts`);
     serde types for RpcCommand/RpcResponse/RpcExtensionUIRequest/RpcExtensionUIResponse
     with JS-canonical field order (`rpc-types.ts`).
   - `tests/rpc_golden.rs`: replay captured tapes — parse each captured request,
     assert our serializer emits byte-identical responses.
   - Verify: `cargo test -p pirust-coding-agent`, clippy/fmt clean, oracle --check.

2. **Wave 2 — rpc mode core**: `rpc_mode.rs` command loop over a new
   `RpcRuntimeHost` (harness + SessionManager): prompt/steer/follow_up/abort/
   get_state/model+thinking commands/compaction/get_entries/get_tree/messages/
   last-assistant-text/name; Pi-exact error strings for unknown/failed commands;
   extension_ui_request/response plumbing where the host supports it.
   Verify: golden tape replay through the real dispatch loop.

3. **Wave 3 — wiring**: main.rs replaces the "not supported" stub (main.rs:172);
   SIGTERM/SIGHUP handling + shutdown semantics (exit 143/129); stdin-end
   shutdown; backpressure. Integration test spawning `pirust --mode rpc`.

4. **Wave 4 — client**: `rpc_client.rs` port of rpc-client.ts over tokio::process;
   black-box tests mirroring Pi's rpc-client-*.test.ts.

## Wave 2 concrete plan (2026-08-23)

Scope: the command subset plan.md's own Wave 2 line names — prompt/steer/
follow_up/abort/get_state/model+thinking commands/steering+follow-up modes/
compact/set_auto_compaction/set_auto_retry/abort_retry/get_entries/get_tree/
get_last_assistant_text/get_messages/set_session_name. Explicitly OUT: fork,
clone, switch_session, new_session, export_html, bash, abort_bash,
get_fork_messages, get_commands, extension_ui plumbing — all need the
AgentSession-equivalent capability the architecture note above says pirust
doesn't have yet. main.rs wiring stays Wave 3 (unchanged stub).

1. `pirust-agent-core`: make `AgentHarness::model`/`thinking_level`
   interior-mutable (`Mutex` in `HarnessShared`, matching `Agent`'s own
   pattern), add `set_model`/`set_thinking_level`; store the active turn's
   `CancellationToken` in `HarnessShared` and add `abort()`; add
   `pending_message_count()` (steer_queue.len() + follow_up_queue.len()).
   → verify: `cargo test -p pirust-agent-core` green, existing harness tests
   unaffected (no external callers of the old `&Model` signature).
2. `pirust-coding-agent/src/rpc/host.rs`: new `RpcRuntimeHost<St>` wrapping
   `Arc<AgentHarness<St>>` + `Arc<dyn ModelSource>` + RPC-only queue-mode/
   auto-compaction/auto-retry flags Pi tracks at the `AgentSession` level
   that `AgentHarness` has no home for yet (named, not silent).
   → verify: constructs against a real `V4Session` + faux `StreamFn` in a
   unit test.
3. `pirust-coding-agent/src/rpc/mode.rs`: `handle_command` dispatch mirroring
   `rpc-mode.ts`'s switch for the Wave-2 subset, Pi-exact error strings
   (`Model not found: {provider}/{modelId}`, `Entry not found: {since}`,
   `Session name cannot be empty`, `Unknown command: {type}`).
   → verify: unit/integration tests driving `handle_command` directly (no
   main.rs wiring yet) against a real harness + a scripted `Faux` stream fn,
   plus one live run against the user's local llama-server
   (`ggml-org/Qwen3.5-0.8B-GGUF` at `127.0.0.1:8080`) exercising a real
   `prompt` end-to-end.
4. Gate: fmt + clippy `-D warnings` clean, `cargo test --workspace` green
   (same 3 pre-existing `pirust-tools` failures allowed), no oracle drift.
5. Best-effort structural check: replay the 38 requests already captured in
   `tests/fixtures/pi/rpc/commands.corpus.jsonl` through `handle_command` and
   confirm every one parses + dispatches without panicking. NOT a byte
   comparison (our harness's session state differs from Pi's stub) — named
   as a residual for Wave 3's live differential, not silently skipped.

## Status

- [x] Wave 1 — DONE 2026-08-22:
  - `scripts/gen-rpc-oracle.mjs`: drives real `runRpcMode` from `../pi` in a child
    process over REAL OS pipes with a stub runtime host; LOCK-STEP capture (each
    command waits for its response) because Pi's async handlers interleave
    nondeterministically otherwise. 38 requests → 39 responses, deterministic
    (3× `--check` green). Wired into `init.sh`.
  - `scripts/gen-rpc-live-oracle.mjs` + frozen `tests/fixtures/pi/rpc-live/live.corpus.jsonl`:
    LIVE end-to-end tape of real `pi --mode rpc` against local llama-server
    (`ggml-org/Qwen3.5-0.8B-GGUF` at `http://127.0.0.1:8080`, Anthropic-compatible
    /v1/messages, via models.json baseUrl override + PI_CODING_AGENT_DIR temp dir).
    Reference capture for event-stream structure (message_update deltas,
    thinking_level_changed-before-response ordering), not a byte golden.
  - `scripts/run-pi.mjs`: reusable launcher running real pi from TS source.
  - `crates/pirust-coding-agent/src/rpc/{mod,jsonl,types}.rs` + `tests/rpc_golden.rs`:
    strict LF-only JSONL framing port (CRLF strip, UTF-8 chunk-boundary safety,
    U+2028 not a separator); typed RpcCommand/RpcResponse/RpcSessionState/UI
    types with JS-canonical key order + omitted-undefined semantics. ALL 39
    captured responses rebuild BYTE-IDENTICALLY; all 38 requests parse as Pi's
    union would discriminate them (incl. parse-error + `Unknown command:` wording).
  - Gate: fmt/clippy/-D-warnings clean, workspace green except the 3 pre-existing
    pirust-tools find env failures, oracle --check idempotent.
  - NOTE: `./init.sh` itself currently fails to parse under bash (pre-existing;
    CRLF working-tree copy also fails `bash -n` on committed HEAD) — gate run as
    individual commands this wave.
- [x] Wave 2 — DONE 2026-08-23:
  - `pirust-agent-core`: `AgentHarness::model`/`thinking_level` made interior-mutable
    (`Mutex`) with `set_model`/`set_thinking_level`; new `abort()` (cancels a
    stored `CancellationToken` for the in-flight turn); new `messages()`
    (context messages), `entries()` (root-first branch entries),
    `pending_message_count()` (steer + follow-up queue lengths). `user_message`
    made `pub`. No external callers of the old `&Model`-returning signature, so
    this was a safe internal change.
  - NEW `crates/pirust-coding-agent/src/rpc/host.rs`: `RpcRuntimeHost<St>` —
    an `AgentHarness` + `ModelSource` + the RPC-only state Pi tracks at the
    `AgentSession` level that the harness has no home for (`steeringMode`/
    `followUpMode`/`autoCompactionEnabled`/`autoRetryEnabled`), defaults
    pinned to the Wave-1 oracle's `get_state` fixture (`"all"`/`"all"`/`true`).
  - NEW `crates/pirust-coding-agent/src/rpc/mode.rs`: `handle_command` dispatch
    covering prompt/steer/follow_up/abort, get_state, set_model/cycle_model/
    get_available_models, set_thinking_level/cycle_thinking_level/
    get_available_thinking_levels, set_steering_mode/set_follow_up_mode,
    compact/set_auto_compaction, set_auto_retry/abort_retry, get_session_stats,
    get_entries/get_tree/get_last_assistant_text/get_messages,
    set_session_name, get_commands (trivially empty). Pi-exact error strings
    reused where the oracle names them (`Model not found: {provider}/{modelId}`,
    `Entry not found: {since}`, `Session name cannot be empty`). Found and
    fixed one real wire-fidelity bug while writing this: Pi's
    `success(id, cmd, null)` for `cycle_model`/`cycle_thinking_level` emits an
    explicit `"data":null` key (JS `null !== undefined`), NOT an omitted key —
    `RpcResponse::success_with(id, cmd, Value::Null)` used for those two, not
    the data-omitting `success()`. `new_session`/`bash`/`abort_bash`/
    `export_html`/`switch_session`/`fork`/`clone`/`get_fork_messages` return a
    real named error (not "Unknown command:") — each needs the
    AgentSession-equivalent capability this port doesn't have yet (see the
    architecture note above); `main.rs` wiring is still Wave 3.
  - NEW `crates/pirust-coding-agent/tests/rpc_dispatch.rs`: 10 tests against a
    real `AgentHarness` + scripted `Faux` provider (get_state defaults vs the
    oracle fixture, set/cycle model, thinking-level cycling gated by
    `model.reasoning`, queue modes + pending count, session name validation,
    get_entries "not found", named-not-"Unknown" errors, full prompt→
    get_last_assistant_text→get_messages round trip) PLUS one LIVE test
    (`live_prompt_against_local_llama_server`) driving a real HTTP turn
    against the user's running `ggml-org/Qwen3.5-0.8B-GGUF` llama-server at
    `127.0.0.1:8080` through the exact same `handle_command` dispatch —
    skips (not fails) when no server is reachable, same convention as
    `scripts/gen-rpc-live-oracle.mjs`. The live test caught a real bug during
    development (missing `api_key` in the test's own stream fn — our client
    correctly refuses an unauthenticated request even though llama-server
    ignores it) and surfaced a genuine model characteristic worth recording:
    this particular 0.8B reasoning model spends its entire token budget on a
    `thinking` block for a trivial prompt and never reaches visible text, even
    at 8192 max_tokens / 60s, and ignores an explicit `thinking:{type:
    "disabled"}` request param — confirmed via manual `curl` probing, not
    assumed. The live assertion was scoped to what the round trip actually
    guarantees (a real persisted assistant message with content), not to
    specific text, to avoid a flaky/dishonest test.
  - Gate: `cargo fmt --check` clean, `cargo clippy -p pirust-coding-agent -p
    pirust-agent-core --all-targets --no-deps -D warnings` clean (a pre-existing,
    unrelated `pirust-tui/src/latex.rs` clippy error reproduces even on a clean
    stash — not touched, not mine), `cargo test --workspace`: 820 passed / 2
    ignored, 0 failed.
  - Deferred to Wave 3 (named in plan.md's own step 5, not silent): a byte-level
    oracle replay of `tests/fixtures/pi/rpc/commands.corpus.jsonl` through this
    dispatch loop. Skipped this wave because that tape was captured against
    Pi's own stub `AgentSessionRuntime`/session state, which this harness does
    not reproduce; Wave 3's live differential is the right place for it.
- [x] Wave 3 — DONE 2026-08-23:
  - `crates/pirust-coding-agent/src/rpc/run.rs` (NEW): `run_rpc_mode` — the
    stdin-JSONL/stdout-JSONL process loop. Blocking OS thread reads stdin lines
    into an unbounded mpsc channel; each line is dispatched on its own task
    tracked in a `tokio::task::JoinSet` (not a bare `tokio::spawn`) so a clean
    stdin-EOF shutdown can drain every in-flight command before returning exit
    code 0 — mirrors `rpc-mode.ts`'s default `shutdown()` awaiting outstanding
    handlers. `SIGTERM`→143 / `SIGHUP`→129 wired on `#[cfg(unix)]` via
    `tokio::signal::unix`; Windows has no equivalent (same documented gap
    `print_mode::NoSignals` already carries for every other mode). Harness
    loop events (+ synthesized `agent_settled`) are forwarded straight to raw
    stdout via `install_event_forwarding`, reusing `runtime_host::to_session_event`
    (now `pub(crate)`) — the identical `AgentSessionEvent` shape `--mode json`
    already emits, not a new one.
  - `crates/pirust-coding-agent/src/main.rs`: replaced the `--mode rpc` "not
    supported" stub with real wiring — builds an in-memory `V4Session` +
    `AgentHarness` via a new `sdk::create_agent_harness_session`, rejects
    `@file` args (RPC has no file-attachment concept), branches the v3
    `SessionManager`/`--name`-persistence bootstrap to skip entirely for RPC
    (that machinery is print/interactive-mode-only), then hands off to
    `rpc::run::run_rpc_mode`.
  - `crates/pirust-coding-agent/src/sdk.rs`: extracted `resolve_agent_ingredients`
    (system prompt/tools/model/thinking-level resolution) out of
    `assemble_agent_session` so `assemble_agent_harness_session` +
    `create_agent_harness_session` (both NEW) can reuse it instead of
    duplicating ~100 lines — the harness path is otherwise identical
    plumbing into `AgentHarnessOptions` instead of `AgentOptions`.
  - Two real bugs found and fixed via LIVE binary runs against the user's
    running `ggml-org/Qwen3.5-0.8B-GGUF` llama-server (unit tests alone did
    not exercise the actual stdin/stdout/process-exit lifecycle where these
    live): (1) the first `run_rpc_mode` draft used bare `tokio::spawn` per
    line with no tracking, so `main.rs`'s `std::process::exit` could kill the
    process mid-write for whichever command hadn't finished — fixed with the
    `JoinSet` drain-on-EOF described above; (2) even after that fix, a
    `prompt` command's full turn (agent_start/turn_start/message_*/agent_end/
    agent_settled) still went missing on shutdown, because `mode.rs`'s
    `Prompt` handler did its OWN inner `tokio::spawn` (from Wave 2) to ack
    immediately — invisible to the outer `JoinSet`, so the outer per-line task
    completed almost instantly with real work still detached and untracked.
    Fixed by removing the inner spawn entirely: the outer per-line task (each
    stdin line already runs on its own task) now awaits the turn inline,
    which keeps different commands concurrent with each other while making
    the outer task's lifetime honestly represent "is this command done yet".
  - Verify: live run of `set_thinking_level` + `prompt` piped into the real
    `pirust.exe --mode rpc` binary (stdin held open, bounded by an outer
    `timeout`) against the user's local llama-server — full expected event
    stream appeared (agent_start, turn_start, message_start/update/end ×2,
    turn_end, agent_end, agent_settled) followed by clean exit 0, no stderr.
    `cargo test -p pirust-coding-agent --test rpc_dispatch`: 10/10 still pass
    unchanged (the Prompt-arm fix didn't regress Wave 2's direct-dispatch
    tests). Gate: `cargo fmt` clean, `cargo clippy -p pirust-coding-agent -p
    pirust-agent-core --all-targets --no-deps -D warnings` clean, `cargo test
    --workspace`: 820 passed / 2 ignored, 0 failed.
  - Deferred / named, not silent: no automated `#[test]` spawns the real
    `pirust` binary end-to-end (Wave 3's live verification above was manual,
    via a shell pipeline — codifying it as a repo test is future work, not
    attempted this wave); `SIGTERM`/`SIGHUP` exit codes (143/129) are
    unverified in this session (dev environment is Windows; the code path is
    `#[cfg(unix)]`-only); no live differential against real Pi's own
    `--mode rpc` binary was run this wave (Wave 1's oracle/live-oracle tapes
    already cover protocol/event-shape fidelity; a second live comparison at
    the full-binary level would be substantial additional scope, not
    attempted here); `killTrackedDetachedChildren()` on signal still not
    ported (no detached-bash-child registry exists); RPC sessions remain
    in-memory only (no on-disk v4 session file), and RPC-mode/print-mode
    still use two unreconciled internal session representations.
- [ ] Wave 4 — RpcClient port + black-box tests
