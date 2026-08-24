# feat-010 - Dynamic WASM extensions (Rust-authored, sandboxed) — REVIVED 2026-08-23

**User decision (2026-08-23):** feat-010 was previously SKIPPED (see
`feature_list.json` history). Revived with a narrower, deliberately-chosen
scope after discussion:

- Extensions are authored in **Rust only**, compiled to `.wasm`, loaded at
  runtime. This is explicitly **not** an attempt to run Pi's real npm/TS
  extension ecosystem (that would require embedding a JS engine — evaluated
  and rejected by the user in favor of speed/memory and a pure-Rust
  toolchain). Named divergence, not an oversight: existing Pi extensions
  (the ones on `pi.dev/packages`) will NOT run under this system.
- Sandbox is mandatory, not optional. Wasmtime's own design gives this for
  free (a wasm module has zero ambient authority — no filesystem, no
  network, no process spawn — until the host wires an explicit import
  function for it). The design goal is a **small, fixed set of "doors"**
  (host-callable functions) mirroring exactly the six actions
  `pirust-extension-api`'s `ExtensionRuntime` already exposes to compile-time
  Rust extensions today (`crates/pirust-extension-api/src/runtime.rs`) — not
  a broad WASI filesystem/env surface.
- **Target: `wasm32-unknown-unknown`, not `wasm32-wasip1`.** WASI's `wasip1`
  target bundles ambient filesystem/env/clock imports that we would then
  have to explicitly lock back down to match the "only doors we built"
  model; `wasm32-unknown-unknown` starts with zero imports, which matches
  the sandbox goal directly. Confirmed already installed on this dev machine
  (`rustup target list --installed`).
- **Key existing-code insight:** `pirust-extension-api`'s `ExtensionHandler`
  is already `Fn(&ExtensionEvent, &ExtensionContext) -> Result<Value, String>`
  — JSON-shaped in, JSON-shaped out, at the exact boundary a WASM ABI needs.
  The WASM host is therefore a **new implementation of that same closure
  type**, not a new extension architecture. `ExtensionRunner` (`runner.rs`)
  does not change at all.

**Success criterion:** a Rust crate, compiled with
`cargo build --target wasm32-unknown-unknown`, loads into a running pirust
session, registers at least one tool via `pi_activate`, and that tool's
`execute` round-trips through the wasm guest via `pi_handle` when the LLM
calls it — with a runaway/malicious guest unable to exceed configured
CPU/memory limits or reach anything the host didn't explicitly expose via
`pi_host_call`.

## Guest ABI (the whole contract an extension author needs)

Four exports, one import — deliberately minimal:

- `pi_alloc(len: i32) -> i32` — guest allocates `len` bytes in its own linear
  memory, returns the pointer. Lets the host write a JSON request into guest
  memory before calling into it.
- `pi_dealloc(ptr: i32, len: i32)` (Wave 5) — frees a `(ptr, len)` buffer this
  guest allocated via `pi_alloc`. Ownership rule: whoever reads a buffer LAST
  frees it. The host frees anything it reads back from this guest
  (`pi_activate`/`pi_handle` results, and the `op`/`payload` buffers it wrote
  via `pi_alloc` once `pi_handle` returns — the guest already read them
  synchronously by then). The one case only the GUEST can free is a
  `pi_host_call` response: the host writes it into the guest's own memory and
  hands back a pointer, but control returns to the guest afterward, so the
  guest frees it itself once parsed. Optional but strongly recommended — a
  guest without this export is tolerated (the host skips freeing, silently,
  and that guest's allocations leak exactly as every guest's did before
  Wave 5) rather than erroring.
- `pi_activate() -> i64` — called once at load. Return value is a packed
  `(ptr << 32) | len` pointing at a UTF-8 JSON registration payload:
  `{ "tools": [...], "commands": [...], "flags": [...] }` (subset of
  `Extension`'s fields the guest can populate; `handlers`/event subscriptions
  come back the same shape under an `"events": [...]` key).
- `pi_handle(op_ptr, op_len, payload_ptr, payload_len) -> i64` — the single
  generic dispatch entrypoint. `op` is a small string tag: `"event:<type>"`,
  `"tool:<name>"`, or `"command:<name>"`. `payload` is the JSON-serialized
  `ExtensionEvent` / tool params / command args. Return is packed `(ptr<<32)|len`
  pointing at `{"ok": true, "value": ...}` or `{"ok": false, "error": "..."}`
  — maps directly onto `ExtensionHandler`'s `Result<Value, String>`.
- Import `pi_host_call(op_ptr, op_len, payload_ptr, payload_len) -> i64` — the
  guest's only way to reach the host. `op` names one of the six
  `ExtensionRuntime` actions (`send_message`, `send_user_message`,
  `append_entry`, `get_active_tools`, `get_all_tools`, `set_active_tools`)
  or a context accessor (`is_idle`, `has_pending_messages`, `get_system_prompt`,
  `abort`, `shutdown`). One import, dispatched by string host-side — not one
  import per action — keeps the guest's declared import surface small and
  auditable at load time (`register_host_imports`-style allow-list, same
  spirit as the neighbor project's `pi_wasm.rs` fail-closed unknown-import
  behavior, reviewed this session for reference, not reused directly).

## Waves

1. **Wave 1 - guest ABI + host loader skeleton — DONE 2026-08-23** (no
   sandbox limits yet). Shipped: `wasm-extensions` Cargo feature on
   `pirust-extension-api` (`dep:wasmtime`, optional, off by default —
   confirmed via `cargo tree -p pirust-extension-api` showing zero wasmtime
   in the default dependency graph); `crates/pirust-extension-api/src/wasm/
   {mod,memory,loader}.rs` (`WasmExtensionLoader::load(path, runtime) ->
   Result<Extension, String>`, `pi_alloc`/`pi_activate`/`pi_handle` guest
   exports + one `pi_host_call` import, `(ptr<<32)|len` packing); a real
   compiled example, `crates/pirust-extension-api/examples/wasm-hello/`
   (its own standalone `[workspace]`, built on demand via `cargo build
   --target wasm32-unknown-unknown --release` from
   `tests/wasm_extension_test.rs`, not by the parent workspace). Gate: 60/60
   feature tests, `cargo test --workspace` unaffected (867/2), fmt/clippy
   clean, default build confirmed wasmtime-free.

2. **Wave 2 - the six action doors + event dispatch — DONE 2026-08-23.**
   Shipped: all four remaining `ExtensionRuntime` actions (`send_message`/
   `send_user_message`/`append_entry`/`set_active_tools`) wired into
   `host_call` alongside Wave 1's `get_active_tools`/`get_all_tools`; a
   shared `call_guest` helper (factored out of Wave 1's `make_tool_executor`
   so the alloc-write-call-read round trip isn't duplicated) reused by both
   tool executors and the new `make_event_handler`; `ActivateResponse`
   extended with an `events: Vec<String>` list that `WasmExtensionLoader::
   load` turns into real `Extension.handlers` entries — `ExtensionRunner`
   dispatches them exactly like compile-time extensions, `runner.rs`
   untouched.

   **Design refinement vs. this plan's original wording (named, not
   silent):** the original text above said the `ExtensionContext`
   accessors should go through `pi_host_call` like the `ExtensionRuntime`
   actions. That doesn't hold up: `ExtensionRuntime`'s six actions are
   stable `Arc<Mutex<Box<dyn Fn>>>` slots set once at `HostState`
   construction; `ExtensionContext`'s closures, by contrast, are freshly
   built by `ExtensionRunner::create_context()` on every single dispatch,
   are not `Arc`-shared, and carry no `Send` bound. Routing them through a
   live `pi_host_call` mid-execution would need a scoped "current context"
   slot in `HostState`, set immediately before each call and cleared after
   — a real re-entrancy design of its own. Implemented instead: the HOST
   snapshots the three read-only accessors (`is_idle`/
   `has_pending_messages`/`get_system_prompt`) into a plain
   `{"is_idle","has_pending_messages","system_prompt"}` JSON object,
   computed in ordinary Rust (no wasm involved) and included alongside the
   event payload on every `pi_handle` call. **`abort()`/`shutdown()` remain
   explicitly deferred past Wave 2** — they are control-flow actions, not
   read-only queries, and need the same scoped-slot mechanism to do
   properly; no shortcut version was attempted.

   Proven end-to-end in `crates/pirust-extension-api/examples/wasm-hello/`
   (a third guest tool, `exercise_doors`, calls all four new doors and
   reports which succeeded; the guest also subscribes to `agent_start` and
   calls `append_entry` from INSIDE that event handler, proving a host-call
   door works there too, not just inside a tool). New tests in
   `tests/wasm_extension_test.rs`: one drives a real `ExtensionRunner::emit`
   and asserts on what a test-double `append_entry` closure actually
   captured (not just the handler's return value); another asserts all
   four new doors individually via their own captured test doubles. Gate:
   62/62 feature tests (60 -> 62, +2), `cargo test --workspace` unaffected
   (867/2), fmt/clippy clean (one `clippy::type_complexity` finding in the
   new tests fixed via named type aliases), default build still wasmtime-free.

3. **Wave 3 - sandbox limits (the part that makes this safe to load
   someone else's `.wasm`) — DONE 2026-08-24.**
   - New `WasmExtensionLimits { fuel: u64, max_memory_bytes: usize }`
     (`wasm/mod.rs`), `Default` = `fuel: 200_000_000`,
     `max_memory_bytes: 16 * 1024 * 1024` (16 MiB) — checked empirically
     against `wasm-hello`'s own well-behaved tools (comfortable headroom)
     and its deliberately-broken ones (trap in well under a second).
     `WasmExtensionLoader::load` stays as an ergonomic wrapper over a new
     `load_with_limits(path, runtime, limits)`, so Wave 1/2's call sites and
     tests needed no changes.
   - CPU cap: `Config::consume_fuel(true)` on a per-load `Engine` (Wave 1/2
     used `Engine::default()`; Wave 3 builds one explicitly) +
     `Store::set_fuel(limits.fuel)` once, right after `Store::new`, before
     `linker.instantiate`. **Confirmed (via `wasmtime-41.0.4`'s own source,
     `src/runtime/store.rs`) this is a per-instance LIFETIME budget, not a
     per-call one** — it is never refilled between calls. Documented as a
     deliberate simplicity-over-generosity tradeoff in `wasm/mod.rs`'s doc
     comment: a legitimate long-lived, high-call-volume extension could
     eventually exhaust its lifetime budget under normal use and need a
     fresh `load`; a per-call refill policy is a named future option, not
     implemented.
   - Memory cap: `wasmtime::StoreLimitsBuilder::new().memory_size(limits.max_memory_bytes).trap_on_grow_failure(true).build()`
     stored on `HostState`, wired via `Store::limiter(|state| &mut state.limits)`
     before instantiation. `trap_on_grow_failure(true)` was a deliberate
     choice over the default `false`: it forces ANY denied growth (host- or
     guest-triggered) to hard-trap the call immediately, rather than making
     `memory.grow` return `-1` to the guest — so a guest that never checks
     `memory.grow`'s return value (plausible for a naive/malicious guest)
     still can't limp along on a failed allocation.
   - **Real bug found and fixed while building the test fixtures (not a
     limiter bug — worth naming for whoever writes the next
     deliberately-broken guest fixture):** the first `grow_memory` guest
     tool (`vec![0u8; 160 * 1024 * 1024]`, then only `.len()` read back) was
     silently optimized away in the `--release` fixture build — LLVM proved
     the huge zero-filled allocation was unobservable (only its
     compile-time-known length was ever used) and deleted it entirely, so
     no real `memory.grow` ever happened and the "malicious" tool always
     trivially "succeeded", regardless of the limiter. Confirmed via a
     throwaway instrumented `ResourceLimiter` that logged every
     `memory_growing` call against the real compiled fixture: only two
     small, well-under-the-ceiling growth calls ever fired (the module's
     own ~1.06 MiB initial memory, then a single +1-page growth) — nothing
     near 160 MiB. Fixed by wrapping the allocation in
     `std::hint::black_box`, matching the pattern `burn_fuel`'s infinite
     loop already used for the same reason. The limiter mechanism itself
     was verified correct throughout via two isolated sanity checks before
     this fix was found: (a) `wasmtime`'s own documented `Memory::new`
     host-triggered-denial example, and (b) a minimal hand-written WAT
     module whose exported function directly executes `memory.grow` from
     guest bytecode with fuel simultaneously enabled — both denied
     correctly, isolating the bug to the fixture's dead-code elimination,
     not the sandboxing mechanism.
   - Tests (`tests/wasm_extension_test.rs`, both use `wasm-hello`'s new
     `burn_fuel`/`grow_memory` guest tools): `runaway_guest_traps_on_fuel_exhaustion_without_wedging_the_host`
     (a genuine infinite loop, run under a small custom fuel budget so the
     test itself stays fast — not the production default — traps as a
     normal `Result::Err`; a completely FRESH `load` afterward still works,
     proving the shared loader/`Engine` machinery isn't wedged by one
     exhausted instance) and `runaway_guest_traps_on_memory_ceiling_without_wedging_the_host`
     (same fresh-load-afterward proof, for the memory ceiling instead of
     fuel).
   - Gate: 64/64 feature tests (62 -> 64, +2), `cargo test --workspace`
     unaffected (867/2), fmt clean (one auto-fix pass, whitespace-only),
     clippy clean on both the feature-gated crate and the full workspace
     (only the same pre-existing, already-documented
     `pirust-tui/src/latex.rs` finding every prior wave has carried),
     default (non-feature) build still confirmed wasmtime-free via
     `cargo tree`.

4. **Wave 4 - real loading path + author docs — DONE 2026-08-24.**
   New Cargo feature `wasm-extensions` on `pirust-coding-agent`
   (`["pirust-extension-api/wasm-extensions"]`, off by default, confirmed
   wasmtime-free on a plain build via `cargo tree`). New
   `discover_wasm_extensions` in `crates/pirust-coding-agent/src/
   runtime_host.rs`, called from `bind_extension_runner` right after
   `builtins` is built from `built_in_extensions()` and before
   `ExtensionRunner::new_with_runtime` — additive, `runner.rs` untouched.
   Discovers `*.wasm` files (non-recursive) in `<agent_dir>/extensions/`
   (resolved via `ConfigEnv::agent_dir()`, respecting the existing
   `PIRUST_CODING_AGENT_DIR` override — the same accessor every other
   pirust subsystem already uses, not a new path-resolution scheme) and
   loads each through `WasmExtensionLoader::load`. Two resilience rules,
   both tested: a missing extensions directory is not an error (zero found,
   silent); a single bad `.wasm` file is caught, printed as a warning
   (`eprintln!`, matching this file's own existing convention — no `tracing`
   dependency added), and skipped, without blocking any other file in the
   same directory from loading.
   - **Design note (named, not silent):** `discover_wasm_extensions` takes a
     `&ConfigEnv` parameter rather than reading `std::env::var` /
     `ConfigEnv::from_process_env()` internally, specifically so tests can
     inject a `ConfigEnv` literal with `agent_dir_override` set — matching
     `config.rs`'s own documented convention of never calling
     `std::env::set_var` in tests (process-global, races under `cargo
     test`'s parallel threads). Tests live inside `runtime_host.rs` itself
     (`#[cfg(all(test, feature = "wasm-extensions"))] mod
     wasm_extension_discovery_tests`) rather than a separate `tests/` file,
     since `discover_wasm_extensions` is intentionally private — making it
     `pub` purely so an external integration test could reach it would leak
     an implementation detail for no real benefit.
   - Real, compiled-`.wasm`-driven tests (3): missing directory → empty,
     not an error; a real `wasm-hello` fixture copied into a temp
     `<agent_dir>/extensions/` loads and its `echo` tool is genuinely
     callable (not just present in the map); a garbage non-wasm file
     alongside a real one is skipped without blocking the real one.
   - New `docs/wasm-extensions.md`: a complete authoring guide for someone
     who has never touched wasmtime — crate setup, the full guest ABI, the
     six host-call doors, the event/context-snapshot shape, the two sandbox
     limits (with Wave 3's actual numbers), failure behavior, and every
     residual below, written for an external reader rather than a resumer.
   - Gate: `cargo fmt --check` clean (workspace-wide); `cargo clippy -p
     pirust-extension-api -p pirust-coding-agent --all-targets
     --features wasm-extensions --no-deps -D warnings` clean; `cargo build
     -p pirust-coding-agent` (no features) clean, `cargo tree` confirms zero
     wasmtime; `cargo test -p pirust-extension-api --features
     wasm-extensions` 64/64 (unchanged — this crate wasn't touched this
     wave); `cargo test -p pirust-coding-agent --features wasm-extensions`
     232/232 (229 pre-existing + 3 new); `cargo test --workspace` 867
     passed / 2 ignored / 0 failed — identical to every prior wave's
     baseline, confirming zero regressions from a feature that is off by
     default.

**feat-010 shipped all 4 planned waves, then closed 4 of its 5 named
residuals in Wave 5.**

5. **Wave 5 - close the memory leak, per-extension limits, abort/shutdown,
   and hot-reload residuals — DONE 2026-08-24.**
   - **`pi_dealloc`** (see Guest ABI above for the ownership rule): guest
     export added to `wasm-hello`; host frees `pi_activate`/`pi_handle`
     results and the `op`/`payload` input buffers via a new `dealloc_guest`
     helper in `wasm/loader.rs` (best-effort — a guest without the export is
     tolerated, not an error); the guest's own `call_host_raw` frees the one
     buffer only it can free (a `pi_host_call` response). Proven by a new
     `many_tool_calls_do_not_exhaust_the_guest_memory_ceiling` test: several
     thousand calls with multi-KB payloads under the default 16 MiB ceiling
     — this would have blown through the ceiling before the fix.
   - **Per-extension sandbox limits**: a `<name>.wasm.limits.json` sidecar
     next to `<name>.wasm` (either/both of `fuel`/`max_memory_bytes`,
     `#[serde(default)]` per field); `discover_wasm_extensions` reads it and
     calls `load_with_limits` instead of the always-default `load`.
     Self-declared limits from `pi_activate` were deliberately rejected — an
     untrusted guest declaring its own ceiling isn't a real sandbox control,
     only an operator-owned file is. Proven by
     `sidecar_limits_file_overrides_the_default_fuel_budget`.
   - **`abort`/`shutdown` host-call doors**: the Wave 2 "scoped current
     context slot" design was never actually needed — both route through
     two new `ExtensionRuntime` fields (`Arc<Mutex<Box<dyn Fn + Send +
     Sync>>>`, the exact same stable slot pattern the other six actions
     already use), which `HostState` already holds. `ExtensionRunner::
     create_context()`'s `abort`/`shutdown` closures now call through those
     slots too — a free win: native (non-wasm) handlers get real abort/
     shutdown, not just wasm ones. Bound in `pirust-coding-agent` to
     `Agent::abort()` and `std::process::exit(0)` respectively. Proven by
     `guest_can_reach_all_six_host_call_doors` (wasm) and
     `native_handler_abort_and_shutdown_reach_bound_closures` (native).
   - **Hot-reload**: deliberately narrowed to WASM-extension discovery only
     — full `/reload` (skills/prompts/themes/context files) stays unwired,
     a separate pre-existing gap. New `/reload-extensions` slash command
     (`interactive_mode.rs`) calls `SingleTurnSession::reload_wasm_
     extensions()`, which re-runs `discover_wasm_extensions` against the
     same bound `ExtensionRuntime` Arc and appends only extensions whose
     `resolved_path` isn't already loaded (the pure merge step,
     `new_extensions_only`, is unit-tested directly —
     `new_extensions_only_dedupes_by_resolved_path` — rather than through a
     full `SingleTurnSession`, to avoid the env-var-agent_dir race
     `config.rs`'s tests already document avoiding).
   - Gate: `cargo fmt --check` clean; `cargo clippy -p pirust-extension-api
     -p pirust-coding-agent --all-targets --features wasm-extensions
     --no-deps -D warnings` clean; `cargo clippy --workspace --all-targets
     --no-deps -D warnings` clean except the same pre-existing
     `pirust-tui/src/latex.rs` finding every prior wave has carried; default
     (no-feature) `cargo build -p pirust-coding-agent` clean, `cargo tree`
     confirms zero wasmtime; `cargo test -p pirust-extension-api --features
     wasm-extensions` 66/66 (64 -> 66, +2); `cargo test -p pirust-coding-agent
     --features wasm-extensions` 234/234 (232 -> 234, +2); `cargo test
     --workspace` 868 passed / 2 ignored / 0 failed (867 -> 868, +1 — the
     native abort/shutdown test runs unconditionally, not behind the wasm
     feature). No git commits/pushes during construction.

Remaining named residual (not closed this wave, not a blocker):

- No live differential or fuzzing of the guest ABI itself (malformed
  `pi_activate`/`pi_handle` JSON is handled defensively via `serde`'s
  `#[serde(default)]`/`Result` plumbing and exercised by the "garbage file"
  test, but not adversarially fuzzed). Also newly named this wave: `commands`/
  `flags` declared in `pi_activate`'s JSON are still parsed but never wired
  into the loaded `Extension` (unchanged from Wave 2 — out of Wave 5's
  approved scope).

## Notes for whoever resumes

- **Do not** reach for `wasmtime::component` (the Component Model / WIT
  interface) — evaluated and rejected this session. The neighbor project
  `pi_agent_rust` uses it (`src/extensions/wasm_host.rs`,
  `docs/wit/extension.wit`) for a much richer, multi-language extension
  surface; pirust's own extension protocol is already a single
  JSON-in/JSON-out shape end-to-end, so a typed WIT layer buys nothing here
  and costs real toolchain complexity. Plain `wasmtime::{Engine, Linker,
  Store}` core-module API is the right level.
- No Pi oracle exists for any of this (named, not silent, same precedent as
  feat-009 Wave 6's `agent_service` addition) — it is a pirust-only feature
  with no TypeScript equivalent to byte-verify against. Verification is
  black-box: real compiled `.wasm` fixtures driven through the real host.
- `crates/pirust-extension-api/src/runtime.rs`'s six `ExtensionRuntime`
  action slots are the complete list of host capabilities to expose in Wave
  2 — do not invent additional doors (file/network/exec) speculatively; add
  one only when a real wasm extension needs it, matching this project's
  YAGNI convention elsewhere (e.g. feat-004's tool scope decisions).
