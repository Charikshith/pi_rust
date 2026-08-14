//! Pi-as-oracle test for `pirust_tools::truncate`.
//!
//! Drives every case in `tests/fixtures/pi/tools/truncate.cases.jsonl` — 71 records
//! produced by executing Pi's real `core/tools/truncate.ts` (see
//! `scripts/gen-tools-oracle.mjs` §B). Each record is
//! `{fn, note, input, options, result}`; nothing here is computed by hand, and a
//! failure means the port diverged from Pi, not that the expectation is stale.
//!
//! For head/tail cases the whole [`TruncationResult`] is compared *and* re-serialized
//! and matched against Pi's `JSON.stringify` bytes (key order, `"truncatedBy":null`,
//! `Number.MAX_SAFE_INTEGER` limits), because the value is persisted in session JSONL.
//!
//! `truncateLine` needs care: fixture case 50 records a **lone high surrogate**
//! (`"👍\ud83d... [truncated]"`), which no Rust `String` can hold, so the exact
//! assertion is made in UTF-16 space against the literal fixture escapes via
//! [`truncate_line_utf16`]. The `String` API is additionally checked against the same
//! expectation with unpaired surrogates mapped to U+FFFD — the divergence documented on
//! `truncate_line`.

use pirust_tools::truncate::{
    format_size, truncate_head, truncate_line, truncate_line_utf16, truncate_tail,
    TruncationOptions, TruncationResult, DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES,
    GREP_MAX_LINE_LENGTH,
};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Every record in the oracle fixture must be exercised; a shrinking file must fail
/// loudly instead of silently weakening the suite.
const EXPECTED_CASES: usize = 71;

/// Per-`fn` record counts as found in the fixture (1 + 29 + 15 + 11 + 15 = 71).
const EXPECTED_BREAKDOWN: [(&str, usize); 5] = [
    ("constants", 1),
    ("truncateHead", 29),
    ("truncateTail", 15),
    ("truncateLine", 11),
    ("formatSize", 15),
];

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/pi/tools/truncate.cases.jsonl")
}

/// Parse a 4-hex-digit `\uXXXX` payload.
fn hex4(bytes: &[u8]) -> Option<u16> {
    let text = std::str::from_utf8(bytes).ok()?;
    u16::from_str_radix(text, 16).ok()
}

/// Rewrite every *unpaired*-surrogate `\uXXXX` escape to the escape for U+FFFD.
///
/// `serde_json` rightly refuses to build a `String` from a lone surrogate, and fixture
/// case 50 contains one because Pi genuinely emits one. Applying exactly the
/// substitution that `truncate_line` documents keeps the expectation Pi's own; the
/// unmodified bytes are still used for the exact UTF-16 comparison, so this rewrite can
/// never hide a real difference.
fn map_lone_surrogates_to_replacement(line: &str) -> String {
    let bytes = line.as_bytes();
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            if bytes[i + 1] == b'u' && i + 6 <= bytes.len() {
                let unit = hex4(&bytes[i + 2..i + 6]);
                // A high surrogate is legitimate only when a low surrogate follows.
                if let Some(u) = unit {
                    if (0xD800..0xDC00).contains(&u) {
                        let low = if i + 12 <= bytes.len()
                            && bytes[i + 6] == b'\\'
                            && bytes[i + 7] == b'u'
                        {
                            hex4(&bytes[i + 8..i + 12])
                        } else {
                            None
                        };
                        if low.is_some_and(|l| (0xDC00..0xE000).contains(&l)) {
                            out.push_str(&line[i..i + 12]);
                            i += 12;
                        } else {
                            out.push_str("\\ufffd");
                            i += 6;
                        }
                        continue;
                    }
                    if (0xDC00..0xE000).contains(&u) {
                        out.push_str("\\ufffd");
                        i += 6;
                        continue;
                    }
                }
                out.push_str(&line[i..i + 6]);
                i += 6;
                continue;
            }
            // Any other escape (`\n`, `\"`, `\\`, ...) is copied verbatim.
            out.push_str(&line[i..i + 2]);
            i += 2;
            continue;
        }
        let ch = line[i..]
            .chars()
            .next()
            .expect("index is on a char boundary");
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Decode the literal `result.text` of a `truncateLine` record into UTF-16 code units,
/// straight from the fixture bytes — no `String` in between, so Pi's lone surrogate
/// survives and the comparison against [`truncate_line_utf16`] is exact.
fn expected_text_utf16(line: &str) -> Vec<u16> {
    const PREFIX: &str = "\"result\":{\"text\":\"";
    let start = line
        .find(PREFIX)
        .unwrap_or_else(|| panic!("truncateLine record must start its result with `text`: {line}"))
        + PREFIX.len();
    let bytes = line.as_bytes();
    let mut i = start;
    let mut units: Vec<u16> = Vec::new();
    loop {
        assert!(
            i < bytes.len(),
            "unterminated JSON string in fixture: {line}"
        );
        match bytes[i] {
            b'"' => return units,
            b'\\' => {
                let escape = bytes[i + 1];
                match escape {
                    b'u' => {
                        units.push(
                            hex4(&bytes[i + 2..i + 6])
                                .unwrap_or_else(|| panic!("bad \\u escape in fixture: {line}")),
                        );
                        i += 6;
                    }
                    b'"' | b'\\' | b'/' => {
                        units.push(u16::from(escape));
                        i += 2;
                    }
                    b'b' => {
                        units.push(0x0008);
                        i += 2;
                    }
                    b'f' => {
                        units.push(0x000C);
                        i += 2;
                    }
                    b'n' => {
                        units.push(0x000A);
                        i += 2;
                    }
                    b'r' => {
                        units.push(0x000D);
                        i += 2;
                    }
                    b't' => {
                        units.push(0x0009);
                        i += 2;
                    }
                    other => panic!("unsupported escape \\{} in fixture", char::from(other)),
                }
            }
            _ => {
                let ch = line[i..]
                    .chars()
                    .next()
                    .expect("index is on a char boundary");
                let mut buf = [0u16; 2];
                units.extend_from_slice(ch.encode_utf16(&mut buf));
                i += ch.len_utf8();
            }
        }
    }
}

fn options_from(value: &Value, label: &str) -> TruncationOptions {
    if value.is_null() {
        // `options: null` means Pi was called with no second argument.
        return TruncationOptions::default();
    }
    serde_json::from_value(value.clone())
        .unwrap_or_else(|e| panic!("{label}: cannot read options {value}: {e}"))
}

fn as_str<'a>(value: &'a Value, label: &str, field: &str) -> &'a str {
    value
        .as_str()
        .unwrap_or_else(|| panic!("{label}: `{field}` must be a string, got {value}"))
}

#[test]
fn truncate_matches_pi_oracle() {
    let path = fixture_path();
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read fixture {}: {e}", path.display()));
    let lines: Vec<&str> = raw.lines().filter(|l| !l.trim().is_empty()).collect();

    assert_eq!(
        lines.len(),
        EXPECTED_CASES,
        "fixture {} must still hold all {EXPECTED_CASES} oracle cases",
        path.display()
    );

    let mut seen: BTreeMap<&str, usize> = BTreeMap::new();

    for (idx, raw_line) in lines.iter().enumerate() {
        let case_no = idx + 1;
        let sanitized = map_lone_surrogates_to_replacement(raw_line);
        let case: Value = serde_json::from_str(&sanitized)
            .unwrap_or_else(|e| panic!("case {case_no}: malformed fixture JSON: {e}"));
        let kind = case["fn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case_no}: missing `fn`"));
        let note = case["note"].as_str().unwrap_or("<no note>");
        let label = format!("case {case_no} [{kind}] \"{note}\"");
        *seen
            .entry(match kind {
                "constants" => "constants",
                "truncateHead" => "truncateHead",
                "truncateTail" => "truncateTail",
                "truncateLine" => "truncateLine",
                "formatSize" => "formatSize",
                other => panic!("case {case_no}: unknown fn {other:?}"),
            })
            .or_default() += 1;

        match kind {
            "constants" => {
                let expected = &case["result"];
                assert_eq!(
                    Value::from(DEFAULT_MAX_LINES),
                    expected["DEFAULT_MAX_LINES"],
                    "{label}: DEFAULT_MAX_LINES"
                );
                assert_eq!(
                    Value::from(DEFAULT_MAX_BYTES),
                    expected["DEFAULT_MAX_BYTES"],
                    "{label}: DEFAULT_MAX_BYTES"
                );
                assert_eq!(
                    Value::from(GREP_MAX_LINE_LENGTH as u64),
                    expected["GREP_MAX_LINE_LENGTH"],
                    "{label}: GREP_MAX_LINE_LENGTH"
                );
            }

            "truncateHead" | "truncateTail" => {
                let input = as_str(&case["input"], &label, "input");
                let options = options_from(&case["options"], &label);
                let actual = if kind == "truncateHead" {
                    truncate_head(input, options)
                } else {
                    truncate_tail(input, options)
                };

                let expected: TruncationResult = serde_json::from_value(case["result"].clone())
                    .unwrap_or_else(|e| {
                        panic!(
                            "{label}: cannot read expected result {}: {e}",
                            case["result"]
                        )
                    });
                assert_eq!(
                    actual, expected,
                    "{label}: result fields diverged from Pi\n  input:   {input:?}\n  options: {options:?}"
                );

                // The result is persisted in session JSONL, so the serialized bytes
                // (key order, explicit nulls, integer formatting) must match too.
                let want = serde_json::to_string(&case["result"]).expect("serialize expected");
                let got = serde_json::to_string(&actual).expect("serialize actual");
                assert_eq!(
                    got, want,
                    "{label}: serialized TruncationResult must match Pi byte for byte"
                );
            }

            "truncateLine" => {
                let input = as_str(&case["input"], &label, "input");
                let max_chars = case["options"].get("maxChars").map(|v| {
                    usize::try_from(
                        v.as_u64()
                            .unwrap_or_else(|| panic!("{label}: maxChars must be a number")),
                    )
                    .expect("maxChars fits in usize")
                });
                let expected_truncated = case["result"]["wasTruncated"]
                    .as_bool()
                    .unwrap_or_else(|| panic!("{label}: missing wasTruncated"));

                // Exact assertion: Pi's literal escapes, decoded to UTF-16 units.
                let expected_units = expected_text_utf16(raw_line);
                let actual_utf16 = truncate_line_utf16(input, max_chars);
                assert_eq!(
                    actual_utf16.text,
                    expected_units,
                    "{label}: UTF-16 code units diverged from Pi\n  expected: {:04X?}\n  actual:   {:04X?}",
                    expected_units,
                    actual_utf16.text
                );
                assert_eq!(
                    actual_utf16.was_truncated, expected_truncated,
                    "{label}: wasTruncated diverged from Pi"
                );

                // `String` API: identical to Pi except that unpaired surrogates become
                // U+FFFD (documented divergence; fixture case 50 is the only record
                // where the two expectations differ).
                let expected_lossy = as_str(&case["result"]["text"], &label, "result.text");
                let actual = truncate_line(input, max_chars);
                assert_eq!(
                    actual.text, expected_lossy,
                    "{label}: text diverged from Pi (surrogates mapped to U+FFFD)"
                );
                assert_eq!(
                    actual.was_truncated, expected_truncated,
                    "{label}: wasTruncated diverged from Pi"
                );
            }

            "formatSize" => {
                let input = case["input"]
                    .as_u64()
                    .unwrap_or_else(|| panic!("{label}: input must be a byte count"));
                let expected = as_str(&case["result"], &label, "result");
                assert_eq!(
                    format_size(input),
                    expected,
                    "{label}: format_size({input}) diverged from Pi"
                );
            }

            other => panic!("case {case_no}: unknown fn {other:?}"),
        }
    }

    let breakdown: BTreeMap<&str, usize> = EXPECTED_BREAKDOWN.into_iter().collect();
    assert_eq!(
        seen, breakdown,
        "fixture must still cover every function with the same number of cases"
    );
}

/// Named guard for the two rows that pin `truncateHead`'s per-line newline charge,
/// `byteLength(line) + (i > 0 ? 1 : 0)` (`truncate.ts:128`).
///
/// The charge is invisible in `outputBytes`, which is recomputed from the joined
/// content, so it is observable only where the loop's *running total* decides the
/// cut. Charging the newline one line later (`i > 1`) survives every other captured
/// case — including `maxBytes: 6` on the same input, which is why that row is in the
/// fixture but is not the one doing the pinning. If these two rows are ever dropped,
/// [`EXPECTED_CASES`] alone would not notice a like-for-like replacement.
#[test]
fn the_newline_charge_rows_are_still_in_the_fixture() {
    let raw = std::fs::read_to_string(fixture_path()).expect("read fixture");
    let rows: Vec<Value> = raw
        .lines()
        // Filter textually first: one `truncateLine` record carries a lone surrogate
        // that `serde_json` cannot parse into a `String` (see
        // [`map_lone_surrogates_to_replacement`]), and no `truncateHead` record does.
        .filter(|line| line.starts_with(r#"{"fn":"truncateHead","#))
        .map(|line| serde_json::from_str(line).expect("parse truncateHead row"))
        .filter(|row: &Value| row["input"].as_str() == Some("aa\nbb\ncc\n"))
        .collect();

    // (maxBytes, captured content, captured truncatedBy) — Pi's own bytes.
    for (max_bytes, content, truncated_by) in [(4u64, "aa", "bytes"), (7, "aa\nbb", "bytes")] {
        let row = rows
            .iter()
            .find(|row| row["options"]["maxBytes"].as_u64() == Some(max_bytes))
            .unwrap_or_else(|| {
                panic!(
                    "the truncateHead(\"aa\\nbb\\ncc\\n\", {{maxBytes: {max_bytes}}}) row is what \
                     pins the `i > 0` newline charge; without it the condition is untested"
                )
            });
        assert_eq!(row["result"]["content"].as_str(), Some(content));
        assert_eq!(row["result"]["truncatedBy"].as_str(), Some(truncated_by));
    }
}
