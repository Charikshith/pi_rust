#!/usr/bin/env node
// Oracle for the 0.84.2 v4 session repo (packages/agent/src/harness/session/jsonl/repo.ts).
//
// Drives Pi's REAL JsonlSessionRepo (+ the Session wrapper it returns) against a
// mock FileSystem that records every call, capturing the created directory/file
// names, the metadata contract, list ordering, error codes/messages, and fork
// byte behavior. The Rust port (v4/repo.rs + v4/session.rs) must reproduce the
// same names, metadata, and errors.
//
// Run:  node scripts/gen-v4-repo-oracle.mjs [--check]

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

const repoMod = await import(pathToFileURL(join(AGENT_SRC, "harness", "session", "jsonl", "repo.ts")).href);

// -- mock FileSystem ---------------------------------------------------------
// Byte-recording in-memory FS implementing JsonlSessionRepoFileSystem.
function mockFs() {
  // dirs: Map<path, string[]>; files: Map<path, string>
  const dirs = new Map();
  const files = new Map();
  const log = [];
  const ensureParent = (path) => {
    const idx = path.lastIndexOf("/");
    const parent = idx < 0 ? "" : path.slice(0, idx);
    const name = idx < 0 ? path : path.slice(idx + 1);
    if (parent !== "" && !dirs.has(parent)) dirs.set(parent, []);
    return { parent, name };
  };
  const registerName = (parent, name) => {
    if (parent !== "" && dirs.has(parent) && !dirs.get(parent).includes(name)) dirs.get(parent).push(name);
  };
  return {
    dirs,
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
      async readTextLines(path, opts) {
        const v = files.get(path);
        if (v === undefined) return { ok: false, error: { code: "not_found", message: `no such file ${path}` } };
        const max = opts?.maxLines ?? Infinity;
        return { ok: true, value: v.split("\n").slice(0, max) };
      },
      async writeFile(path, contents) {
        log.push(`writeFile(${path}, ${JSON.stringify(contents)})`);
        const { parent, name } = ensureParent(path);
        registerName(parent, name);
        files.set(path, contents);
        return { ok: true, value: undefined };
      },
      async appendFile(path, contents) {
        log.push(`appendFile(${path}, ${JSON.stringify(contents)})`);
        const { parent, name } = ensureParent(path);
        registerName(parent, name);
        files.set(path, (files.get(path) ?? "") + contents);
        return { ok: true, value: undefined };
      },
      async renameFile(from, to) {
        log.push(`renameFile(${from}, ${to})`);
        const v = files.get(from);
        if (v === undefined) return { ok: false, error: { code: "not_found", message: `no such file ${from}` } };
        files.set(to, v);
        files.delete(from);
        const { parent: fParent, name: fName } = ensureParent(from);
        const { parent: tParent, name: tName } = ensureParent(to);
        if (fParent !== "" && dirs.has(fParent)) dirs.set(fParent, dirs.get(fParent).filter((n) => n !== fName));
        registerName(tParent, tName);
        return { ok: true, value: undefined };
      },
      async fileInfo(path) {
        if (files.has(path)) {
          return { ok: true, value: { name: path.split("/").pop(), path, kind: "file", size: files.get(path).length, mtimeMs: 1_700_000_000_000 } };
        }
        if (dirs.has(path)) {
          return { ok: true, value: { name: path.split("/").pop(), path, kind: "directory", size: 0, mtimeMs: 1_700_000_000_000 } };
        }
        return { ok: false, error: { code: "not_found", message: `no such file ${path}` } };
      },
      async listDir(path) {
        if (!dirs.has(path)) return { ok: false, error: { code: "not_found", message: `no such dir ${path}` } };
        const out = [];
        for (const name of dirs.get(path)) {
          const p = `${path}/${name}`;
          const isDir = dirs.has(p);
          out.push({ name, path: p, kind: isDir ? "directory" : "file", size: isDir ? 0 : (files.get(p) ?? "").length, mtimeMs: 1_700_000_000_000 });
        }
        return { ok: true, value: out };
      },
      async exists(path) {
        return { ok: true, value: dirs.has(path) || files.has(path) };
      },
      async createDir(path, opts) {
        log.push(`createDir(${path})`);
        // Recursive: create every missing parent AND register the child name in
        // the parent's listing (mirrors a real FS).
        const parts = path.split("/").filter(Boolean);
        let acc = "";
        let prev = "";
        for (const part of parts) {
          acc += `/${part}`;
          if (!dirs.has(acc)) dirs.set(acc, []);
          if (prev !== "" && dirs.has(prev) && !dirs.get(prev).includes(part)) dirs.get(prev).push(part);
          prev = acc;
        }
        return { ok: true, value: undefined };
      },
      async remove(path, opts) {
        log.push(`remove(${path})`);
        if (files.has(path)) files.delete(path);
        if (dirs.has(path)) dirs.delete(path);
        const idx = path.lastIndexOf("/");
        const parent = idx < 0 ? "" : path.slice(0, idx);
        const name = idx < 0 ? path : path.slice(idx + 1);
        if (parent !== "" && dirs.has(parent)) dirs.set(parent, dirs.get(parent).filter((n) => n !== name));
        return { ok: true, value: undefined };
      },
    },
  };
}

const records = [];
const push = (r) => records.push(r);

function userMessage(text) {
  return { role: "user", content: [{ type: "text", text }], timestamp: 1 };
}

// create + metadata contract + list by cwd
await (async () => {
  const m = mockFs();
  const repo = new repoMod.JsonlSessionRepo({ fs: m.fs, sessionsRoot: "/sessions" });
  const cwd = "/workspace/project";
  const session = await repo.create({ id: "metadata", cwd, parentSessionId: "parent", metadata: { owner: "agent", nested: { enabled: true } } });
  const metadata = await session.getMetadata();
  // Normalize createdAt/modifiedAt (Date.now()) for reproducibility.
  const meta = { ...metadata, createdAt: 0, modifiedAt: 0, path: metadata.path.replace(/\d{4}-[^_]*/, "<TS>") };
  push({
    fn: "repo", name: "create-metadata",
    metadata: meta,
    listByCwd: (await repo.list({ cwd })).map((x) => ({ ...x, createdAt: 0, modifiedAt: 0, path: x.path.replace(/\d{4}-[^_]*/, "<TS>") })),
    listOther: (await repo.list({ cwd: "/workspace/other" })).map((x) => ({ ...x, createdAt: 0, modifiedAt: 0 })),
    fsLog: m.log,
    error: null,
  });
})();

// invalid session id
await (async () => {
  const m = mockFs();
  const repo = new repoMod.JsonlSessionRepo({ fs: m.fs, sessionsRoot: "/sessions" });
  try {
    await repo.create({ id: "../escape", cwd: "/workspace/project" });
    push({ fn: "repo", name: "invalid-id", error: null, fsLog: m.log });
  } catch (error) {
    push({ fn: "repo", name: "invalid-id", error: { name: error.name, code: error.code, message: error.message }, fsLog: m.log });
  }
})();

// duplicate id → already_exists
await (async () => {
  const m = mockFs();
  const repo = new repoMod.JsonlSessionRepo({ fs: m.fs, sessionsRoot: "/sessions" });
  const cwd = "/workspace/project";
  await repo.create({ id: "dup", cwd });
  try {
    await repo.create({ id: "dup", cwd });
    push({ fn: "repo", name: "duplicate-id", error: null, fsLog: m.log });
  } catch (error) {
    push({ fn: "repo", name: "duplicate-id", error: { name: error.name, code: error.code, message: error.message }, fsLog: m.log });
  }
})();

// same id in different cwds allowed
await (async () => {
  const m = mockFs();
  const repo = new repoMod.JsonlSessionRepo({ fs: m.fs, sessionsRoot: "/sessions" });
  const a = await repo.create({ id: "shared", cwd: "/workspaces/first" });
  const b = await repo.create({ id: "shared", cwd: "/workspaces/second" });
  const ma = await a.getMetadata();
  const mb = await b.getMetadata();
  push({
    fn: "repo", name: "shared-id-different-cwd",
    first: { cwd: ma.cwd, path: ma.path.replace(/\d{4}-[^_]*/, "<TS>") },
    second: { cwd: mb.cwd, path: mb.path.replace(/\d{4}-[^_]*/, "<TS>") },
    list: (await repo.list()).map((x) => ({ id: x.id, cwd: x.cwd })),
    fsLog: m.log,
    error: null,
  });
})();

// create + append mutation sequence → file bytes + reopen
await (async () => {
  const m = mockFs();
  const repo = new repoMod.JsonlSessionRepo({ fs: m.fs, sessionsRoot: "/sessions" });
  const session = await repo.create({ id: "session", cwd: "/workspace/project" });
  const metadata = await session.getMetadata();
  const entryId = await session.appendCustomEntry("note", { value: 1 });
  await session.createLane("thread", entryId);
  await session.appendRecord({
    type: "operation_started", id: "run", lane: "thread", sourceLeafId: null,
    intent: { kind: "run", originalPrompt: [], initialMessages: [] },
  });
  await session.setName("Example");
  await session.setLabel(entryId, "checkpoint");
  await session.moveLane("main", null);

  const reopened = await repo.open(metadata);
  push({
    fn: "repo", name: "create-append-reopen",
    fileBytes: m.files.get(metadata.path).replace(/("timestamp":)\d+/g, "$10"),
    lanes: await reopened.getLanes(),
    sessionName: await reopened.getName(),
    label: await reopened.getLabel(entryId),
    records: (await reopened.findRecords()).map((r) => r.id),
    openOps: (await reopened.findOpenOperations("thread", { limit: 2 })).map((r) => r.id),
    logSeqs: (await reopened.getLog()).map((x) => x.seq),
    fsLog: m.log,
    error: null,
  });
})();

// list: skips unparseable header files, sorts by modifiedAt desc
await (async () => {
  const m = mockFs();
  const repo = new repoMod.JsonlSessionRepo({ fs: m.fs, sessionsRoot: "/sessions" });
  // First session in cwd A, second in cwd B.
  await repo.create({ id: "valid", cwd: "/workspaces/a" });
  await repo.create({ id: "malformed", cwd: "/workspaces/a" });
  // Corrupt the malformed session's header.
  const all = [...m.files.entries()];
  const malformedPath = all.find(([p]) => p.endsWith("_malformed.jsonl"))[0];
  m.files.set(malformedPath, "not json\n");
  const listed = await repo.list();
  push({
    fn: "repo", name: "list-skips-malformed",
    ids: listed.map((x) => x.id),
    paths: listed.map((x) => x.path.replace(/\d{4}-[^_]*/, "<TS>")),
    fsLog: m.log,
    error: null,
  });
})();

// fork: tree fork produces metadata with parentSessionId + byte-correct file
await (async () => {
  const m = mockFs();
  const repo = new repoMod.JsonlSessionRepo({ fs: m.fs, sessionsRoot: "/sessions" });
  const cwd = "/workspace/project";
  const source = await repo.create({ id: "source", cwd });
  await source.appendMessage(userMessage("one"));
  await source.appendMessage(userMessage("two"));
  const sourceMetadata = await source.getMetadata();
  const fork = await repo.fork(sourceMetadata, { scope: "tree", id: "fork", cwd });
  const metadata = await fork.getMetadata();
  push({
    fn: "repo", name: "fork-tree",
    forkMeta: { id: metadata.id, parentSessionId: metadata.parentSessionId, cwd: metadata.cwd, path: metadata.path.replace(/\d{4}-[^_]*/, "<TS>") },
    forkBytes: m.files.get(metadata.path).replace(/("timestamp":)\d+/g, "$10"),
    messageCount: (await fork.getStats()).messageCount,
    fsLog: m.log,
    error: null,
  });
})();

// open: missing file → not_found
await (async () => {
  const m = mockFs();
  const repo = new repoMod.JsonlSessionRepo({ fs: m.fs, sessionsRoot: "/sessions" });
  try {
    await repo.open({ id: "gone", createdAt: 1, cwd: "/workspace/project", path: "/sessions/--workspace-project--/none.jsonl", modifiedAt: 1, sourceFormat: 4 });
    push({ fn: "repo", name: "open-not-found", error: null, fsLog: m.log });
  } catch (error) {
    push({ fn: "repo", name: "open-not-found", error: { name: error.name, code: error.code, message: error.message }, fsLog: m.log });
  }
})();

// open: header id mismatch → invalid_entry
await (async () => {
  const m = mockFs();
  const repo = new repoMod.JsonlSessionRepo({ fs: m.fs, sessionsRoot: "/sessions" });
  const session = await repo.create({ id: "alpha", cwd: "/workspace/project" });
  const metadata = await session.getMetadata();
  // Rewrite the header id to something else.
  const lines = m.files.get(metadata.path).split("\n");
  const header = JSON.parse(lines[0]);
  header.id = "beta";
  lines[0] = JSON.stringify(header);
  m.files.set(metadata.path, lines.join("\n") + "\n");
  try {
    await repo.open(metadata);
    push({ fn: "repo", name: "open-id-mismatch", error: null, fsLog: m.log });
  } catch (error) {
    push({ fn: "repo", name: "open-id-mismatch", error: { name: error.name, code: error.code, message: error.message }, fsLog: m.log });
  }
})();

// delete
await (async () => {
  const m = mockFs();
  const repo = new repoMod.JsonlSessionRepo({ fs: m.fs, sessionsRoot: "/sessions" });
  const session = await repo.create({ id: "doomed", cwd: "/workspace/project" });
  const metadata = await session.getMetadata();
  await repo.delete(metadata);
  push({
    fn: "repo", name: "delete",
    remaining: (await repo.list()).map((x) => x.id),
    fileExists: m.files.has(metadata.path),
    fsLog: m.log,
    error: null,
  });
})();

const payload = records.map((r) => JSON.stringify(r)).join("\n") + "\n";
const dest = join(OUT, "repo.cases.jsonl");

// Normalize non-deterministic values: timestamps, ISO-datetime filenames
// (both unescaped and JSON-escaped inside fsLog strings). The ISO token only
// ever appears as a filename prefix (`2026-08-21T14-26-02-771Z_<id>.jsonl`).
const ISO_TOKEN = /(\\{0,4})\d{4}-\d{2}-\d{2}T\d{2}-\d{2}-\d{2}-\d{3}Z/g;
// UUIDv7 entry ids are generated per-run — normalize to a fixed placeholder.
// All occurrences of the same run's uuid are replaced with the same token, so
// cross-references (lanes, labels, records) stay consistent.
const UUID_V7 = /(\\{0,4})\b[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[0-9a-f]{4}-[0-9a-f]{12}\b/g;
const normalized = payload
  .replace(/(\\{1,4}?")timestamp\\{1,4}?":\d+/g, (m) => m.replace(/\d+$/, "0"))
  .replace(/(\\{1,4}?")(createdAt|modifiedAt)\\{1,4}?":\d+/g, (m) => m.replace(/\d+$/, "0"))
  .replace(ISO_TOKEN, (m, bs) => bs + "<TS>")
  .replace(UUID_V7, (m, bs) => bs + "<UUID>");
const existing = existsSync(dest) ? readFileSync(dest, "utf8") : null;
if (existing === normalized) {
  console.log(`v4 repo oracle up to date (${records.length} records)`);
} else if (check) {
  console.error(`DRIFT: ${dest} is stale; run node scripts/gen-v4-repo-oracle.mjs`);
  process.exit(1);
} else {
  writeFileSync(dest, normalized);
  console.log(`wrote ${records.length} records`);
}
