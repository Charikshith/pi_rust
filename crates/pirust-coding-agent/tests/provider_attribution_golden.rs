//! Pi oracle for [`pirust_coding_agent::provider_attribution`] (feat-005 Wave 4b).
//!
//! Replays every record of `tests/fixtures/pi/sdk/provider-attribution.cases.jsonl` —
//! captured by executing real Pi's `mergeProviderAttributionHeaders()` — and asserts
//! byte-identical header sets (order-sensitive: Pi's `Object.assign` is
//! insertion-ordered, and so is this port's `assign`).

use std::path::PathBuf;

use std::sync::Arc;

use pirust_coding_agent::provider_attribution::merge_provider_attribution_headers;
use pirust_coding_agent::settings::{
    InMemorySettingsStorage, SettingsManager, SettingsManagerCreateOptions, SettingsScope,
    SettingsStorage,
};
use serde_json::Value;

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/pi/sdk/provider-attribution.cases.jsonl")
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

fn headers_from_object(value: &Value) -> Vec<(String, String)> {
    value
        .as_object()
        .map(|obj| {
            obj.iter()
                .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string()))
                .collect()
        })
        .unwrap_or_default()
}

fn settings_manager(enable_install_telemetry: bool) -> SettingsManager {
    let storage = InMemorySettingsStorage::new();
    let contents = serde_json::to_string(&serde_json::json!({
        "enableInstallTelemetry": enable_install_telemetry
    }))
    .unwrap();
    storage
        .with_lock(SettingsScope::Global, &mut |_| Some(contents.clone()))
        .unwrap();
    SettingsManager::from_storage(Arc::new(storage), SettingsManagerCreateOptions::default())
}

#[test]
fn every_provider_attribution_record_matches_pi() {
    let records = load_records();
    assert_eq!(
        records.len(),
        10,
        "fixture record count changed — update this assertion deliberately"
    );

    let mut failures = Vec::new();
    for record in &records {
        let note = record["note"].as_str().unwrap();
        let model_json = &record["model"];
        let model: pirust_ai::types::model::Model = serde_json::from_value(serde_json::json!({
            "id": "m",
            "name": "m",
            "api": "anthropic-messages",
            "provider": model_json["provider"],
            "baseUrl": model_json["baseUrl"],
            "reasoning": false,
            "input": ["text"],
            "cost": {"input":0.0,"output":0.0,"cacheRead":0.0,"cacheWrite":0.0},
            "contextWindow": 1,
            "maxTokens": 1,
        }))
        .unwrap_or_else(|error| panic!("[{note}] model fixture: {error}"));

        let settings = settings_manager(record["enableInstallTelemetry"].as_bool().unwrap());
        let session_id = record["sessionId"].as_str();
        let header_sources: Vec<Option<Vec<(String, String)>>> = record["headerSources"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| {
                if h.is_null() {
                    None
                } else {
                    Some(headers_from_object(h))
                }
            })
            .collect();

        let actual =
            merge_provider_attribution_headers(&model, &settings, session_id, &header_sources);
        let expected: Option<Vec<(String, String)>> = if record["result"].is_null() {
            None
        } else {
            Some(headers_from_object(&record["result"]))
        };
        if actual != expected {
            failures.push(format!(
                "[{note}]\n  expected: {expected:?}\n  actual:   {actual:?}"
            ));
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));
}
