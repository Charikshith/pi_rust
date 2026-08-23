//! Port of `packages/server/src/testing/service.ts` —
//! `TestServerService`/`TestSessionRuntime`, the in-memory reference double
//! real Pi's own `sessions.test.ts`/`server.test.ts` battery is built on.
//!
//! **Locking (named, not silent):** every field here uses a plain
//! `std::sync::Mutex`, not `tokio::sync::Mutex` — `PiSessionRuntime::get_phase`
//! is a *synchronous* trait method (matching TS's synchronous `getPhase()`),
//! called from `sessions.rs`'s hot dispose-gate path, so the backing state it
//! reads cannot be behind an async-only lock.
//!
//! **`Deferred<T>` -> `oneshot` (named, not silent):** TS's `pendingPrompt.done:
//! Deferred<"complete"|"aborted">` is resolved exactly once, by whichever of
//! `abort()`/`finishPrompt()` happens first — a `tokio::sync::oneshot`
//! channel is the direct Rust equivalent of a promise resolved from outside
//! the function awaiting it.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::{oneshot, Notify};

use crate::errors::PiServerError;
use crate::protocol::schemas::{
    AssistantContent, AssistantTranscriptItem, AssistantTranscriptItemCommon, CompleteStopReason,
    ModelCost, ModelInputKind, ModelMetadata, ModelRef, SessionMetadata, SessionPhase,
    SessionSnapshot, TextOrImageContent, ThinkingLevel, TranscriptItem, TranscriptProgress,
    UserTranscriptItem,
};
use crate::types::{
    CreateSessionOptions, PiServerService, PiSessionRuntime, PiSessionRuntimeEvent, PromptInput,
    RuntimeEventListener, Unsubscribe,
};

pub fn test_model() -> ModelMetadata {
    ModelMetadata {
        provider: "test".to_string(),
        id: "small".to_string(),
        name: "Test Small".to_string(),
        api: "test-api".to_string(),
        reasoning: true,
        input: vec![ModelInputKind::Text, ModelInputKind::Image],
        context_window: 16_000,
        max_tokens: 2_000,
        cost: ModelCost {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
        },
        supported_thinking_levels: vec![
            ThinkingLevel::Off,
            ThinkingLevel::Medium,
            ThinkingLevel::High,
        ],
        authenticated: true,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PromptOutcome {
    Complete,
    Aborted,
}

pub struct TestSessionRuntime {
    stored: Arc<std::sync::Mutex<SessionSnapshot>>,
    on_dispose: Arc<dyn Fn() + Send + Sync>,
    listeners: Arc<std::sync::Mutex<HashMap<u64, RuntimeEventListener>>>,
    next_listener_id: AtomicU64,
    pending_prompt: std::sync::Mutex<Option<oneshot::Sender<PromptOutcome>>>,
    dispose_count: AtomicI64,
    steers: std::sync::Mutex<Vec<PromptInput>>,
}

impl TestSessionRuntime {
    fn new(
        stored: Arc<std::sync::Mutex<SessionSnapshot>>,
        on_dispose: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        Self {
            stored,
            on_dispose,
            listeners: Arc::new(std::sync::Mutex::new(HashMap::new())),
            next_listener_id: AtomicU64::new(0),
            pending_prompt: std::sync::Mutex::new(None),
            dispose_count: AtomicI64::new(0),
            steers: std::sync::Mutex::new(Vec::new()),
        }
    }

    fn emit(&self, event: PiSessionRuntimeEvent) {
        let listeners = self.listeners.lock().unwrap();
        for listener in listeners.values() {
            listener(event.clone());
        }
    }

    fn update(&self, mutate: impl FnOnce(&mut SessionSnapshot)) {
        {
            let mut g = self.stored.lock().unwrap();
            mutate(&mut g);
            g.revision += 1;
            g.updated_at += 1;
        }
        self.emit(PiSessionRuntimeEvent::Snapshot);
    }

    // Test-only helpers (not part of `PiSessionRuntime`) mirroring TS's own
    // `setPhase`/`finishPrompt`/`emitProgress`/`emitError`/`emitSnapshot`.

    pub fn set_phase(&self, phase: SessionPhase) {
        self.stored.lock().unwrap().phase = phase;
    }

    pub fn finish_prompt(&self) {
        let tx = self
            .pending_prompt
            .lock()
            .unwrap()
            .take()
            .expect("no prompt is pending");
        let _ = tx.send(PromptOutcome::Complete);
    }

    pub fn emit_progress(&self, progress: TranscriptProgress) {
        self.emit(PiSessionRuntimeEvent::Progress(Box::new(progress)));
    }

    pub fn emit_error(&self, error: PiServerError) {
        self.emit(PiSessionRuntimeEvent::Error(error));
    }

    pub fn emit_snapshot(&self) {
        self.emit(PiSessionRuntimeEvent::Snapshot);
    }

    pub fn dispose_count(&self) -> i64 {
        self.dispose_count.load(Ordering::SeqCst)
    }

    pub fn steers(&self) -> Vec<PromptInput> {
        self.steers.lock().unwrap().clone()
    }
}

#[async_trait]
impl PiSessionRuntime for TestSessionRuntime {
    async fn snapshot(&self) -> SessionSnapshot {
        self.stored.lock().unwrap().clone()
    }

    fn get_phase(&self) -> SessionPhase {
        self.stored.lock().unwrap().phase
    }

    async fn prompt(&self, input: PromptInput) -> Result<(), PiServerError> {
        if self.get_phase() != SessionPhase::Idle {
            return Err(PiServerError::busy(
                Some("A prompt is already running".to_string()),
                None,
            ));
        }
        let (tx, rx) = oneshot::channel();
        *self.pending_prompt.lock().unwrap() = Some(tx);
        let next_revision = self.stored.lock().unwrap().revision + 1;
        let text = input.text.clone();
        self.update(|s| {
            s.transcript.push(TranscriptItem::User(UserTranscriptItem {
                id: format!("user-{next_revision}"),
                content: vec![TextOrImageContent::Text { text }],
                timestamp: next_revision,
            }));
            s.phase = SessionPhase::Turn;
        });
        let outcome = rx.await.unwrap_or(PromptOutcome::Aborted);
        let (model, assistant_revision) = {
            let g = self.stored.lock().unwrap();
            (g.model.clone(), g.revision + 1)
        };
        let assistant = match outcome {
            PromptOutcome::Complete => AssistantTranscriptItem::Complete {
                common: AssistantTranscriptItemCommon {
                    id: format!("assistant-{assistant_revision}"),
                    content: vec![AssistantContent::Text {
                        text: format!("reply:{}", input.text),
                    }],
                    model,
                    response_model: None,
                    usage: None,
                    timestamp: assistant_revision,
                },
                stop_reason: CompleteStopReason::Stop,
            },
            PromptOutcome::Aborted => AssistantTranscriptItem::Aborted {
                common: AssistantTranscriptItemCommon {
                    id: format!("assistant-{assistant_revision}"),
                    content: vec![AssistantContent::Text {
                        text: String::new(),
                    }],
                    model,
                    response_model: None,
                    usage: None,
                    timestamp: assistant_revision,
                },
                error_message: None,
            },
        };
        self.update(|s| {
            s.transcript.push(TranscriptItem::Assistant(assistant));
            s.phase = SessionPhase::Idle;
        });
        *self.pending_prompt.lock().unwrap() = None;
        Ok(())
    }

    async fn steer(&self, input: PromptInput) -> Result<(), PiServerError> {
        if self.get_phase() == SessionPhase::Idle {
            return Err(PiServerError::busy(
                Some("There is no active prompt to steer".to_string()),
                None,
            ));
        }
        self.steers.lock().unwrap().push(input.clone());
        let next_revision = self.stored.lock().unwrap().revision + 1;
        let text = input.text.clone();
        self.update(|s| {
            s.queued_steer_count += 1;
            s.queued_steer.push(UserTranscriptItem {
                id: format!("steer-{next_revision}"),
                content: vec![TextOrImageContent::Text { text }],
                timestamp: next_revision,
            });
        });
        Ok(())
    }

    async fn abort(&self) -> Result<(), PiServerError> {
        let tx = self.pending_prompt.lock().unwrap().take();
        match tx {
            Some(tx) => {
                let _ = tx.send(PromptOutcome::Aborted);
                Ok(())
            }
            None => Err(PiServerError::busy(
                Some("There is no active prompt to abort".to_string()),
                None,
            )),
        }
    }

    async fn set_model(&self, model: ModelRef) -> Result<(), PiServerError> {
        if self.get_phase() != SessionPhase::Idle {
            return Err(PiServerError::busy(
                Some("Session is busy".to_string()),
                None,
            ));
        }
        self.update(|s| s.model = model);
        Ok(())
    }

    async fn set_thinking(&self, thinking_level: ThinkingLevel) -> Result<(), PiServerError> {
        if self.get_phase() != SessionPhase::Idle {
            return Err(PiServerError::busy(
                Some("Session is busy".to_string()),
                None,
            ));
        }
        self.update(|s| s.thinking_level = thinking_level);
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
        self.dispose_count.fetch_add(1, Ordering::SeqCst);
        (self.on_dispose)();
    }
}

/// Handed back by [`TestServerService::delay_next_list`] so a test can
/// synchronize with `list_sessions()` having actually entered its delay
/// window before releasing it — mirrors TS's `entered`/`release` `Deferred`
/// pair. `Notify::notify_one`/`notified` store one permit for exactly this
/// "signal may arrive before the waiter" ordering, unlike `notify_waiters`.
pub struct ListDelay {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

impl ListDelay {
    pub async fn wait_entered(&self) {
        self.entered.notified().await;
    }

    pub fn release(&self) {
        self.release.notify_one();
    }
}

pub struct TestServerService {
    sessions: std::sync::Mutex<HashMap<String, Arc<std::sync::Mutex<SessionSnapshot>>>>,
    runtimes: std::sync::Mutex<HashMap<String, Vec<Arc<TestSessionRuntime>>>>,
    locked: Arc<std::sync::Mutex<HashSet<String>>>,
    last_created_id: std::sync::Mutex<Option<String>>,
    next_list_delay: std::sync::Mutex<Option<(Arc<Notify>, Arc<Notify>)>>,
}

impl Default for TestServerService {
    fn default() -> Self {
        Self::new()
    }
}

impl TestServerService {
    pub fn new() -> Self {
        Self {
            sessions: std::sync::Mutex::new(HashMap::new()),
            runtimes: std::sync::Mutex::new(HashMap::new()),
            locked: Arc::new(std::sync::Mutex::new(HashSet::new())),
            last_created_id: std::sync::Mutex::new(None),
            next_list_delay: std::sync::Mutex::new(None),
        }
    }

    pub fn last_created_id(&self) -> Option<String> {
        self.last_created_id.lock().unwrap().clone()
    }

    pub fn delay_next_list(&self) -> ListDelay {
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        *self.next_list_delay.lock().unwrap() = Some((entered.clone(), release.clone()));
        ListDelay { entered, release }
    }

    pub fn latest_runtime(&self, id: &str) -> Arc<TestSessionRuntime> {
        self.runtimes
            .lock()
            .unwrap()
            .get(id)
            .and_then(|v| v.last().cloned())
            .unwrap_or_else(|| panic!("No runtime for {id}"))
    }

    /// Seeds a durable session record, matching TS's `seed()` defaults
    /// (`id = "session-1"`, `name = Session ${id}`,
    /// `cwd = /tmp/pi-server-conformance`, `model = {test, small}`,
    /// `thinkingLevel = off`) — Rust has no default-argument sugar, so
    /// [`Self::seed_default`] reproduces the zero-argument call shape.
    pub fn seed(
        &self,
        id: &str,
        name: Option<&str>,
        cwd: Option<&str>,
        model: Option<ModelRef>,
        thinking_level: Option<ThinkingLevel>,
    ) {
        let snapshot = SessionSnapshot {
            id: id.to_string(),
            name: Some(
                name.map(|s| s.to_string())
                    .unwrap_or_else(|| format!("Session {id}")),
            ),
            cwd: cwd
                .map(|s| s.to_string())
                .unwrap_or_else(|| "/tmp/pi-server-conformance".to_string()),
            created_at: 1,
            updated_at: 1,
            phase: SessionPhase::Idle,
            model: model.unwrap_or_else(|| ModelRef {
                provider: "test".to_string(),
                id: "small".to_string(),
            }),
            thinking_level: thinking_level.unwrap_or(ThinkingLevel::Off),
            attached: false,
            locked: false,
            revision: 0,
            transcript: vec![],
            queued_steer: vec![],
            queued_steer_count: 0,
        };
        self.sessions
            .lock()
            .unwrap()
            .insert(id.to_string(), Arc::new(std::sync::Mutex::new(snapshot)));
    }

    pub fn seed_default(&self) {
        self.seed("session-1", None, None, None, None);
    }

    fn acquire(&self, id: &str) -> Arc<TestSessionRuntime> {
        let stored = self
            .sessions
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .unwrap_or_else(|| panic!("Unknown session: {id}"));
        self.locked.lock().unwrap().insert(id.to_string());
        let locked = self.locked.clone();
        let id_owned = id.to_string();
        let runtime = Arc::new(TestSessionRuntime::new(
            stored,
            Arc::new(move || {
                locked.lock().unwrap().remove(&id_owned);
            }),
        ));
        self.runtimes
            .lock()
            .unwrap()
            .entry(id.to_string())
            .or_default()
            .push(runtime.clone());
        runtime
    }
}

#[async_trait]
impl PiServerService for TestServerService {
    async fn list_sessions(&self) -> Result<Vec<SessionMetadata>, PiServerError> {
        let delay = self.next_list_delay.lock().unwrap().take();
        if let Some((entered, release)) = delay {
            entered.notify_one();
            release.notified().await;
        }
        let sessions = self.sessions.lock().unwrap();
        Ok(sessions
            .values()
            .map(|s| {
                let g = s.lock().unwrap();
                SessionMetadata {
                    id: g.id.clone(),
                    created_at: g.created_at,
                    updated_at: Some(g.updated_at),
                    parent_session_id: None,
                    session_name: g.name.clone(),
                    cwd: Some(g.cwd.clone()),
                }
            })
            .collect())
    }

    async fn list_models(&self) -> Result<Vec<ModelMetadata>, PiServerError> {
        Ok(vec![test_model()])
    }

    async fn create_session(
        &self,
        options: CreateSessionOptions,
    ) -> Result<Arc<dyn PiSessionRuntime>, PiServerError> {
        *self.last_created_id.lock().unwrap() = Some(options.id.clone());
        if self.sessions.lock().unwrap().contains_key(&options.id) {
            return Err(PiServerError::session_locked(
                Some("Session already exists".to_string()),
                None,
            ));
        }
        self.seed(
            &options.id,
            options.name.as_deref(),
            options.cwd.as_deref(),
            options.model.clone(),
            options.thinking_level,
        );
        Ok(self.acquire(&options.id))
    }

    async fn open_session(
        &self,
        session_id: &str,
    ) -> Result<Arc<dyn PiSessionRuntime>, PiServerError> {
        if !self.sessions.lock().unwrap().contains_key(session_id) {
            return Err(PiServerError::not_found(
                Some(format!("Unknown session: {session_id}")),
                None,
            ));
        }
        if self.locked.lock().unwrap().contains(session_id) {
            return Err(PiServerError::session_locked(
                Some(format!("Session is locked: {session_id}")),
                None,
            ));
        }
        Ok(self.acquire(session_id))
    }
}
