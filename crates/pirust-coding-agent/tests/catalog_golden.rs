//! Oracle for the generated builtin catalog — `src/catalog.rs` vs the fixture it was
//! generated from.
//!
//! [`pirust_coding_agent::catalog::builtin_catalog`] is emitted by `cargo xtask gen-catalog`
//! out of the `builtinCatalogFingerprint` record of `tests/fixtures/pi/cli/models.cases.jsonl`
//! (captured from real Pi 0.80.10). Nothing keeps a generated file honest on its own, so this
//! suite re-derives the same expectation from the fixture and compares:
//!
//! - a **stale checked-in file** (someone edited `catalog.rs`, or forgot to rerun the
//!   generator after the fixture changed) fails here;
//! - a **drifting fixture** (a recapture that renamed a model or moved the `baseUrl`) fails
//!   here too, which is the point — the two must be regenerated together;
//! - a **shrunken fixture** cannot silently weaken the suite: the model count and the 14 ids
//!   are asserted against literals as well as against the fixture, so deleting fixture rows
//!   fails instead of testing less.
//!
//! Every `want` is either read out of the fixture or, where a literal appears below, pinned
//! *and* cross-checked against the fixture in the same assertion.

use std::path::PathBuf;

use pirust_ai::types::{Api, Model, ProviderId};
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

/// The slice is anthropic-only, and only carries models the ported adapter can stream.
///
/// The 35 other providers of the fingerprint are **not** descriptors here — see the module
/// docs on `src/catalog.rs`. `tests/models_golden.rs` builds them as empty shells for its own
/// `totalProviders` check; that construction is test-side and this asserts the catalog does
/// not repeat it.
#[test]
fn slice_is_anthropic_only_and_every_model_can_stream() {
    let record = fingerprint();
    let catalog = builtin_catalog();

    let ids: Vec<&str> = catalog.providers().iter().map(|p| p.id.as_str()).collect();
    assert_eq!(
        ids,
        ["anthropic"],
        "the slice ships exactly one provider; the fingerprint's other {} ids are deliberately \
         absent because their api adapters are not ported",
        record["providerIds"].as_array().map_or(0, Vec::len) - 1
    );

    let anthropic = catalog.get("anthropic").expect("anthropic provider");
    for model in &anthropic.models {
        assert_eq!(
            model.provider,
            ProviderId::from("anthropic"),
            "{}: every model in the slice belongs to anthropic",
            model.id
        );
        assert_eq!(
            model.api,
            Api::from("anthropic-messages"),
            "{}: the slice may only advertise models the ported adapter can stream",
            model.id
        );
        assert!(
            model.api.is_known(),
            "{}: `anthropic-messages` must stay a KnownApi",
            model.id
        );
    }

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
        !anthropic.has_oauth_auth,
        "feat-005 models no oauth method (matching tests/models_golden.rs)"
    );
}

/// `models.rs` flags that TypeBox accepts any `number` for `contextWindow`/`maxTokens` while
/// [`Model`] stores `u64`, so a fractional value would truncate. This confirms it is a
/// non-issue for all 14 real models: every captured value is a non-negative integer, and the
/// generator refuses to emit anything else.
#[test]
fn context_window_and_max_tokens_are_integers_in_all_14_models() {
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
