//! Port of `packages/server/src/server.ts` (`PiServer`) — the connection
//! state machine (`awaitingHello -> handshaking -> ready -> closing ->
//! closed`), hello-once/hello-first enforcement, and `finishHandshake`.
//!
//! **`to_protocol_error` is narrower than TS's `toProtocolError`** (named,
//! not silent): TS's version dispatches on `unknown` and has branches for
//! `InternalServerError`, `PiServerError`, `ProtocolValidationError`, and a
//! catch-all. This port only ever converts a [`PiServerError`] here — see
//! `sessions.rs`'s own module doc for why the other TS branches are
//! structurally unreachable from this call site in the Rust port (typed
//! service/runtime traits; framing errors already converted before reaching
//! here).
//!
//! **Locking discipline (analysis doc §4/§7 gotcha 7):** every per-connection
//! read/write goes through [`crate::connection::SharedConnectionState`]'s
//! plain `std::sync::Mutex`, whose guard cannot be held across an `.await` —
//! so re-validating connection state after every await point (the exact
//! discipline `server.ts` follows by hand throughout) is enforced by the
//! type system here, not just convention.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use tokio::sync::{watch, Mutex as AsyncMutex};

use crate::connection::{
    is_terminal_connection, ByteConnection, ByteConnectionHandler, ConnectionStage,
    ConnectionState, SharedConnectionState,
};
use crate::errors::{PiServerError, PiServerOperationErrorCode};
use crate::listener::PiServerListener;
use crate::protocol::codec::{encode_server_message, is_supported_protocol_version};
use crate::protocol::framing::DEFAULT_MAX_FRAME_LENGTH;
use crate::protocol::schemas::{
    ClientHello, ClientMessage, EventEnvelope, ProtocolError, ProtocolErrorCode, RequestEnvelope,
    ResponseEnvelope, ServerEvent, ServerHello, ServerHelloError, ServerMessage, PROTOCOL_VERSION,
};
use crate::sessions::LiveSessionManager;
use crate::snapshots::ServerSnapshotPublisher;
use crate::types::PiServerService;

const DEFAULT_HANDSHAKE_TIMEOUT_MS: u64 = 5_000;
const MAX_TIMER_DELAY_MS: u64 = 2_147_483_647;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PiServerConfigError {
    #[error("PiServer serverId must not be empty")]
    EmptyServerId,
    #[error(
        "PiServer maxFrameLength must be an integer between 1 and {}",
        u32::MAX
    )]
    InvalidMaxFrameLength,
    #[error("PiServer handshakeTimeoutMs must be an integer between 1 and {MAX_TIMER_DELAY_MS}")]
    InvalidHandshakeTimeoutMs,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PiServerStartError {
    #[error("PiServer is already started")]
    AlreadyStarted,
    #[error("PiServer is closing or closed")]
    Closing,
    #[error("a listener failed to start: {0}")]
    ListenerFailed(String),
}

pub type ErrorHandler = Box<dyn Fn(&anyhow::Error) + Send + Sync>;

pub struct PiServerOptions {
    pub listeners: Vec<Box<dyn PiServerListener>>,
    pub max_frame_length: Option<u64>,
    pub handshake_timeout_ms: Option<u64>,
    pub server_id: Option<String>,
    pub on_error: Option<ErrorHandler>,
}

struct ResolvedOptions {
    max_frame_length: u64,
    handshake_timeout_ms: u64,
}

fn resolve_options(options: &PiServerOptions) -> Result<ResolvedOptions, PiServerConfigError> {
    if let Some(id) = &options.server_id {
        if id.is_empty() {
            return Err(PiServerConfigError::EmptyServerId);
        }
    }
    let max_frame_length = options.max_frame_length.unwrap_or(DEFAULT_MAX_FRAME_LENGTH);
    if max_frame_length == 0 || max_frame_length > u32::MAX as u64 {
        return Err(PiServerConfigError::InvalidMaxFrameLength);
    }
    let handshake_timeout_ms = options
        .handshake_timeout_ms
        .unwrap_or(DEFAULT_HANDSHAKE_TIMEOUT_MS);
    if handshake_timeout_ms == 0 || handshake_timeout_ms > MAX_TIMER_DELAY_MS {
        return Err(PiServerConfigError::InvalidHandshakeTimeoutMs);
    }
    Ok(ResolvedOptions {
        max_frame_length,
        handshake_timeout_ms,
    })
}

/// Flips a `watch` channel to `true` on drop regardless of which return path
/// was taken — the value-shaped equivalent of TS's `handshake` promise
/// always settling once `finishHandshake` returns, used by a queued message
/// dispatch to know when it can stop waiting (see `connection.rs`'s module
/// doc for why a `watch` channel stands in for a stored `Promise`).
struct HandshakeDoneGuard(Option<watch::Sender<bool>>);

impl Drop for HandshakeDoneGuard {
    fn drop(&mut self) {
        if let Some(tx) = self.0.take() {
            let _ = tx.send(true);
        }
    }
}

/// The shared server state. `PiServer` is a thin `Arc<Inner>` wrapper;
/// `sessions.rs`/`snapshots.rs` hold a `Weak<Inner>` back-reference so they
/// can call these methods, mirroring TS's options-bag-of-closures pattern
/// (`(connection, message) => this.sendMessage(...)`) with plain method
/// calls instead.
pub(crate) struct Inner {
    pub(crate) id: String,
    pub(crate) service: Arc<dyn PiServerService>,
    max_frame_length: u64,
    handshake_timeout_ms: u64,
    on_error: Option<ErrorHandler>,
    pub(crate) connections: std::sync::Mutex<Vec<SharedConnectionState>>,
    pub(crate) sessions: LiveSessionManager,
    pub(crate) snapshots: ServerSnapshotPublisher,
    closing: AtomicBool,
    listeners: AsyncMutex<Vec<Box<dyn PiServerListener>>>,
    started: AtomicBool,
}

impl Inner {
    pub(crate) fn is_closing(&self) -> bool {
        self.closing.load(Ordering::SeqCst)
    }

    pub(crate) fn report_error(&self, error: anyhow::Error) {
        // "Error observers cannot affect server state" (server.ts's own
        // comment) — TS wraps the call in try/catch; this port does not
        // catch a panicking `on_error` callback (a named, minor
        // simplification: such a panic propagates/aborts here instead of
        // being swallowed).
        if let Some(f) = &self.on_error {
            f(&error);
        }
    }

    pub(crate) async fn send_message(
        self: &Arc<Self>,
        state: &SharedConnectionState,
        message: ServerMessage,
    ) -> bool {
        let (disconnected, closed) = {
            let g = state.lock().unwrap();
            (g.disconnected, g.connection.closed())
        };
        if disconnected || closed {
            return false;
        }
        let frame = match encode_server_message(&message, Some(self.max_frame_length)) {
            Ok(f) => f,
            Err(e) => {
                self.report_error(anyhow::anyhow!(e));
                self.close_connection(state, None).await;
                self.disconnect(state.clone()).await;
                return false;
            }
        };
        let conn = { state.lock().unwrap().connection.clone() };
        match conn.send(&frame).await {
            Ok(()) => true,
            Err(e) => {
                self.report_error(anyhow::anyhow!(e));
                self.close_connection(state, None).await;
                self.disconnect(state.clone()).await;
                false
            }
        }
    }

    pub(crate) async fn close_connection(
        &self,
        state: &SharedConnectionState,
        final_chunk: Option<&[u8]>,
    ) {
        let conn = { state.lock().unwrap().connection.clone() };
        if let Err(e) = conn.close(final_chunk).await {
            self.report_error(anyhow::anyhow!(e));
        }
    }

    pub(crate) async fn disconnect(self: &Arc<Self>, state: SharedConnectionState) {
        let (already, handshake_complete) = {
            let mut g = state.lock().unwrap();
            if g.disconnected {
                (true, false)
            } else {
                let hc = g.handshake_complete;
                g.disconnected = true;
                g.stage = ConnectionStage::Closed;
                if let Some(h) = g.handshake_timeout.take() {
                    h.abort();
                }
                (false, hc)
            }
        };
        if already {
            return;
        }
        {
            let mut conns = self.connections.lock().unwrap();
            conns.retain(|c| !Arc::ptr_eq(c, &state));
        }
        self.sessions.disconnect(state).await;
        if !self.is_closing() && handshake_complete {
            self.broadcast_server_snapshot();
        }
    }

    pub(crate) fn broadcast_server_snapshot(self: &Arc<Self>) {
        let inner = self.clone();
        tokio::spawn(async move {
            inner.snapshots.broadcast().await;
        });
    }

    fn to_protocol_error(&self, error: PiServerError) -> ProtocolError {
        if error.code == PiServerOperationErrorCode::NotImplemented {
            return ProtocolError {
                code: ProtocolErrorCode::NotImplemented,
                message: crate::errors::NOT_IMPLEMENTED_MESSAGE.to_string(),
                details: None,
            };
        }
        ProtocolError {
            code: error.code.into(),
            message: error.message,
            details: error.details,
        }
    }

    // ------------------------------------------------------------------
    // Connection lifecycle
    // ------------------------------------------------------------------

    fn accept(
        self: &Arc<Self>,
        connection: Arc<dyn ByteConnection>,
    ) -> Box<dyn ByteConnectionHandler> {
        if self.is_closing() {
            tokio::spawn(async move {
                let _ = connection.close(None).await;
            });
            return Box::new(NoopHandler);
        }
        let id = uuid::Uuid::new_v4().to_string();
        let state: SharedConnectionState = Arc::new(std::sync::Mutex::new(ConnectionState::new(
            id,
            connection,
            Some(self.max_frame_length),
        )));

        let inner = self.clone();
        let timeout_state = state.clone();
        let handshake_timeout_ms = self.handshake_timeout_ms;
        let timer_task = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(handshake_timeout_ms)).await;
            inner
                .fail_protocol(
                    timeout_state,
                    ProtocolError {
                        code: ProtocolErrorCode::InvalidRequest,
                        message: "Handshake timeout".to_string(),
                        details: None,
                    },
                )
                .await;
        });
        {
            state.lock().unwrap().handshake_timeout = Some(timer_task.abort_handle());
        }
        self.connections.lock().unwrap().push(state.clone());

        Box::new(RealHandler {
            inner: self.clone(),
            state,
        })
    }

    fn receive(self: &Arc<Self>, state: SharedConnectionState, chunk: Vec<u8>) {
        if is_terminal_connection(&state.lock().unwrap()) {
            return;
        }
        let messages = {
            let mut g = state.lock().unwrap();
            g.decoder.push(&chunk)
        };
        let messages = match messages {
            Ok(m) => m,
            Err(e) => {
                let inner = self.clone();
                tokio::spawn(async move {
                    inner
                        .fail_protocol(
                            state,
                            ProtocolError {
                                code: ProtocolErrorCode::InvalidRequest,
                                message: e.0,
                                details: None,
                            },
                        )
                        .await;
                });
                return;
            }
        };
        for message in messages {
            if is_terminal_connection(&state.lock().unwrap()) {
                return;
            }
            self.dispatch_message(state.clone(), message);
        }
    }

    fn dispatch_message(self: &Arc<Self>, state: SharedConnectionState, message: ClientMessage) {
        let stage = { state.lock().unwrap().stage };
        if stage == ConnectionStage::AwaitingHello {
            match message {
                ClientMessage::Hello(hello) => {
                    let (tx, rx) = watch::channel(false);
                    {
                        let mut g = state.lock().unwrap();
                        g.stage = ConnectionStage::Handshaking;
                        g.handshake_done = Some(rx);
                    }
                    let inner = self.clone();
                    tokio::spawn(async move {
                        let _guard = HandshakeDoneGuard(Some(tx));
                        inner.finish_handshake(state, hello).await;
                    });
                }
                _ => {
                    let inner = self.clone();
                    tokio::spawn(async move {
                        inner
                            .fail_protocol(
                                state,
                                ProtocolError {
                                    code: ProtocolErrorCode::InvalidRequest,
                                    message: "The first client message must be hello".to_string(),
                                    details: None,
                                },
                            )
                            .await;
                    });
                }
            }
            return;
        }

        if let ClientMessage::Hello(_) = message {
            let inner = self.clone();
            tokio::spawn(async move {
                inner
                    .fail_protocol(
                        state,
                        ProtocolError {
                            code: ProtocolErrorCode::InvalidRequest,
                            message: "hello may only be sent as the first message".to_string(),
                            details: None,
                        },
                    )
                    .await;
            });
            return;
        }
        let request = match message {
            ClientMessage::Request(r) => r,
            ClientMessage::Hello(_) => unreachable!("handled above"),
        };

        if stage == ConnectionStage::Ready {
            let inner = self.clone();
            tokio::spawn(async move {
                inner.handle_request(state, request).await;
            });
            return;
        }
        if stage != ConnectionStage::Handshaking {
            return;
        }
        let handshake_done = { state.lock().unwrap().handshake_done.clone() };
        if let Some(mut rx) = handshake_done {
            let inner = self.clone();
            tokio::spawn(async move {
                loop {
                    if *rx.borrow() {
                        break;
                    }
                    if rx.changed().await.is_err() {
                        break;
                    }
                }
                let (ready, disconnected) = {
                    let g = state.lock().unwrap();
                    (g.stage == ConnectionStage::Ready, g.disconnected)
                };
                if ready && !disconnected {
                    inner.handle_request(state, request).await;
                }
            });
        }
    }

    async fn finish_handshake(self: Arc<Self>, state: SharedConnectionState, hello: ClientHello) {
        if !is_supported_protocol_version(hello.version as f64) {
            self.fail_protocol(
                state,
                ProtocolError {
                    code: ProtocolErrorCode::Version,
                    message: format!(
                        "Unsupported protocol version {}; expected {}",
                        hello.version, PROTOCOL_VERSION
                    ),
                    details: None,
                },
            )
            .await;
            return;
        }

        let snapshot = match self.snapshots.get(None).await {
            Ok(s) => s,
            Err(e) => {
                let protocol_error = self.to_protocol_error(e);
                self.fail_protocol(state, protocol_error).await;
                return;
            }
        };
        // Re-validate after the await (gotcha 7).
        let should_bail = {
            let g = state.lock().unwrap();
            self.is_closing()
                || g.disconnected
                || g.stage != ConnectionStage::Handshaking
                || g.connection.closed()
        };
        if should_bail {
            return;
        }

        let connection_id = { state.lock().unwrap().id.clone() };
        let sent = self
            .send_message(
                &state,
                ServerMessage::Hello(ServerHello {
                    connection_id,
                    snapshot: snapshot.clone(),
                }),
            )
            .await;
        if !sent {
            return;
        }
        let (still_handshaking, disconnected) = {
            let g = state.lock().unwrap();
            (g.stage == ConnectionStage::Handshaking, g.disconnected)
        };
        if disconnected || !still_handshaking {
            return;
        }
        {
            let mut g = state.lock().unwrap();
            g.handshake_complete = true;
            g.stage = ConnectionStage::Ready;
            if let Some(h) = g.handshake_timeout.take() {
                h.abort();
            }
        }
        if snapshot.revision != self.snapshots.current_revision() {
            if let Ok(current) = self.snapshots.get(None).await {
                self.send_message(
                    &state,
                    ServerMessage::Event(EventEnvelope {
                        event: ServerEvent::ServerSnapshot { snapshot: current },
                    }),
                )
                .await;
            }
        }
    }

    async fn handle_request(
        self: &Arc<Self>,
        state: SharedConnectionState,
        envelope: RequestEnvelope,
    ) {
        let result = self
            .sessions
            .execute_command(state.clone(), envelope.request)
            .await;
        let message = match result {
            Ok(result) => ServerMessage::Response(ResponseEnvelope::Success {
                id: envelope.id,
                result,
            }),
            Err(e) => ServerMessage::Response(ResponseEnvelope::Failure {
                id: envelope.id,
                error: self.to_protocol_error(e),
            }),
        };
        self.send_message(&state, message).await;
    }

    async fn transport_closed(self: Arc<Self>, state: SharedConnectionState) {
        let should_end = {
            let g = state.lock().unwrap();
            !g.disconnected && g.stage != ConnectionStage::Closing
        };
        if should_end {
            let result = { state.lock().unwrap().decoder.end() };
            if let Err(e) = result {
                self.report_error(anyhow::anyhow!(e));
            }
        }
        self.disconnect(state).await;
    }

    async fn fail_protocol(self: Arc<Self>, state: SharedConnectionState, error: ProtocolError) {
        let should_return = {
            let mut g = state.lock().unwrap();
            if g.disconnected
                || g.stage == ConnectionStage::Closing
                || g.stage == ConnectionStage::Closed
            {
                true
            } else {
                g.stage = ConnectionStage::Closing;
                if let Some(h) = g.handshake_timeout.take() {
                    h.abort();
                }
                false
            }
        };
        if should_return {
            return;
        }
        let message = ServerMessage::HelloError(ServerHelloError { error });
        let final_frame = encode_server_message(&message, Some(self.max_frame_length))
            .map_err(|e| self.report_error(anyhow::anyhow!(e)))
            .ok();
        self.close_connection(&state, final_frame.as_deref()).await;
        self.disconnect(state).await;
    }

    // ------------------------------------------------------------------
    // Start / close
    // ------------------------------------------------------------------

    async fn start(self: &Arc<Self>) -> Result<(), PiServerStartError> {
        if self.started.swap(true, Ordering::SeqCst) {
            return Err(PiServerStartError::AlreadyStarted);
        }
        if self.is_closing() {
            self.started.store(false, Ordering::SeqCst);
            return Err(PiServerStartError::Closing);
        }
        let mut listeners = self.listeners.lock().await;
        for i in 0..listeners.len() {
            let inner = self.clone();
            let accept: crate::connection::ByteConnectionAcceptor =
                Box::new(move |conn| inner.accept(conn));
            if let Err(e) = listeners[i].start(accept).await {
                self.closing.store(true, Ordering::SeqCst);
                for listener in listeners[..i].iter_mut() {
                    let _ = listener.close().await;
                }
                drop(listeners);
                self.close_server_state().await;
                self.started.store(false, Ordering::SeqCst);
                return Err(PiServerStartError::ListenerFailed(e.to_string()));
            }
        }
        Ok(())
    }

    async fn close(self: &Arc<Self>) {
        if self.closing.swap(true, Ordering::SeqCst) {
            // TS shares one `closePromise` across concurrent callers; this
            // port's simpler equivalent lets a concurrent second caller
            // return immediately rather than awaiting the first caller's
            // completion (named, documented simplification for a low-value
            // race — both callers still observe `closing == true`
            // immediately and the server still only tears down once).
            return;
        }
        let mut listeners = self.listeners.lock().await;
        for listener in listeners.iter_mut() {
            let _ = listener.close().await;
        }
        drop(listeners);
        self.close_server_state().await;
        self.started.store(false, Ordering::SeqCst);
    }

    async fn close_server_state(self: &Arc<Self>) {
        let conns = {
            let mut c = self.connections.lock().unwrap();
            std::mem::take(&mut *c)
        };
        for c in &conns {
            let mut g = c.lock().unwrap();
            g.stage = ConnectionStage::Closing;
            if let Some(h) = g.handshake_timeout.take() {
                h.abort();
            }
        }
        let close_futs = conns.iter().map(|c| self.close_connection(c, None));
        futures::future::join_all(close_futs).await;
        let disconnect_futs = conns.iter().map(|c| self.disconnect(c.clone()));
        futures::future::join_all(disconnect_futs).await;
        self.sessions.close().await;
    }
}

struct NoopHandler;
impl ByteConnectionHandler for NoopHandler {
    fn on_data(&mut self, _chunk: &[u8]) {}
    fn on_close(&mut self) {}
    fn on_error(&mut self, _error: &crate::connection::ByteConnectionError) {}
}

struct RealHandler {
    inner: Arc<Inner>,
    state: SharedConnectionState,
}

impl ByteConnectionHandler for RealHandler {
    fn on_data(&mut self, chunk: &[u8]) {
        self.inner
            .clone()
            .receive(self.state.clone(), chunk.to_vec());
    }

    fn on_close(&mut self) {
        let inner = self.inner.clone();
        let state = self.state.clone();
        tokio::spawn(async move { inner.transport_closed(state).await });
    }

    fn on_error(&mut self, error: &crate::connection::ByteConnectionError) {
        let inner = self.inner.clone();
        let state = self.state.clone();
        inner.report_error(anyhow::anyhow!(error.clone()));
        tokio::spawn(async move {
            let conn = { state.lock().unwrap().connection.clone() };
            let _ = conn.close(None).await;
            inner.disconnect(state).await;
        });
    }
}

/// A `PiServer` connection/session multiplexing instance. Thin `Arc<Inner>`
/// wrapper — see [`Inner`]'s own doc comment for why the shared state lives
/// there instead of directly on this type.
pub struct PiServer {
    inner: Arc<Inner>,
}

impl PiServer {
    pub fn new(
        service: Arc<dyn PiServerService>,
        options: PiServerOptions,
    ) -> Result<Self, PiServerConfigError> {
        let resolved = resolve_options(&options)?;
        let server_id = options
            .server_id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let inner = Arc::new_cyclic(|weak: &Weak<Inner>| Inner {
            id: server_id.clone(),
            service,
            max_frame_length: resolved.max_frame_length,
            handshake_timeout_ms: resolved.handshake_timeout_ms,
            on_error: options.on_error,
            connections: std::sync::Mutex::new(Vec::new()),
            sessions: LiveSessionManager::new(weak.clone()),
            snapshots: ServerSnapshotPublisher::new(server_id, weak.clone()),
            closing: AtomicBool::new(false),
            listeners: AsyncMutex::new(options.listeners),
            started: AtomicBool::new(false),
        });
        Ok(Self { inner })
    }

    pub fn id(&self) -> &str {
        &self.inner.id
    }

    pub async fn addresses(&self) -> Vec<String> {
        self.inner
            .listeners
            .lock()
            .await
            .iter()
            .filter_map(|l| l.address())
            .collect()
    }

    pub async fn start(&self) -> Result<(), PiServerStartError> {
        self.inner.start().await
    }

    pub async fn close(&self) {
        self.inner.close().await
    }

    #[cfg(test)]
    pub(crate) fn inner_for_test(&self) -> Arc<Inner> {
        self.inner.clone()
    }
}

#[cfg(test)]
impl Inner {
    /// Test-only: flips `closing` without running the full `close()`
    /// teardown, so gate-condition tests can isolate just that one
    /// condition of `maybe_dispose`'s five-condition check.
    pub(crate) fn set_closing_for_test(&self, closing: bool) {
        self.closing.store(closing, Ordering::SeqCst);
    }
}
