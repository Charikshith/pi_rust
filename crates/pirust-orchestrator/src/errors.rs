//! Port of `packages/server/src/errors.ts`.
//!
//! **Named, not silent:** TS models `SessionBusyError`/`SessionLockedError`/
//! `SessionNotFoundError`/`NotImplementedError` as `PiServerError` SUBCLASSES
//! (each just sets `.name` and a default message) purely for diagnostic
//! `error.name`/stack-trace purposes — `sessions.ts`'s own real throw sites
//! mostly construct the BASE `PiServerError` directly with an explicit code
//! string anyway (e.g. `new PiServerError("session_locked", ...)`), and
//! `invalid_request` has no dedicated subclass at all. Rust has no
//! inheritance and no reader-visible `error.name`, so this port collapses
//! them to one [`PiServerError`] struct plus convenience constructors
//! (`PiServerError::busy`/`session_locked`/`not_found`/`not_implemented`)
//! that reproduce each subclass's default message — same information, no
//! redundant type hierarchy.

use crate::protocol::schemas::{ProtocolErrorCode, ProtocolJson};
use thiserror::Error;

pub const INTERNAL_SERVER_ERROR_MESSAGE: &str = "Internal server error";
pub const NOT_IMPLEMENTED_MESSAGE: &str = "Operation is not implemented";

/// `PiServerOperationErrorCode = Extract<ProtocolErrorCode, "busy" |
/// "session_locked" | "not_found" | "invalid_request" | "not_implemented">`
/// — `"version"`/`"internal_error"` are server-machinery-only and can never
/// be a service/runtime error's own code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PiServerOperationErrorCode {
    Busy,
    SessionLocked,
    NotFound,
    InvalidRequest,
    NotImplemented,
}

impl From<PiServerOperationErrorCode> for ProtocolErrorCode {
    fn from(code: PiServerOperationErrorCode) -> Self {
        match code {
            PiServerOperationErrorCode::Busy => ProtocolErrorCode::Busy,
            PiServerOperationErrorCode::SessionLocked => ProtocolErrorCode::SessionLocked,
            PiServerOperationErrorCode::NotFound => ProtocolErrorCode::NotFound,
            PiServerOperationErrorCode::InvalidRequest => ProtocolErrorCode::InvalidRequest,
            PiServerOperationErrorCode::NotImplemented => ProtocolErrorCode::NotImplemented,
        }
    }
}

/// A service/runtime error that can safely cross the protocol boundary.
#[derive(Debug, Clone, Error, PartialEq)]
#[error("{message}")]
pub struct PiServerError {
    pub code: PiServerOperationErrorCode,
    pub message: String,
    pub details: Option<ProtocolJson>,
}

impl PiServerError {
    pub fn new(
        code: PiServerOperationErrorCode,
        message: impl Into<String>,
        details: Option<ProtocolJson>,
    ) -> Self {
        Self {
            code,
            message: message.into(),
            details,
        }
    }

    /// `SessionBusyError` (default message `"Session is busy"`).
    pub fn busy(message: Option<String>, details: Option<ProtocolJson>) -> Self {
        Self::new(
            PiServerOperationErrorCode::Busy,
            message.unwrap_or_else(|| "Session is busy".to_string()),
            details,
        )
    }

    /// `SessionLockedError` (default message `"Session is locked"`).
    pub fn session_locked(message: Option<String>, details: Option<ProtocolJson>) -> Self {
        Self::new(
            PiServerOperationErrorCode::SessionLocked,
            message.unwrap_or_else(|| "Session is locked".to_string()),
            details,
        )
    }

    /// `SessionNotFoundError` (default message `"Session was not found"`).
    pub fn not_found(message: Option<String>, details: Option<ProtocolJson>) -> Self {
        Self::new(
            PiServerOperationErrorCode::NotFound,
            message.unwrap_or_else(|| "Session was not found".to_string()),
            details,
        )
    }

    /// `NotImplementedError` — always exactly [`NOT_IMPLEMENTED_MESSAGE`],
    /// no caller-supplied message (matches the TS constructor, which takes
    /// no arguments).
    pub fn not_implemented() -> Self {
        Self::new(
            PiServerOperationErrorCode::NotImplemented,
            NOT_IMPLEMENTED_MESSAGE,
            None,
        )
    }
}

/// An unsafe failure whose cause is retained for reporting (via
/// [`std::error::Error::source`]) but never serialized onto the wire — the
/// server's own `toProtocolError` always renders this as the opaque
/// [`INTERNAL_SERVER_ERROR_MESSAGE`], regardless of `cause`.
#[derive(Debug)]
pub struct InternalServerError {
    pub cause: anyhow::Error,
}

impl InternalServerError {
    pub fn new(cause: impl Into<anyhow::Error>) -> Self {
        Self {
            cause: cause.into(),
        }
    }
}

impl std::fmt::Display for InternalServerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{INTERNAL_SERVER_ERROR_MESSAGE}")
    }
}

impl std::error::Error for InternalServerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.cause.as_ref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn convenience_constructors_use_ts_default_messages() {
        assert_eq!(PiServerError::busy(None, None).message, "Session is busy");
        assert_eq!(
            PiServerError::session_locked(None, None).message,
            "Session is locked"
        );
        assert_eq!(
            PiServerError::not_found(None, None).message,
            "Session was not found"
        );
        assert_eq!(
            PiServerError::not_implemented().message,
            NOT_IMPLEMENTED_MESSAGE
        );
    }

    #[test]
    fn convenience_constructors_accept_overrides() {
        let error = PiServerError::busy(Some("custom".to_string()), None);
        assert_eq!(error.message, "custom");
        assert_eq!(error.code, PiServerOperationErrorCode::Busy);
    }

    #[test]
    fn operation_error_code_maps_onto_the_wire_error_code() {
        assert_eq!(
            ProtocolErrorCode::from(PiServerOperationErrorCode::Busy),
            ProtocolErrorCode::Busy
        );
        assert_eq!(
            ProtocolErrorCode::from(PiServerOperationErrorCode::NotImplemented),
            ProtocolErrorCode::NotImplemented
        );
    }

    #[test]
    fn internal_server_error_always_shows_the_opaque_message() {
        let error = InternalServerError::new(anyhow::anyhow!("some private detail"));
        assert_eq!(error.to_string(), INTERNAL_SERVER_ERROR_MESSAGE);
        assert!(std::error::Error::source(&error).is_some());
    }
}
