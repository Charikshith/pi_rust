//! Token usage and cost accounting (TS `Usage` and its nested `cost`).

use serde::{Deserialize, Serialize};

/// Per-category cost breakdown in USD (the `cost` object inside TS `Usage`).
/// Field order mirrors the TS object so serialized key order matches.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Cost {
    #[serde(serialize_with = "crate::jsnum::serialize_f64")]
    pub input: f64,
    #[serde(serialize_with = "crate::jsnum::serialize_f64")]
    pub output: f64,
    #[serde(serialize_with = "crate::jsnum::serialize_f64")]
    pub cache_read: f64,
    #[serde(serialize_with = "crate::jsnum::serialize_f64")]
    pub cache_write: f64,
    #[serde(serialize_with = "crate::jsnum::serialize_f64")]
    pub total: f64,
}

/// Token usage for an assistant turn (TS `Usage`).
///
/// `reasoning` is a subset of `output` (output already includes it). `cache_write1h`
/// is the 1h-retention subset of `cache_write`, reported only by Anthropic. Both are
/// left absent by providers that don't expose the breakdown.
///
/// FIELD ORDER matches the Anthropic adapter's *runtime insertion order*, not the TS
/// interface's declaration order (spec §4e, `docs/analysis/06-anthropic-runtime-spec.md`):
/// the adapter's initial `usage` literal is `input, output, cacheRead, cacheWrite,
/// totalTokens, cost`, then `cacheWrite1h` is inserted in `message_start` and `reasoning`
/// in `message_delta` — both AFTER `cost`. So `cache_write1h` and `reasoning` are declared
/// last. All current oracles leave both absent (`skip_serializing_if`), so this reorder is
/// byte-irrelevant today; it pins the order for when feat-002 emits them.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    /// Runtime-optional: the abort-before-tokens path builds a `usage` without it, so
    /// it is omitted when absent (verified against real aborted-turn session fixtures)
    /// even though the TS interface declares it required.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u64>,
    pub cost: Cost,
    /// 1h-retention subset of `cache_write` (Anthropic only). Inserted after `cost` in
    /// `message_start` (spec §4e) — declared after `cost` to match runtime key order.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_write1h: Option<u64>,
    /// Reasoning-token subset of `output`. Inserted after `cacheWrite1h` in `message_delta`
    /// (spec §4e) — declared last to match runtime key order.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_key_order_and_omission() {
        let usage = Usage {
            input: 10,
            output: 5,
            cache_read: 0,
            cache_write: 0,
            cache_write1h: None,
            reasoning: None,
            total_tokens: Some(15),
            cost: Cost {
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
                total: 0.0,
            },
        };
        let json = serde_json::to_string(&usage).unwrap();
        // JS-compatible: whole-number floats serialize without a decimal (0.0 -> "0").
        assert_eq!(
            json,
            r#"{"input":10,"output":5,"cacheRead":0,"cacheWrite":0,"totalTokens":15,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}}"#
        );
        let back: Usage = serde_json::from_str(&json).unwrap();
        assert_eq!(back, usage);
    }
}
