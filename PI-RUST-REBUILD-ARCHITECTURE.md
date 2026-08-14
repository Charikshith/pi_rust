# Pi Agent Harness — Architecture Review & Rust Rebuild Blueprint

*Analysis of `pi/` (the Pi agent harness monorepo) with a from-scratch design for a speed-focused Rust reimplementation.*

Date: 2026-08-12 · Source: `@earendil-works/pi-*` v0.80.10 · ~100K LOC TypeScript across 5 packages.

---

## 1. Executive summary

Pi is a self-extensible coding-agent CLI (the same product category as Claude Code / OpenCode). It is a **clean, layered TypeScript monorepo**:

| Layer | Package | LOC | Job |
|---|---|---|---|
| Provider/LLM | `pi-ai` | ~23.6K | One unified message/streaming/tool model over ~40 providers (OpenAI, Anthropic, Google, Bedrock, Mistral, + 30 OpenAI-compatible vendors). |
| Agent runtime | `pi-agent-core` | ~8.2K | The agent loop: stream → tool calls → tool results → repeat, with steering, compaction, an append-only session **tree**, and hooks. |
| Coding agent | `pi-coding-agent` | ~54.7K | The application: CLI, interactive TUI, 7 built-in tools, config, sessions, and a runtime **TypeScript plugin system**. |
| Terminal UI | `pi-tui` | ~12.2K | A differential-rendering TUI toolkit (string-line model, not a cell buffer). |
| Orchestrator | `pi-orchestrator` | ~2.0K | Experimental process supervisor that spawns `pi --mode rpc` children over a socket. |

**The architecture is worth keeping almost verbatim as a design.** The layering, the discriminated-union message model, the event-stream boundary between provider and loop, the append-only session tree, and the tool abstraction all translate to idiomatic Rust and would be *cleaner* there (exhaustive `match`, `thiserror`, typed enums replacing dozens of implicit runtime branches).

**Three things do not port cleanly** and dominate the rebuild plan:
1. **The runtime TypeScript extension system** (`jiti` executing user code in-process, handing out live JS object references). This is the single biggest architectural decision — Rust has no native equivalent.
2. **The TUI's ANSI-string rendering model** — it is upside-down relative to Rust's `ratatui` cell-buffer model. Porting it is a redesign, not a translation (and the redesign is an improvement).
3. **Provider quirks** — ~40 providers each with idiosyncrasies. These are hard-won empirical knowledge that must be carried over verbatim; no abstraction saves you from re-encoding them.

**Where Rust actually wins on speed:** startup time (no Node/V8 warmup, no ~400 KB of JSON model-catalog parsed on import), streaming tool-argument parsing (currently O(n²) re-parse per delta in JS), text width/wrapping in the TUI (currently `Intl.Segmenter` per grapheme), in-process ripgrep/fd (via the `grep`/`ignore` crates instead of downloading + spawning binaries), and native WASM-free image processing (photon is already Rust).

---

## 2. System overview & data flow

```
                    ┌─────────────────────────────────────────────┐
   terminal I/O     │           pi-coding-agent (app)             │
  ◄──────────────►  │  cli → main → mode (interactive|print|rpc)  │
                    │        AgentSession  ──owns──► tools,        │
                    │        (god-object)          config,        │
                    │                              sessions,       │
                    │                              extensions      │
                    └───────┬──────────────────────────┬──────────┘
                            │ uses                       │ renders via
                            ▼                            ▼
                   ┌──────────────────┐        ┌──────────────────┐
                   │  pi-agent-core   │        │     pi-tui        │
                   │  agentLoop(...)  │        │  Component.render │
                   │  EventStream     │        │  → string[]       │
                   └────────┬─────────┘        │  differential     │
                            │ StreamFn         │  patch            │
                            ▼                  └──────────────────┘
                   ┌──────────────────┐
                   │     pi-ai        │  Model → Provider → API module
                   │  streamSimple()  │  → AssistantMessageEventStream
                   └──────────────────┘
```

The **spine of the whole system is one type**: `AssistantMessageEventStream` — a hand-rolled `AsyncIterable` of streaming events (`start`, `text_delta`, `toolcall_delta`, `done`, `error`) that also exposes a `.result()` promise for the final assembled message. Every provider produces one; the agent loop consumes one. This is the clean seam to preserve.

---

## 3. Core design choices worth keeping

### 3.1 Unified message & content model (`pi-ai/src/types.ts`)
Small, deliberate discriminated unions:
- **Content blocks**: `TextContent | ThinkingContent | ImageContent | ToolCall`.
- **Messages**: `UserMessage | AssistantMessage | ToolResultMessage`.
- `ToolCall.arguments` is untyped JSON; `Tool.parameters` is a runtime JSON Schema (TypeBox `TSchema`).
- `Usage` carries a full cost breakdown (input/output/cacheRead/cacheWrite, with Anthropic's 1h-cache split) computed on every usage-bearing event.

**Keep it.** In Rust these become `#[serde(tag = "type")]` enums — safer and faster.

### 3.2 Provider abstraction: protocol modules + vendor factories
The cleverest structural decision in `pi-ai`:
- **`src/api/*` = one module per wire protocol** (`anthropic-messages`, `openai-completions`, `openai-responses`, `google-generative-ai`, `bedrock-converse-stream`, …), each exporting exactly `stream` + `streamSimple`. The module *is* the `ProviderStreams` interface by structural typing.
- **`src/providers/*` = one tiny factory per vendor** (~20 lines each) that picks an API module, a base URL, an auth strategy, and a static model list.
- **`compat` quirk flags** (`OpenAICompletionsCompat`, etc.) — ~20 boolean/enum knobs per API, auto-detected from base URL, that encode per-vendor behavior differences (`thinkingFormat` alone has 9 variants).
- **Lazy loading** (`api/lazy.ts`): a stream is returned *synchronously* while auth resolution and `await import()` of the provider SDK run behind it; setup failures become in-band error events. This keeps startup fast and tree-shakes unused providers.

**Keep the shape.** In Rust: protocol = trait impl, vendor = config struct, `compat` = enums with exhaustive `match`, lazy = `tokio::spawn` + channel. The open-ended `Api = KnownApi | (string & {})` (custom providers by string) is the one piece that loses type-level extensibility — accept a `Custom(String)` enum variant.

### 3.3 The agent loop (`pi-agent-core/src/agent-loop.ts`)
Pure functions over an `EventStream`; owns no state. Two nested loops:
- **Inner**: `turn_start` → inject steering messages → `streamAssistantResponse` → execute tool calls → `turn_end`; `prepareNextTurn` may swap model/context/thinking-level mid-run; `shouldStopAfterTurn` for graceful exit.
- **Outer**: restarts the inner loop when `getFollowUpMessages()` returns work.

Design choices to preserve:
- **`AgentMessage[]` internally, `Message[]` only at the LLM boundary** (`convertToLlm` collapses/filters). This lets the app carry UI-only message types the model never sees.
- **Tool execution modes**: `sequential` vs `parallel`, with a per-tool `executionMode` override. Parallel mode prepares calls *sequentially* (deterministic hook order), executes concurrently, emits `tool_execution_end` in completion order but tool-result *messages* in source order.
- **Truncation safety**: if `stopReason === "length"`, *every* tool call in that message is failed unexecuted (streamed args may validate while silently truncated).
- **Hooks contract**: `beforeToolCall` (can block), `afterToolCall` (can patch result), and all hooks are contractually forbidden from throwing.
- **Cancellation**: a single `AbortSignal` threaded everywhere, checked cooperatively at loop boundaries. No `EventEmitter` anywhere — listeners are a `Set` awaited in insertion order.

**Keep it wholesale.** This maps beautifully to Rust: `AbortSignal` → `tokio_util::sync::CancellationToken`, `EventStream` → `mpsc` + a `oneshot` for the final result, hooks → boxed async closures.

### 3.4 Session model: append-only tree, not a log (`session-manager.ts`, `harness/session/`)
Every session is a **tree** of entries `{id, parentId, timestamp, type}` with a `currentLeafId`; branching, forking, and tree-navigation are first-class. On-disk format is JSONL: a `SessionHeader` line (version 3) then one entry per line, appended with `fs.appendFile`. Context for the model is reconstructed by walking `parentId` to root, applying the last compaction, and projecting each entry to messages. Runtime state (model, thinking level, active tools) is derived by *replaying* the path.

**Keep it.** `#[serde(tag = "type")]` enum + `HashMap<Id, Entry>` index + append-only file. This is one of the easiest and highest-value ports.

### 3.5 Tool abstraction
```ts
interface AgentTool<TParameters extends TSchema, TDetails> {
  name; label; description;
  parameters: TParameters;                    // runtime JSON Schema
  prepareArguments?(raw): Static<TParameters>; // arg coercion shim
  execute(id, params, signal?, onUpdate?): Promise<AgentToolResult<TDetails>>;
  executionMode?: "sequential" | "parallel";
}
```
Tools **throw on failure** (errors become `isError` results upstream), stream partial results via `onUpdate`, and — in `pi-coding-agent` — also carry their **TUI renderers** (`renderCall`/`renderResult` returning components). Each tool exposes a pluggable `*Operations` interface so extensions can redirect execution to SSH/containers.

---

## 4. Recommended Rust architecture

### 4.1 Crate/workspace layout (mirrors the packages, plus a split)
```
pi-rs/
├── crates/
│   ├── pi-core-types    # message/content/tool/model enums, serde. No deps beyond serde.
│   ├── pi-ai            # providers, streaming, auth, cost. reqwest + eventsource.
│   ├── pi-agent         # the loop, tool trait, session tree, compaction.
│   ├── pi-tui           # ratatui-based UI (redesign, see §6.2).
│   ├── pi-coding-agent  # tools, config, modes, extension host.
│   ├── pi-orchestrator  # process supervisor.
│   └── pi-plugin-host   # NEW crate: the JS/RPC extension bridge (see §6.1).
└── bins/  pi (CLI),  pi-rpc (rpc entry)
```

### 4.2 Async runtime & concurrency
- **`tokio`** multi-threaded runtime.
- **`EventStream<T>` → `tokio::sync::mpsc::Receiver<T>` + `oneshot::Receiver<Final>`** (or `async-stream` for the iterator ergonomics). Backpressure is explicit and free.
- **Cancellation → `CancellationToken`** cloned into every future; the current cooperative-abort pattern maps 1:1.
- **Parallel tool execution → `futures::future::join_all`** (preserves index order for result messages) combined with a `FuturesUnordered` if you want completion-order events. Per-tool sequential mode is just an `await` loop.
- **Event listeners** (`Vec<Box<dyn Fn(&Event) -> BoxFuture<'_, Result<()>> + Send + Sync>>`) — the one gotcha is that listeners in the JS version close over the harness. In Rust use `Arc<Harness>` + `Weak` back-references, or (better) send events over a channel and let the UI own the reaction, avoiding the self-reference entirely. The documented `waitForIdle()`-from-a-listener deadlock becomes a real `RwLock` deadlock, so the channel design is preferred.

### 4.3 Error handling
`Result<T, E>` with `thiserror` enums replaces the JS `Result<TValue,TError>` type and the code-tagged error classes (`FileError`, `ExecutionError`, `SessionError`, …) almost 1:1, with `#[source]` for cause chains. The regex-based transient-error classifier (`utils/retry.ts`) becomes a single-pass `regex::RegexSet`.

### 4.4 The tool trait (dynamic dispatch with typed params)
Rust has no `Static<TSchema>`. Recommended two-layer design:
```rust
#[async_trait]
pub trait AgentTool: Send + Sync {
    fn name(&self) -> &str;
    fn label(&self) -> &str;
    fn parameters(&self) -> &serde_json::Value;      // JSON Schema, type-erased
    fn execution_mode(&self) -> Option<ExecutionMode>;
    fn prepare_arguments(&self, raw: Value) -> anyhow::Result<Value> { Ok(raw) }
    async fn execute(&self, id: &str, args: Value,
                     cancel: CancellationToken,
                     updates: mpsc::Sender<ToolResult>) -> anyhow::Result<ToolResult>;
}

// Ergonomic typed layer via a blanket impl:
pub trait TypedTool: Send + Sync {
    type Params: DeserializeOwned + JsonSchema;      // schemars derives the schema
    async fn run(&self, params: Self::Params, ...) -> anyhow::Result<ToolResult>;
}
```
Registry becomes `HashMap<String, Arc<dyn AgentTool>>` — already faster than the current linear `tools.find(...)`. Use **`schemars`** to derive JSON Schema from Rust structs, and **`jsonschema`** + a small coercion pass to replicate TypeBox's `Value.Convert` (string→number, `"true"`→bool, union trial-and-error). `AgentToolResult<TDetails>.details` erases to `serde_json::Value`.

### 4.5 The message enum & custom messages
`AgentMessage` is extended by *downstream packages* via TS declaration merging — Rust enums are closed. Use:
```rust
#[non_exhaustive]
enum AgentMessage {
    User(UserMessage), Assistant(AssistantMessage), ToolResult(ToolResultMessage),
    Custom(Box<dyn CustomMessage>),   // trait carries `to_llm(&self) -> Option<Message>`
}
```
This is the single most invasive type decision; settle it before writing the loop.

---

## 5. Where Rust buys speed (rank-ordered)

1. **Startup.** No V8 init; no ~400 KB provider catalog parsed on import (OpenRouter's `openrouter.json` alone is 105 KB). Load catalogs lazily / from a compiled `phf` map or embedded binary blob. Expect single-digit-ms cold start vs Node's tens-to-hundreds of ms.
2. **Streaming tool-argument parsing.** Every provider today does `partialJson += delta; args = parseStreamingJson(partialJson)` on *every* SSE chunk — O(n²) in argument size with a 3-tier repair fallback. In Rust, parse incrementally over `bytes::BytesMut` and only materialize at `toolcall_end`.
3. **SSE decode.** Anthropic's hand-rolled decoder re-slices/`join("\n")`s its buffer repeatedly. `bytes::BytesMut` + line splitting is allocation-light.
4. **TUI text handling.** `visibleWidth`/wrapping call `Intl.Segmenter` per grapheme (mitigated by a 512-entry cache). `unicode-width` + `unicode-segmentation` are far faster and likely make the cache unnecessary.
5. **Search tools.** Replace the runtime download-and-spawn of `rg`/`fd` (`utils/tools-manager.ts`) with the **`grep`/`grep-searcher` and `ignore` crates linked in-process** — removes the download, the subprocess, the NDJSON parse, and the fd `--full-path`/`--no-require-git` quirk handling.
6. **Image processing.** photon is Rust→WASM today, loaded via a `fs.readFileSync` monkey-patch and run in a `worker_threads` worker to avoid blocking the loop. Use the **`image` crate** directly on a `rayon` thread — the WASM shim, the worker, and the wasm-path hack all disappear.
7. **Session list.** Parsing every JSONL header across all projects (currently concurrency-limited to 10) is trivially parallel with `rayon`/`tokio`.

---

## 6. The three hard problems

### 6.1 Extensions — the defining decision
Today: `jiti` loads user `.ts`/`.js` **in-process**, and extensions get **live JS references** — the `Agent`, pi-tui `Component`s, tool definitions whose renderers *return components*, and even whole `Provider` objects. `core/extensions/types.ts` is 1,460 lines of types alone; there are ~100 example extensions (including a WASM DOOM overlay). In the compiled Bun binary, pi even injects its own packages as "virtual modules" so plugins can `import` from the host.

**Rust cannot preserve in-process live-reference plugins.** Options, in order of fidelity vs effort:

| Option | Fidelity | Cost | Notes |
|---|---|---|---|
| **Embed a JS engine** (Deno core / `rquickjs` / `boa`) | High for logic, **low for UI** | High | Can run extension logic, but pi-tui `Component`s and native `Provider` objects can't cross the FFI boundary as live objects. Renderers would have to become a serializable view protocol. |
| **Out-of-process RPC plugins** (WASM component model or a JSON-RPC sidecar protocol) | Medium | Medium | Clean, sandboxable, language-agnostic. Breaks every *in-process* capability: custom TUI components, custom editors, in-process providers. You'd define a serializable widget/render protocol (which the RPC mode already hints at with `extension_ui_request`). |
| **First-party only + config-driven providers** | Low | Low | Ship the 7 tools and providers natively; expose only declarative extension points (custom providers by config, hooks as WASM). Fastest path to a shippable binary. |

**Recommendation:** design the core so extensibility is a *serializable protocol* from day one (tools, providers, and a declarative render/view model over RPC), and treat "embed a JS runtime" as an optional adapter crate (`pi-plugin-host`). Do **not** try to reproduce live-reference in-process TS plugins in Rust — it will dominate the schedule and fight the borrow checker. The existing `--mode rpc` + `extension_ui_request/response` channel is the seam to build on.

### 6.2 The TUI — redesign onto `ratatui`, don't port
pi-tui's `Component::render(width) -> string[]` returns **fully styled ANSI-embedded lines**, and the renderer diffs frames by **string equality**, patching only the changed line span with synchronized-output mode (`\x1b[?2026h`). ~800 lines (`AnsiCodeTracker`, `extractSegments`, `sliceWithWidth`, `wrapTextWithAnsi`) exist *only* because styling lives inside strings. A load-bearing invariant is that any line wider than the terminal is a fatal crash.

`ratatui` inverts this: you write into a `Buffer` of `Cell{symbol, style}` and it does its own cell-level diff. Consequences:
- Every component that emits raw SGR or uses `(text) => string` theme closures must be rewritten to produce `Span`/`Line`/`Style`. Most of the ANSI-string machinery **deletes itself** — the good news.
- The width-overflow crash disappears (the `Buffer` clips by construction) — the strongest argument for the redesign.
- **Hard parts that survive:** (a) pi renders into **real scrollback**, not the alternate screen — an unbounded document that grows downward past the viewport, with simulated scroll via `"\r\n".repeat(n)`. `ratatui`'s `Viewport::Inline` is the closest fit but is fixed-height; reproducing append/scrollback semantics is the single hardest UI task. (b) **Kitty/iTerm2 graphics** are opaque escape blobs that must reserve rows and be deleted before repaint — no first-class ratatui support (`ratatui-image` is third-party with its own model). (c) The **overlay focus state machine** (`eligible`/`blocked`/`resume`, preFocus chains) uses object-identity comparisons throughout; convert to component IDs/indices.
- **Wins:** key parsing (~1,400 lines of `keys.ts` handling legacy + Kitty CSI-u + modifyOtherKeys) largely evaporates into `crossterm`'s `KeyEvent`; `StdinBuffer`'s escape reassembly evaporates too; and both **native N-API addons disappear** — `SetConsoleMode(ENABLE_VIRTUAL_TERMINAL_INPUT)` is `crossterm`/`windows-sys`, and the macOS `CGEventSourceFlagsState` shift-state probe is a `core-graphics` call. No prebuilds, no multi-path `require` fallback.

Markdown: `marked`'s lexer-walk becomes `pulldown-cmark` (its streaming/event model actually suits the "trim partial closing fence while streaming" problem better).

### 6.3 Provider quirks — port verbatim, no abstraction saves you
Carry these over as data + exhaustive `match`, and pin each with a test:
- Anthropic OAuth token detection + Claude-Code tool-name aliasing, `redacted_thinking` blocks, adaptive-thinking per model id, mandatory tool-call ID shape `^[a-zA-Z0-9_-]{1,64}$`.
- OpenAI Responses tool-call IDs (450+ chars, contain `|`) need an id→id remap table; synthesize `"No result provided"` for orphaned tool calls.
- Google `thoughtSignature` base64 validation, Gemini-major-version-gated multimodal function responses, `sanitizeForOpenApi` (strip `$schema/$defs/...`).
- Bedrock: ARN-embedded region extraction, SigV4 signing of custom headers via a Smithy build-step middleware (reserved `x-amz-*`/`authorization`/`host` dropped), HTTP/2→HTTP/1.1 fallback. In Rust: `aws-sdk-bedrockruntime` or hand-roll SigV4 with `aws-sigv4`.
- Silent context-overflow detection heuristics: z.ai (`usage.input > contextWindow`), Xiaomi MiMo (`stopReason:"length" && output===0 && input >= 0.99*contextWindow`), Moonshot usage on `choice.usage` not `chunk.usage`, chutes.ai emitting both `reasoning` and `reasoning_content`.
- The `compat` matrix (esp. the 9-way `thinkingFormat` chain) maps to enums + exhaustive `match`, which will *surface* the currently-implicit "which branch wins" ordering — a correctness upgrade.

---

## 7. Component-by-component porting difficulty

| Component | Difficulty | Rust approach |
|---|---|---|
| Message/content/model types | Easy | `#[serde(tag="type")]` enums |
| Session tree + JSONL | Easy | serde enum + `HashMap` index + append file; keep v1→v3 migration tests |
| Agent loop | Easy–Med | pure fns over `mpsc`; `join_all` for parallel tools |
| Error taxonomy / retry classifier | Easy | `thiserror` + `regex::RegexSet` |
| Cancellation | Easy | `CancellationToken` |
| Tool trait + validation | Medium | `dyn AgentTool` + `schemars`/`jsonschema` + coercion pass |
| Custom message extensibility | Medium | `#[non_exhaustive]` enum + `dyn CustomMessage` |
| HTTP + SSE streaming | Medium | `reqwest` + `eventsource-stream` or hand-rolled over `bytes` |
| Provider quirks (×40) | Hard (tedious) | data + `match`, per-provider tests |
| Bash/exec platform behavior | Medium | `tokio::process`; POSIX process-group `SIGKILL` vs Windows `taskkill /F /T`, Git Bash discovery, re-arming stdio idle timer — every edge case is a regression test |
| grep/find | Medium (net win) | `grep`/`ignore` crates in-process |
| Image processing | Medium (net win) | `image` crate on `rayon`, drop WASM/worker |
| TUI | **Hard (redesign)** | `ratatui` + `crossterm`; scrollback + graphics are the hard bits |
| Extensions | **Hardest** | serializable RPC/WASM protocol; optional embedded JS adapter |
| `agent-session.ts` (2.9K) + `interactive-mode.ts` (5.4K) god-objects | Hard | decompose into owned sub-states + channels; do not translate 1:1 |
| Orchestrator | Easy | `tokio::process` + `BufReader::lines`; Unix socket → `tokio::net::windows::named_pipe` on Windows |

---

## 8. Recommended Rust tech stack

- **Runtime/async:** `tokio`, `tokio-util` (CancellationToken), `futures`.
- **Serde:** `serde`, `serde_json`; `schemars` (tool schemas), `jsonschema` (validation).
- **HTTP/stream:** `reqwest` (rustls), `eventsource-stream`, `bytes`. AWS: `aws-sdk-bedrockruntime` + `aws-sigv4`.
- **TUI:** `ratatui`, `crossterm`; `unicode-width`, `unicode-segmentation`; `pulldown-cmark`; `syntect` (replaces highlight.js); `ratatui-image` or custom for Kitty/iTerm graphics.
- **Tools:** `grep`/`grep-searcher`/`ignore` (search), `image` (images), `similar` or `imara-diff` (edit diffs), `ropey` (editor buffer).
- **Errors/logging:** `thiserror`, `anyhow`, `tracing`.
- **Config/locking:** `serde` + `fs4`/`fd-lock` (replaces `proper-lockfile`), `figment` or hand-rolled deep-merge for global←project settings.
- **Plugin host (optional):** `wasmtime` (component model) and/or `rquickjs`/Deno core.
- **Auth:** `oauth2` crate + `keyring` for credential storage; PKCE/device-code flows port directly.

---

## 9. Phased migration plan

1. **Foundations (`pi-core-types` + `pi-ai` skeleton).** Message/model enums, one provider end-to-end (Anthropic), SSE streaming, `EventStream`→channel. Prove the streaming seam.
2. **Agent loop + session tree (`pi-agent`).** Tool trait, sequential/parallel execution, hooks, JSONL sessions, cancellation. Non-interactive `print` mode CLI is now usable.
3. **Native tools.** read/write/edit/bash/grep/find/ls with in-process `grep`/`ignore`. Regression-test bash platform edge cases.
4. **Providers breadth.** Port the `compat` matrix and the ×40 vendor quirks, each behind a test. OpenAI, Google, Bedrock, then the OpenAI-compatible long tail.
5. **TUI redesign (`pi-tui` on ratatui).** Start with the scrollback/append viewport model — it gates everything else. Editor via `ropey`. Defer graphics.
6. **Config, compaction, self-update, HTML export.** Mostly mechanical.
7. **Extensibility.** Ship declarative providers + a serializable tool/render RPC protocol first; add the embedded-JS or WASM adapter only if demand justifies it.
8. **Orchestrator.** Straightforward `tokio::process` supervisor; solve the Windows named-pipe path.

Deliver value at the end of phase 3 (a fast headless coding agent), and again at phase 5 (interactive). Extensions (phase 7) are the long pole — scope them explicitly.

---

## 10. Key risks & decisions to make up front

1. **Extension model** (§6.1) — decide *now* between "no in-process plugins, RPC/WASM only" vs "embed a JS runtime." Everything about the API surface depends on it. Recommended: RPC/WASM-first.
2. **TUI scrollback semantics** (§6.2) — the "unbounded document past the viewport" model is the hardest single thing to reproduce on ratatui. Prototype it before committing.
3. **Custom-message extensibility** (§4.5) — closed Rust enums vs the open TS union. Pick `#[non_exhaustive]` + trait object early.
4. **Provider surface = a long tail of empirical quirks** (§6.3). Budget for it; treat the existing TS provider files as the spec and port test-first. Do not "clean up" quirks you don't understand — they encode real vendor bugs.
5. **God-objects** — `agent-session.ts` (2.9K) and `interactive-mode.ts` (5.4K) must be decomposed, not transliterated; their interleaved mutable state will not survive the borrow checker unchanged.

---

*Bottom line:* the Pi design is sound and largely worth reproducing as-is; a Rust rebuild is mostly a **faithful port of the layered core** (types → ai → agent → tools) that gets *cleaner and faster* in the process, gated by **two genuine redesigns** (the TUI onto a cell buffer, and the extension system onto a serializable protocol) and **one large tedious surface** (the ×40 provider quirks). Sequence the work so a fast headless agent ships early, and treat extensions as an explicit, separable milestone.
