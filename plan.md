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
- [ ] Wave 2 — rpc_mode.rs dispatch loop over an RpcRuntimeHost
- [ ] Wave 3 — main.rs wiring (--mode rpc), signals/shutdown, live differential vs pi
- [ ] Wave 4 — RpcClient port + black-box tests
