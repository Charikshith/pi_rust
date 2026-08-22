//! RPC wire types — port of `modes/rpc/rpc-types.ts`.
//!
//! Key-order fidelity: responses are built by JS object literals
//! (`rpc-mode.ts:64-77` — `{ id, type, command, success, data }`), so the
//! persisted key order is `id?`, `type`, `command`, `success`, then `data` /
//! `error`. `JSON.stringify` OMITS keys whose value is `undefined`, which the
//! oracle pins as absent keys (`sessionFile`/`sessionName` in `get_state`,
//! `id` on commands sent without one) — mirrored here with
//! `skip_serializing_if`.

use serde::ser::{SerializeStruct, Serializer};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Commands (stdin) — rpc-types.ts:20-73
// ---------------------------------------------------------------------------

/// A parsed stdin line. `Unknown` covers anything whose `type` falls through
/// rpc-mode.ts:711-715's default arm — including non-object JSON, where TS's
/// `command.type` reads as the STRING `"undefined"` in the error message.
#[derive(Debug, Clone, PartialEq)]
pub enum ParsedInput {
    /// `{"type":"extension_ui_response",...}` — routed to pending extension
    /// dialogs (rpc-mode.ts:764-778), never answered directly.
    ExtensionUiResponse(RpcExtensionUIResponse),
    Command(RpcCommand),
    /// No recognizable `type`. `None` means the JSON had no string `type`
    /// field at all (Pi reports it as the literal text `undefined`).
    Unknown(Option<String>),
}

/// Parse one stdin line the way `handleInputLine` does (rpc-mode.ts:748-780).
/// JSON syntax errors are the caller's concern (it answers with the
/// `parse` error response before consulting this function).
pub fn parse_input(line: &str) -> ParsedInput {
    let value: serde_json::Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => return ParsedInput::Unknown(None),
    };
    if value.get("type").and_then(|t| t.as_str()) == Some("extension_ui_response") {
        return match serde_json::from_value::<RpcExtensionUIResponse>(value) {
            Ok(r) => ParsedInput::ExtensionUiResponse(r),
            Err(_) => ParsedInput::Unknown(Some("extension_ui_response".to_string())),
        };
    }
    match serde_json::from_value::<RpcCommand>(value.clone()) {
        Ok(c) => ParsedInput::Command(c),
        Err(_) => ParsedInput::Unknown(
            value
                .get("type")
                .and_then(|t| t.as_str())
                .map(str::to_string),
        ),
    }
}

fn err_unknown_type(type_name: &str) -> String {
    format!("Unknown command: {type_name}")
}

impl ParsedInput {
    /// The `type` string Pi would report in an `Unknown command:` error.
    pub fn unknown_type_label(&self) -> Option<String> {
        match self {
            ParsedInput::Unknown(Some(t)) => Some(err_unknown_type(t)),
            // TS: `(command as { type: string }).type` on a non-object is the
            // string "undefined".
            ParsedInput::Unknown(None) => Some(err_unknown_type("undefined")),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ThinkingLevel {
    Off,
    Minimal,
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum RpcCommand {
    Prompt {
        message: String,
        #[serde(default)]
        images: Option<serde_json::Value>,
        #[serde(default)]
        streaming_behavior: Option<StreamingBehavior>,
    },
    Steer {
        message: String,
        #[serde(default)]
        images: Option<serde_json::Value>,
    },
    FollowUp {
        message: String,
        #[serde(default)]
        images: Option<serde_json::Value>,
    },
    Abort,
    NewSession {
        #[serde(default)]
        parent_session: Option<String>,
    },
    GetState,
    SetModel {
        provider: String,
        #[serde(rename = "modelId")]
        model_id: String,
    },
    CycleModel,
    GetAvailableModels,
    SetThinkingLevel {
        level: ThinkingLevel,
    },
    CycleThinkingLevel,
    GetAvailableThinkingLevels,
    SetSteeringMode {
        mode: QueueMode,
    },
    SetFollowUpMode {
        mode: QueueMode,
    },
    Compact {
        #[serde(default)]
        custom_instructions: Option<String>,
    },
    SetAutoCompaction {
        enabled: bool,
    },
    SetAutoRetry {
        enabled: bool,
    },
    AbortRetry,
    Bash {
        command: String,
        #[serde(default)]
        exclude_from_context: Option<bool>,
    },
    AbortBash,
    GetSessionStats,
    ExportHtml {
        #[serde(default)]
        output_path: Option<String>,
    },
    SwitchSession {
        #[serde(rename = "sessionPath")]
        session_path: String,
    },
    Fork {
        #[serde(rename = "entryId")]
        entry_id: String,
    },
    Clone,
    GetForkMessages,
    GetEntries {
        #[serde(default)]
        since: Option<String>,
    },
    GetTree,
    GetLastAssistantText,
    SetSessionName {
        name: String,
    },
    GetMessages,
    GetCommands,
}

/// NOTE on ids: `id` is deliberately NOT part of [`RpcCommand`] — every variant
/// carries `id?: string` at the TOP level alongside `type` (rpc-types.ts), so
/// the parser reads it off the raw object before deserializing (same shape as
/// Pi's correlation id).
pub fn parse_command_id(value: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(value)
        .ok()?
        .get("id")
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum QueueMode {
    All,
    OneAtATime,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum StreamingBehavior {
    Steer,
    FollowUp,
}

// ---------------------------------------------------------------------------
// State (rpc-types.ts:95-108)
// ---------------------------------------------------------------------------

/// Serialized exactly in rpc-mode.ts:447-460's literal key order; `model`,
/// `sessionFile` and `sessionName` are omitted when undefined (pinned by the
/// oracle's get_state record).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcSessionState {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<serde_json::Value>,
    pub thinking_level: ThinkingLevelSerde,
    pub is_streaming: bool,
    pub is_compacting: bool,
    pub steering_mode: QueueModeSerde,
    pub follow_up_mode: QueueModeSerde,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_file: Option<String>,
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_name: Option<String>,
    pub auto_compaction_enabled: bool,
    pub message_count: usize,
    pub pending_message_count: usize,
}

/// Newtype wrappers: the wire spelling is the kebab/camel literal, our enum is
/// closed over Pi's vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThinkingLevelSerde(pub ThinkingLevel);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueModeSerde(pub QueueMode);

impl Serialize for ThinkingLevelSerde {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let text = match self.0 {
            ThinkingLevel::Off => "off",
            ThinkingLevel::Minimal => "minimal",
            ThinkingLevel::Low => "low",
            ThinkingLevel::Medium => "medium",
            ThinkingLevel::High => "high",
        };
        s.serialize_str(text)
    }
}

impl Serialize for QueueModeSerde {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let text = match self.0 {
            QueueMode::All => "all",
            QueueMode::OneAtATime => "one-at-a-time",
        };
        s.serialize_str(text)
    }
}

// ---------------------------------------------------------------------------
// Responses (stdout) — rpc-types.ts:115-231, built at rpc-mode.ts:64-77
// ---------------------------------------------------------------------------

/// The single response envelope. `data`/`error` are pre-rendered JSON because
/// their payloads belong to other subsystems (Model, SessionEntry,
/// CompactionResult...) that pass through verbatim.
#[derive(Debug, Clone, PartialEq)]
pub struct RpcResponse {
    pub id: Option<String>,
    pub command: String,
    pub outcome: Outcome,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Success { data: Option<serde_json::Value> },
    Error { error: String },
}

impl RpcResponse {
    pub fn success(id: Option<String>, command: impl Into<String>) -> Self {
        Self {
            id,
            command: command.into(),
            outcome: Outcome::Success { data: None },
        }
    }

    pub fn success_with(
        id: Option<String>,
        command: impl Into<String>,
        data: serde_json::Value,
    ) -> Self {
        Self {
            id,
            command: command.into(),
            outcome: Outcome::Success { data: Some(data) },
        }
    }

    pub fn error(id: Option<String>, command: impl Into<String>, error: impl Into<String>) -> Self {
        Self {
            id,
            command: command.into(),
            outcome: Outcome::Error {
                error: error.into(),
            },
        }
    }
}

// Key order pinned by rpc-mode.ts:69-72: `{ id, type: "response", command,
// success: true }` (+ `, data` only when defined).
impl Serialize for RpcResponse {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut st = s.serialize_struct("RpcResponse", 5)?;
        if let Some(id) = &self.id {
            st.serialize_field("id", id)?;
        }
        st.serialize_field("type", "response")?;
        st.serialize_field("command", &self.command)?;
        match &self.outcome {
            Outcome::Success { data } => {
                st.serialize_field("success", &true)?;
                if let Some(data) = data {
                    st.serialize_field("data", data)?;
                }
            }
            Outcome::Error { error } => {
                st.serialize_field("success", &false)?;
                st.serialize_field("error", error)?;
            }
        }
        st.end()
    }
}

// ---------------------------------------------------------------------------
// Slash commands (rpc-types.ts:80-89)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct RpcSlashCommand {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub source: RpcCommandSource,
    pub source_info: SourceInfoSerde,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum RpcCommandSource {
    Extension,
    Prompt,
    Skill,
}

/// `SourceInfo` passes through verbatim from the resource loader; kept opaque
/// until feat-007's loader owns its Rust shape.
#[derive(Debug, Clone)]
pub struct SourceInfoSerde(pub serde_json::Value);

impl Serialize for SourceInfoSerde {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(s)
    }
}

// ---------------------------------------------------------------------------
// Extension UI requests/responses (rpc-types.ts:238-283)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "type", rename = "extension_ui_request")]
pub struct ExtensionUiRequest {
    pub id: String,
    pub method: UiMethod,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum UiMethod {
    Select {
        title: String,
        options: Vec<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        timeout: Option<u64>,
    },
    Confirm {
        title: String,
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        timeout: Option<u64>,
    },
    Input {
        title: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        placeholder: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        timeout: Option<u64>,
    },
    Editor {
        title: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        prefill: Option<String>,
    },
    Notify {
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        notify_type: Option<NotifyType>,
    },
    SetStatus {
        status_key: String,
        status_text: Option<String>,
    },
    SetWidget {
        widget_key: String,
        widget_lines: Option<Vec<String>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        widget_placement: Option<WidgetPlacement>,
    },
    SetTitle {
        title: String,
    },
    SetEditorText {
        text: String,
    },
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum NotifyType {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum WidgetPlacement {
    AboveEditor,
    BelowEditor,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RpcExtensionUIResponse {
    pub id: String,
    pub payload: UiResponsePayload,
}

impl<'de> Deserialize<'de> for RpcExtensionUIResponse {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = serde_json::Value::deserialize(d)?;
        let id = v
            .get("id")
            .and_then(|x| x.as_str())
            .ok_or_else(|| serde::de::Error::missing_field("id"))?
            .to_string();
        let payload = UiResponsePayload::deserialize(&v)
            .map_err(|_| serde::de::Error::custom("no value/confirmed/cancelled key"))?;
        Ok(Self { id, payload })
    }
}

/// rpc-types.ts:280-283 — exactly one of `value`, `confirmed`, `cancelled`.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum UiResponsePayload {
    Value { value: String },
    Confirmed { confirmed: bool },
    Cancelled { cancelled: TrueLiteral },
}

/// `cancelled: true` only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrueLiteral;

impl<'de> Deserialize<'de> for TrueLiteral {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let b = bool::deserialize(d)?;
        if b {
            Ok(TrueLiteral)
        } else {
            Err(serde::de::Error::custom("expected true"))
        }
    }
}
