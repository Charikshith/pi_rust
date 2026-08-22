//! Rust port of the deterministic helper functions from Pi's
//! `packages/ai/src/api/openai-completions.ts` — the pieces every
//! `openai-completions` provider (cerebras, deepseek, xai, groq, together,
//! openrouter, nvidia, zai, etc.) calls on every streamed chunk / tool call.
//!
//! This module currently ports only the pure, dependency-free helpers that the
//! full stream adapter will call in its event loop. Their outputs are pinned to
//! Pi's literal outputs via the oracle values captured by running real Pi
//! (see the `#[cfg(test)]` block, which asserts byte-for-byte against
//! `node` runs of the real adapter). The large `stream`/`streamSimple` event
//! loop, `buildParams`, and `convertMessages` remain a later feat-008 wave.
//!
//! Oracle source: `../pi/packages/ai/src/api/openai-completions.ts` and
//! `../pi/packages/ai/src/utils/hash.ts`.

use crate::api::anthropic_messages::calculate_cost;
use crate::types::ids::StopReason;
use crate::types::model::Model;
use crate::types::usage::Usage;

/// Fast deterministic hash to shorten long strings (TS `shortHash`,
/// `utils/hash.ts`). JS uses 32-bit wrapping `Math.imul` and signed `>>> 0`
/// normalization; Rust must use explicit `wrapping_` arithmetic to reproduce
/// the exact bit patterns that feed `toString(36)`.
pub fn short_hash(str: &str) -> String {
    let mut h1: u32 = 0xdead_beef;
    let mut h2: u32 = 0x41c6_ce57;
    for ch in str.encode_utf16() {
        let ch = ch as u32;
        h1 = imul(h1 ^ ch, 2654435761);
        h2 = imul(h2 ^ ch, 1597334677);
    }
    h1 = imul(h1 ^ (h1 >> 16), 2246822507) ^ imul(h2 ^ (h2 >> 13), 3266489909);
    h2 = imul(h2 ^ (h2 >> 16), 2246822507) ^ imul(h1 ^ (h1 >> 13), 3266489909);
    format!("{}{}", to_base36(h2), to_base36(h1))
}

/// `Math.imul(a, b)`: 32-bit wrapping multiplication.
fn imul(a: u32, b: u32) -> u32 {
    a.wrapping_mul(b)
}

/// JS `toString(36)`: base-36 of an unsigned 32-bit value using `0-9a-z`.
fn to_base36(mut value: u32) -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut out = Vec::new();
    loop {
        out.push(DIGITS[(value % 36) as usize]);
        value /= 36;
        if value == 0 {
            break;
        }
    }
    out.reverse();
    String::from_utf8(out).unwrap()
}

/// Map an OpenAI Chat Completions `finish_reason` to Pi's [`StopReason`]
/// (TS `mapStopReason`, `openai-completions.ts`). The error cases carry an
/// explanatory message pinned to Pi's exact wording.
pub fn map_stop_reason(reason: &str) -> (StopReason, Option<String>) {
    match reason {
        "stop" | "end" => (StopReason::Stop, None),
        "length" => (StopReason::Length, None),
        "function_call" | "tool_calls" => (StopReason::ToolUse, None),
        "content_filter" => (
            StopReason::Error,
            Some("Provider finish_reason: content_filter".to_string()),
        ),
        "network_error" => (
            StopReason::Error,
            Some("Provider finish_reason: network_error".to_string()),
        ),
        _ => (
            StopReason::Error,
            Some(format!("Provider finish_reason: {reason}")),
        ),
    }
}

/// Raw usage chunk as emitted by OpenAI-compatible endpoints (the TS inline
/// type in `parseChunkUsage`). The three cache placements are normalized into
/// `cache_read`; `prompt_tokens_details.cached_tokens` wins over
/// `prompt_cache_hit_tokens` and top-level `cached_tokens`.
#[derive(Debug, Clone, Copy, Default)]
pub struct RawChunkUsage {
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub cached_tokens: Option<u64>,
    pub prompt_cache_hit_tokens: Option<u64>,
    pub prompt_details_cached_tokens: Option<u64>,
    pub prompt_details_cache_write_tokens: Option<u64>,
    pub completion_details_reasoning_tokens: Option<u64>,
}

/// Point-in-time raw-usage → [`Usage`], matching TS `parseChunkUsage`
/// (`openai-completions.ts`). Normalizes the three cache-token placements
/// (`prompt_tokens_details.cached_tokens`, `prompt_cache_hit_tokens`, top-level
/// `cached_tokens`) into `cache_read`; computes the billable `input` as
/// `prompt_tokens - cache_read - cache_write` (a non-negative floor); then runs
/// Pi's cost model via [`calculate_cost`]. `reasoning` is the OpenAI
/// `completion_tokens_details.reasoning_tokens` subset of `output`.
pub fn parse_chunk_usage(raw: &RawChunkUsage, model: &Model) -> Usage {
    let prompt_tokens = raw.prompt_tokens.unwrap_or(0);
    let cache_read = raw
        .prompt_details_cached_tokens
        .or(raw.prompt_cache_hit_tokens)
        .or(raw.cached_tokens)
        .unwrap_or(0);
    let cache_write = raw.prompt_details_cache_write_tokens.unwrap_or(0);
    let input = prompt_tokens.saturating_sub(cache_read + cache_write);
    let output = raw.completion_tokens.unwrap_or(0);
    let reasoning = raw.completion_details_reasoning_tokens.unwrap_or(0);
    let mut usage = Usage {
        input,
        output,
        cache_read,
        cache_write,
        total_tokens: Some(input + output + cache_read + cache_write),
        cost: crate::types::usage::Cost {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
            total: 0.0,
        },
        cache_write1h: None,
        reasoning: if reasoning > 0 { Some(reasoning) } else { None },
    };
    calculate_cost(model, &mut usage);
    usage
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `shortHash` oracle values captured by running real Pi
    /// (`node scripts/...` driving `packages/ai/src/utils/hash.ts`).
    #[test]
    fn short_hash_matches_oracle() {
        let cases = [
            ("tool_call_abc", "1etmnurhqmven"),
            ("", "k4n83c7h0j2b"),
            (&format!("long-id-{}", "x".repeat(60)), "rvr9x64hrspo"),
            // Unicode exercises UTF-16 code-unit iteration (JS string indices
            // are UTF-16 code units, not bytes).
            ("hello wörld", "xruvbz94tpvu"),
            ("日本語テスト", "1urlhu61qrqskw"),
            (&"x".repeat(45), "v8b4f88ov0d9"),
        ];
        for (input, expected) in cases {
            assert_eq!(short_hash(input), expected, "shortHash({input:?})");
        }
    }

    #[test]
    fn map_stop_reason_covers_all_cases() {
        assert_eq!(map_stop_reason("stop"), (StopReason::Stop, None));
        assert_eq!(map_stop_reason("end"), (StopReason::Stop, None));
        assert_eq!(map_stop_reason("length"), (StopReason::Length, None));
        assert_eq!(
            map_stop_reason("function_call"),
            (StopReason::ToolUse, None)
        );
        assert_eq!(map_stop_reason("tool_calls"), (StopReason::ToolUse, None));
        assert_eq!(
            map_stop_reason("content_filter"),
            (
                StopReason::Error,
                Some("Provider finish_reason: content_filter".to_string()),
            )
        );
        assert_eq!(
            map_stop_reason("network_error"),
            (
                StopReason::Error,
                Some("Provider finish_reason: network_error".to_string()),
            )
        );
        assert_eq!(
            map_stop_reason("bogus"),
            (
                StopReason::Error,
                Some("Provider finish_reason: bogus".to_string()),
            )
        );
    }

    #[test]
    fn parse_chunk_usage_divides_cache_placements() {
        // DeepSeek reports cache hits in prompt_cache_hit_tokens; the
        // cache-written tokens reduce the billable input floor at zero.
        let model = crate::providers::faux::Faux::new().get_model().clone();
        let usage = parse_chunk_usage(
            &RawChunkUsage {
                prompt_tokens: Some(100),
                completion_tokens: Some(30),
                prompt_cache_hit_tokens: Some(60),
                prompt_details_cache_write_tokens: Some(5),
                completion_details_reasoning_tokens: Some(4),
                ..Default::default()
            },
            &model,
        );
        assert_eq!(usage.input, 35); // 100 - 60 - 5
        assert_eq!(usage.output, 30);
        assert_eq!(usage.cache_read, 60);
        assert_eq!(usage.cache_write, 5);
        assert_eq!(usage.reasoning, Some(4));
        assert_eq!(usage.total_tokens, Some(35 + 30 + 60 + 5));

        // prompt_tokens_details.cached_tokens wins over the others.
        let usage = parse_chunk_usage(
            &RawChunkUsage {
                prompt_tokens: Some(50),
                completion_tokens: Some(1),
                cached_tokens: Some(9),
                prompt_cache_hit_tokens: Some(8),
                prompt_details_cached_tokens: Some(7),
                ..Default::default()
            },
            &model,
        );
        assert_eq!(usage.cache_read, 7);
        assert_eq!(usage.input, 43);

        // Sub-cache-write robustness: no negative input.
        let usage = parse_chunk_usage(
            &RawChunkUsage {
                prompt_tokens: Some(5),
                prompt_details_cache_write_tokens: Some(20),
                ..Default::default()
            },
            &model,
        );
        assert_eq!(usage.input, 0);
    }
}
