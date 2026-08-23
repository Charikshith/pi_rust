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
//! Wave 2 adds the command dispatch loop itself:
//! - [`host`] — [`host::RpcRuntimeHost`], an `AgentHarness` plus the RPC-only
//!   state Pi tracks at the `AgentSession` level
//! - [`mode`] — [`mode::handle_command`], the dispatch switch
//!
//! Wave 3 adds the process loop that drives Wave 2 over real stdin/stdout:
//! - [`run`] — [`run::run_rpc_mode`], the stdin-JSONL / stdout-JSONL loop,
//!   event streaming, and shutdown semantics

pub mod host;
pub mod jsonl;
pub mod mode;
pub mod run;
pub mod types;
