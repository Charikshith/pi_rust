#!/usr/bin/env node
// gen-sdk-oracle.mjs
//
// GOLDEN ORACLE for feat-005 Wave 4 (sdk.rs sub-waves 4a/4b). Every byte below is
// produced by EXECUTING Pi's own TypeScript source under Node's native type
// stripping — nothing here is a reimplementation or a hand-authored expectation.
//
// Run:      cd pirust && node scripts/gen-sdk-oracle.mjs
// Verify:   cd pirust && node scripts/gen-sdk-oracle.mjs --check
//
// ---------------------------------------------------------------------------
// OUTPUTS  (tests/fixtures/pi/sdk/)
// ---------------------------------------------------------------------------
//   system-prompt.cases.jsonl        real `buildSystemPrompt()` output across
//                                    representative option combinations (4a).
//   provider-attribution.cases.jsonl real `mergeProviderAttributionHeaders()`
//                                    output across representative
//                                    model/settings/header-source combos (4b).
//
// ---------------------------------------------------------------------------
// DETERMINISM
// ---------------------------------------------------------------------------
// `PI_PACKAGE_DIR` is pinned to the sentinel `C:\oracle\pkg` before every
// buildSystemPrompt() call, so `getReadmePath()`/`getDocsPath()`/`getExamplesPath()`
// are deterministic. pirust has no equivalent installed-package layout (see
// `config.rs`'s `get_package_dir` module docs), so the Rust golden test injects the
// SAME sentinel directly via `build_system_prompt_with_paths` rather than trying to
// reproduce `getPackageDir()`'s Bun/Node-dist-walk logic.

import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { register } from "node:module";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const CHECK = process.argv.includes("--check");

const here = dirname(fileURLToPath(import.meta.url));
const pirustRoot = dirname(here);
const piRoot = join(pirustRoot, "..", "pi");
const PKGS = join(piRoot, "packages");
const CA = join(PKGS, "coding-agent", "src");
const OUT = join(pirustRoot, "tests", "fixtures", "pi", "sdk");

if (!existsSync(join(CA, "core", "sdk.ts"))) {
	console.error(`Pi coding-agent sources not found at ${CA}; fixtures cannot be regenerated.`);
	process.exit(CHECK ? 0 : 1); // don't fail --check when the source repo is simply absent
}

process.env.PI_OFFLINE = "1";
process.env.PI_PACKAGE_DIR = "C:\\oracle\\pkg"; // sentinel — see module docs above

// ---------------------------------------------------------------------------
// Bare-specifier alias hook -> Pi's src/index.ts (dist is not built)
// ---------------------------------------------------------------------------
const ALIASES = {
	"@earendil-works/pi-ai": join(PKGS, "ai", "src", "index.ts"),
	"@earendil-works/pi-ai/compat": join(PKGS, "ai", "src", "compat.ts"),
	"@earendil-works/pi-agent-core": join(PKGS, "agent", "src", "index.ts"),
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

const impCa = (rel) => import(pathToFileURL(join(CA, ...rel.split("/"))).href);

const artifacts = new Map();
const emit = (relPath, records) => artifacts.set(relPath, records.map((r) => JSON.stringify(r)).join("\n") + "\n");

// ===========================================================================
// A. system-prompt.cases.jsonl (4a)
// ===========================================================================
async function genSystemPromptCases() {
	const { buildSystemPrompt } = await impCa("core/system-prompt.ts");
	const records = [];
	const c = (note, options) => records.push({ note, options, result: buildSystemPrompt(options) });

	c("default-tools", { cwd: "/proj" });
	c("selected-tools-with-snippets", {
		cwd: "/proj",
		selectedTools: ["read", "grep", "find"],
		toolSnippets: { read: "read files", grep: "search file contents" },
	});
	c("bash-only-no-explore-tools-gets-the-bash-guideline", {
		cwd: "/proj",
		selectedTools: ["bash"],
		toolSnippets: { bash: "run shell commands" },
	});
	c("bash-plus-grep-suppresses-the-bash-guideline", {
		cwd: "/proj",
		selectedTools: ["bash", "grep"],
		toolSnippets: { bash: "run shell commands", grep: "search file contents" },
	});
	c("prompt-guidelines-trimmed-and-deduped-against-defaults", {
		cwd: "/proj",
		promptGuidelines: ["  Be concise in your responses  ", "", "   ", "Prefer small diffs", "Prefer small diffs"],
	});
	c("append-system-prompt", { cwd: "/proj", appendSystemPrompt: "Extra house rules." });
	c("context-files", {
		cwd: "/proj",
		contextFiles: [{ path: "/proj/AGENTS.md", content: "Follow the ladder." }],
	});
	c("windows-cwd-backslashes-become-forward-slashes", { cwd: "C:\\Users\\me\\proj" });
	c("custom-prompt-short-circuits-the-template", {
		cwd: "/proj",
		customPrompt: "You are a minimal assistant.",
	});
	c("custom-prompt-plus-append-and-context-files", {
		cwd: "/proj",
		customPrompt: "You are a minimal assistant.",
		appendSystemPrompt: "Extra rules.",
		contextFiles: [{ path: "/proj/AGENTS.md", content: "Follow the ladder." }],
	});
	c("no-tool-snippets-renders-none", { cwd: "/proj", selectedTools: ["read"], toolSnippets: {} });

	emit("system-prompt.cases.jsonl", records);
	return records.length;
}

// ===========================================================================
// B. provider-attribution.cases.jsonl (4b)
// ===========================================================================
async function genProviderAttributionCases() {
	const { mergeProviderAttributionHeaders } = await impCa("core/provider-attribution.ts");
	const sm = await impCa("core/settings-manager.ts");

	const settingsWith = (enableInstallTelemetry) => {
		const storage = new sm.InMemorySettingsStorage();
		storage.withLock("global", () => (enableInstallTelemetry === undefined ? {} : { enableInstallTelemetry }));
		return sm.SettingsManager.fromStorage(storage, {});
	};
	const defaultSettings = settingsWith(undefined); // default true
	const noTelemetrySettings = settingsWith(false);

	const records = [];
	const c = (note, model, settings, sessionId, headerSources) =>
		records.push({
			note,
			model,
			enableInstallTelemetry: settings === noTelemetrySettings ? false : true,
			sessionId: sessionId ?? null,
			headerSources: headerSources ?? [],
			result: mergeProviderAttributionHeaders(model, settings, sessionId, ...(headerSources ?? [])) ?? null,
		});

	c("plain-anthropic-model-gets-nothing", { provider: "anthropic", baseUrl: "https://api.anthropic.com" }, defaultSettings);
	c("openrouter-by-provider-id", { provider: "openrouter", baseUrl: "https://openrouter.ai/api/v1" }, defaultSettings);
	c(
		"openrouter-by-base-url-host",
		{ provider: "custom", baseUrl: "https://openrouter.ai/api/v1" },
		defaultSettings,
	);
	c("nvidia-nim-by-host", { provider: "custom", baseUrl: "https://integrate.api.nvidia.com/v1" }, defaultSettings);
	c(
		"cloudflare-ai-gateway-by-provider-id",
		{ provider: "cloudflare-ai-gateway", baseUrl: "https://example.test/v1" },
		defaultSettings,
	);
	c(
		"opencode-by-provider-id-with-session-id",
		{ provider: "opencode", baseUrl: "https://opencode.ai/api" },
		defaultSettings,
		"sess-123",
	);
	c(
		"opencode-host-without-session-id-gets-nothing",
		{ provider: "custom", baseUrl: "https://opencode.ai/api" },
		defaultSettings,
		undefined,
	);
	c(
		"telemetry-disabled-suppresses-attribution-but-not-session-headers",
		{ provider: "opencode", baseUrl: "https://opencode.ai/api" },
		noTelemetrySettings,
		"sess-123",
	);
	c(
		"header-source-overrides-a-default-key-in-place",
		{ provider: "openrouter", baseUrl: "https://openrouter.ai/api/v1" },
		defaultSettings,
		undefined,
		[{ "X-OpenRouter-Title": "overridden" }],
	);
	c(
		"header-source-appends-a-new-key",
		{ provider: "openrouter", baseUrl: "https://openrouter.ai/api/v1" },
		defaultSettings,
		undefined,
		[{ "X-Custom": "1" }, undefined],
	);

	emit("provider-attribution.cases.jsonl", records);
	return records.length;
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------
async function main() {
	const spCount = await genSystemPromptCases();
	const paCount = await genProviderAttributionCases();

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
			console.error(`DRIFT    ${rel}: ${existing === null ? "missing" : `${existing.length} -> ${contents.length} bytes`}`);
			continue;
		}
		writeFileSync(dest, contents, "utf-8");
		console.log(`wrote    ${rel} (${Buffer.byteLength(contents)} bytes)`);
	}
	if (CHECK && drift > 0) {
		console.error(`\nDRIFT: ${drift} sdk fixture(s) are stale; run node scripts/gen-sdk-oracle.mjs`);
		process.exitCode = 1;
	}

	console.log("\n=== SUMMARY ===");
	console.log(`system-prompt cases        : ${spCount}`);
	console.log(`provider-attribution cases : ${paCount}`);
}

await main();
