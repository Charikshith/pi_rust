//! `pirust-orchestrator` — pirust port of `@earendil-works/pi-server` +
//! `@earendil-works/pi-protocol` (`pi_space/pi/packages/{server,protocol}`,
//! formerly `packages/orchestrator` — see `docs/analysis/04-orchestrator.md`
//! for the 2026-08-23 scope-correction rewrite).

/// Wave 6: the real `AgentHarness`-backed `PiServerService` — a pirust-side
/// addition, not a port (see the module's own doc comment).
pub mod agent_service;
pub mod connection;
pub mod errors;
pub mod listener;
pub mod protocol;
pub mod server;
pub mod sessions;
pub mod snapshots;
/// Test-only reference doubles (`TestServerService`/`TestSessionRuntime`,
/// `ProtocolTestClient`, the in-memory duplex transport double) — a normal,
/// unconditionally-compiled `pub` module (not `#[cfg(test)]`) so both this
/// crate's unit tests and its `tests/` integration binaries can use it,
/// matching `pirust-ai`'s existing `Faux` provider convention.
pub mod testing;
/// Concrete [`crate::listener::PiServerListener`] implementations. Wave 5:
/// `unix` (real Unix-domain-socket transport, port of `transports/unix/`).
pub mod transports;
pub mod types;
