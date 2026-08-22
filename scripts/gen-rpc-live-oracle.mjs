#!/usr/bin/env node
// feat-012 LIVE oracle: drive REAL `pi --mode rpc` end-to-end against a local
// llama-server (Anthropic-compatible /v1/messages) and capture the full wire
// tape — responses AND streaming events (message_update/agent_start/
// message_end/agent_end/agent_settled).
//
// Requires a server on http://127.0.0.1:8080 serving ggml-org/Qwen3.5-0.8B-GGUF
// via llama-server's Anthropic endpoint, plus ../pi with deps installed.
//
// Output: tests/fixtures/pi/rpc-live/live.corpus.jsonl — one {"line": "..."}
// record per captured stdout line, with volatile values normalized
// (ids -> {ID}, timestamps -> {TS}, faux-style api markers, session paths).
// This is a REFERENCE capture (structure pinning), not a byte-frozen golden:
// model-generated text varies run to run. The Rust-side parity test parses it
// and asserts event-type sequence + top-level field order, not bytes.
//
// Usage: node scripts/gen-rpc-live-oracle.mjs [--check-server]

import { existsSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { tmpdir } from "node:os";

const here = dirname(fileURLToPath(import.meta.url));
const pirustRoot = dirname(here);
const OUT = join(pirustRoot, "tests", "fixtures", "pi", "rpc-live");
const BASE_URL = process.env.RPC_LIVE_BASE_URL ?? "http://127.0.0.1:8080";

const argv = process.argv.slice(2);

async function main() {
	// 1. Server up?
	try {
		const res = await fetch(`${BASE_URL}/health`);
		if (!res.ok) throw new Error(String(res.status));
	} catch {
		if (argv.includes("--check-server")) process.exit(1);
		console.error(`no server on ${BASE_URL}; skipping live capture (not an error)`);
		process.exit(0);
	}

	const { spawn } = await import("node:child_process");

	// 2. Temp agent dir with a models.json baseUrl override pointing at the
	//    local server (same mechanism pirust implements in models.rs).
	const agentDir = mkdtempSync(join(tmpdir(), "pirust-rpc-live-"));
	writeFileSync(
		join(agentDir, "models.json"),
		JSON.stringify({ providers: { anthropic: { baseUrl: BASE_URL } } }),
		"utf-8",
	);

	const child = spawn(
		process.execPath,
		[join(pirustRoot, "scripts", "run-pi.mjs"), "--", "--mode", "rpc", "--provider", "anthropic", "--model", "claude-opus-4-8"],
		{
			stdio: ["pipe", "pipe", "pipe"],
			env: {
				...process.env,
				PI_CODING_AGENT_DIR: agentDir,
				ANTHROPIC_API_KEY: "test-key-not-used-by-llama-server",
				NO_COLOR: "1",
			},
		},
	);

	const rawLines = [];
	let lineBuf = "";
	let stderrBuf = "";
	child.stdout.on("data", (c) => {
		lineBuf += c.toString();
	});
	child.stderr.on("data", (c) => {
		stderrBuf += c.toString();
	});

	const drain = () => {
		let idx;
		while ((idx = lineBuf.indexOf("\n")) !== -1) {
			rawLines.push(lineBuf.slice(0, idx));
			lineBuf = lineBuf.slice(idx + 1);
		}
	};
	const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
	const waitFor = async (needle, timeoutMs = 180000) => {
		const started = Date.now();
		for (;;) {
			drain();
			if (rawLines.some((l) => l.includes(needle))) return;
			if (Date.now() - started > timeoutMs) {
				throw new Error(`timeout waiting for ${needle}\n--- stderr ---\n${stderrBuf}\n--- have ---\n${rawLines.join("\n")}`);
			}
			await sleep(25);
		}
	};

	// Give pi's rpc mode a moment to boot (module graph + settings load).
	await sleep(2500);
	if (child.exitCode !== null) {
		console.error(`pi exited early (${child.exitCode})\n--- stderr ---\n${stderrBuf}`);
		process.exit(1);
	}

	const send = (line) => child.stdin.write(line + "\n");

	send('{"id":1,"type":"get_state"}');
	await waitFor('"command":"get_state"');
	send('{"id":2,"type":"set_thinking_level","level":"off"}');
	await waitFor('"command":"set_thinking_level"');
	send('{"id":3,"type":"prompt","message":"Reply with exactly the word PONG and nothing else."}');
	// Streaming turn: response line first, then events, ending in agent_settled.
	await waitFor('"type":"agent_settled"', 240000);
	send('{"id":4,"type":"get_last_assistant_text"}');
	await waitFor('"command":"get_last_assistant_text"');
	send('{"id":5,"type":"get_messages"}');
	await waitFor('"command":"get_messages"');
	child.stdin.end();

	const exitCode = await new Promise((resolve) => child.on("close", resolve));
	drain();
	rmSync(agentDir, { recursive: true, force: true });

	if (exitCode !== 0 && exitCode !== null) {
		console.error(`pi exited ${exitCode}\n--- stderr ---\n${stderrBuf}`);
		process.exit(1);
	}

	// 4. Normalize volatile values so runs are structurally comparable.
	const normalize = (text) =>
		text
			.replace(/\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b/gi, "{UUID}")
			.replace(/"(?:id|responseId)":"[^"]*"/g, '"id":"{ID}"')
			.replace(/\b17\d{11}\b/g, "{TS}")
			.replace(agentDir, "{AGENTDIR}");

	mkdirSync(OUT, { recursive: true });
	writeFileSync(
		join(OUT, "live.corpus.jsonl"),
		rawLines.map((l) => JSON.stringify({ line: normalize(l) })).join("\n") + "\n",
		"utf-8",
	);
	console.log(`gen-rpc-live-oracle: captured ${rawLines.length} lines -> tests/fixtures/pi/rpc-live/live.corpus.jsonl`);
}

await main();
