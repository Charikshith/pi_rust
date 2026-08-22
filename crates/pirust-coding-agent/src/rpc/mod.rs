//! RPC mode protocol layer (feat-012) — 1:1 port of
//! `pi/packages/coding-agent/src/modes/rpc/`.
//!
//! Commands arrive as JSON lines on stdin; responses and session events are
//! emitted as JSON lines on stdout. This module currently carries the protocol
//! foundation (Wave 1):
//!
//! - [`jsonl`] — strict LF-only JSONL framing (`jsonl.ts`)
//! - [`types`] — the wire types (`rpc-types.ts`) with JS-canonical field order
//!
//! The command dispatch loop itself is Wave 2 (`rpc_mode.rs`).

pub mod jsonl;
pub mod types;
