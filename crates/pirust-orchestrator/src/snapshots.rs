//! Port of `packages/server/src/snapshots.ts` (`ServerSnapshotPublisher`).
//!
//! **Broadcast serialization:** TS chains `broadcastQueue` (a `Promise`) so
//! concurrent `broadcast()` triggers run `performBroadcast` one at a time, in
//! call order. A `tokio::sync::Mutex` guarding nothing but the critical
//! section gives the identical guarantee here — `lock().await` already
//! queues waiters in arrival order.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Weak};

use tokio::sync::Mutex;

use crate::connection::ConnectionStage;
use crate::errors::PiServerError;
use crate::protocol::schemas::{
    EventEnvelope, ModelMetadata, ServerEvent, ServerMessage, ServerSnapshot,
};
use crate::server::Inner;

pub struct ServerSnapshotPublisher {
    server_id: String,
    weak_server: Weak<Inner>,
    revision: AtomicI64,
    broadcast_lock: Mutex<()>,
}

impl ServerSnapshotPublisher {
    pub(crate) fn new(server_id: String, weak_server: Weak<Inner>) -> Self {
        Self {
            server_id,
            weak_server,
            revision: AtomicI64::new(0),
            broadcast_lock: Mutex::new(()),
        }
    }

    fn server(&self) -> Arc<Inner> {
        self.weak_server
            .upgrade()
            .expect("PiServer dropped while ServerSnapshotPublisher alive")
    }

    pub fn current_revision(&self) -> i64 {
        self.revision.load(Ordering::SeqCst)
    }

    /// Serves the CURRENT revision read-only — never increments. Two
    /// connections doing `hello` concurrently can legitimately observe the
    /// same revision if no broadcast interleaves, matching TS's own comment.
    pub async fn get(
        &self,
        models: Option<Vec<ModelMetadata>>,
    ) -> Result<ServerSnapshot, PiServerError> {
        let inner = self.server();
        let sessions = inner.sessions.list_metadata().await?;
        let models = match models {
            Some(m) => m,
            None => inner.service.list_models().await?,
        };
        Ok(ServerSnapshot {
            server_id: self.server_id.clone(),
            revision: self.revision.load(Ordering::SeqCst),
            sessions,
            models,
        })
    }

    /// Fire-and-forget trigger at every call site (`PiServer` after a
    /// handshake-completed disconnect; `LiveSessionManager` after every
    /// session-count-visible change) — never after a per-session
    /// `session_snapshot` event, which reaches only that session's own
    /// attached connections, a separate broadcast scope from this one.
    pub async fn broadcast(&self) {
        let _guard = self.broadcast_lock.lock().await;
        self.perform_broadcast().await;
    }

    async fn perform_broadcast(&self) {
        let inner = self.server();
        let ready_connections: Vec<_> = {
            let conns = inner.connections.lock().unwrap();
            conns
                .iter()
                .filter(|c| {
                    let g = c.lock().unwrap();
                    g.stage == ConnectionStage::Ready && !g.disconnected
                })
                .cloned()
                .collect()
        };
        if ready_connections.is_empty() || inner.is_closing() {
            return;
        }
        // The revision counter is incremented ONLY here, inside a real
        // broadcast — this is the load-bearing monotonicity invariant.
        let revision = self.revision.fetch_add(1, Ordering::SeqCst) + 1;
        let models = match inner.service.list_models().await {
            Ok(m) => m,
            Err(e) => {
                inner.report_error(anyhow::anyhow!(e));
                return;
            }
        };
        let current = match self.get(Some(models)).await {
            Ok(s) => s,
            Err(e) => {
                inner.report_error(anyhow::anyhow!(e));
                return;
            }
        };
        let snapshot = ServerSnapshot {
            revision,
            ..current
        };
        let envelope = ServerMessage::Event(EventEnvelope {
            event: ServerEvent::ServerSnapshot { snapshot },
        });
        for connection in &ready_connections {
            inner.send_message(connection, envelope.clone()).await;
        }
    }
}

#[cfg(test)]
mod tests {
    //! Unit tests for the two load-bearing invariants named in the wave
    //! directive, independent of any oracle fixture: `get()` never
    //! increments the revision, and only a real broadcast does.

    use super::*;
    use crate::connection::ByteConnectionError;
    use crate::server::{PiServer, PiServerOptions};
    use crate::testing::service::TestServerService;
    use async_trait::async_trait;
    use std::sync::atomic::AtomicBool;

    struct NoopConnection {
        closed: AtomicBool,
    }

    #[async_trait]
    impl crate::connection::ByteConnection for NoopConnection {
        fn closed(&self) -> bool {
            self.closed.load(Ordering::SeqCst)
        }
        async fn send(&self, _chunk: &[u8]) -> Result<(), ByteConnectionError> {
            Ok(())
        }
        async fn close(&self, _final_chunk: Option<&[u8]>) -> Result<(), ByteConnectionError> {
            self.closed.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    fn test_server() -> Arc<PiServer> {
        let service = Arc::new(TestServerService::new());
        Arc::new(
            PiServer::new(
                service,
                PiServerOptions {
                    listeners: vec![],
                    max_frame_length: None,
                    handshake_timeout_ms: None,
                    server_id: Some("srv-1".to_string()),
                    on_error: None,
                },
            )
            .expect("valid options"),
        )
    }

    #[tokio::test]
    async fn get_does_not_increment_revision() {
        let server = test_server();
        let before = server.inner_for_test().snapshots.current_revision();
        let snapshot1 = server.inner_for_test().snapshots.get(None).await.unwrap();
        let snapshot2 = server.inner_for_test().snapshots.get(None).await.unwrap();
        let after = server.inner_for_test().snapshots.current_revision();
        assert_eq!(before, 0);
        assert_eq!(after, 0);
        assert_eq!(snapshot1.revision, 0);
        assert_eq!(snapshot2.revision, 0);
    }

    #[tokio::test]
    async fn broadcast_is_a_no_op_with_zero_ready_connections() {
        let server = test_server();
        server.inner_for_test().snapshots.broadcast().await;
        assert_eq!(server.inner_for_test().snapshots.current_revision(), 0);
    }

    #[tokio::test]
    async fn broadcast_increments_revision_once_per_call_with_a_ready_connection() {
        let server = test_server();
        let inner = server.inner_for_test();
        let state = crate::connection::ConnectionState::new(
            "conn-1".to_string(),
            Arc::new(NoopConnection {
                closed: AtomicBool::new(false),
            }),
            None,
        );
        let mut state = state;
        state.stage = ConnectionStage::Ready;
        let shared = Arc::new(std::sync::Mutex::new(state));
        inner.connections.lock().unwrap().push(shared);

        inner.snapshots.broadcast().await;
        assert_eq!(inner.snapshots.current_revision(), 1);
        inner.snapshots.broadcast().await;
        assert_eq!(inner.snapshots.current_revision(), 2);
    }
}
