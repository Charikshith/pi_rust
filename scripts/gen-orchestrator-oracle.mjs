#!/usr/bin/env node
// feat-009 Wave 1 oracle: drive REAL `encodeCbor`/`decodeCbor` (packages/protocol/
// src/cbor/{encoder,decoder}.ts) and `encodeFrame`/`FrameDecoder`/`assertCompleteFrame`
// (packages/protocol/src/framing.ts) from ../pi directly. Both files are
// self-contained (zero cross-package imports), so no alias/resolve-hook is needed —
// Node's native type-stripping imports them as-is (same pattern as gen-tui-oracle.mjs's
// utils.ts section).
//
// feat-009 Wave 2 addition: drive real `codec.ts` (`parseClientMessage`/
// `parseServerMessage`/`encodeClientMessage`/`encodeServerMessage`/
// `ClientMessageDecoder`/`ServerMessageDecoder`/`isSupportedProtocolVersion`).
// `codec.ts` is also self-contained within packages/protocol (imports only
// `./cbor`, `./framing`, `./schemas` — no resolve-hook needed).
//
// WAVE 2 SCOPE (named, not silent): the Rust port this wave treats `request`/
// `result`/`event` payload BODIES as opaque, generically-validated JSON
// (any value shaped like `JsonValueSchema` — no CBOR byte strings, no cycles,
// finite numbers only) rather than fully typing every `Command`/
// `CommandResult`/`ServerEvent`/`TranscriptItem`/`SessionSnapshot` variant
// from `schemas.ts`. That deep shape-validation (assistant/tool item status
// consistency, `SessionMetadata` required fields, `Command` variant shapes,
// image-rejection in `prompt`, etc.) is deferred to Wave 4 (`sessions.rs`),
// where those types get built against real session-lifecycle behavior
// instead of in isolation. This oracle still CAPTURES the full
// `protocol.test.ts` battery (for Wave 4 reuse); records whose assertion
// depends on the deferred deep-shape checks are tagged
// `"scope":"deferred"` so the Wave 2 Rust test can skip them explicitly
// rather than silently passing on an untested case.
//
// Fixtures written (tests/fixtures/pi/orchestrator/):
//   cbor.cases.jsonl     one record per CBOR test case (see RECORD SHAPES below)
//   framing.cases.jsonl  one record per framing test case
//   codec.cases.jsonl    one record per codec/envelope test case
//
// Usage:
//   node scripts/gen-orchestrator-oracle.mjs            # regenerate fixtures
//   node scripts/gen-orchestrator-oracle.mjs --check    # exit 1 if fixtures would change
//
// Requires the sibling Pi checkout at ../pi (Node type-stripping, no dist build).
//
// RECORD SHAPES (cbor.cases.jsonl):
//   {"kind":"roundtrip","description":"...","input":<tagged>,"hex":"..."}
//     encodeCbor(untag(input)) === hex (verified here), AND the Rust port must
//     produce the same hex AND decode(hex) back to the same tagged value.
//   {"kind":"decode_reject","description":"...","hex":"..."}
//     decodeCbor(fromHex(hex)) must throw CborError (verified here).
//   {"kind":"encode_reject","description":"...","input":<tagged>}
//     encodeCbor(untag(input)) must throw CborError (verified here). Only
//     cases meaningful to a type-constrained Rust input are included (finite-
//     float, safe-integer-range, depth, container-length, byte-length) — JS-only
//     rejections (BigInt/Symbol/Function/Date/Map/cyclic refs/array holes/
//     symbol-keyed objects) are type-system-moot in Rust and are NOT captured.
//   {"kind":"decode_limit","description":"...","hex":"...","options":{...}}
//     decodeCbor(bytes, options) must throw (custom stricter limits).
//
// RECORD SHAPES (framing.cases.jsonl):
//   {"kind":"encode_frame","description":"...","payloadHex":"...","frameHex":"..."}
//   {"kind":"assert_complete","description":"...","frameHex":"...","maxFrameLength":N|null,"ok":bool}
//   {"kind":"decoder_push","description":"...","chunksHex":["...","..."],"maxFrameLength":N|null,"framesHex":["...","..."],"endThrows":bool}

import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const pirustRoot = dirname(here);
const piRoot = join(pirustRoot, "..", "pi");
const PROTO = join(piRoot, "packages", "protocol", "src");
const OUT = join(pirustRoot, "tests", "fixtures", "pi", "orchestrator");

const argv = process.argv.slice(2);
const CHECK = argv.includes("--check");

if (!existsSync(join(PROTO, "cbor", "encoder.ts"))) {
	console.error(`Pi protocol sources not found at ${PROTO}; fixtures cannot be regenerated.`);
	process.exit(CHECK ? 0 : 1);
}

const { encodeCbor, decodeCbor, CborError, DEFAULT_MAX_CBOR_DEPTH, DEFAULT_MAX_CBOR_BYTE_LENGTH, DEFAULT_MAX_CBOR_CONTAINER_LENGTH } =
	await import(pathToFileURL(join(PROTO, "cbor", "index.ts")).href);
const { encodeFrame, assertCompleteFrame, FrameDecoder, FrameError } = await import(
	pathToFileURL(join(PROTO, "framing.ts")).href
);
const {
	parseClientMessage,
	parseServerMessage,
	encodeClientMessage,
	encodeServerMessage,
	ClientMessageDecoder,
	ServerMessageDecoder,
	isSupportedProtocolVersion,
} = await import(pathToFileURL(join(PROTO, "codec.ts")).href);
const { PROTOCOL_VERSION } = await import(pathToFileURL(join(PROTO, "schemas.ts")).href);

// ---------------------------------------------------------------------------
// Tagged-value <-> real JS value
// ---------------------------------------------------------------------------

function toHex(bytes) {
	return Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
}
function fromHex(hex) {
	if (hex.length % 2 !== 0) throw new Error("odd hex length");
	const out = new Uint8Array(hex.length / 2);
	for (let i = 0; i < out.length; i++) out[i] = Number.parseInt(hex.slice(i * 2, i * 2 + 2), 16);
	return out;
}

function untag(spec) {
	switch (spec.t) {
		case "null":
			return null;
		case "bool":
			return spec.v;
		case "int":
			return spec.v;
		case "float":
			return spec.v;
		case "negZero":
			return -0;
		case "nan":
			return Number.NaN;
		case "posInf":
			return Number.POSITIVE_INFINITY;
		case "negInf":
			return Number.NEGATIVE_INFINITY;
		case "bytes":
			return fromHex(spec.hex);
		case "text":
			return spec.v;
		case "array":
			return spec.v.map(untag);
		case "map": {
			const out = {};
			for (const [k, v] of spec.v) out[k] = untag(v);
			return out;
		}
		case "nested": {
			// depth-N array of arrays, innermost null: [[[...null...]]]
			let value = null;
			for (let i = 0; i < spec.depth; i++) value = [value];
			return value;
		}
		default:
			throw new Error(`unknown tag ${spec.t}`);
	}
}

function tag(value) {
	if (value === null) return { t: "null" };
	if (typeof value === "boolean") return { t: "bool", v: value };
	if (typeof value === "number") {
		if (Object.is(value, -0)) return { t: "negZero" };
		if (Number.isInteger(value)) return { t: "int", v: value };
		return { t: "float", v: value };
	}
	if (typeof value === "string") return { t: "text", v: value };
	if (value instanceof Uint8Array) return { t: "bytes", hex: toHex(value) };
	if (Array.isArray(value)) return { t: "array", v: value.map(tag) };
	if (typeof value === "object") return { t: "map", v: Object.entries(value).map(([k, v]) => [k, tag(v)]) };
	throw new Error(`cannot tag ${typeof value}`);
}

// ---------------------------------------------------------------------------
// CBOR cases
// ---------------------------------------------------------------------------

const cborCases = [];

function roundtrip(description, spec) {
	const value = untag(spec);
	const encoded = encodeCbor(value);
	const hex = toHex(encoded);
	// Confirm decode reconstructs the same tagged value (drives real decodeCbor too).
	const decoded = decodeCbor(fromHex(hex));
	const decodedTag = JSON.stringify(tag(decoded));
	const inputTag = JSON.stringify(tag(value));
	if (decodedTag !== inputTag) {
		throw new Error(`round-trip mismatch for "${description}": ${inputTag} -> ${decodedTag}`);
	}
	cborCases.push({ kind: "roundtrip", description, input: spec, hex });
}

function decodeReject(description, hex) {
	try {
		decodeCbor(fromHex(hex));
		throw new Error(`expected decode rejection for "${description}" (hex=${hex})`);
	} catch (error) {
		if (!(error instanceof CborError)) throw error;
	}
	cborCases.push({ kind: "decode_reject", description, hex });
}

function encodeReject(description, spec) {
	const value = untag(spec);
	try {
		encodeCbor(value);
		throw new Error(`expected encode rejection for "${description}"`);
	} catch (error) {
		if (!(error instanceof CborError)) throw error;
	}
	cborCases.push({ kind: "encode_reject", description, input: spec });
}

function decodeLimit(description, hex, options) {
	try {
		decodeCbor(fromHex(hex), options);
		throw new Error(`expected limited decode rejection for "${description}"`);
	} catch (error) {
		if (!(error instanceof CborError)) throw error;
	}
	cborCases.push({ kind: "decode_limit", description, hex, options });
}

// Known RFC 8949 vectors (packages/protocol/test/cbor/cbor.test.ts:23-58).
roundtrip("null", { t: "null" });
roundtrip("false", { t: "bool", v: false });
roundtrip("true", { t: "bool", v: true });
roundtrip("int 0", { t: "int", v: 0 });
roundtrip("int 1", { t: "int", v: 1 });
roundtrip("int 10", { t: "int", v: 10 });
roundtrip("int 23 (minimal boundary)", { t: "int", v: 23 });
roundtrip("int 24 (1-byte arg boundary)", { t: "int", v: 24 });
roundtrip("int 25", { t: "int", v: 25 });
roundtrip("int 100", { t: "int", v: 100 });
roundtrip("int 1000 (2-byte arg)", { t: "int", v: 1000 });
roundtrip("int 1_000_000 (4-byte arg)", { t: "int", v: 1_000_000 });
roundtrip("int 1_000_000_000_000 (8-byte arg)", { t: "int", v: 1_000_000_000_000 });
roundtrip("int MAX_SAFE_INTEGER", { t: "int", v: Number.MAX_SAFE_INTEGER });
roundtrip("int -1", { t: "int", v: -1 });
roundtrip("int -10", { t: "int", v: -10 });
roundtrip("int -24", { t: "int", v: -24 });
roundtrip("int -25", { t: "int", v: -25 });
roundtrip("int -100", { t: "int", v: -100 });
roundtrip("int -1000", { t: "int", v: -1000 });
roundtrip("int -1_000_000", { t: "int", v: -1_000_000 });
roundtrip("int MIN_SAFE_INTEGER", { t: "int", v: Number.MIN_SAFE_INTEGER });
roundtrip("float 1.1", { t: "float", v: 1.1 });
roundtrip("negative zero", { t: "negZero" });
roundtrip("bytes [1,2,3,4]", { t: "bytes", hex: "01020304" });
roundtrip("text empty", { t: "text", v: "" });
roundtrip("text IETF", { t: "text", v: "IETF" });
roundtrip("text u-umlaut (2-byte utf8)", { t: "text", v: "ü" });
roundtrip("text CJK (3-byte utf8)", { t: "text", v: "水" });
roundtrip("text surrogate-pair emoji (4-byte utf8)", { t: "text", v: "\u{10151}" });
roundtrip("array empty", { t: "array", v: [] });
roundtrip("array [1,2,3]", { t: "array", v: [{ t: "int", v: 1 }, { t: "int", v: 2 }, { t: "int", v: 3 }] });
roundtrip("array nested", {
	t: "array",
	v: [
		{ t: "int", v: 1 },
		{ t: "array", v: [{ t: "int", v: 2 }, { t: "int", v: 3 }] },
		{ t: "array", v: [{ t: "int", v: 4 }, { t: "int", v: 5 }] },
	],
});
roundtrip("map {a:1,b:[2,3]}", {
	t: "map",
	v: [
		["a", { t: "int", v: 1 }],
		["b", { t: "array", v: [{ t: "int", v: 2 }, { t: "int", v: 3 }] }],
	],
});

// Falsey-value / undefined-omission semantics (cbor.test.ts:69-72).
{
	const value = { omitted: undefined, zero: 0, empty: "", no: false, nil: null };
	const encoded = encodeCbor(value);
	const decoded = decodeCbor(encoded);
	const expectedTag = tag({ zero: 0, empty: "", no: false, nil: null });
	if (JSON.stringify(tag(decoded)) !== JSON.stringify(expectedTag)) {
		throw new Error("undefined-omission semantics changed upstream");
	}
	cborCases.push({
		kind: "roundtrip",
		description: "object with an undefined property omits that key only",
		input: { t: "map", v: [["omitted", { t: "null" }], ["zero", { t: "int", v: 0 }], ["empty", { t: "text", v: "" }], ["no", { t: "bool", v: false }], ["nil", { t: "null" }]] },
		// NOTE: the Rust fixture consumer must special-case this one record:
		// the *input* spec below stands in for "omitted:undefined" (Rust has no
		// undefined; the port's own encoder simply never emits a None field),
		// so only `hex` is authoritative here.
		hex: toHex(encoded),
	});
}

// Leading-BOM preservation + __proto__-as-data (cbor.test.ts:74-82).
{
	const bomHex = "63efbbbf";
	const decodedBom = decodeCbor(fromHex(bomHex));
	if (decodedBom !== "﻿") throw new Error("BOM decode changed upstream");
	cborCases.push({ kind: "roundtrip", description: "leading BOM text preserved", input: { t: "text", v: "﻿" }, hex: bomHex });
}

// Encode rejections meaningful to a type-constrained Rust input.
encodeReject("NaN is not finite", { t: "nan" });
encodeReject("+Infinity is not finite", { t: "posInf" });
encodeReject("-Infinity is not finite", { t: "negInf" });
encodeReject("unsafe positive integer (MAX_SAFE_INTEGER + 1)", { t: "int", v: Number.MAX_SAFE_INTEGER + 1 });
encodeReject("unsafe negative integer (MIN_SAFE_INTEGER - 1)", { t: "int", v: Number.MIN_SAFE_INTEGER - 1 });
encodeReject(`nesting deeper than ${DEFAULT_MAX_CBOR_DEPTH}`, { t: "nested", depth: DEFAULT_MAX_CBOR_DEPTH + 1 });

// Decode rejections (decoder.test.ts:120-152) — pure byte literals.
decodeReject("empty input", "");
decodeReject("truncated integer", "18");
decodeReject("reserved additional information", "1c");
decodeReject("indefinite byte string", "5f");
decodeReject("indefinite text string", "7f");
decodeReject("indefinite array", "9f");
decodeReject("indefinite map", "bf");
decodeReject("tag", "c000");
decodeReject("undefined simple value", "f7");
decodeReject("unsupported simple value", "e0");
decodeReject("break outside an indefinite item", "ff");
decodeReject("float16", "f93c00");
decodeReject("float32", "fa3f800000");
decodeReject("positive infinity float64", "fb7ff0000000000000");
decodeReject("NaN float64", "fb7ff8000000000000");
decodeReject("truncated float64", "fb3ff00000");
decodeReject("truncated byte string", "44010203");
decodeReject("truncated text string", "636162");
decodeReject("truncated array", "8201");
decodeReject("truncated map", "a16161");
decodeReject("trailing data", "0000");
decodeReject("non-string map key", "a10102");
decodeReject("duplicate map key", "a2616101616102");
decodeReject("invalid UTF-8 byte", "61ff");
decodeReject("overlong UTF-8", "62c080");
decodeReject("UTF-8 surrogate", "63eda080");
decodeReject("unsafe positive integer (arg)", "1b0020000000000000");
decodeReject("unsafe negative integer (arg)", "3b001fffffffffffff");
decodeReject("unsafe integer encoded as float64", "fb4340000000000000");

// Depth/length limits enforced before traversal (decoder.test.ts:154-174).
{
	const tooDeep = new Uint8Array(DEFAULT_MAX_CBOR_DEPTH + 2);
	tooDeep.fill(0x81, 0, -1);
	tooDeep[tooDeep.length - 1] = 0xf6;
	decodeReject(`decoder nesting deeper than ${DEFAULT_MAX_CBOR_DEPTH}`, toHex(tooDeep));
}
decodeReject(
	"oversized declared byte-string length",
	`5a${(DEFAULT_MAX_CBOR_BYTE_LENGTH + 1).toString(16).padStart(8, "0")}`,
);
decodeReject(
	"oversized declared text-string length",
	`7a${(DEFAULT_MAX_CBOR_BYTE_LENGTH + 1).toString(16).padStart(8, "0")}`,
);
decodeReject(
	"oversized declared array length",
	`9a${(DEFAULT_MAX_CBOR_CONTAINER_LENGTH + 1).toString(16).padStart(8, "0")}`,
);
decodeReject(
	"oversized declared map length",
	`ba${(DEFAULT_MAX_CBOR_CONTAINER_LENGTH + 1).toString(16).padStart(8, "0")}`,
);

// Caller-provided stricter limits (decoder.test.ts:169-174).
decodeLimit("stricter maxContainerLength on decode", "83010203", { maxContainerLength: 2 });
decodeLimit("stricter maxByteLength on decode", "626162", { maxByteLength: 2 });
{
	try {
		encodeCbor([1, 2, 3], { maxContainerLength: 2 });
		throw new Error("expected maxContainerLength encode rejection");
	} catch (error) {
		if (!(error instanceof CborError)) throw error;
	}
	cborCases.push({
		kind: "encode_reject",
		description: "stricter maxContainerLength on encode",
		input: { t: "array", v: [{ t: "int", v: 1 }, { t: "int", v: 2 }, { t: "int", v: 3 }] },
		options: { maxContainerLength: 2 },
	});
}
{
	try {
		encodeCbor("ab", { maxByteLength: 2 });
		throw new Error("expected maxByteLength encode rejection");
	} catch (error) {
		if (!(error instanceof CborError)) throw error;
	}
	cborCases.push({
		kind: "encode_reject",
		description: "stricter maxByteLength on encode",
		input: { t: "text", v: "ab" },
		options: { maxByteLength: 2 },
	});
}

// ---------------------------------------------------------------------------
// Framing cases (framing.test.ts)
// ---------------------------------------------------------------------------

const framingCases = [];

function concat(...chunks) {
	const total = chunks.reduce((n, c) => n + c.byteLength, 0);
	const out = new Uint8Array(total);
	let offset = 0;
	for (const c of chunks) {
		out.set(c, offset);
		offset += c.byteLength;
	}
	return out;
}

framingCases.push({
	kind: "encode_frame",
	description: "3-byte payload",
	payloadHex: toHex(new Uint8Array([0xaa, 0xbb, 0xcc])),
	frameHex: toHex(encodeFrame(new Uint8Array([0xaa, 0xbb, 0xcc]))),
});
framingCases.push({
	kind: "encode_frame",
	description: "empty payload",
	payloadHex: "",
	frameHex: toHex(encodeFrame(new Uint8Array())),
});

function assertCase(description, frameHex, maxFrameLength, expectOk) {
	let ok = true;
	try {
		assertCompleteFrame(fromHex(frameHex), maxFrameLength === null ? undefined : { maxFrameLength });
	} catch {
		ok = false;
	}
	if (ok !== expectOk) throw new Error(`assertCompleteFrame expectation changed upstream: ${description}`);
	framingCases.push({ kind: "assert_complete", description, frameHex, maxFrameLength, ok: expectOk });
}
assertCase("complete frame within a stricter limit", toHex(new Uint8Array([0, 0, 0, 2, 1, 2])), 2, true);
assertCase("incomplete frame (missing last payload byte)", toHex(new Uint8Array([0, 0, 0, 2, 1])), null, false);
assertCase("frame with trailing extra byte", toHex(new Uint8Array([0, 0, 0, 1, 1, 2])), null, false);
assertCase("frame exceeding a stricter limit", toHex(new Uint8Array([0, 0, 0, 3, 1, 2, 3])), 2, false);

function decoderCase(description, chunks, maxFrameLength, expectFramesHex, expectEndThrows) {
	const decoder = new FrameDecoder(maxFrameLength === null ? undefined : { maxFrameLength });
	const frames = [];
	let pushThrew = false;
	try {
		for (const chunk of chunks) frames.push(...decoder.push(chunk));
	} catch {
		pushThrew = true;
	}
	const gotHex = frames.map(toHex);
	if (!pushThrew && JSON.stringify(gotHex) !== JSON.stringify(expectFramesHex)) {
		throw new Error(`decoder frames mismatch upstream: ${description}: ${JSON.stringify(gotHex)}`);
	}
	let endThrew = false;
	if (!pushThrew) {
		try {
			decoder.end();
		} catch {
			endThrew = true;
		}
	}
	framingCases.push({
		kind: "decoder_push",
		description,
		chunksHex: chunks.map(toHex),
		maxFrameLength,
		framesHex: pushThrew ? null : gotHex,
		pushThrows: pushThrew,
		endThrows: pushThrew ? null : endThrew,
	});
	if (!pushThrew && endThrew !== expectEndThrows) {
		throw new Error(`decoder end() expectation changed upstream: ${description}`);
	}
}

{
	const wire = concat(encodeFrame(new Uint8Array([1, 2, 3])), encodeFrame(new Uint8Array()), encodeFrame(new Uint8Array([4])));
	const byteChunks = Array.from(wire, (b) => new Uint8Array([b]));
	decoderCase(
		"byte-at-a-time: three frames incl. an empty one",
		byteChunks,
		null,
		[toHex(new Uint8Array([1, 2, 3])), "", toHex(new Uint8Array([4]))],
		false,
	);
	decoderCase(
		"single coalesced chunk: three frames incl. an empty one",
		[wire],
		null,
		[toHex(new Uint8Array([1, 2, 3])), "", toHex(new Uint8Array([4]))],
		false,
	);
}
{
	const payload = Uint8Array.from({ length: 70_000 }, (_, i) => i % 251);
	const wire = encodeFrame(payload);
	decoderCase(
		"payload spanning multiple 64KiB internal blocks",
		[wire.subarray(0, 101), wire.subarray(101, 65_541), wire.subarray(65_541)],
		null,
		[toHex(payload)],
		false,
	);
}
{
	const wire = encodeFrame(new Uint8Array([10, 20, 30, 40]));
	for (let split = 0; split <= wire.byteLength; split++) {
		decoderCase(`split at byte ${split}`, [wire.subarray(0, split), wire.subarray(split)], null, [toHex(new Uint8Array([10, 20, 30, 40]))], false);
	}
}
decoderCase("empty chunk then clean end", [new Uint8Array()], null, [], false);
decoderCase("truncated at header", [new Uint8Array([0, 0, 0])], null, [], true);
decoderCase("truncated mid-payload", [new Uint8Array([0, 0, 0, 2, 1])], null, [], true);
{
	// Oversized declared length must fail as soon as the header completes, and
	// leave the decoder in a permanently failed state (second push also throws).
	const decoder = new FrameDecoder({ maxFrameLength: 3 });
	let firstThrew = false;
	let secondThrew = false;
	try {
		decoder.push(new Uint8Array([0, 0, 0, 4]));
	} catch {
		firstThrew = true;
	}
	try {
		decoder.push(new Uint8Array([1]));
	} catch {
		secondThrew = true;
	}
	if (!firstThrew || !secondThrew) throw new Error("oversized-length failed-state behavior changed upstream");
	framingCases.push({
		kind: "decoder_push",
		description: "oversized declared length fails immediately and latches failed state",
		chunksHex: [toHex(new Uint8Array([0, 0, 0, 4])), toHex(new Uint8Array([1]))],
		maxFrameLength: 3,
		framesHex: null,
		pushThrows: true,
		endThrows: null,
	});
}
decoderCase("frame exactly at the configured maximum", [encodeFrame(new Uint8Array([1, 2, 3]))], 3, [toHex(new Uint8Array([1, 2, 3]))], false);

// ---------------------------------------------------------------------------
// Codec / envelope cases (protocol.test.ts) — see the Wave 2 scope note in
// this file's header comment for what "deferred" means.
// ---------------------------------------------------------------------------

const codecCases = [];

const emptyServerSnapshot = { serverId: "server-1", protocolVersion: PROTOCOL_VERSION, revision: 0, sessions: [], models: [] };
const clientHello = { type: "hello", version: PROTOCOL_VERSION };
const serverHello = { type: "hello", version: PROTOCOL_VERSION, connectionId: "connection-1", snapshot: emptyServerSnapshot };

function clientOk(description, message) {
	const parsed = parseClientMessage(message);
	if (JSON.stringify(parsed) !== JSON.stringify(message)) throw new Error(`parseClientMessage changed the value for ${description}`);
	const hex = toHex(encodeClientMessage(message));
	codecCases.push({ kind: "client_parse_ok", description, message, hex, scope: "wave2" });
}
function clientReject(description, message, scope = "wave2") {
	let threw = false;
	try {
		parseClientMessage(message);
	} catch {
		threw = true;
	}
	if (!threw) throw new Error(`expected parseClientMessage rejection for ${description}`);
	codecCases.push({ kind: "client_parse_reject", description, message, scope });
}
function serverOk(description, message) {
	const parsed = parseServerMessage(message);
	if (JSON.stringify(parsed) !== JSON.stringify(message)) throw new Error(`parseServerMessage changed the value for ${description}`);
	const hex = toHex(encodeServerMessage(message));
	codecCases.push({ kind: "server_parse_ok", description, message, hex, scope: "wave2" });
}
function serverReject(description, message, scope = "wave2") {
	let threw = false;
	try {
		parseServerMessage(message);
	} catch {
		threw = true;
	}
	if (!threw) throw new Error(`expected parseServerMessage rejection for ${description}`);
	codecCases.push({ kind: "server_parse_reject", description, message, scope });
}

// version negotiation (schemas.ts:387 ClientHello vs :414 ServerHello — an
// intentional asymmetry: client hello accepts ANY non-negative integer for
// negotiation; server hello accepts ONLY the exact current version).
{
	if (PROTOCOL_VERSION !== 1) throw new Error("PROTOCOL_VERSION assumption changed upstream");
	if (isSupportedProtocolVersion(1) !== true || isSupportedProtocolVersion(2) !== false || isSupportedProtocolVersion(2.5) !== false) {
		throw new Error("isSupportedProtocolVersion behavior changed upstream");
	}
	codecCases.push({ kind: "is_supported_version", description: "PROTOCOL_VERSION is 1", version: PROTOCOL_VERSION, scope: "wave2" });
}
clientOk("client hello version 0", { type: "hello", version: 0 });
clientOk("client hello version == PROTOCOL_VERSION", { type: "hello", version: PROTOCOL_VERSION });
clientOk("client hello version == PROTOCOL_VERSION + 1 (negotiation-open)", { type: "hello", version: PROTOCOL_VERSION + 1 });
clientReject("client hello with a string version", { type: "hello", version: String(PROTOCOL_VERSION) });
clientReject("client hello with a fractional version", { type: "hello", version: PROTOCOL_VERSION + 0.5 });
clientReject("client hello with an unexpected credential field", { type: "hello", version: PROTOCOL_VERSION, token: "secret" });
clientReject("client hello with an unknown field", { type: "hello", version: PROTOCOL_VERSION, extra: true });
clientReject(
	"a bare CBOR text string is not a valid client message shape",
	// The oracle script represents "parse this raw decoded CBOR value" cases
	// as {"__raw": <any JSON value>} so the Rust test can feed it straight to
	// decode_cbor's Rust equivalent without going through the object-message
	// helpers above (which always parse-then-re-encode an object).
	"not-an-object",
);
clientOk("request envelope wrapping an opaque command body", {
	type: "request",
	id: "request-1",
	request: { command: "list" },
});
clientOk("request envelope with undefined-valued optional fields omitted", {
	type: "request",
	id: "request-1",
	request: { command: "create" },
});
clientReject(
	"prompt command image field (Command shape validation deferred to Wave 4)",
	{
		type: "request",
		id: "request-1",
		request: { command: "prompt", sessionId: "session-1", text: "inspect", images: [{ type: "image", data: "abc", mimeType: "image/png" }] },
	},
	"deferred",
);

serverOk("server hello handshake snapshot", serverHello);
serverReject("server hello with an unsupported version (schema requires the exact literal)", {
	type: "hello",
	version: PROTOCOL_VERSION + 1,
	connectionId: "connection-1",
	snapshot: emptyServerSnapshot,
});
serverReject("server hello_error with an error code outside the enum", {
	type: "hello_error",
	error: { code: "auth", message: "Authentication failed" },
});
serverOk("server hello_error with a real error code", {
	type: "hello_error",
	error: { code: "version", message: "Unsupported protocol version" },
});
for (const code of ["not_implemented", "internal_error"]) {
	serverOk(`server response failure with the ${code} error code`, {
		type: "response",
		id: "request-1",
		ok: false,
		error: { code, message: "safe" },
	});
}
serverOk("server response success wrapping an opaque result body", {
	type: "response",
	id: "request-1",
	ok: true,
	result: { command: "list", sessions: [] },
});
serverReject(
	"response result command variant validation (CommandResult shape deferred to Wave 4)",
	{ type: "response", id: "request-1", ok: true, result: { command: "unknown" } },
	"deferred",
);
serverOk("server event wrapping an opaque event body", {
	type: "event",
	event: { type: "server_snapshot", snapshot: emptyServerSnapshot },
});
serverReject(
	"event body inner-shape validation (ServerEvent shape deferred to Wave 4)",
	{ type: "event", event: { type: "session_removed", sessionId: 42 } },
	"deferred",
);
serverReject(
	"SessionMetadata required-field validation (deferred to Wave 4)",
	{
		type: "response",
		id: "request-1",
		ok: true,
		result: { command: "list", sessions: [{ id: "session-1", createdAt: 1, phase: "idle" }] },
	},
	"deferred",
);

// ---------------------------------------------------------------------------
// Wave 4 additions: the deep Command/CommandResult/ServerEvent/TranscriptItem
// battery (protocol.test.ts's own assistant/tool-consistency + nested-detail
// + nonterminal-item scenarios).
// ---------------------------------------------------------------------------

serverOk("SessionMetadata with every optional field present", {
	type: "response",
	id: "request-1",
	ok: true,
	result: {
		command: "list",
		sessions: [
			{
				id: "session-1",
				createdAt: 1,
				updatedAt: 2,
				parentSessionId: "parent-1",
				sessionName: "Named session",
				cwd: "/workspace",
			},
		],
	},
});

function itemMessage(item, type = "item_finished") {
	return {
		type: "event",
		event: {
			type: "session_progress",
			sessionId: "session-1",
			progress: { type, item },
		},
	};
}

for (const state of [
	{ status: "streaming" },
	{ status: "complete", stopReason: "stop" },
	{ status: "error", stopReason: "error" },
	{ status: "error", stopReason: "error", errorMessage: "failed" },
	{ status: "aborted", stopReason: "aborted" },
]) {
	serverOk(`consistent assistant item: ${JSON.stringify(state)}`, itemMessage(
		{
			id: "assistant-1",
			role: "assistant",
			content: [{ type: "text", text: "hello" }],
			model: { provider: "test", id: "model" },
			timestamp: 1,
			...state,
		},
		state.status === "streaming" ? "item_updated" : "item_finished",
	));
}

for (const state of [
	{ status: "streaming", stopReason: "stop" },
	{ status: "complete" },
	{ status: "complete", stopReason: "error" },
	{ status: "error", stopReason: "error", errorMessage: "" },
	{ status: "aborted", stopReason: "stop" },
]) {
	serverReject(
		`inconsistent assistant item: ${JSON.stringify(state)}`,
		itemMessage({
			id: "assistant-1",
			role: "assistant",
			content: [{ type: "text", text: "hello" }],
			model: { provider: "test", id: "model" },
			timestamp: 1,
			...state,
		}),
	);
}

for (const state of [
	{ status: "running", isError: false },
	{ status: "complete", isError: false },
	{ status: "error", isError: true },
]) {
	serverOk(`consistent tool item: ${JSON.stringify(state)}`, itemMessage(
		{
			id: "tool-1",
			role: "tool",
			toolCallId: "call-1",
			toolName: "read",
			input: {},
			content: [],
			timestamp: 1,
			...state,
		},
		state.status === "running" ? "item_updated" : "item_finished",
	));
}

for (const state of [
	{ status: "running", isError: true },
	{ status: "complete", isError: true },
	{ status: "error", isError: false },
]) {
	serverReject(
		`inconsistent tool item: ${JSON.stringify(state)}`,
		itemMessage({
			id: "tool-1",
			role: "tool",
			toolCallId: "call-1",
			toolName: "read",
			input: {},
			content: [],
			timestamp: 1,
			...state,
		}),
	);
}

serverReject(
	"rejects a nonterminal (streaming) assistant item reported as finished",
	itemMessage({
		id: "assistant-1",
		role: "assistant",
		content: [],
		model: { provider: "test", id: "model" },
		status: "streaming",
		timestamp: 1,
	}),
);
serverReject(
	"rejects a nonterminal (running) tool item reported as finished",
	itemMessage({
		id: "tool-1",
		role: "tool",
		toolCallId: "call-1",
		toolName: "read",
		input: {},
		content: [],
		status: "running",
		isError: false,
		timestamp: 1,
	}),
);
serverOk(
	"validates nested JSON tool details",
	// Field order here follows the REAL construction site
	// (`toProtocolToolResultMessage` in protocol.ts: ...content, details?,
	// usage?, timestamp, status, isError) rather than protocol.test.ts's own
	// hand-written literal order (which happens to put status/isError before
	// timestamp) — parseServerMessage doesn't reorder its input either way,
	// so this is still a faithful oracle capture of a shape a real running
	// PiServer would actually emit, which is the more representative case.
	itemMessage({
		id: "tool-1",
		role: "tool",
		toolCallId: "call-1",
		toolName: "read",
		input: { path: "/tmp/file" },
		content: [{ type: "text", text: "done" }],
		details: { lines: [1, 2, 3], cached: false },
		timestamp: 1,
		status: "complete",
		isError: false,
	}),
);

// Outbound frame-length enforcement (codec.ts's encodeProtocolMessage calls
// assertCompleteFrame after encodeFrame — an outbound size guard, in scope
// for Wave 2's codec.rs regardless of payload-shape depth).
{
	let clientThrew = false;
	let serverThrew = false;
	try {
		encodeClientMessage(clientHello, { maxFrameLength: 8 });
	} catch {
		clientThrew = true;
	}
	try {
		encodeServerMessage(serverHello, { maxFrameLength: 8 });
	} catch {
		serverThrew = true;
	}
	if (!clientThrew || !serverThrew) throw new Error("outbound frame-limit enforcement changed upstream");
	codecCases.push({
		kind: "outbound_frame_limit",
		description: "encodeClientMessage enforces maxFrameLength before returning bytes",
		message: clientHello,
		side: "client",
		maxFrameLength: 8,
		scope: "wave2",
	});
	codecCases.push({
		kind: "outbound_frame_limit",
		description: "encodeServerMessage enforces maxFrameLength before returning bytes",
		message: serverHello,
		side: "server",
		maxFrameLength: 8,
		scope: "wave2",
	});
}

// Incremental fragmented/coalesced decode through the validated decoder
// (Wave 1 already proved FrameDecoder's own splitting; this proves
// ClientMessageDecoder's frame->cbor->schema pipeline over TWO sequential
// messages, replayed at a handful of split points rather than every one —
// FrameDecoder's own every-split-point fidelity is already Wave 1's job).
{
	const request = { type: "request", id: "request-1", request: { command: "list" } };
	const first = encodeClientMessage(clientHello);
	const second = encodeClientMessage(request);
	const wire = new Uint8Array(first.byteLength + second.byteLength);
	wire.set(first);
	wire.set(second, first.byteLength);
	const splits = [0, 1, first.byteLength, first.byteLength + 1, wire.byteLength];
	for (const split of splits) {
		const decoder = new ClientMessageDecoder();
		const messages = [...decoder.push(wire.subarray(0, split)), ...decoder.push(wire.subarray(split))];
		decoder.end();
		if (JSON.stringify(messages) !== JSON.stringify([clientHello, request])) {
			throw new Error(`incremental client decode changed upstream at split ${split}`);
		}
	}
	codecCases.push({
		kind: "incremental_client_decode",
		description: "two concatenated client messages decode correctly split at several points",
		wireHex: toHex(wire),
		splits,
		expectedMessages: [clientHello, request],
		scope: "wave2",
	});
}

// Malformed/schema-invalid framed input latches the validated decoder into a
// permanently-failed state (codec.ts's ValidatedMessageDecoder `failed` flag
// — distinct from FrameDecoder's own Wave-1-proven failed state).
function framedClientRejectCase(description, frameHexBuilder) {
	const frame = frameHexBuilder();
	const decoder = new ClientMessageDecoder();
	let firstThrew = false;
	let secondThrew = false;
	let secondMessage = "";
	try {
		decoder.push(frame);
	} catch {
		firstThrew = true;
	}
	try {
		decoder.push(encodeClientMessage(clientHello));
	} catch (error) {
		secondThrew = true;
		secondMessage = String(error instanceof Error ? error.message : error);
	}
	if (!firstThrew || !secondThrew || !/failed/i.test(secondMessage)) {
		throw new Error(`framed-client-reject latch behavior changed upstream: ${description}`);
	}
	codecCases.push({ kind: "framed_client_reject", description, frameHex: toHex(frame), scope: "wave2" });
}
framedClientRejectCase("empty CBOR payload", () => encodeFrame(new Uint8Array()));
framedClientRejectCase("malformed CBOR", () => encodeFrame(new Uint8Array([0xff])));
framedClientRejectCase("schema-invalid CBOR (extra field)", () =>
	encodeFrame(encodeCbor({ type: "hello", version: PROTOCOL_VERSION, extra: true })),
);

// Truncated / oversized framing through the validated decoder wrapper.
{
	const decoder = new ServerMessageDecoder();
	const pushed = decoder.push(new Uint8Array([0, 0, 0, 2, 1]));
	let endThrew = false;
	try {
		decoder.end();
	} catch {
		endThrew = true;
	}
	if (pushed.length !== 0 || !endThrew) throw new Error("truncated server decoder behavior changed upstream");
	codecCases.push({
		kind: "server_decoder_truncated",
		description: "a truncated frame yields no messages then end() rejects",
		chunkHex: toHex(new Uint8Array([0, 0, 0, 2, 1])),
		scope: "wave2",
	});
}
{
	const decoder = new ClientMessageDecoder({ maxFrameLength: 3 });
	let threw = false;
	try {
		decoder.push(new Uint8Array([0, 0, 0, 4]));
	} catch {
		threw = true;
	}
	if (!threw) throw new Error("oversized client decoder behavior changed upstream");
	codecCases.push({
		kind: "client_decoder_oversized",
		description: "a declared length over maxFrameLength rejects as soon as the header completes",
		chunkHex: toHex(new Uint8Array([0, 0, 0, 4])),
		maxFrameLength: 3,
		scope: "wave2",
	});
}

// ---------------------------------------------------------------------------
// Write / check
// ---------------------------------------------------------------------------

function serialize(records) {
	return records.map((r) => JSON.stringify(r)).join("\n") + "\n";
}

const files = {
	"cbor.cases.jsonl": serialize(cborCases),
	"framing.cases.jsonl": serialize(framingCases),
	"codec.cases.jsonl": serialize(codecCases),
};

if (CHECK) {
	let changed = false;
	for (const [name, content] of Object.entries(files)) {
		const path = join(OUT, name);
		const existing = existsSync(path) ? readFileSync(path, "utf8") : null;
		if (existing !== content) {
			console.error(`orchestrator oracle drift: ${name} would change`);
			changed = true;
		}
	}
	process.exit(changed ? 1 : 0);
}

mkdirSync(OUT, { recursive: true });
for (const [name, content] of Object.entries(files)) {
	writeFileSync(join(OUT, name), content);
}
console.log(
	`Wrote ${cborCases.length} CBOR cases, ${framingCases.length} framing cases, and ${codecCases.length} codec cases to ${OUT}`,
);
