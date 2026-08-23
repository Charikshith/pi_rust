//! Port of `packages/server/src/sessions.ts` (`LiveSessionManager`).
//!
//! **Error-shape simplification (named, not silent):** every fallible path
//! here returns a plain [`PiServerError`], never TS's `toProtocolError`'s
//! `InternalServerError`/`ProtocolValidationError` branches. Those two only
//! apply to error shapes Rust's type system already makes unreachable at
//! this layer: `PiServerService`/`PiSessionRuntime` (`types.rs`) are typed to
//! return `Result<_, PiServerError>` directly (no `unknown`-typed throw
//! site), and framing/decode failures are converted to a
//! [`crate::protocol::schemas::ProtocolError`] before ever reaching
//! `execute_command` (in `server.rs`'s `receive`). `server.rs`'s
//! `to_protocol_error` is correspondingly narrower than TS's
//! `toProtocolError` for the same reason — see its own doc comment.
//!
//! **Dedup mechanism (named, not silent):** TS's `openingSessions: Map<id,
//! Promise<LiveSession>>` shares one JS `Promise` reference across every
//! concurrent `acquire()` caller for the same id. Rust has no equivalent
//! "shared, clonable future" primitive as a map value without extra crates,
//! so this port uses a `tokio::sync::watch` channel per in-flight id instead
//! (`None` while pending, `Some(result)` once settled) — any number of
//! concurrent waiters `subscribe()` to it and block on `changed()`/`borrow()`
//! until a value appears, then all observe the exact same cloned
//! [`PiServerError`]-or-success result. This preserves TS's core guarantee
//! (only the FIRST caller's `acquire_runtime` closure ever runs; every other
//! concurrent caller gets that same settled outcome) without needing a
//! shared-future crate.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Weak};

use tokio::sync::{watch, Mutex};

use crate::connection::{ConnectionStage, SharedConnectionState};
use crate::errors::{PiServerError, PiServerOperationErrorCode};
use crate::protocol::schemas::{
    Command, CommandResult, EventEnvelope, ServerEvent, ServerMessage, SessionMetadata,
    SessionPhase, SessionSnapshot,
};
use crate::server::Inner;
use crate::types::{CreateSessionOptions, PiSessionRuntime, PiSessionRuntimeEvent, PromptInput};

type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;
type AcquireRuntimeFn =
    Box<dyn FnOnce() -> BoxFuture<Result<Arc<dyn PiSessionRuntime>, PiServerError>> + Send>;
type OpeningResult = Option<Result<Arc<LiveSession>, PiServerError>>;

fn to_metadata(snapshot: &SessionSnapshot) -> SessionMetadata {
    SessionMetadata {
        id: snapshot.id.clone(),
        created_at: snapshot.created_at,
        updated_at: Some(snapshot.updated_at),
        parent_session_id: None,
        session_name: snapshot.name.clone(),
        cwd: Some(snapshot.cwd.clone()),
    }
}

pub(crate) struct LiveSession {
    id: String,
    runtime: Arc<dyn PiSessionRuntime>,
    connections: Mutex<Vec<SharedConnectionState>>,
    unsubscribe: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    operation_count: AtomicI64,
    ready: AtomicBool,
    terminal: AtomicBool,
    /// `Some` only while disposing; flips to `true` once `dispose()`
    /// finishes, mirroring TS's `disposing?: Promise<void>` as a value a
    /// concurrent caller can clone and await.
    disposing: Mutex<Option<watch::Receiver<bool>>>,
}

pub struct LiveSessionManager {
    weak_server: Weak<Inner>,
    live_sessions: Mutex<HashMap<String, Arc<LiveSession>>>,
    opening_sessions: Mutex<HashMap<String, watch::Sender<OpeningResult>>>,
}

impl LiveSessionManager {
    pub(crate) fn new(weak_server: Weak<Inner>) -> Self {
        Self {
            weak_server,
            live_sessions: Mutex::new(HashMap::new()),
            opening_sessions: Mutex::new(HashMap::new()),
        }
    }

    fn server(&self) -> Arc<Inner> {
        self.weak_server
            .upgrade()
            .expect("PiServer dropped while LiveSessionManager alive")
    }

    // ------------------------------------------------------------------
    // Public API used by `server.rs`
    // ------------------------------------------------------------------

    pub async fn execute_command(
        &self,
        connection: SharedConnectionState,
        command: Command,
    ) -> Result<CommandResult, PiServerError> {
        match command {
            Command::List => Ok(CommandResult::List {
                sessions: self.list_metadata().await?,
            }),
            Command::Create {
                cwd,
                name,
                model,
                thinking_level,
            } => {
                let id = uuid::Uuid::new_v4().to_string();
                let options = CreateSessionOptions {
                    id: id.clone(),
                    cwd,
                    name,
                    model,
                    thinking_level,
                };
                let service = self.server().service.clone();
                let live = self
                    .acquire(
                        id,
                        Box::new(move || {
                            Box::pin(async move { service.create_session(options).await })
                        }),
                    )
                    .await?;
                self.attach(&connection, &live).await?;
                let snapshot = self.broadcast_snapshot(&live).await?;
                let session = self.for_connection(snapshot, &connection);
                self.server().broadcast_server_snapshot();
                Ok(CommandResult::Create { session })
            }
            Command::Attach { session_id } => {
                let service = self.server().service.clone();
                let sid = session_id.clone();
                let live = self
                    .acquire(
                        session_id,
                        Box::new(move || Box::pin(async move { service.open_session(&sid).await })),
                    )
                    .await?;
                self.attach(&connection, &live).await?;
                let snapshot = self.broadcast_snapshot(&live).await?;
                let session = self.for_connection(snapshot, &connection);
                self.server().broadcast_server_snapshot();
                Ok(CommandResult::Attach { session })
            }
            Command::Detach { session_id } => self.execute_detach(connection, session_id).await,
            Command::Prompt { session_id, text } => {
                let live = self.require_attached(&connection, &session_id).await?;
                let runtime = live.runtime.clone();
                let session = self
                    .run_operation(connection, live, || async move {
                        runtime.prompt(PromptInput { text }).await
                    })
                    .await?;
                Ok(CommandResult::Prompt { session })
            }
            Command::Steer { session_id, text } => {
                let live = self.require_attached(&connection, &session_id).await?;
                let runtime = live.runtime.clone();
                let session = self
                    .run_operation(connection, live, || async move {
                        runtime.steer(PromptInput { text }).await
                    })
                    .await?;
                Ok(CommandResult::Steer { session })
            }
            Command::Abort { session_id } => {
                let live = self.require_attached(&connection, &session_id).await?;
                let runtime = live.runtime.clone();
                let session = self
                    .run_operation(connection, live, || async move { runtime.abort().await })
                    .await?;
                Ok(CommandResult::Abort { session })
            }
            Command::SetModel { session_id, model } => {
                let live = self.require_attached(&connection, &session_id).await?;
                let runtime = live.runtime.clone();
                let session = self
                    .run_operation(connection, live, || async move {
                        runtime.set_model(model).await
                    })
                    .await?;
                Ok(CommandResult::SetModel { session })
            }
            Command::SetThinking {
                session_id,
                thinking_level,
            } => {
                let live = self.require_attached(&connection, &session_id).await?;
                let runtime = live.runtime.clone();
                let session = self
                    .run_operation(connection, live, || async move {
                        runtime.set_thinking(thinking_level).await
                    })
                    .await?;
                Ok(CommandResult::SetThinking { session })
            }
        }
    }

    async fn execute_detach(
        &self,
        connection: SharedConnectionState,
        session_id: String,
    ) -> Result<CommandResult, PiServerError> {
        let live = self.live_sessions.lock().await.get(&session_id).cloned();
        let was_attached = connection.lock().unwrap().session_ids.remove(&session_id);
        if was_attached {
            if let Some(live) = &live {
                let should_broadcast = {
                    let mut conns = live.connections.lock().await;
                    conns.retain(|c| !Arc::ptr_eq(c, &connection));
                    let terminal = live.terminal.load(Ordering::SeqCst);
                    let disposing = live.disposing.lock().await.is_some();
                    !conns.is_empty() && !terminal && !disposing
                };
                if should_broadcast {
                    self.broadcast_snapshot(live).await?;
                }
                self.maybe_dispose(live.clone()).await;
            }
            self.server().broadcast_server_snapshot();
        }
        Ok(CommandResult::Detach { session_id })
    }

    /// `PiServer.disconnect` calls this after removing the connection from
    /// its own registry — detaches it from every live session it had
    /// attached and runs `maybeDispose` for each.
    pub(crate) async fn disconnect(&self, connection: SharedConnectionState) {
        let session_ids: Vec<String> = {
            let mut g = connection.lock().unwrap();
            g.session_ids.drain().collect()
        };
        let sessions: Vec<Arc<LiveSession>> = {
            let live_sessions = self.live_sessions.lock().await;
            session_ids
                .iter()
                .filter_map(|id| live_sessions.get(id).cloned())
                .collect()
        };
        for live in &sessions {
            let mut conns = live.connections.lock().await;
            conns.retain(|c| !Arc::ptr_eq(c, &connection));
        }
        let disposals = sessions.iter().map(|live| self.maybe_dispose(live.clone()));
        futures::future::join_all(disposals).await;
    }

    pub async fn list_metadata(&self) -> Result<Vec<SessionMetadata>, PiServerError> {
        let inner = self.server();
        let stored = inner.service.list_sessions().await?;
        let mut live_by_id: HashMap<String, SessionSnapshot> = HashMap::new();
        {
            let live_sessions = self.live_sessions.lock().await;
            for (id, live) in live_sessions.iter() {
                if live.disposing.lock().await.is_some() {
                    continue;
                }
                let snapshot = self.normalized_snapshot(live).await?;
                live_by_id.insert(id.clone(), snapshot);
            }
        }
        let mut metadata: Vec<SessionMetadata> = stored
            .into_iter()
            .map(|item| match live_by_id.remove(&item.id) {
                Some(snapshot) => SessionMetadata {
                    parent_session_id: item.parent_session_id,
                    ..to_metadata(&snapshot)
                },
                None => item,
            })
            .collect();
        for snapshot in live_by_id.into_values() {
            metadata.push(to_metadata(&snapshot));
        }
        Ok(metadata)
    }

    /// Server shutdown: awaits any in-flight `acquire()`s (ignoring their
    /// outcome — TS "reports rejections, not propagating them"; this port
    /// simplifies further to not even report them, since they are pure
    /// shutdown-race noise with no wire-visible effect), then disposes every
    /// live session unconditionally (no five-condition gate — not graceful
    /// per-session, only ordered, matching TS's own comment).
    pub async fn close(&self) {
        let receivers: Vec<watch::Receiver<OpeningResult>> = {
            self.opening_sessions
                .lock()
                .await
                .values()
                .map(|tx| tx.subscribe())
                .collect()
        };
        let waits = receivers.into_iter().map(|mut rx| async move {
            loop {
                if rx.borrow().is_some() {
                    return;
                }
                if rx.changed().await.is_err() {
                    return;
                }
            }
        });
        futures::future::join_all(waits).await;

        let sessions: Vec<Arc<LiveSession>> = {
            let mut live_sessions = self.live_sessions.lock().await;
            let all = live_sessions.values().cloned().collect();
            live_sessions.clear();
            all
        };
        let disposals = sessions.iter().map(|live| async move {
            let disposing = live.disposing.lock().await.clone();
            if let Some(mut rx) = disposing {
                let _ = rx.wait_for(|done| *done).await;
                return;
            }
            {
                let mut u = live.unsubscribe.lock().await;
                if let Some(f) = u.take() {
                    f();
                }
            }
            live.runtime.dispose().await;
        });
        futures::future::join_all(disposals).await;
    }

    // ------------------------------------------------------------------
    // Internal machinery
    // ------------------------------------------------------------------

    async fn acquire(
        &self,
        id: String,
        acquire_runtime: AcquireRuntimeFn,
    ) -> Result<Arc<LiveSession>, PiServerError> {
        loop {
            let existing = { self.live_sessions.lock().await.get(&id).cloned() };
            if let Some(existing) = existing {
                if existing.terminal.load(Ordering::SeqCst) {
                    return Err(PiServerError::session_locked(
                        Some(format!("Session runtime is terminating: {id}")),
                        None,
                    ));
                }
                let disposing = existing.disposing.lock().await.clone();
                if let Some(mut rx) = disposing {
                    let _ = rx.wait_for(|done| *done).await;
                    continue;
                }
                return Ok(existing);
            }

            let mut opening = self.opening_sessions.lock().await;
            if let Some(sender) = opening.get(&id) {
                let mut rx = sender.subscribe();
                drop(opening);
                loop {
                    if let Some(result) = rx.borrow().clone() {
                        return result;
                    }
                    if rx.changed().await.is_err() {
                        // Sender dropped without ever settling — shouldn't
                        // happen (the creator always sends before dropping
                        // its sender), but fall through to a fresh attempt
                        // rather than hang forever.
                        break;
                    }
                }
                continue;
            }
            let (tx, _rx) = watch::channel(None);
            opening.insert(id.clone(), tx.clone());
            drop(opening);

            // Reached at most once per `acquire()` call: every loop
            // iteration above either `return`s or `continue`s before
            // this point, so `acquire_runtime` (a `FnOnce`) is consumed
            // exactly once here.
            let result = self.create(id.clone(), acquire_runtime).await;
            {
                let mut opening = self.opening_sessions.lock().await;
                opening.remove(&id);
            }
            let _ = tx.send(Some(result.clone()));
            return result;
        }
    }

    async fn create(
        &self,
        id: String,
        acquire_runtime: AcquireRuntimeFn,
    ) -> Result<Arc<LiveSession>, PiServerError> {
        let runtime = acquire_runtime().await?;
        let inner = self.server();
        if inner.is_closing() {
            runtime.dispose().await;
            return Err(PiServerError::new(
                PiServerOperationErrorCode::InvalidRequest,
                "PiServer closed while acquiring a session runtime",
                None,
            ));
        }
        let snapshot = runtime.snapshot().await;
        if snapshot.id != id {
            runtime.dispose().await;
            return Err(PiServerError::new(
                PiServerOperationErrorCode::InvalidRequest,
                format!(
                    "Service returned session {} for server-assigned session {}",
                    snapshot.id, id
                ),
                None,
            ));
        }
        let live = Arc::new(LiveSession {
            id: id.clone(),
            runtime: runtime.clone(),
            connections: Mutex::new(Vec::new()),
            unsubscribe: Mutex::new(None),
            operation_count: AtomicI64::new(0),
            ready: AtomicBool::new(false),
            terminal: AtomicBool::new(false),
            disposing: Mutex::new(None),
        });
        let weak_server = self.weak_server.clone();
        let live_id = id.clone();
        let unsubscribe = runtime.subscribe(Box::new(move |event| {
            let weak_server = weak_server.clone();
            let live_id = live_id.clone();
            tokio::spawn(async move {
                if let Some(inner) = weak_server.upgrade() {
                    let live = inner
                        .sessions
                        .live_sessions
                        .lock()
                        .await
                        .get(&live_id)
                        .cloned();
                    if let Some(live) = live {
                        inner.sessions.handle_runtime_event(&live, event).await;
                    }
                }
            });
        }));
        *live.unsubscribe.lock().await = Some(unsubscribe);
        self.live_sessions.lock().await.insert(id, live.clone());
        live.ready.store(true, Ordering::SeqCst);
        Ok(live)
    }

    pub(crate) async fn handle_runtime_event(
        &self,
        live: &Arc<LiveSession>,
        event: PiSessionRuntimeEvent,
    ) {
        match event {
            PiSessionRuntimeEvent::Error(error) => {
                self.terminate(live.clone(), error).await;
                return;
            }
            PiSessionRuntimeEvent::Progress(progress) => {
                let inner = self.server();
                let envelope = ServerMessage::Event(EventEnvelope {
                    event: ServerEvent::SessionProgress {
                        session_id: live.id.clone(),
                        progress: *progress,
                    },
                });
                let conns = live.connections.lock().await.clone();
                for c in &conns {
                    inner.send_message(c, envelope.clone()).await;
                }
            }
            PiSessionRuntimeEvent::Snapshot => {
                let _ = self.broadcast_snapshot(live).await;
            }
        }
        self.schedule_maybe_dispose(live.clone());
    }

    async fn terminate(&self, live: Arc<LiveSession>, error: PiServerError) {
        if live.terminal.swap(true, Ordering::SeqCst) {
            return;
        }
        let inner = self.server();
        inner.report_error(anyhow::anyhow!(error));
        {
            let mut u = live.unsubscribe.lock().await;
            if let Some(f) = u.take() {
                f();
            }
        }
        let conns = live.connections.lock().await.clone();
        let close_futs = conns.iter().map(|c| inner.close_connection(c, None));
        futures::future::join_all(close_futs).await;
        let disconnect_futs = conns.iter().map(|c| inner.disconnect(c.clone()));
        futures::future::join_all(disconnect_futs).await;
        self.maybe_dispose(live).await;
    }

    async fn normalized_snapshot(
        &self,
        live: &Arc<LiveSession>,
    ) -> Result<SessionSnapshot, PiServerError> {
        let mut snapshot = live.runtime.snapshot().await;
        if snapshot.id != live.id {
            return Err(PiServerError::new(
                PiServerOperationErrorCode::InvalidRequest,
                format!(
                    "Runtime session ID changed from {} to {}",
                    live.id, snapshot.id
                ),
                None,
            ));
        }
        snapshot.phase = live.runtime.get_phase();
        snapshot.attached = !live.connections.lock().await.is_empty();
        snapshot.locked = true;
        Ok(snapshot)
    }

    fn for_connection(
        &self,
        mut snapshot: SessionSnapshot,
        connection: &SharedConnectionState,
    ) -> SessionSnapshot {
        let attached = connection
            .lock()
            .unwrap()
            .session_ids
            .contains(&snapshot.id);
        snapshot.attached = attached;
        snapshot
    }

    async fn broadcast_snapshot(
        &self,
        live: &Arc<LiveSession>,
    ) -> Result<SessionSnapshot, PiServerError> {
        let snapshot = self.normalized_snapshot(live).await?;
        let inner = self.server();
        let envelope = ServerMessage::Event(EventEnvelope {
            event: ServerEvent::SessionSnapshot {
                snapshot: snapshot.clone(),
            },
        });
        let conns = live.connections.lock().await.clone();
        for c in &conns {
            inner.send_message(c, envelope.clone()).await;
        }
        Ok(snapshot)
    }

    async fn attach(
        &self,
        connection: &SharedConnectionState,
        live: &Arc<LiveSession>,
    ) -> Result<(), PiServerError> {
        let should_fail = {
            let g = connection.lock().unwrap();
            g.disconnected || g.stage != ConnectionStage::Ready || g.connection.closed()
        };
        if should_fail {
            self.maybe_dispose(live.clone()).await;
            return Err(PiServerError::new(
                PiServerOperationErrorCode::InvalidRequest,
                "Connection closed while attaching to a session",
                None,
            ));
        }
        {
            let mut g = connection.lock().unwrap();
            g.session_ids.insert(live.id.clone());
        }
        live.connections.lock().await.push(connection.clone());
        Ok(())
    }

    async fn require_attached(
        &self,
        connection: &SharedConnectionState,
        session_id: &str,
    ) -> Result<Arc<LiveSession>, PiServerError> {
        let is_attached = connection.lock().unwrap().session_ids.contains(session_id);
        if !is_attached {
            return Err(PiServerError::new(
                PiServerOperationErrorCode::InvalidRequest,
                format!("Connection is not attached to session {session_id}"),
                None,
            ));
        }
        let live = self.live_sessions.lock().await.get(session_id).cloned();
        let live = match live {
            Some(l) => l,
            None => {
                return Err(PiServerError::not_found(
                    Some(format!("Session is not live: {session_id}")),
                    None,
                ))
            }
        };
        let is_disposing = live.disposing.lock().await.is_some();
        if live.terminal.load(Ordering::SeqCst) || is_disposing {
            return Err(PiServerError::not_found(
                Some(format!("Session is not live: {session_id}")),
                None,
            ));
        }
        Ok(live)
    }

    fn schedule_maybe_dispose(&self, live: Arc<LiveSession>) {
        let weak_server = self.weak_server.clone();
        tokio::spawn(async move {
            if let Some(inner) = weak_server.upgrade() {
                inner.sessions.maybe_dispose(live).await;
            }
        });
    }

    async fn run_operation<F, Fut>(
        &self,
        connection: SharedConnectionState,
        live: Arc<LiveSession>,
        operation: F,
    ) -> Result<SessionSnapshot, PiServerError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<(), PiServerError>>,
    {
        live.operation_count.fetch_add(1, Ordering::SeqCst);
        let _guard = OperationGuard {
            weak_server: self.weak_server.clone(),
            live: live.clone(),
        };
        operation().await?;
        let snapshot = self.broadcast_snapshot(&live).await?;
        Ok(self.for_connection(snapshot, &connection))
    }

    /// **The five-condition dispose gate** (analysis doc §4/§7 gotcha 6): a
    /// live session is disposed only when the server isn't closing, the
    /// session is `ready`, it isn't already disposing, it has zero attached
    /// connections, zero in-flight operations, and — unless `terminal` — its
    /// phase is `idle`. Get this exactly right; every command handler above
    /// depends on it for resource safety.
    pub(crate) async fn maybe_dispose(&self, live: Arc<LiveSession>) {
        let disposing_snapshot = live.disposing.lock().await.clone();
        let inner = self.server();
        let gate_fails = inner.is_closing()
            || !live.ready.load(Ordering::SeqCst)
            || disposing_snapshot.is_some()
            || !live.connections.lock().await.is_empty()
            || live.operation_count.load(Ordering::SeqCst) > 0
            || (!live.terminal.load(Ordering::SeqCst)
                && live.runtime.get_phase() != SessionPhase::Idle);
        if gate_fails {
            if let Some(mut rx) = disposing_snapshot {
                let _ = rx.wait_for(|done| *done).await;
            }
            return;
        }
        let (tx, rx) = watch::channel(false);
        *live.disposing.lock().await = Some(rx);
        {
            let mut u = live.unsubscribe.lock().await;
            if let Some(f) = u.take() {
                f();
            }
        }
        live.runtime.dispose().await;
        {
            let mut sessions = self.live_sessions.lock().await;
            if let Some(existing) = sessions.get(&live.id) {
                if Arc::ptr_eq(existing, &live) {
                    sessions.remove(&live.id);
                }
            }
        }
        let _ = tx.send(true);
        if !inner.is_closing() {
            inner.broadcast_server_snapshot();
        }
    }
}

struct OperationGuard {
    weak_server: Weak<Inner>,
    live: Arc<LiveSession>,
}

impl Drop for OperationGuard {
    fn drop(&mut self) {
        self.live.operation_count.fetch_sub(1, Ordering::SeqCst);
        let weak_server = self.weak_server.clone();
        let live = self.live.clone();
        tokio::spawn(async move {
            if let Some(inner) = weak_server.upgrade() {
                inner.sessions.maybe_dispose(live).await;
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::{ConnectionState, NoopConnection};
    use crate::server::{PiServer, PiServerOptions};
    use crate::testing::service::TestServerService;
    use crate::types::PiServerService;
    use std::time::Duration;

    fn make_server(service: Arc<TestServerService>) -> Arc<PiServer> {
        Arc::new(
            PiServer::new(
                service,
                PiServerOptions {
                    listeners: vec![],
                    max_frame_length: None,
                    handshake_timeout_ms: None,
                    server_id: Some("srv-test".to_string()),
                    on_error: None,
                },
            )
            .unwrap(),
        )
    }

    async fn fresh_live(service: &TestServerService, id: &str) -> Arc<LiveSession> {
        let runtime = service.open_session(id).await.unwrap();
        Arc::new(LiveSession {
            id: id.to_string(),
            runtime,
            connections: Mutex::new(Vec::new()),
            unsubscribe: Mutex::new(None),
            operation_count: AtomicI64::new(0),
            ready: AtomicBool::new(true),
            terminal: AtomicBool::new(false),
            disposing: Mutex::new(None),
        })
    }

    fn shared_connection() -> SharedConnectionState {
        Arc::new(std::sync::Mutex::new(ConnectionState::new(
            "conn-1".to_string(),
            Arc::new(NoopConnection),
            None,
        )))
    }

    /// The five-condition dispose gate (analysis doc §4/§7 gotcha 6),
    /// tested one condition at a time: each must independently block
    /// disposal, and clearing all five must let it through.
    #[tokio::test]
    async fn maybe_dispose_gate_blocks_on_each_condition_independently() {
        let service = Arc::new(TestServerService::new());
        for id in [
            "gate-closing",
            "gate-not-ready",
            "gate-disposing",
            "gate-connections",
            "gate-operations",
            "gate-phase",
            "gate-terminal",
            "gate-clean",
        ] {
            service.seed(id, None, None, None, None);
        }
        let server = make_server(service.clone());
        let inner = server.inner_for_test();

        // Each sub-case below uses its own session id: the test double
        // locks an id until its runtime is disposed, and most sub-cases
        // deliberately leave dispose_count at 0, so reusing one id across
        // cases would make later `open_session` calls fail with
        // `SessionLocked` instead of exercising the gate.

        // 1. server is closing.
        let live = fresh_live(&service, "gate-closing").await;
        let runtime = service.latest_runtime("gate-closing");
        inner.set_closing_for_test(true);
        inner.sessions.maybe_dispose(live.clone()).await;
        assert_eq!(
            runtime.dispose_count(),
            0,
            "closing server must block dispose"
        );
        inner.set_closing_for_test(false);

        // 2. not ready.
        let live = fresh_live(&service, "gate-not-ready").await;
        let runtime = service.latest_runtime("gate-not-ready");
        live.ready.store(false, Ordering::SeqCst);
        inner.sessions.maybe_dispose(live.clone()).await;
        assert_eq!(runtime.dispose_count(), 0, "not-ready must block dispose");

        // 3. already disposing — maybe_dispose should just await the
        // existing disposal rather than starting a new one.
        let live = fresh_live(&service, "gate-disposing").await;
        let runtime = service.latest_runtime("gate-disposing");
        let (tx, rx) = watch::channel(false);
        *live.disposing.lock().await = Some(rx);
        let waiter = {
            let inner = inner.clone();
            let live = live.clone();
            tokio::spawn(async move { inner.sessions.maybe_dispose(live).await })
        };
        tokio::time::sleep(Duration::from_millis(10)).await;
        let _ = tx.send(true);
        tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .expect("maybe_dispose must return once the existing disposal settles")
            .unwrap();
        assert_eq!(
            runtime.dispose_count(),
            0,
            "an already-disposing session's OWN dispose() must not be invoked again"
        );

        // 4. connections not empty.
        let live = fresh_live(&service, "gate-connections").await;
        let runtime = service.latest_runtime("gate-connections");
        live.connections.lock().await.push(shared_connection());
        inner.sessions.maybe_dispose(live.clone()).await;
        assert_eq!(
            runtime.dispose_count(),
            0,
            "attached connection must block dispose"
        );

        // 5. operations in flight.
        let live = fresh_live(&service, "gate-operations").await;
        let runtime = service.latest_runtime("gate-operations");
        live.operation_count.store(1, Ordering::SeqCst);
        inner.sessions.maybe_dispose(live.clone()).await;
        assert_eq!(
            runtime.dispose_count(),
            0,
            "in-flight operation must block dispose"
        );

        // 6. non-idle phase (and not terminal).
        let live = fresh_live(&service, "gate-phase").await;
        let runtime = service.latest_runtime("gate-phase");
        runtime.set_phase(SessionPhase::Turn);
        inner.sessions.maybe_dispose(live.clone()).await;
        assert_eq!(
            runtime.dispose_count(),
            0,
            "non-idle phase must block dispose"
        );

        // Terminal sessions bypass the phase check.
        let live = fresh_live(&service, "gate-terminal").await;
        let runtime = service.latest_runtime("gate-terminal");
        runtime.set_phase(SessionPhase::Turn);
        live.terminal.store(true, Ordering::SeqCst);
        inner.sessions.maybe_dispose(live.clone()).await;
        assert_eq!(
            runtime.dispose_count(),
            1,
            "terminal sessions must dispose even mid-turn"
        );

        // All five conditions clear -> disposal actually runs.
        let live = fresh_live(&service, "gate-clean").await;
        let runtime = service.latest_runtime("gate-clean");
        inner.sessions.maybe_dispose(live.clone()).await;
        assert_eq!(
            runtime.dispose_count(),
            1,
            "a fully-idle, unattached session must dispose"
        );
        assert!(live.disposing.lock().await.is_some());
    }

    /// Full create -> attach -> prompt -> abort -> detach lifecycle through
    /// `execute_command`, against the real `TestServerService`/
    /// `TestSessionRuntime` double — not an oracle replay (see this wave's
    /// evidence for the named scope decision), but exercises the real
    /// dispatch/attach/require-attached/run-operation/detach code paths
    /// end to end.
    #[tokio::test]
    async fn full_command_lifecycle_create_attach_prompt_abort_detach() {
        let service = Arc::new(TestServerService::new());
        let server = make_server(service.clone());
        let inner = server.inner_for_test();
        let connection = shared_connection();
        {
            let mut g = connection.lock().unwrap();
            g.stage = ConnectionStage::Ready;
        }

        let create_result = inner
            .sessions
            .execute_command(
                connection.clone(),
                Command::Create {
                    cwd: None,
                    name: None,
                    model: None,
                    thinking_level: None,
                },
            )
            .await
            .expect("create should succeed");
        let session_id = match create_result {
            CommandResult::Create { session } => {
                assert!(
                    session.attached,
                    "creator's own connection must be attached"
                );
                session.id
            }
            other => panic!("unexpected result: {other:?}"),
        };

        // A second, unattached connection cannot prompt this session.
        let other_connection = shared_connection();
        let err = inner
            .sessions
            .execute_command(
                other_connection,
                Command::Prompt {
                    session_id: session_id.clone(),
                    text: "hi".to_string(),
                },
            )
            .await
            .expect_err("unattached connection must be rejected");
        assert_eq!(err.code, PiServerOperationErrorCode::InvalidRequest);

        // The attached connection can prompt. The test double's `prompt()`
        // only resolves once `finish_prompt()`/`abort()` releases it (like
        // TS's own `Deferred`-backed test double), so — matching
        // sessions.test.ts's own "does not queue prompts ... while a
        // prompt response is pending" case — the request is issued without
        // awaiting it to completion yet.
        let prompt_task = {
            let inner = inner.clone();
            let connection = connection.clone();
            let session_id = session_id.clone();
            tokio::spawn(async move {
                inner
                    .sessions
                    .execute_command(
                        connection,
                        Command::Prompt {
                            session_id,
                            text: "hi".to_string(),
                        },
                    )
                    .await
            })
        };
        // Single-threaded `#[tokio::test]` runtime: yielding once lets the
        // spawned task run synchronously up to its first real suspension
        // point (the prompt's internal wait for completion), by which
        // point the phase has already flipped to `turn`.
        tokio::task::yield_now().await;
        assert_eq!(
            service.latest_runtime(&session_id).get_phase(),
            SessionPhase::Turn,
            "prompt must synchronously move the session into the turn phase"
        );

        // A second prompt while one is in flight is rejected as busy.
        let busy = inner
            .sessions
            .execute_command(
                connection.clone(),
                Command::Prompt {
                    session_id: session_id.clone(),
                    text: "again".to_string(),
                },
            )
            .await
            .expect_err("concurrent prompt must be rejected");
        assert_eq!(busy.code, PiServerOperationErrorCode::Busy);

        // Abort releases the pending prompt; it does not itself resolve the
        // phase synchronously (same race as TS's own test double), so only
        // its own "did the abort command succeed" shape is checked here.
        let abort_result = inner
            .sessions
            .execute_command(
                connection.clone(),
                Command::Abort {
                    session_id: session_id.clone(),
                },
            )
            .await
            .expect("abort should succeed");
        assert!(matches!(abort_result, CommandResult::Abort { .. }));

        // Now that abort has released it, the original prompt call
        // resolves, reflecting the aborted turn's final idle phase.
        let prompt_result = tokio::time::timeout(Duration::from_secs(1), prompt_task)
            .await
            .expect("prompt must resolve once aborted")
            .unwrap()
            .expect("prompt should succeed");
        match prompt_result {
            CommandResult::Prompt { session } => assert_eq!(session.phase, SessionPhase::Idle),
            other => panic!("unexpected result: {other:?}"),
        }

        let runtime = service.latest_runtime(&session_id);
        assert_eq!(
            runtime.dispose_count(),
            0,
            "attached session must not dispose yet"
        );

        let detach_result = inner
            .sessions
            .execute_command(
                connection.clone(),
                Command::Detach {
                    session_id: session_id.clone(),
                },
            )
            .await
            .expect("detach should succeed");
        assert!(matches!(detach_result, CommandResult::Detach { .. }));

        // detach's own maybeDispose call is synchronous within
        // execute_detach, so disposal has already happened by the time
        // detach's Ok(..) is returned.
        assert_eq!(
            runtime.dispose_count(),
            1,
            "detaching the last connection must dispose"
        );
    }

    #[tokio::test]
    async fn list_metadata_merges_stored_and_live_sessions() {
        let service = Arc::new(TestServerService::new());
        service.seed("stored-only", None, None, None, None);
        let server = make_server(service.clone());
        let inner = server.inner_for_test();
        let connection = shared_connection();
        {
            let mut g = connection.lock().unwrap();
            g.stage = ConnectionStage::Ready;
        }
        inner
            .sessions
            .execute_command(
                connection,
                Command::Attach {
                    session_id: "stored-only".to_string(),
                },
            )
            .await
            .expect("attach should succeed");

        let metadata = inner.sessions.list_metadata().await.unwrap();
        assert_eq!(metadata.len(), 1);
        assert_eq!(metadata[0].id, "stored-only");
    }
}
