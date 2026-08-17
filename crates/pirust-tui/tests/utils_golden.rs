//! Pi oracle for [`pirust_tui::utils`] (feat-006 Wave 1).
//!
//! Replays every record of `tests/fixtures/pi/tui/utils.cases.jsonl` — captured by
//! executing real Pi's `packages/tui/src/utils.ts` — and asserts byte-identical results.

use std::path::PathBuf;

use pirust_tui::utils::{
    apply_background_to_line, extract_ansi_code, extract_segments, is_punctuation_char,
    is_whitespace_char, normalize_terminal_output, slice_by_column, slice_with_width,
    truncate_to_width, visible_width, wrap_text_with_ansi,
};
use serde_json::Value;

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/pi/tui/utils.cases.jsonl")
}

fn load_records() -> Vec<Value> {
    let path = fixture_path();
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read fixture {}: {error}", path.display()));
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .enumerate()
        .map(|(index, line)| {
            serde_json::from_str(line)
                .unwrap_or_else(|error| panic!("line {}: {error}\n  {line}", index + 1))
        })
        .collect()
}

fn s(v: &Value) -> &str {
    v.as_str().unwrap()
}

fn u(v: &Value) -> usize {
    v.as_u64().unwrap() as usize
}

fn b(v: &Value) -> bool {
    v.as_bool().unwrap()
}

fn bg_fn(text: &str) -> String {
    format!("<bg>{text}</bg>")
}

fn actual_for(fn_name: &str, args: &[Value]) -> Value {
    match fn_name {
        "visibleWidth" => Value::from(visible_width(s(&args[0]))),
        "wrapTextWithAnsi" => Value::from(wrap_text_with_ansi(s(&args[0]), u(&args[1]))),
        "truncateToWidth" => Value::from(truncate_to_width(
            s(&args[0]),
            u(&args[1]),
            s(&args[2]),
            b(&args[3]),
        )),
        "sliceByColumn" => Value::from(slice_by_column(
            s(&args[0]),
            u(&args[1]),
            u(&args[2]),
            b(&args[3]),
        )),
        "sliceWithWidth" => {
            let r = slice_with_width(s(&args[0]), u(&args[1]), u(&args[2]), b(&args[3]));
            serde_json::json!({ "text": r.text, "width": r.width })
        }
        "extractSegments" => {
            let r = extract_segments(
                s(&args[0]),
                u(&args[1]),
                u(&args[2]),
                u(&args[3]),
                b(&args[4]),
            );
            serde_json::json!({
                "before": r.before,
                "beforeWidth": r.before_width,
                "after": r.after,
                "afterWidth": r.after_width,
            })
        }
        "normalizeTerminalOutput" => Value::from(normalize_terminal_output(s(&args[0]))),
        "extractAnsiCode" => match extract_ansi_code(s(&args[0]), u(&args[1])) {
            Some(ansi) => serde_json::json!({ "code": ansi.code, "length": ansi.length }),
            None => Value::Null,
        },
        "isWhitespaceChar" => Value::from(is_whitespace_char(s(&args[0]))),
        "isPunctuationChar" => Value::from(is_punctuation_char(s(&args[0]))),
        "applyBackgroundToLine" => {
            Value::from(apply_background_to_line(s(&args[0]), u(&args[1]), bg_fn))
        }
        other => panic!("no Rust dispatch wired for oracle fn {other:?}"),
    }
}

#[test]
fn every_utils_record_matches_pi() {
    let records = load_records();
    assert_eq!(
        records.len(),
        99,
        "fixture record count changed — update this assertion deliberately"
    );

    let mut failures = Vec::new();
    for record in &records {
        let note = record["note"].as_str().unwrap();
        let fn_name = record["fn"].as_str().unwrap();
        let args = record["args"].as_array().unwrap();
        let expected = &record["result"];
        let actual = actual_for(fn_name, args);
        if &actual != expected {
            failures.push(format!(
                "[{note}] {fn_name}\n  expected: {expected}\n  actual:   {actual}"
            ));
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));
}
