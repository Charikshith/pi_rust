//! Cross-platform, in-memory transport test double — NOT a port of any Pi
//! source file (Pi never needed one: Node's real Unix socket runs
//! everywhere Pi's own test suite runs). Added this wave specifically so
//! `test/conformance.test.ts`'s transport-agnostic protocol battery (hello/
//! version negotiation, fragmented hello, handshake timeout, oversized
//! frames, request/response ordering, snapshot catch-up, terminal-error
//! disconnects, graceful close) can be driven over a REAL async byte stream
//! on this Windows dev machine, where `tokio::net::UnixListener` does not
//! exist. See `crate::transports::unix`'s module doc for the full Wave 5
//! scope decision this supports.
//!
//! [`DuplexTransport::connect`] wires a fresh `tokio::io::duplex` pair
//! through the SAME [`crate::listener::PiServerListener`]/
//! [`crate::connection::ByteConnection`] traits a real transport implements,
//! so `PiServer`'s own connection/session state machine runs completely
//! unmodified — only the transport underneath is fake.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio::io::{AsyncReadExt, AsyncWriteExt, WriteHalf};
use tokio::sync::Mutex as AsyncMutex;

use crate::connection::{ByteConnection, ByteConnectionAcceptor, ByteConnectionError};
use crate::listener::PiServerListener;
use crate::testing::client::{ProtocolTestClient, WireChannel};

const BUFFER_SIZE: usize = 64 * 1024;

struct Shared {
    address: Mutex<Option<String>>,
    accept: Mutex<Option<Arc<ByteConnectionAcceptor>>>,
}

/// A [`PiServerListener`] backed by in-memory duplex pipes instead of a real
/// socket. Give one to [`crate::server::PiServerOptions::listeners`], then
/// call [`DuplexTransport::connect`] as many times as a test needs a new
/// client.
pub struct DuplexTransport {
    shared: Arc<Shared>,
}

impl DuplexTransport {
    pub fn new(address: impl Into<String>) -> Self {
        Self {
            shared: Arc::new(Shared {
                address: Mutex::new(Some(address.into())),
                accept: Mutex::new(None),
            }),
        }
    }

    pub fn listener(&self) -> Box<dyn PiServerListener> {
        Box::new(DuplexListener {
            shared: self.shared.clone(),
        })
    }

    /// Simulates one client connecting: spins up a fresh in-memory duplex
    /// pair, feeds the server-side half through the listener's stored
    /// `accept` closure exactly as a real transport's accept loop would, and
    /// returns the client side as a ready-to-use [`ProtocolTestClient`].
    ///
    /// Panics if called before the owning `PiServer::start()` has run (no
    /// `accept` closure registered yet) — a test-only precondition, not a
    /// runtime possibility for a real transport.
    pub fn connect(&self) -> Arc<ProtocolTestClient> {
        let accept = self
            .shared
            .accept
            .lock()
            .unwrap()
            .clone()
            .expect("DuplexTransport::connect called before the server started");

        let (client_io, server_io) = tokio::io::duplex(BUFFER_SIZE);
        let (server_read, server_write) = tokio::io::split(server_io);
        let connection = Arc::new(DuplexByteConnection::new(server_write));
        let mut handler = accept(connection.clone() as Arc<dyn ByteConnection>);
        let server_connection = connection.clone();
        tokio::spawn(async move {
            let mut read_half = server_read;
            let mut buf = [0u8; BUFFER_SIZE];
            loop {
                match read_half.read(&mut buf).await {
                    Ok(0) => {
                        server_connection.mark_closed();
                        handler.on_close();
                        break;
                    }
                    Ok(n) => handler.on_data(&buf[..n]),
                    Err(e) => {
                        handler.on_error(&ByteConnectionError(e.to_string()));
                        server_connection.mark_closed();
                        handler.on_close();
                        break;
                    }
                }
            }
        });

        let (client_read, client_write) = tokio::io::split(client_io);
        let client = ProtocolTestClient::new(Box::new(DuplexWireChannel {
            writer: AsyncMutex::new(client_write),
        }));
        let client_for_reader = client.clone();
        tokio::spawn(async move {
            let mut read_half = client_read;
            let mut buf = [0u8; BUFFER_SIZE];
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
        client
    }
}

struct DuplexListener {
    shared: Arc<Shared>,
}

#[async_trait]
impl PiServerListener for DuplexListener {
    fn address(&self) -> Option<String> {
        self.shared.address.lock().unwrap().clone()
    }

    async fn start(
        &mut self,
        accept: ByteConnectionAcceptor,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        *self.shared.accept.lock().unwrap() = Some(Arc::new(accept));
        Ok(())
    }

    async fn close(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        *self.shared.accept.lock().unwrap() = None;
        *self.shared.address.lock().unwrap() = None;
        Ok(())
    }
}

type DuplexWrite = WriteHalf<tokio::io::DuplexStream>;

struct DuplexByteConnection {
    writer: AsyncMutex<Option<DuplexWrite>>,
    closed: AtomicBool,
}

impl DuplexByteConnection {
    fn new(writer: DuplexWrite) -> Self {
        Self {
            writer: AsyncMutex::new(Some(writer)),
            closed: AtomicBool::new(false),
        }
    }

    fn mark_closed(&self) {
        self.closed.store(true, Ordering::SeqCst);
    }
}

#[async_trait]
impl ByteConnection for DuplexByteConnection {
    fn closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    async fn send(&self, chunk: &[u8]) -> Result<(), ByteConnectionError> {
        let mut guard = self.writer.lock().await;
        match guard.as_mut() {
            Some(writer) => writer
                .write_all(chunk)
                .await
                .map_err(|e| ByteConnectionError(e.to_string())),
            None => Err(ByteConnectionError(
                "duplex connection is closed".to_string(),
            )),
        }
    }

    async fn close(&self, final_chunk: Option<&[u8]>) -> Result<(), ByteConnectionError> {
        let mut guard = self.writer.lock().await;
        if let Some(writer) = guard.as_mut() {
            if let Some(bytes) = final_chunk {
                let _ = writer.write_all(bytes).await;
            }
            let _ = writer.shutdown().await;
        }
        *guard = None;
        self.closed.store(true, Ordering::SeqCst);
        Ok(())
    }
}

struct DuplexWireChannel {
    writer: AsyncMutex<DuplexWrite>,
}

#[async_trait]
impl WireChannel for DuplexWireChannel {
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
