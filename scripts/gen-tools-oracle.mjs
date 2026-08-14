#!/usr/bin/env node
// gen-tools-oracle.mjs
//
// GOLDEN ORACLE for feat-004 (pirust coding-agent tools). Every byte written by
// this script is produced by EXECUTING Pi's own TypeScript source in
// ../pi/packages/coding-agent/src/core/tools/ under Node's native type
// stripping. Nothing here is a reimplementation or a hand-authored expectation.
//
// Run:      cd pirust && node scripts/gen-tools-oracle.mjs
// Verify:   cd pirust && node scripts/gen-tools-oracle.mjs --check
//
// ---------------------------------------------------------------------------
// OUTPUTS  (tests/fixtures/pi/tools/)
// ---------------------------------------------------------------------------
//   schemas/<tool>.json           the EXACT `JSON.stringify(definition.parameters)`
//                                 bytes for all 7 tools (read bash edit write
//                                 grep find ls). Written raw so TypeBox's key
//                                 order survives: `type`, then `required`
//                                 (ABSENT for `ls`, which has no required
//                                 props), then `properties`.
//   strings/<tool>.json           {name,label,description,promptSnippet,
//                                 promptGuidelines,executionMode,
//                                 hasPrepareArguments}. `null` means the field
//                                 is `undefined` on Pi's definition object
//                                 (JSON has no undefined); no tool sets
//                                 executionMode, only read/edit/write set
//                                 promptGuidelines.
//   truncate.cases.jsonl          truncateHead / truncateTail / truncateLine /
//                                 formatSize input->output vectors, full
//                                 11-field TruncationResult, raw key order.
//   edit.diff.corpus.jsonl        edit-diff corpus: real applyEditsToNormalized
//                                 Content + generateDiffString +
//                                 generateUnifiedPatch, driven through the real
//                                 edit tool `execute()` with in-memory
//                                 EditOperations so BOM/CRLF write-back is
//                                 captured too.
//   edit.prepare.cases.jsonl      real `definition.prepareArguments` (legacy
//                                 oldText/newText hoisting, JSON-string edits).
//   exec.corpus.jsonl             end-to-end `execute()` for read/write/ls/grep/
//                                 find against a real fixture tree in a TEMP dir.
//   exec.tree.json                the fixture tree exec.corpus.jsonl ran against,
//                                 so a Rust test can rebuild it byte-identically.
//   path_utils.cases.jsonl        real resolveToCwd / expandPath vectors.
//   output_accumulator.cases.jsonl real OutputAccumulator append/finish/snapshot
//                                 sequences crossing the temp-file-spill and
//                                 trimTail thresholds.
//
// ---------------------------------------------------------------------------
// MODULE RESOLUTION
// ---------------------------------------------------------------------------
// A `register()`ed resolve hook maps the bare workspace specifiers
// `@earendil-works/pi-ai`, `@earendil-works/pi-ai/compat`,
// `@earendil-works/pi-agent-core` and `@earendil-works/pi-tui` to the
// corresponding pi/packages/*/src/index.ts (the published `dist/` is not built).
// Third-party deps (`typebox`, `diff`, `chalk`, ...) resolve naturally out of
// pi/node_modules because the importing modules live inside the pi tree.
// All 15 tool modules load cleanly under type stripping - no TS enums,
// decorators, namespaces or other non-erasable syntax.
//
// ---------------------------------------------------------------------------
// NOTHING IS WRITTEN INSIDE THE PI REPO
// ---------------------------------------------------------------------------
// The exec fixture tree, all files the write tool creates, and the
// OutputAccumulator spill files all live under os.tmpdir() and are removed on
// exit. `git -C ../pi status --short` stays clean.
//
// ---------------------------------------------------------------------------
// DETERMINISM / NORMALIZATION  (what was replaced and why)
// ---------------------------------------------------------------------------
// Captured values are byte-verbatim EXCEPT for these placeholder substitutions,
// applied by a deep string walk over the captured JSON:
//
//   "{TMPROOT}"  <- the mkdtemp'd exec fixture-tree root, in both its native
//                   form and its forward-slash form. Reason: the path contains
//                   a random suffix, the machine's user name and the OS temp
//                   location. It appears in nothing but `args.path`-style
//                   inputs we chose ourselves and in absolute-path error
//                   messages; the tool CONTRACT (relativization, "/" suffixes,
//                   notices) is untouched. Rust substitutes its own root.
//   "{TMPFILE}"  <- OutputAccumulator's spill file, `join(tmpdir(),
//                   "<prefix>-<8 random bytes hex>.log")`. Only the identity of
//                   the path is random; that a path is present (vs undefined)
//                   IS the contract and is preserved exactly.
//   "{HOME}"     <- os.homedir(), so the `~` expansion rows in
//                   path_utils.cases.jsonl are machine independent. Each row
//                   also carries the literal `cwd` it was resolved against, so
//                   every non-tilde byte of `result` is derivable.
//
// Also normalized:
//   * `platform` is recorded on every path_utils / exec / tree record.
//     resolveToCwd delegates to node:path.resolve, so separators and drive
//     letters are genuinely platform dependent - that is part of the contract,
//     not noise, so the raw value is kept and the platform is labelled.
//   * grep/find shell out to the real `rg`/`fd`, which walk directories in
//     PARALLEL, so file emission order is not reproducible (measured: `grep
//     "export"` over a 6-file tree produced 2 distinct orderings in 12 runs).
//     Pi itself never sorts, so the order is external-binary noise, not Pi
//     logic. Every grep/find record therefore carries `"orderNormalized": true`
//     and its result rows are canonicalized (grep: rows grouped per file and
//     the groups sorted by path, each file's own row order preserved; find:
//     code-unit line sort). Trailing "\n\n[...notice...]" text is never moved,
//     and nothing else in the record is touched. Records without the flag
//     (read/write/ls, and grep/find errors) are byte-verbatim.
//   * NOTHING ELSE. In particular: no line-ending rewriting (files are written
//     with explicit "\n"), no re-serialization of `parameters` (raw
//     JSON.stringify bytes), no timestamps/PIDs/random ids appear anywhere in
//     these fixtures, and every error message is captured verbatim (`ok:false`
//     + `error`), because the wording branches on whether there is 1 edit or
//     more and that branch is under test.
//
// KNOWN ENVIRONMENT DEPENDENCE (not normalizable, flagged instead):
//   * ls sorts with `a.toLowerCase().localeCompare(b.toLowerCase())`, i.e. the
//     host ICU default collation. The mixed-case + non-ASCII ls row pins the
//     order this machine produced; a Rust port must reproduce ICU root
//     collation, not a byte sort. Recorded as-is on purpose.

import {
	existsSync,
	mkdirSync,
	mkdtempSync,
	readFileSync,
	readdirSync,
	rmSync,
	statSync,
	writeFileSync,
} from "node:fs";
import { register } from "node:module";
import { homedir, tmpdir } from "node:os";
import { dirname, join, relative, sep } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const CHECK = process.argv.includes("--check");

const here = dirname(fileURLToPath(import.meta.url));
const pirustRoot = dirname(here);
const piRoot = join(pirustRoot, "..", "pi");
const PKGS = join(piRoot, "packages");
const TOOLS_SRC = join(PKGS, "coding-agent", "src", "core", "tools");
const OUT = join(pirustRoot, "tests", "fixtures", "pi", "tools");

if (!existsSync(TOOLS_SRC)) {
	console.error(`Pi tool sources not found at ${TOOLS_SRC}; fixtures cannot be regenerated.`);
	process.exit(CHECK ? 0 : 1); // don't fail --check when the source repo is simply absent
}

// No network, ever: ensureTool() must resolve rg/fd from ~/.pi/agent/bin or fail loudly.
process.env.PI_OFFLINE = "1";

// ---------------------------------------------------------------------------
// Bare-specifier alias hook -> Pi's src/index.ts (dist is not built)
// ---------------------------------------------------------------------------
const ALIASES = {
	"@earendil-works/pi-ai": join(PKGS, "ai", "src", "index.ts"),
	"@earendil-works/pi-ai/compat": join(PKGS, "ai", "src", "compat.ts"),
	"@earendil-works/pi-agent-core": join(PKGS, "agent", "src", "index.ts"),
	"@earendil-works/pi-agent-core/node": join(PKGS, "agent", "src", "node.ts"),
	"@earendil-works/pi-tui": join(PKGS, "tui", "src", "index.ts"),
};
const aliasMap = Object.fromEntries(
	Object.entries(ALIASES)
		.filter(([, file]) => existsSync(file))
		.map(([spec, file]) => [spec, pathToFileURL(file).href]),
);
register(
	"data:text/javascript," +
		encodeURIComponent(`
const MAP = ${JSON.stringify(aliasMap)};
export async function resolve(specifier, context, nextResolve) {
  if (Object.hasOwn(MAP, specifier)) return { url: MAP[specifier], shortCircuit: true };
  return nextResolve(specifier, context);
}
`),
	import.meta.url,
);

const impTool = (rel) => import(pathToFileURL(join(TOOLS_SRC, rel)).href);

// ---------------------------------------------------------------------------
// Output collection (write, or diff in --check mode)
// ---------------------------------------------------------------------------
/** @type {Map<string, string>} relative path -> exact file contents */
const artifacts = new Map();
const emit = (relPath, contents) => artifacts.set(relPath.split("\\").join("/"), contents);
const jsonl = (records) => records.map((r) => JSON.stringify(r)).join("\n") + "\n";

// ---------------------------------------------------------------------------
// Normalization
// ---------------------------------------------------------------------------
const HOME = homedir();
/** @type {Array<[string, string]>} longest-first list of [literal, placeholder] */
let SUBSTITUTIONS = [];

function rebuildSubstitutions(pairs) {
	const expanded = [];
	for (const [literal, placeholder] of pairs) {
		if (!literal) continue;
		expanded.push([literal, placeholder]);
		const fwd = literal.split("\\").join("/");
		if (fwd !== literal) expanded.push([fwd, placeholder]);
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

/** Deep-copy a captured value, applying placeholder substitution to every string. */
function normalizeDeep(value) {
	if (typeof value === "string") return normalizeString(value);
	if (value === null || typeof value !== "object") return value;
	if (Array.isArray(value)) return value.map(normalizeDeep);
	const out = {};
	for (const key of Object.keys(value)) out[key] = normalizeDeep(value[key]);
	return out;
}

// --- rg/fd emission-order normalization ------------------------------------
// `rg` and `fd` walk directories in PARALLEL, so the ORDER in which files are
// emitted is not reproducible (verified: `grep "export"` over a 6-file tree
// produced 2 distinct orderings over 12 runs). Pi does not sort, so the order is
// not part of Pi's logic; it is external-binary noise. Records produced by grep
// or find therefore carry `"orderNormalized": true` and are canonicalized:
//   grep -> groups of consecutive rows belonging to the same file are sorted by
//           file path, with each file's own row order PRESERVED (that intra-file
//           ordering, and the `path:N: ` vs `path-N- ` prefixes, ARE contract).
//   find -> plain code-unit line sort (each row is one path).
// Trailing "\n\n[...notice...]" text is never reordered.
const GREP_MATCH_ROW = /^(.*?):(\d+): /;
const GREP_CONTEXT_ROW = /^(.*?)-(\d+)- /;
const grepPathToken = (line) => (GREP_MATCH_ROW.exec(line) ?? GREP_CONTEXT_ROW.exec(line))?.[1] ?? line;

function sortGrepByFile(text) {
	const groups = [];
	for (const line of text.split("\n")) {
		const token = grepPathToken(line);
		const last = groups[groups.length - 1];
		if (last && last.token === token) last.lines.push(line);
		else groups.push({ token, lines: [line] });
	}
	// Array#sort is stable, so equal tokens keep their relative order.
	groups.sort((a, b) => (a.token < b.token ? -1 : a.token > b.token ? 1 : 0));
	return groups.flatMap((g) => g.lines).join("\n");
}

const sortLines = (text) => text.split("\n").sort().join("\n");

const ORDER_NORMALIZERS = { grepByFile: sortGrepByFile, lines: sortLines };

// ===========================================================================
// A. schemas/<tool>.json + strings/<tool>.json
// ===========================================================================
const TOOL_NAMES = ["read", "bash", "edit", "write", "grep", "find", "ls"];

async function genSchemasAndStrings() {
	const tools = await impTool("index.ts");
	// cwd is irrelevant to parameters/strings, but pass a fixed synthetic value
	// so nothing machine-specific can leak into a description.
	const defs = tools.createAllToolDefinitions("/oracle/cwd");
	const summary = [];
	for (const name of TOOL_NAMES) {
		const def = defs[name];
		if (!def) throw new Error(`createAllToolDefinitions() did not return a "${name}" definition`);
		// RAW stringify bytes - do NOT round-trip through a normalizing step.
		const schemaBytes = JSON.stringify(def.parameters);
		emit(`schemas/${name}.json`, schemaBytes + "\n");
		const strings = {
			name: def.name,
			label: def.label,
			description: def.description,
			promptSnippet: def.promptSnippet ?? null,
			promptGuidelines: def.promptGuidelines ?? null,
			executionMode: def.executionMode ?? null,
			hasPrepareArguments: typeof def.prepareArguments === "function",
		};
		emit(`strings/${name}.json`, JSON.stringify(strings, null, 2) + "\n");
		summary.push({
			name,
			schemaBytes: schemaBytes.length,
			hasRequired: Object.hasOwn(def.parameters, "required"),
			hasPrepareArguments: strings.hasPrepareArguments,
		});
	}
	return summary;
}

// ===========================================================================
// B. truncate.cases.jsonl
// ===========================================================================
async function genTruncateCases() {
	const t = await impTool("truncate.ts");
	const records = [];
	const head = (note, input, options) =>
		records.push({
			fn: "truncateHead",
			note,
			input,
			options: options ?? null,
			result: options == null ? t.truncateHead(input) : t.truncateHead(input, options),
		});
	const tail = (note, input, options) =>
		records.push({
			fn: "truncateTail",
			note,
			input,
			options: options ?? null,
			result: options == null ? t.truncateTail(input) : t.truncateTail(input, options),
		});
	const line = (note, input, maxChars) =>
		records.push({
			fn: "truncateLine",
			note,
			input,
			options: maxChars === undefined ? null : { maxChars },
			result: maxChars === undefined ? t.truncateLine(input) : t.truncateLine(input, maxChars),
		});
	const size = (note, input) => records.push({ fn: "formatSize", note, input, options: null, result: t.formatSize(input) });

	// --- constants -----------------------------------------------------------
	records.push({
		fn: "constants",
		note: "module constants",
		input: null,
		options: null,
		result: {
			DEFAULT_MAX_LINES: t.DEFAULT_MAX_LINES,
			DEFAULT_MAX_BYTES: t.DEFAULT_MAX_BYTES,
			GREP_MAX_LINE_LENGTH: t.GREP_MAX_LINE_LENGTH,
		},
	});

	// --- truncateHead --------------------------------------------------------
	head("empty string -> zero lines, zero bytes, not truncated", "", {});
	head("empty string with defaults (no options arg)", "", undefined);
	head("single line, no trailing newline", "only line", {});
	head("single line WITH trailing newline (trailing \\n is not its own line)", "only line\n", {});
	head("three lines, no trailing newline", "a\nb\nc", {});
	head("three lines WITH trailing newline", "a\nb\nc\n", {});
	head("blank line in the middle", "a\n\nc\n", {});
	head("content that is only newlines", "\n\n\n", {});
	head("exactly at maxLines (3 lines, maxLines=3) -> not truncated", "a\nb\nc\n", { maxLines: 3 });
	head("one over maxLines (4 lines, maxLines=3) -> truncatedBy lines", "a\nb\nc\nd\n", { maxLines: 3 });
	head("exactly at maxBytes (3 bytes, maxBytes=3) -> not truncated", "abc", { maxBytes: 3 });
	head("one over maxBytes on a single line -> firstLineExceedsLimit", "abcd", { maxBytes: 3 });
	head("first line exceeds byte limit, more lines follow", "abcdef\nshort\n", { maxBytes: 3 });
	head("byte limit hit mid-file, newline counted for every line after the first", "aa\nbb\ncc\n", { maxBytes: 5 });
	// The per-line newline charge is `byteLength(line) + (i > 0 ? 1 : 0)`
	// (truncate.ts:128). It is observable ONLY when the loop's RUNNING TOTAL
	// decides the cut: `outputBytes` is recomputed from the joined content
	// afterwards, so most byte-limit cases survive charging the newline one line
	// later (`i > 1`). Verified against a local `i > 1` mutant of Pi's own
	// truncateHead over "aa\nbb\ncc\n" x maxBytes 0..14: only 4 and 7 diverge.
	head("newline charge on line 2 decides the cut: 'bb' costs 2+1=3, so 2+3 > 4 and only 'aa' fits (charging the newline from line 3 onwards would wrongly keep 'bb')", "aa\nbb\ncc\n", {
		maxBytes: 4,
	});
	head("maxBytes=6 does NOT pin the newline charge: 'aa\\nbb' fits either way (2+3=5 vs 2+2=4) and 'cc' overflows either way (5+3=8 vs 4+3=7), so the 4- and 7-byte cases above/below are the ones that do", "aa\nbb\ncc\n", {
		maxBytes: 6,
	});
	head("running total decides at line 3: 5+3 > 7 cuts after 'bb' with truncatedBy='bytes'; one byte less of accumulated newline charge would admit 'cc' and leave truncatedBy='lines'", "aa\nbb\ncc\n", {
		maxBytes: 7,
	});
	head(
		"QUIRK: totalBytes(9) > maxBytes(8) so truncated=true, but every line fits in 8 bytes once the trailing newline is dropped, so the loop never sets truncatedBy='bytes' and the initial value 'lines' survives even though maxLines was never reached",
		"aa\nbb\ncc\n",
		{ maxBytes: 8 },
	);
	head("both limits exceeded, lines wins first", "aa\nbb\ncc\ndd\n", { maxLines: 2, maxBytes: 1000 });
	head("both limits exceeded, bytes wins first", "aa\nbb\ncc\ndd\n", { maxLines: 100, maxBytes: 5 });
	head("multi-byte UTF-8: byte counting is UTF-8, not UTF-16", "h\u00e9llo\nw\u00f6rld\n", {});
	head("multi-byte UTF-8 at a byte limit that splits between lines", "h\u00e9llo\nw\u00f6rld\n", { maxBytes: 6 });
	head("CJK line (3 bytes per char) over the byte limit", "\u4f60\u597d\u4e16\u754c\n", { maxBytes: 6 });
	head("CRLF input: \\r stays attached to each line", "a\r\nb\r\nc\r\n", {});
	head("CRLF input over maxLines", "a\r\nb\r\nc\r\nd\r\n", { maxLines: 2 });
	head("maxLines = Number.MAX_SAFE_INTEGER (as ls/grep/find call it)", "aa\nbb\ncc\n", {
		maxLines: Number.MAX_SAFE_INTEGER,
	});
	head("maxLines = Number.MAX_SAFE_INTEGER with a byte limit that truncates", "aa\nbb\ncc\n", {
		maxLines: Number.MAX_SAFE_INTEGER,
		maxBytes: 5,
	});
	head("maxLines = 0", "a\nb\n", { maxLines: 0 });
	head("maxBytes = 0", "a\nb\n", { maxBytes: 0 });

	// --- truncateTail --------------------------------------------------------
	tail("empty string", "", {});
	tail("no truncation needed", "a\nb\nc\n", {});
	tail("single line, no trailing newline", "only line", {});
	tail("one over maxLines -> keeps the LAST lines", "a\nb\nc\nd\n", { maxLines: 3 });
	tail("exactly at maxLines", "a\nb\nc\n", { maxLines: 3 });
	tail("byte limit hit walking backwards", "aa\nbb\ncc\ndd\n", { maxBytes: 5 });
	tail("byte limit hit exactly on a boundary walking backwards", "aa\nbb\ncc\ndd\n", { maxBytes: 8 });
	tail("PARTIAL LAST LINE edge case: final line alone exceeds maxBytes", "ab\ncdefghij", { maxBytes: 4 });
	tail("PARTIAL LAST LINE with trailing newline", "ab\ncdefghij\n", { maxBytes: 4 });
	tail("PARTIAL LAST LINE cut on a multi-byte boundary", "ab\nxx\u00e9\u00e9\u00e9\u00e9", { maxBytes: 5 });
	tail("PARTIAL LAST LINE, single line only", "abcdefghij", { maxBytes: 4 });
	tail("multi-byte UTF-8 tail", "h\u00e9llo\nw\u00f6rld\n", { maxBytes: 7 });
	tail("CRLF input", "a\r\nb\r\nc\r\nd\r\n", { maxLines: 2 });
	tail("both limits exceeded, lines wins", "aa\nbb\ncc\ndd\n", { maxLines: 2, maxBytes: 1000 });
	tail(
		"QUIRK: maxBytes=0 takes the partial-last-line branch, yielding content='' but outputLines=1 and lastLinePartial=true",
		"a\nb\n",
		{ maxBytes: 0 },
	);

	// --- truncateLine --------------------------------------------------------
	line("short line, default 500 char limit", "short line", undefined);
	line("exactly at maxChars", "abcde", 5);
	line("one over maxChars", "abcdef", 5);
	line("maxChars = 0", "abc", 0);
	line("500-char default limit, 501 chars", "z".repeat(501), undefined);
	line("500-char default limit, exactly 500 chars", "z".repeat(500), undefined);
	line("astral plane: 3 thumbs-up is length 6 in UTF-16, 3 code points", "\u{1F44D}\u{1F44D}\u{1F44D}", 2);
	line("astral plane: slice(0,3) SPLITS a surrogate pair (lone high surrogate)", "\u{1F44D}\u{1F44D}\u{1F44D}", 3);
	line("astral plane under the limit by UTF-16 length", "\u{1F44D}\u{1F44D}\u{1F44D}", 6);
	line("combining marks count as separate UTF-16 units", "e\u0301e\u0301e\u0301", 3);
	line("CJK: .length is UTF-16 units, not bytes", "\u4f60\u597d\u4e16\u754c", 2);

	// --- formatSize ----------------------------------------------------------
	for (const n of [0, 1, 512, 1023, 1024, 1025, 1536, 10240, 51200, 51201, 1048575, 1048576, 1572864, 10485760, 104857600]) {
		size(`formatSize(${n})`, n);
	}

	emit("truncate.cases.jsonl", jsonl(records.map(normalizeDeep)));
	return records;
}

// ===========================================================================
// C. edit.diff.corpus.jsonl  (+ edit.prepare.cases.jsonl)
// ===========================================================================
const EDIT_CWD = "/oracle/cwd";

/** In-memory EditOperations so the real execute() runs with zero filesystem I/O. */
function makeMemoryEditOps(files) {
	const store = new Map(Object.entries(files));
	return {
		store,
		ops: {
			async access(absolutePath) {
				if (!store.has(absolutePath)) {
					const err = new Error(`ENOENT: no such file or directory, access '${absolutePath}'`);
					err.code = "ENOENT";
					throw err;
				}
			},
			async readFile(absolutePath) {
				return Buffer.from(store.get(absolutePath), "utf-8");
			},
			async writeFile(absolutePath, content) {
				store.set(absolutePath, content);
			},
		},
	};
}

async function genEditCorpus() {
	const editMod = await impTool("edit.ts");
	const diffMod = await impTool("edit-diff.ts");
	const pathUtils = await impTool("path-utils.ts");

	const numbered = (n, prefix = "line") =>
		Array.from({ length: n }, (_, i) => `${prefix}${i + 1}`).join("\n") + "\n";

	/** @type {Array<{name:string,note:string,path?:string,original:string,edits:Array<{oldText:string,newText:string}>}>} */
	const cases = [
		{
			name: "single-hunk",
			note: "one replacement in the middle of a 12-line file",
			original: numbered(12),
			edits: [{ oldText: "line6", newText: "SIX" }],
		},
		{
			name: "single-hunk-multiline-grow",
			note: "replacement adds more lines than it removes",
			original: numbered(12),
			edits: [{ oldText: "line6\nline7", newText: "A\nB\nC\nD" }],
		},
		{
			name: "single-hunk-multiline-shrink",
			note: "replacement removes lines",
			original: numbered(12),
			edits: [{ oldText: "line5\nline6\nline7\nline8", newText: "COLLAPSED" }],
		},
		{
			name: "two-disjoint-hunks-far-apart",
			note: "unchanged run of 20 lines between hunks: forces leading-4 + ' ... ' + trailing-4 elision",
			original: numbered(30),
			edits: [
				{ oldText: "line3\n", newText: "THREE\n" },
				{ oldText: "line24", newText: "TWENTYFOUR" },
			],
		},
		{
			name: "two-adjacent-hunks-gap-1",
			note: "single unchanged line between hunks: <= 2*contextLines, so every context line is shown",
			original: numbered(12),
			edits: [
				{ oldText: "line5", newText: "FIVE" },
				{ oldText: "line7", newText: "SEVEN" },
			],
		},
		{
			name: "two-hunks-gap-8-exactly-at-2x-context",
			note: "unchanged run of exactly 8 (== 2*contextLines): NO elision marker",
			original: numbered(20),
			edits: [
				{ oldText: "line5", newText: "FIVE" },
				{ oldText: "line14", newText: "FOURTEEN" },
			],
		},
		{
			name: "two-hunks-gap-9-one-over-2x-context",
			note: "unchanged run of exactly 9 (== 2*contextLines + 1): FIRST elision, marker appears",
			original: numbered(20),
			edits: [
				{ oldText: "line5", newText: "FIVE" },
				{ oldText: "line15", newText: "FIFTEEN" },
			],
		},
		{
			name: "three-hunks",
			note: "three disjoint hunks, 15 unchanged lines each side of the middle one: elision on both interior runs",
			original: numbered(40),
			edits: [
				{ oldText: "line4\n", newText: "FOUR\n" },
				{ oldText: "line20", newText: "TWENTY" },
				{ oldText: "line36", newText: "THIRTYSIX" },
			],
		},
		{
			name: "change-at-file-start",
			note: "change on line 1; the only unchanged run has hasLeadingChange only: 4 context rows then ' ... '",
			original: numbered(20),
			edits: [{ oldText: "line1\nline2", newText: "FIRST\nSECOND" }],
		},
		{
			name: "change-at-file-end",
			note: "hasTrailingChange only: ' ... ' then the last 4 context lines",
			original: numbered(20),
			edits: [{ oldText: "line20", newText: "LAST" }],
		},
		{
			name: "change-at-file-end-no-trailing-newline",
			note: "file does not end with a newline; diff lib emits '\\ No newline at end of file' in the patch",
			original: numbered(12).slice(0, -1),
			edits: [{ oldText: "line12", newText: "LAST" }],
		},
		{
			name: "whole-file-replacement",
			note: "oldText is the entire file",
			original: "alpha\nbeta\ngamma\n",
			edits: [{ oldText: "alpha\nbeta\ngamma\n", newText: "one\ntwo\n" }],
		},
		{
			name: "pad-width-1-8-lines",
			note: "line-number padding width: 8 lines + trailing newline -> split() length 9 -> width 1",
			original: numbered(8),
			edits: [{ oldText: "line4", newText: "FOUR" }],
		},
		{
			name: "pad-width-2-9-lines",
			note: "9 lines + trailing newline -> split() length 10 -> width 2 (the 9->10 boundary)",
			original: numbered(9),
			edits: [{ oldText: "line4", newText: "FOUR" }],
		},
		{
			name: "pad-width-2-99-lines",
			note: "99 lines + trailing newline -> split() length 100 -> width 3 (the 99->100 boundary)",
			original: numbered(99),
			edits: [{ oldText: "line50", newText: "FIFTY" }],
		},
		{
			name: "pad-width-3-100-lines",
			note: "100 lines -> width 3; growing the file can widen the column",
			original: numbered(100),
			edits: [{ oldText: "line50", newText: "FIFTY" }],
		},
		{
			name: "pad-width-widens-9-to-10",
			note: "9-line old file, replacement grows it past 10 lines; maxLineNum uses max(old,new)",
			original: numbered(9),
			edits: [{ oldText: "line5", newText: "a\nb\nc\nd\ne" }],
		},
		{
			name: "crlf-input",
			note: "CRLF file: matching happens on LF-normalized content, CRLF is restored on write",
			original: "line1\r\nline2\r\nline3\r\nline4\r\n",
			edits: [{ oldText: "line2", newText: "TWO" }],
		},
		{
			name: "crlf-input-crlf-in-oldtext",
			note: "oldText itself contains CRLF; it is LF-normalized before matching",
			original: "line1\r\nline2\r\nline3\r\nline4\r\n",
			edits: [{ oldText: "line2\r\nline3", newText: "TWO\r\nTHREE" }],
		},
		{
			name: "mixed-line-endings-lf-first",
			note: "detectLineEnding: LF appears before the first CRLF -> ending is LF, file is fully normalized",
			original: "line1\nline2\r\nline3\n",
			edits: [{ oldText: "line2", newText: "TWO" }],
		},
		{
			name: "bare-cr-input",
			note: "lone CR is normalized to LF by normalizeToLF and NOT restored (detectLineEnding sees no \\n)",
			original: "line1\rline2\rline3\n",
			edits: [{ oldText: "line2", newText: "TWO" }],
		},
		{
			name: "bom-input",
			note: "UTF-8 BOM stripped before matching, re-prepended on write",
			original: "\uFEFFline1\nline2\nline3\n",
			edits: [{ oldText: "line2", newText: "TWO" }],
		},
		{
			name: "bom-plus-crlf-input",
			note: "BOM + CRLF: both preserved on write",
			original: "\uFEFFline1\r\nline2\r\nline3\r\n",
			edits: [{ oldText: "line2", newText: "TWO" }],
		},
		{
			name: "bom-in-oldtext-first-line",
			note: "oldText targets the first line; the BOM is invisible to the match",
			original: "\uFEFFline1\nline2\n",
			edits: [{ oldText: "line1", newText: "ONE" }],
		},
		{
			name: "fuzzy-smart-double-quotes",
			note: "FUZZY: file has U+201C/U+201D, oldText has ASCII quotes",
			original: 'const a = \u201Chello\u201D;\nconst b = 2;\nconst c = 3;\n',
			edits: [{ oldText: 'const a = "hello";', newText: 'const a = "HELLO";' }],
		},
		{
			name: "fuzzy-smart-single-quotes",
			note: "FUZZY: file has U+2018/U+2019, oldText has ASCII apostrophes",
			original: "const a = \u2018hi\u2019;\nconst b = 2;\n",
			edits: [{ oldText: "const a = 'hi';", newText: "const a = 'HI';" }],
		},
		{
			name: "fuzzy-nfkc-ligature",
			note: "FUZZY: file has U+FB01 (fi ligature); NFKC folds it to 'fi'",
			original: "read the \uFB01le now\nsecond line\n",
			edits: [{ oldText: "read the file now", newText: "read the FILE now" }],
		},
		{
			name: "fuzzy-em-dash",
			note: "FUZZY: U+2014 em dash folds to ASCII hyphen",
			original: "a \u2014 b\nc\n",
			edits: [{ oldText: "a - b", newText: "a = b" }],
		},
		{
			name: "fuzzy-nbsp",
			note: "FUZZY: U+00A0 folds to a regular space",
			original: "alpha\u00A0beta\ngamma\n",
			edits: [{ oldText: "alpha beta", newText: "ALPHA BETA" }],
		},
		{
			name: "fuzzy-trailing-whitespace",
			note: "FUZZY: file line has trailing spaces, oldText does not",
			original: "line1   \nline2\nline3\n",
			edits: [{ oldText: "line1", newText: "ONE" }],
		},
		{
			name: "fuzzy-trailing-whitespace-elsewhere-preserved",
			note: "FUZZY path runs applyReplacementsPreservingUnchangedLines: trailing whitespace on UNTOUCHED lines survives",
			original: "keep1   \nline2   \nkeep3   \nkeep4\t\n",
			edits: [{ oldText: "line2", newText: "TWO" }],
		},
		{
			name: "fuzzy-one-of-two-edits-switches-whole-call-to-fuzzy",
			note: "edits[0] matches exactly, edits[1] needs fuzzy; usedFuzzyMatch is true for the CALL, so both are re-matched in fuzzy space and applyReplacementsPreservingUnchangedLines runs",
			original: "exact line\nnoise   \n\u201Cfuzzy line\u201D\ntail   \n",
			edits: [
				{ oldText: "exact line", newText: "EXACT LINE" },
				{ oldText: '"fuzzy line"', newText: '"FUZZY LINE"' },
			],
		},
		{
			name: "fuzzy-multiline-oldtext",
			note: "FUZZY over a multi-line oldText spanning several lines",
			original: "head\nalpha   \n\u201Cbeta\u201D\ngamma\ntail\n",
			edits: [{ oldText: 'alpha\n"beta"\ngamma', newText: "REPLACED" }],
		},
		{
			name: "fuzzy-two-groups-merge",
			note: "FUZZY with two replacements landing on adjacent lines; the group-merge branch of applyReplacementsPreservingUnchangedLines",
			original: "a   \nb   \nc   \nd   \ne   \n",
			edits: [
				{ oldText: "b\nc", newText: "BC" },
				{ oldText: "d", newText: "D" },
			],
		},
		{
			name: "err-empty-oldtext-single",
			note: "ERROR wording for exactly ONE edit",
			original: "line1\nline2\n",
			edits: [{ oldText: "", newText: "x" }],
		},
		{
			name: "err-empty-oldtext-multi",
			note: "ERROR wording for MORE THAN ONE edit; index is the array index",
			original: "line1\nline2\n",
			edits: [
				{ oldText: "line1", newText: "ONE" },
				{ oldText: "", newText: "x" },
			],
		},
		{
			name: "err-empty-oldtext-multi-index-0",
			note: "ERROR: the empty oldText is edits[0]",
			original: "line1\nline2\n",
			edits: [
				{ oldText: "", newText: "x" },
				{ oldText: "line1", newText: "ONE" },
			],
		},
		{
			name: "err-not-found-single",
			note: "ERROR wording for exactly ONE edit (no index in the message)",
			original: "line1\nline2\n",
			edits: [{ oldText: "nope", newText: "x" }],
		},
		{
			name: "err-not-found-multi",
			note: "ERROR wording for MORE THAN ONE edit (edits[i] in the message)",
			original: "line1\nline2\n",
			edits: [
				{ oldText: "line1", newText: "ONE" },
				{ oldText: "nope", newText: "x" },
			],
		},
		{
			name: "err-duplicate-single",
			note: "ERROR: 3 occurrences, 1 edit",
			original: "dup\ndup\ndup\nother\n",
			edits: [{ oldText: "dup", newText: "x" }],
		},
		{
			name: "err-duplicate-multi",
			note: "ERROR: 2 occurrences, 2 edits",
			original: "dup\ndup\nunique\n",
			edits: [
				{ oldText: "unique", newText: "UNIQUE" },
				{ oldText: "dup", newText: "x" },
			],
		},
		{
			name: "err-duplicate-counted-in-fuzzy-space",
			note: "ERROR: the two occurrences are only identical AFTER fuzzy normalization (countOccurrences always normalizes)",
			original: "value\u2019s\nvalue's\nother\n",
			edits: [{ oldText: "value's", newText: "VALUE" }],
		},
		{
			name: "err-overlapping-edits",
			note: "ERROR: overlap; the reported indices come from the matchIndex-sorted order",
			original: "abcdefgh\n",
			edits: [
				{ oldText: "abcd", newText: "X" },
				{ oldText: "cdef", newText: "Y" },
			],
		},
		{
			name: "err-overlapping-edits-reversed-input-order",
			note: "ERROR: same overlap, but the later-listed edit matches EARLIER in the file, pinning that the message uses sorted order",
			original: "abcdefgh\n",
			edits: [
				{ oldText: "cdef", newText: "Y" },
				{ oldText: "abcd", newText: "X" },
			],
		},
		{
			name: "err-nested-edits",
			note: "ERROR: one edit fully contains the other",
			original: "abcdefgh\n",
			edits: [
				{ oldText: "abcdef", newText: "X" },
				{ oldText: "cd", newText: "Y" },
			],
		},
		{
			name: "err-noop-single",
			note: "ERROR: replacement produced identical content, 1 edit",
			original: "line1\nline2\n",
			edits: [{ oldText: "line1", newText: "line1" }],
		},
		{
			name: "err-noop-multi",
			note: "ERROR: replacements produced identical content, 2 edits",
			original: "line1\nline2\n",
			edits: [
				{ oldText: "line1", newText: "line1" },
				{ oldText: "line2", newText: "line2" },
			],
		},
		{
			name: "err-empty-edits-array",
			note: "ERROR from validateEditInput before any diffing",
			original: "line1\n",
			edits: [],
		},
		{
			name: "err-file-missing",
			note: "ERROR from ops.access(); message embeds the errno code, not the absolute path",
			original: null, // file is absent from the in-memory store
			edits: [{ oldText: "line1", newText: "ONE" }],
		},
		{
			name: "path-in-patch-header-nested",
			note: "the unified patch header uses the RAW input path verbatim (not the resolved absolute path)",
			path: "sub/dir/file.ts",
			original: numbered(6),
			edits: [{ oldText: "line3", newText: "THREE" }],
		},
		{
			name: "delete-lines-empty-newtext",
			note: "newText is empty: the matched region is deleted",
			original: numbered(10),
			edits: [{ oldText: "line4\nline5\n", newText: "" }],
		},
		{
			name: "insert-only",
			note: "newText is oldText plus extra content: pure insertion",
			original: numbered(10),
			edits: [{ oldText: "line4\n", newText: "line4\nINSERTED\n" }],
		},
		{
			name: "single-line-file-no-newline",
			note: "one-line file with no trailing newline",
			original: "only",
			edits: [{ oldText: "only", newText: "ONLY" }],
		},
		{
			name: "trailing-newline-added",
			note: "the edit adds the file's trailing newline",
			original: "a\nb",
			edits: [{ oldText: "b", newText: "b\n" }],
		},
		{
			name: "utf8-multibyte-content",
			note: "non-ASCII content that must NOT trip the fuzzy path (exact match wins)",
			original: "caf\u00e9\nna\u00efve\n\u4f60\u597d\n",
			edits: [{ oldText: "na\u00efve", newText: "NA\u00cfVE" }],
		},
		{
			name: "tabs-and-indentation",
			note: "leading tabs are significant to the exact match",
			original: "function f() {\n\treturn 1;\n}\n",
			edits: [{ oldText: "\treturn 1;", newText: "\treturn 2;" }],
		},
	];

	const records = [];
	for (const c of cases) {
		const path = c.path ?? "file.txt";
		const absolutePath = pathUtils.resolveToCwd(path, EDIT_CWD);
		const files = c.original === null ? {} : { [absolutePath]: c.original };
		const { store, ops } = makeMemoryEditOps(files);
		const def = editMod.createEditToolDefinition(EDIT_CWD, { operations: ops });

		const record = { name: c.name, note: c.note, path, original: c.original, edits: c.edits };
		try {
			const res = await def.execute("oracle-call", { path, edits: c.edits });
			record.ok = true;
			record.content = res.content;
			record.details = res.details;
			record.writtenContent = store.get(absolutePath);
		} catch (err) {
			record.ok = false;
			record.error = err instanceof Error ? err.message : String(err);
		}

		// Cross-check the low-level seam directly (same inputs, no filesystem at
		// all) so the Rust port can be tested at either level.
		if (c.original !== null && c.edits.length > 0) {
			try {
				const { text } = diffMod.stripBom(c.original);
				const normalized = diffMod.normalizeToLF(text);
				const applied = diffMod.applyEditsToNormalizedContent(normalized, c.edits, path);
				const ds = diffMod.generateDiffString(applied.baseContent, applied.newContent);
				record.lowLevel = {
					detectedLineEnding: diffMod.detectLineEnding(text) === "\r\n" ? "CRLF" : "LF",
					bom: diffMod.stripBom(c.original).bom.length > 0,
					baseContent: applied.baseContent,
					newContent: applied.newContent,
					diff: ds.diff,
					firstChangedLine: ds.firstChangedLine ?? null,
					patch: diffMod.generateUnifiedPatch(path, applied.baseContent, applied.newContent),
				};
			} catch (err) {
				record.lowLevel = { error: err instanceof Error ? err.message : String(err) };
			}
		}
		records.push(record);
	}

	emit("edit.diff.corpus.jsonl", jsonl(records.map(normalizeDeep)));

	// --- prepareArguments ----------------------------------------------------
	const prep = editMod.createEditToolDefinition(EDIT_CWD).prepareArguments;
	const prepCases = [
		{ note: "already-canonical input passes through", input: { path: "a.ts", edits: [{ oldText: "x", newText: "y" }] } },
		{ note: "legacy flat oldText/newText hoisted into edits[]", input: { path: "a.ts", oldText: "x", newText: "y" } },
		{
			note: "legacy flat pair APPENDED after existing edits[]",
			input: { path: "a.ts", edits: [{ oldText: "1", newText: "2" }], oldText: "x", newText: "y" },
		},
		{ note: "edits sent as a JSON string (Opus 4.6 / GLM-5.1 behaviour)", input: { path: "a.ts", edits: '[{"oldText":"x","newText":"y"}]' } },
		{ note: "edits sent as an unparseable string: left as-is", input: { path: "a.ts", edits: "[not json" } },
		{ note: "edits sent as a JSON string that is not an array: left as-is", input: { path: "a.ts", edits: '{"oldText":"x"}' } },
		{ note: "only oldText present (newText missing) -> no hoisting", input: { path: "a.ts", oldText: "x" } },
		{ note: "non-string oldText -> no hoisting", input: { path: "a.ts", oldText: 1, newText: "y" } },
		{ note: "null input passes through", input: null },
		{ note: "string input passes through", input: "nope" },
	];
	const prepRecords = prepCases.map((c) => {
		const input = c.input === null || typeof c.input !== "object" ? c.input : structuredClone(c.input);
		let output;
		let error;
		try {
			output = prep(input);
		} catch (err) {
			error = err instanceof Error ? err.message : String(err);
		}
		return {
			fn: "prepareArguments",
			tool: "edit",
			note: c.note,
			input: c.input,
			ok: error === undefined,
			output: output === undefined ? null : output,
			...(error === undefined ? {} : { error }),
		};
	});
	emit("edit.prepare.cases.jsonl", jsonl(prepRecords.map(normalizeDeep)));

	return { records, prepRecords };
}

// ===========================================================================
// D. exec.corpus.jsonl  (read / write / ls / grep / find over a real temp tree)
// ===========================================================================

/** Declarative description of the fixture tree, emitted as exec.tree.json. */
function buildTreeSpec() {
	const bigLine = (i) => `${String(i).padStart(4, "0")} ${"x".repeat(74)}`;
	return {
		note: "Fixture tree the exec.corpus.jsonl records ran against. Recreate it verbatim (all files UTF-8, LF newlines, written with no BOM) then re-run each record's tool with cwd = the tree root.",
		dirs: ["src", "src/nested", "mixed", "mixed/Sub", "empty", "files", "out"],
		files: {
			"src/a.ts": "export const a = 1;\nconst Foo = 2;\nexport default a;\n",
			"src/b.ts": '// FIXME: fix me\nconst b = "BAR";\nconsole.log(b);\n',
			"src/.hidden.ts": "const hidden = 1;\n",
			"src/nested/c.txt": "hello\nworld\nMATCH here\ntail\n",
			"src/notes.md": "# notes\n\nexport is a keyword.\n",
			"src/long.txt": `prefix ${"L".repeat(520)} needle suffix\nsecond line\n`,
			"src/many.txt": Array.from({ length: 12 }, (_, i) => `hit ${i + 1}`).join("\n") + "\n",
			"mixed/Apple.txt": "a\n",
			"mixed/apple2.txt": "a\n",
			"mixed/banana.txt": "b\n",
			"mixed/Zulu.txt": "z\n",
			"mixed/zebra.txt": "z\n",
			"mixed/\u00c9clair.txt": "e\n",
			"mixed/\u00fcber.txt": "u\n",
			"mixed/.dotfile": "d\n",
			"files/plain.txt": Array.from({ length: 10 }, (_, i) => `plain line ${i + 1}`).join("\n") + "\n",
			"files/utf8.txt": "caf\u00e9\nna\u00efve\n\u4f60\u597d\u4e16\u754c\n\u{1F44D} ok\n",
			// 700 * 80 bytes = 56000 bytes in 700 lines -> byte truncation, not line truncation
			"files/big.txt": Array.from({ length: 700 }, (_, i) => bigLine(i + 1)).join("\n") + "\n",
			// 2500 short lines -> line truncation at DEFAULT_MAX_LINES (2000)
			"files/manylines.txt": Array.from({ length: 2500 }, (_, i) => `L${String(i + 1).padStart(4, "0")}`).join("\n") + "\n",
			// one 60000-byte line -> firstLineExceedsLimit
			"files/huge-line.txt": `${"H".repeat(60000)}\n`,
		},
	};
}

function materializeTree(root, spec) {
	for (const dir of spec.dirs) mkdirSync(join(root, dir), { recursive: true });
	for (const [rel, contents] of Object.entries(spec.files)) {
		const abs = join(root, rel);
		mkdirSync(dirname(abs), { recursive: true });
		writeFileSync(abs, contents, "utf-8");
	}
}

function snapshotDir(root, rel) {
	const abs = join(root, rel);
	if (!existsSync(abs)) return null;
	const out = {};
	const walk = (dir) => {
		for (const entry of readdirSync(dir, { withFileTypes: true }).sort((a, b) => (a.name < b.name ? -1 : 1))) {
			const full = join(dir, entry.name);
			const key = relative(abs, full).split(sep).join("/");
			if (entry.isDirectory()) {
				out[`${key}/`] = null;
				walk(full);
			} else {
				out[key] = readFileSync(full, "utf-8");
			}
		}
	};
	walk(abs);
	return out;
}

async function genExecCorpus(root) {
	const tools = await impTool("index.ts");
	const defs = tools.createAllToolDefinitions(root);
	const records = [];

	const run = async (tool, args, extra = {}) => {
		const orderSensitive = tool === "grep" ? "grepByFile" : tool === "find" ? "lines" : undefined;
		const record = { tool, args, cwd: "{TMPROOT}", ...extra, ...(orderSensitive ? { orderSensitive } : {}) };
		try {
			const res = await defs[tool].execute(`oracle-${tool}-${records.length}`, args);
			record.ok = true;
			record.content = res.content;
			record.details = res.details === undefined ? null : res.details;
		} catch (err) {
			record.ok = false;
			record.error = err instanceof Error ? err.message : String(err);
		}
		records.push(record);
		return record;
	};

	// ---- read ---------------------------------------------------------------
	await run("read", { path: "files/plain.txt" }, { note: "plain 10-line read, no truncation" });
	await run("read", { path: "files/plain.txt", offset: 5 }, { note: "offset only (1-indexed)" });
	await run("read", { path: "files/plain.txt", limit: 3 }, { note: "limit only -> '[N more lines in file...]' notice" });
	await run("read", { path: "files/plain.txt", offset: 3, limit: 2 }, { note: "offset + limit" });
	await run(
		"read",
		{ path: "files/plain.txt", offset: 9, limit: 5 },
		{ note: "limit runs past EOF: no continuation notice (split() yields a trailing empty line)" },
	);
	await run("read", { path: "files/plain.txt", offset: 1, limit: 100 }, { note: "limit exceeds file length" });
	await run("read", { path: "files/plain.txt", offset: 999 }, { note: "ERROR: offset beyond EOF" });
	await run("read", { path: "files/plain.txt", offset: 0 }, { note: "offset 0 is clamped to line 1 by Math.max(0, offset-1)" });
	await run("read", { path: "files/utf8.txt" }, { note: "multi-byte UTF-8 file" });
	await run("read", { path: "files/big.txt" }, { note: "56000 bytes in 700 lines -> BYTE truncation + continuation notice" });
	await run("read", { path: "files/manylines.txt" }, { note: "2500 lines -> LINE truncation at DEFAULT_MAX_LINES + continuation notice" });
	await run("read", { path: "files/huge-line.txt" }, { note: "one 60000-byte line -> firstLineExceedsLimit + sed/head fallback hint" });
	await run("read", { path: "files/nope.txt" }, { note: "ERROR: missing file (ops.access rejects)" });

	// ---- ls -----------------------------------------------------------------
	await run("ls", { path: "src" }, { note: "flat listing: '/' suffix for directories, dotfiles included" });
	await run("ls", { path: "empty" }, { note: "empty directory" });
	await run(
		"ls",
		{ path: "mixed" },
		{
			note: "mixed case + non-ASCII: pins entries.sort((a,b) => a.toLowerCase().localeCompare(b.toLowerCase())) under the host ICU default collation",
		},
	);
	await run("ls", { path: "mixed", limit: 3 }, { note: "entry-limit notice, details.entryLimitReached" });
	// `limit` is `limit ?? DEFAULT_LIMIT` with NO Math.max(1, ...) clamp
	// (ls.ts:125), unlike grep/find. `results.length >= effectiveLimit` is
	// therefore true on the first iteration, so nothing is listed, and the empty
	// `results` takes the "(empty directory)" early return (ls.ts:175-178) -
	// dropping BOTH the entry-limit notice and details.entryLimitReached even
	// though entryLimitReached was set.
	await run(
		"ls",
		{ path: "mixed", limit: 0 },
		{ note: "limit 0 is NOT clamped to 1: loop breaks immediately, so '(empty directory)' and details=undefined, notice and entryLimitReached both lost" },
	);
	await run(
		"ls",
		{ path: "mixed", limit: -1 },
		{ note: "negative limit behaves like limit 0 (same unclamped `>=` break on iteration 1)" },
	);
	await run("ls", {}, { note: "no path -> defaults to '.', i.e. the tree root" });
	await run("ls", { path: "files/plain.txt" }, { note: "ERROR: not a directory" });
	await run("ls", { path: "nope" }, { note: "ERROR: path not found (message embeds the resolved absolute path)" });

	// ---- grep ---------------------------------------------------------------
	await run("grep", { pattern: "export", path: "src" }, { note: "regex match across files" });
	await run("grep", { pattern: "zzz-no-such-pattern", path: "src" }, { note: "no matches" });
	await run("grep", { pattern: "foo", path: "src", ignoreCase: true }, { note: "ignoreCase" });
	await run("grep", { pattern: "foo", path: "src" }, { note: "same pattern WITHOUT ignoreCase" });
	await run("grep", { pattern: "console.log(b)", path: "src", literal: true }, { note: "literal: --fixed-strings" });
	await run("grep", { pattern: "console.log(b)", path: "src" }, { note: "same pattern as a regex ('.' and '(' are metacharacters)" });
	await run("grep", { pattern: "const", path: "src", glob: "*.ts" }, { note: "glob filter" });
	await run("grep", { pattern: "const", path: "src", glob: "*.md" }, { note: "glob filter that excludes every match" });
	await run("grep", { pattern: "MATCH", path: "src", context: 1 }, { note: "context>0: 'path-N- ' context rows vs 'path:N: ' match rows" });
	await run("grep", { pattern: "MATCH", path: "src", context: 10 }, { note: "context larger than the file clamps to its bounds" });
	await run("grep", { pattern: "hit", path: "src/many.txt", limit: 3 }, { note: "match-limit notice + details.matchLimitReached (single file, so rg order is deterministic)" });
	await run("grep", { pattern: "needle", path: "src/long.txt" }, { note: ">500-char line -> '... [truncated]' + details.linesTruncated (path is a FILE, so formatPath uses basename)" });
	await run("grep", { pattern: "needle", path: "src/long.txt", context: 1 }, { note: "same long line via the context/formatBlock path" });
	await run("grep", { pattern: "hidden", path: "src" }, { note: "--hidden: dotfiles are searched" });
	await run("grep", { pattern: "hit", path: "src/nope.txt" }, { note: "ERROR: path not found" });

	// ---- find ---------------------------------------------------------------
	await run("find", { pattern: "*.ts", path: "src" }, { note: "basename glob" });
	await run("find", { pattern: "*.nosuchext", path: "src" }, { note: "no matches" });
	await run(
		"find",
		{ pattern: "nested/*.txt", path: "src" },
		{
			note: "pattern contains '/': Pi adds --full-path and rewrites the pattern to '**/nested/*.txt'. PLATFORM-DEPENDENT OUTCOME: on win32 the candidate path uses '\\\\' separators while fd's globber treats only '/' as a separator, so the literal '/nested/' component never matches and the result is empty. On posix this same call matches nested/c.txt.",
		},
	);
	await run(
		"find",
		{ pattern: "**/*.txt", path: "src" },
		{ note: "pattern already starts with '**/': NOT prefixed again; '**/' can match zero components so this still matches" },
	);
	await run("find", { pattern: "**", path: "src" }, { note: "pattern === '**': --full-path but no prefixing" });
	// limit=1 against a directory holding exactly ONE file: the result SET (not
	// just its order) is then reproducible even though fd applies --max-results
	// to whatever it happens to find first.
	await run(
		"find",
		{ pattern: "*", path: "src/nested", limit: 1 },
		{ note: "result-limit notice + details.resultLimitReached (relativized.length >= effectiveLimit)" },
	);
	await run("find", { pattern: "*.ts", path: "src/nested" }, { note: "no matches in a subdirectory" });
	await run("find", { pattern: ".hidden.ts", path: "src" }, { note: "--hidden: dotfiles are found" });
	await run("find", { pattern: "*", path: "nope" }, { note: "ERROR or empty for a missing directory" });

	// ---- write (last, so nothing above sees these files) --------------------
	const writeCases = [
		{ args: { path: "out/new.txt", content: "hello\nworld\n" }, note: "new file" },
		{ args: { path: "out/new.txt", content: "overwritten\n" }, note: "overwrite an existing file" },
		{ args: { path: "out/deep/a/b/c.txt", content: "nested\n" }, note: "creates missing parent directories" },
		{
			args: { path: "out/utf8.txt", content: "h\u00e9llo w\u00f6rld \u{1F44D}\n" },
			note: "non-ASCII payload: the reported byte count is content.length, i.e. 15 UTF-16 code units, NOT the 19 UTF-8 bytes actually written (see writtenBytes)",
		},
		{ args: { path: "out/empty.txt", content: "" }, note: "empty content" },
		{ args: { path: "out/crlf.txt", content: "a\r\nb\r\n" }, note: "CRLF content is written verbatim (write does no line-ending handling)" },
	];
	for (const wc of writeCases) {
		const rec = await run("write", wc.args, { note: wc.note });
		rec.writtenContent = existsSync(join(root, wc.args.path))
			? readFileSync(join(root, wc.args.path), "utf-8")
			: null;
		rec.writtenBytes = existsSync(join(root, wc.args.path)) ? statSync(join(root, wc.args.path)).size : null;
	}
	const outTree = snapshotDir(root, "out");

	return { records, outTree };
}

// ===========================================================================
// E1. path_utils.cases.jsonl
// ===========================================================================
async function genPathUtilsCases() {
	const pu = await impTool("path-utils.ts");
	const CWD = process.platform === "win32" ? "C:\\oracle\\cwd" : "/oracle/cwd";
	const ABS = process.platform === "win32" ? "C:\\oracle\\abs\\target.txt" : "/oracle/abs/target.txt";
	const FILE_URL = process.platform === "win32" ? "file:///C:/oracle/url/target.txt" : "file:///oracle/url/target.txt";

	const inputs = [
		["plain relative path", "a/b.txt"],
		["relative path with backslashes", "a\\b.txt"],
		["dot-relative path", "./a/b.txt"],
		["parent traversal is collapsed by path.resolve", "a/../b.txt"],
		["leading parent traversal", "../sibling/x.txt"],
		["empty string resolves to the cwd", ""],
		["'.' resolves to the cwd", "."],
		["'..' resolves to the cwd's parent", ".."],
		["absolute path is resolved on its own, ignoring the cwd", ABS],
		["bare '~' expands to the home directory", "~"],
		["'~/...' expands to the home directory", "~/docs/x.txt"],
		["'~\\...' expands on win32 only", "~\\docs\\x.txt"],
		["'~user' is NOT expanded", "~otheruser/x.txt"],
		["'@' prefix is stripped (stripAtPrefix)", "@src/a.ts"],
		["bare '@' becomes the empty string, i.e. the cwd", "@"],
		["'@' plus '~' -> '@' stripped first, then tilde expanded", "@~/x.txt"],
		["U+00A0 NBSP is normalized to a regular space", "dir/a\u00A0b.txt"],
		["U+202F narrow NBSP is normalized to a regular space", "dir/a\u202Fb.txt"],
		["U+3000 ideographic space is normalized to a regular space", "dir/a\u3000b.txt"],
		["U+2000 EN QUAD is normalized to a regular space", "dir/a\u2000b.txt"],
		["a regular space is untouched", "dir/a b.txt"],
		["file:// URL is converted with fileURLToPath", FILE_URL],
		["file:// URL with a percent-escaped space", `${FILE_URL.replace("target.txt", "a%20b.txt")}`],
		["trailing slash", "dir/sub/"],
		["non-ASCII path component", "dir/caf\u00e9/na\u00efve.txt"],
	];

	const records = [];
	for (const [note, input] of inputs) {
		for (const fn of ["resolveToCwd", "expandPath"]) {
			let result;
			let error;
			try {
				result = fn === "resolveToCwd" ? pu.resolveToCwd(input, CWD) : pu.expandPath(input);
			} catch (err) {
				error = err instanceof Error ? err.message : String(err);
			}
			records.push({
				fn,
				note,
				platform: process.platform,
				input,
				...(fn === "resolveToCwd" ? { cwd: CWD } : {}),
				ok: error === undefined,
				result: result === undefined ? null : result,
				...(error === undefined ? {} : { error }),
			});
		}
	}
	emit("path_utils.cases.jsonl", jsonl(records.map(normalizeDeep)));
	return records;
}

// ===========================================================================
// E2. output_accumulator.cases.jsonl
// ===========================================================================
async function genOutputAccumulatorCases() {
	const { OutputAccumulator } = await impTool("output-accumulator.ts");
	const records = [];
	const spilled = new Set();

	const runScenario = async (scenario, options, steps) => {
		const acc = new OutputAccumulator(options);
		let step = 0;
		for (const s of steps) {
			step++;
			let action;
			if (s.append !== undefined) {
				const buf = Buffer.isBuffer(s.append) ? s.append : Buffer.from(s.append, "utf-8");
				acc.append(buf);
				action = { op: "append", bytes: buf.length, hex: buf.toString("hex") };
			} else if (s.finish) {
				acc.finish();
				action = { op: "finish" };
			}
			const snap = acc.snapshot(s.snapshotOptions ?? {});
			if (snap.fullOutputPath) spilled.add(snap.fullOutputPath);
			records.push({
				scenario,
				step,
				note: s.note,
				options,
				action,
				snapshotOptions: s.snapshotOptions ?? {},
				snapshot: {
					content: snap.content,
					truncation: snap.truncation,
					fullOutputPath: snap.fullOutputPath === undefined ? null : snap.fullOutputPath,
				},
				lastLineBytes: acc.getLastLineBytes(),
			});
		}
		await acc.closeTempFile();
	};

	// Scenario 1: never exceeds either limit -> no spill file at all.
	await runScenario("no-truncation", { maxLines: 5, maxBytes: 4096, tempFilePrefix: "pi-oracle" }, [
		{ append: "line1\n", note: "one complete line" },
		{ append: "line2\nline3\n", note: "two more complete lines" },
		{ append: "partial", note: "open line: totalLines counts the incomplete tail" },
		{ finish: true, note: "finish flushes the decoder; still under both limits" },
	]);

	// Scenario 2: crosses the maxLines limit -> temp file opens on append.
	await runScenario("line-limit-spill", { maxLines: 3, maxBytes: 4096, tempFilePrefix: "pi-oracle" }, [
		{ append: "a\nb\nc\n", note: "exactly at maxLines: no spill" },
		{ append: "d\n", note: "one over maxLines: shouldUseTempFile() -> spill file created" },
		{ append: "e\n", note: "further appends go straight to the spill file" },
		{ finish: true, note: "finish" },
	]);

	// Scenario 3: crosses the maxBytes limit; also exercises trimTail
	// (maxRollingBytes = max(maxBytes*2,1) = 80; trimTail fires when
	// tailBytes > maxRollingBytes*2 = 160).
	await runScenario("byte-limit-and-trimtail", { maxLines: 10000, maxBytes: 40, tempFilePrefix: "pi-oracle" }, [
		{ append: "0123456789\n", note: "11 bytes" },
		{ append: "0123456789\n0123456789\n", note: "33 bytes total: still under maxBytes" },
		{ append: "0123456789\n", note: "44 bytes: over maxBytes -> spill + snapshot truncates the tail" },
		{ append: `${"y".repeat(60)}\n`, note: "105 bytes: still under the 160-byte trimTail threshold" },
		{ append: `${"z".repeat(60)}\n`, note: "166 bytes: crosses maxRollingBytes*2 -> trimTail() runs, tailStartsAtLineBoundary is recomputed" },
		{ append: `${"w".repeat(200)}\n`, note: "trimTail again, this time cutting mid-line so getSnapshotText() drops the partial first line" },
		{
			finish: true,
			note: "finish + persistIfTruncated. The last trimTail cut inside the 200-'w' line, so tailStartsAtLineBoundary is false and getSnapshotText() drops everything up to the next newline - which is the end of the tail, hence content=''",
			snapshotOptions: { persistIfTruncated: true },
		},
	]);

	// Scenario 4: multi-byte character split across two appends (streaming decoder).
	const eAcute = Buffer.from("\u00e9", "utf-8"); // 0xC3 0xA9
    await runScenario("split-multibyte", { maxLines: 100, maxBytes: 4096, tempFilePrefix: "pi-oracle" }, [
		{ append: Buffer.concat([Buffer.from("caf", "utf-8"), eAcute.subarray(0, 1)]), note: "first byte of a 2-byte UTF-8 char: the decoder holds it back" },
		{ append: eAcute.subarray(1), note: "second byte completes the char" },
		{ append: Buffer.from("\n", "utf-8"), note: "newline closes the line" },
		{ finish: true, note: "finish" },
	]);

	// Scenario 5: persistIfTruncated forces the spill file from snapshot().
	await runScenario("persist-if-truncated", { maxLines: 2, maxBytes: 4096, tempFilePrefix: "pi-oracle" }, [
		{ append: "a\nb\nc\n", note: "3 lines over maxLines=2; append() already spills", snapshotOptions: { persistIfTruncated: true } },
		{ finish: true, note: "finish" },
	]);

	// Register spill-file placeholders, then normalize.
	rebuildSubstitutions([
		...[...spilled].map((p) => [p, "{TMPFILE}"]),
		[EXEC_ROOT, "{TMPROOT}"],
		[HOME, "{HOME}"],
	]);
	emit("output_accumulator.cases.jsonl", jsonl(records.map(normalizeDeep)));
	for (const p of spilled) rmSync(p, { force: true });
	return records;
}

// ===========================================================================
// main
// ===========================================================================
const EXEC_ROOT = mkdtempSync(join(tmpdir(), "pi-tools-oracle-"));

async function main() {
	rebuildSubstitutions([
		[EXEC_ROOT, "{TMPROOT}"],
		[HOME, "{HOME}"],
	]);

	const warnings = [];

	// -- rg / fd availability (grep/find shell out to the real binaries) ------
	const { ensureTool } = await import(
		pathToFileURL(join(PKGS, "coding-agent", "src", "utils", "tools-manager.ts")).href
	);
	const rgPath = await ensureTool("rg", true);
	const fdPath = await ensureTool("fd", true);
	if (!rgPath) warnings.push("ripgrep (rg) NOT FOUND: every grep record will be an error, not real output.");
	if (!fdPath) warnings.push("fd NOT FOUND: every find record will be an error, not real output.");

	// -- fd's .gitignore behaviour depends on ancestor .git dirs --------------
	let ancestorGit = null;
	for (let cur = EXEC_ROOT; ; ) {
		if (existsSync(join(cur, ".git"))) {
			ancestorGit = cur;
			break;
		}
		const parent = dirname(cur);
		if (parent === cur) break;
		cur = parent;
	}
	if (ancestorGit) {
		warnings.push(
			`An ancestor .git exists at ${ancestorGit}; fd will use git-aware mode instead of --no-require-git, so find records may not be reproducible elsewhere.`,
		);
	}

	const schemas = await genSchemasAndStrings();
	const truncate = await genTruncateCases();
	const edit = await genEditCorpus();

	const treeSpec = buildTreeSpec();
	materializeTree(EXEC_ROOT, treeSpec);
	const exec = await genExecCorpus(EXEC_ROOT);
	const pathUtils = await genPathUtilsCases();
	const accumulator = await genOutputAccumulatorCases();

	// Order-normalize only the records flagged as rg/fd order sensitive.
	const execRecords = exec.records.map((r) => {
		const rec = normalizeDeep(r);
		const normalizer = ORDER_NORMALIZERS[rec.orderSensitive];
		delete rec.orderSensitive;
		if (normalizer && rec.ok && Array.isArray(rec.content)) {
			rec.orderNormalized = true;
			rec.content = rec.content.map((c) => {
				if (c.type !== "text" || typeof c.text !== "string") return c;
				// Notices are appended as "\n\n[...]"; keep them where Pi put them
				// and canonicalize only the result rows above them.
				const idx = c.text.indexOf("\n\n[");
				const body = idx === -1 ? c.text : c.text.slice(0, idx);
				const tail = idx === -1 ? "" : c.text.slice(idx);
				return { ...c, text: normalizer(body) + tail };
			});
		}
		return rec;
	});
	emit("exec.corpus.jsonl", jsonl(execRecords));
	emit(
		"exec.tree.json",
		JSON.stringify(
			normalizeDeep({
				platform: process.platform,
				rgAvailable: Boolean(rgPath),
				fdAvailable: Boolean(fdPath),
				insideGitRepo: ancestorGit !== null,
				...treeSpec,
				outAfterWrites: exec.outTree,
			}),
			null,
			2,
		) + "\n",
	);

	// -- write or check ------------------------------------------------------
	mkdirSync(OUT, { recursive: true });
	mkdirSync(join(OUT, "schemas"), { recursive: true });
	mkdirSync(join(OUT, "strings"), { recursive: true });

	let drift = 0;
	for (const [rel, contents] of [...artifacts].sort((a, b) => (a[0] < b[0] ? -1 : 1))) {
		const dest = join(OUT, rel);
		const existing = existsSync(dest) ? readFileSync(dest, "utf-8") : null;
		if (existing === contents) {
			console.log(`ok       ${rel} (${Buffer.byteLength(contents)} bytes)`);
			continue;
		}
		if (CHECK) {
			drift++;
			if (existing === null) {
				console.error(`DRIFT    ${rel}: missing`);
			} else {
				const a = existing.split("\n");
				const b = contents.split("\n");
				let firstDiff = 0;
				while (firstDiff < a.length && firstDiff < b.length && a[firstDiff] === b[firstDiff]) firstDiff++;
				console.error(
					`DRIFT    ${rel}: ${existing.length} -> ${contents.length} bytes, first differing line ${firstDiff + 1}`,
				);
				console.error(`  committed: ${JSON.stringify((a[firstDiff] ?? "").slice(0, 200))}`);
				console.error(`  derived  : ${JSON.stringify((b[firstDiff] ?? "").slice(0, 200))}`);
			}
			continue;
		}
		mkdirSync(dirname(dest), { recursive: true });
		writeFileSync(dest, contents, "utf-8");
		console.log(`wrote    ${rel} (${Buffer.byteLength(contents)} bytes)`);
	}

	if (CHECK && drift > 0) {
		console.error(`\nDRIFT: ${drift} tools fixture(s) are stale; run node scripts/gen-tools-oracle.mjs`);
		process.exitCode = 1;
	}

	console.log("\n=== SUMMARY ===");
	console.log(`schemas/strings : ${schemas.length} tools ${JSON.stringify(schemas)}`);
	console.log(`truncate cases  : ${truncate.length}`);
	console.log(`edit diff cases : ${edit.records.length} (ok=${edit.records.filter((r) => r.ok).length}, err=${edit.records.filter((r) => !r.ok).length})`);
	console.log(`edit prepare    : ${edit.prepRecords.length}`);
	console.log(`exec cases      : ${execRecords.length} (ok=${execRecords.filter((r) => r.ok).length}, err=${execRecords.filter((r) => !r.ok).length})`);
	console.log(`path_utils      : ${pathUtils.length}`);
	console.log(`accumulator     : ${accumulator.length}`);
	for (const w of warnings) console.log(`WARNING: ${w}`);
}

try {
	await main();
} finally {
	rmSync(EXEC_ROOT, { recursive: true, force: true });
}
