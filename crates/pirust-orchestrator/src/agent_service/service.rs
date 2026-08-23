//! [`AgentServerService`] — the real [`PiServerService`], backed by
//! [`pirust_coding_agent::sdk`]'s harness-assembly machinery (feat-009 Wave
//! 6, a pirust-side addition: no Pi oracle exists for this — see the crate
//! root module doc and `plan.md`).
//!
//! **Durable-session identity (named, not silent):** a session's
//! [`AgentPiSessionRuntime`] is constructed exactly once, in
//! [`AgentServerService::create_session`], and the SAME `Arc` is returned
//! again from every later [`AgentServerService::open_session`] call — unlike
//! [`crate::testing::service::TestServerService`], which builds a fresh
//! [`crate::testing::service::TestSessionRuntime`] wrapper on every acquire
//! over a shared snapshot. Two reasons: (1) the real state of a session
//! (its `AgentHarness`, and therefore its whole message history) lives
//! entirely inside that one object — there is nothing left to "re-wrap" on a
//! later open; (2) `AgentHarness::subscribe` has no unsubscribe mechanism
//! (`runtime.rs`'s own module doc), so a fresh wrapper per acquire would
//! also mean a fresh, permanently-accumulating harness subscription per
//! acquire. The "exclusively acquired" contract
//! ([`crate::types::PiSessionRuntime`]'s own doc comment) is instead
//! enforced purely via `locked`, mirroring `TestServerService`'s own
//! lock/unlock bookkeeping but without re-creating the runtime object it
//! guards.
//!
//! **`HarnessBuilder` (named seam):** production (`main.rs`) and this
//! wave's own end-to-end test need the exact same `PiServerService`/
//! `PiSessionRuntime` code path, differing only in which `StreamFn` powers
//! each session's `AgentHarness` — a real network-calling one
//! ([`sdk::create_agent_harness_session`]) or a scripted
//! [`pirust_ai::providers::faux::Faux`] one
//! ([`sdk::assemble_agent_harness_session`], per plan.md's own stated Wave 6
//! test strategy). `HarnessBuilder` selects between the two at
//! construction, so `AgentServerService` itself never branches on
//! "are we in a test."

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};

use async_trait::async_trait;

use pirust_agent_core::agent_loop::StreamFn;
use pirust_agent_core::harness::session::v4::memory::InMemorySessionStorage;
use pirust_agent_core::harness::session::v4::session::Session as V4Session;
use pirust_agent_core::harness::session::v4::types::SessionMetadata as V4SessionMetadata;
use pirust_agent_core::harness::AgentHarness;
use pirust_coding_agent::models::ModelSource;
use pirust_coding_agent::sdk::{self, CreateAgentSessionOptions};
use pirust_coding_agent::settings::SettingsManager;

use crate::agent_service::conversions::{map_thinking_level_to_core, model_metadata};
use crate::agent_service::runtime::AgentPiSessionRuntime;
use crate::errors::{PiServerError, PiServerOperationErrorCode};
use crate::protocol::schemas::{ModelMetadata, SessionMetadata};
use crate::types::{CreateSessionOptions, PiServerService, PiSessionRuntime};

fn current_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Selects which `StreamFn` powers every session's `AgentHarness` (see
/// module doc).
pub enum HarnessBuilder {
    /// Production: a real, network-calling stream function built internally
    /// by [`sdk::create_agent_harness_session`] from `auth_path`/`settings`.
    Real,
    /// Tests: a pre-built, scripted stream function (typically backed by
    /// [`pirust_ai::providers::faux::Faux`]) passed straight to
    /// [`sdk::assemble_agent_harness_session`].
    Faux(StreamFn),
}

pub struct AgentServerService {
    model_source: Arc<dyn ModelSource + Send + Sync>,
    builder: HarnessBuilder,
    auth_path: PathBuf,
    settings: Arc<SettingsManager>,
    default_cwd: String,
    sessions: StdMutex<HashMap<String, Arc<AgentPiSessionRuntime>>>,
    locked: Arc<StdMutex<HashSet<String>>>,
}

impl AgentServerService {
    pub fn new(
        model_source: Arc<dyn ModelSource + Send + Sync>,
        builder: HarnessBuilder,
        auth_path: PathBuf,
        settings: Arc<SettingsManager>,
        default_cwd: String,
    ) -> Self {
        Self {
            model_source,
            builder,
            auth_path,
            settings,
            default_cwd,
            sessions: StdMutex::new(HashMap::new()),
            locked: Arc::new(StdMutex::new(HashSet::new())),
        }
    }
}

#[async_trait]
impl PiServerService for AgentServerService {
    async fn list_sessions(&self) -> Result<Vec<SessionMetadata>, PiServerError> {
        let runtimes: Vec<Arc<AgentPiSessionRuntime>> =
            self.sessions.lock().unwrap().values().cloned().collect();
        let mut result = Vec::with_capacity(runtimes.len());
        for runtime in runtimes {
            let snapshot = runtime.snapshot().await;
            result.push(SessionMetadata {
                id: snapshot.id,
                created_at: snapshot.created_at,
                updated_at: Some(snapshot.updated_at),
                parent_session_id: None,
                session_name: snapshot.name,
                cwd: Some(snapshot.cwd),
            });
        }
        Ok(result)
    }

    async fn list_models(&self) -> Result<Vec<ModelMetadata>, PiServerError> {
        Ok(self
            .model_source
            .get_models()
            .iter()
            .map(|model| {
                model_metadata(
                    model,
                    self.model_source.has_configured_auth(&model.provider.0),
                )
            })
            .collect())
    }

    async fn create_session(
        &self,
        options: CreateSessionOptions,
    ) -> Result<Arc<dyn PiSessionRuntime>, PiServerError> {
        if self.sessions.lock().unwrap().contains_key(&options.id) {
            return Err(PiServerError::session_locked(
                Some("Session already exists".to_string()),
                None,
            ));
        }

        let cwd = options
            .cwd
            .clone()
            .unwrap_or_else(|| self.default_cwd.clone());
        let storage = Arc::new(InMemorySessionStorage::new(V4SessionMetadata {
            id: options.id.clone(),
            created_at: current_millis(),
            parent_session_id: None,
        }));
        let v4_session = V4Session::new(storage);

        let cli_provider = options.model.as_ref().map(|m| m.provider.as_str());
        let cli_model = options.model.as_ref().map(|m| m.id.as_str());
        let create_options = CreateAgentSessionOptions {
            cwd: &cwd,
            model_source: self.model_source.as_ref(),
            auth_path: self.auth_path.clone(),
            settings: self.settings.clone(),
            cli_provider,
            cli_model,
            tools: None,
            no_tools: false,
            exclude_tools: None,
            session_id: Some(options.id.clone()),
            runtime_api_key: None,
        };

        let assembled = match &self.builder {
            HarnessBuilder::Real => sdk::create_agent_harness_session(create_options, v4_session),
            HarnessBuilder::Faux(stream_fn) => {
                sdk::assemble_agent_harness_session(create_options, stream_fn.clone(), v4_session)
            }
        };
        let (harness, _tool_registry, _model, _thinking_level) = assembled.map_err(|message| {
            PiServerError::new(PiServerOperationErrorCode::InvalidRequest, message, None)
        })?;

        if let Some(level) = options.thinking_level {
            harness.set_thinking_level(map_thinking_level_to_core(level));
        }

        let harness: Arc<AgentHarness<InMemorySessionStorage>> = Arc::new(harness);
        let id = options.id.clone();
        let locked_for_dispose = self.locked.clone();
        let id_for_dispose = id.clone();
        let on_dispose: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            locked_for_dispose.lock().unwrap().remove(&id_for_dispose);
        });

        let runtime = AgentPiSessionRuntime::new(
            id.clone(),
            options.name.clone(),
            cwd,
            harness,
            self.model_source.clone(),
            on_dispose,
        );

        self.sessions
            .lock()
            .unwrap()
            .insert(id.clone(), runtime.clone());
        self.locked.lock().unwrap().insert(id);
        Ok(runtime as Arc<dyn PiSessionRuntime>)
    }

    async fn open_session(
        &self,
        session_id: &str,
    ) -> Result<Arc<dyn PiSessionRuntime>, PiServerError> {
        let runtime = self
            .sessions
            .lock()
            .unwrap()
            .get(session_id)
            .cloned()
            .ok_or_else(|| {
                PiServerError::not_found(Some(format!("Unknown session: {session_id}")), None)
            })?;
        let mut locked = self.locked.lock().unwrap();
        if locked.contains(session_id) {
            return Err(PiServerError::session_locked(
                Some(format!("Session is locked: {session_id}")),
                None,
            ));
        }
        locked.insert(session_id.to_string());
        Ok(runtime as Arc<dyn PiSessionRuntime>)
    }
}
