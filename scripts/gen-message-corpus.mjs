#!/usr/bin/env node
// Build a byte-compat corpus of real pi-ai Messages from Pi session fixtures.
//
// Reads the session JSONL fixtures from the sibling Pi source repo, extracts every
// inner `message` whose role is part of the pi-ai `Message` union (user / assistant /
// toolResult), and writes one canonical `JSON.stringify(message)` per line to
// tests/fixtures/pi/messages.corpus.jsonl. The pirust golden test round-trips each
// line and asserts byte-identity.
//
// `bashExecution` messages are a coding-agent/agent-core variant (not in
// packages/ai/types.ts) and are intentionally skipped here — they belong to feat-003.
//
// Run: `node scripts/gen-message-corpus.mjs` (`--check` fails on drift).

import { readFileSync, writeFileSync, existsSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const check = process.argv.includes("--check");

// Source fixtures live in the sibling Pi repo (../pi). Corpus regeneration requires it.
const srcDir = join(root, "..", "pi", "packages", "coding-agent", "test", "fixtures");
const sources = ["before-compaction.jsonl", "large-session.jsonl"];
const PI_AI_ROLES = new Set(["user", "assistant", "toolResult"]);

if (!existsSync(srcDir)) {
  console.error(`Pi source fixtures not found at ${srcDir}; corpus cannot be regenerated.`);
  process.exit(check ? 0 : 1); // don't fail --check when source is simply absent
}

const skipped = {};
const out = [];
for (const file of sources) {
  const path = join(srcDir, file);
  if (!existsSync(path)) continue;
  for (const line of readFileSync(path, "utf8").trim().split("\n")) {
    const entry = JSON.parse(line);
    if (entry.type !== "message") continue;
    const role = entry.message?.role;
    if (!PI_AI_ROLES.has(role)) {
      skipped[role] = (skipped[role] || 0) + 1;
      continue;
    }
    out.push(JSON.stringify(entry.message));
  }
}

const corpus = out.join("\n") + "\n";
const dest = join(root, "tests", "fixtures", "pi", "messages.corpus.jsonl");
const existing = existsSync(dest) ? readFileSync(dest, "utf8") : null;

if (existing === corpus) {
  console.log(`corpus up to date (${out.length} messages)`);
} else if (check) {
  console.error("DRIFT: messages.corpus.jsonl is stale; run node scripts/gen-message-corpus.mjs");
  process.exit(1);
} else {
  writeFileSync(dest, corpus);
  console.log(`wrote ${out.length} messages; skipped (non-pi-ai roles):`, skipped);
}
