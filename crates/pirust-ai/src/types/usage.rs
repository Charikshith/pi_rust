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
/// FIELD ORDER follows Pi's canonical runtime object for the token breakdown:
/// `input, output, cacheRead, cacheWrite, reasoning, totalTokens, cost, cacheWrite1h`.
/// This is pinned by the REAL openai-completions oracle (parseChunkUsage puts
/// `reasoning` between `cacheWrite` and `totalTokens`; verified byte-for-byte against Pi).
/// The anthropic adapter's `message_start`/`message_delta` case (where cacheWrite1h is
/// inserted after `cost` and reasoning at the very end) is byte-irrelevant today because
/// no current anthropic oracle emits either field; if that order ever matters it will be
/// pinned by a real fixture and this comment corrected then.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    /// Reasoning-token subset of `output` (openai `completion_tokens_details.reasoning_tokens`).
    /// Always present with `0` on the openai-completions path (TS `parseChunkUsage` `|| 0`);
    /// omitted elsewhere (Anthropic never emits it in current oracles).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<u64>,
    /// Runtime-optional: the abort-before-tokens path builds a `usage` without it, so
    /// it is omitted when absent (verified against real aborted-turn session fixtures)
    /// even though the TS interface declares it required.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u64>,
    pub cost: Cost,
    /// 1h-retention subset of `cache_write` (Anthropic only). Declared after `cost`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_write1h: Option<u64>,
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
