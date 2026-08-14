#!/usr/bin/env node
// Generate golden files for byte-compat tests.
//
// For each vendored Pi fixture (tests/fixtures/pi/*.json), emit a sibling `.golden`
// containing the compact `JSON.stringify(JSON.parse(fixture))` — the exact bytes JS
// produces for that data. The pirust golden test asserts our re-serialization matches
// this byte-for-byte. Run: `node scripts/gen-golden.mjs` (add `--check` to fail on drift).

import { readdirSync, readFileSync, writeFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const dir = join(root, "tests", "fixtures", "pi");
const check = process.argv.includes("--check");

let changed = 0;
for (const file of readdirSync(dir)) {
  if (!file.endsWith(".json")) continue;
  const text = readFileSync(join(dir, file), "utf8");
  const golden = JSON.stringify(JSON.parse(text)); // compact, JS-canonical
  const out = join(dir, file.replace(/\.json$/, ".golden"));
  let existing = null;
  try {
    existing = readFileSync(out, "utf8");
  } catch {
    /* not yet generated */
  }
  if (existing !== golden) {
    changed++;
    if (check) {
      console.error(`DRIFT: ${file} golden is stale`);
    } else {
      writeFileSync(out, golden);
      console.log(`wrote ${file.replace(/\.json$/, ".golden")}`);
    }
  }
}

if (check && changed > 0) {
  console.error(`${changed} golden file(s) out of date; run: node scripts/gen-golden.mjs`);
  process.exit(1);
}
if (!check && changed === 0) console.log("goldens up to date");
