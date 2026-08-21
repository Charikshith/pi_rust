//! v4 session model — port of `packages/agent/src/harness/session/` (0.84.2).
//!
//! The 0.84.2 mutation-log session model: [`state::SessionState`] replays
//! [`types::SessionMutation`]s, [`codec`] (next wave) serializes them, and the
//! storage/repo layers (next waves) persist them. Kept in a separate `v4` module
//! so the v3 tree model (`super::jsonl_storage` etc.) stays intact until the
//! port fully lands.

pub mod codec;
pub mod state;
pub mod types;
