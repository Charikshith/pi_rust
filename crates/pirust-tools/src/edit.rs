//! Port of `core/tools/edit.ts` (UI-free half) — the `edit` tool.
//!
//! Ties [`crate::edit_diff`]'s match/replace engine to the filesystem through
//! [`crate::mutation_queue`]. Gated by
//! `tests/fixtures/pi/tools/{schemas,strings}/edit.json`,
//! `edit.prepare.cases.jsonl` and `edit.diff.corpus.jsonl`.
//!
//! # Ported symbols
//!
//! | TS (`edit.ts`) | here |
//! | --- | --- |
//! | `replaceEditSchema` / `editSchema` (`:33-53`) | [`edit_parameters`] |
//! | `EditToolInput` (`:55`) | the raw `Value` `execute` receives — see [`validate_edit_input`] |
//! | `EditToolDetails` (`:61-68`) | [`EditToolDetails`] |
//! | `EditOperations` (`:74-81`) | [`EditOperations`] |
//! | `defaultEditOperations` (`:83-87`) | [`LocalEditOperations`] |
//! | `EditToolOptions` (`:89-92`) | [`EditToolOptions`] |
//! | `prepareEditArguments` (`:94-118`) | [`prepare_edit_arguments`] |
//! | `validateEditInput` (`:120-125`) | [`validate_edit_input`] |
//! | `createEditToolDefinition` data + `execute` (`:287-362`) | [`create_edit_tool_definition`] |
//! | `createEditTool` (`:435-437`) | [`create_edit_tool`] |
//!
//! Everything else in the file is TUI and lands with feat-006/007:
//! `EditRenderState` / `EditCallRenderComponent` and its helpers (`:27-31`,
//! `:127-193`, `:229-285`), `formatEditCall` / `formatEditResult` (`:195-227`),
//! `renderCall` / `renderResult` (`:363-431`) and the `renderShell: "self"`
//! field (`:306`), which [`crate::definition`] does not model. The
//! `computeEditsDiff` preview path (`edit-diff.ts:518-547`) that `renderCall`
//! drives is likewise unported.
//!
//! # `path` is the raw argument, everywhere it is observable
//!
//! `resolveToCwd` (`edit.ts:310`) produces the path the filesystem sees, and
//! *nothing else*. The success message (`:356`), the unified patch's `---`/`+++`
//! sides (`:351`), every match/replace error message (`:343`, via
//! [`crate::edit_diff`]) and the `access` failure (`:330`) all interpolate the
//! **raw, unresolved** `path` argument. `err-file-missing` in the corpus pins
//! that for `access` (`Could not edit file: file.txt. Error code: ENOENT.`), and
//! every `ok: true` row pins it for the patch (`--- file.txt`).
//!
//! Note also that the success string ends in a period (`:356`), unlike `write`'s
//! (`write.ts:222`), and that `edits.length` there is the count *after*
//! [`prepare_edit_arguments`] ran — the legacy-pair shim can add one.
//!
//! # Full-file overwrite, BOM and line endings restored
//!
//! `execute` reads the whole file, strips a BOM, detects the dominant line
//! ending, matches on LF-normalized text and then writes
//! `bom + restoreLineEndings(newContent, originalEnding)` back over the whole
//! file (`:335-347`). So the bytes on disk keep the BOM and the CRLFs the model
//! never saw, while `details.diff` / `details.patch` describe the LF-normalized
//! text. `writtenContent` in the corpus is the only oracle for that split.
//!
//! # Errors
//!
//! Three shapes leave `execute`:
//!
//! * [`EditError`] — `validateEditInput`'s rejection (`:122`) and the wrapped
//!   `access` failure (`:330`).
//! * [`crate::edit_diff::EditDiffError`] — propagated **unwrapped** from
//!   `applyEditsToNormalizedContent` (`:343`); its `Display` is already Pi's
//!   exact message, so no context may be added around it.
//! * [`OperationAborted`] — `Operation aborted` (`:318`), the same message
//!   `write` throws.
//!
//! `ops.readFile` / `ops.writeFile` rejections propagate unwrapped too: only the
//! `access` call sits in a `try`/`catch` (`:324-331`).
//!
//! As in `write`, the abort checks are polls *after* each await (`:317-319`,
//! `:321`, `:327`, `:332`, `:337`, `:344`, `:348`) rather than a race against
//! the token: rejecting from an abort listener would release the
//! [`crate::mutation_queue`] slot while an in-flight filesystem operation could
//! still finish (`:313-315`). The check at `:348` runs *after* `writeFile`, so an
//! abort racing the write reports `Operation aborted` even though the file has
//! already changed.

use std::io;
use std::sync::Arc;

use async_trait::async_trait;
use pirust_agent_core::types::{AgentTool, AgentToolResult, AgentToolUpdateCallback, ToolError};
use pirust_ai::types::{TextContent, UserContent};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio_util::sync::CancellationToken;

use crate::definition::schema::{array_prop, object_schema, required, string_prop};
use crate::definition::PirustToolDefinition;
use crate::edit_diff::{
    apply_edits_to_normalized_content, detect_line_ending, generate_diff_string,
    generate_unified_patch, normalize_to_lf, restore_line_endings, strip_bom, Edit,
};
use crate::mutation_queue::with_file_mutation_queue;
use crate::path_utils::resolve_to_cwd;
use crate::write::OperationAborted;

// ===========================================================================
// Static tool data (edit.ts:33-53, edit.ts:293-305)
// ===========================================================================

/// `name` / `label` (`edit.ts:293-294`) — the same string for this tool.
const EDIT_NAME: &str = "edit";

/// `description` (`edit.ts:295-296`).
const EDIT_DESCRIPTION: &str = "Edit a single file using exact text replacement. Every edits[].oldText must match a unique, non-overlapping region of the original file. If two changes affect the same block or nearby lines, merge them into one edit instead of emitting overlapping edits. Do not include large unchanged regions just to connect distant changes.";

/// `promptSnippet` (`edit.ts:297-298`).
const EDIT_PROMPT_SNIPPET: &str =
    "Make precise file edits with exact text replacement, including multiple disjoint edits in one call";

/// The four `promptGuidelines` bullets (`edit.ts:299-304`).
const EDIT_PROMPT_GUIDELINES: [&str; 4] = [
    "Use edit for precise changes (edits[].oldText must match exactly)",
    "When changing multiple separate locations in one file, use one edit call with multiple entries in edits[] instead of multiple edit calls",
    "Each edits[].oldText is matched against the original file, not after earlier edits are applied. Do not emit overlapping or nested edits. Merge nearby changes into one edit.",
    "Keep edits[].oldText as small as possible while still being unique in the file. Do not pad with large unchanged regions.",
];

/// `editSchema` (`edit.ts:44-53`) with its nested `replaceEditSchema`
/// (`edit.ts:33-42`), built through the TypeBox-key-order helpers so the bytes
/// match `tests/fixtures/pi/tools/schemas/edit.json`.
///
/// Two orderings are load-bearing and only these helpers produce them: the
/// nested `items` object carries its own `required` **before** its `properties`,
/// and the array's `description` comes **after** `items` (see
/// [`crate::definition::schema`] rules 1 and 2).
pub fn edit_parameters() -> Value {
    object_schema([
        required(
            "path",
            string_prop("Path to the file to edit (relative or absolute)"),
        ),
        required(
            "edits",
            array_prop(
                // edit.ts:33-42 — `Type.Object({...}, {})`; the empty options
                // object adds nothing, so this is shaped like a top-level schema.
                object_schema([
                    required(
                        "oldText",
                        string_prop(
                            "Exact text for one targeted replacement. It must be unique in the \
                             original file and must not overlap with any other edits[].oldText in \
                             the same call.",
                        ),
                    ),
                    required(
                        "newText",
                        string_prop("Replacement text for this targeted edit."),
                    ),
                ]),
                "One or more targeted replacements. Each edit is matched against the original \
                 file, not incrementally. Do not include overlapping or nested edits. If two \
                 changes touch the same block or nearby lines, merge them into one edit instead.",
            ),
        ),
    ])
}

/// `EditToolDetails` (`edit.ts:61-68`) — the `details` payload of an edit result.
///
/// Persisted verbatim into the session JSONL, so the shape mirrors Pi's object
/// literal `{ diff, patch, firstChangedLine }` (`edit.ts:359`): that key order,
/// camelCase, and `firstChangedLine` **omitted** rather than `null` when absent,
/// because `JSON.stringify` drops an `undefined` value. (On the success path it
/// is always present: a diff with no added/removed part means no change, which
/// `applyEditsToNormalizedContent` rejects first.)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditToolDetails {
    /// Display-oriented diff of the changes made.
    pub diff: String,
    /// Standard unified patch of the changes made.
    pub patch: String,
    /// Line number of the first change in the new file (for editor navigation).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_changed_line: Option<usize>,
}

// ===========================================================================
// Errors
// ===========================================================================

/// The two `throw` sites of `edit.ts` that build their own message, plus the one
/// seam a Rust port needs.
///
/// `Display` is the whole contract: the loop turns a failed `execute` into an
/// error tool result carrying `err.message`, which is what
/// `edit.diff.corpus.jsonl` captures in its `error` column.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EditError {
    /// `edit.ts:122` — `validateEditInput`'s rejection, thrown when `edits` is
    /// missing, not an array, or empty. Corpus row `err-empty-edits-array`.
    #[error("Edit tool input is invalid. edits must contain at least one replacement.")]
    InvalidInput,

    /// `edit.ts:330` — the wrapped `ops.access` failure. `path` is the **raw**
    /// argument and `error_message` is [`access_error_message`]'s output. Corpus
    /// row `err-file-missing`.
    #[error("Could not edit file: {path}. {error_message}.")]
    CouldNotEditFile {
        /// The raw (unresolved) `path` argument.
        path: String,
        /// `Error code: ${code}`, or the stringified error.
        error_message: String,
    },

    /// Not a Pi error: `args.path` was absent or not a string. Unreachable
    /// through the loop, which validates arguments against [`edit_parameters`]
    /// first (`path` is `required`); TS would instead read `input.path` as
    /// `undefined` (`edit.ts:124`) and propagate a `TypeError` out of
    /// `resolveToCwd`, whose exact V8 wording this port does not fabricate.
    #[error("edit: `path` argument must be a string")]
    PathArgumentNotAString,
}

/// A rejection from [`EditOperations`] that carries a Node `errno` code, i.e.
/// the `"code" in error` branch of `edit.ts:328-329`.
///
/// This is the seam that makes that branch expressible in Rust: [`ToolError`] is
/// a `Box<dyn Error>` with no `code` property, so `execute` recovers the code by
/// downcasting to this type ([`access_error_message`]). A non-local
/// implementation that wants Pi's `Error code: …` wording rejects with this;
/// anything else takes the `String(error)` branch.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct NodeFsError {
    /// The errno name (`ENOENT`, `EACCES`, …) — the only part `edit.ts:329`
    /// puts in the message.
    pub code: &'static str,
    /// Node's `error.message`. Not used by the `access` branch, but `readFile` /
    /// `writeFile` rejections propagate unwrapped (`edit.ts:335`, `:347`), so
    /// this is what reaches the model then.
    pub message: String,
}

impl NodeFsError {
    /// Format an [`io::Error`] the way Node formats a `fs` rejection:
    /// `${code}: ${description}, ${syscall} '${path}'`.
    ///
    /// The same table as `read`'s private `node_fs_error`
    /// (`crate::read`, `read.ts:52-56`'s twin); duplicated rather than shared
    /// because the two tools' error *types* differ and `read`'s formatter is
    /// private to that module. Only the codes this path can surface are mapped;
    /// anything else becomes libuv's own `UNKNOWN` fallback.
    fn from_io(error: &io::Error, syscall: &str, path: &str) -> Self {
        let (code, description) = match error.kind() {
            io::ErrorKind::NotFound => ("ENOENT", "no such file or directory"),
            io::ErrorKind::PermissionDenied => ("EACCES", "permission denied"),
            io::ErrorKind::IsADirectory => ("EISDIR", "illegal operation on a directory"),
            io::ErrorKind::NotADirectory => ("ENOTDIR", "not a directory"),
            io::ErrorKind::AlreadyExists => ("EEXIST", "file already exists"),
            _ => ("UNKNOWN", "unknown error"),
        };
        Self {
            code,
            message: format!("{code}: {description}, {syscall} '{path}'"),
        }
    }
}

/// `edit.ts:328-329` — the `errorMessage` an `ops.access` rejection produces.
///
/// TS is
/// `error instanceof Error && "code" in error ? \`Error code: ${error.code}\` : String(error)`.
/// The first branch is reproduced exactly by downcasting to [`NodeFsError`],
/// which is what [`LocalEditOperations`] always rejects with — so
/// `err-file-missing` (`Could not edit file: file.txt. Error code: ENOENT.`)
/// goes through it.
///
/// Documented divergence on the fallback: JS `String(error)` on an `Error`
/// instance yields `${name}: ${message}` (e.g. `Error: boom`), while a Rust
/// error's `Display` *is* the message alone and carries no class name to
/// recover. This port emits the message, so a custom [`EditOperations`] that
/// rejects without a code produces `Could not edit file: x. boom.` where Pi
/// would produce `Could not edit file: x. Error: boom.`. Unreachable with
/// [`LocalEditOperations`] and not covered by the corpus (all 15 error rows are
/// coded or upstream of `access`).
fn access_error_message(error: &ToolError) -> String {
    match error.downcast_ref::<NodeFsError>() {
        Some(node_error) => format!("Error code: {}", node_error.code),
        None => error.to_string(),
    }
}

// ===========================================================================
// Pluggable operations (edit.ts:70-87)
// ===========================================================================

/// Pluggable operations for the edit tool (`edit.ts:70-81`).
///
/// Override these to delegate file editing to remote systems (for example SSH).
/// Note that — unlike `write` — there *is* an `access` op: `execute` checks the
/// file first (`edit.ts:323-331`), which is what makes `edit` refuse to create a
/// missing file.
///
/// All three return [`ToolError`] rather than `io::Error` so a non-local
/// implementation can reject with its own error type, exactly as Pi's `Promise`
/// can reject with anything. `read_file` / `write_file` rejections propagate out
/// of `execute` unwrapped; an `access` rejection is wrapped by
/// [`EditError::CouldNotEditFile`] and only its errno code survives (see
/// [`access_error_message`]).
#[async_trait]
pub trait EditOperations: Send + Sync {
    /// Read file contents as bytes (TS `Buffer`, `edit.ts:76`).
    async fn read_file(&self, absolute_path: &str) -> Result<Vec<u8>, ToolError>;

    /// Write content to a file (`edit.ts:78`) — a full-file overwrite.
    async fn write_file(&self, absolute_path: &str, content: &str) -> Result<(), ToolError>;

    /// Check that the file is readable **and** writable; `Err` if not
    /// (`edit.ts:80`).
    async fn access(&self, absolute_path: &str) -> Result<(), ToolError>;
}

/// `defaultEditOperations` (`edit.ts:83-87`): the local filesystem.
#[derive(Debug, Clone, Copy, Default)]
pub struct LocalEditOperations;

#[async_trait]
impl EditOperations for LocalEditOperations {
    /// `fsReadFile(path)` (`edit.ts:84`).
    async fn read_file(&self, absolute_path: &str) -> Result<Vec<u8>, ToolError> {
        tokio::fs::read(absolute_path)
            .await
            .map_err(|error| NodeFsError::from_io(&error, "open", absolute_path).into())
    }

    /// `fsWriteFile(path, content, "utf-8")` (`edit.ts:85`) — `&str` is already
    /// UTF-8, so the encoding argument needs no counterpart.
    async fn write_file(&self, absolute_path: &str, content: &str) -> Result<(), ToolError> {
        tokio::fs::write(absolute_path, content)
            .await
            .map_err(|error| NodeFsError::from_io(&error, "open", absolute_path).into())
    }

    /// `fsAccess(path, constants.R_OK | constants.W_OK)` (`edit.ts:86`).
    ///
    /// `metadata` covers existence (`ENOENT` / `ENOTDIR`, which is the branch
    /// `err-file-missing` pins) and the read-only attribute covers `W_OK`, which
    /// is exactly what libuv's `uv_fs_access` inspects on Windows — the platform
    /// the corpus was captured on. On POSIX it is weaker than `access(2)`: mode
    /// bits are read without consulting the effective uid/gid, so a file the
    /// process cannot actually read still passes here and fails later inside
    /// [`EditOperations::read_file`], where the message says `open` rather than
    /// `access`. Same trade-off `read`'s `LocalReadOperations::access` documents.
    async fn access(&self, absolute_path: &str) -> Result<(), ToolError> {
        let metadata = tokio::fs::metadata(absolute_path)
            .await
            .map_err(|error| NodeFsError::from_io(&error, "access", absolute_path))?;
        if metadata.permissions().readonly() {
            return Err(NodeFsError::from_io(
                &io::Error::from(io::ErrorKind::PermissionDenied),
                "access",
                absolute_path,
            )
            .into());
        }
        Ok(())
    }
}

/// `EditToolOptions` (`edit.ts:89-92`).
#[derive(Clone, Default)]
pub struct EditToolOptions {
    /// Custom operations for file editing. Default: local filesystem
    /// ([`LocalEditOperations`]).
    pub operations: Option<Arc<dyn EditOperations>>,
}

impl std::fmt::Debug for EditToolOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EditToolOptions")
            .field("operations", &self.operations.as_ref().map(|_| "<ops>"))
            .finish()
    }
}

// ===========================================================================
// prepareArguments (edit.ts:94-118)
// ===========================================================================

/// `prepareEditArguments` (`edit.ts:94-118`) — the two shims Pi applies before
/// the arguments are validated against [`edit_parameters`].
///
/// Oracle: the 10 captured cases in
/// `tests/fixtures/pi/tools/edit.prepare.cases.jsonl`.
///
/// 1. **`edits` as a JSON string** (`:101-107`). Some models (Opus 4.6,
///    GLM-5.1) send the array as a string; it is parsed **in place** and only
///    adopted when it parses to an array. A string that does not parse, or
///    parses to a non-array, is left exactly as it arrived.
/// 2. **Legacy flat `oldText`/`newText`** (`:109-117`). Both must be strings;
///    then they are appended as **one extra edit, last**, and stripped from the
///    object. Any existing `edits` array is preserved ahead of the new entry.
///    If either is missing or not a string, the object is returned unchanged —
///    *including* any rewrite step 1 made.
///
/// Anything else passes through. `!input || typeof input !== "object"` (`:95`)
/// covers `null` and every primitive; a JSON **array** is `typeof "object"` in
/// JS and so reaches the body, but neither shim can fire on one (`args.edits`
/// and `args.oldText` are both `undefined`), so returning it untouched here is
/// the same observable behaviour.
pub fn prepare_edit_arguments(input: Value) -> Value {
    let Value::Object(mut args) = input else {
        return input;
    };

    // edit.ts:102-107 — `typeof args.edits === "string"`, parsed in place.
    // Cloned out first so the map is free to be mutated (JS needs no such step).
    let edits_string = args
        .get("edits")
        .and_then(Value::as_str)
        .map(str::to_string);
    if let Some(raw) = edits_string {
        if let Ok(parsed) = serde_json::from_str::<Value>(&raw) {
            if parsed.is_array() {
                // Re-inserting an existing key keeps its position (serde_json's
                // `preserve_order`), which is what a JS property assignment does.
                args.insert("edits".to_string(), parsed);
            }
        }
    }

    // edit.ts:110-112 — both legacy fields must be strings, or nothing happens.
    let legacy = match (args.get("oldText"), args.get("newText")) {
        (Some(Value::String(old_text)), Some(Value::String(new_text))) => {
            Some((old_text.clone(), new_text.clone()))
        }
        _ => None,
    };
    let Some((old_text, new_text)) = legacy else {
        return Value::Object(args);
    };

    // edit.ts:114-115 — `Array.isArray(legacy.edits) ? [...legacy.edits] : []`,
    // then push the legacy pair LAST.
    let mut edits = match args.get("edits") {
        Some(Value::Array(edits)) => edits.clone(),
        _ => Vec::new(),
    };
    let mut legacy_edit = Map::new();
    legacy_edit.insert("oldText".to_string(), Value::String(old_text));
    legacy_edit.insert("newText".to_string(), Value::String(new_text));
    edits.push(Value::Object(legacy_edit));

    // edit.ts:116-117 — `const { oldText, newText, ...rest } = legacy; return { ...rest, edits }`.
    // `shift_remove` is the rest-spread's key order; serde_json's `remove` would
    // swap the last key into the hole.
    args.shift_remove("oldText");
    args.shift_remove("newText");
    args.insert("edits".to_string(), Value::Array(edits));
    Value::Object(args)
}

// ===========================================================================
// validateEditInput (edit.ts:120-125)
// ===========================================================================

/// `validateEditInput` (`edit.ts:120-125`).
///
/// Returns the **raw** `path` — `resolveToCwd` runs afterwards, at
/// `edit.ts:310`, and its result never reaches a message.
///
/// Deviations, both forced by Rust and both unreachable through the loop (which
/// validates against [`edit_parameters`] before calling `execute`):
///
/// * `path` must deserialize as a string, or [`EditError::PathArgumentNotAString`]
///   surfaces where TS would carry `undefined` into `resolveToCwd`. The order is
///   still Pi's: the `edits` check runs first, so a call with neither a `path`
///   nor usable `edits` reports the `edits` problem.
/// * each `edits` entry must deserialize as `{oldText, newText}` strings, or the
///   `serde_json` error surfaces; TS only checks `Array.isArray` here and would
///   throw a `TypeError` from inside `normalizeToLF` (`edit-diff.ts:305-307`).
fn validate_edit_input(input: &Value) -> Result<(String, Vec<Edit>), ToolError> {
    // edit.ts:121-123 — missing / non-array / empty are one message.
    let edits = match input.get("edits") {
        Some(Value::Array(edits)) if !edits.is_empty() => edits,
        _ => return Err(EditError::InvalidInput.into()),
    };

    // edit.ts:124 — `input.path`, unresolved.
    let path = input
        .get("path")
        .and_then(Value::as_str)
        .ok_or(EditError::PathArgumentNotAString)?
        .to_string();
    let edits: Vec<Edit> = serde_json::from_value(Value::Array(edits.clone()))?;

    Ok((path, edits))
}

// ===========================================================================
// execute (edit.ts:308-362)
// ===========================================================================

/// `throwIfAborted` (`edit.ts:317-319`).
fn throw_if_aborted(token: &CancellationToken) -> Result<(), OperationAborted> {
    if token.is_cancelled() {
        return Err(OperationAborted);
    }
    Ok(())
}

/// The body of `execute` (`edit.ts:308-362`), in Pi's exact order: validate,
/// resolve the path, then run everything else inside the file mutation queue.
async fn execute_edit(
    cwd: &str,
    ops: &dyn EditOperations,
    args: Value,
    token: &CancellationToken,
) -> Result<AgentToolResult, ToolError> {
    // edit.ts:309-310. `absolute_path` is *not* what any message reports.
    let (path, edits) = validate_edit_input(&args)?;
    let absolute_path = resolve_to_cwd(&path, cwd)?;

    // edit.ts:312 — the whole read-modify-write body, so a concurrent
    // edit/write of the same file cannot interleave.
    with_file_mutation_queue(absolute_path.clone(), || async {
        throw_if_aborted(token)?; // edit.ts:321

        // edit.ts:323-331 — check the file exists (and is readable/writable).
        if let Err(error) = ops.access(&absolute_path).await {
            // edit.ts:327 — an abort that landed during `access` wins over the
            // access failure.
            throw_if_aborted(token)?;
            return Err(EditError::CouldNotEditFile {
                path,
                error_message: access_error_message(&error),
            }
            .into());
        }
        throw_if_aborted(token)?; // edit.ts:332

        // edit.ts:334-337 — read the file.
        let buffer = ops.read_file(&absolute_path).await?;
        let raw_content = String::from_utf8_lossy(&buffer);
        throw_if_aborted(token)?;

        // edit.ts:339-343. Strip the BOM before matching: the model will not
        // include an invisible BOM in `oldText`.
        let (bom, content) = strip_bom(&raw_content);
        let original_ending = detect_line_ending(content);
        let normalized_content = normalize_to_lf(content);
        // Propagated unwrapped: `EditDiffError`'s `Display` is already Pi's
        // message, and `path` inside it is the raw argument.
        let applied = apply_edits_to_normalized_content(&normalized_content, &edits, &path)?;
        throw_if_aborted(token)?; // edit.ts:344

        // edit.ts:346-347 — one full-file overwrite, BOM and dominant line
        // ending restored.
        let final_content = format!(
            "{bom}{}",
            restore_line_endings(&applied.new_content, original_ending)
        );
        ops.write_file(&absolute_path, &final_content).await?;
        // edit.ts:348 — after the write: an abort racing it reports failure even
        // though the file has already changed.
        throw_if_aborted(token)?;

        // edit.ts:350-360, both with the default 4 context lines and the patch
        // naming the raw relative path on both sides.
        let diff_result = generate_diff_string(&applied.base_content, &applied.new_content);
        let patch = generate_unified_patch(&path, &applied.base_content, &applied.new_content);
        Ok(AgentToolResult {
            content: vec![UserContent::Text(TextContent::new(format!(
                "Successfully replaced {} block(s) in {path}.",
                edits.len()
            )))],
            details: serde_json::to_value(EditToolDetails {
                diff: diff_result.diff,
                patch,
                first_changed_line: diff_result.first_changed_line,
            })
            .expect("EditToolDetails is always serializable"),
            added_tool_names: None,
            terminate: None,
        })
    })
    .await
}

// ===========================================================================
// Factories (edit.ts:287-307, edit.ts:435-437)
// ===========================================================================

/// `createEditToolDefinition(cwd, options)` (`edit.ts:287-433`), minus the
/// renderers and `renderShell: "self"` (`:306`).
///
/// `cwd` and the resolved [`EditOperations`] are captured by the `execute`
/// closure, which is what Pi's `ops` binding (`edit.ts:291`) and
/// `wrapToolDefinition`'s `ctxFactory` do between them. `edit` sets no
/// `executionMode`, so the loop default applies.
pub fn create_edit_tool_definition(
    cwd: &str,
    options: Option<EditToolOptions>,
) -> PirustToolDefinition {
    // edit.ts:291 — `options?.operations ?? defaultEditOperations`.
    let ops: Arc<dyn EditOperations> = options
        .and_then(|options| options.operations)
        .unwrap_or_else(|| Arc::new(LocalEditOperations));
    let cwd = cwd.to_string();

    PirustToolDefinition::new(
        EDIT_NAME,
        EDIT_NAME,
        EDIT_DESCRIPTION,
        edit_parameters(),
        move |_tool_call_id: String,
              args: Value,
              token: CancellationToken,
              _on_update: AgentToolUpdateCallback| {
            let cwd = cwd.clone();
            let ops = Arc::clone(&ops);
            async move { execute_edit(&cwd, ops.as_ref(), args, &token).await }
        },
    )
    .with_prompt_snippet(EDIT_PROMPT_SNIPPET)
    .with_prompt_guidelines(EDIT_PROMPT_GUIDELINES)
    .with_prepare_arguments(prepare_edit_arguments)
}

/// `createEditTool(cwd, options)` (`edit.ts:435-437`).
///
/// Pi calls `wrapToolDefinition`; here [`PirustToolDefinition`] *is* the wrapper
/// (see [`crate::definition`]), so this is only the erasure to `Arc<dyn
/// AgentTool>`.
pub fn create_edit_tool(cwd: &str, options: Option<EditToolOptions>) -> Arc<dyn AgentTool> {
    Arc::new(create_edit_tool_definition(cwd, options))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Mutex;

    /// Records every operation with its arguments, and optionally cancels a
    /// token from inside `access` / `write_file` — the only way to force the
    /// abort interleavings deterministically.
    #[derive(Default)]
    struct RecordingOps {
        content: String,
        calls: Arc<Mutex<Vec<String>>>,
        access_error: Option<ToolError>,
        cancel_during_access: Option<CancellationToken>,
        cancel_during_write: Option<CancellationToken>,
    }

    impl RecordingOps {
        /// Not `new`: the log has to be shared with the caller.
        fn pair(ops: Self) -> (Arc<Self>, Arc<Mutex<Vec<String>>>) {
            let calls = Arc::clone(&ops.calls);
            (Arc::new(ops), calls)
        }

        fn with_content(content: &str) -> Self {
            Self {
                content: content.to_string(),
                ..Self::default()
            }
        }
    }

    #[async_trait]
    impl EditOperations for RecordingOps {
        async fn read_file(&self, absolute_path: &str) -> Result<Vec<u8>, ToolError> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("read_file({absolute_path})"));
            Ok(self.content.clone().into_bytes())
        }

        async fn write_file(&self, absolute_path: &str, content: &str) -> Result<(), ToolError> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("write_file({absolute_path}, {content})"));
            if let Some(token) = &self.cancel_during_write {
                token.cancel();
            }
            Ok(())
        }

        async fn access(&self, absolute_path: &str) -> Result<(), ToolError> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("access({absolute_path})"));
            if let Some(token) = &self.cancel_during_access {
                token.cancel();
            }
            match &self.access_error {
                // `ToolError` is not `Clone`; a `NodeFsError` is, and that is the
                // one shape whose `code` the tool reads.
                Some(error) => match error.downcast_ref::<NodeFsError>() {
                    Some(node_error) => Err(node_error.clone().into()),
                    None => Err(error.to_string().into()),
                },
                None => Ok(()),
            }
        }
    }

    fn noop_update() -> AgentToolUpdateCallback {
        Arc::new(|_| {})
    }

    fn entries(calls: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
        calls.lock().unwrap().clone()
    }

    /// `edit.ts:348`: the last `throwIfAborted()` runs *after* `writeFile`, so an
    /// abort landing during the write reports failure even though the file has
    /// already been modified. Not expressible in the captured corpus (no row is
    /// an abort), hence pinned here.
    #[tokio::test]
    async fn aborting_during_the_write_reports_failure_after_the_file_changed() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().to_str().unwrap();
        let token = CancellationToken::new();
        let (ops, calls) = RecordingOps::pair(RecordingOps {
            cancel_during_write: Some(token.clone()),
            ..RecordingOps::with_content("a\nb\n")
        });

        let error = AgentTool::execute(
            &create_edit_tool_definition(
                cwd,
                Some(EditToolOptions {
                    operations: Some(ops),
                }),
            ),
            "call_abort_during_write",
            json!({ "path": "raced.txt", "edits": [{ "oldText": "a", "newText": "A" }] }),
            token,
            noop_update(),
        )
        .await
        .expect_err("the post-write abort check must fail the call");

        assert_eq!(error.to_string(), "Operation aborted");
        // The write really did happen before the failure was reported.
        let expected_path = resolve_to_cwd("raced.txt", cwd).unwrap();
        assert_eq!(
            entries(&calls),
            vec![
                format!("access({expected_path})"),
                format!("read_file({expected_path})"),
                format!("write_file({expected_path}, A\nb\n)"),
            ]
        );
    }

    /// `edit.ts:321`: the first `throwIfAborted()` precedes `access`, so an
    /// already-aborted call touches nothing.
    #[tokio::test]
    async fn aborting_before_access_runs_no_operations() {
        let dir = tempfile::tempdir().unwrap();
        let token = CancellationToken::new();
        token.cancel();
        let (ops, calls) = RecordingOps::pair(RecordingOps::with_content("a\n"));

        let error = AgentTool::execute(
            &create_edit_tool_definition(
                dir.path().to_str().unwrap(),
                Some(EditToolOptions {
                    operations: Some(ops),
                }),
            ),
            "call_abort_before_access",
            json!({ "path": "untouched.txt", "edits": [{ "oldText": "a", "newText": "A" }] }),
            token,
            noop_update(),
        )
        .await
        .expect_err("an already-aborted call must fail");

        assert_eq!(error.to_string(), "Operation aborted");
        assert!(entries(&calls).is_empty(), "{:?}", entries(&calls));
    }

    /// `edit.ts:326-327`: the `throwIfAborted()` inside the `catch` runs *before*
    /// the `Could not edit file` message is built, so an abort that lands while
    /// `access` was failing reports `Operation aborted` instead. The token is
    /// cancelled from inside `access`, which is the only way to reach that branch
    /// with a cancellation the first check (`:321`) could not have seen.
    #[tokio::test]
    async fn an_abort_during_a_failing_access_beats_the_access_message() {
        let dir = tempfile::tempdir().unwrap();
        let token = CancellationToken::new();
        let (ops, calls) = RecordingOps::pair(RecordingOps {
            access_error: Some(Box::new(NodeFsError {
                code: "ENOENT",
                message: "ENOENT: no such file or directory, access 'x'".to_string(),
            })),
            cancel_during_access: Some(token.clone()),
            ..RecordingOps::with_content("a\n")
        });

        let error = AgentTool::execute(
            &create_edit_tool_definition(
                dir.path().to_str().unwrap(),
                Some(EditToolOptions {
                    operations: Some(ops),
                }),
            ),
            "call_abort_and_access_failure",
            json!({ "path": "missing.txt", "edits": [{ "oldText": "a", "newText": "A" }] }),
            token,
            noop_update(),
        )
        .await
        .expect_err("must fail");

        assert_eq!(error.to_string(), "Operation aborted");
        // `access` really did run and really did fail first.
        assert_eq!(entries(&calls).len(), 1, "{:?}", entries(&calls));
    }

    /// `edit.ts:328-329`, second branch: an `access` rejection with no errno code
    /// is stringified instead. See [`access_error_message`] for the documented
    /// divergence from JS `String(error)`.
    #[tokio::test]
    async fn an_uncoded_access_failure_is_stringified() {
        let dir = tempfile::tempdir().unwrap();
        let (ops, _) = RecordingOps::pair(RecordingOps {
            access_error: Some("remote link down".into()),
            ..RecordingOps::with_content("a\n")
        });

        let error = AgentTool::execute(
            &create_edit_tool_definition(
                dir.path().to_str().unwrap(),
                Some(EditToolOptions {
                    operations: Some(ops),
                }),
            ),
            "call_uncoded_access_failure",
            json!({ "path": "remote.txt", "edits": [{ "oldText": "a", "newText": "A" }] }),
            CancellationToken::new(),
            noop_update(),
        )
        .await
        .expect_err("must fail");

        assert_eq!(
            error.to_string(),
            "Could not edit file: remote.txt. remote link down."
        );
    }

    /// The default operations really are the local filesystem: a real file is
    /// read, overwritten in place, and the reported path is the raw argument.
    #[tokio::test]
    async fn default_operations_edit_the_real_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "one\ntwo\n").unwrap();

        let result = AgentTool::execute(
            &create_edit_tool_definition(dir.path().to_str().unwrap(), None),
            "call_default_ops",
            json!({ "path": "f.txt", "edits": [{ "oldText": "two", "newText": "TWO" }] }),
            CancellationToken::new(),
            noop_update(),
        )
        .await
        .expect("edit should succeed");

        assert_eq!(
            serde_json::to_string(&result.content).unwrap(),
            r#"[{"type":"text","text":"Successfully replaced 1 block(s) in f.txt."}]"#
        );
        assert_eq!(
            std::fs::read(dir.path().join("f.txt")).unwrap(),
            "one\nTWO\n".as_bytes()
        );
    }

    /// `edit.ts:86` — `access` demands `W_OK`, so a read-only file is refused
    /// before anything is read. The code is Node's `EACCES`.
    #[tokio::test]
    async fn a_read_only_file_fails_access_with_eacces() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ro.txt");
        std::fs::write(&path, "one\n").unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&path, permissions).unwrap();

        let error = AgentTool::execute(
            &create_edit_tool_definition(dir.path().to_str().unwrap(), None),
            "call_read_only",
            json!({ "path": "ro.txt", "edits": [{ "oldText": "one", "newText": "ONE" }] }),
            CancellationToken::new(),
            noop_update(),
        )
        .await
        .expect_err("a read-only file must fail W_OK");

        assert_eq!(
            error.to_string(),
            "Could not edit file: ro.txt. Error code: EACCES."
        );
        // Nothing was written.
        assert_eq!(std::fs::read(&path).unwrap(), "one\n".as_bytes());

        // Windows cannot delete a read-only file, so hand the file the tempdir's
        // own (writable) permissions and let the guard clean up.
        let writable = std::fs::metadata(dir.path()).unwrap().permissions();
        std::fs::set_permissions(&path, writable).unwrap();
    }

    /// `details` is persisted into the session JSONL, so its key order and the
    /// `undefined` handling are part of the contract (`edit.ts:359`).
    #[test]
    fn details_serializes_in_pis_key_order() {
        assert_eq!(
            serde_json::to_string(&EditToolDetails {
                diff: "d".to_string(),
                patch: "p".to_string(),
                first_changed_line: Some(6),
            })
            .unwrap(),
            r#"{"diff":"d","patch":"p","firstChangedLine":6}"#
        );
        // TS `firstChangedLine: undefined` is dropped by `JSON.stringify`.
        assert_eq!(
            serde_json::to_string(&EditToolDetails {
                diff: "d".to_string(),
                patch: "p".to_string(),
                first_changed_line: None,
            })
            .unwrap(),
            r#"{"diff":"d","patch":"p"}"#
        );
    }
}
