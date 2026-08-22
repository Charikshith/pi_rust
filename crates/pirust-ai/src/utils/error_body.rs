//! Shared normalization for provider HTTP error objects — Rust port of
//! `packages/ai/src/utils/error-body.ts`.
//!
//! `normalizeProviderError` probes the known SDK error field shapes (`statusCode`,
//! `status`, `$metadata.httpStatusCode`, `$response.statusCode` for status; `body`,
//! `error` parsed JSON body, `$response.body` for the body) and returns a struct each
//! provider composes into its display string. `formatProviderError` turns that struct
//! into the user-facing message, preserving the body when the SDK already folded it
//! into `message` (`messageCarriesBody`).

/// Cap on the body text surfaced in a provider error (TS `MAX_PROVIDER_ERROR_BODY_CHARS`).
pub const MAX_PROVIDER_ERROR_BODY_CHARS: usize = 4000;

/// Normalized provider error (TS `NormalizedProviderError`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedProviderError {
    /// HTTP status code, when one could be extracted.
    pub status: Option<u16>,
    /// Raw HTTP body reason, trimmed and truncated to the cap.
    pub body: Option<String>,
    /// `error.message`, or a JSON stringification of a non-`Error` throw.
    pub message: String,
    /// True when `message` already contains the body (no separate body to add).
    pub message_carries_body: bool,
}

/// Normalize an arbitrary thrown value into a [`NormalizedProviderError`].
///
/// The Rust `Result`/`TransportError` worlds collapse into this one shape: a
/// [`crate::http::TransportError::Status`] carries the status + body directly; a
/// plain string is the error message; anything else is JSON-stringified.
pub fn normalize_provider_error(
    status: Option<u16>,
    body: Option<String>,
    message: String,
) -> NormalizedProviderError {
    let body = body.map(|b| {
        let trimmed = b.trim().to_string();
        truncate_error_text(&trimmed, MAX_PROVIDER_ERROR_BODY_CHARS)
    });
    let message_carries_body = match &body {
        None => true,
        Some(b) => b.is_empty() || message.contains(b.as_str()),
    };
    NormalizedProviderError {
        status,
        body,
        message,
        message_carries_body,
    }
}

/// Compose a display string from a normalized error (TS `formatProviderError`).
///
/// - no prefix: `"<status>: <body>"`
/// - prefix:    `"<prefix> (<status>): <body>"`
///
/// When the message already carries the body, or no body/status was extracted, the
/// message is returned unchanged (optionally prefixed with the status).
pub fn format_provider_error(norm: &NormalizedProviderError, prefix: Option<&str>) -> String {
    if norm.message_carries_body || norm.status.is_none() || norm.body.is_none() {
        return match (prefix, norm.status) {
            (Some(p), Some(status)) => format!("{p} ({status}): {}", norm.message),
            _ => norm.message.clone(),
        };
    }
    match (prefix, norm.status) {
        (Some(p), Some(status)) => format!("{p} ({status}): {}", norm.body.as_deref().unwrap()),
        (_, Some(status)) => format!("{status}: {}", norm.body.as_deref().unwrap()),
        _ => norm.message.clone(),
    }
}

/// Truncate text to `max_chars` with a `... [truncated N chars]` suffix
/// (TS `truncateErrorText`). Char-based, matching JS `string.length` (UTF-16 code
/// units) for BMP text.
pub fn truncate_error_text(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let head: String = text.chars().take(max_chars).collect();
    let total = text.chars().count();
    format!("{head}... [truncated {} chars]", total - max_chars)
}

/// JSON-stringify a value for error messages (TS `safeJsonStringify`): `undefined`
/// and non-serializable values collapse to their string form.
pub fn safe_json_stringify(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => "null".to_string(),
        other => serde_json::to_string(other).unwrap_or_else(|_| other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_and_body_when_message_does_not_carry_body() {
        let norm = normalize_provider_error(
            Some(403),
            Some("forbidden: quota".into()),
            "Request failed".into(),
        );
        assert_eq!(format_provider_error(&norm, None), "403: forbidden: quota");
        assert_eq!(
            format_provider_error(&norm, Some("Azure OpenAI API error")),
            "Azure OpenAI API error (403): forbidden: quota"
        );
    }

    #[test]
    fn message_unchanged_when_it_carries_body() {
        let msg = "Error 429: rate limited: you exceeded your quota".to_string();
        let norm = normalize_provider_error(Some(429), Some("rate limited".into()), msg.clone());
        assert_eq!(format_provider_error(&norm, None), msg);
    }

    #[test]
    fn no_status_no_body_returns_message() {
        let norm = normalize_provider_error(None, None, "boom".into());
        assert_eq!(format_provider_error(&norm, Some("Pfx")), "boom");
    }

    #[test]
    fn truncation_adds_suffix() {
        let s = "x".repeat(100);
        let out = truncate_error_text(&s, 10);
        assert!(out.starts_with("xxxxxxxxxx"));
        assert!(out.ends_with("... [truncated 90 chars]"));
    }

    #[test]
    fn under_cap_is_untouched() {
        let s = "short".to_string();
        assert_eq!(truncate_error_text(&s, 10), s);
    }
}
