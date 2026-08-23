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

5. **Wave 5 - Unix transport**
   - Resolve the Windows question named in analysis doc §8 FIRST (try a
     real cross-platform local-socket crate - `interprocess` - before
     assuming named pipes are needed; this is the dev machine's own OS, so
     it can be verified directly, not just reasoned about).
   - `transports/unix.rs`: port `listener.ts`'s bind-then-link scheme,
     platform-conditional path-length check (107 Linux / 103 elsewhere),
     stale-socket probe-and-cleanup, backpressure (`max_pending_bytes`),
     graceful-close-with-deadline.
   - Oracle: `test/unix.test.ts` + `test/unix-connection.test.ts` scenarios
     (fragmented hello, version/hello-ordering enforcement, oversized-frame
     handling, stale-socket recovery) replayed against the Rust listener
     using a Rust `ProtocolTestClient` equivalent (`testing/client.rs`).
   - Verify: `cargo test -p pirust-orchestrator` full conformance battery
     green, clippy/fmt clean, `./init.sh` green.

6. **Wave 6 - pirust-side addition (named as such, not Pi-verified): a real
   `PiServerService` over `AgentHarness` + a runnable `pirust-orchestrator`
   binary.** Only start this after Waves 1-5 are gated green. Scope and
   test strategy (scripted `Faux` provider through a real `AgentHarness`,
   NOT an oracle replay - there is nothing in Pi to replay against) to be
   planned concretely once Wave 5 lands.

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
