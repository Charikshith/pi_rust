//! Byte oracle for the v3 session-tree serializer (§1.4 + §11.A).
//!
//! Proves that [`SessionTreeEntry`] and [`SessionHeader`] round-trip
//! byte-identically against Pi's literal `JSON.stringify` output: for every
//! fixture line we deserialize, re-serialize (compact), and assert the bytes are
//! unchanged. A failure here means the Rust type diverged from Pi (key order,
//! optionality, tag, or number formatting) — the fix is to correct the type, not
//! to loosen the assertion.

use pirust_agent_core::harness::types::{SessionHeader, SessionTreeEntry};

/// Workspace-root fixtures dir (`CARGO_MANIFEST_DIR` = crates/pirust-agent-core).
fn fixture(path: &str) -> String {
    let root = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/pi/agent/"
    );
    std::fs::read_to_string(format!("{root}{path}"))
        .unwrap_or_else(|e| panic!("read fixture {path}: {e}"))
}

/// Report the first byte position at which `got` diverges from `want`.
fn first_diff(want: &str, got: &str) -> String {
    let wb = want.as_bytes();
    let gb = got.as_bytes();
    let n = wb.len().min(gb.len());
    let mut i = 0;
    while i < n && wb[i] == gb[i] {
        i += 1;
    }
    format!(
        "first diff at byte {i}:\n  want …{}\n  got  …{}",
        &want[i..want.len().min(i + 40)],
        &got[i..got.len().min(i + 40)],
    )
}

#[test]
fn entries_corpus_roundtrips_byte_identical() {
    let corpus = fixture("entries.corpus.jsonl");
    let lines: Vec<&str> = corpus.lines().filter(|l| !l.trim().is_empty()).collect();

    assert_eq!(lines.len(), 17, "corpus should have all 17 entry lines");

    for (idx, line) in lines.iter().enumerate() {
        let entry: SessionTreeEntry = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("line {}: deserialize failed: {e}\n  {line}", idx + 1));
        let reser = serde_json::to_string(&entry)
            .unwrap_or_else(|e| panic!("line {}: serialize failed: {e}", idx + 1));
        assert_eq!(
            &reser,
            line,
            "line {} not byte-identical\n{}",
            idx + 1,
            first_diff(line, &reser)
        );
    }
}

#[test]
fn header_golden_roundtrips_byte_identical() {
    for name in ["header.golden", "header.withmeta.golden"] {
        let raw = fixture(name);
        let line = raw
            .lines()
            .find(|l| !l.trim().is_empty())
            .expect("header line");
        let header: SessionHeader = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("{name}: deserialize failed: {e}\n  {line}"));
        let reser = serde_json::to_string(&header)
            .unwrap_or_else(|e| panic!("{name}: serialize failed: {e}"));
        assert_eq!(
            &reser,
            line,
            "{name} not byte-identical\n{}",
            first_diff(line, &reser)
        );
    }
}
