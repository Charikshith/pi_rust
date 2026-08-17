//! Pi oracle for [`pirust_tui::keybindings`] (feat-006 Wave 3).
//!
//! Replays every record of `tests/fixtures/pi/tui/keybindings.cases.jsonl` —
//! captured by executing real Pi's `packages/tui/src/keybindings.ts` — and
//! asserts identical results.
//!
//! `getConflicts()` results are compared with claimant order (and conflict
//! order) normalized (sorted) on both sides — see
//! `pirust_tui::keybindings`'s module docs, "Conflict-list order is not
//! asserted exactly against the oracle": that order depends on the
//! user-config object's own key insertion order, which the JSON fixture
//! boundary does not preserve (no `preserve_order` feature on `serde_json`
//! in this workspace). This is an oracle-fidelity limitation of the JSONL
//! pipeline, not a Rust/TS behavioral divergence — which keybindings
//! conflict, and each binding's own key-list order, are asserted exactly.

use std::collections::HashMap;
use std::path::PathBuf;

use pirust_tui::keybindings::{Keybinding, KeybindingsManager, RawKeys};
use serde_json::Value;

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/pi/tui/keybindings.cases.jsonl")
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

fn parse_raw_keys(v: &Value) -> RawKeys {
    match v {
        Value::String(s) => RawKeys::One(s.clone()),
        Value::Array(arr) => RawKeys::Many(
            arr.iter()
                .map(|x| x.as_str().unwrap().to_string())
                .collect(),
        ),
        other => panic!("unexpected raw-keys shape: {other}"),
    }
}

fn parse_user_bindings(v: &Value) -> HashMap<String, RawKeys> {
    v.as_object()
        .unwrap()
        .iter()
        .map(|(k, val)| (k.clone(), parse_raw_keys(val)))
        .collect()
}

fn raw_keys_to_value(rk: &RawKeys) -> Value {
    match rk {
        RawKeys::One(s) => Value::String(s.clone()),
        RawKeys::Many(v) => Value::Array(v.iter().map(|s| Value::String(s.clone())).collect()),
    }
}

fn user_bindings_to_value(map: &HashMap<String, RawKeys>) -> Value {
    Value::Object(
        map.iter()
            .map(|(k, v)| (k.clone(), raw_keys_to_value(v)))
            .collect(),
    )
}

/// Sorted (conflict order, then claimant-id order within each conflict) so
/// comparisons are insensitive to the JSON object-key-order loss described in
/// this file's module docs.
fn normalized_conflicts_from_manager(mgr: &KeybindingsManager) -> Vec<(String, Vec<String>)> {
    let mut out: Vec<(String, Vec<String>)> = mgr
        .get_conflicts()
        .into_iter()
        .map(|c| {
            let mut ids: Vec<String> = c.keybindings.iter().map(|kb| kb.id().to_string()).collect();
            ids.sort();
            (c.key, ids)
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn normalized_conflicts_from_json(v: &Value) -> Vec<(String, Vec<String>)> {
    let mut out: Vec<(String, Vec<String>)> = v
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            let key = c["key"].as_str().unwrap().to_string();
            let mut ids: Vec<String> = c["keybindings"]
                .as_array()
                .unwrap()
                .iter()
                .map(|x| x.as_str().unwrap().to_string())
                .collect();
            ids.sort();
            (key, ids)
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn run_call(mgr: &mut KeybindingsManager, call: &Value) -> Value {
    let fn_name = call["fn"].as_str().unwrap();
    let args = call["args"].as_array().unwrap();
    match fn_name {
        "matches" => {
            let data = args[0].as_str().unwrap();
            let kb = Keybinding::from_id(args[1].as_str().unwrap()).unwrap();
            Value::Bool(mgr.matches(data, kb))
        }
        "getKeys" => {
            let kb = Keybinding::from_id(args[0].as_str().unwrap()).unwrap();
            serde_json::to_value(mgr.get_keys(kb)).unwrap()
        }
        "getConflicts" => Value::Null, // handled specially in the caller
        "getResolvedBindings" => user_bindings_to_value(&mgr.get_resolved_bindings()),
        "getUserBindings" => user_bindings_to_value(&mgr.get_user_bindings()),
        "setUserBindings" => {
            mgr.set_user_bindings(parse_user_bindings(&args[0]));
            Value::Null
        }
        other => panic!("no Rust dispatch wired for oracle fn {other:?}"),
    }
}

#[test]
fn every_keybindings_record_matches_pi() {
    let records = load_records();
    assert_eq!(
        records.len(),
        8,
        "fixture record count changed — update this assertion deliberately"
    );

    let mut failures = Vec::new();
    for record in &records {
        let note = record["note"].as_str().unwrap();
        let mut mgr = KeybindingsManager::new(parse_user_bindings(&record["userBindings"]));
        let calls = record["calls"].as_array().unwrap();
        let expected_results = record["results"].as_array().unwrap();

        for (call, expected) in calls.iter().zip(expected_results.iter()) {
            let fn_name = call["fn"].as_str().unwrap();
            if fn_name == "getConflicts" {
                let actual = normalized_conflicts_from_manager(&mgr);
                let expected_norm = normalized_conflicts_from_json(expected);
                if actual != expected_norm {
                    failures.push(format!(
                        "[{note}] getConflicts\n  expected: {expected_norm:?}\n  actual:   {actual:?}"
                    ));
                }
                continue;
            }
            let actual = run_call(&mut mgr, call);
            if &actual != expected {
                failures.push(format!(
                    "[{note}] {fn_name}\n  expected: {expected}\n  actual:   {actual}"
                ));
            }
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));
}
