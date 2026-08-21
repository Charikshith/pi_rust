#!/usr/bin/env node
// gen-printmode-oracle.mjs
//
// GOLDEN ORACLE for feat-005's OUTPUT LAYER: `modes/print-mode.ts` +
// `core/output-guard.ts`. Every byte written by this script is produced by
// EXECUTING Pi's own TypeScript source under Node's native type stripping.
// Nothing here is a reimplementation or a hand-authored expectation.
//
// Run:      cd pirust && node scripts/gen-printmode-oracle.mjs
// Verify:   cd pirust && node scripts/gen-printmode-oracle.mjs --check
//
// This is a SEPARATE script from gen-cli-oracle.mjs on purpose (that file is
// being edited concurrently). It copies its mechanism, not its file.
//
// ---------------------------------------------------------------------------
// OUTPUTS  (tests/fixtures/pi/printmode/)
// ---------------------------------------------------------------------------
//   text_mode.cases.jsonl     `runPrintMode(runtime, {mode:"text", ...})` -
//                             byte-exact stdout/stderr/exitCode per case, with
//                             the exact AgentSessionEvent sequence that drove it.
//   json_mode.cases.jsonl     the same scenarios under {mode:"json"}.
//   output_guard.cases.jsonl  takeOverStdout / restoreStdout / writeRawStdout /
//                             isStdoutTakenOver behaviour, recording which
//                             stream every single write landed on. The LAST
//                             record is not a stream capture but main.ts's
//                             `isPlainRuntimeMetadataCommand` decision table -
//                             the gate that decides whether takeOverStdout() is
//                             called at all - with steps/stdout/stderr null.
//   exit_codes.json           every terminal outcome print mode (and the
//                             surrounding main.ts dispatch) can produce, with
//                             its accompanying stderr text.
//   events.provenance.json    WHERE the event sequences came from: the live
//                             faux-provider harvest, the raw (un-normalized)
//                             values observed, and the live-vs-replay
//                             cross-check result.
//   system_prompt.cases.jsonl the byte-exact system prompt from
//                             `core/system-prompt.ts buildSystemPrompt()`, with
//                             every interpolation point isolated, plus the real
//                             AGENTS.md/CLAUDE.md discovery, the real
//                             --system-prompt / --append-system-prompt handling,
//                             and the real end-to-end prompt an AgentSession
//                             actually hands the model. See "SYSTEM PROMPT"
//                             below.
//
// ---------------------------------------------------------------------------
// MODULE RESOLUTION
// ---------------------------------------------------------------------------
// A `register()`ed resolve hook maps the bare workspace specifiers
// `@earendil-works/pi-ai`, `@earendil-works/pi-ai/<sub>`,
// `@earendil-works/pi-agent-core`, `@earendil-works/pi-agent-core/<sub>` and
// `@earendil-works/pi-tui` to pi/packages/*/src/*.ts (the published `dist/` is
// not built). Third-party deps resolve naturally out of pi/node_modules because
// the importing modules live inside the pi tree.
//
// A `load` hook APPENDS a single `export { x as __x }` statement to main.ts so
// its module-private `isPlainRuntimeMetadataCommand` / `resolveAppMode` /
// `toPrintOutputMode` can be driven directly instead of reimplemented. Pi's own
// bytes execute verbatim; only an export list is appended and NOTHING is written
// into the pi checkout.
//
// Every module needed here loads cleanly under type stripping - no TS enums,
// decorators or namespaces on any path reached.
//
// ---------------------------------------------------------------------------
// THE SEAM: HOW `runPrintMode` IS DRIVEN WITHOUT A MODEL OR A NETWORK
// ---------------------------------------------------------------------------
// TWO PHASES, both offline, both executing Pi's code:
//
// PHASE 1 - HARVEST (`--role harvest`). A COMPLETE, REAL coding-agent runtime is
//   built exactly the way Pi's own `test/agent-session-runtime-events.test.ts`
//   builds one: `registerFauxProvider()` (Pi's scripted test double in
//   packages/ai/src/providers/faux.ts) + `AuthStorage.inMemory()` +
//   `ModelRuntime.create({credentials, modelsPath})` + `registerProvider` +
//   `createAgentSessionServices` + `createAgentSessionFromServices` +
//   `createAgentSessionRuntime`. That is a real `AgentSession` over a real
//   `Agent`; the only substitution is the provider's `stream` function. The real
//   `session.bindExtensions({mode})` + `session.subscribe()` + `session.prompt()`
//   are then driven prompt-by-prompt and the resulting `AgentSessionEvent`
//   objects are captured with `structuredClone`. NOTHING about these events is
//   authored by this script.
//
// PHASE 2 - CAPTURE (`--role capture`). The harvested (and normalized, see
//   below) event batches are replayed through the REAL `runPrintMode` inside a
//   fresh CHILD PROCESS whose stdout and stderr are separate pipes, so the two
//   streams are byte-exactly separable. The `runtimeHost` is the REAL
//   `AgentSessionRuntime` class (so `setRebindSession`, the `session` getter and
//   `dispose()` -> `emitSessionShutdownEvent` are Pi's code) wrapping a STUB
//   `AgentSession`. The stub is the same shape Pi's OWN
//   `packages/coding-agent/test/print-mode.test.ts` uses - `runPrintMode` reads
//   exactly 8 members off the session (`sessionManager.getHeader`,
//   `bindExtensions`, `subscribe`, `prompt`, `state`, `extensionRunner`,
//   plus `waitForIdle`/`navigateTree`/`reload` which only appear inside closures
//   handed to `bindExtensions` and are never invoked) and never reaches a
//   provider. The child also calls `takeOverStdout()` first and
//   `restoreStdout()` after, and sets `process.exitCode` only when non-zero,
//   mirroring `main.ts:540-544,846-857`.
//
// WHY NOT run `runPrintMode` against the live faux runtime for the fixtures
// themselves? It IS also run that way, but only as a CROSS-CHECK (see
// `events.provenance.json` -> `crossCheck`): a live json-mode run's stdout is
// parsed back and compared, after identical normalization, to
// `header + harvested events`. It matches, which is the proof that the phase-2
// replay is faithful. The fixtures are taken from the replay because a live run
// bakes wall-clock timestamps, uuidv7 ids, temp paths and faux's
// prompt-length-derived token counts into the SAME bytes that are supposed to be
// the assertion target; replaying normalized events keeps `events` and `stdout`
// exactly consistent with each other, which is what a Rust byte test needs.
//
// ---------------------------------------------------------------------------
// DETERMINISM / NORMALIZATION  (every item, and why)
// ---------------------------------------------------------------------------
// In the HARVEST child, before Pi is imported:
//   * `Date` is replaced by a subclass whose no-arg constructor and `Date.now()`
//     return the fixed `ORACLE_NOW = 1700000000000` ("2023-11-14T22:13:20.000Z").
//     `new Date(x)` keeps real behaviour. This pins every `timestamp` field on
//     every message and the session header's `timestamp`.
//   * `crypto.getRandomValues` is redirected to a seeded mulberry32 PRNG, so
//     `uuidv7()` session ids and 8-hex entry short-ids are reproducible.
//   * `Math.random` is redirected to the same PRNG (faux's chunk splitter and
//     `randomId` use it).
//   * `registerFauxProvider({api:"faux", tokenSize:{min:100000,max:100000}})`:
//     the explicit `api` removes faux's `faux:<Date.now()>:<Math.random()>`
//     default id, and the huge token size forces ONE delta event per content
//     block (faux's `splitStringByTokenSize` is otherwise random-length, which
//     would make the `message_update` COUNT non-deterministic).
//   * `PI_CODING_AGENT_DIR` points at a mkdtemp'd directory and `PI_OFFLINE=1`
//     is set, so `ModelRuntime.create` resolves `allowModelNetwork = false` and
//     never touches the network. `NO_COLOR=1` everywhere.
//
// Applied to the harvested events afterwards (a deep walk), longest literal
// first, in both native-separator and forward-slash form:
//   "{TMPROOT}"     <- the mkdtemp'd root
//   "{PROJECTDIR}"  <- the temp project cwd (appears in the session header)
//   "{AGENTDIR}"    <- the temp agent dir
//   "{SESSIONDIR}"  <- the temp sessions dir
//   "{HOME}"        <- os.homedir()
//   "{PIPKG}"       <- the pi checkout's coding-agent package root (the default
//                      system prompt embeds getReadmePath()/getDocsPath()/...)
//
// And ONE structural normalization:
//   * USAGE VALUES. Every object whose keys are exactly
//     [input,output,cacheRead,cacheWrite,totalTokens,cost] has its NUMBERS
//     replaced by the canonical tuple CANONICAL_USAGE. Reason: faux computes
//     `usage` with `Math.ceil(serializeContext(context).length / 4)`, and the
//     serialized context contains the absolute cwd and the absolute paths of
//     Pi's shipped docs, so the numbers are a function of where the checkout
//     lives - they differ on every machine and would make `--check` fail
//     elsewhere. The KEY ORDER, the presence of the field, and the nesting of
//     `cost` are preserved exactly and ARE contract; only the integers are
//     placeholders. The raw values observed on the capture machine are recorded
//     verbatim in `events.provenance.json -> rawUsageSamples` so nothing is
//     hidden.
//
// NOT normalized, because it is contract:
//   * every event `type`, every key order, every `stopReason`/`errorMessage`,
//     every `isError`, all assistant/tool text, `willRetry`, `toolResults`,
//     `contentIndex`, `delta`, and the exact `\n` placement.
//   * print mode emits NO duration, NO cost line and NO ANSI of its own:
//     print-mode.ts imports no chalk and formats nothing beyond
//     `JSON.stringify(event)` / `${content.text}` / `console.error(msg)`. Colour
//     is additionally forced OFF (NO_COLOR=1) in every child. There is no
//     colour-ON variant to capture because there is nothing to colour.
//
// ---------------------------------------------------------------------------
// THE REAL ~/.pi IS NEVER TOUCHED
// ---------------------------------------------------------------------------
// Every child runs with `PI_CODING_AGENT_DIR` pointed at a mkdtemp'd directory,
// and `assertTemp()` hard-fails if a path about to be used is not under
// `os.tmpdir()`. The whole temp root is removed at the end of the run (and on
// failure). NOTHING is written inside the pi repo.
//
// ---------------------------------------------------------------------------
// WHAT COULD NOT BE CAPTURED  (also restated in events.provenance.json)
// ---------------------------------------------------------------------------
//   * `entry_appended`, `session_info_changed`, `thinking_level_changed`,
//     `queue_update`, `compaction_start`/`compaction_end`,
//     `auto_retry_start`/`auto_retry_end`: none of these are emitted by a plain
//     single-shot faux turn. `entry_appended` is only emitted from the extension
//     API's `appendEntry` (agent-session.ts:2357-2363) and the retry/compaction
//     ones need a retryable provider error or an over-threshold context. They
//     are AgentSession's events, not print mode's: print mode's json branch is
//     `writeRawStdout(`${JSON.stringify(event)}\n`)` for EVERY event with no
//     switch on `type`, so pinning the emitted subset pins the whole contract.
//     Listed here rather than fabricated.
//   * A real mid-stream abort (`session.abort()` racing the stream) is not
//     byte-deterministic - where the abort lands decides how many
//     `text_delta`s precede it. The aborted TERMINAL state is captured instead,
//     via faux's own `stopReason:"aborted"` path (faux.ts:291-298,393-397),
//     which is the same shape a real abort produces.
//   * SIGHUP is only registered off-win32 (print-mode.ts:49-51); on the capture
//     platform its listener count is recorded as observed.
//
// ---------------------------------------------------------------------------
// SYSTEM PROMPT  (system_prompt.cases.jsonl)
// ---------------------------------------------------------------------------
// The builder is `core/system-prompt.ts buildSystemPrompt(options)`. Its only
// caller is `AgentSession._rebuildSystemPrompt(toolNames)`
// (core/agent-session.ts:1009-1043), which assembles the options from the tool
// registry and the ResourceLoader. Three layers are captured, all in ONE child
// (`--role sysprompt`):
//
//   layer "A" (`fn: "buildSystemPrompt"`) - Pi's builder called DIRECTLY with an
//     explicit option object, one input varied at a time. The option objects are
//     oracle-authored INPUT exactly as the argv arrays in the cli oracle are;
//     every `prompt` string is Pi's own output. The `toolSnippets` /
//     `promptGuidelines` fed in are NOT typed by hand: they are read off Pi's
//     real `createAllToolDefinitions(cwd)` definitions in the same child, so the
//     seven builtin snippets and their guideline bullets are authentic.
//
//   layer "B" (`fn: "loadProjectContextFiles"` / `fn: "DefaultResourceLoader"`) -
//     the REAL context-file discovery and the REAL --system-prompt /
//     --append-system-prompt resolution, against a REAL temp filesystem. No
//     model and no AgentSession are needed: `loadProjectContextFiles` is
//     exported, and `DefaultResourceLoader` is constructed directly with
//     `{noExtensions, noSkills, noPromptTemplates, noThemes}` - i.e. exactly the
//     feat-005 configuration.
//
//   layer "C" (`fn: "AgentSession._rebuildSystemPrompt"`) - the prompt a REAL
//     AgentSession actually hands the model, read off `session._baseSystemPrompt`
//     (a TypeScript `private`, i.e. a plain property at runtime) on the same
//     faux-backed runtime the print-mode fixtures use. This is what proves layer
//     A's inputs are the ones Pi really passes.
//
// FACTS WORTH KNOWING BEFORE PORTING (all observable in the fixture):
//   * There is NO platform / OS / date / model / username interpolation. The
//     ONLY platform-sensitive transform is `cwd.replace(/\\/g, "/")`
//     (system-prompt.ts:39), so a win32 cwd is emitted POSIX-style. The
//     `cwd-*` layer-A cases use LITERAL fixed cwds (never the host's) so this
//     transform stays visible; layer B/C cwds are temp dirs and therefore show
//     up as `{PROJECTDIR}`.
//   * A tool appears under "Available tools" ONLY when the caller supplies a
//     `toolSnippets[name]`; `selectedTools` alone is not enough. With no visible
//     tool the list is the literal `(none)`.
//   * `<project_instructions path="...">` does NOT escape the path, so a `"` in a
//     filename produces malformed XML. Captured, because it is contract.
//   * `--system-prompt` / `--append-system-prompt` accept EITHER literal text OR
//     a FILE PATH: `resolvePromptInput` (resource-loader.ts:50-65) does
//     `existsSync(input) ? readFileSync(input) : input`. Both are captured.
//   * Multiple `--append-system-prompt` values are joined with "\n\n"
//     (agent-session.ts:1026-1028) and the whole section is prefixed with
//     another "\n\n" (system-prompt.ts:41).
//   * Context files are ordered: the global `<agentDir>` one FIRST, then the cwd
//     ancestors ROOT-FIRST and cwd LAST (`unshift` while walking up,
//     resource-loader.ts:101-117), de-duplicated by exact path. Per directory the
//     first hit of ["AGENTS.md","AGENTS.MD","CLAUDE.md","CLAUDE.MD"] wins.
//   * `pirustOmits: true` marks the ONE record covering the three Pi-docs paths.
//     The user has decided pirust drops those references because they point into
//     an installed npm package with no Cargo equivalent. That record captures the
//     exact byte span to delete and the text on either side of it, so a test can
//     assert the omission is surgical. NOTHING ELSE in the fixture is exempt.
//   * `feat007: true` marks records whose input can only exist once skills /
//     prompt templates / extensions load (feat-007). In feat-005 the skills block
//     is always absent because `skills` is always empty.

import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { register } from "node:module";
import { homedir, tmpdir } from "node:os";
import { dirname, join, sep } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

// ---------------------------------------------------------------------------
// Paths / roles
// ---------------------------------------------------------------------------
const here = dirname(fileURLToPath(import.meta.url));
const pirustRoot = dirname(here);
const piRoot = join(pirustRoot, "..", "pi");
const PKGS = join(piRoot, "packages");
const CA = join(PKGS, "coding-agent", "src");
const OUT = join(pirustRoot, "tests", "fixtures", "pi", "printmode");
const SELF = fileURLToPath(import.meta.url);

const argv = process.argv.slice(2);
const CHECK = argv.includes("--check");
const roleIndex = argv.indexOf("--role");
const ROLE = roleIndex === -1 ? null : argv[roleIndex + 1];
const specIndex = argv.indexOf("--spec");
const SPEC_FILE = specIndex === -1 ? null : argv[specIndex + 1];
const outIndex = argv.indexOf("--out");
const META_FILE = outIndex === -1 ? null : argv[outIndex + 1];

const ORACLE_NOW = 1700000000000; // "2023-11-14T22:13:20.000Z"
const RNG_SEED = 0x0badf00d;
const CANONICAL_USAGE = {
	input: 1000,
	output: 25,
	cacheRead: 0,
	cacheWrite: 1000,
	totalTokens: 2025,
	cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
};
const USAGE_KEYS = ["input", "output", "cacheRead", "cacheWrite", "totalTokens", "cost"].join(",");

if (!existsSync(join(CA, "modes", "print-mode.ts"))) {
	console.error(`Pi print-mode sources not found at ${CA}; fixtures cannot be regenerated.`);
	process.exit(CHECK ? 0 : 1); // don't fail --check when the source repo is simply absent
}

// ---------------------------------------------------------------------------
// Hooks: bare-specifier aliases + private-export appending (main.ts only)
// ---------------------------------------------------------------------------
const PKG_ROOTS = {
	"@earendil-works/pi-ai": join(PKGS, "ai", "src"),
	"@earendil-works/pi-agent-core": join(PKGS, "agent", "src"),
	"@earendil-works/pi-tui": join(PKGS, "tui", "src"),
	"@earendil-works/pi-telemetry": join(PKGS, "telemetry", "src"),
};

const APPENDED_EXPORTS = {
	"main.ts": ["isPlainRuntimeMetadataCommand", "resolveAppMode", "toPrintOutputMode"],
};

function buildHooks() {
	const roots = Object.fromEntries(
		Object.entries(PKG_ROOTS)
			.filter(([, dir]) => existsSync(dir))
			.map(([spec, dir]) => [spec, pathToFileURL(dir + sep).href]),
	);
	const append = {};
	for (const [rel, names] of Object.entries(APPENDED_EXPORTS)) {
		const file = join(CA, ...rel.split("/"));
		if (!existsSync(file)) continue;
		append[pathToFileURL(file).href] = `\nexport { ${names.map((n) => `${n} as __${n}`).join(", ")} };\n`;
	}
	return (
		"data:text/javascript," +
		encodeURIComponent(`
import { existsSync } from "node:fs";
import { fileURLToPath } from "node:url";
const ROOTS = ${JSON.stringify(roots)};
const APPEND = ${JSON.stringify(append)};
export async function resolve(specifier, context, nextResolve) {
  for (const [pkg, rootUrl] of Object.entries(ROOTS)) {
    if (specifier === pkg) return { url: new URL("index.ts", rootUrl).href, shortCircuit: true };
    if (specifier.startsWith(pkg + "/")) {
      const rest = specifier.slice(pkg.length + 1);
      for (const cand of [rest + ".ts", rest + "/index.ts"]) {
        const u = new URL(cand, rootUrl);
        if (existsSync(fileURLToPath(u))) return { url: u.href, shortCircuit: true };
      }
      throw new Error("alias hook: no source file for " + specifier);
    }
  }
  return nextResolve(specifier, context);
}
export async function load(url, context, nextLoad) {
  if (!Object.hasOwn(APPEND, url)) return nextLoad(url, context);
  const r = await nextLoad(url, context);
  let src = r.source;
  if (typeof src !== "string") src = Buffer.from(src).toString("utf8");
  return { format: r.format, responseURL: r.responseURL, source: src + APPEND[url], shortCircuit: true };
}
`)
	);
}
register(buildHooks(), import.meta.url);

const impPi = (rel) => import(pathToFileURL(join(CA, ...rel.split("/"))).href);
const impAi = (rel) => import(pathToFileURL(join(PKGS, "ai", "src", ...rel.split("/"))).href);

// ---------------------------------------------------------------------------
// Temp-dir safety (guards the real ~/.pi)
// ---------------------------------------------------------------------------
const REAL_TMP = tmpdir();
function assertTemp(p) {
	const lower = (s) => (process.platform === "win32" ? s.toLowerCase() : s);
	if (!lower(p).startsWith(lower(REAL_TMP))) throw new Error(`refusing to use non-temp path: ${p}`);
	return p;
}

// ===========================================================================
// SEEDED DETERMINISM SEAMS (children only)
// ===========================================================================
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

/** Pin Date, crypto.getRandomValues and Math.random. Returns a reset() hook. */
function installDeterminism() {
	const RealDate = Date;
	class MockDate extends RealDate {
		constructor(...args) {
			if (args.length === 0) super(ORACLE_NOW);
			else super(...args);
		}
		static now() {
			return ORACLE_NOW;
		}
	}
	globalThis.Date = MockDate;

	let next = mulberry32(RNG_SEED);
	const fill = (arr) => {
		for (let i = 0; i < arr.length; i++) arr[i] = Math.floor(next() * 256) & 0xff;
		return arr;
	};
	const c = globalThis.crypto;
	try {
		c.getRandomValues = fill;
	} catch {
		Object.getPrototypeOf(c).getRandomValues = function (arr) {
			return fill(arr);
		};
	}
	Math.random = () => next();
	return () => {
		next = mulberry32(RNG_SEED);
	};
}

// ===========================================================================
// CHILD ROLE: harvest - build a REAL faux-backed runtime and record the real
// AgentSessionEvent stream, prompt by prompt.
// ===========================================================================

/** The scenarios. `responses` names faux factory calls; nothing else is authored. */
function scenarioSpecs() {
	return [
		{
			name: "text-only-reply",
			note: "a plain text-only assistant reply, one turn",
			prompts: ["Say hello"],
			responses: [{ kind: "text", text: "Hello there." }],
		},
		{
			name: "tool-call-and-result",
			note: "turn 1 calls a custom tool, turn 2 answers; pins tool rendering in print mode (json mode streams tool_execution_* and the toolResult message; TEXT mode prints NOTHING for them)",
			prompts: ["echo hi"],
			tools: ["echo"],
			responses: [{ kind: "toolCall", name: "echo", args: { text: "hi" }, id: "call-1" }, { kind: "text", text: "Echoed." }],
		},
		{
			name: "multi-turn",
			note: "initialMessage plus one follow-up in options.messages; TEXT mode prints only the LAST assistant message",
			prompts: ["first question", "second question"],
			responses: [{ kind: "text", text: "First answer." }, { kind: "text", text: "Second answer." }],
		},
		{
			name: "reasoning-content",
			note: "an assistant reply carrying thinking content; TEXT mode DROPS thinking blocks and prints only text blocks",
			prompts: ["think about it"],
			responses: [{ kind: "blocks", blocks: [{ t: "thinking", v: "step one, step two" }, { t: "text", v: "The answer is 42." }] }],
		},
		{
			name: "provider-error-mid-stream",
			note: "faux returns stopReason:\"error\" with a NON-retryable errorMessage (it matches neither pattern in ai/src/utils/retry.ts), so there is exactly one turn; TEXT mode prints errorMessage to STDERR and exits 1, JSON mode exits 0",
			prompts: ["boom"],
			responses: [{ kind: "text", text: "", stopReason: "error", errorMessage: "provider exploded: invalid_request_error" }],
		},
		{
			name: "provider-error-retried-then-succeeds",
			note: "the errorMessage contains \"503\", which ai/src/utils/retry.ts classifies as RETRYABLE, so AgentSession auto-retries: the stream carries a real auto_retry_start / auto_retry_end(success:true) pair and a second agent_start..agent_end cycle. TEXT mode prints only the LAST assistant message, so the failed turn is invisible there and the exit code is 0",
			prompts: ["boom"],
			responses: [
				{ kind: "text", text: "", stopReason: "error", errorMessage: "provider exploded: 503 upstream" },
				{ kind: "text", text: "Recovered." },
			],
		},
		{
			name: "provider-error-retried-then-exhausted",
			note: "a retryable 503 with NOTHING queued behind it: auto_retry_start fires, the retry hits faux's own `No more faux responses queued` error (which is NOT retryable), and auto_retry_end carries success:false + finalError. TEXT mode reports the SECOND error message, not the first",
			prompts: ["boom"],
			responses: [{ kind: "text", text: "", stopReason: "error", errorMessage: "provider exploded: 503 upstream" }],
		},
		{
			name: "provider-error-no-message",
			note: "stopReason:\"error\" with errorMessage undefined -> TEXT mode falls back to `Request error`",
			prompts: ["boom"],
			responses: [{ kind: "text", text: "", stopReason: "error", errorMessage: null }],
		},
		{
			name: "no-responses-queued",
			note: "faux has NO queued response: its own createErrorMessage path (faux.ts:451-460) yields stopReason error + `No more faux responses queued`",
			prompts: ["boom"],
			responses: [],
		},
		{
			name: "aborted-run",
			note: "stopReason:\"aborted\" - faux.ts:291-298/393-397, the same terminal shape a real session.abort() produces; TEXT mode prints errorMessage or `Request aborted` and exits 1",
			prompts: ["stop"],
			responses: [{ kind: "text", text: "partial answ", stopReason: "aborted", errorMessage: "Request was aborted" }],
		},
		{
			name: "aborted-no-message",
			note: "stopReason:\"aborted\" with errorMessage undefined -> TEXT mode falls back to `Request aborted`",
			prompts: ["stop"],
			responses: [{ kind: "text", text: "partial answ", stopReason: "aborted", errorMessage: null }],
		},
		{
			name: "empty-response",
			note: "an assistant message with an EMPTY content array: no text blocks, so TEXT mode writes nothing at all and exits 0",
			prompts: ["say nothing"],
			responses: [{ kind: "blocks", blocks: [] }],
		},
		{
			name: "multi-text-blocks",
			note: "two text blocks in ONE assistant message: TEXT mode writes each with its own trailing newline, in block order",
			prompts: ["two parts"],
			responses: [{ kind: "blocks", blocks: [{ t: "text", v: "part one" }, { t: "text", v: "part two" }] }],
		},
		{
			name: "text-with-trailing-newline",
			note: "assistant text that already ENDS in \\n: print mode appends another one unconditionally (`${content.text}\\n`)",
			prompts: ["trailing"],
			responses: [{ kind: "text", text: "ends with newline\n" }],
		},
		{
			name: "text-multiline-and-unicode",
			note: "embedded newlines, CRLF, a tab, non-ASCII and an emoji pass through verbatim; JSON.stringify escapes them, writeRawStdout does not",
			prompts: ["unicode"],
			responses: [{ kind: "text", text: "line1\nline2\r\ntab\there caf\u00e9 \u65e5\u672c\u8a9e \ud83d\ude80 \"quoted\" back\\slash" }],
		},
	];
}

function buildFauxResponses(compat, responses) {
	return responses.map((r) => {
		const opts = {};
		if (r.stopReason) opts.stopReason = r.stopReason;
		if (r.errorMessage) opts.errorMessage = r.errorMessage;
		if (r.kind === "text") return compat.fauxAssistantMessage(r.text, opts);
		if (r.kind === "toolCall") return compat.fauxAssistantMessage([compat.fauxToolCall(r.name, r.args, { id: r.id })], opts);
		const blocks = r.blocks.map((b) => (b.t === "thinking" ? compat.fauxThinking(b.v) : compat.fauxText(b.v)));
		return compat.fauxAssistantMessage(blocks, opts);
	});
}

const ECHO_TOOL_FACTORY = () => ({
	name: "echo",
	label: "Echo",
	description: "Echo the given text straight back.",
	parameters: { type: "object", properties: { text: { type: "string" } }, required: ["text"] },
	execute: async (_toolCallId, params) => ({ content: [{ type: "text", text: String(params?.text) }], details: {} }),
});

async function roleHarvest() {
	const resetRng = installDeterminism();
	process.env.NO_COLOR = "1";
	process.env.PI_OFFLINE = "1";

	const spec = JSON.parse(readFileSync(SPEC_FILE, "utf-8"));
	const { tmproot, agentDir, projectDir, sessionDir, scenario } = spec;
	assertTemp(tmproot);
	process.env.PI_CODING_AGENT_DIR = agentDir;
	mkdirSync(agentDir, { recursive: true });
	mkdirSync(projectDir, { recursive: true });
	mkdirSync(sessionDir, { recursive: true });

	const compat = await impAi("compat.ts");
	const runtimeMod = await impPi("core/agent-session-runtime.ts");
	const { AuthStorage } = await impPi("core/auth-storage.ts");
	const { ModelRuntime } = await impPi("core/model-runtime.ts");
	const { SessionManager } = await impPi("core/session-manager.ts");
	const { runPrintMode } = await impPi("modes/print-mode.ts");
	const { takeOverStdout, restoreStdout } = await impPi("core/output-guard.ts");

	async function buildHost() {
		const faux = compat.registerFauxProvider({ api: "faux", tokenSize: { min: 100000, max: 100000 } });
		faux.setResponses(buildFauxResponses(compat, scenario.responses));
		const authStorage = AuthStorage.inMemory();
		await authStorage.modify(faux.getModel().provider, async () => ({ type: "api_key", key: "faux-key" }));
		const modelRuntime = await ModelRuntime.create({
			credentials: authStorage,
			modelsPath: join(agentDir, "models.json"),
		});
		const m = faux.getModel();
		modelRuntime.registerProvider(m.provider, {
			baseUrl: m.baseUrl,
			api: m.api,
			models: [
				{
					id: m.id,
					name: m.name,
					api: m.api,
					reasoning: m.reasoning,
					input: m.input,
					cost: m.cost,
					contextWindow: m.contextWindow,
					maxTokens: m.maxTokens,
					baseUrl: m.baseUrl,
				},
			],
		});
		const customTools = scenario.tools?.includes("echo") ? [ECHO_TOOL_FACTORY()] : undefined;
		const createRuntime = async ({ cwd, sessionManager, sessionStartEvent }) => {
			const services = await runtimeMod.createAgentSessionServices({
				cwd,
				agentDir,
				modelRuntime,
				model: faux.getModel(),
				resourceLoaderOptions: {
					noSkills: true,
					noPromptTemplates: true,
					noThemes: true,
					noContextFiles: true,
					noExtensions: true,
				},
			});
			return {
				...(await runtimeMod.createAgentSessionFromServices({
					services,
					sessionManager,
					sessionStartEvent,
					model: faux.getModel(),
					customTools,
					tools: scenario.tools,
				})),
				services,
				diagnostics: services.diagnostics,
			};
		};
		const host = await runtimeMod.createAgentSessionRuntime(createRuntime, {
			cwd: projectDir,
			agentDir,
			sessionManager: SessionManager.create(projectDir, sessionDir),
		});
		return { host, faux };
	}

	// ---- (a) harvest: drive session.prompt() directly, prompt by prompt -----
	resetRng();
	const { host: h1, faux: f1 } = await buildHost();
	const header = h1.session.sessionManager.getHeader();
	const tape = [];
	let promptIndex = -1;
	h1.session.subscribe((event) => tape.push({ promptIndex, event: structuredClone(event) }));
	// Mirror exactly what runPrintMode's rebindSession() does before prompting.
	await h1.session.bindExtensions({ mode: scenario.harvestBindMode ?? "json" });
	const batches = [];
	for (const text of scenario.prompts) {
		promptIndex += 1;
		await h1.session.prompt(text);
		batches.push({
			text,
			events: tape.filter((t) => t.promptIndex === promptIndex).map((t) => t.event),
			stateAfter: structuredClone(h1.session.state.messages),
		});
	}
	const activeTools = h1.session.state.tools?.map((t) => t.name) ?? null;
	await h1.dispose();
	f1.unregister();

	// ---- (b) cross-check: a LIVE real runPrintMode in json mode ------------
	// Its stdout goes to this child's stdout pipe; the parent parses it back and
	// compares to header+events after identical normalization.
	resetRng();
	const { host: h2, faux: f2 } = await buildHost();
	const liveHeader = h2.session.sessionManager.getHeader();
	takeOverStdout();
	let liveExitCode;
	try {
		liveExitCode = await runPrintMode(h2, {
			mode: "json",
			initialMessage: scenario.prompts[0],
			messages: scenario.prompts.slice(1),
		});
	} finally {
		restoreStdout();
	}
	f2.unregister();

	writeFileSync(
		META_FILE,
		JSON.stringify({
			scenario: scenario.name,
			header,
			liveHeader,
			liveExitCode,
			batches,
			activeTools,
			platform: process.platform,
		}),
		"utf-8",
	);
}

// ===========================================================================
// CHILD ROLE: capture - replay canned events through the REAL runPrintMode
// ===========================================================================
async function roleCapture() {
	process.env.NO_COLOR = "1";
	process.env.PI_OFFLINE = "1";
	const spec = JSON.parse(readFileSync(SPEC_FILE, "utf-8"));

	const runtimeMod = await impPi("core/agent-session-runtime.ts");
	const { runPrintMode } = await impPi("modes/print-mode.ts");
	const { takeOverStdout, restoreStdout, isStdoutTakenOver } = await impPi("core/output-guard.ts");

	const meta = {
		bindExtensionsCalls: [],
		shutdownEvents: [],
		subscribeCount: 0,
		unsubscribeCount: 0,
		promptCalls: [],
		emittedEventCount: 0,
		sessionDisposed: false,
		takenOverDuringRun: null,
		// Baselines taken BEFORE runPrintMode so the DELTA attributable to
		// registerSignalHandlers() is unambiguous (Node itself may already hold
		// listeners on these signals).
		sigtermListenersBefore: process.listenerCount("SIGTERM"),
		sighupListenersBefore: process.listenerCount("SIGHUP"),
		sigtermListeners: null,
		sighupListeners: null,
	};

	let listeners = [];
	const state = { messages: spec.initialStateMessages ?? [] };
	let promptIndex = 0;

	const session = {
		sessionManager: { getHeader: () => spec.header ?? undefined },
		agent: { waitForIdle: async () => {} },
		state,
		extensionRunner: {
			hasHandlers: (t) => t === "session_shutdown",
			emit: async (event) => {
				meta.shutdownEvents.push(event);
			},
		},
		bindExtensions: async (bindings) => {
			meta.bindExtensionsCalls.push({
				mode: bindings.mode,
				hasCommandContextActions: bindings.commandContextActions !== undefined,
				commandContextActionKeys: Object.keys(bindings.commandContextActions ?? {}),
				hasOnError: typeof bindings.onError === "function",
			});
		},
		subscribe: (listener) => {
			meta.subscribeCount += 1;
			listeners.push(listener);
			return () => {
				meta.unsubscribeCount += 1;
				listeners = listeners.filter((l) => l !== listener);
			};
		},
		prompt: async (text, options) => {
			const batch = spec.batches[promptIndex] ?? { events: [], stateAfter: state.messages };
			meta.promptCalls.push({ text, options: options === undefined ? null : { images: options.images ?? null } });
			promptIndex += 1;
			if (batch.throwMessage !== undefined) {
				meta.takenOverDuringRun = isStdoutTakenOver();
				throw new Error(batch.throwMessage);
			}
			if (batch.throwNonError !== undefined) throw batch.throwNonError;
			if (batch.emitSigterm) {
				meta.sigtermListeners = process.listenerCount("SIGTERM");
				meta.sighupListeners = process.listenerCount("SIGHUP");
				writeMeta(meta);
				process.emit("SIGTERM");
				await new Promise((resolve) => setTimeout(resolve, 5000));
				return;
			}
			meta.takenOverDuringRun = isStdoutTakenOver();
			for (const event of batch.events) {
				meta.emittedEventCount += 1;
				for (const l of [...listeners]) l(event);
			}
			state.messages = batch.stateAfter ?? state.messages;
		},
		reload: async () => {},
		waitForIdle: async () => {},
		navigateTree: async () => ({ cancelled: false }),
		dispose: () => {
			meta.sessionDisposed = true;
		},
		createReplacedSessionContext: () => ({}),
	};

	const services = { cwd: spec.cwd ?? "{PROJECTDIR}", agentDir: spec.agentDir ?? "{AGENTDIR}" };
	const runtimeHost = new runtimeMod.AgentSessionRuntime(session, services, async () => {
		throw new Error("createRuntime must not be called by print mode");
	});

	// main.ts:540-544 - print/json app modes always take over stdout first.
	if (spec.takeOverStdout !== false) takeOverStdout();
	let exitCode;
	try {
		exitCode = await runPrintMode(runtimeHost, {
			mode: spec.mode,
			initialMessage: spec.initialMessage,
			messages: spec.messages,
			initialImages: spec.initialImages,
		});
	} finally {
		// main.ts:855 - restoreStdout() runs after runPrintMode returns.
		restoreStdout();
	}
	meta.returnedExitCode = exitCode;
	meta.sigtermListenersAfter = process.listenerCount("SIGTERM");
	meta.sighupListenersAfter = process.listenerCount("SIGHUP");
	writeMeta(meta);
	// main.ts:856-857 - assign process.exitCode only when non-zero, then return.
	if (exitCode !== 0) process.exitCode = exitCode;
}

function writeMeta(meta) {
	writeFileSync(META_FILE, JSON.stringify(meta), "utf-8");
}

// ===========================================================================
// CHILD ROLE: guard - exercise output-guard.ts step by step
// ===========================================================================
async function roleGuard() {
	process.env.NO_COLOR = "1";
	const spec = JSON.parse(readFileSync(SPEC_FILE, "utf-8"));
	const guard = await impPi("core/output-guard.ts");
	const observations = [];

	for (const step of spec.steps) {
		const [op, arg] = step.split(":");
		switch (op) {
			case "takeover":
				guard.takeOverStdout();
				observations.push({ step, isStdoutTakenOver: guard.isStdoutTakenOver() });
				break;
			case "restore":
				guard.restoreStdout();
				observations.push({ step, isStdoutTakenOver: guard.isStdoutTakenOver() });
				break;
			case "log":
				console.log(arg);
				observations.push({ step, via: "console.log" });
				break;
			case "error":
				console.error(arg);
				observations.push({ step, via: "console.error" });
				break;
			case "raw":
				guard.writeRawStdout(`${arg}\n`);
				observations.push({ step, via: "writeRawStdout" });
				break;
			case "rawEmpty":
				guard.writeRawStdout("");
				observations.push({ step, via: "writeRawStdout(\"\")", note: "empty string is an early return, nothing is written" });
				break;
			case "stdoutWrite":
				observations.push({ step, via: "process.stdout.write(chunk)", returned: process.stdout.write(`${arg}\n`) });
				break;
			case "stdoutWriteCb2": {
				let called = false;
				process.stdout.write(`${arg}\n`, () => {
					called = true;
				});
				await new Promise((r) => setImmediate(r));
				observations.push({ step, via: "process.stdout.write(chunk, cb)", callbackInvoked: called });
				break;
			}
			case "stdoutWriteCb3": {
				let called = false;
				process.stdout.write(`${arg}\n`, "utf8", () => {
					called = true;
				});
				await new Promise((r) => setImmediate(r));
				observations.push({ step, via: "process.stdout.write(chunk, \"utf8\", cb)", callbackInvoked: called });
				break;
			}
			case "stderrWrite":
				observations.push({ step, via: "process.stderr.write(chunk)", returned: process.stderr.write(`${arg}\n`) });
				break;
			case "flush":
				await guard.flushRawStdout();
				observations.push({ step, via: "flushRawStdout" });
				break;
			case "backpressure":
				await guard.waitForRawStdoutBackpressure();
				observations.push({ step, via: "waitForRawStdoutBackpressure" });
				break;
			case "isTakenOver":
				observations.push({ step, isStdoutTakenOver: guard.isStdoutTakenOver() });
				break;
			default:
				throw new Error(`unknown guard step: ${step}`);
		}
	}
	await guard.flushRawStdout();
	writeFileSync(META_FILE, JSON.stringify({ observations }), "utf-8");
}

// ===========================================================================
// CHILD ROLE: sysprompt - core/system-prompt.ts, three layers (see header)
// ===========================================================================
async function roleSysprompt() {
	const resetRng = installDeterminism();
	process.env.NO_COLOR = "1";
	process.env.PI_OFFLINE = "1";

	const spec = JSON.parse(readFileSync(SPEC_FILE, "utf-8"));
	const { tmproot, agentDir, projectDir, sessionDir } = spec;
	assertTemp(tmproot);
	process.env.PI_CODING_AGENT_DIR = agentDir;
	for (const d of [agentDir, projectDir, sessionDir]) mkdirSync(d, { recursive: true });

	const sp = await impPi("core/system-prompt.ts");
	const config = await impPi("config.ts");
	const toolsMod = await impPi("core/tools/index.ts");
	const rl = await impPi("core/resource-loader.ts");
	const { SettingsManager } = await impPi("core/settings-manager.ts");

	const records = [];
	const push = (r) => records.push(r);

	// -- REAL builtin tool snippets/guidelines, straight off Pi's definitions --
	const defs = toolsMod.createAllToolDefinitions(projectDir);
	const ALL_TOOL_NAMES = Object.keys(defs);
	const REAL_SNIPPETS = {};
	const REAL_GUIDELINES = {};
	for (const [name, def] of Object.entries(defs)) {
		if (def.promptSnippet) REAL_SNIPPETS[name] = def.promptSnippet;
		if (def.promptGuidelines) REAL_GUIDELINES[name] = def.promptGuidelines;
	}
	const snippetsFor = (names) => Object.fromEntries(names.filter((n) => REAL_SNIPPETS[n]).map((n) => [n, REAL_SNIPPETS[n]]));
	const guidelinesFor = (names) => names.flatMap((n) => REAL_GUIDELINES[n] ?? []);

	push({
		layer: "A",
		fn: "createAllToolDefinitions",
		name: "builtin-tool-prompt-data",
		note:
			"The REAL promptSnippet / promptGuidelines of all seven builtin tool definitions, read straight off createAllToolDefinitions(cwd). Every layer-A case below feeds these values (never hand-typed strings) into buildSystemPrompt, and AgentSession._rebuildSystemPrompt (agent-session.ts:1011-1023) collects them the same way: snippet by tool name, guidelines concatenated in validToolNames order.",
		toolNamesInDefinitionOrder: ALL_TOOL_NAMES,
		promptSnippets: REAL_SNIPPETS,
		promptGuidelines: REAL_GUIDELINES,
		prompt: null,
	});

	// ---- fixed, LITERAL cwds so the \\ -> / transform stays visible ---------
	const DEFAULT_4 = ["read", "bash", "edit", "write"];
	const READONLY_4 = ["read", "grep", "find", "ls"];
	const BASE_CWD = "/home/user/project";

	const buildA = (name, note, options, extra = {}) => {
		const prompt = sp.buildSystemPrompt(options);
		push({
			layer: "A",
			fn: "buildSystemPrompt",
			name,
			note,
			input: options,
			prompt,
			promptByteLength: Buffer.byteLength(prompt),
			...extra,
		});
		return prompt;
	};

	// 1. BASELINE - the full prompt, default coding tool set
	const baseline = buildA(
		"baseline-default-tools",
		"THE BASELINE: default coding tool set (read/bash/edit/write) with Pi's real snippets and guidelines, a fixed POSIX cwd, no append, no context files, no skills. This is the whole prompt byte-for-byte.",
		{
			cwd: BASE_CWD,
			selectedTools: DEFAULT_4,
			toolSnippets: snippetsFor(DEFAULT_4),
			promptGuidelines: guidelinesFor(DEFAULT_4),
		},
	);

	// 2. THE pirustOmits RECORD - the three Pi-docs paths, isolated
	{
		const readmePath = config.getReadmePath();
		const docsPath = config.getDocsPath();
		const examplesPath = config.getExamplesPath();
		// The block runs from the "Pi documentation" heading to just before the
		// next section. Locate it in the baseline by its literal anchors.
		const startAnchor =
			"\nPi documentation (read only when the user asks about pi itself, its SDK, extensions, themes, skills, or TUI):";
		const endAnchor = "\n- Always read pi .md files completely and follow links to related docs (e.g., tui.md for TUI API details)";
		const start = baseline.indexOf(startAnchor);
		const end = baseline.indexOf(endAnchor);
		const omitted = start === -1 || end === -1 ? null : baseline.slice(start, end + endAnchor.length);
		push({
			layer: "A",
			fn: "buildSystemPrompt",
			name: "pi-docs-paths-block",
			pirustOmits: true,
			note:
				"DELIBERATELY NOT PORTED. system-prompt.ts:131-138 emits an 8-line \"Pi documentation\" block interpolating getReadmePath() / getDocsPath() / getExamplesPath(). Those point into an installed npm package that has no Cargo equivalent, so the user has decided pirust omits the whole block. `omittedText` is the EXACT byte span to delete from the baseline prompt; `precedingText` and `followingText` are the bytes immediately before and after it, so a test can assert the deletion is surgical (and that the surrounding newlines land correctly) rather than approximate. Nothing else in this fixture is exempt.",
			omittedText: omitted,
			omittedByteLength: omitted === null ? null : Buffer.byteLength(omitted),
			omittedStartIndexInBaseline: start,
			omittedEndIndexInBaseline: start === -1 ? -1 : start + endAnchor.length + (end - start),
			precedingText: start === -1 ? null : baseline.slice(Math.max(0, start - 120), start),
			followingText: end === -1 ? null : baseline.slice(end + endAnchor.length, end + endAnchor.length + 120),
			interpolatedPaths: { readmePath, docsPath, examplesPath },
			pathDerivation:
				"config.ts:427-439 - resolve(join(getPackageDir(), \"README.md\" | \"docs\" | \"examples\")). getPackageDir() is where the pi checkout lives, so these are normalized to {PIPKG}; only the SHAPE (\"{PIPKG}\\\\docs\") is contract, and pirust reproduces none of it.",
			promptWithBlockRemoved: omitted === null ? null : baseline.split(omitted).join(""),
			prompt: null,
		});
	}

	// 3. cwd interpolation, isolated (system-prompt.ts:39,159)
	for (const [name, cwd, note] of [
		["cwd-posix", "/home/user/project", "a plain POSIX absolute path passes through untouched"],
		["cwd-windows-backslashes", "C:\\Users\\me\\project", "EVERY backslash becomes a forward slash: `C:/Users/me/project`. The drive letter and colon are kept"],
		["cwd-windows-forward-slashes", "C:/Users/me/project", "already forward-slashed: the replace is a no-op"],
		["cwd-unc", "\\\\server\\share\\dir", "a UNC path becomes `//server/share/dir`"],
		["cwd-with-spaces", "/home/user/my project/sub dir", "spaces are NOT encoded or quoted"],
		["cwd-non-ascii", "/home/user/caf\u00e9/\u65e5\u672c\u8a9e", "non-ASCII passes through verbatim; no percent-encoding, no NFC/NFD normalization"],
		["cwd-trailing-separator", "C:\\Users\\me\\project\\", "a trailing backslash becomes a trailing forward slash; it is NOT stripped"],
		["cwd-filesystem-root-posix", "/", "the shortest possible cwd"],
		["cwd-empty-string", "", "an empty cwd yields the bare line `Current working directory: ` with a trailing space"],
	]) {
		buildA(name, `cwd interpolation isolated - ${note}. Only the FINAL line differs from the baseline.`, {
			cwd,
			selectedTools: DEFAULT_4,
			toolSnippets: snippetsFor(DEFAULT_4),
			promptGuidelines: guidelinesFor(DEFAULT_4),
		}, { lastLine: `Current working directory: ${cwd.replace(/\\/g, "/")}` });
	}

	// 4. tool list + guidelines, isolated
	buildA(
		"tools-selectedTools-undefined-defaults-to-four",
		"selectedTools omitted entirely: system-prompt.ts:81 defaults to [read,bash,edit,write]. Byte-identical to the baseline, which proves the default.",
		{ cwd: BASE_CWD, toolSnippets: snippetsFor(DEFAULT_4), promptGuidelines: guidelinesFor(DEFAULT_4) },
	);
	buildA(
		"tools-read-only-four",
		"the read-only set (read/grep/find/ls, createReadOnlyTools' names): bash is ABSENT so the `Use bash for file operations like ls, rg, find` guideline is NOT added; grep/find/ls contribute no guidelines of their own",
		{ cwd: BASE_CWD, selectedTools: READONLY_4, toolSnippets: snippetsFor(READONLY_4), promptGuidelines: guidelinesFor(READONLY_4) },
	);
	buildA(
		"tools-all-seven",
		"all seven builtins: bash IS present but so are grep/find/ls, so the bash guideline is still NOT added (the condition is hasBash && !hasGrep && !hasFind && !hasLs)",
		{
			cwd: BASE_CWD,
			selectedTools: ALL_TOOL_NAMES,
			toolSnippets: snippetsFor(ALL_TOOL_NAMES),
			promptGuidelines: guidelinesFor(ALL_TOOL_NAMES),
		},
	);
	buildA(
		"tools-none-empty-array",
		"selectedTools: [] - an EMPTY array is truthy, so it does NOT fall back to the default four. visibleTools is empty so the list is the literal `(none)`, and only the two always-on guidelines remain",
		{ cwd: BASE_CWD, selectedTools: [], toolSnippets: snippetsFor(ALL_TOOL_NAMES), promptGuidelines: [] },
	);
	buildA(
		"tools-selected-but-no-snippets",
		"the four default tools are selected but toolSnippets is omitted: `visibleTools = tools.filter(name => !!toolSnippets?.[name])` is empty, so Available tools is `(none)` EVEN THOUGH the tools are active. Presence in the list is driven by the SNIPPET, not by selectedTools",
		{ cwd: BASE_CWD, selectedTools: DEFAULT_4 },
	);
	buildA(
		"tools-snippet-for-unselected-tool-is-ignored",
		"toolSnippets carries all seven but selectedTools names only two: the list follows selectedTools, in selectedTools' ORDER (not definition order)",
		{ cwd: BASE_CWD, selectedTools: ["write", "read"], toolSnippets: snippetsFor(ALL_TOOL_NAMES), promptGuidelines: [] },
	);
	buildA(
		"tools-bash-only-adds-the-bash-guideline",
		"bash with NO grep/find/ls: system-prompt.ts:104-106 adds `Use bash for file operations like ls, rg, find` as the FIRST guideline, before any promptGuidelines entry",
		{ cwd: BASE_CWD, selectedTools: ["bash"], toolSnippets: snippetsFor(["bash"]), promptGuidelines: [] },
	);
	buildA(
		"tools-bash-plus-grep-suppresses-the-bash-guideline",
		"adding grep alone is enough to suppress it",
		{ cwd: BASE_CWD, selectedTools: ["bash", "grep"], toolSnippets: snippetsFor(["bash", "grep"]), promptGuidelines: [] },
	);
	buildA(
		"guidelines-dedupe-and-trim-and-order",
		"promptGuidelines are trimmed, blank/whitespace-only entries are DROPPED, duplicates are collapsed keeping FIRST position, and the two always-on bullets (`Be concise in your responses`, `Show file paths clearly when working with files`) are appended LAST - unless already supplied, in which case their earlier position wins",
		{
			cwd: BASE_CWD,
			selectedTools: ["bash"],
			toolSnippets: snippetsFor(["bash"]),
			promptGuidelines: [
				"  padded with spaces  ",
				"",
				"   ",
				"duplicate me",
				"duplicate me",
				"Be concise in your responses",
				"Use bash for file operations like ls, rg, find",
			],
		},
	);

	// 5. appendSystemPrompt, isolated
	buildA("append-absent", "appendSystemPrompt omitted: appendSection is \"\" and nothing is inserted", {
		cwd: BASE_CWD,
		selectedTools: DEFAULT_4,
		toolSnippets: snippetsFor(DEFAULT_4),
		promptGuidelines: guidelinesFor(DEFAULT_4),
	});
	buildA(
		"append-empty-string-is-falsy",
		"appendSystemPrompt: \"\" is FALSY, so appendSection stays \"\" - byte-identical to append-absent",
		{ cwd: BASE_CWD, selectedTools: DEFAULT_4, toolSnippets: snippetsFor(DEFAULT_4), promptGuidelines: guidelinesFor(DEFAULT_4), appendSystemPrompt: "" },
	);
	buildA(
		"append-single-line",
		"appendSection is `\\n\\n${appendSystemPrompt}` inserted AFTER the fixed body and BEFORE <project_context>",
		{ cwd: BASE_CWD, selectedTools: DEFAULT_4, toolSnippets: snippetsFor(DEFAULT_4), promptGuidelines: guidelinesFor(DEFAULT_4), appendSystemPrompt: "Always answer in haiku." },
	);
	buildA(
		"append-multiline-and-two-joined",
		"what TWO --append-system-prompt values look like once AgentSession has joined them with \"\\n\\n\" (agent-session.ts:1026-1028): the joined string is then prefixed with another \"\\n\\n\"",
		{
			cwd: BASE_CWD,
			selectedTools: DEFAULT_4,
			toolSnippets: snippetsFor(DEFAULT_4),
			promptGuidelines: guidelinesFor(DEFAULT_4),
			appendSystemPrompt: "first append\nwith a second line\n\nsecond append",
		},
	);

	// 6. contextFiles, isolated
	const cf = (path, content) => ({ path, content });
	buildA("context-files-empty-array", "contextFiles: [] - the whole <project_context> block is omitted", {
		cwd: BASE_CWD,
		selectedTools: DEFAULT_4,
		toolSnippets: snippetsFor(DEFAULT_4),
		promptGuidelines: guidelinesFor(DEFAULT_4),
		contextFiles: [],
	});
	buildA(
		"context-files-one",
		"ONE context file. Block shape: `\\n\\n<project_context>\\n\\nProject-specific instructions and guidelines:\\n\\n` then per file `<project_instructions path=\"${path}\">\\n${content}\\n</project_instructions>\\n\\n` then `</project_context>\\n`. Note the cwd line that follows starts with its own \\n, so there is a blank line between them",
		{
			cwd: BASE_CWD,
			selectedTools: DEFAULT_4,
			toolSnippets: snippetsFor(DEFAULT_4),
			promptGuidelines: guidelinesFor(DEFAULT_4),
			contextFiles: [cf("/home/user/project/AGENTS.md", "# Project rules\n\nUse tabs.")],
		},
	);
	buildA(
		"context-files-three-order-preserved",
		"THREE context files: they are emitted in ARRAY order, each in its own <project_instructions> element, separated by a blank line",
		{
			cwd: BASE_CWD,
			selectedTools: DEFAULT_4,
			toolSnippets: snippetsFor(DEFAULT_4),
			promptGuidelines: guidelinesFor(DEFAULT_4),
			contextFiles: [
				cf("/global/AGENTS.md", "global rules"),
				cf("/home/AGENTS.md", "ancestor rules"),
				cf("/home/user/project/CLAUDE.md", "project rules"),
			],
		},
	);
	buildA(
		"context-file-content-edge-cases",
		"content is interpolated RAW: an empty string yields two consecutive newlines, trailing newlines are NOT trimmed (so a file ending in \\n produces a blank line before </project_instructions>), and a `\"` in the PATH is NOT escaped - producing malformed XML. All contract",
		{
			cwd: BASE_CWD,
			selectedTools: DEFAULT_4,
			toolSnippets: snippetsFor(DEFAULT_4),
			promptGuidelines: guidelinesFor(DEFAULT_4),
			contextFiles: [
				cf("/p/empty.md", ""),
				cf("/p/trailing.md", "ends with newline\n"),
				cf('/p/we"ird.md', "path contains a double quote"),
				cf("/p/xml.md", "<tag>&amp; already escaped</tag>"),
			],
		},
	);

	// 7. customPrompt (--system-prompt) full override
	buildA(
		"custom-prompt-only",
		"--system-prompt: the ENTIRE default body (including Available tools, Guidelines and the Pi documentation block) is replaced. Only the cwd line is still appended",
		{ cwd: BASE_CWD, customPrompt: "You are a terse assistant.", selectedTools: DEFAULT_4, toolSnippets: snippetsFor(DEFAULT_4), promptGuidelines: guidelinesFor(DEFAULT_4) },
	);
	buildA(
		"custom-prompt-with-append-and-context",
		"the custom-prompt branch applies, in order: customPrompt, appendSection, <project_context>, skills, cwd line. toolSnippets/promptGuidelines are IGNORED entirely",
		{
			cwd: BASE_CWD,
			customPrompt: "CUSTOM BODY",
			appendSystemPrompt: "APPENDED",
			contextFiles: [cf("/p/AGENTS.md", "ctx")],
			selectedTools: DEFAULT_4,
			toolSnippets: snippetsFor(DEFAULT_4),
			promptGuidelines: guidelinesFor(DEFAULT_4),
		},
	);
	buildA(
		"custom-prompt-empty-string-falls-through-to-default",
		"customPrompt: \"\" is FALSY, so `if (customPrompt)` fails and the DEFAULT body is built - byte-identical to the baseline",
		{ cwd: BASE_CWD, customPrompt: "", selectedTools: DEFAULT_4, toolSnippets: snippetsFor(DEFAULT_4), promptGuidelines: guidelinesFor(DEFAULT_4) },
	);

	// 8. skills (feat-007 shape; always empty in feat-005)
	const SKILLS = [
		{ name: "commit-msg", description: "Write a conventional commit message", filePath: "/p/.agents/skills/commit-msg/SKILL.md", baseDir: "/p/.agents/skills/commit-msg", disableModelInvocation: false },
		{ name: "hidden", description: "never offered", filePath: "/p/.agents/skills/hidden/SKILL.md", baseDir: "/p/.agents/skills/hidden", disableModelInvocation: true },
		{ name: "xml & <escapes>", description: 'quotes " and \' and & and <>', filePath: "/p/.agents/skills/x/SKILL.md", baseDir: "/p/.agents/skills/x", disableModelInvocation: false },
	];
	buildA(
		"skills-block-default-branch",
		"the <available_skills> block (core/skills.ts formatSkillsForPrompt). Emitted only when the `read` tool is active AND at least one non-disableModelInvocation skill exists. disableModelInvocation skills are filtered out; name/description/location are XML-escaped (contrast the context-file path, which is not)",
		{ cwd: BASE_CWD, selectedTools: DEFAULT_4, toolSnippets: snippetsFor(DEFAULT_4), promptGuidelines: guidelinesFor(DEFAULT_4), skills: SKILLS },
		{ feat007: true },
	);
	buildA(
		"skills-suppressed-without-read-tool",
		"`hasRead` is false, so the skills block is omitted even though skills exist",
		{ cwd: BASE_CWD, selectedTools: ["bash"], toolSnippets: snippetsFor(["bash"]), promptGuidelines: [], skills: SKILLS },
		{ feat007: true },
	);
	buildA(
		"skills-all-disabled-yields-no-block",
		"every skill has disableModelInvocation: true, so formatSkillsForPrompt returns \"\" and nothing is appended",
		{ cwd: BASE_CWD, selectedTools: DEFAULT_4, toolSnippets: snippetsFor(DEFAULT_4), promptGuidelines: guidelinesFor(DEFAULT_4), skills: SKILLS.filter((s) => s.disableModelInvocation) },
		{ feat007: true },
	);
	buildA(
		"skills-custom-prompt-branch-gate",
		"in the custom-prompt branch the gate is `!selectedTools || selectedTools.includes(\"read\")` - so selectedTools UNDEFINED also passes, unlike the default branch which reads hasRead off the resolved list",
		{ cwd: BASE_CWD, customPrompt: "CUSTOM", skills: SKILLS },
		{ feat007: true },
	);

	// =====================================================================
	// LAYER B - real filesystem discovery + real ResourceLoader
	// =====================================================================
	const settingsManager = SettingsManager.create(projectDir, agentDir);

	/** Lay out files under a fresh temp tree and run Pi's real discovery. */
	let bCase = 0;
	const layerB = (name, note, files, opts = {}) => {
		bCase += 1;
		const root = join(spec.bRoot, String(bCase).padStart(2, "0"));
		const nested = join(root, "outer", "inner");
		mkdirSync(nested, { recursive: true });
		const bAgentDir = join(root, "agent");
		mkdirSync(bAgentDir, { recursive: true });
		const written = [];
		for (const [rel, content] of Object.entries(files)) {
			const abs = rel.startsWith("agent/") ? join(bAgentDir, rel.slice(6)) : join(root, rel);
			mkdirSync(dirname(abs), { recursive: true });
			writeFileSync(abs, content, "utf-8");
			written.push(abs);
		}
		const cwd = opts.cwd === "nested" ? nested : join(root, "outer");
		const discovered = opts.noContextFiles ? [] : rl.loadProjectContextFiles({ cwd, agentDir: bAgentDir });
		const prompt = sp.buildSystemPrompt({
			cwd,
			selectedTools: DEFAULT_4,
			toolSnippets: snippetsFor(DEFAULT_4),
			promptGuidelines: guidelinesFor(DEFAULT_4),
			contextFiles: discovered,
		});
		push({
			layer: "B",
			fn: "loadProjectContextFiles + buildSystemPrompt",
			name,
			note,
			filesOnDisk: written,
			cwd,
			agentDir: bAgentDir,
			noContextFiles: opts.noContextFiles === true,
			discoveredContextFiles: discovered,
			discoveredPaths: discovered.map((f) => f.path),
			prompt,
			promptByteLength: Buffer.byteLength(prompt),
			projectContextBlock: (() => {
				const s = prompt.indexOf("\n\n<project_context>");
				if (s === -1) return null;
				const e = prompt.indexOf("</project_context>\n");
				return prompt.slice(s, e + "</project_context>\n".length);
			})(),
		});
	};

	layerB("discovery-no-context-file", "nothing on disk: discovery returns [] and the <project_context> block is absent", {});
	layerB("discovery-agents-md-in-cwd", "a single AGENTS.md in the cwd", { "outer/AGENTS.md": "# outer AGENTS\n" });
	layerB("discovery-claude-md-in-cwd", "a single CLAUDE.md in the cwd - discovered identically, and the path recorded is the CLAUDE.md path", {
		"outer/CLAUDE.md": "# outer CLAUDE\n",
	});
	layerB(
		"discovery-both-in-same-dir-agents-wins",
		"AGENTS.md AND CLAUDE.md in the SAME directory: loadContextFileFromDir returns on the FIRST hit of [AGENTS.md, AGENTS.MD, CLAUDE.md, CLAUDE.MD], so ONLY AGENTS.md is loaded - CLAUDE.md is silently ignored",
		{ "outer/AGENTS.md": "# AGENTS wins\n", "outer/CLAUDE.md": "# CLAUDE ignored\n" },
	);
	layerB(
		"discovery-parent-and-cwd-root-first",
		"one context file in the PARENT and one in the cwd: the walk goes UP from cwd and unshifts, so the result is ancestor-FIRST, cwd-LAST",
		{ "outer/AGENTS.md": "# parent of inner\n", "outer/inner/AGENTS.md": "# cwd\n" },
		{ cwd: "nested" },
	);
	layerB(
		"discovery-mixed-agents-and-claude-across-levels",
		"AGENTS.md in the parent and CLAUDE.md in the cwd: both are found, still ancestor-first",
		{ "outer/AGENTS.md": "# parent AGENTS\n", "outer/inner/CLAUDE.md": "# cwd CLAUDE\n" },
		{ cwd: "nested" },
	);
	layerB(
		"discovery-global-agent-dir-comes-first",
		"an AGENTS.md inside the AGENT DIR is pushed BEFORE any cwd ancestor (resource-loader.ts:95-99), so the global one is always element 0",
		{ "agent/AGENTS.md": "# global rules\n", "outer/AGENTS.md": "# project rules\n" },
	);
	layerB(
		"discovery-no-context-files-flag",
		"--no-context-files (DefaultResourceLoader noContextFiles: true) short-circuits to [] even though the files exist on disk (resource-loader.ts:463-470)",
		{ "agent/AGENTS.md": "# global rules\n", "outer/AGENTS.md": "# project rules\n" },
		{ noContextFiles: true },
	);

	// -- the real ResourceLoader: --system-prompt / --append-system-prompt ----
	let cCase = 0;
	const loaderCase = async (name, note, files, options, extra = {}) => {
		cCase += 1;
		const root = join(spec.cRoot, String(cCase).padStart(2, "0"));
		const lCwd = join(root, "project");
		const lAgentDir = join(root, "agent");
		mkdirSync(lCwd, { recursive: true });
		mkdirSync(lAgentDir, { recursive: true });
		const written = {};
		for (const [rel, content] of Object.entries(files)) {
			const abs = rel.startsWith("agent/") ? join(lAgentDir, rel.slice(6)) : join(lCwd, rel);
			mkdirSync(dirname(abs), { recursive: true });
			writeFileSync(abs, content, "utf-8");
			written[rel] = abs;
		}
		// Placeholders like "{FILE:foo.md}" in the options are replaced with the
		// absolute path that file was just written to, so a "file path" input is a
		// REAL existing path and resolvePromptInput takes its readFileSync branch.
		const subst = (v) =>
			typeof v === "string" ? v.replace(/\{FILE:([^}]+)\}/g, (_, k) => written[k] ?? `<<missing:${k}>>`) : v;
		const resolvedOptions = {
			...options,
			systemPrompt: subst(options.systemPrompt),
			appendSystemPrompt: options.appendSystemPrompt?.map(subst),
		};
		const loader = new rl.DefaultResourceLoader({
			cwd: lCwd,
			agentDir: lAgentDir,
			settingsManager: SettingsManager.create(lCwd, lAgentDir),
			noExtensions: true,
			noSkills: true,
			noPromptTemplates: true,
			noThemes: true,
			...resolvedOptions,
		});
		await loader.reload();
		const loaderSystemPrompt = loader.getSystemPrompt();
		const loaderAppend = loader.getAppendSystemPrompt();
		// Exactly what AgentSession._rebuildSystemPrompt does with them.
		const appendSystemPrompt = loaderAppend.length > 0 ? loaderAppend.join("\n\n") : undefined;
		const prompt = sp.buildSystemPrompt({
			cwd: lCwd,
			selectedTools: DEFAULT_4,
			toolSnippets: snippetsFor(DEFAULT_4),
			promptGuidelines: guidelinesFor(DEFAULT_4),
			customPrompt: loaderSystemPrompt,
			appendSystemPrompt,
			contextFiles: loader.getAgentsFiles().agentsFiles,
			skills: loader.getSkills().skills,
		});
		push({
			layer: "B",
			fn: "DefaultResourceLoader + buildSystemPrompt",
			name,
			note,
			filesOnDisk: written,
			loaderOptions: resolvedOptions,
			cwd: lCwd,
			agentDir: lAgentDir,
			getSystemPrompt: loaderSystemPrompt ?? null,
			getAppendSystemPrompt: loaderAppend,
			joinedAppendSystemPrompt: appendSystemPrompt ?? null,
			prompt,
			promptByteLength: Buffer.byteLength(prompt),
			...extra,
		});
	};

	await loaderCase("loader-baseline-nothing-set", "no --system-prompt, no --append-system-prompt, no SYSTEM.md: getSystemPrompt() is undefined and getAppendSystemPrompt() is []", {}, {});
	await loaderCase(
		"cli-system-prompt-literal-text",
		"--system-prompt \"literal text\": resolvePromptInput's existsSync() is false, so the STRING ITSELF becomes the prompt",
		{},
		{ systemPrompt: "You are a literal override." },
	);
	await loaderCase(
		"cli-system-prompt-file-path",
		"--system-prompt <path to an existing file>: resolvePromptInput does existsSync(input) -> readFileSync(input, \"utf-8\"), so the FILE CONTENTS become the prompt. Contents are used RAW - a trailing newline in the file survives into the prompt",
		{ "myprompt.md": "You are loaded from a file.\n" },
		{ systemPrompt: "{FILE:myprompt.md}" },
	);
	await loaderCase(
		"cli-append-system-prompt-literal",
		"--append-system-prompt \"literal\": one entry, literal text",
		{},
		{ appendSystemPrompt: ["Answer in haiku."] },
	);
	await loaderCase(
		"cli-append-system-prompt-file-path",
		"--append-system-prompt <path>: same existsSync/readFileSync rule as --system-prompt",
		{ "extra.md": "Extra instructions from a file.\n" },
		{ appendSystemPrompt: ["{FILE:extra.md}"] },
	);
	await loaderCase(
		"cli-append-system-prompt-repeated-order",
		"THREE --append-system-prompt values, mixing literals and a file path: they stay in CLI order and AgentSession joins them with \"\\n\\n\"",
		{ "mid.md": "from a file" },
		{ appendSystemPrompt: ["first literal", "{FILE:mid.md}", "third literal"] },
	);
	await loaderCase(
		"cli-system-prompt-and-append-together",
		"--system-prompt plus --append-system-prompt: the custom-prompt branch runs and the append lands directly after it",
		{},
		{ systemPrompt: "CUSTOM OVERRIDE", appendSystemPrompt: ["AND AN APPEND"] },
	);
	await loaderCase(
		"discover-project-SYSTEM-md",
		"no --system-prompt: the loader falls back to discoverSystemPromptFile() -> <cwd>/.pi/SYSTEM.md when the project is trusted (resource-loader.ts:966-978). getSystemPrompt() returns the FILE CONTENTS, not the path",
		{ ".pi/SYSTEM.md": "Project SYSTEM.md body.\n" },
		{},
	);
	await loaderCase(
		"discover-global-SYSTEM-md",
		"<agentDir>/SYSTEM.md is used when there is no project one",
		{ "agent/SYSTEM.md": "Global SYSTEM.md body.\n" },
		{},
	);
	await loaderCase(
		"discover-project-SYSTEM-md-wins-over-global",
		"both exist: the PROJECT one wins (checked first)",
		{ ".pi/SYSTEM.md": "PROJECT wins.\n", "agent/SYSTEM.md": "GLOBAL loses.\n" },
		{},
	);
	await loaderCase(
		"discover-APPEND_SYSTEM-md",
		"<cwd>/.pi/APPEND_SYSTEM.md is discovered as a SINGLE append source when --append-system-prompt is absent (resource-loader.ts:480-482)",
		{ ".pi/APPEND_SYSTEM.md": "Appended from APPEND_SYSTEM.md.\n" },
		{},
	);
	await loaderCase(
		"cli-append-overrides-APPEND_SYSTEM-md",
		"an explicit --append-system-prompt REPLACES the discovered APPEND_SYSTEM.md entirely (`??`, not concatenation), so the file is ignored",
		{ ".pi/APPEND_SYSTEM.md": "THIS FILE IS IGNORED.\n" },
		{ appendSystemPrompt: ["cli append only"] },
	);

	// =====================================================================
	// LAYER C - the prompt a REAL AgentSession hands the model
	// =====================================================================
	const compat = await impAi("compat.ts");
	const runtimeMod = await impPi("core/agent-session-runtime.ts");
	const { AuthStorage } = await impPi("core/auth-storage.ts");
	const { ModelRuntime } = await impPi("core/model-runtime.ts");
	const { SessionManager } = await impPi("core/session-manager.ts");

	async function liveSessionPrompt(sessionOptions) {
		resetRng();
		const faux = compat.registerFauxProvider({ api: "faux", tokenSize: { min: 100000, max: 100000 } });
		faux.setResponses([compat.fauxAssistantMessage("ok")]);
		const authStorage = AuthStorage.inMemory();
		await authStorage.modify(faux.getModel().provider, async () => ({ type: "api_key", key: "faux-key" }));
		const modelRuntime = await ModelRuntime.create({ credentials: authStorage, modelsPath: join(agentDir, "models.json") });
		const m = faux.getModel();
		modelRuntime.registerProvider(m.provider, {
			baseUrl: m.baseUrl,
			api: m.api,
			models: [
				{ id: m.id, name: m.name, api: m.api, reasoning: m.reasoning, input: m.input, cost: m.cost, contextWindow: m.contextWindow, maxTokens: m.maxTokens, baseUrl: m.baseUrl },
			],
		});
		const createRuntime = async ({ cwd, sessionManager, sessionStartEvent }) => {
			const services = await runtimeMod.createAgentSessionServices({
				cwd,
				agentDir,
				modelRuntime,
				model: faux.getModel(),
				resourceLoaderOptions: { noSkills: true, noPromptTemplates: true, noThemes: true, noContextFiles: true, noExtensions: true },
			});
			return {
				...(await runtimeMod.createAgentSessionFromServices({ services, sessionManager, sessionStartEvent, model: faux.getModel(), ...sessionOptions })),
				services,
				diagnostics: services.diagnostics,
			};
		};
		const host = await runtimeMod.createAgentSessionRuntime(createRuntime, {
			cwd: projectDir,
			agentDir,
			sessionManager: SessionManager.inMemory(projectDir),
		});
		const session = host.session;
		// `private` in TypeScript is a plain property at runtime.
		const prompt = session._baseSystemPrompt;
		const options = session._baseSystemPromptOptions;
		const activeTools = session.getActiveToolNames();
		await host.dispose();
		faux.unregister();
		return { prompt, options, activeTools };
	}

	for (const [name, note, sessionOptions] of [
		[
			"live-default-tool-set",
			"the REAL prompt a real AgentSession hands the model with the DEFAULT configuration. This is what proves layer A's toolSnippets/promptGuidelines inputs are the ones Pi actually passes: `optionsUsed` is the verbatim BuildSystemPromptOptions object AgentSession._rebuildSystemPrompt assembled",
			{},
		],
		["live-tools-allowlist-read-only", "tools: [read,grep,find,ls] (the --tools allowlist)", { tools: ["read", "grep", "find", "ls"] }],
		["live-tools-all-seven", "tools: all seven builtins", { tools: ["read", "bash", "edit", "write", "grep", "find", "ls"] }],
		["live-no-tools-all", "--no-tools (noTools: \"all\"): no tool is active, so Available tools is `(none)`", { noTools: "all" }],
		["live-no-builtin-tools", "--no-builtin-tools (noTools: \"builtin\")", { noTools: "builtin" }],
		["live-exclude-tools", "--exclude-tools bash,write", { excludeTools: ["bash", "write"] }],
	]) {
		const { prompt, options, activeTools } = await liveSessionPrompt(sessionOptions);
		push({
			layer: "C",
			fn: "AgentSession._rebuildSystemPrompt",
			name,
			note,
			sessionOptions,
			activeToolNames: activeTools,
			optionsUsed: options,
			prompt,
			promptByteLength: Buffer.byteLength(prompt),
			cwdShape: {
				raw: projectDir,
				note:
					"the cwd here is a temp dir, so it normalizes to {PROJECTDIR} and the backslash->slash transform is invisible in this record. The `cwd-*` layer-A cases pin that transform with literal fixed cwds instead.",
			},
		});
	}

	writeFileSync(META_FILE, JSON.stringify({ records, platform: process.platform }), "utf-8");
}

// ===========================================================================
// CHILD ROLE: meta - values that live in main.ts, not print-mode.ts
// ===========================================================================
async function roleMeta() {
	process.env.NO_COLOR = "1";
	process.env.PI_OFFLINE = "1";
	const spec = JSON.parse(readFileSync(SPEC_FILE, "utf-8"));
	process.env.PI_CODING_AGENT_DIR = spec.agentDir;
	mkdirSync(spec.agentDir, { recursive: true });

	const argsMod = await impPi("cli/args.ts");
	const mainMod = await impPi("main.ts");
	const guidance = await impPi("core/auth-guidance.ts");

	const rows = [];
	for (const { argv: a, note } of spec.argvCases) {
		const parsed = argsMod.parseArgs(a);
		for (const [stdinIsTTY, stdoutIsTTY] of [
			[true, true],
			[false, true],
			[true, false],
			[false, false],
		]) {
			const appMode = mainMod.__resolveAppMode(parsed, stdinIsTTY, stdoutIsTTY);
			const plainMetadata = mainMod.__isPlainRuntimeMetadataCommand(parsed);
			rows.push({
				argv: a,
				note,
				stdinIsTTY,
				stdoutIsTTY,
				appMode,
				isPlainRuntimeMetadataCommand: plainMetadata,
				shouldTakeOverStdout: appMode !== "interactive" && !plainMetadata,
				printOutputMode: appMode === "rpc" ? null : mainMod.__toPrintOutputMode(appMode),
			});
		}
	}

	writeFileSync(
		META_FILE,
		JSON.stringify({
			rows,
			noModelsAvailableMessage: guidance.formatNoModelsAvailableMessage(),
			noModelSelectedMessage: guidance.formatNoModelSelectedMessage(),
		}),
		"utf-8",
	);
}

// ===========================================================================
// PARENT
// ===========================================================================

/** @type {Map<string,string>} relative path -> exact file contents */
const artifacts = new Map();
const emit = (relPath, contents) => artifacts.set(relPath.split("\\").join("/"), contents);
const jsonl = (records) => records.map((r) => JSON.stringify(r)).join("\n") + "\n";
const pretty = (value) => JSON.stringify(value, null, 2) + "\n";

let SUBSTITUTIONS = [];
function rebuildSubstitutions(pairs) {
	const expanded = [];
	for (const [literal, placeholder] of pairs) {
		if (!literal) continue;
		expanded.push([literal, placeholder]);
		const fwd = literal.split("\\").join("/");
		if (fwd !== literal) expanded.push([fwd, placeholder]);
		const esc = JSON.stringify(literal).slice(1, -1); // backslash-escaped form, as it appears inside JSON
		if (esc !== literal) expanded.push([esc, placeholder]);
	}
	expanded.sort((a, b) => b[0].length - a[0].length);
	SUBSTITUTIONS = expanded;
}

function normalizeString(text) {
	let out = text;
	for (const [literal, placeholder] of SUBSTITUTIONS) {
		if (out.includes(literal)) out = out.split(literal).join(placeholder);
	}
	return out;
}

const isUsageObject = (value) =>
	value !== null && typeof value === "object" && !Array.isArray(value) && Object.keys(value).join(",") === USAGE_KEYS;

/** Deep copy applying placeholder substitution to strings and CANONICAL_USAGE to usage objects. */
function normalizeDeep(value) {
	if (typeof value === "string") return normalizeString(value);
	if (value === null || typeof value !== "object") return value;
	if (Array.isArray(value)) return value.map(normalizeDeep);
	if (isUsageObject(value)) return structuredClone(CANONICAL_USAGE);
	const out = {};
	for (const key of Object.keys(value)) out[key] = normalizeDeep(value[key]);
	return out;
}

/** Collect the raw usage objects seen, for the provenance record. */
function collectUsage(value, sink) {
	if (value === null || typeof value !== "object") return;
	if (Array.isArray(value)) {
		for (const v of value) collectUsage(v, sink);
		return;
	}
	if (isUsageObject(value)) {
		const key = JSON.stringify(value);
		if (!sink.has(key)) sink.set(key, value);
		return;
	}
	for (const v of Object.values(value)) collectUsage(v, sink);
}

let TMPROOT = "";
let caseCounter = 0;
function newSpecFile(label) {
	const dir = join(TMPROOT, "specs");
	mkdirSync(dir, { recursive: true });
	return join(dir, `${String(++caseCounter).padStart(3, "0")}-${label}.json`);
}

async function runChild(role, spec, { expectStatus = null, label = role } = {}) {
	const { spawnSync } = await import("node:child_process");
	const specFile = newSpecFile(label);
	const metaFile = `${specFile}.meta.json`;
	writeFileSync(specFile, JSON.stringify(spec), "utf-8");
	const env = { ...process.env };
	delete env.FORCE_COLOR;
	env.NO_COLOR = "1";
	env.PI_OFFLINE = "1";
	// NOTE: only the harvest/meta roles use a real agent dir; the capture role's
	// `spec.agentDir` is the already-normalized "{AGENTDIR}" placeholder that goes
	// into the stub services object, so it must NOT be used as an env value.
	env.PI_CODING_AGENT_DIR =
		role === "harvest" || role === "meta" || role === "sysprompt" ? spec.agentDir : join(TMPROOT, "unused-agent");
	// Keep the host's real ~/.pi unreachable no matter what.
	assertTemp(env.PI_CODING_AGENT_DIR);
	const res = spawnSync(process.execPath, [SELF, "--role", role, "--spec", specFile, "--out", metaFile], {
		env,
		encoding: "buffer",
		maxBuffer: 64 * 1024 * 1024,
	});
	const stdout = (res.stdout ?? Buffer.alloc(0)).toString("utf-8");
	const stderr = (res.stderr ?? Buffer.alloc(0)).toString("utf-8");
	if (expectStatus !== null && res.status !== expectStatus) {
		throw new Error(`child ${role}/${label} exited ${res.status} (expected ${expectStatus})\n--- stderr ---\n${stderr}`);
	}
	if (expectStatus === null && res.status !== 0 && !existsSync(metaFile)) {
		throw new Error(`child ${role}/${label} exited ${res.status} with no meta\n--- stderr ---\n${stderr}`);
	}
	const meta = existsSync(metaFile) ? JSON.parse(readFileSync(metaFile, "utf-8")) : null;
	return { stdout, stderr, status: res.status, meta };
}

// ---------------------------------------------------------------------------
// Phase 1: harvest every scenario's real event stream
// ---------------------------------------------------------------------------
async function harvestAll() {
	const scenarios = scenarioSpecs();
	const harvested = new Map();
	const rawUsage = new Map();
	const crossCheck = [];

	for (const scenario of scenarios) {
		const base = join(TMPROOT, "harvest", scenario.name);
		const dirs = {
			tmproot: TMPROOT,
			agentDir: join(base, "agent"),
			projectDir: join(base, "project"),
			sessionDir: join(base, "sessions"),
		};
		for (const d of Object.values(dirs)) assertTemp(d);
		const { stdout, stderr, meta, status } = await runChild("harvest", { ...dirs, scenario }, {
			label: `harvest-${scenario.name}`,
			expectStatus: 0,
		});
		if (!meta) throw new Error(`harvest ${scenario.name} produced no meta; stderr:\n${stderr}`);

		collectUsage(meta.batches, rawUsage);

		// Cross-check: parse the LIVE json-mode stdout back and compare, after
		// identical normalization, to header + flattened harvested events.
		rebuildSubstitutions([
			[dirs.projectDir, "{PROJECTDIR}"],
			[dirs.sessionDir, "{SESSIONDIR}"],
			[dirs.agentDir, "{AGENTDIR}"],
			[TMPROOT, "{TMPROOT}"],
			[homedir(), "{HOME}"],
			[join(PKGS, "coding-agent"), "{PIPKG}"],
		]);
		const liveLines = stdout.split("\n").filter((l) => l.length > 0);
		let liveParsed = null;
		let parseError = null;
		try {
			liveParsed = liveLines.map((l) => normalizeDeep(JSON.parse(l)));
		} catch (err) {
			parseError = err instanceof Error ? err.message : String(err);
		}
		const expectedLive = [
			...(meta.liveHeader ? [normalizeDeep({ ...meta.liveHeader, id: "{SESSIONID}" })] : []),
			...meta.batches.flatMap((b) => b.events.map(normalizeDeep)),
		];
		const liveNormalized = liveParsed?.map((o) => (o.type === "session" ? { ...o, id: "{SESSIONID}" } : o));
		crossCheck.push({
			scenario: scenario.name,
			liveExitCode: meta.liveExitCode,
			liveChildStatus: status,
			liveLineCount: liveLines.length,
			expectedLineCount: expectedLive.length,
			parseError,
			match: parseError === null && JSON.stringify(liveNormalized) === JSON.stringify(expectedLive),
			liveStderr: normalizeString(stderr),
		});

		harvested.set(scenario.name, { scenario, dirs, meta });
	}
	return { harvested, rawUsage, crossCheck };
}

/** Normalize one harvested scenario into the canonical replay input. */
function canonicalize(entry) {
	const { dirs, meta } = entry;
	rebuildSubstitutions([
		[dirs.projectDir, "{PROJECTDIR}"],
		[dirs.sessionDir, "{SESSIONDIR}"],
		[dirs.agentDir, "{AGENTDIR}"],
		[TMPROOT, "{TMPROOT}"],
		[homedir(), "{HOME}"],
		[join(PKGS, "coding-agent"), "{PIPKG}"],
	]);
	const header = meta.header ? normalizeDeep({ ...meta.header, id: "{SESSIONID}" }) : undefined;
	const batches = meta.batches.map((b) => ({
		text: b.text,
		events: b.events.map(normalizeDeep),
		stateAfter: b.stateAfter.map(normalizeDeep),
	}));
	return { header, batches, activeTools: meta.activeTools };
}

// ---------------------------------------------------------------------------
// Phase 2: replay each scenario through the real runPrintMode, per mode
// ---------------------------------------------------------------------------
async function captureModeCases(mode, harvested) {
	const records = [];
	for (const [name, entry] of harvested) {
		const { header, batches, activeTools } = canonicalize(entry);
		const scenario = entry.scenario;
		const spec = {
			mode,
			header,
			batches,
			initialMessage: batches[0]?.text,
			messages: batches.slice(1).map((b) => b.text),
			initialStateMessages: [],
			cwd: "{PROJECTDIR}",
			agentDir: "{AGENTDIR}",
		};
		const { stdout, stderr, status, meta } = await runChild("capture", spec, { label: `${mode}-${name}` });
		records.push({
			name,
			note: scenario.note,
			mode,
			eventSource: "live faux-provider harvest (see events.provenance.json)",
			activeTools,
			header: header ?? null,
			initialMessage: spec.initialMessage ?? null,
			messages: spec.messages,
			events: batches.flatMap((b) => b.events),
			finalStateMessages: batches[batches.length - 1]?.stateAfter ?? [],
			stdout,
			stderr,
			exitCode: meta?.returnedExitCode ?? null,
			processExitCode: status,
			runtime: meta
				? {
						bindExtensionsCalls: meta.bindExtensionsCalls,
						subscribeCount: meta.subscribeCount,
						unsubscribeCount: meta.unsubscribeCount,
						promptCalls: meta.promptCalls,
						emittedEventCount: meta.emittedEventCount,
						shutdownEvents: meta.shutdownEvents,
						sessionDisposed: meta.sessionDisposed,
						stdoutTakenOverDuringRun: meta.takenOverDuringRun,
						// print-mode.ts:153-155 - the finally block runs every
						// signalCleanupHandler, so the counts return to baseline.
						sigtermListenersBeforeRun: meta.sigtermListenersBefore,
						sighupListenersBeforeRun: meta.sighupListenersBefore,
						sigtermListenersAfterCleanup: meta.sigtermListenersAfter,
						sighupListenersAfterCleanup: meta.sighupListenersAfter,
					}
				: null,
		});
	}

	// ---- structural cases that no faux turn can produce -------------------
	const plain = harvested.get("text-only-reply");
	const { header, batches } = canonicalize(plain);

	// no header at all (SessionManager.inMemory() -> getHeader() may be undefined)
	{
		const spec = {
			mode,
			header: undefined,
			batches,
			initialMessage: batches[0].text,
			messages: [],
			initialStateMessages: [],
		};
		const r = await runChild("capture", spec, { label: `${mode}-no-header` });
		records.push({
			name: "no-session-header",
			note: "sessionManager.getHeader() returns undefined: json mode emits NO header line and starts straight at the first event; text mode is unaffected",
			mode,
			eventSource: "live faux-provider harvest, header suppressed",
			header: null,
			initialMessage: spec.initialMessage,
			messages: [],
			events: batches.flatMap((b) => b.events),
			finalStateMessages: batches[batches.length - 1].stateAfter,
			stdout: r.stdout,
			stderr: r.stderr,
			exitCode: r.meta?.returnedExitCode ?? null,
			processExitCode: r.status,
		});
	}

	// no prompts at all -> nothing is sent, nothing is printed
	{
		const spec = { mode, header, batches: [], initialMessage: undefined, messages: [], initialStateMessages: [] };
		const r = await runChild("capture", spec, { label: `${mode}-no-prompts` });
		records.push({
			name: "no-prompts",
			note: "neither initialMessage nor messages: `if (initialMessage)` is falsy and the messages loop is empty, so session.prompt() is NEVER called; json mode still emits the header line",
			mode,
			eventSource: "n/a (no turn)",
			header: header ?? null,
			initialMessage: null,
			messages: [],
			events: [],
			finalStateMessages: [],
			stdout: r.stdout,
			stderr: r.stderr,
			exitCode: r.meta?.returnedExitCode ?? null,
			processExitCode: r.status,
			runtime: r.meta ? { promptCalls: r.meta.promptCalls, bindExtensionsCalls: r.meta.bindExtensionsCalls } : null,
		});
	}

	// empty-string initialMessage -> falsy, so it is NOT sent
	{
		const spec = { mode, header, batches: [], initialMessage: "", messages: [], initialStateMessages: [] };
		const r = await runChild("capture", spec, { label: `${mode}-empty-initial-message` });
		records.push({
			name: "empty-string-initial-message",
			note: "initialMessage === \"\" is FALSY, so session.prompt() is never called for it (contrast options.messages, which is iterated unconditionally)",
			mode,
			eventSource: "n/a (no turn)",
			header: header ?? null,
			initialMessage: "",
			messages: [],
			events: [],
			finalStateMessages: [],
			stdout: r.stdout,
			stderr: r.stderr,
			exitCode: r.meta?.returnedExitCode ?? null,
			processExitCode: r.status,
			runtime: r.meta ? { promptCalls: r.meta.promptCalls } : null,
		});
	}

	// empty-string entry in options.messages -> IS sent
	{
		const spec = {
			mode,
			header,
			batches: [{ text: "", events: [], stateAfter: [] }],
			initialMessage: undefined,
			messages: [""],
			initialStateMessages: [],
		};
		const r = await runChild("capture", spec, { label: `${mode}-empty-message-entry` });
		records.push({
			name: "empty-string-in-messages",
			note: "an empty string INSIDE options.messages is still passed to session.prompt() - the loop has no truthiness guard",
			mode,
			eventSource: "n/a (no events emitted)",
			header: header ?? null,
			initialMessage: null,
			messages: [""],
			events: [],
			finalStateMessages: [],
			stdout: r.stdout,
			stderr: r.stderr,
			exitCode: r.meta?.returnedExitCode ?? null,
			processExitCode: r.status,
			runtime: r.meta ? { promptCalls: r.meta.promptCalls } : null,
		});
	}

	// last message is NOT an assistant message -> text mode prints nothing
	{
		const userOnly = batches[0].stateAfter.filter((m) => m.role === "user");
		const spec = {
			mode,
			header,
			batches: [{ text: batches[0].text, events: batches[0].events, stateAfter: userOnly }],
			initialMessage: batches[0].text,
			messages: [],
			initialStateMessages: [],
		};
		const r = await runChild("capture", spec, { label: `${mode}-last-not-assistant` });
		records.push({
			name: "last-message-not-assistant",
			note: "the last entry of session.state.messages is a USER message: the `lastMessage?.role === \"assistant\"` guard fails, so text mode writes NOTHING and exits 0",
			mode,
			eventSource: "live faux-provider harvest, state truncated to the user message",
			header: header ?? null,
			initialMessage: spec.initialMessage,
			messages: [],
			events: batches[0].events,
			finalStateMessages: userOnly,
			stdout: r.stdout,
			stderr: r.stderr,
			exitCode: r.meta?.returnedExitCode ?? null,
			processExitCode: r.status,
		});
	}

	// empty state.messages -> lastMessage is undefined
	{
		const spec = {
			mode,
			header,
			batches: [{ text: batches[0].text, events: batches[0].events, stateAfter: [] }],
			initialMessage: batches[0].text,
			messages: [],
			initialStateMessages: [],
		};
		const r = await runChild("capture", spec, { label: `${mode}-empty-state` });
		records.push({
			name: "empty-state-messages",
			note: "session.state.messages is EMPTY: `state.messages[length-1]` is undefined and `lastMessage?.role` short-circuits, so text mode writes nothing and exits 0",
			mode,
			eventSource: "live faux-provider harvest, state emptied",
			header: header ?? null,
			initialMessage: spec.initialMessage,
			messages: [],
			events: batches[0].events,
			finalStateMessages: [],
			stdout: r.stdout,
			stderr: r.stderr,
			exitCode: r.meta?.returnedExitCode ?? null,
			processExitCode: r.status,
		});
	}

	// session.prompt() throws -> the catch block
	{
		const spec = {
			mode,
			header,
			batches: [{ text: "boom", throwMessage: "session is not ready" }],
			initialMessage: "boom",
			messages: [],
			initialStateMessages: [],
		};
		const r = await runChild("capture", spec, { label: `${mode}-prompt-throws` });
		records.push({
			name: "prompt-throws-error",
			note: "session.prompt() throws an Error: the catch block writes error.message to STDERR and returns 1, in BOTH modes; the text-mode block is skipped entirely",
			mode,
			eventSource: "n/a (throw before any event)",
			header: header ?? null,
			initialMessage: "boom",
			messages: [],
			events: [],
			finalStateMessages: [],
			stdout: r.stdout,
			stderr: r.stderr,
			exitCode: r.meta?.returnedExitCode ?? null,
			processExitCode: r.status,
			runtime: r.meta ? { shutdownEvents: r.meta.shutdownEvents, stdoutTakenOverDuringRun: r.meta.takenOverDuringRun } : null,
		});
	}

	// session.prompt() throws a NON-Error -> String(value)
	{
		const spec = {
			mode,
			header,
			batches: [{ text: "boom", throwNonError: { code: 7, why: "not an Error instance" } }],
			initialMessage: "boom",
			messages: [],
			initialStateMessages: [],
		};
		const r = await runChild("capture", spec, { label: `${mode}-prompt-throws-nonerror` });
		records.push({
			name: "prompt-throws-non-error",
			note: "session.prompt() rejects with a plain object: `error instanceof Error ? error.message : String(error)` yields String(value) - here \"[object Object]\"",
			mode,
			eventSource: "n/a (throw before any event)",
			header: header ?? null,
			initialMessage: "boom",
			messages: [],
			events: [],
			finalStateMessages: [],
			stdout: r.stdout,
			stderr: r.stderr,
			exitCode: r.meta?.returnedExitCode ?? null,
			processExitCode: r.status,
		});
	}

	return records;
}

// ---------------------------------------------------------------------------
// output_guard.cases.jsonl
// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------
// system_prompt.cases.jsonl
// ---------------------------------------------------------------------------
async function genSystemPromptCases() {
	const base = join(TMPROOT, "sysprompt");
	const dirs = {
		tmproot: TMPROOT,
		agentDir: join(base, "agent"),
		projectDir: join(base, "project"),
		sessionDir: join(base, "sessions"),
		bRoot: join(base, "b"),
		cRoot: join(base, "c"),
	};
	for (const d of Object.values(dirs)) assertTemp(d);
	const { stderr, meta } = await runChild("sysprompt", dirs, { label: "sysprompt", expectStatus: 0 });
	if (!meta) throw new Error(`sysprompt child produced no meta; stderr:\n${stderr}`);

	// Placeholders. NOTE the ordering constraint: bRoot/cRoot live INSIDE
	// sysprompt/, which lives inside TMPROOT, and rebuildSubstitutions sorts
	// longest-literal-first, so the most specific path always wins.
	rebuildSubstitutions([
		[dirs.projectDir, "{PROJECTDIR}"],
		[dirs.sessionDir, "{SESSIONDIR}"],
		[dirs.agentDir, "{AGENTDIR}"],
		[dirs.bRoot, "{CTXROOT}"],
		[dirs.cRoot, "{LOADERROOT}"],
		[TMPROOT, "{TMPROOT}"],
		[join(PKGS, "coding-agent"), "{PIPKG}"],
		[piRoot, "{PIROOT}"],
		[homedir(), "{HOME}"],
	]);

	const records = meta.records.map((r) => normalizeDeep(r));

	// A leading provenance record so the file is self-describing.
	records.unshift(
		normalizeDeep({
			layer: "meta",
			fn: null,
			name: "__provenance",
			note:
				"Every `prompt` in this file is the return value of Pi's own core/system-prompt.ts buildSystemPrompt(), executed under Node type stripping. Layer A calls it directly with an explicit option object (the option object is oracle-authored INPUT, exactly as an argv array is; the toolSnippets/promptGuidelines inside it are read off Pi's real createAllToolDefinitions()). Layer B additionally uses Pi's real loadProjectContextFiles() and a real DefaultResourceLoader against a real temp filesystem. Layer C reads session._baseSystemPrompt off a REAL faux-backed AgentSession, which is what proves layer A's inputs are the ones Pi actually passes.",
			source: {
				builder: "packages/coding-agent/src/core/system-prompt.ts buildSystemPrompt (:28-162)",
				onlyCaller: "packages/coding-agent/src/core/agent-session.ts _rebuildSystemPrompt (:1009-1043)",
				contextFileDiscovery: "packages/coding-agent/src/core/resource-loader.ts loadProjectContextFiles (:85-120), loadContextFileFromDir (:67-83)",
				promptInputResolution: "packages/coding-agent/src/core/resource-loader.ts resolvePromptInput (:50-65) - existsSync(input) ? readFileSync(input) : input",
				systemMdDiscovery: "packages/coding-agent/src/core/resource-loader.ts discoverSystemPromptFile (:966-978), discoverAppendSystemPromptFile (:980-992)",
				skillsBlock: "packages/coding-agent/src/core/skills.ts formatSkillsForPrompt (:335-361)",
				docsPaths: "packages/coding-agent/src/config.ts getReadmePath/getDocsPath/getExamplesPath (:427-439)",
			},
			interpolationPointsIsolated: [
				"cwd (the ONLY platform-sensitive one: cwd.replace(/\\\\/g, \"/\"), system-prompt.ts:39)",
				"selectedTools (undefined -> default four / read-only four / all seven / empty array)",
				"toolSnippets (drives Available tools membership; missing snippet -> tool is invisible; none -> `(none)`)",
				"promptGuidelines (trim, drop blanks, dedupe keeping first position, two always-on bullets last)",
				"the conditional bash guideline (hasBash && !hasGrep && !hasFind && !hasLs)",
				"appendSystemPrompt (absent / empty-string-is-falsy / single / multiple joined with \\n\\n)",
				"customPrompt (--system-prompt full override; empty string is falsy and falls through)",
				"contextFiles (0/1/3, raw content interpolation, unescaped path attribute)",
				"skills (feat-007 only; the read-tool gate differs between the two branches)",
				"the three Pi-docs paths (pirustOmits)",
			],
			notInterpolatedAtAll: [
				"platform / OS name - buildSystemPrompt never reads process.platform or os.*; the only platform effect is the cwd backslash conversion",
				"date / time, model name, provider, username, hostname, token budget, session id - none appear in the prompt",
			],
			placeholders: {
				"{PIPKG}": "the pi checkout's coding-agent package root (getPackageDir()); appears ONLY in the pirustOmits record's three doc paths",
				"{PROJECTDIR}": "the layer-C temp project cwd",
				"{AGENTDIR}": "the layer-C temp agent dir",
				"{CTXROOT}": "the per-case root for the layer-B context-file discovery cases",
				"{LOADERROOT}": "the per-case root for the layer-B DefaultResourceLoader cases",
				"{TMPROOT}": "the oracle's mkdtemp'd root",
				"{HOME}": "os.homedir()",
			},
			normalizationNote:
				"Layer A cwds are LITERAL fixed strings (/home/user/project, C:\\\\Users\\\\me\\\\project, ...), never the host's, so the backslash->forward-slash transform is byte-visible and unnormalized. Layer B/C cwds are real temp dirs and therefore appear as placeholders; the substitution table also contains each literal's forward-slash form, so the post-transform text in the `Current working directory:` line is substituted too. No other value in this file is normalized - no clock, no rng, no token counts (the prompt contains none).",
			couldNotCapture: [
				"Skills and prompt templates loaded from disk: the layer-B DefaultResourceLoader runs with noSkills/noPromptTemplates/noThemes/noExtensions, which IS the feat-005 configuration (feat-007 owns them). The skills BLOCK shape is still captured, in the four `skills-*` layer-A records, marked feat007:true.",
				"Extension-contributed tools, snippets and guidelines: extensions are feat-007. Only the seven builtin tool definitions contribute here.",
				"An extension mutating the prompt per-turn: AgentSession keeps _baseSystemPrompt separate from the per-turn _systemPromptOverride so extensions can re-append each turn; only the BASE prompt is captured.",
			],
			platform: meta.platform,
			prompt: null,
		}),
	);

	return records;
}

/** main.ts's private appMode / takeover decision table, executed not reimplemented. */
async function runMetaChild() {
	return runChild(
		"meta",
		{
			agentDir: join(TMPROOT, "meta", "agent"),
			argvCases: [
				{ argv: ["-p", "hello"], note: "--print with a message" },
				{ argv: ["--mode", "json", "hello"], note: "--mode json" },
				{ argv: ["--mode", "rpc"], note: "--mode rpc" },
				{ argv: ["hello"], note: "a bare message, no mode flags" },
				{ argv: ["--help"], note: "--help alone: a PLAIN runtime metadata command, exempt from takeOverStdout" },
				{ argv: ["-h"], note: "-h alone: same as --help" },
				{ argv: ["--list-models"], note: "--list-models alone: also exempt" },
				{ argv: ["--list-models", "sonnet"], note: "--list-models with a pattern: still exempt (listModels !== undefined)" },
				{ argv: ["-p", "--help"], note: "--print --help: NOT exempt, so help goes to stderr" },
				{ argv: ["--mode", "json", "--list-models"], note: "--mode json --list-models: NOT exempt" },
				{ argv: ["--mode", "text", "--help"], note: "--mode text --help: mode is DEFINED, so the exemption is lost" },
				{ argv: ["--version"], note: "--version is NOT part of the exemption test; it exits at main.ts:521-524 before takeOverStdout" },
			],
		},
		{ label: "meta", expectStatus: 0 },
	);
}

async function genOutputGuardCases(metaChild) {
	const cases = [
		{
			name: "baseline-no-takeover",
			note: "before takeOverStdout(): console.log goes to STDOUT (Node's default) and writeRawStdout also goes to STDOUT via the bound process.stdout.write",
			steps: ["isTakenOver", "log:LOG-1", "raw:RAW-1", "error:ERR-1", "flush"],
		},
		{
			name: "takeover-redirects-console-log-to-stderr",
			note: "THE central hazard: after takeOverStdout(), process.stdout.write is replaced by a function that calls rawStderrWrite, so console.log lands on STDERR while writeRawStdout still reaches real STDOUT",
			steps: ["log:BEFORE-STDOUT", "takeover", "log:AFTER-GOES-TO-STDERR", "raw:RAW-STILL-STDOUT", "error:EXPLICIT-STDERR", "flush"],
		},
		{
			name: "second-takeover-is-a-noop",
			note: "takeOverStdout() early-returns when stdoutTakeoverState is set, so the second call does NOT capture the already-patched write as `originalStdoutWrite`; ONE restoreStdout() therefore fully restores stdout",
			steps: [
				"takeover",
				"log:AFTER-FIRST-TAKEOVER",
				"takeover",
				"log:AFTER-SECOND-TAKEOVER",
				"restore",
				"log:AFTER-SINGLE-RESTORE-BACK-ON-STDOUT",
				"raw:RAW-AFTER-RESTORE",
				"flush",
			],
		},
		{
			name: "restore-puts-the-original-back",
			note: "restoreStdout() reassigns process.stdout.write from the saved originalStdoutWrite and clears the state, so console.log is on STDOUT again and isStdoutTakenOver() is false",
			steps: ["takeover", "log:REDIRECTED", "restore", "log:RESTORED", "isTakenOver", "flush"],
		},
		{
			name: "restore-without-takeover-is-a-noop",
			note: "restoreStdout() with no state early-returns; nothing is broken and console.log still reaches STDOUT",
			steps: ["restore", "isTakenOver", "log:STILL-STDOUT", "flush"],
		},
		{
			name: "writeRawStdout-empty-string-is-a-noop",
			note: "writeRawStdout(\"\") returns immediately without touching the promise tail, so no bytes and no drain barrier",
			steps: ["takeover", "rawEmpty", "raw:AFTER-EMPTY", "flush"],
		},
		{
			name: "writeRawStdout-is-strictly-ordered",
			note: "every call chains onto the module-level rawStdoutWriteTail promise, so output order equals call order and chunks are never interleaved - even though the writes themselves are async",
			steps: ["takeover", "raw:ONE", "raw:TWO", "raw:THREE", "log:INTERLEAVED-STDERR", "raw:FOUR", "flush", "backpressure"],
		},
		{
			name: "patched-stdout-write-forwards-a-callback-in-arg-2",
			note: "process.stdout.write(chunk, cb) under takeover: `typeof encodingOrCallback === \"function\"` is true, so rawStderrWrite(String(chunk), cb) is used and the callback IS invoked",
			steps: ["takeover", "stdoutWriteCb2:CB2-TO-STDERR", "flush"],
		},
		{
			name: "patched-stdout-write-forwards-a-callback-in-arg-3",
			note: "process.stdout.write(chunk, \"utf8\", cb): the encoding is DROPPED (String(chunk) is used) and the arg-3 callback is forwarded",
			steps: ["takeover", "stdoutWriteCb3:CB3-TO-STDERR", "flush"],
		},
		{
			name: "process-stderr-write-is-untouched",
			note: "takeOverStdout() only replaces process.stdout.write; process.stderr.write is left alone (rawStderrWrite is a bound copy of it, captured before any patching)",
			steps: ["takeover", "stderrWrite:DIRECT-STDERR", "stdoutWrite:PATCHED-STDOUT-TO-STDERR", "flush"],
		},
		{
			name: "raw-and-console-interleaving-under-takeover",
			note: "the two sinks are independent: writeRawStdout output appears on stdout in call order, console.log/console.error output appears on stderr in call order, and the relative order ACROSS the two streams is not observable",
			steps: ["takeover", "raw:S1", "log:E1", "raw:S2", "error:E2", "raw:S3", "log:E3", "flush"],
		},
	];

	const records = [];
	for (const c of cases) {
		const r = await runChild("guard", { steps: c.steps }, { label: `guard-${c.name}`, expectStatus: 0 });
		const stdoutMarkers = r.stdout.split("\n").filter((l) => l.length > 0);
		const stderrMarkers = r.stderr.split("\n").filter((l) => l.length > 0);
		const landedOn = {};
		for (const m of stdoutMarkers) landedOn[m] = "stdout";
		for (const m of stderrMarkers) landedOn[m] = landedOn[m] ? "BOTH" : "stderr";
		records.push({
			name: c.name,
			note: c.note,
			steps: c.steps,
			stdout: r.stdout,
			stderr: r.stderr,
			stdoutMarkers,
			stderrMarkers,
			landedOn,
			observations: r.meta?.observations ?? null,
			exitCode: r.status,
		});
	}

	// The exemption that decides WHETHER takeOverStdout() is called at all.
	// main.ts:540-544. Not a stream capture - a decision table - but it is the
	// gate in front of every case above, so it belongs in this fixture.
	records.push({
		name: "isPlainRuntimeMetadataCommand-exemption-table",
		note:
			"main.ts:540-544: `shouldTakeOverStdout = appMode !== \"interactive\" && !isPlainRuntimeMetadataCommand(parsed)`, and main.ts:117-119: `isPlainRuntimeMetadataCommand = !parsed.print && parsed.mode === undefined && (parsed.help === true || parsed.listModels !== undefined)`. When it is TRUE the takeover is skipped, so --help / --list-models print to real STDOUT; when it is FALSE (e.g. `pi -p --help`, `pi --mode json --list-models`) the takeover IS engaged and their console.log output lands on STDERR instead. Every row here was produced by executing main.ts's own module-private functions against real parseArgs output; only the argv arrays are oracle-authored input.",
		steps: null,
		stdout: null,
		stderr: null,
		stdoutMarkers: null,
		stderrMarkers: null,
		landedOn: null,
		decisionTable: metaChild.meta.rows,
		crossReference:
			"The observable consequence is demonstrated by the `baseline-no-takeover` case (exemption TRUE -> console.log on stdout) and the `takeover-redirects-console-log-to-stderr` case (exemption FALSE -> console.log on stderr) above.",
		exitCode: null,
	});

	return records;
}

// ---------------------------------------------------------------------------
// exit_codes.json
// ---------------------------------------------------------------------------
async function genExitCodes(harvested, textRecords, jsonRecords, metaChild) {
	const pick = (records, name) => records.find((r) => r.name === name);
	const row = (outcome, source, rec, extra = {}) => ({
		outcome,
		source,
		mode: rec?.mode ?? null,
		exitCode: rec?.exitCode ?? null,
		processExitCode: rec?.processExitCode ?? null,
		stderr: rec?.stderr ?? null,
		stdout: rec?.stdout ?? null,
		...extra,
	});

	const rows = [
		row("success", "print-mode.ts:34,148 - exitCode initialised to 0 and never changed", pick(textRecords, "text-only-reply")),
		row("success (json)", "print-mode.ts:129 - the whole text block is skipped in json mode", pick(jsonRecords, "text-only-reply")),
		row(
			"provider error (text)",
			"print-mode.ts:135-137 - stopReason === \"error\" -> console.error(errorMessage) + exitCode = 1",
			pick(textRecords, "provider-error-mid-stream"),
		),
		row(
			"provider error, no errorMessage (text)",
			"print-mode.ts:136 - `assistantMsg.errorMessage || `Request ${stopReason}`` falls back",
			pick(textRecords, "provider-error-no-message"),
		),
		row(
			"provider error (json) - NOT an error exit",
			"print-mode.ts:129 gates the whole block on mode === \"text\", so json mode exits 0 even on a failed turn",
			pick(jsonRecords, "provider-error-mid-stream"),
		),
		row("no responses queued (text)", "faux's own error path, surfaced through print-mode.ts:135-137", pick(textRecords, "no-responses-queued")),
		row(
			"retryable provider error that recovers (text)",
			"AgentSession auto-retries a retryable error; the LAST assistant message succeeded, so print mode prints its text and exits 0 - the failed turn is invisible in text mode",
			pick(textRecords, "provider-error-retried-then-succeeds"),
		),
		row(
			"retryable provider error that exhausts (text)",
			"the retry itself fails; print mode reports the SECOND (final) error message, not the first",
			pick(textRecords, "provider-error-retried-then-exhausted"),
		),
		row("aborted (text)", "print-mode.ts:135 - stopReason === \"aborted\"", pick(textRecords, "aborted-run")),
		row(
			"aborted, no errorMessage (text)",
			"print-mode.ts:136 fallback yields exactly `Request aborted`",
			pick(textRecords, "aborted-no-message"),
		),
		row("aborted (json)", "json mode exits 0", pick(jsonRecords, "aborted-run")),
		row("empty response", "no text blocks to write; exitCode stays 0", pick(textRecords, "empty-response")),
		row("last message not assistant", "print-mode.ts:133 guard fails; no output, exit 0", pick(textRecords, "last-message-not-assistant")),
		row(
			"thrown error (text)",
			"print-mode.ts:149-151 - catch { console.error(...); return 1 }",
			pick(textRecords, "prompt-throws-error"),
		),
		row("thrown error (json)", "the same catch; json mode is NOT exempt", pick(jsonRecords, "prompt-throws-error")),
		row(
			"thrown non-Error (text)",
			"print-mode.ts:150 - String(error) for a non-Error rejection",
			pick(textRecords, "prompt-throws-non-error"),
		),
	];

	// ---- SIGTERM / SIGHUP -------------------------------------------------
	const { header, batches } = canonicalize(harvested.get("text-only-reply"));
	const sig = await runChild(
		"capture",
		{
			mode: "text",
			header,
			batches: [{ text: "slow", emitSigterm: true }],
			initialMessage: "slow",
			messages: [],
			initialStateMessages: [],
		},
		{ label: "sigterm" },
	);
	rows.push({
		outcome: "SIGTERM during the run",
		source:
			"print-mode.ts:47-63 - registerSignalHandlers() installs a SIGTERM listener that calls killTrackedDetachedChildren(), then disposeRuntime(), then process.exit(143). The signal is delivered here with process.emit(\"SIGTERM\") from inside session.prompt(), which invokes Pi's own registered handler.",
		mode: "text",
		exitCode: null,
		processExitCode: sig.status,
		stderr: sig.stderr,
		stdout: sig.stdout,
		signalListeners: {
			sigtermBeforeRunPrintMode: sig.meta?.sigtermListenersBefore ?? null,
			sighupBeforeRunPrintMode: sig.meta?.sighupListenersBefore ?? null,
			sigtermDuringRun: sig.meta?.sigtermListeners ?? null,
			sighupDuringRun: sig.meta?.sighupListeners ?? null,
			sigtermAddedByPrintMode: (sig.meta?.sigtermListeners ?? 0) - (sig.meta?.sigtermListenersBefore ?? 0),
			sighupAddedByPrintMode: (sig.meta?.sighupListeners ?? 0) - (sig.meta?.sighupListenersBefore ?? 0),
		},
		platform: process.platform,
		platformNote:
			"print-mode.ts:48-51 registers SIGTERM always and SIGHUP only when process.platform !== \"win32\". `*AddedByPrintMode` is the DELTA over the baseline taken just before runPrintMode was called, so it isolates registerSignalHandlers()'s own listeners from any Node/runtime listeners that already existed. On win32 the SIGHUP delta is 0.",
	});

	// ---- the main.ts-owned exits -----------------------------------------
	rows.push({
		outcome: "no models available (non-interactive)",
		source:
			"main.ts:800-803 - `if (appMode !== \"interactive\" && !session.model) { console.error(chalk.red(formatNoModelsAvailableMessage())); process.exit(1); }`. This exit happens BEFORE runPrintMode is reached, so print-mode.ts never runs.",
		mode: "text|json",
		exitCode: 1,
		processExitCode: 1,
		stderr: `${metaChild.meta.noModelsAvailableMessage}\n`,
		stdout: "",
		note: "captured by calling core/auth-guidance.ts formatNoModelsAvailableMessage() directly; chalk.red is a no-op under NO_COLOR=1, and the trailing \\n is console.error's",
	});

	return {
		description:
			"Terminal outcomes of Pi's print mode. `exitCode` is runPrintMode's RETURN value; `processExitCode` is the child process's real exit status after main.ts's `if (exitCode !== 0) process.exitCode = exitCode` (main.ts:856-857). stdout/stderr are byte-exact apart from the {PIPKG}/{HOME} placeholder substitution described in messages.note.",
		platform: process.platform,
		rows,
		appModeAndStdoutTakeover: {
			note:
				"main.ts:540-544. `shouldTakeOverStdout = appMode !== \"interactive\" && !isPlainRuntimeMetadataCommand(parsed)`. Captured by executing main.ts's own (module-private) resolveAppMode / isPlainRuntimeMetadataCommand / toPrintOutputMode against real parseArgs output.",
			rows: metaChild.meta.rows,
		},
		messages: {
			note:
				"Verbatim from core/auth-guidance.ts. Both embed getDocsPath()-derived absolute paths (getProviderLoginHelp()), which are a property of WHERE the pi checkout lives, so the pi coding-agent package root is substituted with {PIPKG} and os.homedir() with {HOME}. Nothing else in this file is substituted.",
			noModelsAvailable: metaChild.meta.noModelsAvailableMessage,
			noModelSelected: metaChild.meta.noModelSelectedMessage,
		},
	};
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------
async function main() {
	TMPROOT = mkdtempSync(join(REAL_TMP, "pirust-printmode-oracle-"));
	assertTemp(TMPROOT);
	try {
		const { harvested, rawUsage, crossCheck } = await harvestAll();

		const textRecords = await captureModeCases("text", harvested);
		const jsonRecords = await captureModeCases("json", harvested);

		emit("text_mode.cases.jsonl", jsonl(textRecords));
		emit("json_mode.cases.jsonl", jsonl(jsonRecords));

		const metaChild = await runMetaChild();

		const guardRecords = await genOutputGuardCases(metaChild);
		emit("output_guard.cases.jsonl", jsonl(guardRecords));

		const exitCodes = await genExitCodes(harvested, textRecords, jsonRecords, metaChild);
		// The auth-guidance strings embed absolute pi-checkout doc paths; nothing
		// else in this payload carries a host path (the per-case stdout/stderr were
		// already normalized when their records were built).
		rebuildSubstitutions([
			[join(PKGS, "coding-agent"), "{PIPKG}"],
			[piRoot, "{PIROOT}"],
			[homedir(), "{HOME}"],
			[TMPROOT, "{TMPROOT}"],
		]);
		emit("exit_codes.json", pretty(normalizeDeep(exitCodes)));

		// LAST, so it cannot perturb the substitution table used above.
		const sysPromptRecords = await genSystemPromptCases();
		emit("system_prompt.cases.jsonl", jsonl(sysPromptRecords));

		emit(
			"events.provenance.json",
			pretty({
				description:
					"Provenance for the `events` arrays in text_mode.cases.jsonl / json_mode.cases.jsonl. Every event was produced by a REAL coding-agent AgentSession over a REAL Agent, with only the provider's stream function replaced by Pi's own scripted test double (packages/ai/src/providers/faux.ts, via registerFauxProvider). The runtime is assembled exactly the way pi's own packages/coding-agent/test/agent-session-runtime-events.test.ts assembles one.",
				seam: {
					phase1_harvest:
						"real ModelRuntime + AuthStorage.inMemory + registerFauxProvider + createAgentSessionServices + createAgentSessionFromServices + createAgentSessionRuntime, then session.bindExtensions({mode}) and session.prompt(text) driven prompt-by-prompt with session.subscribe() collecting structuredClone'd AgentSessionEvents.",
					phase2_capture:
						"the REAL runPrintMode, in a child process with separate stdout/stderr pipes, over the REAL AgentSessionRuntime class wrapping a STUB AgentSession of the same shape pi's own packages/coding-agent/test/print-mode.test.ts uses. runPrintMode reads only sessionManager.getHeader, bindExtensions, subscribe, prompt, state and extensionRunner off the session and never reaches a provider. The child calls takeOverStdout() before and restoreStdout() after, mirroring main.ts:540-544,855.",
					why:
						"a live run bakes wall-clock timestamps, uuidv7 ids, temp paths and faux's prompt-length-derived token counts into the very bytes that are supposed to be the assertion target. Replaying NORMALIZED events keeps `events` and `stdout` exactly consistent, which is what a byte-for-byte Rust test needs. The crossCheck below proves the replay is faithful.",
				},
				crossCheck: {
					description:
						"For every scenario, a LIVE json-mode runPrintMode was additionally run against the real faux-backed runtime; its stdout was parsed back line by line and compared - after identical normalization - to [header, ...harvestedEvents]. `match: true` means print mode's json stream IS exactly one compact JSON.stringify per event, header first, and that the phase-2 replay reproduces it.",
					allMatch: crossCheck.every((c) => c.match),
					rows: crossCheck,
				},
				determinism: {
					clock: `Date replaced by a subclass returning ORACLE_NOW=${ORACLE_NOW} ("${new Date(ORACLE_NOW).toISOString()}") for new Date() and Date.now(); new Date(x) unchanged.`,
					rng: `crypto.getRandomValues and Math.random redirected to a seeded mulberry32 (seed 0x${RNG_SEED.toString(16)}), reset before each runtime build.`,
					faux: "registerFauxProvider({api:\"faux\", tokenSize:{min:100000,max:100000}}) - the explicit api removes faux's `faux:<Date.now()>:<Math.random()>` default, and the huge token size forces exactly ONE delta event per content block (splitStringByTokenSize is otherwise random-length, making the message_update COUNT non-deterministic).",
					placeholders: {
						"{TMPROOT}": "the mkdtemp'd oracle root",
						"{PROJECTDIR}": "the per-scenario temp project cwd (appears in the session header's `cwd`)",
						"{AGENTDIR}": "the per-scenario temp agent dir",
						"{SESSIONDIR}": "the per-scenario temp sessions dir",
						"{SESSIONID}": "the session header's uuidv7 `id`",
						"{HOME}": "os.homedir()",
						"{PIPKG}": "the pi checkout's coding-agent package root (the default system prompt embeds getReadmePath()/getDocsPath()/getExamplesPath())",
					},
					usageNormalization: {
						what: "every object whose key list is exactly [input,output,cacheRead,cacheWrite,totalTokens,cost] had its integers replaced by CANONICAL_USAGE.",
						canonical: CANONICAL_USAGE,
						why: "faux derives usage from Math.ceil(serializeContext(context).length / 4), and the serialized context embeds the absolute cwd and the absolute paths of pi's shipped docs, so the numbers are a function of WHERE the checkout lives and differ on every machine. Key order, field presence and the nested `cost` object are preserved exactly and ARE contract; only the integers are placeholders.",
						rawObserved: [...rawUsage.values()],
					},
					ansi: "print-mode.ts imports no chalk and emits no escape sequences of its own; NO_COLOR=1 is set in every child anyway. There is no colour-ON variant because there is nothing to colour. (main.ts's diagnostics DO use chalk, but they run before runPrintMode.)",
				},
				notCaptured: [
					"entry_appended - only emitted from the extension API's appendEntry (agent-session.ts:2357-2363); needs a loaded extension (feat-007).",
					"session_info_changed / thinking_level_changed - need /name and /thinking, i.e. interactive slash commands.",
					"queue_update - needs a steering/follow-up message queued while the agent is streaming.",
					"compaction_start / compaction_end - need an over-threshold context window.",
					"CAPTURED (not missing): auto_retry_start / auto_retry_end appear for real in the provider-error-retried-then-succeeds and provider-error-retried-then-exhausted scenarios, because an errorMessage containing \"503\" is classified retryable by ai/src/utils/retry.ts.",
					"a real mid-stream session.abort() - not byte-deterministic (where the abort lands decides how many text_delta events precede it). The aborted TERMINAL shape is captured instead, via faux's own stopReason:\"aborted\" path (faux.ts:291-298,393-397).",
					"SIGHUP - print-mode.ts:49-51 only registers it off win32; the observed listener counts are in exit_codes.json.",
					"Any of these matters only for WHICH events exist, not for how print mode renders them: the json branch is `writeRawStdout(`${JSON.stringify(event)}\\n`)` for EVERY event with no switch on type, and the text branch never looks at events at all.",
				],
				scenarios: scenarioSpecs().map((s) => ({
					name: s.name,
					note: s.note,
					prompts: s.prompts,
					fauxResponses: s.responses,
					tools: s.tools ?? null,
				})),
			}),
		);

		// -- write or diff -----------------------------------------------------
		mkdirSync(OUT, { recursive: true });
		let drift = 0;
		for (const [rel, contents] of [...artifacts].sort()) {
			const abs = join(OUT, ...rel.split("/"));
			if (CHECK) {
				const existing = existsSync(abs) ? readFileSync(abs, "utf-8") : null;
				if (existing === contents) {
					console.log(`ok    ${rel} (${Buffer.byteLength(contents)} bytes)`);
					continue;
				}
				drift += 1;
				if (existing === null) {
					console.error(`MISSING ${rel}`);
					continue;
				}
				console.error(`DRIFT ${rel}: ${Buffer.byteLength(existing)} -> ${Buffer.byteLength(contents)} bytes`);
				const a = existing.split("\n");
				const b = contents.split("\n");
				let shown = 0;
				for (let i = 0; i < Math.max(a.length, b.length) && shown < 6; i++) {
					if (a[i] === b[i]) continue;
					shown += 1;
					console.error(`  line ${i + 1}:`);
					console.error(`    on disk : ${JSON.stringify((a[i] ?? "<absent>").slice(0, 400))}`);
					console.error(`    derived : ${JSON.stringify((b[i] ?? "<absent>").slice(0, 400))}`);
				}
			} else {
				mkdirSync(dirname(abs), { recursive: true });
				writeFileSync(abs, contents, "utf-8");
				console.log(`wrote ${rel} (${Buffer.byteLength(contents)} bytes)`);
			}
		}

		if (!crossCheck.every((c) => c.match)) {
			console.error("\nWARNING: live-vs-replay cross-check did NOT match for:");
			for (const c of crossCheck.filter((x) => !x.match)) console.error(`  ${c.scenario}: ${JSON.stringify(c).slice(0, 300)}`);
		}

		console.log("\n=== SUMMARY ===");
		console.log(`text cases        : ${textRecords.length}`);
		console.log(`json cases        : ${jsonRecords.length}`);
		console.log(`output-guard cases: ${guardRecords.length}`);
		console.log(`exit-code rows    : ${exitCodes.rows.length}`);
		console.log(
			`system-prompt cases: ${sysPromptRecords.length} (A=${sysPromptRecords.filter((r) => r.layer === "A").length} B=${sysPromptRecords.filter((r) => r.layer === "B").length} C=${sysPromptRecords.filter((r) => r.layer === "C").length})`,
		);
		console.log(`cross-check       : ${crossCheck.filter((c) => c.match).length}/${crossCheck.length} live runs matched the replay`);

		if (CHECK && drift > 0) {
			console.error(`\n${drift} artifact(s) drifted.`);
			process.exit(1);
		}
	} finally {
		rmSync(TMPROOT, { recursive: true, force: true });
	}
}

// ---------------------------------------------------------------------------
if (ROLE === "harvest") await roleHarvest();
else if (ROLE === "sysprompt") await roleSysprompt();
else if (ROLE === "capture") await roleCapture();
else if (ROLE === "guard") await roleGuard();
else if (ROLE === "meta") await roleMeta();
else
	await main().catch((err) => {
		console.error(err?.stack || err);
		process.exit(1);
	});
