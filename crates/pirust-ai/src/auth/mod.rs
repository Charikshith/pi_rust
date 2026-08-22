//! API-key auth (single-provider path) — Rust port of the pieces of
//! `packages/ai/src/env-api-keys.ts` + `utils/provider-env.ts` actually consumed by the
//! anthropic-messages adapter.
//!
//! See `docs/analysis/06-anthropic-runtime-spec.md` §5. Env precedence for Anthropic is
//! `ANTHROPIC_OAUTH_TOKEN` then `ANTHROPIC_API_KEY` (`getApiKeyEnvVars`,
//! `env-api-keys.ts:64-111`), used only when no explicit `apiKey` is set (`withEnvApiKey`,
//! `compat.ts:222-230`). OAuth tokens are detected by the substring `sk-ant-oat`
//! (`isOAuthToken`, `anthropic-messages.ts:828-830`).
//!
//! ## Header mapping (used later by the adapter, not here)
//! `resolve_api_key` returns the raw key; the adapter selects the header via
//! [`is_oauth_token`]:
//! - OAuth token (`is_oauth_token` true)  → `Authorization: Bearer <token>`
//! - plain API key (`is_oauth_token` false) → `X-Api-Key: <token>`
//!
//! ## Divergences from Pi (documented, intentional)
//! - **Bun `/proc/self/environ` fallback dropped.** Pi's `getProviderEnvValue`
//!   (`provider-env.ts:45-52`) is `env[name] || process.env[name] || <bun-sandbox-fallback>`;
//!   the Bun compiled-binary sandbox workaround (oven-sh/bun#27802) is out of scope for this
//!   port. Here the lookup is `provider_env.get(name)` then `std::env::var(name)`.
//! - **Single provider only.** The `github-copilot` (`COPILOT_GITHUB_TOKEN`) and the wider
//!   `envMap` providers, plus the Vertex ADC / Bedrock ambient-credential probes, are not
//!   ported. Anthropic is the only provider on this path.
//! - The higher-level credential-store / oauth-refresh resolver (`auth/resolve.ts:37-139`) is
//!   out of scope for the minimal port.

use std::collections::BTreeMap;

/// Env var checked first for the Anthropic provider (`env-api-keys.ts:71`).
pub const ANTHROPIC_OAUTH_TOKEN_ENV: &str = "ANTHROPIC_OAUTH_TOKEN";
/// Env var checked second for the Anthropic provider (`env-api-keys.ts:71`).
pub const ANTHROPIC_API_KEY_ENV: &str = "ANTHROPIC_API_KEY";
/// Substring identifying an OAuth token (`isOAuthToken`, `anthropic-messages.ts:828-830`).
pub const OAUTH_TOKEN_MARKER: &str = "sk-ant-oat";

/// Anthropic env-var precedence: `ANTHROPIC_OAUTH_TOKEN` then `ANTHROPIC_API_KEY`
/// (`getApiKeyEnvVars`, `env-api-keys.ts:70-72`). Exposed so the adapter can report which
/// vars are consulted without duplicating the order.
pub const ANTHROPIC_API_KEY_ENV_PRECEDENCE: [&str; 2] =
    [ANTHROPIC_OAUTH_TOKEN_ENV, ANTHROPIC_API_KEY_ENV];

/// Resolve the API key to use.
///
/// An explicit `options.apiKey` wins; otherwise fall back to the Anthropic env precedence
/// `ANTHROPIC_OAUTH_TOKEN` then `ANTHROPIC_API_KEY` (spec §5).
/// `provider_env` is the caller-scoped override map (`options.env`); for each var name in
/// precedence order the lookup is `provider_env.get(name)` then `std::env::var(name)` — a
/// direct port of `getProviderEnvValue` minus the Bun sandbox fallback (see module docs).
/// The first name that resolves to a value wins.
pub fn resolve_api_key(
    explicit: Option<&str>,
    provider_env: &BTreeMap<String, String>,
) -> Option<String> {
    if let Some(key) = explicit {
        return Some(key.to_string());
    }

    ANTHROPIC_API_KEY_ENV_PRECEDENCE.iter().find_map(|name| {
        provider_env
            .get(*name)
            .cloned()
            .or_else(|| std::env::var(name).ok())
    })
}

/// Whether `key` is an OAuth token (substring `sk-ant-oat`) → `Authorization: Bearer`, else
/// `X-Api-Key` (TS `isOAuthToken`, `anthropic-messages.ts:828-830`).
pub fn is_oauth_token(key: &str) -> bool {
    key.contains(OAUTH_TOKEN_MARKER)
}

/// `getProviderEnvValue(name, env)` — `env[name]` then the process env (TS
/// `provider-env.ts:45-52` minus the Bun sandbox fallback).
pub fn get_provider_env_value(
    name: &str,
    env: Option<&std::collections::HashMap<String, String>>,
) -> Option<String> {
    env.and_then(|e| e.get(name))
        .cloned()
        .or_else(|| std::env::var(name).ok())
}

/// The provider → env-var map (TS `getApiKeyEnvVars`'s `envMap`,
/// `env-api-keys.ts:79-110`), minus the OAuth-only and ambient-credential providers.
/// `None` means the provider has no simple env-var key (OAuth / ADC / AWS only).
pub fn api_key_env_var(provider: &str) -> Option<&'static str> {
    Some(match provider {
        "github-copilot" => "COPILOT_GITHUB_TOKEN",
        "ant-ling" => "ANT_LING_API_KEY",
        "qwen-token-plan" | "qwen-token-plan-individual" => "QWEN_TOKEN_PLAN_API_KEY",
        "qwen-token-plan-cn" => "QWEN_TOKEN_PLAN_CN_API_KEY",
        "openai" => "OPENAI_API_KEY",
        "azure-openai-responses" => "AZURE_OPENAI_API_KEY",
        "nvidia" => "NVIDIA_API_KEY",
        "deepseek" => "DEEPSEEK_API_KEY",
        "google" => "GEMINI_API_KEY",
        "google-vertex" => "GOOGLE_CLOUD_API_KEY",
        "groq" => "GROQ_API_KEY",
        "cerebras" => "CEREBRAS_API_KEY",
        "xai" => "XAI_API_KEY",
        "radius" => "RADIUS_API_KEY",
        "openrouter" => "OPENROUTER_API_KEY",
        "vercel-ai-gateway" => "AI_GATEWAY_API_KEY",
        "zai" => "ZAI_API_KEY",
        "zai-coding-cn" => "ZAI_CODING_CN_API_KEY",
        "mistral" => "MISTRAL_API_KEY",
        "minimax" => "MINIMAX_API_KEY",
        "minimax-cn" => "MINIMAX_CN_API_KEY",
        "moonshotai" | "moonshotai-cn" => "MOONSHOT_API_KEY",
        "huggingface" => "HF_TOKEN",
        "fireworks" => "FIREWORKS_API_KEY",
        "together" => "TOGETHER_API_KEY",
        "baseten" => "BASETEN_API_KEY",
        "opencode" | "opencode-go" => "OPENCODE_API_KEY",
        "kimi-coding" => "KIMI_API_KEY",
        "cloudflare-workers-ai" | "cloudflare-ai-gateway" => "CLOUDFLARE_API_KEY",
        "xiaomi" => "XIAOMI_API_KEY",
        "xiaomi-token-plan-cn" => "XIAOMI_TOKEN_PLAN_CN_API_KEY",
        "xiaomi-token-plan-ams" => "XIAOMI_TOKEN_PLAN_AMS_API_KEY",
        "xiaomi-token-plan-sgp" => "XIAOMI_TOKEN_PLAN_SGP_API_KEY",
        _ => return None,
    })
}

/// `getEnvApiKey(provider, env)` (`env-api-keys.ts:146-149`): the provider's env-var key
/// from `env` then the process env. Skips the anthropic auth-token and the vertex/bedrock
/// ambient-credential branches (OAuth-only / not ported).
pub fn resolve_env_api_key(provider: &str, env: &BTreeMap<String, String>) -> Option<String> {
    if provider == "anthropic" {
        return ANTHROPIC_API_KEY_ENV_PRECEDENCE
            .iter()
            .find(|name| env.contains_key(**name))
            .and_then(|name| env.get(*name))
            .cloned()
            .or_else(|| {
                ANTHROPIC_API_KEY_ENV_PRECEDENCE
                    .iter()
                    .find(|name| std::env::var(name).ok().is_some_and(|v| !v.is_empty()))
                    .and_then(|name| std::env::var(name).ok())
            });
    }
    let name = api_key_env_var(provider)?;
    env.get(name).cloned().or_else(|| std::env::var(name).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn explicit_key_wins_over_env() {
        let e = env(&[(ANTHROPIC_API_KEY_ENV, "sk-ant-api03-from-env")]);
        assert_eq!(
            resolve_api_key(Some("sk-ant-explicit"), &e),
            Some("sk-ant-explicit".to_string())
        );
    }

    #[test]
    fn oauth_token_takes_precedence_over_api_key() {
        let e = env(&[
            (ANTHROPIC_API_KEY_ENV, "sk-ant-api03-xxx"),
            (ANTHROPIC_OAUTH_TOKEN_ENV, "sk-ant-oat01-xxx"),
        ]);
        assert_eq!(
            resolve_api_key(None, &e),
            Some("sk-ant-oat01-xxx".to_string())
        );
    }

    #[test]
    fn api_key_used_when_only_api_key_present() {
        let e = env(&[(ANTHROPIC_API_KEY_ENV, "sk-ant-api03-xxx")]);
        assert_eq!(
            resolve_api_key(None, &e),
            Some("sk-ant-api03-xxx".to_string())
        );
    }

    #[test]
    fn falls_through_to_process_env_when_absent_from_map() {
        // Exercises the real `std::env::var` fallback: the map is empty, so resolution must
        // read the precedence var from process env. This is the ONLY test that touches the
        // process-wide anthropic env vars (all others short-circuit via the map or the
        // explicit key), so it cannot race other tests. Set → assert → remove, in-body.
        let empty = env(&[]);
        std::env::set_var(ANTHROPIC_API_KEY_ENV, "sk-ant-api03-process-env");
        let got = resolve_api_key(None, &empty);
        std::env::remove_var(ANTHROPIC_API_KEY_ENV);
        assert_eq!(got, Some("sk-ant-api03-process-env".to_string()));
    }

    #[test]
    fn is_oauth_token_detects_oat_marker() {
        assert!(is_oauth_token("sk-ant-oat01-xxx"));
        assert!(!is_oauth_token("sk-ant-api03-xxx"));
    }
}
