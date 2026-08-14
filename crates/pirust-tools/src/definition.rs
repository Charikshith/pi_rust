//! Port of `core/tools/tool-definition-wrapper.ts` — the UI-free half of Pi's
//! `ToolDefinition`, plus its `AgentTool` bridge.
//!
//! Pi's `ToolDefinition` (extensions/types.ts:439-486) is what every built-in
//! tool file returns from `create<X>ToolDefinition`; `create<X>Tool` then calls
//! `wrapToolDefinition` (tool-definition-wrapper.ts:5-19) to hand the core
//! runtime an `AgentTool`. That wrapper forwards **exactly seven** fields —
//! `name`, `label`, `description`, `parameters`, `prepareArguments`,
//! `executionMode`, `execute` (tool-definition-wrapper.ts:10-17) — so here the
//! `impl AgentTool for PirustToolDefinition` *is* `wrapToolDefinition`: no
//! separate wrapper type exists and every tool gets the bridge for free.
//!
//! Field-by-field port status:
//!
//! | TS field (extensions/types.ts) | here |
//! | --- | --- |
//! | `name` (:441) | [`PirustToolDefinition::name`] |
//! | `label` (:443) | [`PirustToolDefinition::label`] |
//! | `description` (:445) | [`PirustToolDefinition::description`] |
//! | `promptSnippet?` (:447) | [`PirustToolDefinition::prompt_snippet`] — prompt builder only (feat-005), not on `AgentTool` |
//! | `promptGuidelines?` (:449) | [`PirustToolDefinition::prompt_guidelines`] — same |
//! | `parameters` (:451) | [`PirustToolDefinition::parameters`] (see [`schema`]) |
//! | `renderShell?` (:453) | **omitted** — TUI only (feat-006/007) |
//! | `prepareArguments?` (:456) | [`PirustToolDefinition::prepare_arguments`] |
//! | `executionMode?` (:465) | [`PirustToolDefinition::execution_mode`] |
//! | `execute` (:468-474) | [`PirustToolDefinition::execute`] |
//! | `renderCall?` (:477) | **omitted** — TUI only |
//! | `renderResult?` (:480-485) | **omitted** — TUI only |
//!
//! Two deliberate deviations from the TS shape, both forced by Rust:
//!
//! * `wrapToolDefinition`'s `ctxFactory` (tool-definition-wrapper.ts:7,17)
//!   supplies the sixth `execute` argument, `ctx: ExtensionContext`. There is no
//!   `ExtensionContext` port yet and a Rust closure can simply *capture* the
//!   state it needs (cwd, an `Operations` impl, …), which subsumes the factory —
//!   so [`ToolExecuteFn`] has the four-argument `AgentTool::execute` shape.
//! * `execute` receives `tool_call_id` as an owned `String` rather than `&str`.
//!   Boxing a borrowing future would force a higher-ranked lifetime on every
//!   tool closure; the trait method still takes `&str` and clones once, which is
//!   invisible to callers.
//!
//! `wrapToolDefinitions` (tool-definition-wrapper.ts:22-27) needs no port: it is
//! `defs.into_iter().map(|d| Arc::new(d) as Arc<dyn AgentTool>).collect()`.

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use pirust_agent_core::types::{
    AgentTool, AgentToolResult, AgentToolUpdateCallback, ToolError, ToolExecutionMode,
};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

/// Boxed future returned by a tool's `execute` (the erased form of
/// `Promise<AgentToolResult<TDetails>>`, extensions/types.ts:474).
pub type BoxToolFuture =
    Pin<Box<dyn Future<Output = Result<AgentToolResult, ToolError>> + Send + 'static>>;

/// Erased `execute` body (extensions/types.ts:468-474).
///
/// Argument order mirrors [`AgentTool::execute`]: `tool_call_id`, `args`,
/// cancellation `token` (Pi's `AbortSignal`), `on_update`. Build one with
/// [`tool_execute`], or let [`PirustToolDefinition::new`] do it for you.
pub type ToolExecuteFn = Arc<
    dyn Fn(String, Value, CancellationToken, AgentToolUpdateCallback) -> BoxToolFuture
        + Send
        + Sync,
>;

/// Erased `prepareArguments` shim (extensions/types.ts:456).
///
/// Must return a value conforming to the tool's `parameters` schema. Only `edit`
/// has one; when absent the bridge is the identity function and the loop treats
/// the unchanged return as "no shim ran".
pub type PrepareArgumentsFn = Arc<dyn Fn(Value) -> Value + Send + Sync>;

/// Erase an `async fn` (or async closure) into a [`ToolExecuteFn`].
///
/// Only needed when building a [`PirustToolDefinition`] by struct literal;
/// [`PirustToolDefinition::new`] applies it internally.
pub fn tool_execute<F, Fut>(execute: F) -> ToolExecuteFn
where
    F: Fn(String, Value, CancellationToken, AgentToolUpdateCallback) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<AgentToolResult, ToolError>> + Send + 'static,
{
    Arc::new(move |tool_call_id, args, token, on_update| {
        Box::pin(execute(tool_call_id, args, token, on_update))
    })
}

/// The UI-free half of Pi's `ToolDefinition` (extensions/types.ts:439-486).
///
/// `Send + Sync + 'static` (all fields are owned or `Arc`) so the runtime can
/// hold it as `Arc<dyn AgentTool>`; `Clone` is cheap (the two callable fields
/// are `Arc`s).
///
/// # Construction
///
/// The intended pattern for a tool module: declare the schema with the
/// [`schema`] helpers, write `execute` as a free `async fn`, then chain the
/// optional prompt metadata. Fields are public, so a struct literal with
/// [`tool_execute`] works too.
///
/// ```
/// use pirust_agent_core::types::{AgentToolResult, AgentToolUpdateCallback, ToolError};
/// use pirust_tools::definition::schema::{number_prop, object_schema, optional, required, string_prop};
/// use pirust_tools::definition::PirustToolDefinition;
/// use serde_json::{json, Value};
/// use tokio_util::sync::CancellationToken;
///
/// async fn execute_read(
///     _tool_call_id: String,
///     _args: Value,
///     _token: CancellationToken,
///     _on_update: AgentToolUpdateCallback,
/// ) -> Result<AgentToolResult, ToolError> {
///     Ok(AgentToolResult {
///         content: Vec::new(),
///         details: json!({}),
///         added_tool_names: None,
///         terminate: None,
///     })
/// }
///
/// let read = PirustToolDefinition::new(
///     "read",
///     "read",
///     "Read a file from the filesystem.",
///     object_schema([
///         required("path", string_prop("Path to the file to read (relative or absolute)")),
///         optional("offset", number_prop("Line number to start reading from (1-indexed)")),
///         optional("limit", number_prop("Maximum number of lines to read")),
///     ]),
///     execute_read,
/// )
/// .with_prompt_snippet("Read file contents")
/// .with_prompt_guidelines(["Use read to examine files instead of cat or sed."]);
///
/// assert_eq!(read.name, "read");
/// ```
#[derive(Clone)]
pub struct PirustToolDefinition {
    /// Tool name used in LLM tool calls (extensions/types.ts:441).
    pub name: String,
    /// Human-readable label for UI (extensions/types.ts:443).
    pub label: String,
    /// Description sent to the model (extensions/types.ts:445).
    pub description: String,
    /// One-line snippet for the system prompt's "Available tools" section
    /// (extensions/types.ts:447). Custom tools are omitted from that section
    /// when this is `None`.
    pub prompt_snippet: Option<String>,
    /// Guideline bullets appended to the system prompt's Guidelines section
    /// while this tool is active (extensions/types.ts:449).
    pub prompt_guidelines: Option<Vec<String>>,
    /// TypeBox parameter schema, pre-serialized (extensions/types.ts:451). Must
    /// be built so its key order matches Pi byte-for-byte — see [`schema`].
    pub parameters: Value,
    /// Optional pre-validation argument shim (extensions/types.ts:456).
    pub prepare_arguments: Option<PrepareArgumentsFn>,
    /// Per-tool execution-mode override (extensions/types.ts:465). `None` on all
    /// seven built-ins, so the loop default applies.
    pub execution_mode: Option<ToolExecutionMode>,
    /// Tool body (extensions/types.ts:468-474).
    pub execute: ToolExecuteFn,
}

impl PirustToolDefinition {
    /// Build a definition from the five always-present fields, taking `execute`
    /// as a plain `async fn`/async closure. Optional fields start `None`; add
    /// them with the `with_*` methods.
    pub fn new<F, Fut>(
        name: impl Into<String>,
        label: impl Into<String>,
        description: impl Into<String>,
        parameters: Value,
        execute: F,
    ) -> Self
    where
        F: Fn(String, Value, CancellationToken, AgentToolUpdateCallback) -> Fut
            + Send
            + Sync
            + 'static,
        Fut: Future<Output = Result<AgentToolResult, ToolError>> + Send + 'static,
    {
        Self {
            name: name.into(),
            label: label.into(),
            description: description.into(),
            prompt_snippet: None,
            prompt_guidelines: None,
            parameters,
            prepare_arguments: None,
            execution_mode: None,
            execute: tool_execute(execute),
        }
    }

    /// Set `promptSnippet` (extensions/types.ts:447).
    #[must_use]
    pub fn with_prompt_snippet(mut self, snippet: impl Into<String>) -> Self {
        self.prompt_snippet = Some(snippet.into());
        self
    }

    /// Set `promptGuidelines` (extensions/types.ts:449).
    #[must_use]
    pub fn with_prompt_guidelines<I, S>(mut self, guidelines: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.prompt_guidelines = Some(guidelines.into_iter().map(Into::into).collect());
        self
    }

    /// Set `prepareArguments` (extensions/types.ts:456). Only `edit` uses this.
    #[must_use]
    pub fn with_prepare_arguments<F>(mut self, prepare: F) -> Self
    where
        F: Fn(Value) -> Value + Send + Sync + 'static,
    {
        self.prepare_arguments = Some(Arc::new(prepare));
        self
    }

    /// Set `executionMode` (extensions/types.ts:465).
    #[must_use]
    pub fn with_execution_mode(mut self, mode: ToolExecutionMode) -> Self {
        self.execution_mode = Some(mode);
        self
    }

    /// Synthesize a minimal definition from an [`AgentTool`]
    /// (`createToolDefinitionFromAgentTool`, tool-definition-wrapper.ts:35-45).
    ///
    /// Keeps a definition-first registry usable even when a caller supplies a
    /// plain `AgentTool` that carries no prompt metadata, hence
    /// `prompt_snippet`/`prompt_guidelines` are `None`.
    ///
    /// Deviation: TS forwards `tool.prepareArguments`, preserving `undefined`.
    /// Rust cannot observe whether a trait default was overridden, so the shim
    /// is always forwarded. That is behaviour-preserving — an unoverridden
    /// [`AgentTool::prepare_arguments`] is the identity, which the loop reads as
    /// "no shim ran" — but it does make `prepare_arguments` always `Some`.
    #[must_use]
    pub fn from_agent_tool(tool: Arc<dyn AgentTool>) -> Self {
        let for_execute = Arc::clone(&tool);
        let for_prepare = Arc::clone(&tool);
        Self {
            name: tool.name().to_string(),
            label: tool.label().to_string(),
            description: tool.description().to_string(),
            prompt_snippet: None,
            prompt_guidelines: None,
            parameters: tool.parameters(),
            prepare_arguments: Some(Arc::new(move |raw| for_prepare.prepare_arguments(raw))),
            execution_mode: tool.execution_mode(),
            execute: tool_execute(move |tool_call_id: String, args, token, on_update| {
                let tool = Arc::clone(&for_execute);
                async move { tool.execute(&tool_call_id, args, token, on_update).await }
            }),
        }
    }
}

/// `wrapToolDefinition` (tool-definition-wrapper.ts:5-19): forward the seven
/// runtime fields, drop the prompt metadata and the renderers.
#[async_trait]
impl AgentTool for PirustToolDefinition {
    fn name(&self) -> &str {
        &self.name
    }

    fn label(&self) -> &str {
        &self.label
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters(&self) -> Value {
        self.parameters.clone()
    }

    fn prepare_arguments(&self, raw: Value) -> Value {
        match &self.prepare_arguments {
            Some(prepare) => prepare(raw),
            // `definition.prepareArguments` was `undefined`: the loop sees an
            // unchanged value and records that no shim ran.
            None => raw,
        }
    }

    async fn execute(
        &self,
        tool_call_id: &str,
        args: Value,
        token: CancellationToken,
        on_update: AgentToolUpdateCallback,
    ) -> Result<AgentToolResult, ToolError> {
        (self.execute)(tool_call_id.to_string(), args, token, on_update).await
    }

    fn execution_mode(&self) -> Option<ToolExecutionMode> {
        self.execution_mode
    }
}

impl fmt::Debug for PirustToolDefinition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PirustToolDefinition")
            .field("name", &self.name)
            .field("label", &self.label)
            .field("description", &self.description)
            .field("prompt_snippet", &self.prompt_snippet)
            .field("prompt_guidelines", &self.prompt_guidelines)
            .field("parameters", &self.parameters)
            .field(
                "prepare_arguments",
                &self.prepare_arguments.as_ref().map(|_| "<fn>"),
            )
            .field("execution_mode", &self.execution_mode)
            .field("execute", &"<fn>")
            .finish()
    }
}

/// TypeBox-key-order JSON Schema builders.
///
/// Pi declares every tool's `parameters` with TypeBox — e.g. `ls.ts:14-17`,
/// `edit.ts:33-56` — and ships `JSON.stringify(parameters)` verbatim to the
/// provider. The captured bytes live in `tests/fixtures/pi/tools/schemas/` and
/// pin an ordering that `#[derive(Serialize)]` on a Rust struct cannot produce,
/// so schemas are written as data through these helpers instead. They rely on
/// `serde_json`'s `preserve_order` feature (enabled in this crate's manifest):
/// `Map` is insertion-ordered, so key order is exactly the call order below.
///
/// Ordering rules, each verified against the seven captured schemas by
/// `tests/schema_builder.rs`:
///
/// 1. TypeBox emits `{ ...intrinsic keys, ...options }` — the options object
///    (which carries `description`) is spread **last**. Hence
///    `{"type":"array","items":{…},"description":"…"}`: `description` comes
///    *after* `items` (`edit.json`).
/// 2. An object's intrinsic keys are `type`, `required`, `properties` — that is,
///    `required` **before** `properties` (all six required-bearing schemas).
/// 3. `required` is **omitted entirely** when no property is required
///    (`ls.json` has no `required` key at all — both its params are
///    `Type.Optional`).
/// 4. `required` lists names in declaration order, filtered to the
///    non-`Type.Optional` properties (`edit.json`: `["path","edits"]`).
/// 5. `properties` is in declaration order; `Type.Optional` does not move a
///    property (`grep.json` interleaves required and optional).
/// 6. `Type.Object(props, {})` — an empty options object — adds nothing, so a
///    nested object schema is byte-identical to a top-level one
///    (`edit.json`'s `items`).
pub mod schema {
    use serde_json::{Map, Value};

    /// One declared property: its name, its schema, and whether TypeBox wrapped
    /// it in `Type.Optional`. Build with [`required`] / [`optional`].
    #[derive(Debug, Clone)]
    pub struct Prop {
        name: String,
        schema: Value,
        required: bool,
    }

    /// A property that is listed in the object's `required` array.
    pub fn required(name: impl Into<String>, schema: Value) -> Prop {
        Prop {
            name: name.into(),
            schema,
            required: true,
        }
    }

    /// A `Type.Optional(...)` property: present in `properties`, absent from
    /// `required`.
    pub fn optional(name: impl Into<String>, schema: Value) -> Prop {
        Prop {
            name: name.into(),
            schema,
            required: false,
        }
    }

    /// `Type.Object({ … })` → `{"type":"object","required":[…],"properties":{…}}`,
    /// with `required` omitted when empty (rules 2-5 above).
    pub fn object_schema<I>(props: I) -> Value
    where
        I: IntoIterator<Item = Prop>,
    {
        let props: Vec<Prop> = props.into_iter().collect();

        let mut object = Map::new();
        object.insert("type".to_string(), Value::String("object".to_string()));

        let required: Vec<Value> = props
            .iter()
            .filter(|prop| prop.required)
            .map(|prop| Value::String(prop.name.clone()))
            .collect();
        if !required.is_empty() {
            object.insert("required".to_string(), Value::Array(required));
        }

        let mut properties = Map::new();
        for prop in props {
            properties.insert(prop.name, prop.schema);
        }
        object.insert("properties".to_string(), Value::Object(properties));

        Value::Object(object)
    }

    /// `Type.String({ description })` → `{"type":"string","description":"…"}`.
    pub fn string_prop(description: impl Into<String>) -> Value {
        scalar_prop("string", description)
    }

    /// `Type.Number({ description })` → `{"type":"number","description":"…"}`.
    pub fn number_prop(description: impl Into<String>) -> Value {
        scalar_prop("number", description)
    }

    /// `Type.Boolean({ description })` → `{"type":"boolean","description":"…"}`.
    pub fn boolean_prop(description: impl Into<String>) -> Value {
        scalar_prop("boolean", description)
    }

    /// `Type.Array(items, { description })` →
    /// `{"type":"array","items":{…},"description":"…"}` — `description` after
    /// `items` (rule 1 above).
    pub fn array_prop(items: Value, description: impl Into<String>) -> Value {
        let mut array = Map::new();
        array.insert("type".to_string(), Value::String("array".to_string()));
        array.insert("items".to_string(), items);
        array.insert("description".to_string(), Value::String(description.into()));
        Value::Object(array)
    }

    fn scalar_prop(ty: &str, description: impl Into<String>) -> Value {
        let mut prop = Map::new();
        prop.insert("type".to_string(), Value::String(ty.to_string()));
        prop.insert("description".to_string(), Value::String(description.into()));
        Value::Object(prop)
    }
}

#[cfg(test)]
mod tests {
    use super::schema::{
        array_prop, boolean_prop, number_prop, object_schema, optional, required, string_prop,
    };
    use super::*;
    use serde_json::json;

    fn ok_result(details: Value) -> AgentToolResult {
        AgentToolResult {
            content: Vec::new(),
            details,
            added_tool_names: None,
            terminate: None,
        }
    }

    fn noop_update() -> AgentToolUpdateCallback {
        Arc::new(|_| {})
    }

    fn probe() -> PirustToolDefinition {
        PirustToolDefinition::new(
            "probe",
            "Probe",
            "A probe tool.",
            object_schema([required("path", string_prop("p"))]),
            |tool_call_id: String, args: Value, _token, _on_update| async move {
                Ok(ok_result(json!({ "id": tool_call_id, "args": args })))
            },
        )
    }

    #[test]
    fn defaults_match_wrapper_forwarding() {
        let def = probe();
        assert_eq!(AgentTool::name(&def), "probe");
        assert_eq!(AgentTool::label(&def), "Probe");
        assert_eq!(AgentTool::description(&def), "A probe tool.");
        assert_eq!(AgentTool::execution_mode(&def), None);
        assert!(def.prompt_snippet.is_none());
        assert!(def.prompt_guidelines.is_none());
        // No `prepareArguments`: the bridge is the identity, so the loop sees an
        // unchanged value and records that no shim ran.
        let raw = json!({ "path": "a.txt" });
        assert_eq!(AgentTool::prepare_arguments(&def, raw.clone()), raw);
    }

    #[test]
    fn optional_fields_round_trip() {
        let def = probe()
            .with_prompt_snippet("Probe things")
            .with_prompt_guidelines(["First bullet", "Second bullet"])
            .with_execution_mode(ToolExecutionMode::Sequential)
            .with_prepare_arguments(|mut raw: Value| {
                if let Some(object) = raw.as_object_mut() {
                    object.insert("shimmed".to_string(), Value::Bool(true));
                }
                raw
            });

        assert_eq!(def.prompt_snippet.as_deref(), Some("Probe things"));
        assert_eq!(
            def.prompt_guidelines.as_deref(),
            Some(["First bullet".to_string(), "Second bullet".to_string()].as_slice())
        );
        assert_eq!(
            AgentTool::execution_mode(&def),
            Some(ToolExecutionMode::Sequential)
        );
        assert_eq!(
            AgentTool::prepare_arguments(&def, json!({ "path": "a.txt" })),
            json!({ "path": "a.txt", "shimmed": true })
        );
    }

    #[tokio::test]
    async fn execute_forwards_all_four_arguments() {
        let def = probe();
        let result = AgentTool::execute(
            &def,
            "call_1",
            json!({ "path": "a.txt" }),
            CancellationToken::new(),
            noop_update(),
        )
        .await
        .expect("execute");
        assert_eq!(
            result.details,
            json!({ "id": "call_1", "args": { "path": "a.txt" } })
        );
    }

    #[tokio::test]
    async fn from_agent_tool_forwards_the_runtime_fields() {
        let inner: Arc<dyn AgentTool> = Arc::new(probe());
        let def = PirustToolDefinition::from_agent_tool(Arc::clone(&inner));

        assert_eq!(def.name, "probe");
        assert_eq!(def.label, "Probe");
        assert_eq!(def.description, "A probe tool.");
        assert_eq!(def.parameters, inner.parameters());
        assert!(def.prompt_snippet.is_none());

        let result = AgentTool::execute(
            &def,
            "call_2",
            Value::Null,
            CancellationToken::new(),
            noop_update(),
        )
        .await
        .expect("execute");
        assert_eq!(result.details, json!({ "id": "call_2", "args": null }));
    }

    #[test]
    fn definition_is_send_sync_static() {
        fn assert_bounds<T: Send + Sync + 'static>() {}
        assert_bounds::<PirustToolDefinition>();
        let _boxed: Arc<dyn AgentTool> = Arc::new(probe());
    }

    #[test]
    fn required_is_omitted_when_no_property_is_required() {
        let schema = object_schema([
            optional(
                "path",
                string_prop("Directory to list (default: current directory)"),
            ),
            optional(
                "limit",
                number_prop("Maximum number of entries to return (default: 500)"),
            ),
        ]);
        assert!(schema.get("required").is_none());
        assert!(serde_json::to_string(&schema)
            .unwrap()
            .starts_with(r#"{"type":"object","properties":"#));
    }

    #[test]
    fn scalar_and_array_props_put_description_last() {
        assert_eq!(
            serde_json::to_string(&boolean_prop("Case-insensitive search (default: false)"))
                .unwrap(),
            r#"{"type":"boolean","description":"Case-insensitive search (default: false)"}"#
        );
        assert_eq!(
            serde_json::to_string(&array_prop(
                object_schema([required("oldText", string_prop("old"))]),
                "One or more targeted replacements."
            ))
            .unwrap(),
            concat!(
                r#"{"type":"array","items":{"type":"object","required":["oldText"],"#,
                r#""properties":{"oldText":{"type":"string","description":"old"}}},"#,
                r#""description":"One or more targeted replacements."}"#
            )
        );
    }
}
