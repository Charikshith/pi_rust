//! Model metadata and cost tiers (TS `Model`, `ModelCost`, `ModelCostRates`,
//! `ModelCostTier`).
//!
//! The TS `Model<TApi>` uses conditional generics to bind `compat` to an api-specific
//! shape. Per the port plan (docs/analysis/00-overview.md §5) generics collapse to a
//! runtime `api: Api`. The typed `compat` unions (OpenAICompletionsCompat, etc.) are
//! provider-facing and land with the provider layer (feat-008); here `compat` is
//! carried as opaque JSON so a `Model` still round-trips losslessly.

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};

use super::ids::{Api, ProviderId};

/// Deserializer for `Option<Option<T>>` fields that must distinguish an absent key
/// (outer `None`, via `#[serde(default)]`) from an explicit JSON `null` (`Some(None)`).
/// Only invoked when the key is present, so any present value maps to `Some(_)`.
fn double_option<'de, T, D>(de: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: Deserializer<'de>,
{
    Deserialize::deserialize(de).map(Some)
}

/// Input/output modality (`("text" | "image")[]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Modality {
    Text,
    Image,
}

/// Per-category cost rates in USD per million tokens (TS `ModelCostRates`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCostRates {
    #[serde(serialize_with = "crate::jsnum::serialize_f64")]
    pub input: f64,
    #[serde(serialize_with = "crate::jsnum::serialize_f64")]
    pub output: f64,
    #[serde(serialize_with = "crate::jsnum::serialize_f64")]
    pub cache_read: f64,
    #[serde(serialize_with = "crate::jsnum::serialize_f64")]
    pub cache_write: f64,
}

/// A request-wide pricing tier (TS `ModelCostTier extends ModelCostRates`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCostTier {
    #[serde(flatten)]
    pub rates: ModelCostRates,
    /// This tier applies when total input usage exceeds this token count.
    pub input_tokens_above: u64,
}

/// Model pricing with optional tiers (TS `ModelCost extends ModelCostRates`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCost {
    #[serde(flatten)]
    pub rates: ModelCostRates,
    /// Highest matching input threshold applies to the full request.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tiers: Option<Vec<ModelCostTier>>,
}

/// Maps pi thinking levels to provider/model-specific values (TS `ThinkingLevelMap =
/// Partial<Record<ModelThinkingLevel, string | null>>`).
///
/// Absent field = key omitted; `Some(None)` = explicit JSON `null` (level
/// unsupported); `Some(Some(s))` = the provider-specific value.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThinkingLevelMap {
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub off: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub minimal: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub low: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub medium: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub high: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub xhigh: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub max: Option<Option<String>>,
}

impl ThinkingLevelMap {
    /// Look up the provider value for a thinking level (TS `map[level]`): the
    /// flattened value, or `None` when absent or explicitly `null`.
    pub fn get(&self, level: crate::types::ids::ThinkingLevel) -> Option<&str> {
        let v = match level {
            crate::types::ids::ThinkingLevel::Minimal => self.minimal.as_ref(),
            crate::types::ids::ThinkingLevel::Low => self.low.as_ref(),
            crate::types::ids::ThinkingLevel::Medium => self.medium.as_ref(),
            crate::types::ids::ThinkingLevel::High => self.high.as_ref(),
            crate::types::ids::ThinkingLevel::Xhigh => self.xhigh.as_ref(),
            crate::types::ids::ThinkingLevel::Max => self.max.as_ref(),
        };
        v.and_then(|inner| inner.as_deref())
    }

    /// The `off` value flattened (`map.off`): `None` when absent, `Some(None)`
    /// stays as `None`-meaning-unset; the double-option collapses to a plain
    /// `Option<&str>`.
    pub fn off_value(&self) -> Option<&str> {
        self.off.as_ref().and_then(|inner| inner.as_deref())
    }
}

/// A model in the unified model system (TS `Model<TApi>`). Field order mirrors the TS
/// interface so serialized key order matches.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Model {
    pub id: String,
    pub name: String,
    pub api: Api,
    pub provider: ProviderId,
    pub base_url: String,
    pub reasoning: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_level_map: Option<ThinkingLevelMap>,
    pub input: Vec<Modality>,
    pub cost: ModelCost,
    pub context_window: u64,
    pub max_tokens: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<Map<String, Value>>,
    /// Compatibility overrides (typed per-api in feat-008); opaque JSON for now.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compat: Option<Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cost_tier_flattens_rates() {
        let tier = ModelCostTier {
            rates: ModelCostRates {
                input: 1.0,
                output: 2.0,
                cache_read: 0.1,
                cache_write: 0.2,
            },
            input_tokens_above: 200_000,
        };
        let json = serde_json::to_string(&tier).unwrap();
        // whole-number rates serialize without a trailing ".0" (JS parity)
        assert_eq!(
            json,
            r#"{"input":1,"output":2,"cacheRead":0.1,"cacheWrite":0.2,"inputTokensAbove":200000}"#
        );
        assert_eq!(serde_json::from_str::<ModelCostTier>(&json).unwrap(), tier);
    }

    #[test]
    fn model_roundtrips_and_omits_optionals() {
        let model = Model {
            id: "claude-opus".into(),
            name: "Claude Opus".into(),
            api: Api::from("anthropic-messages"),
            provider: ProviderId::from("anthropic"),
            base_url: "https://api.anthropic.com".into(),
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
            max_tokens: 64_000,
            headers: None,
            compat: None,
        };
        let json = serde_json::to_string(&model).unwrap();
        assert!(json.starts_with(r#"{"id":"claude-opus","name":"Claude Opus","api":"anthropic-messages","provider":"anthropic","baseUrl":"https://api.anthropic.com","reasoning":true,"input":["text","image"]"#));
        assert!(!json.contains("thinkingLevelMap"));
        assert!(!json.contains("compat"));
        assert_eq!(serde_json::from_str::<Model>(&json).unwrap(), model);
    }

    #[test]
    fn thinking_level_map_null_vs_absent() {
        let map = ThinkingLevelMap {
            off: Some(None),                 // explicit null
            high: Some(Some("high".into())), // value
            ..Default::default()             // rest absent
        };
        let json = serde_json::to_string(&map).unwrap();
        assert_eq!(json, r#"{"off":null,"high":"high"}"#);
        assert_eq!(
            serde_json::from_str::<ThinkingLevelMap>(&json).unwrap(),
            map
        );
    }
}
