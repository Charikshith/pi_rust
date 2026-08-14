# feat-002 — Anthropic Messages Streaming Runtime: Port Spec

READ-ONLY analysis of Pi's Anthropic Messages streaming path. Target: a byte-for-byte
1:1 Rust port under `crates/pirust-ai/src`. All source citations are `path:line` into
`C:/Users/CharikshithPolimera/Downloads/PI_NEW/pi_space/pi`.

---

## Executive summary (~25 lines)

1. **Scope**: port the async event stream, lenient JSON repair, inline SSE decoder, the
   `anthropic-messages` state machine, api-key auth, and the `faux` provider — plus the
   `StreamFunction`/`StreamOptions`/`ProviderStreams` contract deferred in feat-001.
2. **Event stream** (`utils/event-stream.ts`) is a hand-rolled push/pull queue that is
   BOTH an async-iterable of events AND a promise of a final value. It completes on the
   first `done`/`error` event. Map to `tokio::sync::mpsc` (events) + `tokio::sync::oneshot`
   (final `AssistantMessage`). No real backpressure (unbounded queue).
3. **JSON repair** (`utils/json-parse.ts`) has an exact 3-stage fallback:
   `JSON.parse` → `repairJson`+parse → `partial-json` parse → `partial-json(repairJson)` →
   `{}`. `repairJson` is a precise character scanner (documented below). The `partial-json`
   dep (v0.1.7, `Allow.ALL`) tolerates truncated JSON.
4. **SSE decoder is INLINE** in `api/anthropic-messages.ts:292-482`, not a separate file.
   Line-based, CR/LF/CRLF aware, multi-line `data:` joined with `\n`, blank line flushes,
   `[DONE]`/comments/unknown events ignored, no reliance on `[DONE]`.
5. **anthropic-messages `stream()`** builds an `AssistantMessage` incrementally, driving it
   through Anthropic raw stream events and emitting `AssistantMessageEvent`s in a fixed
   order (documented per-event below). Tool args accumulate in a scratch `partialJson`
   deleted on completion.
6. **CRITICAL byte-order finding**: the final message's JSON key order is *runtime
   insertion order*, NOT the feat-001 struct field order. `responseId` is inserted AFTER
   `timestamp`; `usage.cacheWrite1h` and `usage.reasoning` are inserted AFTER `usage.cost`.
   The feat-001 `AssistantMessage`/`Usage` structs will NOT reproduce this — see §4e.
7. **Cost** = `models.ts:calculateCost`; the 1h-cache split (`cacheWrite1h`) is billed at
   `2×input`, the rest at `cacheWrite` rate. `reasoning` comes from
   `usage.output_tokens_details.thinking_tokens` on `message_delta`.
8. **Auth** (api-key path): env `ANTHROPIC_OAUTH_TOKEN` then `ANTHROPIC_API_KEY`; header
   `x-api-key: <key>` (or `authorization: Bearer` for oauth/copilot). Version header
   `anthropic-version: 2023-06-01`. Endpoint `POST {baseUrl}/v1/messages`.
9. **Oracle for golden fixtures**: tests inject a fake `Anthropic` client via
   `options.client` whose `messages.create().asResponse()` resolves a real `Response` built
   from an SSE string. This drives `stream()` fully offline. Skeleton in §Oracle.
10. Raw SSE is stored as inline TS string fixtures inside the `.test.ts` files; there are
    NO committed `.sse`/`.txt` byte fixtures.
11. Proposed Rust layout + crates listed in §Rust-layout.

---

## 1. Event stream — `packages/ai/src/utils/event-stream.ts`

### Contract (generic `EventStream<T, R>`, `event-stream.ts:4-67`)
- **State**: `queue: T[]` (buffered events), `waiting: resolver[]` (parked consumers),
  `done: bool`, `finalResultPromise: Promise<R>` with captured `resolveFinalResult`,
  plus injected predicates `isComplete(event): bool` and `extractResult(event): R`.
- **`push(event)`** (`:21-36`):
  1. If `done`, drop the event silently (`if (this.done) return`).
  2. If `isComplete(event)`: set `done=true` and resolve the final-result promise with
     `extractResult(event)`. NOTE: it resolves BUT still delivers this same event to the
     iterator below — the terminal event IS yielded to consumers.
  3. Deliver: if a consumer is parked (`waiting.shift()`), resolve it with
     `{value: event, done:false}`; else push to `queue`.
- **`end(result?)`** (`:38-48`): set `done=true`; if `result!==undefined` resolve final
  promise (used by faux to end with an explicit message); then drain `waiting`, resolving
  each with `{value:undefined, done:true}`. Queued-but-unconsumed events remain in `queue`
  and are still drainable by the async iterator (see below).
- **`[Symbol.asyncIterator]`** (`:50-62`): loop — if `queue` non-empty, yield
  `queue.shift()`; else if `done`, `return`; else park a resolver in `waiting` and await;
  on wake, if `{done:true}` return, else yield the value. IMPORTANT: queue is drained
  BEFORE the `done` check, so all buffered events are delivered even after `end()`.
- **`result()`** (`:64-66`): returns `finalResultPromise`.

### `AssistantMessageEventStream` (`event-stream.ts:69-83`)
- `isComplete = (e) => e.type === "done" || e.type === "error"`.
- `extractResult = (e) => e.type==="done" ? e.message : e.type==="error" ? e.error : throw`.
- So `result()` resolves with the final `AssistantMessage` on BOTH success and error (the
  error message carries `stopReason: "error"|"aborted"` and `errorMessage`). `result()`
  never rejects on a stream-level error — the error is a *value*. It only rejects if
  neither `done` nor `error` is ever pushed (promise stays pending forever → hang).

### Ordering / backpressure / termination
- FIFO, single-logical-producer (the async IIFE in `stream()`), any number of consumers,
  but each event goes to exactly ONE consumer (shift semantics) — this is a queue, NOT a
  broadcast. In practice there is one consumer (`for await`) plus one `result()` awaiter.
- No backpressure: `queue` is unbounded; `push` never blocks.
- Terminal events (`done`/`error`) both resolve `result()` and are yielded; after that
  `done=true` and further `push` is a no-op.

### Rust mapping
- `AssistantMessageEventStream` = a struct holding `rx: mpsc::UnboundedReceiver<AssistantMessageEvent>`
  and `result_rx: oneshot::Receiver<AssistantMessage>`.
- Producer holds `tx: mpsc::UnboundedSender` + `Option<oneshot::Sender<AssistantMessage>>`.
- `push(ev)`: on `done`/`error`, `result_tx.take().send(final.clone())`; then `tx.send(ev)`.
  (Clone the message into the oneshot; the same value is also carried in the event.)
- Implement `futures::Stream<Item=AssistantMessageEvent>` over the `mpsc` receiver.
- `async fn result(self) -> AssistantMessage` awaits the oneshot; if the sender dropped
  without sending (producer panicked/early-return), surface an `error` AssistantMessage
  rather than hanging — Pi hangs here, but the port should not.
- Use `tokio::spawn` for the producer IIFE. Because the mpsc is unbounded, the "drop
  events after done / drain-before-done" semantics come for free (send after receiver
  close is a no-op; buffered items drain before `None`).

---

## 2. Lenient JSON — `packages/ai/src/utils/json-parse.ts`

### `repairJson(json)` (`json-parse.ts:32-83`) — exact rules
Single left-to-right scan, `inString` flag starts false, output string `repaired`:
- **Outside a string**: copy char verbatim; if char is `"`, enter string mode. (`:39-45`)
- **Inside a string**:
  - `"` → copy, leave string mode. (`:47-51`)
  - `\` (backslash) (`:53-77`):
    - next char undefined (trailing backslash at EOF) → emit `\\` (doubled). (`:55-58`)
    - next char `u` AND next 4 chars match `/^[0-9a-fA-F]{4}$/` → emit `\u` + those 4 hex,
      advance index by 5. (`:60-67`)
    - next char is a valid escape (`" \ / b f n r t u`, set at `:3`) → emit `\` + next,
      advance index by 1. (`:69-73`)
    - otherwise (invalid escape, e.g. `\H`, `\x`) → emit `\\` (double the lone backslash),
      do NOT consume next char. (`:75`)
  - any raw control char (codepoint `0x00..=0x1f`, test at `:5-8`) → escape it: `\b \f \n
    \r \t` for those five, else `\uXXXX` zero-padded to 4 lower-hex. (`:10-25`, `:79`)
  - any other char → copy verbatim. (`:79`)

Worked example from the SSE test (`test/anthropic-sse-parsing.test.ts:98`): partial_json
`{"path":"A\H",...,"text":"col1<TAB>col2"}` — `\H` is an invalid escape so becomes `\\H`
→ parses to `path: "A\\H"` (i.e. the 2-char string `A\H`), and the raw tab becomes `\t`
→ `text: "col1\tcol2"`. Asserted at `:163-166`.

### `parseJsonWithRepair<T>(json)` (`:85-95`)
1. `try JSON.parse(json)`.
2. On throw: `r = repairJson(json)`; if `r !== json` return `JSON.parse(r)`; else rethrow.

### `parseStreamingJson<T>(partialJson)` (`:104-124`)
1. Falsy or whitespace-only → `{}`. (`:105-107`)
2. `try parseJsonWithRepair(partialJson)`. (`:110`)
3. On throw: `try partialParse(partialJson) ?? {}`. (`:112-114`)
4. On throw: `try partialParse(repairJson(partialJson)) ?? {}`. (`:116-118`)
5. On throw: `{}`. (`:119-121`)

Used at `anthropic-messages.ts:648` (per-delta live view) and `:684` (finalize on stop).

### `partial-json` dep (node_modules/partial-json v0.1.7, `Allow.ALL`)
Recursive descent that tolerates truncation by returning the partial structure at the
point input runs out (behavior confirmed from `dist/index.js`):
- Objects: return the object accumulated so far if the value/`}` is missing.
- Arrays: return elements so far if `]` missing.
- Strings: if unterminated, close them (append `"`); on invalid trailing escape, cut back
  to the last `\`.
- Bare `null/true/false/Infinity/-Infinity/NaN` prefixes are completed.
- Numbers: parse the longest numeric prefix; drop a dangling exponent.

### Rust mapping (`json_repair` module)
- Port `repair_json` as a byte/char scanner — trivial, deterministic, no deps.
- `parse_json_with_repair` and `parse_streaming_json` over `serde_json::Value`.
- **`partial-json` has no drop-in Rust crate**; hand-roll a small tolerant parser matching
  v0.1.7 `Allow.ALL` semantics (only the object/array/string/number/keyword-prefix cases
  above are reachable). In practice tool-call args are COMPLETE by `content_block_stop`, so
  stage 3/4 rarely fire for finalized calls — but the *live* per-delta view (`:648`) does
  exercise them, and the `partial` snapshots ride along in every emitted event, so a faithful
  port is required for event-level golden matching (not just the final message).
- Preserve object key insertion order → `serde_json` `preserve_order` (already enabled).

---

## 3. SSE decoder — INLINE in `api/anthropic-messages.ts` (no separate file)

Search hits for `text/event-stream`/`data:` land here; there is no `utils/*sse*`.

### Types
- `ServerSentEvent { event: string|null; data: string; raw: string[] }` (`:292-296`).
- `SseDecoderState { event: string|null; data: string[]; raw: string[] }` (`:298-302`).

### Line decoding — `decodeSseLine(line, state)` (`:329-353`)
- Empty line `""` → `flushSseEvent(state)`. (`:330-332`)
- Push `line` to `state.raw`. (`:334`)
- Line starting with `:` → comment, ignored (returns null). (`:335-337`)
- Split on FIRST `:`; `fieldName` = before, `value` = after (or `""` if no colon). If
  `value` starts with a single space, strip exactly one. (`:339-344`)
- `event` field → `state.event = value`. `data` field → `state.data.push(value)`. Any
  other field name ignored. (`:346-352`)

### Flush — `flushSseEvent(state)` (`:313-327`)
- If no `event` AND empty `data` → null (blank runs collapse). (`:314-316`)
- Else emit `{ event, data: data.join("\n"), raw: [...raw] }` and reset state. Multi-line
  `data:` fields are joined with `\n`. (`:318-326`)

### Line splitting across chunks — `iterateSseMessages(body, signal)` (`:384-441`)
- `TextDecoder` streaming decode; accumulate into `buffer`. (`:389-404`)
- `nextLineBreakIndex` finds min of first `\r` / `\n` (`:355-365`); `consumeLine`
  splits one line, treating `\r\n` as a single break (`:367-382`).
- Loop: on each chunk, consume all complete lines, feeding each to `decodeSseLine`, yield
  any event produced. (`:405-413`)
- After `done`: `decoder.decode()` final flush, drain remaining complete lines (`:416-425`),
  then if a partial `buffer` remains decode it as a final line (`:427-432`), then a trailing
  `flushSseEvent` for any event with no terminating blank line (`:434-437`).
- `signal?.aborted` → throw `"Request was aborted"`. (`:395-397`)
- `finally { reader.releaseLock() }`. (`:438-440`)

### Anthropic layer — `iterateAnthropicEvents(response, signal)` (`:443-482`)
- No `response.body` → throw. (`:447-449`)
- For each SSE: `event === "error"` → `throw new Error(sse.data)`. (`:455-457`)
- If `sse.event` NOT in `ANTHROPIC_MESSAGE_EVENTS` (the 6 canonical types, set at
  `:304-311`) → `continue` (skips `ping`, `[DONE]`, `proxy.stats`, unknown). (`:459-461`)
- Else `parseJsonWithRepair<RawMessageStreamEvent>(sse.data)`; track `sawMessageStart`
  (`message_start`) / `sawMessageEnd` (`message_stop`); `yield event`. (`:463-470`)
- Parse failure → throw with a diagnostic incl. `sse.event`, message, `data`, joined
  `raw`. (`:471-476`)
- After the loop: if `sawMessageStart && !sawMessageEnd` → throw `"Anthropic stream ended
  before message_stop"`. (`:479-481`)

### Rust mapping (`sse` module)
- A `SseDecoderState` + `decode_line`/`flush` mirroring the above exactly (byte-for-byte
  behavior, including the single-leading-space strip and `\r\n` handling).
- `iterate_sse_messages`: an `async` adapter over `reqwest::Response::bytes_stream()` +
  `tokio_util::codec` or a manual `String` buffer with a UTF-8 incremental decoder. A
  hand-rolled buffer matches Pi's `TextDecoder{stream:true}` most precisely. `eventsource-stream`
  does NOT match Pi's framing (it drops comments differently and uses `id:`/`retry:`); prefer
  hand-rolled.
- `ANTHROPIC_MESSAGE_EVENTS` as a `const [&str; 6]`/`phf`/match.

---

## 4. Core — `api/anthropic-messages.ts`

### (a) HTTP request construction
Pi uses the `@anthropic-ai/sdk` client; the port replicates its wire output with `reqwest`.
- **Endpoint**: `POST {model.baseUrl}/v1/messages` (SDK default path; `baseURL` from
  `model.baseUrl`, `createClient` `:832-918`). Body includes `stream: true` (`:555`,
  `buildParams` sets `stream:true` `:953`).
- **Version header**: `anthropic-version: 2023-06-01` (SDK default, confirmed
  `node_modules/@anthropic-ai/sdk/client.js:468`).
- **Auth header** (three modes in `createClient`):
  - github-copilot (`:852-871`): `authToken` → `Authorization: Bearer <key>`, `apiKey:null`.
    Headers: `accept: application/json`, `anthropic-dangerous-direct-browser-access: true`,
    `anthropic-beta` (if any), merged `model.headers` + dynamic copilot headers + option headers.
  - OAuth token (`apiKey` contains `sk-ant-oat`, `isOAuthToken` `:828-830`, branch `:874-894`):
    `Authorization: Bearer <token>`; `anthropic-beta: claude-code-20250219,oauth-2025-04-20`
    + beta features; `user-agent: claude-cli/2.1.75` (`claudeCodeVersion` `:74`); `x-app: cli`.
  - API-key / header-owned (`:896-917`): `X-Api-Key: <key>` (SDK `client.js:127`); optional
    `x-session-affinity: <sessionId>` when `sendSessionAffinityHeaders` compat is on (`:897-898`).
- **Beta features list** (`:843-849`): push `fine-grained-tool-streaming-2025-05-14` when
  `shouldUseFineGrainedToolStreamingBeta` (tools present AND `!supportsEagerToolInputStreaming`,
  `:1256-1258`); push `interleaved-thinking-2025-05-14` when `interleavedThinking` AND
  model is not `forceAdaptiveThinking` (`:842`). Joined with `,` into `anthropic-beta`.
- **Auth assertion** `assertRequestAuth` (`:280-290`): if no `apiKey` and no
  `authorization`/`x-api-key`/`cf-aig-authorization` header present → throw
  `No API key for provider: {provider}`. (This throw happens synchronously in `streamSimple`
  `:791`, but inside the async IIFE for `stream` `:519`, so it surfaces as an `error` event.)

- **Body shape** (`buildParams` `:920-1047`, `MessageCreateParamsStreaming`):
  - `model: model.id`; `messages: convertMessages(...)`; `max_tokens: options.maxTokens ??
    model.maxTokens`; `stream: true`. (`:942-954`)
  - `system`: for OAuth, array `[{type:text,text:"You are Claude Code, Anthropic's official
    CLI for Claude."}, (systemPrompt?)]` each with optional `cache_control` (`:957-971`);
    for non-OAuth, `[{type:text,text:systemPrompt, cache_control?}]` when a systemPrompt
    exists (`:972-981`).
  - `temperature`: only if `options.temperature!==undefined && !thinkingEnabled &&
    supportsTemperature` (`:983-986`).
  - `tools`: `convertTools(immediate) ++ convertTools(deferred, deferLoading=true)` when any
    tools; each tool → `{name, description, eager_input_streaming?(when supported),
    input_schema:{type:"object",properties,required}, defer_loading?, cache_control?(last
    immediate tool only)}` (`:988-998`, `convertTools` `:1260-1285`). OAuth renames tool
    names to Claude Code canonical casing (`toClaudeCodeName`, `:99-102`).
  - `thinking`: gated on `model.reasoning` (`:1000-1029`). Adaptive models →
    `{type:"adaptive",display}` plus `output_config:{effort}` when `effort` set; older models
    → `{type:"enabled",budget_tokens:thinkingBudgetTokens||1024,display}`; explicitly
    disabled → `{type:"disabled"}` (unless `thinkingLevelMap.off === null`). `display`
    defaults `"summarized"`.
  - `metadata: {user_id}` only if `options.metadata.user_id` is a string (`:1031-1036`).
  - `tool_choice`: string → `{type:string}`; object → passed through (`:1038-1044`).
  - `onPayload` hook may replace the whole params object (`:546-549`).
- Message conversion `convertMessages` (`:1089-1254`), `convertToolResult` (`:1054-1087`),
  `convertContentBlocks` (`:115-162`), `normalizeToolCallId` (`:1050-1052`, `[^a-zA-Z0-9_-]`→`_`,
  slice 64), `transformMessages` + `splitDeferredTools` (external), `sanitizeSurrogates`
  on all text. cache_control is attached to the last user message's last block (`:1229-1251`).

### (b) SSE event state machine → emitted `AssistantMessageEvent`s (exact order)
Loop body `:562-733`. `blocks = output.content` viewed as `Block` with a transient `index`
field mirroring the Anthropic content-block index (`:559-560`).

Pre-loop: after the response resolves, push **`{type:"start", partial:output}`** (`:557`).

- **`message_start`** (`:563-575`): set `output.responseId = message.id`; set
  `usage.input/output/cacheRead/cacheWrite` from `message.usage.*_tokens`; set
  `usage.cacheWrite1h = message.usage.cache_creation?.ephemeral_1h_input_tokens || 0`;
  recompute `totalTokens = input+output+cacheRead+cacheWrite`; `calculateCost`. Emits NO
  event.
- **`content_block_start`** (`:576-617`) by `content_block.type`:
  - `text` → push block `{type:"text",text:"",index}`; emit **`text_start`** with
    `contentIndex = content.length-1`.
  - `thinking` → push `{type:"thinking",thinking:"",thinkingSignature:"",index}`; emit
    **`thinking_start`**.
  - `redacted_thinking` → push `{type:"thinking",thinking:"[Reasoning redacted]",
    thinkingSignature:<data>,redacted:true,index}`; emit **`thinking_start`**. (see §4f)
  - `tool_use` → push `{type:"toolCall",id,name(OAuth→fromClaudeCodeName),
    arguments:input??{},partialJson:"",index}`; emit **`toolcall_start`**.
- **`content_block_delta`** (`:618-663`), found by `blocks.findIndex(b=>b.index===event.index)`:
  - `text_delta` → `block.text += delta.text`; emit **`text_delta`** (`delta:delta.text`).
  - `thinking_delta` → `block.thinking += delta.thinking`; emit **`thinking_delta`**.
  - `input_json_delta` → `block.partialJson += delta.partial_json`;
    `block.arguments = parseStreamingJson(block.partialJson)`; emit **`toolcall_delta`**
    (`delta:delta.partial_json`).
  - `signature_delta` → `block.thinkingSignature += delta.signature`; emits NO event.
- **`content_block_stop`** (`:664-695`): `delete block.index`; then by type:
  - `text` → emit **`text_end`** (`content:block.text`).
  - `thinking` → emit **`thinking_end`** (`content:block.thinking`).
  - `toolCall` → `block.arguments = parseStreamingJson(block.partialJson)`;
    `delete block.partialJson`; emit **`toolcall_end`** (`toolCall:block`).
- **`message_delta`** (`:696-732`): map stop_reason (see §4g); set `output.stopReason` and
  optional `output.errorMessage`; update usage fields ONLY when present & non-null (`:706-727`)
  — `input/output/cacheRead/cacheWrite` from tokens, `reasoning` from
  `usage.output_tokens_details.thinking_tokens` (§4d). NOTE `cacheWrite1h` is NOT touched
  here (only set in `message_start`). Recompute `totalTokens`; `calculateCost`. Emits NO event.
- **`message_stop`**: no explicit branch; only flips `sawMessageEnd` in the SSE layer.
- **`ping`, unknown events**: filtered out in `iterateAnthropicEvents` (`:459`).

Post-loop (`:735-744`): if `signal.aborted` throw; if `stopReason==="aborted"|"error"`
throw `errorMessage`; else push **`{type:"done", reason:stopReason, message:output}`** then
`stream.end()`.

Catch (`:745-755`): for every content block `delete index` and `delete partialJson`; set
`stopReason = aborted?"aborted":"error"`, `errorMessage = err.message || JSON.stringify`;
push **`{type:"error", reason:stopReason, error:output}`**; `stream.end()`.

So the canonical event tape for a text+toolcall turn is:
`start, text_start, text_delta*, text_end, toolcall_start, toolcall_delta*, toolcall_end,
done` (indices assigned in arrival order).

### (c) `partialJson` accumulation / deletion
- Created `""` on `tool_use` `content_block_start` (`:612`).
- Appended each `input_json_delta` (`:647`), with a live `arguments` re-parse (`:648`).
- Re-parsed and then `delete`d on that block's `content_block_stop` (`:684-687`).
- Also defensively deleted for ALL blocks in the catch path (`:749`) — so an aborted
  mid-tool-call turn drops the scratch buffer too. (Contrast: feat-001's `ToolCall.partial_json`
  is retained for aborted sessions — that persistence is a DIFFERENT code path, NOT this
  adapter. This adapter never persists `partialJson`.)

### (d) Usage & cost (`calculateCost` in `models.ts:639-659`)
- `inputTokens = input + cacheRead + cacheWrite`; pick highest matching tier where
  `inputTokens > tier.inputTokensAbove` (`:640-648`).
- `longWrite = cacheWrite1h ?? 0`; `shortWrite = cacheWrite - longWrite`.
- `cost.input = rates.input/1e6 * input`; `cost.output = rates.output/1e6 * output`;
  `cost.cacheRead = rates.cacheRead/1e6 * cacheRead`;
  `cost.cacheWrite = (rates.cacheWrite*shortWrite + rates.input*2*longWrite)/1e6`;
  `cost.total = sum`. (`:650-657`)
- `reasoning`: read from `event.usage.output_tokens_details.thinking_tokens` via a narrow
  cast on `message_delta` (`:722-726`); it is a subset of `output`, set only when present.
- Verified by `test/anthropic-cache-write-1h-cost.test.ts`: `cacheWrite=1_000_000`,
  `cacheWrite1h=400_000` → `cost.cacheWrite = 600k*6.25/M + 400k*10/M = 7.75`; with no 1h
  breakdown → `6.25`.

### (e) EXACT construction & key-insertion order of the final `AssistantMessage` — CRITICAL
Initial literal (`stream()` `:492-508`), keys in this order:
`role, content, api, provider, model, usage, stopReason, timestamp`
where `usage` literal (`:498-505`) is:
`input, output, cacheRead, cacheWrite, totalTokens, cost` and `cost` is
`input, output, cacheRead, cacheWrite, total`.

Dynamic key insertions (JS objects preserve insertion order; new keys append):
- `output.responseId` inserted in `message_start` (`:564`) → appears AFTER `timestamp`.
- `output.usage.cacheWrite1h` inserted in `message_start` (`:571`) → appears AFTER `cost`.
- `output.usage.reasoning` inserted in `message_delta` (`:725`) when present → AFTER
  `cacheWrite1h`.
- `output.errorMessage` inserted in `message_delta` refusal (`:701`) or catch (`:752`) →
  AFTER `responseId`.

**Resulting `JSON.stringify(result)` top-level order**:
`{role, content, api, provider, model, usage, stopReason, timestamp, responseId,
  errorMessage?}`
and `usage` order:
`{input, output, cacheRead, cacheWrite, totalTokens, cost, cacheWrite1h, reasoning?}`.

**Divergence from feat-001 structs (MUST fix for byte-exact golden matching):**
- `types/message.rs:63-90` `AssistantMessage` declares `response_id` BEFORE `usage` (field
  6) — runtime emits `responseId` AFTER `timestamp` (field 9). MISMATCH.
- `types/usage.rs` `Usage` declares `cache_write1h` and `reasoning` BEFORE `total_tokens`
  and `cost` — runtime emits them AFTER `cost`. MISMATCH.
- `error_message` already declared last in `message.rs:85-89` — matches runtime. `response_model`,
  `diagnostics` are never set here and are `skip_serializing_if=None` → correctly absent.

The feat-001 note in `message.rs:85-87` reflects *persisted session* order, which is NOT
the raw adapter output. For the oracle golden fixtures (direct `stream().result()` +
`JSON.stringify`), the port's `anthropic_messages` module must emit the runtime order.
Two viable strategies:
  1. **Recommended**: build the final message as a `serde_json::Map`/`Value` (preserve_order
     is on) mutated in the SAME sequence as the TS adapter — a literal 1:1 port of the
     mutation code — and both send it through the event stream and serialize it directly.
  2. Introduce an adapter-local `#[serde]` DTO (or `#[serde(serialize_with)]` on a wrapper)
     whose field order matches runtime insertion order, distinct from the feat-001 struct.
Either way, add a golden test asserting byte-equality against the Node oracle (§Oracle).
Also mind number formatting: costs are `f64` → must use the feat-001 `jsnum::serialize_f64`
(JS number formatting) to match `JSON.stringify` of e.g. `7.75`, `6.25`, integer-valued
floats without `.0`, etc.

### (f) Redacted thinking
- Inbound stream `redacted_thinking` block (`:594-603`): stored as a `thinking` block with
  `thinking:"[Reasoning redacted]"`, `redacted:true`, and the opaque `data` in
  `thinkingSignature`; emits `thinking_start` (NOT a distinct event type). No delta/stop
  special-casing beyond the normal `thinking` path.
- Outbound (replay) `convertMessages` (`:1151-1159`): a `redacted:true` thinking block is
  re-emitted as `{type:"redacted_thinking", data: thinkingSignature}`.

### (g) Stop-reason mapping — `mapStopReason` (`:1287-1313`)
`end_turn→stop`; `max_tokens→length`; `tool_use→toolUse`;
`refusal→{error, errorMessage: stop_details.explanation || "The model refused to complete
the request"}`; `pause_turn→stop`; `stop_sequence→stop`; `sensitive→error`;
default → `throw new Error("Unhandled stop reason: {reason}")` (this throw is caught by the
IIFE catch → becomes an `error` message). Refusal detail preservation verified by
`test/anthropic-sse-parsing.test.ts:169-225`.

---

## 5. Auth (api-key, single provider) — `packages/ai/src/auth/*` + `env-api-keys.ts`

For the anthropic-messages path the only auth input actually consumed by the adapter is
`options.apiKey` (+ optional `options.headers`). The `auth/resolve.ts` machinery is the
higher-level `Models` collection resolver (credential store + oauth refresh, `resolve.ts:37-139`)
and is NOT needed for a minimal single-provider port.
- Env var precedence for anthropic (`env-api-keys.ts:getApiKeyEnvVars`): `ANTHROPIC_OAUTH_TOKEN`
  then `ANTHROPIC_API_KEY` (github-copilot: `COPILOT_GITHUB_TOKEN`). `compat.ts` injects
  this via `withEnvApiKey` (`compat.ts:222-230`) only when no explicit `apiKey` is set.
- `getProviderEnvValue` (`utils/provider-env.ts:45-52`): `env[name] || process.env[name] ||
  <bun-sandbox-fallback>`. Port: `options.env.get(name).or_else(|| std::env::var(name))`;
  drop the Bun `/proc/self/environ` fallback.
- Header selection (see §4a): OAuth token detected by substring `sk-ant-oat`
  (`isOAuthToken` `:828-830`) → Bearer; else `X-Api-Key`.
Minimal port: an `auth` module exposing `resolve_api_key(options, env) -> Option<String>`
with the 2-var precedence, plus the `is_oauth_token` predicate.

---

## 6. Faux provider & `ProviderStreams` contract

### Contract types (`types.ts`)
- `StreamOptions` (`:113-189`): `temperature?, maxTokens?, signal?, apiKey?, transport?,
  cacheRetention?, sessionId?, onPayload?, onResponse?, headers?, timeoutMs?,
  websocketConnectTimeoutMs?, maxRetries?, maxRetryDelayMs?, metadata?, env?`.
- `SimpleStreamOptions extends StreamOptions` (`:295-299`): `+ reasoning?: ThinkingLevel`,
  `thinkingBudgets?`.
- `StreamFunction<TApi,TOptions>` (`:309-313`): `(model, context, options?) =>
  AssistantMessageEventStream`. Contract (`:303-308`): must NOT throw post-invocation —
  encode request/runtime failures as an `error` event with `stopReason "error"|"aborted"`.
- `ProviderStreams` (`:227-230`): `{ stream(model,ctx,opts?), streamSimple(model,ctx,opts?)
  }` — every `src/api/*` module IS a `ProviderStreams` by exporting exactly `stream` and
  `streamSimple`. `AnthropicOptions` (`anthropic-messages.ts:199-259`) extends `StreamOptions`
  with anthropic-specific fields (`thinkingEnabled, thinkingBudgetTokens, effort,
  thinkingDisplay, interleavedThinking, toolChoice, client`).

### `faux.ts` behavior
- `createFauxCore(options)` (`:403-508`) returns `{api, provider, models, stream,
  streamSimple, getModel, state, setResponses, appendResponses, getPendingResponseCount}`.
- `stream` (`:442-476`): shift one queued `FauxResponseStep`, `state.callCount++`, then in
  a `queueMicrotask`: call `onResponse({status:200,headers:{}})`; if no step → error message
  `"No more faux responses queued"`; else resolve the step (or call the factory), clone +
  `withUsageEstimate` (char/4 token estimate, prefix-based cache split `:213-251`), then
  `streamWithDeltas`.
- `streamWithDeltas` (`:308-401`): emits `start`, then per content block the matching
  `*_start`/`*_delta`(chunked by `splitStringByTokenSize`)/`*_end`, honoring `signal`
  aborts (emits `error`+`end`), and finally `done` (or `error` for error/aborted
  stopReasons). This is the canonical reference for the event tape shape.
- `fauxProvider()` (`:520-538`) wraps the core into a `Provider` via `createProvider`, with
  a trivial api-key auth stub (`:524`).
Port `faux` as a test-support module implementing the same `ProviderStreams` trait so the
Rust event stream + types can be exercised without HTTP; mirror `streamWithDeltas`'s tape.

---

## Oracle — driving Pi offline to generate golden fixtures

### How the mock is injected (no network, no fetch mock needed)
Both `test/anthropic-sse-parsing.test.ts` and `test/anthropic-cache-write-1h-cost.test.ts`
inject a **fake Anthropic client** through `AnthropicOptions.client` (`anthropic-messages.ts:258`,
consumed at `:514-516` — when `options.client` is set, `createClient` is skipped entirely).
The fake client only needs `.messages.create(...).asResponse()` (called at `:555`):

```ts
// test/anthropic-sse-parsing.test.ts:8-14, :71-79
function createSseResponse(events: Array<{event:string; data:string}>): Response {
  const body = events.map(({event,data}) => `event: ${event}\ndata: ${data}\n`).join("\n");
  return new Response(body, { status: 200, headers: { "content-type": "text/event-stream" } });
}
function createFakeAnthropicClient(response: Response): Anthropic {
  return { messages: { create: () => ({ asResponse: async () => response }) } } as unknown as Anthropic;
}
```

NOTE the framing `createSseResponse` produces: each event is `event: X\ndata: Y\n` and
events are joined with `\n`, so between events there is a blank line (the trailing `\n` of
one + the join `\n`) which flushes the SSE accumulator. This exercises the real inline SSE
decoder (§3) and the real state machine (§4b) with zero network. There is NO global `fetch`
mock; the SDK transport is bypassed at the client boundary.

### There are NO committed raw `.sse` byte fixtures
All SSE bodies are inline TS string arrays inside the `*.test.ts` files (e.g.
`minimalAnthropicEvents` at `anthropic-sse-parsing.test.ts:16-69`; `eventsWithCacheCreation`
at `anthropic-cache-write-1h-cost.test.ts:18-57`). `getModel(provider, id)` comes from
`../src/compat.ts` and reads the generated catalog in `providers/data/*.json`.

### What the oracle tests assert
- `anthropic-sse-parsing.test.ts`: (1) malformed tool-JSON + SSE repair → `arguments ==
  {path:"A\\H", text:"col1\tcol2"}`, `stopReason "toolUse"`; (2) refusal → `stopReason
  "error"`, `errorMessage == explanation`; (3) `message_delta` without `usage` preserves
  `message_start` usage (input 12, total 12); (4) unknown events after `message_stop`
  ignored.
- `anthropic-cache-write-1h-cost.test.ts`: cost split for 1h cache writes (7.75) and the
  5m fallback (6.25); `cacheWrite`/`cacheWrite1h` values.

### Minimal snippet to capture BOTH the event tape and the final message JSON
Run with `tsx`/`vitest` from `packages/ai`. This is the exact recipe to mint golden
fixtures for the Rust port:

```ts
import { stream as streamAnthropic } from "./src/api/anthropic-messages.ts";
import { getModel } from "./src/compat.ts";
import type { Context } from "./src/types.ts";
import type Anthropic from "@anthropic-ai/sdk";

function sseResponse(events: {event:string; data:string}[]): Response {
  const body = events.map(e => `event: ${e.event}\ndata: ${e.data}\n`).join("\n");
  return new Response(body, { status: 200, headers: { "content-type": "text/event-stream" }});
}
function fakeClient(r: Response): Anthropic {
  return { messages: { create: () => ({ asResponse: async () => r }) } } as unknown as Anthropic;
}

const model = getModel("anthropic", "claude-opus-4-8");
const context: Context = { messages: [{ role:"user", content:"hi", timestamp: 0 }] };
const events = [ /* message_start … content_block_* … message_delta … message_stop */ ];

const s = streamAnthropic(model, context, { client: fakeClient(sseResponse(events)) });

const tape: unknown[] = [];
for await (const ev of s) tape.push(ev);          // (i) full event sequence
const final = await s.result();                    // (ii) final AssistantMessage
console.log(JSON.stringify({ tape, final }, null, 0)); // stringify == byte oracle
```

Notes for deterministic goldens: pass `timestamp` explicitly in inputs and treat
`output.timestamp` (`Date.now()`, `:507`) as non-deterministic — either overwrite it before
comparison or assert all keys except `timestamp`. `responseId` comes from the mocked
`message_start.message.id`, so it is stable. Because `for await` consumes the queue and
`result()` reads the same resolved promise, both can be awaited on the one stream.

---

## Rust module layout & crates

### Proposed layout under `crates/pirust-ai/src`
```
src/
  lib.rs                     # add: pub mod stream; sse; json_repair; http; auth; api; providers;
  types/                     # (feat-001, existing) — but see §4e byte-order caveat
  stream/
    mod.rs                   # AssistantMessageEventStream + EventStream<T,R> generic
                             #   = mpsc::UnboundedSender/Receiver + oneshot for result()
  sse/
    mod.rs                   # SseDecoderState, decode_line, flush_event, iterate_sse_messages
  json_repair/
    mod.rs                   # repair_json, parse_json_with_repair, parse_streaming_json,
                             #   + hand-rolled partial_json (Allow.ALL subset)
  http/
    mod.rs                   # reqwest client wrapper: POST /v1/messages, header assembly,
                             #   bytes_stream(); a `HttpResponse` trait so tests can inject
                             #   a canned SSE body (mirrors options.client / asResponse())
  auth/
    mod.rs                   # resolve_api_key (ANTHROPIC_OAUTH_TOKEN|ANTHROPIC_API_KEY),
                             #   is_oauth_token, provider_env lookup
  api/
    mod.rs
    anthropic_messages.rs    # stream(), stream_simple(), build_params, convert_messages,
                             #   convert_tools, map_stop_reason, calculate_cost call,
                             #   the SSE→event state machine (§4b), runtime-ordered final msg
    simple_options.rs        # buildBaseOptions/clamp/adjust helpers (as needed)
  providers/
    faux.rs                  # faux ProviderStreams for offline tests (§6)
    anthropic.rs             # model/provider metadata + calculate_cost (or in types/model.rs)
  models.rs / cost.rs        # calculateCost port (§4d)
```
Mirror Pi's `ProviderStreams` as a Rust trait:
```rust
pub trait ProviderStreams {
    fn stream(&self, model: &Model, ctx: &Context, opts: Option<StreamOptions>) -> AssistantMessageEventStream;
    fn stream_simple(&self, model: &Model, ctx: &Context, opts: Option<SimpleStreamOptions>) -> AssistantMessageEventStream;
}
```
For the injectable HTTP boundary (the oracle), define an `AnthropicTransport` trait with a
`send(request) -> impl Stream<Item=Bytes>` method; the reqwest impl is production, a
canned-SSE impl is the test double equivalent to `options.client`/`asResponse()`.

### Crates to add to `crates/pirust-ai/Cargo.toml`
- `tokio` (features `["rt", "rt-multi-thread", "macros", "sync"]`) — mpsc/oneshot, spawn.
- `reqwest` (features `["stream", "rustls-tls"]`, `default-features=false`) — HTTP + SSE
  byte stream over rustls (no OpenSSL).
- `futures` (or `futures-util`) — `Stream`/`StreamExt` for the event stream & byte stream.
- `tokio-util` (feature `["codec"]`) — optional, for line framing of the byte stream
  (hand-rolled buffer is also fine and matches Pi's `TextDecoder` more exactly).
- `bytes` — `reqwest::Response::bytes_stream()` yields `Bytes`.
- `base64` — image `data` handling in message conversion (used by `convertContentBlocks`).
- (already present) `serde`, `serde_json` (`preserve_order`, `raw_value`, `float_roundtrip`),
  `thiserror`.
- Dev-deps: `tokio` with `["test-util","macros"]`; no extra SSE crate — avoid
  `eventsource-stream` (framing/comment/`id:` semantics differ from Pi's inline decoder).
