# feat-008 Wave 6 — openai-completions stream event-generator core

> STATUS: DONE — all steps implemented (oracle-captured + byte-verified). This
> plan is retained as the working record; details in feature_list.json feat-008
> evidence. Next wave (feat-008): transport/retry/error-normalization +
> onPayload/onResponse hooks, then sdk.rs routing, then remaining adapters.

## Success criterion
A canned OpenAI chat-completions SSE byte stream (recorded from real Pi) replayed
through the ported `stream` event generator produces the SAME `AssistantMessageEvent`
sequence + final `AssistantMessage` as real Pi — byte-identical where determinism
allows (timestamp/ids zeroed), verified via a golden harness like `anthropic_golden.rs`.

## Scope boundary
- In scope: the deterministic core of Pi's `stream` — OpenAI chunk→event state machine
  (`ensureTextBlock`/`ensureThinkingBlock`/`ensureToolCallBlock`/`finishBlock`,
  streaming tool-arg JSON accumulation via `partialArgs`, reasoning-content fields,
  `reasoning_details` → `thoughtSignature`, chunk usage, finish_reason →
  stop-reason + error-message, no-finish-reason fallback rules), plus
  `streamSimple`'s option mapping (`buildBaseOptions` + `clampThinkingLevel`) where
  the helpers are small.
- Out of scope (named, next waves): live transport (`createClient`/OpenAI client,
  `retryProviderRequest`, `onResponse`/`onPayload` hooks, `response.status`),
  `normalizeProviderError`/`formatProviderError`, `getClientApiKey` wiring into a
  transport, sdk.rs routing seam, remaining adapters, OAuth.

## Steps

1. Add missing `StreamOptions`/`OpenAICompletionsOptions` fields needed by the stream
   (signal/abort, on_payload/on_response as trait-object slots) → verify: crate compiles.
   DONE — `transport` added to `StreamOptions` (TS `StreamOptions.transport`, the
   injectable seam); `OpenAICompletionsOptions::with_transport` helper.
2. Port the OpenAI chunk→event state machine as a pure, testable `fn` over a
   `Vec<Value>` chunk sequence (TS `stream` `:204-618` minus transport) →
   verify: unit tests for text/thinking/tool-call/usage/finish-reason paths.
   DONE — `run_stream_state_machine` + `StreamingState` scratch model.
3. Wire it into a `stream()` that spawns the loop over `iterate_sse_messages`
   (reusing feat-002's SSE decoder) with a CannedTransport-equivalent OpenAI
   transport seam → verify: compile + event types match.
   DONE — `stream`/`stream_simple`/`produce`/`run_produce` + `ProviderStreams` impl.
4. Extend `scripts/gen-openai-completions-oracle.mjs` to capture a real Pi
   `streamSimple` run over a recorded SSE fixture (fake fetch returning canned
   chunks) → verify: oracle --check deterministic + idempotent.
   DONE — 3 `stream` records (text-stream, tool-call-stream, thinking-reasoning)
   with `sseBody` + event tape + final message; `--check` idempotent (22 records).
5. Add `tests/openai_completions_stream_golden.rs` replaying the recorded streams
   byte-identically vs the oracle capture → verify: golden green.
   DONE — 3/3 scenarios byte-verified (final message + tape type sequence + stable
   fields + terminal event).
6. Gate: fmt + clippy --all-targets -D warnings + workspace tests + oracle --check.
   DONE — fmt clean, clippy 0 warnings, workspace green except 3 pre-existing
   pirust-tools find.rs env failures (verified failing on clean stash), oracle
   --check green.

## Definition of done
- [x] State machine produces byte-identical events vs recorded Pi for ≥3 scenarios
      (text-only, tool-call streaming with partial JSON, thinking/reasoning).
- [x] `streamSimple` maps options correctly (buildBaseOptions/clampThinkingLevel).
- [x] Reuses existing SSE decoder + event/sink types (no new deps).
- [x] Gate green; deferred pieces named not silent.

## Bonus fixes landed in this wave (byte-compat bugs surfaced by the new fixtures)
- `Usage` field order: `reasoning` moved to Pi's canonical position (between
  `cacheWrite` and `totalTokens`) — pinned by the REAL openai parseChunkUsage
  output; the old order (reasoning last) was tuned to a hypothetical anthropic
  `message_delta` emission that no oracle ever exercises.
- `parse_chunk_usage` `reasoning` now `Some(0)` when reasoning_tokens is
  absent/0 (Pi's `|| 0`), not `None`-omitted.

## Deferred (named, next waves)
- `createClient`/OpenAI client construction (model.headers merge, copilot dynamic
  headers, session-affinity headers, forcePiUserAgent), `retryProviderRequest`,
  `onResponse`/`onPayload` hooks, `response.status`/headers plumbing,
  `normalizeProviderError`/`formatProviderError` — the transport layer.
- sdk.rs routing seam (build_stream_fn dispatch by api).
- Other adapters (openai-codex-responses, google-generative-ai, google-vertex,
  bedrock-converse-stream SigV4, mistral-conversations, pi-messages) + OAuth flows.
