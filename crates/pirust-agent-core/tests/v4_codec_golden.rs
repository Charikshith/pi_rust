//! Golden replay for the v4 session codec against
//! `tests/fixtures/pi/agent/v4/codec.cases.jsonl` (captured from real Pi 0.84.2's
//! `harness/session/jsonl/codec.ts`).
//!
//! Every `encodeHeader` / `encodeMutation` record carries Pi's EXACT `encoded`
//! bytes; the Rust port must produce identical bytes for the same input, and
//! parse the same line back losslessly. Error records must fail with the same
//! `kind` / message shape.

use std::path::PathBuf;

use pirust_agent_core::harness::session::v4::codec::{
    encode_header, encode_mutation, parse_header, parse_mutation,
};
use serde_json::Value;

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("fixtures")
        .join("pi")
        .join("agent")
        .join("v4")
        .join("codec.cases.jsonl")
}

fn load_records() -> Vec<Value> {
    let text = std::fs::read_to_string(fixture_path()).unwrap_or_else(|e| {
        panic!("read v4 codec fixture ({e}); run: node scripts/gen-v4-session-oracle.mjs")
    });
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("fixture line is JSON"))
        .collect()
}

/// Compare a mutation parsed from Pi's own encoded line (the `parsed` field) —
/// re-encoded by the port — against Pi's literal `encoded` bytes.
fn check_encoded_mutation(record: &Value) -> Vec<String> {
    let mut problems = Vec::new();
    let name = record["name"].as_str().unwrap_or("?");
    let want_encoded = record["encoded"].as_str().expect("encoded");

    // The mutation line Pi produced (strip the trailing newline).
    let line = want_encoded.trim_end();
    let parsed = match parse_mutation(line) {
        Ok(m) => m,
        Err(e) => {
            problems.push(format!("{name}: port could not parse Pi's own line: {e:?}"));
            return problems;
        }
    };
    // Re-encode and compare bytes.
    let got = encode_mutation(&parsed);
    if got != want_encoded {
        problems.push(format!(
            "{name}: encode mismatch\n  want {want_encoded:?}\n  got  {got:?}"
        ));
    }
    problems
}

fn check_encoded_header(record: &Value) -> Vec<String> {
    let mut problems = Vec::new();
    let name = record["name"].as_str().unwrap_or("?");
    let want_encoded = record["encoded"].as_str().expect("encoded");
    let line = want_encoded.trim_end();
    match parse_header(line) {
        Ok(header) => {
            let got = encode_header(&header);
            if got != want_encoded {
                problems.push(format!(
                    "{name}: header encode mismatch\n  want {want_encoded:?}\n  got  {got:?}"
                ));
            }
        }
        Err(e) => problems.push(format!("{name}: port could not parse Pi's header: {e:?}")),
    }
    problems
}

/// Error records: the port must reject the same lines with the same kind.
fn check_error(record: &Value) -> Vec<String> {
    let mut problems = Vec::new();
    let name = record["name"].as_str().unwrap_or("?");
    let line = record["line"].as_str().expect("line");
    let want_ok = record["ok"].as_bool().expect("ok");
    let want_kind = record["errorKind"].as_str();
    let want_message = record["errorMessage"].as_str();

    let result = parse_mutation(line);
    match result {
        Ok(_) => {
            if !want_ok {
                problems.push(format!("{name}: port accepted a line Pi rejects"));
            }
        }
        Err(e) => {
            if want_ok {
                problems.push(format!("{name}: port rejected a line Pi accepts"));
            }
            if let Some(k) = want_kind {
                if e.kind != k {
                    problems.push(format!("{name}: error kind want {k:?} got {:?}", e.kind));
                }
            }
            if let Some(m) = want_message {
                if e.message != m {
                    problems.push(format!(
                        "{name}: error message\n  want {m:?}\n  got  {:?}",
                        e.message
                    ));
                }
            }
        }
    }
    problems
}

#[test]
fn every_codec_record_matches_pis_bytes() {
    let records = load_records();
    assert!(
        records.len() >= 25,
        "the v4 codec fixture should carry all 25 records"
    );

    let mut problems: Vec<String> = Vec::new();
    let mut counts = std::collections::BTreeMap::<&str, usize>::new();
    for record in &records {
        let fn_name = record["fn"].as_str().expect("fn");
        *counts.entry(fn_name).or_default() += 1;
        match fn_name {
            "encodeHeader" => problems.extend(check_encoded_header(record)),
            "metadataFromHeader" => {}
            "encodeMutation" => problems.extend(check_encoded_mutation(record)),
            "parseMutation-error" => problems.extend(check_error(record)),
            _ => {}
        }
    }

    assert_eq!(
        counts.get("encodeHeader"),
        Some(&3),
        "3 header records expected"
    );
    assert_eq!(
        counts.get("encodeMutation"),
        Some(&11),
        "11 mutation records expected"
    );
    assert_eq!(
        counts.get("parseMutation-error"),
        Some(&8),
        "8 error records expected"
    );
    assert_eq!(counts.get("metadataFromHeader"), Some(&3));

    if !problems.is_empty() {
        panic!(
            "{} v4 codec assertion(s) failed:\n{}",
            problems.len(),
            problems.join("\n")
        );
    }
}
