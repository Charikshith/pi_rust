#!/usr/bin/env node
// feat-012 Wave 1 oracle: drive REAL `runRpcMode` from ../pi and capture the exact
// stdout protocol lines for a fixed command script.
//
// Fixtures written (tests/fixtures/pi/rpc/):
//   requests.corpus.jsonl   the exact JSONL lines fed to stdin, in order
//   responses.corpus.jsonl  one record per captured stdout line: {"line": "..."}
//   meta.json               exit code + capture notes
//
// THE SEAM: runRpcMode reads only duck-typed members off the runtime host
// (`session`, `setRebindSession`, `newSession`, `switchSession`, `fork`,
// `dispose`) — the same shape pi's own print-mode tests stub. We feed it a stub
// session whose every member is deterministic; the REAL rpc-mode code does all
// dispatch/serialization through the real output-guard + jsonl + json-event
// modules. The child's stdin/stdout are real OS pipes, so framing, LF handling,
// takeOverStdout routing, response ordering and JSON.stringify key order are all
// Pi-literal.
//
// Usage:
//   node scripts/gen-rpc-oracle.mjs            # regenerate fixtures
//   node scripts/gen-rpc-oracle.mjs --check    # exit 1 if fixtures would change
//
// Requires the sibling Pi checkout at ../pi (Node type-stripping, no dist build).

import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join, sep } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { tmpdir } from "node:os";
import { register } from "node:module";

const here = dirname(fileURLToPath(import.meta.url));
const pirustRoot = dirname(here);
const piRoot = join(pirustRoot, "..", "pi");
const PKGS = join(piRoot, "packages");
const CA = join(PKGS, "coding-agent", "src");
const OUT = join(pirustRoot, "tests", "fixtures", "pi", "rpc");
const SELF = fileURLToPath(import.meta.url);

const argv = process.argv.slice(2);
const CHECK = argv.includes("--check");
const roleIndex = argv.indexOf("--role");
const ROLE = roleIndex === -1 ? null : argv[roleIndex + 1];
const specIndex = argv.indexOf("--spec");
const SPEC_FILE = specIndex === -1 ? null : argv[specIndex + 1];
const outIndex = argv.indexOf("--out");
const OUT_FILE = outIndex === -1 ? null : argv[outIndex + 1];

if (!existsSync(join(CA, "modes", "rpc", "rpc-mode.ts"))) {
	console.error(`Pi rpc sources not found at ${CA}; fixtures cannot be regenerated.`);
	process.exit(CHECK ? 0 : 1); // don't fail --check when the source repo is simply absent
}

// ---------------------------------------------------------------------------
// Bare-specifier aliases (same pattern as gen-printmode-oracle.mjs)
// ---------------------------------------------------------------------------
const PKG_ROOTS = {
	"@earendil-works/pi-ai": join(PKGS, "ai", "src"),
	"@earendil-works/pi-agent-core": join(PKGS, "agent", "src"),
	"@earendil-works/pi-tui": join(PKGS, "tui", "src"),
	"@earendil-works/pi-telemetry": join(PKGS, "telemetry", "src"),
};

function buildHooks() {
	const roots = Object.fromEntries(
		Object.entries(PKG_ROOTS)
			.filter(([, dir]) => existsSync(dir))
			.map(([spec, dir]) => [spec, pathToFileURL(dir + sep).href]),
	);
	return (
		"data:text/javascript," +
		encodeURIComponent(`
import { existsSync } from "node:fs";
import { fileURLToPath } from "node:url";
const ROOTS = ${JSON.stringify(roots)};
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
`)
	);
}

register(buildHooks(), import.meta.url);

const impPi = (rel) => import(pathToFileURL(join(CA, ...rel.split("/"))).href);

// ---------------------------------------------------------------------------
// Spec: the exact stdin script + every stub value the responses will contain.
// All of it must be deterministic — no ids, no timestamps, no Math.random.
// ---------------------------------------------------------------------------
const NOW = 1700000000000;

function buildSpec() {
	return {
		stub: {
			sessionId: "sess_rpc_test_0000",
			sessionName: undefined, // pinned OMITTED key in get_state
			sessionFile: undefined, // pinned OMITTED key in get_state
			thinkingLevel: "off",
			isStreaming: false,
			isCompacting: false,
			steeringMode: "all",
			followUpMode: "all",
			autoCompactionEnabled: true,
			cwd: "{PROJECTDIR}",
			messages: [
				{ role: "user", content: "hello world", timestamp: NOW },
				{
					role: "assistant",
					content: [{ type: "text", text: "hi there" }],
					api: "anthropic-messages",
					provider: "anthropic",
					model: "claude-opus-4-8",
					usage: {
						input: 10,
						output: 5,
						cacheRead: 0,
						cacheWrite: 0,
						totalTokens: 15,
						cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
					},
					stopReason: "stop",
					timestamp: NOW,
				},
			],
			pendingMessageCount: 0,
			lastAssistantText: "hi there",
			entries: [
				{ id: "e1", parentId: null, type: "message", message: { role: "user" } },
				{ id: "e2", parentId: "e1", type: "message", message: { role: "assistant" } },
			],
			leafId: "e2",
			tree: [
				{
					entry: { id: "e1", parentId: null, type: "message", message: { role: "user" } },
					children: [
						{
							entry: { id: "e2", parentId: "e1", type: "message", message: { role: "assistant" } },
							children: [],
						},
					],
				},
			],
			forkMessages: [{ entryId: "e1", text: "hello world" }],
			stats: {
				sessionFile: undefined,
				sessionId: "sess_rpc_test_0000",
				userMessages: 1,
				assistantMessages: 1,
				toolCalls: 0,
				toolResults: 0,
				totalMessages: 2,
				tokens: { input: 10, output: 5, cacheRead: 0, cacheWrite: 0, total: 15 },
				cost: 0.000037,
			},
			compactionResult: {
				summary: "[stub summary]",
				firstKeptEntryId: "e2",
				tokensBefore: 1234,
			},
			bashResult: {
				output: "hi\n",
				exitCode: 0,
				cancelled: false,
				truncated: false,
			},
			cycleThinkingLevelResult: "medium",
			availableThinkingLevels: ["off", "minimal", "low", "medium", "high"],
			fauxModelRef: "faux", // which snapshot model set_model/get_available_models use
			promptEvents: [
				{ type: "queue_update", steering: [], followUp: [] },
				{ type: "agent_settled" },
			],
		},
		commands: [
			// 1. invalid JSON -> parse error response (no id)
			"{not json",
			// 2. get_state with no id -> success without id key
			'{"type":"get_state"}',
			// extension_ui_response with unknown id -> silently ignored, NO output
			'{"type":"extension_ui_response","id":"nonexistent","value":"x"}',
			'{"id":"a","type":"set_thinking_level","level":"high"}',
			'{"id":"b","type":"cycle_thinking_level"}',
			'{"id":"c","type":"get_available_thinking_levels"}',
			'{"id":"d","type":"set_steering_mode","mode":"one-at-a-time"}',
			'{"id":"e","type":"set_follow_up_mode","mode":"all"}',
			'{"id":"f","type":"compact","customInstructions":"focus on tests"}',
			'{"type":"compact"}',
			'{"id":"g","type":"set_auto_compaction","enabled":false}',
			'{"id":"h","type":"set_auto_retry","enabled":true}',
			'{"id":"i","type":"abort_retry"}',
			'{"id":"j","type":"bash","command":"echo hi"}',
			'{"id":"k","type":"abort_bash"}',
			'{"id":"l","type":"abort"}',
			'{"id":"m","type":"steer","message":"mid-run change"}',
			'{"id":"n","type":"follow_up","message":"after this"}',
			'{"id":"o","type":"new_session"}',
			'{"id":"p","type":"switch_session","sessionPath":"{TMP}/other.session.jsonl"}',
			'{"id":"q","type":"fork","entryId":"e1"}',
			'{"id":"r","type":"clone"}',
			'{"id":"s","type":"get_fork_messages"}',
			'{"id":"t","type":"get_entries"}',
			'{"id":"u","type":"get_entries","since":"nope"}',
			'{"id":"v","type":"get_tree"}',
			'{"id":"w","type":"get_last_assistant_text"}',
			'{"id":"x","type":"set_session_name","name":"   "}',
			'{"id":"y","type":"set_session_name","name":"  My Session  "}',
			'{"id":"z","type":"get_messages"}',
			'{"id":"aa","type":"get_commands"}',
			'{"id":"ab","type":"get_session_stats"}',
			'{"id":"ac","type":"set_model","provider":"faux","modelId":"missing-model"}',
			'{"id":"ad","type":"set_model","provider":"faux","modelId":"faux-1"}',
			'{"id":"ae","type":"cycle_model"}',
			'{"id":"af","type":"get_available_models"}',
			'{"type":"bogus_command"}',
			// prompt LAST: async, emits events after the response line.
			'{"id":"ag","type":"prompt","message":"hello again","streamingBehavior":"followUp"}',
		],
		// Per-command completion predicate over the RAW stdout lines: the parent
		// sends command[i] only after responses[0..i-1] have all been observed.
		// Fields: needle = required substring; all = true when several matching
		// lines must exist by now (duplicate commands); none = expect NO output.
		wait: [
			{ needle: '"command":"parse"' },
			{ needle: '"command":"get_state"' },
			{ none: true }, // unknown extension_ui_response id -> silently ignored
			{ needle: '"id":"a","type":"response","command":"set_thinking_level"' },
			{ needle: '"id":"b","type":"response","command":"cycle_thinking_level"' },
			{ needle: '"id":"c","type":"response","command":"get_available_thinking_levels"' },
			{ needle: '"id":"d","type":"response","command":"set_steering_mode"' },
			{ needle: '"id":"e","type":"response","command":"set_follow_up_mode"' },
			{ needle: '"id":"f","type":"response","command":"compact"' },
			{ needle: '"command":"compact"', all: 2 },
			{ needle: '"id":"g","type":"response","command":"set_auto_compaction"' },
			{ needle: '"id":"h","type":"response","command":"set_auto_retry"' },
			{ needle: '"id":"i","type":"response","command":"abort_retry"' },
			{ needle: '"id":"j","type":"response","command":"bash"' },
			{ needle: '"id":"k","type":"response","command":"abort_bash"' },
			{ needle: '"id":"l","type":"response","command":"abort"' },
			{ needle: '"id":"m","type":"response","command":"steer"' },
			{ needle: '"id":"n","type":"response","command":"follow_up"' },
			{ needle: '"id":"o","type":"response","command":"new_session"' },
			{ needle: '"id":"p","type":"response","command":"switch_session"' },
			{ needle: '"id":"q","type":"response","command":"fork"' },
			{ needle: '"id":"r","type":"response","command":"clone"' },
			{ needle: '"id":"s","type":"response","command":"get_fork_messages"' },
			{ needle: '"id":"t","type":"response","command":"get_entries"' },
			{ needle: '"id":"u","type":"response","command":"get_entries"' },
			{ needle: '"id":"v","type":"response","command":"get_tree"' },
			{ needle: '"id":"w","type":"response","command":"get_last_assistant_text"' },
			{ needle: '"id":"x","type":"response","command":"set_session_name"' },
			{ needle: '"id":"y","type":"response","command":"set_session_name"' },
			{ needle: '"id":"z","type":"response","command":"get_messages"' },
			{ needle: '"id":"aa","type":"response","command":"get_commands"' },
			{ needle: '"id":"ab","type":"response","command":"get_session_stats"' },
			{ needle: '"id":"ac","type":"response","command":"set_model"' },
			{ needle: '"id":"ad","type":"response","command":"set_model"' },
			{ needle: '"id":"ae","type":"response","command":"cycle_model"' },
			{ needle: '"id":"af","type":"response","command":"get_available_models"' },
			{ needle: '"command":"bogus_command"' },
			{ needle: '"type":"agent_settled"' },
		],
	};
}

// ---------------------------------------------------------------------------
// CHILD ROLE: build the stub runtime and hand it to the REAL runRpcMode.
// ---------------------------------------------------------------------------
async function roleCapture() {
	process.env.NO_COLOR = "1";
	process.env.PI_OFFLINE = "1";
	const spec = JSON.parse(readFileSync(SPEC_FILE, "utf-8"));
	const S = spec.stub;

	const { runRpcMode } = await impPi("modes/rpc/rpc-mode.ts");
	const { takeOverStdout } = await impPi("core/output-guard.ts");
	const compat = await import(pathToFileURL(join(PKGS, "ai", "src", "compat.ts")).href);
	const reg = compat.registerFauxProvider({ tokenSize: { min: 3, max: 3 } });
	const fauxModel = reg.getModel();

	const meta = {
		bindExtensionsCalls: [],
		setModelCalls: [],
		setThinkingLevelCalls: [],
		recordedBash: [],
		promptCalls: [],
		disposed: false,
	};

	let listeners = [];
	let leafId = S.leafId;
	let sessionName = S.sessionName;

	const sessionManager = {
		getCwd: () => "/proj",
		getEntries: () => S.entries,
		getTree: () => S.tree,
		getLeafId: () => leafId,
	};

	const session = {
		sessionManager,
		model: fauxModel,
		thinkingLevel: S.thinkingLevel,
		isStreaming: S.isStreaming,
		isCompacting: S.isCompacting,
		steeringMode: S.steeringMode,
		followUpMode: S.followUpMode,
		sessionFile: S.sessionFile,
		sessionId: S.sessionId,
		sessionName,
		autoCompactionEnabled: S.autoCompactionEnabled,
		messages: S.messages,
		pendingMessageCount: S.pendingMessageCount,
		modelRuntime: { getAvailableSnapshot: () => [fauxModel] },
		cycleModel: async () => ({ model: fauxModel, thinkingLevel: "medium", isScoped: false }),
		setModel: async (model) => {
			meta.setModelCalls.push(model.id);
		},
		getSessionStats: () => S.stats,
		promptTemplates: [],
		resourceLoader: { getSkills: () => ({ skills: [] }) },
		extensionRunner: {
			getRegisteredCommands: () => [],
			emitUserBash: async () => undefined,
		},
		bindExtensions: async (bindings) => {
			meta.bindExtensionsCalls.push({ mode: bindings.mode });
		},
		subscribe: (listener) => {
			listeners.push(listener);
			return () => {
				listeners = listeners.filter((l) => l !== listener);
			};
		},
		waitForIdle: async () => {},
		reload: async () => {},
		navigateTree: async () => ({ cancelled: false }),
		agent: { subscribe: (_cb) => () => {} },
		setThinkingLevel(level) {
			meta.setThinkingLevelCalls.push(level);
			this.thinkingLevel = level;
		},
		cycleThinkingLevel: () => S.cycleThinkingLevelResult,
		getAvailableThinkingLevels: () => S.availableThinkingLevels,
		setSteeringMode(mode) {
			this.steeringMode = mode;
		},
		setFollowUpMode(mode) {
			this.followUpMode = mode;
		},
		setAutoCompactionEnabled(enabled) {
			this.autoCompactionEnabled = enabled;
		},
		setAutoRetryEnabled: () => {},
		abortRetry: () => {},
		abortBash: () => {},
		abort: async () => {},
		steer: async () => {},
		followUp: async () => {},
		async compact(_customInstructions) {
			return S.compactionResult;
		},
		async executeBash(command, _cwd, options) {
			return { ...S.bashResult, __rpcId: options?.id ?? null };
		},
		recordBashResult(command, result, _opts) {
			meta.recordedBash.push({ command, result });
		},
		getLastAssistantText: () => S.lastAssistantText,
		setSessionName(name) {
			sessionName = name;
		},
		getUserMessagesForForking: () => S.forkMessages,
		exportToHtml: async (outputPath) => outputPath ?? "{PROJECTDIR}/session.html",
		async prompt(text, options) {
			meta.promptCalls.push({
				text,
				source: options?.source ?? null,
				streamingBehavior: options?.streamingBehavior ?? null,
			});
			// Async boundary first, like a real prompt's preflight path.
			await Promise.resolve();
			if (options?.preflightResult) options.preflightResult(true);
			for (const event of S.promptEvents) {
				for (const l of [...listeners]) l(event);
			}
		},
	};

	const runtimeHost = {
		get session() {
			return session;
		},
		setRebindSession(fn) {
			meta.rebindSessionRegistered = typeof fn === "function";
		},
		async newSession(_options) {
			return { cancelled: false };
		},
		async switchSession(sessionPath) {
			return { cancelled: sessionPath.endsWith("cancelled") };
		},
		async fork(entryId, forkOptions) {
			return {
				selectedText: entryId === "e1" ? "hello world" : "hi there",
				cancelled: Boolean(forkOptions?.cancel),
			};
		},
		async dispose() {
			meta.disposed = true;
		},
	};

	if (OUT_FILE) {
		writeFileSync(OUT_FILE + ".meta.tmp", JSON.stringify(meta), "utf-8");
	}
	takeOverStdout();
	try {
		await runRpcMode(runtimeHost);
	} catch (err) {
		// shutdown() calls process.exit internally on the stdin-end path; any other
		// error is a capture failure.
		console.error("runRpcMode threw:", err);
		process.exit(1);
	}
}

// ---------------------------------------------------------------------------
// PARENT ROLE: spawn the child, feed commands, capture stdout lines.
// ---------------------------------------------------------------------------
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function runParent() {
	const { spawn } = await import("node:child_process");
	mkdirSync(OUT, { recursive: true });

	const spec = buildSpec();
	const specFile = join(tmpdir(), `pirust-rpc-spec-${process.pid}.json`);
	writeFileSync(specFile, JSON.stringify(spec), "utf-8");

	const child = spawn(process.execPath, [SELF, "--role", "capture", "--spec", specFile], {
		stdio: ["pipe", "pipe", "pipe"],
		env: process.env,
	});

	// Feed the script LOCK-STEP: command[i] is written only after every response
	// its predecessors must have produced has been observed on stdout. Without
	// this, Pi's genuinely-async handleInputLine handlers interleave differently
	// run to run and the captured tape order would be nondeterministic.
	const rawLines = [];
	let lineBuf = "";
	child.stdout.on("data", (chunk) => {
		lineBuf += chunk;
	});
	const waitFor = (pred, timeoutMs = 15000) =>
		new Promise((resolve, reject) => {
			const started = Date.now();
			const tick = () => {
				let idx;
				while ((idx = lineBuf.indexOf("\n")) !== -1) {
					rawLines.push(lineBuf.slice(0, idx));
					lineBuf = lineBuf.slice(idx + 1);
				}
				if (rawLines.some(pred)) return resolve();
				if (Date.now() - started > timeoutMs) {
					return reject(new Error(`timeout waiting for ${JSON.stringify(pred)}\nhave:\n${rawLines.join("\n")}`));
				}
				setTimeout(tick, 10);
			};
			tick();
		});

	for (let i = 0; i < spec.commands.length; i++) {
		const w = spec.wait[i];
		child.stdin.write(spec.commands[i] + "\n");
		if (w.none) {
			await sleep(60);
		} else {
			await waitFor((l) => l.includes(w.needle));
		}
	}
	child.stdin.end();

	const exitCode = await new Promise((resolve) => child.on("close", resolve));

	if (exitCode !== 0) {
		console.error(`capture child exited ${exitCode}\n--- stderr ---\n${stderrBuf}`);
		process.exit(1);
	}

	// Normalize machine-specific paths and the faux provider's per-run api marker
	// ("faux:<Date.now()>:<random>" — registerFauxProvider stamps the model at import).
	const TMP_REAL = dirname(specFile);
	const normalize = (text) =>
		text.split(TMP_REAL).join("{TMP}").replace(/faux:\d+:[a-z0-9]+/g, "faux:{FAUX_API}");

	const requests = spec.commands;
	const responses = rawLines.map((line) => ({ line: normalize(line) }));
	const meta = existsSync(specFile + "")
		? {}
		: {};

	const requestsText = requests.map((l) => l).join("\n") + "\n";
	const responsesText = responses.map((r) => JSON.stringify(r)).join("\n") + "\n";

	if (CHECK) {
		const oldReq = readFileSync(join(OUT, "requests.corpus.jsonl"), "utf-8");
		const oldRes = readFileSync(join(OUT, "responses.corpus.jsonl"), "utf-8");
		if (oldReq !== requestsText || oldRes !== responsesText) {
			console.error("gen-rpc-oracle: --check FAILED (fixtures would change)");
			process.exit(1);
		}
		console.log("gen-rpc-oracle: --check OK (no drift)");
		process.exit(0);
	}

	writeFileSync(join(OUT, "requests.corpus.jsonl"), requestsText, "utf-8");
	writeFileSync(join(OUT, "responses.corpus.jsonl"), responsesText, "utf-8");
	writeFileSync(
		join(OUT, "meta.json"),
		JSON.stringify(
			{
				exitCode,
				commandCount: requests.length,
				responseCount: responses.length,
				notes:
					"Captured by driving real packages/coding-agent/src/modes/rpc/rpc-mode.ts runRpcMode in a child process over real OS pipes with a stub runtime host/session. {TMP} normalizes the temp spec dir. Stub values are listed in gen-rpc-oracle.mjs buildSpec().",
			},
			null,
			2,
		) + "\n",
		"utf-8",
	);
	console.log(`gen-rpc-oracle: wrote ${requests.length} requests / ${responses.length} responses`);
}

if (ROLE === "capture") {
	await roleCapture();
} else {
	await runParent();
}
