//! Oracle test for [`OutputAccumulator`] against real Pi.
//!
//! `tests/fixtures/pi/tools/output_accumulator.cases.jsonl` was captured by
//! `scripts/gen-tools-oracle.mjs` driving Pi's own `OutputAccumulator` class
//! (`core/tools/output-accumulator.ts`): 5 scenarios, 21 steps total. Each line is
//! ONE step — the `append`/`finish` call to make, the `snapshot()` options to pass,
//! and the literal snapshot Pi returned plus `getLastLineBytes()` afterwards.
//!
//! So the replay must be stateful: steps of a scenario run in order against a single
//! accumulator built from that scenario's `options`, and every step's snapshot is
//! compared field by field. The only normalization is the spill file's path, which
//! is random by construction and which the fixture records as `{TMPFILE}`; that a
//! path is present at all (vs. `null`) IS the contract and is asserted exactly.
//!
//! A failure here means the port diverged from Pi — fix the port, never the
//! expectation.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use pirust_tools::output_accumulator::{
    OutputAccumulator, OutputAccumulatorOptions, SnapshotOptions,
};
use pirust_tools::truncate::TruncationResult;
use serde::Deserialize;

/// Every step Pi recorded. Asserted so a truncated fixture cannot silently weaken
/// this suite.
const CASE_COUNT: usize = 21;

/// Distinct scenarios in the fixture (`no-truncation`, `line-limit-spill`,
/// `byte-limit-and-trimtail`, `split-multibyte`, `persist-if-truncated`).
const SCENARIO_COUNT: usize = 5;

/// The placeholder `scripts/gen-tools-oracle.mjs` substitutes for the spill path.
const TMPFILE_PLACEHOLDER: &str = "{TMPFILE}";

// ---------------------------------------------------------------------------
// Fixture shape
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Case {
    scenario: String,
    step: usize,
    note: String,
    options: OutputAccumulatorOptions,
    action: Action,
    snapshot_options: SnapshotOptions,
    snapshot: ExpectedSnapshot,
    last_line_bytes: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Action {
    /// `"append"` or `"finish"`.
    op: String,
    /// Length of `hex`'s decoding, for `append` only (a fixture self-check).
    #[serde(default)]
    bytes: Option<usize>,
    /// The appended chunk, hex-encoded (`buf.toString("hex")`).
    #[serde(default)]
    hex: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExpectedSnapshot {
    content: String,
    truncation: TruncationResult,
    /// `{TMPFILE}` once a spill file exists, else `null` (the generator maps Pi's
    /// `undefined` to `null`).
    full_output_path: Option<String>,
}

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/pi/tools/output_accumulator.cases.jsonl")
}

fn load_cases() -> Vec<Case> {
    let path = fixture_path();
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read fixture {}: {e}", path.display()));
    raw.lines()
        .filter(|line| !line.trim().is_empty())
        .enumerate()
        .map(|(i, line)| {
            serde_json::from_str(line)
                .unwrap_or_else(|e| panic!("line {}: deserialize failed: {e}\n  {line}", i + 1))
        })
        .collect()
}

fn decode_hex(hex: &str) -> Vec<u8> {
    assert!(hex.len().is_multiple_of(2), "odd-length hex string {hex:?}");
    hex.as_bytes()
        .chunks(2)
        .map(|pair| {
            let s = std::str::from_utf8(pair).expect("hex is ASCII");
            u8::from_str_radix(s, 16).unwrap_or_else(|e| panic!("bad hex byte {s:?}: {e}"))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Diffing
// ---------------------------------------------------------------------------

fn cmp<T: PartialEq + std::fmt::Debug>(diffs: &mut Vec<String>, field: &str, want: &T, got: &T) {
    if want != got {
        diffs.push(format!("  {field}:\n    want {want:?}\n    got  {got:?}"));
    }
}

/// All 11 [`TruncationResult`] fields, individually, so a failure names the one
/// that drifted.
fn diff_truncation(diffs: &mut Vec<String>, want: &TruncationResult, got: &TruncationResult) {
    cmp(diffs, "truncation.content", &want.content, &got.content);
    cmp(
        diffs,
        "truncation.truncated",
        &want.truncated,
        &got.truncated,
    );
    cmp(
        diffs,
        "truncation.truncatedBy",
        &want.truncated_by,
        &got.truncated_by,
    );
    cmp(
        diffs,
        "truncation.totalLines",
        &want.total_lines,
        &got.total_lines,
    );
    cmp(
        diffs,
        "truncation.totalBytes",
        &want.total_bytes,
        &got.total_bytes,
    );
    cmp(
        diffs,
        "truncation.outputLines",
        &want.output_lines,
        &got.output_lines,
    );
    cmp(
        diffs,
        "truncation.outputBytes",
        &want.output_bytes,
        &got.output_bytes,
    );
    cmp(
        diffs,
        "truncation.lastLinePartial",
        &want.last_line_partial,
        &got.last_line_partial,
    );
    cmp(
        diffs,
        "truncation.firstLineExceedsLimit",
        &want.first_line_exceeds_limit,
        &got.first_line_exceeds_limit,
    );
    cmp(
        diffs,
        "truncation.maxLines",
        &want.max_lines,
        &got.max_lines,
    );
    cmp(
        diffs,
        "truncation.maxBytes",
        &want.max_bytes,
        &got.max_bytes,
    );
}

/// Removes every spill file the replay created, even on panic (so a failing
/// assertion does not leave litter in the temp dir).
#[derive(Default)]
struct SpillFiles(HashSet<PathBuf>);

impl Drop for SpillFiles {
    fn drop(&mut self) {
        for path in &self.0 {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Asserts Pi's `join(tmpdir(), `${prefix}-${randomBytes(8).toString("hex")}.log`)`
/// shape, which the `{TMPFILE}` placeholder necessarily hides.
fn assert_spill_path_shape(path: &Path, prefix: &str, case: &str) {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_else(|| panic!("{case}: spill path has no file name: {path:?}"));
    let id = name
        .strip_prefix(&format!("{prefix}-"))
        .and_then(|rest| rest.strip_suffix(".log"))
        .unwrap_or_else(|| panic!("{case}: spill name {name:?} is not `{prefix}-<id>.log`"));
    assert_eq!(id.len(), 16, "{case}: id {id:?} should be 8 bytes as hex");
    assert!(
        id.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
        "{case}: id {id:?} should be lowercase hex"
    );
    assert_eq!(
        path.parent(),
        Some(std::env::temp_dir().as_path()),
        "{case}: spill file should live in the temp dir"
    );
}

// ---------------------------------------------------------------------------
// The replay
// ---------------------------------------------------------------------------

#[tokio::test]
async fn oracle_cases_replay_identically() {
    let cases = load_cases();
    assert_eq!(
        cases.len(),
        CASE_COUNT,
        "fixture should hold all {CASE_COUNT} oracle steps"
    );

    let mut spills = SpillFiles::default();
    let mut scenario_order: Vec<String> = Vec::new();
    // Scenario -> (accumulator, every raw byte appended so far).
    let mut current: Option<(String, OutputAccumulator, Vec<u8>)> = None;
    let mut steps_per_scenario: HashMap<String, usize> = HashMap::new();
    let mut checked = 0usize;

    for case in &cases {
        let label = format!("{}#{} ({})", case.scenario, case.step, case.note);

        // A new scenario means a fresh accumulator; close out the previous one.
        let is_new_scenario = current
            .as_ref()
            .is_none_or(|(name, ..)| *name != case.scenario);
        if is_new_scenario {
            if let Some((name, acc, raw)) = current.take() {
                finish_scenario(&name, acc, &raw).await;
            }
            assert!(
                !scenario_order.contains(&case.scenario),
                "{label}: scenario rows must be contiguous"
            );
            scenario_order.push(case.scenario.clone());
            current = Some((
                case.scenario.clone(),
                OutputAccumulator::new(case.options.clone()),
                Vec::new(),
            ));
        }
        let (_, acc, raw) = current.as_mut().expect("initialized above");

        let seen = steps_per_scenario.entry(case.scenario.clone()).or_default();
        *seen += 1;
        assert_eq!(case.step, *seen, "{label}: steps must be 1..n in order");

        // ---- drive the recorded action ----
        match case.action.op.as_str() {
            "append" => {
                let hex = case.action.hex.as_deref().expect("append records hex");
                let data = decode_hex(hex);
                if let Some(expected_len) = case.action.bytes {
                    assert_eq!(
                        data.len(),
                        expected_len,
                        "{label}: fixture hex/bytes mismatch"
                    );
                }
                acc.append(&data)
                    .unwrap_or_else(|e| panic!("{label}: append failed: {e}"));
                raw.extend_from_slice(&data);
            }
            "finish" => acc.finish(),
            other => panic!("{label}: unknown op {other:?}"),
        }

        // ---- compare the snapshot Pi produced ----
        let got = acc.snapshot(case.snapshot_options);
        let got_path_normalized = got.full_output_path.as_ref().map(|path| {
            let prefix = case
                .options
                .temp_file_prefix
                .as_deref()
                .expect("fixture always sets tempFilePrefix");
            assert_spill_path_shape(path, prefix, &label);
            spills.0.insert(path.clone());
            TMPFILE_PLACEHOLDER.to_string()
        });

        let mut diffs = Vec::new();
        cmp(&mut diffs, "content", &case.snapshot.content, &got.content);
        diff_truncation(&mut diffs, &case.snapshot.truncation, &got.truncation);
        cmp(
            &mut diffs,
            "fullOutputPath",
            &case.snapshot.full_output_path,
            &got_path_normalized,
        );
        cmp(
            &mut diffs,
            "getLastLineBytes()",
            &case.last_line_bytes,
            &acc.get_last_line_bytes(),
        );
        // Pi returns the same string in both places (`snapshot.content` is
        // `truncation.content`).
        cmp(
            &mut diffs,
            "content == truncation.content",
            &got.content,
            &got.truncation.content,
        );

        assert!(
            diffs.is_empty(),
            "case {label} diverges from Pi:\n{}",
            diffs.join("\n")
        );
        checked += 1;
    }

    if let Some((name, acc, raw)) = current.take() {
        finish_scenario(&name, acc, &raw).await;
    }

    assert_eq!(checked, CASE_COUNT, "every case must be compared");
    assert_eq!(
        scenario_order.len(),
        SCENARIO_COUNT,
        "fixture scenarios: {scenario_order:?}"
    );
}

/// End-of-scenario: `closeTempFile()` (the generator awaits it too), then verify the
/// contract the fixture cannot express — the spill file holds EVERY raw byte, in
/// arrival order, including the chunks buffered in memory before the threshold
/// tripped.
async fn finish_scenario(scenario: &str, mut acc: OutputAccumulator, raw: &[u8]) {
    let path = acc.temp_file_path().map(Path::to_path_buf);
    acc.close_temp_file()
        .await
        .unwrap_or_else(|e| panic!("{scenario}: closeTempFile failed: {e}"));

    if let Some(path) = path {
        let written = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("{scenario}: read spill {}: {e}", path.display()));
        assert_eq!(
            written,
            raw,
            "{scenario}: spill file must hold all {} raw bytes in order (got {})",
            raw.len(),
            written.len()
        );
    }
}

/// `append` after `finish` throws in Pi (`output-accumulator.ts:65-67`); no fixture
/// row can capture a throw, so it is pinned here with Pi's literal message.
#[test]
fn append_after_finish_matches_pi_message() {
    let mut acc = OutputAccumulator::new(OutputAccumulatorOptions::default());
    acc.append(b"hello\n").expect("first append");
    acc.finish();
    let err = acc.append(b"more\n").expect_err("append after finish");
    assert_eq!(
        err.to_string(),
        "Cannot append to a finished output accumulator"
    );
    assert!(
        acc.temp_file_path().is_none(),
        "no spill under the defaults"
    );
}

/// The oracle only splits a 2-byte character across two appends
/// (`split-multibyte`). Pi's `TextDecoder(..., { stream: true })` must hold a
/// partial sequence across *any* number of chunks, so drive the same accumulator
/// one byte at a time over 2-, 3- and 4-byte characters and check the counters land
/// where a single append would have put them.
#[test]
fn multibyte_split_one_byte_at_a_time_matches_a_single_append() {
    let text = "caf\u{e9}\n世界\n🎉\n";
    let options = OutputAccumulatorOptions {
        max_lines: Some(100),
        max_bytes: Some(4096),
        temp_file_prefix: Some("pirust-oa-golden".to_string()),
    };

    let mut whole = OutputAccumulator::new(options.clone());
    whole.append(text.as_bytes()).unwrap();
    whole.finish();
    let expected = whole.snapshot(SnapshotOptions::default());

    let mut split = OutputAccumulator::new(options);
    for byte in text.as_bytes() {
        split.append(&[*byte]).unwrap();
    }
    split.finish();
    let got = split.snapshot(SnapshotOptions::default());

    assert_eq!(got.content, text);
    assert_eq!(
        got, expected,
        "per-byte chunking must not change the result"
    );
    assert_eq!(
        got.truncation.total_bytes,
        text.len() as u64,
        "no U+FFFD may be minted for a merely incomplete sequence"
    );
    assert!(got.full_output_path.is_none());
}
