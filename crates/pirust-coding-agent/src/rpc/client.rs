//! feat-012 Wave 4 — [`RpcClient`]: a typed embedding client for `--mode rpc`,
//! port of `modes/rpc/rpc-client.ts` (601 lines).
//!
//! **Divergence from `rpc-client.ts` (named, not silent):**
//! - Pi always spawns `node <cliPath> --mode rpc ...`, because `cli.js` is a
//!   JS entry point that needs a runtime. `pirust` is a compiled binary, so
//!   [`RpcClientOptions::program`] is spawned directly — no interpreter
//!   wrapper. `cliPath` therefore has no `program`-adjacent "extra args"
//!   counterpart; use [`RpcClientOptions::args`] for anything appended after
//!   `--mode rpc [--provider ...] [--model ...]`.
//! - `stop()`'s graceful path (`kill("SIGTERM")`, 1s grace, then `SIGKILL`) is
//!   ported on Unix by shelling out to `kill -TERM <pid>` — the same
//!   `#![forbid(unsafe_code)]` constraint documented at
//!   [`pirust_tools::bash::kill_process_tree`] (no `libc`/`nix` dependency in
//!   this crate). Windows has no SIGTERM equivalent (the same gap
//!   [`crate::rpc::run::run_rpc_mode`] already documents for the *server*
//!   side), so it goes straight to the force-kill.
//! - `onEvent(listener)` (an unsubscribe-closure API) becomes
//!   [`RpcClient::subscribe`], returning a [`tokio::sync::broadcast::Receiver`]
//!   — the idiomatic Rust shape for a multi-consumer event stream. A
//!   subscriber that falls behind observes
//!   [`tokio::sync::broadcast::error::RecvError::Lagged`] instead of silently
//!   missing events (TS's plain array of listeners never drops); documented,
//!   not hidden.
//! - `handleLine`'s fallback — a `type: "response"` line whose `id` matches no
//!   pending request is forwarded to event listeners verbatim — is NOT
//!   reproduced: such a line is dropped rather than mis-delivered as an
//!   [`AgentSessionEvent`] it doesn't actually parse as. Only lines that
//!   genuinely deserialize as one of that enum's variants are broadcast.
//! - Several command response shapes (`compact`, `get_session_stats`,
//!   `bash`, `export_html`, `get_tree`) have no first-class Rust type on our
//!   side yet (`CompactionResult`/`BashResult`/`SessionTreeNode` are TS-only
//!   concepts this port hasn't given a byte-verified Rust shape); those
//!   methods return the exact server-emitted [`serde_json::Value`] instead of
//!   inventing a struct. `get_tree` additionally reuses the *flat* `Entry`
//!   list [`crate::rpc::mode::handle_command`]'s `get_tree` arm actually
//!   returns this wave (no real tree-shaped `SessionTreeNode` exists yet
//!   server-side either) — same shape as `get_entries`, not Pi's nested tree.
//! - `cycle_model`'s TS type is `{model:{provider,id}, thinkingLevel,
//!   isScoped} | null`; our host's Wave 2 `cycle_model` only cycles the model
//!   and replies with the bare [`Model`] object (see
//!   `crate::rpc::mode::handle_command`'s `CycleModel` arm) — the client
//!   types this method's success value as `Option<Model>` to match what our
//!   server actually sends, not Pi's richer shape it doesn't yet produce.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{broadcast, mpsc, oneshot, Mutex as TokioMutex};

use pirust_agent_core::harness::messages::AgentMessage;
use pirust_agent_core::harness::session::v4::types::Entry;
use pirust_ai::types::{ImageContent, Model};

use crate::print_mode::AgentSessionEvent;
use crate::rpc::jsonl::{serialize_json_line, JsonLineSplitter};
use crate::rpc::types::{QueueMode, RpcCommand, RpcSlashCommand, ThinkingLevel};

// ============================================================================
// Options
// ============================================================================

#[derive(Debug, Clone)]
pub struct RpcClientOptions {
    /// The `pirust` executable to spawn. Defaults to `"pirust"` (resolved via
    /// `PATH`, the same way `spawn("node", ...)` relies on `PATH` in TS).
    pub program: PathBuf,
    /// Working directory for the agent process.
    pub cwd: Option<PathBuf>,
    /// Extra environment variables (merged over the current process's own,
    /// like TS's `{ ...process.env, ...this.options.env }`).
    pub env: HashMap<String, String>,
    /// `--provider`.
    pub provider: Option<String>,
    /// `--model`.
    pub model: Option<String>,
    /// Additional CLI arguments, appended after `--mode rpc [--provider ...]
    /// [--model ...]`.
    pub args: Vec<String>,
}

impl Default for RpcClientOptions {
    fn default() -> Self {
        Self {
            program: PathBuf::from("pirust"),
            cwd: None,
            env: HashMap::new(),
            provider: None,
            model: None,
            args: Vec::new(),
        }
    }
}

// ============================================================================
// Errors
// ============================================================================

#[derive(Debug, Clone, thiserror::Error)]
pub enum RpcClientError {
    #[error("Client already started")]
    AlreadyStarted,
    #[error("Client not started")]
    NotStarted,
    #[error("Agent process error: {0}. Stderr: {1}")]
    Spawn(String, String),
    #[error("Agent process exited (code={code} signal={signal}). Stderr: {stderr}")]
    ProcessExited {
        code: String,
        signal: String,
        stderr: String,
    },
    #[error("Agent process stdin error: {0}. Stderr: {1}")]
    StdinError(String, String),
    #[error("Timeout waiting for response to {command}. Stderr: {stderr}")]
    Timeout { command: String, stderr: String },
    #[error("Failed to serialize command: {0}")]
    Serialize(String),
    /// A `success: false` response, or a malformed/undecodable one.
    #[error("{0}")]
    Remote(String),
}

impl RpcClientError {
    fn process_exited(code: Option<i32>, signal: Option<&str>, stderr: &str) -> Self {
        // `code=${code} signal=${signal}` (rpc-client.ts:529-531): JS renders a
        // `null` template value as the literal text "null".
        Self::ProcessExited {
            code: code.map_or_else(|| "null".to_string(), |c| c.to_string()),
            signal: signal.map_or_else(|| "null".to_string(), str::to_string),
            stderr: stderr.to_string(),
        }
    }
}

// ============================================================================
// Wire response envelope (client-side parse; server-side shape lives in
// `rpc::types::RpcResponse`, which is Serialize-only)
// ============================================================================

#[derive(Debug, Clone, Deserialize)]
struct RawResponse {
    // `id` is consumed off the raw `Value` in `dispatch_line` (to look up the
    // pending request) before this struct is even built; `command` is parsed
    // for completeness but no caller needs it back. Both kept for parity with
    // the wire shape rather than trimmed to only what's read today.
    #[allow(dead_code)]
    id: Option<String>,
    #[allow(dead_code)]
    command: Option<String>,
    #[serde(default)]
    success: Option<bool>,
    #[serde(default)]
    data: Option<Value>,
    #[serde(default)]
    error: Option<String>,
}

fn get_data<T: DeserializeOwned>(response: RawResponse) -> Result<T, RpcClientError> {
    if response.success == Some(true) {
        serde_json::from_value(response.data.unwrap_or(Value::Null))
            .map_err(|e| RpcClientError::Remote(format!("unexpected response shape: {e}")))
    } else {
        Err(RpcClientError::Remote(
            response
                .error
                .unwrap_or_else(|| "unknown error".to_string()),
        ))
    }
}

// ============================================================================
// Typed result shapes for methods whose data our server-side host already
// produces in a stable, well-known shape (see module docs for the ones that
// don't and fall back to `Value`).
// ============================================================================

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Cancelled {
    pub cancelled: bool,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct ForkOutcome {
    pub text: String,
    pub cancelled: bool,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ForkMessage {
    #[serde(rename = "entryId")]
    pub entry_id: String,
    pub text: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct CycleThinkingLevelResult {
    pub level: ThinkingLevel,
}

/// `get_state`'s response shape (`rpc-mode.ts:447-460`). A client-local type
/// rather than reusing [`crate::rpc::types::RpcSessionState`], which is
/// Serialize-only (built server-side from live [`AgentHarness`] state, not
/// meant to round-trip back in).
///
/// [`AgentHarness`]: pirust_agent_core::harness::AgentHarness
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionState {
    #[serde(default)]
    pub model: Option<Value>,
    pub thinking_level: ThinkingLevel,
    pub is_streaming: bool,
    pub is_compacting: bool,
    pub steering_mode: QueueMode,
    pub follow_up_mode: QueueMode,
    #[serde(default)]
    pub session_file: Option<String>,
    pub session_id: String,
    #[serde(default)]
    pub session_name: Option<String>,
    pub auto_compaction_enabled: bool,
    pub message_count: usize,
    pub pending_message_count: usize,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EntriesResult {
    pub entries: Vec<Entry>,
    pub leaf_id: Option<String>,
}

/// `get_tree`'s response — see the module-doc divergence note: this is the
/// same flat `Entry` list `get_entries` returns, not a nested
/// `SessionTreeNode` tree (neither side has built that shape yet).
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TreeResult {
    pub tree: Vec<Entry>,
    pub leaf_id: Option<String>,
}

// ============================================================================
// Shared child-process state
// ============================================================================

type PendingMap =
    StdMutex<HashMap<String, oneshot::Sender<Result<RawResponse, Arc<RpcClientError>>>>>;

struct Shared {
    stdin: TokioMutex<ChildStdin>,
    pending: PendingMap,
    stderr_buf: StdMutex<String>,
    exit_error: StdMutex<Option<Arc<RpcClientError>>>,
    request_id: AtomicU64,
}

struct ClientState {
    shared: Arc<Shared>,
    control_tx: mpsc::UnboundedSender<oneshot::Sender<()>>,
    reader_task: tokio::task::JoinHandle<()>,
}

// ============================================================================
// RpcClient
// ============================================================================

pub struct RpcClient {
    options: RpcClientOptions,
    state: TokioMutex<Option<ClientState>>,
    events: broadcast::Sender<Arc<AgentSessionEvent>>,
}

impl RpcClient {
    pub fn new(options: RpcClientOptions) -> Self {
        let (events, _) = broadcast::channel(1024);
        Self {
            options,
            state: TokioMutex::new(None),
            events,
        }
    }

    /// Start the RPC agent process (`start()`, rpc-client.ts:74-140).
    ///
    /// **Quirk ported verbatim, not fixed:** if the process exits within the
    /// first 100ms, TS records `exitError` and throws but never clears
    /// `this.process` — a second `start()` call still hits "Client already
    /// started", not a fresh spawn attempt. Behavior here matches: the failed
    /// state stays installed so the same quirk reproduces.
    pub async fn start(&self) -> Result<(), RpcClientError> {
        let mut guard = self.state.lock().await;
        if guard.is_some() {
            return Err(RpcClientError::AlreadyStarted);
        }

        let mut command = Command::new(&self.options.program);
        command.arg("--mode").arg("rpc");
        if let Some(provider) = &self.options.provider {
            command.arg("--provider").arg(provider);
        }
        if let Some(model) = &self.options.model {
            command.arg("--model").arg(model);
        }
        command.args(&self.options.args);
        if let Some(cwd) = &self.options.cwd {
            command.current_dir(cwd);
        }
        command.envs(&self.options.env);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let mut child = command
            .spawn()
            .map_err(|e| RpcClientError::Spawn(e.to_string(), String::new()))?;
        let stdin = child.stdin.take().expect("stdin was piped");

        let shared = Arc::new(Shared {
            stdin: TokioMutex::new(stdin),
            pending: StdMutex::new(HashMap::new()),
            stderr_buf: StdMutex::new(String::new()),
            exit_error: StdMutex::new(None),
            request_id: AtomicU64::new(0),
        });

        let (control_tx, reader_task) =
            spawn_io_tasks(child, Arc::clone(&shared), self.events.clone());

        *guard = Some(ClientState {
            shared: Arc::clone(&shared),
            control_tx,
            reader_task,
        });
        drop(guard);

        // "Wait a moment for process to initialize" (rpc-client.ts:132-139).
        tokio::time::sleep(Duration::from_millis(100)).await;
        if let Some(err) = shared.exit_error.lock().unwrap().clone() {
            return Err((*err).clone());
        }
        Ok(())
    }

    /// Stop the RPC agent process (`stop()`, rpc-client.ts:145-167).
    pub async fn stop(&self) {
        let state = self.state.lock().await.take();
        let Some(state) = state else { return };

        let (done_tx, done_rx) = oneshot::channel();
        if state.control_tx.send(done_tx).is_ok() {
            let _ = done_rx.await;
        }
        let _ = state.reader_task.await;
        state.shared.pending.lock().unwrap().clear();
    }

    /// Subscribe to agent events (`onEvent`, rpc-client.ts:172-180 — see the
    /// module-doc note on why this returns a broadcast receiver instead of
    /// taking a listener closure).
    pub fn subscribe(&self) -> broadcast::Receiver<Arc<AgentSessionEvent>> {
        self.events.subscribe()
    }

    /// Collected stderr output so far (`getStderr`, rpc-client.ts:185-187).
    pub async fn stderr(&self) -> String {
        match self.state.lock().await.as_ref() {
            Some(state) => state.shared.stderr_buf.lock().unwrap().clone(),
            None => String::new(),
        }
    }

    // =========================================================================
    // Command methods (rpc-client.ts:193-446)
    // =========================================================================

    pub async fn prompt(
        &self,
        message: impl Into<String>,
        images: Option<Vec<ImageContent>>,
    ) -> Result<(), RpcClientError> {
        self.send(RpcCommand::Prompt {
            message: message.into(),
            images: images_to_value(images),
            streaming_behavior: None,
        })
        .await?;
        Ok(())
    }

    pub async fn steer(
        &self,
        message: impl Into<String>,
        images: Option<Vec<ImageContent>>,
    ) -> Result<(), RpcClientError> {
        self.send(RpcCommand::Steer {
            message: message.into(),
            images: images_to_value(images),
        })
        .await?;
        Ok(())
    }

    pub async fn follow_up(
        &self,
        message: impl Into<String>,
        images: Option<Vec<ImageContent>>,
    ) -> Result<(), RpcClientError> {
        self.send(RpcCommand::FollowUp {
            message: message.into(),
            images: images_to_value(images),
        })
        .await?;
        Ok(())
    }

    pub async fn abort(&self) -> Result<(), RpcClientError> {
        self.send(RpcCommand::Abort).await?;
        Ok(())
    }

    pub async fn new_session(
        &self,
        parent_session: Option<String>,
    ) -> Result<Cancelled, RpcClientError> {
        get_data(self.send(RpcCommand::NewSession { parent_session }).await?)
    }

    pub async fn get_state(&self) -> Result<SessionState, RpcClientError> {
        get_data(self.send(RpcCommand::GetState).await?)
    }

    pub async fn set_model(
        &self,
        provider: impl Into<String>,
        model_id: impl Into<String>,
    ) -> Result<Model, RpcClientError> {
        get_data(
            self.send(RpcCommand::SetModel {
                provider: provider.into(),
                model_id: model_id.into(),
            })
            .await?,
        )
    }

    /// See the module-doc divergence note: our server replies with the bare
    /// [`Model`], not Pi's `{model,thinkingLevel,isScoped}` shape.
    pub async fn cycle_model(&self) -> Result<Option<Model>, RpcClientError> {
        get_data(self.send(RpcCommand::CycleModel).await?)
    }

    pub async fn get_available_models(&self) -> Result<Vec<Model>, RpcClientError> {
        #[derive(Deserialize)]
        struct Models {
            models: Vec<Model>,
        }
        get_data::<Models>(self.send(RpcCommand::GetAvailableModels).await?).map(|m| m.models)
    }

    pub async fn set_thinking_level(&self, level: ThinkingLevel) -> Result<(), RpcClientError> {
        self.send(RpcCommand::SetThinkingLevel { level }).await?;
        Ok(())
    }

    pub async fn cycle_thinking_level(
        &self,
    ) -> Result<Option<CycleThinkingLevelResult>, RpcClientError> {
        get_data(self.send(RpcCommand::CycleThinkingLevel).await?)
    }

    pub async fn get_available_thinking_levels(
        &self,
    ) -> Result<Vec<ThinkingLevel>, RpcClientError> {
        #[derive(Deserialize)]
        struct Levels {
            levels: Vec<ThinkingLevel>,
        }
        get_data::<Levels>(self.send(RpcCommand::GetAvailableThinkingLevels).await?)
            .map(|l| l.levels)
    }

    pub async fn set_steering_mode(&self, mode: QueueMode) -> Result<(), RpcClientError> {
        self.send(RpcCommand::SetSteeringMode { mode }).await?;
        Ok(())
    }

    pub async fn set_follow_up_mode(&self, mode: QueueMode) -> Result<(), RpcClientError> {
        self.send(RpcCommand::SetFollowUpMode { mode }).await?;
        Ok(())
    }

    /// No byte-verified `CompactionResult` Rust type exists yet — see module
    /// docs.
    pub async fn compact(
        &self,
        custom_instructions: Option<String>,
    ) -> Result<Value, RpcClientError> {
        get_data(
            self.send(RpcCommand::Compact {
                custom_instructions,
            })
            .await?,
        )
    }

    pub async fn set_auto_compaction(&self, enabled: bool) -> Result<(), RpcClientError> {
        self.send(RpcCommand::SetAutoCompaction { enabled }).await?;
        Ok(())
    }

    pub async fn set_auto_retry(&self, enabled: bool) -> Result<(), RpcClientError> {
        self.send(RpcCommand::SetAutoRetry { enabled }).await?;
        Ok(())
    }

    pub async fn abort_retry(&self) -> Result<(), RpcClientError> {
        self.send(RpcCommand::AbortRetry).await?;
        Ok(())
    }

    /// Not supported by our host yet (`crate::rpc::mode`'s `not_supported`
    /// arm) — always errors this wave; typed for API parity/forward-compat.
    pub async fn bash(&self, command: impl Into<String>) -> Result<Value, RpcClientError> {
        get_data(
            self.send(RpcCommand::Bash {
                command: command.into(),
                exclude_from_context: None,
            })
            .await?,
        )
    }

    pub async fn abort_bash(&self) -> Result<(), RpcClientError> {
        self.send(RpcCommand::AbortBash).await?;
        Ok(())
    }

    /// No byte-verified `SessionStats` shape on the client side yet — see
    /// module docs.
    pub async fn get_session_stats(&self) -> Result<Value, RpcClientError> {
        get_data(self.send(RpcCommand::GetSessionStats).await?)
    }

    pub async fn export_html(&self, output_path: Option<String>) -> Result<Value, RpcClientError> {
        get_data(self.send(RpcCommand::ExportHtml { output_path }).await?)
    }

    pub async fn switch_session(
        &self,
        session_path: impl Into<String>,
    ) -> Result<Cancelled, RpcClientError> {
        get_data(
            self.send(RpcCommand::SwitchSession {
                session_path: session_path.into(),
            })
            .await?,
        )
    }

    pub async fn fork(&self, entry_id: impl Into<String>) -> Result<ForkOutcome, RpcClientError> {
        get_data(
            self.send(RpcCommand::Fork {
                entry_id: entry_id.into(),
            })
            .await?,
        )
    }

    /// Named `clone_session` (not `clone`) — `Clone` is a trait method name.
    pub async fn clone_session(&self) -> Result<Cancelled, RpcClientError> {
        get_data(self.send(RpcCommand::Clone).await?)
    }

    pub async fn get_fork_messages(&self) -> Result<Vec<ForkMessage>, RpcClientError> {
        #[derive(Deserialize)]
        struct Messages {
            messages: Vec<ForkMessage>,
        }
        get_data::<Messages>(self.send(RpcCommand::GetForkMessages).await?).map(|m| m.messages)
    }

    pub async fn get_entries(
        &self,
        since: Option<String>,
    ) -> Result<EntriesResult, RpcClientError> {
        get_data(self.send(RpcCommand::GetEntries { since }).await?)
    }

    pub async fn get_tree(&self) -> Result<TreeResult, RpcClientError> {
        get_data(self.send(RpcCommand::GetTree).await?)
    }

    pub async fn get_last_assistant_text(&self) -> Result<Option<String>, RpcClientError> {
        #[derive(Deserialize)]
        struct Text {
            text: Option<String>,
        }
        get_data::<Text>(self.send(RpcCommand::GetLastAssistantText).await?).map(|t| t.text)
    }

    pub async fn set_session_name(&self, name: impl Into<String>) -> Result<(), RpcClientError> {
        self.send(RpcCommand::SetSessionName { name: name.into() })
            .await?;
        Ok(())
    }

    pub async fn get_messages(&self) -> Result<Vec<AgentMessage>, RpcClientError> {
        #[derive(Deserialize)]
        struct Messages {
            messages: Vec<AgentMessage>,
        }
        get_data::<Messages>(self.send(RpcCommand::GetMessages).await?).map(|m| m.messages)
    }

    pub async fn get_commands(&self) -> Result<Vec<RpcSlashCommand>, RpcClientError> {
        #[derive(Deserialize)]
        struct Commands {
            commands: Vec<RpcSlashCommand>,
        }
        get_data::<Commands>(self.send(RpcCommand::GetCommands).await?).map(|c| c.commands)
    }

    // =========================================================================
    // Helpers (rpc-client.ts:452-502)
    // =========================================================================

    /// Wait for the agent to become idle (`waitForIdle`, rpc-client.ts:456-471).
    pub async fn wait_for_idle(&self, timeout: Duration) -> Result<(), RpcClientError> {
        let rx = self.subscribe();
        let stderr = self.stderr().await;
        drain_until_settled(rx, timeout, stderr, "wait_for_idle")
            .await
            .map(|_| ())
    }

    /// Collect events until the agent becomes idle
    /// (`collectEvents`, rpc-client.ts:476-493).
    pub async fn collect_events(
        &self,
        timeout: Duration,
    ) -> Result<Vec<Arc<AgentSessionEvent>>, RpcClientError> {
        let rx = self.subscribe();
        let stderr = self.stderr().await;
        drain_until_settled(rx, timeout, stderr, "collect_events").await
    }

    /// Send a prompt and wait for completion, returning all events
    /// (`promptAndWait`, rpc-client.ts:498-502). Subscribes *before* sending
    /// the prompt, like TS's `collectEvents()` (which registers its listener
    /// synchronously) started ahead of `await this.prompt(...)` — otherwise
    /// events emitted between send and subscribe would be lost.
    pub async fn prompt_and_wait(
        &self,
        message: impl Into<String>,
        images: Option<Vec<ImageContent>>,
        timeout: Duration,
    ) -> Result<Vec<Arc<AgentSessionEvent>>, RpcClientError> {
        let rx = self.subscribe();
        self.prompt(message, images).await?;
        let stderr = self.stderr().await;
        drain_until_settled(rx, timeout, stderr, "prompt_and_wait").await
    }

    // =========================================================================
    // Internal
    // =========================================================================

    async fn send(&self, command: RpcCommand) -> Result<RawResponse, RpcClientError> {
        let shared = {
            let guard = self.state.lock().await;
            guard
                .as_ref()
                .map(|s| Arc::clone(&s.shared))
                .ok_or(RpcClientError::NotStarted)?
        };

        if let Some(err) = shared.exit_error.lock().unwrap().clone() {
            return Err((*err).clone());
        }

        let mut value =
            serde_json::to_value(&command).map_err(|e| RpcClientError::Serialize(e.to_string()))?;
        let command_name = value
            .get("type")
            .and_then(|t| t.as_str())
            .unwrap_or("unknown")
            .to_string();
        let id = format!(
            "req_{}",
            shared.request_id.fetch_add(1, Ordering::SeqCst) + 1
        );
        if let Value::Object(map) = &mut value {
            map.insert("id".to_string(), Value::String(id.clone()));
        }
        let line = serialize_json_line(&value);

        let (tx, rx) = oneshot::channel();
        shared.pending.lock().unwrap().insert(id.clone(), tx);

        {
            let mut stdin = shared.stdin.lock().await;
            if let Err(e) = stdin.write_all(line.as_bytes()).await {
                shared.pending.lock().unwrap().remove(&id);
                let stderr = shared.stderr_buf.lock().unwrap().clone();
                return Err(RpcClientError::StdinError(e.to_string(), stderr));
            }
        }

        match tokio::time::timeout(Duration::from_secs(30), rx).await {
            Ok(Ok(Ok(response))) => Ok(response),
            Ok(Ok(Err(err))) => Err((*err).clone()),
            Ok(Err(_recv_error)) => {
                // Sender dropped without sending: the process ended between
                // the write above and here without `record_exit` running yet
                // in some interleaving — fall back to whatever exit_error is
                // now recorded, else a generic message.
                let stderr = shared.stderr_buf.lock().unwrap().clone();
                Err(shared
                    .exit_error
                    .lock()
                    .unwrap()
                    .clone()
                    .map(|e| (*e).clone())
                    .unwrap_or_else(|| {
                        RpcClientError::Remote(format!(
                            "Agent process ended without a response. Stderr: {stderr}"
                        ))
                    }))
            }
            Err(_timeout) => {
                shared.pending.lock().unwrap().remove(&id);
                let stderr = shared.stderr_buf.lock().unwrap().clone();
                Err(RpcClientError::Timeout {
                    command: command_name,
                    stderr,
                })
            }
        }
    }
}

/// Shared tail of `waitForIdle`/`collectEvents`/`promptAndWait`: drain a
/// subscription until `agent_settled` arrives or `timeout` elapses. A lagged
/// receiver (see the module-doc divergence note) is skipped past rather than
/// treated as fatal.
async fn drain_until_settled(
    mut rx: broadcast::Receiver<Arc<AgentSessionEvent>>,
    timeout: Duration,
    stderr: String,
    label: &str,
) -> Result<Vec<Arc<AgentSessionEvent>>, RpcClientError> {
    tokio::time::timeout(timeout, async move {
        let mut events = Vec::new();
        loop {
            match rx.recv().await {
                Ok(event) => {
                    let settled = matches!(*event, AgentSessionEvent::AgentSettled);
                    events.push(event);
                    if settled {
                        return events;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return events,
            }
        }
    })
    .await
    .map_err(|_| RpcClientError::Timeout {
        command: label.to_string(),
        stderr,
    })
}

fn images_to_value(images: Option<Vec<ImageContent>>) -> Option<Value> {
    images.map(|imgs| serde_json::to_value(imgs).unwrap_or(Value::Null))
}

/// `data.type === "response" && data.id && pendingRequests.has(data.id)` ->
/// resolve the pending request; otherwise try it as an event
/// (`handleLine`, rpc-client.ts:508-527 — see the module-doc divergence note).
fn dispatch_line(
    shared: &Arc<Shared>,
    events: &broadcast::Sender<Arc<AgentSessionEvent>>,
    line: &str,
) {
    let Ok(value) = serde_json::from_str::<Value>(line) else {
        return; // "Ignore non-JSON lines" (rpc-client.ts:524-526)
    };
    if value.get("type").and_then(|t| t.as_str()) == Some("response") {
        if let Some(id) = value.get("id").and_then(|i| i.as_str()) {
            if let Some(tx) = shared.pending.lock().unwrap().remove(id) {
                let result = serde_json::from_value::<RawResponse>(value).map_err(|e| {
                    Arc::new(RpcClientError::Remote(format!(
                        "malformed response envelope: {e}"
                    )))
                });
                let _ = tx.send(result);
                return;
            }
        }
    }
    if let Ok(event) = serde_json::from_value::<AgentSessionEvent>(value) {
        let _ = events.send(Arc::new(event));
    }
}

fn record_exit(shared: &Arc<Shared>, status: std::io::Result<std::process::ExitStatus>) {
    let stderr = shared.stderr_buf.lock().unwrap().clone();
    let (code, signal) = match &status {
        Ok(exit_status) => (exit_status.code(), signal_name(exit_status)),
        Err(_) => (None, None),
    };
    let error = Arc::new(RpcClientError::process_exited(
        code,
        signal.as_deref(),
        &stderr,
    ));
    *shared.exit_error.lock().unwrap() = Some(Arc::clone(&error));
    for (_, tx) in shared.pending.lock().unwrap().drain() {
        let _ = tx.send(Err(Arc::clone(&error)));
    }
}

/// Best-effort signal NUMBER -> NAME (`NodeJS.Signals`, e.g. `"SIGTERM"`).
/// Unix-only; a process killed by signal has no exit code, only the number
/// `std::os::unix::process::ExitStatusExt::signal()` exposes.
fn signal_name(status: &std::process::ExitStatus) -> Option<String> {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        status.signal().map(|n| {
            match n {
                1 => "SIGHUP",
                2 => "SIGINT",
                3 => "SIGQUIT",
                6 => "SIGABRT",
                9 => "SIGKILL",
                11 => "SIGSEGV",
                13 => "SIGPIPE",
                15 => "SIGTERM",
                _ => return format!("SIG{n}"),
            }
            .to_string()
        })
    }
    #[cfg(not(unix))]
    {
        let _ = status;
        None
    }
}

/// Graceful-then-forced shutdown, the `stop()` half of the process lifecycle
/// (rpc-client.ts:150-163) — see the module-doc divergence note on Unix
/// signal delivery.
async fn terminate_child(child: &mut Child) {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        let _ = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("kill -TERM {pid}"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
    }
    let graceful = tokio::time::timeout(Duration::from_millis(1000), child.wait()).await;
    if graceful.is_err() {
        let _ = child.start_kill();
        let _ = child.wait().await;
    }
}

/// Spawns the stderr-collection task and the stdout-reader/exit-watcher task
/// (`attachJsonlLineReader` on stdout + the `exit`/`error` listeners,
/// rpc-client.ts:101-130). Returns the control channel `stop()` uses to ask
/// the reader task to terminate the child, and the task's join handle.
fn spawn_io_tasks(
    mut child: Child,
    shared: Arc<Shared>,
    events: broadcast::Sender<Arc<AgentSessionEvent>>,
) -> (
    mpsc::UnboundedSender<oneshot::Sender<()>>,
    tokio::task::JoinHandle<()>,
) {
    if let Some(mut stderr) = child.stderr.take() {
        let shared_for_stderr = Arc::clone(&shared);
        tokio::spawn(async move {
            let mut buf = [0u8; 8192];
            loop {
                match stderr.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let text = String::from_utf8_lossy(&buf[..n]);
                        shared_for_stderr.stderr_buf.lock().unwrap().push_str(&text);
                        eprint!("{text}");
                    }
                }
            }
        });
    }

    let (control_tx, mut control_rx) = mpsc::unbounded_channel::<oneshot::Sender<()>>();
    let reader_task = tokio::spawn(async move {
        let mut stdout = match child.stdout.take() {
            Some(s) => s,
            None => return,
        };
        let mut splitter = JsonLineSplitter::new();
        let mut buf = [0u8; 8192];
        loop {
            tokio::select! {
                biased;
                Some(done) = control_rx.recv() => {
                    terminate_child(&mut child).await;
                    let _ = done.send(());
                    break;
                }
                read = stdout.read(&mut buf) => {
                    match read {
                        Ok(0) | Err(_) => {
                            if let Some(line) = splitter.finish() {
                                dispatch_line(&shared, &events, &line);
                            }
                            let status = child.wait().await;
                            record_exit(&shared, status);
                            break;
                        }
                        Ok(n) => {
                            for line in splitter.push(&buf[..n]) {
                                dispatch_line(&shared, &events, &line);
                            }
                        }
                    }
                }
                status = child.wait() => {
                    record_exit(&shared, status);
                    break;
                }
            }
        }
    });

    (control_tx, reader_task)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `clone()` (rpc-client.ts:388-391): `send({ type: "clone" })` — no other
    /// keys. Exercises the same `serde_json::to_value` + `id`-insertion path
    /// `send()` uses, without spawning a process.
    #[test]
    fn clone_command_serializes_to_bare_type_plus_id() {
        let mut value = serde_json::to_value(RpcCommand::Clone).unwrap();
        assert_eq!(value, serde_json::json!({ "type": "clone" }));
        value
            .as_object_mut()
            .unwrap()
            .insert("id".to_string(), Value::String("req_1".to_string()));
        assert_eq!(value, serde_json::json!({ "type": "clone", "id": "req_1" }));
    }

    /// `prompt(message)` with no images: TS's `JSON.stringify` omits the
    /// `undefined` `images`/`streamingBehavior` keys entirely — this pins
    /// that the `skip_serializing_if` added in Wave 4 actually fires.
    #[test]
    fn prompt_without_images_omits_optional_keys() {
        let value = serde_json::to_value(RpcCommand::Prompt {
            message: "hi".to_string(),
            images: None,
            streaming_behavior: None,
        })
        .unwrap();
        assert_eq!(
            value,
            serde_json::json!({ "type": "prompt", "message": "hi" })
        );
    }

    #[test]
    fn process_exited_renders_null_like_js_template_literals() {
        let err = RpcClientError::process_exited(None, None, "");
        assert_eq!(
            err.to_string(),
            "Agent process exited (code=null signal=null). Stderr: "
        );
        let err = RpcClientError::process_exited(Some(43), None, "boom");
        assert_eq!(
            err.to_string(),
            "Agent process exited (code=43 signal=null). Stderr: boom"
        );
    }

    #[test]
    fn get_data_surfaces_the_remote_error_on_failure() {
        let response = RawResponse {
            id: Some("req_1".to_string()),
            command: Some("clone".to_string()),
            success: Some(false),
            data: None,
            error: Some("boom".to_string()),
        };
        let err = get_data::<Cancelled>(response).unwrap_err();
        assert_eq!(err.to_string(), "boom");
    }

    #[test]
    fn get_data_decodes_success_payload() {
        let response = RawResponse {
            id: Some("req_1".to_string()),
            command: Some("clone".to_string()),
            success: Some(true),
            data: Some(serde_json::json!({ "cancelled": true })),
            error: None,
        };
        let cancelled = get_data::<Cancelled>(response).unwrap();
        assert!(cancelled.cancelled);
    }
}
