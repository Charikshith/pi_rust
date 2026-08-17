//! Pi oracle for [`pirust_tui::keys`] (feat-006 Wave 2).
//!
//! Replays every record of `tests/fixtures/pi/tui/keys.cases.jsonl` — captured by
//! executing real Pi's `packages/tui/src/keys.ts` — and asserts identical results.

use std::path::PathBuf;

use pirust_tui::keys::{
    decode_kitty_printable, decode_printable_key, is_key_release, is_key_repeat, matches_key,
    parse_key, set_kitty_protocol_active,
};
use serde_json::Value;

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/pi/tui/keys.cases.jsonl")
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

const ENV_KEYS: [&str; 4] = ["WT_SESSION", "SSH_CONNECTION", "SSH_CLIENT", "SSH_TTY"];

/// Mirrors the oracle generator's `withEnv`: only touches process env at all
/// when `overrides` is present, exactly like the JS side only calls `withEnv`
/// for cases that pass `envOverrides`.
fn with_env<T>(overrides: Option<&Value>, f: impl FnOnce() -> T) -> T {
    let Some(Value::Object(map)) = overrides else {
        return f();
    };
    let saved: Vec<(&str, Option<String>)> = ENV_KEYS
        .iter()
        .map(|k| (*k, std::env::var(k).ok()))
        .collect();
    for k in ENV_KEYS {
        std::env::remove_var(k);
    }
    for (k, v) in map {
        std::env::set_var(k, v.as_str().unwrap());
    }
    let result = f();
    for (k, v) in saved {
        match v {
            Some(val) => std::env::set_var(k, val),
            None => std::env::remove_var(k),
        }
    }
    result
}

fn actual_for(fn_name: &str, args: &[Value]) -> Value {
    let data = args[0].as_str().unwrap();
    match fn_name {
        "matchesKey" => Value::from(matches_key(data, args[1].as_str().unwrap())),
        "parseKey" => parse_key(data).map_or(Value::Null, Value::from),
        "decodeKittyPrintable" => decode_kitty_printable(data).map_or(Value::Null, Value::from),
        "decodePrintableKey" => decode_printable_key(data).map_or(Value::Null, Value::from),
        "isKeyRelease" => Value::from(is_key_release(data)),
        "isKeyRepeat" => Value::from(is_key_repeat(data)),
        other => panic!("no Rust dispatch wired for oracle fn {other:?}"),
    }
}

#[test]
fn every_keys_record_matches_pi() {
    let records = load_records();
    assert_eq!(
        records.len(),
        306,
        "fixture record count changed — update this assertion deliberately"
    );

    let mut failures = Vec::new();
    for record in &records {
        let note = record["note"].as_str().unwrap();
        let kitty_active = record["kittyActive"].as_bool().unwrap();
        let env_overrides = record.get("envOverrides").filter(|v| !v.is_null());
        let fn_name = record["fn"].as_str().unwrap();
        let args = record["args"].as_array().unwrap();
        let expected = &record["result"];

        set_kitty_protocol_active(kitty_active);
        let actual = with_env(env_overrides, || actual_for(fn_name, args));

        if &actual != expected {
            failures.push(format!(
                "[{note}] {fn_name}\n  expected: {expected}\n  actual:   {actual}"
            ));
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));
}
