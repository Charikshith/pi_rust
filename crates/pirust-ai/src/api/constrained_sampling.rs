//! Rust port of Pi's `packages/ai/src/api/constrained-sampling.ts` — the
//! provider-side constrained-sampling helpers used by the `openai-completions`
//! adapter (`convertTools`) and the streaming tool-call grammar buffers.
//!
//! Semantics preserved 1:1 from the TS:
//! - `make_strict_json_schema` deep-clones the tool schema (TS `structuredClone`)
//!   and mutates the clone into the strict subset providers enforce.
//! - Unsupported constructs raise [`UnsupportedStrictJsonSchemaError`]; the
//!   strict-resolution caller turns a "prefer" config into a soft fallback and
//!   a "require" config into a hard error carrying Pi's exact wording.
//! - Grammar buffers track monotonic input appends (`append_grammar_tool_input_json_delta`).

use crate::types::message::{ConstrainedSamplingConfig, Tool};
use serde_json::{Map, Value};

/// Error raised when a JSON schema uses constructs outside the strict subset
/// (TS `UnsupportedStrictJsonSchemaError`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsupportedStrictJsonSchemaError(pub String);

impl std::fmt::Display for UnsupportedStrictJsonSchemaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Keys that cannot appear in a strict JSON schema (TS `UNSUPPORTED_STRICT_SCHEMA_KEYS`).
const UNSUPPORTED_STRICT_SCHEMA_KEYS: &[&str] = &[
    "$ref",
    "$defs",
    "definitions",
    "allOf",
    "oneOf",
    "patternProperties",
    "dependentSchemas",
    "dependencies",
    "unevaluatedProperties",
    "propertyNames",
    "contains",
    "prefixItems",
    "not",
    "if",
    "then",
    "else",
];

fn is_json_schema_object(value: &Value) -> bool {
    value.is_object()
}

fn schema_type(value: &Value) -> Vec<&str> {
    match value.get("type") {
        Some(Value::String(s)) => vec![s.as_str()],
        Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).collect(),
        _ => vec![],
    }
}

fn is_structured_schema(schema: &Value) -> bool {
    if !is_json_schema_object(schema) {
        return false;
    }
    let types = schema_type(schema);
    types.contains(&"object")
        || types.contains(&"array")
        || schema.get("properties").is_some()
        || schema.get("items").is_some()
}

fn schema_allows_null(schema: &Value) -> bool {
    if !is_json_schema_object(schema) {
        return false;
    }
    let types = schema_type(schema);
    if types.contains(&"null") {
        return true;
    }
    if schema.get("const") == Some(&Value::Null) {
        return true;
    }
    if let Some(Value::Array(enum_values)) = schema.get("enum") {
        if enum_values.iter().any(|v| v.is_null()) {
            return true;
        }
    }
    if let Some(Value::Array(any_of)) = schema.get("anyOf") {
        if any_of.iter().any(schema_allows_null) {
            return true;
        }
    }
    false
}

fn unsupported(message: &str) -> UnsupportedStrictJsonSchemaError {
    UnsupportedStrictJsonSchemaError(message.to_string())
}

/// Mutate `schema` (a cloned JSON schema) into the strict subset (TS
/// `makeJsonSchemaNodeStrict`).
fn make_json_schema_node_strict(
    schema: &mut Value,
) -> Result<(), UnsupportedStrictJsonSchemaError> {
    let obj = match schema {
        Value::Object(o) => o,
        _ => return Err(unsupported("boolean schemas are unsupported")),
    };

    for key in UNSUPPORTED_STRICT_SCHEMA_KEYS {
        if obj.get(*key).is_some() {
            return Err(unsupported(&format!("{key} schemas are unsupported")));
        }
    }

    if let Some(Value::Array(variants)) = obj.get_mut("anyOf") {
        if variants.is_empty() {
            return Err(unsupported("anyOf must contain at least one schema"));
        }
        for variant in variants.iter_mut() {
            if is_structured_schema(variant) {
                return Err(unsupported("object and array unions are unsupported"));
            }
            make_json_schema_node_strict(variant)?;
        }
    }

    if let Some(items) = obj.get_mut("items") {
        if items.is_array() {
            return Err(unsupported("tuple schemas are unsupported"));
        }
        make_json_schema_node_strict(items)?;
    }

    let is_object_schema = obj.get("type") == Some(&Value::String("object".into()));
    if obj.get("properties").is_some() && !is_object_schema {
        return Err(unsupported("properties require type object"));
    }
    if !is_object_schema {
        return Ok(());
    }

    if let Some(additional_properties) = obj.get("additionalProperties") {
        if additional_properties != &Value::Bool(false) {
            return Err(unsupported(
                "schema-valued or true additionalProperties is unsupported",
            ));
        }
    }
    if let Some(properties) = obj.get("properties") {
        if !properties.is_object() {
            return Err(unsupported("object properties must be a schema map"));
        }
    }
    if let Some(required) = obj.get("required") {
        let bad = match required {
            Value::Array(a) => a.iter().any(|v| !v.is_string()),
            _ => true,
        };
        if bad {
            return Err(unsupported("object required must be a string array"));
        }
    }

    // Snapshot the required set (validating shape first), then take the
    // properties map OUT of the schema so the recursion can mutate it without
    // fighting the outer borrow.
    let property_names: Vec<String> = match obj.get("properties") {
        Some(Value::Object(p)) => p.keys().cloned().collect(),
        _ => vec![],
    };
    let required: Vec<String> = match obj.get("required") {
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect(),
        _ => vec![],
    };
    for key in &required {
        if !property_names.contains(key) {
            return Err(unsupported("required contains an unknown property"));
        }
    }

    let properties_obj: Map<String, Value> = match obj.get_mut("properties") {
        Some(Value::Object(p)) => std::mem::take(p),
        _ => Map::new(),
    };

    let mut next_properties: Map<String, Value> = Map::new();
    for (key, mut property) in properties_obj {
        make_json_schema_node_strict(&mut property)?;
        if !required.contains(&key) && !schema_allows_null(&property) {
            let any_of = Value::Array(vec![
                property,
                Value::Object(Map::from_iter([(
                    "type".to_string(),
                    Value::String("null".into()),
                )])),
            ]);
            next_properties.insert(
                key,
                Value::Object(Map::from_iter([("anyOf".to_string(), any_of)])),
            );
        } else {
            next_properties.insert(key, property);
        }
    }
    obj.insert("properties".to_string(), Value::Object(next_properties));
    obj.insert(
        "required".to_string(),
        Value::Array(property_names.into_iter().map(Value::String).collect()),
    );
    obj.insert("additionalProperties".to_string(), Value::Bool(false));
    Ok(())
}

/// Convert a tool schema to the strict subset expected by provider constrained
/// sampling (TS `makeStrictJsonSchema`). The schema is cloned before mutation.
pub fn make_strict_json_schema(schema: &Value) -> Result<Value, UnsupportedStrictJsonSchemaError> {
    let mut cloned = schema.clone();
    if !cloned.is_object() {
        return Err(unsupported("root schema must have type object"));
    }
    make_json_schema_node_strict(&mut cloned)?;
    if cloned.get("type") != Some(&Value::String("object".into())) {
        return Err(unsupported("root schema must have type object"));
    }
    Ok(cloned)
}

/// `getJsonSchemaToolParameters`: pass the schema through untouched unless
/// strict sampling is requested, in which case enforce the strict subset.
pub fn get_json_schema_tool_parameters(
    tool: &Tool,
    strict: Option<bool>,
) -> Result<Value, UnsupportedStrictJsonSchemaError> {
    if strict == Some(true) {
        make_strict_json_schema(&tool.parameters)
    } else {
        Ok(tool.parameters.clone())
    }
}

/// Resolved grammar constrained-sampling config (TS `GrammarConstrainedSampling`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrammarConstrainedSampling {
    pub format: GrammarFormatChoice,
    pub definition: String,
    pub input_property: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrammarFormatChoice {
    Lark,
    Regex,
}

/// Monotonic grammar tool-input buffer (TS `GrammarToolInputJsonBuffer`).
#[derive(Debug, Clone, Default)]
pub struct GrammarToolInputJsonBuffer {
    pub input: String,
    pub started: bool,
    pub closed: bool,
}

/// `getGrammarToolInput`: the grammar tool call must carry a string input for
/// the property inferred from the tool's schema.
pub fn get_grammar_tool_input(
    tool_name: &str,
    arguments: &Map<String, Value>,
    input_property: &str,
) -> Result<String, String> {
    match arguments.get(input_property) {
        Some(Value::String(s)) => Ok(s.clone()),
        _ => Err(format!(
            "Grammar tool call \"{tool_name}\" requires argument \"{input_property}\" to be a string."
        )),
    }
}

/// `appendGrammarToolInputJsonDelta`: append the next monotonic input slice,
/// producing the raw JSON delta (with property key on first append, closing
/// `"}` on close). Returns `None` when there is nothing to emit.
pub fn append_grammar_tool_input_json_delta(
    buffer: &mut GrammarToolInputJsonBuffer,
    input_property: &str,
    next_input: &str,
    close: bool,
) -> Result<Option<String>, String> {
    if buffer.closed {
        if close && next_input == buffer.input {
            return Ok(None);
        }
        return Err(format!(
            "grammar tool input for property \"{input_property}\" changed after it was closed"
        ));
    }
    if !next_input.starts_with(&buffer.input) {
        return Err(format!(
            "grammar tool input for property \"{input_property}\" changed non-monotonically"
        ));
    }

    let input_delta = &next_input[buffer.input.len()..];
    if !close && input_delta.is_empty() {
        return Ok(None);
    }

    let mut delta = String::new();
    if !buffer.started {
        delta.push_str(&format!("{{\"{}\":\"", input_property));
        buffer.started = true;
    }
    delta.push_str(&json_string_slice(input_delta));
    buffer.input = next_input.to_string();

    if close {
        delta.push_str("\"}");
        buffer.closed = true;
    }
    Ok(Some(delta))
}

/// `JSON.stringify(delta).slice(1,-1)` — the JSON-escaped string body without
/// the surrounding quotes.
fn json_string_slice(s: &str) -> String {
    let json = serde_json::to_string(s).unwrap_or_default();
    json[1..json.len().saturating_sub(1)].to_string()
}

/// Infer the single required string property a grammar tool samples (TS
/// `inferGrammarInputProperty`).
fn infer_grammar_input_property(tool: &Tool) -> Result<String, String> {
    let schema = match &tool.parameters {
        Value::Object(o) => o,
        _ => {
            return Err(
                "grammar constrained sampling requires an object parameter schema".to_string(),
            )
        }
    };
    if schema.get("type") != Some(&Value::String("object".into())) {
        return Err("grammar constrained sampling requires an object parameter schema".to_string());
    }
    let required: Vec<&str> = match schema.get("required") {
        Some(Value::Array(a)) => a.iter().filter_map(|v| v.as_str()).collect(),
        _ => vec![],
    };
    if required.len() != 1 {
        return Err(
            "grammar constrained sampling requires exactly one required string property"
                .to_string(),
        );
    }
    let input_property = required[0];
    let properties = match schema.get("properties") {
        Some(Value::Object(p)) => p,
        _ => {
            return Err(format!(
                "grammar constrained sampling requires a properties entry for {input_property}"
            ))
        }
    };
    let prop = properties.get(input_property).ok_or_else(|| {
        format!("grammar constrained sampling requires a properties entry for {input_property}")
    })?;
    if prop.get("type") != Some(&Value::String("string".into())) {
        return Err(format!(
            "grammar constrained sampling property {input_property} must have type string"
        ));
    }
    Ok(input_property.to_string())
}

/// `resolveJsonSchemaStrictSampling` — decide whether to enforce strict JSON
/// schema sampling for a tool, following Pi's prefer/require semantics and
/// exact error wording.
pub fn resolve_json_schema_strict_sampling(
    tool: &Tool,
    supports_strict_mode: bool,
) -> Result<Option<bool>, String> {
    let Some(config) = &tool.constrained_sampling else {
        return Ok(None);
    };
    let ConstrainedSamplingConfig::JsonSchema { strict } = config else {
        return Ok(None);
    };

    if supports_strict_mode {
        match make_strict_json_schema(&tool.parameters) {
            Ok(_) => Ok(Some(true)),
            Err(err) => {
                if *strict != crate::types::message::ConstrainedSamplingStrictness::Require {
                    return Ok(None);
                }
                Err(format!(
                    "Tool \"{}\" requires JSON-schema constrained sampling, but {}.",
                    tool.name, err
                ))
            }
        }
    } else if *strict == crate::types::message::ConstrainedSamplingStrictness::Require {
        Err(format!(
            "Tool \"{}\" requires JSON-schema constrained sampling, but strict tools are unsupported.",
            tool.name
        ))
    } else {
        Ok(None)
    }
}

/// `resolveGrammarConstrainedSampling` — pick the supported grammar variant
/// (lark preferred) and infer the input property, or error with Pi's wording.
pub fn resolve_grammar_constrained_sampling(
    tool: &Tool,
    supports_openai_grammar_tools: bool,
) -> Result<Option<GrammarConstrainedSampling>, String> {
    let Some(config) = &tool.constrained_sampling else {
        return Ok(None);
    };
    let ConstrainedSamplingConfig::Grammar { variants } = config else {
        return Ok(None);
    };
    if !supports_openai_grammar_tools {
        return Ok(None);
    }

    let lark_definition = variants
        .openai_lark
        .as_deref()
        .filter(|s| !s.trim().is_empty());
    let regex_definition = variants
        .openai_regex
        .as_deref()
        .filter(|s| !s.trim().is_empty());
    if lark_definition.is_none() && regex_definition.is_none() {
        return Err(format!(
            "Tool \"{}\" cannot use grammar constrained sampling: no supported grammar variant was provided.",
            tool.name
        ));
    }

    let result: Result<GrammarConstrainedSampling, String> =
        match infer_grammar_input_property(tool) {
            Ok(input_property) => Ok(GrammarConstrainedSampling {
                format: if lark_definition.is_some() {
                    GrammarFormatChoice::Lark
                } else {
                    GrammarFormatChoice::Regex
                },
                definition: lark_definition
                    .unwrap_or_else(|| regex_definition.unwrap())
                    .to_string(),
                input_property,
            }),
            Err(message) => Err(format!(
                "Tool \"{}\" cannot use grammar constrained sampling: {}.",
                tool.name, message
            )),
        };
    result.map(Some)
}

/// `createGrammarToolInputProperties` — map tool name → inferred grammar input
/// property for the tools that resolve a grammar config.
pub fn create_grammar_tool_input_properties(
    tools: Option<&[Tool]>,
    supports_openai_grammar_tools: bool,
) -> std::collections::HashMap<String, String> {
    let mut properties = std::collections::HashMap::new();
    for tool in tools.into_iter().flatten() {
        if let Ok(Some(grammar)) =
            resolve_grammar_constrained_sampling(tool, supports_openai_grammar_tools)
        {
            properties.insert(tool.name.clone(), grammar.input_property);
        }
    }
    properties
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tool(name: &str, params: Value, sampling: Option<ConstrainedSamplingConfig>) -> Tool {
        Tool {
            name: name.to_string(),
            description: String::new(),
            parameters: params,
            constrained_sampling: sampling,
        }
    }

    #[test]
    fn strict_schema_matches_real_pi_oracle() {
        // Ground-truth strings captured by running real Pi's
        // makeStrictJsonSchema (packages/ai/src/api/constrained-sampling.ts)
        // on the same inputs.
        let cases: Vec<(Value, &str)> = vec![
            (
                json!({"type":"object","properties":{"name":{"type":"string"}}}),
                "{\"type\":\"object\",\"properties\":{\"name\":{\"anyOf\":[{\"type\":\"string\"},{\"type\":\"null\"}]}},\"required\":[\"name\"],\"additionalProperties\":false}",
            ),
            (
                json!({"type":"object","required":["name"],"properties":{"name":{"type":"string"}}}),
                "{\"type\":\"object\",\"required\":[\"name\"],\"properties\":{\"name\":{\"type\":\"string\"}},\"additionalProperties\":false}",
            ),
            (
                json!({"type":"object","properties":{"v":{"type":"string","enum":["a",null]}}}),
                "{\"type\":\"object\",\"properties\":{\"v\":{\"type\":\"string\",\"enum\":[\"a\",null]}},\"required\":[\"v\"],\"additionalProperties\":false}",
            ),
            (
                json!({"type":"object","properties":{"items":{"type":"array","items":{"type":"string"}}}}),
                "{\"type\":\"object\",\"properties\":{\"items\":{\"anyOf\":[{\"type\":\"array\",\"items\":{\"type\":\"string\"}},{\"type\":\"null\"}]}},\"required\":[\"items\"],\"additionalProperties\":false}",
            ),
        ];
        for (input, expected) in cases {
            let actual = serde_json::to_string(&make_strict_json_schema(&input).unwrap()).unwrap();
            assert_eq!(actual, expected, "input: {input}");
        }
    }

    #[test]
    fn strict_schema_error_wordings_match_oracle() {
        let cases: Vec<(Value, &str)> = vec![
            (
                json!({"type":"object","properties":{"x":{"$ref":"#/definitions/X"}}}),
                "$ref schemas are unsupported",
            ),
            (
                json!({"type":"object","properties":{"x":{"type":"array","items":[{"type":"string"}]}}}),
                "tuple schemas are unsupported",
            ),
            (
                json!({"type":"object","additionalProperties":true,"properties":{"x":{"type":"string"}}}),
                "schema-valued or true additionalProperties is unsupported",
            ),
            (
                json!({"type":"object","required":["missing"],"properties":{"x":{"type":"string"}}}),
                "required contains an unknown property",
            ),
        ];
        for (input, expected) in cases {
            let err = make_strict_json_schema(&input).unwrap_err();
            assert_eq!(err.0, expected, "input: {input}");
        }
    }

    #[test]
    fn strict_schema_accepts_plain_object() {
        let schema = json!({
            "type": "object",
            "properties": { "name": { "type": "string" } }
        });
        let strict = make_strict_json_schema(&schema).unwrap();
        assert_eq!(strict["required"], json!(["name"]));
        assert_eq!(strict["additionalProperties"], json!(false));
        // non-required property wrapped in anyOf [prop, {type:"null"}]
        assert_eq!(strict["properties"]["name"]["anyOf"][0]["type"], "string");
        assert_eq!(strict["properties"]["name"]["anyOf"][1]["type"], "null");
        // original untouched (structuredClone semantics)
        assert!(schema.get("required").is_none());
    }

    #[test]
    fn strict_schema_keeps_required_unwrapped() {
        let schema = json!({
            "type": "object",
            "required": ["name"],
            "properties": { "name": { "type": "string" } }
        });
        let strict = make_strict_json_schema(&schema).unwrap();
        // Required properties are NOT null-wrapped — kept as-is.
        assert_eq!(strict["properties"]["name"], json!({"type":"string"}));
        assert_eq!(strict["required"], json!(["name"]));
        assert_eq!(strict["additionalProperties"], json!(false));
    }

    #[test]
    fn strict_schema_rejects_unsupported_keys() {
        let schema = json!({
            "type": "object",
            "properties": { "x": { "$ref": "#/definitions/X" } }
        });
        let err = make_strict_json_schema(&schema).unwrap_err();
        assert_eq!(err.0, "$ref schemas are unsupported");
    }

    #[test]
    fn strict_schema_rejects_tuple_items() {
        let schema = json!({
            "type": "object",
            "properties": { "x": { "type": "array", "items": [{ "type": "string" }] } }
        });
        let err = make_strict_json_schema(&schema).unwrap_err();
        assert_eq!(err.0, "tuple schemas are unsupported");
    }

    #[test]
    fn resolve_strict_prefer_falls_back_softly() {
        let t = tool(
            "search",
            json!({ "type": "object", "properties": { "q": { "$ref": "#" } } }),
            Some(ConstrainedSamplingConfig::JsonSchema {
                strict: crate::types::message::ConstrainedSamplingStrictness::Prefer,
            }),
        );
        assert_eq!(resolve_json_schema_strict_sampling(&t, true).unwrap(), None);
    }

    #[test]
    fn resolve_strict_require_errors_with_wording() {
        let t = tool(
            "search",
            json!({ "type": "object", "properties": { "q": { "$ref": "#" } } }),
            Some(ConstrainedSamplingConfig::JsonSchema {
                strict: crate::types::message::ConstrainedSamplingStrictness::Require,
            }),
        );
        let err = resolve_json_schema_strict_sampling(&t, true).unwrap_err();
        assert!(
            err.contains("Tool \"search\" requires JSON-schema constrained sampling, but $ref schemas are unsupported."),
            "got: {err}"
        );
    }

    #[test]
    fn grammar_resolution_picks_lark_and_infers_property() {
        let t = tool(
            "sql",
            json!({
                "type": "object",
                "required": ["query"],
                "properties": { "query": { "type": "string" } }
            }),
            Some(ConstrainedSamplingConfig::Grammar {
                variants: crate::types::message::GrammarVariants {
                    openai_lark: Some("start: \"SELECT\"".to_string()),
                    openai_regex: None,
                },
            }),
        );
        let g = resolve_grammar_constrained_sampling(&t, true)
            .unwrap()
            .unwrap();
        assert_eq!(g.format, GrammarFormatChoice::Lark);
        assert_eq!(g.definition, "start: \"SELECT\"");
        assert_eq!(g.input_property, "query");
    }

    #[test]
    fn grammar_no_variant_errors() {
        let t = tool(
            "sql",
            json!({
                "type": "object",
                "required": ["query"],
                "properties": { "query": { "type": "string" } }
            }),
            Some(ConstrainedSamplingConfig::Grammar {
                variants: crate::types::message::GrammarVariants {
                    openai_lark: None,
                    openai_regex: None,
                },
            }),
        );
        let err = resolve_grammar_constrained_sampling(&t, true).unwrap_err();
        assert!(
            err.contains("no supported grammar variant was provided"),
            "got: {err}"
        );
    }

    #[test]
    fn grammar_delta_append_is_monotonic() {
        let mut buffer = GrammarToolInputJsonBuffer::default();
        let d1 = append_grammar_tool_input_json_delta(&mut buffer, "query", "SEL", false)
            .unwrap()
            .unwrap();
        assert_eq!(d1, "{\"query\":\"SEL");
        let d2 = append_grammar_tool_input_json_delta(&mut buffer, "query", "SELECT", false)
            .unwrap()
            .unwrap();
        assert_eq!(d2, "ECT");
        let d3 = append_grammar_tool_input_json_delta(&mut buffer, "query", "SELECT", true)
            .unwrap()
            .unwrap();
        assert_eq!(d3, "\"}");
        // non-monotonic
        let err =
            append_grammar_tool_input_json_delta(&mut buffer, "query", "SELX", true).unwrap_err();
        assert!(err.contains("changed after it was closed"), "got: {err}");
    }

    #[test]
    fn grammar_delta_rejects_non_monotonic() {
        let mut buffer = GrammarToolInputJsonBuffer {
            input: "AB".to_string(),
            ..Default::default()
        };
        let err = append_grammar_tool_input_json_delta(&mut buffer, "q", "A", false).unwrap_err();
        assert!(err.contains("changed non-monotonically"), "got: {err}");
    }

    #[test]
    fn create_grammar_properties_map() {
        let tools = vec![
            tool(
                "sql",
                json!({
                    "type": "object",
                    "required": ["query"],
                    "properties": { "query": { "type": "string" } }
                }),
                Some(ConstrainedSamplingConfig::Grammar {
                    variants: crate::types::message::GrammarVariants {
                        openai_lark: Some("x".into()),
                        openai_regex: None,
                    },
                }),
            ),
            tool("plain", json!({"type": "object"}), None),
        ];
        let props = create_grammar_tool_input_properties(Some(&tools), true);
        assert_eq!(props.get("sql").map(String::as_str), Some("query"));
        assert!(!props.contains_key("plain"));
    }
}
