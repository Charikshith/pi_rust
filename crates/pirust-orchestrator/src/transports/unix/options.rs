//! Port of the pure, non-I/O pieces of `transports/unix/listener.ts`:
//! `validateUnixSocketPath`, `resolveUnixListenerOptions`,
//! `getOwnedBindPath`. See the module doc on `super` for why this file is
//! NOT `#[cfg(unix)]`-gated (unlike `listener.rs`) — none of it touches an
//! OS socket, so it compiles and unit-tests on any platform.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::protocol::framing::DEFAULT_MAX_FRAME_LENGTH;
use crate::server::ErrorHandler;

const DEFAULT_SOCKET_MODE: u32 = 0o600;
const DEFAULT_GRACEFUL_CLOSE_TIMEOUT_MS: u64 = 5_000;
const MAX_TIMER_DELAY_MS: u64 = 2_147_483_647;

/// `process.platform === "linux" ? 107 : 103` — a `cfg!` compile-time check
/// standing in for TS's runtime `process.platform` branch (analysis doc §7
/// gotcha 5: "port it as an actual `cfg!`/runtime check, not a single
/// constant").
pub const MAX_UNIX_SOCKET_PATH_BYTES: usize = if cfg!(target_os = "linux") { 107 } else { 103 };

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum UnixListenerConfigError {
    #[error("{0} must not be empty")]
    EmptyPath(String),
    #[error("{0} is too long; maximum is {MAX_UNIX_SOCKET_PATH_BYTES} UTF-8 bytes")]
    PathTooLong(String),
    #[error("PiServer Unix socket mode must be an integer between 0 and 0o777")]
    InvalidMode,
    #[error(
        "PiServer maxFrameLength must be an integer between 1 and {}",
        u32::MAX
    )]
    InvalidMaxFrameLength,
    #[error("PiServer maxPendingBytes must be a safe integer at least maxFrameLength + 4")]
    InvalidMaxPendingBytes,
    #[error(
        "PiServer gracefulCloseTimeoutMs must be an integer between 1 and {MAX_TIMER_DELAY_MS}"
    )]
    InvalidGracefulCloseTimeoutMs,
}

/// Port of `validateUnixSocketPath`. `path.len()` is a UTF-8 byte length in
/// Rust (same as `Buffer.byteLength(path)`), not a codepoint/char count.
pub fn validate_unix_socket_path(
    path: &str,
    description: &str,
) -> Result<(), UnixListenerConfigError> {
    if path.is_empty() {
        return Err(UnixListenerConfigError::EmptyPath(description.to_string()));
    }
    if path.len() > MAX_UNIX_SOCKET_PATH_BYTES {
        return Err(UnixListenerConfigError::PathTooLong(
            description.to_string(),
        ));
    }
    Ok(())
}

/// Input mirroring TS's `UnixListenerOptions` interface.
pub struct UnixListenerOptions {
    pub path: String,
    /// Socket filesystem permissions. Defaults to owner read/write only
    /// (0o600). Unlike TS's `number`, a Rust `u32` cannot be negative or
    /// non-integer, so `Number.isInteger(mode) && mode >= 0` is moot by
    /// construction (same simplification `server.rs`'s
    /// `PiServerConfigError` resolution already uses).
    pub mode: Option<u32>,
    pub max_pending_bytes: Option<u64>,
    pub graceful_close_timeout_ms: Option<u64>,
    pub max_frame_length: Option<u64>,
    pub on_error: Option<ErrorHandler>,
}

pub struct ResolvedUnixListenerOptions {
    pub path: String,
    pub mode: u32,
    pub graceful_close_timeout_ms: u64,
    pub max_pending_bytes: u64,
    pub on_error: Option<ErrorHandler>,
}

/// Port of `resolveUnixListenerOptions`.
pub fn resolve_unix_listener_options(
    options: UnixListenerOptions,
) -> Result<ResolvedUnixListenerOptions, UnixListenerConfigError> {
    validate_unix_socket_path(&options.path, "PiServer Unix socket path")?;
    let mode = options.mode.unwrap_or(DEFAULT_SOCKET_MODE);
    if mode > 0o777 {
        return Err(UnixListenerConfigError::InvalidMode);
    }
    let max_frame_length = options.max_frame_length.unwrap_or(DEFAULT_MAX_FRAME_LENGTH);
    if max_frame_length == 0 || max_frame_length > u32::MAX as u64 {
        return Err(UnixListenerConfigError::InvalidMaxFrameLength);
    }
    let max_pending_bytes = options
        .max_pending_bytes
        .unwrap_or(max_frame_length.saturating_mul(4));
    if max_pending_bytes < max_frame_length + 4 {
        return Err(UnixListenerConfigError::InvalidMaxPendingBytes);
    }
    let graceful_close_timeout_ms = options
        .graceful_close_timeout_ms
        .unwrap_or(DEFAULT_GRACEFUL_CLOSE_TIMEOUT_MS);
    if graceful_close_timeout_ms == 0 || graceful_close_timeout_ms > MAX_TIMER_DELAY_MS {
        return Err(UnixListenerConfigError::InvalidGracefulCloseTimeoutMs);
    }
    Ok(ResolvedUnixListenerOptions {
        path: options.path,
        mode,
        graceful_close_timeout_ms,
        max_pending_bytes,
        on_error: options.on_error,
    })
}

/// Port of `getOwnedBindPath`: `join(dirname(path), \`.p-${sha256(path)
/// .hex().slice(0,8)}\`)`.
///
/// **Residual (documented, not chased):** this uses `std::path::Path`'s own
/// join/parent semantics rather than a literal transcription of Node's
/// `path.dirname`/`path.join`. The two differ on some exotic inputs (e.g. a
/// bare relative filename with no directory separator at all), but this
/// value is never observed on the wire or persisted anywhere outside a
/// private, local, ephemeral bind path this process creates and deletes
/// itself — unlike the wire-format helpers elsewhere in this crate, there is
/// no byte-exact oracle contract to satisfy here, only "resolves to a
/// sibling path in the same directory," which both implementations satisfy
/// for every path this crate's own tests and callers construct (always an
/// absolute, directory-containing path).
pub fn get_owned_bind_path(path: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(path.as_bytes());
    let digest = hasher.finalize();
    let suffix: String = digest[..4].iter().map(|b| format!("{b:02x}")).collect();
    let dir = Path::new(path).parent().unwrap_or_else(|| Path::new(""));
    dir.join(format!(".p-{suffix}"))
        .to_string_lossy()
        .into_owned()
}

/// Sibling-path helper shared by `listener.rs`'s stale-socket removal and
/// cleanup-owned-socket dances (`.s-<uuid6>` / `.c-<uuid6>` prefixes).
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) fn sibling_path(path: &Path, prefix: &str) -> PathBuf {
    let dir = path.parent().unwrap_or_else(|| Path::new(""));
    let suffix = uuid::Uuid::new_v4().to_string();
    dir.join(format!("{prefix}{}", &suffix[..6]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_path() {
        assert_eq!(
            validate_unix_socket_path("", "d"),
            Err(UnixListenerConfigError::EmptyPath("d".to_string()))
        );
    }

    #[test]
    fn rejects_paths_over_the_platform_byte_limit() {
        let long = "a".repeat(MAX_UNIX_SOCKET_PATH_BYTES + 1);
        assert!(validate_unix_socket_path(&long, "d").is_err());
        let ok = "a".repeat(MAX_UNIX_SOCKET_PATH_BYTES);
        assert!(validate_unix_socket_path(&ok, "d").is_ok());
    }

    #[test]
    fn platform_limit_is_107_on_linux_else_103() {
        let expected = if cfg!(target_os = "linux") { 107 } else { 103 };
        assert_eq!(MAX_UNIX_SOCKET_PATH_BYTES, expected);
    }

    #[test]
    fn resolve_applies_defaults() {
        let resolved = resolve_unix_listener_options(UnixListenerOptions {
            path: "/tmp/s.sock".to_string(),
            mode: None,
            max_pending_bytes: None,
            graceful_close_timeout_ms: None,
            max_frame_length: None,
            on_error: None,
        })
        .unwrap();
        assert_eq!(resolved.mode, 0o600);
        assert_eq!(resolved.graceful_close_timeout_ms, 5_000);
        assert_eq!(resolved.max_pending_bytes, DEFAULT_MAX_FRAME_LENGTH * 4);
    }

    #[test]
    fn resolve_rejects_mode_above_0o777() {
        let err = resolve_unix_listener_options(UnixListenerOptions {
            path: "/tmp/s.sock".to_string(),
            mode: Some(0o1000),
            max_pending_bytes: None,
            graceful_close_timeout_ms: None,
            max_frame_length: None,
            on_error: None,
        })
        .err()
        .expect("mode above 0o777 must be rejected");
        assert_eq!(err, UnixListenerConfigError::InvalidMode);
    }

    #[test]
    fn resolve_rejects_max_pending_bytes_below_frame_length_plus_four() {
        let err = resolve_unix_listener_options(UnixListenerOptions {
            path: "/tmp/s.sock".to_string(),
            mode: None,
            max_pending_bytes: Some(10),
            graceful_close_timeout_ms: None,
            max_frame_length: Some(128),
            on_error: None,
        })
        .err()
        .expect("must be rejected");
        assert_eq!(err, UnixListenerConfigError::InvalidMaxPendingBytes);
    }

    #[test]
    fn resolve_rejects_zero_graceful_close_timeout() {
        let err = resolve_unix_listener_options(UnixListenerOptions {
            path: "/tmp/s.sock".to_string(),
            mode: None,
            max_pending_bytes: None,
            graceful_close_timeout_ms: Some(0),
            max_frame_length: None,
            on_error: None,
        })
        .err()
        .expect("must be rejected");
        assert_eq!(err, UnixListenerConfigError::InvalidGracefulCloseTimeoutMs);
    }

    #[test]
    fn owned_bind_path_is_a_deterministic_sibling_of_the_given_path() {
        let a = get_owned_bind_path("/tmp/pi/server.sock");
        let b = get_owned_bind_path("/tmp/pi/server.sock");
        assert_eq!(a, b, "same input must hash to the same owned path");
        assert!(a.starts_with("/tmp/pi") || a.contains(".p-"));
        assert_ne!(
            a,
            get_owned_bind_path("/tmp/pi/other.sock"),
            "different inputs must not collide"
        );
    }
}
