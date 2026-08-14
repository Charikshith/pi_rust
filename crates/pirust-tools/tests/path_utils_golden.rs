//! Oracle for `core/tools/path-utils.ts` + `utils/paths.ts`.
//!
//! Replays every row of `tests/fixtures/pi/tools/path_utils.cases.jsonl` — 50
//! `resolveToCwd` / `expandPath` vectors captured from real Pi by
//! `scripts/gen-tools-oracle.mjs` — and compares the exact result string. A
//! failure means the Rust port diverged from Pi; the fix is the port, never the
//! assertion.
//!
//! The capture script replaced `os.homedir()` with the literal `{HOME}` and
//! recorded `platform` plus (for `resolveToCwd`) the literal `cwd` on every row,
//! so the corpus is machine independent. This test therefore feeds
//! [`PathEnv`] the row's own platform and a fixed synthetic home rather than the
//! test machine's — the substitution and the port then see the *same* home, and
//! the comparison stays exact on any box. That the default seam really reads
//! `os.homedir()` / `process.platform` is pinned separately by
//! `current_env_wires_the_seams_through` in `src/path_utils.rs`.

use pirust_tools::path_utils::{self, PathEnv, PathError, Platform};
use serde_json::Value;
use std::path::PathBuf;

/// Every non-empty line of the captured corpus.
fn fixture_lines() -> Vec<String> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/pi/tools/path_utils.cases.jsonl");
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read fixture {}: {e}", path.display()));
    raw.lines()
        .filter(|l| !l.trim().is_empty())
        .map(str::to_owned)
        .collect()
}

/// The stand-in for the capture machine's `os.homedir()`, shaped for the row's
/// platform. Substituted into `{HOME}` *and* handed to the port, so it cancels
/// out of the comparison.
fn synthetic_home(platform: Platform) -> &'static str {
    match platform {
        Platform::Win32 => "C:\\oracle\\home",
        Platform::Posix => "/oracle/home",
    }
}

/// `process.cwd()` for rows that carry no `cwd` (the `expandPath` half). Only
/// reachable as `path.resolve`'s last-resort base, which `expandPath` never hits.
fn default_cwd(platform: Platform) -> &'static str {
    match platform {
        Platform::Win32 => "C:\\oracle\\cwd",
        Platform::Posix => "/oracle/cwd",
    }
}

#[test]
fn path_utils_cases_match_pi() {
    let lines = fixture_lines();

    // A truncated fixture must not silently weaken the suite.
    assert_eq!(
        lines.len(),
        50,
        "path_utils.cases.jsonl should hold all 50 captured cases"
    );

    for (idx, line) in lines.iter().enumerate() {
        let case = idx + 1;
        let rec: Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("case {case}: deserialize failed: {e}\n  {line}"));

        let func = rec["fn"].as_str().expect("fn");
        let note = rec["note"].as_str().expect("note");
        let input = rec["input"].as_str().expect("input");
        let ok = rec["ok"].as_bool().expect("ok");
        let platform = match rec["platform"].as_str().expect("platform") {
            "win32" => Platform::Win32,
            "darwin" | "linux" | "freebsd" | "openbsd" | "sunos" | "aix" => Platform::Posix,
            other => panic!("case {case}: unknown captured platform {other:?}"),
        };

        let home = synthetic_home(platform);
        let cwd = rec["cwd"].as_str().map_or_else(
            || default_cwd(platform).to_string(),
            |c| c.replace("{HOME}", home),
        );
        let env = PathEnv {
            platform,
            home_dir: home.to_string(),
            cwd: cwd.clone(),
        };

        let actual: Result<String, PathError> = match func {
            "expandPath" => path_utils::expand_path_in(&env, input),
            "resolveToCwd" => path_utils::resolve_to_cwd_in(&env, input, &cwd),
            other => panic!("case {case}: unknown captured fn {other:?}"),
        };

        if ok {
            let want = rec["result"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case}: ok row without a string result"))
                .replace("{HOME}", home);
            let got = actual.unwrap_or_else(|e| {
                panic!(
                    "case {case} {func} ({note})\n  input:    {input:?}\n  cwd:      {cwd:?}\n  \
                     expected: {want:?}\n  actual:   Err({e})"
                )
            });
            assert_eq!(
                got, want,
                "case {case} {func} ({note})\n  input:    {input:?}\n  cwd:      {cwd:?}\n  \
                 expected: {want:?}\n  actual:   {got:?}"
            );
        } else {
            // No captured row throws today; if one is ever added, Pi's exact
            // `err.message` is what PathError::Display reproduces.
            let want = rec["error"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case}: !ok row without an error message"))
                .replace("{HOME}", home);
            let err = actual.expect_err(&format!(
                "case {case} {func} ({note}): Pi threw {want:?}, port returned Ok"
            ));
            assert_eq!(
                err.to_string(),
                want,
                "case {case} {func} ({note})\n  input:    {input:?}\n  \
                 expected: Err({want})\n  actual:   Err({err})"
            );
        }
    }
}

/// Guards the placeholder contract itself: if the capture script ever stops
/// emitting `{HOME}`, the tilde rows would silently become no-ops.
#[test]
fn tilde_rows_still_carry_the_home_placeholder() {
    let with_placeholder = fixture_lines()
        .iter()
        .filter(|l| l.contains("{HOME}"))
        .count();
    assert_eq!(
        with_placeholder, 8,
        "the 4 tilde inputs x 2 fns should each carry a {{HOME}} placeholder"
    );
}

/// **Oracle for `tryNFDVariant`, not a captured fixture case.**
///
/// `path-utils.ts:11-14` `tryNFDVariant` is `filePath.normalize("NFD")`, i.e.
/// full Unicode canonical decomposition. `try_nfd_variant` gets that from
/// `unicode_normalization::UnicodeNormalization::nfd`; the four pairs below are
/// what real Pi returns for inputs the old Latin-1-only decomposition could not
/// handle (Latin Extended-A/B, Hangul, multi-step), so they pin the whole of NFD
/// rather than one block of it.
///
/// None of these inputs appears in `path_utils.cases.jsonl` — the captured
/// corpus does not exercise `resolveReadPath` at all, so every one of its 50
/// rows passes today.
#[test]
fn nfd_variant_matches_js_string_normalize() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_string_lossy().into_owned();

    // Each pair is (what the user types, the NFD filename macOS actually stores).
    let cases: &[(&str, &str)] = &[
        // Latin Extended-A: ō = o + U+0304 (macron).
        ("t\u{014D}kyo.png", "to\u{0304}kyo.png"),
        // Latin Extended-B / additional: ș = s + U+0326.
        ("bucure\u{0219}ti.png", "bucures\u{0326}ti.png"),
        // Hangul syllable decomposition: 한 = U+1112 U+1161 U+11AB.
        ("\u{D55C}.png", "\u{1112}\u{1161}\u{11AB}.png"),
        // Multi-step: ǭ = o + U+0328 + U+0304 (U+01EB then U+0304).
        ("\u{01ED}.png", "o\u{0328}\u{0304}.png"),
    ];

    for (typed, stored) in cases {
        std::fs::write(tmp.path().join(stored), b"x").unwrap();
        let got = path_utils::resolve_read_path(typed, &root).unwrap();
        let want = path_utils::resolve_to_cwd(stored, &root).unwrap();
        assert_eq!(
            got, want,
            "resolveReadPath({typed:?}) should have found the NFD filename {stored:?}"
        );
    }
}
