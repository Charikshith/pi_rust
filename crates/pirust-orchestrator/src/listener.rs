//! Port of `packages/server/src/listener.ts`.

use async_trait::async_trait;

use crate::connection::ByteConnectionAcceptor;

/// Supplies established byte connections after any required transport
/// authentication.
#[async_trait]
pub trait PiServerListener: Send + Sync {
    /// Human-readable bound address after startup, when the transport has
    /// one.
    fn address(&self) -> Option<String>;
    /// Starts listening and passes authorized connections to `accept`.
    async fn start(
        &mut self,
        accept: ByteConnectionAcceptor,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;
    async fn close(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;
}
