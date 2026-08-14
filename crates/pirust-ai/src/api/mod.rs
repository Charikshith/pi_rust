//! Provider API contract — Rust port of the `StreamOptions`/`ProviderStreams`/`StreamFunction`
//! surface deferred from feat-001 (TS `packages/ai/src/types.ts`), plus per-provider modules.
//!
//! See `docs/analysis/06-anthropic-runtime-spec.md` §6. Every `src/api/*` module IS a
//! `ProviderStreams` by exporting exactly `stream` and `streamSimple`. Per the contract, a
//! stream function must NOT throw after invocation — request/runtime failures are encoded as
//! an `error` event carrying `stopReason "error"|"aborted"` (spec §6 / §4e).
//!
//! Scaffolding only: option structs carry the data fields the adapter needs; behavioral
//! fields (signal/transport/callbacks) and stream bodies are added by the adapter subagent.
// TODO(feat-002 api): implemented by subagent

use std::collections::HashMap;
use std::sync::Arc;

use crate::http::{AnthropicTransport, DynTransport};
use crate::stream::AssistantMessageEventStream;
use crate::types::ids::{CacheRetention, ThinkingBudgets, ThinkingLevel};
use crate::types::message::Context;
use crate::types::model::Model;

pub mod anthropic_messages;

/// Request metadata (TS `StreamOptions.metadata`). Only `user_id` is consumed by the adapter
/// (`buildParams` `:1031-1036`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Metadata {
    /// Forwarded to Anthropic `metadata.user_id` when a string is present.
    pub user_id: Option<String>,
}

/// Common per-request options (TS `StreamOptions`, `types.ts:113-189`).
///
/// Data fields are populated here; the behavioral fields — `signal` (cancellation),
/// `transport` (injected [`crate::http::AnthropicTransport`]), and the `onPayload`/`onResponse`
/// callbacks — are trait objects/closures added by the adapter subagent (kept out of the
/// skeleton so it derives `Debug`/`Clone`/`Default`).
// TODO(feat-002 api): add `signal`, `transport`, `on_payload`, `on_response`.
#[derive(Debug, Clone, Default)]
pub struct StreamOptions {
    pub temperature: Option<f64>,
    pub max_tokens: Option<u64>,
    pub api_key: Option<String>,
    /// Extra request headers (ordered to preserve SDK/JS emission order).
    pub headers: Option<Vec<(String, String)>>,
    /// Explicit environment map consulted before the process env (spec §5).
    pub env: Option<HashMap<String, String>>,
    pub cache_retention: Option<CacheRetention>,
    pub session_id: Option<String>,
    pub timeout_ms: Option<u64>,
    pub websocket_connect_timeout_ms: Option<u64>,
    pub max_retries: Option<u32>,
    pub max_retry_delay_ms: Option<u64>,
    pub metadata: Option<Metadata>,
}

/// The higher-level "simple" options (TS `SimpleStreamOptions extends StreamOptions`,
/// `types.ts:295-299`): adds a coarse `reasoning` level and per-level thinking budgets.
#[derive(Debug, Clone, Default)]
pub struct SimpleStreamOptions {
    /// Shared base options.
    pub base: StreamOptions,
    pub reasoning: Option<ThinkingLevel>,
    pub thinking_budgets: Option<ThinkingBudgets>,
}

/// Anthropic-specific options (TS `AnthropicOptions extends StreamOptions`,
/// `anthropic-messages.ts:199-259`). The injected `transport` is the Rust equivalent of Pi's
/// `AnthropicOptions.client` (`:258`): when set, internal client/auth construction is skipped
/// entirely and the request is sent through the injected transport (the oracle seam, spec
/// §Oracle).
#[derive(Debug, Clone, Default)]
pub struct AnthropicOptions {
    /// Shared base options.
    pub base: StreamOptions,
    pub thinking_enabled: Option<bool>,
    pub thinking_budget_tokens: Option<u64>,
    /// Reasoning effort (adaptive models), e.g. `"high"`.
    pub effort: Option<String>,
    /// Thinking display mode; defaults to `"summarized"`.
    pub thinking_display: Option<String>,
    pub interleaved_thinking: Option<bool>,
    /// `tool_choice`: a string (→ `{type:string}`) or a passthrough object.
    pub tool_choice: Option<serde_json::Value>,
    /// Injected transport (TS `options.client`). When present the adapter skips auth/client
    /// construction and streams through it (test double = [`crate::http::CannedTransport`]).
    pub transport: Option<Arc<dyn DynTransport>>,
}

impl AnthropicOptions {
    /// Inject a transport (TS `{ ...options, client }`), returning the updated options. Used by
    /// the golden oracle to feed a [`crate::http::CannedTransport`] carrying a canned SSE body.
    #[must_use]
    pub fn with_transport(
        mut self,
        transport: impl AnthropicTransport + Clone + std::fmt::Debug + 'static,
    ) -> Self {
        self.transport = Some(Arc::new(transport) as Arc<dyn DynTransport>);
        self
    }
}

/// The provider streaming contract (TS `ProviderStreams`, `types.ts:227-230`). Every provider
/// exposes `stream` (full options) and `stream_simple` (coarse options).
pub trait ProviderStreams {
    /// Stream a completion with full per-request options.
    fn stream(
        &self,
        model: &Model,
        ctx: &Context,
        opts: Option<StreamOptions>,
    ) -> AssistantMessageEventStream;

    /// Stream a completion with the coarse "simple" options.
    fn stream_simple(
        &self,
        model: &Model,
        ctx: &Context,
        opts: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream;
}
