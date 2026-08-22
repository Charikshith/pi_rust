//! Retry policy for provider HTTP requests — Rust port of
//! `packages/ai/src/utils/provider-retry.ts`.
//!
//! Mirrors the pinned OpenAI/Anthropic SDK retry policy (status 408/409/429/5xx,
//! honoring `x-should-retry`, `retry-after-ms`, and `retry-after`), with the backoff
//! sleep made abortable via a [`CancellationToken`] (TS `AbortSignal`).

use std::collections::HashMap;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

/// Default cap on provider-requested retry delays (TS `DEFAULT_MAX_RETRY_DELAY_MS`).
pub const DEFAULT_MAX_RETRY_DELAY_MS: u64 = 60_000;

/// A provider error with retry metadata (TS `ProviderError`).
#[derive(Debug, Clone)]
pub struct ProviderError {
    /// HTTP status (`undefined` in TS → `None` here).
    pub status: Option<u16>,
    /// Response headers (lower-cased keys).
    pub headers: HashMap<String, String>,
    /// Error message.
    pub message: String,
}

impl ProviderError {
    /// From a [`crate::http::TransportError::Status`] (status + body + headers).
    pub fn from_status(status: u16, headers: HashMap<String, String>, body: String) -> Self {
        Self {
            status: Some(status),
            headers,
            message: format!("HTTP {status}: {body}"),
        }
    }

    /// From a generic transport failure (no status, no headers).
    pub fn from_request(message: String) -> Self {
        Self {
            status: None,
            headers: HashMap::new(),
            message,
        }
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(String::as_str)
    }
}

/// Whether an error is retryable (TS `isRetryableProviderError`).
pub fn is_retryable_provider_error(error: &ProviderError) -> bool {
    match error.header("x-should-retry") {
        Some("true") => return true,
        Some("false") => return false,
        _ => {}
    }
    match error.status {
        None => true,
        Some(408) | Some(409) | Some(429) => true,
        Some(status) => status >= 500,
    }
}

/// Validate a server-requested retry delay against the cap (TS
/// `validateServerRetryDelayMs`). Returns `Err` when it exceeds `maxRetryDelayMs`.
fn validate_server_retry_delay_ms(
    delay_ms: f64,
    max_retry_delay_ms: Option<u64>,
    provider_error_message: &str,
) -> Result<f64, String> {
    let max_delay_ms = max_retry_delay_ms.unwrap_or(DEFAULT_MAX_RETRY_DELAY_MS);
    if max_delay_ms > 0 && delay_ms > max_delay_ms as f64 {
        return Err(format!(
            "Server requested {}s retry delay (max: {}s). {}",
            (delay_ms / 1000.0).ceil(),
            (max_delay_ms as f64 / 1000.0).ceil(),
            provider_error_message
        ));
    }
    Ok(delay_ms)
}

/// Compute the retry delay (TS `getRetryDelayMs`). Honours `retry-after-ms`,
/// `retry-after` (seconds), then exponential backoff with jitter.
fn get_retry_delay_ms(
    error: &ProviderError,
    retry_index: u32,
    max_retry_delay_ms: Option<u64>,
) -> Result<f64, String> {
    if let Some(retry_after_ms) = error.header("retry-after-ms") {
        if let Ok(value) = retry_after_ms.parse::<f64>() {
            return validate_server_retry_delay_ms(value, max_retry_delay_ms, &error.message);
        }
    }
    if let Some(retry_after) = error.header("retry-after") {
        // Seconds (an HTTP-date string is not parsed, matching the practical use of the
        // `Date.parse(retryAfter) - Date.now()` branch which yields NaN → exponential).
        if let Ok(seconds) = retry_after.parse::<f64>() {
            return validate_server_retry_delay_ms(
                seconds * 1000.0,
                max_retry_delay_ms,
                &error.message,
            );
        }
    }
    let exponential = (0.5 * 2f64.powi(retry_index as i32)).min(8.0) * 1000.0;
    // `exponentialDelay * (1 - Math.random() * 0.25)`. A deterministic fast-RNG keeps
    // tests reproducible while preserving the jitter range.
    let jitter = exponential * (1.0 - deterministic_unit_fraction() * 0.25);
    Ok(jitter)
}

/// A tiny deterministic fraction in `[0,1)` for retry jitter (JS `Math.random()`).
/// Not cryptographically secure — it only shapes backoff spread.
fn deterministic_unit_fraction() -> f64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    ((n.wrapping_mul(1_103_515_245) + 12_345) & 0x7fff_ffff) as f64 / 0x8000_0000u64 as f64
}

/// Abortable sleep (TS `abortableSleep`). Rejects when the token is cancelled.
async fn abortable_sleep(duration: Duration, token: &CancellationToken) -> Result<(), ()> {
    tokio::select! {
        _ = token.cancelled() => Err(()),
        _ = tokio::time::sleep(duration) => Ok(()),
    }
}

/// Reproduce the SDK retry behavior with an abortable backoff (TS
/// `retryProviderRequest`). Each retry invokes `request()` afresh (so
/// `X-Stainless-Retry-Count` stays zero). Returns the request's `Ok` value, or the
/// last error once retries are exhausted / the error is non-retryable / aborted.
pub async fn retry_provider_request<T, E, Fut>(
    request: impl Fn() -> Fut,
    max_retries: Option<u32>,
    max_retry_delay_ms: Option<u64>,
    token: &CancellationToken,
    classify: impl Fn(&E) -> ProviderError,
) -> Result<T, E>
where
    Fut: std::future::Future<Output = Result<T, E>>,
{
    let max_retries = max_retries.unwrap_or(0);
    let mut retries_remaining = max_retries;

    loop {
        match request().await {
            Ok(value) => return Ok(value),
            Err(error) => {
                if token.is_cancelled() {
                    return Err(error);
                }
                let provider_error = classify(&error);
                if retries_remaining == 0 || !is_retryable_provider_error(&provider_error) {
                    return Err(error);
                }
                let retry_index = max_retries - retries_remaining;
                retries_remaining -= 1;
                let delay =
                    match get_retry_delay_ms(&provider_error, retry_index, max_retry_delay_ms) {
                        Ok(d) => d,
                        Err(_) => return Err(error),
                    };
                if abortable_sleep(Duration::from_millis(delay.max(0.0) as u64), token)
                    .await
                    .is_err()
                {
                    return Err(error);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err(status: Option<u16>, headers: &[(&str, &str)]) -> ProviderError {
        ProviderError {
            status,
            headers: headers
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            message: "boom".into(),
        }
    }

    #[test]
    fn retryable_statuses_match_the_sdk_policy() {
        for status in [408, 409, 429, 500, 502, 503, 504, 524] {
            assert!(
                is_retryable_provider_error(&err(Some(status), &[])),
                "{status}"
            );
        }
        for status in [400, 401, 403, 404, 422] {
            assert!(
                !is_retryable_provider_error(&err(Some(status), &[])),
                "{status}"
            );
        }
        // no status → retryable (network failure)
        assert!(is_retryable_provider_error(&err(None, &[])));
    }

    #[test]
    fn x_should_retry_header_overrides_status() {
        assert!(!is_retryable_provider_error(&err(
            Some(500),
            &[("x-should-retry", "false")]
        )));
        assert!(is_retryable_provider_error(&err(
            Some(400),
            &[("x-should-retry", "true")]
        )));
    }

    #[test]
    fn retry_after_ms_is_parsed_and_capped() {
        let e = err(Some(429), &[("retry-after-ms", "250")]);
        let delay = get_retry_delay_ms(&e, 0, None).unwrap();
        assert_eq!(delay, 250.0);

        // above the cap → error naming both ceilings
        let e = err(Some(429), &[("retry-after-ms", "120000")]);
        let capped = get_retry_delay_ms(&e, 0, Some(10_000));
        assert!(capped.is_err());
        assert!(capped.unwrap_err().contains("retry delay"));
    }

    #[test]
    fn retry_after_seconds_is_multiplied() {
        let e = err(Some(429), &[("retry-after", "3")]);
        assert_eq!(get_retry_delay_ms(&e, 0, None).unwrap(), 3000.0);
    }
}
