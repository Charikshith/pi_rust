# feat-008 Wave 7 — openai-completions transport layer + sdk.rs routing

> STATUS: DONE — all steps implemented (gated green). Next wave (feat-008):
> remaining adapters (openai-codex-responses, google-generative-ai, google-vertex,
> bedrock-converse-stream SigV4, mistral-conversations, pi-messages) + OAuth flows.

## Success criterion
A non-Anthropic `openai-completions` model streams end-to-end: `sdk.rs` dispatches
on `model.api`, the transport POSTs to `{baseUrl}/chat/completions` with Pi's exact
headers (Authorization Bearer, model.headers, copilot/xai dynamic, session-affinity),
retries transient failures per `provider-retry.ts`, surfaces the HTTP status through
`onResponse`, and normalizes errors via `normalizeProviderError`/`formatProviderError`.
Pinned by unit tests + the 22-record oracle --check staying green.

## Scope boundary
- In scope: `normalizeProviderError`/`formatProviderError` (error-body.ts), `retryProviderRequest`
  (provider-retry.ts, incl. abortable backoff), `HttpResponse` status plumb, `createClient`
  headers (incl. `forcePiUserAgent`, `buildCopilotDynamicHeaders`, session-affinity),
  `onPayload`/`onResponse` hook slots, `ReqwestTransport` Authorization header, `sdk.rs`
  routing seam (`build_stream_fn` dispatch + provider cred lookup).
- Out of scope (named, next waves): other adapters, OAuth flows, `retry.ts` assistant-call
  retry classifier, transformHeaders extension hook.

## Steps
1. `http/mod.rs`: add `HttpResponse { status, headers }`, `TransportStatus` variant,
   `Authorization` header on `HttpRequest`, `with_bearer_auth`; ReqwestTransport fills them → DONE.
2. New `utils/error_body.rs` → DONE (4 unit tests).
3. New `utils/provider_retry.rs` → DONE (4 unit tests).
4. `api/mod.rs`: `ProviderPayloadCallback`/`ProviderResponseCallback` slots + `signal` on
   `StreamOptions` (drop `Debug` derive); `ProviderResponse { status, headers }` → DONE.
5. `openai_completions.rs`: header build, onPayload/onResponse, retry, error normalization → DONE.
6. `sdk.rs`: dispatch `build_stream_fn` on `model.api` → DONE.
7. Gate → DONE (clippy -D warnings clean, fmt clean, workspace builds, oracle --check green;
   only 3 pre-existing pirust-tools find.rs env failures).

## Definition of done
- [x] `formatProviderError(normalizeProviderError(err))` matches Pi on unit-tested shapes.
- [x] Retry policy matches provider-retry.ts (status set, x-should-retry, retry-after-ms/-after,
      exponential jitter, maxRetryDelayMs cap).
- [x] `sdk.rs` dispatches by api; a non-anthropic model builds an openai-completions stream.
- [x] Gate green; deferred pieces named not silent.

## Deferred (named, next waves)
- Other adapters: openai-codex-responses, google-generative-ai, google-vertex,
  bedrock-converse-stream (SigV4), mistral-conversations, pi-messages.
- OAuth flows (github-copilot token refresh etc.).
- `utils/retry.ts` assistant-call retry classifier (retryAssistantCall).
- Extension `transformHeaders` hook (emitBeforeProviderHeaders).
