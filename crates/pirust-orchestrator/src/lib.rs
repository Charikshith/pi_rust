//! `pirust-orchestrator` — pirust port of `@earendil-works/pi-server` +
//! `@earendil-works/pi-protocol` (`pi_space/pi/packages/{server,protocol}`,
//! formerly `packages/orchestrator` — see `docs/analysis/04-orchestrator.md`
//! for the 2026-08-23 scope-correction rewrite).

pub mod connection;
pub mod errors;
pub mod listener;
pub mod protocol;
pub mod server;
pub mod sessions;
pub mod snapshots;
/// Test-only reference doubles (`TestServerService`/`TestSessionRuntime`) —
/// a normal, unconditionally-compiled `pub` module (not `#[cfg(test)]`) so
/// both this crate's unit tests and its `tests/` integration binaries can
/// use it, matching `pirust-ai`'s existing `Faux` provider convention.
pub mod testing;
pub mod types;
