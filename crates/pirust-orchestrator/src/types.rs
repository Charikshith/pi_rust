//! Port of `packages/server/src/types.ts` — the `PiServerService`/
//! `PiSessionRuntime` boundary traits that `sessions.rs`/`server.rs` are
//! built against.
//!
//! **Named divergence (not silent):** `PiSessionRuntime::dispose` is
//! infallible here (`async fn dispose(&self)`, no `Result`) where TS's
//! `dispose(): Promise<void>` can technically reject. Real Pi's own
//! `maybeDispose` doesn't catch a rejection from this specific call either
//! (only `scheduleMaybeDispose`'s outer `.catch` does, via a different
//! codepath) — collapsing it to infallible here removes an error path this
//! wave's `TestSessionRuntime` port never exercises anyway, and keeps every
//! `dispose()` call site (which matches every one of `sessions.ts`'s) free of
//! a `Result` it would otherwise have to thread through for no behavioral
//! difference in the only implementation this wave ships.
//!
//! `PiSessionRuntime::snapshot` is `async` here (TS's `MaybePromise<T>`
//! collapses to always-async in Rust, matching the `Promise<T>` case of that
//! union; a synchronous implementation just doesn't await anything).

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;

use crate::errors::PiServerError;
use crate::protocol::schemas::{
    ModelMetadata, ModelRef, SessionMetadata, SessionPhase, SessionSnapshot, ThinkingLevel,
    TranscriptProgress,
};

/// `Omit<Extract<Command, {command:"prompt"}>, "command"|"sessionId">` /
/// the `steer` equivalent — both reduce to just the prompt text today.
#[derive(Debug, Clone, PartialEq)]
pub struct PromptInput {
    pub text: String,
}

pub type SteerInput = PromptInput;

/// A collision-resistant ID assigned by `PiServer`; the service must persist
/// this exact ID (`CreateSessionOptions.id`'s own doc comment in `types.ts`).
#[derive(Debug, Clone, PartialEq)]
pub struct CreateSessionOptions {
    pub id: String,
    pub cwd: Option<String>,
    pub name: Option<String>,
    pub model: Option<ModelRef>,
    pub thinking_level: Option<ThinkingLevel>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PiSessionRuntimeEvent {
    Snapshot,
    // Boxed so the enum doesn't balloon to `TranscriptProgress`'s size for
    // every event, including the far more common `Snapshot`/`Error` ones.
    Progress(Box<TranscriptProgress>),
    Error(PiServerError),
}

pub type RuntimeEventListener = Box<dyn Fn(PiSessionRuntimeEvent) + Send + Sync>;
/// `() => void` unsubscribe closure returned by `subscribe`.
pub type Unsubscribe = Box<dyn FnOnce() + Send>;
pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// One acquired durable session. Conflicting operations must reject rather
/// than queue (`types.ts`'s own doc comment).
#[async_trait]
pub trait PiSessionRuntime: Send + Sync {
    async fn snapshot(&self) -> SessionSnapshot;
    fn get_phase(&self) -> SessionPhase;
    async fn prompt(&self, input: PromptInput) -> Result<(), PiServerError>;
    async fn steer(&self, input: SteerInput) -> Result<(), PiServerError>;
    async fn abort(&self) -> Result<(), PiServerError>;
    async fn set_model(&self, model: ModelRef) -> Result<(), PiServerError>;
    async fn set_thinking(&self, thinking_level: ThinkingLevel) -> Result<(), PiServerError>;
    /// Returns an unsubscribe closure, matching TS's `subscribe(listener):
    /// () => void`.
    fn subscribe(&self, listener: RuntimeEventListener) -> Unsubscribe;
    async fn dispose(&self);
}

/// Service boundary for durable sessions and exclusively acquired runtimes.
#[async_trait]
pub trait PiServerService: Send + Sync {
    async fn list_sessions(&self) -> Result<Vec<SessionMetadata>, PiServerError>;
    async fn list_models(&self) -> Result<Vec<ModelMetadata>, PiServerError>;
    async fn create_session(
        &self,
        options: CreateSessionOptions,
    ) -> Result<Arc<dyn PiSessionRuntime>, PiServerError>;
    async fn open_session(
        &self,
        session_id: &str,
    ) -> Result<Arc<dyn PiSessionRuntime>, PiServerError>;
}
