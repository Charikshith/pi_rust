//! `pirust-ai` — unified multi-provider LLM API.
//!
//! pirust port of `packages/ai` (`@earendil-works/pi-ai`). See
//! `docs/analysis/02-ai.md` for the source map and `docs/analysis/00-overview.md`
//! §6 for the port order.

#![forbid(unsafe_code)]

pub mod jsnum;
pub mod types;

// feat-002 Anthropic Messages streaming runtime (docs/analysis/06-anthropic-runtime-spec.md
// §Rust-layout). Scaffolded here; module bodies are filled by follow-up subagents.
pub mod api;
pub mod auth;
pub mod http;
pub mod json_repair;
pub mod providers;
pub mod sse;
pub mod stream;

/// Returns the crate name. Retained as a smoke symbol for dependent crates' stubs.
pub fn name() -> &'static str {
    "pirust-ai"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_is_stable() {
        assert_eq!(name(), "pirust-ai");
    }
}
