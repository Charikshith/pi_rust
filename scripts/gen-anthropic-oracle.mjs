#!/usr/bin/env node
// Generate authentic golden fixtures by driving Pi's REAL Anthropic Messages
// adapter fully offline (no network) via an injected fake `options.client`.
//
// For each scenario we run the real `stream(model, context, options)`, collect
// the full event tape via `for await`, then `await s.result()` for the final
// AssistantMessage, and write three files per scenario into
//   tests/fixtures/pi/anthropic/
//     <name>.sse           raw SSE body bytes exactly as fed to the transport
//     <name>.request.json  { provider, modelId, context, options(minus client) }
//     <name>.expected.json { tape: [...events], final: AssistantMessage }
//     <name>.final.golden  the EXACT compact JSON.stringify(final) bytes Pi's
//                          adapter produced (timestamps normalized to 0) — used
//                          by the Rust golden test to assert byte-for-byte parity
//                          against Pi's LITERAL output (no deserialize/reserialize).
//
// Every `timestamp` field (final + nested partial/message/error) is normalized
// to the integer 0 (Date.now()-based, non-deterministic). `responseId` is stable
// (from the mocked message id) and preserved. Serialized compact via JSON.stringify
// so the bytes are the exact JS form. Deterministic + idempotent.
//
// Run: cd pirust && node scripts/gen-anthropic-oracle.mjs
// The SSE event arrays are copied verbatim from Pi's test files so the oracle
// is authentic:
//   ../pi/packages/ai/test/anthropic-sse-parsing.test.ts
//   ../pi/packages/ai/test/anthropic-cache-write-1h-cost.test.ts

import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const pirustRoot = dirname(here);
const piRoot = join(pirustRoot, "..", "pi");
const aiSrc = join(piRoot, "packages", "ai", "src");

// --- Dynamic import of Pi's REAL adapter + compat + typebox -----------------
const { stream: streamAnthropic } = await import(
  pathToFileURL(join(aiSrc, "api", "anthropic-messages.ts")).href
);
const { getModel } = await import(pathToFileURL(join(aiSrc, "compat.ts")).href);
const { Type } = await import(
  pathToFileURL(join(piRoot, "node_modules", "typebox", "build", "index.mjs")).href
);

// --- Fake client / SSE plumbing (verbatim from Pi's test helpers) -----------
function sseBody(events) {
  return events.map((e) => `event: ${e.event}\ndata: ${e.data}\n`).join("\n");
}
function createSseResponse(events) {
  return new Response(sseBody(events), {
    status: 200,
    headers: { "content-type": "text/event-stream" },
  });
}
function createFakeAnthropicClient(response) {
  return { messages: { create: () => ({ asResponse: async () => response }) } };
}

// --- Scenario 1 & 5 shared event array --------------------------------------
// `minimalAnthropicEvents` copied verbatim from anthropic-sse-parsing.test.ts:16-69
const minimalAnthropicEvents = [
  {
    event: "message_start",
    data: JSON.stringify({
      type: "message_start",
      message: {
        id: "msg_test",
        usage: {
          input_tokens: 12,
          output_tokens: 0,
          cache_read_input_tokens: 0,
          cache_creation_input_tokens: 0,
        },
      },
    }),
  },
  {
    event: "content_block_start",
    data: JSON.stringify({
      type: "content_block_start",
      index: 0,
      content_block: { type: "text", text: "" },
    }),
  },
  {
    event: "content_block_delta",
    data: JSON.stringify({
      type: "content_block_delta",
      index: 0,
      delta: { type: "text_delta", text: "Hello" },
    }),
  },
  {
    event: "content_block_stop",
    data: JSON.stringify({ type: "content_block_stop", index: 0 }),
  },
  {
    event: "message_delta",
    data: JSON.stringify({
      type: "message_delta",
      delta: { stop_reason: "end_turn" },
      usage: {
        input_tokens: 12,
        output_tokens: 5,
        cache_read_input_tokens: 0,
        cache_creation_input_tokens: 0,
      },
    }),
  },
  {
    event: "message_stop",
    data: JSON.stringify({ type: "message_stop" }),
  },
];

// --- Scenario 2: toolcall-repair --------------------------------------------
// malformedToolJsonDelta copied verbatim (anthropic-sse-parsing.test.ts:98). The
// original source uses String.raw with a literal TAB between "col1" and "col2";
// reproduced here by concatenating an explicit "\t".
const malformedToolJsonDelta =
  String.raw`{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\"A\H\",\"text\":\"col1` +
  "\t" +
  String.raw`col2\"}"}}`;

const toolcallRepairEvents = [
  {
    event: "message_start",
    data: JSON.stringify({
      type: "message_start",
      message: {
        id: "msg_test",
        usage: {
          input_tokens: 12,
          output_tokens: 0,
          cache_read_input_tokens: 0,
          cache_creation_input_tokens: 0,
        },
      },
    }),
  },
  {
    event: "content_block_start",
    data: JSON.stringify({
      type: "content_block_start",
      index: 0,
      content_block: { type: "tool_use", id: "toolu_test", name: "edit", input: {} },
    }),
  },
  { event: "content_block_delta", data: malformedToolJsonDelta },
  {
    event: "content_block_stop",
    data: JSON.stringify({ type: "content_block_stop", index: 0 }),
  },
  {
    event: "message_delta",
    data: JSON.stringify({
      type: "message_delta",
      delta: { stop_reason: "tool_use" },
      usage: {
        input_tokens: 12,
        output_tokens: 5,
        cache_read_input_tokens: 0,
        cache_creation_input_tokens: 0,
      },
    }),
  },
  {
    event: "message_stop",
    data: JSON.stringify({ type: "message_stop" }),
  },
];

// --- Scenario 3: refusal-error ----------------------------------------------
const refusalExplanation =
  "This request triggered restrictions on violative cyber content and was blocked under Anthropic's Usage Policy. To learn more, provide feedback, or request an exemption based on how you use Claude, visit our help center: https://support.claude.com/en/articles/14604842-real-time-cyber-safeguards-on-claude.";

const refusalEvents = [
  {
    event: "message_start",
    data: JSON.stringify({
      type: "message_start",
      message: {
        id: "msg_01XFUDYJgAACzvnptvVoYEL",
        usage: {
          input_tokens: 412,
          output_tokens: 0,
          cache_read_input_tokens: 0,
          cache_creation_input_tokens: 0,
        },
      },
    }),
  },
  {
    event: "message_delta",
    data: JSON.stringify({
      type: "message_delta",
      delta: {
        stop_reason: "refusal",
        stop_details: { type: "refusal", category: "cyber", explanation: refusalExplanation },
      },
      usage: {
        input_tokens: 412,
        output_tokens: 0,
        cache_read_input_tokens: 0,
        cache_creation_input_tokens: 0,
      },
    }),
  },
  {
    event: "message_stop",
    data: JSON.stringify({ type: "message_stop" }),
  },
];

// --- Scenario 4: cache-write-1h ---------------------------------------------
// eventsWithCacheCreation copied verbatim from anthropic-cache-write-1h-cost.test.ts:18-57
function eventsWithCacheCreation(cacheCreation) {
  const startUsage = {
    input_tokens: 100,
    output_tokens: 0,
    cache_read_input_tokens: 0,
    cache_creation_input_tokens: 1_000_000,
  };
  if (cacheCreation) startUsage.cache_creation = cacheCreation;
  return [
    {
      event: "message_start",
      data: JSON.stringify({ type: "message_start", message: { id: "msg_test", usage: startUsage } }),
    },
    {
      event: "content_block_start",
      data: JSON.stringify({ type: "content_block_start", index: 0, content_block: { type: "text", text: "" } }),
    },
    {
      event: "content_block_delta",
      data: JSON.stringify({ type: "content_block_delta", index: 0, delta: { type: "text_delta", text: "Hi" } }),
    },
    { event: "content_block_stop", data: JSON.stringify({ type: "content_block_stop", index: 0 }) },
    {
      event: "message_delta",
      data: JSON.stringify({
        type: "message_delta",
        delta: { stop_reason: "end_turn" },
        usage: {
          input_tokens: 100,
          output_tokens: 5,
          cache_read_input_tokens: 0,
          cache_creation_input_tokens: 1_000_000,
        },
      }),
    },
    { event: "message_stop", data: JSON.stringify({ type: "message_stop" }) },
  ];
}

// --- Scenario 5: no-usage-in-delta ------------------------------------------
// minimalAnthropicEvents with the message_delta rewritten to omit `usage`
// (anthropic-sse-parsing.test.ts:227-253 "treats message_delta without usage as
//  no-op usage accumulation").
const noUsageInDeltaEvents = minimalAnthropicEvents.map((event) =>
  event.event === "message_delta"
    ? {
        event: "message_delta",
        data: JSON.stringify({ type: "message_delta", delta: { stop_reason: "end_turn" } }),
      }
    : event,
);

// --- Scenario definitions ---------------------------------------------------
const simpleContext = { messages: [{ role: "user", content: "Say hello.", timestamp: 0 }] };

const scenarios = [
  {
    name: "text-basic",
    modelId: "claude-haiku-4-5",
    context: simpleContext,
    events: minimalAnthropicEvents,
  },
  {
    name: "toolcall-repair",
    modelId: "claude-haiku-4-5",
    context: {
      messages: [{ role: "user", content: "Use the edit tool.", timestamp: 0 }],
      tools: [
        {
          name: "edit",
          description: "Edit a file.",
          parameters: Type.Object({ path: Type.String(), text: Type.String() }),
        },
      ],
    },
    events: toolcallRepairEvents,
  },
  {
    name: "refusal-error",
    modelId: "claude-fable-5",
    context: { messages: [{ role: "user", content: "blocked request", timestamp: 0 }] },
    events: refusalEvents,
  },
  {
    name: "cache-write-1h",
    modelId: "claude-opus-4-8",
    context: { messages: [{ role: "user", content: "hi", timestamp: 0 }] },
    events: eventsWithCacheCreation({
      ephemeral_5m_input_tokens: 600_000,
      ephemeral_1h_input_tokens: 400_000,
    }),
  },
  {
    name: "no-usage-in-delta",
    modelId: "claude-haiku-4-5",
    context: simpleContext,
    events: noUsageInDeltaEvents,
  },
];

// --- Timestamp normalization ------------------------------------------------
// Recursively set any `timestamp` key to the integer 0. Covers final.timestamp
// and every nested partial/message/error.timestamp. `responseId` is left intact.
function normalizeTimestamps(value, seen = new Set()) {
  if (value === null || typeof value !== "object") return;
  if (seen.has(value)) return; // shared references (partial === final === output)
  seen.add(value);
  if (Array.isArray(value)) {
    for (const item of value) normalizeTimestamps(item, seen);
    return;
  }
  for (const key of Object.keys(value)) {
    if (key === "timestamp") value[key] = 0;
    else normalizeTimestamps(value[key], seen);
  }
}

// --- Run --------------------------------------------------------------------
const outDir = join(pirustRoot, "tests", "fixtures", "pi", "anthropic");
mkdirSync(outDir, { recursive: true });

const finals = {};
for (const scenario of scenarios) {
  const { name, modelId, context, events } = scenario;
  const model = getModel("anthropic", modelId);
  const response = createSseResponse(events);
  const s = streamAnthropic(model, context, { client: createFakeAnthropicClient(response) });

  const tape = [];
  for await (const ev of s) tape.push(ev);
  const final = await s.result();

  // Normalize the combined structure (tape + final share the mutated output ref).
  const expected = { tape, final };
  normalizeTimestamps(expected);

  const request = { provider: "anthropic", modelId, context, options: {} };

  writeFileSync(join(outDir, `${name}.sse`), sseBody(events));
  writeFileSync(join(outDir, `${name}.request.json`), JSON.stringify(request));
  writeFileSync(join(outDir, `${name}.expected.json`), JSON.stringify(expected));
  // Standalone literal-bytes golden for the final message. `final` was mutated
  // in place by normalizeTimestamps(expected) above, so its timestamps are 0.
  // These are the EXACT bytes Pi's adapter emitted for the final AssistantMessage.
  writeFileSync(join(outDir, `${name}.final.golden`), JSON.stringify(final));

  finals[name] = final;
  const types = tape.map((e) => e.type);
  console.log(`wrote ${name}: tape types = [${types.join(", ")}]`);
}

// Emit the two finals the orchestrator wants to eyeball.
console.log("\n=== cache-write-1h final ===");
console.log(JSON.stringify(finals["cache-write-1h"]));
console.log("\n=== toolcall-repair final ===");
console.log(JSON.stringify(finals["toolcall-repair"]));
