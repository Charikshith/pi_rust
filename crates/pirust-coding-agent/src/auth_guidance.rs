//! Port of `core/auth-guidance.ts` — user-facing "no model/no auth" guidance text.

use crate::config::get_docs_path;

const UNKNOWN_PROVIDER: &str = "unknown";

/// `getProviderLoginHelp()` (`:6-12`).
pub fn get_provider_login_help() -> String {
    let docs = get_docs_path();
    format!(
        "Use /login to log into a provider via OAuth or API key. See:\n  {}\n  {}",
        docs.join("providers.md").display(),
        docs.join("models.md").display(),
    )
}

/// `formatNoModelsAvailableMessage()` (`:14-16`).
pub fn format_no_models_available_message() -> String {
    format!("No models available. {}", get_provider_login_help())
}

/// `formatNoModelSelectedMessage()` (`:18-20`).
pub fn format_no_model_selected_message() -> String {
    format!(
        "No model selected.\n\n{}\n\nThen use /model to select a model.",
        get_provider_login_help()
    )
}

/// `formatNoApiKeyFoundMessage(provider)` (`:22-25`).
pub fn format_no_api_key_found_message(provider: &str) -> String {
    let provider_display = if provider == UNKNOWN_PROVIDER {
        "the selected model"
    } else {
        provider
    };
    format!(
        "No API key found for {provider_display}.\n\n{}",
        get_provider_login_help()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // `PIRUST_PACKAGE_DIR` is not overridden here: `std::env::set_var` is `unsafe` (and
    // this crate forbids unsafe code), and mutating process-global env state would race
    // other tests anyway. These assertions hold regardless of the docs path's value.

    #[test]
    fn no_models_available_wraps_the_login_help() {
        let msg = format_no_models_available_message();
        assert!(msg.starts_with("No models available. Use /login"));
        assert!(msg.contains("providers.md"));
    }

    #[test]
    fn no_api_key_found_maps_unknown_to_the_selected_model() {
        assert!(format_no_api_key_found_message("unknown")
            .starts_with("No API key found for the selected model."));
        assert!(format_no_api_key_found_message("anthropic")
            .starts_with("No API key found for anthropic."));
    }
}
