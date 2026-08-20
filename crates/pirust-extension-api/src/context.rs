//! Extension context + handler types — port of `ExtensionContext`,
//! `ExtensionCommandContext`, `ExtensionHandler` (extensions/types.ts).
//!
//! Pi's context is a lazily-guarded facade over the runner. The Rust port
//! keeps the same shape as a struct of accessor closures; the runner fills
//! them in. `mode`/`hasUI`/`cwd` are plain values (they cannot change during
//! a run), everything else is a function so stale-instance guards can be
//! layered later (Wave 6/7).

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::events::ExtensionEvent;

/// `ExtensionMode` (types.ts:312).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtensionMode {
    Tui,
    Rpc,
    Json,
    Print,
}

impl ExtensionMode {
    /// `mode` string Pi stores.
    pub fn as_str(&self) -> &'static str {
        match self {
            ExtensionMode::Tui => "tui",
            ExtensionMode::Rpc => "rpc",
            ExtensionMode::Json => "json",
            ExtensionMode::Print => "print",
        }
    }
}

/// `ExtensionContext` (types.ts:315) — passed to every event handler.
pub struct ExtensionContext {
    /// Current run mode.
    pub mode: ExtensionMode,
    /// Whether dialog-capable UI is available (true in TUI and RPC).
    pub has_ui: bool,
    /// Current working directory.
    pub cwd: String,
    /// Whether the agent is idle (not streaming).
    pub is_idle: Box<dyn Fn() -> bool>,
    /// The current abort signal, or None when the agent is not streaming.
    pub signal: Option<()>,
    /// Abort the current agent operation.
    pub abort: Box<dyn Fn()>,
    /// Whether there are queued messages waiting.
    pub has_pending_messages: Box<dyn Fn() -> bool>,
    /// Gracefully shutdown pi and exit.
    pub shutdown: Box<dyn Fn()>,
    /// Get current context usage for the active model.
    pub get_context_usage: Box<dyn Fn() -> Option<ContextUsage>>,
    /// Get the current effective system prompt.
    pub get_system_prompt: Box<dyn Fn() -> String>,
}

/// `ContextUsage` (types.ts:296).
#[derive(Debug, Clone, Copy)]
pub struct ContextUsage {
    /// Estimated context tokens, or None if unknown.
    pub tokens: Option<u64>,
    pub context_window: u64,
    /// Context usage as percentage of context window, or None.
    pub percent: Option<f64>,
}

/// `ExtensionCommandContext` (types.ts:366) — extended context for command
/// handlers. Session-control methods only safe in user-initiated commands.
pub struct ExtensionCommandContext {
    pub base: ExtensionContext,
    /// Wait for the agent to finish streaming.
    pub wait_for_idle: Box<dyn Fn()>,
    /// Reload extensions, skills, prompts, themes, and context files.
    pub reload: Box<dyn Fn()>,
}

/// `ExtensionHandler` (types.ts:1270) — handler function type for events.
pub type ExtensionHandler =
    Box<dyn Fn(&ExtensionEvent, &ExtensionContext) -> Result<Value, String> + Send + Sync>;

/// The event + context passed to a handler; keeps the union type out of the
/// handler signature.
pub type HandlerEvent<'a> = (&'a ExtensionEvent, &'a ExtensionContext);

/// `ContextEventResult` (types.ts:1405).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ContextEventResult {
    pub messages: Option<Value>,
}

/// `ToolCallEventResult` (types.ts:1413). All fields are optional in Pi's TS
/// (an extension may return only `{ block: true }`), so each has a serde
/// default — a missing key must not fail the parse (runner.ts:935
/// `unwrap_or_default` would then silently drop a real `block`).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ToolCallEventResult {
    /// Block tool execution.
    #[serde(default)]
    pub block: bool,
    #[serde(default)]
    pub reason: Option<String>,
    /// Hint that the agent should stop after the current tool batch.
    #[serde(default)]
    pub terminate: bool,
}

/// `ToolResultEventResult` (types.ts:1439).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ToolResultEventResult {
    pub content: Option<Value>,
    pub details: Option<Value>,
    pub is_error: Option<bool>,
}

/// `UserBashEventResult` (types.ts:1431).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct UserBashEventResult {
    pub result: Option<Value>,
}

/// `InputEventResult` (types.ts:915) � `{action: ...}` discriminated union.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum InputEventResult {
    Continue,
    Transform { text: String },
    Handled,
}

/// `MessageEndEventResult` (types.ts:1447).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct MessageEndEventResult {
    /// Replace the finalized message (must keep the original role).
    pub message: Option<Value>,
}

/// `BeforeAgentStartEventResult` (types.ts:1452).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct BeforeAgentStartEventResult {
    pub message: Option<Value>,
    /// Replace the system prompt for this turn (chained across extensions).
    pub system_prompt: Option<String>,
}

/// `ResourcesDiscoverResult` (types.ts:589).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ResourcesDiscoverResult {
    pub skill_paths: Vec<String>,
    pub prompt_paths: Vec<String>,
    pub theme_paths: Vec<String>,
}

/// `SessionBeforeSwitchResult` (types.ts:1459).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct SessionBeforeSwitchResult {
    pub cancel: bool,
}

/// `SessionBeforeForkResult` (types.ts:1463).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct SessionBeforeForkResult {
    pub cancel: bool,
    pub skip_conversation_restore: bool,
}

/// `SessionBeforeCompactResult` (types.ts:1467).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct SessionBeforeCompactResult {
    pub cancel: bool,
    pub compaction: Option<Value>,
}

/// `SessionBeforeTreeResult` (types.ts:1471).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct SessionBeforeTreeResult {
    pub cancel: bool,
    pub summary: Option<Value>,
    pub custom_instructions: Option<String>,
    pub replace_instructions: Option<bool>,
    pub label: Option<String>,
}
