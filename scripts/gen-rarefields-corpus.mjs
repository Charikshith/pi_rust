#!/usr/bin/env node
// Type-fidelity corpus for pi-ai Message fields that do NOT appear in the available real
// session fixtures (image content, textSignature, redacted, thoughtSignature, usage
// reasoning/cacheWrite1h, responseModel/responseId/diagnostics, addedToolNames, user
// string content).
//
// Each shape follows packages/ai/src/types.ts exactly; real values are used where they
// exist in the Pi repo (the test PNG, the Google signature). Serialization is done by
// real Node JSON.stringify. The Rust test does a lossless SEMANTIC round-trip
// (order-independent): this proves the pirust types faithfully represent these fields
// (names, camelCase, number formatting, optionality) with no loss.
//
// It does NOT assert Pi's persisted key ORDER for these fields — no real session contains
// them, and (as with Model) Pi's order for provider-produced fields is not canonical.
// That byte-order is pinned when the runtime produces + persists them (feat-002/003).
//
// Run: `node scripts/gen-rarefields-corpus.mjs` (`--check` fails on drift).

import { readFileSync, writeFileSync, existsSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const check = process.argv.includes("--check");

// Real base64 from Pi's own test asset when available; else a tiny placeholder.
const pngPath = join(root, "..", "pi", "packages", "ai", "test", "data", "red-circle.png");
const pngB64 = existsSync(pngPath) ? readFileSync(pngPath).toString("base64") : "iVBORw0KGgo=";
// Real Google thought signature used in Pi's test suite.
const thoughtSignature = "AAAAAAAAAAAAAAAAAAAAAA==";

const usageFull = (extra = {}) => ({
  input: 100,
  output: 200,
  cacheRead: 10,
  cacheWrite: 5,
  ...extra,
  totalTokens: 300,
  cost: { input: 0.000005, output: 0.0006, cacheRead: 0.00001, cacheWrite: 0.00002, total: 0.00063 },
});

const messages = [
  // user, bare string content
  { role: "user", content: "plain string content", timestamp: 1 },
  // user, image content (real PNG bytes)
  {
    role: "user",
    content: [
      { type: "text", text: "look at this" },
      { type: "image", data: pngB64, mimeType: "image/png" },
    ],
    timestamp: 2,
  },
  // assistant: text+textSignature, thinking+redacted, toolCall+thoughtSignature
  {
    role: "assistant",
    content: [
      { type: "text", text: "hi", textSignature: "sig-legacy-id" },
      { type: "thinking", thinking: "[Reasoning redacted]", thinkingSignature: "enc==", redacted: true },
      { type: "toolCall", id: "c1", name: "bash", arguments: { command: "echo hi" }, thoughtSignature },
    ],
    api: "google-generative-ai",
    provider: "google",
    model: "gemini-3-pro",
    usage: usageFull({ reasoning: 40, cacheWrite1h: 3 }),
    stopReason: "toolUse",
    timestamp: 3,
  },
  // assistant: responseModel + responseId + diagnostics
  {
    role: "assistant",
    content: [{ type: "text", text: "routed" }],
    api: "openai-completions",
    provider: "openrouter",
    model: "openrouter/auto",
    responseModel: "anthropic/claude-opus-4.8",
    responseId: "resp_abc123",
    diagnostics: [{ type: "provider_retry", timestamp: 123, error: { message: "429 rate limited", code: 429 } }],
    usage: usageFull(),
    stopReason: "stop",
    timestamp: 4,
  },
  // toolResult with addedToolNames + details
  {
    role: "toolResult",
    toolCallId: "c1",
    toolName: "base_tool",
    content: [{ type: "text", text: "done" }],
    details: { exitCode: 0 },
    addedToolNames: ["late_tool"],
    isError: false,
    timestamp: 5,
  },
];

const corpus = messages.map((m) => JSON.stringify(m)).join("\n") + "\n";
const dest = join(root, "tests", "fixtures", "pi", "rarefields.corpus.jsonl");
const existing = existsSync(dest) ? readFileSync(dest, "utf8") : null;

if (existing === corpus) {
  console.log(`rare-fields corpus up to date (${messages.length} messages)`);
} else if (check) {
  console.error("DRIFT: rarefields.corpus.jsonl is stale; run node scripts/gen-rarefields-corpus.mjs");
  process.exit(1);
} else {
  writeFileSync(dest, corpus);
  console.log(`wrote ${messages.length} messages`);
}
