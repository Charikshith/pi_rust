//! feat-012 Wave 2 — [`RpcRuntimeHost`]: an [`AgentHarness`] plus the RPC-only
//! state Pi tracks at the `AgentSession` level (`steeringMode`/`followUpMode`/
//! `autoCompactionEnabled`/`autoRetryEnabled`) that `AgentHarness` has no home
//! for yet — named here, not silently invented as harness state.
//!
//! `session_id` has no `AgentHarness`/`V4Session` equivalent either (Pi's
//! `AgentSessionRuntime` owns it); generated once at construction from the
//! session's own UUIDv7 id generator, matching Pi's id shape.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use pirust_agent_core::harness::session::v4::types::SessionStorage as V4SessionStorage;
use pirust_agent_core::harness::AgentHarness;

use crate::models::ModelSource;
use crate::rpc::types::QueueMode;

pub struct RpcRuntimeHost<St: V4SessionStorage + Send + Sync + 'static> {
    pub harness: Arc<AgentHarness<St>>,
    pub model_source: Arc<dyn ModelSource + Send + Sync>,
    pub session_id: String,
    steering_mode: Mutex<QueueMode>,
    follow_up_mode: Mutex<QueueMode>,
    auto_compaction_enabled: AtomicBool,
    auto_retry_enabled: AtomicBool,
}

impl<St: V4SessionStorage + Send + Sync + 'static> RpcRuntimeHost<St> {
    /// Defaults match the oracle's `get_state` fixture
    /// (`tests/fixtures/pi/rpc/responses.corpus.jsonl`): `steeringMode`/
    /// `followUpMode` = `"all"`, `autoCompactionEnabled` = `true`.
    pub fn new(
        harness: Arc<AgentHarness<St>>,
        model_source: Arc<dyn ModelSource + Send + Sync>,
    ) -> Self {
        let session_id = harness.session().new_id();
        Self {
            harness,
            model_source,
            session_id,
            steering_mode: Mutex::new(QueueMode::All),
            follow_up_mode: Mutex::new(QueueMode::All),
            auto_compaction_enabled: AtomicBool::new(true),
            auto_retry_enabled: AtomicBool::new(true),
        }
    }

    pub fn steering_mode(&self) -> QueueMode {
        *self.steering_mode.lock().unwrap()
    }

    pub fn set_steering_mode(&self, mode: QueueMode) {
        *self.steering_mode.lock().unwrap() = mode;
    }

    pub fn follow_up_mode(&self) -> QueueMode {
        *self.follow_up_mode.lock().unwrap()
    }

    pub fn set_follow_up_mode(&self, mode: QueueMode) {
        *self.follow_up_mode.lock().unwrap() = mode;
    }

    pub fn auto_compaction_enabled(&self) -> bool {
        self.auto_compaction_enabled.load(Ordering::SeqCst)
    }

    pub fn set_auto_compaction_enabled(&self, enabled: bool) {
        self.auto_compaction_enabled
            .store(enabled, Ordering::SeqCst);
    }

    /// Stored but behaviorally inert this wave: no automatic-retry-after-error
    /// mechanism exists yet anywhere in the harness/loop (named residual, not
    /// silently dropped — `abort_retry` below is the same story).
    pub fn auto_retry_enabled(&self) -> bool {
        self.auto_retry_enabled.load(Ordering::SeqCst)
    }

    pub fn set_auto_retry_enabled(&self, enabled: bool) {
        self.auto_retry_enabled.store(enabled, Ordering::SeqCst);
    }
}
