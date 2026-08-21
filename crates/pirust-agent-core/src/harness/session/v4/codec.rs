//! v4 JSONL codec — port of `packages/agent/src/harness/session/jsonl/codec.ts`.
//!
//! The byte contract of the 0.84.2 session file: a v4 header line
//! (`{kind:"header",version:4,id,createdAt,cwd,parentSessionId?,metadata?}`)
//! followed by one compact `JSON.stringify(mutation)` per line, mutations of
//! kind `entry` / `record` / `lane` / `fact`. Every line is strict-validated the
//! way `codec.ts` does (safe integers, required fields, known discriminants,
//! lane/record/fact shapes).

use serde::Serialize;
use serde_json::{Map, Value};

use super::types::{v4_error, Entry, LaneRecord, SessionMutation};
use crate::harness::types::{SessionError, SessionErrorCode};

/// `JsonlDecodeError` (jsonl/errors.ts:4-9) — codec-level parse/schema failure
/// with a `kind` (`syntax` | `schema`).
#[derive(Debug, Clone, PartialEq)]
pub struct JsonlDecodeError {
    pub kind: String,
    pub message: String,
}

/// `JsonlV4Header` (jsonl/types.ts:47-55).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JsonlV4Header {
    pub kind: String,
    pub version: u8,
    pub id: String,
    #[serde(rename = "createdAt")]
    pub created_at: i64,
    pub cwd: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub legacy_parent_session_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Map<String, Value>>,
}

impl Default for JsonlV4Header {
    fn default() -> Self {
        Self {
            kind: "header".to_string(),
            version: 4,
            id: String::new(),
            created_at: 0,
            cwd: String::new(),
            parent_session_id: None,
            legacy_parent_session_path: None,
            metadata: None,
        }
    }
}

/// `isObject` (codec.ts:24-27).
fn is_object(value: &Value) -> bool {
    value.is_object()
}

/// `parseObject` (codec.ts:29-34).
fn parse_object(line: &str) -> Result<Map<String, Value>, JsonlDecodeError> {
    let value: Value = serde_json::from_str(line).map_err(|_| JsonlDecodeError {
        kind: "syntax".to_string(),
        message: "is not valid JSON".to_string(),
    })?;
    match value {
        Value::Object(map) => Ok(map),
        _ => Err(JsonlDecodeError {
            kind: "schema".to_string(),
            message: "is not a JSON object".to_string(),
        }),
    }
}

/// `requireString` (codec.ts:36-39).
fn require_string(value: Option<&Value>, field: &str) -> Result<String, JsonlDecodeError> {
    match value.and_then(Value::as_str) {
        Some(s) => Ok(s.to_string()),
        None => Err(JsonlDecodeError {
            kind: "schema".to_string(),
            message: format!("has invalid {field}"),
        }),
    }
}

/// `requireSequence` (codec.ts:41-46).
fn require_sequence(value: Option<&Value>) -> Result<i64, JsonlDecodeError> {
    match value.and_then(Value::as_i64) {
        Some(seq) if seq > 0 => Ok(seq),
        _ => Err(JsonlDecodeError {
            kind: "schema".to_string(),
            message: "has invalid seq".to_string(),
        }),
    }
}

/// `requireTimestamp` (codec.ts:48-53).
fn require_timestamp(value: Option<&Value>) -> Result<i64, JsonlDecodeError> {
    match value.and_then(Value::as_i64) {
        Some(ts) if ts >= 0 => Ok(ts),
        _ => Err(JsonlDecodeError {
            kind: "schema".to_string(),
            message: "has invalid timestamp".to_string(),
        }),
    }
}

/// `requireNullableId` (codec.ts:55-59).
fn require_nullable_id(
    value: Option<&Value>,
    field: &str,
) -> Result<Option<String>, JsonlDecodeError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(JsonlDecodeError {
            kind: "schema".to_string(),
            message: format!("has invalid {field}"),
        }),
    }
}

/// The entry-type allow-set (codec.ts:7-15).
pub const ENTRY_TYPES: [&str; 7] = [
    "message",
    "model_change",
    "thinking_level_change",
    "active_tools_change",
    "compaction",
    "branch_summary",
    "custom",
];

/// The record-type allow-set (codec.ts:16-27).
pub const RECORD_TYPES: [&str; 9] = [
    "operation_started",
    "abort_requested",
    "operation_finished",
    "step_attempt",
    "tool_started",
    "queue_enqueued",
    "queue_cancelled",
    "write_deferred",
    "usage",
];

/// The operation-kind allow-set (codec.ts:28-31).
pub const OPERATION_KINDS: [&str; 3] = ["run", "compaction", "navigation"];

/// `decodeHeader` (codec.ts:80-109).
fn decode_header(line: &str) -> Result<JsonlV4Header, JsonlDecodeError> {
    let value = parse_object(line)?;
    if value.get("kind").and_then(Value::as_str) != Some("header") {
        return Err(JsonlDecodeError {
            kind: "schema".to_string(),
            message: "is not a header".to_string(),
        });
    }
    if value.get("version").and_then(Value::as_u64) != Some(4) {
        return Err(JsonlDecodeError {
            kind: "schema".to_string(),
            message: "has unsupported session version".to_string(),
        });
    }
    if let Some(p) = value.get("parentSessionId") {
        if !p.is_string() {
            return Err(JsonlDecodeError {
                kind: "schema".to_string(),
                message: "has invalid parentSessionId".to_string(),
            });
        }
    }
    if let Some(p) = value.get("legacyParentSessionPath") {
        if !p.is_string() {
            return Err(JsonlDecodeError {
                kind: "schema".to_string(),
                message: "has invalid legacyParentSessionPath".to_string(),
            });
        }
    }
    if value.contains_key("parentSessionId") && value.contains_key("legacyParentSessionPath") {
        return Err(JsonlDecodeError {
            kind: "schema".to_string(),
            message: "has both parentSessionId and legacyParentSessionPath".to_string(),
        });
    }
    if let Some(m) = value.get("metadata") {
        if !is_object(m) {
            return Err(JsonlDecodeError {
                kind: "schema".to_string(),
                message: "has invalid metadata".to_string(),
            });
        }
    }
    Ok(JsonlV4Header {
        kind: "header".to_string(),
        version: 4,
        id: require_string(value.get("id"), "id")?,
        created_at: require_timestamp(value.get("createdAt"))?,
        cwd: require_string(value.get("cwd"), "cwd")?,
        parent_session_id: value
            .get("parentSessionId")
            .and_then(Value::as_str)
            .map(str::to_string),
        legacy_parent_session_path: value
            .get("legacyParentSessionPath")
            .and_then(Value::as_str)
            .map(str::to_string),
        metadata: value
            .get("metadata")
            .cloned()
            .and_then(|m| m.as_object().cloned()),
    })
}

/// `parseHeader` (codec.ts:111-119).
pub fn parse_header(line: &str) -> Result<JsonlV4Header, JsonlDecodeError> {
    decode_header(line)
}

/// `encodeHeader` (codec.ts:121-123).
pub fn encode_header(header: &JsonlV4Header) -> String {
    format!(
        "{}\n",
        serde_json::to_string(header).expect("header serializes")
    )
}

/// `metadataFromHeader` (codec.ts:125-140).
pub fn metadata_from_header(
    header: &JsonlV4Header,
    path: String,
    modified_at: i64,
) -> crate::harness::session::v4::types::JsonlSessionMetadata {
    use super::types::JsonlSessionMetadata;
    JsonlSessionMetadata {
        id: header.id.clone(),
        created_at: header.created_at,
        cwd: header.cwd.clone(),
        path,
        modified_at,
        source_format: 4,
        parent_session_id: header.parent_session_id.clone(),
        legacy_parent_session_path: header.legacy_parent_session_path.clone(),
        metadata: header.metadata.clone(),
    }
}

/// `parseEntryMutation` (codec.ts:142-159).
fn parse_entry_mutation(
    value: &Map<String, Value>,
    seq: i64,
) -> Result<SessionMutation, JsonlDecodeError> {
    let lane = match value.get("lane") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(_) => {
            return Err(JsonlDecodeError {
                kind: "schema".to_string(),
                message: "has invalid lane".to_string(),
            })
        }
    };
    let _ = require_string(value.get("id"), "id")?;
    let entry_type = require_string(value.get("type"), "entry type")?;
    if !ENTRY_TYPES.contains(&entry_type.as_str()) {
        return Err(JsonlDecodeError {
            kind: "schema".to_string(),
            message: format!("has unknown entry type {entry_type}"),
        });
    }
    let _ = require_nullable_id(value.get("parentId"), "parentId")?;
    let _ = require_timestamp(value.get("timestamp"))?;
    if entry_type == "custom" {
        let _ = require_string(value.get("customType"), "customType")?;
    }
    // Rebuild the entry object with `seq` injected, exactly as codec.ts does
    // (`{ ...entryFields, id, type, parentId, seq, timestamp }`).
    let mut entry_value: Map<String, Value> = value.clone();
    entry_value.remove("kind");
    entry_value.remove("lane");
    entry_value.insert("seq".to_string(), Value::from(seq));
    let entry: Entry =
        serde_json::from_value(Value::Object(entry_value)).map_err(|e| JsonlDecodeError {
            kind: "schema".to_string(),
            message: format!("is not a valid entry: {e}"),
        })?;
    Ok(SessionMutation::Entry { lane, entry })
}

/// `parseRecordMutation` (codec.ts:161-183).
fn parse_record_mutation(
    value: &Map<String, Value>,
    seq: i64,
) -> Result<SessionMutation, JsonlDecodeError> {
    let _ = require_string(value.get("id"), "id")?;
    let _ = require_string(value.get("lane"), "lane")?;
    let record_type = require_string(value.get("type"), "record type")?;
    if !RECORD_TYPES.contains(&record_type.as_str()) {
        return Err(JsonlDecodeError {
            kind: "schema".to_string(),
            message: format!("has unknown record type {record_type}"),
        });
    }
    let _ = require_timestamp(value.get("timestamp"))?;
    if record_type == "operation_started" {
        let intent = value.get("intent").ok_or_else(|| JsonlDecodeError {
            kind: "schema".to_string(),
            message: "has invalid intent".to_string(),
        })?;
        if !is_object(intent) {
            return Err(JsonlDecodeError {
                kind: "schema".to_string(),
                message: "has invalid intent".to_string(),
            });
        }
        let operation_kind = require_string(intent.get("kind"), "operation kind")?;
        if !OPERATION_KINDS.contains(&operation_kind.as_str()) {
            return Err(JsonlDecodeError {
                kind: "schema".to_string(),
                message: format!("has unknown operation kind {operation_kind}"),
            });
        }
    }
    if record_type == "operation_finished" {
        let _ = require_string(value.get("runId"), "runId")?;
    }
    let mut record_value: Map<String, Value> = value.clone();
    record_value.remove("kind");
    record_value.insert("seq".to_string(), Value::from(seq));
    let record: LaneRecord =
        serde_json::from_value(Value::Object(record_value)).map_err(|e| JsonlDecodeError {
            kind: "schema".to_string(),
            message: format!("is not a valid record: {e}"),
        })?;
    Ok(SessionMutation::Record { record })
}

/// `parseLaneMutation` (codec.ts:185-192).
fn parse_lane_mutation(
    value: &Map<String, Value>,
    seq: i64,
) -> Result<SessionMutation, JsonlDecodeError> {
    Ok(SessionMutation::Lane {
        seq,
        lane: require_string(value.get("lane"), "lane")?,
        leaf_id: require_nullable_id(value.get("leafId"), "leafId")?,
    })
}

/// `parseFactMutation` (codec.ts:194-215).
fn parse_fact_mutation(
    value: &Map<String, Value>,
    seq: i64,
) -> Result<SessionMutation, JsonlDecodeError> {
    match value.get("fact").and_then(Value::as_str) {
        Some("name") => {
            if let Some(n) = value.get("name") {
                if !n.is_string() {
                    return Err(JsonlDecodeError {
                        kind: "schema".to_string(),
                        message: "has invalid name".to_string(),
                    });
                }
            }
            Ok(SessionMutation::FactName {
                seq,
                name: value
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            })
        }
        Some("label") => {
            let target_id = require_string(value.get("targetId"), "targetId")?;
            if let Some(l) = value.get("label") {
                if !l.is_string() {
                    return Err(JsonlDecodeError {
                        kind: "schema".to_string(),
                        message: "has invalid label".to_string(),
                    });
                }
            }
            Ok(SessionMutation::FactLabel {
                seq,
                target_id,
                label: value
                    .get("label")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            })
        }
        _ => Err(JsonlDecodeError {
            kind: "schema".to_string(),
            message: "has unknown fact type".to_string(),
        }),
    }
}

/// `decodeMutation` (codec.ts:217-230).
fn decode_mutation(line: &str) -> Result<SessionMutation, JsonlDecodeError> {
    let value = parse_object(line)?;
    let seq = require_sequence(value.get("seq"))?;
    match value.get("kind").and_then(Value::as_str) {
        Some("entry") => parse_entry_mutation(&value, seq),
        Some("record") => parse_record_mutation(&value, seq),
        Some("lane") => parse_lane_mutation(&value, seq),
        Some("fact") => parse_fact_mutation(&value, seq),
        _ => Err(JsonlDecodeError {
            kind: "schema".to_string(),
            message: "has unknown mutation kind".to_string(),
        }),
    }
}

/// `parseMutation` (codec.ts:232-241).
pub fn parse_mutation(line: &str) -> Result<SessionMutation, JsonlDecodeError> {
    decode_mutation(line)
}

/// `encodeMutation` (codec.ts:243-250).
pub fn encode_mutation(mutation: &SessionMutation) -> String {
    match mutation {
        SessionMutation::Entry { lane, entry } => {
            // Pi: `{kind: "entry", lane, ...entry}` — `kind` and `lane` FIRST, then the
            // entry fields in their declared order. serde's internally-tagged enum emits
            // `type` first, so rebuild a fresh map with `kind` inserted before copying.
            let mut map = Map::new();
            map.insert("kind".to_string(), Value::String("entry".to_string()));
            if let Some(l) = lane {
                map.insert("lane".to_string(), Value::String(l.clone()));
            }
            if let Value::Object(fields) = serde_json::to_value(entry).expect("entry serializes") {
                for (k, v) in fields {
                    map.insert(k, v);
                }
            }
            format!(
                "{}\n",
                serde_json::to_string(&Value::Object(map)).expect("serializes")
            )
        }
        SessionMutation::Record { record } => {
            // Pi: `{kind: "record", ...record}` — `kind` first, then record fields.
            let mut map = Map::new();
            map.insert("kind".to_string(), Value::String("record".to_string()));
            if let Value::Object(fields) = serde_json::to_value(record).expect("record serializes")
            {
                for (k, v) in fields {
                    map.insert(k, v);
                }
            }
            format!(
                "{}\n",
                serde_json::to_string(&Value::Object(map)).expect("serializes")
            )
        }
        SessionMutation::Lane { seq, lane, leaf_id } => {
            let mut map = Map::new();
            map.insert("kind".to_string(), Value::String("lane".to_string()));
            map.insert("seq".to_string(), Value::from(*seq));
            map.insert("lane".to_string(), Value::String(lane.clone()));
            map.insert(
                "leafId".to_string(),
                leaf_id
                    .as_ref()
                    .map_or(Value::Null, |l| Value::String(l.clone())),
            );
            format!(
                "{}\n",
                serde_json::to_string(&Value::Object(map)).expect("serializes")
            )
        }
        SessionMutation::FactName { seq, name } => {
            let mut map = Map::new();
            map.insert("kind".to_string(), Value::String("fact".to_string()));
            map.insert("seq".to_string(), Value::from(*seq));
            map.insert("fact".to_string(), Value::String("name".to_string()));
            // JS `JSON.stringify` drops `undefined` — a cleared name emits NO `name` key.
            if let Some(n) = name {
                map.insert("name".to_string(), Value::String(n.clone()));
            }
            format!(
                "{}\n",
                serde_json::to_string(&Value::Object(map)).expect("serializes")
            )
        }
        SessionMutation::FactLabel {
            seq,
            target_id,
            label,
        } => {
            let mut map = Map::new();
            map.insert("kind".to_string(), Value::String("fact".to_string()));
            map.insert("seq".to_string(), Value::from(*seq));
            map.insert("fact".to_string(), Value::String("label".to_string()));
            map.insert("targetId".to_string(), Value::String(target_id.clone()));
            // JS `JSON.stringify` drops `undefined` — a cleared label emits NO `label` key.
            if let Some(l) = label {
                map.insert("label".to_string(), Value::String(l.clone()));
            }
            format!(
                "{}\n",
                serde_json::to_string(&Value::Object(map)).expect("serializes")
            )
        }
    }
}

// ===========================================================================
// JSONL decode-error → SessionError bridge (jsonl/errors.ts)
// ===========================================================================

/// `invalidFile` (jsonl/errors.ts:25-29) — a decode failure inside a session
/// file becomes an `invalid_entry` `SessionError` naming the file and line.
/// Pi's exact string: `Invalid JSONL v4 session {path}: line {line} {message}`
/// (the cause.message — no extra suffix for syntax errors).
pub fn invalid_file(path: &str, line_number: usize, error: &JsonlDecodeError) -> SessionError {
    v4_error(
        SessionErrorCode::InvalidEntry,
        format!(
            "Invalid JSONL v4 session {path}: line {line_number} {}",
            error.message
        ),
    )
}

/// `fileResult` (jsonl/errors.ts:14-18) — unwrap a filesystem `Result` with the
/// given error message.
pub fn file_result<T>(result: Result<T, String>, message: &str) -> Result<T, SessionError> {
    result.map_err(|_| v4_error(SessionErrorCode::Storage, message))
}

/// A minimal `FileSystem` subset the v4 storage needs (jsonl/types.ts
/// `JsonlSessionRepoFileSystem`).
pub trait V4FileSystem: Send + Sync {
    fn absolute_path(&self, path: &str) -> Result<String, String>;
    fn join_path(&self, parts: &[String]) -> Result<String, String>;
    fn read_text_file(&self, path: &str) -> Result<String, String>;
    fn read_text_lines(&self, path: &str, max_lines: Option<usize>) -> Result<Vec<String>, String>;
    fn write_file(&self, path: &str, contents: &str) -> Result<(), String>;
    fn append_file(&self, path: &str, contents: &str) -> Result<(), String>;
    fn rename_file(&self, from: &str, to: &str) -> Result<(), String>;
    fn file_info(&self, path: &str) -> Result<FileInfo, String>;
    fn list_dir(&self, path: &str) -> Result<Vec<DirEntry>, String>;
    fn exists(&self, path: &str) -> Result<bool, String>;
    fn create_dir(&self, path: &str, recursive: bool) -> Result<(), String>;
    fn remove(&self, path: &str, force: bool) -> Result<(), String>;
}

#[derive(Debug, Clone)]
pub struct FileInfo {
    pub mtime_ms: i64,
}

#[derive(Debug, Clone)]
pub struct DirEntry {
    pub kind: String, // "file" | "directory" | "symlink"
    pub name: String,
    pub path: String,
}

// ===========================================================================
// Unit tests — replay semantics vs Pi's codec
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_round_trips() {
        let header = JsonlV4Header {
            id: "session-1".to_string(),
            created_at: 1_700_000_000_000,
            cwd: "/oracle/cwd".to_string(),
            parent_session_id: Some("parent-1".to_string()),
            ..Default::default()
        };
        let line = encode_header(&header);
        assert!(line.starts_with(
            r#"{"kind":"header","version":4,"id":"session-1","createdAt":1700000000000,"cwd":"/oracle/cwd","parentSessionId":"parent-1"}"#
        ));
        let parsed = parse_header(line.trim_end()).expect("parses");
        assert_eq!(parsed, header);
    }

    #[test]
    fn header_rejects_wrong_version_and_missing_fields() {
        assert_eq!(
            parse_header(r#"{"kind":"header","version":3}"#)
                .unwrap_err()
                .kind,
            "schema"
        );
        assert_eq!(
            parse_header(r#"{"kind":"notheader","version":4,"id":"x","createdAt":1,"cwd":"/"}"#)
                .unwrap_err()
                .message,
            "is not a header"
        );
        assert_eq!(
            parse_header(r#"{"kind":"header","version":4,"id":"x","createdAt":-1,"cwd":"/"}"#)
                .unwrap_err()
                .message,
            "has invalid timestamp"
        );
    }

    #[test]
    fn lane_mutation_round_trips() {
        let m = SessionMutation::Lane {
            seq: 1,
            lane: "main".to_string(),
            leaf_id: Some("abc123".to_string()),
        };
        let line = encode_mutation(&m);
        assert_eq!(
            line.trim_end(),
            r#"{"kind":"lane","seq":1,"lane":"main","leafId":"abc123"}"#
        );
        assert_eq!(parse_mutation(line.trim_end()).expect("parses"), m);
    }

    #[test]
    fn fact_name_round_trips() {
        let m = SessionMutation::FactName {
            seq: 2,
            name: Some("My session".to_string()),
        };
        let line = encode_mutation(&m);
        assert_eq!(
            line.trim_end(),
            r#"{"kind":"fact","seq":2,"fact":"name","name":"My session"}"#
        );
        assert_eq!(parse_mutation(line.trim_end()).expect("parses"), m);
    }

    #[test]
    fn mutation_seq_must_be_positive() {
        assert_eq!(
            parse_mutation(r#"{"kind":"lane","seq":0,"lane":"main","leafId":null}"#)
                .unwrap_err()
                .message,
            "has invalid seq"
        );
    }
}
