//! Oracle for `core/tools/index.ts` — the tool registry.
//!
//! Every other golden test in this crate reaches one tool through its own
//! constructor. This one reaches *all* of them through the registry's single
//! switch ([`create_tool_definition`] / [`create_tool`]) and asserts the result
//! against the bytes captured from real Pi by `scripts/gen-tools-oracle.mjs`:
//!
//! 1. `tests/fixtures/pi/tools/schemas/<tool>.json` — `AgentTool::parameters()`
//!    must serialize byte-identically.
//! 2. `tests/fixtures/pi/tools/strings/<tool>.json` — `AgentTool::name()`,
//!    `label()`, `description()`, plus `promptSnippet`, `promptGuidelines`,
//!    `executionMode` and `hasPrepareArguments`.
//! 3. The three named sets return Pi's tools in Pi's exact order
//!    (`index.ts:138-195`).
//! 4. `allToolNames` (`index.ts:84`) has seven members, in the union's order.
//!
//! `edit` is asserted separately (`edit_is_constructible_and_matches_its_fixtures`)
//! rather than inside the loops, because `all_tool_names_has_seven_members`
//! measures the loop set against [`ALL_TOOL_NAMES`]. Its own byte-level oracle is
//! `tests/edit_golden.rs`.
//!
//! A failure means the Rust port diverged from Pi; the fix is the port, never the
//! assertion.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use pirust_agent_core::types::{AgentTool, AgentToolUpdateCallback, ToolError};
use pirust_tools::read::{ReadOperations, ReadToolOptions};
use pirust_tools::{
    create_all_tool_definitions, create_all_tools, create_coding_tool_definitions,
    create_coding_tools, create_read_only_tool_definitions, create_read_only_tools, create_tool,
    create_tool_definition, ToolName, ToolsOptions, ALL_TOOL_NAMES, CODING_TOOL_NAMES,
    READ_ONLY_TOOL_NAMES,
};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

/// Any absolute path: nothing asserted here touches the filesystem, and no
/// built-in's metadata depends on `cwd`.
const CWD: &str = "C:\\anywhere";

/// The six names the loops below assert. `edit` is deliberately absent — see
/// [`edit_is_constructible_and_matches_its_fixtures`].
const PORTED_TOOL_NAMES: [ToolName; 6] = [
    ToolName::Read,
    ToolName::Bash,
    ToolName::Write,
    ToolName::Grep,
    ToolName::Find,
    ToolName::Ls,
];

fn fixture(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/pi/tools")
        .join(relative)
}

fn read_fixture(relative: &str) -> String {
    let path = fixture(relative);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read fixture {}: {e}", path.display()))
}

fn noop_update() -> AgentToolUpdateCallback {
    Arc::new(|_| {})
}

// ===========================================================================
// 1 + 2. Schema bytes and prompt strings, for every tool, through the registry
// ===========================================================================

#[test]
fn registry_parameters_match_the_captured_schema_bytes() {
    for tool_name in PORTED_TOOL_NAMES {
        let stem = tool_name.as_str();
        let want = read_fixture(&format!("schemas/{stem}.json"))
            .trim_end()
            .to_string();
        let definition = create_tool_definition(tool_name, CWD, None);

        // Through the `AgentTool` bridge, not the inherent field: this is what
        // the loop hands the provider.
        let got = serde_json::to_string(&AgentTool::parameters(&definition))
            .expect("serialize parameters");
        assert_eq!(
            got, want,
            "`{stem}` parameters must be byte-identical to schemas/{stem}.json\n  \
             expected: {want}\n  actual:   {got}"
        );
    }
}

#[test]
fn registry_strings_match_the_captured_metadata() {
    for tool_name in PORTED_TOOL_NAMES {
        let stem = tool_name.as_str();
        let raw = read_fixture(&format!("strings/{stem}.json"));
        let want: Value =
            serde_json::from_str(&raw).unwrap_or_else(|e| panic!("parse strings/{stem}.json: {e}"));
        let definition = create_tool_definition(tool_name, CWD, None);

        for (field, expected, actual) in [
            (
                "name",
                want["name"].as_str(),
                Some(AgentTool::name(&definition)),
            ),
            (
                "label",
                want["label"].as_str(),
                Some(AgentTool::label(&definition)),
            ),
            (
                "description",
                want["description"].as_str(),
                Some(AgentTool::description(&definition)),
            ),
            (
                "promptSnippet",
                want["promptSnippet"].as_str(),
                definition.prompt_snippet.as_deref(),
            ),
        ] {
            assert_eq!(
                actual, expected,
                "`{stem}`.{field} diverged\n  expected: {expected:?}\n  actual:   {actual:?}"
            );
        }

        // `name` is also the registry key, and Pi's `name`/`label` are the same
        // string on all seven built-ins.
        assert_eq!(
            AgentTool::name(&definition),
            stem,
            "the registry key must be the tool's own name"
        );

        // promptGuidelines: `null` in the fixture <-> `None` here; otherwise an
        // element-for-element match.
        let want_guidelines = &want["promptGuidelines"];
        match definition.prompt_guidelines.as_deref() {
            None => assert!(
                want_guidelines.is_null(),
                "`{stem}` has promptGuidelines in the fixture but none in the port: \
                 {want_guidelines}"
            ),
            Some(guidelines) => {
                let expected = want_guidelines
                    .as_array()
                    .unwrap_or_else(|| panic!("`{stem}` promptGuidelines should be an array"));
                let actual: Vec<Value> = guidelines
                    .iter()
                    .map(|line| Value::String(line.clone()))
                    .collect();
                assert_eq!(
                    &actual, expected,
                    "`{stem}` promptGuidelines diverged from strings/{stem}.json"
                );
            }
        }

        // No built-in overrides executionMode (`extensions/types.ts:465`).
        assert!(
            want["executionMode"].is_null(),
            "the fixture says `{stem}` has an executionMode override; the port assumes none"
        );
        assert_eq!(
            AgentTool::execution_mode(&definition),
            None,
            "`{stem}` must not override executionMode"
        );

        // Only `edit` has a prepareArguments shim, so every ported tool is
        // `false` — and the definition's `Option` must agree with the capture.
        assert_eq!(
            want["hasPrepareArguments"].as_bool(),
            Some(definition.prepare_arguments.is_some()),
            "`{stem}` hasPrepareArguments diverged from strings/{stem}.json"
        );
    }
}

/// `create_tool` (`index.ts:117-136`) is `wrapToolDefinition` over the same
/// switch, so the erased `Arc<dyn AgentTool>` must carry the same four fields.
#[test]
fn create_tool_erases_to_the_same_agent_tool() {
    for tool_name in PORTED_TOOL_NAMES {
        let stem = tool_name.as_str();
        let definition = create_tool_definition(tool_name, CWD, None);
        let tool = create_tool(tool_name, CWD, None);

        assert_eq!(tool.name(), AgentTool::name(&definition), "`{stem}` name");
        assert_eq!(
            tool.label(),
            AgentTool::label(&definition),
            "`{stem}` label"
        );
        assert_eq!(
            tool.description(),
            AgentTool::description(&definition),
            "`{stem}` description"
        );
        assert_eq!(
            tool.parameters(),
            AgentTool::parameters(&definition),
            "`{stem}` parameters"
        );
        assert_eq!(
            tool.execution_mode(),
            AgentTool::execution_mode(&definition),
            "`{stem}` executionMode"
        );
    }
}

// ===========================================================================
// 3. The three named sets, in Pi's exact order
// ===========================================================================

fn names_of(definitions: &[pirust_tools::definition::PirustToolDefinition]) -> Vec<&str> {
    definitions.iter().map(AgentTool::name).collect()
}

/// `index.ts:147-154` / `index.ts:177-184` — read, grep, find, ls.
#[test]
fn read_only_set_is_read_grep_find_ls_in_that_order() {
    assert_eq!(
        READ_ONLY_TOOL_NAMES.map(ToolName::as_str),
        ["read", "grep", "find", "ls"]
    );

    let definitions = create_read_only_tool_definitions(CWD, None);
    assert_eq!(names_of(&definitions), vec!["read", "grep", "find", "ls"]);

    let tools = create_read_only_tools(CWD, None);
    let names: Vec<&str> = tools.iter().map(|tool| tool.name()).collect();
    assert_eq!(names, vec!["read", "grep", "find", "ls"]);

    // The read-only set is the one set that contains no unported tool, so it is
    // infallible: nothing was dropped to make it succeed.
    assert_eq!(definitions.len(), READ_ONLY_TOOL_NAMES.len());
    assert!(!READ_ONLY_TOOL_NAMES.contains(&ToolName::Edit));
}

/// `index.ts:138-145` / `index.ts:168-175` — read, bash, edit, write. `edit` is
/// the third element, and the set must contain it: shipping three tools where Pi
/// ships four would change what the model can do.
#[test]
fn coding_set_is_read_bash_edit_write_in_that_order() {
    assert_eq!(
        CODING_TOOL_NAMES.map(ToolName::as_str),
        ["read", "bash", "edit", "write"]
    );

    let definitions = create_coding_tool_definitions(CWD, None);
    assert_eq!(
        names_of(&definitions),
        vec!["read", "bash", "edit", "write"]
    );

    let tools = create_coding_tools(CWD, None);
    let names: Vec<&str> = tools.iter().map(|tool| tool.name()).collect();
    assert_eq!(names, vec!["read", "bash", "edit", "write"]);
}

/// `index.ts:156-166` / `index.ts:186-195` — all seven, keyed in the order
/// read, bash, edit, write, grep, find, ls.
#[test]
fn all_set_keeps_pis_key_order() {
    let expected = ["read", "bash", "edit", "write", "grep", "find", "ls"];
    assert_eq!(ALL_TOOL_NAMES.map(ToolName::as_str), expected);

    let definitions = create_all_tool_definitions(CWD, None);
    // Both the keys and the values are in Pi's object-literal order, and each
    // key names its own tool (`index.ts:157-165`).
    let keys: Vec<&str> = definitions
        .iter()
        .map(|(tool_name, _)| tool_name.as_str())
        .collect();
    assert_eq!(keys, expected);
    let values: Vec<&str> = definitions
        .iter()
        .map(|(_, definition)| AgentTool::name(definition))
        .collect();
    assert_eq!(values, expected);

    let tools = create_all_tools(CWD, None);
    let tool_keys: Vec<&str> = tools
        .iter()
        .map(|(tool_name, _)| tool_name.as_str())
        .collect();
    assert_eq!(tool_keys, expected);
    let tool_names: Vec<&str> = tools.iter().map(|(_, tool)| tool.name()).collect();
    assert_eq!(tool_names, expected);
}

// ===========================================================================
// 4. allToolNames
// ===========================================================================

#[test]
fn all_tool_names_has_seven_members() {
    assert_eq!(ALL_TOOL_NAMES.len(), 7);

    // A JS `Set` has no duplicates; neither may the array standing in for it.
    let mut sorted = ALL_TOOL_NAMES;
    sorted.sort_unstable();
    let mut deduped = sorted.to_vec();
    deduped.dedup();
    assert_eq!(deduped.len(), 7, "allToolNames must have no duplicates");

    // Every name in the two named sets is a member of the full set.
    for tool_name in CODING_TOOL_NAMES.iter().chain(READ_ONLY_TOOL_NAMES.iter()) {
        assert!(
            ALL_TOOL_NAMES.contains(tool_name),
            "{tool_name} is not in allToolNames"
        );
    }

    // Six ported + `edit` accounts for all seven.
    assert_eq!(PORTED_TOOL_NAMES.len() + 1, ALL_TOOL_NAMES.len());
}

// ===========================================================================
// `edit`, the seventh name
// ===========================================================================

/// `edit` reaches the registry like every other name (`index.ts:102-103`) and
/// carries the captured fixtures' bytes.
///
/// This test replaces `edit_is_captured_but_unported`, which asserted
/// `RegistryError::EditNotPorted`: `core/tools/edit.ts` is ported now
/// (`crates/pirust-tools/src/edit.rs`), so that claim is false. `edit` stays out
/// of [`PORTED_TOOL_NAMES`] — which `all_tool_names_has_seven_members` measures
/// against [`ALL_TOOL_NAMES`] — so the fixture assertions the loops would have
/// made are made here instead. The exhaustive oracle (10 `prepareArguments`
/// cases + 56 corpus rows) is `tests/edit_golden.rs`.
#[test]
fn edit_is_constructible_and_matches_its_fixtures() {
    let want_schema = read_fixture("schemas/edit.json").trim_end().to_string();
    let strings: Value =
        serde_json::from_str(&read_fixture("strings/edit.json")).expect("parse strings/edit.json");

    let definition = create_tool_definition(ToolName::Edit, CWD, None);

    let got_schema =
        serde_json::to_string(&AgentTool::parameters(&definition)).expect("serialize parameters");
    assert_eq!(
        got_schema, want_schema,
        "`edit` parameters must be byte-identical to schemas/edit.json\n  \
         expected: {want_schema}\n  actual:   {got_schema}"
    );

    assert_eq!(AgentTool::name(&definition), "edit");
    assert_eq!(strings["name"].as_str(), Some(AgentTool::name(&definition)));
    assert_eq!(
        strings["label"].as_str(),
        Some(AgentTool::label(&definition))
    );
    assert_eq!(
        strings["description"].as_str(),
        Some(AgentTool::description(&definition))
    );
    assert_eq!(
        strings["promptSnippet"].as_str(),
        definition.prompt_snippet.as_deref()
    );
    let want_guidelines = strings["promptGuidelines"]
        .as_array()
        .expect("edit has promptGuidelines");
    let got_guidelines: Vec<Value> = definition
        .prompt_guidelines
        .as_deref()
        .expect("edit must set promptGuidelines")
        .iter()
        .map(|line| Value::String(line.clone()))
        .collect();
    assert_eq!(&got_guidelines, want_guidelines);

    assert!(
        strings["executionMode"].is_null(),
        "the fixture says `edit` has an executionMode override; the port assumes none"
    );
    assert_eq!(AgentTool::execution_mode(&definition), None);

    // `edit` is the one built-in with a `prepareArguments` shim (`edit.ts:307`).
    assert_eq!(
        strings["hasPrepareArguments"].as_bool(),
        Some(definition.prepare_arguments.is_some())
    );
    assert_eq!(strings["hasPrepareArguments"].as_bool(), Some(true));

    // The erased form carries the same four fields.
    let tool = create_tool(ToolName::Edit, CWD, None);
    assert_eq!(tool.name(), AgentTool::name(&definition));
    assert_eq!(tool.label(), AgentTool::label(&definition));
    assert_eq!(tool.description(), AgentTool::description(&definition));
    assert_eq!(tool.parameters(), AgentTool::parameters(&definition));
}

// ===========================================================================
// Options threading (index.ts:96-115 — `options?.<tool>`)
// ===========================================================================

/// A [`ReadOperations`] that answers with a fixed body, so "the `read` field of
/// the bag reached the `read` constructor" is observable.
struct FixedReadOperations;

#[async_trait]
impl ReadOperations for FixedReadOperations {
    async fn read_file(&self, _absolute_path: &str) -> Result<Vec<u8>, ToolError> {
        Ok(b"threaded through ToolsOptions.read\n".to_vec())
    }

    async fn access(&self, _absolute_path: &str) -> Result<(), ToolError> {
        Ok(())
    }
}

#[tokio::test]
async fn per_tool_options_reach_their_own_constructor() {
    let options = ToolsOptions {
        read: Some(ReadToolOptions {
            auto_resize_images: false,
            operations: Arc::new(FixedReadOperations),
        }),
        ..ToolsOptions::default()
    };

    let tool = create_tool(ToolName::Read, CWD, Some(&options));
    let result = tool
        .execute(
            "call_1",
            json!({ "path": "does-not-need-to-exist.txt" }),
            CancellationToken::new(),
            noop_update(),
        )
        .await
        .expect("execute");

    let text = serde_json::to_value(&result.content).expect("serialize content");
    assert_eq!(
        text[0]["text"].as_str(),
        Some("threaded through ToolsOptions.read\n"),
        "ToolsOptions.read must be the bag handed to createReadToolDefinition"
    );

    // A default bag is indistinguishable from `None` (TS `options?.read` is
    // `undefined` either way).
    let default_bag = ToolsOptions::default();
    let with_bag = create_tool_definition(ToolName::Ls, CWD, Some(&default_bag));
    let without_bag = create_tool_definition(ToolName::Ls, CWD, None);
    assert_eq!(
        AgentTool::parameters(&with_bag),
        AgentTool::parameters(&without_bag)
    );
}
