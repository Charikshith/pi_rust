//! Port of `core/sdk.ts` — assembles a [`pirust_agent_core`] `Agent` from resolved
//! settings, model, tools and session, plus the system prompt.
//!
//! Extension-runner hooks (`transformContext`, `onPayload`, `onResponse`,
//! `transformHeaders`) are STUBBED here — extensions land in feat-007.
