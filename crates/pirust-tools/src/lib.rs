//! `pirust-tools` — UI-free built-in tool logic and JSON schemas.
//!
//! Port of `packages/coding-agent/src/core/tools` (spec:
//! `docs/analysis/08-tools-spec.md`; roadmap: `docs/analysis/03-coding-agent.md` §4).
//!
//! Pi already splits UI from logic in every tool file: `create<X>ToolDefinition`
//! returns the prompt metadata + `execute` **plus** TUI-only `renderCall` /
//! `renderResult`, and `create<X>Tool` = `wrapToolDefinition(...)` drops the
//! renderers. This crate ports only the non-render half; the renderers land with
//! the TUI (feat-006/007).
//!
//! Every tool's `<X>Operations` side-effect seam becomes a trait with a
//! `Local<X>Operations` default impl, so tool behaviour is verifiable without
//! touching the filesystem — that is what makes the byte-identity goldens in
//! `tests/fixtures/pi/tools/` (captured from real Pi by
//! `scripts/gen-tools-oracle.mjs`) assertable.
//!
//! # The registry
//!
//! The crate root is also the port of `core/tools/index.ts` — Pi's tool registry:
//! [`ToolName`], [`ALL_TOOL_NAMES`], [`ToolsOptions`], the two switch functions
//! ([`create_tool_definition`] / [`create_tool`]) and the three named sets
//! (coding / read-only / all). `index.ts`'s re-export block (`index.ts:1-69`) has
//! no port: Rust reaches the same items through `pirust_tools::<module>::…`, and
//! the modules are already `pub`.
//!
//! All seven tools are ported, so every arm of the switch now hands back a tool
//! and the three named sets are all satisfiable. Every constructor is therefore
//! infallible and returns the value Pi returns: `index.ts:96-195` only throws
//! from `default:` arms that are unreachable here (see
//! [`create_tool_definition`]).

#![forbid(unsafe_code)]

use std::fmt;
use std::sync::Arc;

use pirust_agent_core::types::AgentTool;
use serde::{Deserialize, Serialize};

use crate::bash::{create_bash_tool_definition, BashToolOptions};
use crate::definition::PirustToolDefinition;
use crate::edit::{create_edit_tool_definition, EditToolOptions};
use crate::find::{create_find_tool_definition, FindToolOptions};
use crate::grep::{create_grep_tool_definition, GrepToolOptions};
use crate::ls::{create_ls_tool_definition, LsToolOptions};
use crate::read::{create_read_tool_definition, ReadToolOptions};
use crate::write::{create_write_tool_definition, WriteToolOptions};

pub mod bash;
pub mod binaries;
pub mod definition;
pub mod edit;
pub mod edit_diff;
pub mod find;
pub mod grep;
pub mod ls;
pub mod mutation_queue;
pub mod output_accumulator;
pub mod path_utils;
pub mod read;
pub mod truncate;
pub mod write;

/// Returns the crate name — linkage probe used by the `pirust` scaffold binary,
/// matching the sibling crates' `name()`.
pub fn name() -> &'static str {
    "pirust-tools"
}

// ===========================================================================
// Type aliases (index.ts:81-82)
// ===========================================================================

/// `index.ts:81` — `Tool = AgentTool<any>`.
///
/// TS can name the interface directly; Rust needs the erased, shared form to put
/// tools of different concrete types in one collection, so the alias is the
/// `Arc<dyn AgentTool>` the runtime actually holds (`AgentContext::tools`).
pub type Tool = Arc<dyn AgentTool>;

/// `index.ts:82` — `ToolDef = ToolDefinition<any, any>`. `any, any` erases the
/// input/details generics, which [`PirustToolDefinition`] does not have at all
/// (both are `serde_json::Value`), so the alias is exact.
pub type ToolDef = PirustToolDefinition;

/// `Record<ToolName, ToolDef>` (`index.ts:156`) as an ordered association list —
/// see [`create_all_tool_definitions`] for why.
pub type ToolDefRecord = Vec<(ToolName, ToolDef)>;

/// `Record<ToolName, Tool>` (`index.ts:186`), same shape as [`ToolDefRecord`].
pub type ToolRecord = Vec<(ToolName, Tool)>;

// ===========================================================================
// ToolName (index.ts:83-84)
// ===========================================================================

/// `index.ts:83` — `ToolName = "read" | "bash" | "edit" | "write" | "grep" |
/// "find" | "ls"`.
///
/// Variants are declared in the union's order, which is also `allToolNames`'
/// insertion order (`index.ts:84`) and the key order of the objects
/// `createAllToolDefinitions` / `createAllTools` return (`index.ts:157-165`,
/// `index.ts:187-194`). Serde renames each variant to the exact lowercase string
/// Pi puts on the wire, so `serde_json::to_string(&ToolName::Ls) == "\"ls\""`.
///
/// The enum is closed, which is what lets the two switch functions drop Pi's
/// `default: throw new Error(...)` arms — see [`create_tool_definition`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolName {
    /// `"read"` — [`crate::read`].
    Read,
    /// `"bash"` — [`crate::bash`].
    Bash,
    /// `"edit"` — [`crate::edit`].
    Edit,
    /// `"write"` — [`crate::write`].
    Write,
    /// `"grep"` — [`crate::grep`].
    Grep,
    /// `"find"` — [`crate::find`].
    Find,
    /// `"ls"` — [`crate::ls`].
    Ls,
}

impl ToolName {
    /// The wire string (`index.ts:83`), identical to the serde representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            ToolName::Read => "read",
            ToolName::Bash => "bash",
            ToolName::Edit => "edit",
            ToolName::Write => "write",
            ToolName::Grep => "grep",
            ToolName::Find => "find",
            ToolName::Ls => "ls",
        }
    }
}

impl fmt::Display for ToolName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `index.ts:84` — `allToolNames: Set<ToolName>`.
///
/// A JS `Set` iterates in insertion order, so the faithful Rust shape is an
/// ordered array rather than a `HashSet`; membership is
/// `ALL_TOOL_NAMES.contains(&name)`, which for seven elements is also what a set
/// would cost. Order: read, bash, edit, write, grep, find, ls.
pub const ALL_TOOL_NAMES: [ToolName; 7] = [
    ToolName::Read,
    ToolName::Bash,
    ToolName::Edit,
    ToolName::Write,
    ToolName::Grep,
    ToolName::Find,
    ToolName::Ls,
];

/// The `createCodingTool*` set, in Pi's order (`index.ts:139-144`,
/// `index.ts:169-174`): read, bash, edit, write.
pub const CODING_TOOL_NAMES: [ToolName; 4] = [
    ToolName::Read,
    ToolName::Bash,
    ToolName::Edit,
    ToolName::Write,
];

/// The `createReadOnlyTool*` set, in Pi's order (`index.ts:148-153`,
/// `index.ts:178-183`): read, grep, find, ls.
pub const READ_ONLY_TOOL_NAMES: [ToolName; 4] =
    [ToolName::Read, ToolName::Grep, ToolName::Find, ToolName::Ls];

// ===========================================================================
// ToolsOptions (index.ts:86-94)
// ===========================================================================

/// `index.ts:86-94` — `ToolsOptions`.
///
/// Fields are declared in Pi's order (read, bash, write, edit, grep, find, ls —
/// note `write` precedes `edit` here, unlike in [`ToolName`]). Each TS `?` field
/// becomes an `Option`, and "absent" means the same thing it does in Pi: the
/// tool's own `options ?? default` fallback applies. The per-tool constructors
/// disagree about whether they take `Option<X>` or `X`
/// ([`create_read_tool_definition`] vs [`create_ls_tool_definition`]); the
/// registry normalizes that, so this bag is uniformly `Option`.
///
/// No `Debug`: [`ReadToolOptions`] holds an `Arc<dyn ReadOperations>` and has no
/// `Debug` impl of its own.
#[derive(Clone, Default)]
pub struct ToolsOptions {
    /// `index.ts:87` — `read?: ReadToolOptions`.
    pub read: Option<ReadToolOptions>,
    /// `index.ts:88` — `bash?: BashToolOptions`.
    pub bash: Option<BashToolOptions>,
    /// `index.ts:89` — `write?: WriteToolOptions`.
    pub write: Option<WriteToolOptions>,
    /// `index.ts:90` — `edit?: EditToolOptions`.
    pub edit: Option<EditToolOptions>,
    /// `index.ts:91` — `grep?: GrepToolOptions`.
    pub grep: Option<GrepToolOptions>,
    /// `index.ts:92` — `find?: FindToolOptions`.
    pub find: Option<FindToolOptions>,
    /// `index.ts:93` — `ls?: LsToolOptions`.
    pub ls: Option<LsToolOptions>,
}

/// `options?.read` (`index.ts:99`).
fn read_options(options: Option<&ToolsOptions>) -> Option<ReadToolOptions> {
    options.and_then(|options| options.read.clone())
}

/// `options?.bash` (`index.ts:101`).
fn bash_options(options: Option<&ToolsOptions>) -> Option<BashToolOptions> {
    options.and_then(|options| options.bash.clone())
}

/// `options?.write` (`index.ts:105`).
fn write_options(options: Option<&ToolsOptions>) -> Option<WriteToolOptions> {
    options.and_then(|options| options.write.clone())
}

/// `options?.edit` (`index.ts:103`).
fn edit_options(options: Option<&ToolsOptions>) -> Option<EditToolOptions> {
    options.and_then(|options| options.edit.clone())
}

/// `options?.grep` (`index.ts:107`), collapsed to the constructor's non-optional
/// argument by the same `?? {}` the tool would have applied internally.
fn grep_options(options: Option<&ToolsOptions>) -> GrepToolOptions {
    options
        .and_then(|options| options.grep.clone())
        .unwrap_or_default()
}

/// `options?.find` (`index.ts:109`).
fn find_options(options: Option<&ToolsOptions>) -> FindToolOptions {
    options
        .and_then(|options| options.find.clone())
        .unwrap_or_default()
}

/// `options?.ls` (`index.ts:111`).
fn ls_options(options: Option<&ToolsOptions>) -> LsToolOptions {
    options
        .and_then(|options| options.ls.clone())
        .unwrap_or_default()
}

// ===========================================================================
// The two switch functions (index.ts:96-136)
// ===========================================================================

/// `index.ts:96-115` — `createToolDefinition(toolName, cwd, options?)`.
///
/// Deviations from the TS:
///
/// * Infallible. Pi's `default:` arm (`index.ts:112-113`,
///   `throw new Error(\`Unknown tool name: ${toolName}\`)`) is **unreachable**
///   here: [`ToolName`] is a closed enum, so the `match` is exhaustive over
///   exactly the seven names and an "unknown tool name" cannot be constructed.
///   That arm therefore has no port and no invented error variant.
/// * `options` is borrowed (`Option<&ToolsOptions>`) rather than moved, matching
///   TS's `options?` read-only access; the per-tool bag is cloned out of it.
pub fn create_tool_definition(
    tool_name: ToolName,
    cwd: &str,
    options: Option<&ToolsOptions>,
) -> ToolDef {
    match tool_name {
        // index.ts:98-99
        ToolName::Read => create_read_tool_definition(cwd, read_options(options)),
        // index.ts:100-101
        ToolName::Bash => create_bash_tool_definition(cwd, bash_options(options)),
        // index.ts:102-103
        ToolName::Edit => create_edit_tool_definition(cwd, edit_options(options)),
        // index.ts:104-105
        ToolName::Write => create_write_tool_definition(cwd, write_options(options)),
        // index.ts:106-107
        ToolName::Grep => create_grep_tool_definition(cwd, grep_options(options)),
        // index.ts:108-109
        ToolName::Find => create_find_tool_definition(cwd, find_options(options)),
        // index.ts:110-111
        ToolName::Ls => create_ls_tool_definition(cwd, ls_options(options)),
    }
}

/// `index.ts:117-136` — `createTool(toolName, cwd, options?)`.
///
/// In Pi each arm is `createXTool = wrapToolDefinition(createXToolDefinition(…))`
/// (e.g. `read.ts:349-351`). Here [`PirustToolDefinition`] *is* the
/// [`AgentTool`] — the wrapper is the trait impl (see [`definition`]) — so this
/// is a one-line conversion of [`create_tool_definition`] rather than a second
/// switch. Same `default:`-arm note as there.
pub fn create_tool(tool_name: ToolName, cwd: &str, options: Option<&ToolsOptions>) -> Tool {
    Arc::new(create_tool_definition(tool_name, cwd, options)) as Tool
}

/// `wrapToolDefinitions` (tool-definition-wrapper.ts:22-27) over a whole set.
fn wrap(definitions: Vec<ToolDef>) -> Vec<Tool> {
    definitions
        .into_iter()
        .map(|definition| Arc::new(definition) as Tool)
        .collect()
}

// ===========================================================================
// The coding set (index.ts:138-145, 168-175) — read, bash, edit, write
// ===========================================================================

/// `index.ts:138-145` — `createCodingToolDefinitions`: **read, bash, edit,
/// write**, in that order.
///
/// Each name goes through [`create_tool_definition`], which is exhaustive over
/// [`ToolName`], so there is no failure mode here either.
pub fn create_coding_tool_definitions(cwd: &str, options: Option<&ToolsOptions>) -> Vec<ToolDef> {
    CODING_TOOL_NAMES
        .iter()
        .map(|&tool_name| create_tool_definition(tool_name, cwd, options))
        .collect()
}

/// `index.ts:168-175` — `createCodingTools`. Same set, wrapped.
pub fn create_coding_tools(cwd: &str, options: Option<&ToolsOptions>) -> Vec<Tool> {
    wrap(create_coding_tool_definitions(cwd, options))
}

// ===========================================================================
// The read-only set (index.ts:147-154, 177-184) — read, grep, find, ls
// ===========================================================================

/// `index.ts:147-154` — `createReadOnlyToolDefinitions`: **read, grep, find,
/// ls**, in that order.
///
/// The constructors are called directly rather than through
/// [`create_tool_definition`], mirroring `index.ts:149-152`.
pub fn create_read_only_tool_definitions(
    cwd: &str,
    options: Option<&ToolsOptions>,
) -> Vec<ToolDef> {
    vec![
        // index.ts:149
        create_read_tool_definition(cwd, read_options(options)),
        // index.ts:150
        create_grep_tool_definition(cwd, grep_options(options)),
        // index.ts:151
        create_find_tool_definition(cwd, find_options(options)),
        // index.ts:152
        create_ls_tool_definition(cwd, ls_options(options)),
    ]
}

/// `index.ts:177-184` — `createReadOnlyTools`. Same set, wrapped.
pub fn create_read_only_tools(cwd: &str, options: Option<&ToolsOptions>) -> Vec<Tool> {
    wrap(create_read_only_tool_definitions(cwd, options))
}

// ===========================================================================
// The full set (index.ts:156-166, 186-195) — all seven
// ===========================================================================

/// `index.ts:156-166` — `createAllToolDefinitions`.
///
/// Pi returns a `Record<ToolName, ToolDef>`; the Rust shape is
/// [`ToolDefRecord`] — a **`Vec<(ToolName, ToolDef)>` in Pi's object-key order**
/// (read, bash, edit, write, grep, find, ls — [`ALL_TOOL_NAMES`]). An association list
/// was chosen over `HashMap`/`BTreeMap` because a V8 object literal iterates in
/// insertion order and that order is observable (`Object.values(...)` is what a
/// caller passes to the loop as its tool list); `HashMap` would lose it and
/// `BTreeMap` would replace it with an alphabetical one. The key is still there,
/// so `.iter().find(|(name, _)| *name == …)` recovers `record[name]`.
pub fn create_all_tool_definitions(cwd: &str, options: Option<&ToolsOptions>) -> ToolDefRecord {
    ALL_TOOL_NAMES
        .iter()
        .map(|&tool_name| (tool_name, create_tool_definition(tool_name, cwd, options)))
        .collect()
}

/// `index.ts:186-195` — `createAllTools`. Same shape as
/// [`create_all_tool_definitions`], wrapped.
pub fn create_all_tools(cwd: &str, options: Option<&ToolsOptions>) -> ToolRecord {
    create_all_tool_definitions(cwd, options)
        .into_iter()
        .map(|(tool_name, definition)| (tool_name, Arc::new(definition) as Tool))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_is_stable() {
        assert_eq!(name(), "pirust-tools");
    }

    /// The registry's own contract lives in `tests/registry.rs` (it is asserted
    /// against the captured Pi fixtures). These two only pin what a unit test
    /// can: the wire strings and that every name resolves to a tool.
    #[test]
    fn tool_names_serialize_to_pis_lowercase_strings() {
        assert_eq!(
            serde_json::to_string(&ALL_TOOL_NAMES).expect("serialize"),
            r#"["read","bash","edit","write","grep","find","ls"]"#
        );
        for tool_name in ALL_TOOL_NAMES {
            assert_eq!(
                serde_json::to_string(&tool_name).expect("serialize"),
                format!("\"{}\"", tool_name.as_str())
            );
            assert_eq!(tool_name.to_string(), tool_name.as_str());
        }
    }

    /// Every one of the seven names now builds; `edit` was the last gap
    /// (`index.ts:102-103`). The byte-level assertions against
    /// `tests/fixtures/pi/tools/` live in `tests/registry.rs` and
    /// `tests/edit_golden.rs`.
    #[test]
    fn every_tool_name_is_constructible() {
        for tool_name in ALL_TOOL_NAMES {
            let definition = create_tool_definition(tool_name, "C:\\anywhere", None);
            assert_eq!(AgentTool::name(&definition), tool_name.as_str());
            let tool = create_tool(tool_name, "C:\\anywhere", None);
            assert_eq!(tool.name(), tool_name.as_str());
        }
        // Only `edit` carries a `prepareArguments` shim (`edit.ts:307`).
        let with_shim: Vec<&str> = ALL_TOOL_NAMES
            .iter()
            .filter(|&&tool_name| {
                create_tool_definition(tool_name, "C:\\anywhere", None)
                    .prepare_arguments
                    .is_some()
            })
            .map(|tool_name| tool_name.as_str())
            .collect();
        assert_eq!(with_shim, vec!["edit"]);
    }
}
