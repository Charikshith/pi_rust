//! Diff previews for file-editing tool calls (`write` / `edit`) in the
//! interactive TUI.
//!
//! Per `docs/tui-design-samples.html`: "Preview diffs before writes, identify
//! changed paths, allow reject/accept, and never hide partial failure." Today
//! `interactive_mode.rs`'s `ToolExecutionComponent::format_tool_execution`
//! falls back to `serde_json::to_string_pretty(&self.args)` for every tool,
//! including `write`/`edit`, so an approval prompt or a completed edit shows
//! raw JSON (`{"path": "...", "content": "...a 400-line file..."}`) instead of
//! a diff. This module builds the diff renderer; wiring it into
//! `interactive_mode.rs` (calling [`parse_file_change`] and swapping in a
//! [`DiffPreview`] when it returns `Some`) is a separate change.
//!
//! # Reuse, not reimplementation
//!
//! The actual line-diff algorithm is **not** reimplemented here. It is
//! reused from `pirust_tools::edit_diff`, a byte-parity port of Pi's own
//! `edit-diff.ts` (itself a literal port of npm `diff`/jsdiff 8.0.4's Myers
//! diff, gated by a 56-case golden corpus — see
//! `crates/pirust-tools/tests/edit_diff_golden.rs` and the module docs at
//! `crates/pirust-tools/src/edit_diff.rs:1-56`). Specifically this module
//! calls:
//! - [`generate_unified_patch`] (`edit_diff.rs:753`) for the diff text
//!   itself — chosen over `generate_diff_string` (`edit_diff.rs:775`)
//!   because only the unified-patch form has `@@ -a,b +c,d @@` hunk headers,
//!   which the design spec asks for.
//! - [`apply_edits_to_normalized_content`] (`edit_diff.rs:646`) to compute
//!   the post-edit content for an `edit` tool call, so its diff is against
//!   what the tool would *actually* produce (fuzzy matching included), not a
//!   naive text substitution.
//!
//! That algorithm is Myers' O((N+M)·D) diff (D = edit distance), not the
//! O(N·M) textbook LCS matrix, so no additional complexity work is needed
//! here — only a size gate (below) to avoid ever handing it a pathological
//! multi-megabyte file in a preview context where nobody reads a
//! 40,000-line diff before approving a tool call anyway.
//!
//! # Tool shapes (from `pirust-tools` source, not assumed)
//! - `write` (`crates/pirust-tools/src/write.rs`, `WRITE_NAME = "write"`):
//!   `{ "path": string, "content": string }` — always a full overwrite/create.
//! - `edit` (`crates/pirust-tools/src/edit.rs`, `EDIT_NAME = "edit"`):
//!   `{ "path": string, "edits": [{ "oldText": string, "newText": string }] }`,
//!   with legacy shims (a JSON-encoded string `edits`, or flat top-level
//!   `oldText`/`newText`) normalized by `edit.rs`'s `prepare_edit_arguments`
//!   before the tool executes. [`parse_file_change`] defensively accepts all
//!   three shapes itself, since it is not certain whether the
//!   `ToolExecutionStart` args this module will actually observe have
//!   already been through that normalization.
//!
//! # No filesystem I/O (with one clearly-named exception)
//! Every function in this module except [`read_old_content_from_disk`] is
//! pure: given `serde_json::Value` args and (optionally) old file content as
//! a `&str`, it returns text/lines. That is what makes it unit-testable
//! without a temp directory. Reading the real file to obtain "old content"
//! for a true diff is the caller's job — see [`FileChange::with_old_content`].

use pirust_tools::edit_diff::{
    apply_edits_to_normalized_content, generate_unified_patch, normalize_to_lf, Edit,
};
use pirust_tui::tui::Component;
use pirust_tui::utils::truncate_to_width;
use serde_json::Value;

use crate::interactive_theme::{dark, fg};

/// Local diff colors. `interactive_theme::dark` (feat-007 Wave 3) has no
/// diff-specific colors yet (`TEXT`/`GRAY`/`TOOL_*_BG`/`USER_MESSAGE_BG`
/// only — confirmed by inspection), and this module may not add to that
/// file, so added/removed/hunk colors live here instead. Chosen close to the
/// ANSI bright-green/bright-red terminals already use for `git diff`, so a
/// diff preview reads the same way conventionally.
mod colors {
    /// Added-line green.
    pub const ADDED: &str = "#4fbf67";
    /// Removed-line red.
    pub const REMOVED: &str = "#e5534b";
    /// `@@ -a,b +c,d @@` hunk-header blue — visually distinct from both the
    /// added/removed coloring and the dim context/gray text.
    pub const HUNK: &str = "#4a9eda";
}

/// Bytes, per side, above which this module refuses to hand content to the
/// real diff engine and summarizes instead.
///
/// The underlying algorithm ([`generate_unified_patch`]) is Myers diff, not
/// O(N·M) LCS, so this cap is not a memory-safety necessity — it exists
/// because a diff *preview* has a different job than a diff *engine*: a
/// tool call can legitimately write a multi-megabyte generated file (a
/// vendored lockfile, a bundled asset) where even a tiny logical change
/// produces a diff no human reviews line-by-line before approving. 512 KiB
/// is comfortably above any hand-authored source file a reviewer reads
/// start-to-end, and comfortably below where computing *and rendering* a
/// preview would stop feeling instantaneous inside the render loop.
pub const MAX_DIFF_INPUT_BYTES: usize = 512 * 1024;

/// Lines shown in [`DiffPreview`]'s collapsed state: a summary line plus a
/// handful of hunk lines, matching the design spec's "identify changed
/// paths" — enough to orient, not enough to replace expanding.
const COLLAPSED_MAX_LINES: usize = 6;

/// Lines shown in [`DiffPreview`]'s expanded state. Still bounded — an
/// approval prompt that scrolls the whole file off screen defeats its own
/// purpose — but generous enough for the diffs a human actually reviews.
const EXPANDED_MAX_LINES: usize = 200;

/// What kind of file mutation a parsed tool call represents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileChangeKind {
    /// A `write` tool call: full file create/overwrite. There is no notion
    /// of a partial change — the entire file becomes `content`.
    Write {
        /// The full new file content, verbatim from the tool args.
        content: String,
    },
    /// An `edit` tool call: one or more disjoint `oldText` → `newText`
    /// replacements applied against the file's *current* on-disk content,
    /// which this module never reads on its own (see
    /// [`FileChange::with_old_content`]).
    Edit {
        /// The edits, in the order the model requested them (matching
        /// happens against a shared base and is order-independent for
        /// non-overlapping edits, but display order follows request order).
        edits: Vec<Edit>,
    },
}

/// A parsed file-editing tool call, ready to render as a diff.
///
/// `old_content` starts `None` (this module never touches disk). A caller
/// that read the target file — or captured it before the tool ran — can
/// attach it with [`FileChange::with_old_content`] to turn a "new file" /
/// "synthetic before-after" preview into a true diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    /// The (unresolved) path as passed to the tool.
    pub path: String,
    /// What kind of mutation this is.
    pub kind: FileChangeKind,
    /// The file's content before this tool call, if known. `None` for a
    /// `write` renders as an all-`+` "new file" diff; `None` for an `edit`
    /// renders a synthetic before/after view of the raw `oldText`/`newText`
    /// pairs, since there is no base content to apply them to.
    pub old_content: Option<String>,
}

impl FileChange {
    /// Attach the file's real current content so the diff is computed
    /// against it instead of being synthesized. Reading the file is the
    /// caller's job (see [`read_old_content_from_disk`]) — this is a pure
    /// setter.
    pub fn with_old_content(mut self, old: impl Into<String>) -> Self {
        self.old_content = Some(old.into());
        self
    }
}

/// Parse a tool call's name + args into a [`FileChange`], or `None` if this
/// tool is not a file-editing tool (or its args don't match the expected
/// shape) — the caller falls back to the plain JSON view in that case.
///
/// Covers the two real file-mutating tools in `pirust-tools`: `write`
/// (`crates/pirust-tools/src/write.rs`) and `edit`
/// (`crates/pirust-tools/src/edit.rs`), including `edit`'s legacy arg shims
/// (see module docs).
pub fn parse_file_change(tool_name: &str, args: &Value) -> Option<FileChange> {
    match tool_name {
        "write" => {
            let path = args.get("path")?.as_str()?.to_string();
            let content = args.get("content")?.as_str()?.to_string();
            Some(FileChange {
                path,
                kind: FileChangeKind::Write { content },
                old_content: None,
            })
        }
        "edit" => {
            let path = args.get("path")?.as_str()?.to_string();
            let edits = parse_edits(args)?;
            if edits.is_empty() {
                return None;
            }
            Some(FileChange {
                path,
                kind: FileChangeKind::Edit { edits },
                old_content: None,
            })
        }
        _ => None,
    }
}

/// Parse `edit`'s `edits` argument in every shape `edit.rs`'s
/// `prepare_edit_arguments` normalizes: the canonical array of
/// `{oldText, newText}`, a JSON-encoded string of the same, or flat
/// top-level `oldText`/`newText` (single-edit legacy form). `None` if none
/// of these shapes match.
fn parse_edits(args: &Value) -> Option<Vec<Edit>> {
    if let Some(arr) = args.get("edits").and_then(Value::as_array) {
        return parse_edit_array(arr);
    }
    if let Some(s) = args.get("edits").and_then(Value::as_str) {
        if let Ok(Value::Array(arr)) = serde_json::from_str::<Value>(s) {
            return parse_edit_array(&arr);
        }
    }
    if let (Some(old_text), Some(new_text)) = (
        args.get("oldText").and_then(Value::as_str),
        args.get("newText").and_then(Value::as_str),
    ) {
        return Some(vec![Edit {
            old_text: old_text.to_string(),
            new_text: new_text.to_string(),
        }]);
    }
    None
}

fn parse_edit_array(arr: &[Value]) -> Option<Vec<Edit>> {
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let old_text = item.get("oldText")?.as_str()?.to_string();
        let new_text = item.get("newText")?.as_str()?.to_string();
        out.push(Edit { old_text, new_text });
    }
    Some(out)
}

/// The body of a computed diff: either real hunk lines (each already
/// prefixed with jsdiff's marker — `"+"`, `"-"`, `" "`, `"\\"`, or `"@@ "`)
/// ready for per-line coloring, or a single free-text explanation when a
/// real diff couldn't be computed or wasn't attempted.
enum DiffContent {
    Lines(Vec<String>),
    Message(String),
}

/// Everything [`render_diff_lines`] needs to render the header + body.
struct DiffData {
    added: usize,
    removed: usize,
    /// A short parenthetical shown next to the path, e.g. `"new file"` or
    /// an error summary — never silently dropped (design spec: "never hide
    /// partial failure").
    label: Option<String>,
    content: DiffContent,
}

/// Count added/removed lines in a unified-patch body (post file-header
/// lines). `"@@ "` hunk headers and `"\\"` no-newline markers don't count
/// either way.
fn count_changes(lines: &[String]) -> (usize, usize) {
    let mut added = 0usize;
    let mut removed = 0usize;
    for line in lines {
        if line.starts_with("@@ ") || line.starts_with('\\') {
            continue;
        }
        match line.as_bytes().first() {
            Some(b'+') => added += 1,
            Some(b'-') => removed += 1,
            _ => {}
        }
    }
    (added, removed)
}

/// Split a [`generate_unified_patch`] result into its body lines, dropping
/// the two fixed `"--- path"` / `"+++ path"` file-header lines
/// (`edit_diff.rs`'s `format_patch` always emits exactly these two first —
/// see `crates/pirust-tools/src/edit_diff.rs:1411-1412` — so a positional
/// skip is exact, not a prefix guess that a real `+`/`-`/`@@`-prefixed
/// content line could ever collide with).
fn patch_body_lines(patch: &str) -> Vec<String> {
    patch.lines().skip(2).map(str::to_string).collect()
}

fn oversized_summary(old_len: usize, new_len: usize) -> String {
    format!(
        "content too large to diff ({old_len} + {new_len} bytes exceeds the {MAX_DIFF_INPUT_BYTES}-byte-per-side cap) — showing counts only"
    )
}

/// Build a synthetic before/after view straight from `edits` (no base
/// content to diff against): each edit's `oldText` renders as removed
/// lines, its `newText` as added lines, with an `"@@ edit[i] @@"` separator
/// when there's more than one. Reuses the exact same per-line prefix
/// convention (`+`/`-`/`@@ `) as a real unified patch so [`render_diff_lines`]
/// can color both the same way.
fn synthetic_edit_lines(edits: &[Edit]) -> (usize, usize, Vec<String>) {
    let mut lines: Vec<String> = Vec::with_capacity(edits.len() * 4);
    let mut added = 0usize;
    let mut removed = 0usize;
    for (i, edit) in edits.iter().enumerate() {
        if edits.len() > 1 {
            lines.push(format!("@@ edit[{i}] @@"));
        }
        for line in edit.old_text.lines() {
            lines.push(format!("-{line}"));
            removed += 1;
        }
        for line in edit.new_text.lines() {
            lines.push(format!("+{line}"));
            added += 1;
        }
    }
    (added, removed, lines)
}

/// Compute the diff data for a [`FileChange`]: real unified-patch hunks when
/// possible, a synthetic before/after view when there's no base content to
/// diff against, or a plain summary when either side is too large (see
/// [`MAX_DIFF_INPUT_BYTES`]) or applying the edits failed.
fn compute_diff_data(change: &FileChange) -> DiffData {
    match &change.kind {
        FileChangeKind::Write { content } => {
            let old = change.old_content.as_deref().unwrap_or("");
            let label = if change.old_content.is_none() {
                Some("new file".to_string())
            } else {
                None
            };
            if old.len() > MAX_DIFF_INPUT_BYTES || content.len() > MAX_DIFF_INPUT_BYTES {
                return DiffData {
                    added: 0,
                    removed: 0,
                    label,
                    content: DiffContent::Message(oversized_summary(old.len(), content.len())),
                };
            }
            let patch = generate_unified_patch(&change.path, old, content);
            let lines = patch_body_lines(&patch);
            let (added, removed) = count_changes(&lines);
            DiffData {
                added,
                removed,
                label,
                content: DiffContent::Lines(lines),
            }
        }
        FileChangeKind::Edit { edits } => match &change.old_content {
            None => {
                let (added, removed, lines) = synthetic_edit_lines(edits);
                DiffData {
                    added,
                    removed,
                    label: Some("preview — old content unknown".to_string()),
                    content: DiffContent::Lines(lines),
                }
            }
            Some(old) => {
                if old.len() > MAX_DIFF_INPUT_BYTES {
                    return DiffData {
                        added: 0,
                        removed: 0,
                        label: None,
                        content: DiffContent::Message(oversized_summary(old.len(), 0)),
                    };
                }
                let normalized_old = normalize_to_lf(old);
                match apply_edits_to_normalized_content(&normalized_old, edits, &change.path) {
                    Ok(applied) if applied.new_content.len() <= MAX_DIFF_INPUT_BYTES => {
                        let patch = generate_unified_patch(
                            &change.path,
                            &applied.base_content,
                            &applied.new_content,
                        );
                        let lines = patch_body_lines(&patch);
                        let (added, removed) = count_changes(&lines);
                        DiffData {
                            added,
                            removed,
                            label: None,
                            content: DiffContent::Lines(lines),
                        }
                    }
                    Ok(applied) => DiffData {
                        added: 0,
                        removed: 0,
                        label: None,
                        content: DiffContent::Message(oversized_summary(
                            normalized_old.len(),
                            applied.new_content.len(),
                        )),
                    },
                    Err(e) => {
                        // Never hide partial failure: fall back to the
                        // synthetic view AND say why the real diff failed.
                        let (added, removed, lines) = synthetic_edit_lines(edits);
                        DiffData {
                            added,
                            removed,
                            label: Some(format!("preview — could not apply edits: {e}")),
                            content: DiffContent::Lines(lines),
                        }
                    }
                }
            }
        },
    }
}

fn render_header(
    path: &str,
    added: usize,
    removed: usize,
    label: Option<&str>,
    width: usize,
) -> String {
    let path_part = fg(dark::TEXT)(path);
    let added_part = fg(colors::ADDED)(&format!("+{added}"));
    let removed_part = fg(colors::REMOVED)(&format!("-{removed}"));
    let mut header = format!("{path_part} \u{b7} {added_part} {removed_part}");
    if let Some(l) = label {
        header.push_str(&fg(dark::GRAY)(&format!(" ({l})")));
    }
    truncate_to_width(&header, width, "\u{2026}", true)
}

/// Color one already-prefixed body line by its jsdiff marker, then clip it
/// to `width` (a diff preview trades wrapping for a stable one-line-per-diff-line
/// layout — matches how `+N -M` counts read against what's actually shown).
fn render_body_line(raw: &str, width: usize) -> String {
    let colored = if raw.starts_with("@@ ") {
        fg(colors::HUNK)(raw)
    } else if raw.starts_with('+') {
        fg(colors::ADDED)(raw)
    } else if raw.starts_with('-') {
        fg(colors::REMOVED)(raw)
    } else {
        // Context lines (leading space) and "\ No newline at end of file".
        fg(dark::GRAY)(raw)
    };
    truncate_to_width(&colored, width, "\u{2026}", true)
}

/// Render a [`FileChange`] as colored diff lines: a header line
/// (`path \u{b7} +N -M`, plus a parenthetical when relevant) followed by up
/// to `max_lines` colored hunk lines, truncated with a
/// `"\u{2026} (N more lines)"` trailer when the real diff is longer.
///
/// `+` lines render green, `-` red, `@@ ...` hunk headers blue, everything
/// else (context, no-newline markers) dim gray. Every line is clipped to
/// `width` — a long line can never destroy the surrounding layout.
pub fn render_diff_lines(change: &FileChange, width: usize, max_lines: usize) -> Vec<String> {
    let width = width.max(1);
    let data = compute_diff_data(change);

    let mut out: Vec<String> = Vec::with_capacity(max_lines.min(4096) + 2);
    out.push(render_header(
        &change.path,
        data.added,
        data.removed,
        data.label.as_deref(),
        width,
    ));

    match data.content {
        DiffContent::Message(msg) => {
            out.push(truncate_to_width(
                &fg(dark::GRAY)(&msg),
                width,
                "\u{2026}",
                true,
            ));
        }
        DiffContent::Lines(lines) => {
            if lines.is_empty() {
                out.push(truncate_to_width(
                    &fg(dark::GRAY)("(no visible changes)"),
                    width,
                    "\u{2026}",
                    true,
                ));
            } else {
                let shown = lines.len().min(max_lines);
                for line in &lines[..shown] {
                    out.push(render_body_line(line, width));
                }
                if lines.len() > shown {
                    let hidden = lines.len() - shown;
                    let note = format!(
                        "\u{2026} ({hidden} more line{})",
                        if hidden == 1 { "" } else { "s" }
                    );
                    out.push(truncate_to_width(&fg(dark::GRAY)(&note), width, "", true));
                }
            }
        }
    }

    out
}

/// Read a file's content from disk, ready to feed to
/// [`FileChange::with_old_content`] / [`DiffPreview::with_old_content`].
///
/// This is the **only** function in this module that touches the
/// filesystem — every other function here is pure text-in/text-out, which
/// is what makes them unit-testable without a temp directory. Whether and
/// when to call this (e.g. skip it when the target path doesn't exist yet —
/// that's the "new file" case, not an error to surface) is the caller's
/// decision; this function has no opinion, it just reads bytes.
pub fn read_old_content_from_disk(path: &std::path::Path) -> std::io::Result<String> {
    std::fs::read_to_string(path)
}

/// A [`Component`] wrapping a [`FileChange`]: collapsed shows a compact
/// `path \u{b7} +N -M` summary plus a few hunk lines, expanded shows up to
/// [`EXPANDED_MAX_LINES`]. Output is cached by `(width, expanded)`, mirroring
/// `Text`'s caching pattern (`crates/pirust-tui/src/components/text.rs`) —
/// a cache hit still clones the cached `Vec<String>` because `Component::render`
/// returns an owned `Vec<String>`, not a borrow; that clone (not a full
/// re-diff-and-recolor pass) is the cost this cache is designed to avoid,
/// exactly as `Text`'s own doc comment discusses.
pub struct DiffPreview {
    change: FileChange,
    expanded: bool,
    cached_key: Option<(usize, bool)>,
    cached_lines: Option<Vec<String>>,
}

impl DiffPreview {
    /// Wrap a parsed file change, starting collapsed.
    pub fn new(change: FileChange) -> Self {
        Self {
            change,
            expanded: false,
            cached_key: None,
            cached_lines: None,
        }
    }

    /// Attach the file's real current content (see
    /// [`FileChange::with_old_content`]) and invalidate any cached render.
    pub fn with_old_content(mut self, old: impl Into<String>) -> Self {
        self.change = self.change.with_old_content(old);
        self.clear_cache();
        self
    }

    /// Set the expanded/collapsed state, invalidating the cache only when it
    /// actually changes (mirrors `BoxComponent`/`Text`'s "mutator clears
    /// cache" convention).
    pub fn set_expanded(&mut self, expanded: bool) {
        if self.expanded != expanded {
            self.expanded = expanded;
            self.clear_cache();
        }
    }

    /// Flip collapsed \u{2194} expanded.
    pub fn toggle(&mut self) {
        self.set_expanded(!self.expanded);
    }

    pub fn is_expanded(&self) -> bool {
        self.expanded
    }

    pub fn change(&self) -> &FileChange {
        &self.change
    }

    fn clear_cache(&mut self) {
        self.cached_key = None;
        self.cached_lines = None;
    }
}

impl Component for DiffPreview {
    fn render(&mut self, width: usize) -> Vec<String> {
        let key = (width, self.expanded);
        if self.cached_key == Some(key) {
            if let Some(lines) = &self.cached_lines {
                return lines.clone();
            }
        }

        let max_lines = if self.expanded {
            EXPANDED_MAX_LINES
        } else {
            COLLAPSED_MAX_LINES
        };
        let lines = render_diff_lines(&self.change, width, max_lines);

        self.cached_key = Some(key);
        self.cached_lines = Some(lines.clone());
        lines
    }

    fn invalidate(&mut self) {
        self.clear_cache();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn write_change(path: &str, content: &str) -> FileChange {
        parse_file_change("write", &json!({ "path": path, "content": content })).expect("parses")
    }

    fn edit_change(path: &str, edits: Vec<(&str, &str)>) -> FileChange {
        let edits: Vec<Value> = edits
            .into_iter()
            .map(|(old, new)| json!({ "oldText": old, "newText": new }))
            .collect();
        parse_file_change("edit", &json!({ "path": path, "edits": edits })).expect("parses")
    }

    // -- parse_file_change ---------------------------------------------

    #[test]
    fn parses_write_tool_call() {
        let change = write_change("src/main.rs", "fn main() {}\n");
        assert_eq!(change.path, "src/main.rs");
        match change.kind {
            FileChangeKind::Write { content } => assert_eq!(content, "fn main() {}\n"),
            _ => panic!("expected Write"),
        }
        assert!(change.old_content.is_none());
    }

    #[test]
    fn parses_edit_tool_call_with_edits_array() {
        let change = edit_change("src/lib.rs", vec![("foo", "bar")]);
        match change.kind {
            FileChangeKind::Edit { edits } => {
                assert_eq!(edits.len(), 1);
                assert_eq!(edits[0].old_text, "foo");
                assert_eq!(edits[0].new_text, "bar");
            }
            _ => panic!("expected Edit"),
        }
    }

    #[test]
    fn parses_edit_legacy_flat_old_new_text() {
        let args = json!({ "path": "a.txt", "oldText": "x", "newText": "y" });
        let change = parse_file_change("edit", &args).expect("parses");
        match change.kind {
            FileChangeKind::Edit { edits } => {
                assert_eq!(
                    edits,
                    vec![Edit {
                        old_text: "x".into(),
                        new_text: "y".into()
                    }]
                );
            }
            _ => panic!("expected Edit"),
        }
    }

    #[test]
    fn parses_edit_legacy_json_string_edits() {
        let raw_edits =
            serde_json::to_string(&json!([{ "oldText": "a", "newText": "b" }])).unwrap();
        let args = json!({ "path": "a.txt", "edits": raw_edits });
        let change = parse_file_change("edit", &args).expect("parses");
        match change.kind {
            FileChangeKind::Edit { edits } => assert_eq!(edits.len(), 1),
            _ => panic!("expected Edit"),
        }
    }

    #[test]
    fn returns_none_for_non_file_tool() {
        assert!(parse_file_change("bash", &json!({ "command": "ls" })).is_none());
    }

    #[test]
    fn returns_none_for_write_missing_content() {
        assert!(parse_file_change("write", &json!({ "path": "a.txt" })).is_none());
    }

    #[test]
    fn returns_none_for_edit_missing_edits() {
        assert!(parse_file_change("edit", &json!({ "path": "a.txt" })).is_none());
    }

    // -- render_diff_lines ------------------------------------------------

    #[test]
    fn write_without_old_content_renders_new_file_diff() {
        let change = write_change("new.txt", "hello\nworld\n");
        let lines = render_diff_lines(&change, 80, 20);
        assert!(lines[0].contains("new.txt"));
        assert!(lines[0].contains("+2"));
        assert!(lines[0].contains("-0"));
        assert!(lines[0].to_lowercase().contains("new file"));
        assert!(lines.iter().any(|l| l.contains("+hello")));
        assert!(lines.iter().any(|l| l.contains("+world")));
        assert!(!lines.iter().any(|l| l.contains('-') && l.contains("hello")));
    }

    #[test]
    fn write_with_old_content_renders_true_diff() {
        let change = write_change("f.txt", "a\nb\nc\n").with_old_content("a\nX\nc\n");
        let lines = render_diff_lines(&change, 80, 20);
        assert!(lines[0].contains("+1"));
        assert!(lines[0].contains("-1"));
        assert!(!lines[0].to_lowercase().contains("new file"));
        assert!(lines.iter().any(|l| l.contains("-X")));
        assert!(lines.iter().any(|l| l.contains("+b")));
    }

    #[test]
    fn edit_without_old_content_renders_synthetic_preview() {
        let change = edit_change("f.txt", vec![("x", "y")]);
        let lines = render_diff_lines(&change, 80, 20);
        assert!(lines[0].to_lowercase().contains("unknown"));
        assert!(lines.iter().any(|l| l.contains("-x")));
        assert!(lines.iter().any(|l| l.contains("+y")));
    }

    #[test]
    fn edit_with_old_content_renders_true_diff() {
        let change = edit_change("f.txt", vec![("bar", "baz")]).with_old_content("foo\nbar\n");
        let lines = render_diff_lines(&change, 80, 20);
        assert!(lines[0].contains("+1"));
        assert!(lines[0].contains("-1"));
        assert!(lines.iter().any(|l| l.contains("-bar")));
        assert!(lines.iter().any(|l| l.contains("+baz")));
    }

    #[test]
    fn edit_with_old_content_that_does_not_match_falls_back_with_reason() {
        let change = edit_change("f.txt", vec![("nope", "baz")]).with_old_content("foo\nbar\n");
        let lines = render_diff_lines(&change, 80, 20);
        assert!(lines[0].to_lowercase().contains("could not apply edits"));
        // Never hide partial failure: the synthetic before/after still shows.
        assert!(lines.iter().any(|l| l.contains("-nope")));
        assert!(lines.iter().any(|l| l.contains("+baz")));
    }

    #[test]
    fn oversized_content_summarizes_instead_of_diffing() {
        let big = "a\n".repeat(MAX_DIFF_INPUT_BYTES / 2 + 1);
        let change = write_change("big.txt", &big);
        let lines = render_diff_lines(&change, 80, 20);
        assert_eq!(lines.len(), 2);
        assert!(lines[1].to_lowercase().contains("too large"));
    }

    #[test]
    fn truncation_note_appears_when_diff_exceeds_max_lines() {
        let content: String = (0..50).map(|i| format!("line{i}\n")).collect();
        let change = write_change("many.txt", &content);
        let lines = render_diff_lines(&change, 80, 5);
        // header + 5 body lines + 1 trailer.
        assert_eq!(lines.len(), 7);
        assert!(lines.last().unwrap().contains("more line"));
    }

    #[test]
    fn long_line_is_clipped_to_width_not_wrapped() {
        let content = format!("{}\n", "x".repeat(500));
        let change = write_change("wide.txt", &content);
        let lines = render_diff_lines(&change, 40, 20);
        for line in &lines {
            assert!(pirust_tui::utils::visible_width(line) <= 40);
        }
    }

    // -- DiffPreview / Component -------------------------------------------

    #[test]
    fn diff_preview_caches_identical_render_by_width_and_expanded() {
        let change = write_change("f.txt", "a\nb\n");
        let mut preview = DiffPreview::new(change);
        let first = preview.render(80);
        let second = preview.render(80);
        assert_eq!(first, second);
    }

    #[test]
    fn diff_preview_toggle_changes_rendered_line_count() {
        let content: String = (0..30).map(|i| format!("line{i}\n")).collect();
        let change = write_change("many.txt", &content);
        let mut preview = DiffPreview::new(change);

        let collapsed = preview.render(80);
        assert!(collapsed.iter().any(|l| l.contains("more line")));

        preview.toggle();
        assert!(preview.is_expanded());
        let expanded = preview.render(80);
        assert!(!expanded.iter().any(|l| l.contains("more line")));
        assert!(expanded.len() > collapsed.len());
    }

    #[test]
    fn diff_preview_with_old_content_invalidates_cache_and_changes_output() {
        let change = write_change("f.txt", "a\nb\n");
        let mut preview = DiffPreview::new(change);
        let before = preview.render(80);
        assert!(before[0].to_lowercase().contains("new file"));

        let mut preview = preview.with_old_content("a\nX\n");
        let after = preview.render(80);
        assert!(!after[0].to_lowercase().contains("new file"));
        assert_ne!(before, after);
    }

    #[test]
    fn set_expanded_to_same_value_is_a_no_op_for_the_cache() {
        let change = write_change("f.txt", "a\n");
        let mut preview = DiffPreview::new(change);
        preview.render(80);
        preview.set_expanded(false); // already false
        assert!(
            preview.cached_lines.is_some(),
            "cache should survive a no-op set_expanded"
        );
    }
}
