//! Port of `core/provider-attribution.ts` — provider-specific attribution headers
//! merged into an outbound request.
//!
//! `isInstallTelemetryEnabled` (`core/telemetry.ts`, 14 lines) is folded in here as a
//! private helper rather than its own module: this is its only in-scope consumer
//! (the other call site, `interactive-mode.ts`, is feat-006/007).

use pirust_ai::types::model::Model;

use crate::settings::SettingsManager;

const OPENROUTER_HOST: &str = "openrouter.ai";
const NVIDIA_NIM_HOST: &str = "integrate.api.nvidia.com";
const CLOUDFLARE_API_HOST: &str = "api.cloudflare.com";
const CLOUDFLARE_AI_GATEWAY_HOST: &str = "gateway.ai.cloudflare.com";
const OPENCODE_HOST: &str = "opencode.ai";

/// `isTruthyEnvFlag` (`telemetry.ts:3-6`).
fn is_truthy_env_flag(value: Option<&str>) -> bool {
    match value {
        None => false,
        Some(v) => v == "1" || v.to_lowercase() == "true" || v.to_lowercase() == "yes",
    }
}

/// `isInstallTelemetryEnabled(settingsManager, telemetryEnv = process.env.PI_TELEMETRY)`
/// (`telemetry.ts:8-13`). **Intentionally diverges from Pi**: the env var is
/// `PIRUST_TELEMETRY`, following the project's `PIRUST_*` naming convention.
pub fn is_install_telemetry_enabled(settings: &SettingsManager) -> bool {
    match std::env::var("PIRUST_TELEMETRY") {
        Ok(v) => is_truthy_env_flag(Some(&v)),
        Err(_) => settings.get_enable_install_telemetry(),
    }
}

fn matches_host(base_url: &str, expected_host: &str) -> bool {
    url::Url::parse(base_url)
        .map(|u| u.host_str() == Some(expected_host))
        .unwrap_or(false)
}

fn is_openrouter_model(model: &Model) -> bool {
    model.provider.0 == "openrouter" || model.base_url.contains(OPENROUTER_HOST)
}

fn is_nvidia_nim_model(model: &Model) -> bool {
    model.provider.0 == "nvidia" || matches_host(&model.base_url, NVIDIA_NIM_HOST)
}

fn is_cloudflare_model(model: &Model) -> bool {
    model.provider.0 == "cloudflare-workers-ai"
        || model.provider.0 == "cloudflare-ai-gateway"
        || matches_host(&model.base_url, CLOUDFLARE_API_HOST)
        || matches_host(&model.base_url, CLOUDFLARE_AI_GATEWAY_HOST)
}

fn get_default_attribution_headers(
    model: &Model,
    settings: &SettingsManager,
) -> Option<Vec<(String, String)>> {
    if !is_install_telemetry_enabled(settings) {
        return None;
    }
    if is_openrouter_model(model) {
        return Some(vec![
            ("HTTP-Referer".to_string(), "https://pi.dev".to_string()),
            ("X-OpenRouter-Title".to_string(), "pi".to_string()),
            (
                "X-OpenRouter-Categories".to_string(),
                "cli-agent".to_string(),
            ),
        ]);
    }
    if is_nvidia_nim_model(model) {
        return Some(vec![(
            "X-BILLING-INVOKE-ORIGIN".to_string(),
            "Pi".to_string(),
        )]);
    }
    if is_cloudflare_model(model) {
        return Some(vec![(
            "User-Agent".to_string(),
            "pi-coding-agent".to_string(),
        )]);
    }
    None
}

fn get_session_headers(model: &Model, session_id: Option<&str>) -> Option<Vec<(String, String)>> {
    let session_id = session_id?;
    if model.provider.0 != "opencode"
        && model.provider.0 != "opencode-go"
        && !matches_host(&model.base_url, OPENCODE_HOST)
    {
        return None;
    }
    Some(vec![
        ("x-opencode-session".to_string(), session_id.to_string()),
        ("x-opencode-client".to_string(), "pi".to_string()),
    ])
}

/// Insertion-ordered `Object.assign(merged, headers)`: an existing key's value is
/// overwritten IN PLACE (position preserved); a new key is appended.
fn assign(merged: &mut Vec<(String, String)>, headers: Vec<(String, String)>) {
    for (key, value) in headers {
        if let Some(entry) = merged.iter_mut().find(|(k, _)| *k == key) {
            entry.1 = value;
        } else {
            merged.push((key, value));
        }
    }
}

/// `mergeProviderAttributionHeaders(model, settingsManager, sessionId, ...headerSources)`
/// (`:79-97`). Returns `None` when the merged set is empty, matching
/// `Object.keys(merged).length > 0 ? merged : undefined`.
pub fn merge_provider_attribution_headers(
    model: &Model,
    settings: &SettingsManager,
    session_id: Option<&str>,
    header_sources: &[Option<Vec<(String, String)>>],
) -> Option<Vec<(String, String)>> {
    let mut merged = Vec::new();
    if let Some(session_headers) = get_session_headers(model, session_id) {
        assign(&mut merged, session_headers);
    }
    if let Some(default_headers) = get_default_attribution_headers(model, settings) {
        assign(&mut merged, default_headers);
    }
    for headers in header_sources.iter().flatten() {
        assign(&mut merged, headers.clone());
    }
    if merged.is_empty() {
        None
    } else {
        Some(merged)
    }
}
