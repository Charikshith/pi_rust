//! [`AgentPiSessionRuntime`] — the [`PiSessionRuntime`] backed by a real
//! [`AgentHarness`], one instance per durable session (feat-009 Wave 6, a
//! pirust-side addition: no Pi oracle constructs a `PiSessionRuntime` this
//! way, since real Pi never wrote one against `AgentHarness` at all).
//!
//! **Single-instance-per-session design (named, not silent):** unlike
//! [`crate::testing::service::TestSessionRuntime`] (a fresh, disposable
//! wrapper built on every `open_session`), exactly one
//! [`AgentPiSessionRuntime`] is constructed per session, at `create_session`
//! time, and the same `Arc` is handed out again on every later
//! `open_session` — see `service.rs`'s module doc for why. This matters here
//! because [`AgentHarness::subscribe`] has no unsubscribe mechanism: a
//! fresh-wrapper-per-acquire design would leak one permanent harness
//! subscription per attach cycle. Registering the fan-out listener exactly
//! ONCE, in [`AgentPiSessionRuntime::new`], means the leak cannot happen —
//! there is only ever one subscription for this harness's entire lifetime.
//!
//! **`steer`/`abort` are always `Ok(())`** (named divergence from
//! [`crate::testing::service::TestSessionRuntime`], which rejects when there
//! is no active prompt to steer/abort): `AgentHarness::steer`/`abort` are
//! synchronous and infallible — they queue a message / cancel a token
//! regardless of current phase. Reproducing the test double's busy-rejection
//! would require tracking phase transitions this wave has no independent
//! need for; steering/aborting an idle harness is simply a no-op, not an
//! error, in the harness's own model.
//!
//! **`queued_steer` is always empty in `snapshot()`** (named simplification):
//! `AgentHarness` exposes only a queued-message *count*
//! (`pending_message_count`), not the message contents, so
//! `SessionSnapshot.queued_steer` (the actual list) cannot be reconstructed;
//! `queued_steer_count` is populated from the real count.
//!
//! **`attached`/`locked` are always `false`** here: `sessions.rs`'s
//! `normalized_snapshot` overwrites both fields after calling
//! [`PiSessionRuntime::snapshot`] (see `crate::sessions` — Wave 4b), so any
//! value here is discarded by the only caller that matters.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};

use async_trait::async_trait;
use futures::future::BoxFuture;

use pirust_agent_core::harness::{user_message, AgentHarness, HarnessEvent};
use pirust_coding_agent::models::ModelSource;

use crate::agent_service::conversions::{
    entries_to_transcript, harness_error_to_pi_error, map_phase, map_thinking_level,
    map_thinking_level_to_core, model_ref,
};
use crate::errors::{PiServerError, PiServerOperationErrorCode};
use crate::protocol::schemas::{ModelRef, SessionPhase, SessionSnapshot, ThinkingLevel};
use crate::types::{
    PiSessionRuntime, PiSessionRuntimeEvent, PromptInput, RuntimeEventListener, SteerInput,
    Unsubscribe,
};

use super::conversions::agent_event_to_progress;

type HarnessHandle =
    Arc<AgentHarness<pirust_agent_core::harness::session::v4::memory::InMemorySessionStorage>>;

fn current_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn emit(listeners: &StdMutex<HashMap<u64, RuntimeEventListener>>, event: PiSessionRuntimeEvent) {
    let listeners = listeners.lock().unwrap();
    for listener in listeners.values() {
        listener(event.clone());
    }
}

pub struct AgentPiSessionRuntime {
    id: String,
    name: Option<String>,
    cwd: String,
    created_at: i64,
    updated_at: Arc<AtomicI64>,
    revision: Arc<AtomicI64>,
    harness: HarnessHandle,
    model_source: Arc<dyn ModelSource + Send + Sync>,
    listeners: Arc<StdMutex<HashMap<u64, RuntimeEventListener>>>,
    next_listener_id: AtomicU64,
    on_dispose: Arc<dyn Fn() + Send + Sync>,
}

impl AgentPiSessionRuntime {
    pub fn new(
        id: String,
        name: Option<String>,
        cwd: String,
        harness: HarnessHandle,
        model_source: Arc<dyn ModelSource + Send + Sync>,
        on_dispose: Arc<dyn Fn() + Send + Sync>,
    ) -> Arc<Self> {
        let now = current_millis();
        let runtime = Arc::new(Self {
            id,
            name,
            cwd,
            created_at: now,
            updated_at: Arc::new(AtomicI64::new(now)),
            revision: Arc::new(AtomicI64::new(0)),
            harness: harness.clone(),
            model_source,
            listeners: Arc::new(StdMutex::new(HashMap::new())),
            next_listener_id: AtomicU64::new(0),
            on_dispose,
        });

        // The one, permanent harness subscription for this session's entire
        // lifetime (see module doc).
        let listeners = runtime.listeners.clone();
        let updated_at = runtime.updated_at.clone();
        let revision = runtime.revision.clone();
        let harness_for_listener = harness.clone();
        harness.subscribe(Arc::new(move |event: HarnessEvent| {
            let listeners = listeners.clone();
            let updated_at = updated_at.clone();
            let revision = revision.clone();
            let harness = harness_for_listener.clone();
            Box::pin(async move {
                if let HarnessEvent::Loop(agent_event) = &event {
                    updated_at.store(current_millis(), Ordering::SeqCst);
                    revision.fetch_add(1, Ordering::SeqCst);
                    let fallback_model_id = harness.model().id;
                    if let Some(progress) = agent_event_to_progress(agent_event, &fallback_model_id)
                    {
                        emit(
                            &listeners,
                            PiSessionRuntimeEvent::Progress(Box::new(progress)),
                        );
                    }
                    emit(&listeners, PiSessionRuntimeEvent::Snapshot);
                }
            }) as BoxFuture<'static, ()>
        }));

        runtime
    }

    fn touch_and_emit_snapshot(&self) {
        self.updated_at.store(current_millis(), Ordering::SeqCst);
        self.revision.fetch_add(1, Ordering::SeqCst);
        emit(&self.listeners, PiSessionRuntimeEvent::Snapshot);
    }
}

#[async_trait]
impl PiSessionRuntime for AgentPiSessionRuntime {
    async fn snapshot(&self) -> SessionSnapshot {
        let model = self.harness.model();
        let fallback_model_id = model.id.clone();
        let entries = self.harness.entries().unwrap_or_default();
        SessionSnapshot {
            id: self.id.clone(),
            name: self.name.clone(),
            cwd: self.cwd.clone(),
            created_at: self.created_at,
            updated_at: self.updated_at.load(Ordering::SeqCst),
            phase: map_phase(self.harness.phase()),
            model: model_ref(&model),
            thinking_level: map_thinking_level(self.harness.thinking_level()),
            attached: false,
            locked: false,
            revision: self.revision.load(Ordering::SeqCst),
            transcript: entries_to_transcript(&entries, &fallback_model_id),
            queued_steer: Vec::new(),
            queued_steer_count: self.harness.pending_message_count() as i64,
        }
    }

    fn get_phase(&self) -> SessionPhase {
        map_phase(self.harness.phase())
    }

    async fn prompt(&self, input: PromptInput) -> Result<(), PiServerError> {
        self.harness
            .prompt(&input.text)
            .await
            .map(|_| ())
            .map_err(harness_error_to_pi_error)
    }

    async fn steer(&self, input: SteerInput) -> Result<(), PiServerError> {
        self.harness.steer(user_message(&input.text));
        self.touch_and_emit_snapshot();
        Ok(())
    }

    async fn abort(&self) -> Result<(), PiServerError> {
        self.harness.abort();
        self.touch_and_emit_snapshot();
        Ok(())
    }

    async fn set_model(&self, model: ModelRef) -> Result<(), PiServerError> {
        let resolved = self
            .model_source
            .get_model(&model.provider, &model.id)
            .cloned()
            .ok_or_else(|| {
                PiServerError::new(
                    PiServerOperationErrorCode::InvalidRequest,
                    format!("Unknown model {}/{}", model.provider, model.id),
                    None,
                )
            })?;
        self.harness.set_model(resolved);
        self.touch_and_emit_snapshot();
        Ok(())
    }

    async fn set_thinking(&self, thinking_level: ThinkingLevel) -> Result<(), PiServerError> {
        self.harness
            .set_thinking_level(map_thinking_level_to_core(thinking_level));
        self.touch_and_emit_snapshot();
        Ok(())
    }

    fn subscribe(&self, listener: RuntimeEventListener) -> Unsubscribe {
        let id = self.next_listener_id.fetch_add(1, Ordering::SeqCst);
        self.listeners.lock().unwrap().insert(id, listener);
        let listeners = self.listeners.clone();
        Box::new(move || {
            listeners.lock().unwrap().remove(&id);
        })
    }

    async fn dispose(&self) {
        (self.on_dispose)();
    }
}
