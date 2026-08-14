//! `pirust-agent-core` — agent loop, tool pipeline, sessions, harness.
//!
//! pirust port of `packages/agent` (`@earendil-works/pi-agent-core`). See
//! `docs/analysis/01-agent.md`. Runtime lands in feat-003.

#![forbid(unsafe_code)]

// Module tree per docs/analysis/07-agent-core-spec.md §13. Currently documented
// skeletons (feat-003 scaffolding); individual subagents fill each module.
pub mod agent;
pub mod agent_loop;
pub mod harness;
pub mod types;

/// Returns the crate name — placeholder until the runtime (feat-003) lands.
pub fn name() -> &'static str {
    "pirust-agent-core"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_is_stable() {
        assert_eq!(name(), "pirust-agent-core");
    }

    #[test]
    fn depends_on_pirust_ai() {
        assert_eq!(pirust_ai::name(), "pirust-ai");
    }
}
