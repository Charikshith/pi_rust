//! Port of `packages/server/src/connection.ts`.
//!
//! **Wave 3 scope (superseded by Wave 4b, named below):** Wave 3 shipped
//! [`ConnectionState`] with only the fields that didn't require a concrete
//! async-runtime shape yet. Wave 4b (`server.rs`'s real `PiServer`) now adds
//! the remaining TS fields — `connection: ByteConnection`,
//! `handshake?: Promise<void>`, `handshakeTimeout: NodeJS.Timeout` — with
//! concrete `tokio` shapes: `connection` is `Arc<dyn ByteConnection>`;
//! `handshakeTimeout` is a `tokio::task::AbortHandle` (`abort()` ==
//! `clearTimeout()`); `handshake` becomes `handshake_done: Option<watch::
//! Receiver<bool>>` — TS stores the actual in-flight `Promise<void>` so a
//! queued message can `.then()` off it, but Rust's `.await` can't be stored
//! as a value the way a `Promise` reference can, so a `watch` channel that
//! `finishHandshake` flips to `true` on completion (success OR failure) is
//! the value-shaped equivalent a queued dispatch task can clone and await.
//!
//! **Locking (named, not silent):** [`ConnectionState`] is wrapped in a
//! plain `std::sync::Mutex` by callers (`server.rs`'s `SharedConnectionState`
//! = `Arc<std::sync::Mutex<ConnectionState>>`), not a `tokio::sync::Mutex`.
//! Every operation on it (decoder push, stage reads/writes) is synchronous
//! and fast, mirroring `server.ts`'s own synchronous `receive`/
//! `dispatchMessage` — and a sync `Mutex`'s guard cannot be held across an
//! `.await` at all (it isn't `Send`), which structurally *enforces* the
//! analysis doc's gotcha 7 ("re-validate connection state after every
//! await") rather than relying on manual discipline for it.

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::watch;
use tokio::task::AbortHandle;

use crate::protocol::codec::ClientMessageDecoder;

#[derive(Debug, Clone)]
pub struct ByteConnectionError(pub String);

impl std::fmt::Display for ByteConnectionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for ByteConnectionError {}

/// An established, authorized ordered byte connection.
#[async_trait]
pub trait ByteConnection: Send + Sync {
    fn closed(&self) -> bool;
    async fn send(&self, chunk: &[u8]) -> Result<(), ByteConnectionError>;
    async fn close(&self, final_chunk: Option<&[u8]>) -> Result<(), ByteConnectionError>;
}

pub trait ByteConnectionHandler: Send {
    fn on_data(&mut self, chunk: &[u8]);
    fn on_close(&mut self);
    fn on_error(&mut self, error: &ByteConnectionError);
}

pub type ByteConnectionAcceptor =
    Box<dyn Fn(Arc<dyn ByteConnection>) -> Box<dyn ByteConnectionHandler> + Send + Sync>;

/// `server.rs`'s per-connection handle. A plain `std::sync::Mutex` (see the
/// module doc) — never held across an `.await`.
pub type SharedConnectionState = Arc<std::sync::Mutex<ConnectionState>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionStage {
    AwaitingHello,
    Handshaking,
    Ready,
    Closing,
    Closed,
}

pub struct ConnectionState {
    pub id: String,
    pub connection: Arc<dyn ByteConnection>,
    pub decoder: ClientMessageDecoder,
    pub session_ids: HashSet<String>,
    pub stage: ConnectionStage,
    pub disconnected: bool,
    pub handshake_complete: bool,
    /// `Some` only while `stage == Handshaking`; a queued message dispatch
    /// clones this receiver and awaits it flipping to `true` (`finishHandshake`
    /// sets it right before returning, success or failure) instead of TS's
    /// `handshake.then(...)`.
    pub handshake_done: Option<watch::Receiver<bool>>,
    /// `Some` only before the handshake completes or times out; `abort()` is
    /// `clearTimeout(state.handshakeTimeout)`.
    pub handshake_timeout: Option<AbortHandle>,
}

impl ConnectionState {
    pub fn new(
        id: String,
        connection: Arc<dyn ByteConnection>,
        max_frame_length: Option<u64>,
    ) -> Self {
        Self {
            id,
            connection,
            decoder: ClientMessageDecoder::new(max_frame_length),
            session_ids: HashSet::new(),
            stage: ConnectionStage::AwaitingHello,
            disconnected: false,
            handshake_complete: false,
            handshake_done: None,
            handshake_timeout: None,
        }
    }
}

pub fn is_terminal_connection(state: &ConnectionState) -> bool {
    state.disconnected
        || matches!(
            state.stage,
            ConnectionStage::Closing | ConnectionStage::Closed
        )
}

#[cfg(test)]
pub(crate) struct NoopConnection;

#[cfg(test)]
#[async_trait]
impl ByteConnection for NoopConnection {
    fn closed(&self) -> bool {
        false
    }
    async fn send(&self, _chunk: &[u8]) -> Result<(), ByteConnectionError> {
        Ok(())
    }
    async fn close(&self, _final_chunk: Option<&[u8]>) -> Result<(), ByteConnectionError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_state(id: &str) -> ConnectionState {
        ConnectionState::new(id.to_string(), Arc::new(NoopConnection), None)
    }

    #[test]
    fn fresh_connection_is_not_terminal() {
        let state = test_state("connection-1");
        assert!(!is_terminal_connection(&state));
        assert_eq!(state.stage, ConnectionStage::AwaitingHello);
        assert!(!state.handshake_complete);
    }

    #[test]
    fn closing_or_closed_or_disconnected_is_terminal() {
        let mut state = test_state("connection-1");
        state.stage = ConnectionStage::Closing;
        assert!(is_terminal_connection(&state));

        let mut state = test_state("connection-1");
        state.stage = ConnectionStage::Closed;
        assert!(is_terminal_connection(&state));

        let mut state = test_state("connection-1");
        state.disconnected = true;
        assert!(is_terminal_connection(&state));
    }

    #[test]
    fn handshaking_and_ready_are_not_terminal() {
        let mut state = test_state("connection-1");
        state.stage = ConnectionStage::Handshaking;
        assert!(!is_terminal_connection(&state));
        state.stage = ConnectionStage::Ready;
        assert!(!is_terminal_connection(&state));
    }
}
