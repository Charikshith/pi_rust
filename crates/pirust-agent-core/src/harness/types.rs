//! Foundational harness types — port of `packages/agent/src/harness/types.ts`.
//!
//! Spec: `docs/analysis/07-agent-core-spec.md` §1.4 (session tree entries + exact
//! v3 key order), §1.5 (Result & error taxonomy), §8 (`FileSystem` / `Shell` /
//! `ExecutionEnv` / `SessionStorage` / `SessionRepo` trait boundaries).
//!
//! `[LEAF, foundational]` (§13): every other harness module depends on this one.
//!
//! Byte contract for [`SessionTreeEntry`] and [`SessionHeader`]:
//! - every entry serializes `type, id, parentId, timestamp, <variant fields>` in
//!   the documented order (§1.4). Modelled as a `#[serde(tag = "type")]`
//!   internally-tagged enum: serde emits the tag first, then struct fields in
//!   declaration order.
//! - `undefined` fields are omitted (`skip_serializing_if = "Option::is_none"`);
//! - `parentId` and `leaf.targetId` are explicit `null` (Option WITHOUT skip);
//! - `label.label` / `session_info.name` are `undefined`-omitted (Option WITH skip).
//! - arbitrary maps (`custom.data`, `details`, header `metadata`) preserve key
//!   order via serde_json's `preserve_order` feature (enabled in this crate).

use async_trait::async_trait;
use pirust_ai::types::UserMessageContent;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use super::messages::AgentMessage;

// ===========================================================================
// §1.5 Result & error taxonomy
// ===========================================================================
//
// Pi's `Result<V, E>` maps to Rust's native `Result`; the `ok`/`err`/`getOrThrow`
// helpers are not needed. Each error class carries a stable `code` string that
// callers match on — modelled as a `*Code` enum with `snake_case` serde renaming
// (which reproduces every code string verbatim) plus an `as_str` accessor.

/// Stable [`FileSystem`] error codes (types.ts:111-119).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileErrorCode {
    Aborted,
    NotFound,
    PermissionDenied,
    NotDirectory,
    IsDirectory,
    Invalid,
    NotSupported,
    Unknown,
}

impl FileErrorCode {
    /// Exact wire string callers match on.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Aborted => "aborted",
            Self::NotFound => "not_found",
            Self::PermissionDenied => "permission_denied",
            Self::NotDirectory => "not_directory",
            Self::IsDirectory => "is_directory",
            Self::Invalid => "invalid",
            Self::NotSupported => "not_supported",
            Self::Unknown => "unknown",
        }
    }
}

/// Error returned by [`FileSystem`] operations (types.ts:121-134).
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct FileError {
    /// Backend-independent error code.
    pub code: FileErrorCode,
    /// Human-readable message.
    pub message: String,
    /// Absolute addressed path associated with the failure, when available.
    pub path: Option<String>,
    /// Underlying cause, when available.
    #[source]
    pub source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl FileError {
    /// Construct a [`FileError`] with the given code and message.
    pub fn new(code: FileErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            path: None,
            source: None,
        }
    }

    /// Attach the addressed path.
    pub fn with_path(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }
}

/// Stable [`ExecutionEnv`] exec error codes (types.ts:137-143).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionErrorCode {
    Aborted,
    Timeout,
    ShellUnavailable,
    SpawnError,
    CallbackError,
    Unknown,
}

impl ExecutionErrorCode {
    /// Exact wire string callers match on.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Aborted => "aborted",
            Self::Timeout => "timeout",
            Self::ShellUnavailable => "shell_unavailable",
            Self::SpawnError => "spawn_error",
            Self::CallbackError => "callback_error",
            Self::Unknown => "unknown",
        }
    }
}

/// Error returned by [`Shell::exec`] (types.ts:145-155).
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct ExecutionError {
    /// Backend-independent error code.
    pub code: ExecutionErrorCode,
    /// Human-readable message.
    pub message: String,
    /// Underlying cause, when available.
    #[source]
    pub source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl ExecutionError {
    /// Construct an [`ExecutionError`] with the given code and message.
    pub fn new(code: ExecutionErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            source: None,
        }
    }
}

/// Stable compaction error codes (types.ts:158).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactionErrorCode {
    Aborted,
    SummarizationFailed,
    InvalidSession,
    Unknown,
}

impl CompactionErrorCode {
    /// Exact wire string callers match on.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Aborted => "aborted",
            Self::SummarizationFailed => "summarization_failed",
            Self::InvalidSession => "invalid_session",
            Self::Unknown => "unknown",
        }
    }
}

/// Error returned by compaction helpers (types.ts:160-170).
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct CompactionError {
    /// Backend-independent error code.
    pub code: CompactionErrorCode,
    /// Human-readable message.
    pub message: String,
    /// Underlying cause, when available.
    #[source]
    pub source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl CompactionError {
    /// Construct a [`CompactionError`] with the given code and message.
    pub fn new(code: CompactionErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            source: None,
        }
    }
}

/// Stable branch-summary error codes (types.ts:173).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BranchSummaryErrorCode {
    Aborted,
    SummarizationFailed,
    InvalidSession,
}

impl BranchSummaryErrorCode {
    /// Exact wire string callers match on.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Aborted => "aborted",
            Self::SummarizationFailed => "summarization_failed",
            Self::InvalidSession => "invalid_session",
        }
    }
}

/// Error returned by branch summarization helpers (types.ts:175-185).
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct BranchSummaryError {
    /// Backend-independent error code.
    pub code: BranchSummaryErrorCode,
    /// Human-readable message.
    pub message: String,
    /// Underlying cause, when available.
    #[source]
    pub source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl BranchSummaryError {
    /// Construct a [`BranchSummaryError`] with the given code and message.
    pub fn new(code: BranchSummaryErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            source: None,
        }
    }
}

/// Session subsystem error codes (types.ts:187-193).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionErrorCode {
    NotFound,
    InvalidSession,
    InvalidEntry,
    InvalidForkTarget,
    Storage,
    Unknown,
    /// 0.84.2 v4 session codes (session/types.ts `SessionErrorCode`).
    AlreadyExists,
    InvalidPayload,
    InvalidLane,
    InvalidQuery,
}

impl SessionErrorCode {
    /// Exact wire string callers match on.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::NotFound => "not_found",
            Self::InvalidSession => "invalid_session",
            Self::InvalidEntry => "invalid_entry",
            Self::InvalidForkTarget => "invalid_fork_target",
            Self::Storage => "storage",
            Self::Unknown => "unknown",
            Self::AlreadyExists => "already_exists",
            Self::InvalidPayload => "invalid_payload",
            Self::InvalidLane => "invalid_lane",
            Self::InvalidQuery => "invalid_query",
        }
    }
}

/// Error thrown by session storage, repositories, and tree operations
/// (types.ts:195-205).
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct SessionError {
    /// Session subsystem error code.
    pub code: SessionErrorCode,
    /// Human-readable message.
    pub message: String,
    /// Underlying cause, when available.
    #[source]
    pub source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl SessionError {
    /// Construct a [`SessionError`] with the given code and message.
    pub fn new(code: SessionErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            source: None,
        }
    }
}

/// Top-level [`AgentHarness`](super) failure classification (types.ts:207-216).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentHarnessErrorCode {
    Busy,
    InvalidState,
    InvalidArgument,
    Session,
    Hook,
    Auth,
    Compaction,
    BranchSummary,
    Unknown,
}

impl AgentHarnessErrorCode {
    /// Exact wire string callers match on.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Busy => "busy",
            Self::InvalidState => "invalid_state",
            Self::InvalidArgument => "invalid_argument",
            Self::Session => "session",
            Self::Hook => "hook",
            Self::Auth => "auth",
            Self::Compaction => "compaction",
            Self::BranchSummary => "branch_summary",
            Self::Unknown => "unknown",
        }
    }
}

/// Public [`AgentHarness`](super) failure (types.ts:218-227).
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct AgentHarnessError {
    /// Stable top-level classification.
    pub code: AgentHarnessErrorCode,
    /// Human-readable message.
    pub message: String,
    /// Underlying cause, when available.
    #[source]
    pub source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl AgentHarnessError {
    /// Construct an [`AgentHarnessError`] with the given code and message.
    pub fn new(code: AgentHarnessErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            source: None,
        }
    }
}

// ===========================================================================
// §1.4 Session tree entries (v3)
// ===========================================================================

/// Append-only session-tree entry (types.ts:334-420). Internally tagged on
/// `type`; serialization emits `type, id, parentId, timestamp, <variant fields>`
/// with the exact §1.4 key order per variant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
#[allow(clippy::large_enum_variant)] // `Message` variant carries the full AgentMessage (see mod.rs:92 precedent)
pub enum SessionTreeEntry {
    /// A conversation message (types.ts:341-344).
    #[serde(rename = "message")]
    Message {
        id: String,
        #[serde(rename = "parentId")]
        parent_id: Option<String>,
        timestamp: String,
        message: AgentMessage,
    },
    /// Thinking-level change (types.ts:346-349).
    #[serde(rename = "thinking_level_change")]
    ThinkingLevelChange {
        id: String,
        #[serde(rename = "parentId")]
        parent_id: Option<String>,
        timestamp: String,
        #[serde(rename = "thinkingLevel")]
        thinking_level: String,
    },
    /// Model change (types.ts:351-355).
    #[serde(rename = "model_change")]
    ModelChange {
        id: String,
        #[serde(rename = "parentId")]
        parent_id: Option<String>,
        timestamp: String,
        provider: String,
        #[serde(rename = "modelId")]
        model_id: String,
    },
    /// Active-tools change (types.ts:357-360).
    #[serde(rename = "active_tools_change")]
    ActiveToolsChange {
        id: String,
        #[serde(rename = "parentId")]
        parent_id: Option<String>,
        timestamp: String,
        #[serde(rename = "activeToolNames")]
        active_tool_names: Vec<String>,
    },
    /// Compaction record (types.ts:362-369).
    #[serde(rename = "compaction")]
    Compaction {
        id: String,
        #[serde(rename = "parentId")]
        parent_id: Option<String>,
        timestamp: String,
        summary: String,
        #[serde(rename = "firstKeptEntryId")]
        first_kept_entry_id: String,
        #[serde(rename = "tokensBefore")]
        tokens_before: i64,
        #[serde(skip_serializing_if = "Option::is_none")]
        details: Option<Value>,
        #[serde(rename = "fromHook", skip_serializing_if = "Option::is_none")]
        from_hook: Option<bool>,
    },
    /// Branch summary (types.ts:371-377).
    #[serde(rename = "branch_summary")]
    BranchSummary {
        id: String,
        #[serde(rename = "parentId")]
        parent_id: Option<String>,
        timestamp: String,
        #[serde(rename = "fromId")]
        from_id: String,
        summary: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        details: Option<Value>,
        #[serde(rename = "fromHook", skip_serializing_if = "Option::is_none")]
        from_hook: Option<bool>,
    },
    /// Application-defined entry (types.ts:379-383).
    #[serde(rename = "custom")]
    Custom {
        id: String,
        #[serde(rename = "parentId")]
        parent_id: Option<String>,
        timestamp: String,
        #[serde(rename = "customType")]
        custom_type: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        data: Option<Value>,
    },
    /// Application-defined message entry (types.ts:385-391).
    #[serde(rename = "custom_message")]
    CustomMessage {
        id: String,
        #[serde(rename = "parentId")]
        parent_id: Option<String>,
        timestamp: String,
        #[serde(rename = "customType")]
        custom_type: String,
        /// `string | (TextContent | ImageContent)[]`.
        content: UserMessageContent,
        display: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        details: Option<Value>,
    },
    /// Label on another entry (types.ts:393-397). `label` is `string | undefined`
    /// and omitted when absent.
    #[serde(rename = "label")]
    Label {
        id: String,
        #[serde(rename = "parentId")]
        parent_id: Option<String>,
        timestamp: String,
        #[serde(rename = "targetId")]
        target_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    /// Session info / name (types.ts:399-402). Legacy `session_info` tag kept for
    /// back-compat.
    #[serde(rename = "session_info")]
    SessionInfo {
        id: String,
        #[serde(rename = "parentId")]
        parent_id: Option<String>,
        timestamp: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    /// Leaf pointer (types.ts:404-407). `targetId` is `string | null` and
    /// serialized as explicit `null` when absent.
    #[serde(rename = "leaf")]
    Leaf {
        id: String,
        #[serde(rename = "parentId")]
        parent_id: Option<String>,
        timestamp: String,
        #[serde(rename = "targetId")]
        target_id: Option<String>,
    },
}

/// Derived context state built from a session-tree path (types.ts:422-427).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionContext {
    pub messages: Vec<AgentMessage>,
    pub thinking_level: String,
    pub model: Option<SessionContextModel>,
    pub active_tool_names: Option<Vec<String>>,
}

/// `{ provider, modelId }` model reference used in [`SessionContext`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionContextModel {
    pub provider: String,
    pub model_id: String,
}

/// Minimal session metadata (types.ts:429-432).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionMetadata {
    pub id: String,
    pub created_at: String,
}

/// JSONL-backed session metadata (types.ts:434-439).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JsonlSessionMetadata {
    pub id: String,
    pub created_at: String,
    pub cwd: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_session_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
}

/// Unit tag serializing as `"session"` for the JSONL header line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum SessionHeaderTag {
    #[serde(rename = "session")]
    #[default]
    Session,
}

/// v3 JSONL header line (jsonl-storage.ts:220-231, §1.4).
///
/// Key order: `type, version, id, timestamp, cwd, parentSession?, metadata?`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionHeader {
    #[serde(rename = "type", default)]
    pub kind: SessionHeaderTag,
    /// Always `3` for the v3 format.
    pub version: u32,
    pub id: String,
    pub timestamp: String,
    pub cwd: String,
    #[serde(rename = "parentSession", skip_serializing_if = "Option::is_none")]
    pub parent_session: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
}

/// A queued session write: a [`SessionTreeEntry`] minus `id` / `parentId` /
/// `timestamp` (types.ts:496-500). The harness fills those in when flushing the
/// write at a turn boundary.
#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::large_enum_variant)] // `Message` variant carries the full AgentMessage (see mod.rs:92 precedent)
pub enum PendingSessionWrite {
    Message {
        message: AgentMessage,
    },
    ThinkingLevelChange {
        thinking_level: String,
    },
    ModelChange {
        provider: String,
        model_id: String,
    },
    ActiveToolsChange {
        active_tool_names: Vec<String>,
    },
    Compaction {
        summary: String,
        first_kept_entry_id: String,
        tokens_before: i64,
        details: Option<Value>,
        from_hook: Option<bool>,
    },
    BranchSummary {
        from_id: String,
        summary: String,
        details: Option<Value>,
        from_hook: Option<bool>,
    },
    Custom {
        custom_type: String,
        data: Option<Value>,
    },
    CustomMessage {
        custom_type: String,
        content: UserMessageContent,
        display: bool,
        details: Option<Value>,
    },
    Label {
        target_id: String,
        label: Option<String>,
    },
    SessionInfo {
        name: Option<String>,
    },
    Leaf {
        target_id: Option<String>,
    },
}

// ===========================================================================
// §8 Trait boundaries
// ===========================================================================

/// Kind of filesystem object (types.ts:107-108). Symlinks are not followed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileKind {
    File,
    Directory,
    Symlink,
}

/// Metadata for one filesystem object (types.ts:229-241).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileInfo {
    pub name: String,
    pub path: String,
    pub kind: FileKind,
    pub size: u64,
    pub mtime_ms: f64,
}

/// Content payload for a write/append: `string | Uint8Array` in TS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteContent {
    Text(String),
    Binary(Vec<u8>),
}

/// Options for [`FileSystem::read_text_lines`].
#[derive(Debug, Default, Clone)]
pub struct ReadTextLinesOptions {
    pub max_lines: Option<usize>,
    pub abort_signal: Option<CancellationToken>,
}

/// Options for [`FileSystem::create_dir`]. Defaults: `recursive: true`.
#[derive(Debug, Default, Clone)]
pub struct CreateDirOptions {
    pub recursive: Option<bool>,
    pub abort_signal: Option<CancellationToken>,
}

/// Options for [`FileSystem::remove`]. Defaults: `recursive: false`, `force: false`.
#[derive(Debug, Default, Clone)]
pub struct RemoveOptions {
    pub recursive: Option<bool>,
    pub force: Option<bool>,
    pub abort_signal: Option<CancellationToken>,
}

/// Options for [`FileSystem::create_temp_file`].
#[derive(Debug, Default, Clone)]
pub struct CreateTempFileOptions {
    pub prefix: Option<String>,
    pub suffix: Option<String>,
    pub abort_signal: Option<CancellationToken>,
}

/// Filesystem capability used by the harness (types.ts:252-302).
///
/// MUST-NOT-PANIC contract: every operation returns `Result<_, FileError>`;
/// implementations must never panic or unwind for backend failures, encoding
/// them in the returned error instead (types.ts:249-251).
#[async_trait]
pub trait FileSystem: Send + Sync {
    /// Current working directory for relative paths.
    fn cwd(&self) -> &str;

    /// Return an absolute addressed path without requiring existence.
    async fn absolute_path(
        &self,
        path: &str,
        abort_signal: Option<CancellationToken>,
    ) -> Result<String, FileError>;

    /// Join path segments in the filesystem namespace.
    async fn join_path(
        &self,
        parts: &[String],
        abort_signal: Option<CancellationToken>,
    ) -> Result<String, FileError>;

    /// Read a UTF-8 text file.
    async fn read_text_file(
        &self,
        path: &str,
        abort_signal: Option<CancellationToken>,
    ) -> Result<String, FileError>;

    /// Read UTF-8 text lines, stopping once `max_lines` is reached.
    async fn read_text_lines(
        &self,
        path: &str,
        options: ReadTextLinesOptions,
    ) -> Result<Vec<String>, FileError>;

    /// Read a binary file.
    async fn read_binary_file(
        &self,
        path: &str,
        abort_signal: Option<CancellationToken>,
    ) -> Result<Vec<u8>, FileError>;

    /// Create or overwrite a file.
    async fn write_file(
        &self,
        path: &str,
        content: WriteContent,
        abort_signal: Option<CancellationToken>,
    ) -> Result<(), FileError>;

    /// Create or append to a file.
    async fn append_file(
        &self,
        path: &str,
        content: WriteContent,
        abort_signal: Option<CancellationToken>,
    ) -> Result<(), FileError>;

    /// Return metadata for the addressed path without following symlinks.
    async fn file_info(
        &self,
        path: &str,
        abort_signal: Option<CancellationToken>,
    ) -> Result<FileInfo, FileError>;

    /// List direct children of a directory without following symlinks.
    async fn list_dir(
        &self,
        path: &str,
        abort_signal: Option<CancellationToken>,
    ) -> Result<Vec<FileInfo>, FileError>;

    /// Return the canonical path, resolving symlinks where supported.
    async fn canonical_path(
        &self,
        path: &str,
        abort_signal: Option<CancellationToken>,
    ) -> Result<String, FileError>;

    /// Return false for missing paths; other failures return a [`FileError`].
    async fn exists(
        &self,
        path: &str,
        abort_signal: Option<CancellationToken>,
    ) -> Result<bool, FileError>;

    /// Create a directory.
    async fn create_dir(&self, path: &str, options: CreateDirOptions) -> Result<(), FileError>;

    /// Remove a file or directory.
    async fn remove(&self, path: &str, options: RemoveOptions) -> Result<(), FileError>;

    /// Create a temporary directory and return its absolute path.
    async fn create_temp_dir(
        &self,
        prefix: Option<&str>,
        abort_signal: Option<CancellationToken>,
    ) -> Result<String, FileError>;

    /// Create a temporary file and return its absolute path.
    async fn create_temp_file(&self, options: CreateTempFileOptions) -> Result<String, FileError>;

    /// Release filesystem resources. Best-effort; must not panic.
    async fn cleanup(&self);
}

/// Callback invoked with an output chunk as it is produced.
pub type OutputCallback = Arc<dyn Fn(String) + Send + Sync>;

/// Options for [`Shell::exec`] (types.ts:305-318).
#[derive(Default, Clone)]
pub struct ShellExecOptions {
    /// Working directory. Defaults to [`FileSystem::cwd`].
    pub cwd: Option<String>,
    /// Additional environment variables.
    pub env: Option<HashMap<String, String>>,
    /// Timeout in seconds.
    pub timeout: Option<u64>,
    /// Abort signal used to terminate the command.
    pub abort_signal: Option<CancellationToken>,
    /// Called with stdout chunks as they are produced.
    pub on_stdout: Option<OutputCallback>,
    /// Called with stderr chunks as they are produced.
    pub on_stderr: Option<OutputCallback>,
}

/// Result of a shell command (types.ts:325-326).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShellOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i64,
}

/// Shell execution capability used by the harness (types.ts:320-329).
#[async_trait]
pub trait Shell: Send + Sync {
    /// Execute a shell command.
    async fn exec(
        &self,
        command: &str,
        options: ShellExecOptions,
    ) -> Result<ShellOutput, ExecutionError>;

    /// Release shell resources. Best-effort; must not panic.
    async fn cleanup(&self);
}

/// Filesystem + process execution environment (types.ts:332).
pub trait ExecutionEnv: FileSystem + Shell {}

/// Session storage backend (types.ts:441-455). Methods return `Result<_,
/// SessionError>`; Pi's `Promise<T>` throws map to the `Err` arm.
#[async_trait]
pub trait SessionStorage: Send + Sync {
    /// Metadata type for this backend (TS `TMetadata extends SessionMetadata`).
    type Metadata: Send + Sync;

    /// Return session metadata.
    async fn get_metadata(&self) -> Result<Self::Metadata, SessionError>;

    /// Return the active leaf entry id, if any.
    async fn get_leaf_id(&self) -> Result<Option<String>, SessionError>;

    /// Persist the active session-tree leaf.
    async fn set_leaf_id(&self, leaf_id: Option<String>) -> Result<(), SessionError>;

    /// Allocate a new entry id.
    async fn create_entry_id(&self) -> Result<String, SessionError>;

    /// Append an entry.
    async fn append_entry(&self, entry: SessionTreeEntry) -> Result<(), SessionError>;

    /// Look up an entry by id.
    async fn get_entry(&self, id: &str) -> Result<Option<SessionTreeEntry>, SessionError>;

    /// Return all entries of the given `type` tag.
    async fn find_entries(&self, entry_type: &str) -> Result<Vec<SessionTreeEntry>, SessionError>;

    /// Return the label string for an entry, if any.
    async fn get_label(&self, id: &str) -> Result<Option<String>, SessionError>;

    /// Walk the `parentId` chain from `leaf_id` to the root (root-first).
    async fn get_path_to_root(
        &self,
        leaf_id: Option<String>,
    ) -> Result<Vec<SessionTreeEntry>, SessionError>;

    /// Return every entry in the store.
    async fn get_entries(&self) -> Result<Vec<SessionTreeEntry>, SessionError>;
}

/// Options for [`SessionRepo::create`] (types.ts:459-461).
#[derive(Debug, Default, Clone)]
pub struct SessionCreateOptions {
    pub id: Option<String>,
}

/// Fork position for [`SessionRepo::fork`] (types.ts:463-467).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionForkPosition {
    Before,
    At,
}

/// Options for [`SessionRepo::fork`] (types.ts:463-467).
#[derive(Debug, Default, Clone)]
pub struct SessionForkOptions {
    pub entry_id: Option<String>,
    pub position: Option<SessionForkPosition>,
    pub id: Option<String>,
}

/// Session repository / persistence backend (types.ts:469-479). Generic over the
/// backend's concrete `Session`, metadata, and option types so the foundational
/// contract does not depend on the not-yet-ported `Session` struct.
#[async_trait]
pub trait SessionRepo: Send + Sync {
    /// Concrete session handle produced by this repo.
    type Session: Send + Sync;
    /// Metadata describing a stored session.
    type Metadata: Send + Sync;
    /// Backend-specific create options (TS `TCreateOptions`).
    type CreateOptions: Send + Sync;
    /// Backend-specific list filter (TS `TListOptions`).
    type ListOptions: Send + Sync;

    /// Create a new session.
    async fn create(&self, options: Self::CreateOptions) -> Result<Self::Session, SessionError>;

    /// Open an existing session by metadata.
    async fn open(&self, metadata: Self::Metadata) -> Result<Self::Session, SessionError>;

    /// List stored session metadata.
    async fn list(
        &self,
        options: Option<Self::ListOptions>,
    ) -> Result<Vec<Self::Metadata>, SessionError>;

    /// Delete a stored session.
    async fn delete(&self, metadata: Self::Metadata) -> Result<(), SessionError>;

    /// Fork a session at/before a target entry into a new session.
    async fn fork(
        &self,
        source: Self::Metadata,
        options: SessionForkOptions,
        create: Self::CreateOptions,
    ) -> Result<Self::Session, SessionError>;
}
