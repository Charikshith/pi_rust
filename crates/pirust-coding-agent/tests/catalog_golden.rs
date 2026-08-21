//! Oracle for the generated builtin catalog — `src/catalog.rs` vs the oracle data it was
//! generated from.
//!
//! [`pirust_coding_agent::catalog::builtin_catalog`] is emitted by `cargo xtask gen-catalog`
//! from real Pi 0.84.2's generated catalog data (`providers/data/*.json`, sha256-pinned by
//! `.manifest.json`) plus the `builtinCatalogFingerprint` record of
//! `tests/fixtures/pi/cli/models.cases.jsonl` (captured from real Pi). Nothing keeps a
//! generated file honest on its own, so this suite re-derives the same expectation from the
//! oracle and compares:
//!
//! - a **stale checked-in file** (someone edited `catalog.rs`, or forgot to rerun the
//!   generator after the oracle data changed) fails here;
//! - a **drifting oracle** (a recapture that renamed a model or moved the `baseUrl`) fails
//!   here too, which is the point — the two must be regenerated together;
//! - a **shrunken oracle** cannot silently weaken the suite: the provider count and the
//!   model counts are asserted against the fixture as well as the data, so deleting rows
//!   fails instead of testing less.
//!
//! Every `want` is either read out of the oracle or, where a literal appears below, pinned
//! *and* cross-checked against the oracle in the same assertion.

use std::path::PathBuf;

use pirust_ai::types::{Model, ProviderId};
use pirust_coding_agent::catalog::builtin_catalog;
use serde_json::Value;

/// House style: `crates/pirust-agent-core/tests/session_golden.rs`.
fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/pi/cli/models.cases.jsonl")
}

/// The one `builtinCatalogFingerprint` record — the generator's whole input.
fn fingerprint() -> Value {
    let path = fixture_path();
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let mut found: Vec<Value> = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("{}:{}: not JSON: {e}", path.display(), index + 1));
        if value["fn"].as_str() == Some("builtinCatalogFingerprint") {
            found.push(value);
        }
    }
    assert_eq!(
        found.len(),
        1,
        "expected exactly one builtinCatalogFingerprint record in {}",
        path.display()
    );
    found.pop().expect("one record")
}

/// The fixture's `anthropic.models` array, deserialized through the same
/// [`pirust_ai::types::Model`] the generator targets.
///
/// A field the generator dropped, renamed or mistyped shows up as an inequality against
/// these; a field `Model` cannot represent shows up as a deserialization failure.
fn fixture_models(record: &Value) -> Vec<Model> {
    record["anthropic"]["models"]
        .as_array()
        .expect("the fingerprint enumerates anthropic's models")
        .iter()
        .map(|value| {
            serde_json::from_value::<Model>(value.clone()).unwrap_or_else(|e| {
                panic!("fixture model does not fit pirust_ai::Model: {e}\n  {value}")
            })
        })
        .collect()
}

/// The 13 ids `cargo xtask gen-catalog` emitted, in catalog order.
///
/// Pinned as a literal *and* asserted against the fixture, so neither side can drift alone.
/// Order is load-bearing: it seeds `getModels()` and therefore `availableModels[0]` in step 4
/// of `find_initial_model`.
const ANTHROPIC_MODEL_IDS: [&str; 13] = [
    "claude-fable-5",
    "claude-haiku-4-5",
    "claude-haiku-4-5-20251001",
    "claude-opus-4-5",
    "claude-opus-4-5-20251101",
    "claude-opus-4-6",
    "claude-opus-4-7",
    "claude-opus-4-8",
    "claude-opus-5",
    "claude-sonnet-4-5",
    "claude-sonnet-4-5-20250929",
    "claude-sonnet-4-6",
    "claude-sonnet-5",
];

/// The generated catalog is field-for-field what the fixture record says.
///
/// This is the assertion that makes `catalog.rs` trustworthy despite being checked in: it
/// compares whole [`Model`] values, so `cost`, `thinkingLevelMap` (including `off: null` vs an
/// absent key), `input`, `contextWindow`, `maxTokens`, `headers` and `compat` are all covered,
/// not just the ids.
#[test]
fn generated_catalog_matches_the_fixture_field_for_field() {
    let record = fingerprint();
    let want = fixture_models(&record);
    let catalog = builtin_catalog();
    let anthropic = catalog
        .get("anthropic")
        .expect("the generated catalog carries the anthropic provider");

    assert_eq!(
        want.len(),
        ANTHROPIC_MODEL_IDS.len(),
        "the fixture record must still enumerate all {} anthropic models — a shrunken fixture \
         would silently weaken every assertion in this file",
        ANTHROPIC_MODEL_IDS.len()
    );
    assert_eq!(
        anthropic.models.len(),
        want.len(),
        "catalog.rs is stale: run `cargo xtask gen-catalog`"
    );

    for (position, want_model) in want.iter().enumerate() {
        let got = &anthropic.models[position];
        assert_eq!(
            got, want_model,
            "models[{position}] ({}) differs from the fixture — run `cargo xtask gen-catalog`",
            want_model.id
        );
    }
}

/// The 14 ids, and the provider's `name` / `baseUrl`, are exactly the fixture's.
#[test]
fn model_ids_and_provider_identity_are_the_fixtures() {
    let record = fingerprint();
    let catalog = builtin_catalog();
    let anthropic = catalog.get("anthropic").expect("anthropic provider");

    // The fixture's ids, in fixture order.
    let want_ids: Vec<&str> = record["anthropic"]["models"]
        .as_array()
        .expect("models")
        .iter()
        .map(|m| m["id"].as_str().expect("model id is a string"))
        .collect();
    assert_eq!(
        want_ids, ANTHROPIC_MODEL_IDS,
        "the fixture's model ids moved; regenerate catalog.rs and update ANTHROPIC_MODEL_IDS"
    );

    let got_ids: Vec<&str> = anthropic.models.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(
        got_ids, want_ids,
        "catalog.rs's model ids/order are stale: run `cargo xtask gen-catalog`"
    );

    assert_eq!(
        anthropic.id, "anthropic",
        "the descriptor id keys `ModelCatalog::get`"
    );
    assert_eq!(
        Some(anthropic.name.as_str()),
        record["anthropic"]["name"].as_str(),
        "provider name (what `--list-models` and `/model` show)"
    );
    assert_eq!(
        anthropic.base_url.as_deref(),
        record["anthropic"]["baseUrl"].as_str(),
        "provider baseUrl (what spec §9.5's models.json override replaces)"
    );
}

/// The catalog carries every builtin provider of the fingerprint, in fingerprint order.
///
/// Each provider's `name`/`baseUrl`/auth flags come from the fingerprint's `providers`
/// array (captured from real Pi's `ModelRuntime.getProviders()`); every provider whose
/// models are in the generated data contributes them. Radius is the dynamic provider with
/// no static catalog — it is present as a provider (id/name/auth) with zero models, exactly
/// matching real Pi (`MODELS["radius"]` is undefined → `getModels("radius")` is `[]`).
#[test]
fn catalog_carries_every_builtin_provider_in_fingerprint_order() {
    let record = fingerprint();
    let catalog = builtin_catalog();

    let want: Vec<&str> = record["providers"]
        .as_array()
        .expect("fingerprint.providers array")
        .iter()
        .map(|p| p["id"].as_str().expect("provider id"))
        .collect();
    assert_eq!(
        want.len(),
        40,
        "the fingerprint must still enumerate all builtin providers"
    );

    let got: Vec<&str> = catalog.providers().iter().map(|p| p.id.as_str()).collect();
    assert_eq!(
        got, want,
        "the catalog must carry every builtin provider in fingerprint order — run `cargo xtask gen-catalog`"
    );

    for provider in catalog.providers() {
        let meta = record["providers"]
            .as_array()
            .expect("providers")
            .iter()
            .find(|p| p["id"] == provider.id)
            .unwrap_or_else(|| panic!("{}: no fingerprint metadata", provider.id));
        assert_eq!(
            provider.name,
            meta["name"].as_str().expect("name"),
            "{}: provider name must be the fingerprint's",
            provider.id
        );
        assert_eq!(
            provider.base_url.as_deref(),
            meta["baseUrl"].as_str(),
            "{}: baseUrl must be the fingerprint's (None for per-request providers)",
            provider.id
        );
        assert_eq!(
            provider.has_api_key_auth,
            meta["hasApiKeyAuth"].as_bool().expect("hasApiKeyAuth"),
            "{}: api-key auth flag",
            provider.id
        );
        assert_eq!(
            provider.has_oauth_auth,
            meta["hasOauthAuth"].as_bool().expect("hasOauthAuth"),
            "{}: oauth auth flag",
            provider.id
        );

        // Every model belongs to its provider; radius has none (dynamic catalog).
        for model in &provider.models {
            assert_eq!(
                model.provider,
                ProviderId::from(provider.id.as_str()),
                "{}: every model belongs to its provider",
                model.id
            );
            assert!(
                model.api.is_known(),
                "{}: model.api {:?} must be a known adapter",
                model.id,
                model.api
            );
        }
    }
}

/// The anthropic provider's `api_key_env` is the real env precedence; the oauth flag now
/// matches real Pi (anthropic has both api-key and oauth auth in 0.84.2).
#[test]
fn anthropic_auth_shape_matches_real_pi() {
    let catalog = builtin_catalog();
    let anthropic = catalog.get("anthropic").expect("anthropic provider");

    assert_eq!(
        anthropic.api_key_env,
        pirust_ai::auth::ANTHROPIC_API_KEY_ENV_PRECEDENCE
            .iter()
            .map(|name| (*name).to_string())
            .collect::<Vec<String>>(),
        "api_key_env is `ModelRuntime::check_auth`'s last resort, in precedence order"
    );
    assert!(
        anthropic.has_api_key_auth,
        "anthropic inherits api-key auth"
    );
    assert!(
        anthropic.has_oauth_auth,
        "anthropic has oauth auth in the 0.84.2 oracle (oauth flows themselves are a later wave)"
    );
}

/// `models.rs` flags that TypeBox accepts any `number` for `contextWindow`/`maxTokens` while
/// [`Model`] stores `u64`, so a fractional value would truncate. This confirms it is a
/// non-issue for all 13 real models: every captured value is a non-negative integer, and the
/// generator refuses to emit anything else.
#[test]
fn context_window_and_max_tokens_are_integers_in_all_13_models() {
    let record = fingerprint();
    let models = record["anthropic"]["models"].as_array().expect("models");
    assert_eq!(models.len(), ANTHROPIC_MODEL_IDS.len());

    for model in models {
        let id = model["id"].as_str().unwrap_or_default();
        for field in ["contextWindow", "maxTokens"] {
            let value = &model[field];
            assert!(
                value.is_u64(),
                "{id}.{field} = {value} is not a non-negative integer, so pirust_ai::Model's \
                 u64 would truncate it"
            );
        }
    }
}
