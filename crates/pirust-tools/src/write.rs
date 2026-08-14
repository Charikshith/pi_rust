//! Port of `core/tools/write.ts` (UI-free half) — the `write` tool.
//!
//! Gated by `tests/fixtures/pi/tools/{schemas,strings}/write.json` and the `write`
//! rows of `exec.corpus.jsonl`.
//!
//! Only `createWriteToolDefinition`'s data + `execute` (`write.ts:181-226`),
//! `WriteOperations` (`write.ts:25-35`), `WriteToolOptions` (`write.ts:37-40`) and
//! `createWriteTool` (`write.ts:265-267`) are ported. Everything else in that file
//! is TUI — `WriteCallRenderComponent` / the highlight cache
//! (`write.ts:42-121`), `formatWriteCall` (`write.ts:131-162`),
//! `formatWriteResult` (`write.ts:164-179`), `renderCall` / `renderResult`
//! (`write.ts:227-261`) — and lands with feat-006/007.
//!
//! # `${content.length} bytes` is **not** bytes
//!
//! The success string (`write.ts:222`) is
//! `Successfully wrote ${content.length} bytes to ${path}` and every word of it is
//! load-bearing:
//!
//! * `content.length` is a JS string length, i.e. **UTF-16 code units**, despite
//!   the literal word "bytes". The reported number is therefore
//!   `content.encode_utf16().count()`, never `content.len()`. The oracle row for
//!   `out/utf8.txt` writes 19 UTF-8 bytes (`writtenBytes: 19`) and reports
//!   **15** — a `content.len()` port fails that row.
//! * `path` is the **raw argument**, not the resolved absolute path.
//! * there is no trailing period.
//!
//! # No existence check, and the abort check that runs too late
//!
//! `execute` (`write.ts:194-226`) is deliberately unconditional: `WriteOperations`
//! has no `access` op and nothing reads the file first, so a write is always an
//! overwrite with no read-before-write guard.
//!
//! The three `throwIfAborted()` calls sit at `write.ts:212`, `write.ts:215` and
//! `write.ts:219` — the third one **after** `writeFile` has settled. An abort that
//! races the write therefore reports `Operation aborted` even though the file has
//! already been modified. That is not a bug to fix here: the whole body runs
//! inside [`with_file_mutation_queue`], and rejecting from an abort *listener*
//! would release the queue slot while an in-flight filesystem operation could
//! still finish (`write.ts:204-207`, mirrored in
//! [`crate::mutation_queue`]'s "Documented divergences" §2). Polling the token
//! after each await observes the same aborts while keeping the slot locked.
//! `tests::aborting_during_the_write_reports_failure_after_the_file_changed`
//! pins the ordering; the captured corpus cannot, since all six rows are
//! `ok: true`.
//!
//! # Errors
//!
//! `mkdir` / `writeFile` rejections propagate unwrapped (EACCES, EISDIR, …) —
//! there is no `try`/`catch` anywhere in `execute`.

use std::sync::Arc;

use async_trait::async_trait;
use pirust_agent_core::types::{AgentTool, AgentToolResult, AgentToolUpdateCallback, ToolError};
use pirust_ai::types::{TextContent, UserContent};
use serde::Deserialize;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::definition::schema::{object_schema, required, string_prop};
use crate::definition::PirustToolDefinition;
use crate::mutation_queue::with_file_mutation_queue;
use crate::path_utils::{resolve_to_cwd, Platform};

// ===========================================================================
// Static tool data (write.ts:14-17, write.ts:187-193)
// ===========================================================================

/// `name` / `label` (`write.ts:187-188`) — the same string for this tool.
const WRITE_NAME: &str = "write";

/// `description` (`write.ts:189-190`).
const WRITE_DESCRIPTION: &str = "Write content to a file. Creates the file if it doesn't exist, overwrites if it does. Automatically creates parent directories.";

/// `promptSnippet` (`write.ts:191`).
const WRITE_PROMPT_SNIPPET: &str = "Create or overwrite files";

/// The single `promptGuidelines` bullet (`write.ts:192`).
const WRITE_PROMPT_GUIDELINE: &str = "Use write only for new files or complete rewrites.";

/// `writeSchema` (`write.ts:14-17`), built through the TypeBox-key-order helpers
/// so the bytes match `tests/fixtures/pi/tools/schemas/write.json`.
fn write_parameters() -> Value {
    object_schema([
        required(
            "path",
            string_prop("Path to the file to write (relative or absolute)"),
        ),
        required("content", string_prop("Content to write to the file")),
    ])
}

/// `Static<typeof writeSchema>` (`write.ts:19`).
///
/// Deviation, forced by Rust: Pi's `execute` destructures the already-validated
/// argument object (`write.ts:196`), so a malformed payload would surface as a
/// `TypeError` from inside the body. Here the same payload fails to deserialize
/// and the `serde_json` error surfaces instead. Unobservable in practice — the
/// agent loop validates against [`write_parameters`] before calling `execute` —
/// but the error text differs on that path.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct WriteToolInput {
    /// Path to the file to write (relative or absolute).
    pub path: String,
    /// Content to write to the file.
    pub content: String,
}

// ===========================================================================
// Pluggable operations (write.ts:25-35)
// ===========================================================================

/// `new Error("Operation aborted")` (`write.ts:209`; `edit.ts` throws the
/// identical message).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("Operation aborted")]
pub struct OperationAborted;

/// Pluggable operations for the write tool (`write.ts:25-30`).
///
/// Override these to delegate file writing to remote systems (for example SSH).
/// Note what is *absent*: there is no `access` / `exists` op, because `execute`
/// never checks whether the target exists (see the module docs).
///
/// Both methods return [`ToolError`] rather than `io::Error` so a non-local
/// implementation can reject with its own error type, exactly as Pi's
/// `Promise<void>` can reject with anything; either way the error propagates out
/// of `execute` unwrapped.
#[async_trait]
pub trait WriteOperations: Send + Sync {
    /// Write content to a file (`write.ts:27`).
    async fn write_file(&self, absolute_path: &str, content: &str) -> Result<(), ToolError>;

    /// Create directory recursively (`write.ts:29`).
    async fn mkdir(&self, dir: &str) -> Result<(), ToolError>;
}

/// `defaultWriteOperations` (`write.ts:32-35`): the local filesystem.
#[derive(Debug, Clone, Copy, Default)]
pub struct LocalWriteOperations;

#[async_trait]
impl WriteOperations for LocalWriteOperations {
    /// `fsWriteFile(path, content, "utf-8")` (`write.ts:33`) — `&str` is already
    /// UTF-8, so the encoding argument needs no counterpart.
    async fn write_file(&self, absolute_path: &str, content: &str) -> Result<(), ToolError> {
        tokio::fs::write(absolute_path, content).await?;
        Ok(())
    }

    /// `fsMkdir(dir, { recursive: true })` (`write.ts:34`) — like Node's
    /// recursive `mkdir`, [`tokio::fs::create_dir_all`] succeeds when the
    /// directory already exists.
    async fn mkdir(&self, dir: &str) -> Result<(), ToolError> {
        tokio::fs::create_dir_all(dir).await?;
        Ok(())
    }
}

/// `WriteToolOptions` (`write.ts:37-40`).
#[derive(Clone, Default)]
pub struct WriteToolOptions {
    /// Custom operations for file writing. Default: local filesystem
    /// ([`LocalWriteOperations`]).
    pub operations: Option<Arc<dyn WriteOperations>>,
}

impl std::fmt::Debug for WriteToolOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WriteToolOptions")
            .field("operations", &self.operations.as_ref().map(|_| "<ops>"))
            .finish()
    }
}

// ===========================================================================
// node:path dirname (write.ts:4, write.ts:202)
// ===========================================================================

/// win32 `isPathSeparator` (Node `lib/path.js`).
fn is_path_separator_win32(byte: u8) -> bool {
    byte == b'/' || byte == b'\\'
}

/// Node's `isWindowsDeviceRoot`: an ASCII letter, i.e. the `C` of `C:\`.
fn is_windows_device_root(byte: u8) -> bool {
    byte.is_ascii_alphabetic()
}

/// Lexical port of Node's `path.win32.dirname` (`lib/path.js`).
///
/// Byte indexing rather than Pi's UTF-16 indexing is exact here: every index this
/// function computes lands on an ASCII separator, on the `:` of a device root, or
/// on the end of the string, so no slice can split a multi-byte character and no
/// comparison can be fooled by a continuation byte.
fn win32_dirname(path: &str) -> String {
    let bytes = path.as_bytes();
    let len = bytes.len();
    if len == 0 {
        return ".".to_string();
    }

    let mut root_end: Option<usize> = None;
    let mut offset = 0usize;
    let code = bytes[0];

    // A lone path separator is its own dirname; anything else of length 1 is ".".
    if len == 1 {
        return if is_path_separator_win32(code) {
            path.to_string()
        } else {
            ".".to_string()
        };
    }

    if is_path_separator_win32(code) {
        // Possible UNC root.
        root_end = Some(1);
        offset = 1;

        if is_path_separator_win32(bytes[1]) {
            let mut j = 2usize;
            let mut last = j;
            while j < len && !is_path_separator_win32(bytes[j]) {
                j += 1;
            }
            if j < len && j != last {
                last = j;
                while j < len && is_path_separator_win32(bytes[j]) {
                    j += 1;
                }
                if j < len && j != last {
                    last = j;
                    while j < len && !is_path_separator_win32(bytes[j]) {
                        j += 1;
                    }
                    if j == len {
                        // A UNC root only.
                        return path.to_string();
                    }
                    if j != last {
                        // UNC root with leftovers: treat the separator after the
                        // root as a "normal root" on top of the UNC root.
                        root_end = Some(j + 1);
                        offset = j + 1;
                    }
                }
            }
        }
    } else if is_windows_device_root(code) && bytes[1] == b':' {
        root_end = Some(if len > 2 && is_path_separator_win32(bytes[2]) {
            3
        } else {
            2
        });
        offset = root_end.unwrap_or(0);
    }

    let mut end: Option<usize> = None;
    let mut matched_slash = true;
    let mut i = len;
    while i > offset {
        i -= 1;
        if is_path_separator_win32(bytes[i]) {
            if !matched_slash {
                end = Some(i);
                break;
            }
        } else {
            matched_slash = false;
        }
    }

    let end = match end.or(root_end) {
        Some(end) => end,
        None => return ".".to_string(),
    };
    path[..end].to_string()
}

/// Lexical port of Node's `path.posix.dirname` (`lib/path.js`). Same
/// byte-vs-UTF-16 argument as [`win32_dirname`].
fn posix_dirname(path: &str) -> String {
    let bytes = path.as_bytes();
    if bytes.is_empty() {
        return ".".to_string();
    }

    let has_root = bytes[0] == b'/';
    let mut end: Option<usize> = None;
    let mut matched_slash = true;
    let mut i = bytes.len();
    // Node scans down to index 1, never 0: a leading slash is the root, not a
    // separator that can be cut.
    while i > 1 {
        i -= 1;
        if bytes[i] == b'/' {
            if !matched_slash {
                end = Some(i);
                break;
            }
        } else {
            matched_slash = false;
        }
    }

    match end {
        None => {
            if has_root {
                "/".to_string()
            } else {
                ".".to_string()
            }
        }
        // `//foo` keeps both slashes, as Node does.
        Some(1) if has_root => "//".to_string(),
        Some(end) => path[..end].to_string(),
    }
}

/// `dirname` from `node:path` (`write.ts:4`), dispatched on `process.platform`
/// the way `node:path`'s default export is.
fn dirname(path: &str) -> String {
    match Platform::current() {
        Platform::Win32 => win32_dirname(path),
        Platform::Posix => posix_dirname(path),
    }
}

// ===========================================================================
// execute (write.ts:194-226)
// ===========================================================================

/// `throwIfAborted` (`write.ts:208-210`).
fn throw_if_aborted(token: &CancellationToken) -> Result<(), OperationAborted> {
    if token.is_cancelled() {
        return Err(OperationAborted);
    }
    Ok(())
}

/// `${content.length}` — UTF-16 code units, *not* bytes. See the module docs.
fn written_units(content: &str) -> usize {
    content.encode_utf16().count()
}

/// The body of `execute` (`write.ts:194-226`), in Pi's exact order: resolve the
/// path, take its `dirname`, then run everything else inside the file mutation
/// queue.
async fn execute_write(
    cwd: &str,
    ops: &dyn WriteOperations,
    args: Value,
    token: &CancellationToken,
) -> Result<AgentToolResult, ToolError> {
    let WriteToolInput { path, content } = serde_json::from_value(args)?;

    // write.ts:201-202 — outside the queue, and `absolutePath` is *not* what the
    // success message reports.
    let absolute_path = resolve_to_cwd(&path, cwd)?;
    let dir = dirname(&absolute_path);

    // write.ts:203 — the whole body, so a concurrent edit/write of the same file
    // cannot interleave.
    with_file_mutation_queue(absolute_path.clone(), || async {
        throw_if_aborted(token)?; // write.ts:212

        // write.ts:214 — create parent directories if needed.
        ops.mkdir(&dir).await?;
        throw_if_aborted(token)?; // write.ts:215

        // write.ts:218 — write the file contents.
        ops.write_file(&absolute_path, &content).await?;
        // write.ts:219 — after the write: an abort racing it reports failure even
        // though the file has already changed.
        throw_if_aborted(token)?;

        // write.ts:221-224. `details: undefined` becomes `Value::Null`, which is
        // what the corpus records (`"details":null`).
        Ok(AgentToolResult {
            content: vec![UserContent::Text(TextContent::new(format!(
                "Successfully wrote {} bytes to {}",
                written_units(&content),
                path
            )))],
            details: Value::Null,
            added_tool_names: None,
            terminate: None,
        })
    })
    .await
}

// ===========================================================================
// Factories (write.ts:181-186, write.ts:265-267)
// ===========================================================================

/// `createWriteToolDefinition(cwd, options)` (`write.ts:181-263`), minus the
/// renderers.
///
/// `cwd` and the resolved [`WriteOperations`] are captured by the `execute`
/// closure, which is what Pi's `ops` binding (`write.ts:185`) and
/// `wrapToolDefinition`'s `ctxFactory` do between them.
pub fn create_write_tool_definition(
    cwd: &str,
    options: Option<WriteToolOptions>,
) -> PirustToolDefinition {
    // write.ts:185 — `options?.operations ?? defaultWriteOperations`.
    let ops: Arc<dyn WriteOperations> = options
        .and_then(|options| options.operations)
        .unwrap_or_else(|| Arc::new(LocalWriteOperations));
    let cwd = cwd.to_string();

    PirustToolDefinition::new(
        WRITE_NAME,
        WRITE_NAME,
        WRITE_DESCRIPTION,
        write_parameters(),
        move |_tool_call_id: String,
              args: Value,
              token: CancellationToken,
              _on_update: AgentToolUpdateCallback| {
            let cwd = cwd.clone();
            let ops = Arc::clone(&ops);
            async move { execute_write(&cwd, ops.as_ref(), args, &token).await }
        },
    )
    .with_prompt_snippet(WRITE_PROMPT_SNIPPET)
    .with_prompt_guidelines([WRITE_PROMPT_GUIDELINE])
}

/// `createWriteTool(cwd, options)` (`write.ts:265-267`).
///
/// Pi calls `wrapToolDefinition`; here [`PirustToolDefinition`] *is* the wrapper
/// (see [`crate::definition`]), so this is only the erasure to `Arc<dyn
/// AgentTool>`.
pub fn create_write_tool(cwd: &str, options: Option<WriteToolOptions>) -> Arc<dyn AgentTool> {
    Arc::new(create_write_tool_definition(cwd, options))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Mutex;

    /// Records every operation with its arguments, and optionally cancels a token
    /// from inside `write_file` — the only way to force the abort-after-write
    /// interleaving deterministically.
    struct RecordingOps {
        calls: Arc<Mutex<Vec<String>>>,
        cancel_during_write: Option<CancellationToken>,
    }

    impl RecordingOps {
        /// Not `new`: the log has to be shared with the caller.
        fn pair(
            cancel_during_write: Option<CancellationToken>,
        ) -> (Arc<Self>, Arc<Mutex<Vec<String>>>) {
            let calls = Arc::new(Mutex::new(Vec::new()));
            (
                Arc::new(Self {
                    calls: Arc::clone(&calls),
                    cancel_during_write,
                }),
                calls,
            )
        }
    }

    #[async_trait]
    impl WriteOperations for RecordingOps {
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

        async fn mkdir(&self, dir: &str) -> Result<(), ToolError> {
            self.calls.lock().unwrap().push(format!("mkdir({dir})"));
            Ok(())
        }
    }

    fn noop_update() -> AgentToolUpdateCallback {
        Arc::new(|_| {})
    }

    fn entries(calls: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
        calls.lock().unwrap().clone()
    }

    /// `write.ts:219`: the third `throwIfAborted()` runs *after* `writeFile`, so
    /// an abort that lands during the write reports failure even though the file
    /// has already been modified. Not expressible in the captured corpus (all six
    /// rows are `ok: true`), hence pinned here.
    #[tokio::test]
    async fn aborting_during_the_write_reports_failure_after_the_file_changed() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().to_str().unwrap();
        let token = CancellationToken::new();
        let (ops, calls) = RecordingOps::pair(Some(token.clone()));

        let error = AgentTool::execute(
            &create_write_tool_definition(
                cwd,
                Some(WriteToolOptions {
                    operations: Some(ops),
                }),
            ),
            "call_abort_during_write",
            json!({ "path": "raced.txt", "content": "hi" }),
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
                format!("mkdir({})", dirname(&expected_path)),
                format!("write_file({expected_path}, hi)"),
            ]
        );
    }

    /// `write.ts:212`: the first `throwIfAborted()` precedes `mkdir`, so an
    /// already-aborted call touches nothing.
    #[tokio::test]
    async fn aborting_before_mkdir_runs_no_operations() {
        let dir = tempfile::tempdir().unwrap();
        let token = CancellationToken::new();
        token.cancel();
        let (ops, calls) = RecordingOps::pair(None);

        let error = AgentTool::execute(
            &create_write_tool_definition(
                dir.path().to_str().unwrap(),
                Some(WriteToolOptions {
                    operations: Some(ops),
                }),
            ),
            "call_abort_before_mkdir",
            json!({ "path": "untouched.txt", "content": "hi" }),
            token,
            noop_update(),
        )
        .await
        .expect_err("an already-aborted call must fail");

        assert_eq!(error.to_string(), "Operation aborted");
        assert!(entries(&calls).is_empty(), "{:?}", entries(&calls));
    }

    /// `${content.length}` counts UTF-16 code units. The corpus proves this for
    /// one payload; this guards the helper itself against a `len()` "fix".
    #[test]
    fn written_units_counts_utf16_code_units_not_bytes() {
        // Corpus row `out/utf8.txt`: reported 15, `writtenBytes` 19.
        assert_eq!(written_units("héllo wörld 👍\n"), 15);
        assert_eq!("héllo wörld 👍\n".len(), 19);
        // An astral char is 2 units but 4 bytes; a BMP non-ASCII char 1 vs 2.
        assert_eq!(written_units("👍"), 2);
        assert_eq!(written_units("é"), 1);
        assert_eq!(written_units(""), 0);
    }

    /// Node's `path.dirname` semantics relied on at `write.ts:202`. Values are
    /// read off Node's `lib/path.js` algorithm, which both helpers port literally.
    #[test]
    fn dirname_matches_node_path_dirname() {
        assert_eq!(posix_dirname("/a/b/c.txt"), "/a/b");
        assert_eq!(posix_dirname("/a/b/"), "/a");
        assert_eq!(posix_dirname("/a"), "/");
        assert_eq!(posix_dirname("/"), "/");
        assert_eq!(posix_dirname("//a"), "//");
        assert_eq!(posix_dirname("a.txt"), ".");
        assert_eq!(posix_dirname(""), ".");

        assert_eq!(win32_dirname(r"C:\a\b\c.txt"), r"C:\a\b");
        assert_eq!(win32_dirname(r"C:\a"), r"C:\");
        assert_eq!(win32_dirname(r"C:\"), r"C:\");
        assert_eq!(win32_dirname("C:a"), "C:");
        assert_eq!(win32_dirname(r"\\server\share"), r"\\server\share");
        assert_eq!(
            win32_dirname(r"\\server\share\dir\f.txt"),
            r"\\server\share\dir"
        );
        assert_eq!(win32_dirname(r"\a\b"), r"\a");
        assert_eq!(win32_dirname(r"\"), r"\");
        assert_eq!(win32_dirname("a.txt"), ".");
        assert_eq!(win32_dirname(""), ".");
        // Forward slashes are separators on win32 too.
        assert_eq!(win32_dirname("C:/a/b/c.txt"), "C:/a/b");
    }

    /// The default operations really are the local filesystem, and `mkdir` is
    /// recursive (`write.ts:34`) — so a missing parent chain is created and the
    /// bytes on disk are the UTF-8 encoding of `content`.
    #[tokio::test]
    async fn default_operations_create_parents_and_write_utf8() {
        let dir = tempfile::tempdir().unwrap();
        let result = AgentTool::execute(
            &create_write_tool_definition(dir.path().to_str().unwrap(), None),
            "call_default_ops",
            json!({ "path": "x/y/z.txt", "content": "é\n" }),
            CancellationToken::new(),
            noop_update(),
        )
        .await
        .expect("write should succeed");

        assert_eq!(
            serde_json::to_string(&result.content).unwrap(),
            r#"[{"type":"text","text":"Successfully wrote 2 bytes to x/y/z.txt"}]"#
        );
        assert_eq!(
            std::fs::read(dir.path().join("x/y/z.txt")).unwrap(),
            "é\n".as_bytes()
        );
    }
}
