//! Extension events — port of `ExtensionEvent` (extensions/types.ts).
//!
//! The TS discriminated union is a Rust enum with the same variant names and
//! field names (camelCase preserved via serde renames for any on-disk/wire
//! serialization). Payloads that are unstructured blobs (`args`, `result`,
//! `payload`, `headers`) stay `serde_json::Value`; structured ones get typed
//! fields where the port has the types already.

use serde::Serialize;
use serde_json::Value;

/// `ExtensionEvent` (types.ts:1399) — union of all extension events.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ExtensionEvent {
    // Startup / resource events
    ProjectTrust {
        cwd: String,
    },
    ResourcesDiscover {
        cwd: String,
        reason: ResourceDiscoverReason,
    },

    // Session events
    SessionStart {
        reason: SessionStartReason,
        #[serde(
            rename = "previousSessionFile",
            skip_serializing_if = "Option::is_none"
        )]
        previous_session_file: Option<String>,
    },
    SessionInfoChanged {
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    SessionBeforeSwitch {
        reason: SessionSwitchReason,
        #[serde(rename = "targetSessionFile", skip_serializing_if = "Option::is_none")]
        target_session_file: Option<String>,
    },
    SessionBeforeFork {
        #[serde(rename = "entryId")]
        entry_id: String,
        position: ForkPosition,
    },
    SessionBeforeCompact {
        reason: CompactReason,
        #[serde(rename = "willRetry")]
        will_retry: bool,
    },
    SessionCompact {
        #[serde(rename = "fromExtension")]
        from_extension: bool,
        reason: CompactReason,
        #[serde(rename = "willRetry")]
        will_retry: bool,
    },
    SessionCompactFailed {
        reason: CompactReason,
        #[serde(skip_serializing_if = "Option::is_none")]
        #[serde(rename = "errorMessage")]
        error_message: Option<String>,
        aborted: bool,
        #[serde(rename = "willRetry")]
        will_retry: bool,
        #[serde(rename = "fromExtension")]
        from_extension: bool,
    },
    SessionShutdown {
        reason: SessionShutdownReason,
        #[serde(rename = "targetSessionFile", skip_serializing_if = "Option::is_none")]
        target_session_file: Option<String>,
    },
    SessionBeforeTree {
        #[serde(rename = "targetId")]
        target_id: String,
    },
    SessionTree {
        #[serde(rename = "newLeafId")]
        new_leaf_id: Option<String>,
        #[serde(rename = "oldLeafId")]
        old_leaf_id: Option<String>,
    },

    // Agent events
    Context {
        messages: Value,
    },
    BeforeProviderRequest {
        payload: Value,
    },
    BeforeProviderHeaders {
        headers: Value,
    },
    AfterProviderResponse {
        status: u16,
        headers: Value,
    },
    BeforeAgentStart {
        prompt: String,
        system_prompt: String,
    },
    AgentStart,
    AgentEnd {
        messages: Value,
    },
    AgentSettled,
    TurnStart {
        #[serde(rename = "turnIndex")]
        turn_index: usize,
        timestamp: u64,
    },
    TurnEnd {
        #[serde(rename = "turnIndex")]
        turn_index: usize,
        message: Value,
        #[serde(rename = "toolResults")]
        tool_results: Value,
    },
    MessageStart {
        message: Value,
    },
    MessageUpdate {
        message: Value,
        #[serde(rename = "assistantMessageEvent")]
        assistant_message_event: Value,
    },
    MessageEnd {
        message: Value,
    },
    ToolExecutionStart {
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        #[serde(rename = "toolName")]
        tool_name: String,
        args: Value,
    },
    ToolExecutionUpdate {
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        #[serde(rename = "toolName")]
        tool_name: String,
        args: Value,
        #[serde(rename = "partialResult")]
        partial_result: Value,
    },
    ToolExecutionEnd {
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        #[serde(rename = "toolName")]
        tool_name: String,
        result: Value,
        #[serde(rename = "isError")]
        is_error: bool,
    },

    // Model events
    ModelSelect {
        model: Value,
        #[serde(rename = "previousModel")]
        previous_model: Option<Value>,
        source: ModelSelectSource,
    },
    ThinkingLevelSelect {
        level: Value,
        #[serde(rename = "previousLevel")]
        previous_level: Value,
    },

    // Input / bash / tool events
    UserBash {
        command: String,
        #[serde(rename = "excludeFromContext")]
        exclude_from_context: bool,
        cwd: String,
    },
    Input {
        text: String,
        source: InputSource,
    },
    ToolCall {
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        #[serde(rename = "toolName")]
        tool_name: String,
        input: Value,
    },
    ToolResult {
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        #[serde(rename = "toolName")]
        tool_name: String,
        input: Value,
        content: Value,
        #[serde(rename = "isError")]
        is_error: bool,
    },
}

impl ExtensionEvent {
    /// `event.type` — the discriminator string Pi's `emit` uses to look up
    /// handlers.
    pub fn event_type(&self) -> &'static str {
        match self {
            ExtensionEvent::ProjectTrust { .. } => "project_trust",
            ExtensionEvent::ResourcesDiscover { .. } => "resources_discover",
            ExtensionEvent::SessionStart { .. } => "session_start",
            ExtensionEvent::SessionInfoChanged { .. } => "session_info_changed",
            ExtensionEvent::SessionBeforeSwitch { .. } => "session_before_switch",
            ExtensionEvent::SessionBeforeFork { .. } => "session_before_fork",
            ExtensionEvent::SessionBeforeCompact { .. } => "session_before_compact",
            ExtensionEvent::SessionCompact { .. } => "session_compact",
            ExtensionEvent::SessionCompactFailed { .. } => "session_compact_failed",
            ExtensionEvent::SessionShutdown { .. } => "session_shutdown",
            ExtensionEvent::SessionBeforeTree { .. } => "session_before_tree",
            ExtensionEvent::SessionTree { .. } => "session_tree",
            ExtensionEvent::Context { .. } => "context",
            ExtensionEvent::BeforeProviderRequest { .. } => "before_provider_request",
            ExtensionEvent::BeforeProviderHeaders { .. } => "before_provider_headers",
            ExtensionEvent::AfterProviderResponse { .. } => "after_provider_response",
            ExtensionEvent::BeforeAgentStart { .. } => "before_agent_start",
            ExtensionEvent::AgentStart => "agent_start",
            ExtensionEvent::AgentEnd { .. } => "agent_end",
            ExtensionEvent::AgentSettled => "agent_settled",
            ExtensionEvent::TurnStart { .. } => "turn_start",
            ExtensionEvent::TurnEnd { .. } => "turn_end",
            ExtensionEvent::MessageStart { .. } => "message_start",
            ExtensionEvent::MessageUpdate { .. } => "message_update",
            ExtensionEvent::MessageEnd { .. } => "message_end",
            ExtensionEvent::ToolExecutionStart { .. } => "tool_execution_start",
            ExtensionEvent::ToolExecutionUpdate { .. } => "tool_execution_update",
            ExtensionEvent::ToolExecutionEnd { .. } => "tool_execution_end",
            ExtensionEvent::ModelSelect { .. } => "model_select",
            ExtensionEvent::ThinkingLevelSelect { .. } => "thinking_level_select",
            ExtensionEvent::UserBash { .. } => "user_bash",
            ExtensionEvent::Input { .. } => "input",
            ExtensionEvent::ToolCall { .. } => "tool_call",
            ExtensionEvent::ToolResult { .. } => "tool_result",
        }
    }
}

/// `ResourcesDiscoverEvent["reason"]` (types.ts:585).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceDiscoverReason {
    Startup,
    Reload,
}

/// `SessionStartEvent["reason"]` (types.ts:599).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStartReason {
    Startup,
    Reload,
    New,
    Resume,
    Fork,
}

/// `SessionBeforeSwitchEvent["reason"]` (types.ts:626).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionSwitchReason {
    New,
    Resume,
}

/// `SessionBeforeForkEvent["position"]` (types.ts:633).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ForkPosition {
    Before,
    At,
}

/// The compaction trigger (`SessionBeforeCompactEvent["reason"]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactReason {
    Manual,
    Threshold,
    Overflow,
}

/// `SessionShutdownEvent["reason"]` (types.ts:708).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionShutdownReason {
    Quit,
    Reload,
    New,
    Resume,
    Fork,
}

/// `ModelSelectSource` (types.ts:828).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelSelectSource {
    Set,
    Cycle,
    Restore,
}

/// `InputSource` (types.ts:889).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InputSource {
    Interactive,
    Rpc,
    Extension,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_type_matches_pi_discriminators() {
        assert_eq!(ExtensionEvent::AgentStart.event_type(), "agent_start");
        assert_eq!(
            ExtensionEvent::ToolExecutionStart {
                tool_call_id: "c".into(),
                tool_name: "bash".into(),
                args: Value::Null,
            }
            .event_type(),
            "tool_execution_start"
        );
        assert_eq!(
            ExtensionEvent::ToolCall {
                tool_call_id: "c".into(),
                tool_name: "bash".into(),
                input: Value::Null,
            }
            .event_type(),
            "tool_call"
        );
    }

    #[test]
    fn serializes_with_type_tag() {
        let e = ExtensionEvent::AgentStart;
        assert_eq!(
            serde_json::to_string(&e).unwrap(),
            r#"{"type":"agent_start"}"#
        );
    }
}
