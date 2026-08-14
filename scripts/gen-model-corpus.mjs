#!/usr/bin/env node
// Build a corpus of every real Pi Model from the generated catalog.
//
// Reads packages/ai/src/providers/data/*.json from the sibling Pi repo (produced by
// `npm --prefix packages/ai run generate-models`, which fetches models.dev + provider
// registries) and writes one JS-canonical `JSON.stringify(model)` per line to
// tests/fixtures/pi/models.corpus.jsonl.
//
// NOTE: Model JSON key order is NOT canonical in Pi (it varies per provider/model), so
// the Rust test does a lossless SEMANTIC round-trip (order-independent), not byte-equal.
// Models are never persisted per-session, so byte-identity is neither achievable nor
// required — see docs/analysis/00-overview.md and feat-001 evidence.
//
// Run: `node scripts/gen-model-corpus.mjs` (`--check` fails on drift).

import { readFileSync, writeFileSync, existsSync, readdirSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const check = process.argv.includes("--check");
const dataDir = join(root, "..", "pi", "packages", "ai", "src", "providers", "data");

if (!existsSync(dataDir)) {
  console.error(`Pi model data not found at ${dataDir}.`);
  console.error("Generate it first: (in ../pi) npm --prefix packages/ai run generate-models");
  process.exit(check ? 0 : 1);
}

const out = [];
for (const file of readdirSync(dataDir).sort()) {
  if (!file.endsWith(".json")) continue;
  const models = JSON.parse(readFileSync(join(dataDir, file), "utf8"));
  for (const id of Object.keys(models)) {
    out.push(JSON.stringify(models[id]));
  }
}

const corpus = out.join("\n") + "\n";
const dest = join(root, "tests", "fixtures", "pi", "models.corpus.jsonl");
const existing = existsSync(dest) ? readFileSync(dest, "utf8") : null;

if (existing === corpus) {
  console.log(`model corpus up to date (${out.length} models)`);
} else if (check) {
  console.error("DRIFT: models.corpus.jsonl is stale; run node scripts/gen-model-corpus.mjs");
  process.exit(1);
} else {
  writeFileSync(dest, corpus);
  console.log(`wrote ${out.length} models`);
}
