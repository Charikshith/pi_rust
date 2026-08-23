# feat-009 - PiServer session-multiplex library (pirust-orchestrator)

**Success criterion:** `pirust-orchestrator` speaks Pi's exact binary wire
protocol (length-prefixed CBOR frames, TypeBox-schema-shaped messages) and
reproduces `PiServer`'s connection/session lifecycle behavior, verified
against real Pi's own conformance test suite as the oracle - same pattern as
every prior wave. See `docs/analysis/04-orchestrator.md` (rewritten
2026-08-23) for the full analysis; this plan assumes that document as read.

**Scope correction (2026-08-23):** the original feat-009 description (spawn
`pirust --mode rpc` workers, Radius remote presence, JSONL-over-socket) was
based on a version of Pi that no longer exists. Real Pi renamed
`packages/orchestrator` to `packages/server` and redesigned it into a
generic, transport-neutral session-multiplexing library with **no
process-spawning and no Radius**. This plan builds the current thing.

**Named, not silent:** no package in `pi_space/pi` (including
`packages/coding-agent`) implements `PiServerService` yet. So Waves 1-5
below port real, oracle-verifiable Pi library code. Wave 6 (an
`AgentHarness`-backed `PiServerService` + a runnable binary) is a
**pirust-side addition** with no Pi oracle to check it against - build it,
but its evidence must say so plainly, the same way `sdk.rs`'s
`SingleTurnSession` bridge was labeled in feat-005.

## Waves

1. **Wave 1 - CBOR + framing (pure codec, no I/O)**
   - `scripts/gen-orchestrator-oracle.mjs`: import `packages/protocol/src/
     cbor/{encoder,decoder}.ts` and `framing.ts` directly (both are
     self-contained, no cross-package imports) and drive them with (a) the
     known hex vectors already in `cbor.test.ts`/`framing.test.ts`, (b) a
     wider generated battery (nested containers, boundary integers, UTF-8
     edge cases, every documented rejection case) -> `tests/fixtures/pi/
     orchestrator/{cbor,framing}.cases.jsonl`.
   - `crates/pirust-orchestrator/src/protocol/{cbor,framing}.rs`: hand-rolled
     port (not a generic CBOR crate - see analysis doc §6 for why), matching
     the exact restricted RFC 8949 subset and the 4-byte-BE frame format.
   - `tests/cbor_golden.rs` + `tests/framing_golden.rs`: replay every
     captured case, byte-identical encode and equivalent decode, matching
     every rejection case's error class.
   - Verify: `cargo test -p pirust-orchestrator`, clippy/fmt clean, oracle
     `--check` idempotent.

2. **Wave 2 - schemas + codec (validation layer) — DONE 2026-08-23, scope
   narrowed to the envelope layer** (see `feature_list.json`'s feat-009
   evidence for the full writeup). Implemented: `ClientMessage`
   (`hello`/`request`), `ServerMessage` (`hello`/`hello_error`/`response`/
   `event`), `ProtocolError`/`ProtocolErrorCode`, and the codec composition
   (`encode_client_message`/`encode_server_message`/`ClientMessageDecoder`/
   `ServerMessageDecoder`/`is_supported_protocol_version`) — all hand-written
   validators (not serde derive; `ResponseEnvelope`'s boolean `ok`-
   discriminated split doesn't map cleanly onto serde's tag mechanism).
   `request`/`result`/`event`/`snapshot` payload bodies are generic
   `ProtocolJson` (the `JsonValueSchema` domain) this wave, NOT the real
   `Command`/`CommandResult`/`ServerEvent`/`SessionSnapshot` unions — that
   deep shape typing moved to Wave 4 below (folded in, not skipped), since
   it makes more sense to build those types against real session-lifecycle
   behavior than in isolation. `scripts/gen-orchestrator-oracle.mjs`
   captured the full `protocol.test.ts` battery either way (31 cases; 4
   tagged `"scope":"deferred"` for Wave 4 reuse).

3. **Wave 3 - errors + connection + listener traits — DONE 2026-08-23**
   (see `feature_list.json`'s feat-009 evidence for the full writeup).
   `errors.rs`: one `PiServerError` struct + convenience constructors
   (not TS's subclass hierarchy — no inheritance/`.name` in Rust) +
   `InternalServerError`. `connection.rs`: `ByteConnection`/
   `ByteConnectionHandler` traits (`async_trait`), `ConnectionStage`,
   `is_terminal_connection`; `ConnectionState` scoped down to the fields
   that don't need a concrete async-runtime shape yet (`connection`/
   `handshake`/`handshakeTimeout` deferred to Wave 4/5). `listener.rs`:
   `PiServerListener` trait. No oracle needed (pure type/trait definitions,
   same proportionality precedent as `auth_guidance.rs`) — unit-tested
   only, 14/14 green.

4. **Wave 4 - sessions + snapshots + PiServer (the state machine), split into
   two sub-waves once the real size became clear:**

   **Wave 4a - deep schema typing — DONE 2026-08-23** (see
   `feature_list.json`'s feat-009 evidence for the full writeup). Replaced
   Wave 2's generic `ProtocolJson` payload bodies with the real typed
   unions: `ThinkingLevel`/`SessionPhase`/`ModelRef`/`ModelMetadata`,
   content types, `Usage`, `TranscriptItem`/`TranscriptProgress` (role→
   status two-level dispatch + cross-field consistency enforced by
   construction), `SessionMetadata`/`SessionSnapshot`/`ServerSnapshot`,
   `Command`/`CommandResult`/`ServerEvent`. `RequestEnvelope`/
   `ResponseEnvelope`/`EventEnvelope`/`ServerHello` now carry these real
   types instead of opaque JSON. Field order cross-checked against REAL
   `protocol.ts`/`sessions.ts` construction sites, not just schema
   declaration order (caught a genuinely non-obvious detail: `Usage
   .reasoning` sits between `cacheWrite` and `totalTokens` when present,
   not at the end). Oracle extended with `protocol.test.ts`'s full
   remaining battery (assistant/tool status consistency, nonterminal
   items, nested tool details) — 51 codec cases total, the 4 Wave-2
   `"scope":"deferred"` records now asserted normally.

   **Wave 4b - the live state machine — DONE 2026-08-23** (see
   `feature_list.json`'s feat-009 evidence for the full writeup): `sessions.rs`
   (`LiveSessionManager`, the five-condition `maybe_dispose` gate —
   analysis doc §7 gotcha 6 — implemented exactly as specified),
   `snapshots.rs` (`ServerSnapshotPublisher`, serialized broadcast queue),
   `server.rs` (`PiServer` connection/handshake state machine, hello-once
   enforcement, version check, `fail_protocol`) — built against Wave
   4a's real types instead of opaque JSON. `testing/service.rs` ported as
   the reference double (`TestServerService`/`TestSessionRuntime`).
   - Oracle scope decision (named, not silent): adapting `test/
     sessions.test.ts`/`test/server.test.ts` into `gen-orchestrator-oracle.mjs`
     was deferred rather than attempted this wave — the gate and full command
     lifecycle are instead covered by plain Rust unit tests against the ported
     `testing/service.rs` double directly. Real oracle-replay parity against
     those two TS test files remains open for a future wave.

5. **Wave 5 - Unix transport — DONE 2026-08-23** (see `feature_list.json`'s
   feat-009 evidence for the full writeup). Windows question (analysis doc
   §8) resolved, not silently: `interprocess` was evaluated and NOT adopted
   (its Windows backend is a named pipe, not a real `AF_UNIX` filesystem
   socket — no inode identity, no `lstat`/`link`/`chmod` semantics; adopting
   it would not have let this dev machine verify `listener.ts`'s actual
   behavior any better than not using it). Shipped instead: `transports/
   unix/options.rs` (pure, cross-platform — path validation, option
   resolution, owned-bind-path SHA-256 hash); `transports/unix/listener.rs`
   (`#[cfg(unix)]`, real 1:1 port of `UnixListener`/`UnixByteConnection`
   against `tokio::net::UnixListener`/`UnixStream` — bind-then-link,
   stale-socket cleanup, backpressure, graceful close); `testing/client.rs`
   (`ProtocolTestClient`, cross-platform over a `WireChannel` trait, port of
   `testing/client.ts`); `testing/duplex.rs` (a NEW, non-Pi in-memory
   transport double so the transport-agnostic conformance battery runs over
   real async byte I/O on this Windows dev machine). Verification split
   honestly: `tests/conformance.rs` (11 tests, cross-platform via the duplex
   double) actually RUNS and passes here; `tests/unix_transport.rs`
   (`#[cfg(unix)]`, ported from `unix.test.ts`/`unix-connection.test.ts`)
   type-checks and clippy-lints clean cross-compiled to
   `x86_64-unknown-linux-gnu` (which caught and this wave fixed two real
   `Send`-future bugs). **Update 2026-08-23 (later session):** the "has not
   been RUN on this dev machine" gap is now closed — run for real inside a
   `rust:1` Linux container (Podman, WSL2 backend, already present on this
   Windows machine) via `podman run --rm -v <repo>:/workspace -w /workspace
   docker.io/library/rust:1 cargo test -p pirust-orchestrator --test
   unix_transport`: 6/6 passed against a real `AF_UNIX` socket, not just
   type-checked. No new async runtime or transport crate needed — tokio
   stays; this only needed a real Linux execution environment on the dev
   machine.

6. **Wave 6 - pirust-side addition (named as such, not Pi-verified): a real
   `PiServerService` over `AgentHarness` + a runnable `pirust-orchestrator`
   binary — DONE 2026-08-23** (see `feature_list.json`'s feat-009 evidence
   for the full writeup). New `crates/pirust-orchestrator/src/agent_service/
   {mod,conversions,runtime,service}.rs`: `AgentPiSessionRuntime`
   (`PiSessionRuntime` over one real `AgentHarness` per session — one
   instance per session, permanent single harness-subscription, since
   `AgentHarness::subscribe` has no unsubscribe) and `AgentServerService`
   (`PiServerService`, builds harnesses via `pirust-coding-agent`'s existing
   `sdk::create_agent_harness_session` rather than reimplementing model/
   tool/session wiring). `main.rs` replaced with a real `--socket <path>`
   binary reusing `pirust-coding-agent`'s settings/auth/model-runtime
   bootstrap directly (not a CLI-parity clone — model/thinking choices move
   per-session over the wire instead of CLI flags, named not silent).
   Tested per the stated strategy: `tests/agent_service_e2e.rs` drives a
   real `AgentServerService`/`AgentHarness` (scripted `Faux` provider, no
   live network) through the actual wire protocol (hello/create/attach/
   prompt over `DuplexTransport`), asserting a real assistant transcript
   item comes back — not an oracle replay, since none exists for this
   addition. Verification note (named, not silent): adding `pirust-ai` as a
   dependency pulls in `reqwest`/`ring` transitively, which broke Wave 5's
   free `x86_64-unknown-linux-gnu` cross-check trick for this crate (`ring`
   needs a C cross-compiler this Windows dev machine doesn't have) — the new
   `agent_service` code and `main.rs`'s trivial `#[cfg(unix)]` split were
   therefore verified by native Windows fmt/clippy/test only, not also
   cross-compiled like Wave 5's `transports/unix` code was.

   **Update 2026-08-23 (later session):** the "run the actual binary against
   a real Unix socket" gap is now closed the same way as Wave 5's - native
   build inside a `rust:1` Linux container (Podman/WSL2, already on this
   machine) sidesteps the `ring` cross-compiler problem entirely since it is
   a native build, not a cross-compile. New `tests/real_binary_unix_socket.rs`
   spawns the real compiled `pirust-orchestrator` binary and drives a real
   client through a real handshake over a real `AF_UNIX` socket. First run
   found a REAL bug: the real builtin catalog's `openrouter/auto` /
   `openrouter/auto-beta` entries carry real Pi's own unknown-pricing
   sentinel (`cost.input = -1_000_000`), and `agent_service::conversions::
   model_metadata` was missing the `nonNegativeNumber`/`Math.max(1, ...)`
   clamps real Pi's own `toProtocolModelMetadata`
   (`packages/server/src/protocol.ts`) applies before putting cost/context/
   max-token fields on the wire - so the real binary panicked building its
   own `ServerHello` snapshot. Fixed by porting that clamp exactly (a
   `non_negative_number` helper + a `.max(1)` floor on context_window/
   max_tokens). Re-verified in the same container: `cargo test -p
   pirust-orchestrator` 41 passed/10 suites/0 failed, `clippy --all-targets
   --no-deps -D warnings` clean, `fmt --check` clean. Only remaining named
   residual: real end-to-end verification against a live model provider
   (not the `Faux` double).

## Notes for whoever resumes

- Reuse opportunity already confirmed: `SessionPhase` in the wire schema
  (`idle|turn|compaction|branch_summary|retry`) is documented in Pi's own
  source as matching `AgentHarnessPhase` - check `pirust-agent-core`'s v4
  harness for an existing enum with this exact vocabulary before defining a
  new one in `schemas.rs`.
- `pirust-coding-agent`'s feat-012 `RpcClient`/RPC types are **not** reused
  here - the new protocol has a different, smaller command set and a
  different (binary CBOR, not JSONL) wire format. Do not try to unify them.
- No new workspace crate: everything lives inside `crates/pirust-orchestrator`
  (already scaffolded, stub `main.rs`). New dep needed later: `interprocess`
  (Wave 5 transport) and possibly `sha2` (owned-bind-path hashing, Wave 5).
  No new dep needed for Waves 1-4.
