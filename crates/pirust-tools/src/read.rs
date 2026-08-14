//! Port of `core/tools/read.ts` (UI-free half) — the `read` tool.
//!
//! Gated by `tests/fixtures/pi/tools/{schemas,strings}/read.json` and the `read`
//! rows of `exec.corpus.jsonl`.
//!
//! # What is here and what is not
//!
//! `read.ts` is half tool, half TUI. Ported here: the `createReadToolDefinition`
//! data (`read.ts:203-215`), its `execute` (`read.ts:216-328`) and the
//! `ReadOperations` side-effect seam (`read.ts:39-63`). Skipped as TUI
//! (feat-006/007): `formatReadCall`, `formatReadLineRange`,
//! `formatCompactReadCall`, `formatReadResult`, `getCompactReadClassification`,
//! `getPiDocsClassification`, `trimTrailingEmptyLines`, `renderCall`,
//! `renderResult`. `createReadTool` (`read.ts:349-351`) needs no port either:
//! [`PirustToolDefinition`] *is* the `AgentTool`, so `wrapToolDefinition` is the
//! trait impl (see [`crate::definition`]).
//!
//! # The text branch is the whole contract
//!
//! Every one of the four mutually exclusive output forms (`read.ts:290-314`) is
//! pinned by a fixture row, and each carries a different continuation hint:
//!
//! | form | notice | `details` |
//! | --- | --- | --- |
//! | first line over the byte limit | `[Line N is …, exceeds … limit. Use bash: sed -n …]` | `Some` |
//! | truncated by lines | `[Showing lines a-b of N. Use offset=… to continue.]` | `Some` |
//! | truncated by bytes | `[Showing lines a-b of N (50.0KB limit). Use offset=… to continue.]` | `Some` |
//! | user `limit` stopped early | `[K more lines in file. Use offset=… to continue.]` | `None` |
//!
//! Two counts coexist and must not be conflated: `totalFileLines` is
//! `split("\n").length`, which a trailing newline inflates by one (so a 10-line
//! file reports `of 11`, fixture rows 1 and 7), whereas
//! [`TruncationResult::total_lines`] drops that phantom line (`of 701` next to
//! `totalLines: 700`, fixture row 10). Likewise `50KB` in the tool description is
//! a template literal over `DEFAULT_MAX_BYTES / 1024`, while `50.0KB` in the
//! notices comes from [`format_size`] — both literals exist and both are pinned.
//!
//! # Numbers are JavaScript numbers
//!
//! `offset`/`limit` are TypeBox `Type.Number()`, so schema validation admits
//! `0`, negatives and fractions. Pi then leans on JS semantics: `offset ? … : 0`
//! makes `0` behave like "absent" (fixture row 8), `Math.max`/`Math.min` work on
//! doubles, `Array.prototype.slice` truncates its bounds, and the notices
//! interpolate the raw value. This port keeps the arithmetic in `f64`, converts
//! to indices with [`js_slice_index`] and formats with `js_number`, so a
//! fractional offset produces Pi's (odd) `[Showing lines 3.5-…]` rather than a
//! rounded number.
//!
//! # Abort
//!
//! Pi wires an `abort` listener that rejects with `Operation aborted` the instant
//! the signal fires, *and* re-checks an `aborted` flag after every await
//! (`read.ts:225-239`). Here the established mapping applies: the
//! [`CancellationToken`] is checked at entry and after each await, yielding the
//! same `Operation aborted` error for the same inputs. Only the timing differs —
//! Pi's promise settles mid-await, this one at the next checkpoint — which is
//! unobservable to the loop, since it awaits `execute` either way.
//!
//! # Gap: image *processing* (not detection)
//!
//! MIME detection (`utils/mime.ts`) is a pure byte sniff and is ported in full
//! ([`image_mime`]), so a local read of a PNG still takes Pi's image branch and
//! is never mis-read as text. What cannot be ported in feat-004 is
//! `processImage` (`utils/image-process.ts`): it decodes, resizes and re-encodes
//! through Photon/WASM, i.e. it needs the `image` crate
//! (`docs/analysis/03-coding-agent.md:201`) that this feature is not allowed to
//! add. [`process_image`] is therefore the plug-in seam and currently returns
//! [`ReadError::ImageProcessingUnported`]; everything around it — detection, the
//! note strings, the hint/vision-note assembly in [`image_content`] — is ported
//! and unit-tested. See the `#[ignore]`d
//! `image_branch_needs_image_processing_dep` in `tests/read_golden.rs`.
//!
//! The vision check has no ported equivalent yet: `getNonVisionImageNote`
//! (`read.ts:87-92`) reads `ctx?.model`, and there is no `ExtensionContext` port
//! (see [`crate::definition`]). The function is ported as
//! [`non_vision_image_note`] and [`execute_read`] calls it with `None`, which is
//! exactly what Pi does today for a definition invoked without a context — as
//! the oracle did when capturing `exec.corpus.jsonl`.

use std::io;
use std::sync::Arc;

use async_trait::async_trait;
use pirust_agent_core::types::{AgentToolResult, ToolError};
use pirust_ai::jsnum::js_number;
use pirust_ai::types::{Modality, Model, TextContent, UserContent};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::definition::schema::{number_prop, object_schema, optional, required, string_prop};
use crate::definition::PirustToolDefinition;
use crate::path_utils::resolve_read_path_async;
use crate::truncate::{
    format_size, truncate_head, TruncatedBy, TruncationOptions, TruncationResult,
    DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES,
};

// ===========================================================================
// Definition data (read.ts:20-24, 210-215)
// ===========================================================================

/// `read.ts:210` / `read.ts:211` — `name` and `label` are the same string.
pub const READ_TOOL_NAME: &str = "read";

/// `read.ts:213` — `promptSnippet`.
pub const READ_PROMPT_SNIPPET: &str = "Read file contents";

/// `read.ts:214` — the single `promptGuidelines` bullet.
pub const READ_PROMPT_GUIDELINES: [&str; 1] = ["Use read to examine files instead of cat or sed."];

/// `read.ts:212` — `description`, a template literal over [`DEFAULT_MAX_LINES`]
/// and `DEFAULT_MAX_BYTES / 1024`.
///
/// The division is JS floating point, but `51200 / 1024` is exactly `50`, so the
/// integer division below renders the same `50KB` the fixture pins. Note this is
/// **not** [`format_size`]'s `50.0KB`.
pub fn read_description() -> String {
    format!(
        "Read the contents of a file. Supports text files and images (jpg, png, gif, webp, bmp). \
         Images are sent as attachments. For text files, output is truncated to \
         {DEFAULT_MAX_LINES} lines or {}KB (whichever is hit first). Use offset/limit for large \
         files. When you need the full file, continue with offset until complete.",
        DEFAULT_MAX_BYTES / 1024
    )
}

/// `read.ts:20-24` — `readSchema`. Key order is TypeBox's; see
/// [`crate::definition::schema`].
pub fn read_parameters() -> Value {
    object_schema([
        required(
            "path",
            string_prop("Path to the file to read (relative or absolute)"),
        ),
        optional(
            "offset",
            number_prop("Line number to start reading from (1-indexed)"),
        ),
        optional("limit", number_prop("Maximum number of lines to read")),
    ])
}

/// `read.ts:28-30` — `ReadToolDetails`, the `details` payload of a read result.
///
/// Persisted (it lands in the session JSONL through the tool result), so the
/// shape mirrors Pi's object literal `{ truncation }` (`read.ts:294`): a single
/// optional key, omitted rather than `null` when absent — Pi only ever builds
/// this value with the key present, and `details` itself is `undefined` in the
/// two non-truncating forms.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadToolDetails {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncation: Option<TruncationResult>,
}

// ===========================================================================
// Errors
// ===========================================================================

/// The `throw` sites of `read.ts`'s `execute`, plus the two seams a Rust port
/// needs.
///
/// `Display` is the whole contract: the loop turns a failed `execute` into an
/// error tool result carrying `err.message`, which is what `exec.corpus.jsonl`
/// captures in its `error` column.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReadError {
    /// `read.ts:226` / `read.ts:231` — the abort rejection.
    #[error("Operation aborted")]
    Aborted,

    /// `read.ts:275` — `Offset ${offset} is beyond end of file (${n} lines total)`.
    /// `offset` is the raw argument, formatted as JS would interpolate it.
    #[error("Offset {offset} is beyond end of file ({total_lines} lines total)")]
    OffsetBeyondEndOfFile { offset: String, total_lines: usize },

    /// A `node:fs` rejection from [`ReadOperations`], formatted as Node does.
    #[error("{code}: {description}, {syscall} '{path}'")]
    NodeFs {
        code: &'static str,
        description: &'static str,
        syscall: &'static str,
        path: String,
    },

    /// Not a Pi error: the [`process_image`] seam is unported (see the module
    /// docs' "Gap"). Loud on purpose — the alternative would be silently
    /// returning an image-less result Pi never produces.
    #[error("read: image processing is not ported yet (feat-004 has no image codec dependency); cannot inline {mime_type} from {path}")]
    ImageProcessingUnported { mime_type: String, path: String },

    /// Not a Pi error: `args.path` was absent or not a string. Unreachable
    /// through the loop, which validates arguments against
    /// [`read_parameters`] first (`path` is `required`); TS would instead
    /// propagate a `TypeError` from `normalizePath`, whose exact V8 wording this
    /// port does not attempt to fabricate.
    #[error("read: `path` argument must be a string")]
    PathArgumentNotAString,
}

/// Format an [`io::Error`] the way Node formats a `fs` rejection:
/// `${code}: ${description}, ${syscall} '${path}'` — the shape fixture row 13
/// pins (`ENOENT: no such file or directory, access '…\files\nope.txt'`).
///
/// Only the codes Pi's read path can realistically surface are mapped; anything
/// else becomes `UNKNOWN: unknown error`, which is also libuv's own fallback.
/// One uncovered divergence: Node's `readFile` of a *directory* reports
/// `EISDIR: illegal operation on a directory, read` with **no** path, because the
/// failure comes from the pathless `read` syscall; this formatter always names
/// the syscall it was given and always includes the path. No fixture row reaches
/// that case.
fn node_fs_error(error: &io::Error, syscall: &'static str, path: &str) -> ReadError {
    let (code, description) = match error.kind() {
        io::ErrorKind::NotFound => ("ENOENT", "no such file or directory"),
        io::ErrorKind::PermissionDenied => ("EACCES", "permission denied"),
        io::ErrorKind::IsADirectory => ("EISDIR", "illegal operation on a directory"),
        io::ErrorKind::NotADirectory => ("ENOTDIR", "not a directory"),
        io::ErrorKind::AlreadyExists => ("EEXIST", "file already exists"),
        _ => ("UNKNOWN", "unknown error"),
    };
    ReadError::NodeFs {
        code,
        description,
        syscall,
        path: path.to_string(),
    }
}

// ===========================================================================
// ReadOperations (read.ts:39-63)
// ===========================================================================

/// `read.ts:43-50` — pluggable operations for the read tool. Override to
/// delegate file reading to a remote system (for example SSH).
///
/// `detectImageMimeType` is optional in TS (`read.ts:49`) and `execute` treats a
/// missing detector exactly like one that returned `null`/`undefined`
/// (`read.ts:243`), so a single `Option<String>` return models both; the default
/// method is the "not provided" case.
#[async_trait]
pub trait ReadOperations: Send + Sync {
    /// `read.ts:45` — read file contents as bytes (TS `Buffer`).
    async fn read_file(&self, absolute_path: &str) -> Result<Vec<u8>, ToolError>;

    /// `read.ts:47` — check that the file is readable; `Err` if not.
    async fn access(&self, absolute_path: &str) -> Result<(), ToolError>;

    /// `read.ts:49` — detect an image MIME type; `None` for non-images.
    async fn detect_image_mime_type(
        &self,
        absolute_path: &str,
    ) -> Result<Option<String>, ToolError> {
        let _ = absolute_path;
        Ok(None)
    }
}

/// `read.ts:52-56` — `defaultReadOperations`, the local filesystem.
///
/// `access` is Pi's `fsAccess(path, R_OK)`. `metadata` is the closest std
/// equivalent and matches libuv on Windows (where `uv_fs_access` only inspects
/// existence and the read-only attribute), the platform `exec.tree.json` was
/// captured on. On POSIX it is weaker: a file whose mode denies reading passes
/// `access` here and fails later inside [`ReadOperations::read_file`], so the
/// error says `open` where Pi's says `access`.
#[derive(Debug, Clone, Copy, Default)]
pub struct LocalReadOperations;

#[async_trait]
impl ReadOperations for LocalReadOperations {
    async fn read_file(&self, absolute_path: &str) -> Result<Vec<u8>, ToolError> {
        tokio::fs::read(absolute_path)
            .await
            .map_err(|error| node_fs_error(&error, "open", absolute_path).into())
    }

    async fn access(&self, absolute_path: &str) -> Result<(), ToolError> {
        tokio::fs::metadata(absolute_path)
            .await
            .map(|_| ())
            .map_err(|error| node_fs_error(&error, "access", absolute_path).into())
    }

    async fn detect_image_mime_type(
        &self,
        absolute_path: &str,
    ) -> Result<Option<String>, ToolError> {
        image_mime::detect_from_file(absolute_path).await
    }
}

// ===========================================================================
// ReadToolOptions (read.ts:58-63)
// ===========================================================================

/// `read.ts:58-63` — `ReadToolOptions`.
///
/// TS's optional fields become resolved values here: [`Default`] applies the
/// same `?? true` / `?? defaultReadOperations` that `read.ts:207-208` does, and
/// `createReadToolDefinition(cwd)` (no options) is
/// `create_read_tool_definition(cwd, None)`.
#[derive(Clone)]
pub struct ReadToolOptions {
    /// `read.ts:60` — whether to auto-resize images to 2000x2000 max.
    pub auto_resize_images: bool,
    /// `read.ts:62` — custom operations for file reading.
    pub operations: Arc<dyn ReadOperations>,
}

impl Default for ReadToolOptions {
    fn default() -> Self {
        Self {
            auto_resize_images: true,
            operations: Arc::new(LocalReadOperations),
        }
    }
}

// ===========================================================================
// The image branch's strings (utils/image-process.ts, read.ts:87-92)
// ===========================================================================

/// `read.ts:91` — appended when the active model cannot accept images.
pub const NON_VISION_IMAGE_NOTE: &str =
    "[Current model does not support images. The image will be omitted from this request.]";

/// `image-process.ts:82` — `processImage`'s conversion failure message.
pub const IMAGE_CONVERSION_FAILED_MESSAGE: &str =
    "[Image omitted: could not be converted to a supported inline image format.]";

/// `image-process.ts:91` — `processImage`'s resize failure message.
pub const IMAGE_RESIZE_FAILED_MESSAGE: &str =
    "[Image omitted: could not be resized below the inline image size limit.]";

/// `read.ts:87-92` — `getNonVisionImageNote`.
///
/// `None` for "no model known" (TS `!model`, i.e. `ctx?.model` being
/// `undefined`) and for a model that lists the `image` input modality.
pub fn non_vision_image_note(model: Option<&Model>) -> Option<&'static str> {
    match model {
        Some(model) if !model.input.contains(&Modality::Image) => Some(NON_VISION_IMAGE_NOTE),
        _ => None,
    }
}

/// `image-process.ts:11-21` — `ProcessImageResult`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessImageResult {
    /// `ok: true` — base64 `data`, the (possibly rewritten) `mime_type`, and the
    /// conversion/dimension hints (`image-process.ts:95-106`).
    Ok {
        data: String,
        mime_type: String,
        hints: Vec<String>,
    },
    /// `ok: false` — one of [`IMAGE_CONVERSION_FAILED_MESSAGE`] /
    /// [`IMAGE_RESIZE_FAILED_MESSAGE`].
    Failed { message: String },
}

/// `image-process.ts:72-119` — `processImage`. **Unported seam**: see the module
/// docs' "Gap". Returns [`ReadError::ImageProcessingUnported`] for every input.
///
/// The signature is the one a real implementation needs, so filling it in is a
/// body swap: decode `bytes` (already sniffed as `mime_type`), normalize to
/// png/jpeg/gif/webp, resize to the inline limit when `auto_resize_images`, and
/// base64-encode the result.
async fn process_image(
    bytes: &[u8],
    mime_type: &str,
    auto_resize_images: bool,
    path: &str,
) -> Result<ProcessImageResult, ReadError> {
    let _ = (bytes, auto_resize_images);
    Err(ReadError::ImageProcessingUnported {
        mime_type: mime_type.to_string(),
        path: path.to_string(),
    })
}

/// `read.ts:247-263` — turn a [`ProcessImageResult`] into the tool's content
/// blocks.
///
/// Split out of `execute` so the note assembly is testable while
/// [`process_image`] is unported. `detected_mime_type` is the *sniffed* type,
/// which the failure note uses (`read.ts:252`), whereas the success note uses
/// the processed one (`read.ts:256`).
fn image_content(
    processed: &ProcessImageResult,
    detected_mime_type: &str,
    non_vision_note: Option<&str>,
) -> Vec<UserContent> {
    match processed {
        ProcessImageResult::Failed { message } => {
            let mut text_note = format!("Read image file [{detected_mime_type}]\n{message}");
            if let Some(note) = non_vision_note {
                text_note.push('\n');
                text_note.push_str(note);
            }
            vec![UserContent::Text(TextContent::new(text_note))]
        }
        ProcessImageResult::Ok {
            data,
            mime_type,
            hints,
        } => {
            let mut text_note = format!("Read image file [{mime_type}]");
            if !hints.is_empty() {
                text_note.push('\n');
                text_note.push_str(&hints.join("\n"));
            }
            if let Some(note) = non_vision_note {
                text_note.push('\n');
                text_note.push_str(note);
            }
            vec![
                UserContent::Text(TextContent::new(text_note)),
                UserContent::Image(pirust_ai::types::ImageContent {
                    kind: pirust_ai::types::ImageTag::Image,
                    data: data.clone(),
                    mime_type: mime_type.clone(),
                }),
            ]
        }
    }
}

// ===========================================================================
// The tool
// ===========================================================================

/// `read.ts:203-347` — `createReadToolDefinition`, minus the renderers.
///
/// `cwd` and the resolved options are captured by the `execute` closure, which
/// is how this port replaces `wrapToolDefinition`'s `ctxFactory` (see
/// [`crate::definition`]).
pub fn create_read_tool_definition(
    cwd: impl Into<String>,
    options: Option<ReadToolOptions>,
) -> PirustToolDefinition {
    // read.ts:207-208
    let options = options.unwrap_or_default();
    let auto_resize_images = options.auto_resize_images;
    let operations = options.operations;
    let cwd = cwd.into();

    PirustToolDefinition::new(
        READ_TOOL_NAME,
        READ_TOOL_NAME,
        read_description(),
        read_parameters(),
        move |_tool_call_id, args, token, _on_update| {
            let cwd = cwd.clone();
            let operations = Arc::clone(&operations);
            async move {
                execute_read(&cwd, operations.as_ref(), auto_resize_images, &args, &token).await
            }
        },
    )
    .with_prompt_snippet(READ_PROMPT_SNIPPET)
    .with_prompt_guidelines(READ_PROMPT_GUIDELINES)
}

/// `read.ts:216-328` — the `execute` body.
///
/// Exposed (rather than hidden in the closure) so a caller can drive the tool
/// with its own [`ReadOperations`] without building a definition; the definition
/// forwards straight to it.
pub async fn execute_read(
    cwd: &str,
    operations: &dyn ReadOperations,
    auto_resize_images: bool,
    args: &Value,
    token: &CancellationToken,
) -> Result<AgentToolResult, ToolError> {
    // read.ts:225-228 — `if (signal?.aborted) reject(...)`.
    if token.is_cancelled() {
        return Err(ReadError::Aborted.into());
    }

    // read.ts:218 — `{ path, offset, limit }`. A JSON `null` is read as absent:
    // schema validation rejects it upstream, so the two are indistinguishable
    // in practice (`offset: null` is falsy in TS anyway).
    let path = args
        .get("path")
        .and_then(Value::as_str)
        .ok_or(ReadError::PathArgumentNotAString)?;
    let offset = args.get("offset").and_then(Value::as_f64);
    let limit = args.get("limit").and_then(Value::as_f64);

    // read.ts:238
    let absolute_path = resolve_read_path_async(path, cwd).await?;
    if token.is_cancelled() {
        return Err(ReadError::Aborted.into());
    }
    // read.ts:241 — check that the file exists and is readable.
    operations.access(&absolute_path).await?;
    if token.is_cancelled() {
        return Err(ReadError::Aborted.into());
    }
    // read.ts:243
    let mime_type = operations.detect_image_mime_type(&absolute_path).await?;
    if token.is_cancelled() {
        return Err(ReadError::Aborted.into());
    }

    // read.ts:246 — no `ExtensionContext` port yet, so `ctx?.model` is `undefined`.
    let non_vision_note = non_vision_image_note(None);

    // read.ts:247 — `if (mimeType)`: an empty string is falsy in JS.
    if let Some(mime_type) = mime_type.as_deref().filter(|mime| !mime.is_empty()) {
        // read.ts:249-263 — read the image as binary and process it.
        let buffer = operations.read_file(&absolute_path).await?;
        if token.is_cancelled() {
            return Err(ReadError::Aborted.into());
        }
        let processed =
            process_image(&buffer, mime_type, auto_resize_images, &absolute_path).await?;
        if token.is_cancelled() {
            return Err(ReadError::Aborted.into());
        }
        return Ok(read_result(
            image_content(&processed, mime_type, non_vision_note),
            None,
        ));
    }

    // read.ts:265-315 — read text content.
    let buffer = operations.read_file(&absolute_path).await?;
    if token.is_cancelled() {
        return Err(ReadError::Aborted.into());
    }
    Ok(read_text_content(&buffer, path, offset, limit)?)
}

/// The text branch (`read.ts:266-315`), lifted out of `execute` verbatim.
///
/// `path` is the **raw argument**, not the resolved absolute path: the
/// first-line-too-long notice interpolates it into a `sed` command exactly as
/// the user wrote it (`read.ts:293`, fixture row 12).
fn read_text_content(
    buffer: &[u8],
    path: &str,
    offset: Option<f64>,
    limit: Option<f64>,
) -> Result<AgentToolResult, ReadError> {
    // read.ts:267 — `buffer.toString("utf-8")`, which replaces invalid
    // sequences with U+FFFD instead of failing.
    let text_content = String::from_utf8_lossy(buffer);
    // read.ts:268-269 — `split("\n")`, so a trailing newline yields a final
    // empty element and `totalFileLines` is one more than the line count.
    let all_lines: Vec<&str> = text_content.split('\n').collect();
    let total_file_lines = all_lines.len();

    // read.ts:271 — `offset ? Math.max(0, offset - 1) : 0`. `0` is falsy, so
    // `offset: 0` reads from line 1 (fixture row 8).
    let start_line = match offset {
        Some(offset) if offset != 0.0 && !offset.is_nan() => (offset - 1.0).max(0.0),
        _ => 0.0,
    };
    // read.ts:272
    let start_line_display = start_line + 1.0;

    // read.ts:274-276
    if start_line >= total_file_lines as f64 {
        return Err(ReadError::OffsetBeyondEndOfFile {
            offset: offset.map_or_else(|| "undefined".to_string(), js_number),
            total_lines: total_file_lines,
        });
    }

    let start_index = js_slice_index(start_line, total_file_lines);

    // read.ts:280-286 — honour a user `limit` first; otherwise `truncateHead`
    // decides.
    let (selected_content, user_limited_lines) = match limit {
        Some(limit) => {
            let end_line = (start_line + limit).min(total_file_lines as f64);
            let end_index = js_slice_index(end_line, total_file_lines);
            let selected = if end_index > start_index {
                all_lines[start_index..end_index].join("\n")
            } else {
                String::new()
            };
            (selected, Some(end_line - start_line))
        }
        None => (all_lines[start_index..].join("\n"), None),
    };

    // read.ts:288 — respect both the line and the byte limit.
    let truncation = truncate_head(&selected_content, TruncationOptions::default());

    let mut details: Option<ReadToolDetails> = None;
    let output_text = if truncation.first_line_exceeds_limit {
        // read.ts:290-294 — the first line alone busts the byte limit; point the
        // model at a bash fallback. `${DEFAULT_MAX_BYTES}` is the raw number.
        let first_line_size = format_size(all_lines[start_index].len() as u64);
        let display = js_number(start_line_display);
        details = Some(ReadToolDetails {
            truncation: Some(truncation),
        });
        format!(
            "[Line {display} is {first_line_size}, exceeds {} limit. Use bash: sed -n '{display}p' {path} | head -c {DEFAULT_MAX_BYTES}]",
            format_size(DEFAULT_MAX_BYTES)
        )
    } else if truncation.truncated {
        // read.ts:295-305 — actionable continuation notice.
        let end_line_display = start_line_display + truncation.output_lines as f64 - 1.0;
        let next_offset = end_line_display + 1.0;
        let mut output_text = truncation.content.clone();
        let by_lines = truncation.truncated_by == Some(TruncatedBy::Lines);
        output_text.push_str(&if by_lines {
            format!(
                "\n\n[Showing lines {}-{} of {total_file_lines}. Use offset={} to continue.]",
                js_number(start_line_display),
                js_number(end_line_display),
                js_number(next_offset)
            )
        } else {
            format!(
                "\n\n[Showing lines {}-{} of {total_file_lines} ({} limit). Use offset={} to continue.]",
                js_number(start_line_display),
                js_number(end_line_display),
                format_size(DEFAULT_MAX_BYTES),
                js_number(next_offset)
            )
        });
        details = Some(ReadToolDetails {
            truncation: Some(truncation),
        });
        output_text
    } else if let Some(lines) =
        user_limited_lines.filter(|lines| start_line + lines < total_file_lines as f64)
    {
        // read.ts:306-310 — the user's `limit` stopped early but the file has
        // more content. No `details`.
        let consumed = start_line + lines;
        format!(
            "{}\n\n[{} more lines in file. Use offset={} to continue.]",
            truncation.content,
            js_number(total_file_lines as f64 - consumed),
            js_number(consumed + 1.0)
        )
    } else {
        // read.ts:311-314 — no truncation, nothing left over.
        truncation.content
    };

    Ok(read_result(
        vec![UserContent::Text(TextContent::new(output_text))],
        details,
    ))
}

/// `read.ts:320` — `resolve({ content, details })`, in `AgentToolResult` shape.
/// TS `details: undefined` is `Value::Null` (which is how the oracle recorded
/// it: `"details":null`).
fn read_result(content: Vec<UserContent>, details: Option<ReadToolDetails>) -> AgentToolResult {
    AgentToolResult {
        content,
        details: details.map_or(Value::Null, |details| {
            serde_json::to_value(details).expect("ReadToolDetails is always serializable")
        }),
        added_tool_names: None,
        terminate: None,
    }
}

/// `Array.prototype.slice`'s bound clamping (ECMA-262 §23.1.3.30 steps 4-8) for
/// a `f64` argument over a `len`-element array: truncate toward zero, treat a
/// negative value as an offset from the end, then clamp into `0..=len`.
///
/// Needed because `read.ts:282`/`read.ts:285` slice with values derived from the
/// raw `offset`/`limit`, which the schema only constrains to "number".
fn js_slice_index(relative: f64, len: usize) -> usize {
    let integer = relative.trunc();
    if integer.is_nan() {
        return 0;
    }
    if integer < 0.0 {
        let from_end = len as f64 + integer;
        if from_end < 0.0 {
            0
        } else {
            from_end as usize
        }
    } else if integer >= len as f64 {
        len
    } else {
        integer as usize
    }
}

// ===========================================================================
// utils/mime.ts — image type sniffing
// ===========================================================================

/// Transcription of `utils/mime.ts` — the detection half of the image branch.
///
/// It lives here because `utils/mime.ts` has no module of its own yet and
/// `defaultReadOperations.detectImageMimeType` (`read.ts:55`) is its only
/// caller; move it out when the `utils` port lands. Pure byte inspection, so no
/// dependency is involved — unlike `utils/image-process.ts`, see the module
/// docs' "Gap". Not covered by any captured fixture (the oracle read no images).
pub mod image_mime {
    use pirust_agent_core::types::ToolError;
    use tokio::io::AsyncReadExt;

    use super::node_fs_error;

    /// `mime.ts:3` — how many leading bytes are sniffed.
    const IMAGE_TYPE_SNIFF_BYTES: usize = 4100;

    /// `mime.ts:4` — `PNG_SIGNATURE`.
    const PNG_SIGNATURE: [u8; 8] = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];

    /// `mime.ts:25-34` — `detectSupportedImageMimeTypeFromFile`.
    ///
    /// One `read` of at most [`IMAGE_TYPE_SNIFF_BYTES`] bytes from offset 0, as
    /// Pi's single `fileHandle.read(buffer, 0, IMAGE_TYPE_SNIFF_BYTES, 0)` does;
    /// a short read narrows the sniff window identically in both.
    pub async fn detect_from_file(file_path: &str) -> Result<Option<String>, ToolError> {
        let mut file = tokio::fs::File::open(file_path)
            .await
            .map_err(|error| node_fs_error(&error, "open", file_path))?;
        let mut buffer = vec![0u8; IMAGE_TYPE_SNIFF_BYTES];
        let bytes_read = file
            .read(&mut buffer)
            .await
            .map_err(|error| node_fs_error(&error, "read", file_path))?;
        Ok(detect(&buffer[..bytes_read]).map(str::to_string))
    }

    /// `mime.ts:6-23` — `detectSupportedImageMimeType`.
    pub fn detect(buffer: &[u8]) -> Option<&'static str> {
        if starts_with(buffer, &[0xff, 0xd8, 0xff]) {
            // A 3-byte buffer has no `buffer[3]`, and `undefined !== 0xf7`.
            return if buffer.get(3) == Some(&0xf7) {
                None
            } else {
                Some("image/jpeg")
            };
        }
        if starts_with(buffer, &PNG_SIGNATURE) {
            return if is_png(buffer) && !is_animated_png(buffer) {
                Some("image/png")
            } else {
                None
            };
        }
        if starts_with_ascii(buffer, 0, b"GIF") {
            return Some("image/gif");
        }
        if starts_with_ascii(buffer, 0, b"RIFF") && starts_with_ascii(buffer, 8, b"WEBP") {
            return Some("image/webp");
        }
        if starts_with_ascii(buffer, 0, b"BM") && is_bmp(buffer) {
            return Some("image/bmp");
        }
        None
    }

    /// `mime.ts:36-40` — `isPng`.
    fn is_png(buffer: &[u8]) -> bool {
        buffer.len() >= 16
            && read_u32_be(buffer, PNG_SIGNATURE.len()) == 13
            && starts_with_ascii(buffer, 12, b"IHDR")
    }

    /// `mime.ts:42-55` — `isAnimatedPng`: walk the chunk list until `acTL`
    /// (animated) or `IDAT` (still image) appears.
    fn is_animated_png(buffer: &[u8]) -> bool {
        let mut offset = PNG_SIGNATURE.len();
        while offset + 8 <= buffer.len() {
            let chunk_length = read_u32_be(buffer, offset);
            let chunk_type_offset = offset + 4;
            if starts_with_ascii(buffer, chunk_type_offset, b"acTL") {
                return true;
            }
            if starts_with_ascii(buffer, chunk_type_offset, b"IDAT") {
                return false;
            }

            // `chunk_length` is a u32 read, so this cannot overflow u64/usize on
            // a 64-bit target; the guards below are Pi's, kept as written.
            let next_offset = offset + 8 + chunk_length as usize + 4;
            if next_offset <= offset || next_offset > buffer.len() {
                return false;
            }
            offset = next_offset;
        }
        false
    }

    /// `mime.ts:57-81` — `isBmp`.
    fn is_bmp(buffer: &[u8]) -> bool {
        if buffer.len() < 26 {
            return false;
        }

        let declared_file_size = read_u32_le(buffer, 2);
        let pixel_data_offset = read_u32_le(buffer, 10);
        let dib_header_size = read_u32_le(buffer, 14);
        if declared_file_size != 0 && declared_file_size < 26 {
            return false;
        }
        if pixel_data_offset < 14 + dib_header_size {
            return false;
        }
        if declared_file_size != 0 && pixel_data_offset >= declared_file_size {
            return false;
        }

        let (color_planes, bits_per_pixel) = if dib_header_size == 12 {
            (read_u16_le(buffer, 22), read_u16_le(buffer, 24))
        } else if (40..=124).contains(&dib_header_size) {
            if buffer.len() < 30 {
                return false;
            }
            (read_u16_le(buffer, 26), read_u16_le(buffer, 28))
        } else {
            return false;
        };

        color_planes == 1 && [1, 4, 8, 16, 24, 32].contains(&bits_per_pixel)
    }

    /// `mime.ts:83-85` — missing bytes read as `0` (TS `?? 0`).
    fn read_u16_le(buffer: &[u8], offset: usize) -> u32 {
        u32::from(byte(buffer, offset)) + (u32::from(byte(buffer, offset + 1)) << 8)
    }

    /// `mime.ts:87-94` — `readUint32BE`.
    fn read_u32_be(buffer: &[u8], offset: usize) -> u32 {
        (u32::from(byte(buffer, offset)) << 24)
            | (u32::from(byte(buffer, offset + 1)) << 16)
            | (u32::from(byte(buffer, offset + 2)) << 8)
            | u32::from(byte(buffer, offset + 3))
    }

    /// `mime.ts:96-103` — `readUint32LE`.
    fn read_u32_le(buffer: &[u8], offset: usize) -> u32 {
        u32::from(byte(buffer, offset))
            | (u32::from(byte(buffer, offset + 1)) << 8)
            | (u32::from(byte(buffer, offset + 2)) << 16)
            | (u32::from(byte(buffer, offset + 3)) << 24)
    }

    fn byte(buffer: &[u8], offset: usize) -> u8 {
        buffer.get(offset).copied().unwrap_or(0)
    }

    /// `mime.ts:105-108` — `startsWith`.
    fn starts_with(buffer: &[u8], bytes: &[u8]) -> bool {
        buffer.len() >= bytes.len() && buffer.starts_with(bytes)
    }

    /// `mime.ts:110-116` — `startsWithAscii`.
    fn starts_with_ascii(buffer: &[u8], offset: usize, text: &[u8]) -> bool {
        buffer.len() >= offset + text.len() && &buffer[offset..offset + text.len()] == text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The oracle lives in `tests/read_golden.rs`; these cover the pieces no
    /// captured row reaches — the image-branch strings (no fixture read an
    /// image), the unported [`process_image`] seam, and `slice`'s clamping for
    /// the non-integer arguments the schema admits. They are transcriptions of
    /// the TS literals, not independent expectations.
    fn ok_processed(hints: &[&str]) -> ProcessImageResult {
        ProcessImageResult::Ok {
            data: "QUJD".to_string(),
            mime_type: "image/png".to_string(),
            hints: hints.iter().map(|hint| (*hint).to_string()).collect(),
        }
    }

    fn text_of(content: &UserContent) -> &str {
        match content {
            UserContent::Text(text) => &text.text,
            UserContent::Image(_) => panic!("expected a text block"),
        }
    }

    #[test]
    fn failed_processing_note_carries_the_detected_mime_type() {
        let processed = ProcessImageResult::Failed {
            message: IMAGE_CONVERSION_FAILED_MESSAGE.to_string(),
        };
        let content = image_content(&processed, "image/bmp", None);
        assert_eq!(content.len(), 1);
        assert_eq!(
            text_of(&content[0]),
            format!("Read image file [image/bmp]\n{IMAGE_CONVERSION_FAILED_MESSAGE}")
        );

        // read.ts:253 — the non-vision note is appended, not substituted.
        let content = image_content(&processed, "image/bmp", Some(NON_VISION_IMAGE_NOTE));
        assert_eq!(
            text_of(&content[0]),
            format!(
                "Read image file [image/bmp]\n{IMAGE_CONVERSION_FAILED_MESSAGE}\n{NON_VISION_IMAGE_NOTE}"
            )
        );
    }

    #[test]
    fn successful_processing_emits_note_then_image() {
        // read.ts:256-262 — hints are joined with "\n"; the note uses the
        // *processed* mime type and the image block follows it.
        let content = image_content(
            &ok_processed(&[
                "[Image converted from image/bmp to image/png.]",
                "[Image: …]",
            ]),
            "image/bmp",
            Some(NON_VISION_IMAGE_NOTE),
        );
        assert_eq!(
            text_of(&content[0]),
            format!(
                "Read image file [image/png]\n[Image converted from image/bmp to image/png.]\n[Image: …]\n{NON_VISION_IMAGE_NOTE}"
            )
        );
        match &content[1] {
            UserContent::Image(image) => {
                assert_eq!(image.data, "QUJD");
                assert_eq!(image.mime_type, "image/png");
            }
            UserContent::Text(_) => panic!("expected an image block"),
        }

        // No hints -> no extra newline (read.ts:257).
        let content = image_content(&ok_processed(&[]), "image/png", None);
        assert_eq!(text_of(&content[0]), "Read image file [image/png]");
    }

    #[test]
    fn non_vision_note_only_fires_for_a_text_only_model() {
        // TS `!model` -> undefined: no note (read.ts:88).
        assert_eq!(non_vision_image_note(None), None);
    }

    /// The documented gap: [`process_image`] refuses, loudly, for every input.
    #[tokio::test]
    async fn process_image_is_an_unported_seam() {
        let error = process_image(&[0x89, 0x50], "image/png", true, "/tmp/x.png")
            .await
            .expect_err("image processing is not ported");
        assert_eq!(
            error,
            ReadError::ImageProcessingUnported {
                mime_type: "image/png".to_string(),
                path: "/tmp/x.png".to_string(),
            }
        );
    }

    #[test]
    fn js_slice_index_truncates_and_clamps() {
        assert_eq!(js_slice_index(0.0, 5), 0);
        assert_eq!(js_slice_index(2.9, 5), 2);
        assert_eq!(js_slice_index(9.0, 5), 5);
        // Negative bounds count from the end, then clamp at 0.
        assert_eq!(js_slice_index(-2.0, 5), 3);
        assert_eq!(js_slice_index(-9.0, 5), 0);
    }

    #[test]
    fn image_mime_sniffs_the_five_supported_signatures() {
        // A minimal still PNG: signature + a 13-byte IHDR + an IDAT chunk type.
        let mut png = vec![0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
        png.extend_from_slice(&13u32.to_be_bytes());
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&[0u8; 13]);
        png.extend_from_slice(&[0, 0, 0, 0]); // CRC
        png.extend_from_slice(&0u32.to_be_bytes());
        png.extend_from_slice(b"IDAT");
        assert_eq!(image_mime::detect(&png), Some("image/png"));

        // acTL before IDAT -> animated -> not inlined (mime.ts:11).
        let mut apng = png[..8 + 4 + 4 + 13 + 4].to_vec();
        apng.extend_from_slice(&0u32.to_be_bytes());
        apng.extend_from_slice(b"acTL");
        assert_eq!(image_mime::detect(&apng), None);

        assert_eq!(
            image_mime::detect(&[0xff, 0xd8, 0xff, 0xe0]),
            Some("image/jpeg")
        );
        // 0xf7 is lossless JPEG, which Pi rejects (mime.ts:8).
        assert_eq!(image_mime::detect(&[0xff, 0xd8, 0xff, 0xf7]), None);
        assert_eq!(image_mime::detect(b"GIF89a"), Some("image/gif"));
        assert_eq!(
            image_mime::detect(b"RIFF\x00\x00\x00\x00WEBPVP8 "),
            Some("image/webp")
        );
        assert_eq!(image_mime::detect(b"plain text, not an image"), None);
    }

    #[test]
    fn node_fs_error_reproduces_nodes_message_shape() {
        // The shape fixture row 13 of exec.corpus.jsonl pins.
        let error = node_fs_error(
            &io::Error::from(io::ErrorKind::NotFound),
            "access",
            "C:\\tmp\\files\\nope.txt",
        );
        assert_eq!(
            error.to_string(),
            "ENOENT: no such file or directory, access 'C:\\tmp\\files\\nope.txt'"
        );
    }
}
