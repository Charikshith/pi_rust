#!/usr/bin/env node
// Oracle for the 0.84.2 v4 session storage (packages/agent/src/harness/session/jsonl/storage.ts).
//
// Drives Pi's REAL JsonlSessionStorage against a byte-recording mock FileSystem and
// records, per scenario, the exact file bytes on disk at each checkpoint plus the
// error strings it throws. The Rust port (v4/storage.rs) must reproduce the same
// bytes and errors for the same operation sequence.
//
// The mock FS records every writeFile/appendFile/renameFile call's effect on a
// per-path string buffer, so the "bytes" a scenario asserts are the REAL bytes Pi's
// storage wrote — torn-tail repair, atomic publish, fork staging all included.
//
// Run:  node scripts/gen-v4-storage-oracle.mjs [--check]

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

const storage = await import(pathToFileURL(join(AGENT_SRC, "harness", "session", "jsonl", "storage.ts")).href);
const codec = await import(pathToFileURL(join(AGENT_SRC, "harness", "session", "jsonl", "codec.ts")).href);

const records = [];
const push = (r) => records.push(r);

// -- byte-recording mock FileSystem -----------------------------------------
// Implements the subset of JsonlSessionRepoFileSystem storage.ts uses.
function mockFs() {
  const files = new Map(); // path -> string
  const log = [];
  return {
    files,
    log,
    fs: {
      async absolutePath(path) {
        return { ok: true, value: path };
      },
      async joinPath(parts) {
        return { ok: true, value: parts.join("/") };
      },
      async readTextFile(path) {
        const v = files.get(path);
        if (v === undefined) return { ok: false, error: { code: "not_found", message: `no such file ${path}` } };
        return { ok: true, value: v };
      },
      async writeFile(path, contents) {
        log.push(`writeFile(${path}, ${JSON.stringify(contents)})`);
        files.set(path, contents);
        return { ok: true, value: undefined };
      },
      async appendFile(path, contents) {
        log.push(`appendFile(${path}, ${JSON.stringify(contents)})`);
        files.set(path, (files.get(path) ?? "") + contents);
        return { ok: true, value: undefined };
      },
      async renameFile(from, to) {
        log.push(`renameFile(${from}, ${to})`);
        const v = files.get(from);
        if (v === undefined) return { ok: false, error: { code: "not_found", message: `no such file ${from}` } };
        files.set(to, v);
        files.delete(from);
        return { ok: true, value: undefined };
      },
      async fileInfo(path) {
        if (!files.has(path)) return { ok: false, error: { code: "not_found", message: `no such file ${path}` } };
        return { ok: true, value: { mtimeMs: 1_700_000_000_000 } };
      },
    },
  };
}

// -- helpers ----------------------------------------------------------------
function userMessage(text) {
  return { role: "user", content: [{ type: "text", text }], timestamp: 1 };
}

async function runScenario(name, fn) {
  const m = mockFs();
  const path = "/sessions/session.jsonl";
  const header = { kind: "header", version: 4, id: "session", createdAt: 1_700_000_000_000, cwd: "/workspace/project" };
  try {
    const created = await storage.JsonlSessionStorage.create(m.fs, path, header);
    const extra = await fn(created, m);
    push({
      fn: "storage",
      name,
      finalBytes: m.files.get(path) ?? null,
      forkBytes: m.files.get("/sessions/fork.jsonl") ?? null,
      fsLog: m.log,
      extra: extra ?? null,
      error: null,
    });
  } catch (error) {
    push({
      fn: "storage",
      name,
      finalBytes: m.files.get(path) ?? null,
      forkBytes: m.files.get("/sessions/fork.jsonl") ?? null,
      fsLog: m.log,
      extra: null,
      error: {
        name: error.name,
        code: error.code ?? null,
        message: error.message ?? String(error),
      },
    });
  }
}

// Scenario: create + a sequence of append operations → the exact file bytes.
await runScenario("create-append", async (s, m) => {
  await s.appendEntry({ type: "custom", id: "entry-1", customType: "note", data: { value: 1 } }, "main");
  await s.createLane("thread", "entry-1");
  await s.appendEntry({ type: "custom", id: "entry-2", customType: "note" }, "thread");
  await s.appendRecord({
    type: "operation_started",
    id: "run",
    lane: "main",
    sourceLeafId: null,
    intent: { kind: "run", originalPrompt: [], initialMessages: [] },
  });
  await s.setName("Example");
  await s.setLabel("entry-1", "checkpoint");
  await s.moveLane("main", null);
});

// Scenario: torn-tail repair — a partial final line is dropped on load by
// atomically publishing the valid prefix (the kept-entry bytes, truncated to
// the last good line).
await runScenario("torn-tail-repair", async (s, m) => {
  await s.appendEntry({ type: "custom", id: "kept", customType: "note" }, "main");
  // Corrupt the file directly: append a partial JSON line after the kept entry.
  const good = m.files.get("/sessions/session.jsonl");
  m.files.set("/sessions/session.jsonl", good + "{\"kind\":\"entry\"");
  const before = m.files.get("/sessions/session.jsonl");
  // Loading must truncate the torn tail by publishing the valid prefix.
  const reloaded = await storage.JsonlSessionStorage.load(m.fs, "/sessions/session.jsonl");
  const after = m.files.get("/sessions/session.jsonl");
  return {
    beforeRepair: before,
    afterRepair: after,
    entries: (await reloaded.findEntries({ order: "oldestFirst" })).map((e) => e.id),
    fsLog: m.log,
  };
});

// Scenario: fork — a tree fork stages a new file then atomically renames it
// over the fork destination.
await runScenario("fork-tree", async (s, m) => {
  await s.appendEntry({ type: "custom", id: "entry-1", customType: "note" }, "main");
  await s.appendEntry({ type: "custom", id: "entry-2", customType: "note" }, "main");
  await s.setName("Source");
  const forkPath = "/sessions/fork.jsonl";
  const forkHeader = { kind: "header", version: 4, id: "fork", createdAt: 1_700_000_000_001, cwd: "/workspace/project" };
  await s.fork(forkPath, forkHeader, { scope: "tree" });
});

// Scenario: load-after-reopen returns identical state.
await runScenario("reopen", async (s, m) => {
  await s.appendEntry({ type: "custom", id: "entry-1", customType: "note" }, "main");
  await s.appendEntry({ type: "custom", id: "entry-2", customType: "note" }, "main");
  await s.setName("Example");
  await s.setLabel("entry-1", "label");
  const loaded = await storage.JsonlSessionStorage.load(m.fs, "/sessions/session.jsonl");
  return {
    lanes: await loaded.getLanes(),
    name: await loaded.getName(),
    label: await loaded.getLabel("entry-1"),
    entries: (await loaded.findEntries({ order: "oldestFirst" })).map((e) => e.id),
  };
});

// Scenario: getStats computes ledger statistics from usage records.
await runScenario("stats", async (s) => {
  await s.appendEntry({ type: "custom", id: "entry-1", customType: "note" }, "main");
  await s.appendRecord({
    type: "usage",
    id: "u1",
    lane: "main",
    cause: "assistant",
    runId: "run",
    entryId: "entry-1",
    attempt: 1,
    stopReason: "stop",
    usage: {
      input: 10,
      output: 20,
      cacheRead: 30,
      cacheWrite: 40,
      totalTokens: 100,
      cost: { input: 1, output: 2, cacheRead: 3, cacheWrite: 4, total: 10 },
    },
  });
  return { stats: await s.getStats() };
});

// Scenario: load rejects a malformed INTERIOR mutation without changing the file.
await runScenario("malformed-interior", async (s, m) => {
  await s.appendEntry({ type: "custom", id: "first", customType: "note" }, "main");
  await s.appendEntry({ type: "custom", id: "second", customType: "note" }, "main");
  const good = m.files.get("/sessions/session.jsonl");
  // Three mutation lines: header, e1, e2. Corrupt the middle one (e1).
  const [h, e1, e2] = good.split("\n");
  const corrupted = `${h}\nnot-json\n${e2}\n`;
  m.files.set("/sessions/session.jsonl", corrupted);
  const before = m.files.get("/sessions/session.jsonl");
  try {
    await storage.JsonlSessionStorage.load(m.fs, "/sessions/session.jsonl");
    return { before, after: m.files.get("/sessions/session.jsonl") };
  } catch (error) {
    return {
      before: before,
      after: m.files.get("/sessions/session.jsonl"),
      error: { name: error.name, code: error.code, message: error.message },
    };
  }
});

// Scenario: load repairs a valid final line missing its trailing newline.
await runScenario("unterminated-final-line", async (s, m) => {
  await s.appendEntry({ type: "custom", id: "first", customType: "note" }, "main");
  const good = m.files.get("/sessions/session.jsonl");
  m.files.set("/sessions/session.jsonl", good.replace(/\n$/, ""));
  const before = m.files.get("/sessions/session.jsonl");
  await storage.JsonlSessionStorage.load(m.fs, "/sessions/session.jsonl");
  return { before, after: m.files.get("/sessions/session.jsonl") };
});

// Scenario: invalid payload → no file mutation + error code.
await runScenario("invalid-payload", async (s) => {
  const cyclic = {};
  cyclic.self = cyclic;
  try {
    await s.appendEntry({ type: "custom", id: "bad", customType: "note", data: cyclic }, "main");
  } catch (error) {
    return { error: { name: error.name, code: error.code, message: error.message } };
  }
  return null;
});

const payload = records.map((r) => JSON.stringify(r)).join("\n") + "\n";
const dest = join(OUT, "storage.cases.jsonl");

// Timestamps are `Date.now()` at capture time — inherently non-deterministic.
// Normalize every `"timestamp":<digits>` to a fixed placeholder so the
// fixture is reproducible across runs; the golden test asserts shapes and the
// repair/fork byte contracts, never the wall-clock values.
const normalized = payload.replace(/(\\{1,4}?")timestamp\\{1,4}?":\d+/g, (m) => m.replace(/\d+$/, "0"));
const existing = existsSync(dest) ? readFileSync(dest, "utf8") : null;
if (existing === normalized) {
  console.log(`v4 storage oracle up to date (${records.length} records)`);
} else if (check) {
  console.error(`DRIFT: ${dest} is stale; run node scripts/gen-v4-storage-oracle.mjs`);
  process.exit(1);
} else {
  writeFileSync(dest, normalized);
  console.log(`wrote ${records.length} records`);
}
