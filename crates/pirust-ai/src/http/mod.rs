//! HTTP transport boundary — Rust port of the Anthropic SDK request path used by
//! `packages/ai/src/api/anthropic-messages.ts` (`createClient` `:832-918`, request at `:555`).
//!
//! See `docs/analysis/06-anthropic-runtime-spec.md` §4a and §Rust-layout. The production
//! transport POSTs to `{baseUrl}/v1/messages` with `anthropic-version: 2023-06-01` and the
//! appropriate auth header, then exposes `bytes_stream()`. The [`AnthropicTransport`] trait
//! is the injectable seam that lets tests supply a canned SSE body — the Rust equivalent of
//! Pi's `options.client` / `.messages.create().asResponse()` oracle hook (spec §Oracle).
//!
//! Scaffolding only: request assembly and both transport impls are stubs.
// TODO(feat-002 http): implemented by subagent

use std::future::Future;
use std::pin::Pin;

use bytes::Bytes;
use futures::{Stream, StreamExt};

/// A streaming body of response bytes (mirrors `reqwest::Response::bytes_stream()`).
pub type ByteStream = Pin<Box<dyn Stream<Item = Result<Bytes, TransportError>> + Send>>;

/// Transport-level failures.
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    /// Non-2xx HTTP status; carries the status code and any body text.
    #[error("HTTP {status}: {body}")]
    Status { status: u16, body: String },
    /// Network / client error.
    #[error("request failed: {0}")]
    Request(String),
    /// Missing response body.
    #[error("response had no body")]
    NoBody,
}

/// A prepared outbound request (method is always `POST` for the messages endpoint).
#[derive(Debug, Clone)]
pub struct HttpRequest {
    /// Absolute URL, e.g. `{baseUrl}/v1/messages`.
    pub url: String,
    /// Ordered header list (insertion order preserved to match SDK output).
    pub headers: Vec<(String, String)>,
    /// Serialized JSON request body.
    pub body: String,
}

/// The injectable HTTP seam (spec §Rust-layout). `send` issues the request and resolves a
/// streaming byte body. Desugared future (explicit `+ Send`) to avoid the
/// `async_fn_in_trait` auto-trait-bound lint.
pub trait AnthropicTransport: Send + Sync {
    /// Send `request` and resolve its response byte stream.
    fn send(
        &self,
        request: HttpRequest,
    ) -> impl Future<Output = Result<ByteStream, TransportError>> + Send;
}

/// Object-safe, type-erased view of an [`AnthropicTransport`].
///
/// [`AnthropicTransport::send`] returns `impl Future` (RPITIT), which is not object-safe, so a
/// bare `dyn AnthropicTransport` cannot be stored in [`crate::api::AnthropicOptions`]. This
/// blanket-implemented shim boxes the future so a `dyn DynTransport` can be held behind an
/// `Arc` (the Rust equivalent of Pi's injectable `options.client`, spec §Oracle). The
/// implementor is cloned into the returned future so it borrows nothing and is `'static`.
pub trait DynTransport: std::fmt::Debug + Send + Sync {
    /// Boxed-future form of [`AnthropicTransport::send`].
    fn send_dyn(
        &self,
        request: HttpRequest,
    ) -> Pin<Box<dyn Future<Output = Result<ByteStream, TransportError>> + Send>>;
}

impl<T> DynTransport for T
where
    T: AnthropicTransport + Clone + std::fmt::Debug + 'static,
{
    fn send_dyn(
        &self,
        request: HttpRequest,
    ) -> Pin<Box<dyn Future<Output = Result<ByteStream, TransportError>> + Send>> {
        let this = self.clone();
        Box::pin(async move { this.send(request).await })
    }
}

/// Production transport backed by `reqwest` over rustls (spec §4a). POSTs `request` and, on a
/// 2xx status, exposes the response `bytes_stream()`.
#[derive(Debug, Clone)]
pub struct ReqwestTransport {
    client: reqwest::Client,
}

impl Default for ReqwestTransport {
    fn default() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }
}

impl ReqwestTransport {
    /// Construct a transport with default client configuration.
    pub fn new() -> Self {
        Self::default()
    }
}

impl AnthropicTransport for ReqwestTransport {
    fn send(
        &self,
        request: HttpRequest,
    ) -> impl Future<Output = Result<ByteStream, TransportError>> + Send {
        let client = self.client.clone();
        async move {
            let mut builder = client.post(&request.url).body(request.body);
            for (name, value) in &request.headers {
                builder = builder.header(name, value);
            }
            let response = builder
                .send()
                .await
                .map_err(|error| TransportError::Request(error.to_string()))?;
            let status = response.status();
            if !status.is_success() {
                let body = response.text().await.unwrap_or_default();
                return Err(TransportError::Status {
                    status: status.as_u16(),
                    body,
                });
            }
            let stream = response
                .bytes_stream()
                .map(|chunk| chunk.map_err(|error| TransportError::Request(error.to_string())));
            Ok(Box::pin(stream) as ByteStream)
        }
    }
}

/// Test double: replays a canned SSE body regardless of the request — the Rust equivalent of
/// Pi's fake `Anthropic` client (`options.client` / `asResponse()`, spec §Oracle).
#[derive(Debug, Clone, Default)]
pub struct CannedTransport {
    /// The raw SSE body to stream back (as produced by the oracle's `sseResponse`).
    pub body: String,
}

impl CannedTransport {
    /// Construct a canned transport that always yields `body`.
    pub fn new(body: impl Into<String>) -> Self {
        Self { body: body.into() }
    }
}

impl AnthropicTransport for CannedTransport {
    fn send(
        &self,
        _request: HttpRequest,
    ) -> impl Future<Output = Result<ByteStream, TransportError>> + Send {
        let body = self.body.clone();
        async move {
            let bytes = Bytes::from(body.into_bytes());
            let stream = futures::stream::once(async move { Ok::<Bytes, TransportError>(bytes) });
            Ok(Box::pin(stream) as ByteStream)
        }
    }
}
