#!/usr/bin/env node
// Oracle capture for the v4 **in-memory** session layer: Pi's real
// `InMemorySessionStorage`, `InMemorySessionRepo`, `Session`, and the context
// builders (`buildSessionContext` / `sessionEntryToContextMessages`) from
// `packages/agent/src/harness/session/`.
//
// Unlike the JSONL storage/repo, the in-memory layer has no FileSystem and no
// bytes — its behavior is the mutation/replay contract plus the context
// projection. We drive it directly and record normalized results (uuidv7 ids
// and Date.now() timestamps normalized) as lines of JSONL in
// `tests/fixtures/pi/agent/v4/memory.cases.jsonl`:
//   { name, body }
//
// Run `node scripts/gen-v4-memory-oracle.mjs` to (re)write, `--check` to compare.

import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { register } from "node:module";

const here = dirname(fileURLToPath(import.meta.url));
const piRoot = join(here, "..", "..", "pi", "packages");
const AGENT_SRC = join(piRoot, "agent", "src");
const AI_SRC = join(piRoot, "ai", "src");
const OUT = join(here, "..", "tests", "fixtures", "pi", "agent", "v4");
const check = process.argv.includes("--check");
mkdirSync(OUT, { recursive: true });

const ROOTS = {
  "@earendil-works/pi-ai": pathToFileURL(join(AI_SRC, "index.ts")).href,
  "@earendil-works/pi-agent-core": pathToFileURL(join(AGENT_SRC, "index.ts")).href,
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

const memMod = await import(
  pathToFileURL(join(AGENT_SRC, "harness", "session", "memory.ts")).href,
);
const ctxMod = await import(
  pathToFileURL(join(AGENT_SRC, "harness", "session", "context.ts")).href,
);
const sessionMod = await import(
  pathToFileURL(join(AGENT_SRC, "harness", "session", "session.ts")).href,
);

const FIXTURE = join(OUT, "memory.cases.jsonl");
const RECORDS = [];

// ---- normalization (determinism) ----------------------------------------
function normValue(v) {
  const j = JSON.stringify(v);
  return j
    .replace(/"(?:timestamp|createdAt)":\d+/g, (m) => m.replace(/\d+$/, "0"))
    .replace(/"[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[0-9a-f]{4}-[0-9a-f]{12}"/gi, '"<UUID>"');
}
function json(v) {
  return JSON.parse(normValue(v));
}
function record(name, body) {
  RECORDS.push({ name, body });
}

// deterministic id generator
class SeqIds {
  constructor() {
    this.n = 0;
  }
  next() {
    return `id-${this.n++}`;
  }
}
const mkUser = (t) => ({ role: "user", content: [{ type: "text", text: t }], timestamp: 0 });
const mkAsst = (model, provider, stop) => ({
  role: "assistant",
  content: [{ type: "text", text: "ok" }],
  api: "anthropic",
  provider,
  model,
  usage: { inputTokens: 1, outputTokens: 1, totalTokens: 2, inputCost: 0, outputCost: 0, totalCost: 0 },
  stopReason: stop,
  timestamp: 0,
});

// ---- Scenario 1: memory storage mutation/replay contract -----------------
{
  const storage = new memMod.InMemorySessionStorage({
    id: "mem",
    createdAt: 1000,
    parentSessionId: "parent",
  });
  const s = new sessionMod.Session(storage, { idGenerator: new SeqIds() });
  const out = {};
  out.lanes0 = json(await s.getLanes());
  await s.createLane("thread", null);
  const e1 = await s.appendMessage(mkUser("hi"));
  out.msgId = e1;
  out.lanes = json(await s.getLanes());
  out.entry = json(await s.getEntry(e1));
  await s.appendRecord({
    type: "operation_started",
    id: "run",
    lane: "thread",
    sourceLeafId: null,
    intent: { type: "run", originalPrompt: [], initialMessages: [], systemPromptOverride: null, resumeData: null },
  });
  out.records = json(await s.findRecords({ type: "operation_started" }));
  await s.setName("Example");
  out.name = json(await s.getName());
  await s.setLabel(e1, "checkpoint");
  out.label = json(await s.getLabel(e1));
  out.log = json(await s.getLog({}));
  out.stats = json(await s.getStats());
  record("memory-storage", out);
}

// ---- Scenario 2: memory repo create/open/list/delete/duplicate -----------
{
  const repo = new memMod.InMemorySessionRepo();
  const out = {};
  const created = await repo.create({ id: "a" });
  out.meta = json(await created.getMetadata());
  await created.setName("A");
  out.duplicate = await repo.create({ id: "a" }).then(() => "ok", (e) => e.code + ":" + e.message);
  const list = await repo.list();
  out.list = json(list.map((m) => m.id));
  const opened = await repo.open(list[0]);
  out.openedName = json(await opened.getName());
  await repo.delete(list[0]);
  out.afterDelete = json((await repo.list()).map((m) => m.id));
  out.gone = await repo.open({ id: "a", createdAt: 0, parentSessionId: undefined }).then(() => "ok", (e) => e.code + ":" + e.message);
  record("memory-repo", out);
}

// ---- Scenario 3: memory repo fork tree ------------------------------------
{
  const repo = new memMod.InMemorySessionRepo();
  const source = await repo.create({ id: "source" });
  await source.appendMessage(mkUser("one"));
  await source.appendMessage(mkUser("two"));
  const sourceMeta = await source.getMetadata();
  const fork = await repo.fork(sourceMeta, { scope: "tree", id: "forked" });
  const fMeta = await fork.getMetadata();
  const out = {
    forkId: fMeta.id,
    parent: fMeta.parentSessionId,
    entries: (await fork.findEntries({})).length,
    lanes: json(await fork.getLanes()),
  };
  record("memory-repo-fork", out);
}

// ---- Scenario 4: context building over a path -----------------------------
{
  const path = [
    { type: "message", id: "n1", seq: 1, parentId: null, timestamp: 0, message: mkUser("hi"), terminate: undefined },
    { type: "thinking_level_change", id: "t1", seq: 2, parentId: "n1", timestamp: 0, thinkingLevel: "high" },
    { type: "model_change", id: "m1", seq: 3, parentId: "t1", timestamp: 0, provider: "anthropic", modelId: "claude-3-5" },
    { type: "active_tools_change", id: "at1", seq: 4, parentId: "m1", timestamp: 0, activeToolNames: ["bash", "read"] },
    { type: "message", id: "n2", seq: 5, parentId: "at1", timestamp: 0, message: mkAsst("claude-3-5", "anthropic", "stop"), terminate: undefined },
    { type: "compaction", id: "c1", seq: 6, parentId: "n2", timestamp: 0, summary: "sum", retainedTail: [mkUser("tail")], tokensBefore: 10, details: undefined, usage: undefined },
    { type: "message", id: "n3", seq: 7, parentId: "c1", timestamp: 0, message: mkUser("after"), terminate: undefined },
    { type: "custom", id: "cu1", seq: 8, parentId: "n3", timestamp: 0, customType: "note", data: { v: 1 } },
  ];
  const ctx = await ctxMod.buildSessionContext(path);
  const mapped = await Promise.all(
    ctx.messages.map(async (m) => {
      if (m.role === "compactionSummary") return { role: "compactionSummary", summary: m.summary, tokensBefore: m.tokensBefore, timestamp: 0 };
      return m;
    }),
  );
  const out = {
    thinkingLevel: ctx.thinkingLevel,
    model: ctx.model,
    activeToolNames: ctx.activeToolNames,
    messages: json(mapped),
  };
  // with custom-entry projector
  const ctxProj = await ctxMod.buildSessionContext(path, {
    entryProjectors: {
      note: (entry) => [{ role: "user", content: [{ type: "text", text: "proj:" + entry.data.v }], timestamp: 0 }],
    },
  });
  out.messagesWithProjector = json(await Promise.all(ctxProj.messages.map(async (m) => (m.role === "compactionSummary" ? "compaction:" + m.summary : m))));
  record("context-path", out);
}

// ---- Scenario 5: deferred assistant message dropped -----------------------
{
  const path = [{ type: "message", id: "d1", seq: 1, parentId: null, timestamp: 0, message: mkAsst("m", "x", "deferred"), terminate: undefined }];
  const ctx = await ctxMod.buildSessionContext(path);
  record("context-deferred", { messages: json(ctx.messages) });
}

// ---- write / check --------------------------------------------------------
const normalized = RECORDS.map((r) => ({ name: r.name, body: r.body }));
const existing = existsSync(FIXTURE)
  ? readFileSync(FIXTURE, "utf8").trim().split("\n").filter(Boolean).map((l) => JSON.parse(l))
  : [];
if (check) {
  const same = JSON.stringify(existing) === JSON.stringify(normalized);
  console.log(
    same
      ? `v4 memory oracle up to date (${normalized.length} records)`
      : `v4 memory oracle DRIFT (${existing.length} existing vs ${normalized.length} new); run node scripts/gen-v4-memory-oracle.mjs`,
  );
  process.exit(same ? 0 : 1);
} else {
  writeFileSync(FIXTURE, normalized.map((r) => JSON.stringify(r)).join("\n") + "\n");
  console.log(`wrote ${normalized.length} records`);
}