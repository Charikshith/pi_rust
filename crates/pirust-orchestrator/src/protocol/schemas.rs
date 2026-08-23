//! Port of `packages/protocol/src/schemas.ts`.
//!
//! **WAVE 2 SCOPE (named, not silent):** only the envelope layer is typed
//! here — [`ClientMessage`] (`hello`/`request`), [`ServerMessage`]
//! (`hello`/`hello_error`/`response`/`event`), and [`ProtocolError`]. The
//! `request`/`result`/`event`/`snapshot` payload BODIES are represented as
//! generic, [`ProtocolJson`]-validated JSON rather than the full `Command`/
//! `CommandResult`/`ServerEvent`/`SessionSnapshot`/`TranscriptItem` unions —
//! that deep shape validation (assistant/tool item status consistency,
//! `SessionMetadata` required fields, `Command` variant shapes, image
//! rejection in `prompt`, etc.) is deferred to Wave 4 (`sessions.rs`), where
//! those types get built against real session-lifecycle behavior instead of
//! in isolation. See `docs/analysis/04-orchestrator.md` and `plan.md`.
//!
//! **Field-order residual (documented, not silent):** wire byte order for
//! `to_json` below is cross-checked against REAL construction call sites in
//! `server.ts`/`sessions.ts`/`snapshots.ts` for every type that has one
//! (`ServerHello`, `ResponseEnvelope`, `EventEnvelope`, `ServerHelloError`,
//! `ProtocolError` all matched exactly). `ClientHello` and `RequestEnvelope`
//! have no reference client implementation in this checkout to confirm
//! against — their field order here follows `schemas.ts`'s own declaration
//! order, the best available inference, not a confirmed live example.

use super::cbor::CborValue;
use thiserror::Error;

pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, Error, PartialEq, Eq)]
#[error("{0}")]
pub struct ValidationError(pub String);

impl ValidationError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

// ============================================================================
// ProtocolJson — port of `JsonValueSchema` / `isProtocolValue`'s value domain
// ============================================================================

/// The protocol's JSON-compatible value domain (no byte strings — a CBOR
/// byte string anywhere in a JSON-valued field is a validation error, same
/// as `isProtocolValue`'s recursive walk rejecting non-plain values).
#[derive(Debug, Clone, PartialEq)]
pub enum ProtocolJson {
    Null,
    Bool(bool),
    Number(f64),
    Text(String),
    Array(Vec<ProtocolJson>),
    /// Insertion-ordered, matching JS object key enumeration order.
    Map(Vec<(String, ProtocolJson)>),
}

impl ProtocolJson {
    pub fn from_cbor(value: &CborValue) -> Result<Self, ValidationError> {
        Ok(match value {
            CborValue::Null => ProtocolJson::Null,
            CborValue::Bool(b) => ProtocolJson::Bool(*b),
            CborValue::Number(n) => {
                if !n.is_finite() {
                    return Err(ValidationError::new("Protocol JSON numbers must be finite"));
                }
                ProtocolJson::Number(*n)
            }
            CborValue::Text(s) => ProtocolJson::Text(s.clone()),
            CborValue::Bytes(_) => {
                return Err(ValidationError::new(
                    "Protocol JSON values must not contain byte strings",
                ))
            }
            CborValue::Array(items) => ProtocolJson::Array(
                items
                    .iter()
                    .map(ProtocolJson::from_cbor)
                    .collect::<Result<_, _>>()?,
            ),
            CborValue::Map(entries) => ProtocolJson::Map(
                entries
                    .iter()
                    .map(|(key, value)| Ok((key.clone(), ProtocolJson::from_cbor(value)?)))
                    .collect::<Result<_, _>>()?,
            ),
        })
    }

    pub fn to_cbor(&self) -> CborValue {
        match self {
            ProtocolJson::Null => CborValue::Null,
            ProtocolJson::Bool(b) => CborValue::Bool(*b),
            ProtocolJson::Number(n) => CborValue::Number(*n),
            ProtocolJson::Text(s) => CborValue::Text(s.clone()),
            ProtocolJson::Array(items) => {
                CborValue::Array(items.iter().map(ProtocolJson::to_cbor).collect())
            }
            ProtocolJson::Map(entries) => CborValue::Map(
                entries
                    .iter()
                    .map(|(key, value)| (key.clone(), value.to_cbor()))
                    .collect(),
            ),
        }
    }

    fn fields(&self) -> Result<&[(String, ProtocolJson)], ValidationError> {
        match self {
            ProtocolJson::Map(entries) => Ok(entries),
            _ => Err(ValidationError::new("expected an object")),
        }
    }
}

// ============================================================================
// Field-access helpers (the manual equivalent of TypeBox's `Check` + the
// `additionalProperties: false` / `minLength: 1` constraints `schemas.ts`
// declares per type)
// ============================================================================

fn require_object<'a>(
    value: &'a ProtocolJson,
    context: &str,
) -> Result<&'a [(String, ProtocolJson)], ValidationError> {
    value
        .fields()
        .map_err(|_| ValidationError::new(format!("{context} must be an object")))
}

fn require_field<'a>(
    fields: &'a [(String, ProtocolJson)],
    key: &str,
    context: &str,
) -> Result<&'a ProtocolJson, ValidationError> {
    fields
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v)
        .ok_or_else(|| {
            ValidationError::new(format!("{context} is missing required field \"{key}\""))
        })
}

fn optional_field<'a>(fields: &'a [(String, ProtocolJson)], key: &str) -> Option<&'a ProtocolJson> {
    fields.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

/// `additionalProperties: false`.
fn deny_unknown_fields(
    fields: &[(String, ProtocolJson)],
    allowed: &[&str],
    context: &str,
) -> Result<(), ValidationError> {
    for (key, _) in fields {
        if !allowed.contains(&key.as_str()) {
            return Err(ValidationError::new(format!(
                "{context} has an unexpected field \"{key}\""
            )));
        }
    }
    Ok(())
}

/// `IdSchema = Type.String({ minLength: 1 })`.
fn require_id(value: &ProtocolJson, context: &str) -> Result<String, ValidationError> {
    match value {
        ProtocolJson::Text(s) if !s.is_empty() => Ok(s.clone()),
        _ => Err(ValidationError::new(format!(
            "{context} must be a non-empty string"
        ))),
    }
}

fn require_text(value: &ProtocolJson, context: &str) -> Result<String, ValidationError> {
    match value {
        ProtocolJson::Text(s) => Ok(s.clone()),
        _ => Err(ValidationError::new(format!("{context} must be a string"))),
    }
}

/// `Type.Integer({ minimum: 0 })`-style check (a plain, non-negative integer,
/// not JS's full unbounded-precision — `i64` is more than sufficient here).
fn require_integer(
    value: &ProtocolJson,
    minimum: Option<i64>,
    context: &str,
) -> Result<i64, ValidationError> {
    match value {
        ProtocolJson::Number(n) if n.is_finite() && *n == n.trunc() => {
            let integer = *n as i64;
            if let Some(min) = minimum {
                if integer < min {
                    return Err(ValidationError::new(format!("{context} must be >= {min}")));
                }
            }
            Ok(integer)
        }
        _ => Err(ValidationError::new(format!(
            "{context} must be an integer"
        ))),
    }
}

fn require_bool(value: &ProtocolJson, context: &str) -> Result<bool, ValidationError> {
    match value {
        ProtocolJson::Bool(b) => Ok(*b),
        _ => Err(ValidationError::new(format!("{context} must be a boolean"))),
    }
}

/// `Type.Number({ minimum: 0 })`-style check — a finite, non-negative
/// number (not necessarily an integer; used for cost fields).
fn require_number_min0(value: &ProtocolJson, context: &str) -> Result<f64, ValidationError> {
    match value {
        ProtocolJson::Number(n) if n.is_finite() && *n >= 0.0 => Ok(*n),
        _ => Err(ValidationError::new(format!(
            "{context} must be a number >= 0"
        ))),
    }
}

fn require_array<T>(
    value: &ProtocolJson,
    context: &str,
    parse_item: impl Fn(&ProtocolJson) -> Result<T, ValidationError>,
) -> Result<Vec<T>, ValidationError> {
    match value {
        ProtocolJson::Array(items) => items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                parse_item(item)
                    .map_err(|e| ValidationError::new(format!("{context}[{index}]: {}", e.0)))
            })
            .collect(),
        _ => Err(ValidationError::new(format!("{context} must be an array"))),
    }
}

// ============================================================================
// ProtocolErrorCode / ProtocolError
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolErrorCode {
    Version,
    Busy,
    SessionLocked,
    NotFound,
    InvalidRequest,
    NotImplemented,
    InternalError,
}

impl ProtocolErrorCode {
    fn parse(value: &ProtocolJson) -> Result<Self, ValidationError> {
        let text = require_text(value, "ProtocolErrorCode")?;
        match text.as_str() {
            "version" => Ok(Self::Version),
            "busy" => Ok(Self::Busy),
            "session_locked" => Ok(Self::SessionLocked),
            "not_found" => Ok(Self::NotFound),
            "invalid_request" => Ok(Self::InvalidRequest),
            "not_implemented" => Ok(Self::NotImplemented),
            "internal_error" => Ok(Self::InternalError),
            other => Err(ValidationError::new(format!(
                "unknown ProtocolErrorCode \"{other}\""
            ))),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Version => "version",
            Self::Busy => "busy",
            Self::SessionLocked => "session_locked",
            Self::NotFound => "not_found",
            Self::InvalidRequest => "invalid_request",
            Self::NotImplemented => "not_implemented",
            Self::InternalError => "internal_error",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProtocolError {
    pub code: ProtocolErrorCode,
    pub message: String,
    pub details: Option<ProtocolJson>,
}

impl ProtocolError {
    fn parse(value: &ProtocolJson) -> Result<Self, ValidationError> {
        let fields = require_object(value, "ProtocolError")?;
        deny_unknown_fields(fields, &["code", "message", "details"], "ProtocolError")?;
        let code = ProtocolErrorCode::parse(require_field(fields, "code", "ProtocolError")?)?;
        let message = require_text(
            require_field(fields, "message", "ProtocolError")?,
            "ProtocolError.message",
        )?;
        let details = optional_field(fields, "details").cloned();
        Ok(Self {
            code,
            message,
            details,
        })
    }

    fn to_json(&self) -> ProtocolJson {
        let mut fields = vec![
            (
                "code".to_string(),
                ProtocolJson::Text(self.code.as_str().to_string()),
            ),
            (
                "message".to_string(),
                ProtocolJson::Text(self.message.clone()),
            ),
        ];
        if let Some(details) = &self.details {
            fields.push(("details".to_string(), details.clone()));
        }
        ProtocolJson::Map(fields)
    }
}

// ============================================================================
// WAVE 4: the deep shape typing deferred from Wave 2 — ThinkingLevel,
// SessionPhase, ModelRef/ModelMetadata, content/Usage/TranscriptItem/
// TranscriptProgress, SessionMetadata/SessionSnapshot/ServerSnapshot,
// Command/CommandResult/ServerEvent. Field order for every `to_json` below
// is cross-checked against REAL construction call sites in `protocol.ts`
// (`toProtocolModelMetadata`/`toProtocolUsage`/`toProtocolUserMessage`/
// `toProtocolAssistantMessage`/`toProtocolToolResultMessage`) and
// `sessions.ts` (`toMetadata`) / `testing/service.ts` (`seed`'s literal
// `SessionSnapshot`) — not just `schemas.ts`'s declaration order — for every
// type that has one. `Command`'s own wire order (client → server) has no
// reference client in this checkout; it follows schema declaration order,
// the same documented-residual class as Wave 2's `ClientHello`.
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThinkingLevel {
    Off,
    Minimal,
    Low,
    Medium,
    High,
    XHigh,
    Max,
}

impl ThinkingLevel {
    fn parse(value: &ProtocolJson) -> Result<Self, ValidationError> {
        match require_text(value, "ThinkingLevel")?.as_str() {
            "off" => Ok(Self::Off),
            "minimal" => Ok(Self::Minimal),
            "low" => Ok(Self::Low),
            "medium" => Ok(Self::Medium),
            "high" => Ok(Self::High),
            "xhigh" => Ok(Self::XHigh),
            "max" => Ok(Self::Max),
            other => Err(ValidationError::new(format!(
                "unknown ThinkingLevel \"{other}\""
            ))),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Max => "max",
        }
    }

    fn to_json(self) -> ProtocolJson {
        ProtocolJson::Text(self.as_str().to_string())
    }
}

/// "Matches `AgentHarnessPhase`" per `schemas.ts`'s own comment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionPhase {
    Idle,
    Turn,
    Compaction,
    BranchSummary,
    Retry,
}

impl SessionPhase {
    fn parse(value: &ProtocolJson) -> Result<Self, ValidationError> {
        match require_text(value, "SessionPhase")?.as_str() {
            "idle" => Ok(Self::Idle),
            "turn" => Ok(Self::Turn),
            "compaction" => Ok(Self::Compaction),
            "branch_summary" => Ok(Self::BranchSummary),
            "retry" => Ok(Self::Retry),
            other => Err(ValidationError::new(format!(
                "unknown SessionPhase \"{other}\""
            ))),
        }
    }

    fn as_str(&self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Turn => "turn",
            Self::Compaction => "compaction",
            Self::BranchSummary => "branch_summary",
            Self::Retry => "retry",
        }
    }

    fn to_json(self) -> ProtocolJson {
        ProtocolJson::Text(self.as_str().to_string())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModelRef {
    pub provider: String,
    pub id: String,
}

impl ModelRef {
    fn parse(value: &ProtocolJson) -> Result<Self, ValidationError> {
        let fields = require_object(value, "ModelRef")?;
        deny_unknown_fields(fields, &["provider", "id"], "ModelRef")?;
        Ok(Self {
            provider: require_id(
                require_field(fields, "provider", "ModelRef")?,
                "ModelRef.provider",
            )?,
            id: require_id(require_field(fields, "id", "ModelRef")?, "ModelRef.id")?,
        })
    }

    fn to_json(&self) -> ProtocolJson {
        ProtocolJson::Map(vec![
            (
                "provider".to_string(),
                ProtocolJson::Text(self.provider.clone()),
            ),
            ("id".to_string(), ProtocolJson::Text(self.id.clone())),
        ])
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModelCost {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
}

impl ModelCost {
    fn parse(value: &ProtocolJson) -> Result<Self, ValidationError> {
        let fields = require_object(value, "ModelCost")?;
        deny_unknown_fields(
            fields,
            &["input", "output", "cacheRead", "cacheWrite"],
            "ModelCost",
        )?;
        Ok(Self {
            input: require_number_min0(
                require_field(fields, "input", "ModelCost")?,
                "ModelCost.input",
            )?,
            output: require_number_min0(
                require_field(fields, "output", "ModelCost")?,
                "ModelCost.output",
            )?,
            cache_read: require_number_min0(
                require_field(fields, "cacheRead", "ModelCost")?,
                "ModelCost.cacheRead",
            )?,
            cache_write: require_number_min0(
                require_field(fields, "cacheWrite", "ModelCost")?,
                "ModelCost.cacheWrite",
            )?,
        })
    }

    fn to_json(&self) -> ProtocolJson {
        ProtocolJson::Map(vec![
            ("input".to_string(), ProtocolJson::Number(self.input)),
            ("output".to_string(), ProtocolJson::Number(self.output)),
            (
                "cacheRead".to_string(),
                ProtocolJson::Number(self.cache_read),
            ),
            (
                "cacheWrite".to_string(),
                ProtocolJson::Number(self.cache_write),
            ),
        ])
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelInputKind {
    Text,
    Image,
}

impl ModelInputKind {
    fn parse(value: &ProtocolJson) -> Result<Self, ValidationError> {
        match require_text(value, "ModelMetadata.input item")?.as_str() {
            "text" => Ok(Self::Text),
            "image" => Ok(Self::Image),
            other => Err(ValidationError::new(format!(
                "unknown model input kind \"{other}\""
            ))),
        }
    }

    fn to_json(self) -> ProtocolJson {
        ProtocolJson::Text(
            match self {
                Self::Text => "text",
                Self::Image => "image",
            }
            .to_string(),
        )
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModelMetadata {
    pub provider: String,
    pub id: String,
    pub name: String,
    pub api: String,
    pub reasoning: bool,
    pub input: Vec<ModelInputKind>,
    pub context_window: i64,
    pub max_tokens: i64,
    pub cost: ModelCost,
    pub supported_thinking_levels: Vec<ThinkingLevel>,
    pub authenticated: bool,
}

impl ModelMetadata {
    fn parse(value: &ProtocolJson) -> Result<Self, ValidationError> {
        let fields = require_object(value, "ModelMetadata")?;
        deny_unknown_fields(
            fields,
            &[
                "provider",
                "id",
                "name",
                "api",
                "reasoning",
                "input",
                "contextWindow",
                "maxTokens",
                "cost",
                "supportedThinkingLevels",
                "authenticated",
            ],
            "ModelMetadata",
        )?;
        let supported_thinking_levels = require_array(
            require_field(fields, "supportedThinkingLevels", "ModelMetadata")?,
            "ModelMetadata.supportedThinkingLevels",
            ThinkingLevel::parse,
        )?;
        if supported_thinking_levels.is_empty() {
            return Err(ValidationError::new(
                "ModelMetadata.supportedThinkingLevels must have at least one item",
            ));
        }
        Ok(Self {
            provider: require_id(
                require_field(fields, "provider", "ModelMetadata")?,
                "ModelMetadata.provider",
            )?,
            id: require_id(
                require_field(fields, "id", "ModelMetadata")?,
                "ModelMetadata.id",
            )?,
            name: require_id(
                require_field(fields, "name", "ModelMetadata")?,
                "ModelMetadata.name",
            )?,
            api: require_id(
                require_field(fields, "api", "ModelMetadata")?,
                "ModelMetadata.api",
            )?,
            reasoning: require_bool(
                require_field(fields, "reasoning", "ModelMetadata")?,
                "ModelMetadata.reasoning",
            )?,
            input: require_array(
                require_field(fields, "input", "ModelMetadata")?,
                "ModelMetadata.input",
                ModelInputKind::parse,
            )?,
            context_window: require_integer(
                require_field(fields, "contextWindow", "ModelMetadata")?,
                Some(1),
                "ModelMetadata.contextWindow",
            )?,
            max_tokens: require_integer(
                require_field(fields, "maxTokens", "ModelMetadata")?,
                Some(1),
                "ModelMetadata.maxTokens",
            )?,
            cost: ModelCost::parse(require_field(fields, "cost", "ModelMetadata")?)?,
            supported_thinking_levels,
            authenticated: require_bool(
                require_field(fields, "authenticated", "ModelMetadata")?,
                "ModelMetadata.authenticated",
            )?,
        })
    }

    fn to_json(&self) -> ProtocolJson {
        ProtocolJson::Map(vec![
            (
                "provider".to_string(),
                ProtocolJson::Text(self.provider.clone()),
            ),
            ("id".to_string(), ProtocolJson::Text(self.id.clone())),
            ("name".to_string(), ProtocolJson::Text(self.name.clone())),
            ("api".to_string(), ProtocolJson::Text(self.api.clone())),
            ("reasoning".to_string(), ProtocolJson::Bool(self.reasoning)),
            (
                "input".to_string(),
                ProtocolJson::Array(self.input.iter().map(|k| k.to_json()).collect()),
            ),
            (
                "contextWindow".to_string(),
                ProtocolJson::Number(self.context_window as f64),
            ),
            (
                "maxTokens".to_string(),
                ProtocolJson::Number(self.max_tokens as f64),
            ),
            ("cost".to_string(), self.cost.to_json()),
            (
                "supportedThinkingLevels".to_string(),
                ProtocolJson::Array(
                    self.supported_thinking_levels
                        .iter()
                        .map(|t| t.to_json())
                        .collect(),
                ),
            ),
            (
                "authenticated".to_string(),
                ProtocolJson::Bool(self.authenticated),
            ),
        ])
    }
}

/// `UserContentSchema`/`ToolContentSchema` are structurally identical
/// (`text | image`) in `schemas.ts` today — one shared Rust type rather than
/// two identical enums, revisited if they ever diverge.
#[derive(Debug, Clone, PartialEq)]
pub enum TextOrImageContent {
    Text { text: String },
    Image { data: String, mime_type: String },
}

pub type UserContent = TextOrImageContent;
pub type ToolContent = TextOrImageContent;

impl TextOrImageContent {
    fn parse(value: &ProtocolJson) -> Result<Self, ValidationError> {
        let fields = require_object(value, "content")?;
        match require_field(fields, "type", "content")? {
            ProtocolJson::Text(t) if t == "text" => {
                deny_unknown_fields(fields, &["type", "text"], "content(text)")?;
                Ok(Self::Text {
                    text: require_text(require_field(fields, "text", "content")?, "content.text")?,
                })
            }
            ProtocolJson::Text(t) if t == "image" => {
                deny_unknown_fields(fields, &["type", "data", "mimeType"], "content(image)")?;
                Ok(Self::Image {
                    data: require_text(require_field(fields, "data", "content")?, "content.data")?,
                    mime_type: require_id(
                        require_field(fields, "mimeType", "content")?,
                        "content.mimeType",
                    )?,
                })
            }
            _ => Err(ValidationError::new(
                "content.type must be \"text\" or \"image\"",
            )),
        }
    }

    fn to_json(&self) -> ProtocolJson {
        match self {
            Self::Text { text } => ProtocolJson::Map(vec![
                ("type".to_string(), ProtocolJson::Text("text".to_string())),
                ("text".to_string(), ProtocolJson::Text(text.clone())),
            ]),
            Self::Image { data, mime_type } => ProtocolJson::Map(vec![
                ("type".to_string(), ProtocolJson::Text("image".to_string())),
                ("data".to_string(), ProtocolJson::Text(data.clone())),
                (
                    "mimeType".to_string(),
                    ProtocolJson::Text(mime_type.clone()),
                ),
            ]),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum AssistantContent {
    Text {
        text: String,
    },
    Thinking {
        thinking: String,
        redacted: Option<bool>,
    },
    ToolCall {
        tool_call_id: String,
        tool_name: String,
        input: ProtocolJson,
    },
}

impl AssistantContent {
    fn parse(value: &ProtocolJson) -> Result<Self, ValidationError> {
        let fields = require_object(value, "AssistantContent")?;
        match require_field(fields, "type", "AssistantContent")? {
            ProtocolJson::Text(t) if t == "text" => {
                deny_unknown_fields(fields, &["type", "text"], "AssistantContent(text)")?;
                Ok(Self::Text {
                    text: require_text(
                        require_field(fields, "text", "AssistantContent")?,
                        "AssistantContent.text",
                    )?,
                })
            }
            ProtocolJson::Text(t) if t == "thinking" => {
                deny_unknown_fields(
                    fields,
                    &["type", "thinking", "redacted"],
                    "AssistantContent(thinking)",
                )?;
                let thinking = require_text(
                    require_field(fields, "thinking", "AssistantContent")?,
                    "AssistantContent.thinking",
                )?;
                let redacted = match optional_field(fields, "redacted") {
                    Some(v) => Some(require_bool(v, "AssistantContent.redacted")?),
                    None => None,
                };
                Ok(Self::Thinking { thinking, redacted })
            }
            ProtocolJson::Text(t) if t == "toolCall" => {
                deny_unknown_fields(
                    fields,
                    &["type", "toolCallId", "toolName", "input"],
                    "AssistantContent(toolCall)",
                )?;
                Ok(Self::ToolCall {
                    tool_call_id: require_id(
                        require_field(fields, "toolCallId", "AssistantContent")?,
                        "AssistantContent.toolCallId",
                    )?,
                    tool_name: require_id(
                        require_field(fields, "toolName", "AssistantContent")?,
                        "AssistantContent.toolName",
                    )?,
                    input: require_field(fields, "input", "AssistantContent")?.clone(),
                })
            }
            _ => Err(ValidationError::new(
                "AssistantContent.type must be \"text\", \"thinking\", or \"toolCall\"",
            )),
        }
    }

    fn to_json(&self) -> ProtocolJson {
        match self {
            Self::Text { text } => ProtocolJson::Map(vec![
                ("type".to_string(), ProtocolJson::Text("text".to_string())),
                ("text".to_string(), ProtocolJson::Text(text.clone())),
            ]),
            Self::Thinking { thinking, redacted } => {
                let mut fields = vec![
                    (
                        "type".to_string(),
                        ProtocolJson::Text("thinking".to_string()),
                    ),
                    ("thinking".to_string(), ProtocolJson::Text(thinking.clone())),
                ];
                if let Some(r) = redacted {
                    fields.push(("redacted".to_string(), ProtocolJson::Bool(*r)));
                }
                ProtocolJson::Map(fields)
            }
            Self::ToolCall {
                tool_call_id,
                tool_name,
                input,
            } => ProtocolJson::Map(vec![
                (
                    "type".to_string(),
                    ProtocolJson::Text("toolCall".to_string()),
                ),
                (
                    "toolCallId".to_string(),
                    ProtocolJson::Text(tool_call_id.clone()),
                ),
                (
                    "toolName".to_string(),
                    ProtocolJson::Text(tool_name.clone()),
                ),
                ("input".to_string(), input.clone()),
            ]),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct UsageCost {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
    pub total: f64,
}

impl UsageCost {
    fn parse(value: &ProtocolJson) -> Result<Self, ValidationError> {
        let fields = require_object(value, "Usage.cost")?;
        deny_unknown_fields(
            fields,
            &["input", "output", "cacheRead", "cacheWrite", "total"],
            "Usage.cost",
        )?;
        Ok(Self {
            input: require_number_min0(
                require_field(fields, "input", "Usage.cost")?,
                "Usage.cost.input",
            )?,
            output: require_number_min0(
                require_field(fields, "output", "Usage.cost")?,
                "Usage.cost.output",
            )?,
            cache_read: require_number_min0(
                require_field(fields, "cacheRead", "Usage.cost")?,
                "Usage.cost.cacheRead",
            )?,
            cache_write: require_number_min0(
                require_field(fields, "cacheWrite", "Usage.cost")?,
                "Usage.cost.cacheWrite",
            )?,
            total: require_number_min0(
                require_field(fields, "total", "Usage.cost")?,
                "Usage.cost.total",
            )?,
        })
    }

    fn to_json(&self) -> ProtocolJson {
        ProtocolJson::Map(vec![
            ("input".to_string(), ProtocolJson::Number(self.input)),
            ("output".to_string(), ProtocolJson::Number(self.output)),
            (
                "cacheRead".to_string(),
                ProtocolJson::Number(self.cache_read),
            ),
            (
                "cacheWrite".to_string(),
                ProtocolJson::Number(self.cache_write),
            ),
            ("total".to_string(), ProtocolJson::Number(self.total)),
        ])
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Usage {
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_write: i64,
    /// Positioned between `cacheWrite` and `totalTokens` when present —
    /// confirmed against `toProtocolUsage`'s real spread order, not just
    /// `schemas.ts`'s declaration order.
    pub reasoning: Option<i64>,
    pub total_tokens: i64,
    pub cost: UsageCost,
}

impl Usage {
    fn parse(value: &ProtocolJson) -> Result<Self, ValidationError> {
        let fields = require_object(value, "Usage")?;
        deny_unknown_fields(
            fields,
            &[
                "input",
                "output",
                "cacheRead",
                "cacheWrite",
                "reasoning",
                "totalTokens",
                "cost",
            ],
            "Usage",
        )?;
        let reasoning = match optional_field(fields, "reasoning") {
            Some(v) => Some(require_integer(v, Some(0), "Usage.reasoning")?),
            None => None,
        };
        Ok(Self {
            input: require_integer(
                require_field(fields, "input", "Usage")?,
                Some(0),
                "Usage.input",
            )?,
            output: require_integer(
                require_field(fields, "output", "Usage")?,
                Some(0),
                "Usage.output",
            )?,
            cache_read: require_integer(
                require_field(fields, "cacheRead", "Usage")?,
                Some(0),
                "Usage.cacheRead",
            )?,
            cache_write: require_integer(
                require_field(fields, "cacheWrite", "Usage")?,
                Some(0),
                "Usage.cacheWrite",
            )?,
            reasoning,
            total_tokens: require_integer(
                require_field(fields, "totalTokens", "Usage")?,
                Some(0),
                "Usage.totalTokens",
            )?,
            cost: UsageCost::parse(require_field(fields, "cost", "Usage")?)?,
        })
    }

    fn to_json(&self) -> ProtocolJson {
        let mut fields = vec![
            ("input".to_string(), ProtocolJson::Number(self.input as f64)),
            (
                "output".to_string(),
                ProtocolJson::Number(self.output as f64),
            ),
            (
                "cacheRead".to_string(),
                ProtocolJson::Number(self.cache_read as f64),
            ),
            (
                "cacheWrite".to_string(),
                ProtocolJson::Number(self.cache_write as f64),
            ),
        ];
        if let Some(r) = self.reasoning {
            fields.push(("reasoning".to_string(), ProtocolJson::Number(r as f64)));
        }
        fields.push((
            "totalTokens".to_string(),
            ProtocolJson::Number(self.total_tokens as f64),
        ));
        fields.push(("cost".to_string(), self.cost.to_json()));
        ProtocolJson::Map(fields)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct UserTranscriptItem {
    pub id: String,
    pub content: Vec<UserContent>,
    pub timestamp: i64,
}

impl UserTranscriptItem {
    fn parse(fields: &[(String, ProtocolJson)]) -> Result<Self, ValidationError> {
        deny_unknown_fields(
            fields,
            &["id", "role", "content", "timestamp"],
            "UserTranscriptItem",
        )?;
        Ok(Self {
            id: require_id(
                require_field(fields, "id", "UserTranscriptItem")?,
                "UserTranscriptItem.id",
            )?,
            content: require_array(
                require_field(fields, "content", "UserTranscriptItem")?,
                "UserTranscriptItem.content",
                UserContent::parse,
            )?,
            timestamp: require_integer(
                require_field(fields, "timestamp", "UserTranscriptItem")?,
                Some(0),
                "UserTranscriptItem.timestamp",
            )?,
        })
    }

    fn to_json(&self) -> ProtocolJson {
        ProtocolJson::Map(vec![
            ("id".to_string(), ProtocolJson::Text(self.id.clone())),
            ("role".to_string(), ProtocolJson::Text("user".to_string())),
            (
                "content".to_string(),
                ProtocolJson::Array(self.content.iter().map(UserContent::to_json).collect()),
            ),
            (
                "timestamp".to_string(),
                ProtocolJson::Number(self.timestamp as f64),
            ),
        ])
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompleteStopReason {
    Stop,
    Length,
    ToolUse,
}

impl CompleteStopReason {
    fn parse(value: &ProtocolJson) -> Result<Self, ValidationError> {
        match require_text(value, "stopReason")?.as_str() {
            "stop" => Ok(Self::Stop),
            "length" => Ok(Self::Length),
            "toolUse" => Ok(Self::ToolUse),
            other => Err(ValidationError::new(format!(
                "unknown complete stopReason \"{other}\""
            ))),
        }
    }

    fn as_str(&self) -> &'static str {
        match self {
            Self::Stop => "stop",
            Self::Length => "length",
            Self::ToolUse => "toolUse",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AssistantTranscriptItemCommon {
    pub id: String,
    pub content: Vec<AssistantContent>,
    pub model: ModelRef,
    pub response_model: Option<String>,
    pub usage: Option<Usage>,
    pub timestamp: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AssistantTranscriptItem {
    Streaming(AssistantTranscriptItemCommon),
    Complete {
        common: AssistantTranscriptItemCommon,
        stop_reason: CompleteStopReason,
    },
    Error {
        common: AssistantTranscriptItemCommon,
        error_message: Option<String>,
    },
    Aborted {
        common: AssistantTranscriptItemCommon,
        error_message: Option<String>,
    },
}

impl AssistantTranscriptItem {
    fn parse(fields: &[(String, ProtocolJson)]) -> Result<Self, ValidationError> {
        let common = AssistantTranscriptItemCommon {
            id: require_id(
                require_field(fields, "id", "AssistantTranscriptItem")?,
                "AssistantTranscriptItem.id",
            )?,
            content: require_array(
                require_field(fields, "content", "AssistantTranscriptItem")?,
                "AssistantTranscriptItem.content",
                AssistantContent::parse,
            )?,
            model: ModelRef::parse(require_field(fields, "model", "AssistantTranscriptItem")?)?,
            response_model: match optional_field(fields, "responseModel") {
                Some(v) => Some(require_id(v, "AssistantTranscriptItem.responseModel")?),
                None => None,
            },
            usage: match optional_field(fields, "usage") {
                Some(v) => Some(Usage::parse(v)?),
                None => None,
            },
            timestamp: require_integer(
                require_field(fields, "timestamp", "AssistantTranscriptItem")?,
                Some(0),
                "AssistantTranscriptItem.timestamp",
            )?,
        };
        let base_allowed = [
            "id",
            "role",
            "content",
            "model",
            "responseModel",
            "usage",
            "timestamp",
            "status",
        ];
        match require_field(fields, "status", "AssistantTranscriptItem")? {
            ProtocolJson::Text(s) if s == "streaming" => {
                deny_unknown_fields(fields, &base_allowed, "AssistantTranscriptItem(streaming)")?;
                Ok(Self::Streaming(common))
            }
            ProtocolJson::Text(s) if s == "complete" => {
                let allowed: Vec<&str> =
                    base_allowed.iter().copied().chain(["stopReason"]).collect();
                deny_unknown_fields(fields, &allowed, "AssistantTranscriptItem(complete)")?;
                Ok(Self::Complete {
                    stop_reason: CompleteStopReason::parse(require_field(
                        fields,
                        "stopReason",
                        "AssistantTranscriptItem",
                    )?)?,
                    common,
                })
            }
            ProtocolJson::Text(s) if s == "error" => {
                let allowed: Vec<&str> = base_allowed
                    .iter()
                    .copied()
                    .chain(["stopReason", "errorMessage"])
                    .collect();
                deny_unknown_fields(fields, &allowed, "AssistantTranscriptItem(error)")?;
                require_text_literal(
                    require_field(fields, "stopReason", "AssistantTranscriptItem")?,
                    "error",
                    "AssistantTranscriptItem(error).stopReason",
                )?;
                let error_message = match optional_field(fields, "errorMessage") {
                    Some(v) => Some(require_id(v, "AssistantTranscriptItem.errorMessage")?),
                    None => None,
                };
                Ok(Self::Error {
                    common,
                    error_message,
                })
            }
            ProtocolJson::Text(s) if s == "aborted" => {
                let allowed: Vec<&str> = base_allowed
                    .iter()
                    .copied()
                    .chain(["stopReason", "errorMessage"])
                    .collect();
                deny_unknown_fields(fields, &allowed, "AssistantTranscriptItem(aborted)")?;
                require_text_literal(
                    require_field(fields, "stopReason", "AssistantTranscriptItem")?,
                    "aborted",
                    "AssistantTranscriptItem(aborted).stopReason",
                )?;
                let error_message = match optional_field(fields, "errorMessage") {
                    Some(v) => Some(require_text(v, "AssistantTranscriptItem.errorMessage")?),
                    None => None,
                };
                Ok(Self::Aborted {
                    common,
                    error_message,
                })
            }
            _ => Err(ValidationError::new(
                "AssistantTranscriptItem.status must be one of streaming|complete|error|aborted",
            )),
        }
    }

    fn to_json(&self) -> ProtocolJson {
        fn common_fields(common: &AssistantTranscriptItemCommon) -> Vec<(String, ProtocolJson)> {
            let mut fields = vec![
                ("id".to_string(), ProtocolJson::Text(common.id.clone())),
                (
                    "role".to_string(),
                    ProtocolJson::Text("assistant".to_string()),
                ),
                (
                    "content".to_string(),
                    ProtocolJson::Array(
                        common
                            .content
                            .iter()
                            .map(AssistantContent::to_json)
                            .collect(),
                    ),
                ),
                ("model".to_string(), common.model.to_json()),
            ];
            if let Some(rm) = &common.response_model {
                fields.push(("responseModel".to_string(), ProtocolJson::Text(rm.clone())));
            }
            if let Some(u) = &common.usage {
                fields.push(("usage".to_string(), u.to_json()));
            }
            fields.push((
                "timestamp".to_string(),
                ProtocolJson::Number(common.timestamp as f64),
            ));
            fields
        }
        match self {
            Self::Streaming(common) => {
                let mut fields = common_fields(common);
                fields.push((
                    "status".to_string(),
                    ProtocolJson::Text("streaming".to_string()),
                ));
                ProtocolJson::Map(fields)
            }
            Self::Complete {
                common,
                stop_reason,
            } => {
                let mut fields = common_fields(common);
                fields.push((
                    "status".to_string(),
                    ProtocolJson::Text("complete".to_string()),
                ));
                fields.push((
                    "stopReason".to_string(),
                    ProtocolJson::Text(stop_reason.as_str().to_string()),
                ));
                ProtocolJson::Map(fields)
            }
            Self::Error {
                common,
                error_message,
            } => {
                let mut fields = common_fields(common);
                fields.push((
                    "status".to_string(),
                    ProtocolJson::Text("error".to_string()),
                ));
                fields.push((
                    "stopReason".to_string(),
                    ProtocolJson::Text("error".to_string()),
                ));
                if let Some(m) = error_message {
                    fields.push(("errorMessage".to_string(), ProtocolJson::Text(m.clone())));
                }
                ProtocolJson::Map(fields)
            }
            Self::Aborted {
                common,
                error_message,
            } => {
                let mut fields = common_fields(common);
                fields.push((
                    "status".to_string(),
                    ProtocolJson::Text("aborted".to_string()),
                ));
                fields.push((
                    "stopReason".to_string(),
                    ProtocolJson::Text("aborted".to_string()),
                ));
                if let Some(m) = error_message {
                    fields.push(("errorMessage".to_string(), ProtocolJson::Text(m.clone())));
                }
                ProtocolJson::Map(fields)
            }
        }
    }
}

fn require_text_literal(
    value: &ProtocolJson,
    expected: &str,
    context: &str,
) -> Result<(), ValidationError> {
    match value {
        ProtocolJson::Text(s) if s == expected => Ok(()),
        _ => Err(ValidationError::new(format!(
            "{context} must be exactly \"{expected}\""
        ))),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolTranscriptItemCommon {
    pub id: String,
    pub tool_call_id: String,
    pub tool_name: String,
    pub input: ProtocolJson,
    pub content: Vec<ToolContent>,
    pub details: Option<ProtocolJson>,
    pub usage: Option<Usage>,
    pub timestamp: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ToolTranscriptItem {
    Running(ToolTranscriptItemCommon),
    Complete(ToolTranscriptItemCommon),
    Error(ToolTranscriptItemCommon),
}

impl ToolTranscriptItem {
    fn parse(fields: &[(String, ProtocolJson)]) -> Result<Self, ValidationError> {
        let allowed = [
            "id",
            "role",
            "toolCallId",
            "toolName",
            "input",
            "content",
            "details",
            "usage",
            "timestamp",
            "status",
            "isError",
        ];
        deny_unknown_fields(fields, &allowed, "ToolTranscriptItem")?;
        let common = ToolTranscriptItemCommon {
            id: require_id(
                require_field(fields, "id", "ToolTranscriptItem")?,
                "ToolTranscriptItem.id",
            )?,
            tool_call_id: require_id(
                require_field(fields, "toolCallId", "ToolTranscriptItem")?,
                "ToolTranscriptItem.toolCallId",
            )?,
            tool_name: require_id(
                require_field(fields, "toolName", "ToolTranscriptItem")?,
                "ToolTranscriptItem.toolName",
            )?,
            input: require_field(fields, "input", "ToolTranscriptItem")?.clone(),
            content: require_array(
                require_field(fields, "content", "ToolTranscriptItem")?,
                "ToolTranscriptItem.content",
                ToolContent::parse,
            )?,
            details: optional_field(fields, "details").cloned(),
            usage: match optional_field(fields, "usage") {
                Some(v) => Some(Usage::parse(v)?),
                None => None,
            },
            timestamp: require_integer(
                require_field(fields, "timestamp", "ToolTranscriptItem")?,
                Some(0),
                "ToolTranscriptItem.timestamp",
            )?,
        };
        let is_error = require_bool(
            require_field(fields, "isError", "ToolTranscriptItem")?,
            "ToolTranscriptItem.isError",
        )?;
        match require_field(fields, "status", "ToolTranscriptItem")? {
            ProtocolJson::Text(s) if s == "running" => {
                if is_error {
                    return Err(ValidationError::new(
                        "ToolTranscriptItem(running).isError must be false",
                    ));
                }
                Ok(Self::Running(common))
            }
            ProtocolJson::Text(s) if s == "complete" => {
                if is_error {
                    return Err(ValidationError::new(
                        "ToolTranscriptItem(complete).isError must be false",
                    ));
                }
                Ok(Self::Complete(common))
            }
            ProtocolJson::Text(s) if s == "error" => {
                if !is_error {
                    return Err(ValidationError::new(
                        "ToolTranscriptItem(error).isError must be true",
                    ));
                }
                Ok(Self::Error(common))
            }
            _ => Err(ValidationError::new(
                "ToolTranscriptItem.status must be one of running|complete|error",
            )),
        }
    }

    fn to_json(&self) -> ProtocolJson {
        fn common_fields(common: &ToolTranscriptItemCommon) -> Vec<(String, ProtocolJson)> {
            let mut fields = vec![
                ("id".to_string(), ProtocolJson::Text(common.id.clone())),
                ("role".to_string(), ProtocolJson::Text("tool".to_string())),
                (
                    "toolCallId".to_string(),
                    ProtocolJson::Text(common.tool_call_id.clone()),
                ),
                (
                    "toolName".to_string(),
                    ProtocolJson::Text(common.tool_name.clone()),
                ),
                ("input".to_string(), common.input.clone()),
                (
                    "content".to_string(),
                    ProtocolJson::Array(common.content.iter().map(ToolContent::to_json).collect()),
                ),
            ];
            if let Some(d) = &common.details {
                fields.push(("details".to_string(), d.clone()));
            }
            if let Some(u) = &common.usage {
                fields.push(("usage".to_string(), u.to_json()));
            }
            fields.push((
                "timestamp".to_string(),
                ProtocolJson::Number(common.timestamp as f64),
            ));
            fields
        }
        let (common, status, is_error) = match self {
            Self::Running(c) => (c, "running", false),
            Self::Complete(c) => (c, "complete", false),
            Self::Error(c) => (c, "error", true),
        };
        let mut fields = common_fields(common);
        fields.push(("status".to_string(), ProtocolJson::Text(status.to_string())));
        fields.push(("isError".to_string(), ProtocolJson::Bool(is_error)));
        ProtocolJson::Map(fields)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum TranscriptItem {
    User(UserTranscriptItem),
    Assistant(AssistantTranscriptItem),
    Tool(ToolTranscriptItem),
}

impl TranscriptItem {
    pub fn parse(value: &ProtocolJson) -> Result<Self, ValidationError> {
        let fields = require_object(value, "TranscriptItem")?;
        match require_field(fields, "role", "TranscriptItem")? {
            ProtocolJson::Text(r) if r == "user" => {
                Ok(Self::User(UserTranscriptItem::parse(fields)?))
            }
            ProtocolJson::Text(r) if r == "assistant" => {
                Ok(Self::Assistant(AssistantTranscriptItem::parse(fields)?))
            }
            ProtocolJson::Text(r) if r == "tool" => {
                Ok(Self::Tool(ToolTranscriptItem::parse(fields)?))
            }
            _ => Err(ValidationError::new(
                "TranscriptItem.role must be one of user|assistant|tool",
            )),
        }
    }

    pub fn to_json(&self) -> ProtocolJson {
        match self {
            Self::User(u) => u.to_json(),
            Self::Assistant(a) => a.to_json(),
            Self::Tool(t) => t.to_json(),
        }
    }

    /// Assistant items are terminal unless `Streaming`; tool items are
    /// terminal unless `Running`. User items are never a meaningful
    /// "terminal item" at all — callers must reject them separately.
    fn is_terminal(&self) -> bool {
        match self {
            Self::User(_) => false,
            Self::Assistant(a) => !matches!(a, AssistantTranscriptItem::Streaming(_)),
            Self::Tool(t) => !matches!(t, ToolTranscriptItem::Running(_)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentDeltaKind {
    Text,
    Thinking,
    ToolCall,
}

impl ContentDeltaKind {
    fn parse(value: &ProtocolJson) -> Result<Self, ValidationError> {
        match require_text(value, "assistant_delta.kind")?.as_str() {
            "text" => Ok(Self::Text),
            "thinking" => Ok(Self::Thinking),
            "toolCall" => Ok(Self::ToolCall),
            other => Err(ValidationError::new(format!(
                "unknown assistant_delta kind \"{other}\""
            ))),
        }
    }

    fn as_str(&self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Thinking => "thinking",
            Self::ToolCall => "toolCall",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum TranscriptProgress {
    ItemStarted {
        item: TranscriptItem,
    },
    AssistantDelta {
        message_id: String,
        content_index: i64,
        kind: ContentDeltaKind,
        delta: String,
    },
    /// `item` restricted to `Assistant`/`Tool` (any status) — no `User`.
    ItemUpdated {
        item: TranscriptItem,
    },
    /// `item` restricted to a *terminal* `Assistant`/`Tool` item (no
    /// `Streaming`/`Running`, no `User`).
    ItemFinished {
        item: TranscriptItem,
    },
}

impl TranscriptProgress {
    pub fn parse(value: &ProtocolJson) -> Result<Self, ValidationError> {
        let fields = require_object(value, "TranscriptProgress")?;
        match require_field(fields, "type", "TranscriptProgress")? {
            ProtocolJson::Text(t) if t == "item_started" => {
                deny_unknown_fields(fields, &["type", "item"], "TranscriptProgress(item_started)")?;
                Ok(Self::ItemStarted { item: TranscriptItem::parse(require_field(fields, "item", "TranscriptProgress")?)? })
            }
            ProtocolJson::Text(t) if t == "assistant_delta" => {
                deny_unknown_fields(fields, &["type", "messageId", "contentIndex", "kind", "delta"], "TranscriptProgress(assistant_delta)")?;
                Ok(Self::AssistantDelta {
                    message_id: require_id(require_field(fields, "messageId", "TranscriptProgress")?, "TranscriptProgress.messageId")?,
                    content_index: require_integer(require_field(fields, "contentIndex", "TranscriptProgress")?, Some(0), "TranscriptProgress.contentIndex")?,
                    kind: ContentDeltaKind::parse(require_field(fields, "kind", "TranscriptProgress")?)?,
                    delta: require_text(require_field(fields, "delta", "TranscriptProgress")?, "TranscriptProgress.delta")?,
                })
            }
            ProtocolJson::Text(t) if t == "item_updated" => {
                deny_unknown_fields(fields, &["type", "item"], "TranscriptProgress(item_updated)")?;
                let item = TranscriptItem::parse(require_field(fields, "item", "TranscriptProgress")?)?;
                if matches!(item, TranscriptItem::User(_)) {
                    return Err(ValidationError::new("TranscriptProgress(item_updated).item must be an assistant or tool item"));
                }
                Ok(Self::ItemUpdated { item })
            }
            ProtocolJson::Text(t) if t == "item_finished" => {
                deny_unknown_fields(fields, &["type", "item"], "TranscriptProgress(item_finished)")?;
                let item = TranscriptItem::parse(require_field(fields, "item", "TranscriptProgress")?)?;
                if !item.is_terminal() {
                    return Err(ValidationError::new(
                        "TranscriptProgress(item_finished).item must be a terminal assistant or tool item",
                    ));
                }
                Ok(Self::ItemFinished { item })
            }
            _ => Err(ValidationError::new(
                "TranscriptProgress.type must be one of item_started|assistant_delta|item_updated|item_finished",
            )),
        }
    }

    pub fn to_json(&self) -> ProtocolJson {
        match self {
            Self::ItemStarted { item } => ProtocolJson::Map(vec![
                (
                    "type".to_string(),
                    ProtocolJson::Text("item_started".to_string()),
                ),
                ("item".to_string(), item.to_json()),
            ]),
            Self::AssistantDelta {
                message_id,
                content_index,
                kind,
                delta,
            } => ProtocolJson::Map(vec![
                (
                    "type".to_string(),
                    ProtocolJson::Text("assistant_delta".to_string()),
                ),
                (
                    "messageId".to_string(),
                    ProtocolJson::Text(message_id.clone()),
                ),
                (
                    "contentIndex".to_string(),
                    ProtocolJson::Number(*content_index as f64),
                ),
                (
                    "kind".to_string(),
                    ProtocolJson::Text(kind.as_str().to_string()),
                ),
                ("delta".to_string(), ProtocolJson::Text(delta.clone())),
            ]),
            Self::ItemUpdated { item } => ProtocolJson::Map(vec![
                (
                    "type".to_string(),
                    ProtocolJson::Text("item_updated".to_string()),
                ),
                ("item".to_string(), item.to_json()),
            ]),
            Self::ItemFinished { item } => ProtocolJson::Map(vec![
                (
                    "type".to_string(),
                    ProtocolJson::Text("item_finished".to_string()),
                ),
                ("item".to_string(), item.to_json()),
            ]),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionMetadata {
    pub id: String,
    pub created_at: i64,
    pub updated_at: Option<i64>,
    pub parent_session_id: Option<String>,
    pub session_name: Option<String>,
    pub cwd: Option<String>,
}

impl SessionMetadata {
    fn parse(value: &ProtocolJson) -> Result<Self, ValidationError> {
        let fields = require_object(value, "SessionMetadata")?;
        deny_unknown_fields(
            fields,
            &[
                "id",
                "createdAt",
                "updatedAt",
                "parentSessionId",
                "sessionName",
                "cwd",
            ],
            "SessionMetadata",
        )?;
        Ok(Self {
            id: require_id(
                require_field(fields, "id", "SessionMetadata")?,
                "SessionMetadata.id",
            )?,
            created_at: require_integer(
                require_field(fields, "createdAt", "SessionMetadata")?,
                Some(0),
                "SessionMetadata.createdAt",
            )?,
            updated_at: match optional_field(fields, "updatedAt") {
                Some(v) => Some(require_integer(v, Some(0), "SessionMetadata.updatedAt")?),
                None => None,
            },
            parent_session_id: match optional_field(fields, "parentSessionId") {
                Some(v) => Some(require_id(v, "SessionMetadata.parentSessionId")?),
                None => None,
            },
            session_name: match optional_field(fields, "sessionName") {
                Some(v) => Some(require_text(v, "SessionMetadata.sessionName")?),
                None => None,
            },
            cwd: match optional_field(fields, "cwd") {
                Some(v) => Some(require_id(v, "SessionMetadata.cwd")?),
                None => None,
            },
        })
    }

    fn to_json(&self) -> ProtocolJson {
        let mut fields = vec![
            ("id".to_string(), ProtocolJson::Text(self.id.clone())),
            (
                "createdAt".to_string(),
                ProtocolJson::Number(self.created_at as f64),
            ),
        ];
        if let Some(v) = self.updated_at {
            fields.push(("updatedAt".to_string(), ProtocolJson::Number(v as f64)));
        }
        if let Some(v) = &self.parent_session_id {
            fields.push(("parentSessionId".to_string(), ProtocolJson::Text(v.clone())));
        }
        if let Some(v) = &self.session_name {
            fields.push(("sessionName".to_string(), ProtocolJson::Text(v.clone())));
        }
        if let Some(v) = &self.cwd {
            fields.push(("cwd".to_string(), ProtocolJson::Text(v.clone())));
        }
        ProtocolJson::Map(fields)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionSnapshot {
    pub id: String,
    pub name: Option<String>,
    pub cwd: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub phase: SessionPhase,
    pub model: ModelRef,
    pub thinking_level: ThinkingLevel,
    pub attached: bool,
    pub locked: bool,
    pub revision: i64,
    pub transcript: Vec<TranscriptItem>,
    pub queued_steer: Vec<UserTranscriptItem>,
    pub queued_steer_count: i64,
}

impl SessionSnapshot {
    pub fn parse(value: &ProtocolJson) -> Result<Self, ValidationError> {
        let fields = require_object(value, "SessionSnapshot")?;
        deny_unknown_fields(
            fields,
            &[
                "id",
                "name",
                "cwd",
                "createdAt",
                "updatedAt",
                "phase",
                "model",
                "thinkingLevel",
                "attached",
                "locked",
                "revision",
                "transcript",
                "queuedSteer",
                "queuedSteerCount",
            ],
            "SessionSnapshot",
        )?;
        Ok(Self {
            id: require_id(
                require_field(fields, "id", "SessionSnapshot")?,
                "SessionSnapshot.id",
            )?,
            name: match optional_field(fields, "name") {
                Some(v) => Some(require_text(v, "SessionSnapshot.name")?),
                None => None,
            },
            cwd: require_id(
                require_field(fields, "cwd", "SessionSnapshot")?,
                "SessionSnapshot.cwd",
            )?,
            created_at: require_integer(
                require_field(fields, "createdAt", "SessionSnapshot")?,
                Some(0),
                "SessionSnapshot.createdAt",
            )?,
            updated_at: require_integer(
                require_field(fields, "updatedAt", "SessionSnapshot")?,
                Some(0),
                "SessionSnapshot.updatedAt",
            )?,
            phase: SessionPhase::parse(require_field(fields, "phase", "SessionSnapshot")?)?,
            model: ModelRef::parse(require_field(fields, "model", "SessionSnapshot")?)?,
            thinking_level: ThinkingLevel::parse(require_field(
                fields,
                "thinkingLevel",
                "SessionSnapshot",
            )?)?,
            attached: require_bool(
                require_field(fields, "attached", "SessionSnapshot")?,
                "SessionSnapshot.attached",
            )?,
            locked: require_bool(
                require_field(fields, "locked", "SessionSnapshot")?,
                "SessionSnapshot.locked",
            )?,
            revision: require_integer(
                require_field(fields, "revision", "SessionSnapshot")?,
                Some(0),
                "SessionSnapshot.revision",
            )?,
            transcript: require_array(
                require_field(fields, "transcript", "SessionSnapshot")?,
                "SessionSnapshot.transcript",
                TranscriptItem::parse,
            )?,
            queued_steer: require_array(
                require_field(fields, "queuedSteer", "SessionSnapshot")?,
                "SessionSnapshot.queuedSteer",
                |v| match TranscriptItem::parse(v)? {
                    TranscriptItem::User(u) => Ok(u),
                    _ => Err(ValidationError::new(
                        "SessionSnapshot.queuedSteer items must be user transcript items",
                    )),
                },
            )?,
            queued_steer_count: require_integer(
                require_field(fields, "queuedSteerCount", "SessionSnapshot")?,
                Some(0),
                "SessionSnapshot.queuedSteerCount",
            )?,
        })
    }

    pub fn to_json(&self) -> ProtocolJson {
        let mut fields = vec![("id".to_string(), ProtocolJson::Text(self.id.clone()))];
        if let Some(n) = &self.name {
            fields.push(("name".to_string(), ProtocolJson::Text(n.clone())));
        }
        fields.push(("cwd".to_string(), ProtocolJson::Text(self.cwd.clone())));
        fields.push((
            "createdAt".to_string(),
            ProtocolJson::Number(self.created_at as f64),
        ));
        fields.push((
            "updatedAt".to_string(),
            ProtocolJson::Number(self.updated_at as f64),
        ));
        fields.push(("phase".to_string(), self.phase.to_json()));
        fields.push(("model".to_string(), self.model.to_json()));
        fields.push(("thinkingLevel".to_string(), self.thinking_level.to_json()));
        fields.push(("attached".to_string(), ProtocolJson::Bool(self.attached)));
        fields.push(("locked".to_string(), ProtocolJson::Bool(self.locked)));
        fields.push((
            "revision".to_string(),
            ProtocolJson::Number(self.revision as f64),
        ));
        fields.push((
            "transcript".to_string(),
            ProtocolJson::Array(
                self.transcript
                    .iter()
                    .map(TranscriptItem::to_json)
                    .collect(),
            ),
        ));
        fields.push((
            "queuedSteer".to_string(),
            ProtocolJson::Array(
                self.queued_steer
                    .iter()
                    .map(UserTranscriptItem::to_json)
                    .collect(),
            ),
        ));
        fields.push((
            "queuedSteerCount".to_string(),
            ProtocolJson::Number(self.queued_steer_count as f64),
        ));
        ProtocolJson::Map(fields)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ServerSnapshot {
    pub server_id: String,
    pub revision: i64,
    pub sessions: Vec<SessionMetadata>,
    pub models: Vec<ModelMetadata>,
}

impl ServerSnapshot {
    fn parse(value: &ProtocolJson) -> Result<Self, ValidationError> {
        let fields = require_object(value, "ServerSnapshot")?;
        deny_unknown_fields(
            fields,
            &[
                "serverId",
                "protocolVersion",
                "revision",
                "sessions",
                "models",
            ],
            "ServerSnapshot",
        )?;
        require_string_literal_number(
            require_field(fields, "protocolVersion", "ServerSnapshot")?,
            PROTOCOL_VERSION as f64,
            "ServerSnapshot.protocolVersion",
        )?;
        Ok(Self {
            server_id: require_id(
                require_field(fields, "serverId", "ServerSnapshot")?,
                "ServerSnapshot.serverId",
            )?,
            revision: require_integer(
                require_field(fields, "revision", "ServerSnapshot")?,
                Some(0),
                "ServerSnapshot.revision",
            )?,
            sessions: require_array(
                require_field(fields, "sessions", "ServerSnapshot")?,
                "ServerSnapshot.sessions",
                SessionMetadata::parse,
            )?,
            models: require_array(
                require_field(fields, "models", "ServerSnapshot")?,
                "ServerSnapshot.models",
                ModelMetadata::parse,
            )?,
        })
    }

    fn to_json(&self) -> ProtocolJson {
        ProtocolJson::Map(vec![
            (
                "serverId".to_string(),
                ProtocolJson::Text(self.server_id.clone()),
            ),
            (
                "protocolVersion".to_string(),
                ProtocolJson::Number(PROTOCOL_VERSION as f64),
            ),
            (
                "revision".to_string(),
                ProtocolJson::Number(self.revision as f64),
            ),
            (
                "sessions".to_string(),
                ProtocolJson::Array(self.sessions.iter().map(SessionMetadata::to_json).collect()),
            ),
            (
                "models".to_string(),
                ProtocolJson::Array(self.models.iter().map(ModelMetadata::to_json).collect()),
            ),
        ])
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    List,
    Create {
        cwd: Option<String>,
        name: Option<String>,
        model: Option<ModelRef>,
        thinking_level: Option<ThinkingLevel>,
    },
    Attach {
        session_id: String,
    },
    Detach {
        session_id: String,
    },
    Prompt {
        session_id: String,
        text: String,
    },
    Steer {
        session_id: String,
        text: String,
    },
    Abort {
        session_id: String,
    },
    SetModel {
        session_id: String,
        model: ModelRef,
    },
    SetThinking {
        session_id: String,
        thinking_level: ThinkingLevel,
    },
}

impl Command {
    pub fn parse(value: &ProtocolJson) -> Result<Self, ValidationError> {
        let fields = require_object(value, "Command")?;
        match require_field(fields, "command", "Command")? {
            ProtocolJson::Text(c) if c == "list" => {
                deny_unknown_fields(fields, &["command"], "Command(list)")?;
                Ok(Self::List)
            }
            ProtocolJson::Text(c) if c == "create" => {
                deny_unknown_fields(
                    fields,
                    &["command", "cwd", "name", "model", "thinkingLevel"],
                    "Command(create)",
                )?;
                Ok(Self::Create {
                    cwd: match optional_field(fields, "cwd") {
                        Some(v) => Some(require_id(v, "Command(create).cwd")?),
                        None => None,
                    },
                    name: match optional_field(fields, "name") {
                        Some(v) => Some(require_text(v, "Command(create).name")?),
                        None => None,
                    },
                    model: match optional_field(fields, "model") {
                        Some(v) => Some(ModelRef::parse(v)?),
                        None => None,
                    },
                    thinking_level: match optional_field(fields, "thinkingLevel") {
                        Some(v) => Some(ThinkingLevel::parse(v)?),
                        None => None,
                    },
                })
            }
            ProtocolJson::Text(c) if c == "attach" => {
                deny_unknown_fields(fields, &["command", "sessionId"], "Command(attach)")?;
                Ok(Self::Attach {
                    session_id: require_id(
                        require_field(fields, "sessionId", "Command")?,
                        "Command(attach).sessionId",
                    )?,
                })
            }
            ProtocolJson::Text(c) if c == "detach" => {
                deny_unknown_fields(fields, &["command", "sessionId"], "Command(detach)")?;
                Ok(Self::Detach {
                    session_id: require_id(
                        require_field(fields, "sessionId", "Command")?,
                        "Command(detach).sessionId",
                    )?,
                })
            }
            ProtocolJson::Text(c) if c == "prompt" => {
                deny_unknown_fields(fields, &["command", "sessionId", "text"], "Command(prompt)")?;
                Ok(Self::Prompt {
                    session_id: require_id(
                        require_field(fields, "sessionId", "Command")?,
                        "Command(prompt).sessionId",
                    )?,
                    text: require_text(
                        require_field(fields, "text", "Command")?,
                        "Command(prompt).text",
                    )?,
                })
            }
            ProtocolJson::Text(c) if c == "steer" => {
                deny_unknown_fields(fields, &["command", "sessionId", "text"], "Command(steer)")?;
                Ok(Self::Steer {
                    session_id: require_id(
                        require_field(fields, "sessionId", "Command")?,
                        "Command(steer).sessionId",
                    )?,
                    text: require_text(
                        require_field(fields, "text", "Command")?,
                        "Command(steer).text",
                    )?,
                })
            }
            ProtocolJson::Text(c) if c == "abort" => {
                deny_unknown_fields(fields, &["command", "sessionId"], "Command(abort)")?;
                Ok(Self::Abort {
                    session_id: require_id(
                        require_field(fields, "sessionId", "Command")?,
                        "Command(abort).sessionId",
                    )?,
                })
            }
            ProtocolJson::Text(c) if c == "set_model" => {
                deny_unknown_fields(
                    fields,
                    &["command", "sessionId", "model"],
                    "Command(set_model)",
                )?;
                Ok(Self::SetModel {
                    session_id: require_id(
                        require_field(fields, "sessionId", "Command")?,
                        "Command(set_model).sessionId",
                    )?,
                    model: ModelRef::parse(require_field(fields, "model", "Command")?)?,
                })
            }
            ProtocolJson::Text(c) if c == "set_thinking" => {
                deny_unknown_fields(
                    fields,
                    &["command", "sessionId", "thinkingLevel"],
                    "Command(set_thinking)",
                )?;
                Ok(Self::SetThinking {
                    session_id: require_id(
                        require_field(fields, "sessionId", "Command")?,
                        "Command(set_thinking).sessionId",
                    )?,
                    thinking_level: ThinkingLevel::parse(require_field(
                        fields,
                        "thinkingLevel",
                        "Command",
                    )?)?,
                })
            }
            _ => Err(ValidationError::new("unknown Command variant")),
        }
    }

    pub fn to_json(&self) -> ProtocolJson {
        match self {
            Self::List => ProtocolJson::Map(vec![(
                "command".to_string(),
                ProtocolJson::Text("list".to_string()),
            )]),
            Self::Create {
                cwd,
                name,
                model,
                thinking_level,
            } => {
                let mut fields = vec![(
                    "command".to_string(),
                    ProtocolJson::Text("create".to_string()),
                )];
                if let Some(v) = cwd {
                    fields.push(("cwd".to_string(), ProtocolJson::Text(v.clone())));
                }
                if let Some(v) = name {
                    fields.push(("name".to_string(), ProtocolJson::Text(v.clone())));
                }
                if let Some(v) = model {
                    fields.push(("model".to_string(), v.to_json()));
                }
                if let Some(v) = thinking_level {
                    fields.push(("thinkingLevel".to_string(), v.to_json()));
                }
                ProtocolJson::Map(fields)
            }
            Self::Attach { session_id } => ProtocolJson::Map(vec![
                (
                    "command".to_string(),
                    ProtocolJson::Text("attach".to_string()),
                ),
                (
                    "sessionId".to_string(),
                    ProtocolJson::Text(session_id.clone()),
                ),
            ]),
            Self::Detach { session_id } => ProtocolJson::Map(vec![
                (
                    "command".to_string(),
                    ProtocolJson::Text("detach".to_string()),
                ),
                (
                    "sessionId".to_string(),
                    ProtocolJson::Text(session_id.clone()),
                ),
            ]),
            Self::Prompt { session_id, text } => ProtocolJson::Map(vec![
                (
                    "command".to_string(),
                    ProtocolJson::Text("prompt".to_string()),
                ),
                (
                    "sessionId".to_string(),
                    ProtocolJson::Text(session_id.clone()),
                ),
                ("text".to_string(), ProtocolJson::Text(text.clone())),
            ]),
            Self::Steer { session_id, text } => ProtocolJson::Map(vec![
                (
                    "command".to_string(),
                    ProtocolJson::Text("steer".to_string()),
                ),
                (
                    "sessionId".to_string(),
                    ProtocolJson::Text(session_id.clone()),
                ),
                ("text".to_string(), ProtocolJson::Text(text.clone())),
            ]),
            Self::Abort { session_id } => ProtocolJson::Map(vec![
                (
                    "command".to_string(),
                    ProtocolJson::Text("abort".to_string()),
                ),
                (
                    "sessionId".to_string(),
                    ProtocolJson::Text(session_id.clone()),
                ),
            ]),
            Self::SetModel { session_id, model } => ProtocolJson::Map(vec![
                (
                    "command".to_string(),
                    ProtocolJson::Text("set_model".to_string()),
                ),
                (
                    "sessionId".to_string(),
                    ProtocolJson::Text(session_id.clone()),
                ),
                ("model".to_string(), model.to_json()),
            ]),
            Self::SetThinking {
                session_id,
                thinking_level,
            } => ProtocolJson::Map(vec![
                (
                    "command".to_string(),
                    ProtocolJson::Text("set_thinking".to_string()),
                ),
                (
                    "sessionId".to_string(),
                    ProtocolJson::Text(session_id.clone()),
                ),
                ("thinkingLevel".to_string(), thinking_level.to_json()),
            ]),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum CommandResult {
    List { sessions: Vec<SessionMetadata> },
    Create { session: SessionSnapshot },
    Attach { session: SessionSnapshot },
    Detach { session_id: String },
    Prompt { session: SessionSnapshot },
    Steer { session: SessionSnapshot },
    Abort { session: SessionSnapshot },
    SetModel { session: SessionSnapshot },
    SetThinking { session: SessionSnapshot },
}

impl CommandResult {
    pub fn parse(value: &ProtocolJson) -> Result<Self, ValidationError> {
        let fields = require_object(value, "CommandResult")?;
        match require_field(fields, "command", "CommandResult")? {
            ProtocolJson::Text(c) if c == "list" => {
                deny_unknown_fields(fields, &["command", "sessions"], "CommandResult(list)")?;
                Ok(Self::List {
                    sessions: require_array(
                        require_field(fields, "sessions", "CommandResult")?,
                        "CommandResult(list).sessions",
                        SessionMetadata::parse,
                    )?,
                })
            }
            ProtocolJson::Text(c) if c == "create" => {
                deny_unknown_fields(fields, &["command", "session"], "CommandResult(create)")?;
                Ok(Self::Create {
                    session: SessionSnapshot::parse(require_field(
                        fields,
                        "session",
                        "CommandResult",
                    )?)?,
                })
            }
            ProtocolJson::Text(c) if c == "attach" => {
                deny_unknown_fields(fields, &["command", "session"], "CommandResult(attach)")?;
                Ok(Self::Attach {
                    session: SessionSnapshot::parse(require_field(
                        fields,
                        "session",
                        "CommandResult",
                    )?)?,
                })
            }
            ProtocolJson::Text(c) if c == "detach" => {
                deny_unknown_fields(fields, &["command", "sessionId"], "CommandResult(detach)")?;
                Ok(Self::Detach {
                    session_id: require_id(
                        require_field(fields, "sessionId", "CommandResult")?,
                        "CommandResult(detach).sessionId",
                    )?,
                })
            }
            ProtocolJson::Text(c) if c == "prompt" => {
                deny_unknown_fields(fields, &["command", "session"], "CommandResult(prompt)")?;
                Ok(Self::Prompt {
                    session: SessionSnapshot::parse(require_field(
                        fields,
                        "session",
                        "CommandResult",
                    )?)?,
                })
            }
            ProtocolJson::Text(c) if c == "steer" => {
                deny_unknown_fields(fields, &["command", "session"], "CommandResult(steer)")?;
                Ok(Self::Steer {
                    session: SessionSnapshot::parse(require_field(
                        fields,
                        "session",
                        "CommandResult",
                    )?)?,
                })
            }
            ProtocolJson::Text(c) if c == "abort" => {
                deny_unknown_fields(fields, &["command", "session"], "CommandResult(abort)")?;
                Ok(Self::Abort {
                    session: SessionSnapshot::parse(require_field(
                        fields,
                        "session",
                        "CommandResult",
                    )?)?,
                })
            }
            ProtocolJson::Text(c) if c == "set_model" => {
                deny_unknown_fields(fields, &["command", "session"], "CommandResult(set_model)")?;
                Ok(Self::SetModel {
                    session: SessionSnapshot::parse(require_field(
                        fields,
                        "session",
                        "CommandResult",
                    )?)?,
                })
            }
            ProtocolJson::Text(c) if c == "set_thinking" => {
                deny_unknown_fields(
                    fields,
                    &["command", "session"],
                    "CommandResult(set_thinking)",
                )?;
                Ok(Self::SetThinking {
                    session: SessionSnapshot::parse(require_field(
                        fields,
                        "session",
                        "CommandResult",
                    )?)?,
                })
            }
            _ => Err(ValidationError::new("unknown CommandResult variant")),
        }
    }

    pub fn to_json(&self) -> ProtocolJson {
        match self {
            Self::List { sessions } => ProtocolJson::Map(vec![
                (
                    "command".to_string(),
                    ProtocolJson::Text("list".to_string()),
                ),
                (
                    "sessions".to_string(),
                    ProtocolJson::Array(sessions.iter().map(SessionMetadata::to_json).collect()),
                ),
            ]),
            Self::Create { session } => ProtocolJson::Map(vec![
                (
                    "command".to_string(),
                    ProtocolJson::Text("create".to_string()),
                ),
                ("session".to_string(), session.to_json()),
            ]),
            Self::Attach { session } => ProtocolJson::Map(vec![
                (
                    "command".to_string(),
                    ProtocolJson::Text("attach".to_string()),
                ),
                ("session".to_string(), session.to_json()),
            ]),
            Self::Detach { session_id } => ProtocolJson::Map(vec![
                (
                    "command".to_string(),
                    ProtocolJson::Text("detach".to_string()),
                ),
                (
                    "sessionId".to_string(),
                    ProtocolJson::Text(session_id.clone()),
                ),
            ]),
            Self::Prompt { session } => ProtocolJson::Map(vec![
                (
                    "command".to_string(),
                    ProtocolJson::Text("prompt".to_string()),
                ),
                ("session".to_string(), session.to_json()),
            ]),
            Self::Steer { session } => ProtocolJson::Map(vec![
                (
                    "command".to_string(),
                    ProtocolJson::Text("steer".to_string()),
                ),
                ("session".to_string(), session.to_json()),
            ]),
            Self::Abort { session } => ProtocolJson::Map(vec![
                (
                    "command".to_string(),
                    ProtocolJson::Text("abort".to_string()),
                ),
                ("session".to_string(), session.to_json()),
            ]),
            Self::SetModel { session } => ProtocolJson::Map(vec![
                (
                    "command".to_string(),
                    ProtocolJson::Text("set_model".to_string()),
                ),
                ("session".to_string(), session.to_json()),
            ]),
            Self::SetThinking { session } => ProtocolJson::Map(vec![
                (
                    "command".to_string(),
                    ProtocolJson::Text("set_thinking".to_string()),
                ),
                ("session".to_string(), session.to_json()),
            ]),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ServerEvent {
    ServerSnapshot {
        snapshot: ServerSnapshot,
    },
    SessionSnapshot {
        snapshot: SessionSnapshot,
    },
    SessionProgress {
        session_id: String,
        progress: TranscriptProgress,
    },
    /// Named, not silent: `schemas.ts` declares this variant but it is never
    /// constructed anywhere in `server.ts`/`sessions.ts` in this checkout —
    /// schema-ready, not yet wired on the real Pi side either.
    SessionRemoved {
        session_id: String,
    },
}

impl ServerEvent {
    pub fn parse(value: &ProtocolJson) -> Result<Self, ValidationError> {
        let fields = require_object(value, "ServerEvent")?;
        match require_field(fields, "type", "ServerEvent")? {
            ProtocolJson::Text(t) if t == "server_snapshot" => {
                deny_unknown_fields(
                    fields,
                    &["type", "snapshot"],
                    "ServerEvent(server_snapshot)",
                )?;
                Ok(Self::ServerSnapshot {
                    snapshot: ServerSnapshot::parse(require_field(
                        fields,
                        "snapshot",
                        "ServerEvent",
                    )?)?,
                })
            }
            ProtocolJson::Text(t) if t == "session_snapshot" => {
                deny_unknown_fields(
                    fields,
                    &["type", "snapshot"],
                    "ServerEvent(session_snapshot)",
                )?;
                Ok(Self::SessionSnapshot {
                    snapshot: SessionSnapshot::parse(require_field(
                        fields,
                        "snapshot",
                        "ServerEvent",
                    )?)?,
                })
            }
            ProtocolJson::Text(t) if t == "session_progress" => {
                deny_unknown_fields(
                    fields,
                    &["type", "sessionId", "progress"],
                    "ServerEvent(session_progress)",
                )?;
                Ok(Self::SessionProgress {
                    session_id: require_id(
                        require_field(fields, "sessionId", "ServerEvent")?,
                        "ServerEvent(session_progress).sessionId",
                    )?,
                    progress: TranscriptProgress::parse(require_field(
                        fields,
                        "progress",
                        "ServerEvent",
                    )?)?,
                })
            }
            ProtocolJson::Text(t) if t == "session_removed" => {
                deny_unknown_fields(
                    fields,
                    &["type", "sessionId"],
                    "ServerEvent(session_removed)",
                )?;
                Ok(Self::SessionRemoved {
                    session_id: require_id(
                        require_field(fields, "sessionId", "ServerEvent")?,
                        "ServerEvent(session_removed).sessionId",
                    )?,
                })
            }
            _ => Err(ValidationError::new("unknown ServerEvent variant")),
        }
    }

    pub fn to_json(&self) -> ProtocolJson {
        match self {
            Self::ServerSnapshot { snapshot } => ProtocolJson::Map(vec![
                (
                    "type".to_string(),
                    ProtocolJson::Text("server_snapshot".to_string()),
                ),
                ("snapshot".to_string(), snapshot.to_json()),
            ]),
            Self::SessionSnapshot { snapshot } => ProtocolJson::Map(vec![
                (
                    "type".to_string(),
                    ProtocolJson::Text("session_snapshot".to_string()),
                ),
                ("snapshot".to_string(), snapshot.to_json()),
            ]),
            Self::SessionProgress {
                session_id,
                progress,
            } => ProtocolJson::Map(vec![
                (
                    "type".to_string(),
                    ProtocolJson::Text("session_progress".to_string()),
                ),
                (
                    "sessionId".to_string(),
                    ProtocolJson::Text(session_id.clone()),
                ),
                ("progress".to_string(), progress.to_json()),
            ]),
            Self::SessionRemoved { session_id } => ProtocolJson::Map(vec![
                (
                    "type".to_string(),
                    ProtocolJson::Text("session_removed".to_string()),
                ),
                (
                    "sessionId".to_string(),
                    ProtocolJson::Text(session_id.clone()),
                ),
            ]),
        }
    }
}

// ============================================================================
// Client -> Server: ClientHello, RequestEnvelope, ClientMessage
// ============================================================================

#[derive(Debug, Clone, PartialEq)]
pub struct ClientHello {
    /// `Type.Integer({ minimum: 0 })` — open for negotiation, unlike
    /// [`ServerHello`]'s exact-literal version.
    pub version: i64,
}

impl ClientHello {
    fn parse(fields: &[(String, ProtocolJson)]) -> Result<Self, ValidationError> {
        deny_unknown_fields(fields, &["type", "version"], "ClientHello")?;
        let version = require_integer(
            require_field(fields, "version", "ClientHello")?,
            Some(0),
            "ClientHello.version",
        )?;
        Ok(Self { version })
    }

    fn to_json(&self) -> ProtocolJson {
        ProtocolJson::Map(vec![
            ("type".to_string(), ProtocolJson::Text("hello".to_string())),
            (
                "version".to_string(),
                ProtocolJson::Number(self.version as f64),
            ),
        ])
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RequestEnvelope {
    pub id: String,
    pub request: Command,
}

impl RequestEnvelope {
    fn parse(fields: &[(String, ProtocolJson)]) -> Result<Self, ValidationError> {
        deny_unknown_fields(fields, &["type", "id", "request"], "RequestEnvelope")?;
        let id = require_id(
            require_field(fields, "id", "RequestEnvelope")?,
            "RequestEnvelope.id",
        )?;
        let request = Command::parse(require_field(fields, "request", "RequestEnvelope")?)?;
        Ok(Self { id, request })
    }

    fn to_json(&self) -> ProtocolJson {
        ProtocolJson::Map(vec![
            (
                "type".to_string(),
                ProtocolJson::Text("request".to_string()),
            ),
            ("id".to_string(), ProtocolJson::Text(self.id.clone())),
            ("request".to_string(), self.request.to_json()),
        ])
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ClientMessage {
    Hello(ClientHello),
    Request(RequestEnvelope),
}

impl ClientMessage {
    pub fn parse(value: &ProtocolJson) -> Result<Self, ValidationError> {
        let fields = require_object(value, "ClientMessage")?;
        match require_field(fields, "type", "ClientMessage")? {
            ProtocolJson::Text(t) if t == "hello" => Ok(Self::Hello(ClientHello::parse(fields)?)),
            ProtocolJson::Text(t) if t == "request" => {
                Ok(Self::Request(RequestEnvelope::parse(fields)?))
            }
            _ => Err(ValidationError::new(
                "ClientMessage.type must be \"hello\" or \"request\"",
            )),
        }
    }

    pub fn to_json(&self) -> ProtocolJson {
        match self {
            Self::Hello(hello) => hello.to_json(),
            Self::Request(request) => request.to_json(),
        }
    }
}

// ============================================================================
// Server -> Client: ServerHello, ServerHelloError, ResponseEnvelope,
// EventEnvelope, ServerMessage
// ============================================================================

#[derive(Debug, Clone, PartialEq)]
pub struct ServerHello {
    pub connection_id: String,
    pub snapshot: ServerSnapshot,
}

impl ServerHello {
    fn parse(fields: &[(String, ProtocolJson)]) -> Result<Self, ValidationError> {
        deny_unknown_fields(
            fields,
            &["type", "version", "connectionId", "snapshot"],
            "ServerHello",
        )?;
        // `Type.Literal(PROTOCOL_VERSION)` — unlike ClientHello, no negotiation.
        require_string_literal_number(
            require_field(fields, "version", "ServerHello")?,
            PROTOCOL_VERSION as f64,
            "ServerHello.version",
        )?;
        let connection_id = require_id(
            require_field(fields, "connectionId", "ServerHello")?,
            "ServerHello.connectionId",
        )?;
        let snapshot = ServerSnapshot::parse(require_field(fields, "snapshot", "ServerHello")?)?;
        Ok(Self {
            connection_id,
            snapshot,
        })
    }

    fn to_json(&self) -> ProtocolJson {
        ProtocolJson::Map(vec![
            ("type".to_string(), ProtocolJson::Text("hello".to_string())),
            (
                "version".to_string(),
                ProtocolJson::Number(PROTOCOL_VERSION as f64),
            ),
            (
                "connectionId".to_string(),
                ProtocolJson::Text(self.connection_id.clone()),
            ),
            ("snapshot".to_string(), self.snapshot.to_json()),
        ])
    }
}

fn require_string_literal_number(
    value: &ProtocolJson,
    expected: f64,
    context: &str,
) -> Result<(), ValidationError> {
    match value {
        ProtocolJson::Number(n) if *n == expected => Ok(()),
        _ => Err(ValidationError::new(format!(
            "{context} must be exactly {expected}"
        ))),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ServerHelloError {
    pub error: ProtocolError,
}

impl ServerHelloError {
    fn parse(fields: &[(String, ProtocolJson)]) -> Result<Self, ValidationError> {
        deny_unknown_fields(fields, &["type", "error"], "ServerHelloError")?;
        let error = ProtocolError::parse(require_field(fields, "error", "ServerHelloError")?)?;
        Ok(Self { error })
    }

    fn to_json(&self) -> ProtocolJson {
        ProtocolJson::Map(vec![
            (
                "type".to_string(),
                ProtocolJson::Text("hello_error".to_string()),
            ),
            ("error".to_string(), self.error.to_json()),
        ])
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ResponseEnvelope {
    Success { id: String, result: CommandResult },
    Failure { id: String, error: ProtocolError },
}

impl ResponseEnvelope {
    fn parse(fields: &[(String, ProtocolJson)]) -> Result<Self, ValidationError> {
        let id = require_id(
            require_field(fields, "id", "ResponseEnvelope")?,
            "ResponseEnvelope.id",
        )?;
        match require_field(fields, "ok", "ResponseEnvelope")? {
            ProtocolJson::Bool(true) => {
                deny_unknown_fields(fields, &["type", "id", "ok", "result"], "ResponseEnvelope")?;
                let result =
                    CommandResult::parse(require_field(fields, "result", "ResponseEnvelope")?)?;
                Ok(Self::Success { id, result })
            }
            ProtocolJson::Bool(false) => {
                deny_unknown_fields(fields, &["type", "id", "ok", "error"], "ResponseEnvelope")?;
                let error =
                    ProtocolError::parse(require_field(fields, "error", "ResponseEnvelope")?)?;
                Ok(Self::Failure { id, error })
            }
            _ => Err(ValidationError::new(
                "ResponseEnvelope.ok must be a boolean",
            )),
        }
    }

    fn to_json(&self) -> ProtocolJson {
        match self {
            Self::Success { id, result } => ProtocolJson::Map(vec![
                (
                    "type".to_string(),
                    ProtocolJson::Text("response".to_string()),
                ),
                ("id".to_string(), ProtocolJson::Text(id.clone())),
                ("ok".to_string(), ProtocolJson::Bool(true)),
                ("result".to_string(), result.to_json()),
            ]),
            Self::Failure { id, error } => ProtocolJson::Map(vec![
                (
                    "type".to_string(),
                    ProtocolJson::Text("response".to_string()),
                ),
                ("id".to_string(), ProtocolJson::Text(id.clone())),
                ("ok".to_string(), ProtocolJson::Bool(false)),
                ("error".to_string(), error.to_json()),
            ]),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct EventEnvelope {
    pub event: ServerEvent,
}

impl EventEnvelope {
    fn parse(fields: &[(String, ProtocolJson)]) -> Result<Self, ValidationError> {
        deny_unknown_fields(fields, &["type", "event"], "EventEnvelope")?;
        let event = ServerEvent::parse(require_field(fields, "event", "EventEnvelope")?)?;
        Ok(Self { event })
    }

    fn to_json(&self) -> ProtocolJson {
        ProtocolJson::Map(vec![
            ("type".to_string(), ProtocolJson::Text("event".to_string())),
            ("event".to_string(), self.event.to_json()),
        ])
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ServerMessage {
    Hello(ServerHello),
    HelloError(ServerHelloError),
    Response(ResponseEnvelope),
    Event(EventEnvelope),
}

impl ServerMessage {
    pub fn parse(value: &ProtocolJson) -> Result<Self, ValidationError> {
        let fields = require_object(value, "ServerMessage")?;
        match require_field(fields, "type", "ServerMessage")? {
            ProtocolJson::Text(t) if t == "hello" => Ok(Self::Hello(ServerHello::parse(fields)?)),
            ProtocolJson::Text(t) if t == "hello_error" => {
                Ok(Self::HelloError(ServerHelloError::parse(fields)?))
            }
            ProtocolJson::Text(t) if t == "response" => {
                Ok(Self::Response(ResponseEnvelope::parse(fields)?))
            }
            ProtocolJson::Text(t) if t == "event" => Ok(Self::Event(EventEnvelope::parse(fields)?)),
            _ => Err(ValidationError::new(
                "ServerMessage.type must be one of hello|hello_error|response|event",
            )),
        }
    }

    pub fn to_json(&self) -> ProtocolJson {
        match self {
            Self::Hello(hello) => hello.to_json(),
            Self::HelloError(hello_error) => hello_error.to_json(),
            Self::Response(response) => response.to_json(),
            Self::Event(event) => event.to_json(),
        }
    }
}
