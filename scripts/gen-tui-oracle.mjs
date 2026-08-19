#!/usr/bin/env node
// gen-tui-oracle.mjs
//
// GOLDEN ORACLE for feat-006 Wave 1 (pirust-tui's utils.rs). Every result below is
// produced by EXECUTING Pi's own TypeScript source (packages/tui/src/utils.ts) under
// Node's native type stripping — nothing here is a reimplementation or a hand-authored
// expectation.
//
// Run:      cd pirust && node scripts/gen-tui-oracle.mjs
// Verify:   cd pirust && node scripts/gen-tui-oracle.mjs --check
//
// OUTPUT: tests/fixtures/pi/tui/utils.cases.jsonl
//   One JSON record per case: { note, fn, args, result }.

import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const CHECK = process.argv.includes("--check");

const here = dirname(fileURLToPath(import.meta.url));
const pirustRoot = dirname(here);
const piRoot = join(pirustRoot, "..", "pi");
const TUI_SRC = join(piRoot, "packages", "tui", "src");
const OUT_DIR = join(pirustRoot, "tests", "fixtures", "pi", "tui");
const OUT_FILE = join(OUT_DIR, "utils.cases.jsonl");

const utilsPath = join(TUI_SRC, "utils.ts");
if (!existsSync(utilsPath)) {
	console.error(`Pi tui sources not found at ${utilsPath}; fixtures cannot be regenerated.`);
	process.exit(CHECK ? 0 : 1); // don't fail --check when the source repo is simply absent
}

const {
	visibleWidth,
	truncateToWidth,
	sliceByColumn,
	sliceWithWidth,
	extractSegments,
	wrapTextWithAnsi,
	normalizeTerminalOutput,
	extractAnsiCode,
	isWhitespaceChar,
	isPunctuationChar,
	applyBackgroundToLine,
} = await import(pathToFileURL(utilsPath).href);

const keysPath = join(TUI_SRC, "keys.ts");
const {
	matchesKey,
	parseKey,
	decodeKittyPrintable,
	decodePrintableKey,
	isKeyRelease,
	isKeyRepeat,
	setKittyProtocolActive,
} = await import(pathToFileURL(keysPath).href);

const stdinBufferPath = join(TUI_SRC, "stdin-buffer.ts");
const { StdinBuffer } = await import(pathToFileURL(stdinBufferPath).href);

const wordNavPath = join(TUI_SRC, "word-navigation.ts");
const { findWordBackward, findWordForward } = await import(pathToFileURL(wordNavPath).href);

const keybindingsPath = join(TUI_SRC, "keybindings.ts");
const { KeybindingsManager, TUI_KEYBINDINGS } = await import(pathToFileURL(keybindingsPath).href);

const fuzzyPath = join(TUI_SRC, "fuzzy.ts");
const { fuzzyMatch, fuzzyFilter } = await import(pathToFileURL(fuzzyPath).href);

const terminalColorsPath = join(TUI_SRC, "terminal-colors.ts");
const { isOsc11BackgroundColorResponse, parseOsc11BackgroundColor, parseTerminalColorSchemeReport } = await import(
	pathToFileURL(terminalColorsPath).href,
);

const terminalImagePath = join(TUI_SRC, "terminal-image.ts");
const {
	detectCapabilities,
	getCapabilities,
	resetCapabilitiesCache,
	setCapabilities,
	isImageLine,
	encodeKitty,
	deleteKittyImage,
	deleteAllKittyImages,
	encodeITerm2,
	calculateImageCellSize,
	calculateImageRows,
	getPngDimensions,
	getJpegDimensions,
	getGifDimensions,
	getWebpDimensions,
	getImageDimensions,
	renderImage,
	hyperlink,
	imageFallback,
} = await import(pathToFileURL(terminalImagePath).href);

const terminalPath = join(TUI_SRC, "terminal.ts");
const { parseKeyboardProtocolNegotiationSequence, normalizeAppleTerminalInput } = await import(
	pathToFileURL(terminalPath).href,
);

const tuiPath = join(TUI_SRC, "tui.ts");
const mainScreenPath = join(TUI_SRC, "tui-main-screen.ts");
// Upstream Pi renamed the class `TuiBase` and made it abstract (older Pi exported
// the concrete `TUI` from tui.ts). The Wave-4 fixture was captured against the
// concrete main-screen TUI — import that so `new TUI(terminal, false)` (and the
// abstract `doRender`) resolves.
const { TuiMainScreen: TUI, Container } = await import(pathToFileURL(mainScreenPath).href);

// feat-006 Wave 5 components
const COMPONENTS_SRC = join(TUI_SRC, "components");
const { Text } = await import(pathToFileURL(join(COMPONENTS_SRC, "text.ts")).href);
const { TruncatedText } = await import(pathToFileURL(join(COMPONENTS_SRC, "truncated-text.ts")).href);
const { Spacer } = await import(pathToFileURL(join(COMPONENTS_SRC, "spacer.ts")).href);
const { SelectList } = await import(pathToFileURL(join(COMPONENTS_SRC, "select-list.ts")).href);
const { Input } = await import(pathToFileURL(join(COMPONENTS_SRC, "input.ts")).href);
const { Box: PiBox } = await import(pathToFileURL(join(COMPONENTS_SRC, "box.ts")).href);
const { Image: PiImage } = await import(pathToFileURL(join(COMPONENTS_SRC, "image.ts")).href);
const { SettingsList } = await import(pathToFileURL(join(COMPONENTS_SRC, "settings-list.ts")).href);
const { getCellDimensions, setCellDimensions } = await import(pathToFileURL(terminalImagePath).href);
const { Editor } = await import(pathToFileURL(join(COMPONENTS_SRC, "editor.ts")).href);
const { Markdown } = await import(pathToFileURL(join(COMPONENTS_SRC, "markdown.ts")).href);

const bgFn = (text) => `<bg>${text}</bg>`;

const records = [];
const c = (note, fn, args, result) => records.push({ note, fn, args, result });

// ===========================================================================
// visibleWidth
// ===========================================================================
c("ascii-plain", "visibleWidth", ["hello world"], visibleWidth("hello world"));
c("empty-string", "visibleWidth", [""], visibleWidth(""));
c("cjk-wide-japanese", "visibleWidth", ["日本語"], visibleWidth("日本語"));
c("cjk-wide-chinese", "visibleWidth", ["中文测试"], visibleWidth("中文测试"));
c("emoji-single-grinning", "visibleWidth", ["😀"], visibleWidth("😀"));
c("emoji-vs16-warning", "visibleWidth", ["⚠️"], visibleWidth("⚠️"));
c("emoji-family-zwj", "visibleWidth", ["👨‍👩‍👧‍👦"], visibleWidth("👨‍👩‍👧‍👦"));
c("emoji-skin-tone", "visibleWidth", ["👍🏽"], visibleWidth("👍🏽"));
c("emoji-flag-pair", "visibleWidth", ["🇺🇸"], visibleWidth("🇺🇸"));
c("combining-mark-alone", "visibleWidth", ["́"], visibleWidth("́"));
c("combining-mark-with-base", "visibleWidth", ["é"], visibleWidth("é"));
c("zwj-alone", "visibleWidth", ["‍"], visibleWidth("‍"));
c("thai-am-vowel", "visibleWidth", ["ำ"], visibleWidth("ำ"));
c("lao-am-vowel", "visibleWidth", ["ຳ"], visibleWidth("ຳ"));
c("thai-base-plus-am", "visibleWidth", ["กำ"], visibleWidth("กำ"));
c("tabs-only", "visibleWidth", ["\t\t"], visibleWidth("\t\t"));
c("ansi-sgr-only-zero-width", "visibleWidth", ["\x1b[1;31m"], visibleWidth("\x1b[1;31m"));
c("ansi-sgr-plus-text", "visibleWidth", ["\x1b[1mhi\x1b[0m"], visibleWidth("\x1b[1mhi\x1b[0m"));
c("ansi-256-color", "visibleWidth", ["\x1b[38;5;200mhi"], visibleWidth("\x1b[38;5;200mhi"));
c("ansi-rgb-color", "visibleWidth", ["\x1b[38;2;10;20;30mhi"], visibleWidth("\x1b[38;2;10;20;30mhi"));
c("osc8-hyperlink-bel", "visibleWidth", ["\x1b]8;;http://x\x07link\x1b]8;;\x07"], visibleWidth("\x1b]8;;http://x\x07link\x1b]8;;\x07"));
c("osc8-hyperlink-st", "visibleWidth", ["\x1b]8;;http://x\x1b\\link\x1b]8;;\x1b\\"], visibleWidth("\x1b]8;;http://x\x1b\\link\x1b]8;;\x1b\\"));
c("mixed-ansi-tabs-cjk", "visibleWidth", ["\x1b[1m日\t本\x1b[0m"], visibleWidth("\x1b[1m日\t本\x1b[0m"));
c("halfwidth-fullwidth-form", "visibleWidth", ["ｱ"], visibleWidth("ｱ"));

// ===========================================================================
// wrapTextWithAnsi
// ===========================================================================
c("wrap-short-line-no-wrap", "wrapTextWithAnsi", ["hello", 10], wrapTextWithAnsi("hello", 10));
c("wrap-simple-words", "wrapTextWithAnsi", ["the quick brown fox jumps", 10], wrapTextWithAnsi("the quick brown fox jumps", 10));
c(
	"wrap-single-word-too-long",
	"wrapTextWithAnsi",
	["supercalifragilisticexpialidocious", 10],
	wrapTextWithAnsi("supercalifragilisticexpialidocious", 10),
);
c(
	"wrap-ansi-preserved-across-lines",
	"wrapTextWithAnsi",
	["\x1b[1mbold text that must wrap across multiple lines here\x1b[0m", 12],
	wrapTextWithAnsi("\x1b[1mbold text that must wrap across multiple lines here\x1b[0m", 12),
);
c(
	"wrap-underline-reset-at-break",
	"wrapTextWithAnsi",
	["\x1b[4munderlined text wrapping test\x1b[0m", 10],
	wrapTextWithAnsi("\x1b[4munderlined text wrapping test\x1b[0m", 10),
);
c("wrap-cjk-forces-break", "wrapTextWithAnsi", ["日本語のテキストです長い文章", 6], wrapTextWithAnsi("日本語のテキストです長い文章", 6));
c("wrap-embedded-newline", "wrapTextWithAnsi", ["line one\nline two is a bit longer than ten", 10], wrapTextWithAnsi("line one\nline two is a bit longer than ten", 10));
c("wrap-crlf-newline", "wrapTextWithAnsi", ["line one\r\nline two", 20], wrapTextWithAnsi("line one\r\nline two", 20));
c("wrap-lone-cr-newline", "wrapTextWithAnsi", ["line one\rline two", 20], wrapTextWithAnsi("line one\rline two", 20));
c("wrap-empty-string", "wrapTextWithAnsi", ["", 10], wrapTextWithAnsi("", 10));
c(
	"wrap-hyperlink-close-reopen-at-break",
	"wrapTextWithAnsi",
	["\x1b]8;;http://example.test\x07click here to visit the site now\x1b]8;;\x07", 12],
	wrapTextWithAnsi("\x1b]8;;http://example.test\x07click here to visit the site now\x1b]8;;\x07", 12),
);

// ===========================================================================
// truncateToWidth
// ===========================================================================
c("truncate-fits-exactly", "truncateToWidth", ["hello", 5, "...", false], truncateToWidth("hello", 5, "...", false));
c("truncate-simple", "truncateToWidth", ["hello world", 8, "...", false], truncateToWidth("hello world", 8, "...", false));
c("truncate-with-pad", "truncateToWidth", ["hi", 8, "...", true], truncateToWidth("hi", 8, "...", true));
c("truncate-no-ellipsis", "truncateToWidth", ["hello world", 5, "", false], truncateToWidth("hello world", 5, "", false));
c("truncate-ellipsis-wider-than-max", "truncateToWidth", ["hello world", 2, "...", false], truncateToWidth("hello world", 2, "...", false));
c("truncate-ellipsis-equal-to-max", "truncateToWidth", ["hello world", 3, "...", false], truncateToWidth("hello world", 3, "...", false));
c("truncate-max-width-zero", "truncateToWidth", ["hello", 0, "...", false], truncateToWidth("hello", 0, "...", false));
c("truncate-empty-text-padded", "truncateToWidth", ["", 5, "...", true], truncateToWidth("", 5, "...", true));
c("truncate-empty-text-unpadded", "truncateToWidth", ["", 5, "...", false], truncateToWidth("", 5, "...", false));
c("truncate-cjk-text", "truncateToWidth", ["日本語のテキスト", 6, "...", false], truncateToWidth("日本語のテキスト", 6, "...", false));
c("truncate-ansi-text", "truncateToWidth", ["\x1b[1mhello world\x1b[0m", 8, "...", false], truncateToWidth("\x1b[1mhello world\x1b[0m", 8, "...", false));
c("truncate-ansi-with-pad", "truncateToWidth", ["\x1b[1mhi\x1b[0m", 8, "...", true], truncateToWidth("\x1b[1mhi\x1b[0m", 8, "...", true));
c("truncate-tabs-text", "truncateToWidth", ["a\tb\tc\td\te", 6, "...", false], truncateToWidth("a\tb\tc\td\te", 6, "...", false));
c("truncate-emoji-boundary", "truncateToWidth", ["a😀b😀c😀d", 5, "...", false], truncateToWidth("a😀b😀c😀d", 5, "...", false));

// ===========================================================================
// sliceByColumn / sliceWithWidth
// ===========================================================================
c("slice-basic-ascii", "sliceByColumn", ["hello world", 6, 5, false], sliceByColumn("hello world", 6, 5, false));
c("slice-with-width-basic", "sliceWithWidth", ["hello world", 0, 5, false], sliceWithWidth("hello world", 0, 5, false));
c("slice-wide-char-boundary-lenient", "sliceWithWidth", ["ab日本cd", 1, 3, false], sliceWithWidth("ab日本cd", 1, 3, false));
c("slice-wide-char-boundary-strict", "sliceWithWidth", ["ab日本cd", 1, 3, true], sliceWithWidth("ab日本cd", 1, 3, true));
c("slice-zero-length", "sliceWithWidth", ["hello", 0, 0, false], sliceWithWidth("hello", 0, 0, false));
c("slice-with-ansi", "sliceWithWidth", ["\x1b[1mhello world\x1b[0m", 6, 5, false], sliceWithWidth("\x1b[1mhello world\x1b[0m", 6, 5, false));
c("slice-past-end", "sliceWithWidth", ["hi", 0, 10, false], sliceWithWidth("hi", 0, 10, false));

// ===========================================================================
// extractSegments
// ===========================================================================
c("extract-segments-basic", "extractSegments", ["hello world", 5, 6, 5, false], extractSegments("hello world", 5, 6, 5, false));
c(
	"extract-segments-inherits-style-for-after",
	"extractSegments",
	["\x1b[1mhello\x1b[0m world", 5, 11, 5, false],
	extractSegments("\x1b[1mhello\x1b[0m world", 5, 11, 5, false),
);
c("extract-segments-wide-char-strict-after", "extractSegments", ["ab日本cd", 2, 3, 2, true], extractSegments("ab日本cd", 2, 3, 2, true));
c("extract-segments-no-after", "extractSegments", ["hello world", 5, 5, 0, false], extractSegments("hello world", 5, 5, 0, false));
c("extract-segments-ansi-straddling-before-after", "extractSegments", ["\x1b[1ma\x1b[3mb\x1b[0mc", 1, 2, 1, false], extractSegments("\x1b[1ma\x1b[3mb\x1b[0mc", 1, 2, 1, false));

// ===========================================================================
// normalizeTerminalOutput
// ===========================================================================
c("normalize-plain-text", "normalizeTerminalOutput", ["hello"], normalizeTerminalOutput("hello"));
c("normalize-tabs", "normalizeTerminalOutput", ["a\tb"], normalizeTerminalOutput("a\tb"));
c("normalize-thai-am", "normalizeTerminalOutput", ["กำข"], normalizeTerminalOutput("กำข"));
c("normalize-lao-am", "normalizeTerminalOutput", ["ຳ"], normalizeTerminalOutput("ຳ"));
c("normalize-tabs-with-ansi", "normalizeTerminalOutput", ["\x1b[1ma\tb\x1b[0m"], normalizeTerminalOutput("\x1b[1ma\tb\x1b[0m"));
c("normalize-thai-am-and-tabs", "normalizeTerminalOutput", ["กำ\tข"], normalizeTerminalOutput("กำ\tข"));

// ===========================================================================
// extractAnsiCode
// ===========================================================================
c("extract-ansi-csi-sgr", "extractAnsiCode", ["\x1b[1;31mtext", 0], extractAnsiCode("\x1b[1;31mtext", 0));
c("extract-ansi-csi-cursor", "extractAnsiCode", ["\x1b[2Ktext", 0], extractAnsiCode("\x1b[2Ktext", 0));
c("extract-ansi-osc8-bel", "extractAnsiCode", ["\x1b]8;;http://x\x07rest", 0], extractAnsiCode("\x1b]8;;http://x\x07rest", 0));
c("extract-ansi-osc8-st", "extractAnsiCode", ["\x1b]8;;http://x\x1b\\rest", 0], extractAnsiCode("\x1b]8;;http://x\x1b\\rest", 0));
c("extract-ansi-apc-cursor-marker", "extractAnsiCode", ["\x1b_pi:c\x07rest", 0], extractAnsiCode("\x1b_pi:c\x07rest", 0));
c("extract-ansi-not-escape", "extractAnsiCode", ["hello", 0], extractAnsiCode("hello", 0));
c("extract-ansi-mid-string", "extractAnsiCode", ["ab\x1b[1mcd", 2], extractAnsiCode("ab\x1b[1mcd", 2));
c("extract-ansi-unterminated-csi", "extractAnsiCode", ["\x1b[1;31", 0], extractAnsiCode("\x1b[1;31", 0));
c("extract-ansi-unterminated-osc", "extractAnsiCode", ["\x1b]8;;http://x", 0], extractAnsiCode("\x1b]8;;http://x", 0));

// ===========================================================================
// isWhitespaceChar / isPunctuationChar
// ===========================================================================
for (const ch of [" ", "\t", "\n", "a", "1", ".", "(", "@", "_", ""]) {
	c(`is-whitespace-char-${JSON.stringify(ch)}`, "isWhitespaceChar", [ch], isWhitespaceChar(ch));
	c(`is-punctuation-char-${JSON.stringify(ch)}`, "isPunctuationChar", [ch], isPunctuationChar(ch));
}

// ===========================================================================
// applyBackgroundToLine
// ===========================================================================
c("apply-bg-basic-padding", "applyBackgroundToLine", ["hi", 5], applyBackgroundToLine("hi", 5, bgFn));
c("apply-bg-exact-width", "applyBackgroundToLine", ["hello", 5], applyBackgroundToLine("hello", 5, bgFn));
c("apply-bg-wide-chars", "applyBackgroundToLine", ["日本", 6], applyBackgroundToLine("日本", 6, bgFn));

// ===========================================================================
// KEYS (feat-006 Wave 2)
// ===========================================================================
const KEYS_OUT_FILE = join(OUT_DIR, "keys.cases.jsonl");
const keysRecords = [];

const ENV_KEYS = ["WT_SESSION", "SSH_CONNECTION", "SSH_CLIENT", "SSH_TTY"];
function withEnv(envOverrides, fn) {
	const saved = {};
	for (const k of ENV_KEYS) {
		saved[k] = process.env[k];
		delete process.env[k];
	}
	if (envOverrides) {
		for (const [k, v] of Object.entries(envOverrides)) process.env[k] = v;
	}
	try {
		return fn();
	} finally {
		for (const k of ENV_KEYS) {
			if (saved[k] === undefined) delete process.env[k];
			else process.env[k] = saved[k];
		}
	}
}

// mod is the 0-indexed bitmask (shift=1,alt=2,ctrl=4,super=8); wire format is 1-indexed.
function kittyCsiU(codepoint, mod, { shifted, base, event } = {}) {
	let keyPart = String(codepoint);
	if (shifted !== undefined || base !== undefined) {
		keyPart += `:${shifted !== undefined ? shifted : ""}`;
		if (base !== undefined) keyPart += `:${base}`;
	}
	let modPart = "";
	if (mod !== undefined) {
		modPart = `;${mod + 1}`;
		if (event !== undefined) modPart += `:${event}`;
	}
	return `\x1b[${keyPart}${modPart}u`;
}
function modifyOtherKeys(codepoint, mod) {
	return `\x1b[27;${mod + 1};${codepoint}~`;
}
function kittyArrow(letter, mod, event) {
	return `\x1b[1;${mod + 1}${event !== undefined ? `:${event}` : ""}${letter}`;
}
function kittyFunctional(num, mod, event) {
	const modPart = mod !== undefined ? `;${mod + 1}` : "";
	const evtPart = event !== undefined ? `:${event}` : "";
	return `\x1b[${num}${modPart}${evtPart}~`;
}
function kittyHomeEnd(letter, mod, event) {
	return `\x1b[1;${mod + 1}${event !== undefined ? `:${event}` : ""}${letter}`;
}

function kc(note, kittyActive, fn, args, envOverrides) {
	const run = () => {
		setKittyProtocolActive(kittyActive);
		let fnImpl;
		if (fn === "matchesKey") fnImpl = matchesKey;
		else if (fn === "parseKey") fnImpl = parseKey;
		else if (fn === "decodeKittyPrintable") fnImpl = decodeKittyPrintable;
		else if (fn === "decodePrintableKey") fnImpl = decodePrintableKey;
		else if (fn === "isKeyRelease") fnImpl = isKeyRelease;
		else if (fn === "isKeyRepeat") fnImpl = isKeyRepeat;
		else throw new Error(`unknown fn ${fn}`);
		const result = fnImpl(...args) ?? null;
		return result;
	};
	const result = envOverrides ? withEnv(envOverrides, run) : run();
	keysRecords.push({ note, kittyActive, envOverrides: envOverrides ?? null, fn, args, result });
}

// --- escape / space / tab / enter / backspace, kitty active + inactive ------
for (const kitty of [false, true]) {
	kc(`escape-plain-esc-kitty${kitty}`, kitty, "matchesKey", ["\x1b", "escape"]);
	kc(`escape-csiu-kitty${kitty}`, kitty, "matchesKey", [kittyCsiU(27, 0), "escape"]);
	kc(`escape-modifyotherkeys-kitty${kitty}`, kitty, "matchesKey", [modifyOtherKeys(27, 0), "escape"]);
	kc(`escape-with-modifier-always-false-kitty${kitty}`, kitty, "matchesKey", ["\x1b", "ctrl+escape"]);

	kc(`space-plain-kitty${kitty}`, kitty, "matchesKey", [" ", "space"]);
	kc(`space-ctrl-legacy-null-kitty${kitty}`, kitty, "matchesKey", ["\x00", "ctrl+space"]);
	kc(`space-alt-legacy-kitty${kitty}`, kitty, "matchesKey", ["\x1b ", "alt+space"]);
	kc(`space-csiu-ctrl-kitty${kitty}`, kitty, "matchesKey", [kittyCsiU(32, 4), "ctrl+space"]);

	kc(`tab-plain-kitty${kitty}`, kitty, "matchesKey", ["\t", "tab"]);
	kc(`tab-shift-legacy-kitty${kitty}`, kitty, "matchesKey", ["\x1b[Z", "shift+tab"]);
	kc(`tab-shift-csiu-kitty${kitty}`, kitty, "matchesKey", [kittyCsiU(9, 1), "shift+tab"]);
	kc(`tab-ctrl-csiu-kitty${kitty}`, kitty, "matchesKey", [kittyCsiU(9, 4), "ctrl+tab"]);

	kc(`enter-plain-cr-kitty${kitty}`, kitty, "matchesKey", ["\r", "enter"]);
	kc(`enter-plain-lf-kitty${kitty}`, kitty, "matchesKey", ["\n", "enter"]);
	kc(`enter-shift-esc-cr-kitty${kitty}`, kitty, "matchesKey", ["\x1b\r", "shift+enter"]);
	kc(`enter-shift-lf-kitty${kitty}`, kitty, "matchesKey", ["\n", "shift+enter"]);
	kc(`enter-alt-esc-cr-kitty${kitty}`, kitty, "matchesKey", ["\x1b\r", "alt+enter"]);
	kc(`enter-shift-csiu-kitty${kitty}`, kitty, "matchesKey", [kittyCsiU(13, 1), "shift+enter"]);
	kc(`enter-alt-csiu-kpenter-kitty${kitty}`, kitty, "matchesKey", [kittyCsiU(57414, 2), "alt+enter"]);
	kc(`enter-ctrl-csiu-kitty${kitty}`, kitty, "matchesKey", [kittyCsiU(13, 4), "ctrl+enter"]);
	kc(`enter-ss3-numpad-kitty${kitty}`, kitty, "matchesKey", ["\x1bOM", "enter"]);

	kc(`backspace-plain-del-kitty${kitty}`, kitty, "matchesKey", ["\x7f", "backspace"]);
	kc(`backspace-alt-esc-del-kitty${kitty}`, kitty, "matchesKey", ["\x1b\x7f", "alt+backspace"]);
	kc(`backspace-alt-esc-bs-kitty${kitty}`, kitty, "matchesKey", ["\x1b\x08", "alt+backspace"]);
	kc(`backspace-alt-csiu-kitty${kitty}`, kitty, "matchesKey", [kittyCsiU(127, 2), "alt+backspace"]);
	kc(`backspace-ctrl-csiu-kitty${kitty}`, kitty, "matchesKey", [kittyCsiU(127, 4), "ctrl+backspace"]);
}

// --- raw backspace / 0x08 ambiguity under every env combo -------------------
for (const env of [
	{},
	{ WT_SESSION: "1" },
	{ WT_SESSION: "1", SSH_CONNECTION: "x" },
	{ WT_SESSION: "1", SSH_CLIENT: "x" },
	{ WT_SESSION: "1", SSH_TTY: "x" },
]) {
	const label = Object.keys(env).length === 0 ? "no-env" : Object.entries(env).map(([k, v]) => `${k}=${v}`).join(",");
	kc(`raw-0x08-backspace-matcheskey-plain-${label}`, false, "matchesKey", ["\x08", "backspace"], env);
	kc(`raw-0x08-backspace-matcheskey-ctrl-${label}`, false, "matchesKey", ["\x08", "ctrl+backspace"], env);
	kc(`raw-0x08-parsekey-${label}`, false, "parseKey", ["\x08"], env);
}

// --- insert / delete / clear / home / end / pageUp / pageDown ---------------
for (const [name, legacyPlain, legacyShift, legacyCtrl, fnCode] of [
	["insert", "\x1b[2~", "\x1b[2$", "\x1b[2^", 2],
	["delete", "\x1b[3~", "\x1b[3$", "\x1b[3^", 3],
	["pageUp", "\x1b[5~", "\x1b[5$", "\x1b[5^", 5],
	["pageDown", "\x1b[6~", "\x1b[6$", "\x1b[6^", 6],
	["home", "\x1b[H", "\x1b[7$", "\x1b[7^", 7],
	["end", "\x1b[F", "\x1b[8$", "\x1b[8^", 8],
]) {
	const keyId = name.toLowerCase();
	kc(`${name}-legacy-plain`, false, "matchesKey", [legacyPlain, keyId]);
	kc(`${name}-legacy-shift`, false, "matchesKey", [legacyShift, `shift+${keyId}`]);
	kc(`${name}-legacy-ctrl`, false, "matchesKey", [legacyCtrl, `ctrl+${keyId}`]);
	kc(`${name}-csiu-plain`, false, "matchesKey", [kittyFunctional(fnCode), keyId]);
	kc(`${name}-csiu-ctrl`, false, "matchesKey", [kittyFunctional(fnCode, 4), `ctrl+${keyId}`]);
}
kc("clear-legacy-plain", false, "matchesKey", ["\x1b[E", "clear"]);
kc("clear-legacy-shift", false, "matchesKey", ["\x1b[e", "shift+clear"]);
kc("clear-legacy-ctrl", false, "matchesKey", ["\x1bOe", "ctrl+clear"]);

// --- arrows -------------------------------------------------------------
for (const [name, letter, arrowCp] of [
	["up", "A", -1],
	["down", "B", -2],
	["right", "C", -3],
	["left", "D", -4],
]) {
	kc(`${name}-legacy-plain`, false, "matchesKey", [`\x1b[${letter}`, name]);
	kc(`${name}-csiu-plain`, false, "matchesKey", [kittyCsiU(arrowCp, 0), name]);
	kc(`${name}-legacy-shift`, false, "matchesKey", [`\x1b[${letter.toLowerCase()}`, `shift+${name}`]);
	kc(`${name}-csiu-shift`, false, "matchesKey", [kittyArrow(letter, 1), `shift+${name}`]);
}
kc("up-alt-legacy", false, "matchesKey", ["\x1bp", "alt+up"]);
kc("down-alt-legacy", false, "matchesKey", ["\x1bn", "alt+down"]);
kc("left-alt-special", false, "matchesKey", ["\x1b[1;3D", "alt+left"]);
kc("left-alt-legacy-b", false, "matchesKey", ["\x1bb", "alt+left"]);
kc("left-alt-legacy-B-kitty-inactive", false, "matchesKey", ["\x1bB", "alt+left"]);
kc("left-alt-legacy-B-kitty-active-false", true, "matchesKey", ["\x1bB", "alt+left"]);
kc("left-ctrl-special", false, "matchesKey", ["\x1b[1;5D", "ctrl+left"]);
kc("right-alt-special", false, "matchesKey", ["\x1b[1;3C", "alt+right"]);
kc("right-alt-legacy-f", false, "matchesKey", ["\x1bf", "alt+right"]);
kc("right-ctrl-special", false, "matchesKey", ["\x1b[1;5C", "ctrl+right"]);

// --- f1-f12 -------------------------------------------------------------
for (const [i, seq] of [
	[1, "\x1bOP"],
	[2, "\x1bOQ"],
	[3, "\x1bOR"],
	[4, "\x1bOS"],
	[5, "\x1b[15~"],
	[6, "\x1b[17~"],
	[7, "\x1b[18~"],
	[8, "\x1b[19~"],
	[9, "\x1b[20~"],
	[10, "\x1b[21~"],
	[11, "\x1b[23~"],
	[12, "\x1b[24~"],
]) {
	kc(`f${i}-legacy`, false, "matchesKey", [seq, `f${i}`]);
	kc(`f${i}-modified-always-false`, false, "matchesKey", [seq, `ctrl+f${i}`]);
}

// --- single letter/digit/symbol keys ------------------------------------
for (const letter of ["a", "k", "z"]) {
	const cp = letter.charCodeAt(0);
	kc(`letter-${letter}-plain`, false, "matchesKey", [letter, letter]);
	kc(`letter-${letter}-plain-csiu`, false, "matchesKey", [kittyCsiU(cp, 0), letter]);
	kc(`letter-${letter}-shift-uppercase`, false, "matchesKey", [letter.toUpperCase(), `shift+${letter}`]);
	kc(`letter-${letter}-shift-csiu`, false, "matchesKey", [kittyCsiU(cp - 32, 1), `shift+${letter}`]);
	kc(`letter-${letter}-ctrl-legacy`, false, "matchesKey", [String.fromCharCode(cp & 0x1f), `ctrl+${letter}`]);
	kc(`letter-${letter}-ctrl-csiu`, false, "matchesKey", [kittyCsiU(cp, 4), `ctrl+${letter}`]);
	kc(`letter-${letter}-ctrl-alt-legacy-kitty-inactive`, false, "matchesKey", [`\x1b${String.fromCharCode(cp & 0x1f)}`, `ctrl+alt+${letter}`]);
	kc(`letter-${letter}-alt-legacy-kitty-inactive`, false, "matchesKey", [`\x1b${letter}`, `alt+${letter}`]);
	kc(`letter-${letter}-shift-ctrl-csiu`, false, "matchesKey", [kittyCsiU(cp, 5), `shift+ctrl+${letter}`]);
}
kc("digit-1-plain", false, "matchesKey", ["1", "1"]);
kc("digit-1-ctrl-csiu", false, "matchesKey", [kittyCsiU(49, 4), "ctrl+1"]);
for (const sym of ["[", "\\", "]", "_", "-", "/"]) {
	const cp = sym.charCodeAt(0);
	kc(`symbol-${JSON.stringify(sym)}-plain`, false, "matchesKey", [sym, sym]);
	const rawCtrl = sym === "-" ? String.fromCharCode(31) : String.fromCharCode(cp & 0x1f);
	kc(`symbol-${JSON.stringify(sym)}-ctrl-legacy`, false, "matchesKey", [rawCtrl, `ctrl+${sym}`]);
}

// --- base-layout-key non-Latin fallback ---------------------------------
// Cyrillic "с" (U+0441) reported with baseLayoutKey=99 ('c') — should match ctrl+c.
kc(
	"base-layout-fallback-cyrillic-matches",
	false,
	"matchesKey",
	[kittyCsiU(0x0441, 4, { base: 99 }), "ctrl+c"],
);
// codepoint IS already a recognized Latin letter ('a', base='b') — must NOT fall back.
kc(
	"base-layout-fallback-guarded-when-codepoint-is-latin",
	false,
	"matchesKey",
	[kittyCsiU(97, 4, { base: 98 }), "ctrl+b"],
);

// --- numpad / functional-key normalization ------------------------------
kc("numpad-kp0-normalizes-to-digit-0", false, "matchesKey", [kittyCsiU(57399, 0), "0"]);
kc("numpad-kp-add-normalizes-to-plus", false, "matchesKey", [kittyCsiU(57413, 0), "+"]);

// --- decodeKittyPrintable / decodePrintableKey --------------------------
kc("decode-kitty-printable-plain", false, "decodeKittyPrintable", [kittyCsiU(97, 0)]);
kc("decode-kitty-printable-shifted-preferred", false, "decodeKittyPrintable", [kittyCsiU(97, 1, { shifted: 65 })]);
kc("decode-kitty-printable-ctrl-rejected", false, "decodeKittyPrintable", [kittyCsiU(97, 4)]);
kc("decode-kitty-printable-alt-rejected", false, "decodeKittyPrintable", [kittyCsiU(97, 2)]);
kc("decode-kitty-printable-super-rejected", false, "decodeKittyPrintable", [kittyCsiU(97, 8)]);
kc("decode-kitty-printable-control-codepoint-rejected", false, "decodeKittyPrintable", [kittyCsiU(9, 0)]);
kc("decode-kitty-printable-not-csiu", false, "decodeKittyPrintable", ["a"]);
kc("decode-modify-other-keys-printable-plain", false, "decodePrintableKey", [modifyOtherKeys(97, 1)]);
kc("decode-modify-other-keys-printable-ctrl-rejected", false, "decodePrintableKey", [modifyOtherKeys(97, 4)]);
kc("decode-printable-key-prefers-kitty", false, "decodePrintableKey", [kittyCsiU(98, 0)]);
kc("decode-printable-key-neither", false, "decodePrintableKey", ["\x1b[999999"]);

// --- isKeyRelease / isKeyRepeat -----------------------------------------
for (const terminator of ["u", "~", "A", "B", "C", "D", "H", "F"]) {
	kc(`is-key-release-:3${terminator}`, false, "isKeyRelease", [`\x1b[97;5:3${terminator}`]);
	kc(`is-key-repeat-:2${terminator}`, false, "isKeyRepeat", [`\x1b[97;5:2${terminator}`]);
}
kc("is-key-release-paste-guard", false, "isKeyRelease", ["\x1b[200~90:62:3F:A5\x1b[201~"]);
kc("is-key-repeat-paste-guard", false, "isKeyRepeat", ["\x1b[200~90:62:2F:A5\x1b[201~"]);
kc("is-key-release-false-for-plain-key", false, "isKeyRelease", ["a"]);

// --- parseKey fallback chain --------------------------------------------
for (const kitty of [false, true]) {
	kc(`parsekey-csiu-kitty${kitty}`, kitty, "parseKey", [kittyCsiU(97, 4)]);
	kc(`parsekey-modifyotherkeys-kitty${kitty}`, kitty, "parseKey", [modifyOtherKeys(97, 1)]);
	kc(`parsekey-shift-enter-esc-cr-kitty${kitty}`, kitty, "parseKey", ["\x1b\r"]);
	kc(`parsekey-shift-enter-lf-kitty${kitty}`, kitty, "parseKey", ["\n"]);
	kc(`parsekey-legacy-table-shift-up-kitty${kitty}`, kitty, "parseKey", ["\x1b[a"]);
	kc(`parsekey-escape-kitty${kitty}`, kitty, "parseKey", ["\x1b"]);
	kc(`parsekey-ctrl-backslash-kitty${kitty}`, kitty, "parseKey", ["\x1c"]);
	kc(`parsekey-ctrl-rbracket-kitty${kitty}`, kitty, "parseKey", ["\x1d"]);
	kc(`parsekey-ctrl-hyphen-kitty${kitty}`, kitty, "parseKey", ["\x1f"]);
	kc(`parsekey-ctrl-alt-lbracket-kitty${kitty}`, kitty, "parseKey", ["\x1b\x1b"]);
	kc(`parsekey-tab-kitty${kitty}`, kitty, "parseKey", ["\t"]);
	kc(`parsekey-enter-cr-kitty${kitty}`, kitty, "parseKey", ["\r"]);
	kc(`parsekey-ctrl-space-kitty${kitty}`, kitty, "parseKey", ["\x00"]);
	kc(`parsekey-space-kitty${kitty}`, kitty, "parseKey", [" "]);
	kc(`parsekey-backspace-del-kitty${kitty}`, kitty, "parseKey", ["\x7f"]);
	kc(`parsekey-shift-tab-kitty${kitty}`, kitty, "parseKey", ["\x1b[Z"]);
	kc(`parsekey-alt-enter-kitty${kitty}`, kitty, "parseKey", ["\x1b\r"]);
	kc(`parsekey-alt-space-kitty${kitty}`, kitty, "parseKey", ["\x1b "]);
	kc(`parsekey-alt-backspace-del-kitty${kitty}`, kitty, "parseKey", ["\x1b\x7f"]);
	kc(`parsekey-alt-left-B-kitty${kitty}`, kitty, "parseKey", ["\x1bB"]);
	kc(`parsekey-alt-right-F-kitty${kitty}`, kitty, "parseKey", ["\x1bF"]);
	kc(`parsekey-ctrl-alt-letter-kitty${kitty}`, kitty, "parseKey", ["\x1b\x01"]);
	kc(`parsekey-alt-letter-kitty${kitty}`, kitty, "parseKey", ["\x1bq"]);
	kc(`parsekey-alt-digit-kitty${kitty}`, kitty, "parseKey", ["\x1b5"]);
	kc(`parsekey-alt-symbol-kitty${kitty}`, kitty, "parseKey", ["\x1b/"]);
	kc(`parsekey-up-kitty${kitty}`, kitty, "parseKey", ["\x1b[A"]);
	kc(`parsekey-down-kitty${kitty}`, kitty, "parseKey", ["\x1b[B"]);
	kc(`parsekey-right-kitty${kitty}`, kitty, "parseKey", ["\x1b[C"]);
	kc(`parsekey-left-kitty${kitty}`, kitty, "parseKey", ["\x1b[D"]);
	kc(`parsekey-home-kitty${kitty}`, kitty, "parseKey", ["\x1b[H"]);
	kc(`parsekey-end-kitty${kitty}`, kitty, "parseKey", ["\x1b[F"]);
	kc(`parsekey-delete-kitty${kitty}`, kitty, "parseKey", ["\x1b[3~"]);
	kc(`parsekey-pageup-kitty${kitty}`, kitty, "parseKey", ["\x1b[5~"]);
	kc(`parsekey-pagedown-kitty${kitty}`, kitty, "parseKey", ["\x1b[6~"]);
	kc(`parsekey-raw-ctrl-letter-kitty${kitty}`, kitty, "parseKey", ["\x01"]);
	kc(`parsekey-raw-printable-passthrough-kitty${kitty}`, kitty, "parseKey", ["q"]);
	kc(`parsekey-unmatched-garbage-kitty${kitty}`, kitty, "parseKey", ["\x1b[999999zzz"]);
	kc(`parsekey-ss3-numpad-kitty${kitty}`, kitty, "parseKey", ["\x1bOM"]);
}
kc("parsekey-numpad-normalization", false, "parseKey", [kittyCsiU(57399, 0)]);
kc("parsekey-base-layout-non-latin-fallback", false, "parseKey", [kittyCsiU(0x0441, 4, { base: 99 })]);
kc("parsekey-shifted-letter-identity", false, "parseKey", [kittyCsiU(65, 1)]);

// --- unmatched / garbage --------------------------------------------------
kc("matcheskey-unmatched-garbage", false, "matchesKey", ["garbage-input-\x00\x01", "ctrl+c"]);
kc("matcheskey-unknown-keyid-empty", false, "matchesKey", ["a", ""]);

// ===========================================================================
// STDIN-BUFFER (feat-006 Wave 2)
// ===========================================================================
const STDIN_OUT_FILE = join(OUT_DIR, "stdin-buffer.cases.jsonl");
const stdinRecords = [];

function sb(note, calls) {
	const buf = new StdinBuffer();
	const events = [];
	buf.on("data", (v) => events.push({ type: "data", value: v }));
	buf.on("paste", (v) => events.push({ type: "paste", value: v }));
	for (const call of calls) {
		if (call.op === "process") {
			const arg = Array.isArray(call.data) ? Buffer.from(call.data) : call.data;
			buf.process(arg);
		} else if (call.op === "flush") {
			for (const seq of buf.flush()) events.push({ type: "data", value: seq });
		}
	}
	stdinRecords.push({ note, calls, events });
}

// Canonical fragmented-mouse-SGR example from the module's own doc comment.
sb("doc-comment-mouse-sgr-split-across-3-events", [
	{ op: "process", data: "\x1b" },
	{ op: "process", data: "[<35" },
	{ op: "process", data: ";20;5m" },
]);

sb("csi-whole", [{ op: "process", data: "\x1b[1;31m" }]);
sb("csi-byte-by-byte", "\x1b[1;31m".split("").map((ch) => ({ op: "process", data: ch })));
sb("osc-whole-bel", [{ op: "process", data: "\x1b]8;;http://x\x07" }]);
sb("osc-byte-by-byte-bel", "\x1b]8;;http://x\x07".split("").map((ch) => ({ op: "process", data: ch })));
sb("dcs-whole", [{ op: "process", data: "\x1bP>|pi\x1b\\" }]);
sb("apc-whole", [{ op: "process", data: "\x1b_Gpi:c\x1b\\" }]);
sb("ss3-whole", [{ op: "process", data: "\x1bOP" }]);
sb("old-mouse-whole", [{ op: "process", data: "\x1b[M\x20\x21\x22" }]);
sb("old-mouse-split", [
	{ op: "process", data: "\x1b[M" },
	{ op: "process", data: "\x20" },
	{ op: "process", data: "\x21\x22" },
]);

sb("paste-whole", [{ op: "process", data: "\x1b[200~hello world\x1b[201~" }]);
sb("paste-start-marker-fragmented", [
	{ op: "process", data: "\x1b[20" },
	{ op: "process", data: "0~pasted text\x1b[201~" },
]);
sb("paste-content-spans-multiple-calls", [
	{ op: "process", data: "\x1b[200~hello " },
	{ op: "process", data: "world\x1b[201~" },
]);
sb("paste-end-marker-fragmented", [
	{ op: "process", data: "\x1b[200~pasted text\x1b[20" },
	{ op: "process", data: "1~" },
]);
sb("paste-content-contains-mac-address-like-3F", [
	{ op: "process", data: "\x1b[200~90:62:3F:A5\x1b[201~" },
]);
sb("paste-preceded-by-plain-chars", [{ op: "process", data: "ab\x1b[200~pasted\x1b[201~" }]);
sb("paste-followed-by-remaining-data", [{ op: "process", data: "\x1b[200~pasted\x1b[201~more" }]);

// WezTerm double-escape: bare ESC (key press) immediately followed by a full
// Kitty CSI-u release sequence for the same key (module doc comment :208-230).
sb("wezterm-double-escape-splits-bare-esc-then-csiu", [
	{ op: "process", data: "\x1b\x1b[27;1:3;27u" },
]);

// Duplicate-raw-codepoint-after-Kitty-CSI-u suppression (emitDataSequence).
sb("kitty-csiu-then-duplicate-raw-codepoint-suppressed", [
	{ op: "process", data: "\x1b[97u" },
	{ op: "process", data: "a" },
]);
sb("kitty-csiu-then-different-raw-codepoint-not-suppressed", [
	{ op: "process", data: "\x1b[97u" },
	{ op: "process", data: "b" },
]);

// High-byte single-byte Buffer -> ESC + meta conversion.
sb("high-byte-buffer-converts-to-esc-meta", [{ op: "process", data: [200] }]);

// Unterminated sequence flushed via explicit flush() (simulating the 10ms timeout).
sb("unterminated-csi-flushed", [
	{ op: "process", data: "\x1b[1;3" },
	{ op: "flush" },
]);
sb("empty-process-emits-empty-data", [{ op: "process", data: "" }]);

// ===========================================================================
// WORD-NAVIGATION (feat-006 Wave 3)
// ===========================================================================
const WORD_NAV_OUT_FILE = join(OUT_DIR, "word-navigation.cases.jsonl");
const wordNavRecords = [];
function wn(note, fn, text, cursor) {
	const impl = fn === "findWordBackward" ? findWordBackward : findWordForward;
	wordNavRecords.push({ note, fn, text, cursor, result: impl(text, cursor) });
}

wn("backward-cursor-zero-short-circuits", "findWordBackward", "hello world", 0);
wn("forward-cursor-at-length-short-circuits", "findWordForward", "hello world", 11);
wn("forward-cursor-beyond-length-clamped", "findWordForward", "hi", 10);
wn("backward-mid-word", "findWordBackward", "hello wo", 8);
wn("forward-mid-word", "findWordForward", "hello world", 3);
wn("backward-mid-whitespace-run", "findWordBackward", "hello   world", 8);
wn("forward-mid-whitespace-run", "findWordForward", "hello   world", 6);
wn("backward-mid-punctuation-run", "findWordBackward", "foo!!!bar", 6);
wn("forward-mid-punctuation-run", "findWordForward", "foo!!!bar", 3);
wn("backward-word-with-trailing-punctuation", "findWordBackward", "foo.bar", 7);
wn("forward-word-with-leading-punctuation-boundary", "findWordForward", "foo.bar", 0);
wn("backward-arrow-punctuation", "findWordBackward", "foo->bar", 8);
wn("forward-arrow-punctuation", "findWordForward", "foo->bar", 0);
wn("backward-all-whitespace", "findWordBackward", "     ", 5);
wn("forward-all-whitespace", "findWordForward", "     ", 0);
wn("backward-all-punctuation", "findWordBackward", "!!!...", 6);
wn("forward-all-punctuation", "findWordForward", "!!!...", 0);
wn("backward-cjk-text", "findWordBackward", "日本語のテキスト", 8);
wn("forward-cjk-text", "findWordForward", "日本語のテキスト", 0);
wn("backward-emoji-boundary", "findWordBackward", "hello 😀 world", 8);
wn("forward-emoji-boundary", "findWordForward", "hello 😀 world", 6);
wn("backward-emoji-within-word-boundary", "findWordBackward", "a😀b", 3);
wn("forward-emoji-within-word-boundary", "findWordForward", "a😀b", 1);
wn("backward-combining-mark", "findWordBackward", "café bar", 11);
wn("forward-combining-mark", "findWordForward", "café bar", 0);
wn("backward-single-space-between-words", "findWordBackward", "foo bar", 7);
wn("forward-single-space-between-words", "findWordForward", "foo bar", 0);
wn("backward-empty-string", "findWordBackward", "", 0);
wn("forward-empty-string", "findWordForward", "", 0);

// ===========================================================================
// KEYBINDINGS (feat-006 Wave 3)
// ===========================================================================
const KEYBINDINGS_OUT_FILE = join(OUT_DIR, "keybindings.cases.jsonl");
const keybindingsRecords = [];
function kb(note, userBindings, calls) {
	const mgr = new KeybindingsManager(TUI_KEYBINDINGS, userBindings ?? {});
	const results = calls.map((call) => {
		if (call.fn === "matches") return mgr.matches(call.args[0], call.args[1]);
		if (call.fn === "getKeys") return mgr.getKeys(call.args[0]);
		if (call.fn === "getConflicts") return mgr.getConflicts();
		if (call.fn === "getResolvedBindings") return mgr.getResolvedBindings();
		if (call.fn === "getUserBindings") return mgr.getUserBindings();
		if (call.fn === "setUserBindings") {
			mgr.setUserBindings(call.args[0]);
			return null;
		}
		throw new Error(`unknown fn ${call.fn}`);
	});
	keybindingsRecords.push({ note, userBindings: userBindings ?? {}, calls, results });
}

kb("defaults-only-multi-key-binding", {}, [
	{ fn: "getKeys", args: ["tui.editor.cursorLeft"] },
	{ fn: "getKeys", args: ["tui.editor.cursorUp"] },
]);
kb("override-single-string-replaces-default", { "tui.editor.undo": "ctrl+z" }, [
	{ fn: "getKeys", args: ["tui.editor.undo"] },
	{ fn: "matches", args: ["\x1a", "tui.editor.undo"] },
	{ fn: "matches", args: ["\x1f", "tui.editor.undo"] },
]);
kb("override-array-form", { "tui.input.copy": ["ctrl+c", "ctrl+insert"] }, [
	{ fn: "getKeys", args: ["tui.input.copy"] },
]);
kb("conflict-two-bindings-claim-same-key", { "tui.editor.undo": "ctrl+x", "tui.editor.yank": "ctrl+x" }, [
	{ fn: "getConflicts", args: [] },
]);
kb(
	"unknown-binding-id-ignored-for-conflicts-but-kept-raw",
	{ "tui.editor.undo": "ctrl+z", "tui.nonexistent.thing": "ctrl+q" },
	[
		{ fn: "getConflicts", args: [] },
		{ fn: "getUserBindings", args: [] },
		{ fn: "getKeys", args: ["tui.editor.undo"] },
	],
);
kb("duplicate-keys-in-own-array-deduped", { "tui.input.copy": ["ctrl+c", "ctrl+c", "ctrl+insert"] }, [
	{ fn: "getKeys", args: ["tui.input.copy"] },
]);
kb("resolved-bindings-single-vs-array-unwrap", {}, [{ fn: "getResolvedBindings", args: [] }]);
kb("set-user-bindings-rebuilds-conflicts", {}, [
	{ fn: "getConflicts", args: [] },
	{ fn: "setUserBindings", args: [{ "tui.editor.undo": "ctrl+z", "tui.editor.yank": "ctrl+z" }] },
	{ fn: "getConflicts", args: [] },
]);

// ===========================================================================
// FUZZY (feat-006 Wave 3)
// ===========================================================================
const FUZZY_OUT_FILE = join(OUT_DIR, "fuzzy.cases.jsonl");
const fuzzyRecords = [];
function fm(note, query, text) {
	fuzzyRecords.push({ note, fn: "fuzzyMatch", query, text, result: fuzzyMatch(query, text) });
}
function ff(note, items, query) {
	fuzzyRecords.push({ note, fn: "fuzzyFilter", items, query, result: fuzzyFilter(items, query, (s) => s) });
}

fm("exact-match", "hello", "hello");
fm("no-match-query-longer", "helloworld", "hi");
fm("no-match-missing-char", "xyz", "hello");
fm("consecutive-match", "abc", "abcxyz");
fm("scattered-match", "abc", "axbxcx");
fm("word-boundary-match", "foo", "bar_foo_baz");
fm("mid-word-match", "oo", "foobar");
fm("case-insensitive", "HELLO", "hello world");
fm("gap-penalty-multiple-positions", "ac", "a_____c");
fm("numeric-alpha-swap-fires", "2fa", "fa2-setup");
fm("alpha-numeric-swap-fires", "fa2", "2fa-setup");
fm("swap-not-needed-when-primary-succeeds", "fa2", "fa2-setup");
fm("empty-query", "", "anything");
fm("empty-text", "abc", "");

ff("filter-multi-token-all-must-match", ["apple pie", "apple juice", "banana split"], "apple pie");
ff("filter-empty-query-returns-unchanged", ["a", "b", "c"], "   ");
ff("filter-one-token-no-items-match", ["apple", "banana"], "xyz");
ff("filter-ordering-by-score", ["zzzzza", "zzazzz", "zazzzz", "azzzzz"], "a");
ff("filter-slash-separated-tokens", ["src/main.rs", "src/lib.rs"], "src/main");

// ===========================================================================
// TERMINAL-COLORS (feat-006 Wave 4)
// ===========================================================================
const TERMINAL_COLORS_OUT_FILE = join(OUT_DIR, "terminal-colors.cases.jsonl");
const terminalColorsRecords = [];
function tc(note, fn, args) {
	const impl =
		fn === "isOsc11BackgroundColorResponse"
			? isOsc11BackgroundColorResponse
			: fn === "parseOsc11BackgroundColor"
				? parseOsc11BackgroundColor
				: parseTerminalColorSchemeReport;
	terminalColorsRecords.push({ note, fn, args, result: impl(...args) ?? null });
}

tc("osc11-hex6-bel", "parseOsc11BackgroundColor", ["\x1b]11;#1a2b3c\x07"]);
tc("osc11-hex6-st", "parseOsc11BackgroundColor", ["\x1b]11;#1a2b3c\x1b\\"]);
tc("osc11-hex12", "parseOsc11BackgroundColor", ["\x1b]11;#1111222233334444\x07".replace("4444", "3333")]);
tc("osc11-hex12-exact", "parseOsc11BackgroundColor", ["\x1b]11;#aaaa bbbb cccc".replaceAll(" ", "") + "\x07"]);
tc("osc11-rgb-4hex-channels", "parseOsc11BackgroundColor", ["\x1b]11;rgb:1a1a/2b2b/3c3c\x07"]);
tc("osc11-rgb-2hex-channels", "parseOsc11BackgroundColor", ["\x1b]11;rgb:1a/2b/3c\x07"]);
tc("osc11-rgb-1hex-channel", "parseOsc11BackgroundColor", ["\x1b]11;rgb:a/b/c\x07"]);
tc("osc11-rgba-prefix", "parseOsc11BackgroundColor", ["\x1b]11;rgba:1a/2b/3c\x07"]);
tc("osc11-malformed-channel", "parseOsc11BackgroundColor", ["\x1b]11;rgb:zz/2b/3c\x07"]);
tc("osc11-missing-terminator", "parseOsc11BackgroundColor", ["\x1b]11;#1a2b3c"]);
tc("osc11-not-a-response", "isOsc11BackgroundColorResponse", ["hello"]);
tc("osc11-is-response-bel", "isOsc11BackgroundColorResponse", ["\x1b]11;#1a2b3c\x07"]);
tc("osc11-is-response-st", "isOsc11BackgroundColorResponse", ["\x1b]11;#1a2b3c\x1b\\"]);
tc("color-scheme-dark", "parseTerminalColorSchemeReport", ["\x1b[?997;1n"]);
tc("color-scheme-light", "parseTerminalColorSchemeReport", ["\x1b[?997;2n"]);
tc("color-scheme-non-matching", "parseTerminalColorSchemeReport", ["\x1b[?997;3n"]);
tc("color-scheme-garbage", "parseTerminalColorSchemeReport", ["not-a-sequence"]);

// ===========================================================================
// TERMINAL-IMAGE (feat-006 Wave 4)
// ===========================================================================
const TERMINAL_IMAGE_OUT_FILE = join(OUT_DIR, "terminal-image.cases.jsonl");
const terminalImageRecords = [];
function ti(note, fn, args, result) {
	terminalImageRecords.push({ note, fn, args, result: result === undefined ? null : result });
}

const ENV_IMAGE_KEYS = [
	"TERM_PROGRAM",
	"TERMINAL_EMULATOR",
	"TERM",
	"COLORTERM",
	"KITTY_WINDOW_ID",
	"GHOSTTY_RESOURCES_DIR",
	"WEZTERM_PANE",
	"WARP_SESSION_ID",
	"WARP_TERMINAL_SESSION_UUID",
	"ITERM_SESSION_ID",
	"WT_SESSION",
	"TMUX",
];
function withImageEnv(overrides, fn) {
	const saved = {};
	for (const k of ENV_IMAGE_KEYS) saved[k] = process.env[k];
	for (const k of ENV_IMAGE_KEYS) delete process.env[k];
	Object.assign(process.env, overrides);
	try {
		return fn();
	} finally {
		for (const k of ENV_IMAGE_KEYS) delete process.env[k];
		for (const k of ENV_IMAGE_KEYS) if (saved[k] !== undefined) process.env[k] = saved[k];
	}
}
function detect(note, envOverrides, tmuxForwards) {
	const effectiveTmuxForwards = tmuxForwards ?? (() => false);
	const result = withImageEnv(envOverrides, () => detectCapabilities(effectiveTmuxForwards));
	terminalImageRecords.push({
		note,
		fn: "detectCapabilities",
		args: [envOverrides, effectiveTmuxForwards() ? "true" : "false"],
		result,
	});
}

detect("kitty-window-id", { KITTY_WINDOW_ID: "1" });
detect("ghostty-term-program", { TERM_PROGRAM: "ghostty" });
detect("ghostty-resources-dir", { GHOSTTY_RESOURCES_DIR: "/x" });
detect("wezterm-pane", { WEZTERM_PANE: "1" });
detect("warp-session-id", { WARP_SESSION_ID: "1" });
detect("warp-terminal-session-uuid", { WARP_TERMINAL_SESSION_UUID: "1" });
detect("iterm-session-id", { ITERM_SESSION_ID: "1" });
detect("iterm-term-program", { TERM_PROGRAM: "iTerm.app" });
detect("wt-session", { WT_SESSION: "1" });
detect("vscode-term-program", { TERM_PROGRAM: "vscode" });
detect("alacritty-term-program", { TERM_PROGRAM: "alacritty" });
detect("jetbrains-jediterm", { TERMINAL_EMULATOR: "JetBrains-JediTerm" });
detect("tmux-no-hyperlink-forward", { TMUX: "1" }, () => false);
detect("tmux-with-hyperlink-forward", { TMUX: "1" }, () => true);
detect("tmux-via-term-prefix", { TERM: "tmux-256color" }, () => false);
detect("screen-term-prefix", { TERM: "screen" });
detect("unknown-conservative-fallback", {});
detect("truecolor-hint-unknown-terminal", { COLORTERM: "truecolor" });

withImageEnv({}, () => {
	resetCapabilitiesCache();
	setCapabilities({ images: "kitty", trueColor: true, hyperlinks: true });
	terminalImageRecords.push({ note: "get-capabilities-uses-override", fn: "getCapabilities", args: [], result: getCapabilities() });
	resetCapabilitiesCache();
});

ti("is-image-line-kitty-prefix", "isImageLine", ["\x1b_Gsome-data"], isImageLine("\x1b_Gsome-data"));
ti("is-image-line-iterm2-prefix", "isImageLine", ["\x1b]1337;File=data"], isImageLine("\x1b]1337;File=data"));
ti("is-image-line-mid-line", "isImageLine", ["\x1b[1A\x1b_Gsome-data"], isImageLine("\x1b[1A\x1b_Gsome-data"));
ti("is-image-line-none", "isImageLine", ["plain text"], isImageLine("plain text"));

ti("encode-kitty-basic", "encodeKitty", ["aGVsbG8=", {}], encodeKitty("aGVsbG8=", {}));
ti(
	"encode-kitty-full-options",
	"encodeKitty",
	["aGVsbG8=", { columns: 10, rows: 5, imageId: 42, moveCursor: false }],
	encodeKitty("aGVsbG8=", { columns: 10, rows: 5, imageId: 42, moveCursor: false }),
);
const bigBase64 = "A".repeat(5000);
ti("encode-kitty-chunked", "encodeKitty", [bigBase64, { imageId: 1 }], encodeKitty(bigBase64, { imageId: 1 }));
ti("delete-kitty-image", "deleteKittyImage", [42], deleteKittyImage(42));
ti("delete-all-kitty-images", "deleteAllKittyImages", [], deleteAllKittyImages());

ti("encode-iterm2-basic", "encodeITerm2", ["aGVsbG8=", {}], encodeITerm2("aGVsbG8=", {}));
ti(
	"encode-iterm2-full-options",
	"encodeITerm2",
	["aGVsbG8=", { width: 10, height: "auto", name: "pic.png", preserveAspectRatio: false, inline: true }],
	encodeITerm2("aGVsbG8=", { width: 10, height: "auto", name: "pic.png", preserveAspectRatio: false, inline: true }),
);

ti(
	"calc-cell-size-width-constrained",
	"calculateImageCellSize",
	[{ widthPx: 1000, heightPx: 200 }, 20, undefined, { widthPx: 9, heightPx: 18 }],
	calculateImageCellSize({ widthPx: 1000, heightPx: 200 }, 20, undefined, { widthPx: 9, heightPx: 18 }),
);
ti(
	"calc-cell-size-height-constrained",
	"calculateImageCellSize",
	[{ widthPx: 200, heightPx: 1000 }, 40, 10, { widthPx: 9, heightPx: 18 }],
	calculateImageCellSize({ widthPx: 200, heightPx: 1000 }, 40, 10, { widthPx: 9, heightPx: 18 }),
);
ti(
	"calc-image-rows",
	"calculateImageRows",
	[{ widthPx: 400, heightPx: 300 }, 20, { widthPx: 9, heightPx: 18 }],
	calculateImageRows({ widthPx: 400, heightPx: 300 }, 20, { widthPx: 9, heightPx: 18 }),
);

function pngHeader(width, height) {
	const buf = Buffer.alloc(24);
	buf[0] = 0x89;
	buf[1] = 0x50;
	buf[2] = 0x4e;
	buf[3] = 0x47;
	buf.writeUInt32BE(width, 16);
	buf.writeUInt32BE(height, 20);
	return buf.toString("base64");
}
function jpegHeader(width, height) {
	const buf = Buffer.alloc(12);
	buf[0] = 0xff;
	buf[1] = 0xd8;
	buf[2] = 0xff;
	buf[3] = 0xc0;
	buf[4] = 0x08;
	buf.writeUInt16BE(height, 5);
	buf.writeUInt16BE(width, 7);
	return buf.toString("base64");
}
function gifHeader(width, height, sig = "GIF89a") {
	const buf = Buffer.alloc(10);
	buf.write(sig, 0, "ascii");
	buf.writeUInt16LE(width, 6);
	buf.writeUInt16LE(height, 8);
	return buf.toString("base64");
}
function webpVp8Header(width, height) {
	const buf = Buffer.alloc(30);
	buf.write("RIFF", 0, "ascii");
	buf.write("WEBP", 8, "ascii");
	buf.write("VP8 ", 12, "ascii");
	buf.writeUInt16LE(width & 0x3fff, 26);
	buf.writeUInt16LE(height & 0x3fff, 28);
	return buf.toString("base64");
}
function webpVp8lHeader(width, height) {
	const buf = Buffer.alloc(30);
	buf.write("RIFF", 0, "ascii");
	buf.write("WEBP", 8, "ascii");
	buf.write("VP8L", 12, "ascii");
	const w = width - 1;
	const h = height - 1;
	const bits = (w & 0x3fff) | ((h & 0x3fff) << 14);
	buf.writeUInt32LE(bits >>> 0, 21);
	return buf.toString("base64");
}
function webpVp8xHeader(width, height) {
	const buf = Buffer.alloc(30);
	buf.write("RIFF", 0, "ascii");
	buf.write("WEBP", 8, "ascii");
	buf.write("VP8X", 12, "ascii");
	const w = width - 1;
	const h = height - 1;
	buf[24] = w & 0xff;
	buf[25] = (w >> 8) & 0xff;
	buf[26] = (w >> 16) & 0xff;
	buf[27] = h & 0xff;
	buf[28] = (h >> 8) & 0xff;
	buf[29] = (h >> 16) & 0xff;
	return buf.toString("base64");
}

ti("png-valid", "getPngDimensions", [pngHeader(64, 32)], getPngDimensions(pngHeader(64, 32)));
ti("png-too-short", "getPngDimensions", [Buffer.alloc(10).toString("base64")], getPngDimensions(Buffer.alloc(10).toString("base64")));
ti("png-wrong-magic", "getPngDimensions", [Buffer.alloc(24).toString("base64")], getPngDimensions(Buffer.alloc(24).toString("base64")));
ti("jpeg-valid", "getJpegDimensions", [jpegHeader(800, 600)], getJpegDimensions(jpegHeader(800, 600)));
ti("jpeg-too-short", "getJpegDimensions", ["AA=="], getJpegDimensions("AA=="));
ti("jpeg-wrong-magic", "getJpegDimensions", [Buffer.alloc(12).toString("base64")], getJpegDimensions(Buffer.alloc(12).toString("base64")));
ti("gif-valid-89a", "getGifDimensions", [gifHeader(320, 240)], getGifDimensions(gifHeader(320, 240)));
ti("gif-valid-87a", "getGifDimensions", [gifHeader(16, 16, "GIF87a")], getGifDimensions(gifHeader(16, 16, "GIF87a")));
ti("gif-too-short", "getGifDimensions", [Buffer.alloc(4).toString("base64")], getGifDimensions(Buffer.alloc(4).toString("base64")));
ti("gif-wrong-signature", "getGifDimensions", [Buffer.alloc(10).toString("base64")], getGifDimensions(Buffer.alloc(10).toString("base64")));
ti("webp-vp8-valid", "getWebpDimensions", [webpVp8Header(100, 50)], getWebpDimensions(webpVp8Header(100, 50)));
ti("webp-vp8l-valid", "getWebpDimensions", [webpVp8lHeader(100, 50)], getWebpDimensions(webpVp8lHeader(100, 50)));
ti("webp-vp8x-valid", "getWebpDimensions", [webpVp8xHeader(100, 50)], getWebpDimensions(webpVp8xHeader(100, 50)));
ti("webp-too-short", "getWebpDimensions", [Buffer.alloc(10).toString("base64")], getWebpDimensions(Buffer.alloc(10).toString("base64")));
ti(
	"webp-wrong-riff",
	"getWebpDimensions",
	[Buffer.alloc(30).toString("base64")],
	getWebpDimensions(Buffer.alloc(30).toString("base64")),
);

ti("image-dimensions-png", "getImageDimensions", [pngHeader(10, 10), "image/png"], getImageDimensions(pngHeader(10, 10), "image/png"));
ti(
	"image-dimensions-jpeg",
	"getImageDimensions",
	[jpegHeader(10, 10), "image/jpeg"],
	getImageDimensions(jpegHeader(10, 10), "image/jpeg"),
);
ti("image-dimensions-gif", "getImageDimensions", [gifHeader(10, 10), "image/gif"], getImageDimensions(gifHeader(10, 10), "image/gif"));
ti(
	"image-dimensions-webp",
	"getImageDimensions",
	[webpVp8Header(10, 10), "image/webp"],
	getImageDimensions(webpVp8Header(10, 10), "image/webp"),
);
ti("image-dimensions-unknown-mime", "getImageDimensions", [pngHeader(10, 10), "image/bmp"], getImageDimensions(pngHeader(10, 10), "image/bmp"));

withImageEnv({}, () => {
	resetCapabilitiesCache();
	setCapabilities({ images: "kitty", trueColor: true, hyperlinks: true });
	terminalImageRecords.push({
		note: "render-image-kitty",
		fn: "renderImage",
		args: ["aGVsbG8=", { widthPx: 100, heightPx: 50 }, { maxWidthCells: 20 }],
		result: renderImage("aGVsbG8=", { widthPx: 100, heightPx: 50 }, { maxWidthCells: 20 }),
	});
	setCapabilities({ images: "iterm2", trueColor: true, hyperlinks: true });
	terminalImageRecords.push({
		note: "render-image-iterm2",
		fn: "renderImage",
		args: ["aGVsbG8=", { widthPx: 100, heightPx: 50 }, { maxWidthCells: 20 }],
		result: renderImage("aGVsbG8=", { widthPx: 100, heightPx: 50 }, { maxWidthCells: 20 }),
	});
	setCapabilities({ images: null, trueColor: true, hyperlinks: true });
	terminalImageRecords.push({
		note: "render-image-no-protocol",
		fn: "renderImage",
		args: ["aGVsbG8=", { widthPx: 100, heightPx: 50 }, { maxWidthCells: 20 }],
		result: renderImage("aGVsbG8=", { widthPx: 100, heightPx: 50 }, { maxWidthCells: 20 }),
	});
	resetCapabilitiesCache();
});

ti("hyperlink-basic", "hyperlink", ["click here", "http://example.test"], hyperlink("click here", "http://example.test"));
ti("image-fallback-full", "imageFallback", ["image/png", { widthPx: 10, heightPx: 20 }, "pic.png"], imageFallback("image/png", { widthPx: 10, heightPx: 20 }, "pic.png"));
ti("image-fallback-minimal", "imageFallback", ["image/png", undefined, undefined], imageFallback("image/png", undefined, undefined));

// ===========================================================================
// TERMINAL (feat-006 Wave 4 — pure helpers only; ProcessTerminal's live I/O
// has no oracle, see terminal.rs's module docs)
// ===========================================================================
const TERMINAL_OUT_FILE = join(OUT_DIR, "terminal.cases.jsonl");
const terminalRecords = [];
function tn(note, fn, args, result) {
	terminalRecords.push({ note, fn, args, result: result === undefined ? null : result });
}

tn("kitty-flags-nonzero", "parseKeyboardProtocolNegotiationSequence", ["\x1b[?7u"], parseKeyboardProtocolNegotiationSequence("\x1b[?7u"));
tn("kitty-flags-zero", "parseKeyboardProtocolNegotiationSequence", ["\x1b[?0u"], parseKeyboardProtocolNegotiationSequence("\x1b[?0u"));
tn(
	"device-attributes",
	"parseKeyboardProtocolNegotiationSequence",
	["\x1b[?1;2c"],
	parseKeyboardProtocolNegotiationSequence("\x1b[?1;2c"),
);
tn(
	"device-attributes-no-params",
	"parseKeyboardProtocolNegotiationSequence",
	["\x1b[?c"],
	parseKeyboardProtocolNegotiationSequence("\x1b[?c"),
);
tn("not-a-negotiation-sequence", "parseKeyboardProtocolNegotiationSequence", ["\x1b[31m"], parseKeyboardProtocolNegotiationSequence("\x1b[31m"));
tn(
	"apple-terminal-shift-enter",
	"normalizeAppleTerminalInput",
	["\r", true, true],
	normalizeAppleTerminalInput("\r", true, true),
);
tn(
	"apple-terminal-no-shift",
	"normalizeAppleTerminalInput",
	["\r", true, false],
	normalizeAppleTerminalInput("\r", true, false),
);
tn(
	"not-apple-terminal",
	"normalizeAppleTerminalInput",
	["\r", false, true],
	normalizeAppleTerminalInput("\r", false, true),
);
tn(
	"apple-terminal-different-key",
	"normalizeAppleTerminalInput",
	["a", true, true],
	normalizeAppleTerminalInput("a", true, true),
);

// ===========================================================================
// TUI (feat-006 Wave 4) — real Pi `TUI` driven against a fake `Terminal`.
// Modest coverage (see tui.rs's module docs on this wave's scope): first
// render, no-op re-render, single-line diff, append growth, width change,
// overlay show/focus/hide. Full exhaustive coverage of doRender's branch
// count is not attempted in this wave.
// ===========================================================================
const TUI_OUT_FILE = join(OUT_DIR, "tui.cases.jsonl");
const tuiRecords = [];

function makeFakeTerminal(columns, rows) {
	return {
		columns,
		rows,
		kittyProtocolActive: false,
		writes: [],
		start() {},
		stop() {},
		async drainInput() {},
		write(data) {
			this.writes.push(data);
		},
		moveBy() {},
		hideCursor() {},
		showCursor() {},
		clearLine() {},
		clearFromCursor() {},
		clearScreen() {},
		setTitle() {},
		setProgress() {},
	};
}

function makeFakeComponent(id, linesFn) {
	return {
		id,
		render(width) {
			return linesFn(width);
		},
		invalidate() {},
	};
}

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

async function tuiCase(note, columns, rows, script) {
	const terminal = makeFakeTerminal(columns, rows);
	const tui = new TUI(terminal, false);
	const events = [];
	await script(tui, terminal, events);
	tuiRecords.push({ note, columns, rows, writes: terminal.writes, events });
}

await tuiCase("first-render-then-noop", 20, 5, async (tui, terminal) => {
	tui.addChild(makeFakeComponent("a", () => ["hello", "world"]));
	tui.requestRender(true);
	terminal.writes.length = 0;
	tui.requestRender(false);
	await sleep(20);
	// second requestRender with no content change -> no writes expected beyond
	// what the throttled render loop already emitted before this snapshot.
});

await tuiCase("single-line-diff", 20, 5, async (tui, terminal) => {
	let line2 = "world";
	tui.addChild(makeFakeComponent("a", () => ["hello", line2]));
	tui.requestRender(true);
	await sleep(20);
	terminal.writes.length = 0;
	line2 = "WORLD!";
	tui.requestRender(false);
	await sleep(20);
});

await tuiCase("append-growth", 20, 5, async (tui, terminal) => {
	let extra = [];
	tui.addChild(makeFakeComponent("a", () => ["hello", ...extra]));
	tui.requestRender(true);
	await sleep(20);
	terminal.writes.length = 0;
	extra = ["more"];
	tui.requestRender(false);
	await sleep(20);
});

await tuiCase("width-change-forces-full-redraw", 20, 5, async (tui, terminal) => {
	tui.addChild(makeFakeComponent("a", () => ["hello", "world"]));
	tui.requestRender(true);
	await sleep(20);
	terminal.writes.length = 0;
	terminal.columns = 30;
	tui.requestRender(false);
	await sleep(20);
});

await tuiCase("overlay-show-focus-hide-restores-prior-focus", 20, 10, async (tui, terminal, events) => {
	const base = makeFakeComponent("base", () => ["base-line"]);
	base.focused = false;
	tui.addChild(base);
	tui.setFocus(base);
	tui.requestRender(true);
	await sleep(20);
	terminal.writes.length = 0;

	const overlayComponent = makeFakeComponent("overlay", () => ["overlay-line"]);
	overlayComponent.focused = false;
	const handle = tui.showOverlay(overlayComponent, {});
	events.push({ afterShow_focusedIsOverlay: tui.focusedComponent === overlayComponent, hasOverlay: tui.hasOverlay() });
	await sleep(20);

	handle.hide();
	events.push({ afterHide_focusedIsBase: tui.focusedComponent === base, hasOverlay: tui.hasOverlay() });
	await sleep(20);
});

await tuiCase("two-overlays-hide-non-topmost-does-not-move-focus", 20, 10, async (tui, terminal, events) => {
	const overlayA = makeFakeComponent("overlayA", () => ["a"]);
	const overlayB = makeFakeComponent("overlayB", () => ["b"]);
	tui.requestRender(true);
	await sleep(20);
	const handleA = tui.showOverlay(overlayA, {});
	await sleep(20);
	const handleB = tui.showOverlay(overlayB, {});
	await sleep(20);
	events.push({ focusedIsB: tui.focusedComponent === overlayB });
	handleA.hide();
	await sleep(20);
	events.push({ stillFocusedIsB: tui.focusedComponent === overlayB, hasOverlay: tui.hasOverlay() });
	handleB.hide();
});

await tuiCase("cursor-marker-extracted-and-stripped", 20, 5, async (tui, terminal) => {
	const CURSOR_MARKER = "\x1b_pi:c\x07";
	const editorLike = makeFakeComponent("editor", () => [`abc${CURSOR_MARKER}def`]);
	editorLike.focused = true;
	tui.addChild(editorLike);
	tui.setFocus(editorLike);
	tui.requestRender(true);
	await sleep(20);
});

// ===========================================================================
// TEXT (feat-006 Wave 5)
// ===========================================================================
const TEXT_OUT_FILE = join(OUT_DIR, "text.cases.jsonl");
const textRecords = [];
function tx(note, text, paddingX, paddingY, width) {
	const t = new Text(text, paddingX, paddingY);
	textRecords.push({ note, text, paddingX, paddingY, width, result: t.render(width) });
}
tx("empty-text-renders-nothing", "", 1, 1, 20);
tx("whitespace-only-renders-nothing", "   ", 1, 1, 20);
tx("simple-text-default-padding", "hi", 1, 1, 20);
tx("no-horizontal-padding", "hello world", 0, 0, 20);
tx("wraps-long-text", "the quick brown fox jumps over the lazy dog", 1, 0, 15);
tx("tabs-become-three-spaces", "a\tb", 0, 0, 10);
tx("vertical-padding-two", "x", 0, 2, 10);
tx("multiline-input-preserved", "line1\nline2", 0, 0, 10);

// ===========================================================================
// TRUNCATED TEXT (feat-006 Wave 5)
// ===========================================================================
const TRUNCATED_TEXT_OUT_FILE = join(OUT_DIR, "truncated-text.cases.jsonl");
const truncatedTextRecords = [];
function tt(note, text, paddingX, paddingY, width) {
	const t = new TruncatedText(text, paddingX, paddingY);
	truncatedTextRecords.push({ note, text, paddingX, paddingY, width, result: t.render(width) });
}
tt("short-text-padded-to-width", "hi", 0, 0, 10);
tt("stops-at-first-newline", "first\nsecond", 0, 0, 20);
tt("truncates-with-ellipsis", "this is a very long string indeed", 0, 0, 15);
tt("horizontal-padding", "hi", 2, 0, 10);
tt("vertical-padding", "hi", 0, 2, 10);
tt("empty-text", "", 0, 0, 10);

// ===========================================================================
// SPACER (feat-006 Wave 5)
// ===========================================================================
const SPACER_OUT_FILE = join(OUT_DIR, "spacer.cases.jsonl");
const spacerRecords = [];
function sp(note, lines, width) {
	const s = new Spacer(lines);
	spacerRecords.push({ note, lines, width, result: s.render(width) });
}
sp("default-one-line", 1, 20);
sp("three-lines", 3, 20);
sp("zero-lines", 0, 20);

// ===========================================================================
// SELECT LIST (feat-006 Wave 5)
// ===========================================================================
const SELECT_LIST_OUT_FILE = join(OUT_DIR, "select-list.cases.jsonl");
const selectListRecords = [];
const identityTheme = () => ({
	selectedPrefix: (s) => s,
	selectedText: (s) => s,
	description: (s) => s,
	scrollInfo: (s) => s,
	noMatch: (s) => s,
});
function sl(note, items, maxVisible, layout, ops) {
	const list = new SelectList(items, maxVisible, identityTheme(), layout ?? {});
	const events = [];
	for (const op of ops) {
		if (op.op === "render") {
			events.push({ render: list.render(op.width) });
		} else if (op.op === "handleInput") {
			list.handleInput(op.data);
			events.push({ selectedIndex: list.selectedIndex });
		} else if (op.op === "setFilter") {
			list.setFilter(op.filter);
			events.push({ filteredLength: list.filteredItems.length, selectedIndex: list.selectedIndex });
		}
	}
	selectListRecords.push({ note, items, maxVisible, ops, events });
}
const items3 = [
	{ value: "alpha", label: "Alpha", description: undefined },
	{ value: "beta", label: "Beta", description: undefined },
	{ value: "gamma", label: "Gamma", description: undefined },
];
sl("empty-list-shows-no-match", [], 5, undefined, [{ op: "render", width: 40 }]);
sl("basic-render-and-down", items3, 5, undefined, [
	{ op: "render", width: 40 },
	{ op: "handleInput", data: "\x1b[B" },
	{ op: "render", width: 40 },
]);
sl("up-wraps-to-bottom", items3, 5, undefined, [{ op: "handleInput", data: "\x1b[A" }]);
sl("down-wraps-to-top-from-last", items3, 5, undefined, [
	{ op: "handleInput", data: "\x1b[B" },
	{ op: "handleInput", data: "\x1b[B" },
	{ op: "handleInput", data: "\x1b[B" },
]);
sl("set-filter-narrows-list", items3, 5, undefined, [
	{ op: "setFilter", filter: "al" },
	{ op: "render", width: 40 },
]);
sl("with-description-wide-terminal", [{ value: "a", label: "Alpha", description: "A test description here" }], 5, undefined, [
	{ op: "render", width: 60 },
]);
sl("scroll-indicator-with-many-items", Array.from({ length: 10 }, (_, i) => ({ value: `i${i}`, label: `Item ${i}` })), 3, undefined, [
	{ op: "render", width: 40 },
]);

// ===========================================================================
// INPUT (feat-006 Wave 5)
// ===========================================================================
const INPUT_OUT_FILE = join(OUT_DIR, "input.cases.jsonl");
const inputRecords = [];
function ip(note, ops, width) {
	const input = new Input();
	const events = [];
	for (const op of ops) {
		if (op.op === "handleInput") {
			input.handleInput(op.data);
		} else if (op.op === "setValue") {
			input.setValue(op.value);
		}
	}
	events.push({ value: input.getValue(), render: input.render(width ?? 20) });
	inputRecords.push({ note, ops, width: width ?? 20, events });
}
ip("typing_appends", [{ op: "handleInput", data: "h" }, { op: "handleInput", data: "i" }]);
ip("backspace_removes_last_char", [
	{ op: "setValue", value: "hi" },
	{ op: "handleInput", data: "\x7f" },
]);
ip("ctrl_a_moves_to_line_start_then_k_kills_to_end", [
	{ op: "setValue", value: "hello world" },
	{ op: "handleInput", data: "\x01" }, // ctrl+a
	{ op: "handleInput", data: "\x0b" }, // ctrl+k
]);
ip("ctrl_u_kills_to_line_start", [
	{ op: "setValue", value: "hello world" },
	{ op: "handleInput", data: "\x01" },
	{ op: "handleInput", data: "\x05" }, // ctrl+e (end)
	{ op: "handleInput", data: "\x15" }, // ctrl+u
]);
ip("undo_after_typing", [
	{ op: "handleInput", data: "a" },
	{ op: "handleInput", data: "b" },
	{ op: "handleInput", data: "\x1f" }, // ctrl+-
]);
ip("astral_plane_backspace", [
	{ op: "setValue", value: "a😀b" },
	{ op: "handleInput", data: "\x7f" },
	{ op: "handleInput", data: "\x7f" },
]);
ip("bracketed_paste_strips_newlines", [
	{ op: "handleInput", data: "\x1b[200~line1\nline2\t\x1b[201~" },
]);
ip("render_scrolls_when_value_exceeds_width", [{ op: "setValue", value: "this is a very long input value that overflows" }], 20);
ip("render_shows_hardware_cursor_marker_when_focused", [{ op: "setValue", value: "hi" }]);

// ===========================================================================
// BOX (feat-006 Wave 5)
// ===========================================================================
const BOX_OUT_FILE = join(OUT_DIR, "box.cases.jsonl");
const boxRecords = [];
function fixedChild(lines) {
	return { render: () => lines, invalidate: () => {} };
}
function bx(note, paddingX, paddingY, useBg, children, width) {
	const box = new PiBox(paddingX, paddingY, useBg ? bgFn : undefined);
	for (const c of children) box.addChild(fixedChild(c));
	boxRecords.push({ note, paddingX, paddingY, useBg, children, width, result: box.render(width) });
}
bx("no-children-renders-nothing", 1, 1, false, [], 10);
bx("single-child-default-padding", 1, 1, false, [["hi"]], 10);
bx("two-children-no-padding", 0, 0, false, [["a"], ["b"]], 10);
bx("with-background-fn", 1, 1, true, [["hi"]], 10);
bx("vertical-padding-two", 0, 2, false, [["x"]], 6);

// ===========================================================================
// IMAGE (feat-006 Wave 5)
// ===========================================================================
const IMAGE_OUT_FILE = join(OUT_DIR, "image.cases.jsonl");
const imageRecords = [];
function img(note, capsImages, base64Data, mimeType, dims, options, width) {
	resetCapabilitiesCache();
	setCapabilities({ images: capsImages, trueColor: true, hyperlinks: true });
	setCellDimensions({ widthPx: 9, heightPx: 18 });
	const theme = { fallbackColor: (s) => `<fb>${s}</fb>` };
	const image = new PiImage(base64Data, mimeType, theme, options ?? {}, dims);
	const result = image.render(width);
	imageRecords.push({ note, capsImages, base64Data, mimeType, dims, options: options ?? {}, width, result });
	resetCapabilitiesCache();
}
// `imageId` is always passed explicitly below (never left to the real
// `allocateImageId()`'s `Math.random()`) so every case is byte-deterministic
// across regenerations, matching this oracle's `--check` idempotency
// contract; `allocateImageId`'s own randomness is exercised by a plain unit
// test in `terminal_image.rs` (Wave 4), not here.
img("no-capability-renders-fallback", null, "", "image/png", { widthPx: 100, heightPx: 50 }, { filename: "cat.png" }, 40);
img("kitty-capability-renders-sequence", "kitty", pngHeader(90, 180), "image/png", undefined, { maxWidthCells: 10, imageId: 7 }, 40);
img("iterm2-capability-renders-sequence", "iterm2", pngHeader(90, 180), "image/png", undefined, { maxWidthCells: 10 }, 40);
img(
	"kitty-with-explicit-image-id-reuses-id",
	"kitty",
	pngHeader(90, 180),
	"image/png",
	undefined,
	{ maxWidthCells: 10, imageId: 42 },
	40,
);

// ===========================================================================
// SETTINGS LIST (feat-006 Wave 5)
// ===========================================================================
const SETTINGS_LIST_OUT_FILE = join(OUT_DIR, "settings-list.cases.jsonl");
const settingsListRecords = [];
const settingsIdentityTheme = () => ({
	label: (s) => s,
	value: (s) => s,
	description: (s) => s,
	cursor: "> ",
	hint: (s) => s,
});
function sanitizeSettingItem(item) {
	// Drop the `submenu` function (not JSON-serializable) — its presence/
	// absence is what the fixture cares about, recorded separately below.
	const { submenu, ...rest } = item;
	return { ...rest, hasSubmenu: submenu !== undefined };
}
function stl(note, itemsInput, maxVisible, enableSearch, ops, width) {
	// Snapshot the ORIGINAL config before construction — `SettingsList` keeps
	// live references to these objects and mutates `currentValue` in place
	// (e.g. `activateItem`'s value-cycling), so recording `itemsInput` after
	// running `ops` would silently capture post-mutation state as if it were
	// the fixture's initial input. Caught by inspecting a generated fixture
	// by hand before wiring the Rust side to it.
	const itemsSnapshot = itemsInput.map(sanitizeSettingItem);
	const items = itemsInput.map((i) => ({ ...i }));
	const changes = [];
	let cancelled = false;
	const list = new SettingsList(
		items,
		maxVisible,
		settingsIdentityTheme(),
		(id, newValue) => changes.push({ id, newValue }),
		() => {
			cancelled = true;
		},
		{ enableSearch },
	);
	const events = [];
	for (const op of ops) {
		if (op.op === "render") {
			events.push({ render: list.render(op.width ?? width) });
		} else if (op.op === "handleInput") {
			list.handleInput(op.data);
			events.push({ afterInput: op.data, cancelled, changes: [...changes] });
		}
	}
	settingsListRecords.push({
		note,
		items: itemsSnapshot,
		maxVisible,
		enableSearch,
		ops,
		width,
		events,
	});
}
const cycleItem = { id: "a", label: "A", currentValue: "x", values: ["x", "y", "z"] };
const descItem = { id: "b", label: "B", currentValue: "1", description: "A helpful description" };
stl("empty-list-shows-hint", [], 5, false, [{ op: "render" }], 40);
stl("cycles-value-on-confirm", [cycleItem], 5, false, [
	{ op: "handleInput", data: " " },
	{ op: "render" },
], 40);
stl("down-then-up-wraps", [cycleItem, descItem], 5, false, [
	{ op: "handleInput", data: "\x1b[A" }, // up from index 0 wraps to last
	{ op: "render" },
], 40);
stl("cancel-fires-callback", [cycleItem], 5, false, [{ op: "handleInput", data: "\x1b" }], 40);
stl("search-filters-then-confirm", [{ id: "a", label: "Alpha", currentValue: "1" }, { id: "b", label: "Beta", currentValue: "2" }], 5, true, [
	{ op: "handleInput", data: "Beta" },
	{ op: "render" },
], 40);
stl("shows-description-for-selected-item", [descItem], 5, false, [{ op: "render", width: 60 }], 40);

// ---------------------------------------------------------------------------
// EDITOR (feat-006 Wave 6) — real Pi `Editor` driven against a fake `TUI`.
// The editor needs `tui.terminal.rows` and `tui.requestRender()`; we reuse the
// fake terminal + real `TuiMainScreen` from the tui section.
// ---------------------------------------------------------------------------
const EDITOR_OUT_FILE = join(OUT_DIR, "editor.cases.jsonl");
const editorRecords = [];

// editorCase: build a fresh Editor over a fresh fake TUI, run `ops`, then
// snapshot render + getText + getCursor.
async function editorCase(note, ops, width = 40, rows = 24) {
	const terminal = makeFakeTerminal(width, rows);
	const tui = new TUI(terminal, false);
	const editor = new Editor(
		tui,
		{ borderColor: (s) => s, selectList: { primary: (s) => s, secondary: (s) => s, selected: (s) => s } },
		{},
	);
	const events = [];
	editor.onChange = (text) => events.push({ change: text });
	editor.onSubmit = (text) => events.push({ submit: text });
	for (const op of ops) {
		if (op.op === "handleInput") editor.handleInput(op.data);
		else if (op.op === "setText") editor.setText(op.value);
		else if (op.op === "addToHistory") editor.addToHistory(op.value);
		else if (op.op === "setPaddingX") editor.setPaddingX(op.value);
	}
	editorRecords.push({
		note,
		ops,
		width,
		rows,
		render: editor.render(width),
		text: editor.getText(),
		cursor: editor.getCursor(),
		events,
	});
}

// Core typing / editing
await editorCase("typing_appends", [{ op: "handleInput", data: "h" }, { op: "handleInput", data: "i" }]);
await editorCase("multiline_typing", [
	{ op: "handleInput", data: "a" },
	{ op: "handleInput", data: "\r" },
	{ op: "handleInput", data: "b" },
]);
await editorCase("backspace_removes_grapheme", [
	{ op: "setText", value: "a😀b" },
	{ op: "handleInput", data: "\x7f" },
]);
await editorCase("backspace_merges_lines", [
	{ op: "setText", value: "ab\ncd" },
	{ op: "handleInput", data: "\x01" }, // ctrl+a -> start
	{ op: "handleInput", data: "\x1b[B" }, // down
	{ op: "handleInput", data: "\x7f" }, // backspace (merge)
]);
await editorCase("ctrl_a_e_line_edges", [
	{ op: "setText", value: "hello world" },
	{ op: "handleInput", data: "\x01" }, // ctrl+a
	{ op: "handleInput", data: "\x05" }, // ctrl+e
]);
await editorCase("ctrl_u_kills_to_line_start", [
	{ op: "setText", value: "hello world" },
	{ op: "handleInput", data: "\x01" },
	{ op: "handleInput", data: "\x15" }, // ctrl+u
]);
await editorCase("ctrl_k_kills_to_line_end", [
	{ op: "setText", value: "hello world" },
	{ op: "handleInput", data: "\x05" }, // ctrl+e
	{ op: "handleInput", data: "\x0b" }, // ctrl+k
]);
await editorCase("undo_after_typing", [
	{ op: "handleInput", data: "a" },
	{ op: "handleInput", data: "b" },
	{ op: "handleInput", data: "\x1f" }, // ctrl+-
]);
await editorCase("undo_after_delete", [
	{ op: "setText", value: "hello" },
	{ op: "handleInput", data: "\x7f" },
	{ op: "handleInput", data: "\x1f" }, // ctrl+-
]);
await editorCase("word_navigation", [
	{ op: "setText", value: "hello world foo" },
	{ op: "handleInput", data: "\x1bb" }, // alt+b
	{ op: "handleInput", data: "\x1bf" }, // alt+f
]);
await editorCase("delete_word_backward", [
	{ op: "setText", value: "hello world foo" },
	{ op: "handleInput", data: "\x17" }, // ctrl+w
]);
await editorCase("delete_word_forward", [
	{ op: "setText", value: "hello world foo" },
	{ op: "handleInput", data: "\x01" }, // ctrl+a (start)
	{ op: "handleInput", data: "\x1bd" }, // alt+d
]);
await editorCase("yank_after_kill", [
	{ op: "setText", value: "hello world" },
	{ op: "handleInput", data: "\x01" },
	{ op: "handleInput", data: "\x0b" }, // ctrl+k
	{ op: "handleInput", data: "\x19" }, // ctrl+y
]);
await editorCase("submit_clears_and_fires", [
	{ op: "setText", value: "hello" },
	{ op: "handleInput", data: "\r" },
]);
await editorCase("history_navigation", [
	{ op: "addToHistory", value: "first prompt" },
	{ op: "addToHistory", value: "second prompt" },
	{ op: "handleInput", data: "\x1b[A" }, // up
	{ op: "handleInput", data: "\x1b[A" }, // up
]);
await editorCase("wrapping_long_line", [
	{ op: "setText", value: "this is a very long line that should wrap across multiple visual lines for sure" },
], 30, 24);
await editorCase("cursor_moves_across_wrapped_lines", [
	{ op: "setText", value: "this is a very long line that should wrap across multiple visual lines for sure" },
	{ op: "handleInput", data: "\x1b[B" }, // down
], 30, 24);
await editorCase("paste_small_single_line", [
	{ op: "handleInput", data: "\x1b[200~hello world\x1b[201~" },
]);
await editorCase("paste_multiline", [
	{ op: "handleInput", data: "\x1b[200~line1\nline2\nline3\x1b[201~" },
]);
await editorCase("large_paste_creates_marker", [
	{ op: "handleInput", data: "\x1b[200~" + "x".repeat(1100) + "\x1b[201~" },
]);
await editorCase("large_paste_marker_expands_on_submit", [
	{ op: "handleInput", data: "\x1b[200~" + "y".repeat(1100) + "\x1b[201~" },
	{ op: "handleInput", data: "\r" },
]);
await editorCase("jump_mode_forward", [
	{ op: "setText", value: "abc abc abc" },
	{ op: "handleInput", data: "\x1d]" }, // ctrl+]
	{ op: "handleInput", data: "b" },
]);
await editorCase("jump_mode_backward", [
	{ op: "setText", value: "abc abc abc" },
	{ op: "handleInput", data: "\x05" }, // ctrl+e (end)
	{ op: "handleInput", data: "\x1d\x1d]" }, // ctrl+alt+] -> backward jump
	{ op: "handleInput", data: "a" },
]);
await editorCase("render_scroll_indicator", [
	{ op: "setText", value: "line1\nline2\nline3\nline4\nline5\nline6\nline7\nline8" },
], 40, 8);
await editorCase("padding_x_affects_layout", [
	{ op: "setPaddingX", value: 3 },
	{ op: "setText", value: "hi" },
], 20, 10);


// ---------------------------------------------------------------------------
// Markdown — drive the real Pi `Markdown` component with a deterministic
// fake theme (fixed ANSI codes, not chalk) and capture render() output.
// ---------------------------------------------------------------------------
const MARKDOWN_OUT_FILE = join(OUT_DIR, "markdown.cases.jsonl");
const markdownRecords = [];

// Deterministic fake theme: each fn wraps text in a distinct SGR sequence so
// the oracle is byte-stable across machines (chalk emits terminal-dependent
// codes).
const mdTheme = {
	heading: (t) => `\x1b[1;36m${t}\x1b[0m`,
	link: (t) => `\x1b[34m${t}\x1b[0m`,
	linkUrl: (t) => `\x1b[2m${t}\x1b[0m`,
	code: (t) => `\x1b[33m${t}\x1b[0m`,
	codeBlock: (t) => `\x1b[32m${t}\x1b[0m`,
	codeBlockBorder: (t) => `\x1b[2m${t}\x1b[0m`,
	quote: (t) => `\x1b[3m${t}\x1b[0m`,
	quoteBorder: (t) => `\x1b[2m${t}\x1b[0m`,
	hr: (t) => `\x1b[2m${t}\x1b[0m`,
	listBullet: (t) => `\x1b[36m${t}\x1b[0m`,
	bold: (t) => `\x1b[1m${t}\x1b[0m`,
	italic: (t) => `\x1b[3m${t}\x1b[0m`,
	strikethrough: (t) => `\x1b[9m${t}\x1b[0m`,
	underline: (t) => `\x1b[4m${t}\x1b[0m`,
};

// markdownCase: render `text` at `width` and record the exact lines.
function markdownCase(note, text, width = 80, options) {
	const md = new Markdown(text, 0, 0, mdTheme, undefined, options);
	markdownRecords.push({
		note,
		text,
		width,
		options: options ?? null,
		render: md.render(width),
	});
}

markdownCase("heading_and_paragraph", "# Title\n\nSome paragraph text here.\n");
markdownCase("bold_italic_code", "**bold** and *italic* and `code` and ~~strike~~\n");
markdownCase("links_no_hyperlink", "[link text](https://example.com)\n");
markdownCase("simple_list", "- one\n- two\n- three\n");
markdownCase("nested_list", "- one\n  - a\n  - b\n- two\n");
markdownCase("ordered_list", "1. first\n2. second\n3. third\n");
markdownCase("code_block", "```js\nconst x = 1;\nconsole.log(x);\n```\n");
markdownCase("blockquote", "> quoted line\n> second line\n");
markdownCase("hr", "---\n");
markdownCase("table", "| a | b |\n|---| --- |\n| 1 | 2 |\n");
markdownCase("strikethrough", "~~gone~~ stays\n");
markdownCase("autolink_email", "<foo@bar.com>\n");
markdownCase("image_line_skip", "![alt](img.png)\n");
markdownCase("latex_inline", "The value is $x^2 + 1$.\n");
markdownCase("wrapped_long", "This is a very long paragraph that should wrap across multiple lines at width 30. It has several words and keeps going.\n", 30);
markdownCase("padding", "hello\n", 20);
markdownCase("html_tag", "<div>plain</div>\n");
markdownCase("task_list", "- [ ] todo\n- [x] done\n");
markdownCase("trailing_space_paragraph", "para with trailing space  \nnext\n");
markdownCase("nested_blockquote", "> outer\n> > inner\n");
markdownCase("list_with_code", "- item\n  ```\n  code\n  ```\n");


markdownCase("heading_levels", "## Sub\n### Subsub\n");
markdownCase("table_align", "| L | C | R |\n|:--|:-:|--:|\n| 1 | 2 | 3 |\n");
markdownCase("table_varying", "| short | longcellcontent |\n|-------|-----------------|\n| a | b |\n", 30);
markdownCase("table_narrow", "| a | b |\n|---| --- |\n| 1 | 2 |\n", 10);
markdownCase("br_and_escape", "line1\\nline2 and \\*not emph*\n");
markdownCase("lazy_blockquote", "> lazy\ncontinuation\n");
markdownCase("nested_list_ordered", "1. one\n   1. a\n   2. b\n2. two\n");
markdownCase("loose_list", "- a\n\n- b\n");
markdownCase("start_numbered_list", "3. three\n4. four\n");
markdownCase("list_paragraph_code", "- first\n\n  para\n- second\n");
markdownCase("multiline_code", "\`\`\`python\ndef f():\n    return 1\n\`\`\`\n");
markdownCase("latex_block_display", "$$\\sum_{i=1}^{n} i$$\n");
markdownCase("latex_pending_dollar", "The cost is $5 and $10 total.\n");
markdownCase("heading_no_space_after", "# H1\n");
markdownCase("code_without_lang", "\`\`\`\nplain code\n\`\`\`\n");
markdownCase("escape_asterisk", "not \\*emph* here\n");
markdownCase("autolink_url", "<https://example.com>\n");

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------
mkdirSync(OUT_DIR, { recursive: true });

function writeFixture(outFile, records, label) {
	const contents = records.map((r) => JSON.stringify(r)).join("\n") + "\n";
	const existing = existsSync(outFile) ? readFileSync(outFile, "utf-8") : null;
	if (existing === contents) {
		console.log(`ok    ${label} (${records.length} cases, ${Buffer.byteLength(contents)} bytes)`);
		return 0;
	}
	if (CHECK) {
		console.error(`DRIFT ${label}: ${existing === null ? "missing" : `${existing.length} -> ${contents.length} bytes`}`);
		return 1;
	}
	writeFileSync(outFile, contents, "utf-8");
	console.log(`wrote ${label} (${records.length} cases, ${Buffer.byteLength(contents)} bytes)`);
	return 0;
}

let drift = 0;
drift += writeFixture(OUT_FILE, records, "utils.cases.jsonl");
drift += writeFixture(KEYS_OUT_FILE, keysRecords, "keys.cases.jsonl");
drift += writeFixture(STDIN_OUT_FILE, stdinRecords, "stdin-buffer.cases.jsonl");
drift += writeFixture(WORD_NAV_OUT_FILE, wordNavRecords, "word-navigation.cases.jsonl");
drift += writeFixture(KEYBINDINGS_OUT_FILE, keybindingsRecords, "keybindings.cases.jsonl");
drift += writeFixture(FUZZY_OUT_FILE, fuzzyRecords, "fuzzy.cases.jsonl");
drift += writeFixture(TERMINAL_COLORS_OUT_FILE, terminalColorsRecords, "terminal-colors.cases.jsonl");
drift += writeFixture(TERMINAL_IMAGE_OUT_FILE, terminalImageRecords, "terminal-image.cases.jsonl");
drift += writeFixture(TERMINAL_OUT_FILE, terminalRecords, "terminal.cases.jsonl");
drift += writeFixture(TUI_OUT_FILE, tuiRecords, "tui.cases.jsonl");
drift += writeFixture(TEXT_OUT_FILE, textRecords, "text.cases.jsonl");
drift += writeFixture(TRUNCATED_TEXT_OUT_FILE, truncatedTextRecords, "truncated-text.cases.jsonl");
drift += writeFixture(SPACER_OUT_FILE, spacerRecords, "spacer.cases.jsonl");
drift += writeFixture(SELECT_LIST_OUT_FILE, selectListRecords, "select-list.cases.jsonl");
drift += writeFixture(INPUT_OUT_FILE, inputRecords, "input.cases.jsonl");
drift += writeFixture(BOX_OUT_FILE, boxRecords, "box.cases.jsonl");
drift += writeFixture(IMAGE_OUT_FILE, imageRecords, "image.cases.jsonl");
drift += writeFixture(SETTINGS_LIST_OUT_FILE, settingsListRecords, "settings-list.cases.jsonl");
drift += writeFixture(EDITOR_OUT_FILE, editorRecords, "editor.cases.jsonl");
drift += writeFixture(MARKDOWN_OUT_FILE, markdownRecords, "markdown.cases.jsonl");

if (CHECK && drift > 0) {
	console.error("\nDRIFT: tui fixture(s) stale; run: node scripts/gen-tui-oracle.mjs");
	process.exitCode = 1;
}
