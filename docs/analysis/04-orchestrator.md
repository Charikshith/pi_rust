# Package Analysis: `packages/server` (formerly `packages/orchestrator`)

> **REWRITTEN 2026-08-23.** The original version of this document analyzed
> `packages/orchestrator` (process-supervisor + Radius remote presence). That
> package **no longer exists** in the current `pi_space/pi` checkout
> (HEAD `2ff8ba622`): it was renamed to `packages/server` in commit
> `8495f9d0d` ("chore: rename orchestrator to server", #6898) and then
> substantially redesigned — `supervisor.ts`, `radius.ts`, `rpc-process.ts`,
> `storage.ts`, `handler.ts`, `ipc/*`, and `cli.ts` are all **gone**. There is
> **no process-spawning, no Radius, and no CLI** in the current design. This
> document describes the real, current package. feat-009 in `feature_list.json`
> has been updated to match.
>
> Source: `pi_space/pi/packages/server` (`@earendil-works/pi-server`, v0.84.2,
> 2,300 TS lines, 17 files) + its sole non-`pi-ai` dependency
> `pi_space/pi/packages/protocol` (`@earendil-works/pi-protocol`, v0.84.2,
> ~700 TS lines across `cbor/`, `framing.ts`, `codec.ts`, `schemas.ts`).
> Both packages are marked **"Experimental... may change or be removed
> without notice"** in their own READMEs. Nothing else in `pi_space/pi`
> (in particular, `packages/coding-agent`) depends on either package yet —
> confirmed by `grep -rl "pi-server" packages/*/package.json` returning only
> `packages/server/package.json` itself. So `pi-server` is presently a
> **standalone library with no first-party consumer**, not a shipped
> orchestrator daemon.

---

## 1. Purpose & Responsibilities

`PiServer` is a **transport-neutral, multi-session multiplexing server**.
It is a generic building block an application plugs into — it does not spawn
or supervise child processes itself, and does not know anything about
`pirust --mode rpc` workers. The application supplies:

- a **`PiServerService`** (`types.ts`): `listSessions`, `listModels`,
  `createSession`, `openSession` — the durable-storage boundary.
- one or more **`PiServerListener`**s (`listener.ts`): transport adapters that
  authenticate a connection and hand `PiServer` a `ByteConnection`. Only a
  Unix-domain-socket listener ships today (`transports/unix/`).

`PiServer` itself owns: the connection/handshake state machine, the wire
protocol (hello → request/response → events), a `LiveSessionManager`
(acquire/attach/detach/dispose reference-counted session runtimes), and a
`ServerSnapshotPublisher` (revision-numbered full-state broadcasts).

The wire protocol (`pi-protocol`) is **binary**: 4-byte big-endian
length-prefixed frames containing **CBOR** (a restricted, definite-length RFC
8949 subset), validated against **TypeBox** runtime schemas before encoding
and after decoding.

---

## 2. Public API Surface (`index.ts` + subpath exports)

Main entry (`.`): re-exports `errors.ts`, `listener.ts`, `protocol.ts`
(re-exported `pi-protocol` codec bits), `server.ts` (`PiServer`), `types.ts`.

Subpath `./unix` (`transports/unix/index.ts`): `createUnixListener`,
`createUnixServer`, `validateUnixSocketPath`, `UnixListenerOptions`,
`UnixServerOptions`. (Also exports `UnixByteConnection`, marked
`@internal ... for transport-level verification` only.)

Subpath `./testing` (`testing/index.ts`): `createTestServer`,
`TestServerService`, `TestSessionRuntime`, `ProtocolTestClient`,
`connectUnixTestClient`, `WireChannel`, `Deferred` — a full reference
service + protocol test client used by Pi's own conformance suite
(`test/conformance.test.ts`, `test/server.test.ts`, `test/sessions.test.ts`,
`test/unix.test.ts`, `test/unix-connection.test.ts`).

`@earendil-works/pi-protocol` (`.`): `encodeCbor`/`decodeCbor`/`CborError`
(+ size/depth limit constants), `encodeFrame`/`FrameDecoder`/
`assertCompleteFrame`/`FrameError`, `encodeClientMessage`/
`encodeServerMessage`/`ClientMessageDecoder`/`ServerMessageDecoder`/
`isSupportedProtocolVersion`/`ProtocolValidationError`, and every TypeBox
schema + inferred type from `schemas.ts` (`Command`, `ClientMessage`,
`ServerMessage`, `SessionSnapshot`, `SessionMetadata`, `ModelMetadata`,
`TranscriptItem`, `TranscriptProgress`, `ProtocolError`, etc).

`protocol.ts` in the server package additionally owns the **`pi-ai` ↔
`pi-protocol` bridge**: `toProtocolModelMetadata`, `toProtocolUsage`,
`toProtocolUserMessage`, `toProtocolAssistantMessage`,
`toProtocolToolResultMessage`, `toProtocolJsonValue`,
`sanitizeProtocolDetails` — pure mapping functions with compile-time
exhaustiveness checks (`ExactKeys<...>` type assertions) so a new `pi-ai`
field fails compilation here until explicitly reviewed.

---

## 3. Wire Protocol (`schemas.ts`, `codec.ts`, `framing.ts`, `cbor/`)

### Framing (`framing.ts`)
One frame = 4-byte big-endian unsigned length prefix + that many payload
bytes. `FrameDecoder` incrementally splits arbitrary byte chunks into
frames (header buffered across chunk boundaries; payload assembled from
64KiB internal blocks then concatenated only if >1 block — an allocation
optimization, not a behavior). `DEFAULT_MAX_FRAME_LENGTH = 16 MiB`.
`end()` on a decoder mid-header-or-payload throws `FrameError` ("Truncated
frame at end of stream").

### CBOR (`cbor/encoder.ts`, `cbor/decoder.ts`, `cbor/options.ts`)
A **strict, definite-length-only RFC 8949 subset** — explicitly documented
in the encoder's own comment. Supported major types: 0/1 (uint/negint, up
to `Number.MAX_SAFE_INTEGER`/`MIN_SAFE_INTEGER`, minimal-length encoding
required both directions), 2 (byte string = `Uint8Array`), 3 (UTF-8 text,
round-trip-validated), 4 (array, no holes/`undefined` elements), 5 (map,
string keys only, `undefined`-valued object properties are **omitted**, not
encoded as null), 7/27 (float64 — the ONLY float width; float16/float32 on
the wire are rejected on decode). Non-integer numbers always use the
9-byte float64 form (major type 7, additional info 27, 0xfb prefix); `-0`
therefore encodes as float64 too (`fb8000000000000000`), not as integer 0.
**Rejected outright, both directions**: indefinite-length items (0x1f
argument), CBOR tags (major type 6), the break marker (0xff), float16/
float32, non-finite numbers (`NaN`/`Infinity`), unsafe integers (outside
`Number.isSafeInteger`) even when they fit the wire's own integer range,
BigInt/Symbol/Function/Date/Map/circular references/array holes, trailing
bytes after one top-level item, duplicate map keys, non-string map keys,
depth beyond `DEFAULT_MAX_CBOR_DEPTH=64` (encode) — decode reads the length
argument and rejects **before** allocating/traversing when it exceeds
`maxByteLength`/`maxContainerLength`/`maxDepth`. Known hex vectors (ported
verbatim as the Rust golden fixture, `packages/protocol/test/cbor/cbor.test.ts`
lines 23-58): includes minimal-length boundary cases (23→`17` vs 24→`1818`),
multi-byte UTF-8 (`ü`,`水`,`𐅑` — the last is a 4-byte/2-UTF-16-surrogate-pair
codepoint), nested arrays, and `{a:1,b:[2,3]}`. `__proto__` as a **plain data
key** round-trips correctly (map entries are set via
`Object.defineProperty(..., enumerable: true)`, not `obj[key] = value`, so
Node's own prototype pollution guard doesn't fire) — Rust has no prototype
pollution concept, so this is automatically satisfied by any `HashMap`/
`BTreeMap`/`serde_json::Map`-backed decode, not a porting hazard.

### Codec / schema validation (`codec.ts`, `schemas.ts`)
`schemas.ts` defines every wire type as a TypeBox schema (`PROTOCOL_VERSION
= 1`); `codec.ts` wraps encode with **schema validation with a custom
plain-JSON-value walk (`isProtocolValue`) BEFORE the TypeBox `Check`** — this
double gate exists because TypeBox's `Check` alone doesn't reject exotic
prototypes/non-JSON values the way `isProtocolValue` does; decode runs CBOR
first, then TypeBox `Check`, and wraps any failure as
`ProtocolValidationError`. `ClientMessageDecoder`/`ServerMessageDecoder`
compose `FrameDecoder` + `decodeCbor` + schema check, and go into a
**permanent `failed` state** on the first error — no partial recovery, all
subsequent `push()`/`end()` calls throw immediately. Schema highlights:
- All wire objects are `additionalProperties: false` (`StrictObject` helper)
  — an unknown extra key anywhere fails validation, both directions.
- `IdSchema = Type.String({minLength:1})` — every id (session, request,
  connectionId, tool-call id...) must be non-empty.
- `ThinkingLevel`: `off|minimal|low|medium|high|xhigh|max` (7 variants —
  matches `pi-ai`'s `ModelThinkingLevel` per the file's own compile-time
  assertion, `_AiThinkingLevelsFitProtocol`).
- `SessionPhase`: `idle|turn|compaction|branch_summary|retry` — explicitly
  documented as "Matches `AgentHarnessPhase`" (pirust's own
  `pirust-agent-core` v4 harness already has this exact vocabulary —
  feat-008's v4 harness-swap prerequisite; direct reuse opportunity, not a
  new type to invent).
- `Command` (client→server, tagged on `command`): `list | create | attach |
  detach | prompt | steer | abort | set_model | set_thinking`. Note: **no
  `new_session`/`fork`/`clone`/`compact`/`bash`/model-listing-as-a-command**
  — this is a much smaller surface than the old RPC-mode protocol
  (`pirust-coding-agent`'s `rpc::types::RpcCommand`, 28 variants, feat-012).
  `list`/`prompt`/`steer`/`abort`/`set_model`/`set_thinking` each return a
  `CommandResult` tagged the same as the command name; `create`/`attach`
  return `{command, session: SessionSnapshot}`; `detach` returns
  `{command, sessionId}`.
- `ClientMessage = ClientHello | RequestEnvelope`; **hello must be the
  first message on every connection**, exactly once — a second hello, or
  a request before hello, is a protocol error (`PiServer.dispatchMessage`,
  not the schema layer).
- `ServerMessage = ServerHello | ServerHelloError | ResponseEnvelope |
  EventEnvelope`. `ServerEvent`: `server_snapshot | session_snapshot |
  session_progress | session_removed` (the last is declared in the schema
  but **never constructed anywhere in `server.ts`/`sessions.ts`** in this
  version — schema-ready, not yet wired; named, not silent, if the Rust
  port notices the same gap).
- `ProtocolErrorCode`: `version | busy | session_locked | not_found |
  invalid_request | not_implemented | internal_error` — `PiServerError`'s
  own code type is a strict `Extract<...>` **excluding** `version` and
  `internal_error` (those two are server-machinery-only, never
  service-thrown).
- `TranscriptItem` (user/assistant/tool) and `TranscriptProgress`
  (`item_started|assistant_delta|item_updated|item_finished`) model an
  **incremental transcript** distinct from `pirust-agent-core`'s v4
  `Entry`/`SessionTreeEntry` — `protocol.ts`'s `toProtocolAssistantMessage`
  etc. are the mapping layer, itself worth porting faithfully (pure
  functions, no I/O, straightforward oracle target) but is a **separate,
  optional bridge**, not required for the core server/protocol library to
  work (the reference `TestServerService` doesn't use it at all — it
  builds `SessionSnapshot`/`TranscriptItem` values by hand).

---

## 4. Connection & Session Lifecycle (`server.ts`, `sessions.ts`, `snapshots.ts`)

### Connection state machine (`connection.ts`, `server.ts`)
`ConnectionStage = awaitingHello → handshaking → ready → closing → closed`.
`accept()` creates a `ConnectionState` with a `handshakeTimeout`
(default 5000ms, `.unref()`'d so it can't keep the process alive) and wires
transport callbacks (`onData`/`onClose`/`onError`) to `receive`/
`transportClosed`/error-then-disconnect. `receive()` pushes bytes through
the connection's own `ClientMessageDecoder`; a decode error → `failProtocol`
(sends a `hello_error` frame best-effort, then disconnects — **even after
the handshake completed**, i.e. `hello_error` can arrive mid-session on a
framing violation, not just during handshake). `dispatchMessage` enforces
hello-first/hello-once; while `handshaking`, subsequent messages are queued
behind the in-flight `finishHandshake` promise rather than dropped.

`finishHandshake`: rejects unsupported `version` (`isSupportedProtocolVersion`
is a strict `===` check, not `>=`/`<=` — **only exactly `PROTOCOL_VERSION=1`
is accepted**), else sends `ServerHello{version,connectionId,snapshot}` where
`snapshot` was fetched **before** send (so it can race a concurrent
`broadcast()` — handled: if `snapshot.revision !== current` after send, a
**second** `server_snapshot` event frame is sent immediately after hello to
catch the connection up). Connection state checks (`closing`/`disconnected`/
stage/`connection.closed`) are re-validated after every `await` — this
codebase is disciplined about "did the world change while we were
awaiting" races throughout; the Rust port's async equivalent needs the same
discipline (recheck `Arc`/state after every `.await`, don't assume
pre-await state still holds).

### `LiveSessionManager` (`sessions.ts`)
Reference-counted session runtime cache, keyed by session id:
- `acquire(id, acquireRuntime)`: de-dupes concurrent `create`/`attach`
  racing on the same id via `openingSessions: Map<id, Promise<LiveSession>>`;
  loops if the existing entry is mid-`disposing` (waits, then retries from
  scratch — the entry may be gone or different by the time it resumes);
  throws `session_locked` if `terminal`.
- **`create()`** calls the service's `createSession`/`openSession`, checks
  `isClosing()` (disposes immediately + throws if the server closed mid-
  acquire), calls `runtime.snapshot()` **once** to validate
  `snapshot.id === id` (defends against a service bug returning the wrong
  session), THEN subscribes to runtime events, THEN inserts into
  `liveSessions` — subscribe-before-insert ordering matters so no event can
  arrive for an entry not yet in the map.
- **`executeCommand`** dispatch: `list`→service+live snapshots merged
  (live overrides stored, extra live-only sessions appended); `create`→
  `randomUUID()`-assigned id (server-assigned, service must persist that
  exact id — `CreateSessionOptions.id` doc comment), attach the calling
  connection, broadcast; `attach`→same but `openSession`; `detach`→remove
  connection from the live set, re-broadcast to the session's remaining
  connections if any, `maybeDispose`; `prompt`/`steer`/`abort`/
  `set_model`/`set_thinking`→`requireAttached` (the CALLING connection must
  have previously attached this session id, else `invalid_request`) then
  `runOperation` (bumps `operationCount` around the runtime call so
  `maybeDispose` won't fire mid-operation, broadcasts the resulting
  snapshot to **all** connections attached to that session, not just the
  caller).
- **`maybeDispose`**: disposes a live session ONLY when: server isn't
  closing, `ready`, not already disposing, **zero attached connections**,
  **zero in-flight operations**, and (unless `terminal`) `phase === "idle"`.
  This is the core resource-lifecycle invariant — a session with any
  attached connection, or a running turn, is never torn down, even if
  nothing GC-visible references it. `dispose()` runs exactly once
  (`live.disposing` memoizes it); on completion, removes from `liveSessions`
  and broadcasts a fresh server snapshot (unless closing).
- **`terminate`** (on a runtime-emitted `error` event): marks `terminal`
  (locks out `acquire`'s retry loop), force-closes every attached
  connection, then disposes. A `PiServerError` from the *runtime* — as
  opposed to one thrown synchronously from a command handler — always ends
  the session, unlike the connection-level errors above which only end the
  *connection*.
- **`close()`** (server shutdown): awaits any in-flight `openingSessions`
  first (reporting rejections, not propagating them), then disposes every
  live session unconditionally (does NOT wait for `operationCount`/attach
  count to reach zero — shutdown is not graceful per-session, only
  ordered).

### `ServerSnapshotPublisher` (`snapshots.ts`)
Monotonic `revision` counter, incremented only inside `performBroadcast`
(never on `get()`, which serves the CURRENT revision read-only — this
matters: two connections doing `hello` concurrently can legitimately
receive the same revision number if no broadcast interleaves). Broadcasts
are **serialized through `broadcastQueue`** (a chained promise) so
concurrent triggers (a command completing, a session disposing, a runtime
event) can't interleave two `performBroadcast` calls and produce
out-of-order revisions; a broadcast is skipped entirely if there are zero
`ready` connections or the server is closing (no wasted `listModels()`
calls). `PiServer` triggers a broadcast after every `disconnect()` with a
**previously-handshake-completed** connection, and `LiveSessionManager`
triggers one after every session count-visible change (create/attach/
detach/dispose) but deliberately NOT after every `session_snapshot` (that
one only reaches the session's own attached connections, not everyone —
two independent broadcast scopes, don't conflate them in the port).

---

## 5. Unix Transport (`transports/unix/listener.ts`)

The most operationally careful file in the package — worth reading in full
before porting, not summarizing away:

- **Socket path length validated up front** against the platform's real
  `sockaddr_un` limit: **107 bytes on Linux, 103 elsewhere** (macOS/BSD) —
  `MAX_UNIX_SOCKET_PATH_BYTES = process.platform === "linux" ? 107 : 103`.
  A silent bind failure past this limit is a real, previously-seen class of
  bug; the port must replicate this exact platform-conditional check, not
  hard-code one number, and needs a Windows answer too (see §8).
  **UPDATE (this rewrite):** Windows 10 1803+ / Server 2019+ support
  `AF_UNIX` natively on NTFS; this code path is not `#[cfg(unix)]`-gated in
  TS at all (no platform branch in `listener.ts` other than the byte-limit
  constant and the `chmod` skip). The port should try the same: attempt a
  real Unix-domain socket on Windows first via a crate that supports it
  (see §8), and only fall back to a different transport if that's
  infeasible — do not assume "Windows needs named pipes" without checking
  fitness first, since Pi itself doesn't appear to think so.
- **Two-file bind-then-link scheme, not a direct bind**: binds the actual
  `net.Server` to a **hashed private path** (`getOwnedBindPath` =
  `.p-<sha256(path).slice(0,8)>` in the same directory), verifies the bound
  path is really a socket (`lstat().isSocket()`), THEN hard-`link()`s the
  public `path` to it, THEN `chmod`s the public path (skipped on win32 —
  ACLs, not POSIX mode bits, govern Windows named-pipe-style access; if the
  port lands on a real AF_UNIX socket on Windows this chmod-skip should be
  revisited against whatever that platform actually enforces). This
  indirection exists so **stale-socket cleanup never destroys a live
  peer's bound path** — cleanup only ever touches paths whose
  `{dev,ino}` identity was captured at bind time by *this* listener
  instance.
- **Stale-socket removal** (`removeStaleSocket`): if `path` exists and
  `isSocket()`, probe `isSocketLive` (real connect attempt, 1s timeout,
  `.unref()`'d) — live → throw "already running"; the four dead-connection
  error codes tolerated are `ECONNREFUSED|ENOENT|EPIPE|ECONNRESET`. If
  dead: `rename()` to a temp sibling, re-`lstat` to confirm identity
  (`dev`+`ino`) didn't change during the rename race, then unlink the temp
  file — never a bare `unlink(path)` on a path something else might have
  just recreated.
- **`cleanupOwnedSocket`** (mirror logic on close): same rename-verify-
  unlink dance, but on failure to restore identity it renames the
  temp file **back** to `path` rather than losing it, and throws loudly
  ("path changed during cleanup") rather than silently discarding another
  process's socket.
- **Backpressure**: `UnixByteConnection.send` tracks `pendingBytes` and
  rejects (does not block/queue-forever) once
  `maxPendingBytes` (default `maxFrameLength * 4`) would be exceeded — a
  slow/stalled reader gets disconnected, not an unbounded write buffer.
  Writes for one connection are still strictly ordered (`writeTail` promise
  chain).
- **Graceful close with a hard deadline**: `close(finalChunk?)` writes any
  final chunk (e.g. a `hello_error` frame) after the outstanding
  `writeTail` drains, then `socket.end()`s; a `gracefulCloseTimeoutMs`
  (default 5000ms, `.unref()`'d) forces `socket.destroy()` if the peer
  never acks the FIN.

---

## 6. External Dependencies → Rust Crate Equivalents

| TS / Node API | Usage | Rust equivalent |
|---|---|---|
| `node:net` (`createServer`, `createConnection`, `Socket`) | Unix listener + stale-socket probe | `tokio::net::UnixListener`/`UnixStream` on unix; see §8 for Windows |
| `node:fs/promises` (`mkdir`,`lstat`,`link`,`rename`,`unlink`,`chmod`) | bind-then-link scheme, stale-socket cleanup, mode | `tokio::fs` equivalents; `std::os::unix::fs::PermissionsExt` for chmod (unix only) |
| `node:crypto` (`randomUUID`, `createHash("sha256")`) | ids, connectionId, owned-bind-path hash | `uuid` crate; `sha2` crate (new dep) |
| TypeBox (`Type.*`, `Check`) | runtime schema validation | hand-written `serde` structs + manual validators for the few non-structural constraints (`minLength:1`, `additionalProperties:false` is `serde(deny_unknown_fields)`, integer-range via newtypes or manual checks) |
| custom CBOR encoder/decoder | wire payload codec | hand-rolled port (no existing crate matches this exact restricted-subset spec byte-for-byte; `ciborium`/`serde_cbor` would need extensive option-tuning and still risk float-width/tag/indefinite-length divergence — a from-scratch ~250-line port matching `encoder.ts`/`decoder.ts` 1:1 is the safer, oracle-verifiable choice, same call the project made for `edit_diff.rs` in feat-004 over a generic diff crate) |
| length-prefix framing | `framing.ts` | hand-rolled (trivial, no crate needed) |
| `setTimeout(...).unref()` | handshake/graceful-close/socket-probe timeouts that must not keep the process alive | `tokio::time::sleep` inside a task that's simply not `.await`ed by anything keeping the runtime alive, or `tokio::time::timeout` — Rust has no direct `.unref()` analogue; the effect (timer doesn't block shutdown) falls out naturally from not holding a `JoinHandle` |
| `Promise` chaining for serialized broadcast | `ServerSnapshotPublisher.broadcastQueue` | a single-consumer `tokio::sync::mpsc` channel drained by one task, or a `tokio::sync::Mutex`-guarded async fn called in sequence |
| `Set<ConnectionState>` / `Map<id, LiveSession>` | connection/session registries | `Arc<Mutex<HashMap<...>>>` or `DashMap` |

---

## 7. Rust Porting Notes

### Proposed module layout (inside the existing `pirust-orchestrator` crate —
no new workspace crate; nothing else in pirust depends on this protocol yet,
so a split-out `pirust-protocol` crate would be premature per the Ponytail
simplicity-first precedent already set elsewhere in this codebase):
```
crates/pirust-orchestrator/
  src/
    lib.rs              # re-exports mirroring index.ts
    protocol/
      mod.rs
      cbor.rs            # encode_cbor/decode_cbor, CborError (pi-protocol/cbor/*)
      framing.rs         # encode_frame/FrameDecoder/assert_complete_frame (pi-protocol/framing.ts)
      schemas.rs         # serde types + manual validators (pi-protocol/schemas.ts)
      codec.rs           # encode/decode + validate composition (pi-protocol/codec.ts)
    ai_bridge.rs         # toProtocolModelMetadata/...Message (server/protocol.ts) — optional, separate wave
    errors.rs            # PiServerError + subtypes (server/errors.ts)
    connection.rs        # ByteConnection/ConnectionState traits (server/connection.ts)
    listener.rs          # PiServerListener trait (server/listener.ts)
    sessions.rs          # LiveSessionManager (server/sessions.ts)
    snapshots.rs         # ServerSnapshotPublisher (server/snapshots.ts)
    server.rs            # PiServer (server/server.ts)
    types.rs             # PiServerService/PiSessionRuntime traits (server/types.ts)
    transports/
      mod.rs
      unix.rs            # createUnixListener/createUnixServer (transports/unix/*)
    testing/             # test-only: TestServerService/ProtocolTestClient equivalents
      mod.rs
      service.rs
      client.rs
  src/bin/pirust-orchestrator.rs  # wires a real PiServerService over AgentHarness — PIRUST-SIDE ADDITION, see §9
```

### Porting gotchas / fidelity checklist
1. CBOR integers must round-trip through `i64`/`u64` with the SAME safe-
   integer boundary as JS (`Number.isSafeInteger`, ±2^53-1) — do not widen
   silently to full `i64`/`u64` range on decode; the oracle's own test
   vectors (`Number.MAX_SAFE_INTEGER`/`MIN_SAFE_INTEGER` and their
   rejected-out-of-range neighbors) pin this exactly.
2. `-0` and all non-integer finite numbers ALWAYS encode as the 9-byte
   float64 form — never attempt an integer-form encoding for `-0`, and
   never accept float16/float32 on decode.
3. The double validation gate in `codec.ts` (`isProtocolValue` walk BEFORE
   schema `Check`) exists for a reason (rejects exotic prototypes/symbols
   TypeBox's `Check` alone wouldn't catch) — a Rust `serde`-based decode
   gets most of this "for free" via strong typing, but `deny_unknown_fields`
   must be applied everywhere `additionalProperties:false` appears in the
   schema, and empty-string id fields need an explicit check (serde alone
   won't enforce `minLength:1` on `String`).
4. `ClientMessageDecoder`/`ServerMessageDecoder` latch into a permanent
   failed state on the first error — a naive Rust port using `?` per-call
   without a `failed: bool`/enum-state guard would silently allow recovery
   after a corrupt frame; this must be replicated.
5. The Unix listener's platform-conditional socket-path byte limit
   (107 Linux / 103 elsewhere) is a real, easy-to-drop detail — port it as
   an actual `cfg!`/runtime `std::env::consts::OS` check, not a single
   constant.
6. `maybeDispose`'s five-condition gate (§4) is the crux of the whole
   session lifecycle; get this one function's port exactly right (ideally
   oracle/property-tested with a scripted sequence of attach/detach/
   operation-start/operation-end/phase-change events) before anything else
   in `sessions.rs`, since every other command handler depends on its
   correctness for resource safety.
7. Async re-validation-after-every-`await` discipline (§4) is pervasive in
   `server.ts`; Rust's `async`/`.await` has the identical reentrancy hazard
   (state can change across any await point under a shared `Arc<Mutex<_>>`)
   — don't assume this class of bug "can't happen in Rust."

---

## 8. Open question for the Rust port: Windows Unix-domain sockets

Pi's own `listener.ts` makes **no Windows-specific transport branch** beyond
the socket-path byte limit and skipping `chmod` — it appears to rely on
Node's `net` module transparently using `AF_UNIX` on Windows (supported
since Windows 10 build 17063 / Windows Server 2019, on NTFS volumes only,
not FAT32/exFAT/network shares). Rust's `tokio::net::UnixListener` is
`#[cfg(unix)]`-gated and does **not** support Windows even where the OS
does. Two real options, to decide before Wave 5 (transport), not silently:
- The `interprocess` crate's `LocalSocketListener` supports real AF_UNIX
  sockets on Windows (when available) with a named-pipe fallback, and unix
  sockets natively on unix — closest behavioral match to what Pi actually
  does, previously identified as the pragmatic choice in the old analysis
  and still true here.
- A raw Windows named pipe (`tokio::net::windows::named_pipe`) is a
  materially different transport (no filesystem path collision semantics,
  no POSIX mode bits, different stale-handle detection) and would require
  a *documented, named* divergence from `listener.ts`'s actual behavior,
  not a drop-in substitute.

This document does not resolve it — that's the transport wave's job — but
flags it up front since it is THE porting risk in this package, and the
dev machine for this project is Windows.

---

## 9. What is — and is NOT — oracle-verifiable here

- **Oracle-verifiable against real Pi** (this package's own test suite is
  the direct oracle, same pattern as every other pirust wave):
  `packages/protocol/test/{cbor/cbor.test.ts,framing.test.ts,protocol.test.ts}`
  and `packages/server/test/{conformance,server,sessions,listener,unix,
  unix-connection}.test.ts`. The CBOR/framing/codec/schema layer, the
  `PiServer` connection+session state machine, and the Unix transport can
  all be driven the same way earlier waves drove Pi's real TS modules
  directly (these files have few/no cross-package imports beyond `pi-ai`
  types used only for compile-time assertions in `protocol.ts`, which is
  itself optional — see below).
- **NOT oracle-verifiable, because Pi hasn't built it either** (confirmed:
  no package in this checkout depends on `pi-server`): a concrete
  `PiServerService` backed by `pirust-agent-core`'s `AgentHarness`, wired
  into a real `pirust-orchestrator` binary that's actually useful as "a
  supervisor over pirust workers." This is a **pirust-side addition**, not
  a port — analogous to `sdk.rs`'s `SingleTurnSession` bridge in feat-005
  (built because Pi's own equivalent, `AgentSession`, wasn't a match for
  what pirust had), and must be named as such in the feature's evidence,
  not silently presented as "ported from Pi." The `session.ts` bridge
  functions (`protocol.ts`'s `toProtocol*` mapping functions) ARE Pi code
  and ARE oracle-verifiable on their own (pure functions); only the
  *wiring* of them into a live `AgentHarness`-backed service is new.
