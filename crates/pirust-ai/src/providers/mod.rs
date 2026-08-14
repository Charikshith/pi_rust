//! Provider implementations — Rust port of `packages/ai/src/providers/*`.
//!
//! See `docs/analysis/06-anthropic-runtime-spec.md` §6. Currently only the `faux` provider is
//! in scope (offline test support); real-provider metadata/catalog wiring lands later.
//!
//! The `faux` provider is implemented; real-provider metadata/catalog wiring lands later.

pub mod faux;

pub use faux::{
    faux_assistant_message, faux_text, faux_text_message, faux_thinking, faux_tool_call, Faux,
    FauxMessageOptions, FauxResponseFactory, FauxResponseStep,
};
