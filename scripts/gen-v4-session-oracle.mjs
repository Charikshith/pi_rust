#!/usr/bin/env node
// Oracle for the 0.84.2 v4 session codec (packages/agent/src/harness/session/jsonl/codec.ts).
//
// Drives Pi's REAL codec functions (encodeHeader / parseHeader / encodeMutation /
// parseMutation / metadataFromHeader) through the source alias hook (the published
// dist/ is not built) and records, per case:
//   { name, kind, input, encoded, decoded, error? }
// The `encoded` string is the EXACT bytes Pi emits (JSON.stringify + "\n"), and
// `decoded` is the round-trip parse (JS-canonical, key order = JSON.parse). A Rust
// test replays the same inputs through the port and compares both sides.
//
// Run:  node scripts/gen-v4-session-oracle.mjs [--check]

import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { register } from "node:module";

const here = dirname(fileURLToPath(import.meta.url));
const piRoot = join(here, "..", "..", "pi", "packages");
const AGENT_SRC = join(piRoot, "agent", "src");
const AI_SRC = join(piRoot, "ai", "src");
const TELEM_SRC = join(piRoot, "telemetry", "src");
const OUT = join(here, "..", "tests", "fixtures", "pi", "agent", "v4");
const check = process.argv.includes("--check");
mkdirSync(OUT, { recursive: true });

const ROOTS = {
  "@earendil-works/pi-ai": pathToFileURL(join(AI_SRC, "index.ts")).href,
  "@earendil-works/pi-agent-core": pathToFileURL(join(AGENT_SRC, "index.ts")).href,
  "@earendil-works/pi-tui": pathToFileURL(join(piRoot, "tui", "src", "index.ts")).href,
  "@earendil-works/pi-telemetry": pathToFileURL(join(TELEM_SRC, "index.ts")).href,
};
register("data:text/javascript," + encodeURIComponent(`
import { existsSync } from "node:fs";
const ROOTS=${JSON.stringify(ROOTS)};
export async function resolve(specifier, context, nextResolve) {
  for (const [pkg, root] of Object.entries(ROOTS)) {
    if (specifier === pkg) return { url: root, shortCircuit: true };
    if (specifier.startsWith(pkg + "/")) {
      const rest = specifier.slice(pkg.length + 1);
      for (const cand of [rest + ".ts", rest + "/index.ts"]) {
        const u = new URL(cand, new URL("./", root));
        if (existsSync(fileURLToPath(u))) return { url: u.href, shortCircuit: true };
      }
      throw new Error("alias hook: no source file for " + specifier);
    }
  }
  return nextResolve(specifier, context);
}
`), import.meta.url);

const codec = await import(pathToFileURL(join(AGENT_SRC, "harness", "session", "jsonl", "codec.ts")).href);

const records = [];
const push = (r) => records.push(r);

// -- headers ----------------------------------------------------------------
const headers = [
  { name: "header-resolved-parent", header: { kind: "header", version: 4, id: "session", createdAt: 1_700_000_000_000, cwd: "/workspace/project", parentSessionId: "parent", metadata: { owner: "agent", nested: { enabled: true }, values: [1, null, "two"] } } },
  { name: "header-legacy-parent-path", header: { kind: "header", version: 4, id: "legacy-child", createdAt: 1_700_000_000_001, cwd: "/workspace/project", legacyParentSessionPath: "/sessions/missing-parent.jsonl" } },
  { name: "header-minimal", header: { kind: "header", version: 4, id: "s", createdAt: 100, cwd: "/" } },
];
for (const { name, header } of headers) {
  const encoded = codec.encodeHeader(header);
  const parsed = codec.parseHeader(encoded.trimEnd());
  push({ fn: "encodeHeader", name, input: header, encoded, parsed: parsed.ok ? parsed.value : null, parseError: parsed.ok ? null : String(parsed.error) });
  const meta = codec.metadataFromHeader(parsed.ok ? parsed.value : header, "/sessions/session.jsonl", 1_700_000_000_100);
  push({ fn: "metadataFromHeader", name, input: { header: parsed.ok ? parsed.value : header, path: "/sessions/session.jsonl", modifiedAt: 1_700_000_000_100 }, metadata: meta });
}

// -- mutation lines ---------------------------------------------------------
const mutations = [
  { name: "entry-lane-bound", mutation: { kind: "entry", lane: "main", entry: { type: "custom", id: "entry-1", seq: 1, parentId: null, timestamp: 100, customType: "note", data: { text: "hello" } } } },
  { name: "entry-no-lane", mutation: { kind: "entry", entry: { type: "custom", id: "entry-1", seq: 1, parentId: null, timestamp: 100, customType: "note" } } },
  { name: "record-operation-started", mutation: { kind: "record", record: { type: "operation_started", id: "run-1", seq: 1, lane: "main", timestamp: 100, sourceLeafId: null, intent: { kind: "run", originalPrompt: [], initialMessages: [] } } } },
  { name: "record-tool-started", mutation: { kind: "record", record: { type: "tool_started", id: "t-1", seq: 2, lane: "main", timestamp: 101, runId: "run-1", assistantEntryId: "e-1", toolIndex: 0, toolCallId: "call_1", toolName: "bash", effectiveArgs: { command: "ls" }, resultEntryId: "e-2", replay: "never" } } },
  { name: "record-usage", mutation: { kind: "record", record: { type: "usage", id: "u-1", seq: 3, lane: "main", timestamp: 102, usage: { input: 12, output: 5, cacheRead: 0, cacheWrite: 0, totalTokens: 17, cost: { input: 0.000012, output: 0.000025, cacheRead: 0, cacheWrite: 0, total: 0.000037 } }, cause: "assistant", runId: "run-1", entryId: "e-1", attempt: 1, stopReason: "stop" } } },
  { name: "lane-move", mutation: { kind: "lane", seq: 4, lane: "thread", leafId: "entry-1" } },
  { name: "lane-move-null", mutation: { kind: "lane", seq: 5, lane: "thread", leafId: null } },
  { name: "fact-name", mutation: { kind: "fact", seq: 6, fact: "name", name: "Example" } },
  { name: "fact-name-cleared", mutation: { kind: "fact", seq: 7, fact: "name", name: undefined } },
  { name: "fact-label", mutation: { kind: "fact", seq: 8, fact: "label", targetId: "entry-1", label: "checkpoint" } },
  { name: "fact-label-cleared", mutation: { kind: "fact", seq: 9, fact: "label", targetId: "entry-1", label: undefined } },
];
for (const { name, mutation } of mutations) {
  const encoded = codec.encodeMutation(mutation);
  const parsed = codec.parseMutation(encoded.trimEnd());
  push({ fn: "encodeMutation", name, input: mutation, encoded, parsed: parsed.ok ? parsed.value : null, parseError: parsed.ok ? null : String(parsed.error) });
}

// -- error cases ------------------------------------------------------------
const badLines = [
  { name: "syntax-error", line: "{" },
  { name: "unknown-mutation-kind", line: JSON.stringify({ kind: "unknown", seq: 1 }) },
  { name: "seq-zero", line: JSON.stringify({ kind: "lane", seq: 0, lane: "main", leafId: null }) },
  { name: "custom-no-customtype", line: JSON.stringify({ kind: "entry", type: "custom", id: "entry", parentId: null, seq: 1, timestamp: 1 }) },
  { name: "record-no-intent", line: JSON.stringify({ kind: "record", type: "operation_started", id: "run", lane: "main", seq: 1, timestamp: 1, sourceLeafId: null }) },
  { name: "record-finished-no-runid", line: JSON.stringify({ kind: "record", type: "operation_finished", id: "finish", lane: "main", seq: 1, timestamp: 1, outcome: "completed" }) },
  { name: "header-v3", line: JSON.stringify({ kind: "header", version: 3 }) },
  { name: "header-both-parents", line: JSON.stringify({ kind: "header", version: 4, id: "x", createdAt: 1, cwd: "/", parentSessionId: "a", legacyParentSessionPath: "b" }) },
];
for (const { name, line } of badLines) {
  const parsed = codec.parseMutation(line);
  push({ fn: "parseMutation-error", name, line, ok: parsed.ok, errorKind: parsed.ok ? null : parsed.error.kind, errorMessage: parsed.ok ? null : parsed.error.message });
}

const payload = records.map((r) => JSON.stringify(r)).join("\n") + "\n";
const dest = join(OUT, "codec.cases.jsonl");
const existing = existsSync(dest) ? readFileSync(dest, "utf8") : null;
if (existing === payload) {
  console.log(`v4 codec oracle up to date (${records.length} records)`);
} else if (check) {
  console.error(`DRIFT: ${dest} is stale; run node scripts/gen-v4-session-oracle.mjs`);
  process.exit(1);
} else {
  writeFileSync(dest, payload);
  console.log(`wrote ${records.length} records`);
}
