//! Pi oracle for [`pirust_tui::terminal_colors`] (feat-006 Wave 4).
//!
//! Replays every record of `tests/fixtures/pi/tui/terminal-colors.cases.jsonl`
//! — captured by executing real Pi's `packages/tui/src/terminal-colors.ts` —
//! and asserts identical results.

use std::path::PathBuf;

use pirust_tui::terminal_colors::{
    is_osc11_background_color_response, parse_osc11_background_color,
    parse_terminal_color_scheme_report, RgbColor, TerminalColorScheme,
};
use serde_json::Value;

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/pi/tui/terminal-colors.cases.jsonl")
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

fn rgb_to_value(rgb: RgbColor) -> Value {
    serde_json::json!({ "r": rgb.r, "g": rgb.g, "b": rgb.b })
}

fn scheme_to_str(scheme: TerminalColorScheme) -> &'static str {
    match scheme {
        TerminalColorScheme::Dark => "dark",
        TerminalColorScheme::Light => "light",
    }
}

#[test]
fn every_terminal_colors_record_matches_pi() {
    let records = load_records();
    assert_eq!(
        records.len(),
        17,
        "fixture record count changed — update this assertion deliberately"
    );

    let mut failures = Vec::new();
    for record in &records {
        let note = record["note"].as_str().unwrap();
        let fn_name = record["fn"].as_str().unwrap();
        let args = record["args"].as_array().unwrap();
        let data = args[0].as_str().unwrap();
        let expected = &record["result"];

        let actual: Value = match fn_name {
            "isOsc11BackgroundColorResponse" => {
                Value::Bool(is_osc11_background_color_response(data))
            }
            "parseOsc11BackgroundColor" => match parse_osc11_background_color(data) {
                Some(rgb) => rgb_to_value(rgb),
                None => Value::Null,
            },
            "parseTerminalColorSchemeReport" => match parse_terminal_color_scheme_report(data) {
                Some(scheme) => Value::String(scheme_to_str(scheme).to_string()),
                None => Value::Null,
            },
            other => panic!("unknown fn {other}"),
        };

        if &actual != expected {
            failures.push(format!(
                "[{note}] {fn_name}({data:?})\n  expected: {expected}\n  actual:   {actual}"
            ));
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));
}
