//! The builtin model catalog — **GENERATED, do not edit**.
//!
//! Run `cargo xtask gen-catalog` to regenerate. Every literal below comes from the
//! `builtinCatalogFingerprint` record of
//! `tests/fixtures/pi/cli/models.cases.jsonl`, captured from real Pi 0.80.10.
//!
//! Pi imports a generated 36-provider / 1062-model table from
//! `@earendil-works/pi-ai/providers/all`. feat-008 owns the equivalent generator; until it
//! lands, this file carries the **anthropic-only slice** — `anthropic-messages` is the only ported
//! api adapter, so `anthropic` is the only provider whose 14 models can actually
//! stream, and spec §9.5's "override the builtin `baseUrl`" shape keeps working.
//!
//! # Why one [`ProviderDescriptor`] and not 36
//!
//! The fingerprint enumerates 36 provider ids but reduces every non-anthropic one to just
//! its id — no name, no `baseUrl`, no models. `tests/models_golden.rs` rebuilds those 35 as
//! empty-model shells so the fixture's `totalProviders` stays exact; that is a **test-side**
//! construction and deliberately not repeated here. Shells are model-invisible, so
//! `get_models`, `get_available`, `list_models` and every resolver in [`crate::models`]
//! behave identically without them — but they *are* visible to
//! `ModelRuntime::provider_ids`, `get_provider` and `configured_providers`, where they
//! would advertise 35 providers with an invented name, no `baseUrl` and nothing to stream.
//!
//! Delete this module when feat-008's generator arrives.

use pirust_ai::types::{
    Api, Modality, Model, ModelCost, ModelCostRates, ProviderId, ThinkingLevelMap,
};
use serde_json::{Map, Value};

use crate::models::{ModelCatalog, ProviderDescriptor};

/// The builtin catalog `ModelRuntime::create` is handed — one provider, 14 models.
///
/// A plain constructor rather than a `static`: [`Model`] and [`ProviderDescriptor`] are
/// `String`/`Vec`/`Value`-shaped, so no `const` form exists, and `ModelRuntime::create`
/// takes the catalog **by value** — a `LazyLock` would only add a clone of the same
/// allocations. There is no parsing here at any point, which is the whole point of
/// generating Rust instead of embedding the fixture JSON.
pub fn builtin_catalog() -> ModelCatalog {
    ModelCatalog::new(vec![anthropic_provider()])
}

/// Pi's builtin `anthropic` provider.
///
/// `id`, `name`, `base_url` and `models` are the fingerprint's. The three auth fields are
/// not in that record — it captures the catalog's *shape*, not its auth wiring — so they
/// come from the oracle-verified construction in `tests/models_golden.rs`:
/// [`pirust_ai::auth::ANTHROPIC_API_KEY_ENV_PRECEDENCE`] is the env order
/// `ModelRuntime::check_auth`'s last resort walks, api-key auth is inherited, and no
/// oauth method is modelled (feat-005 has no radius/oauth flow).
fn anthropic_provider() -> ProviderDescriptor {
    ProviderDescriptor {
        id: "anthropic".to_string(),
        name: "Anthropic".to_string(),
        base_url: Some("https://api.anthropic.com".to_string()),
        models: anthropic_models(),
        api_key_env: pirust_ai::auth::ANTHROPIC_API_KEY_ENV_PRECEDENCE
            .iter()
            .map(|name| (*name).to_string())
            .collect(),
        has_api_key_auth: true,
        has_oauth_auth: false,
    }
}

/// The 14 builtin `anthropic` models, in catalog order — which is load-bearing: it seeds
/// `getModels()`' order and therefore `availableModels[0]` in step 4 of
/// `find_initial_model`.
fn anthropic_models() -> Vec<Model> {
    vec![
        Model {
            id: "claude-fable-5".to_string(),
            name: "Claude Fable 5".to_string(),
            api: Api::from("anthropic-messages"),
            provider: ProviderId::from("anthropic"),
            base_url: "https://api.anthropic.com".to_string(),
            reasoning: true,
            thinking_level_map: Some(ThinkingLevelMap {
                off: Some(None),
                minimal: None,
                low: None,
                medium: None,
                high: None,
                xhigh: Some(Some("xhigh".to_string())),
                max: Some(Some("max".to_string())),
            }),
            input: vec![Modality::Text, Modality::Image],
            cost: ModelCost {
                rates: ModelCostRates {
                    input: 10.0,
                    output: 50.0,
                    cache_read: 1.0,
                    cache_write: 12.5,
                },
                tiers: None,
            },
            context_window: 1_000_000,
            max_tokens: 128_000,
            headers: None,
            compat: compat_object([("forceAdaptiveThinking", Value::Bool(true))]),
        },
        Model {
            id: "claude-haiku-4-5".to_string(),
            name: "Claude Haiku 4.5 (latest)".to_string(),
            api: Api::from("anthropic-messages"),
            provider: ProviderId::from("anthropic"),
            base_url: "https://api.anthropic.com".to_string(),
            reasoning: true,
            thinking_level_map: None,
            input: vec![Modality::Text, Modality::Image],
            cost: ModelCost {
                rates: ModelCostRates {
                    input: 1.0,
                    output: 5.0,
                    cache_read: 0.1,
                    cache_write: 1.25,
                },
                tiers: None,
            },
            context_window: 200_000,
            max_tokens: 64_000,
            headers: None,
            compat: None,
        },
        Model {
            id: "claude-haiku-4-5-20251001".to_string(),
            name: "Claude Haiku 4.5".to_string(),
            api: Api::from("anthropic-messages"),
            provider: ProviderId::from("anthropic"),
            base_url: "https://api.anthropic.com".to_string(),
            reasoning: true,
            thinking_level_map: None,
            input: vec![Modality::Text, Modality::Image],
            cost: ModelCost {
                rates: ModelCostRates {
                    input: 1.0,
                    output: 5.0,
                    cache_read: 0.1,
                    cache_write: 1.25,
                },
                tiers: None,
            },
            context_window: 200_000,
            max_tokens: 64_000,
            headers: None,
            compat: None,
        },
        Model {
            id: "claude-opus-4-1".to_string(),
            name: "Claude Opus 4.1 (latest)".to_string(),
            api: Api::from("anthropic-messages"),
            provider: ProviderId::from("anthropic"),
            base_url: "https://api.anthropic.com".to_string(),
            reasoning: true,
            thinking_level_map: None,
            input: vec![Modality::Text, Modality::Image],
            cost: ModelCost {
                rates: ModelCostRates {
                    input: 15.0,
                    output: 75.0,
                    cache_read: 1.5,
                    cache_write: 18.75,
                },
                tiers: None,
            },
            context_window: 200_000,
            max_tokens: 32_000,
            headers: None,
            compat: None,
        },
        Model {
            id: "claude-opus-4-1-20250805".to_string(),
            name: "Claude Opus 4.1".to_string(),
            api: Api::from("anthropic-messages"),
            provider: ProviderId::from("anthropic"),
            base_url: "https://api.anthropic.com".to_string(),
            reasoning: true,
            thinking_level_map: None,
            input: vec![Modality::Text, Modality::Image],
            cost: ModelCost {
                rates: ModelCostRates {
                    input: 15.0,
                    output: 75.0,
                    cache_read: 1.5,
                    cache_write: 18.75,
                },
                tiers: None,
            },
            context_window: 200_000,
            max_tokens: 32_000,
            headers: None,
            compat: None,
        },
        Model {
            id: "claude-opus-4-5".to_string(),
            name: "Claude Opus 4.5 (latest)".to_string(),
            api: Api::from("anthropic-messages"),
            provider: ProviderId::from("anthropic"),
            base_url: "https://api.anthropic.com".to_string(),
            reasoning: true,
            thinking_level_map: None,
            input: vec![Modality::Text, Modality::Image],
            cost: ModelCost {
                rates: ModelCostRates {
                    input: 5.0,
                    output: 25.0,
                    cache_read: 0.5,
                    cache_write: 6.25,
                },
                tiers: None,
            },
            context_window: 200_000,
            max_tokens: 64_000,
            headers: None,
            compat: None,
        },
        Model {
            id: "claude-opus-4-5-20251101".to_string(),
            name: "Claude Opus 4.5".to_string(),
            api: Api::from("anthropic-messages"),
            provider: ProviderId::from("anthropic"),
            base_url: "https://api.anthropic.com".to_string(),
            reasoning: true,
            thinking_level_map: None,
            input: vec![Modality::Text, Modality::Image],
            cost: ModelCost {
                rates: ModelCostRates {
                    input: 5.0,
                    output: 25.0,
                    cache_read: 0.5,
                    cache_write: 6.25,
                },
                tiers: None,
            },
            context_window: 200_000,
            max_tokens: 64_000,
            headers: None,
            compat: None,
        },
        Model {
            id: "claude-opus-4-6".to_string(),
            name: "Claude Opus 4.6".to_string(),
            api: Api::from("anthropic-messages"),
            provider: ProviderId::from("anthropic"),
            base_url: "https://api.anthropic.com".to_string(),
            reasoning: true,
            thinking_level_map: Some(ThinkingLevelMap {
                off: None,
                minimal: None,
                low: None,
                medium: None,
                high: None,
                xhigh: None,
                max: Some(Some("max".to_string())),
            }),
            input: vec![Modality::Text, Modality::Image],
            cost: ModelCost {
                rates: ModelCostRates {
                    input: 5.0,
                    output: 25.0,
                    cache_read: 0.5,
                    cache_write: 6.25,
                },
                tiers: None,
            },
            context_window: 1_000_000,
            max_tokens: 128_000,
            headers: None,
            compat: compat_object([("forceAdaptiveThinking", Value::Bool(true))]),
        },
        Model {
            id: "claude-opus-4-7".to_string(),
            name: "Claude Opus 4.7".to_string(),
            api: Api::from("anthropic-messages"),
            provider: ProviderId::from("anthropic"),
            base_url: "https://api.anthropic.com".to_string(),
            reasoning: true,
            thinking_level_map: Some(ThinkingLevelMap {
                off: None,
                minimal: None,
                low: None,
                medium: None,
                high: None,
                xhigh: Some(Some("xhigh".to_string())),
                max: Some(Some("max".to_string())),
            }),
            input: vec![Modality::Text, Modality::Image],
            cost: ModelCost {
                rates: ModelCostRates {
                    input: 5.0,
                    output: 25.0,
                    cache_read: 0.5,
                    cache_write: 6.25,
                },
                tiers: None,
            },
            context_window: 1_000_000,
            max_tokens: 128_000,
            headers: None,
            compat: compat_object([
                ("forceAdaptiveThinking", Value::Bool(true)),
                ("supportsTemperature", Value::Bool(false)),
            ]),
        },
        Model {
            id: "claude-opus-4-8".to_string(),
            name: "Claude Opus 4.8".to_string(),
            api: Api::from("anthropic-messages"),
            provider: ProviderId::from("anthropic"),
            base_url: "https://api.anthropic.com".to_string(),
            reasoning: true,
            thinking_level_map: Some(ThinkingLevelMap {
                off: None,
                minimal: None,
                low: None,
                medium: None,
                high: None,
                xhigh: Some(Some("xhigh".to_string())),
                max: Some(Some("max".to_string())),
            }),
            input: vec![Modality::Text, Modality::Image],
            cost: ModelCost {
                rates: ModelCostRates {
                    input: 5.0,
                    output: 25.0,
                    cache_read: 0.5,
                    cache_write: 6.25,
                },
                tiers: None,
            },
            context_window: 1_000_000,
            max_tokens: 128_000,
            headers: None,
            compat: compat_object([
                ("forceAdaptiveThinking", Value::Bool(true)),
                ("supportsTemperature", Value::Bool(false)),
            ]),
        },
        Model {
            id: "claude-sonnet-4-5".to_string(),
            name: "Claude Sonnet 4.5 (latest)".to_string(),
            api: Api::from("anthropic-messages"),
            provider: ProviderId::from("anthropic"),
            base_url: "https://api.anthropic.com".to_string(),
            reasoning: true,
            thinking_level_map: None,
            input: vec![Modality::Text, Modality::Image],
            cost: ModelCost {
                rates: ModelCostRates {
                    input: 3.0,
                    output: 15.0,
                    cache_read: 0.3,
                    cache_write: 3.75,
                },
                tiers: None,
            },
            context_window: 1_000_000,
            max_tokens: 64_000,
            headers: None,
            compat: None,
        },
        Model {
            id: "claude-sonnet-4-5-20250929".to_string(),
            name: "Claude Sonnet 4.5".to_string(),
            api: Api::from("anthropic-messages"),
            provider: ProviderId::from("anthropic"),
            base_url: "https://api.anthropic.com".to_string(),
            reasoning: true,
            thinking_level_map: None,
            input: vec![Modality::Text, Modality::Image],
            cost: ModelCost {
                rates: ModelCostRates {
                    input: 3.0,
                    output: 15.0,
                    cache_read: 0.3,
                    cache_write: 3.75,
                },
                tiers: None,
            },
            context_window: 1_000_000,
            max_tokens: 64_000,
            headers: None,
            compat: None,
        },
        Model {
            id: "claude-sonnet-4-6".to_string(),
            name: "Claude Sonnet 4.6".to_string(),
            api: Api::from("anthropic-messages"),
            provider: ProviderId::from("anthropic"),
            base_url: "https://api.anthropic.com".to_string(),
            reasoning: true,
            thinking_level_map: Some(ThinkingLevelMap {
                off: None,
                minimal: None,
                low: None,
                medium: None,
                high: None,
                xhigh: None,
                max: Some(Some("max".to_string())),
            }),
            input: vec![Modality::Text, Modality::Image],
            cost: ModelCost {
                rates: ModelCostRates {
                    input: 3.0,
                    output: 15.0,
                    cache_read: 0.3,
                    cache_write: 3.75,
                },
                tiers: None,
            },
            context_window: 1_000_000,
            max_tokens: 128_000,
            headers: None,
            compat: compat_object([("forceAdaptiveThinking", Value::Bool(true))]),
        },
        Model {
            id: "claude-sonnet-5".to_string(),
            name: "Claude Sonnet 5".to_string(),
            api: Api::from("anthropic-messages"),
            provider: ProviderId::from("anthropic"),
            base_url: "https://api.anthropic.com".to_string(),
            reasoning: true,
            thinking_level_map: Some(ThinkingLevelMap {
                off: None,
                minimal: None,
                low: None,
                medium: None,
                high: None,
                xhigh: Some(Some("xhigh".to_string())),
                max: Some(Some("max".to_string())),
            }),
            input: vec![Modality::Text, Modality::Image],
            cost: ModelCost {
                rates: ModelCostRates {
                    input: 2.0,
                    output: 10.0,
                    cache_read: 0.2,
                    cache_write: 2.5,
                },
                tiers: None,
            },
            context_window: 1_000_000,
            max_tokens: 128_000,
            headers: None,
            compat: compat_object([("forceAdaptiveThinking", Value::Bool(true))]),
        },
    ]
}

/// `Model::compat` from an ordered key/value list.
fn compat_object<const N: usize>(entries: [(&str, Value); N]) -> Option<Value> {
    Some(Value::Object(object(entries)))
}

/// A `serde_json` object from an ordered key/value list. `Map` is insertion-ordered
/// here (`preserve_order`), so the argument order is the record's key order.
fn object<const N: usize>(entries: [(&str, Value); N]) -> Map<String, Value> {
    entries
        .into_iter()
        .map(|(key, value)| (key.to_string(), value))
        .collect()
}
