//! feat-009 Wave 6: a real, `AgentHarness`-backed [`crate::types::PiServerService`]
//! plus the [`crate::types::PiSessionRuntime`] it hands out — the piece that
//! lets `pirust-orchestrator` actually run agent turns instead of the
//! in-memory [`crate::testing::service`] reference double.
//!
//! **This whole module is a pirust-side addition, not a port** (plan.md's
//! and `feature_list.json`'s own framing for this wave): nothing in
//! `pi_space/pi` builds a `PiServerService` on top of an `AgentHarness` —
//! real Pi's orchestrator package was redesigned away from process-spawning
//! before this crate's own `PiServer`/`PiSessionRuntime` traits were even
//! written (see `docs/analysis/04-orchestrator.md`). There is therefore no
//! construction site to check any of `conversions.rs`'s type mappings
//! against; every non-obvious call here is a documented, reasoned decision,
//! not a verified replay.

pub mod conversions;
pub mod runtime;
pub mod service;

pub use runtime::AgentPiSessionRuntime;
pub use service::{AgentServerService, HarnessBuilder};
