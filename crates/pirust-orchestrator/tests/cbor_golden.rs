//! feat-009 Wave 1 goldens: replay the CBOR codec cases captured from REAL
//! Pi (`packages/protocol/src/cbor/{encoder,decoder}.ts`) by
//! `scripts/gen-orchestrator-oracle.mjs`.
//!
//! What each assertion proves:
//! - `roundtrip` rows: our `encode_cbor` produces Pi's exact wire bytes for
//!   the same logical value, AND `decode_cbor` reconstructs an equal value.
//! - `decode_reject`/`decode_limit` rows: our decoder rejects every input
//!   real Pi's decoder rejects (malformed frames, unsafe integers, oversized
//!   declared lengths, invalid UTF-8, duplicate map keys, etc).
//! - `encode_reject` rows: our encoder rejects every input real Pi's encoder
//!   rejects that is still representable by `CborValue` (non-finite floats,
//!   unsafe integers, excess depth, stricter caller-provided limits). JS-only
//!   rejections (BigInt/Symbol/Function/Date/Map/cycles/array holes) are not
//!   present in the fixture — see `scripts/gen-orchestrator-oracle.mjs`'s own
//!   header comment for why they're type-system-moot in Rust.

use pirust_orchestrator::protocol::cbor::{
    decode_cbor, decode_cbor_with, encode_cbor, encode_cbor_with, CborOptions, CborValue,
};
use serde_json::Value as Json;

fn fixture_lines() -> Vec<Json> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/pi/orchestrator")
        .join("cbor.cases.jsonl");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!("missing fixture {path:?}: {e} — run scripts/gen-orchestrator-oracle.mjs")
    });
    text.lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn from_hex(hex: &str) -> Vec<u8> {
    assert_eq!(hex.len() % 2, 0, "odd hex length: {hex}");
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect()
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Rebuilds a [`CborValue`] from the oracle script's tagged JSON spec
/// (`untag` in `gen-orchestrator-oracle.mjs`, mirrored here).
fn untag(spec: &Json) -> CborValue {
    let t = spec["t"].as_str().expect("tagged spec has a `t` field");
    match t {
        "null" => CborValue::Null,
        "bool" => CborValue::Bool(spec["v"].as_bool().unwrap()),
        "int" => CborValue::Number(spec["v"].as_f64().unwrap()),
        "float" => CborValue::Number(spec["v"].as_f64().unwrap()),
        "negZero" => CborValue::Number(-0.0),
        "nan" => CborValue::Number(f64::NAN),
        "posInf" => CborValue::Number(f64::INFINITY),
        "negInf" => CborValue::Number(f64::NEG_INFINITY),
        "bytes" => CborValue::Bytes(from_hex(spec["hex"].as_str().unwrap())),
        "text" => CborValue::Text(spec["v"].as_str().unwrap().to_string()),
        "array" => CborValue::Array(spec["v"].as_array().unwrap().iter().map(untag).collect()),
        "map" => CborValue::Map(
            spec["v"]
                .as_array()
                .unwrap()
                .iter()
                .map(|pair| {
                    let pair = pair.as_array().unwrap();
                    (pair[0].as_str().unwrap().to_string(), untag(&pair[1]))
                })
                .collect(),
        ),
        "nested" => {
            let depth = spec["depth"].as_u64().unwrap();
            let mut value = CborValue::Null;
            for _ in 0..depth {
                value = CborValue::Array(vec![value]);
            }
            value
        }
        other => panic!("unknown tag {other}"),
    }
}

fn parse_options(spec: &Json) -> Option<CborOptions> {
    let obj = spec.as_object()?;
    let mut options = CborOptions::default();
    if let Some(v) = obj.get("maxByteLength").and_then(Json::as_u64) {
        options.max_byte_length = v;
    }
    if let Some(v) = obj.get("maxContainerLength").and_then(Json::as_u64) {
        options.max_container_length = v;
    }
    if let Some(v) = obj.get("maxDepth").and_then(Json::as_u64) {
        options.max_depth = v as u32;
    }
    Some(options)
}

#[test]
fn cbor_cases_match_real_pi() {
    let records = fixture_lines();
    assert!(!records.is_empty(), "fixture should not be empty");

    let mut roundtrip = 0;
    let mut decode_reject = 0;
    let mut encode_reject = 0;
    let mut decode_limit = 0;

    for record in &records {
        let description = record["description"].as_str().unwrap();
        match record["kind"].as_str().unwrap() {
            "roundtrip"
                if description == "object with an undefined property omits that key only" =>
            {
                roundtrip += 1;
                // Rust's `CborValue::Map` has no "omitted key" input — a
                // caller who wants a key absent simply doesn't push it into
                // the `Vec`. So there is nothing to test on the *encode*
                // side beyond what the other map fixtures already cover;
                // only the decode direction (Pi's real omission behavior,
                // captured as `hex`) is meaningful here.
                let expected_hex = record["hex"].as_str().unwrap();
                let expected = CborValue::Map(vec![
                    ("zero".to_string(), CborValue::Number(0.0)),
                    ("empty".to_string(), CborValue::Text(String::new())),
                    ("no".to_string(), CborValue::Bool(false)),
                    ("nil".to_string(), CborValue::Null),
                ]);
                let decoded = decode_cbor(&from_hex(expected_hex))
                    .unwrap_or_else(|e| panic!("decode failed for {description:?}: {e}"));
                assert_eq!(decoded, expected, "decode mismatch for {description:?}");
            }
            "roundtrip" => {
                roundtrip += 1;
                let value = untag(&record["input"]);
                let expected_hex = record["hex"].as_str().unwrap();
                let encoded = encode_cbor(&value)
                    .unwrap_or_else(|e| panic!("encode failed for {description:?}: {e}"));
                assert_eq!(
                    to_hex(&encoded),
                    expected_hex,
                    "encode mismatch for {description:?}"
                );
                let decoded = decode_cbor(&from_hex(expected_hex))
                    .unwrap_or_else(|e| panic!("decode failed for {description:?}: {e}"));
                assert_eq!(decoded, value, "decode mismatch for {description:?}");
            }
            "decode_reject" => {
                decode_reject += 1;
                let hex = record["hex"].as_str().unwrap();
                let result = decode_cbor(&from_hex(hex));
                assert!(
                    result.is_err(),
                    "expected decode rejection for {description:?}, got {result:?}"
                );
            }
            "encode_reject" => {
                encode_reject += 1;
                let value = untag(&record["input"]);
                let result = match record.get("options").and_then(parse_options) {
                    Some(options) => encode_cbor_with(&value, &options),
                    None => encode_cbor(&value),
                };
                assert!(
                    result.is_err(),
                    "expected encode rejection for {description:?}, got {result:?}"
                );
            }
            "decode_limit" => {
                decode_limit += 1;
                let hex = record["hex"].as_str().unwrap();
                let options =
                    parse_options(&record["options"]).expect("decode_limit rows carry options");
                let result = decode_cbor_with(&from_hex(hex), &options);
                assert!(
                    result.is_err(),
                    "expected limited decode rejection for {description:?}, got {result:?}"
                );
            }
            other => panic!("unknown record kind {other}"),
        }
    }

    assert!(
        roundtrip >= 30,
        "expected a substantial roundtrip battery, got {roundtrip}"
    );
    assert!(
        decode_reject >= 20,
        "expected a substantial decode-rejection battery, got {decode_reject}"
    );
    assert!(
        encode_reject >= 5,
        "expected encode-rejection cases, got {encode_reject}"
    );
    assert!(
        decode_limit >= 2,
        "expected decode-limit cases, got {decode_limit}"
    );
}

#[test]
fn negative_zero_is_distinct_from_positive_zero() {
    assert_ne!(CborValue::Number(-0.0), CborValue::Number(0.0));
    assert_eq!(CborValue::Number(-0.0), CborValue::Number(-0.0));
}

#[test]
fn top_level_trailing_bytes_are_rejected() {
    // "trailing data" is already in the fixture, but this pins the exact
    // wire shape (two back-to-back `0` integers) independent of fixture drift.
    let result = decode_cbor(&[0x00, 0x00]);
    assert!(result.is_err());
}
