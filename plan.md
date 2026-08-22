# feat-008 Wave 8 — openai-responses adapter family (responses + azure + codex)

> STATUS: DONE — openai-responses + azure-openai-responses landed, oracle-verified (13
> records). codex (websocket+zstd) stays deferred.

## Success criterion
(unchanged) The `openai-responses` API adapter streams end-to-end and its conversion
layer is byte-identical to real Pi, oracle-pinned.

## Steps (all DONE for responses + azure; codex deferred)
1. `ToolCall.namespace` field + 9 construction sites → DONE.
2. `openai_responses_shared.rs` (conversion + tool conversion + stream state machine +
   `split_deferred_tools`) → DONE.
3. Oracle `gen-openai-responses-oracle.mjs` (13 records) → DONE.
4. `openai_responses_golden.rs` replays the oracle byte-identically → DONE.
5. `openai_responses.rs` adapter → DONE (stream golden replayed through the adapter).
6. `azure_openai_responses.rs` + `api/mod.rs` + `sdk.rs` routing → DONE.
7. Gate → DONE (clippy -D warnings clean, fmt clean, workspace 539 passed, oracle --check
   green; only the 3 pre-existing pirust-tools find failures).

## Bonus fix this wave (oracle-forced)
`transformMessages` now drops `textSignature` on cross-model text blocks (TS
`transform-messages.ts:120-123`) — the Rust port previously kept it. Pinned by a new
oracle case.

## Definition of done
- [x] `convertResponsesMessages`/`convertResponsesTools` byte-identical to Pi (oracle).
- [x] `processResponsesStream` event tape + final message byte-identical (oracle).
- [x] `sdk.rs` routes `openai-responses` and `azure-openai-responses`.
- [x] Gate green; deferred adapters named not silent.

## Deferred (named, next waves)
- openai-codex-responses (websocket + zstd), google-generative-ai, google-vertex,
  bedrock-converse-stream (SigV4), mistral-conversations, pi-messages.
- OAuth flows.
