#!/usr/bin/env node
// Generate authentic golden fixtures by driving Pi's REAL `openai-completions`
// adapter fully offline.
//
// Two capture paths, both running Pi's actual TS source (no reimplementation):
//
// 1. `convertMessages(model, context, compat, options)` — exported directly
//    from `packages/ai/src/api/openai-completions.ts`. We pass explicit resolved
//    compat objects (as Pi's own `deferred-tools.test.ts` does) to pin the
//    message-conversion layer: user/assistant/tool roles, thinking blocks
//    (as-text / signature / empty), tool calls incl. grammar-input custom tools,
//    image blocks (attached-from-tool-result), Kimi deferred-tools system
//    messages, requiresAssistantAfterToolResult bridging, DeepSeek
//    reasoning_content, requiresToolResultName.
//
// 2. `streamSimple(model, context, { apiKey, onPayload })` — exercises
//    `getCompat(model)` + `buildParams` (which internally calls
//    `convertMessages` + `convertTools` + `normalizeToolCallId` and applies the
//    resolved compat to `store`/`stream_options`/`max_completion_tokens` vs
//    `max_tokens`/`tools`). The `onPayload` callback captures the exact params
//    object and throws to stop before any network I/O (same trick as Pi's
//    own `capturePayload` test helper). The payload is serialized with
//    JSON.stringify so bytes are Pi's exact JS form.
//
// This is the oracle for the Rust `convertMessages`/`convertTools`/`detectCompat`/
// `getCompat`/`normalizeToolCallId` ports in
// `crates/pirust-ai/src/api/openai_completions.rs`.
//
// Deterministic + idempotent. Run:
//   cd pirust && node scripts/gen-openai-completions-oracle.mjs
//   node scripts/gen-openai-completions-oracle.mjs --check

import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const pirustRoot = dirname(here);
const piRoot = join(pirustRoot, "..", "pi");
const aiSrc = join(piRoot, "packages", "ai", "src");
const outDir = join(pirustRoot, "tests", "fixtures", "pi", "openai-completions");

const checkMode = process.argv.includes("--check");

// --- Import Pi's REAL adapter + compat registry + typebox -------------------
const { convertMessages, streamSimple } = await import(
  pathToFileURL(join(aiSrc, "api", "openai-completions.ts")).href
);
const { getModel } = await import(pathToFileURL(join(aiSrc, "compat.ts")).href);
const { Type } = await import(
  pathToFileURL(join(piRoot, "node_modules", "typebox", "build", "index.mjs")).href
);

// --- Fixture builders (mirror Pi's deferred-tools.test.ts helpers) ----------
function makeTool(name) {
  return {
    name,
    description: `The ${name} tool`,
    parameters: Type.Object({ value: Type.String() }),
  };
}



function makeUserMessage(timestamp, content = "Hello") {
  return { role: "user", content, timestamp };
}

function makeAssistantText(text, extra = {}) {
  return {
    role: "assistant",
    content: [{ type: "text", text }],
    api: "anthropic-messages",
    provider: "anthropic",
    model: "claude-opus-4-6",
    usage: {
      input: 0,
      output: 0,
      cacheRead: 0,
      cacheWrite: 0,
      totalTokens: 0,
      cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
    },
    stopReason: "stop",
    timestamp: 2,
    ...extra,
  };
}

function makeAssistantToolCall(id = "call_1", name = "base_tool", arguments_ = {}, extra = {}) {
  return {
    role: "assistant",
    content: [{ type: "toolCall", id, name, arguments: arguments_ }],
    api: "anthropic-messages",
    provider: "anthropic",
    model: "claude-opus-4-6",
    usage: {
      input: 0,
      output: 0,
      cacheRead: 0,
      cacheWrite: 0,
      totalTokens: 0,
      cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
    },
    stopReason: "toolUse",
    timestamp: 2,
    ...extra,
  };
}

function makeAssistantThinking(thinking, thinkingSignature, extra = {}) {
  return {
    role: "assistant",
    content: [{ type: "thinking", thinking, thinkingSignature }],
    api: "anthropic-messages",
    provider: "anthropic",
    model: "claude-opus-4-6",
    usage: {
      input: 0,
      output: 0,
      cacheRead: 0,
      cacheWrite: 0,
      totalTokens: 0,
      cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
    },
    stopReason: "stop",
    timestamp: 2,
    ...extra,
  };
}

function makeToolResult(toolCallId, toolName, content, addedToolNames, extra = {}) {
  return {
    role: "toolResult",
    toolCallId,
    toolName,
    content,
    addedToolNames,
    isError: false,
    timestamp: 3,
    ...extra,
  };
}

function makeContext(messages, tools) {
  return { systemPrompt: undefined, messages, tools };
}

function baseCompat(overrides = {}) {
  return {
    supportsStore: true,
    supportsDeveloperRole: true,
    supportsReasoningEffort: true,
    supportsUsageInStreaming: true,
    supportsFinishReason: true,
    maxTokensField: "max_completion_tokens",
    requiresToolResultName: false,
    requiresAssistantAfterToolResult: false,
    requiresThinkingAsText: false,
    requiresReasoningContentOnAssistantMessages: false,
    thinkingFormat: "openai",
    openRouterRouting: {},
    vercelGatewayRouting: {},
    chatTemplateKwargs: {},
    chatTemplateArgs: {},
    zaiToolStream: false,
    supportsThinkingTokenBudget: false,
    thinkingTokenBudgetField: undefined,
    supportsStrictMode: true,
    supportsOpenAIGrammarTools: false,
    cacheControlFormat: undefined,
    sendSessionAffinityHeaders: false,
    deferredToolsMode: undefined,
    sessionAffinityFormat: "openai",
    supportsLongCacheRetention: true,
    ...overrides,
  };
}

// --- Scenario definitions ---------------------------------------------------
const convertCases = [
  {
    name: "basic-user-assistant",
    model: {
      id: "test-model",
      name: "Test",
      api: "openai-completions",
      provider: "together",
      baseUrl: "http://127.0.0.1:9/v1",
      reasoning: false,
      input: ["text"],
      cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
      contextWindow: 128000,
      maxTokens: 4096,
    },
    context: makeContext([
      makeUserMessage(1),
      makeAssistantText("on it"),
      makeUserMessage(2, "go"),
    ]),
    compat: baseCompat(),
  },
  {
    name: "thinking-signature",
    model: {
      id: "test-model",
      name: "Test",
      api: "openai-completions",
      provider: "opencode-go",
      baseUrl: "http://127.0.0.1:9/v1",
      reasoning: true,
      input: ["text"],
      cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
      contextWindow: 128000,
      maxTokens: 4096,
    },
    context: makeContext([
      makeUserMessage(1),
      makeAssistantThinking("let me think", "reasoning"),
      makeAssistantText("answer", {}),
    ]),
    compat: baseCompat(),
  },
  {
    name: "thinking-as-text",
    model: {
      id: "test-model",
      name: "Test",
      api: "openai-completions",
      provider: "together",
      baseUrl: "http://127.0.0.1:9/v1",
      reasoning: true,
      input: ["text"],
      cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
      contextWindow: 128000,
      maxTokens: 4096,
    },
    context: makeContext([
      makeUserMessage(1),
      makeAssistantThinking("step one", "signature"),
      makeAssistantText("done"),
    ]),
    compat: baseCompat({ requiresThinkingAsText: true }),
  },
  {
    name: "deepseek-reasoning-content",
    model: {
      id: "test-model",
      name: "Test",
      api: "openai-completions",
      provider: "deepseek",
      baseUrl: "http://127.0.0.1:9/v1",
      reasoning: true,
      input: ["text"],
      cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
      contextWindow: 128000,
      maxTokens: 4096,
    },
    context: makeContext([makeUserMessage(1), makeAssistantText("ok")]),
    compat: baseCompat({ requiresReasoningContentOnAssistantMessages: true }),
  },
  {
    name: "tool-calls",
    model: {
      id: "test-model",
      name: "Test",
      api: "openai-completions",
      provider: "together",
      baseUrl: "http://127.0.0.1:9/v1",
      reasoning: false,
      input: ["text"],
      cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
      contextWindow: 128000,
      maxTokens: 4096,
    },
    context: makeContext(
      [
        makeUserMessage(1),
        makeAssistantToolCall("call_1", "base_tool", { value: "x" }),
        makeToolResult("call_1", "base_tool", [{ type: "text", text: "done" }], undefined),
        makeUserMessage(4, "thanks"),
      ],
      [makeTool("base_tool")]
    ),
    compat: baseCompat(),
  },
  {
    name: "pipe-tool-call-ids",
    model: {
      id: "test-model",
      name: "Test",
      api: "openai-completions",
      provider: "github-copilot",
      baseUrl: "http://127.0.0.1:9/v1",
      reasoning: false,
      input: ["text"],
      cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
      contextWindow: 128000,
      maxTokens: 4096,
    },
    context: makeContext([
      makeUserMessage(1),
      makeAssistantToolCall(
        "call_abc|item_12345",
        "base_tool",
        { value: "x" },
        { model: "gpt" }
      ),
      makeToolResult("call_abc|item_12345", "base_tool", [{ type: "text", text: "done" }], undefined),
      makeUserMessage(4),
    ]),
    compat: baseCompat(),
  },
  {
    name: "pipe-tool-call-id-hash",
    model: {
      id: "test-model",
      name: "Test",
      api: "openai-completions",
      provider: "github-copilot",
      baseUrl: "http://127.0.0.1:9/v1",
      reasoning: false,
      input: ["text"],
      cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
      contextWindow: 128000,
      maxTokens: 4096,
    },
    context: makeContext([
      makeUserMessage(1),
      makeAssistantToolCall(
        "call_abc|" + "long-item-" + "x".repeat(80),
        "base_tool",
        { value: "x" }
      ),
      makeToolResult("call_abc|" + "long-item-" + "x".repeat(80), "base_tool", [{ type: "text", text: "done" }], undefined),
      makeUserMessage(4),
    ]),
    compat: baseCompat(),
  },
  {
    name: "assistant-after-tool-result",
    model: {
      id: "test-model",
      name: "Test",
      api: "openai-completions",
      provider: "together",
      baseUrl: "http://127.0.0.1:9/v1",
      reasoning: false,
      input: ["text"],
      cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
      contextWindow: 128000,
      maxTokens: 4096,
    },
    context: makeContext(
      [
        makeUserMessage(1),
        makeAssistantToolCall(),
        makeToolResult("call_1", "base_tool", [{ type: "text", text: "done" }], undefined),
        makeUserMessage(4, "thanks"),
      ],
      [makeTool("base_tool")]
    ),
    compat: baseCompat({ requiresAssistantAfterToolResult: true }),
  },
  {
    name: "tool-result-images",
    model: {
      id: "test-model",
      name: "Test",
      api: "openai-completions",
      provider: "together",
      baseUrl: "http://127.0.0.1:9/v1",
      reasoning: false,
      input: ["text", "image"],
      cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
      contextWindow: 128000,
      maxTokens: 4096,
    },
    context: makeContext(
      [
        makeUserMessage(1),
        makeAssistantToolCall(),
        makeToolResult("call_1", "base_tool", [
          { type: "text", text: "here is the image" },
          { type: "image", data: "AAAA", mimeType: "image/png" },
        ], undefined),
      ],
      [makeTool("base_tool")]
    ),
    compat: baseCompat(),
  },
  {
    name: "kimi-deferred-tools",
    model: {
      id: "test-model",
      name: "Test",
      api: "openai-completions",
      provider: "moonshotai",
      baseUrl: "http://127.0.0.1:9/v1",
      reasoning: false,
      input: ["text"],
      cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
      contextWindow: 128000,
      maxTokens: 4096,
    },
    context: makeContext(
      [
        makeUserMessage(1),
        makeAssistantToolCall(),
        makeToolResult("call_1", "base_tool", [{ type: "text", text: "done" }], ["late_tool"]),
        makeUserMessage(4, "thanks"),
      ],
      [makeTool("base_tool"), makeTool("late_tool")]
    ),
    compat: baseCompat({ deferredToolsMode: "kimi" }),
  },
  {
    name: "tool-result-name",
    model: {
      id: "test-model",
      name: "Test",
      api: "openai-completions",
      provider: "together",
      baseUrl: "http://127.0.0.1:9/v1",
      reasoning: false,
      input: ["text"],
      cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
      contextWindow: 128000,
      maxTokens: 4096,
    },
    context: makeContext(
      [
        makeUserMessage(1),
        makeAssistantToolCall(),
        makeToolResult("call_1", "base_tool", [{ type: "text", text: "done" }], undefined),
      ],
      [makeTool("base_tool")]
    ),
    compat: baseCompat({ requiresToolResultName: true }),
  },
  {
    name: "grammar-custom-tool-call",
    model: {
      id: "test-model",
      name: "Test",
      api: "openai-completions",
      provider: "together",
      baseUrl: "http://127.0.0.1:9/v1",
      reasoning: false,
      input: ["text"],
      cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
      contextWindow: 128000,
      maxTokens: 4096,
    },
    context: makeContext([
      makeUserMessage(1),
      makeAssistantToolCall("call_g", "grammar_tool", { value: "input string" }),
      makeToolResult("call_g", "grammar_tool", [{ type: "text", text: "done" }], undefined),
      makeUserMessage(4),
    ]),
    compat: baseCompat(),
    options: { grammarToolInputProperties: new Map([["grammar_tool", "value"]]) },
  },
  {
    name: "system-prompt-developer-role",
    model: {
      id: "test-model",
      name: "Test",
      api: "openai-completions",
      provider: "together",
      baseUrl: "http://127.0.0.1:9/v1",
      reasoning: true,
      input: ["text"],
      cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
      contextWindow: 128000,
      maxTokens: 4096,
    },
    context: {
      systemPrompt: "You are a helpful assistant.",
      messages: [makeUserMessage(1, "hi")],
      tools: undefined,
    },
    compat: baseCompat(),
  },
];

// --- Capture functions ------------------------------------------------------
function captureConvertMessages(c) {
  const result = convertMessages(c.model, c.context, c.compat, c.options);
  return JSON.parse(JSON.stringify(result));
}

class PayloadCaptured extends Error {}

// Empty SSE stream: lets the pipeline run to completion without network I/O.
function emptySseResponse() {
  return new Response("data: [DONE]\n\n", {
    status: 200,
    headers: { "content-type": "text/event-stream" },
  });
}

async function captureBuildParams(model, context) {
  let captured;
  const stream = streamSimple({ ...model, baseUrl: "http://127.0.0.1:9" }, context, {
    apiKey: "fake-key",
    fetch: async () => emptySseResponse(),
    onPayload: (payload) => {
      captured = payload;
    },
  });
  await stream.result();
  // `onPayload` is invoked (and awaited) before the fetch call, so `captured`
  // is assigned synchronously with `buildParams` — well before the stream
  // settles. Wait a couple of ticks to be safe against microtask ordering.
  for (let t = 0; t < 50 && !captured; t++) {
    await new Promise((r) => setTimeout(r, 10));
  }
  if (!captured) throw new Error("Expected payload capture");
  return JSON.parse(JSON.stringify(captured));
}

// --- Streaming scenarios (raw SSE chunks -> event tape + final message) -----
// Canned OpenAI chat-completions SSE bodies. Each is fed through real Pi's
// `streamSimple` via a fake `fetch`; the emitted event tape (type sequence +
// emission-stable fields, `partial` excluded — see anthropic_golden.rs) and the
// final `AssistantMessage` (byte-compared) are captured.
function sseEvent(obj) {
  return `data: ${JSON.stringify(obj)}\n\n`;
}

const streamScenarios = [
  {
    name: "text-stream",
    model: {
      ...getModel("together", "meta-llama/Llama-3.3-70B-Instruct-Turbo"),
      baseUrl: "http://127.0.0.1:9/v1",
      compat: undefined,
    },
    context: makeContext([makeUserMessage(1, "hi")]),
    chunks: [
      sseEvent({ id: "chatcmpl-1", object: "chat.completion.chunk", created: 1, model: "meta-llama/Llama-3.3-70B-Instruct-Turbo", choices: [{ index: 0, delta: { role: "assistant", content: "Hel" }, finish_reason: null }] }),
      sseEvent({ id: "chatcmpl-1", object: "chat.completion.chunk", created: 1, model: "meta-llama/Llama-3.3-70B-Instruct-Turbo", choices: [{ index: 0, delta: { content: "lo " }, finish_reason: null }] }),
      sseEvent({ id: "chatcmpl-1", object: "chat.completion.chunk", created: 1, model: "meta-llama/Llama-3.3-70B-Instruct-Turbo", choices: [{ index: 0, delta: { content: "world" }, finish_reason: null }] }),
      sseEvent({ id: "chatcmpl-1", object: "chat.completion.chunk", created: 1, model: "meta-llama/Llama-3.3-70B-Instruct-Turbo", choices: [{ index: 0, delta: {}, finish_reason: "stop" }], usage: { prompt_tokens: 12, completion_tokens: 5, total_tokens: 17 } }),
      "data: [DONE]\n\n",
    ],
  },
  {
    name: "tool-call-stream",
    model: {
      ...getModel("together", "meta-llama/Llama-3.3-70B-Instruct-Turbo"),
      baseUrl: "http://127.0.0.1:9/v1",
      compat: undefined,
    },
    context: makeContext([makeUserMessage(1, "use the tool")], [makeTool("base_tool")]),
    chunks: [
      sseEvent({ id: "chatcmpl-2", object: "chat.completion.chunk", created: 1, model: "meta-llama/Llama-3.3-70B-Instruct-Turbo", choices: [{ index: 0, delta: { role: "assistant", tool_calls: [{ index: 0, id: "call_abc", type: "function", function: { name: "base_tool", arguments: "" } }] }, finish_reason: null }] }),
      sseEvent({ id: "chatcmpl-2", object: "chat.completion.chunk", created: 1, model: "meta-llama/Llama-3.3-70B-Instruct-Turbo", choices: [{ index: 0, delta: { tool_calls: [{ index: 0, function: { arguments: '{"value":' } }] }, finish_reason: null }] }),
      sseEvent({ id: "chatcmpl-2", object: "chat.completion.chunk", created: 1, model: "meta-llama/Llama-3.3-70B-Instruct-Turbo", choices: [{ index: 0, delta: { tool_calls: [{ index: 0, function: { arguments: '"x"}' } }] }, finish_reason: null }] }),
      sseEvent({ id: "chatcmpl-2", object: "chat.completion.chunk", created: 1, model: "meta-llama/Llama-3.3-70B-Instruct-Turbo", choices: [{ index: 0, delta: {}, finish_reason: "tool_calls" }], usage: { prompt_tokens: 30, completion_tokens: 9, total_tokens: 39 } }),
      "data: [DONE]\n\n",
    ],
  },
  {
    name: "thinking-reasoning",
    model: {
      ...getModel("deepseek", "deepseek-v4-flash"),
      baseUrl: "http://127.0.0.1:9/v1",
      compat: undefined,
    },
    context: makeContext([makeUserMessage(1, "think hard")]),
    chunks: [
      sseEvent({ id: "chatcmpl-3", object: "chat.completion.chunk", created: 1, model: "deepseek-v4-flash", choices: [{ index: 0, delta: { role: "assistant", reasoning_content: "Let me " }, finish_reason: null }] }),
      sseEvent({ id: "chatcmpl-3", object: "chat.completion.chunk", created: 1, model: "deepseek-v4-flash", choices: [{ index: 0, delta: { reasoning_content: "think carefully" }, finish_reason: null }] }),
      sseEvent({ id: "chatcmpl-3", object: "chat.completion.chunk", created: 1, model: "deepseek-v4-flash", choices: [{ index: 0, delta: { content: "Answer: " }, finish_reason: null }] }),
      sseEvent({ id: "chatcmpl-3", object: "chat.completion.chunk", created: 1, model: "deepseek-v4-flash", choices: [{ index: 0, delta: { content: "42" }, finish_reason: null }] }),
      sseEvent({ id: "chatcmpl-3", object: "chat.completion.chunk", created: 1, model: "deepseek-v4-flash", choices: [{ index: 0, delta: {}, finish_reason: "stop" }], usage: { prompt_tokens: 10, completion_tokens: 12, total_tokens: 22 } }),
      "data: [DONE]\n\n",
    ],
  },
];

function sseBody(chunks) {
  return chunks.join("");
}

async function captureStream(s) {
  const events = [];
  const body = sseBody(s.chunks);
  const stream = streamSimple({ ...s.model, baseUrl: "http://127.0.0.1:9" }, s.context, {
    apiKey: "fake-key",
    fetch: async () =>
      new Response(body, {
        status: 200,
        headers: { "content-type": "text/event-stream" },
      }),
  });
  for await (const event of stream) {
    const { partial, ...rest } = event;
    void partial;
    events.push(rest);
  }
  const final = await stream.result();
  final.timestamp = 0;
  return { sseBody: body, events, final: JSON.parse(JSON.stringify(final)) };
}

// --- Model scenarios for the buildParams (getCompat) path -------------------
// Models that exercise distinct detectCompat branches.
const modelScenarios = [
  {
    name: "deepseek",
    model: {
      ...getModel("deepseek", "deepseek-v4-flash"),
      baseUrl: "http://127.0.0.1:9/v1",
      compat: undefined,
    },
    context: makeContext(
      [
        makeUserMessage(1),
        makeAssistantToolCall(),
        makeToolResult("call_1", "base_tool", [{ type: "text", text: "done" }], undefined),
        makeUserMessage(4, "thanks"),
      ],
      [makeTool("base_tool")]
    ),
  },
  {
    name: "together",
    model: {
      ...getModel("together", "meta-llama/Llama-3.3-70B-Instruct-Turbo"),
      baseUrl: "http://127.0.0.1:9/v1",
      compat: undefined,
    },
    context: makeContext([makeUserMessage(1, "hi")], [makeTool("base_tool")]),
  },
  {
    name: "zai",
    model: {
      ...getModel("zai", "glm-4.7"),
      baseUrl: "http://127.0.0.1:9/v1",
      compat: undefined,
    },
    context: makeContext([makeUserMessage(1, "hi")]),
  },
  {
    name: "openrouter-anthropic",
    model: {
      ...getModel("openrouter", "anthropic/claude-3-haiku"),
      baseUrl: "http://127.0.0.1:9/v1",
      compat: undefined,
    },
    context: makeContext([makeUserMessage(1, "hi")], [makeTool("base_tool")]),
  },
  {
    name: "nvidia",
    model: {
      ...getModel("nvidia", "meta/llama-3.1-70b-instruct"),
      baseUrl: "http://127.0.0.1:9/v1",
      compat: undefined,
    },
    context: makeContext([makeUserMessage(1, "hi")], [makeTool("base_tool")]),
  },
  {
    name: "moonshot-kimi",
    model: {
      ...getModel("moonshotai", "kimi-k2-0711-preview"),
      baseUrl: "http://127.0.0.1:9/v1",
      compat: undefined,
    },
    context: makeContext([makeUserMessage(1, "hi")], [makeTool("base_tool")]),
  },
];

// --- Write / check ----------------------------------------------------------
mkdirSync(outDir, { recursive: true });
const records = [];

// Section 1: convertMessages
for (const c of convertCases) {
  let payload;
  try {
    payload = captureConvertMessages(c);
  } catch (error) {
    throw new Error(`convertMessages case ${c.name} failed: ${error.message}`);
  }
  records.push({ section: "convertMessages", name: c.name, payload });
}

// Section 2: buildParams (getCompat + convertTools + normalizeToolCallId)
for (const s of modelScenarios) {
  let payload;
  try {
    payload = await captureBuildParams(s.model, s.context);
  } catch (error) {
    throw new Error(`buildParams case ${s.name} failed: ${error.stack}`);
  }
  records.push({ section: "buildParams", name: s.name, payload });
}

// Section 3: stream (raw SSE chunks -> event tape + final message)
for (const s of streamScenarios) {
  let payload;
  try {
    payload = await captureStream(s);
  } catch (error) {
    throw new Error(`stream case ${s.name} failed: ${error.stack}`);
  }
  records.push({ section: "stream", name: s.name, payload });
}

const outFile = join(outDir, "cases.jsonl");
const lines = records.map((r) => JSON.stringify(r));

if (checkMode) {
  if (!existsSync(outFile)) {
    console.error("--check: fixture missing:", outFile);
    process.exit(1);
  }
  const existing = readFileSync(outFile, "utf8").trim().split("\n").filter(Boolean);
  if (existing.length !== lines.length) {
    console.error(
      `--check: count mismatch. generated ${lines.length}, existing ${existing.length}`,
    );
    process.exit(1);
  }
  let failed = 0;
  for (let i = 0; i < lines.length; i++) {
    if (lines[i] !== existing[i]) {
      failed++;
      const a = JSON.parse(lines[i]);
      const b = JSON.parse(existing[i]);
      console.error(
        `--check: record ${i} mismatch: ${a.section}/${a.name} vs ${b.section}/${b.name}`,
      );
    }
  }
  if (failed > 0) {
    console.error(`--check: ${failed}/${lines.length} records drifted`);
    process.exit(1);
  }
  console.log(`--check: openai-completions oracle green (${lines.length} records)`);
} else {
  writeFileSync(outFile, lines.join("\n") + "\n");
  console.log(`wrote ${lines.length} records to ${outFile}`);
}
