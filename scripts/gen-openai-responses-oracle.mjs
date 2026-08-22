#!/usr/bin/env node
// Generate authentic golden fixtures by driving Pi's REAL `openai-responses`
// adapter (and its shared conversion layer) fully offline.
//
// Three capture paths, all running Pi's actual TS source:
//
// 1. `convertResponsesMessages(model, context, allowedToolCallProviders, options)`
//    — exported from `packages/ai/src/api/openai-responses-shared.ts`. Pins the
//    input-item conversion: user/assistant/toolResult roles, text signatures
//    (versioned + legacy), pipe-separated tool-call ids, foreign tool-call item ids,
//    grammar custom-tool calls, deferred-tool additional_tools / tool_search placement.
//
// 2. `convertResponsesTools(tools, options)` — pins the tool-definition conversion:
//    grammar custom tools and JSON-schema strict/function tools.
//
// 3. `streamSimple(model, context, { apiKey, onPayload, fetch })` — exercises
//    `getCompat` + `buildParams` (which calls convertResponsesMessages +
//    convertResponsesTools) and, with a canned SSE `fetch`, the full
//    `processResponsesStream` state machine → event tape + final message.
//
// Deterministic + idempotent. Run:
//   node scripts/gen-openai-responses-oracle.mjs
//   node scripts/gen-openai-responses-oracle.mjs --check

import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const pirustRoot = dirname(here);
const piRoot = join(pirustRoot, "..", "pi");
const aiSrc = join(piRoot, "packages", "ai", "src");
const outDir = join(pirustRoot, "tests", "fixtures", "pi", "openai-responses");

const checkMode = process.argv.includes("--check");

const { convertResponsesMessages, convertResponsesTools } = await import(
  pathToFileURL(join(aiSrc, "api", "openai-responses-shared.ts")).href
);
const { streamSimple } = await import(
  pathToFileURL(join(aiSrc, "api", "openai-responses.ts")).href
);
const { getBuiltinModel } = await import(
  pathToFileURL(join(aiSrc, "providers", "all.ts")).href
);
const { Type } = await import(
  pathToFileURL(join(piRoot, "node_modules", "typebox", "build", "index.mjs")).href
);

// --- Fixture builders -------------------------------------------------------
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
    api: "openai-responses",
    provider: "openai",
    model: "gpt-5.2",
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

function makeContext(messages, tools, systemPrompt) {
  return { systemPrompt, messages, tools };
}

function openaiModel(id = "gpt-5.2") {
  const m = getBuiltinModel("openai", id);
  return { ...m, baseUrl: "http://127.0.0.1:9/v1" };
}

const OPENAI_TOOL_CALL_PROVIDERS = new Set(["openai", "openai-codex", "opencode"]);

// --- Conversion scenarios ---------------------------------------------------
const convertCases = [
  {
    name: "basic-user-assistant",
    model: openaiModel("gpt-5.2"),
    context: makeContext([
      makeUserMessage(1, "hi"),
      makeAssistantText("on it"),
      makeUserMessage(2, "go"),
    ]),
  },
  {
    name: "text-signature-versioned",
    model: openaiModel("gpt-5.2"),
    context: makeContext([
      makeUserMessage(1),
      {
        role: "assistant",
        content: [{ type: "text", text: "answer", textSignature: JSON.stringify({ v: 1, id: "msg_123", phase: "final_answer" }) }],
        api: "openai-responses",
        provider: "openai",
        model: "gpt-5.2",
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
      },
    ]),
  },
  {
    name: "text-signature-legacy",
    model: openaiModel("gpt-5.2"),
    context: makeContext([
      makeUserMessage(1),
      {
        role: "assistant",
        content: [{ type: "text", text: "answer", textSignature: "legacy-id" }],
        api: "openai-responses",
        provider: "openai",
        model: "gpt-5.2",
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
      },
    ]),
  },
  {
    name: "text-signature-cross-model-dropped",
    model: openaiModel("gpt-5.2"),
    context: makeContext([
      makeUserMessage(1),
      makeAssistantText("answer", {
        content: [{ type: "text", text: "answer", textSignature: JSON.stringify({ v: 1, id: "msg_999", phase: "final_answer" }) }],
      }),
    ]),
  },
  {
    name: "tool-calls",
    model: openaiModel("gpt-5.2"),
    context: makeContext(
      [
        makeUserMessage(1),
        makeAssistantToolCall("call_abc|fc_123", "base_tool", { value: "x" }),
        makeToolResult("call_abc|fc_123", "base_tool", [{ type: "text", text: "done" }], undefined),
        makeUserMessage(4),
      ],
      [makeTool("base_tool")]
    ),
  },
  {
    name: "foreign-tool-call-id",
    model: openaiModel("gpt-5.2"),
    context: makeContext([
      makeUserMessage(1),
      makeAssistantToolCall("call_abc|" + "x".repeat(80), "base_tool", { value: "x" }, { provider: "anthropic", api: "anthropic-messages" }),
      makeToolResult("call_abc|" + "x".repeat(80), "base_tool", [{ type: "text", text: "done" }], undefined),
    ]),
  },
  {
    name: "tool-result-images",
    model: openaiModel("gpt-5.2"),
    context: makeContext([
      makeUserMessage(1),
      makeAssistantToolCall(),
      makeToolResult("call_1", "base_tool", [
        { type: "text", text: "here" },
        { type: "image", data: "AAAA", mimeType: "image/png" },
      ], undefined),
    ]),
  },
  {
    name: "system-prompt-developer-role",
    model: openaiModel("gpt-5.2"),
    context: makeContext([makeUserMessage(1, "hi")], undefined, "You are helpful."),
  },
  {
    name: "deferred-additional-tools",
    model: openaiModel("gpt-5.2"),
    context: makeContext(
      [
        makeUserMessage(1),
        makeAssistantToolCall(),
        makeToolResult("call_1", "base_tool", [{ type: "text", text: "done" }], ["late_tool"]),
        makeUserMessage(4),
      ],
      [makeTool("base_tool"), makeTool("late_tool")]
    ),
    options: { deferredToolsMode: "additional-tools" },
  },
];

// --- Tool conversion scenarios ---------------------------------------------
const toolCases = [
  {
    name: "plain-tools",
    tools: [makeTool("base_tool"), makeTool("late_tool")],
    options: { supportsStrictMode: true },
  },
  {
    name: "grammar-tool",
    tools: [
      {
        name: "grammar_tool",
        description: "Grammar tool",
        parameters: Type.Object({ value: Type.String() }),
        constrainedSampling: {
          type: "grammar",
          variants: { openai_lark: "start: \"hello\"" },
        },
      },
    ],
    options: { supportsOpenAIGrammarTools: true },
  },
];

// --- Streaming scenarios ----------------------------------------------------
function sseEvent(obj) {
  return `data: ${JSON.stringify(obj)}\n\n`;
}

const streamScenarios = [
  {
    name: "text-stream",
    model: openaiModel("gpt-5.2"),
    context: makeContext([makeUserMessage(1, "hi")]),
    chunks: [
      sseEvent({ type: "response.created", response: { id: "resp_1", status: "in_progress" } }),
      sseEvent({ type: "response.output_item.added", output_index: 0, item: { type: "message", id: "msg_1", role: "assistant", status: "in_progress", content: [] } }),
      sseEvent({ type: "response.output_text.delta", output_index: 0, delta: "Hel" }),
      sseEvent({ type: "response.output_text.delta", output_index: 0, delta: "lo " }),
      sseEvent({ type: "response.output_text.delta", output_index: 0, delta: "world" }),
      sseEvent({ type: "response.output_item.done", output_index: 0, item: { type: "message", id: "msg_1", role: "assistant", status: "completed", phase: "final_answer", content: [{ type: "output_text", text: "Hello world" }] } }),
      sseEvent({ type: "response.completed", response: { id: "resp_1", status: "completed", usage: { input_tokens: 12, output_tokens: 5, total_tokens: 17, input_tokens_details: { cached_tokens: 0 } } } }),
      "data: [DONE]\n\n",
    ],
  },
  {
    name: "tool-call-stream",
    model: openaiModel("gpt-5.2"),
    context: makeContext([makeUserMessage(1, "use the tool")], [makeTool("base_tool")]),
    chunks: [
      sseEvent({ type: "response.created", response: { id: "resp_2", status: "in_progress" } }),
      sseEvent({ type: "response.output_item.added", output_index: 0, item: { type: "function_call", id: "fc_1", call_id: "call_1", name: "base_tool", arguments: "" } }),
      sseEvent({ type: "response.function_call_arguments.delta", output_index: 0, delta: "{\"value\":" }),
      sseEvent({ type: "response.function_call_arguments.delta", output_index: 0, delta: "\"x\"}" }),
      sseEvent({ type: "response.function_call_arguments.done", output_index: 0, arguments: "{\"value\":\"x\"}" }),
      sseEvent({ type: "response.output_item.done", output_index: 0, item: { type: "function_call", id: "fc_1", call_id: "call_1", name: "base_tool", arguments: "{\"value\":\"x\"}" } }),
      sseEvent({ type: "response.completed", response: { id: "resp_2", status: "completed", usage: { input_tokens: 30, output_tokens: 9, total_tokens: 39, input_tokens_details: { cached_tokens: 0 } } } }),
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
  const stream = streamSimple({ ...s.model, baseUrl: "http://127.0.0.1:9/v1" }, s.context, {
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

// --- Write / check ----------------------------------------------------------
mkdirSync(outDir, { recursive: true });
const records = [];

for (const c of convertCases) {
  let payload;
  try {
    const messages = convertResponsesMessages(
      c.model,
      c.context,
      OPENAI_TOOL_CALL_PROVIDERS,
      c.options,
    );
    payload = JSON.parse(JSON.stringify(messages));
  } catch (error) {
    throw new Error(`convertMessages case ${c.name} failed: ${error.message}`);
  }
  records.push({ section: "convertMessages", name: c.name, payload });
}

for (const c of toolCases) {
  let payload;
  try {
    payload = JSON.parse(JSON.stringify(convertResponsesTools(c.tools, c.options)));
  } catch (error) {
    throw new Error(`convertTools case ${c.name} failed: ${error.message}`);
  }
  records.push({ section: "convertTools", name: c.name, payload });
}

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
    console.error(`--check: count mismatch. generated ${lines.length}, existing ${existing.length}`);
    process.exit(1);
  }
  let failed = 0;
  for (let i = 0; i < lines.length; i++) {
    if (lines[i] !== existing[i]) {
      failed++;
      const a = JSON.parse(lines[i]);
      const b = JSON.parse(existing[i]);
      console.error(`--check: record ${i} mismatch: ${a.section}/${a.name} vs ${b.section}/${b.name}`);
    }
  }
  if (failed > 0) {
    console.error(`--check: ${failed}/${lines.length} records drifted`);
    process.exit(1);
  }
  console.log(`--check: openai-responses oracle green (${lines.length} records)`);
} else {
  writeFileSync(outFile, lines.join("\n") + "\n");
  console.log(`wrote ${lines.length} records to ${outFile}`);
}
