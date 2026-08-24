# Writing a WASM extension for pirust

This is a pirust-only feature (feat-010) — it has no equivalent in Pi, which
loads `.ts`/`.js` extensions in-process instead. Extensions here are written
in Rust, compiled to a `.wasm` file, and loaded at startup from
`<agent_dir>/extensions/*.wasm` (typically `~/.pirust/agent/extensions/`, or
wherever `PIRUST_CODING_AGENT_DIR` points). They run inside a sandbox: no
filesystem, no network, no process spawn, and no memory/CPU beyond
configured limits — a wasm module starts with zero ambient authority, and
only gets what this doc's host-call list below grants it.

If you've never touched wasmtime or written anything WASM before, this doc
is written for you. The complete, real, working reference implementation
this doc describes is `crates/pirust-extension-api/examples/wasm-hello/` in
this repo — read its `src/lib.rs` alongside this doc rather than trying to
build purely from this description.

## 1. Set up your crate

```bash
cargo new --lib my-extension
cd my-extension
rustup target add wasm32-unknown-unknown   # one-time, per machine
```

In `Cargo.toml`, tell Rust to build a `.wasm` file instead of a normal
library:

```toml
[lib]
crate-type = ["cdylib"]
```

Build it with:

```bash
cargo build --target wasm32-unknown-unknown --release
```

Your compiled extension will be at
`target/wasm32-unknown-unknown/release/my_extension.wasm`. Copy it into
`<agent_dir>/extensions/` and it will be picked up the next time pirust
starts (there is no hot-reload yet — see Residuals below).

## 2. The contract: three exports, one import

Your crate must export exactly three `extern "C"` functions, and may import
one:

- `pi_alloc(len: i32) -> i32` — the host calls this to ask your extension to
  set aside `len` bytes of its own memory and hand back a pointer. The host
  then writes a JSON request into that space before calling you.
- `pi_activate() -> i64` — called once, when your extension loads. Return a
  packed `(pointer, length)` (see below) pointing at UTF-8 JSON describing
  what you're registering:

  ```json
  {
    "tools": [
      { "name": "echo", "label": "Echo", "description": "..." }
    ],
    "commands": [],
    "flags": [],
    "events": ["agent_start"]
  }
  ```

  `commands`/`flags` are parsed but not wired to anything yet (see
  Residuals). `events` is a list of event names you want to be told about —
  see §4.

- `pi_handle(op_ptr, op_len, payload_ptr, payload_len) -> i64` — called every
  time one of your tools is invoked, or one of your subscribed events fires.
  `op` is a short string: `"tool:<name>"` for a tool call, `"event:<type>"`
  for an event. `payload` is the JSON request. Return packed
  `(pointer, length)` pointing at `{"ok": true, "value": ...}` on success or
  `{"ok": false, "error": "..."}` on failure.

- `pi_host_call(op_ptr, op_len, payload_ptr, payload_len) -> i64` (import,
  not export) — your one way to ask the host to do something. See §3.

### Packing a pointer and a length into one number

The host and your extension pass `(pointer, length)` pairs as a single
64-bit number, to avoid needing two return values:

```rust
fn pack(ptr: u32, len: u32) -> i64 {
    (((ptr as u64) << 32) | (len as u64)) as i64
}
fn unpack(packed: i64) -> (u32, u32) {
    let bits = packed as u64;
    ((bits >> 32) as u32, (bits & 0xFFFF_FFFF) as u32)
}
```

## 3. Calling back into pirust: the host-call doors

Your extension can ask the host to do six things, by calling the imported
`pi_host_call` with one of these op names and a JSON payload:

| op | payload | what it does |
|---|---|---|
| `get_active_tools` | (none) | returns the list of currently active tool names |
| `get_all_tools` | (none) | returns the list of every known tool name |
| `set_active_tools` | `{"tools": ["read", "bash"]}` | changes which tools are active |
| `send_message` | `{"message": ..., "options": ...}` | sends a custom message |
| `send_user_message` | `{"content": "...", "options": ...}` | sends a message as if the user typed it |
| `append_entry` | `{"custom_type": "...", "data": ...}` | persists an app-defined entry |

Every response comes back as `{"ok": true, "value": ...}` or
`{"ok": false, "error": "..."}`, the same envelope shape your own
`pi_handle` returns.

`abort`/`shutdown` are **not** available yet — deliberately deferred, see
Residuals.

## 4. Reacting to events, and the context snapshot

If `pi_activate`'s `"events"` list names an event type (e.g.
`"agent_start"`), your `pi_handle` will be called with
`op = "event:agent_start"` whenever that event fires, with a payload shaped
like:

```json
{
  "event": { "type": "agent_start" },
  "context": {
    "is_idle": true,
    "has_pending_messages": false,
    "system_prompt": "..."
  }
}
```

The `context` object is a snapshot of three read-only values, computed by
the host right before calling you — it is not a live door you can call at
arbitrary times, just data included with each call.

## 5. The sandbox limits

Every loaded extension gets, by default:

- **A CPU budget of 200,000,000 wasmtime "fuel" units**, for the extension's
  entire loaded lifetime (not per call — see the doc comment on
  `WasmExtensionLimits` in `pirust-extension-api`'s source for the exact
  reasoning). An infinite loop or runaway computation will trap with an
  error once the budget runs out, rather than hanging pirust. Once
  exhausted, that loaded instance is done — a fresh call to load it again
  starts a new budget.
- **A memory ceiling of 16 MiB.** Trying to grow past it fails cleanly
  rather than succeeding or crashing anything.

If your extension legitimately needs more of either, there is currently no
per-extension way to configure it — that is a named residual below.

## 6. What happens if something goes wrong

- If `<agent_dir>/extensions/` doesn't exist, pirust starts normally with
  zero wasm extensions loaded — this is not an error or a warning.
- If one `.wasm` file fails to load (corrupt, missing an export, hits its
  fuel/memory limit while just being loaded, etc.), pirust prints a warning
  for that file and keeps going — every other extension in the folder still
  loads. One broken extension never takes down your session.

## Residuals (named, not silent)

- `abort`/`shutdown` host-call doors are not implemented. They need a
  different mechanism than the other six actions (a scoped "current
  context" slot, not a stable shared one) and were deliberately deferred.
- `commands` and `flags` declared in `pi_activate` are parsed but not wired
  to anything — declaring one has no effect yet.
- No hot-reload: a `.wasm` file added or changed after startup is not picked
  up until pirust restarts.
- No per-extension limits: every loaded extension gets the same fuel/memory
  budget; `WasmExtensionLimits` as a type supports different values per
  load, but nothing in the real startup path exposes a way to set them
  per-file yet.
