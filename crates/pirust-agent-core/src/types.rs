//! Persisted / serialized agent types — port of `packages/agent/src/types.ts`.
//!
//! Spec: `docs/analysis/07-agent-core-spec.md` §1.1 (message model), §1.3 (other
//! exported types), §8 (`AgentTool` trait contract), §10 (`convert_to_llm`).
//! `[LEAF]` module (§13, wave 0/1).
//!
//! This module holds the agent-level contract surface: the tool trait + result,
//! the hook context/result data shapes, the loop config seam, and the loop
//! [`AgentEvent`] union (types.ts:415-430). Signatures + data shapes only — the
//! loop/execution logic lands with the `agent_loop` integrator.
//!
//! `AgentMessage` itself lives in [`crate::harness::messages`] (it is the union of
//! pi-ai's `Message` and the four agent-core custom variants); it is re-exported
//! here for convenience.

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use futures::future::BoxFuture;
use pirust_ai::types::{
    AssistantMessage, AssistantMessageEvent, Message, Model, ToolCall, ToolResultMessage,
    UserContent,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

pub use crate::harness::messages::AgentMessage;

/// Reasoning-effort level (types.ts:289). Wire tags are exact: `off | minimal |
/// low | medium | high | xhigh | max`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThinkingLevel {
    Off,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

/// How a single assistant message's tool calls are executed (types.ts:41).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolExecutionMode {
    /// Each call is prepared, executed, and finalized before the next starts.
    Sequential,
    /// Calls are prepared sequentially, then allowed tools execute concurrently.
    Parallel,
}

/// Queue drain policy for pending user messages (types.ts:49).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum QueueMode {
    /// Drain and inject every queued message at the drain point.
    #[serde(rename = "all")]
    All,
    /// Drain and inject only the oldest queued message.
    #[serde(rename = "one-at-a-time")]
    OneAtATime,
}

/// A single tool-call content block from an assistant message (types.ts:52).
/// Reuses pi-ai's byte-verified `ToolCall`.
pub type AgentToolCall = ToolCall;

/// Final or partial result produced by a tool (types.ts:350-362, §8).
#[derive(Debug, Clone, PartialEq)]
pub struct AgentToolResult {
    /// Text or image content returned to the model (`(TextContent | ImageContent)[]`).
    pub content: Vec<UserContent>,
    /// Arbitrary structured details for logs or UI rendering (TS generic `TDetails`).
    pub details: Value,
    /// Names of tools introduced by this result, available from here onward.
    pub added_tool_names: Option<Vec<String>>,
    /// Hint that the agent should stop after the current tool batch. Early
    /// termination only happens when every finalized result sets this to `true`.
    pub terminate: Option<bool>,
}

/// Callback used by tools to stream partial execution updates (types.ts:370).
/// Calls made after the `execute` future settles are ignored by the loop.
pub type AgentToolUpdateCallback = Arc<dyn Fn(AgentToolResult) + Send + Sync>;

/// Error type surfaced when a tool `execute` fails. Pi throws; the loop converts
/// the thrown value into an error tool result (agent-loop.ts:757-762).
pub type ToolError = Box<dyn std::error::Error + Send + Sync>;

/// Tool definition used by the agent runtime (types.ts:373-396, extends pi-ai
/// `Tool<TParameters>`).
///
/// Spec §8: `execute` returns an error on failure (the loop converts it into a
/// tool-error message); argument validation mirrors pi-ai `validateToolArguments`
/// via the `jsonschema` crate. Cancellation is delivered through `token` (§6).
#[async_trait]
pub trait AgentTool: Send + Sync {
    /// Stable tool identifier (from pi-ai `Tool`).
    fn name(&self) -> &str;

    /// Human-readable label shown in transcripts / UI.
    fn label(&self) -> &str;

    /// Prompt-facing tool description sent to the model (from pi-ai `Tool`).
    /// Distinct from [`label`](Self::label): Pi's `AgentTool extends Tool`, so the
    /// provider request carries this long description while `label` is UI-only.
    fn description(&self) -> &str;

    /// JSON schema describing the tool parameters (from pi-ai `Tool`).
    fn parameters(&self) -> Value;

    /// Coerce raw model-supplied args into schema shape; identity by default
    /// (the loop treats an unchanged return as "no shim ran").
    fn prepare_arguments(&self, raw: Value) -> Value {
        raw
    }

    /// Execute the tool call. Return `Err` on failure instead of encoding errors
    /// in `content`.
    async fn execute(
        &self,
        tool_call_id: &str,
        args: Value,
        token: CancellationToken,
        on_update: AgentToolUpdateCallback,
    ) -> Result<AgentToolResult, ToolError>;

    /// Per-tool execution-mode override; `None` uses the loop default.
    fn execution_mode(&self) -> Option<ToolExecutionMode> {
        None
    }
}

/// Result returned from `beforeToolCall` (types.ts:60-63).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BeforeToolCallResult {
    /// `Some(true)` prevents the tool from executing (an error result is emitted).
    pub block: Option<bool>,
    /// Text shown in the blocked error result; a default is used when omitted.
    pub reason: Option<String>,
}

/// Partial override returned from `afterToolCall` (types.ts:77-86). Merge is
/// field-by-field; omitted fields keep the executed result. No deep merge.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AfterToolCallResult {
    /// Replaces the full content array when provided.
    pub content: Option<Vec<UserContent>>,
    /// Replaces the full details payload when provided.
    pub details: Option<Value>,
    /// Replaces the error flag when provided.
    pub is_error: Option<bool>,
    /// Replaces the early-termination hint when provided.
    pub terminate: Option<bool>,
}

/// Context snapshot passed into the low-level agent loop (types.ts:399-406).
#[derive(Clone)]
pub struct AgentContext {
    /// System prompt included with the request.
    pub system_prompt: String,
    /// Transcript visible to the model.
    pub messages: Vec<AgentMessage>,
    /// Tools available for this run.
    pub tools: Option<Vec<Arc<dyn AgentTool>>>,
}

/// Context passed to `beforeToolCall` (types.ts:89-98).
pub struct BeforeToolCallContext {
    pub assistant_message: AssistantMessage,
    pub tool_call: AgentToolCall,
    pub args: Value,
    pub context: AgentContext,
}

/// Context passed to `afterToolCall` (types.ts:101-114).
pub struct AfterToolCallContext {
    pub assistant_message: AssistantMessage,
    pub tool_call: AgentToolCall,
    pub args: Value,
    pub result: AgentToolResult,
    pub is_error: bool,
    pub context: AgentContext,
}

/// Context passed to `shouldStopAfterTurn` (types.ts:117-126).
pub struct ShouldStopAfterTurnContext {
    pub message: AssistantMessage,
    pub tool_results: Vec<ToolResultMessage>,
    pub context: AgentContext,
    pub new_messages: Vec<AgentMessage>,
}

/// Context passed to `prepareNextTurn` (types.ts:138) — identical to
/// [`ShouldStopAfterTurnContext`].
pub type PrepareNextTurnContext = ShouldStopAfterTurnContext;

/// Replacement runtime state for the next provider request (types.ts:129-136).
#[derive(Clone)]
pub struct AgentLoopTurnUpdate {
    pub context: Option<AgentContext>,
    pub model: Option<Model>,
    pub thinking_level: Option<ThinkingLevel>,
}

/// Public agent state (types.ts:322-347). `tools` / `messages` copy-on-assign
/// accessor semantics are re-established by the `Agent` owner.
#[derive(Clone)]
pub struct AgentState {
    /// System prompt sent with each model request.
    pub system_prompt: String,
    /// Active model used for future turns.
    pub model: Model,
    /// Requested reasoning level for future turns.
    pub thinking_level: ThinkingLevel,
    /// Available tools.
    pub tools: Vec<Arc<dyn AgentTool>>,
    /// Conversation transcript.
    pub messages: Vec<AgentMessage>,
    /// True while processing a prompt/continuation (until `agent_end` listeners settle).
    pub is_streaming: bool,
    /// Partial assistant message for the current streamed response, if any.
    pub streaming_message: Option<AgentMessage>,
    /// Tool call ids currently executing.
    pub pending_tool_calls: HashSet<String>,
    /// Error message from the most recent failed/aborted turn, if any.
    pub error_message: Option<String>,
}

// --- AgentLoopConfig callback seams -----------------------------------------
//
// `AgentLoopConfig extends SimpleStreamOptions` in TS (types.ts:140). pi-ai's
// `SimpleStreamOptions` is not yet ported (deferred to the provider phase), so
// the streaming-option fields are represented by `api_key` only for now; the
// remainder are wired by the loop integrator. All hooks carry a MUST-NOT-THROW
// contract (types.ts:150,199,211,233,246): implementations return a safe
// fallback rather than erroring.

/// Required converter: `AgentMessage[]` → LLM `Message[]` before each call
/// (types.ts:169).
pub type ConvertToLlmFn =
    Box<dyn Fn(Vec<AgentMessage>) -> BoxFuture<'static, Vec<Message>> + Send + Sync>;

/// Optional `AgentMessage`-level transform applied before `convertToLlm`
/// (types.ts:191).
pub type TransformContextFn = Box<
    dyn Fn(Vec<AgentMessage>, Option<CancellationToken>) -> BoxFuture<'static, Vec<AgentMessage>>
        + Send
        + Sync,
>;

/// Optional dynamic API-key resolver keyed by provider (types.ts:201).
pub type GetApiKeyFn = Box<dyn Fn(String) -> BoxFuture<'static, Option<String>> + Send + Sync>;

/// Optional graceful-stop predicate evaluated after each `turn_end` (types.ts:213).
pub type ShouldStopAfterTurnFn =
    Box<dyn Fn(ShouldStopAfterTurnContext) -> BoxFuture<'static, bool> + Send + Sync>;

/// Optional next-turn state override (types.ts:220-222).
pub type PrepareNextTurnFn = Box<
    dyn Fn(PrepareNextTurnContext) -> BoxFuture<'static, Option<AgentLoopTurnUpdate>> + Send + Sync,
>;

/// Optional queue drain hook returning steering / follow-up messages
/// (types.ts:235,248).
pub type GetMessagesFn = Box<dyn Fn() -> BoxFuture<'static, Vec<AgentMessage>> + Send + Sync>;

/// Optional pre-execution tool hook (types.ts:267).
pub type BeforeToolCallFn = Box<
    dyn Fn(
            BeforeToolCallContext,
            Option<CancellationToken>,
        ) -> BoxFuture<'static, Option<BeforeToolCallResult>>
        + Send
        + Sync,
>;

/// Optional post-execution tool hook (types.ts:281).
pub type AfterToolCallFn = Box<
    dyn Fn(
            AfterToolCallContext,
            Option<CancellationToken>,
        ) -> BoxFuture<'static, Option<AfterToolCallResult>>
        + Send
        + Sync,
>;

/// Configuration for the low-level agent loop (types.ts:140-282). Data shape +
/// hook seams; behaviour is implemented by the `agent_loop` integrator.
pub struct AgentLoopConfig {
    /// Model for provider requests (types.ts:141).
    pub model: Model,
    /// API key (from `SimpleStreamOptions`); other stream options land later.
    pub api_key: Option<String>,
    /// Tool execution mode. Default: `Parallel` (types.ts:259).
    pub tool_execution: Option<ToolExecutionMode>,
    /// Reasoning level for the first turn's provider call (agent.ts:436).
    /// `prepare_next_turn` may override this for later turns; before this
    /// field existed the first call of every prompt always ran with
    /// reasoning off regardless of the agent's configured thinking level.
    pub reasoning: Option<ThinkingLevel>,
    /// Required LLM conversion hook.
    pub convert_to_llm: ConvertToLlmFn,
    /// Optional context transform.
    pub transform_context: Option<TransformContextFn>,
    /// Optional API-key resolver.
    pub get_api_key: Option<GetApiKeyFn>,
    /// Optional graceful-stop predicate.
    pub should_stop_after_turn: Option<ShouldStopAfterTurnFn>,
    /// Optional next-turn state override.
    pub prepare_next_turn: Option<PrepareNextTurnFn>,
    /// Optional steering-message drain.
    pub get_steering_messages: Option<GetMessagesFn>,
    /// Optional follow-up-message drain.
    pub get_follow_up_messages: Option<GetMessagesFn>,
    /// Optional pre-execution tool hook.
    pub before_tool_call: Option<BeforeToolCallFn>,
    /// Optional post-execution tool hook.
    pub after_tool_call: Option<AfterToolCallFn>,
}

/// Events emitted by the loop for UI updates (types.ts:415-430).
///
/// Discriminated union on `type`. These are the ten loop-level events; the
/// harness layers additional own-events (e.g. `save_point`, `settled`,
/// `after_provider_response`) on top via its own event enum — those live with
/// the harness integrator, not here, matching Pi's `AgentEvent` /
/// `AgentHarnessOwnEvent` split (types.ts:415-430 vs harness/types.ts:636-658).
// Variants carry full `AgentMessage`s (a large union); the size spread is
// inherent to the faithful event shape and these are moved, not stored in bulk.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    /// Run started.
    AgentStart,
    /// Last event of a run; carries the messages the run produced.
    AgentEnd { messages: Vec<AgentMessage> },
    /// A turn (one assistant response + tool calls/results) started.
    TurnStart,
    /// A turn completed.
    TurnEnd {
        message: AgentMessage,
        #[serde(rename = "toolResults")]
        tool_results: Vec<ToolResultMessage>,
    },
    /// A user / assistant / toolResult message started.
    MessageStart { message: AgentMessage },
    /// Assistant streaming delta.
    ///
    /// Field order is the **construction site's** (`agent-loop.ts:340-344`), which emits
    /// `assistantMessageEvent` before `message` — not the declaration order at
    /// `types.ts:425`. `JSON.stringify` follows the object literal that was built, so this
    /// is the byte order every `message_update` line carries (confirmed against all 100+
    /// such lines in `tests/fixtures/pi/printmode/json_mode.cases.jsonl`). Declaring
    /// `message` first would silently emit the wrong bytes for anyone serializing this
    /// union; agent-core's own goldens serialize session *entries*, not events, so the
    /// defect was latent until print mode needed it.
    MessageUpdate {
        #[serde(rename = "assistantMessageEvent")]
        assistant_message_event: AssistantMessageEvent,
        message: AgentMessage,
    },
    /// A message finished.
    MessageEnd { message: AgentMessage },
    /// A tool began executing.
    ToolExecutionStart {
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        #[serde(rename = "toolName")]
        tool_name: String,
        args: Value,
    },
    /// A tool streamed a partial update.
    ToolExecutionUpdate {
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        #[serde(rename = "toolName")]
        tool_name: String,
        args: Value,
        #[serde(rename = "partialResult")]
        partial_result: Value,
    },
    /// A tool finished executing.
    ToolExecutionEnd {
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        #[serde(rename = "toolName")]
        tool_name: String,
        result: Value,
        #[serde(rename = "isError")]
        is_error: bool,
    },
}
