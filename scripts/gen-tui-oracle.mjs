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

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------
mkdirSync(OUT_DIR, { recursive: true });
const contents = records.map((r) => JSON.stringify(r)).join("\n") + "\n";
const existing = existsSync(OUT_FILE) ? readFileSync(OUT_FILE, "utf-8") : null;

if (existing === contents) {
	console.log(`ok    utils.cases.jsonl (${records.length} cases, ${Buffer.byteLength(contents)} bytes)`);
} else if (CHECK) {
	console.error(`DRIFT utils.cases.jsonl: ${existing === null ? "missing" : `${existing.length} -> ${contents.length} bytes`}`);
	console.error("\nDRIFT: utils fixture is stale; run: node scripts/gen-tui-oracle.mjs");
	process.exitCode = 1;
} else {
	writeFileSync(OUT_FILE, contents, "utf-8");
	console.log(`wrote utils.cases.jsonl (${records.length} cases, ${Buffer.byteLength(contents)} bytes)`);
}
