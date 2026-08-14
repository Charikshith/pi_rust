#!/usr/bin/env node
// gen-cli-oracle.mjs
//
// GOLDEN ORACLE for feat-005 (pirust coding-agent CLI surface). Every byte
// written by this script is produced by EXECUTING Pi's own TypeScript source in
// ../pi/packages/coding-agent/src/ under Node's native type stripping. Nothing
// here is a reimplementation or a hand-authored expectation.
//
// Run:      cd pirust && node scripts/gen-cli-oracle.mjs
// Verify:   cd pirust && node scripts/gen-cli-oracle.mjs --check
//
// ---------------------------------------------------------------------------
// OUTPUTS  (tests/fixtures/pi/cli/)
// ---------------------------------------------------------------------------
//   args.corpus.jsonl          real `parseArgs(argv)` for every documented flag,
//                              every alias, and every confirmed trap.
//   help.plain.golden          `printHelp()` stdout, colour forced OFF.
//   help.color.golden          `printHelp()` stdout, colour forced ON.
//   help.plain.ext.golden      `printHelp(extensionFlags)` stdout, colour OFF.
//   help.color.ext.golden      `printHelp(extensionFlags)` stdout, colour ON.
//   help.ext-flags.json        the synthetic ExtensionFlag[] the *.ext.golden
//                              files were rendered from.
//   help.identity.json         APP_NAME / APP_TITLE / CONFIG_DIR_NAME /
//                              ENV_AGENT_DIR / ENV_SESSION_DIR / VERSION /
//                              PACKAGE_NAME in effect when the goldens were
//                              captured, so a Rust test can substitute Pi's
//                              identity back into pirust's template.
//   app_mode.cases.jsonl       real `resolveAppMode` / `toPrintOutputMode` /
//                              `isPlainRuntimeMetadataCommand`.
//   config_paths.json          every path accessor in config.ts, under BOTH the
//                              PI_CODING_AGENT_DIR branch and the
//                              homedir()/.pi/agent fallback, plus tilde cases.
//   settings.merge.cases.jsonl real `deepMergeSettings` + `migrateSettings` +
//                              the malformed-settings diagnostic text.
//   migrations.cases.jsonl     all 5 migrations + `runMigrations`, each against
//                              a real temp agent dir (before/after trees).
//   session_dir.cases.jsonl    cwd -> session-directory-name encoding, observed
//                              on a real filesystem.
//   session_migration.cases.jsonl
//                              the v1 -> v2 -> v3 session migration, run over
//                              trimmed slices of Pi's OWN 2.3 MB / 951 KB test
//                              fixtures plus the edge cases those files do not
//                              contain.
//   auth.json.cases.jsonl      real AuthStorage read/write/merge round-trips.
//   models.cases.jsonl         the model-resolution chain: parseModelPattern,
//                              findExactModelReferenceMatch, resolveCliModel,
//                              findInitialModel, resolveModelScopeWithDiagnostics,
//                              ModelConfig.load (models.json schema + errors),
//                              real ModelRuntime provider composition, and
//                              --list-models' exact table.
//
// ---------------------------------------------------------------------------
// MODULE RESOLUTION
// ---------------------------------------------------------------------------
// A `register()`ed resolve hook maps the bare workspace specifiers
// `@earendil-works/pi-ai`, `@earendil-works/pi-ai/<sub>`,
// `@earendil-works/pi-agent-core`, `@earendil-works/pi-agent-core/<sub>` and
// `@earendil-works/pi-tui` to the corresponding pi/packages/*/src/*.ts (the
// published `dist/` is not built). Third-party deps (`chalk`,
// `proper-lockfile`, ...) resolve naturally out of pi/node_modules because the
// importing modules live inside the pi tree.
//
// A `load` hook APPENDS a single `export { ... as __name }` statement to four
// of Pi's modules so that module-private functions can be driven directly
// instead of being reimplemented. Pi's own bytes are executed verbatim; only an
// export list is appended, and NOTHING is written into the pi checkout:
//   main.ts                 -> resolveAppMode, toPrintOutputMode,
//                              isPlainRuntimeMetadataCommand,
//                              collectSettingsDiagnostics, isTruthyEnvFlag
//   core/settings-manager.ts-> deepMergeSettings
//   core/session-manager.ts -> getDefaultSessionDirPath
//   migrations.ts           -> migrateToolsToBin, migrateKeybindingsConfigFile,
//                              migrateExtensionSystem, migrateCommandsToPrompts,
//                              checkDeprecatedExtensionDirs
// `SettingsManager.migrateSettings` is a TypeScript `private static`, i.e. a
// plain static method at runtime, and is called directly.
// All modules load cleanly under type stripping - no TS enums, decorators,
// namespaces or other non-erasable syntax.
//
// ---------------------------------------------------------------------------
// THE REAL ~/.pi IS NEVER TOUCHED
// ---------------------------------------------------------------------------
// Everything that reads or writes agent state runs with PI_CODING_AGENT_DIR
// pointed at a mkdtemp'd directory, and `assertTemp()` hard-fails if a path
// about to be used is not under os.tmpdir(). The one capture that exercises the
// `homedir()/.pi/agent` fallback (config_paths.json, branch "unset") only calls
// pure path-computing accessors - config.ts's getAgentDir/getAuthPath/... do no
// filesystem I/O at all - and its output is placeholder-substituted.
// NOTHING is written inside the pi repo: `git -C ../pi status --short` stays
// clean.
//
// ---------------------------------------------------------------------------
// DETERMINISM / NORMALIZATION  (what was replaced and why)
// ---------------------------------------------------------------------------
// Captured values are byte-verbatim EXCEPT for these placeholder substitutions,
// applied by a deep string walk over the captured JSON (longest literal first,
// in both native-separator and forward-slash form):
//
//   "{AGENTDIR}"  <- the per-case mkdtemp'd agent directory. Random suffix +
//                    user name + OS temp location; the Rust port substitutes
//                    its own. Every path INSIDE it stays relative and verbatim.
//   "{PROJECTDIR}"<- the per-case mkdtemp'd project cwd (same reason).
//   "{TMPROOT}"   <- the mkdtemp'd root that holds both of the above.
//   "{HOME}"      <- os.homedir(), for the config_paths "unset" branch and the
//                    expandTildePath rows.
//   "{PIPKG}"     <- getPackageDir(), i.e. the pi checkout's coding-agent
//                    package root. Only appears in the package-asset accessors
//                    (getThemesDir, getReadmePath, ...), which are properties
//                    of where pi is installed, not of Pi's logic.
//
// Also normalized / explicitly flagged:
//   * `platform` is recorded on every record whose result is platform
//     dependent. Four surfaces genuinely are, and the raw host value is kept
//     because it IS the contract on this platform:
//       - config.ts joins with node:path.join, so separators/drive letters are
//         native. Recorded raw, with `platform`.
//       - migrateSessionsFromAgentRoot's `file.split("/").pop() ||
//         file.split("\\").pop()` makes the whole migration a NO-OP on win32
//         (the first split always yields a truthy last element, so the
//         backslash fallback is dead and `fileName` is the full absolute path).
//         Captured faithfully; each record carries
//         `platformDependent: "m2-filename-split"`.
//       - file modes: win32 cannot express 0600, so `mode` on win32 is the
//         host's synthetic value. Each auth/migration record carries
//         `modeMeaningful: false` on win32 so a Rust test can skip it there.
//       - normalizePath's `~\x` branch is gated on process.platform === "win32".
//       - getDefaultSessionDirPath calls resolvePath(cwd) first, and on win32
//         path.resolve() of a ROOTED-BUT-DRIVELESS input ("/home/user/project")
//         adopts the DRIVE LETTER OF process.cwd(). That drive therefore ends up
//         baked into the encoded directory name ("--C--home-user-project--"),
//         which makes the raw `result` capture-machine dependent. Each such
//         record now states it explicitly: `resolveCwd` (the cwd it resolved
//         against), `resolvedCwd`, `inputHadDrive`, `cwdDriveInjected` (null when
//         no drive was injected - POSIX hosts, drive-qualified inputs, UNC
//         paths), and `resultTemplate`, which is `result` with that injected
//         drive letter replaced by the literal placeholder "{CWDDRIVE}" so a test
//         can substitute its own. The raw `result` is kept alongside it.
//   * JSON.parse failure messages are recorded VERBATIM but flagged
//     `v8Dependent: true` - the wording comes from V8, not from Pi.
//   * proper-lockfile creates a transient `<file>.lock` directory; tree
//     snapshots filter `*.lock` entries (they never survive a released lock).
//   * Tree snapshots are a flat list sorted by path string, directories
//     suffixed "/", file contents read as UTF-8. Sorting is ours (readdir order
//     is not a Pi contract); every other byte is verbatim.
//   * `unknownFlags` is a JS `Map`, so it is serialized as an ORDERED array of
//     [key, value] pairs - insertion order is observable and is contract.
//     `parseArgs`'s result keys are emitted in their real insertion order too
//     (messages, fileArgs, unknownFlags, diagnostics first - they come from the
//     object literal - then each flag in the order the loop assigned it).
//   * In settings.merge.cases.jsonl, a JS `undefined` VALUE is encoded as
//     `{"$undefined":true}` (JSON has no undefined) so that "key present with
//     value undefined" stays distinguishable from "key absent". Nothing else is
//     re-encoded.
//   * NOTHING ELSE. No timestamps, PIDs, UUIDs or random ids appear in any
//     fixture; every diagnostic and console string is captured verbatim.
//
// ---------------------------------------------------------------------------
// session_migration.cases.jsonl: EXTRA NORMALIZATIONS AND WHY
// ---------------------------------------------------------------------------
//   * TRIMMED INPUTS, NOT VENDORED ONES. Two slices come from Pi's own test
//     fixtures (test/fixtures/before-compaction.jsonl, 2 370 492 bytes / 1003
//     entries, and test/fixtures/large-session.jsonl, 974 011 bytes / 1019
//     entries; both v1). Each is trimmed to the SMALLEST entry of every distinct
//     (type, message.role) it contains - smallest by JSON.stringify length, ties
//     broken by the lower original index - kept in original order. That rule is
//     re-derived on every run, so the slice cannot drift silently; the chosen
//     indices are recorded in `sliceIndices` and the rule in `sliceRule`. Slice
//     lines are byte-exact copies of the source lines except for the one rewrite
//     below.
//   * THE ONE REWRITE. `firstKeptEntryIndex` indexes the WHOLE entries array, so
//     trimming invalidates it. Each kept compaction's value is retargeted to a
//     slice-local position and the change is recorded in
//     `firstKeptEntryIndexRewrites: [{originalIndex, newIndex, targetsSliceEntry}]`.
//     Nothing else inside a real line is altered - not the summary text, not the
//     timestamps, not the key order.
//   * GENERATED IDS ARE PLACEHOLDERS. migrateV1ToV2 assigns
//     `entry.id = randomUUID().slice(0, 8)`, so ids, parentIds and resolved
//     firstKeptEntryIds are random per run. Byte-pinning random bytes is both
//     impossible and useless (a Rust port generates its own), so each NEWLY
//     GENERATED id is replaced by "{ID:<n>}" where n is the id's ASSIGNMENT
//     ORDINAL - the entry's position among the non-session entries, i.e. exactly
//     the order migrateV1ToV2 walks. Substitution happens on the object graph by
//     exact string equality, never by substring search, so unrelated content
//     cannot be corrupted. Ids that were ALREADY in the input are left untouched,
//     which is why the v2->v3 and already-v3 records contain no placeholders at
//     all and carry `byteIdentical` computed from the real lines.
//     `preExistingIds` and `idAssignment` make the split explicit.
//   * ORACLE-AUTHORED INPUTS ARE LABELLED. Neither real fixture contains a
//     `hookMessage` role, a v2 header, or the guarded firstKeptEntryIndex shapes,
//     so four records use authored inputs (`source: "oracle-authored ..."`),
//     exactly as args.corpus.jsonl's argv arrays are authored. The migration that
//     produces every `after` is still Pi's own migrateToCurrentVersion.
//   * KEY ORDER IS THE POINT, so it is captured redundantly:
//     `headerKeyOrderBefore`/`After` and `compactionKeyOrderBefore`/`After` sit
//     next to the raw lines. `entry.version = N` APPENDS `version` when the key
//     is absent (v1 header -> version lands LAST, holding 3, never 2) but KEEPS
//     ITS POSITION when re-assigned (v2 header -> version stays where it was);
//     `id`, `parentId` and `firstKeptEntryId` are appended in that order while
//     `firstKeptEntryIndex` is deleted from the middle.
//   * No V8- or locale-dependent value appears in this fixture: the migration
//     does no parsing that can fail, no sorting and no collation.
//
// ---------------------------------------------------------------------------
// models.cases.jsonl: EXTRA NORMALIZATIONS AND WHY
// ---------------------------------------------------------------------------
//   * `catalogSource` labels every record's model source:
//       "synthetic" - the model list in the `syntheticCatalog` record. That list
//                     is oracle-authored INPUT (exactly as the argv arrays in
//                     args.corpus.jsonl are), because the branches under test
//                     (cross-provider ambiguity, alias-vs-dated tie-breaks,
//                     provider-name-vs-slash-in-id conflicts, an empty provider)
//                     cannot be constructed from the real catalog. Every
//                     `result` is still produced by executing Pi's own function.
//                     The list carries only the fields the code reads or copies.
//       "builtin"   - the real generated catalog via a real ModelRuntime.
//       "n/a"       - ModelConfig.load, which has no catalog at all.
//   * THE BUILTIN CATALOG IS NEVER DUMPED. It holds 1000+ generated models. Each
//     "builtin" record keeps only: totals (provider count, model count,
//     available count, configured provider ids) plus the FULL composed model
//     objects for the providers that record is about - the provider ids named in
//     its models.json, plus `anthropic` (the one provider pirust ports). Model
//     fields kept: provider, id, name, api, baseUrl, reasoning, input,
//     contextWindow, maxTokens, cost, thinkingLevelMap, headers, compat - i.e.
//     everything composeModelProvider/modelFromJson produce and everything
//     resolveCliModel and listModels read. Nothing else exists on these objects.
//   * The single `builtinCatalogFingerprint` record and its `piVersion` are a
//     DRIFT SIGNAL, not a contract: the catalog is generated and grows with
//     every pi release.
//   * ENV ALLOWLIST. ModelRuntime reads provider auth from process.env, so every
//     "builtin" record is captured in a CHILD PROCESS whose environment is an
//     explicit allowlist (MODELS_CHILD_ENV_KEYS, echoed into the
//     `captureEnvironment` record) plus NO_COLOR=1, PI_OFFLINE=1 and a temp
//     PI_CODING_AGENT_DIR. No host ANTHROPIC_API_KEY / AWS_PROFILE /
//     ANTHROPIC_BASE_URL can reach the capture, which is what makes provider
//     availability reproducible. (This machine had ANTHROPIC_BASE_URL and
//     ANTHROPIC_CUSTOM_HEADERS set; both are excluded by the allowlist.)
//   * OFFLINE. PI_OFFLINE=1 makes ModelRuntime.create resolve
//     `allowModelNetwork = process.env.PI_OFFLINE === undefined` to false, so the
//     remote-catalog refresh never runs. Every such record carries
//     `offline: true`. No record required network access.
//   * `localeCompareDependent: true` marks records whose result depends on
//     tryMatchModel's `sort((a, b) => b.id.localeCompare(a.id))`, i.e. on the
//     host's ICU collation, and records that rely on Array#sort STABILITY for
//     equal ids. For the ASCII ids used here a descending byte sort agrees, but
//     the flag says where a Rust port must look if it disagrees.
//   * `stubRuntime` supplies the two members resolveCliModel calls (getModels,
//     hasConfiguredAuth) and the three findInitialModel calls (getModel,
//     hasConfiguredAuth, getAvailable). Nothing else is reachable from those
//     functions; Pi's functions run unmodified.
//   * findInitialModel calls `process.exit(1)` on an unresolvable --provider
//     /--model pair. process.exit is NODE's, not Pi's, so it is temporarily
//     replaced with a throw; the record keeps the real `exitCode` and the real
//     stderr line. Nothing about Pi's control flow is altered.
//   * models.json error strings echo the file's absolute path, so {TMPROOT}
//     appears inside them. `Failed to parse models.json: ...` embeds V8's
//     JSON.parse wording; those records carry `v8Dependent: true`.
//
// ---------------------------------------------------------------------------
// PURITY OF parseArgs (verified, not assumed)
// ---------------------------------------------------------------------------
// args.ts imports only `chalk` (used by printHelp), four string constants from
// config.ts, and two types. `parseArgs`'s body contains no reference to
// `process`, `env`, `fs`, `cwd`, `isTTY`, `Date` or `Math.random`; this script
// asserts that by scanning the real source text, and additionally re-runs every
// case a second time with a mutated process.env / process.chdir'd cwd and
// asserts byte-identical output. See assertParseArgsPure().

import {
	chmodSync,
	existsSync,
	mkdirSync,
	mkdtempSync,
	readdirSync,
	readFileSync,
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
const CA = join(PKGS, "coding-agent", "src");
const OUT = join(pirustRoot, "tests", "fixtures", "pi", "cli");

if (!existsSync(join(CA, "cli", "args.ts"))) {
	console.error(`Pi CLI sources not found at ${CA}; fixtures cannot be regenerated.`);
	process.exit(CHECK ? 0 : 1); // don't fail --check when the source repo is simply absent
}

// `--emit-help <variant>` re-invokes this same file as a CHILD process whose
// only job is to print printHelp() to stdout. chalk decides its colour level
// ONCE, at import time, from the environment, so the plain and colour goldens
// cannot be produced by the same process. The parent controls the child's env
// (see genHelpGoldens) and the child must not touch NO_COLOR/FORCE_COLOR.
const helpArgIndex = process.argv.indexOf("--emit-help");
const HELP_VARIANT = helpArgIndex === -1 ? null : process.argv[helpArgIndex + 1];

if (HELP_VARIANT === null) {
	// Colour OFF for everything captured in THIS process (migration console
	// output, deprecation warnings).
	process.env.NO_COLOR = "1";
	process.env.PI_OFFLINE = "1"; // no network, ever
}

// ---------------------------------------------------------------------------
// Hooks: bare-specifier aliases + private-export appending
// ---------------------------------------------------------------------------
const PKG_ROOTS = {
	"@earendil-works/pi-ai": join(PKGS, "ai", "src"),
	"@earendil-works/pi-agent-core": join(PKGS, "agent", "src"),
	"@earendil-works/pi-tui": join(PKGS, "tui", "src"),
};

const APPENDED_EXPORTS = {
	"main.ts": [
		"resolveAppMode",
		"toPrintOutputMode",
		"isPlainRuntimeMetadataCommand",
		"collectSettingsDiagnostics",
		"isTruthyEnvFlag",
	],
	"core/settings-manager.ts": ["deepMergeSettings"],
	"core/session-manager.ts": ["getDefaultSessionDirPath", "migrateToCurrentVersion", "migrateV1ToV2", "migrateV2ToV3"],
	"migrations.ts": [
		"migrateToolsToBin",
		"migrateKeybindingsConfigFile",
		"migrateExtensionSystem",
		"migrateCommandsToPrompts",
		"checkDeprecatedExtensionDirs",
	],
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

/**
 * Synthetic ExtensionFlag[] for the `printHelp(extensionFlags)` branch. Chosen
 * to exercise every sub-branch of that formatter:
 *   - type "boolean" (no " <value>" suffix) and type "string" (with it)
 *   - a flag WITH a description and flags WITHOUT one (which fall back to
 *     `Registered by ${flag.extensionPath}`)
 *   - a name short enough for `padEnd(30)` to pad, and one long enough that
 *     padEnd is a no-op and the description butts straight up against it.
 */
const EXT_FLAGS = [
	{ name: "plan", type: "boolean", description: "Enable plan mode", extensionPath: "/ext/plan-mode.ts" },
	{ name: "plan-model", type: "string", description: "Model used for planning", extensionPath: "/ext/plan-mode.ts" },
	{ name: "x", type: "boolean", extensionPath: "/ext/x.ts" },
	{ name: "a-very-long-extension-flag-name", type: "string", extensionPath: "/ext/long.ts" },
];

// ---------------------------------------------------------------------------
// Output collection (write, or diff in --check mode)
// ---------------------------------------------------------------------------
/** @type {Map<string, string>} relative path -> exact file contents */
const artifacts = new Map();
const emit = (relPath, contents) => artifacts.set(relPath.split("\\").join("/"), contents);
const jsonl = (records) => records.map((r) => JSON.stringify(r)).join("\n") + "\n";
const pretty = (value) => JSON.stringify(value, null, 2) + "\n";

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

const PLATFORM = process.platform;
const MODE_MEANINGFUL = PLATFORM !== "win32";

// ---------------------------------------------------------------------------
// Temp-dir safety
// ---------------------------------------------------------------------------
let TMPROOT = "";
const REAL_TMP = tmpdir();

/** Hard-fail if `p` is not under os.tmpdir(). Guards the real ~/.pi. */
function assertTemp(p) {
	const lower = (s) => (PLATFORM === "win32" ? s.toLowerCase() : s);
	if (!lower(p).startsWith(lower(REAL_TMP))) {
		throw new Error(`refusing to use non-temp path: ${p}`);
	}
	return p;
}

let caseCounter = 0;
/** Fresh, empty, temp-verified agent dir for one capture. */
function newAgentDir(label) {
	const dir = join(TMPROOT, `${String(++caseCounter).padStart(3, "0")}-${label}`, "agent");
	assertTemp(dir);
	mkdirSync(dir, { recursive: true });
	return dir;
}
function newProjectDir(label) {
	const dir = join(TMPROOT, `${String(caseCounter).padStart(3, "0")}-${label}`, "project");
	assertTemp(dir);
	mkdirSync(dir, { recursive: true });
	return dir;
}

/** Run `fn` with PI_CODING_AGENT_DIR pointed at a temp dir, then restore. */
function withAgentDir(agentDir, fn) {
	assertTemp(agentDir);
	const prev = process.env.PI_CODING_AGENT_DIR;
	process.env.PI_CODING_AGENT_DIR = agentDir;
	try {
		return fn();
	} finally {
		if (prev === undefined) delete process.env.PI_CODING_AGENT_DIR;
		else process.env.PI_CODING_AGENT_DIR = prev;
	}
}

// ---------------------------------------------------------------------------
// Tree snapshots
// ---------------------------------------------------------------------------
const octal = (p) => `0${(statSync(p).mode & 0o777).toString(8)}`;

/**
 * Flat, path-sorted snapshot of a directory: directories as "<rel>/", files as
 * {path, mode, content}. `*.lock` entries (proper-lockfile transients) are
 * filtered. Returns null when the directory does not exist.
 */
function snapshotTree(root) {
	if (!existsSync(root)) return null;
	const out = [];
	const walk = (dir) => {
		for (const entry of readdirSync(dir, { withFileTypes: true })) {
			if (entry.name.endsWith(".lock")) continue;
			const full = join(dir, entry.name);
			const rel = relative(root, full).split(sep).join("/");
			if (entry.isDirectory()) {
				out.push({ path: `${rel}/` });
				walk(full);
			} else {
				let content;
				try {
					content = readFileSync(full, "utf-8");
				} catch (err) {
					content = `<<unreadable: ${err instanceof Error ? err.message : String(err)}>>`;
				}
				out.push({ path: rel, mode: octal(full), content });
			}
		}
	};
	walk(root);
	out.sort((a, b) => (a.path < b.path ? -1 : a.path > b.path ? 1 : 0));
	return out;
}

/** Write a file, creating parents. Contents are written with explicit "\n". */
function put(root, rel, contents) {
	const abs = join(root, ...rel.split("/"));
	mkdirSync(dirname(abs), { recursive: true });
	writeFileSync(abs, contents, "utf-8");
	return abs;
}

/** Capture everything console.log emits while `fn` runs. */
function captureConsole(fn) {
	const lines = [];
	const origLog = console.log;
	const origErr = console.error;
	console.log = (...args) => lines.push({ stream: "stdout", text: args.join(" ") });
	console.error = (...args) => lines.push({ stream: "stderr", text: args.join(" ") });
	try {
		const result = fn();
		return { result, console: lines };
	} finally {
		console.log = origLog;
		console.error = origErr;
	}
}

// ===========================================================================
// A. args.corpus.jsonl
// ===========================================================================

/**
 * Serialize a real `Args` object. Key order is the object's own insertion
 * order (contract: it shows which branch fired first). `unknownFlags` is a Map
 * -> ordered [key, value] pairs. Nothing else is transformed.
 */
function serializeArgs(parsed) {
	const out = {};
	for (const key of Object.keys(parsed)) {
		out[key] = key === "unknownFlags" ? [...parsed.unknownFlags.entries()] : parsed[key];
	}
	return out;
}

/** Static + dynamic proof that parseArgs is pure. Throws on failure. */
function assertParseArgsPure(parseArgs, cases) {
	const src = readFileSync(join(CA, "cli", "args.ts"), "utf-8");
	const body = src.slice(src.indexOf("export function parseArgs"), src.indexOf("export function printHelp"));
	const forbidden = ["process.", "process.env", "readFile", "existsSync", "cwd(", "isTTY", "Date.", "Math.random", "require("];
	const found = forbidden.filter((needle) => body.includes(needle));
	if (found.length > 0) {
		throw new Error(`parseArgs body is not pure; found ${JSON.stringify(found)}`);
	}
	// Dynamic: same argv under a mutated env and a different cwd must produce
	// byte-identical output.
	const before = cases.map((c) => JSON.stringify(serializeArgs(parseArgs(c.argv))));
	const prevCwd = process.cwd();
	process.env.PI_ORACLE_PURITY_PROBE = "mutated";
	process.chdir(tmpdir());
	let after;
	try {
		after = cases.map((c) => JSON.stringify(serializeArgs(parseArgs(c.argv))));
	} finally {
		process.chdir(prevCwd);
		delete process.env.PI_ORACLE_PURITY_PROBE;
	}
	for (let i = 0; i < before.length; i++) {
		if (before[i] !== after[i]) throw new Error(`parseArgs is env/cwd dependent for case ${cases[i].name}`);
	}
	return { staticScan: "clean", dynamicReruns: cases.length };
}

function argsCases() {
	/** @type {Array<{name:string,note:string,argv:string[]}>} */
	const cases = [];
	const c = (name, note, argv) => cases.push({ name, note, argv });

	// ---- baseline ----------------------------------------------------------
	c("empty-argv", "no arguments at all: only the four literal-initialised keys exist", []);
	c("bare-message", "a non-flag token becomes a message", ["hello world"]);
	c("two-messages", "multiple non-flag tokens accumulate in order", ["first", "second"]);

	// ---- every documented flag, long form then alias ------------------------
	c("help-long", "--help", ["--help"]);
	c("help-short", "-h", ["-h"]);
	c("version-long", "--version", ["--version"]);
	c("version-short", "-v", ["-v"]);
	c("mode-text", "--mode text", ["--mode", "text"]);
	c("mode-json", "--mode json", ["--mode", "json"]);
	c("mode-rpc", "--mode rpc", ["--mode", "rpc"]);
	c("mode-invalid-silently-ignored", "--mode bogus: value IS consumed, mode stays unset, NO diagnostic", ["--mode", "bogus"]);
	c("continue-long", "--continue", ["--continue"]);
	c("continue-short", "-c", ["-c"]);
	c("resume-long", "--resume", ["--resume"]);
	c("resume-short", "-r", ["-r"]);
	c("provider", "--provider <name>", ["--provider", "openai"]);
	c("model", "--model <pattern>", ["--model", "sonnet"]);
	c("api-key", "--api-key <key>", ["--api-key", "sk-oracle-123"]);
	c("system-prompt", "--system-prompt <text>", ["--system-prompt", "be terse"]);
	c("append-system-prompt-once", "--append-system-prompt <text>", ["--append-system-prompt", "extra"]);
	c("name-long", "--name <name>", ["--name", "Refactor auth"]);
	c("name-short", "-n <name>", ["-n", "short name"]);
	c("no-session", "--no-session", ["--no-session"]);
	c("session", "--session <path|id>", ["--session", "abc123"]);
	c("session-id", "--session-id <id>", ["--session-id", "0198c0de-dead-beef-cafe-000000000000"]);
	c("fork", "--fork <path|id>", ["--fork", "abc123"]);
	c("session-dir", "--session-dir <dir>", ["--session-dir", "/tmp/sessions"]);
	c("models", "--models <patterns>", ["--models", "claude-sonnet,claude-haiku,gpt-4o"]);
	c("no-tools-long", "--no-tools", ["--no-tools"]);
	c("no-tools-short", "-nt", ["-nt"]);
	c("no-builtin-tools-long", "--no-builtin-tools", ["--no-builtin-tools"]);
	c("no-builtin-tools-short", "-nbt", ["-nbt"]);
	c("tools-long", "--tools <tools>", ["--tools", "read,grep,find,ls"]);
	c("tools-short", "-t <tools>", ["-t", "read,bash"]);
	c("exclude-tools-long", "--exclude-tools <tools>", ["--exclude-tools", "ask_question"]);
	c("exclude-tools-short", "-xt <tools>", ["-xt", "bash,write"]);
	for (const level of ["off", "minimal", "low", "medium", "high", "xhigh", "max"]) {
		c(`thinking-${level}`, `--thinking ${level} (valid level)`, ["--thinking", level]);
	}
	c("print-long-no-message", "--print with nothing after it", ["--print"]);
	c("print-short-no-message", "-p with nothing after it", ["-p"]);
	c("print-short-with-message", "-p <message>: the next token is consumed as a message", ["-p", "review src/"]);
	c("export", "--export <file>", ["--export", "session.jsonl"]);
	c("export-two-args", "--export takes ONE value; the second becomes a message", ["--export", "session.jsonl", "out.html"]);
	c("extension-long", "--extension <path>", ["--extension", "./ext.ts"]);
	c("extension-short", "-e <path>", ["-e", "./ext.ts"]);
	c("no-extensions-long", "--no-extensions", ["--no-extensions"]);
	c("no-extensions-short", "-ne", ["-ne"]);
	c("skill", "--skill <path>", ["--skill", "./skills/foo.md"]);
	c("prompt-template", "--prompt-template <path>", ["--prompt-template", "./prompts"]);
	c("theme", "--theme <path>", ["--theme", "./themes/dark.json"]);
	c("no-skills-long", "--no-skills", ["--no-skills"]);
	c("no-skills-short", "-ns", ["-ns"]);
	c("no-prompt-templates-long", "--no-prompt-templates", ["--no-prompt-templates"]);
	c("no-prompt-templates-short", "-np", ["-np"]);
	c("no-themes", "--no-themes (no short alias)", ["--no-themes"]);
	c("no-context-files-long", "--no-context-files", ["--no-context-files"]);
	c("no-context-files-short", "-nc", ["-nc"]);
	c("verbose", "--verbose", ["--verbose"]);
	c("approve-long", "--approve sets projectTrustOverride true", ["--approve"]);
	c("approve-short", "-a", ["-a"]);
	c("no-approve-long", "--no-approve sets projectTrustOverride false", ["--no-approve"]);
	c("no-approve-short", "-na", ["-na"]);
	c("offline", "--offline", ["--offline"]);
	c("file-arg", "@file strips the @ and lands in fileArgs", ["@prompt.md"]);
	c("file-args-and-message", "@files then a message", ["@prompt.md", "@image.png", "What color is the sky?"]);

	// ---- TRAPS (each named after the trap) ---------------------------------
	c(
		"trap:--model=sonnet-is-NOT-the-known-flag",
		"the `=` form is never matched by the known-flag branches, so it falls through to the generic `--` handler and lands in unknownFlags",
		["--model=sonnet"],
	);
	c("trap:--mode=json-is-NOT-the-known-flag", "same for --mode=json: unknownFlags {mode => \"json\"}, mode stays unset", ["--mode=json"]);
	c("trap:--foo=-empty-value", "`--foo=` yields the empty-string value, not true", ["--foo="]);
	c("trap:--foo=bar=baz-splits-on-FIRST-equals", "indexOf(\"=\") is the first one", ["--foo=bar=baz"]);
	c("trap:--=x-yields-empty-key", "`--=x`: eqIndex is 2, so slice(2,2) is the empty key", ["--=x"]);
	c(
		"trap:value-flag-as-last-token--model",
		"a LONG value-taking flag as the last token fails its `i + 1 < args.length` guard and falls through to the generic `--` handler: unknownFlags {model => true}, NO diagnostic",
		["--model"],
	);
	c("trap:value-flag-as-last-token--mode", "same for --mode", ["--mode"]);
	c("trap:value-flag-as-last-token--thinking", "same for --thinking", ["--thinking"]);
	c("trap:value-flag-as-last-token--tools", "same for --tools", ["--tools"]);
	c("trap:value-flag-as-last-token--session-dir", "same for --session-dir", ["--session-dir"]);
	c(
		"trap:short-alias-as-last-token--t-IS-FATAL",
		"CONTRAST with the long forms: `-t` alone fails its guard and falls into the `startsWith(\"-\") && !startsWith(\"--\")` branch, producing a FATAL `Unknown option: -t` diagnostic instead of an unknown flag",
		["-t"],
	);
	c("trap:short-alias-as-last-token--xt-IS-FATAL", "same for -xt", ["-xt"]);
	c("trap:short-alias-as-last-token--e-IS-FATAL", "same for -e", ["-e"]);
	c(
		"trap:short-alias-as-last-token--n-IS-A-NAME-ERROR",
		"-n is handled by the --name branch, which has its own else, so it errors with `--name requires a value` rather than `Unknown option`",
		["-n"],
	);
	c("trap:--name-as-last-token-errors", "--name with no value pushes an ERROR diagnostic", ["--name"]);
	c(
		"trap:--model---print-consumes-value-blindly",
		"value-taking flags do NOT inspect their value: model becomes the literal \"--print\" and --print is never seen",
		["--model", "--print"],
	);
	c(
		"trap:-p-consumes-a-triple-dash-token",
		"the -p guard is `!startsWith(\"@\") && (!startsWith(\"-\") || startsWith(\"---\"))`, so `---foo` IS consumed as a message",
		["-p", "---foo"],
	);
	c("trap:-p-does-NOT-consume-a-double-dash-token", "`--foo` after -p is left for the next iteration", ["-p", "--foo"]);
	c("trap:-p-does-NOT-consume-an-at-token", "`@f` after -p is left for the next iteration", ["-p", "@f"]);
	c("trap:-p-does-NOT-consume-a-single-dash-token", "`-x` after -p is left for the next iteration (and then errors)", ["-p", "-x"]);
	c(
		"trap:bare-double-dash-is-an-empty-key-unknown-flag",
		"`--` is not an end-of-options marker: it hits the generic `--` handler with flagName \"\" and greedily eats the next token, so messages stays EMPTY",
		["--", "hello"],
	);
	c("trap:bare-double-dash-alone", "`--` as the only token: unknownFlags {\"\" => true}", ["--"]);
	c("trap:unknown-flag-eats-the-next-token", "an unknown `--foo` consumes a following plain token as its value", ["--foo", "bar"]);
	c("trap:unknown-flag-does-NOT-eat-a-dash-token", "a following token starting with `-` is not eaten", ["--foo", "-bar"]);
	c("trap:unknown-flag-does-NOT-eat-an-at-token", "a following token starting with `@` is not eaten; it becomes a fileArg", ["--foo", "@baz"]);
	c("trap:unknown-flag-repeated-key-last-value-wins-first-position-kept", "Map.set on an existing key keeps its insertion position", [
		"--foo",
		"one",
		"--bar",
		"two",
		"--foo",
		"three",
	]);
	c("trap:--models-does-NOT-filter-empty-segments", "--models \"a,,b\" -> [\"a\",\"\",\"b\"] (only trim, no filter)", ["--models", "a,,b"]);
	c("trap:--models-trims-each-segment", "--models \"  a , b  \" -> [\"a\",\"b\"]", ["--models", "  a , b  "]);
	c("trap:--models-empty-string", "--models \"\" -> [\"\"]", ["--models", ""]);
	c("trap:--tools-DOES-filter-empty-segments", "--tools \"\" -> [] (filter(length > 0))", ["--tools", ""]);
	c("trap:--tools-drops-empty-segments", "--tools \"a,,b\" -> [\"a\",\"b\"]", ["--tools", "a,,b"]);
	c("trap:--exclude-tools-DOES-filter-empty-segments", "--exclude-tools \"\" -> []", ["--exclude-tools", ""]);
	c(
		"trap:--thinking-bogus-warns-and-leaves-the-field-unset",
		"an invalid level pushes a WARNING diagnostic and never assigns result.thinking",
		["--thinking", "bogus"],
	);
	c("trap:--thinking-is-case-sensitive", "\"HIGH\" is not a valid level", ["--thinking", "HIGH"]);
	c("trap:-zzz-unknown-option-error", "any unmatched single-dash token is a FATAL diagnostic", ["-zzz"]);
	c("trap:bare-dash", "`-` alone matches startsWith(\"-\") && !startsWith(\"--\") -> `Unknown option: -`", ["-"]);
	c("trap:empty-string-arg", "the empty string does not start with `-`, so it becomes an EMPTY message", [""]);
	c("trap:-a-then--na-last-wins", "projectTrustOverride ends up false", ["-a", "-na"]);
	c("trap:-na-then--a-last-wins", "projectTrustOverride ends up true", ["-na", "-a"]);
	c("trap:repeated-scalar-last-wins-model", "the second --model overwrites the first", ["--model", "a", "--model", "b"]);
	c("trap:repeated-scalar-last-wins-mode", "the second --mode overwrites the first", ["--mode", "json", "--mode", "rpc"]);
	c("trap:repeated-scalar-last-wins-tools", "the second --tools replaces the whole array", ["--tools", "read", "--tools", "bash,ls"]);
	c("trap:repeated-append-system-prompt-accumulates", "--append-system-prompt pushes", [
		"--append-system-prompt",
		"one",
		"--append-system-prompt",
		"two",
	]);
	c("trap:repeated-extension-accumulates", "--extension / -e push into the same array", ["--extension", "a.ts", "-e", "b.ts"]);
	c("trap:repeated-skill-accumulates", "--skill pushes", ["--skill", "a", "--skill", "b"]);
	c("trap:repeated-prompt-template-accumulates", "--prompt-template pushes", ["--prompt-template", "a", "--prompt-template", "b"]);
	c("trap:repeated-theme-accumulates", "--theme pushes", ["--theme", "a", "--theme", "b"]);
	c("trap:@-alone-yields-an-empty-file-arg", "`@`.slice(1) is \"\"", ["@"]);
	c("trap:@--weird-is-a-file-arg", "the `@` branch is checked BEFORE the `--` branch", ["@--weird"]);
	c("trap:@-with-spaces", "`@` keeps everything after the first character verbatim", ["@my file.md"]);
	c("trap:--list-models-with-a-value", "a following non-flag non-@ token becomes the search pattern", ["--list-models", "sonnet"]);
	c("trap:--list-models-without-a-value", "listModels is the boolean true", ["--list-models"]);
	c("trap:--list-models-followed-by-a-flag", "a following `-` token is NOT eaten; listModels is true", ["--list-models", "--print"]);
	c("trap:--list-models-followed-by-an-at-token", "a following `@` token is NOT eaten", ["--list-models", "@f"]);
	c("trap:--list-models-followed-by-empty-string", "\"\" does not start with - or @, so it IS taken as the pattern", ["--list-models", ""]);

	// ---- combinations ------------------------------------------------------
	c("combo:print-model-tools", "a realistic non-interactive invocation", [
		"--tools",
		"read,grep,find,ls",
		"-p",
		"Review the code in src/",
	]);
	c("combo:everything", "one token of nearly every kind, to pin insertion order of the result keys", [
		"--provider",
		"anthropic",
		"--model",
		"sonnet:high",
		"--thinking",
		"medium",
		"--mode",
		"json",
		"-c",
		"--verbose",
		"-a",
		"--offline",
		"@notes.md",
		"--unknown-ext-flag",
		"value",
		"trailing message",
	]);
	c("combo:flag-after-message", "flags are positional-independent", ["hello", "--verbose", "world"]);
	c("combo:diagnostics-order", "diagnostics accumulate in argv order (warning then error)", [
		"--thinking",
		"bogus",
		"-zzz",
		"--name",
	]);

	return cases;
}

async function genArgsCorpus() {
	const args = await impPi("cli/args.ts");
	const cases = argsCases();
	const purity = assertParseArgsPure(args.parseArgs, cases);
	const records = cases.map((c) => ({
		name: c.name,
		note: c.note,
		argv: c.argv,
		result: serializeArgs(args.parseArgs(c.argv)),
	}));
	emit("args.corpus.jsonl", jsonl(records.map(normalizeDeep)));
	return { records, purity };
}

// ===========================================================================
// B. help.*.golden + help.identity.json
// ===========================================================================

/**
 * Capture `printHelp()`'s stdout, four ways.
 *
 * ENV VARS, exactly:
 *   plain      -> NO_COLOR="1",   FORCE_COLOR deleted  (chalk level 0)
 *   color      -> FORCE_COLOR="1", NO_COLOR deleted     (chalk level >= 1)
 * Both children also inherit CI/TERM unchanged; stdout is a PIPE (never a TTY),
 * which is why colour has to be forced rather than merely allowed.
 *
 * The child is this very file re-invoked with `--emit-help <variant>`, so it
 * goes through the identical resolve/load hooks. Its stdout bytes are taken
 * verbatim (no line-ending rewriting: printHelp's template and console.log both
 * emit "\n").
 */
async function genHelpGoldens() {
	const { spawnSync } = await import("node:child_process");
	const selfPath = fileURLToPath(import.meta.url);
	const variants = [
		{ variant: "plain", file: "help.plain.golden", color: false, ext: false },
		{ variant: "color", file: "help.color.golden", color: true, ext: false },
		{ variant: "plain-ext", file: "help.plain.ext.golden", color: false, ext: true },
		{ variant: "color-ext", file: "help.color.ext.golden", color: true, ext: true },
	];
	const sizes = {};
	for (const v of variants) {
		const env = { ...process.env };
		delete env.NO_COLOR;
		delete env.FORCE_COLOR;
		if (v.color) env.FORCE_COLOR = "1";
		else env.NO_COLOR = "1";
		const res = spawnSync(process.execPath, [selfPath, "--emit-help", v.variant], {
			env,
			encoding: "buffer",
			maxBuffer: 8 * 1024 * 1024,
		});
		if (res.status !== 0) {
			throw new Error(`help child (${v.variant}) exited ${res.status}: ${res.stderr?.toString("utf-8")}`);
		}
		const text = res.stdout.toString("utf-8");
		if (v.color !== text.includes("\u001b[1m")) {
			throw new Error(`help child (${v.variant}) colour expectation not met (bold sequence present=${!v.color})`);
		}
		emit(v.file, text);
		sizes[v.file] = Buffer.byteLength(text);
	}

	emit(
		"help.ext-flags.json",
		pretty({
			note: "The exact ExtensionFlag[] passed to printHelp() for the *.ext.golden files. Pi renders each as `  --${name}${type === \"string\" ? \" <value>\" : \"\"}`.padEnd(30) + (description ?? `Registered by ${extensionPath}`), joined with \"\\n\", wrapped in a leading \"\\n\" + bold \"Extension CLI Flags:\" + \"\\n\" and a trailing \"\\n\".",
			flags: EXT_FLAGS,
		}),
	);

	// -- identity in effect ---------------------------------------------------
	const config = await impPi("config.ts");
	emit(
		"help.identity.json",
		pretty({
			note: "App identity read from pi/packages/coding-agent/package.json when the help goldens were captured. printHelp() interpolates APP_NAME, CONFIG_DIR_NAME, ENV_AGENT_DIR and ENV_SESSION_DIR into its template; the Rust port renders the SAME template with pirust's identity, so a golden test substitutes these values back in to prove byte-identity. VERSION/APP_TITLE/PACKAGE_NAME do NOT appear in the help text and are recorded for reference only.",
			PACKAGE_NAME: config.PACKAGE_NAME,
			APP_NAME: config.APP_NAME,
			APP_TITLE: config.APP_TITLE,
			CONFIG_DIR_NAME: config.CONFIG_DIR_NAME,
			VERSION: config.VERSION,
			ENV_AGENT_DIR: config.ENV_AGENT_DIR,
			ENV_SESSION_DIR: config.ENV_SESSION_DIR,
			isBunBinary: config.isBunBinary,
			isBunRuntime: config.isBunRuntime,
			boldOn: "\u001b[1m",
			boldOff: "\u001b[22m",
			envVars: {
				plainGoldens: { NO_COLOR: "1", FORCE_COLOR: null },
				colorGoldens: { NO_COLOR: null, FORCE_COLOR: "1" },
			},
		}),
	);
	return sizes;
}

// ===========================================================================
// G. session_dir.cases.jsonl
// ===========================================================================

/**
 * The cwd -> session-directory-name encoding, observed on a REAL filesystem.
 *
 * Two independent implementations of the same encoding exist in Pi and both are
 * captured:
 *   migrations.ts:112          `--${cwd.replace(/^[/\\]/, "").replace(/[/\\:]/g, "-")}--`
 *                              on the RAW header cwd (no path resolution).
 *   session-manager.ts:475     the same expression on `resolvePath(cwd)`.
 *
 * The migrations.ts encoding is driven through the real
 * `migrateSessionsFromAgentRoot()`: a session .jsonl is planted in the agent
 * root with the given cwd in its header, the migration runs, and the resulting
 * tree is snapshotted. This also pins a platform bug, faithfully:
 *
 *   migrations.ts:121 computes `file.split("/").pop() || file.split("\\").pop()`.
 *   On win32 `join()` produces backslash paths, so the FIRST split yields a
 *   single element - the entire absolute path - which is truthy, so the
 *   backslash fallback is dead. `fileName` becomes the full absolute path,
 *   `join(correctDir, fileName)` is then an invalid win32 path and renameSync
 *   throws into the swallowing catch. Net effect on win32: the encoded
 *   sessions/<name>/ directory IS created (mkdirSync runs first) but the file is
 *   NEVER moved. Records carry `platformDependent: "m2-filename-split"`.
 */
async function genSessionDirCases() {
	const migrations = await impPi("migrations.ts");
	const sessionManager = await impPi("core/session-manager.ts");

	const HEADER_EXTRA = { version: 2, id: "0198c0de-0000-4000-8000-000000000001" };
	const cases = [
		{ name: "posix-path", note: "a plain POSIX absolute path; the single leading slash is stripped", cwd: "/home/user/project" },
		{ name: "windows-path-with-drive-letter", note: "backslashes AND the drive colon are replaced; the drive letter is NOT stripped (the leading-separator strip only matches position 0)", cwd: "C:\\Users\\me\\project" },
		{ name: "windows-path-forward-slashes", note: "a drive-letter path written with forward slashes", cwd: "C:/Users/me/project" },
		{ name: "path-with-spaces", note: "spaces are NOT encoded", cwd: "/tmp/my project/with spaces" },
		{ name: "path-with-non-ascii", note: "non-ASCII is passed through verbatim (no percent/punycode encoding, no NFC/NFD normalization by Pi)", cwd: "/tmp/caf\u00e9/\u65e5\u672c\u8a9e" },
		{ name: "unc-path", note: "UNC: only ONE of the two leading backslashes is stripped, so the name gains an extra leading dash (`---server-share-dir--`)", cwd: "\\\\server\\share\\dir" },
		{ name: "filesystem-root-posix", note: "\"/\" strips to the empty string, so the directory name is exactly `----`", cwd: "/" },
		{ name: "filesystem-root-windows-drive", note: "\"C:\\\\\" -> `--C----`", cwd: "C:\\" },
		{ name: "relative-path", note: "the migration does NOT resolve the header cwd, so a relative value is encoded as-is", cwd: "sub/dir" },
		{ name: "trailing-slash", note: "a trailing separator becomes a trailing dash", cwd: "/home/user/project/" },
		{ name: "empty-cwd-is-skipped", note: "`!header.cwd` is falsy, so the file is left alone and no sessions/ dir is created", cwd: "" },
	];

	const records = [];
	for (const c of cases) {
		const agentDir = newAgentDir(`sessdir-${c.name}`);
		const header = JSON.stringify({ type: "session", cwd: c.cwd, ...HEADER_EXTRA });
		put(agentDir, "sess.jsonl", `${header}\n`);
		const before = snapshotTree(agentDir);
		const { console: consoleOut } = captureConsole(() => withAgentDir(agentDir, () => migrations.migrateSessionsFromAgentRoot()));
		const after = snapshotTree(agentDir);
		const createdDirs = after
			.filter((e) => e.path.startsWith("sessions/") && e.path.endsWith("/") && e.path.split("/").length === 3)
			.map((e) => e.path.slice("sessions/".length, -1));
		records.push({
			fn: "migrateSessionsFromAgentRoot",
			name: c.name,
			note: c.note,
			cwd: c.cwd,
			platform: PLATFORM,
			platformDependent: "m2-filename-split",
			encodedDirName: createdDirs.length === 1 ? createdDirs[0] : createdDirs,
			fileWasMoved: !after.some((e) => e.path === "sess.jsonl"),
			before,
			after,
			console: consoleOut,
		});
	}

	// -- already-migrated / guard cases ---------------------------------------
	const guardCases = [
		{
			name: "guard:header-type-is-not-session",
			note: "`header.type !== \"session\"` -> file untouched, no sessions/ dir",
			firstLine: JSON.stringify({ type: "message", cwd: "/home/user/project" }),
		},
		{
			name: "guard:header-has-no-cwd",
			note: "`!header.cwd` -> file untouched",
			firstLine: JSON.stringify({ type: "session", id: "x" }),
		},
		{ name: "guard:blank-first-line", note: "`!firstLine?.trim()` -> skipped", firstLine: "" },
		{ name: "guard:unparseable-first-line", note: "JSON.parse throws into the swallowing catch -> skipped", firstLine: "{not json" },
	];
	for (const g of guardCases) {
		const agentDir = newAgentDir(`sessdir-${g.name.replace(/[^a-z0-9]+/gi, "-")}`);
		put(agentDir, "sess.jsonl", `${g.firstLine}\n`);
		const before = snapshotTree(agentDir);
		const { console: consoleOut } = captureConsole(() => withAgentDir(agentDir, () => migrations.migrateSessionsFromAgentRoot()));
		records.push({
			fn: "migrateSessionsFromAgentRoot",
			name: g.name,
			note: g.note,
			cwd: null,
			platform: PLATFORM,
			platformDependent: "m2-filename-split",
			before,
			after: snapshotTree(agentDir),
			console: consoleOut,
		});
	}

	// -- the resolving variant used at runtime --------------------------------
	// getDefaultSessionDirPath(cwd, agentDir) is pure (the mkdirSync lives in its
	// exported wrapper getDefaultSessionDir, which is deliberately NOT called).
	// Only ABSOLUTE cwds are fed to it: resolvePath() would resolve a relative
	// value against process.cwd(), which is not reproducible.
	//
	// !! NOT CWD-INDEPENDENT ON WIN32 !!
	// A rooted-but-driveless path like "/home/user/project" is NOT absolute in the
	// win32 sense: path.win32.resolve() adopts the DRIVE LETTER OF process.cwd(),
	// so the encoded directory name silently embeds the capture machine's drive
	// ("--C--home-user-project--"). Every such record therefore carries the drive
	// explicitly (`cwdDriveInjected`), the cwd it was resolved against
	// (`resolveCwd`), and a `resultTemplate` in which that injected drive letter is
	// replaced by the literal placeholder "{CWDDRIVE}" so a test can substitute its
	// own. `cwdDriveInjected` is null when nothing was injected (POSIX hosts, or a
	// win32 input that already carried a drive or was a UNC path).
	const pathUtils = await impPi("utils/paths.ts");
	const fixedAgentDir = PLATFORM === "win32" ? "C:\\oracle\\agent" : "/oracle/agent";
	const captureCwd = process.cwd();
	for (const c of cases) {
		if (c.cwd === "" || c.name === "relative-path") continue;
		const result = sessionManager.__getDefaultSessionDirPath(c.cwd, fixedAgentDir);
		const resolved = pathUtils.resolvePath(c.cwd);
		const inputHadDrive = /^[A-Za-z]:/.test(c.cwd);
		const resolvedDrive = /^([A-Za-z]):/.exec(resolved)?.[1] ?? null;
		const cwdDriveInjected = !inputHadDrive && resolvedDrive !== null ? resolvedDrive : null;
		records.push({
			fn: "getDefaultSessionDirPath",
			name: c.name,
			note: "session-manager.ts:475 - the same encoding, but applied to resolvePath(cwd) instead of the raw string. See cwdDriveInjected/resultTemplate: on win32 a rooted-but-driveless input picks up process.cwd()'s drive letter.",
			cwd: c.cwd,
			agentDir: fixedAgentDir,
			platform: PLATFORM,
			resolveCwd: captureCwd,
			resolvedCwd: resolved,
			inputHadDrive,
			cwdDriveInjected,
			result,
			resultTemplate: cwdDriveInjected ? result.replace(`--${cwdDriveInjected}--`, "--{CWDDRIVE}--") : result,
		});
	}

	emit("session_dir.cases.jsonl", jsonl(records.map(normalizeDeep)));
	return records;
}

// ===========================================================================
// J. session_migration.cases.jsonl
// ===========================================================================

/**
 * The v1 -> v2 -> v3 session migration, run through Pi's real
 * migrateToCurrentVersion / migrateV1ToV2 / migrateV2ToV3.
 *
 * INPUTS. Two of the slices come from Pi's OWN test fixtures,
 * ../pi/packages/coding-agent/test/fixtures/{before-compaction,large-session}.jsonl
 * (2.3 MB and 951 KB). Vendoring those wholesale is not worth it, so each is
 * TRIMMED to the smallest entry of every distinct (type, message.role) it
 * contains - "smallest" measured as JSON.stringify(entry).length, ties broken by
 * the lower original index. That selection rule is deterministic and is
 * re-derived on every run, so the slice cannot drift silently. Original indices
 * are recorded in `sliceIndices`. Slice lines are BYTE-EXACT copies of the
 * source lines with ONE documented exception, below.
 *
 * THE ONE REWRITE: `firstKeptEntryIndex` is an index into the WHOLE entries
 * array, so trimming invalidates it. Each kept compaction's index is retargeted
 * to the slice-local position of a chosen entry, recorded as
 * `firstKeptEntryIndexRewrites: [{originalIndex, newIndex, targetsSliceEntry}]`.
 * Nothing else in a real line is altered.
 *
 * NON-DETERMINISM: migrateV1ToV2 assigns `entry.id = randomUUID().slice(0, 8)`,
 * so every id, every `parentId` and every resolved `firstKeptEntryId` is random
 * per run. Byte-pinning random bytes is impossible AND useless (a Rust port
 * generates its own), so each generated id is replaced by the placeholder
 * "{ID:<n>}", where n is the id's ASSIGNMENT ORDINAL - i.e. the entry's position
 * among the non-session entries, which is exactly the order migrateV1ToV2 walks.
 * The substitution is done on the entry objects (exact string equality), never
 * by substring search, so it cannot corrupt unrelated content. A Rust test
 * substitutes its own ids in the same order and then compares byte-for-byte.
 * `idAssignment` lists the mapping. Records that generate no ids (v2->v3 only,
 * already-v3) carry no placeholders at all and ARE literally byte-exact.
 *
 * KEY ORDER IS THE POINT. `entry.version = N` APPENDS `version` when the key is
 * absent (a v1 header) but KEEPS ITS POSITION when re-assigned (a v2 header), and
 * `id`/`parentId`/`firstKeptEntryId` are appended in that order while
 * `firstKeptEntryIndex` is deleted from the middle. Every record therefore
 * carries `headerKeyOrderBefore`/`headerKeyOrderAfter` and, for compactions,
 * `compactionKeyOrderBefore`/`After`, so the contrast is machine-checkable.
 */
async function genSessionMigrationCases() {
	const sm = await impPi("core/session-manager.ts");
	const PI_TEST_FIXTURES = join(PKGS, "coding-agent", "test", "fixtures");
	const records = [];

	const bucketKey = (e) => (e.type === "message" && e.message ? `message:${e.message.role}` : e.type);

	/** Deterministic trim: smallest entry per (type, role), ties -> lower index. */
	function pickSliceIndices(entries) {
		const best = new Map();
		entries.forEach((entry, index) => {
			const key = bucketKey(entry);
			const len = JSON.stringify(entry).length;
			const current = best.get(key);
			if (!current || len < current.len) best.set(key, { index, len });
		});
		return [...best.values()].map((v) => v.index).sort((a, b) => a - b);
	}

	/**
	 * Replace every NEWLY GENERATED id with "{ID:<assignment ordinal>}", where the
	 * ordinal is the entry's position among the non-session entries - exactly the
	 * order migrateV1ToV2 walks. Ids that were already present in the input are
	 * left ALONE, so a v2->v3 or already-v3 record stays literally byte-exact.
	 * Runs on the object graph by exact string equality, never by substring search.
	 */
	function placeholderizeIds(entries, preExistingIds) {
		const map = new Map();
		let ordinal = 0;
		for (const entry of entries) {
			if (entry.type === "session") continue;
			if (typeof entry.id === "string" && !preExistingIds.has(entry.id) && !map.has(entry.id)) {
				map.set(entry.id, `{ID:${ordinal}}`);
			}
			ordinal++;
		}
		const walk = (value) => {
			if (typeof value === "string") return map.get(value) ?? value;
			if (value === null || typeof value !== "object") return value;
			if (Array.isArray(value)) return value.map(walk);
			const out = {};
			for (const key of Object.keys(value)) out[key] = walk(value[key]);
			return out;
		};
		return { entries: entries.map(walk), idAssignment: [...map.entries()].map(([, placeholder]) => placeholder) };
	}

	const lines = (entries) => entries.map((e) => JSON.stringify(e));
	const compactionKeyOrders = (entries) => entries.filter((e) => e.type === "compaction").map((e) => Object.keys(e));

	/**
	 * Run the real migration over `beforeEntries` and build one record.
	 * `beforeEntries` must already be plain JSON-round-tripped objects.
	 */
	function migrationRecord({ name, source, note, beforeEntries, extra = {} }) {
		const before = lines(beforeEntries);
		const header = beforeEntries.find((e) => e.type === "session");
		// The migration MUTATES in place, so hand it a fresh parse of the exact
		// bytes recorded as `before`.
		const working = before.map((line) => JSON.parse(line));
		const preExistingIds = new Set(
			beforeEntries.filter((e) => e.type !== "session" && typeof e.id === "string").map((e) => e.id),
		);
		const migrated = sm.__migrateToCurrentVersion(working);
		const { entries: placeholdered, idAssignment } = placeholderizeIds(working, preExistingIds);
		const afterHeader = placeholdered.find((e) => e.type === "session");
		const after = lines(placeholdered);
		return {
			fn: "migrateToCurrentVersion",
			name,
			source,
			note,
			platform: PLATFORM,
			entryTypes: [...new Set(beforeEntries.map(bucketKey))].sort(),
			versionBefore: header?.version ?? null,
			versionAfter: afterHeader?.version ?? null,
			migrated,
			byteIdentical: before.join("\n") === after.join("\n"),
			idsGenerated: idAssignment.length,
			preExistingIds: [...preExistingIds],
			idAssignment,
			headerKeyOrderBefore: header ? Object.keys(header) : null,
			headerKeyOrderAfter: afterHeader ? Object.keys(afterHeader) : null,
			compactionKeyOrderBefore: compactionKeyOrders(beforeEntries),
			compactionKeyOrderAfter: compactionKeyOrders(placeholdered),
			...extra,
			before,
			after,
		};
	}

	// -- slices of Pi's own 2.3 MB / 951 KB test fixtures ----------------------
	const REAL_SOURCES = [
		{
			file: "before-compaction.jsonl",
			note: "Slice of Pi's own before-compaction.jsonl (2 370 492 bytes, 1003 entries, v1 - the header has NO `version` key and no entry has id/parentId). Covers every type in that file: session, message (roles user/assistant/toolResult/bashExecution), thinking_level_change, model_change, compaction. The compaction still carries firstKeptEntryIndex, so this record pins the index->id resolution AND the `firstKeptEntryIndex` deletion.",
			// Retarget the kept compaction at the toolResult message inside the slice.
			retarget: (sliceEntries) => sliceEntries.findIndex((e) => e.type === "message" && e.message?.role === "toolResult"),
		},
		{
			file: "large-session.jsonl",
			note: "Slice of Pi's own large-session.jsonl (974 011 bytes, 1019 entries, v1). Covers session, message (roles user/assistant/toolResult), thinking_level_change, model_change. It has NO compaction and NO hookMessage, so it isolates the plain id/parentId chaining.",
			retarget: null,
		},
	];

	const realSlices = {};
	for (const src of REAL_SOURCES) {
		const path = join(PI_TEST_FIXTURES, src.file);
		if (!existsSync(path)) {
			records.push({
				fn: "migrateToCurrentVersion",
				name: `slice:${src.file}`,
				source: src.file,
				note: `SKIPPED: ${path} is absent from this checkout, so no slice could be taken.`,
				skipped: true,
			});
			continue;
		}
		const raw = readFileSync(path, "utf-8");
		const allEntries = sm.parseSessionEntries(raw);
		const sliceIndices = pickSliceIndices(allEntries);
		const sliceEntries = sliceIndices.map((i) => JSON.parse(JSON.stringify(allEntries[i])));
		const rewrites = [];
		if (src.retarget) {
			const newIndex = src.retarget(sliceEntries);
			for (const entry of sliceEntries) {
				if (entry.type !== "compaction" || typeof entry.firstKeptEntryIndex !== "number") continue;
				rewrites.push({
					originalIndex: entry.firstKeptEntryIndex,
					newIndex,
					targetsSliceEntry: bucketKey(sliceEntries[newIndex]),
				});
				entry.firstKeptEntryIndex = newIndex;
			}
		}
		realSlices[src.file] = sliceEntries;
		records.push(
			migrationRecord({
				name: `slice:${src.file}`,
				source: `pi/packages/coding-agent/test/fixtures/${src.file}`,
				note: src.note,
				beforeEntries: sliceEntries,
				extra: {
					sourceBytes: Buffer.byteLength(raw),
					sourceEntryCount: allEntries.length,
					sliceIndices,
					sliceRule: "the smallest entry (by JSON.stringify length) of every distinct (type, message.role) in the source, ties broken by the lower original index, kept in original order",
					firstKeptEntryIndexRewrites: rewrites,
				},
			}),
		);
	}

	// -- compaction index edge cases, built from a REAL compaction entry -------
	const realCompaction = (realSlices["before-compaction.jsonl"] ?? []).find((e) => e.type === "compaction");
	if (realCompaction) {
		const baseSlice = realSlices["before-compaction.jsonl"];
		const userMsg = baseSlice.find((e) => e.type === "message" && e.message?.role === "user");
		const header = baseSlice.find((e) => e.type === "session");
		const comp = (firstKeptEntryIndex) => ({ ...structuredCloneJson(realCompaction), firstKeptEntryIndex });
		const entries = [
			structuredCloneJson(header),
			structuredCloneJson(userMsg),
			comp(1), // -> the user message: resolves
			comp(0), // -> the session header: guarded out
			comp(99), // -> out of range: guarded out
			{ ...structuredCloneJson(realCompaction), firstKeptEntryIndex: "1" }, // non-number
		];
		delete entries[5].firstKeptEntryId;
		records.push(
			migrationRecord({
				name: "compaction-firstKeptEntryIndex-edge-cases",
				source: "pi/packages/coding-agent/test/fixtures/before-compaction.jsonl (the real compaction entry, re-targeted four ways)",
				note: "Four copies of the SAME real compaction entry with different firstKeptEntryIndex values, to pin every branch of the resolution: (a) index -> a normal entry, firstKeptEntryId is APPENDED after id/parentId and firstKeptEntryIndex is DELETED from the middle; (b) index 0 -> the session header, where `targetEntry.type !== \"session\"` fails so firstKeptEntryId is NOT set but firstKeptEntryIndex is STILL DELETED; (c) an out-of-range index, where targetEntry is undefined - same outcome as (b); (d) a firstKeptEntryIndex that is a STRING, where the `typeof === \"number\"` guard fails so the key is NOT deleted and survives untouched into v3.",
				beforeEntries: entries,
				extra: {
					firstKeptEntryIndexRewrites: [
						{ originalIndex: realCompaction.firstKeptEntryIndex, newIndex: 1, targetsSliceEntry: "message:user" },
						{ originalIndex: realCompaction.firstKeptEntryIndex, newIndex: 0, targetsSliceEntry: "session (guarded out)" },
						{ originalIndex: realCompaction.firstKeptEntryIndex, newIndex: 99, targetsSliceEntry: "out of range (guarded out)" },
						{ originalIndex: realCompaction.firstKeptEntryIndex, newIndex: "1 (string, guard fails)", targetsSliceEntry: "none" },
					],
				},
			}),
		);
	}

	// -- v2 -> v3 only: hookMessage, and `version` KEEPING its position --------
	// Neither real fixture contains a hookMessage role or a v2 header, so this
	// input is oracle-authored (like args.corpus.jsonl's argv arrays); the
	// migration that produces `after` is still Pi's own. It is built to be fully
	// deterministic: every entry ALREADY has id/parentId, so version >= 2 means
	// migrateV1ToV2 never runs and NO random ids are generated - `after` is
	// literally byte-exact with no placeholders.
	const v2Entries = [
		{ type: "session", id: "0198c0de-0000-4000-8000-000000000001", version: 2, timestamp: "2025-12-09T00:53:29.825Z", cwd: "/home/user/project", provider: "anthropic", modelId: "claude-opus-4-8", thinkingLevel: "medium" },
		{ type: "message", id: "aaaaaaaa", parentId: null, timestamp: "2025-12-09T00:53:30.000Z", message: { role: "user", content: "hello" } },
		{ type: "message", id: "bbbbbbbb", parentId: "aaaaaaaa", timestamp: "2025-12-09T00:53:31.000Z", message: { role: "hookMessage", content: "from a hook", customType: "my-ext" } },
		{ type: "message", id: "cccccccc", parentId: "bbbbbbbb", timestamp: "2025-12-09T00:53:32.000Z", message: { role: "assistant", content: [{ type: "text", text: "hi" }] } },
		{ type: "custom", id: "dddddddd", parentId: "cccccccc", timestamp: "2025-12-09T00:53:33.000Z", customType: "my-ext", data: { k: 1 } },
		{ type: "custom_message", id: "eeeeeeee", parentId: "dddddddd", timestamp: "2025-12-09T00:53:34.000Z", customType: "my-ext", message: { role: "hookMessage", content: "nested role on a custom_message entry" } },
		{ type: "compaction", id: "ffffffff", parentId: "eeeeeeee", timestamp: "2025-12-09T00:53:35.000Z", summary: "already migrated", firstKeptEntryId: "aaaaaaaa", tokensBefore: 1234 },
		{ type: "message", id: "11111111", parentId: "ffffffff", timestamp: "2025-12-09T00:53:36.000Z", message: { role: "toolResult", content: "ok" } },
	];
	records.push(
		migrationRecord({
			name: "v2-to-v3-only:hookMessage-and-version-key-KEEPS-POSITION",
			source: "oracle-authored (see note)",
			note: "ORACLE-AUTHORED INPUT - neither real fixture contains a hookMessage role or a v2 header. THE KEY-ORDER CONTRAST: `version` is the header's SECOND key here, and because migrateV2ToV3 RE-ASSIGNS an existing key it stays second (value 2 -> 3). Compare the v1 slices above, where `version` was ABSENT and the same statement APPENDED it at the very end. Also pinned: only `type: \"message\"` entries have their role rewritten, so the `hookMessage` on the message entry becomes `custom` while the IDENTICAL role nested in the `custom_message` entry is LEFT ALONE (migrateV2ToV3 only looks at entry.type === \"message\"); `type: \"custom\"` entries are untouched; a compaction that already has firstKeptEntryId is untouched; and no ids are generated because the file is already v2.",
			beforeEntries: v2Entries,
		}),
	);

	// -- v1 with a hookMessage: BOTH migrations run on one file ----------------
	records.push(
		migrationRecord({
			name: "v1-to-v3:both-migrations-in-one-pass",
			source: "oracle-authored (see note)",
			note: "ORACLE-AUTHORED INPUT. version < 2 so migrateV1ToV2 runs (appending `version`, then id/parentId per entry) AND version < 3 so migrateV2ToV3 runs immediately after, re-assigning the just-appended `version` to 3 - which is why the v1 path ends with `version` LAST in the header but holding the value 3, never 2. The hookMessage role is rewritten in the same pass.",
			beforeEntries: [
				{ type: "session", id: "0198c0de-0000-4000-8000-000000000002", timestamp: "2025-12-09T00:53:29.825Z", cwd: "/home/user/project", provider: "anthropic", modelId: "claude-opus-4-8", thinkingLevel: "medium" },
				{ type: "message", timestamp: "2025-12-09T00:53:30.000Z", message: { role: "user", content: "hello" } },
				{ type: "message", timestamp: "2025-12-09T00:53:31.000Z", message: { role: "hookMessage", content: "from a hook" } },
				{ type: "compaction", timestamp: "2025-12-09T00:53:32.000Z", summary: "s", firstKeptEntryIndex: 1, tokensBefore: 7 },
			],
		}),
	);

	// -- already current: the guard ------------------------------------------
	const v3Entries = v2Entries.map((e) => (e.type === "session" ? { ...e, version: 3 } : structuredCloneJson(e)));
	records.push(
		migrationRecord({
			name: "already-v3-is-a-NO-OP",
			source: "oracle-authored (the v2 record's entries with version: 3)",
			note: "GUARD: `if (version >= CURRENT_SESSION_VERSION) return false` - migrateToCurrentVersion returns FALSE and mutates nothing, so `after` is byte-identical to `before` (INCLUDING the still-`hookMessage` roles and the header's key order). In the real load path setSessionFile only calls _rewriteFile() when this returns true, so the file on disk is not even reopened.",
			beforeEntries: v3Entries,
		}),
	);
	records.push(
		migrationRecord({
			name: "future-version-is-also-a-NO-OP",
			source: "oracle-authored",
			note: "the guard is `>=`, so a version NEWER than CURRENT_SESSION_VERSION is also left completely alone rather than being downgraded or rejected",
			beforeEntries: v3Entries.map((e) => (e.type === "session" ? { ...e, version: 99 } : structuredCloneJson(e))),
		}),
	);
	records.push(
		migrationRecord({
			name: "no-session-header-is-treated-as-v1",
			source: "oracle-authored",
			note: "EDGE: `header?.version ?? 1` - a file with NO session header at all is treated as v1, so ids are generated for every entry and there is no header to stamp a `version` onto",
			beforeEntries: [
				{ type: "message", timestamp: "2025-12-09T00:53:30.000Z", message: { role: "user", content: "hello" } },
				{ type: "message", timestamp: "2025-12-09T00:53:31.000Z", message: { role: "assistant", content: "hi" } },
			],
		}),
	);

	// -- constants + the on-disk rewrite format --------------------------------
	records.push({
		fn: "constants",
		name: "session-migration-constants",
		note: "CURRENT_SESSION_VERSION gates the migration. The real load path is setSessionFile -> loadEntriesFromFile -> migrateToCurrentVersion, and ONLY when that returns true does _rewriteFile() run; _rewriteFile writes `${JSON.stringify(entry)}\\n` per entry to a freshly truncated file, so the migrated file's bytes are exactly the `after` lines joined by \"\\n\" WITH a trailing newline and no CR. parseSessionEntries trims the whole content, splits on \"\\n\", skips blank lines, and SILENTLY DROPS lines that fail JSON.parse.",
		CURRENT_SESSION_VERSION: sm.CURRENT_SESSION_VERSION,
		rewriteFormat: "for each entry: JSON.stringify(entry) + \"\\n\"",
		platform: PLATFORM,
	});

	emit("session_migration.cases.jsonl", jsonl(records.map(normalizeDeep)));
	return records;
}

/** JSON round-trip clone (keeps key order, drops undefined). */
function structuredCloneJson(value) {
	return JSON.parse(JSON.stringify(value));
}

// ===========================================================================
// C. app_mode.cases.jsonl
// ===========================================================================

/**
 * `resolveAppMode(parsed, stdinIsTTY, stdoutIsTTY)` over the full matrix, plus
 * `toPrintOutputMode` and `isPlainRuntimeMetadataCommand`, plus the two
 * PI_OFFLINE truthiness rules.
 *
 * `parsed` is always built by the REAL parseArgs from a real argv, so no
 * hand-shaped Args object is ever fed in.
 *
 * NOTE on the "piped stdin downgrades interactive -> print" branch at
 * main.ts:770-772: it is unreachable. readPipedStdin() returns undefined
 * whenever process.stdin.isTTY, and appMode === "interactive" already implies
 * stdinIsTTY (resolveAppMode returns "print" for !stdinIsTTY). No fixture case
 * models it; the real mechanism is resolveAppMode's `!stdinIsTTY -> "print"`.
 */
async function genAppModeCases() {
	const argsMod = await impPi("cli/args.ts");
	const mainMod = await impPi("main.ts");
	const records = [];

	const MODES = [undefined, "text", "json", "rpc"];
	for (const mode of MODES) {
		for (const print of [false, true]) {
			for (const stdinIsTTY of [true, false]) {
				for (const stdoutIsTTY of [true, false]) {
					const argv = [...(mode === undefined ? [] : ["--mode", mode]), ...(print ? ["-p"] : [])];
					const parsed = argsMod.parseArgs(argv);
					const note =
						mode === "text"
							? "--mode text is parsed into parsed.mode but resolveAppMode has NO branch for it, so it falls through to the print/TTY logic exactly like an absent --mode"
							: mode === "rpc" || mode === "json"
								? `--mode ${mode} short-circuits before the print/TTY logic`
								: "no --mode: print flag, then either TTY being false, decides";
					records.push({
						fn: "resolveAppMode",
						argv,
						mode: mode ?? null,
						print,
						stdinIsTTY,
						stdoutIsTTY,
						note,
						result: mainMod.__resolveAppMode(parsed, stdinIsTTY, stdoutIsTTY),
					});
				}
			}
		}
	}

	for (const appMode of ["interactive", "print", "json", "rpc"]) {
		records.push({
			fn: "toPrintOutputMode",
			appMode,
			note: "only \"json\" maps to \"json\"; everything else (including \"rpc\", which never reaches print mode) maps to \"text\"",
			result: mainMod.__toPrintOutputMode(appMode),
		});
	}

	const plainCases = [
		{ argv: ["--help"], note: "help with no print/mode -> exempt from takeOverStdout" },
		{ argv: ["-h"], note: "alias behaves identically" },
		{ argv: ["-p", "--help"], note: "print is set, so NOT exempt" },
		{ argv: ["--help", "--mode", "json"], note: "mode is set, so NOT exempt" },
		{ argv: ["--help", "--mode", "text"], note: "even --mode text disqualifies it: the check is `mode === undefined`, not `mode !== \"json\"`" },
		{ argv: ["--list-models"], note: "listModels === true -> exempt" },
		{ argv: ["--list-models", "sonnet"], note: "listModels is a string -> still `!== undefined` -> exempt" },
		{ argv: ["--mode", "json", "--list-models"], note: "mode is set, so NOT exempt" },
		{ argv: ["-p", "--list-models"], note: "print is set, so NOT exempt" },
		{ argv: ["--list-models", "--print"], note: "--print after --list-models is NOT eaten as a pattern, so print is true -> NOT exempt" },
		{ argv: ["--version"], note: "version is NOT part of the exemption" },
		{ argv: [], note: "plain invocation -> not exempt" },
		{ argv: ["--help=1"], note: "`--help=1` is an unknown flag, so help is unset -> not exempt" },
	];
	for (const c of plainCases) {
		const parsed = argsMod.parseArgs(c.argv);
		records.push({
			fn: "isPlainRuntimeMetadataCommand",
			argv: c.argv,
			note: c.note,
			parsedPrint: parsed.print ?? null,
			parsedMode: parsed.mode ?? null,
			parsedHelp: parsed.help ?? null,
			parsedListModels: parsed.listModels === undefined ? null : parsed.listModels,
			result: mainMod.__isPlainRuntimeMetadataCommand(parsed),
		});
	}

	// -- PI_OFFLINE: two different truthiness rules in the same codebase -------
	for (const value of [undefined, "", "0", "1", "true", "TRUE", "True", "yes", "YES", "no", "false", "2", "off", " 1"]) {
		records.push({
			fn: "isTruthyEnvFlag",
			input: value === undefined ? null : value,
			note: "main.ts:95-98. `!value` first (so \"\" and undefined are false), then value === \"1\" || lower === \"true\" || lower === \"yes\". Also used verbatim by core/package-manager.ts:42 and utils/tools-manager.ts:14.",
			result: mainMod.__isTruthyEnvFlag(value),
		});
	}
	records.push({
		fn: "PI_OFFLINE-check-sites",
		note: "The SAME env var is tested three different ways. Only isTruthyEnvFlag's results above are executed values; the other two rows quote Pi's source expression verbatim (executing them would require the whole ModelRuntime / TUI).",
		sites: [
			{
				file: "main.ts:476",
				expression: 'args.includes("--offline") || isTruthyEnvFlag(process.env.PI_OFFLINE)',
				rule: "1/true/yes (case-insensitive)",
				effect: 'sets process.env.PI_OFFLINE = "1" when true',
			},
			{
				file: "core/model-runtime.ts:152",
				expression: "options.allowModelNetwork ?? process.env.PI_OFFLINE === undefined",
				rule: "MERE PRESENCE: PI_OFFLINE=0 and PI_OFFLINE= both still disable model network access",
			},
			{
				file: "modes/interactive/interactive-mode.ts:909 and :1005, utils/version-check.ts:34",
				expression: "if (process.env.PI_OFFLINE)",
				rule: 'JS truthiness: "0" is TRUTHY here, "" is falsy',
			},
		],
	});

	emit("app_mode.cases.jsonl", jsonl(records.map(normalizeDeep)));
	return records;
}

// ===========================================================================
// E. settings.merge.cases.jsonl
// ===========================================================================

/**
 * JSON has no `undefined`, and "key present with value undefined" is
 * behaviourally distinct from "key absent" in deepMergeSettings (the
 * `overrideValue === undefined -> continue` guard, and `{...base}` copying an
 * undefined-valued key). So an undefined VALUE is encoded as
 * `{"$undefined":true}`. Nothing else is re-encoded.
 */
function encUndef(value) {
	if (value === undefined) return { $undefined: true };
	if (value === null || typeof value !== "object") return value;
	if (Array.isArray(value)) return value.map(encUndef);
	const out = {};
	for (const key of Object.keys(value)) out[key] = encUndef(value[key]);
	return out;
}

async function genSettingsCases() {
	const sm = await impPi("core/settings-manager.ts");
	const mainMod = await impPi("main.ts");
	const merge = sm.__deepMergeSettings;
	const records = [];

	const mergeCase = (name, note, global, project) => {
		const result = merge(structuredClone_(global), structuredClone_(project));
		records.push({
			fn: "deepMergeSettings",
			name,
			note,
			global: encUndef(global),
			project: encUndef(project),
			result: encUndef(result),
			resultKeys: Object.keys(result),
		});
	};
	// structuredClone chokes on undefined-valued keys? It does not, but it also
	// cannot be used on functions; a shallow JSON-free clone keeps undefined.
	function structuredClone_(v) {
		if (v === null || typeof v !== "object") return v;
		if (Array.isArray(v)) return v.map(structuredClone_);
		const out = {};
		for (const k of Object.keys(v)) out[k] = structuredClone_(v[k]);
		return out;
	}

	// ---- shape of the merge -------------------------------------------------
	mergeCase("both-empty", "nothing to merge", {}, {});
	mergeCase("disjoint-keys", "keys from both scopes survive; base keys come FIRST because result starts as {...base}", { defaultProvider: "google" }, { theme: "dark" });
	mergeCase("project-overrides-scalar", "project wins for a plain scalar; the key keeps its BASE position", { defaultModel: "a", theme: "x" }, { defaultModel: "b" });
	mergeCase("project-only", "empty base", {}, { quietStartup: true });
	mergeCase("global-only", "empty override", { quietStartup: true }, {});

	// ---- ONE LEVEL DEEP ONLY (the doc comment says "recursively"; it is not) --
	mergeCase(
		"nested-terminal-partial-override",
		"terminal is merged with a single object spread: base keys survive alongside the override's",
		{ terminal: { showImages: false, imageWidthCells: 40, clearOnShrink: true } },
		{ terminal: { imageWidthCells: 80 } },
	);
	mergeCase("nested-images-partial-override", "`images` (the ImageSettings field) merges the same way", { images: { autoResize: false, blockImages: true } }, { images: { blockImages: false } });
	mergeCase("nested-markdown-partial-override", "markdown.codeBlockIndent", { markdown: { codeBlockIndent: "    " } }, { markdown: {} });
	mergeCase("nested-warnings-partial-override", "warnings.anthropicExtraUsage", { warnings: { anthropicExtraUsage: true } }, { warnings: { anthropicExtraUsage: false } });
	mergeCase(
		"CRITICAL-two-levels-deep-is-NOT-merged",
		"the merge is `{...baseValue, ...overrideValue}` - a SINGLE spread, not a recursive call. retry.provider is therefore REPLACED wholesale and base's retry.provider.timeoutMs is LOST. A recursive implementation would keep it; this is the case that distinguishes shallow from deep.",
		{ retry: { enabled: true, maxRetries: 5, provider: { timeoutMs: 1000, maxRetries: 2 } } },
		{ retry: { provider: { maxRetries: 9 } } },
	);
	mergeCase(
		"nested-object-only-in-project",
		"baseValue is undefined, so the object-merge guard fails and the override object is taken AS IS",
		{ theme: "dark" },
		{ terminal: { showImages: false } },
	);
	mergeCase("nested-object-over-scalar", "typeof baseValue is not object -> else branch -> the override object replaces the scalar", { terminal: "nope" }, { terminal: { showImages: true } });
	mergeCase("scalar-over-nested-object", "typeof overrideValue is not object -> else branch -> the scalar replaces the object", { terminal: { showImages: true } }, { terminal: "nope" });

	// ---- ARRAYS: REPLACED, never concatenated -------------------------------
	mergeCase(
		"CRITICAL-array-in-both-is-REPLACED",
		"both Array.isArray guards exclude arrays from the object-merge branch, so the override array wins WHOLESALE. It is not concatenated, not element-merged, and not deduplicated.",
		{ extensions: ["/g/a.ts", "/g/b.ts"] },
		{ extensions: ["/p/c.ts"] },
	);
	mergeCase("array-only-in-project", "no base value at all", { theme: "dark" }, { skills: ["/p/skills"] });
	mergeCase("array-only-in-global", "project has no opinion, base array survives", { skills: ["/g/skills"] }, {});
	mergeCase("empty-array-in-project-clears-the-list", "[] is not undefined, so it replaces a non-empty base array", { themes: ["/g/t"] }, { themes: [] });
	mergeCase("array-over-object", "override array replaces a base object", { skills: { enableSkillCommands: true } }, { skills: ["/p/s"] });
	mergeCase("object-over-array", "override object replaces a base array", { skills: ["/g/s"] }, { skills: { enableSkillCommands: true } });
	mergeCase("array-of-objects-packages", "packages entries are never merged element-wise", { packages: [{ source: "npm:a", autoload: false }] }, { packages: ["npm:b"] });
	mergeCase("npmCommand-array-replaced", "argv-style arrays behave like any other array", { npmCommand: ["npm"] }, { npmCommand: ["mise", "exec", "node@20", "--", "npm"] });

	// ---- null vs absent vs undefined ---------------------------------------
	mergeCase("null-in-project-OVERWRITES", "null is not undefined, and the object-merge guard rejects it, so the else branch assigns null", { theme: "dark" }, { theme: null });
	mergeCase("undefined-in-project-is-SKIPPED", "`overrideValue === undefined -> continue`, so the base value survives", { theme: "dark" }, { theme: undefined });
	mergeCase("absent-in-project-keeps-base", "the key is simply not iterated", { theme: "dark" }, {});
	mergeCase("undefined-in-project-with-no-base-key-STILL-SKIPPED", "continue happens before any assignment, so the key does not appear in the result at all", {}, { theme: undefined });
	mergeCase("undefined-in-global-is-COPIED-BY-THE-SPREAD", "`{...base}` copies a key whose value is undefined, so it IS present in resultKeys", { theme: undefined, defaultModel: "a" }, {});
	mergeCase("undefined-in-global-overridden-by-project", "the override replaces the undefined base value", { theme: undefined }, { theme: "dark" });
	mergeCase("null-in-global-merged-with-object-in-project", "baseValue is null, so the object-merge guard fails and the override object is taken as is", { terminal: null }, { terminal: { showImages: true } });
	mergeCase("null-in-global-kept-when-project-absent", "null survives the spread", { terminal: null }, {});
	mergeCase("nested-undefined-value-inside-an-object-merge", "the inner spread does NOT skip undefined: terminal.showImages becomes undefined", { terminal: { showImages: true, clearOnShrink: true } }, { terminal: { showImages: undefined } });

	// ---- migrateSettings ----------------------------------------------------
	const migrateCase = (name, note, input) => {
		const cloned = structuredClone_(input);
		let result;
		let error;
		try {
			result = sm.SettingsManager.migrateSettings(cloned);
		} catch (err) {
			error = err instanceof Error ? `${err.name}: ${err.message}` : String(err);
		}
		records.push({
			fn: "migrateSettings",
			name,
			note,
			input: encUndef(input),
			ok: error === undefined,
			...(error === undefined
				? { result: encUndef(result), resultKeys: Array.isArray(result) ? null : Object.keys(result) }
				: { error, v8Dependent: true }),
			mutatesInputInPlace: error === undefined ? result === cloned : null,
		});
	};

	migrateCase("legacy:queueMode-to-steeringMode", "renamed and the old key deleted; the NEW key lands at the END of the key order because it is assigned after iteration started", {
		queueMode: "all",
		theme: "dark",
	});
	migrateCase("legacy:queueMode-ignored-when-steeringMode-present", "`!(\"steeringMode\" in settings)` fails, so queueMode is NOT renamed and NOT deleted", {
		queueMode: "all",
		steeringMode: "one-at-a-time",
	});
	migrateCase("legacy:websockets-true-to-transport-websocket", "boolean true -> \"websocket\"", { websockets: true });
	migrateCase("legacy:websockets-false-to-transport-sse", "boolean false -> \"sse\"", { websockets: false });
	migrateCase("legacy:websockets-ignored-when-transport-present", "transport already set -> websockets left in place", { transport: "auto", websockets: true });
	migrateCase("legacy:websockets-non-boolean-left-alone", "the guard is `typeof === \"boolean\"`", { websockets: "yes" });
	migrateCase("legacy:skills-object-with-customDirectories", "the object form becomes the array form and enableSkillCommands is hoisted", {
		skills: { enableSkillCommands: false, customDirectories: ["/a", "/b"] },
	});
	migrateCase("legacy:skills-object-with-empty-customDirectories-deletes-skills", "an empty array takes the `delete settings.skills` branch", {
		skills: { enableSkillCommands: true, customDirectories: [] },
	});
	migrateCase("legacy:skills-object-without-customDirectories-deletes-skills", "no array at all -> skills deleted, enableSkillCommands still hoisted", {
		skills: { enableSkillCommands: true },
	});
	migrateCase("legacy:skills-object-does-not-clobber-existing-enableSkillCommands", "the hoist is guarded on `settings.enableSkillCommands === undefined`", {
		enableSkillCommands: false,
		skills: { enableSkillCommands: true, customDirectories: ["/a"] },
	});
	migrateCase("legacy:skills-array-untouched", "Array.isArray short-circuits the whole block", { skills: ["/a"] });
	migrateCase("legacy:retry-maxDelayMs-to-provider-maxRetryDelayMs", "moved into retry.provider and the old key deleted", { retry: { enabled: true, maxDelayMs: 1234 } });
	migrateCase("legacy:retry-maxDelayMs-merges-into-an-existing-provider", "the existing provider object is spread first", {
		retry: { maxDelayMs: 1234, provider: { timeoutMs: 5 } },
	});
	migrateCase("legacy:retry-maxDelayMs-NOT-moved-when-maxRetryDelayMs-set", "the guard fails, but `delete retrySettings.maxDelayMs` runs unconditionally, so the legacy value is silently DROPPED", {
		retry: { maxDelayMs: 1234, provider: { maxRetryDelayMs: 999 } },
	});
	migrateCase("legacy:retry-maxDelayMs-moved-when-maxRetryDelayMs-is-null", "null is explicitly treated like undefined", {
		retry: { maxDelayMs: 1234, provider: { maxRetryDelayMs: null } },
	});
	migrateCase("legacy:retry-without-maxDelayMs-still-deletes-the-key", "the delete is unconditional inside the retry block; the object is otherwise unchanged", {
		retry: { enabled: false },
	});
	migrateCase("legacy:retry-non-object-skipped", "the retry block is guarded on typeof object && !Array.isArray", { retry: [1] });
	migrateCase("legacy:all-four-migrations-at-once", "the full legacy shape; pins the resulting key ORDER too", {
		queueMode: "all",
		websockets: true,
		skills: { enableSkillCommands: false, customDirectories: ["/a"] },
		retry: { enabled: true, maxDelayMs: 60000 },
		theme: "dark",
	});
	migrateCase("modern:no-op", "a settings object with nothing legacy in it is returned unchanged", { theme: "dark", terminal: { showImages: true } });
	migrateCase("edge:empty-object", "nothing to do", {});

	// ---- malformed settings files -------------------------------------------
	// InMemorySettingsStorage keeps this entirely off the filesystem.
	const malformedCase = (name, note, globalRaw, projectRaw, options = {}) => {
		const storage = new sm.InMemorySettingsStorage();
		if (globalRaw !== undefined) storage.withLock("global", () => globalRaw);
		if (projectRaw !== undefined) storage.withLock("project", () => projectRaw);
		const manager = sm.SettingsManager.fromStorage(storage, options);
		const diagnostics = mainMod.__collectSettingsDiagnostics(manager, "startup");
		// drainErrors() was already consumed by collectSettingsDiagnostics, so a
		// second manager is built to observe the raw SettingsError[] shape.
		const storage2 = new sm.InMemorySettingsStorage();
		if (globalRaw !== undefined) storage2.withLock("global", () => globalRaw);
		if (projectRaw !== undefined) storage2.withLock("project", () => projectRaw);
		const manager2 = sm.SettingsManager.fromStorage(storage2, options);
		const raw = manager2.drainErrors().map((e) => ({ scope: e.scope, name: e.error.name, message: e.error.message }));
		records.push({
			fn: "SettingsManager.fromStorage",
			name,
			note,
			globalRaw: globalRaw ?? null,
			projectRaw: projectRaw ?? null,
			options,
			thrown: false,
			globalSettings: encUndef(manager.getGlobalSettings()),
			projectSettings: encUndef(manager.getProjectSettings()),
			mergedSettings: encUndef(manager.settings),
			errors: raw,
			diagnostics,
			drainErrorsIsIdempotent: manager2.drainErrors().length === 0,
			v8Dependent: raw.length > 0,
		});
	};

	malformedCase("malformed:global-unparseable-json", "the parse error is CAUGHT, the scope falls back to {}, and a SettingsError is queued. drainErrors() is what main.ts turns into a `Warning: (startup, global settings) ...` line. The message wording comes from V8, not from Pi.", "{ not json", undefined);
	malformedCase("malformed:project-unparseable-json", "same, for the project scope", '{"theme":"dark"}', "oops");
	malformedCase("malformed:both-scopes-unparseable", "two diagnostics, global first (that is the order fromStorage pushes them)", "{", "}");
	malformedCase("malformed:global-truncated-object", "a plausible half-written file", '{"theme": "dar', undefined);
	malformedCase("malformed:global-is-the-literal-null", '"null" is a truthy string so it is parsed, yielding null, and migrateSettings\' `"queueMode" in settings` throws a TypeError', "null", undefined);
	malformedCase("malformed:global-is-a-number", "JSON.parse returns 123 and the `in` operator throws", "123", undefined);
	malformedCase("malformed:global-is-a-json-array", "an array IS an object, so migrateSettings passes it straight through and deepMergeSettings iterates zero keys", "[]", undefined);
	malformedCase("malformed:global-is-a-quoted-string", '"\\"hi\\"" parses to a string; `in` throws on a primitive', '"hi"', undefined);
	malformedCase("edge:global-is-the-empty-string", "`if (!content) return {}` short-circuits before JSON.parse, so this is NOT an error", "", undefined);
	malformedCase("edge:global-is-an-empty-object", "the happy path", "{}", undefined);
	malformedCase("edge:project-untrusted-is-not-even-read", "`scope === \"project\" && !projectTrusted -> return {}`, so even unparseable project JSON produces NO diagnostic", '{"theme":"dark"}', "{ not json", { projectTrusted: false });
	malformedCase("edge:legacy-shape-is-migrated-on-load", "loadFromStorage runs migrateSettings, so a legacy file is normalised before the merge", '{"queueMode":"all","websockets":true}', '{"retry":{"maxDelayMs":5}}');

	emit("settings.merge.cases.jsonl", jsonl(records.map(normalizeDeep)));
	return records;
}

// ===========================================================================
// F. migrations.cases.jsonl
// ===========================================================================

/**
 * All five migrations plus `runMigrations`, each against a freshly mkdtemp'd
 * agent dir (and, where relevant, a mkdtemp'd project cwd) in the exact
 * pre-migration state. PI_CODING_AGENT_DIR points at the temp agent dir for the
 * duration of every call, so the real ~/.pi is never read or written.
 *
 * Three of the five are module-private in migrations.ts and are reached through
 * the load hook's appended export list, NOT reimplemented:
 * migrateToolsToBin, migrateKeybindingsConfigFile, migrateExtensionSystem.
 *
 * `warnings` holds the values a migration RETURNS; `console` holds every line it
 * printed, in order, with colour disabled (NO_COLOR=1) so the text is the
 * message and not an escape soup. chalk.green / chalk.yellow are the only
 * styling migrations.ts applies.
 */
async function genMigrationCases() {
	const migrations = await impPi("migrations.ts");
	const records = [];

	/**
	 * @param {{name:string,note:string,fn:string,agent?:Record<string,string>,project?:Record<string,string>,
	 *          agentDirs?:string[],projectDirs?:string[],needsProject?:boolean}} spec
	 */
	function runCase(spec) {
		const label = spec.name.replace(/[^a-z0-9]+/gi, "-").slice(0, 40);
		const agentDir = newAgentDir(label);
		const needsProject = spec.needsProject || spec.project || spec.projectDirs;
		const projectDir = needsProject ? newProjectDir(label) : null;

		for (const dir of spec.agentDirs ?? []) mkdirSync(join(agentDir, ...dir.split("/")), { recursive: true });
		for (const [rel, contents] of Object.entries(spec.agent ?? {})) put(agentDir, rel, contents);
		if (projectDir) {
			for (const dir of spec.projectDirs ?? []) mkdirSync(join(projectDir, ...dir.split("/")), { recursive: true });
			for (const [rel, contents] of Object.entries(spec.project ?? {})) put(projectDir, rel, contents);
		}

		const before = { agent: snapshotTree(agentDir), project: projectDir ? snapshotTree(projectDir) : null };
		const invoke = () => {
			switch (spec.fn) {
				case "migrateAuthToAuthJson":
					return migrations.migrateAuthToAuthJson();
				case "migrateSessionsFromAgentRoot":
					return migrations.migrateSessionsFromAgentRoot();
				case "migrateToolsToBin":
					return migrations.__migrateToolsToBin();
				case "migrateKeybindingsConfigFile":
					return migrations.__migrateKeybindingsConfigFile();
				case "migrateExtensionSystem":
					return migrations.__migrateExtensionSystem(projectDir);
				case "runMigrations":
					return migrations.runMigrations(projectDir);
				default:
					throw new Error(`unknown migration ${spec.fn}`);
			}
		};
		let returned;
		let error;
		let consoleOut = [];
		try {
			const captured = captureConsole(() => withAgentDir(agentDir, invoke));
			returned = captured.result;
			consoleOut = captured.console;
		} catch (err) {
			error = err instanceof Error ? `${err.name}: ${err.message}` : String(err);
		}

		records.push({
			fn: spec.fn,
			name: spec.name,
			note: spec.note,
			platform: PLATFORM,
			modeMeaningful: MODE_MEANINGFUL,
			...(spec.platformDependent ? { platformDependent: spec.platformDependent } : {}),
			cwd: projectDir ? "{PROJECTDIR}" : null,
			before,
			returned: returned === undefined ? null : returned,
			...(error === undefined ? {} : { error }),
			after: { agent: snapshotTree(agentDir), project: projectDir ? snapshotTree(projectDir) : null },
			console: consoleOut,
		});
	}

	// -- M1 migrateAuthToAuthJson ---------------------------------------------
	const OAUTH_ONE = `${JSON.stringify({ anthropic: { access: "at-1", refresh: "rt-1", expires: 1730000000000 } }, null, 2)}\n`;
	runCase({
		fn: "migrateAuthToAuthJson",
		name: "M1:oauth-json-only",
		note: "oauth.json entries get `type: \"oauth\"` PREPENDED (spread after), the file is renamed to oauth.json.migrated, and auth.json is written with JSON.stringify(x, null, 2) and NO trailing newline, mode 0600",
		agent: { "oauth.json": OAUTH_ONE },
	});
	runCase({
		fn: "migrateAuthToAuthJson",
		name: "M1:settings-apiKeys-only",
		note: "settings.apiKeys becomes {type:\"api_key\", key}. settings.json is REWRITTEN without apiKeys, also with JSON.stringify(x, null, 2) and no trailing newline - key order of the surviving fields is preserved.",
		agent: { "settings.json": `${JSON.stringify({ theme: "dark", apiKeys: { openai: "sk-oracle-1" }, quietStartup: true }, null, 2)}\n` },
	});
	runCase({
		fn: "migrateAuthToAuthJson",
		name: "M1:both-sources-oauth-wins-for-the-same-provider",
		note: "the `!migrated[provider]` guard means an oauth entry is never overwritten by a settings apiKey for the same provider; the returned providers array is oauth-first then settings order",
		agent: {
			"oauth.json": OAUTH_ONE,
			"settings.json": `${JSON.stringify({ apiKeys: { anthropic: "sk-should-be-ignored", openai: "sk-oracle-2" } }, null, 2)}\n`,
		},
	});
	runCase({
		fn: "migrateAuthToAuthJson",
		name: "M1:already-migrated-noop",
		note: "GUARD: auth.json exists, so the function returns [] immediately - oauth.json is NOT renamed and settings.apiKeys is NOT stripped",
		agent: {
			"auth.json": `${JSON.stringify({ anthropic: { type: "api_key", key: "existing" } }, null, 2)}`,
			"oauth.json": OAUTH_ONE,
			"settings.json": `${JSON.stringify({ apiKeys: { openai: "sk-x" } }, null, 2)}\n`,
		},
	});
	runCase({
		fn: "migrateAuthToAuthJson",
		name: "M1:malformed-oauth-json-is-skipped-silently",
		note: "the JSON.parse throw is swallowed, so oauth.json is NOT renamed and nothing is written; no diagnostic reaches the user",
		agent: { "oauth.json": "{ not json" },
	});
	runCase({
		fn: "migrateAuthToAuthJson",
		name: "M1:malformed-settings-json-is-skipped-silently",
		note: "same swallow for settings.json; the file is left byte-identical",
		agent: { "settings.json": "{ not json" },
	});
	runCase({
		fn: "migrateAuthToAuthJson",
		name: "M1:settings-without-apiKeys-is-not-rewritten",
		note: "the rewrite only happens inside `if (settings.apiKeys && typeof ... === \"object\")`, so the file keeps its original bytes INCLUDING its trailing newline",
		agent: { "settings.json": `${JSON.stringify({ theme: "dark" }, null, 2)}\n` },
	});
	runCase({
		fn: "migrateAuthToAuthJson",
		name: "M1:non-string-apiKey-values-are-dropped-but-apiKeys-is-still-deleted",
		note: "EDGE: `typeof key === \"string\"` filters the entry out, yet `delete settings.apiKeys` + rewrite run unconditionally, so the value is silently LOST and no auth.json is created",
		agent: { "settings.json": `${JSON.stringify({ apiKeys: { openai: 12345, azure: null } }, null, 2)}\n` },
	});
	runCase({
		fn: "migrateAuthToAuthJson",
		name: "M1:empty-oauth-json-still-gets-renamed",
		note: "EDGE: `{}` parses fine, so renameSync runs, but `Object.keys(migrated).length > 0` is false so NO auth.json is written - the user ends up with only oauth.json.migrated",
		agent: { "oauth.json": "{}\n" },
	});
	runCase({
		fn: "migrateAuthToAuthJson",
		name: "M1:nothing-to-migrate",
		note: "empty agent dir: returns [], writes nothing",
	});

	// -- M2 migrateSessionsFromAgentRoot --------------------------------------
	const sessionHeader = (cwd, id) => `${JSON.stringify({ type: "session", cwd, version: 2, id })}\n{"type":"message"}\n`;
	runCase({
		fn: "migrateSessionsFromAgentRoot",
		name: "M2:single-session-in-agent-root",
		note: "the encoded sessions/<name>/ directory is created; on win32 the rename then FAILS silently (see platformDependent) so the .jsonl stays put",
		platformDependent: "m2-filename-split",
		agent: { "a.jsonl": sessionHeader("/home/user/proj", "0198c0de-0000-4000-8000-00000000000a") },
	});
	runCase({
		fn: "migrateSessionsFromAgentRoot",
		name: "M2:two-sessions-different-cwds",
		note: "one encoded directory per distinct cwd",
		platformDependent: "m2-filename-split",
		agent: {
			"a.jsonl": sessionHeader("/home/user/one", "0198c0de-0000-4000-8000-00000000000a"),
			"b.jsonl": sessionHeader("/home/user/two", "0198c0de-0000-4000-8000-00000000000b"),
		},
	});
	runCase({
		fn: "migrateSessionsFromAgentRoot",
		name: "M2:no-jsonl-files-noop",
		note: "GUARD: `files.length === 0` returns before any mkdir",
		agent: { "settings.json": "{}\n" },
	});
	runCase({
		fn: "migrateSessionsFromAgentRoot",
		name: "M2:already-migrated-noop",
		note: "GUARD: the session already lives under sessions/<encoded>/, so readdirSync of the agent ROOT finds no .jsonl and the function returns early",
		platformDependent: "m2-filename-split",
		agent: { "sessions/--home-user-proj--/a.jsonl": sessionHeader("/home/user/proj", "0198c0de-0000-4000-8000-00000000000a") },
	});
	runCase({
		fn: "migrateSessionsFromAgentRoot",
		name: "M2:target-already-exists-is-skipped",
		note: "GUARD: `if (existsSync(newPath)) continue` - the root copy is left in place and the existing target is not overwritten",
		platformDependent: "m2-filename-split",
		agent: {
			"a.jsonl": sessionHeader("/home/user/proj", "0198c0de-0000-4000-8000-00000000000a"),
			"sessions/--home-user-proj--/a.jsonl": "PRE-EXISTING\n",
		},
	});
	runCase({
		fn: "migrateSessionsFromAgentRoot",
		name: "M2:missing-agent-dir-noop",
		note: "GUARD: readdirSync throws ENOENT into the catch and the function returns. (The dir is created by the harness, so this row instead pins that a dir containing only subdirectories is a no-op.)",
		agentDirs: ["sessions"],
	});

	// -- M3 migrateToolsToBin -------------------------------------------------
	runCase({
		fn: "migrateToolsToBin",
		name: "M3:moves-fd-and-rg-and-prints",
		note: "bin/ is created lazily; only the four names fd, rg, fd.exe, rg.exe are considered, in that order. Anything else in tools/ is left behind. Prints exactly one green line.",
		agent: { "tools/rg.exe": "RG-BINARY\n", "tools/fd.exe": "FD-BINARY\n", "tools/custom-tool.ts": "export default 1;\n" },
	});
	runCase({
		fn: "migrateToolsToBin",
		name: "M3:moves-extensionless-names-too",
		note: "the POSIX names are checked before the .exe ones",
		agent: { "tools/fd": "FD\n", "tools/rg": "RG\n" },
	});
	runCase({
		fn: "migrateToolsToBin",
		name: "M3:target-exists-deletes-the-old-copy-and-does-NOT-print",
		note: "when bin/<name> already exists the tools/ copy is rmSync'd, movedAny stays false, so NO console line is emitted and bin/ keeps ITS bytes",
		agent: { "tools/rg.exe": "OLD-RG\n", "bin/rg.exe": "NEW-RG\n" },
	});
	runCase({
		fn: "migrateToolsToBin",
		name: "M3:mixed-move-and-delete-still-prints",
		note: "one move is enough to set movedAny, so the line is printed even though the other binary was merely deleted",
		agent: { "tools/rg.exe": "OLD-RG\n", "tools/fd.exe": "FD\n", "bin/rg.exe": "NEW-RG\n" },
	});
	runCase({
		fn: "migrateToolsToBin",
		name: "M3:no-tools-dir-noop",
		note: "GUARD: `if (!existsSync(toolsDir)) return` - bin/ is not even created",
		agent: { "settings.json": "{}\n" },
	});
	runCase({
		fn: "migrateToolsToBin",
		name: "M3:tools-dir-without-managed-binaries-noop",
		note: "tools/ exists but holds nothing named fd/rg: no bin/ is created and nothing is printed",
		agent: { "tools/custom-tool.ts": "export default 1;\n" },
	});
	runCase({
		fn: "migrateToolsToBin",
		name: "M3:already-migrated-noop",
		note: "GUARD: no tools/ dir at all, bin/ already populated",
		agent: { "bin/rg.exe": "RG\n", "bin/fd.exe": "FD\n" },
	});
	runCase({
		fn: "migrateToolsToBin",
		name: "M3:case-sensitivity-of-the-binary-names",
		note: "the move list is compared EXACTLY (unlike checkDeprecatedExtensionDirs, which lowercases), so tools/RG.EXE is NOT moved",
		agent: { "tools/RG.EXE": "RG\n" },
	});

	// -- M4 migrateKeybindingsConfigFile --------------------------------------
	runCase({
		fn: "migrateKeybindingsConfigFile",
		name: "M4:legacy-names-renamed-and-reordered",
		note: "legacy camelCase ids map to dotted ids, then orderKeybindingsConfig re-emits them in KEYBINDINGS declaration order with unknown keys sorted at the end. The file is rewritten as JSON.stringify(config, null, 2) + \"\\n\".",
		agent: {
			"keybindings.json": `${JSON.stringify({ zzzUnknownCustom: "f9", cycleModelForward: "ctrl+p", interrupt: "ctrl+c", aaaUnknownCustom: "f8" }, null, 2)}\n`,
		},
	});
	runCase({
		fn: "migrateKeybindingsConfigFile",
		name: "M4:array-valued-bindings-are-preserved",
		note: "values are copied verbatim; only KEYS are migrated",
		agent: { "keybindings.json": `${JSON.stringify({ cycleModelForward: ["ctrl+p", "f2"] }, null, 2)}\n` },
	});
	runCase({
		fn: "migrateKeybindingsConfigFile",
		name: "M4:no-legacy-names-means-NO-WRITE",
		note: "`if (!migrated) return` - the file keeps its exact original bytes, including its non-canonical key order and its lack of a trailing newline",
		agent: { "keybindings.json": '{"app.model.cycleForward":"ctrl+p","app.interrupt":"ctrl+c"}' },
	});
	runCase({
		fn: "migrateKeybindingsConfigFile",
		name: "M4:legacy-and-new-key-both-present-legacy-is-DROPPED",
		note: "`if (key !== nextKey && Object.hasOwn(rawConfig, nextKey)) continue` - the new key's value wins and migrated is still true, so the file IS rewritten",
		agent: { "keybindings.json": `${JSON.stringify({ interrupt: "ctrl+legacy", "app.interrupt": "ctrl+new" }, null, 2)}\n` },
	});
	runCase({
		fn: "migrateKeybindingsConfigFile",
		name: "M4:malformed-json-untouched",
		note: "the parse throw is swallowed; bytes unchanged",
		agent: { "keybindings.json": "{ not json" },
	});
	runCase({
		fn: "migrateKeybindingsConfigFile",
		name: "M4:top-level-array-untouched",
		note: "GUARD: `Array.isArray(parsed) -> return` before migrateKeybindingsConfig is called",
		agent: { "keybindings.json": '["nope"]' },
	});
	runCase({
		fn: "migrateKeybindingsConfigFile",
		name: "M4:top-level-null-untouched",
		note: "GUARD: `parsed === null -> return`",
		agent: { "keybindings.json": "null" },
	});
	runCase({
		fn: "migrateKeybindingsConfigFile",
		name: "M4:missing-file-noop",
		note: "GUARD: `if (!existsSync(configPath)) return`",
		agent: { "settings.json": "{}\n" },
	});

	// -- M5 migrateExtensionSystem --------------------------------------------
	runCase({
		fn: "migrateExtensionSystem",
		name: "M5:global-commands-to-prompts",
		note: "renameSync of the whole directory + one green line; the label is \"Global\" for the agent dir",
		agent: { "commands/hello.md": "# hello\n" },
		needsProject: true,
	});
	runCase({
		fn: "migrateExtensionSystem",
		name: "M5:project-commands-to-prompts",
		note: "the project dir is `join(cwd, CONFIG_DIR_NAME)`, i.e. <cwd>/.pi; the label is \"Project\"",
		project: { ".pi/commands/hello.md": "# hello\n" },
	});
	runCase({
		fn: "migrateExtensionSystem",
		name: "M5:both-scopes-global-line-first",
		note: "migrateCommandsToPrompts is called for the agent dir before the project dir, so the two lines are ordered Global then Project",
		agent: { "commands/g.md": "g\n" },
		project: { ".pi/commands/p.md": "p\n" },
	});
	runCase({
		fn: "migrateExtensionSystem",
		name: "M5:prompts-already-exists-no-rename",
		note: "GUARD: `existsSync(commandsDir) && !existsSync(promptsDir)` - commands/ is LEFT BEHIND alongside prompts/ and nothing is printed",
		agent: { "commands/old.md": "old\n", "prompts/new.md": "new\n" },
		needsProject: true,
	});
	runCase({
		fn: "migrateExtensionSystem",
		name: "M5:global-hooks-warning",
		note: "hooks/ is never moved, only reported; the warning text is returned, not printed",
		agent: { "hooks/h.ts": "export default 1;\n" },
		needsProject: true,
	});
	runCase({
		fn: "migrateExtensionSystem",
		name: "M5:project-hooks-warning",
		note: "same check against <cwd>/.pi",
		project: { ".pi/hooks/h.ts": "export default 1;\n" },
	});
	runCase({
		fn: "migrateExtensionSystem",
		name: "M5:global-tools-with-custom-tools-warning",
		note: "tools/ only warns when it holds something that is not fd/rg/fd.exe/rg.exe and does not start with a dot",
		agent: { "tools/custom.ts": "export default 1;\n" },
		needsProject: true,
	});
	runCase({
		fn: "migrateExtensionSystem",
		name: "M5:global-tools-only-managed-binaries-and-dotfiles-no-warning",
		note: "the filter lowercases each entry and also drops anything starting with \".\", so RG.EXE and .DS_Store are both ignored",
		agent: { "tools/RG.EXE": "rg\n", "tools/fd": "fd\n", "tools/.DS_Store": "junk\n" },
		needsProject: true,
	});
	runCase({
		fn: "migrateExtensionSystem",
		name: "M5:warning-order-global-then-project-hooks-then-tools",
		note: "checkDeprecatedExtensionDirs pushes hooks before tools, and the global call precedes the project call, so the four warnings come out in exactly this order",
		agent: { "hooks/h.ts": "h\n", "tools/custom.ts": "c\n" },
		project: { ".pi/hooks/h.ts": "h\n", ".pi/tools/custom.ts": "c\n" },
	});
	runCase({
		fn: "migrateExtensionSystem",
		name: "M5:clean-dirs-noop",
		note: "no commands/, hooks/ or tools/ anywhere: no warnings, no output",
		agent: { "settings.json": "{}\n" },
		needsProject: true,
	});

	// -- runMigrations end-to-end ---------------------------------------------
	runCase({
		fn: "runMigrations",
		name: "runMigrations:needs-several-ORDER-OF-EFFECTS",
		note: "call order is fixed: migrateAuthToAuthJson, migrateSessionsFromAgentRoot, migrateToolsToBin, migrateKeybindingsConfigFile, migrateExtensionSystem. The `console` array therefore proves the ordering: the tools/->bin/ line is printed BEFORE the commands/->prompts/ lines, and the deprecation warnings are RETURNED (not printed) so they come last of all.",
		agent: {
			"oauth.json": OAUTH_ONE,
			"settings.json": `${JSON.stringify({ theme: "dark", apiKeys: { openai: "sk-oracle-3" } }, null, 2)}\n`,
			"a.jsonl": sessionHeader("/home/user/proj", "0198c0de-0000-4000-8000-00000000000a"),
			"tools/rg.exe": "RG\n",
			"tools/custom.ts": "export default 1;\n",
			"keybindings.json": `${JSON.stringify({ cycleModelForward: "ctrl+p" }, null, 2)}\n`,
			"commands/g.md": "g\n",
			"hooks/h.ts": "h\n",
		},
		project: { ".pi/commands/p.md": "p\n", ".pi/hooks/h.ts": "h\n" },
		platformDependent: "m2-filename-split",
	});
	runCase({
		fn: "runMigrations",
		name: "runMigrations:already-migrated-noop",
		note: "every guard trips: auth.json exists, no root .jsonl, no tools/, keybindings already dotted, prompts/ instead of commands/, no hooks/. Nothing is written, nothing is printed, both returned arrays are empty.",
		agent: {
			"auth.json": `${JSON.stringify({ anthropic: { type: "api_key", key: "k" } }, null, 2)}`,
			"sessions/--home-user-proj--/a.jsonl": sessionHeader("/home/user/proj", "0198c0de-0000-4000-8000-00000000000a"),
			"bin/rg.exe": "RG\n",
			"keybindings.json": `${JSON.stringify({ "app.model.cycleForward": "ctrl+p" }, null, 2)}\n`,
			"prompts/g.md": "g\n",
		},
		project: { ".pi/prompts/p.md": "p\n" },
	});
	runCase({
		fn: "runMigrations",
		name: "runMigrations:empty-agent-dir",
		note: "the coldest possible start: an existing but empty agent dir and an empty cwd",
		needsProject: true,
	});

	// -- showDeprecationWarnings (captured in a child; it blocks on a keypress) --
	const { spawnSync } = await import("node:child_process");
	const dep = spawnSync(process.execPath, [fileURLToPath(import.meta.url), "--emit-help", "deprecation"], {
		env: { ...process.env, NO_COLOR: "1" },
		input: Buffer.from("x", "utf-8"),
		maxBuffer: 1024 * 1024,
	});
	records.push({
		fn: "showDeprecationWarnings",
		name: "showDeprecationWarnings:two-warnings",
		note: "captured in a CHILD process with a byte on stdin, because the real function awaits a keypress (process.stdin.once(\"data\")). Colour disabled with NO_COLOR=1; chalk.yellow wraps the warning lines and chalk.dim the prompt. The final console.log() emits a bare newline AFTER the keypress.",
		platform: PLATFORM,
		input: ["first warning text", "second warning text"],
		exitStatus: dep.status,
		stdout: dep.stdout.toString("utf-8"),
		stderr: dep.stderr.toString("utf-8"),
	});

	emit("migrations.cases.jsonl", jsonl(records.map(normalizeDeep)));
	return records;
}

// ===========================================================================
// I. models.cases.jsonl  (part 1: the pure resolution chain)
// ===========================================================================

/**
 * A SMALL, FULLY DETERMINISTIC model list, authored here as the INPUT to Pi's
 * real matching functions. Every `result` in the `catalogSource: "synthetic"`
 * records is produced by executing Pi's own parseModelPattern /
 * findExactModelReferenceMatch / resolveCliModel / findInitialModel; only the
 * input list is ours, exactly as the argv arrays in args.corpus.jsonl are ours.
 *
 * Why not the builtin catalog: it holds 1000+ generated models, and the branches
 * that matter (cross-provider ambiguity, alias-vs-dated tie-breaks,
 * provider-name-vs-slash-in-id conflicts) cannot be constructed from it
 * reliably. A handful of records DO run against the real builtin catalog and are
 * marked `catalogSource: "builtin"`.
 *
 * The literals carry only the fields the matching code reads or copies:
 * provider, id, name, api, baseUrl, reasoning, input, contextWindow, maxTokens,
 * cost. The array is emitted into the fixture as `syntheticCatalog` so a Rust
 * test can rebuild it byte-identically.
 */
function buildSyntheticCatalog() {
	const M = (provider, id, extra = {}) => ({
		provider,
		id,
		name: extra.name ?? id,
		api: extra.api ?? "anthropic-messages",
		baseUrl: extra.baseUrl ?? "https://api.example.test",
		reasoning: extra.reasoning ?? false,
		input: extra.input ?? ["text"],
		contextWindow: extra.contextWindow ?? 200000,
		maxTokens: extra.maxTokens ?? 64000,
		cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
	});
	return [
		// defaultModelPerProvider["anthropic"] - the buildFallbackModel base and
		// the model findInitialModel's step-4 scan looks for.
		M("anthropic", "claude-opus-4-8", { name: "Claude Opus 4.8", reasoning: true, contextWindow: 1000000, maxTokens: 128000, input: ["text", "image"] }),
		M("anthropic", "claude-sonnet-4-5", { name: "Claude Sonnet 4.5", reasoning: true, input: ["text", "image"] }),
		M("anthropic", "claude-sonnet-4-5-20250929", { name: "Claude Sonnet 4.5 (2025-09-29)", reasoning: true }),
		M("anthropic", "claude-sonnet-4-5-20250101", { name: "Claude Sonnet 4.5 (2025-01-01)", reasoning: true }),
		M("anthropic", "claude-haiku-4-5-latest", { name: "Claude Haiku 4.5 Latest" }),
		M("anthropic", "claude-haiku-4-5-20251001", { name: "Claude Haiku 4.5 (2025-10-01)" }),
		M("openai", "gpt-5.5", { name: "GPT 5.5 Turbo Fast", api: "openai-completions" }),
		// ids that literally contain "/" (OpenRouter style) and ":" .
		M("openrouter", "openai/gpt-4o", { api: "openai-completions" }),
		M("openrouter", "openai/gpt-4o:extended", { api: "openai-completions" }),
		// a provider whose NAME is the leading path segment of another provider's id
		M("xiaomi", "mimo-v2.5-pro"),
		M("commandcode", "xiaomi/mimo-v2.5-pro"),
		// the same bare id on two providers -> findExactModelReferenceMatch rejects it
		M("groq", "shared-model-id"),
		M("cerebras", "shared-model-id"),
		// provider-prefix interpretation vs a literal slash id
		M("zai", "glm-5.1"),
		M("vercel-ai-gateway", "zai/glm-5.1"),
	];
}

/**
 * Minimal stand-in for ModelRuntime. resolveCliModel calls exactly two members
 * (`getModels`, `hasConfiguredAuth`) and findInitialModel exactly three
 * (`getModel`, `hasConfiguredAuth`, `getAvailable`); nothing else is reachable
 * from those functions. Pi's functions run unmodified - only the data source is
 * substituted, which is what makes the synthetic catalog usable at all.
 */
function stubRuntime({ models, available, configuredProviders }) {
	const authed = new Set(configuredProviders ?? models.map((m) => m.provider));
	return {
		getModels: () => models,
		getModel: (provider, id) => models.find((m) => m.provider === provider && m.id === id),
		getAvailable: async () => available ?? models.filter((m) => authed.has(m.provider)),
		hasConfiguredAuth: (provider) => authed.has(provider),
	};
}

const summarizeModel = (m) =>
	m === undefined || m === null
		? null
		: {
				provider: m.provider,
				id: m.id,
				name: m.name ?? null,
				api: m.api,
				baseUrl: m.baseUrl ?? null,
				reasoning: m.reasoning ?? null,
				input: m.input ?? null,
				contextWindow: m.contextWindow ?? null,
				maxTokens: m.maxTokens ?? null,
			};

async function genModelResolutionCases() {
	const resolver = await impPi("core/model-resolver.ts");
	const defaults = await impPi("core/defaults.ts");
	const CAT = buildSyntheticCatalog();
	const records = [];

	records.push({
		fn: "constants",
		note: "DEFAULT_THINKING_LEVEL (core/defaults.ts) is what findInitialModel falls back to, and defaultModelPerProvider drives buildFallbackModel's base pick and findInitialModel's step-4 scan ORDER (Object.keys order of the literal).",
		DEFAULT_THINKING_LEVEL: defaults.DEFAULT_THINKING_LEVEL,
		defaultModelPerProviderKeyOrder: Object.keys(resolver.defaultModelPerProvider),
		defaultModelPerProvider: resolver.defaultModelPerProvider,
	});
	records.push({
		fn: "syntheticCatalog",
		note: "The exact model list every `catalogSource: \"synthetic\"` record below ran against. Rebuild this verbatim.",
		models: CAT,
	});

	// -- findExactModelReferenceMatch ------------------------------------------
	const exactCases = [
		{ ref: "claude-sonnet-4-5", note: "bare id, unique -> matched by the idMatches pass" },
		{ ref: "anthropic/claude-sonnet-4-5", note: "canonical provider/id" },
		{ ref: "ANTHROPIC/CLAUDE-SONNET-4-5", note: "the canonical compare is case-INSENSITIVE" },
		{ ref: "  anthropic/claude-sonnet-4-5  ", note: "the reference is TRIMMED first" },
		{ ref: "", note: "GUARD: an empty (or whitespace-only) reference returns undefined immediately" },
		{ ref: "   ", note: "GUARD: whitespace-only trims to empty" },
		{ ref: "shared-model-id", note: "AMBIGUOUS bare id across two providers -> undefined (NOT the first match)" },
		{ ref: "groq/shared-model-id", note: "disambiguated by the canonical form" },
		{ ref: "openai/gpt-4o", note: "the canonical pass wins for openrouter's literal-slash id BEFORE the provider/id split could interpret \"openai\" as a provider" },
		{ ref: "openai/gpt-5.5", note: "canonical match on the real openai provider" },
		{ ref: "openrouter/openai/gpt-4o", note: "canonical match where the id itself contains a slash" },
		{ ref: "openai/openai/gpt-4o", note: "no canonical match; the split takes provider=\"openai\", modelId=\"openai/gpt-4o\", which openai does not have -> undefined" },
		{ ref: "zai/glm-5.1", note: "canonical match on provider zai, NOT on vercel-ai-gateway's literal id \"zai/glm-5.1\"" },
		{ ref: "vercel-ai-gateway/zai/glm-5.1", note: "canonical match on the literal-slash id" },
		{ ref: "xiaomi/mimo-v2.5-pro", note: "canonical match on provider xiaomi wins over commandcode's identical literal id" },
		{ ref: "anthropic/nope", note: "no match at any pass" },
		{ ref: "anthropic /claude-sonnet-4-5", note: "the split TRIMS both halves, so a space before the slash is tolerated" },
		{ ref: "/claude-sonnet-4-5", note: "the split yields an empty provider, so the `provider && modelId` guard fails; the bare-id pass then matches" },
		{ ref: "anthropic/", note: "the split yields an empty modelId -> guard fails -> no bare-id match either" },
	];
	for (const c of exactCases) {
		records.push({
			fn: "findExactModelReferenceMatch",
			catalogSource: "synthetic",
			input: c.ref,
			note: c.note,
			result: summarizeModel(resolver.findExactModelReferenceMatch(c.ref, CAT)),
		});
	}

	// -- parseModelPattern ----------------------------------------------------
	const parseCase = (pattern, note, options, extra = {}) => {
		const r = options === undefined ? resolver.parseModelPattern(pattern, CAT) : resolver.parseModelPattern(pattern, CAT, options);
		records.push({
			fn: "parseModelPattern",
			catalogSource: "synthetic",
			input: pattern,
			options: options ?? null,
			note,
			...extra,
			result: { model: summarizeModel(r.model), thinkingLevel: r.thinkingLevel ?? null, warning: r.warning ?? null },
		});
	};

	parseCase("claude-sonnet-4-5", "bare id, exact");
	parseCase("anthropic/claude-sonnet-4-5", "provider/id, exact");
	parseCase("claude-sonnet-4-5:high", "bare id + valid :<thinking> suffix");
	parseCase("anthropic/claude-sonnet-4-5:high", "provider/id + valid :<thinking> suffix");
	parseCase("claude-sonnet-4-5:off", "\"off\" is a valid level and IS returned (it is not treated as absent)");
	for (const level of ["minimal", "low", "medium", "high", "xhigh", "max"]) {
		parseCase(`claude-sonnet-4-5:${level}`, `valid level "${level}"`);
	}
	parseCase(
		"claude-sonnet-4-5:bogus",
		"INVALID suffix, allowInvalidThinkingLevelFallback DEFAULTS TO TRUE: the prefix is retried and a warning is produced, with thinkingLevel left unset",
	);
	parseCase("claude-sonnet-4-5:bogus", "the SAME pattern in strict mode (what resolveCliModel uses): no model, no warning at all", {
		allowInvalidThinkingLevelFallback: false,
	});
	parseCase("claude-sonnet-4-5:high", "strict mode does not affect a VALID suffix", { allowInvalidThinkingLevelFallback: false });
	parseCase("openai/gpt-4o:extended", "the FULL pattern is tried as a model id first, so an id containing a colon resolves with NO thinking level");
	parseCase("openai/gpt-4o:extended:high", "two colons: the outer :high is stripped, then the remaining full id matches exactly");
	parseCase(
		"openai/gpt-4o:bogus:high",
		"two colons, inner invalid: :high is valid so it recurses; the inner call warns about \"bogus\"; because result.warning is set the OUTER thinking level is SUPPRESSED and the warning propagates",
	);
	parseCase(
		"claude-sonnet-4-5:high:low",
		"two colons, BOTH valid levels: the recursion strips :low first, the inner call returns thinkingLevel \"high\", but the OUTER frame overwrites it with \"low\" - the LAST suffix wins",
	);
	parseCase("openai/gpt-4o:bogus", "invalid suffix on an id that itself contains a colon-free prefix which DOES match", undefined);
	parseCase("openai/gpt-4o:bogus", "same, strict mode", { allowInvalidThinkingLevelFallback: false });
	parseCase(
		"",
		"TRAP: findExactModelReferenceMatch rejects the empty string, but the substring fallback uses `id.includes(\"\")`, which is TRUE for every model - so the empty pattern matches the whole catalog and the alias tie-break picks a model",
		undefined,
		{ localeCompareDependent: true },
	);
	parseCase("anthropic/*", "parseModelPattern has NO glob handling (that lives in resolveModelScopeWithDiagnostics); \"*\" is matched literally and finds nothing");
	parseCase("*sonnet*", "same: a glob reaches no model here");
	parseCase("sonnet", "substring match on the ID", undefined, { localeCompareDependent: true });
	parseCase("Sonnet 4.5", "substring match on the NAME (both sides lowercased)", undefined, { localeCompareDependent: true });
	parseCase("Turbo Fast", "name-only substring match, no id overlap");
	parseCase(
		"claude",
		"substring match hits 3 aliases and 3 dated ids; aliases win, then `sort((a,b) => b.id.localeCompare(a.id))[0]`",
		undefined,
		{ localeCompareDependent: true },
	);
	parseCase(
		"2025",
		"substring match hits ONLY dated ids, so the datedVersions branch runs and the highest-sorting id wins",
		undefined,
		{ localeCompareDependent: true },
	);
	parseCase("claude-haiku-4-5", "\"-latest\" counts as an alias, so the -latest id beats the dated one", undefined, { localeCompareDependent: true });
	parseCase("shared-model-id", "exact match is ambiguous -> undefined, then the substring pass finds both; both are aliases and their ids are EQUAL, so Array#sort stability decides", undefined, {
		localeCompareDependent: true,
	});
	parseCase("nope-no-such-model", "no match, no colon -> everything undefined");
	parseCase("nope:high", "no match for the prefix either -> the inner result (all undefined) is returned unchanged");
	parseCase(":high", "the prefix is the empty string, which matches everything via includes(\"\")", undefined, { localeCompareDependent: true });
	parseCase("CLAUDE-SONNET-4-5", "the exact pass is case-insensitive");
	parseCase("  claude-sonnet-4-5  ", "the exact pass trims; the substring pass would not, but it is never reached");

	// -- resolveCliModel ------------------------------------------------------
	const cliCase = (name, note, options, runtimeOptions = {}) => {
		const runtime = stubRuntime({ models: CAT, ...runtimeOptions });
		const r = resolver.resolveCliModel({ ...options, modelRuntime: runtime });
		records.push({
			fn: "resolveCliModel",
			catalogSource: "synthetic",
			name,
			note,
			input: {
				cliProvider: options.cliProvider ?? null,
				cliModel: options.cliModel ?? null,
				cliThinking: options.cliThinking ?? null,
			},
			configuredProviders: runtimeOptions.configuredProviders ?? "ALL",
			result: {
				model: summarizeModel(r.model),
				thinkingLevel: r.thinkingLevel ?? null,
				warning: r.warning ?? null,
				error: r.error ?? null,
			},
		});
	};

	cliCase("no-cliModel-is-a-no-op", "GUARD: without --model the function returns immediately; --provider ALONE is ignored here (findInitialModel needs BOTH)", { cliProvider: "anthropic" });
	cliCase("model-only-bare-id", "--model <id>", { cliModel: "claude-sonnet-4-5" });
	cliCase("model-only-provider-slash-id", "--model <provider>/<id>: the prefix before the FIRST slash is looked up as a provider and stripped", { cliModel: "anthropic/claude-sonnet-4-5" });
	cliCase("model-only-fuzzy", "--model sonnet resolves through parseModelPattern's substring pass", { cliModel: "sonnet" });
	cliCase("provider-and-model", "--provider + --model", { cliProvider: "anthropic", cliModel: "claude-sonnet-4-5" });
	cliCase("provider-and-model-with-redundant-prefix", "--provider anthropic --model anthropic/claude-sonnet-4-5: the redundant prefix is stripped case-insensitively", {
		cliProvider: "anthropic",
		cliModel: "ANTHROPIC/claude-sonnet-4-5",
	});
	cliCase("provider-case-insensitive", "--provider is canonicalised through a lowercased provider map", { cliProvider: "ANTHROPIC", cliModel: "claude-sonnet-4-5" });
	cliCase("unknown-provider-is-an-ERROR", "an unrecognised --provider fails before any model matching", { cliProvider: "nope", cliModel: "claude-sonnet-4-5" });
	cliCase("unknown-model-is-an-ERROR", "no provider, no match anywhere -> error naming the raw --model value", { cliModel: "no-such-model" });
	cliCase(
		"unknown-model-WITH-provider-builds-a-FALLBACK-model",
		"when --provider resolved, buildFallbackModel clones that provider's default model (defaultModelPerProvider, else providerModels[0]) and renames it - so an unknown id becomes a usable custom model plus a warning",
		{ cliProvider: "anthropic", cliModel: "my-private-snapshot" },
	);
	cliCase(
		"fallback-model-with---thinking-sets-reasoning-true",
		"cliThinking !== \"off\" flips reasoning on the cloned model",
		{ cliProvider: "anthropic", cliModel: "my-private-snapshot", cliThinking: "high" },
	);
	cliCase("fallback-model-with---thinking-off-leaves-reasoning-alone", "\"off\" is excluded by `requestedThinking !== \"off\"`", {
		cliProvider: "anthropic",
		cliModel: "my-private-snapshot",
		cliThinking: "off",
	});
	cliCase(
		"fallback-model-parses-its-own-thinking-suffix",
		"with no --thinking, a trailing :<level> is split off the pattern BEFORE the fallback id is built, and is returned as thinkingLevel",
		{ cliProvider: "anthropic", cliModel: "my-private-snapshot:high" },
	);
	cliCase(
		"explicit---thinking-suppresses-the-suffix-split",
		"when cliThinking is set the suffix is NOT split, so the colon stays part of the fallback model id",
		{ cliProvider: "anthropic", cliModel: "my-private-snapshot:high", cliThinking: "low" },
	);
	cliCase("provider-with-no-models-cannot-build-a-fallback", "buildFallbackModel returns undefined for a provider with zero models, so the generic not-found error wins", {
		cliProvider: "openai",
		cliModel: "no-such-model",
	});
	cliCase("invalid-thinking-suffix-is-STRICT-here", "resolveCliModel passes allowInvalidThinkingLevelFallback:false, so \":bogus\" is not stripped and the model is not found", {
		cliModel: "claude-sonnet-4-5:bogus",
	});
	cliCase("valid-thinking-suffix-is-returned", "--model <id>:<level>", { cliModel: "claude-sonnet-4-5:high" });
	cliCase(
		"literal-slash-id-wins-when-the-prefix-is-NOT-a-provider",
		"\"openai/gpt-4o\" - openai IS a provider here, so provider inference fires first, finds nothing in openai, and the inferredProvider fallback then matches openrouter's literal id",
		{ cliModel: "openai/gpt-4o" },
	);
	cliCase(
		"inferredProvider-fallback-to-a-full-raw-id-with-a-colon",
		"\"openai/gpt-4o:extended\": inference strips openai, \"gpt-4o:extended\" misses inside openai, then the exact full-input match lands on openrouter",
		{ cliModel: "openai/gpt-4o:extended" },
	);
	cliCase("provider-prefix-beats-a-literal-slash-id", "\"zai/glm-5.1\" resolves to provider zai, NOT to vercel-ai-gateway's literal id of the same text", { cliModel: "zai/glm-5.1" });
	cliCase("explicit-provider-selects-the-literal-slash-id", "--provider vercel-ai-gateway pins the other interpretation", {
		cliProvider: "vercel-ai-gateway",
		cliModel: "zai/glm-5.1",
	});
	cliCase(
		"inferredProvider-prefers-an-AUTHENTICATED-raw-id-match",
		"\"xiaomi/mimo-v2.5-pro\" infers provider xiaomi, but xiaomi has NO configured auth while commandcode (whose literal model id is \"xiaomi/mimo-v2.5-pro\") does, and there is exactly ONE such match - so the authenticated raw match wins",
		{ cliModel: "xiaomi/mimo-v2.5-pro" },
		{ configuredProviders: ["commandcode", "anthropic"] },
	);
	cliCase(
		"inferredProvider-keeps-the-provider-interpretation-when-it-IS-authenticated",
		"same input, but xiaomi has auth, so the `!hasConfiguredAuth(model.provider)` guard fails and provider inference stands",
		{ cliModel: "xiaomi/mimo-v2.5-pro" },
		{ configuredProviders: ["xiaomi", "commandcode"] },
	);
	cliCase(
		"inferredProvider-auth-preference-needs-EXACTLY-one-authenticated-raw-match",
		"with neither xiaomi nor commandcode authenticated there is no authenticated raw match, so provider inference stands",
		{ cliModel: "xiaomi/mimo-v2.5-pro" },
		{ configuredProviders: ["anthropic"] },
	);
	cliCase("ambiguous-bare-id-falls-through-to-the-substring-pass", "\"shared-model-id\" is ambiguous for the exact pass", { cliModel: "shared-model-id" });
	{
		// Empty catalog: the earliest guard in the function.
		const r = resolver.resolveCliModel({ cliModel: "anything", modelRuntime: stubRuntime({ models: [] }) });
		records.push({
			fn: "resolveCliModel",
			catalogSource: "synthetic",
			name: "empty-catalog-is-an-ERROR",
			note: "GUARD: `availableModels.length === 0` produces the installation error before --provider is even validated",
			input: { cliProvider: null, cliModel: "anything", cliThinking: null },
			configuredProviders: [],
			result: { model: summarizeModel(r.model), thinkingLevel: r.thinkingLevel ?? null, warning: r.warning ?? null, error: r.error ?? null },
		});
	}

	// -- findInitialModel -----------------------------------------------------
	// findInitialModel calls process.exit(1) when the CLI pair fails to resolve.
	// process.exit is Node's, not Pi's, so replacing it with a throw observes the
	// real control flow without killing the generator.
	class ExitSignal extends Error {
		constructor(code) {
			super(`process.exit(${code})`);
			this.code = code;
		}
	}
	const initialCase = async (name, note, options, runtimeOptions = {}) => {
		const runtime = stubRuntime({ models: CAT, ...runtimeOptions });
		const realExit = process.exit;
		let exitCode = null;
		process.exit = (code) => {
			throw new ExitSignal(code);
		};
		let result;
		let consoleOut;
		try {
			const captured = captureConsole(() => resolver.findInitialModel({ ...options, modelRuntime: runtime }));
			consoleOut = captured.console;
			result = await captured.result;
		} catch (err) {
			if (err instanceof ExitSignal) exitCode = err.code;
			else throw err;
		} finally {
			process.exit = realExit;
		}
		records.push({
			fn: "findInitialModel",
			catalogSource: "synthetic",
			name,
			note,
			input: {
				cliProvider: options.cliProvider ?? null,
				cliModel: options.cliModel ?? null,
				scopedModels: (options.scopedModels ?? []).map((sm) => ({ model: `${sm.model.provider}/${sm.model.id}`, thinkingLevel: sm.thinkingLevel ?? null })),
				isContinuing: options.isContinuing,
				defaultProvider: options.defaultProvider ?? null,
				defaultModelId: options.defaultModelId ?? null,
				defaultThinkingLevel: options.defaultThinkingLevel ?? null,
			},
			configuredProviders: runtimeOptions.configuredProviders ?? "ALL",
			availableOverride: runtimeOptions.available ? runtimeOptions.available.map((m) => `${m.provider}/${m.id}`) : null,
			exitCode,
			console: consoleOut ?? [],
			result:
				result === undefined
					? null
					: { model: summarizeModel(result.model), thinkingLevel: result.thinkingLevel, fallbackMessage: result.fallbackMessage ?? null },
		});
	};

	const scoped = (provider, id, thinkingLevel) => ({ model: CAT.find((m) => m.provider === provider && m.id === id), thinkingLevel });

	await initialCase("step1:cli-pair-wins", "PRECEDENCE 1: --provider AND --model both set and resolvable. NOTE the parsed thinking level is DISCARDED here - DEFAULT_THINKING_LEVEL is returned regardless.", {
		cliProvider: "anthropic",
		cliModel: "claude-sonnet-4-5:high",
		scopedModels: [scoped("openai", "gpt-5.5")],
		isContinuing: false,
		defaultThinkingLevel: "low",
	});
	await initialCase("step1:cli-pair-needs-BOTH-flags", "with only --model the step-1 block is skipped entirely and step 2 (scoped models) wins", {
		cliModel: "claude-sonnet-4-5",
		scopedModels: [scoped("openai", "gpt-5.5")],
		isContinuing: false,
	});
	await initialCase("step1:cli-pair-error-EXITS", "an unresolvable pair prints the error in red to stderr and calls process.exit(1)", {
		cliProvider: "openai",
		cliModel: "no-such-model",
		scopedModels: [],
		isContinuing: false,
	});
	await initialCase("step1:cli-pair-unresolved-WITHOUT-an-error-falls-through", "resolveCliModel can return neither model nor error (strict invalid thinking suffix); step 1 then falls through instead of exiting", {
		cliProvider: "openai",
		cliModel: "gpt-5.5:bogus",
		scopedModels: [scoped("anthropic", "claude-sonnet-4-5")],
		isContinuing: false,
	});
	await initialCase("step2:first-scoped-model", "PRECEDENCE 2: scopedModels[0] wins over settings defaults", {
		scopedModels: [scoped("openai", "gpt-5.5"), scoped("anthropic", "claude-sonnet-4-5")],
		isContinuing: false,
		defaultProvider: "anthropic",
		defaultModelId: "claude-opus-4-8",
	});
	await initialCase("step2:scoped-thinking-level-wins-over-settings", "scopedModels[0].thinkingLevel ?? defaultThinkingLevel ?? DEFAULT_THINKING_LEVEL", {
		scopedModels: [scoped("openai", "gpt-5.5", "xhigh")],
		isContinuing: false,
		defaultThinkingLevel: "low",
	});
	await initialCase("step2:scoped-without-thinking-uses-settings-default", "the ?? chain's second link", {
		scopedModels: [scoped("openai", "gpt-5.5")],
		isContinuing: false,
		defaultThinkingLevel: "low",
	});
	await initialCase("step2:scoped-without-thinking-and-no-settings-uses-DEFAULT", "the ?? chain's third link", {
		scopedModels: [scoped("openai", "gpt-5.5")],
		isContinuing: false,
	});
	await initialCase("step2:SKIPPED-when-continuing", "isContinuing suppresses the scoped-model shortcut so the session's own model can be restored later", {
		scopedModels: [scoped("openai", "gpt-5.5")],
		isContinuing: true,
		defaultProvider: "anthropic",
		defaultModelId: "claude-opus-4-8",
		defaultThinkingLevel: "low",
	});
	await initialCase("step3:settings-default-when-authenticated", "PRECEDENCE 3: settings.defaultProvider + settings.defaultModel, and settings.defaultThinkingLevel is applied here", {
		scopedModels: [],
		isContinuing: false,
		defaultProvider: "anthropic",
		defaultModelId: "claude-sonnet-4-5",
		defaultThinkingLevel: "low",
	});
	await initialCase("step3:settings-default-without-a-thinking-level", "thinkingLevel stays DEFAULT_THINKING_LEVEL", {
		scopedModels: [],
		isContinuing: false,
		defaultProvider: "anthropic",
		defaultModelId: "claude-sonnet-4-5",
	});
	await initialCase(
		"step3:settings-default-SKIPPED-when-its-provider-is-unauthenticated",
		"the `hasConfiguredAuth(found.provider)` guard drops the saved default and step 4 takes over",
		{ scopedModels: [], isContinuing: false, defaultProvider: "anthropic", defaultModelId: "claude-sonnet-4-5", defaultThinkingLevel: "low" },
		{ configuredProviders: ["openai"] },
	);
	await initialCase("step3:settings-default-SKIPPED-when-the-model-is-gone", "getModel returns undefined for an id that no longer exists", {
		scopedModels: [],
		isContinuing: false,
		defaultProvider: "anthropic",
		defaultModelId: "claude-removed-from-catalog",
	});
	await initialCase("step3:needs-BOTH-defaultProvider-and-defaultModelId", "one without the other skips the block", {
		scopedModels: [],
		isContinuing: false,
		defaultProvider: "anthropic",
	});
	await initialCase(
		"step4:scans-defaultModelPerProvider-IN-KEY-ORDER",
		"PRECEDENCE 4: the first (provider, defaultId) pair from defaultModelPerProvider that is present in getAvailable() wins - so it is the ORDER of that literal, not the order of the available list, that decides",
		{ scopedModels: [], isContinuing: false },
	);
	await initialCase(
		"step4:falls-back-to-available[0]-when-no-default-matches",
		"none of the defaultModelPerProvider ids are available, so the FIRST available model is used",
		{ scopedModels: [], isContinuing: false },
		{ available: CAT.filter((m) => m.provider === "openrouter" || m.provider === "groq") },
	);
	await initialCase(
		"step5:nothing-available-yields-no-model",
		"PRECEDENCE 5: model undefined, thinkingLevel DEFAULT_THINKING_LEVEL, no fallback message",
		{ scopedModels: [], isContinuing: false },
		{ available: [] },
	);

	// -- resolveModelScopeWithDiagnostics (glob handling) ---------------------
	const scopeRuntime = stubRuntime({ models: CAT });
	const scopeCases = [
		{ patterns: ["anthropic/*"], note: "a glob against the `provider/id` form" },
		{ patterns: ["*sonnet*"], note: "a bare glob is matched against BOTH `provider/id` and `id`" },
		{ patterns: ["anthropic/*:high"], note: "a trailing valid :<level> is stripped from the GLOB and applied to every match" },
		{ patterns: ["anthropic/*:bogus"], note: "an INVALID suffix is NOT stripped, so it becomes part of the glob and matches nothing" },
		{ patterns: ["claude-?aiku-4-5-latest"], note: "\"?\" also triggers the glob branch" },
		{ patterns: ["claude-[sh]*"], note: "\"[\" also triggers the glob branch" },
		{ patterns: ["ANTHROPIC/*"], note: "minimatch runs with nocase: true" },
		{ patterns: ["no-such-*"], note: "a glob with no matches emits a `No models match pattern` warning" },
		{ patterns: ["nope"], note: "a NON-glob pattern with no match emits the same warning via the parseModelPattern path" },
		{ patterns: ["claude-sonnet-4-5:bogus"], note: "non-glob with an invalid level: the scope path uses the DEFAULT (permissive) fallback, so it warns AND still resolves" },
		{ patterns: ["anthropic/*", "*sonnet*"], note: "duplicates across patterns are dropped by modelsAreEqual (provider+id)" },
		{ patterns: ["claude-sonnet-4-5", "anthropic/claude-sonnet-4-5"], note: "the same model reached two ways is de-duplicated" },
		{ patterns: ["claude-sonnet-4-5:high", "claude-sonnet-4-5:low"], note: "de-duplication is by MODEL only, so the second pattern's thinking level is DISCARDED" },
		{ patterns: [], note: "no patterns -> no scoped models, no diagnostics" },
	];
	for (const c of scopeCases) {
		const r = await resolver.resolveModelScopeWithDiagnostics(c.patterns, scopeRuntime);
		records.push({
			fn: "resolveModelScopeWithDiagnostics",
			catalogSource: "synthetic",
			input: c.patterns,
			note: c.note,
			result: {
				scopedModels: r.scopedModels.map((sm) => ({ model: `${sm.model.provider}/${sm.model.id}`, thinkingLevel: sm.thinkingLevel ?? null })),
				diagnostics: r.diagnostics,
			},
		});
	}

	return records;
}

// ===========================================================================
// I. models.cases.jsonl  (part 2: models.json + provider composition + --list-models)
// ===========================================================================

/**
 * The `models.json` variants every composition / --list-models record runs
 * against. Emitted into the fixture verbatim so a Rust test can rebuild each
 * file byte-for-byte.
 *
 * `raw` is the exact file contents (or null for "no file at all"). Anything with
 * `stripComments` in its note exercises models.json's stripJsonComments pass,
 * which settings.json does NOT have.
 */
function modelsJsonVariants() {
	const pretty2 = (v) => `${JSON.stringify(v, null, 2)}\n`;
	return [
		{
			name: "meridian-local-proxy",
			note: "THE SHAPE THIS PROJECT ACTUALLY RUNS: override the builtin anthropic provider's baseUrl + apiKey and add a custom header. Every builtin anthropic model keeps its id/name/cost/contextWindow and gets the new baseUrl; the header is NOT copied onto the model objects (modelFromJson sets headers: undefined and applyModelsJson does not touch model.headers) - it is applied at request time by resolveConfiguredModelHeaders.",
			raw: pretty2({
				providers: {
					anthropic: {
						baseUrl: "http://127.0.0.1:3456",
						apiKey: "x",
						headers: { "x-meridian-agent": "pi" },
					},
				},
			}),
		},
		{
			name: "baseUrl-only-override-on-a-builtin",
			note: "no apiKey: composeApiKeyAuth still finds the builtin's env-var auth, so composition succeeds and only baseUrl changes",
			raw: pretty2({ providers: { anthropic: { baseUrl: "http://127.0.0.1:9999" } } }),
		},
		{
			name: "fully-custom-provider-with-inline-models",
			note: "a provider id that is NOT in the builtin catalog: base is undefined, so `models` + `api` + `baseUrl` must come from the file. modelFromJson supplies the defaults - name := id, reasoning := false, input := [\"text\"], cost := all zeros, contextWindow := 128000, maxTokens := 16384, headers := undefined.",
			raw: pretty2({
				providers: {
					"oracle-local": {
						name: "Oracle Local",
						baseUrl: "http://127.0.0.1:3456",
						apiKey: "test-key",
						api: "anthropic-messages",
						models: [
							{ id: "oracle-small" },
							{
								id: "oracle-large",
								name: "Oracle Large",
								reasoning: true,
								input: ["text", "image"],
								contextWindow: 1000000,
								maxTokens: 64000,
								cost: { input: 1, output: 2, cacheRead: 0.5, cacheWrite: 1.5 },
							},
							{ id: "oracle-tiny", api: "openai-completions", baseUrl: "http://127.0.0.1:4567", contextWindow: 8192, maxTokens: 1024 },
						],
					},
				},
			}),
		},
		{
			name: "custom-provider-missing-api-is-a-COMPOSITION-ERROR",
			note: "no api at provider or model level and no builtin base -> modelFromJson throws; ModelRuntime catches it into compositionErrors and, with no base to fall back to, DELETES the provider. getError() still reports it.",
			raw: pretty2({ providers: { "oracle-noapi": { baseUrl: "http://127.0.0.1:3456", apiKey: "k", models: [{ id: "m1" }] } } }),
		},
		{
			name: "custom-provider-missing-baseUrl-is-a-COMPOSITION-ERROR",
			note: "api present but no baseUrl anywhere",
			raw: pretty2({ providers: { "oracle-nourl": { apiKey: "k", api: "anthropic-messages", models: [{ id: "m1" }] } } }),
		},
		{
			name: "unknown-api-value-COMPOSES-FINE",
			note: "an unrecognised `api` string is NOT rejected at composition time - the model is created with it and the failure only surfaces at stream time as `No API provider registered for api: <value>`. So there is NO compositionError and the model IS listed.",
			raw: pretty2({
				providers: {
					"oracle-badapi": { baseUrl: "http://127.0.0.1:3456", apiKey: "k", api: "not-a-real-api", models: [{ id: "m1" }] },
				},
			}),
		},
		{
			name: "empty-provider-object-is-a-COMPOSITION-ERROR",
			note: "applyModelsJson's `must specify \"baseUrl\", \"headers\", \"compat\", \"modelOverrides\", or \"models\"` guard; anthropic HAS a builtin base, so composition falls back to the untouched builtin and the provider survives",
			raw: pretty2({ providers: { anthropic: {} } }),
		},
		{
			name: "modelOverrides-on-a-builtin",
			note: "modelOverrides are the topmost layer, applied after custom-model upserts; only the listed keys change",
			raw: pretty2({
				providers: {
					anthropic: {
						baseUrl: "http://127.0.0.1:3456",
						apiKey: "x",
						modelOverrides: { "claude-sonnet-4-5": { name: "Renamed Sonnet", maxTokens: 4096, reasoning: false } },
					},
				},
			}),
		},
		{
			name: "models-array-UPSERTS-onto-a-builtin-id",
			note: "a definition whose id already exists REPLACES that entry in place (keeping its index); an unknown id is APPENDED. The replacement is a full modelFromJson build, so unspecified fields get modelFromJson's defaults, NOT the builtin's values.",
			raw: pretty2({
				providers: {
					anthropic: {
						baseUrl: "http://127.0.0.1:3456",
						apiKey: "x",
						models: [{ id: "claude-sonnet-4-5" }, { id: "claude-private-snapshot" }],
					},
				},
			}),
		},
		{
			name: "json-with-comments-is-ACCEPTED",
			note: "models.json is parsed as JSON.parse(stripJsonComments(content)), so // and /* */ comments and are legal here (settings.json does NOT allow them)",
			raw: '{\n  // a line comment\n  "providers": {\n    /* a block comment */\n    "anthropic": { "baseUrl": "http://127.0.0.1:3456", "apiKey": "x" }\n  }\n}\n',
		},
		{
			name: "malformed-json-is-a-CONFIG-ERROR",
			note: "`Failed to parse models.json: <V8 message>\\n\\nFile: <path>` - the whole config becomes empty, so NO provider is overlaid and every builtin is used untouched",
			raw: '{ "providers": { "anthropic": ',
			v8Dependent: true,
		},
		{
			name: "missing-providers-key-is-a-SCHEMA-ERROR",
			note: "the top-level schema requires `providers`; the error body is one `  - <path>: <message>` line per TypeBox error, and a `required` error appends the missing property name to the path",
			raw: pretty2({ notProviders: {} }),
		},
		{
			name: "provider-with-an-unknown-field-is-a-SCHEMA-ERROR",
			note: "ProviderConfigSchema is a closed Type.Object, so an unexpected key fails validation",
			raw: pretty2({ providers: { anthropic: { baseUrl: "http://x", totallyUnknownField: 1 } } }),
		},
		{
			name: "empty-string-baseUrl-is-a-SCHEMA-ERROR",
			note: "minLength: 1 on baseUrl/apiKey/api/name",
			raw: pretty2({ providers: { anthropic: { baseUrl: "" } } }),
		},
		{
			name: "model-without-an-id-is-a-SCHEMA-ERROR",
			note: "ModelDefinitionSchema requires `id`; the instancePath is turned into a dotted path",
			raw: pretty2({ providers: { "oracle-x": { baseUrl: "http://x", api: "anthropic-messages", models: [{ name: "no id" }] } } }),
		},
		{
			name: "providers-as-an-array-is-a-SCHEMA-ERROR",
			note: "Type.Record rejects an array",
			raw: pretty2({ providers: [] }),
		},
		{
			name: "empty-providers-object-is-VALID",
			note: "`{ \"providers\": {} }` validates and produces an empty overlay - identical to having no file",
			raw: pretty2({ providers: {} }),
		},
		{ name: "missing-file", note: "ENOENT is NOT an error: an absent models.json yields an empty config with getError() undefined", raw: null },
	];
}

/**
 * `ModelRuntime.create()` reads provider auth from the process environment, so
 * these records are captured in a CHILD PROCESS with an EXPLICIT ALLOWLIST
 * environment (see MODELS_CHILD_ENV_KEYS). That is what makes them reproducible:
 * no host ANTHROPIC_API_KEY / AWS_PROFILE / ANTHROPIC_BASE_URL can leak in.
 *
 * PI_OFFLINE=1 is set, so ModelRuntime.create resolves
 * `allowModelNetwork = process.env.PI_OFFLINE === undefined` to FALSE and the
 * remote-catalog refresh is skipped entirely - every record ran with no network.
 * Each record carries `offline: true`.
 */
const MODELS_CHILD_ENV_KEYS = [
	"PATH",
	"Path",
	"PATHEXT",
	"SystemRoot",
	"SystemDrive",
	"windir",
	"ComSpec",
	"TEMP",
	"TMP",
	"TMPDIR",
	"USERPROFILE",
	"HOMEDRIVE",
	"HOMEPATH",
	"HOME",
	"LOCALAPPDATA",
	"APPDATA",
	"OS",
	"NUMBER_OF_PROCESSORS",
	"LANG",
	"LC_ALL",
];

async function genModelRuntimeCases() {
	const { spawnSync } = await import("node:child_process");
	const root = join(TMPROOT, "models-runtime");
	assertTemp(root);
	mkdirSync(root, { recursive: true });
	const outFile = join(root, "records.json");

	const env = {};
	for (const key of MODELS_CHILD_ENV_KEYS) {
		if (process.env[key] !== undefined) env[key] = process.env[key];
	}
	env.NO_COLOR = "1";
	env.PI_OFFLINE = "1";

	const res = spawnSync(process.execPath, [fileURLToPath(import.meta.url), "--emit-models", root, outFile], {
		env,
		encoding: "buffer",
		maxBuffer: 32 * 1024 * 1024,
	});
	if (res.status !== 0 || !existsSync(outFile)) {
		throw new Error(`models child exited ${res.status}:\n${res.stderr?.toString("utf-8")}\n${res.stdout?.toString("utf-8")}`);
	}
	const records = JSON.parse(readFileSync(outFile, "utf-8"));
	records.unshift({
		fn: "captureEnvironment",
		note: "The exact environment the `catalogSource: \"builtin\"` records below were captured under. Only these keys were passed through from the host (those that existed), plus NO_COLOR=1 and PI_OFFLINE=1; every provider credential env var was therefore ABSENT, which is what makes provider availability reproducible.",
		envAllowlist: MODELS_CHILD_ENV_KEYS,
		envSet: { NO_COLOR: "1", PI_OFFLINE: "1", PI_CODING_AGENT_DIR: "<per-case temp dir>" },
		offline: true,
		childStderr: res.stderr.toString("utf-8"),
	});
	return records;
}

/** CHILD MODE body for --emit-models. */
async function emitModelRuntimeRecords(root, outFile) {
	const configMod = await impPi("core/model-config.ts");
	const runtimeMod = await impPi("core/model-runtime.ts");
	const listMod = await impPi("cli/list-models.ts");
	const records = [];

	/** Only the fields ModelRuntime/composeModelProvider produce for a model. */
	const fullModel = (m) => ({
		provider: m.provider,
		id: m.id,
		name: m.name ?? null,
		api: m.api,
		baseUrl: m.baseUrl ?? null,
		reasoning: m.reasoning ?? null,
		input: m.input ?? null,
		contextWindow: m.contextWindow ?? null,
		maxTokens: m.maxTokens ?? null,
		cost: m.cost ?? null,
		thinkingLevelMap: m.thinkingLevelMap ?? null,
		headers: m.headers ?? null,
		compat: m.compat ?? null,
	});

	let caseIndex = 0;
	const caseDir = (name) => {
		const dir = join(root, `${String(++caseIndex).padStart(2, "0")}-${name.replace(/[^a-z0-9]+/gi, "-").slice(0, 40)}`, "agent");
		mkdirSync(dir, { recursive: true });
		return dir;
	};

	const variants = modelsJsonVariants();

	// -- ModelConfig.load in isolation ----------------------------------------
	for (const v of variants) {
		const dir = caseDir(`cfg-${v.name}`);
		const modelsPath = join(dir, "models.json");
		if (v.raw !== null) writeFileSync(modelsPath, v.raw, "utf-8");
		const config = await configMod.ModelConfig.load(modelsPath);
		records.push({
			fn: "ModelConfig.load",
			catalogSource: "n/a",
			name: v.name,
			note: v.note,
			offline: true,
			modelsJson: v.raw,
			...(v.v8Dependent ? { v8Dependent: true } : {}),
			providerIds: config.getProviderIds(),
			providers: Object.fromEntries(config.getProviderIds().map((id) => [id, config.getProvider(id)])),
			error: config.getError() ?? null,
		});
	}
	{
		const config = await configMod.ModelConfig.load(undefined);
		records.push({
			fn: "ModelConfig.load",
			catalogSource: "n/a",
			name: "no-path-at-all",
			note: "GUARD: `if (!modelsJsonPath) return new ModelConfig(new Map())` - the path is never touched",
			offline: true,
			modelsJson: null,
			providerIds: config.getProviderIds(),
			providers: {},
			error: config.getError() ?? null,
		});
	}

	// -- full ModelRuntime composition ----------------------------------------
	// Providers a record reports on: the one the models.json names, plus anthropic
	// (the provider this project actually uses). The 1000+ model builtin catalog
	// is NEVER dumped - only counts, plus the models of the reported providers.
	for (const v of variants) {
		const dir = caseDir(`rt-${v.name}`);
		const modelsPath = join(dir, "models.json");
		if (v.raw !== null) writeFileSync(modelsPath, v.raw, "utf-8");
		process.env.PI_CODING_AGENT_DIR = dir;
		const runtime = await runtimeMod.ModelRuntime.create({
			authPath: join(dir, "auth.json"),
			modelsPath,
			modelsStorePath: join(dir, "models-store.json"),
		});
		const available = [...(await runtime.getAvailable())];
		const configured = new Set(available.map((m) => m.provider));
		const namedProviders = new Set(["anthropic"]);
		try {
			const parsed = v.raw === null ? {} : JSON.parse(v.raw.replace(/^\s*\/\/.*$/gm, "").replace(/\/\*[\s\S]*?\*\//g, ""));
			for (const id of Object.keys(parsed.providers ?? {})) namedProviders.add(id);
		} catch {
			// malformed variant: only anthropic is reported
		}
		const reported = {};
		for (const id of [...namedProviders].sort()) {
			const provider = runtime.getProvider(id);
			reported[id] = {
				present: provider !== undefined,
				name: provider?.name ?? null,
				baseUrl: provider?.baseUrl ?? null,
				hasConfiguredAuth: runtime.hasConfiguredAuth(id),
				modelCount: runtime.getModels(id).length,
				models: [...runtime.getModels(id)].map(fullModel),
			};
		}
		records.push({
			fn: "ModelRuntime.create",
			catalogSource: "builtin",
			name: v.name,
			note: v.note,
			offline: true,
			modelsJson: v.raw,
			...(v.v8Dependent ? { v8Dependent: true } : {}),
			getError: runtime.getError() ?? null,
			totals: {
				providers: runtime.getProviders().length,
				models: runtime.getModels().length,
				availableModels: available.length,
				configuredProviders: [...configured].sort(),
			},
			reportedProviders: reported,
		});
	}

	// -- builtin catalog fingerprint ------------------------------------------
	{
		const dir = caseDir("fingerprint");
		process.env.PI_CODING_AGENT_DIR = dir;
		const runtime = await runtimeMod.ModelRuntime.create({
			authPath: join(dir, "auth.json"),
			modelsPath: join(dir, "models.json"),
			modelsStorePath: join(dir, "models-store.json"),
		});
		records.push({
			fn: "builtinCatalogFingerprint",
			catalogSource: "builtin",
			note: "Shape of the untouched builtin catalog with NO models.json. The catalog is GENERATED and grows with every pi release, so these numbers are version-dependent - treat them as a drift signal, not a contract. Only the anthropic provider (the one pirust ports) is enumerated; every other provider is reduced to its id.",
			offline: true,
			piVersion: (await impPi("config.ts")).VERSION,
			totalProviders: runtime.getProviders().length,
			totalModels: runtime.getModels().length,
			providerIds: runtime.getProviders().map((p) => p.id).sort(),
			availableModels: (await runtime.getAvailable()).length,
			anthropic: {
				name: runtime.getProvider("anthropic")?.name ?? null,
				baseUrl: runtime.getProvider("anthropic")?.baseUrl ?? null,
				models: [...runtime.getModels("anthropic")].map(fullModel),
			},
		});
	}

	// -- --list-models ---------------------------------------------------------
	// A models.json with ONE custom authenticated provider keeps `getAvailable()`
	// small and deterministic, which is what makes the computed column widths
	// assertable. Column widths come from the DATA, so the row set matters.
	const listVariants = [
		{
			name: "list:three-custom-models",
			note: "the header row plus one row per model, columns joined with TWO spaces and padEnd'd to max(header, data) width. formatTokenCount renders 1000000 -> \"1M\", 128000 -> \"128K\", 8192 -> \"8.2K\", 900 -> \"900\".",
			search: undefined,
			raw: `${JSON.stringify(
				{
					providers: {
						"oracle-local": {
							name: "Oracle Local",
							baseUrl: "http://127.0.0.1:3456",
							apiKey: "test-key",
							api: "anthropic-messages",
							models: [
								{ id: "oracle-small", contextWindow: 8192, maxTokens: 900 },
								{ id: "oracle-large", reasoning: true, input: ["text", "image"], contextWindow: 1000000, maxTokens: 128000 },
								{ id: "z-oracle-medium", reasoning: true, contextWindow: 200000, maxTokens: 64000 },
							],
						},
					},
				},
				null,
				2,
			)}\n`,
		},
		{
			name: "list:sorted-by-provider-then-id",
			note: "two providers: rows are sorted by provider.localeCompare then id.localeCompare, NOT by declaration order",
			search: undefined,
			raw: `${JSON.stringify(
				{
					providers: {
						"zzz-provider": { baseUrl: "http://127.0.0.1:1", apiKey: "k", api: "anthropic-messages", models: [{ id: "b-model" }, { id: "a-model" }] },
						"aaa-provider": { baseUrl: "http://127.0.0.1:2", apiKey: "k", api: "anthropic-messages", models: [{ id: "only" }] },
					},
				},
				null,
				2,
			)}\n`,
		},
		{
			name: "list:fuzzy-search-hit",
			note: "the search pattern goes through pi-tui's fuzzyFilter over `${provider} ${id}`, which also REORDERS by score before the provider/id sort is applied",
			search: "large",
			raw: null, // reuses the previous variant's file, set below
		},
		{
			name: "list:fuzzy-search-miss",
			note: 'no fuzzy hit -> the single line `No models matching "<pattern>"`',
			search: "zzzz-no-such-model",
			raw: null,
		},
		{
			name: "list:no-models-available",
			note: "with no configured provider at all, getAvailable() is empty and formatNoModelsAvailableMessage() is printed instead of a table",
			search: undefined,
			raw: `${JSON.stringify({ providers: {} }, null, 2)}\n`,
		},
		{
			name: "list:models.json-error-warns-on-stderr-first",
			note: "getError() is printed to STDERR as `Warning: errors loading models.json:\\n<error>` before the table; the table itself still renders from whatever loaded",
			search: undefined,
			raw: '{ "providers": { "oracle-local": ',
			v8Dependent: true,
		},
	];
	const listBaseRaw = listVariants[0].raw;
	for (const v of listVariants) {
		const raw = v.raw ?? listBaseRaw;
		const dir = caseDir(`list-${v.name}`);
		const modelsPath = join(dir, "models.json");
		writeFileSync(modelsPath, raw, "utf-8");
		process.env.PI_CODING_AGENT_DIR = dir;
		const runtime = await runtimeMod.ModelRuntime.create({
			authPath: join(dir, "auth.json"),
			modelsPath,
			modelsStorePath: join(dir, "models-store.json"),
		});
		const lines = [];
		const origLog = console.log;
		const origErr = console.error;
		console.log = (...args) => lines.push({ stream: "stdout", text: args.join(" ") });
		console.error = (...args) => lines.push({ stream: "stderr", text: args.join(" ") });
		try {
			await listMod.listModels(runtime, v.search);
		} finally {
			console.log = origLog;
			console.error = origErr;
		}
		records.push({
			fn: "listModels",
			catalogSource: "builtin",
			name: v.name,
			note: v.note,
			offline: true,
			modelsJson: raw,
			searchPattern: v.search ?? null,
			...(v.v8Dependent ? { v8Dependent: true } : {}),
			availableCount: (await runtime.getAvailable()).length,
			output: lines,
			stdout: `${lines
				.filter((l) => l.stream === "stdout")
				.map((l) => l.text)
				.join("\n")}\n`,
		});
	}

	writeFileSync(outFile, JSON.stringify(records), "utf-8");
}

// ===========================================================================
// D. config_paths.json
// ===========================================================================

/**
 * Every path accessor in config.ts, captured under BOTH resolution branches of
 * getAgentDir() plus three edge values of the env var.
 *
 * SAFETY: none of the agent-dir accessors touch the filesystem - they are pure
 * node:path.join calls over getAgentDir(), and getAgentDir() only reads the env
 * var and homedir(). So the "unset" branch (which names the user's real
 * ~/.pi/agent) COMPUTES that path and nothing more; its output is substituted to
 * {HOME}. The three package-asset accessors that do call existsSync only ever
 * probe inside the pi checkout, read-only.
 */
async function genConfigPaths(pkgDir) {
	const config = await impPi("config.ts");

	const AGENT_ACCESSORS = [
		"getAgentDir",
		"getCustomThemesDir",
		"getModelsPath",
		"getAuthPath",
		"getSettingsPath",
		"getToolsDir",
		"getBinDir",
		"getPromptsDir",
		"getSessionsDir",
		"getDebugLogPath",
	];

	const envAgentDir = join(TMPROOT, "config-branch", "agent");
	assertTemp(envAgentDir);

	const branches = [
		{
			branch: "env-set-absolute",
			note: "PI_CODING_AGENT_DIR is an absolute path: getAgentDir returns expandTildePath(envDir), which for a non-tilde absolute path is the value VERBATIM - it is NOT resolve()d, so it is not canonicalised and a relative value would stay relative",
			value: envAgentDir,
		},
		{
			branch: "env-set-tilde-only",
			note: "PI_CODING_AGENT_DIR=\"~\" expands to homedir() exactly",
			value: "~",
		},
		{
			branch: "env-set-tilde-slash",
			note: "PI_CODING_AGENT_DIR=\"~/custom-agent\" -> join(home, \"custom-agent\")",
			value: "~/custom-agent",
		},
		{
			branch: "env-set-tilde-backslash",
			note: "PI_CODING_AGENT_DIR=\"~\\\\custom-agent\" - the backslash form is only recognised when process.platform === \"win32\"",
			value: "~\\custom-agent",
		},
		{
			branch: "env-set-relative",
			note: "a RELATIVE env value is returned as-is; every accessor then joins onto a relative base",
			value: "rel/agent",
		},
		{
			branch: "env-set-empty-string",
			note: "EDGE: \"\" is falsy, so the `if (envDir)` guard fails and the homedir fallback is used - an empty env var behaves exactly like an unset one",
			value: "",
		},
		{
			branch: "unset",
			note: "the fallback: join(homedir(), CONFIG_DIR_NAME, \"agent\"). No filesystem access happens; this row is a pure path computation over the real home dir, substituted to {HOME}.",
			value: null,
		},
	];

	const byBranch = {};
	const prev = process.env.PI_CODING_AGENT_DIR;
	try {
		for (const b of branches) {
			if (b.value === null) delete process.env.PI_CODING_AGENT_DIR;
			else process.env.PI_CODING_AGENT_DIR = b.value;
			const paths = {};
			for (const name of AGENT_ACCESSORS) paths[name] = config[name]();
			byBranch[b.branch] = { note: b.note, envValue: b.value, paths };
		}
	} finally {
		if (prev === undefined) delete process.env.PI_CODING_AGENT_DIR;
		else process.env.PI_CODING_AGENT_DIR = prev;
	}

	// -- package-asset accessors (independent of PI_CODING_AGENT_DIR) ---------
	const packagePaths = {};
	for (const name of [
		"getPackageDir",
		"getThemesDir",
		"getExportTemplateDir",
		"getPackageJsonPath",
		"getReadmePath",
		"getDocsPath",
		"getExamplesPath",
		"getChangelogPath",
		"getInteractiveAssetsDir",
	]) {
		packagePaths[name] = config[name]();
	}
	packagePaths['getBundledInteractiveAssetPath("logo.png")'] = config.getBundledInteractiveAssetPath("logo.png");

	// -- expandTildePath / normalizePath -------------------------------------
	const tildeCases = [
		{ input: "~", note: "the bare tilde returns homedir() with no separator appended" },
		{ input: "~/x", note: "join(home, \"x\") - note join(), so the separator is native" },
		{ input: "~\\x", note: "win32 only: `process.platform === \"win32\" && startsWith(\"~\\\\\")`. On other platforms this is a LITERAL relative filename." },
		{ input: "~/", note: "slice(2) is the empty string, so join(home, \"\") normalises to home itself" },
		{ input: "~/a/b/c", note: "deeper path" },
		{ input: "~user/x", note: "NOT expanded: only \"~\", \"~/\" and (win32) \"~\\\\\" are recognised" },
		{ input: "~~", note: "NOT expanded" },
		{ input: "x/~/y", note: "a tilde that is not at position 0 is left alone" },
		{ input: "", note: "the empty string passes straight through" },
		{ input: "  ~/x  ", note: "expandTildePath passes no options, so `trim` defaults to FALSE and the leading spaces defeat the tilde check" },
		{ input: "/abs/path", note: "absolute POSIX path, returned verbatim (no resolve, no separator conversion)" },
		{ input: "C:\\abs\\path", note: "absolute win32 path, returned verbatim" },
		{ input: "relative/path", note: "relative path, returned verbatim" },
		{ input: "file:///C:/tmp/x", note: "the /^file:\\/\\//" + " branch runs fileURLToPath()" },
		{ input: "file:///tmp/x", note: "a POSIX file URL; on win32 fileURLToPath yields a rooted path with no drive" },
		{ input: "FILE:///tmp/x", note: "the regex is case SENSITIVE, so this is returned verbatim" },
		{ input: "a\u00a0b", note: "expandTildePath passes no options, so normalizeUnicodeSpaces is off and U+00A0 survives" },
		{ input: "@file.md", note: "stripAtPrefix is off by default, so the @ survives" },
	];
	const tilde = tildeCases.map((c) => {
		let result;
		let error;
		try {
			result = config.expandTildePath(c.input);
		} catch (err) {
			error = err instanceof Error ? `${err.name}: ${err.message}` : String(err);
		}
		return { input: c.input, note: c.note, ...(error === undefined ? { result } : { error }) };
	});

	// -- share viewer url ----------------------------------------------------
	const prevShare = process.env.PI_SHARE_VIEWER_URL;
	const share = [];
	try {
		delete process.env.PI_SHARE_VIEWER_URL;
		share.push({ env: null, note: "default base URL", input: "abc123", result: config.getShareViewerUrl("abc123") });
		process.env.PI_SHARE_VIEWER_URL = "https://example.test/s/";
		share.push({
			env: "https://example.test/s/",
			note: "PI_SHARE_VIEWER_URL overrides the base; the gist id is appended after a literal \"#\" with no separator normalisation",
			input: "abc123",
			result: config.getShareViewerUrl("abc123"),
		});
	} finally {
		if (prevShare === undefined) delete process.env.PI_SHARE_VIEWER_URL;
		else process.env.PI_SHARE_VIEWER_URL = prevShare;
	}

	emit(
		"config_paths.json",
		pretty(
			normalizeDeep({
				note: "Every path accessor in pi/packages/coding-agent/src/config.ts. Paths are raw node:path.join output, so separators and drive letters are native to `platform`. {HOME} is os.homedir(), {TMPROOT} the oracle temp root, {PIPKG} the pi coding-agent package root.",
				platform: PLATFORM,
				sep,
				identity: {
					APP_NAME: config.APP_NAME,
					CONFIG_DIR_NAME: config.CONFIG_DIR_NAME,
					ENV_AGENT_DIR: config.ENV_AGENT_DIR,
					ENV_SESSION_DIR: config.ENV_SESSION_DIR,
				},
				agentDirBranches: byBranch,
				packageAssetPaths: {
					note: "These depend on where pi is installed, not on Pi's logic. getThemesDir/getExportTemplateDir/getInteractiveAssetsDir each pick \"src\" or \"dist\" with existsSync(join(packageDir, \"src\")); this capture ran against a source checkout, so they resolved to src.",
					srcOrDist: existsSync(join(pkgDir, "src")) ? "src" : "dist",
					paths: packagePaths,
				},
				expandTildePath: tilde,
				getShareViewerUrl: share,
			}),
		),
	);
	return { branches: Object.keys(byBranch).length, tilde: tilde.length };
}

// ===========================================================================
// H. auth.json.cases.jsonl
// ===========================================================================

/**
 * Real AuthStorage round-trips against a real auth.json in a temp agent dir,
 * through the real FileAuthStorageBackend (proper-lockfile included). Every
 * `fileAfter` field is the EXACT bytes on disk, so the JSON shape - key order,
 * `type` first inside each entry, two-space indent, NO trailing newline - is
 * pinned rather than described.
 */
async function genAuthCases() {
	const authMod = await impPi("core/auth-storage.ts");
	const records = [];

	const readAuth = (p) =>
		existsSync(p) ? { content: readFileSync(p, "utf-8"), mode: octal(p) } : null;

	let n = 0;
	const freshAuthPath = (label) => {
		const dir = join(TMPROOT, `auth-${String(++n).padStart(2, "0")}-${label}`, "agent");
		assertTemp(dir);
		mkdirSync(dir, { recursive: true });
		return join(dir, "auth.json");
	};

	const record = (o) => records.push({ platform: PLATFORM, modeMeaningful: MODE_MEANINGFUL, ...o });

	// 1. constructing the store already materialises the file --------------------
	{
		const p = freshAuthPath("create");
		const store = authMod.AuthStorage.create(p);
		record({
			name: "create:constructor-materialises-an-empty-auth.json",
			note: "AuthStorage.create -> new FileAuthStorageBackend -> reload() -> withLock -> ensureParentDir(mode 0700) + ensureFileExists writes the literal two bytes \"{}\" with mode 0600 and then chmods it to 0600 again",
			ops: ["AuthStorage.create(authPath)"],
			fileBefore: null,
			fileAfter: readAuth(p),
			list: await store.list(),
		});
	}

	// 2. api-key entry ---------------------------------------------------------
	{
		const p = freshAuthPath("apikey");
		const store = authMod.AuthStorage.create(p);
		const seen = [];
		const written = await store.modify("anthropic", async (current) => {
			seen.push(current === undefined ? null : current);
			return { type: "api_key", key: "sk-oracle-1" };
		});
		record({
			name: "write:api-key-entry",
			note: "modify() persists JSON.stringify({...currentData, [provider]: next}, null, 2) - two-space indent, no trailing newline. The callback's `current` argument is undefined for a provider that has no entry yet.",
			ops: ['modify("anthropic", () => ({type:"api_key", key:"sk-oracle-1"}))'],
			callbackSawCurrent: seen,
			returned: written,
			fileAfter: readAuth(p),
			readBack: await store.read("anthropic"),
			list: await store.list(),
		});
	}

	// 3. oauth entry -----------------------------------------------------------
	{
		const p = freshAuthPath("oauth");
		const store = authMod.AuthStorage.create(p);
		const written = await store.modify("anthropic", async () => ({
			type: "oauth",
			refresh: "rt-oracle",
			access: "at-oracle",
			expires: 1730000000000,
		}));
		record({
			name: "write:oauth-token-entry",
			note: "an OAuthCredential is stored flat (no nesting under a \"tokens\" key); the property order inside the entry is the order the object literal was built in, which is what JSON.stringify emits",
			ops: ['modify("anthropic", () => ({type:"oauth", refresh, access, expires}))'],
			returned: written,
			fileAfter: readAuth(p),
			readBack: await store.read("anthropic"),
			list: await store.list(),
		});
	}

	// 4. both kinds together ---------------------------------------------------
	{
		const p = freshAuthPath("both");
		const store = authMod.AuthStorage.create(p);
		await store.modify("openai", async () => ({ type: "api_key", key: "sk-openai" }));
		await store.modify("anthropic", async () => ({ type: "oauth", refresh: "rt", access: "at", expires: 1730000000001 }));
		record({
			name: "write:both-kinds-provider-order-is-insertion-order",
			note: "`{...currentData, [provider]: next}` APPENDS a new provider, so the file's top-level key order is the order the providers were first written - NOT alphabetical",
			ops: ['modify("openai", api_key)', 'modify("anthropic", oauth)'],
			fileAfter: readAuth(p),
			readOpenai: await store.read("openai"),
			readAnthropic: await store.read("anthropic"),
			list: await store.list(),
		});
	}

	// 5. overwriting keeps position -------------------------------------------
	{
		const p = freshAuthPath("overwrite");
		const store = authMod.AuthStorage.create(p);
		await store.modify("a", async () => ({ type: "api_key", key: "1" }));
		await store.modify("b", async () => ({ type: "api_key", key: "2" }));
		const before = readAuth(p);
		await store.modify("a", async (current) => ({ type: "api_key", key: `${current?.key}-updated` }));
		record({
			name: "write:overwriting-an-existing-provider-keeps-its-POSITION",
			note: "the computed key already exists in the spread, so it is updated in place and stays first",
			ops: ['modify("a", ...)', 'modify("b", ...)', 'modify("a", current => ...)'],
			fileBefore: before,
			fileAfter: readAuth(p),
		});
	}

	// 6. undefined from the callback = no write --------------------------------
	{
		const p = freshAuthPath("noop");
		const store = authMod.AuthStorage.create(p);
		await store.modify("a", async () => ({ type: "api_key", key: "1" }));
		const before = readAuth(p);
		const returned = await store.modify("a", async () => undefined);
		record({
			name: "write:callback-returning-undefined-does-NOT-write",
			note: "`if (next === undefined) return { result: currentData[provider] }` - no `next` means withLockAsync skips writeFileSync entirely, and modify() resolves to the UNCHANGED current credential",
			ops: ['modify("a", () => undefined)'],
			fileBefore: before,
			returned,
			fileAfter: readAuth(p),
		});
	}

	// 7. delete ----------------------------------------------------------------
	{
		const p = freshAuthPath("delete");
		const store = authMod.AuthStorage.create(p);
		await store.modify("a", async () => ({ type: "api_key", key: "1" }));
		await store.modify("b", async () => ({ type: "api_key", key: "2" }));
		const before = readAuth(p);
		await store.delete("a");
		const afterDelete = readAuth(p);
		await store.delete("does-not-exist");
		record({
			name: "delete:removes-a-provider-and-rewrites-unconditionally",
			note: "delete() always returns a `next`, so deleting a provider that is not present still REWRITES the file (re-serialising it, which can change formatting of a hand-edited file)",
			ops: ['delete("a")', 'delete("does-not-exist")'],
			fileBefore: before,
			fileAfterDelete: afterDelete,
			fileAfterNoopDelete: readAuth(p),
			list: await store.list(),
		});
	}

	// 8. read() resolves configured api-key values ------------------------------
	{
		const p = freshAuthPath("resolve");
		const store = authMod.AuthStorage.create(p);
		const prevEnv = process.env.ORACLE_AUTH_KEY;
		process.env.ORACLE_AUTH_KEY = "sk-from-env";
		try {
			await store.modify("plain", async () => ({ type: "api_key", key: "sk-literal" }));
			await store.modify("envref", async () => ({ type: "api_key", key: "$ORACLE_AUTH_KEY" }));
			await store.modify("envbraces", async () => ({ type: "api_key", key: "${ORACLE_AUTH_KEY}" }));
			await store.modify("envmissing", async () => ({ type: "api_key", key: "$ORACLE_AUTH_KEY_MISSING" }));
			await store.modify("envprefixed", async () => ({ type: "api_key", key: "Bearer $ORACLE_AUTH_KEY" }));
			await store.modify("envoverride", async () => ({
				type: "api_key",
				key: "$ORACLE_AUTH_KEY",
				env: { ORACLE_AUTH_KEY: "sk-from-credential-env" },
			}));
			await store.modify("escaped", async () => ({ type: "api_key", key: "sk-$$literal" }));
			await store.modify("nokey", async () => ({ type: "api_key" }));
			await store.modify("oauth", async () => ({ type: "oauth", refresh: "rt", access: "at", expires: 1 }));
			const reads = {};
			for (const provider of ["plain", "envref", "envbraces", "envmissing", "envprefixed", "envoverride", "escaped", "nokey", "oauth", "absent"]) {
				const value = await store.read(provider);
				reads[provider] = value === undefined ? null : value;
			}
			record({
				name: "read:api-key-values-go-through-resolveConfigValue",
				note: "read() rewrites ONLY api_key credentials, as `{...credential, key: resolveConfigValue(credential.key, credential.env)}`. `credential.env` wins over process.env. A missing variable resolves to undefined, so the returned credential has NO key property at all. An api_key with no key, an oauth entry, and an absent provider are returned untouched. ORACLE_AUTH_KEY=\"sk-from-env\" was set for this record only.",
				ops: ["read(<each provider>)"],
				env: { ORACLE_AUTH_KEY: "sk-from-env" },
				fileAfter: readAuth(p),
				reads,
				list: await store.list(),
			});
		} finally {
			if (prevEnv === undefined) delete process.env.ORACLE_AUTH_KEY;
			else process.env.ORACLE_AUTH_KEY = prevEnv;
		}
	}

	// 9. round-trip through the writer, byte for byte --------------------------
	{
		const p = freshAuthPath("roundtrip");
		const store = authMod.AuthStorage.create(p);
		await store.modify("anthropic", async () => ({ type: "oauth", refresh: "rt", access: "at", expires: 1730000000002 }));
		await store.modify("openai", async () => ({ type: "api_key", key: "sk-rt" }));
		const bytes = readFileSync(p, "utf-8");
		const reparsed = JSON.parse(bytes);
		const store2 = authMod.AuthStorage.create(p);
		record({
			name: "roundtrip:writer-output-reparsed-and-reloaded",
			note: "the exact on-disk bytes, the value JSON.parse gives back, and what a FRESH AuthStorage reads from the same file - so a Rust writer can be asserted byte-for-byte",
			ops: ["modify x2", "readFileSync", "JSON.parse", "AuthStorage.create(samePath)"],
			fileAfter: readAuth(p),
			reparsedKeyOrder: Object.keys(reparsed),
			reparsed,
			freshStoreReadAnthropic: await store2.read("anthropic"),
			freshStoreReadOpenai: await store2.read("openai"),
			freshStoreList: await store2.list(),
			readStoredCredential: {
				anthropic: authMod.readStoredCredential("anthropic", p) ?? null,
				openai: authMod.readStoredCredential("openai", p) ?? null,
				absent: authMod.readStoredCredential("absent", p) ?? null,
			},
		});
	}

	// 10. malformed file -------------------------------------------------------
	{
		const p = freshAuthPath("malformed");
		const store = authMod.AuthStorage.create(p);
		await store.modify("a", async () => ({ type: "api_key", key: "good" }));
		const before = readAuth(p);
		writeFileSync(p, "{ not json", "utf-8");
		store.reload();
		record({
			name: "malformed:reload-keeps-the-last-valid-snapshot",
			note: "reload()'s catch is empty, so a corrupted auth.json leaves the in-memory data untouched and NO error surfaces. readStoredCredential's catch returns undefined instead.",
			ops: ["modify", "corrupt the file externally", "reload()"],
			fileBefore: before,
			fileAfter: readAuth(p),
			readAfterCorruption: (await store.read("a")) ?? null,
			readStoredCredentialAfterCorruption: authMod.readStoredCredential("a", p) ?? null,
			readStoredCredentialOnMissingFile: authMod.readStoredCredential("a", join(dirname(p), "nope.json")) ?? null,
		});
	}

	// 11. in-memory backend ----------------------------------------------------
	{
		const store = authMod.AuthStorage.inMemory({
			anthropic: { type: "oauth", refresh: "rt", access: "at", expires: 5 },
			openai: { type: "api_key", key: "sk-mem" },
		});
		record({
			name: "inMemory:same-serialisation-without-a-file",
			note: "AuthStorage.inMemory seeds InMemoryAuthStorageBackend with JSON.stringify(data, null, 2), i.e. the identical serialisation the file backend writes - useful as a no-IO reference for the Rust port",
			ops: ["AuthStorage.inMemory({...})"],
			fileAfter: null,
			readAnthropic: await store.read("anthropic"),
			readOpenai: await store.read("openai"),
			list: await store.list(),
		});
	}

	emit("auth.json.cases.jsonl", jsonl(records.map(normalizeDeep)));
	return records;
}

// ===========================================================================
// main
// ===========================================================================
async function main() {
	const cfg = await impPi("config.ts");
	const PKG_DIR = cfg.getPackageDir();
	// Longest-first: PKG_DIR lives under HOME on most machines, so it must be
	// substituted before HOME. rebuildSubstitutions sorts by length for us.
	rebuildSubstitutions([
		[TMPROOT, "{TMPROOT}"],
		[PKG_DIR, "{PIPKG}"],
		[HOME, "{HOME}"],
	]);

	const summary = [];
	const argsCorpus = await genArgsCorpus();
	summary.push(`args.corpus.jsonl        : ${argsCorpus.records.length} cases (purity: ${JSON.stringify(argsCorpus.purity)})`);
	const helpSizes = await genHelpGoldens();
	summary.push(`help goldens             : ${JSON.stringify(helpSizes)}`);
	const sessionDir = await genSessionDirCases();
	summary.push(`session_dir.cases.jsonl  : ${sessionDir.length} cases`);
	const sessionMigration = await genSessionMigrationCases();
	summary.push(`session_migration.cases.jsonl: ${sessionMigration.length} cases`);
	const appMode = await genAppModeCases();
	summary.push(`app_mode.cases.jsonl     : ${appMode.length} cases`);
	const settings = await genSettingsCases();
	summary.push(`settings.merge.cases.jsonl: ${settings.length} cases`);
	const migrationCases = await genMigrationCases();
	summary.push(`migrations.cases.jsonl   : ${migrationCases.length} cases`);
	const configPaths = await genConfigPaths(PKG_DIR);
	summary.push(`config_paths.json        : ${configPaths.branches} agent-dir branches, ${configPaths.tilde} tilde cases`);
	const authCases = await genAuthCases();
	summary.push(`auth.json.cases.jsonl    : ${authCases.length} cases`);
	const modelPure = await genModelResolutionCases();
	const modelRuntimeRecords = await genModelRuntimeCases();
	const modelRecords = [...modelPure, ...modelRuntimeRecords];
	emit("models.cases.jsonl", jsonl(modelRecords.map(normalizeDeep)));
	summary.push(`models.cases.jsonl       : ${modelRecords.length} cases (${modelPure.length} pure, ${modelRuntimeRecords.length} via a real ModelRuntime)`);

	// -- write or check ------------------------------------------------------
	mkdirSync(OUT, { recursive: true });
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
				console.error(`DRIFT    ${rel}: ${existing.length} -> ${contents.length} bytes, first differing line ${firstDiff + 1}`);
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
		console.error(`\nDRIFT: ${drift} cli fixture(s) are stale; run node scripts/gen-cli-oracle.mjs`);
		process.exitCode = 1;
	}

	console.log("\n=== SUMMARY ===");
	for (const line of summary) console.log(line);
}

const modelsArgIndex = process.argv.indexOf("--emit-models");

if (modelsArgIndex !== -1) {
	// CHILD MODE: capture everything that needs a real ModelRuntime, under the
	// parent's allowlisted environment. Records go to a FILE, never stdout,
	// because listModels() writes to stdout itself.
	TMPROOT = process.argv[modelsArgIndex + 1];
	await emitModelRuntimeRecords(process.argv[modelsArgIndex + 1], process.argv[modelsArgIndex + 2]);
} else if (HELP_VARIANT === "deprecation") {
	// CHILD MODE: showDeprecationWarnings() awaits a keypress, so it can only be
	// driven with a real stdin. The parent supplies one byte.
	const migrations = await impPi("migrations.ts");
	await migrations.showDeprecationWarnings(["first warning text", "second warning text"]);
} else if (HELP_VARIANT !== null) {
	// CHILD MODE: print printHelp() to stdout and nothing else. No temp dirs, no
	// process.exit (so Node flushes the stdout pipe naturally).
	const args = await impPi("cli/args.ts");
	args.printHelp(HELP_VARIANT.endsWith("-ext") ? EXT_FLAGS : undefined);
} else {
	TMPROOT = mkdtempSync(join(tmpdir(), "pi-cli-oracle-"));
	try {
		await main();
	} finally {
		rmSync(TMPROOT, { recursive: true, force: true });
	}
}
