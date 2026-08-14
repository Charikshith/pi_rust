# Vendored Pi fixtures (test oracle)

These files are copied verbatim from the original Pi source repo and serve as
**ground-truth** for byte-compatibility tests — they were produced by Pi itself, not
authored by the port.

## Byte-identity oracles (persisted data — order matters)

| File | Origin / generator | Exercises | Bar |
|---|---|---|---|
| `assistant-message-with-thinking-code.json` (+ `.golden`) | `packages/coding-agent/test/fixtures/`; golden via `scripts/gen-golden.mjs` | one real `AssistantMessage` | byte-identical |
| `messages.corpus.jsonl` | `scripts/gen-message-corpus.mjs` (from Pi session JSONL) | 1901 real `Message`s (user/assistant/toolResult) | byte-identical |

These are *persisted* formats, so re-serialization must match Pi's `JSON.stringify`
**byte-for-byte** (`gen-golden.mjs` writes the JS-canonical form).

## Semantic oracles (non-persisted — key order not canonical in Pi)

| File | Generator | Exercises | Bar |
|---|---|---|---|
| `models.corpus.jsonl` | `scripts/gen-model-corpus.mjs` (from generated `providers/data/*.json`) | 1062 real `Model`s across ~35 providers | lossless semantic round-trip |
| `events.corpus.jsonl` | `scripts/gen-event-corpus.mjs` (Pi's faux provider) — **frozen capture** | all 12 `AssistantMessageEvent` variants | lossless semantic round-trip |

Models/events are never persisted per-session and Pi's own key order for them is not
canonical, so the Rust test checks order-independent `Value` equality (no field dropped,
no value changed), not byte-identity. `events.corpus.jsonl` is a frozen capture
(non-deterministic ids/timestamps) — refresh manually via its generator.

Do not hand-edit. Regenerate via the `scripts/gen-*.mjs` tools (require the sibling `pi`
repo with deps installed + model catalog generated).
