//! `pirust-coding-agent` — the headless bootstrap behind the `pirust` binary.
//!
//! Port of `packages/coding-agent` (spec: `docs/analysis/09-cli-config-spec.md`;
//! survey: `docs/analysis/03-coding-agent.md`).
//!
//! Exposed as a library as well as a binary so the golden suites can drive the pure
//! layers — [`args`], [`settings`], [`migrations`], [`print_mode`] — directly, without
//! spawning a process.
//!
//! # Scope (feat-005)
//!
//! IN: arg parsing + `--help`, config paths + app identity, the 5 startup migrations,
//! layered settings, `auth.json`, `models.json` resolution (**anthropic api only** — the
//! only adapter ported so far), the session manager, sdk wiring to
//! [`pirust_agent_core`]'s `Agent`, the system prompt, and print/json run modes.
//!
//! OUT: RPC mode (feat-012), the interactive TUI (feat-006/007), extensions and the
//! resource loader (stubbed — feat-007), and the package-manager verbs, HTML export,
//! telemetry and self-update paths (not ported).
//!
//! # On-disk naming
//!
//! pirust owns its own state directory — `~/.pirust/agent`, `PIRUST_CODING_AGENT_DIR`,
//! `PIRUST_CODING_AGENT_SESSION_DIR`, `PIRUST_OFFLINE` — while every file *format* stays
//! byte-compatible with Pi. The constants live in [`pirust_tools::binaries`] and are
//! re-exported by [`config`] rather than redeclared.

#![forbid(unsafe_code)]

pub mod args;
pub mod auth;
pub mod auth_guidance;
pub mod catalog;
pub mod config;
pub mod initial_message;
pub mod interactive_mode;
pub mod interactive_theme;
pub mod migrations;
pub mod models;
pub mod print_mode;
pub mod provider_attribution;
pub mod rpc;
pub mod runtime_host;
pub mod sdk;
pub mod session;
pub mod settings;
pub mod system_prompt;

/// Returns the crate name — linkage probe matching the sibling crates' `name()`.
pub fn name() -> &'static str {
    "pirust-coding-agent"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_is_stable() {
        assert_eq!(name(), "pirust-coding-agent");
    }
}
