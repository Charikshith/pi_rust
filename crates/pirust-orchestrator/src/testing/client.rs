//! Port of `packages/server/src/testing/client.ts` — `ProtocolTestClient` +
//! the `WireChannel` transport seam. Cross-platform: [`ProtocolTestClient`]
//! is generic over any [`WireChannel`] impl, so both a real Unix socket
//! (`connect_unix_test_client`, `#[cfg(unix)]`) and the in-memory
//! [`super::duplex`] transport double drive the exact same client code.
//!
//! **Waiter design (named, not silent):** TS keeps a `Set<MessageWaiter>`
//! and resolves/rejects individual waiters as matching messages arrive or
//! the connection fails/closes. This port instead bumps a `watch::Sender<u64>`
//! generation counter on every `receive`/`fail`/`mark_closed`, and every
//! waiter re-scans the full message log against its predicate each time the
//! generation changes. `tokio::sync::watch::Receiver::changed()` compares
//! version numbers rather than relying on "was a waiter registered in time,"
//! so — unlike a naive `Notify`-based design — it cannot miss an update that
//! happens between a waiter's own check and its await point. Behaviorally
//! equivalent to TS's design (every waiter eventually observes every
//! terminal state), just implemented as poll-on-change instead of a
//! predicate-keyed waiter registry.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio::sync::watch;

use crate::protocol::codec::{encode_client_message, ServerMessageDecoder};
use crate::protocol::schemas::{
    ClientHello, ClientMessage, Command, RequestEnvelope, ResponseEnvelope, ServerMessage,
    PROTOCOL_VERSION,
};

#[derive(Debug, Clone)]
pub enum ClientTestError {
    Closed,
    Failed(String),
    Wire(String),
}

impl std::fmt::Display for ClientTestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Closed => write!(f, "Wire client is closed"),
            Self::Failed(e) => write!(f, "{e}"),
            Self::Wire(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ClientTestError {}

/// Port of TS's `WireChannel` interface — the byte-transport seam a test
/// client sends over and closes.
#[async_trait]
pub trait WireChannel: Send + Sync {
    async fn send(&self, chunk: &[u8]) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;
    async fn send_fragmented(
        &self,
        chunk: &[u8],
        split_at: usize,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;
    async fn close(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;
}

/// Port of `ProtocolTestClient`.
pub struct ProtocolTestClient {
    channel: Box<dyn WireChannel>,
    decoder: Mutex<ServerMessageDecoder>,
    messages: Mutex<Vec<ServerMessage>>,
    closed: AtomicBool,
    failed: Mutex<Option<String>>,
    generation_tx: watch::Sender<u64>,
    request_sequence: AtomicU64,
}

impl ProtocolTestClient {
    pub fn new(channel: Box<dyn WireChannel>) -> Arc<Self> {
        let (generation_tx, _rx) = watch::channel(0u64);
        Arc::new(Self {
            channel,
            decoder: Mutex::new(ServerMessageDecoder::new(None)),
            messages: Mutex::new(Vec::new()),
            closed: AtomicBool::new(false),
            failed: Mutex::new(None),
            generation_tx,
            request_sequence: AtomicU64::new(0),
        })
    }

    fn bump(&self) {
        self.generation_tx.send_modify(|g| *g += 1);
    }

    pub fn messages(&self) -> Vec<ServerMessage> {
        self.messages.lock().unwrap().clone()
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    /// Feed raw bytes received from the transport (the reader loop's job).
    pub fn receive(&self, chunk: &[u8]) {
        let result = self.decoder.lock().unwrap().push(chunk);
        match result {
            Ok(new_messages) => {
                if !new_messages.is_empty() {
                    self.messages.lock().unwrap().extend(new_messages);
                }
                self.bump();
            }
            Err(e) => self.fail(e.to_string()),
        }
    }

    pub fn fail(&self, error: impl Into<String>) {
        let mut failed = self.failed.lock().unwrap();
        if failed.is_none() {
            *failed = Some(error.into());
        }
        drop(failed);
        self.bump();
    }

    pub fn mark_closed(&self) {
        if !self.closed.swap(true, Ordering::SeqCst) {
            self.bump();
        }
    }

    pub async fn wait_for_close(&self) {
        if self.is_closed() {
            return;
        }
        let mut rx = self.generation_tx.subscribe();
        loop {
            if self.is_closed() {
                return;
            }
            if rx.changed().await.is_err() {
                return;
            }
        }
    }

    pub async fn next(
        &self,
        predicate: impl Fn(&ServerMessage) -> bool,
    ) -> Result<ServerMessage, ClientTestError> {
        self.next_from(0, predicate).await
    }

    pub async fn next_from(
        &self,
        index: usize,
        predicate: impl Fn(&ServerMessage) -> bool,
    ) -> Result<ServerMessage, ClientTestError> {
        let mut rx = self.generation_tx.subscribe();
        loop {
            {
                let messages = self.messages.lock().unwrap();
                if let Some(m) = messages.iter().skip(index).find(|m| predicate(m)) {
                    return Ok(m.clone());
                }
            }
            if let Some(err) = self.failed.lock().unwrap().clone() {
                return Err(ClientTestError::Failed(err));
            }
            if self.is_closed() {
                return Err(ClientTestError::Closed);
            }
            if rx.changed().await.is_err() {
                return Err(ClientTestError::Closed);
            }
        }
    }

    pub async fn hello(&self, version: i64) -> Result<ServerMessage, ClientTestError> {
        let response =
            self.next(|m| matches!(m, ServerMessage::Hello(_) | ServerMessage::HelloError(_)));
        self.send_message(ClientMessage::Hello(ClientHello { version }))
            .await?;
        response.await
    }

    pub async fn hello_default(&self) -> Result<ServerMessage, ClientTestError> {
        self.hello(PROTOCOL_VERSION as i64).await
    }

    pub async fn request(
        &self,
        command: Command,
        id: Option<String>,
    ) -> Result<ResponseEnvelope, ClientTestError> {
        let id = id.unwrap_or_else(|| {
            format!(
                "request-{}",
                self.request_sequence.fetch_add(1, Ordering::SeqCst) + 1
            )
        });
        let expect_id = id.clone();
        let response = self.next(move |m| match m {
            ServerMessage::Response(ResponseEnvelope::Success { id, .. }) => *id == expect_id,
            ServerMessage::Response(ResponseEnvelope::Failure { id, .. }) => *id == expect_id,
            _ => false,
        });
        self.send_message(ClientMessage::Request(RequestEnvelope {
            id,
            request: command,
        }))
        .await?;
        match response.await? {
            ServerMessage::Response(envelope) => Ok(envelope),
            _ => unreachable!("predicate only ever matches ServerMessage::Response"),
        }
    }

    pub async fn send_message(&self, message: ClientMessage) -> Result<(), ClientTestError> {
        let bytes = encode_client_message(&message, None)
            .map_err(|e| ClientTestError::Wire(e.to_string()))?;
        self.channel
            .send(&bytes)
            .await
            .map_err(|e| ClientTestError::Wire(e.to_string()))
    }

    pub async fn send_bytes(&self, chunk: &[u8]) -> Result<(), ClientTestError> {
        self.channel
            .send(chunk)
            .await
            .map_err(|e| ClientTestError::Wire(e.to_string()))
    }

    pub async fn send_fragmented_message(
        &self,
        message: ClientMessage,
        split_at: usize,
    ) -> Result<(), ClientTestError> {
        let bytes = encode_client_message(&message, None)
            .map_err(|e| ClientTestError::Wire(e.to_string()))?;
        self.channel
            .send_fragmented(&bytes, split_at)
            .await
            .map_err(|e| ClientTestError::Wire(e.to_string()))
    }

    pub async fn close(&self) -> Result<(), ClientTestError> {
        self.channel
            .close()
            .await
            .map_err(|e| ClientTestError::Wire(e.to_string()))
    }
}

#[cfg(unix)]
mod unix_client {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::unix::OwnedWriteHalf;
    use tokio::sync::Mutex as AsyncMutex;

    struct UnixWireChannel {
        writer: AsyncMutex<OwnedWriteHalf>,
    }

    #[async_trait]
    impl WireChannel for UnixWireChannel {
        async fn send(&self, chunk: &[u8]) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            self.writer
                .lock()
                .await
                .write_all(chunk)
                .await
                .map_err(Into::into)
        }

        async fn send_fragmented(
            &self,
            chunk: &[u8],
            split_at: usize,
        ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            let mut writer = self.writer.lock().await;
            writer.write_all(&chunk[..split_at]).await?;
            writer.write_all(&chunk[split_at..]).await?;
            Ok(())
        }

        async fn close(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            let _ = self.writer.lock().await.shutdown().await;
            Ok(())
        }
    }

    /// Port of `connectUnixTestClient`.
    pub async fn connect_unix_test_client(
        path: impl AsRef<std::path::Path>,
    ) -> std::io::Result<Arc<ProtocolTestClient>> {
        let stream = tokio::net::UnixStream::connect(path.as_ref()).await?;
        let (mut read_half, write_half) = stream.into_split();
        let client = ProtocolTestClient::new(Box::new(UnixWireChannel {
            writer: AsyncMutex::new(write_half),
        }));
        let client_for_reader = client.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 64 * 1024];
            loop {
                match read_half.read(&mut buf).await {
                    Ok(0) => {
                        client_for_reader.mark_closed();
                        break;
                    }
                    Ok(n) => client_for_reader.receive(&buf[..n]),
                    Err(e) => {
                        client_for_reader.fail(e.to_string());
                        client_for_reader.mark_closed();
                        break;
                    }
                }
            }
        });
        Ok(client)
    }
}

#[cfg(unix)]
pub use unix_client::connect_unix_test_client;
