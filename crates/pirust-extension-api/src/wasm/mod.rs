//! Sandboxed Rust->WASM extension loader (feat-010 Waves 1-3).
//!
//! No Pi oracle exists for this — it is a pirust-only addition (see
//! `plan.md` at the repo root for the guest ABI contract and wave
//! breakdown), feature-gated behind `wasm-extensions` so a plain build never
//! links wasmtime. `ExtensionRunner` (`crate::runner`) does not change at
//! all: a loaded wasm extension is just another `Extension` whose tool
//! closures and event handlers happen to call back into a wasm guest
//! instead of running native Rust directly.

mod loader;
mod memory;

pub use loader::WasmExtensionLoader;

/// Per-load sandbox limits (feat-010 Wave 3) — the part that actually earns
/// the word "sandboxed": a CPU budget (wasmtime fuel units) and a linear
/// memory ceiling (bytes), both wired via wasmtime's own built-in
/// mechanisms (`Config::consume_fuel` + `Store::set_fuel`;
/// `StoreLimitsBuilder`/`ResourceLimiter`) — no extra crate needed.
///
/// **Fuel is a per-instance lifetime budget, not a per-call one:** it is set
/// once when the `Store` is created and is never refilled afterward, so it
/// is shared across every tool call/event dispatch made through one loaded
/// extension for as long as that extension stays loaded. A runaway call
/// that exhausts it traps immediately (surfacing as a normal
/// `Result::Err`); a *different*, well-behaved call made through the SAME
/// already-exhausted instance will also fail from then on — recovering
/// requires loading a fresh instance (`WasmExtensionLoader::load` again).
/// This was chosen for simplicity over a "refill before every call" policy;
/// a real long-lived, high-call-volume extension exhausting its lifetime
/// budget under legitimate use (not a runaway loop) is a named, plausible
/// limitation of this wave, not something hit by any current fixture —
/// revisit with a per-call refill policy if a real extension needs it.
///
/// Defaults were checked empirically against this crate's own `wasm-hello`
/// fixture (see `tests/wasm_extension_test.rs`): large enough that every
/// well-behaved tool call in that fixture succeeds comfortably, small
/// enough that the fixture's own deliberately-broken `burn_fuel` guest tool
/// (a genuine infinite loop) traps in well under a second rather than
/// hanging the test suite.
#[derive(Debug, Clone, Copy)]
pub struct WasmExtensionLimits {
    /// Fuel units available to the `Store` for its entire loaded lifetime.
    pub fuel: u64,
    /// Maximum linear memory size, in bytes, any guest memory may grow to.
    /// Growing past this traps (`trap_on_grow_failure(true)`) rather than
    /// silently returning `-1` to the guest, so a guest that never checks
    /// `memory.grow`'s return value still can't limp along with a failed
    /// allocation — the call fails cleanly at the host boundary instead.
    pub max_memory_bytes: usize,
}

impl Default for WasmExtensionLimits {
    fn default() -> Self {
        Self {
            fuel: 200_000_000,
            max_memory_bytes: 16 * 1024 * 1024, // 16 MiB
        }
    }
}

/// Wasmtime `Store` data — the one thing a loaded extension's `pi_host_call`
/// door can reach into, plus the Wave 3 resource limiter. Wave 2 dispatches
/// the full six `ExtensionRuntime` actions; `ExtensionContext`'s read-only
/// accessors travel as a host-computed JSON snapshot instead of a live door
/// (see `loader.rs`'s doc comment), and `abort`/`shutdown` remain deferred.
pub(crate) struct HostState {
    runtime: std::sync::Arc<crate::runtime::ExtensionRuntime>,
    limits: wasmtime::StoreLimits,
}
