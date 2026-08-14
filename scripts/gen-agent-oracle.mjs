// gen-agent-oracle.mjs
//
// Generates AUTHENTIC golden fixtures for the pirust-agent-core Rust port by
// driving Pi's REAL packages/agent TypeScript source offline (Node 24 type
// stripping). Nothing under the pi source repo is modified; all outputs land
// under pirust/tests/fixtures/pi/agent/.
//
// Fixtures produced (see docs/analysis/07-agent-core-spec.md §11.A/B/C, §1.4, §7, §12):
//   1. entries.corpus.jsonl        v3 SessionTreeEntry byte corpus (all 11 types)
//   2. header.golden               v3 JsonlSessionStorage header line (minimal)
//      header.withmeta.golden      header line with parentSession + metadata present
//   3. uuidv7-vectors.json         authentic uuidv7 input->id vectors
//   4. loop-echo.json              behavioural loop tape + resulting session (structure-only)
//   5. compaction.json             deterministic compaction numbers cross-checked to tests
//
// Run:  cd pirust ; node scripts/gen-agent-oracle.mjs
//
// Determinism strategy:
//   * Date is replaced by a mock whose no-arg constructor / Date.now() read a
//     mutable `clock.now`, so `new Date().toISOString()` and `Date.now()` are fixed.
//     `new Date(x)` (used by message constructors) keeps real behaviour.
//   * crypto.getRandomValues is redirected to a settable provider: a fixed byte
//     pattern for the uuid vectors, a seeded mulberry32 PRNG for the corpus.
//   * Entry short-ids are uuidv7().slice(-8) = random tail bytes 12..15, so a
//     seeded PRNG yields stable + unique ids independent of the module-level
//     monotonic sequence counter. Reruns are byte-identical (idempotent).

import { writeFileSync, mkdirSync } from "node:fs";
import { fileURLToPath, pathToFileURL } from "node:url";
import { dirname, join } from "node:path";
import { register } from "node:module";

const __dirname = dirname(fileURLToPath(import.meta.url));

// pirust/scripts -> pi_space/pi/packages/{ai,agent}/src
const PI = join(__dirname, "..", "..", "pi", "packages");
const AI_SRC = join(PI, "ai", "src");
const AGENT_SRC = join(PI, "agent", "src");
const OUT = join(__dirname, "..", "tests", "fixtures", "pi", "agent");
mkdirSync(OUT, { recursive: true });

// ---------------------------------------------------------------------------
// Bare-specifier alias hook: map "@earendil-works/pi-ai" and ".../compat" to Pi's
// source (its dist is not built; only the loop path needs pi-ai at runtime).
// ---------------------------------------------------------------------------
const AI_INDEX = pathToFileURL(join(AI_SRC, "index.ts")).href;
const AI_COMPAT = pathToFileURL(join(AI_SRC, "compat.ts")).href;
const hookSrc = `
export async function resolve(specifier, context, nextResolve) {
  if (specifier === "@earendil-works/pi-ai") return { url: ${JSON.stringify(AI_INDEX)}, shortCircuit: true };
  if (specifier === "@earendil-works/pi-ai/compat") return { url: ${JSON.stringify(AI_COMPAT)}, shortCircuit: true };
  return nextResolve(specifier, context);
}
`;
register("data:text/javascript," + encodeURIComponent(hookSrc), import.meta.url);

// ---------------------------------------------------------------------------
// Global determinism seams
// ---------------------------------------------------------------------------
const RealDate = Date;
const clock = { now: 1700000000000 }; // 2023-11-14T22:13:20.000Z
class MockDate extends RealDate {
	constructor(...args) {
		if (args.length === 0) super(clock.now);
		else super(...args);
	}
	static now() {
		return clock.now;
	}
}
globalThis.Date = MockDate;

const rng = { fill: null };
{
	const c = globalThis.crypto;
	const patched = (arr) => {
		rng.fill(arr);
		return arr;
	};
	try {
		c.getRandomValues = patched;
	} catch {
		Object.getPrototypeOf(c).getRandomValues = function (arr) {
			return patched(arr);
		};
	}
}
function mulberry32(seed) {
	let a = seed >>> 0;
	return () => {
		a |= 0;
		a = (a + 0x6d2b79f5) | 0;
		let t = Math.imul(a ^ (a >>> 15), 1 | a);
		t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
		return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
	};
}
function useSeededRng(seed) {
	const next = mulberry32(seed);
	rng.fill = (arr) => {
		for (let i = 0; i < arr.length; i++) arr[i] = Math.floor(next() * 256) & 0xff;
	};
}
function useFixedBytes(bytes) {
	rng.fill = (arr) => {
		for (let i = 0; i < arr.length; i++) arr[i] = bytes[i % bytes.length] & 0xff;
	};
}

const imp = (base, rel) => import(pathToFileURL(join(base, rel)).href);
const write = (name, content) => {
	writeFileSync(join(OUT, name), content);
	console.log(`wrote ${name} (${content.length} bytes)`);
};

// ===========================================================================
// FIXTURE 2 FIRST: UUIDv7 vectors (§7, §11.C).  Done before the corpus so the
// module-level monotonic counter starts clean (lastTimestamp = -Infinity).
// ===========================================================================
async function genUuidVectors() {
	const { uuidv7 } = await imp(AGENT_SRC, "harness/session/uuid.ts");
	const vectors = [];
	function vector(label, nowMs, bytes, note) {
		clock.now = nowMs;
		useFixedBytes(bytes);
		const id = uuidv7();
		vectors.push({ label, note, nowMs, randomBytes: [...bytes], id, shortId: id.slice(-8) });
	}
	// (a) fresh timestamp, sequential byte pattern
	vector("fresh-seq", 1000, Array.from({ length: 16 }, (_, i) => i), "fresh-timestamp branch (timestamp > lastTimestamp); sequence seeded from random[6..9]");
	// (b) SAME nowMs -> monotonic sequence+1 branch
	vector("monotonic-same-now", 1000, Array.from({ length: 16 }, (_, i) => 0x10 + i), "same nowMs as previous -> sequence = (sequence+1)>>>0; only bytes 6..9 change vs a pure-random uuid");
	// (c) different nowMs + pattern
	vector("fresh-pattern-a", 2000, [0xde, 0xad, 0xbe, 0xef, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0x11, 0x22, 0x33, 0x44], "fresh-timestamp branch, distinctive pattern");
	// (d) realistic ms timestamp + all-high leading bytes (checks 48-bit big-endian ts packing)
	vector("fresh-realistic-ms", 1765233665292, [0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xaa, 0xbb, 0xcc, 0xdd], "fresh branch; 48-bit ms 1765233665292 packed big-endian into bytes 0..5");

	const payload = {
		description:
			"Authentic uuidv7() input->id vectors from Pi packages/agent/src/harness/session/uuid.ts. " +
			"crypto.getRandomValues was patched to the given 16-byte pattern and Date.now() to nowMs, then uuidv7() called. " +
			"Byte layout: bytes[0..5]=48-bit ms big-endian; bytes[6]=0x70|(seq>>>28 &0xf); bytes[8]=0x80|(seq>>>14 &0x3f); " +
			"bytes[10]=((seq&0x3f)<<2)|(random[10]&0x03); bytes[11..15]=random[11..15]. shortId = id.slice(-8) (bytes 12..15).",
		monotonicNote:
			"Vectors run in array order against a single shared module. 'fresh-seq' is the first call (lastTimestamp=-Infinity). " +
			"'monotonic-same-now' reuses nowMs=1000 so it takes the sequence=(sequence+1)>>>0 path; its id differs from 'fresh-seq' only in the sequence-derived bytes and its own random tail.",
		vectors,
	};
	write("uuidv7-vectors.json", JSON.stringify(payload, null, 2) + "\n");
	return vectors;
}

// ===========================================================================
// FIXTURE 1: v3 SessionTreeEntry byte corpus + header golden (§11.A, §1.4)
// ===========================================================================
async function genCorpus() {
	const { InMemorySessionStorage } = await imp(AGENT_SRC, "harness/session/memory-storage.ts");
	const { Session } = await imp(AGENT_SRC, "harness/session/session.ts");
	const { JsonlSessionStorage } = await imp(AGENT_SRC, "harness/session/jsonl-storage.ts");

	// Fixed clock for every outer-entry timestamp; seeded PRNG for entry short-ids.
	clock.now = 1700000000000; // -> "2023-11-14T22:13:20.000Z"
	useSeededRng(0x0badf00d);

	const storage = new InMemorySessionStorage({
		metadata: { id: "session-oracle", createdAt: "2026-01-01T00:00:00.000Z" },
	});
	const session = new Session(storage);

	const userMsg = { role: "user", content: [{ type: "text", text: "hello world" }], timestamp: 1700000000000 };
	const asstMsg = {
		role: "assistant",
		content: [
			{ type: "text", text: "hi" },
			{ type: "toolCall", id: "call-1", name: "read", arguments: { path: "a.ts" } },
		],
		api: "anthropic-messages",
		provider: "anthropic",
		model: "claude-sonnet-4-5",
		usage: { input: 10, output: 5, cacheRead: 0, cacheWrite: 0, totalTokens: 15, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } },
		stopReason: "stop",
		timestamp: 1700000000001,
	};
	const toolResultMsg = {
		role: "toolResult",
		toolCallId: "call-1",
		toolName: "read",
		content: [{ type: "text", text: "file body" }],
		isError: false,
		timestamp: 1700000000002,
	};
	const bashMsg = {
		role: "bashExecution",
		command: "echo hi",
		output: "hi\n",
		exitCode: 0,
		cancelled: false,
		truncated: false,
		timestamp: 1700000000003,
	};

	const idUser = await session.appendMessage(userMsg);
	const idAsst = await session.appendMessage(asstMsg);
	await session.appendMessage(toolResultMsg);
	await session.appendMessage(bashMsg);
	await session.appendThinkingLevelChange("high");
	await session.appendModelChange("anthropic", "claude-sonnet-4-5");
	await session.appendActiveToolsChange(["read", "write"]);
	// compaction WITH details + fromHook present
	await session.appendCompaction("SUMMARY TEXT", idUser, 4321, { readFiles: ["a.ts"], modifiedFiles: ["b.ts"] }, true);
	// custom: data present, then data omitted (undefined)
	await session.appendCustomEntry("mytype", { note: "data" });
	await session.appendCustomEntry("notype");
	// custom_message: details present + string content, then details omitted + array content
	await session.appendCustomMessageEntry("note", "hello content", true, { d: 1 });
	await session.appendCustomMessageEntry("note2", [{ type: "text", text: "arr" }], false);
	// label present, then label omitted (undefined)
	await session.appendLabel(idUser, "my-label");
	await session.appendLabel(idAsst, undefined);
	// session_info (newlines sanitized to spaces)
	await session.appendSessionName("My Session\nName");
	// moveTo -> appends a `leaf` entry (via setLeafId) then a `branch_summary` entry
	await session.moveTo(idUser, { summary: "branch summary text", details: { k: 1 }, fromHook: false });

	const entries = await storage.getEntries();
	const corpus = entries.map((e) => JSON.stringify(e)).join("\n") + "\n";
	write("entries.corpus.jsonl", corpus);

	const covered = {};
	for (const e of entries) {
		const key = e.type === "message" ? `message:${e.message.role}` : e.type;
		covered[key] = (covered[key] ?? 0) + 1;
	}

	// ---- header golden via JsonlSessionStorage.create with a capturing fs ----
	const makeFakeFs = () => {
		const state = { content: "" };
		return {
			state,
			async writeFile(_p, c) {
				state.content = c;
				return { ok: true, value: undefined };
			},
			async appendFile(_p, c) {
				state.content += c;
				return { ok: true, value: undefined };
			},
			async readTextFile() {
				return { ok: true, value: state.content };
			},
			async readTextLines() {
				return { ok: true, value: state.content.split("\n").filter((l) => l.trim()) };
			},
		};
	};

	clock.now = 1700000000000;
	const fs1 = makeFakeFs();
	await JsonlSessionStorage.create(fs1, "/oracle/session.jsonl", { cwd: "/oracle/cwd", sessionId: "session-oracle" });
	const headerMin = fs1.state.content.replace(/\n$/, "");
	write("header.golden", headerMin + "\n");

	const fs2 = makeFakeFs();
	await JsonlSessionStorage.create(fs2, "/oracle/session.jsonl", {
		cwd: "/oracle/cwd",
		sessionId: "session-oracle",
		parentSessionPath: "/parent/session.jsonl",
		metadata: { foo: "bar", n: 1 },
	});
	const headerMeta = fs2.state.content.replace(/\n$/, "");
	write("header.withmeta.golden", headerMeta + "\n");

	return { covered, headerMin, headerMeta, entryCount: entries.length };
}

// ===========================================================================
// FIXTURE 4: Compaction determinism (§11.B, §12).  Deterministic pieces only.
// Cross-checked against packages/agent/test/harness/compaction.test.ts.
// ===========================================================================
async function genCompaction() {
	const comp = await imp(AGENT_SRC, "harness/compaction/compaction.ts");
	const { buildSessionContext } = await imp(AGENT_SRC, "harness/session/session.ts");
	const { getOrThrow } = await imp(AGENT_SRC, "harness/types.ts");
	const {
		estimateTokens,
		estimateContextTokens,
		calculateContextTokens,
		shouldCompact,
		findCutPoint,
		findTurnStartIndex,
		prepareCompaction,
		DEFAULT_COMPACTION_SETTINGS,
	} = comp;

	clock.now = 1700000000000;
	let nextId = 0;
	const createId = () => `entry-${nextId++}`;
	const mockUsage = (input, output, cacheRead = 0, cacheWrite = 0) => ({
		input,
		output,
		cacheRead,
		cacheWrite,
		totalTokens: input + output + cacheRead + cacheWrite,
		cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
	});
	const userMessage = (text) => ({ role: "user", content: [{ type: "text", text }], timestamp: clock.now });
	const assistantMessage = (text, usage = mockUsage(100, 50)) => ({
		role: "assistant",
		content: [{ type: "text", text }],
		api: "anthropic-messages",
		provider: "anthropic",
		model: "claude-sonnet-4-5",
		usage,
		stopReason: "stop",
		timestamp: clock.now,
	});
	const msgEntry = (message, parentId = null) => ({ type: "message", id: createId(), parentId, timestamp: new Date().toISOString(), message });
	const compactionEntry = (summary, firstKeptEntryId, parentId = null) => ({
		type: "compaction",
		id: createId(),
		parentId,
		timestamp: new Date().toISOString(),
		summary,
		firstKeptEntryId,
		tokensBefore: 1234,
	});
	const thinkingEntry = (level, parentId = null) => ({ type: "thinking_level_change", id: createId(), parentId, timestamp: new Date().toISOString(), thinkingLevel: level });
	const modelChangeEntry = (provider, modelId, parentId = null) => ({ type: "model_change", id: createId(), parentId, timestamp: new Date().toISOString(), provider, modelId });

	// (A) estimateTokens across roles (test lines 237-294; §12 unknown->0) --------
	const usage = mockUsage(10, 5, 3, 2); // totalTokens 20
	const assistant = assistantMessage("assistant", usage);
	const assistantWithThinkingAndTool = { ...assistant, content: [{ type: "thinking", thinking: "thinking" }, { type: "toolCall", id: "call-1", name: "read", arguments: { path: "file.ts" } }] };
	const customString = { role: "custom", customType: "note", content: "custom text", display: true, timestamp: clock.now };
	const toolResultWithImage = { role: "toolResult", toolCallId: "call-1", toolName: "read", content: [{ type: "text", text: "tool text" }, { type: "image", mimeType: "image/png", data: "abc" }], isError: false, timestamp: clock.now };
	const bashExecution = { role: "bashExecution", command: "npm run check", output: "ok", exitCode: 0, cancelled: false, truncated: false, timestamp: clock.now };
	const branchSummaryMessage = { role: "branchSummary", summary: "branch", fromId: "x", timestamp: clock.now };
	const compactionSummaryMessage = { role: "compactionSummary", summary: "compact", tokensBefore: 123, timestamp: clock.now };

	const estimateTokensByRole = {
		"user 'plain user'": estimateTokens({ role: "user", content: "plain user", timestamp: clock.now }),
		"assistant thinking+toolCall(read,{path:file.ts})": estimateTokens(assistantWithThinkingAndTool),
		"custom string 'custom text'": estimateTokens(customString),
		"toolResult text+image": estimateTokens(toolResultWithImage),
		"bashExecution 'npm run check'/'ok'": estimateTokens(bashExecution),
		"branchSummary 'branch'": estimateTokens(branchSummaryMessage),
		"compactionSummary 'compact'": estimateTokens(compactionSummaryMessage),
		"unknown role (test line 294)": estimateTokens({ role: "unknown", timestamp: clock.now }),
	};

	// (B) calculateContextTokens (test lines 151-152) ------------------------------
	const calculateContextTokens_ = {
		"usage(1000,500,200,100) -> 1800": calculateContextTokens(mockUsage(1000, 500, 200, 100)),
		"usage(0,0,0,0) -> 0": calculateContextTokens(mockUsage(0, 0, 0, 0)),
	};

	// (C) shouldCompact (test lines 161-163) ---------------------------------------
	const scSettings = { enabled: true, reserveTokens: 10000, keepRecentTokens: 20000 };
	const shouldCompact_ = {
		"95000/100000 -> true": shouldCompact(95000, 100000, scSettings),
		"89000/100000 -> false": shouldCompact(89000, 100000, scSettings),
		"95000/100000 disabled -> false": shouldCompact(95000, 100000, { ...scSettings, enabled: false }),
	};

	// (D) findCutPoint / findTurnStartIndex edge cases (test lines 184-235) --------
	nextId = 100;
	const thinking = thinkingEntry("high");
	const modelChange = modelChangeEntry("openai", "gpt-4", thinking.id);
	const branchSummaryE = { type: "branch_summary", id: createId(), parentId: modelChange.id, timestamp: new Date().toISOString(), fromId: "branch", summary: "branch summary" };
	const customMessageE = { type: "custom_message", id: createId(), parentId: branchSummaryE.id, timestamp: new Date().toISOString(), customType: "note", content: "custom content", display: true };
	const toolResultE = msgEntry({ role: "toolResult", toolCallId: "call-1", toolName: "read", content: [{ type: "text", text: "tool output" }], isError: false, timestamp: clock.now });
	const uE = msgEntry(userMessage("user"));
	const compactionE = compactionEntry("summary", uE.id, uE.id);
	const assistantE = msgEntry(assistantMessage("assistant"), compactionE.id);

	const cutPoints = {
		"findCutPoint([thinking,modelChange],0,2,1) (line 187)": findCutPoint([thinking, modelChange], 0, 2, 1),
		"findTurnStartIndex([thinking,branchSummary],1,0) -> 1 (line 210)": findTurnStartIndex([thinking, branchSummaryE], 1, 0),
		"findTurnStartIndex([thinking,customMessage],1,0) -> 1 (line 211)": findTurnStartIndex([thinking, customMessageE], 1, 0),
		"findTurnStartIndex([thinking,modelChange],1,0) -> -1 (line 212)": findTurnStartIndex([thinking, modelChange], 1, 0),
		"findCutPoint([thinking,branchSummary,customMessage],0,3,1).firstKeptEntryIndex -> 0 (line 214)": findCutPoint([thinking, branchSummaryE, customMessageE], 0, 3, 1).firstKeptEntryIndex,
		"findCutPoint([toolResult],0,1,1) (line 225)": findCutPoint([toolResultE], 0, 1, 1),
		"findCutPoint([user,compaction,assistant],0,3,1).firstKeptEntryIndex -> 2 (line 234)": findCutPoint([uE, compactionE, assistantE], 0, 3, 1).firstKeptEntryIndex,
	};

	// (E) estimateContextTokens (test lines 311-325) -------------------------------
	const ctx2 = estimateContextTokens([assistant, userMessage("tail")]);
	const ctx4 = estimateContextTokens([userMessage("Hello"), assistant, userMessage("continue"), assistantMessage("Partial thinking", mockUsage(0, 0))]);
	const estimateContextTokens_ = {
		"[user 'no usage']: lastUsageIndex (line 311)": estimateContextTokens([userMessage("no usage")]).lastUsageIndex,
		"[assistant, user 'tail'] (lines 312-315)": { usageTokens: ctx2.usageTokens, lastUsageIndex: ctx2.lastUsageIndex, trailingTokens: ctx2.trailingTokens, tokens: ctx2.tokens },
		"[user,assistant,user,assistant(0,0)] (lines 316-325)": { usageTokens: ctx4.usageTokens, lastUsageIndex: ctx4.lastUsageIndex, trailingTokens: ctx4.trailingTokens, tokens: ctx4.tokens },
	};

	// (F) buildSessionContext compaction collapse (test lines 328-339) -------------
	nextId = 200;
	const u1 = msgEntry(userMessage("1"));
	const a1 = msgEntry(assistantMessage("a"), u1.id);
	const u2 = msgEntry(userMessage("2"), a1.id);
	const a2 = msgEntry(assistantMessage("b"), u2.id);
	const compaction = compactionEntry("Summary of 1,a,2,b", u2.id, a2.id);
	const u3 = msgEntry(userMessage("3"), compaction.id);
	const a3 = msgEntry(assistantMessage("c"), u3.id);
	const collapsed = buildSessionContext([u1, a1, u2, a2, compaction, u3, a3]);
	const buildSessionContext_ = {
		"messages.length (expect 5, line 337)": collapsed.messages.length,
		"messages[0].role (expect compactionSummary, line 338)": collapsed.messages[0]?.role,
		roles: collapsed.messages.map((m) => m.role),
	};

	// (G) prepareCompaction previousSummary + tokensBefore (test lines 351-365) ----
	nextId = 300;
	const p_u1 = msgEntry(userMessage("user msg 1"));
	const p_a1 = msgEntry(assistantMessage("assistant msg 1"), p_u1.id);
	const p_u2 = msgEntry(userMessage("user msg 2"), p_a1.id);
	const p_a2 = msgEntry(assistantMessage("assistant msg 2", mockUsage(5000, 1000)), p_u2.id);
	const p_comp = compactionEntry("First summary", p_u2.id, p_a2.id);
	const p_u3 = msgEntry(userMessage("user msg 3"), p_comp.id);
	const p_a3 = msgEntry(assistantMessage("assistant msg 3", mockUsage(8000, 2000)), p_u3.id);
	const pathEntries = [p_u1, p_a1, p_u2, p_a2, p_comp, p_u3, p_a3];
	const prep = getOrThrow(prepareCompaction(pathEntries, DEFAULT_COMPACTION_SETTINGS));
	const expectedTokensBefore = estimateContextTokens(buildSessionContext(pathEntries).messages).tokens;
	const prepareCompaction_ = {
		"previousSummary (expect 'First summary')": prep?.previousSummary,
		firstKeptEntryId: prep?.firstKeptEntryId,
		isSplitTurn: prep?.isSplitTurn,
		tokensBefore: prep?.tokensBefore,
		"expected tokensBefore (estimateContextTokens(...).tokens)": expectedTokensBefore,
		"tokensBefore matches (line 364)": prep?.tokensBefore === expectedTokensBefore,
		messagesToSummarizeRoles: prep?.messagesToSummarize.map((m) => m.role),
	};

	// (G2) empty / already-compacted -> undefined (test lines 426-430) -------------
	nextId = 400;
	const noopCases = {
		"prepareCompaction([compaction]) returns undefined (line 428)":
			getOrThrow(prepareCompaction([compactionEntry("already compacted", "entry-keep")], DEFAULT_COMPACTION_SETTINGS)) === undefined,
		"prepareCompaction([]) returns undefined (line 429)":
			getOrThrow(prepareCompaction([], DEFAULT_COMPACTION_SETTINGS)) === undefined,
	};

	const payload = {
		description:
			"Deterministic compaction numbers from packages/agent/src/harness/compaction/compaction.ts and session.ts. " +
			"Token counts use UTF-16 .length then Math.ceil(chars/4); image=4800 chars (ESTIMATED_IMAGE_CHARS). " +
			"Cross-references are to packages/agent/test/harness/compaction.test.ts line numbers.",
		DEFAULT_COMPACTION_SETTINGS,
		estimateTokensByRole,
		calculateContextTokens: calculateContextTokens_,
		shouldCompact: shouldCompact_,
		cutPoints,
		estimateContextTokens: estimateContextTokens_,
		buildSessionContext: buildSessionContext_,
		prepareCompaction: prepareCompaction_,
		noopCases,
	};
	write("compaction.json", JSON.stringify(payload, null, 2) + "\n");
	return payload;
}

// ===========================================================================
// FIXTURE 3: Behavioural loop tape + resulting session (§11.B). Structure-only:
// entry ids/timestamps and message_update counts depend on runtime randomness.
// tokenSize min=max=1000 forces one delta per content block so the event tape
// is stable in shape; ids/timestamps remain non-deterministic (flagged below).
// ===========================================================================
async function genLoop() {
	const { createModels } = await imp(AI_SRC, "models.ts");
	const { fauxProvider, fauxAssistantMessage, fauxToolCall } = await imp(AI_SRC, "providers/faux.ts");
	const { AgentHarness } = await imp(AGENT_SRC, "harness/agent-harness.ts");
	const { NodeExecutionEnv } = await imp(AGENT_SRC, "harness/env/nodejs.ts");
	const { InMemorySessionStorage } = await imp(AGENT_SRC, "harness/session/memory-storage.ts");
	const { Session } = await imp(AGENT_SRC, "harness/session/session.ts");

	const models = createModels();
	const faux = fauxProvider({ provider: "faux-loop", tokenSize: { min: 1000, max: 1000 } });
	models.setProvider(faux.provider);
	faux.setResponses([
		fauxAssistantMessage([fauxToolCall("echo", { text: "hi" }, { id: "call-1" })]),
		fauxAssistantMessage("done"),
	]);

	const echo = {
		name: "echo",
		label: "Echo",
		parameters: { type: "object", properties: { text: { type: "string" } }, required: ["text"] },
		execute: async (_id, args) => ({ content: [{ type: "text", text: String(args?.text) }], details: {} }),
	};

	const session = new Session(new InMemorySessionStorage());
	const harness = new AgentHarness({
		models,
		env: new NodeExecutionEnv({ cwd: process.cwd() }),
		session,
		model: faux.getModel(),
		tools: [echo],
	});

	const tape = [];
	harness.subscribe((e) => {
		tape.push(e);
	});
	const final = await harness.prompt("please echo hi");

	const tapeTypes = tape.map((e) => e.type);
	const entries = await session.getEntries();
	const entryTypes = entries.map((e) => (e.type === "message" ? `message:${e.message.role}` : e.type));

	// Stable structural fields (independent of ids/timestamps/chunking)
	const toolNames = [];
	const assistantTexts = [];
	const toolResultInfo = [];
	const stopReasons = [];
	for (const e of entries) {
		if (e.type !== "message") continue;
		const m = e.message;
		if (m.role === "assistant") {
			stopReasons.push(m.stopReason);
			for (const b of m.content) {
				if (b.type === "toolCall") toolNames.push(b.name);
				if (b.type === "text") assistantTexts.push(b.text);
			}
		} else if (m.role === "toolResult") {
			toolResultInfo.push({ toolName: m.toolName, toolCallId: m.toolCallId, isError: m.isError, text: m.content.map((c) => (c.type === "text" ? c.text : `[${c.type}]`)).join("") });
		}
	}

	const payload = {
		description:
			"Behavioural oracle: fauxProvider + AgentHarness scripted 2-turn run (turn 1 assistant calls tool 'echo'; " +
			"turn 2 assistant text 'done'). Captured via harness.subscribe wildcard listener + resulting session entries.",
		determinism:
			"STRUCTURE-ONLY. Deterministic: ordered tapeTypes, entryTypes, tool names, roles, stopReasons, message text, " +
			"toolResult content. NON-deterministic (do NOT assert bytes): entry ids (uuidv7 tail), all timestamps, faux " +
			"tool-call ids when unspecified, and the exact number of message_update events (delta chunking uses Math.random; " +
			"pinned to one delta per block here via tokenSize min=max=1000, but still runtime-dependent in general).",
		tapeTypes,
		tapeTypeCounts: tapeTypes.reduce((acc, t) => ((acc[t] = (acc[t] ?? 0) + 1), acc), {}),
		entryTypes,
		stableFields: {
			toolNames,
			assistantTexts,
			toolResultInfo,
			stopReasons,
			finalMessageRole: final?.role,
			finalMessageText: final?.content?.filter((b) => b.type === "text").map((b) => b.text).join(""),
			finalStopReason: final?.stopReason,
		},
	};
	write("loop-echo.json", JSON.stringify(payload, null, 2) + "\n");
	return payload;
}

// ===========================================================================
async function main() {
	const vectors = await genUuidVectors();
	const corpus = await genCorpus();
	const compaction = await genCompaction();
	let loop = null;
	try {
		loop = await genLoop();
	} catch (err) {
		console.error("LOOP FIXTURE FAILED:", err?.stack || err);
	}

	console.log("\n=== SUMMARY ===");
	console.log("uuid vectors:", vectors.length);
	console.log("corpus entries:", corpus.entryCount, "coverage:", JSON.stringify(corpus.covered));
	console.log("header.golden:", corpus.headerMin);
	console.log("compaction cross-checks captured:", Object.keys(compaction.cutPoints).length, "cut cases");
	console.log("loop:", loop ? `tape ${loop.tapeTypes.length} events, entries ${loop.entryTypes.length}` : "FAILED");
}

main().catch((e) => {
	console.error(e?.stack || e);
	process.exit(1);
});
