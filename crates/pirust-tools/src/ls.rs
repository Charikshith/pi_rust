//! Port of `core/tools/ls.ts` (UI-free half) — the `ls` tool.
//!
//! Gated by `tests/fixtures/pi/tools/{schemas,strings}/ls.json` and the `ls` rows
//! of `exec.corpus.jsonl`.
//!
//! `ls` is Pi's *flat* directory lister: depth 1, one bare entry name per line, a
//! `/` appended to directories, dotfiles included, and **no ignore mechanism at
//! all** — `.gitignore` is not consulted (`ls.ts:140-171`). Two independent caps
//! apply, whichever is hit first: [`DEFAULT_LIMIT`] entries and
//! [`DEFAULT_MAX_BYTES`] bytes (`ls.ts:180-197`).
//!
//! Ported items:
//!
//! | TS (`ls.ts`) | here |
//! | --- | --- |
//! | `lsSchema` (:14-17) | the `object_schema` in [`create_ls_tool_definition`] |
//! | `DEFAULT_LIMIT` (:21) | [`DEFAULT_LIMIT`] |
//! | `LsToolDetails` (:23-26) | [`LsToolDetails`] |
//! | `LsOperations` (:32-39) | [`LsOperations`] + [`LsStat`] |
//! | `defaultLsOperations` (:41-45) | [`LocalLsOperations`] |
//! | `LsToolOptions` (:47-50) | [`LsToolOptions`] |
//! | `createLsToolDefinition` (:95-221) | [`create_ls_tool_definition`] |
//! | `createLsTool` (:223-225) | [`create_ls_tool`] |
//! | `formatLsCall` (:52-60), `formatLsResult` (:62-93), `renderCall` (:210-214), `renderResult` (:215-219) | **omitted** — TUI only (feat-006/007) |
//!
//! Two behaviours that look like oversights but are the contract, both reproduced
//! verbatim:
//!
//! - An entry whose `stat` throws is silently `continue`d (`ls.ts:166-169`), so a
//!   broken symlink both vanishes from the output *and* does not consume the entry
//!   budget — the `results.length >= effectiveLimit` guard is checked before the
//!   `stat`, and `continue` never pushes.
//! - `limit` is applied as `limit ?? DEFAULT_LIMIT` with **no clamping**
//!   (`ls.ts:125`), unlike `grep`'s `Math.max(1, …)`. `limit: 0` therefore breaks
//!   out of the loop on the first iteration, leaving `results` empty, which takes
//!   the `(empty directory)` branch (`ls.ts:175-178`) — so the entry-limit notice
//!   and `details.entryLimitReached` are both dropped.
//!
//! Sorting is the one place this port is not exact; see [`sort_entries`].

use std::fmt;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use pirust_agent_core::types::{AgentTool, AgentToolResult, AgentToolUpdateCallback, ToolError};
use pirust_ai::jsnum::js_number;
use pirust_ai::types::{TextContent, UserContent};
use serde::{Deserialize, Serialize, Serializer};
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use unicode_normalization::char::is_combining_mark;
use unicode_normalization::UnicodeNormalization;

use crate::definition::schema::{number_prop, object_schema, optional, string_prop};
use crate::definition::PirustToolDefinition;
use crate::path_utils;
use crate::truncate::DEFAULT_MAX_BYTES;
use crate::truncate::{format_size, truncate_head, TruncationOptions, TruncationResult};

/// Default maximum number of entries returned (TS `ls.ts:21`).
pub const DEFAULT_LIMIT: f64 = 500.0;

/// `Number.MAX_SAFE_INTEGER`, passed as `maxLines` so that only the byte cap is
/// live (TS `ls.ts:182`: "There is no separate line limit because entry count is
/// already capped").
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// `ls.ts:103` — the `description` template literal with its two substitutions
/// already performed: `${DEFAULT_LIMIT}` → `500` and `${DEFAULT_MAX_BYTES / 1024}`
/// → `50`.
///
/// Note that the byte cap appears here as the bare `"50KB"` of an integer
/// division, *not* as [`format_size`]'s `"50.0KB"` — both literals exist in this
/// module (the latter in the truncation notice, `ls.ts:192`) and they are not
/// interchangeable. `description_matches_the_template_literal` pins the
/// substitution.
pub const LS_DESCRIPTION: &str = "List directory contents. Returns entries sorted alphabetically, with '/' suffix for directories. Includes dotfiles. Output is truncated to 500 entries or 50KB (whichever is hit first).";

/// `ls.ts:104` — `promptSnippet`.
pub const LS_PROMPT_SNIPPET: &str = "List directory contents";

/// The message of the `Error` Pi rejects with on abort, both from the up-front
/// `signal.aborted` check (`ls.ts:115`) and from the `abort` listener
/// (`ls.ts:119`).
const ABORT_MESSAGE: &str = "Operation aborted";

/// Output for a directory with nothing listable in it (TS `ls.ts:176`).
const EMPTY_DIRECTORY: &str = "(empty directory)";

// ===========================================================================
// Operations seam
// ===========================================================================

/// The single bit of `fs.Stats` that `ls` consumes (TS `{ isDirectory: () =>
/// boolean }`, `ls.ts:36`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LsStat {
    /// TS `stat.isDirectory()`.
    pub is_directory: bool,
}

/// Pluggable operations for the `ls` tool (TS `LsOperations`, `ls.ts:32-39`).
///
/// Override to delegate directory listing to a remote system (for example SSH).
///
/// `stat` and `readdir` return [`ToolError`] rather than a typed error because
/// their only use of the failure is Pi's `e.message`: `readdir`'s is interpolated
/// into `Cannot read directory: ${e.message}` (`ls.ts:145`) and `stat`'s is
/// re-thrown untouched (`ls.ts:203-205`). See [`LocalLsOperations::readdir`] for
/// the one thing that cannot be reproduced there.
#[async_trait]
pub trait LsOperations: Send + Sync {
    /// Check if path exists (TS `ls.ts:34`).
    async fn exists(&self, absolute_path: &str) -> bool;
    /// Get file or directory stats; errors if not found (TS `ls.ts:36`).
    async fn stat(&self, absolute_path: &str) -> Result<LsStat, ToolError>;
    /// Read directory entry names — bare names, not paths (TS `ls.ts:38`).
    async fn readdir(&self, absolute_path: &str) -> Result<Vec<String>, ToolError>;
}

/// `defaultLsOperations` (TS `ls.ts:41-45`): `pathExists`, `fs/promises.stat`,
/// `fs/promises.readdir`.
#[derive(Debug, Clone, Copy, Default)]
pub struct LocalLsOperations;

#[async_trait]
impl LsOperations for LocalLsOperations {
    /// TS `pathExists` (`ls.ts:42`), i.e. `path-utils.ts:31-38`.
    async fn exists(&self, absolute_path: &str) -> bool {
        path_utils::path_exists(absolute_path).await
    }

    /// TS `fsStat` (`ls.ts:43`) — Node's `stat`, which **follows** symlinks, so a
    /// broken symlink errors here and is skipped by the caller. `tokio::fs::metadata`
    /// follows symlinks too.
    async fn stat(&self, absolute_path: &str) -> Result<LsStat, ToolError> {
        let metadata = tokio::fs::metadata(absolute_path).await?;
        Ok(LsStat {
            is_directory: metadata.is_dir(),
        })
    }

    /// TS `fsReaddir` (`ls.ts:44`).
    ///
    /// # Known gap
    ///
    /// On failure Pi surfaces Node's message — `EACCES: permission denied, scandir
    /// 'C:\x'` — which is assembled from libuv's error-string table and is not
    /// derivable from a `std::io::Error`, whose `Display` is the OS text plus
    /// `(os error N)`. So `Cannot read directory: …` carries a different tail than
    /// Pi's. No captured corpus row exercises a `readdir` failure (the
    /// not-a-directory case is caught one step earlier by the `isDirectory` guard,
    /// `ls.ts:135`), and the trait seam lets a caller supply the exact text; closing
    /// it properly needs a libuv errno→message table.
    ///
    /// Non-UTF-8 names are lossy-converted, matching Node, which hands JS a UTF-8
    /// string with U+FFFD for unpaired UTF-16 surrogates / invalid byte sequences.
    async fn readdir(&self, absolute_path: &str) -> Result<Vec<String>, ToolError> {
        let mut reader = tokio::fs::read_dir(absolute_path).await?;
        let mut names = Vec::new();
        while let Some(entry) = reader.next_entry().await? {
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
        Ok(names)
    }
}

/// `LsToolOptions` (TS `ls.ts:47-50`).
#[derive(Clone, Default)]
pub struct LsToolOptions {
    /// Custom operations for directory listing. `None` → [`LocalLsOperations`]
    /// (TS `ls.ts:49`, resolved by `ls.ts:99`).
    pub operations: Option<Arc<dyn LsOperations>>,
}

impl fmt::Debug for LsToolOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LsToolOptions")
            .field("operations", &self.operations.as_ref().map(|_| "<dyn>"))
            .finish()
    }
}

// ===========================================================================
// Details
// ===========================================================================

/// `LsToolDetails` (TS `ls.ts:23-26`) — the `details` payload, persisted into the
/// session JSONL.
///
/// **Field order is deliberately the reverse of the TS interface.** The interface
/// declares `truncation` then `entryLimitReached` (`ls.ts:24-25`), but the object
/// is built empty and filled `entryLimitReached` first (`ls.ts:189`), `truncation`
/// second (`ls.ts:193`) — and `JSON.stringify` follows *insertion* order. Since
/// this value is compared byte-for-byte against captured Pi output, the Rust
/// declaration order matches the insertion order instead.
///
/// Both fields are omitted when absent, and the whole object collapses to `null`
/// when neither is set (`ls.ts:201`: `Object.keys(details).length > 0 ? details :
/// undefined`) — see [`LsToolDetails::is_empty`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LsToolDetails {
    /// The `effectiveLimit` that was reached, set only when the entry cap cut the
    /// listing (TS `ls.ts:189`). `f64` because `limit` is a schema `number`; see
    /// [`serialize_js_number`] for how it reaches JSON.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_js_number"
    )]
    pub entry_limit_reached: Option<f64>,
    /// The byte-truncation result, set only when the byte cap cut the listing
    /// (TS `ls.ts:193`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncation: Option<TruncationResult>,
}

impl LsToolDetails {
    /// TS `Object.keys(details).length === 0` (`ls.ts:201`).
    pub fn is_empty(&self) -> bool {
        self.entry_limit_reached.is_none() && self.truncation.is_none()
    }

    /// TS `details` as resolved into the tool result: the object, or `undefined`
    /// (→ JSON `null`) when it never got a key.
    fn into_value(self) -> Value {
        if self.is_empty() {
            Value::Null
        } else {
            serde_json::to_value(self).expect("LsToolDetails is always representable as JSON")
        }
    }
}

/// Emit an `f64` the way `JSON.stringify` would: as an integer literal whenever
/// the value is integral, since every JS number that happens to be a whole number
/// stringifies without a `.0`.
///
/// `serde_json` would otherwise write `3.0` for the `f64` `3.0`, and the captured
/// `details` reads `{"entryLimitReached":3}`. Non-integral values fall through to
/// `serde_json`'s shortest round-trip formatting, which agrees with
/// `Number::toString` except in exponential notation (`1e21` vs JS `1e+21`) —
/// unreachable for a plausible `limit`.
fn serialize_js_number<S: Serializer>(
    value: &Option<f64>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    let value = value.expect("skip_serializing_if guarantees Some");
    // 2^53: above it every f64 is integral but no longer i64-exact in JS terms.
    if value.fract() == 0.0 && value.abs() < 9_007_199_254_740_992.0 {
        serializer.serialize_i64(value as i64)
    } else {
        serializer.serialize_f64(value)
    }
}

// ===========================================================================
// Collation
// ===========================================================================

/// TS `ls.ts:150` — `entries.sort((a, b) => a.toLowerCase().localeCompare(b.toLowerCase()))`,
/// i.e. **ICU root-locale collation** of the lowercased names. Sorts in place, and
/// is stable, like `Array.prototype.sort`.
///
/// A naive `to_lowercase()` + `str::cmp()` does not reproduce this and is not a
/// near miss: the captured order for `tests/fixtures/pi/tools/exec.corpus.jsonl`'s
/// `mixed` row is
///
/// ```text
/// .dotfile  Apple.txt  apple2.txt  banana.txt  Éclair.txt  Sub/  über.txt  zebra.txt  Zulu.txt
/// ```
///
/// Codepoint order would put `Éclair.txt` and `über.txt` *after* `zebra.txt`
/// (U+00C9 and U+00FC both exceed `z`), and would put `apple2.txt` before
/// `Apple.txt` (`'2' < '.'`). ICU instead compares **primary weights** first,
/// where every accented Latin letter carries the weight of its base letter and
/// punctuation sorts below digits, which sort below letters.
///
/// # What this implementation reproduces
///
/// Two levels, compared in order:
///
/// 1. **Primary**: each character of the lowercased name is mapped to a primary
///    weight. For printable ASCII the weights come from [`ASCII_PRIMARY_ORDER`],
///    which *is* ICU's root order (derived by asking the host's own ICU — the same
///    one Pi runs on — to sort U+0020..U+007E through
///    `a.toLowerCase().localeCompare(b.toLowerCase())`). For anything else, the
///    character is NFD-decomposed and its combining marks dropped; if that leaves
///    an ASCII base letter it takes that letter's weight, which is what puts
///    `éclair` in the `e` bucket and `über` in the `u` bucket.
/// 2. **Tiebreak**: codepoint order of the lowercased names.
///
/// Consequently the ordering is **faithful** for names built from printable ASCII
/// plus Latin letters carrying combining accents (that is: NFD-decomposable
/// letters — `é ü ō ș à ñ …`, precomposed or not), including all cross-category
/// comparisons (punctuation < digits < letters) and all base-letter comparisons.
/// Every one of the nine captured `ls` rows lands in that subset.
///
/// # What it does not reproduce
///
/// - Level 2/3 of the real algorithm. Two names that differ only in their accents
///   (`resume` vs `résumé`, `á` vs `à`) tie at the primary level and fall to the
///   codepoint tiebreak, which is not ICU's secondary order — ICU orders acute
///   before grave (U+0301 before U+0300), the opposite of codepoint order. The
///   direction is right for *presence* of an accent (`a` < `á`) and wrong only
///   between two different accents on the same base letter.
/// - Letters with no canonical decomposition whose ICU primary weight is still a
///   Latin letter, or an expansion of several: `ø`/`đ`/`ł` (weight `o`/`d`/`l`),
///   `æ` (`ae`), `ß` (`ss`), `þ` (`th`). These fall into the past-ASCII bucket and
///   sort after `z` instead.
/// - Any non-Latin script's placement relative to another, and the relative order
///   of two non-ASCII punctuation or symbol characters: both are codepoint-ordered
///   here.
/// - Primary-ignorable characters other than the C0 controls and DEL that
///   [`primary_weight`] drops (soft hyphen, zero-width joiners, most format
///   characters).
///
/// **Closing the gap needs a dependency**: `icu_collator` (ICU4X) with the root
/// locale and default strength would make this exact. It was not added because
/// adding dependencies is not this port's call to make; every captured row passes
/// without it.
pub fn sort_entries(entries: &mut [String]) {
    // Decorate-sort-undecorate: the key is O(len) to build and would otherwise be
    // rebuilt on every comparison.
    let mut keyed: Vec<(Vec<u32>, String, usize)> = entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let lowered = entry.to_lowercase();
            (primary_key(&lowered), lowered, index)
        })
        .collect();
    // `sort_by` is stable, as `Array.prototype.sort` has been since ES2019, so
    // entries that collate equal keep their `readdir` order in both runtimes.
    // `index` is carried only to permute `entries`, never compared.
    keyed.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));

    let reordered: Vec<String> = keyed
        .iter()
        .map(|(_, _, index)| entries[*index].clone())
        .collect();
    entries.clone_from_slice(&reordered);
}

/// ICU root-locale primary order of the printable ASCII range, with the two cases
/// of each letter folded onto one weight (they differ only at ICU's tertiary
/// level, and [`sort_entries`] lowercases first anyway).
///
/// This is not a guess: it is `[...chars].sort((a, b) =>
/// a.toLowerCase().localeCompare(b.toLowerCase()))` over U+0020..U+007E, run on
/// the host ICU, deduplicated by case. Codepoint order is nowhere close — `_`
/// leads the punctuation and `-` follows it, while `!` (U+0021) comes eighth.
const ASCII_PRIMARY_ORDER: &[u8] =
    b" _-,;:!?.'\"()[]{}@*/\\&#%`^+<=>|~$0123456789abcdefghijklmnopqrstuvwxyz";

/// `ASCII_PRIMARY_ORDER` inverted into a byte-indexed lookup. `0` means "no
/// weight": a C0 control or DEL, which ICU treats as primary-ignorable.
static ASCII_PRIMARY_WEIGHT: [u8; 128] = build_ascii_primary_weight();

const fn build_ascii_primary_weight() -> [u8; 128] {
    let mut table = [0u8; 128];
    let mut i = 0;
    while i < ASCII_PRIMARY_ORDER.len() {
        let byte = ASCII_PRIMARY_ORDER[i];
        // Weights are 1-based so that `0` can mean "ignorable".
        table[byte as usize] = (i + 1) as u8;
        if byte >= b'a' && byte <= b'z' {
            table[(byte - 32) as usize] = (i + 1) as u8;
        }
        i += 1;
    }
    table
}

/// The primary weight of one character, or `None` when it is primary-ignorable.
fn primary_weight(c: char) -> Option<u32> {
    let code = c as u32;
    if code < 128 {
        let weight = ASCII_PRIMARY_WEIGHT[code as usize];
        // C0 control or DEL: ICU gives these no primary weight at all, so they
        // drop out of the comparison rather than sorting before space.
        return (weight != 0).then_some(u32::from(weight));
    }
    // Past ASCII, and no ASCII base letter survived decomposition: order by
    // codepoint after every ASCII weight. Documented gap on `sort_entries`.
    Some(ASCII_PRIMARY_ORDER.len() as u32 + 1 + code)
}

/// The primary-level sort key of an already-lowercased name: NFD, combining marks
/// dropped, each remaining character mapped through [`primary_weight`].
fn primary_key(lowered: &str) -> Vec<u32> {
    lowered
        .nfd()
        .filter(|c| !is_combining_mark(*c))
        .filter_map(primary_weight)
        .collect()
}

// ===========================================================================
// Tool definition
// ===========================================================================

/// `createLsToolDefinition` (TS `ls.ts:95-221`), minus `renderCall` /
/// `renderResult`.
///
/// Note that `ops` is resolved **once, here** (`ls.ts:99`) and captured by
/// `execute` — unlike `grep`/`find`, which resolve theirs per call. Swapping
/// `options.operations` after this returns has no effect, and that is the
/// contract.
pub fn create_ls_tool_definition(
    cwd: impl Into<String>,
    options: LsToolOptions,
) -> PirustToolDefinition {
    let cwd: Arc<str> = Arc::from(cwd.into());
    let ops: Arc<dyn LsOperations> = options
        .operations
        .unwrap_or_else(|| Arc::new(LocalLsOperations));

    PirustToolDefinition::new(
        "ls",
        "ls",
        LS_DESCRIPTION,
        // `ls` is the only built-in whose schema has no `required` key at all:
        // both properties are `Type.Optional` (TS `ls.ts:14-17`).
        object_schema([
            optional(
                "path",
                string_prop("Directory to list (default: current directory)"),
            ),
            optional(
                "limit",
                number_prop("Maximum number of entries to return (default: 500)"),
            ),
        ]),
        move |_tool_call_id: String,
              args: Value,
              token: CancellationToken,
              _on_update: AgentToolUpdateCallback| {
            let cwd = Arc::clone(&cwd);
            let ops = Arc::clone(&ops);
            async move { execute_ls(&cwd, ops.as_ref(), &args, &token).await }
        },
    )
    .with_prompt_snippet(LS_PROMPT_SNIPPET)
}

/// `createLsTool` (TS `ls.ts:223-225`).
///
/// `wrapToolDefinition` has no separate type here — `PirustToolDefinition` *is*
/// the [`AgentTool`] — so this is [`create_ls_tool_definition`] behind the trait
/// object.
pub fn create_ls_tool(cwd: impl Into<String>, options: LsToolOptions) -> Arc<dyn AgentTool> {
    Arc::new(create_ls_tool_definition(cwd, options))
}

/// TS `execute` (`ls.ts:106-209`).
///
/// Pi hand-rolls a `Promise` so that an `abort` arriving mid-listing rejects
/// immediately rather than after the current `stat` chain finishes (`ls.ts:113-120`),
/// and removes the listener before resolving (`ls.ts:173`) so a late abort cannot
/// clobber a finished result. The `select!` below is that shape: `biased` polls the
/// body first, so a body that is already `Ready` wins — the Rust equivalent of
/// unregistering the listener — while a body that is `Pending` yields to the
/// cancellation branch.
async fn execute_ls(
    cwd: &str,
    ops: &dyn LsOperations,
    args: &Value,
    token: &CancellationToken,
) -> Result<AgentToolResult, ToolError> {
    // TS `ls.ts:114-117`: the synchronous pre-check, before any I/O.
    if token.is_cancelled() {
        return Err(ABORT_MESSAGE.into());
    }

    tokio::select! {
        biased;
        result = ls_body(cwd, ops, args) => result,
        () = token.cancelled() => Err(ABORT_MESSAGE.into()),
    }
}

/// The `async` IIFE inside Pi's promise (TS `ls.ts:122-207`).
async fn ls_body(
    cwd: &str,
    ops: &dyn LsOperations,
    args: &Value,
) -> Result<AgentToolResult, ToolError> {
    // TS `ls.ts:124`: `path || "."`, so `undefined`, `null` *and* `""` all fall
    // back to the cwd — `??` would keep `""`.
    let path = args
        .get("path")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
        .unwrap_or(".");
    let dir_path = path_utils::resolve_to_cwd(path, cwd)?;
    // TS `ls.ts:125`: `limit ?? DEFAULT_LIMIT` — `??`, so an explicit `0` survives,
    // and there is no `Math.max(1, …)` clamp.
    let effective_limit = args
        .get("limit")
        .and_then(Value::as_f64)
        .unwrap_or(DEFAULT_LIMIT);

    // TS `ls.ts:128-131`.
    if !ops.exists(&dir_path).await {
        return Err(format!("Path not found: {dir_path}").into());
    }

    // TS `ls.ts:134-138`. A `stat` failure here propagates untouched, as Pi's
    // outer `catch` re-`reject`s the raw error (`ls.ts:203-205`).
    if !ops.stat(&dir_path).await?.is_directory {
        return Err(format!("Not a directory: {dir_path}").into());
    }

    // TS `ls.ts:141-147`.
    let mut entries = ops
        .readdir(&dir_path)
        .await
        .map_err(|e| -> ToolError { format!("Cannot read directory: {e}").into() })?;

    // TS `ls.ts:150`. Sorts the bare names — the `/` suffix is appended after, so
    // it never participates in the comparison.
    sort_entries(&mut entries);

    // TS `ls.ts:153-171`.
    let mut results: Vec<String> = Vec::new();
    let mut entry_limit_reached = false;
    for entry in entries {
        // Checked *before* the `stat`, which is what makes an unstattable entry
        // free of budget.
        if results.len() as f64 >= effective_limit {
            entry_limit_reached = true;
            break;
        }

        // TS `nodePath.join(dirPath, entry)` (`ls.ts:161`). `dir_path` is already
        // `path.resolve`d and `entry` is a bare `readdir` name, so `join`'s
        // normalization has nothing to do; `Path::join` matches it (including not
        // doubling the separator after a root like `C:\`). The value is only ever
        // fed to `stat`, never printed.
        let full_path = Path::new(&dir_path).join(&entry);
        let suffix = match ops.stat(&full_path.to_string_lossy()).await {
            Ok(stat) if stat.is_directory => "/",
            Ok(_) => "",
            // TS `ls.ts:166-169`: skip entries we cannot stat.
            Err(_) => continue,
        };
        results.push(format!("{entry}{suffix}"));
    }

    // TS `ls.ts:175-178`: `details` is `undefined` here even if the entry limit
    // was hit, because a zero/negative `limit` empties `results`.
    if results.is_empty() {
        return Ok(text_result(EMPTY_DIRECTORY, Value::Null));
    }

    let raw_output = results.join("\n");
    // TS `ls.ts:182`: only the byte cap is live.
    let truncation = truncate_head(
        &raw_output,
        TruncationOptions {
            max_lines: Some(MAX_SAFE_INTEGER),
            max_bytes: None,
        },
    );
    let mut output = truncation.content.clone();
    let mut details = LsToolDetails::default();

    // TS `ls.ts:186-194`. Note the absence of grep/find's "or refine pattern".
    let mut notices: Vec<String> = Vec::new();
    if entry_limit_reached {
        notices.push(format!(
            "{} entries limit reached. Use limit={} for more",
            js_number(effective_limit),
            js_number(effective_limit * 2.0)
        ));
        details.entry_limit_reached = Some(effective_limit);
    }
    if truncation.truncated {
        // `formatSize`, hence `50.0KB` — not the `50KB` of the description.
        notices.push(format!("{} limit reached", format_size(DEFAULT_MAX_BYTES)));
        details.truncation = Some(truncation);
    }
    // TS `ls.ts:195-197`.
    if !notices.is_empty() {
        output += &format!("\n\n[{}]", notices.join(". "));
    }

    Ok(text_result(&output, details.into_value()))
}

/// TS `{ content: [{ type: "text", text }], details }` (`ls.ts:176`, `ls.ts:199-202`).
fn text_result(text: &str, details: Value) -> AgentToolResult {
    AgentToolResult {
        content: vec![UserContent::Text(TextContent::new(text))],
        details,
        added_tool_names: None,
        terminate: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Mutex;

    /// [`LS_DESCRIPTION`] is a flattened template literal; this reproduces the
    /// interpolation so the two constants it depends on cannot drift.
    #[test]
    fn description_matches_the_template_literal() {
        let composed = format!(
            "List directory contents. Returns entries sorted alphabetically, with '/' suffix for \
             directories. Includes dotfiles. Output is truncated to {} entries or {}KB (whichever \
             is hit first).",
            js_number(DEFAULT_LIMIT),
            DEFAULT_MAX_BYTES / 1024
        );
        assert_eq!(LS_DESCRIPTION, composed);
        // The description's `50KB` and the notice's `50.0KB` are different strings
        // and must not be conflated.
        assert_eq!(format_size(DEFAULT_MAX_BYTES), "50.0KB");
        assert!(!LS_DESCRIPTION.contains("50.0KB"));
    }

    /// Pins the ASCII half of [`sort_entries`] against the host-ICU-derived order:
    /// codepoint sorting gets all three of these pairs wrong.
    #[test]
    fn ascii_primary_order_beats_codepoint_order() {
        let mut entries: Vec<String> = ["a2", "a.", "a_", "a-", "a!"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        sort_entries(&mut entries);
        assert_eq!(entries, ["a_", "a-", "a!", "a.", "a2"]);
    }

    /// Pins the diacritic-folding half: accented Latin letters collate with their
    /// base letter, not after `z`.
    #[test]
    fn accents_fold_onto_their_base_letter() {
        let mut entries: Vec<String> = ["zebra", "\u{00FC}ber", "\u{00C9}clair", "sub"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        sort_entries(&mut entries);
        assert_eq!(entries, ["\u{00C9}clair", "sub", "\u{00FC}ber", "zebra"]);

        // Presence of an accent is ordered correctly (primary tie, tiebreak on the
        // lowercased codepoints puts bare `e` first, as ICU does).
        let mut pair = ["r\u{00E9}sum\u{00E9}".to_string(), "resume".to_string()];
        sort_entries(&mut pair);
        assert_eq!(pair, ["resume", "r\u{00E9}sum\u{00E9}"]);
    }

    /// The documented secondary-level gap, asserted in the direction this port
    /// actually goes so that a future `icu_collator` swap shows up as a failure
    /// here rather than silently.
    ///
    /// Real Pi (ICU root) yields `á` before `à`: acute has a lower secondary
    /// weight than grave. This port ties them at the primary level and falls back
    /// to codepoint order, which puts U+00E0 (`à`) first.
    #[test]
    fn two_accents_on_one_base_letter_is_the_known_gap() {
        let mut entries = ["\u{00E1}".to_string(), "\u{00E0}".to_string()];
        sort_entries(&mut entries);
        assert_eq!(
            entries,
            ["\u{00E0}", "\u{00E1}"],
            "documented divergence: ICU root orders acute before grave"
        );
    }

    /// A substitute [`LsOperations`] for the one behaviour no corpus row can reach:
    /// an entry whose `stat` fails.
    ///
    /// The captured `exec.corpus.jsonl` tree is materialized from a JSON spec of
    /// plain directories and files, and a broken symlink — the real-world way to
    /// make `stat` fail on a name `readdir` just returned — is not portably
    /// creatable from such a spec on Windows. This fake is also the only
    /// [`LsOperations`] implementation besides [`LocalLsOperations`], so it is what
    /// gives the trait seam any coverage at all.
    struct FakeLsOps {
        /// What `readdir` returns, in its pre-sort order.
        entries: Vec<&'static str>,
        /// The one entry whose `stat` rejects.
        broken: &'static str,
        /// Every path `stat` was called with, in call order.
        stat_calls: Mutex<Vec<String>>,
    }

    impl FakeLsOps {
        fn new(entries: Vec<&'static str>, broken: &'static str) -> Self {
            Self {
                entries,
                broken,
                stat_calls: Mutex::new(Vec::new()),
            }
        }

        fn stat_calls(&self) -> Vec<String> {
            self.stat_calls.lock().expect("stat_calls mutex").clone()
        }
    }

    #[async_trait]
    impl LsOperations for FakeLsOps {
        async fn exists(&self, _absolute_path: &str) -> bool {
            true
        }

        /// Anything whose file name is one of `entries` is a plain file (or, for
        /// `broken`, an error); anything else is the listed directory itself.
        async fn stat(&self, absolute_path: &str) -> Result<LsStat, ToolError> {
            self.stat_calls
                .lock()
                .expect("stat_calls mutex")
                .push(absolute_path.to_string());
            let name = Path::new(absolute_path)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            if name == self.broken {
                // Node's message for `stat` on a dangling symlink target.
                return Err(
                    format!("ENOENT: no such file or directory, stat '{absolute_path}'").into(),
                );
            }
            Ok(LsStat {
                is_directory: !self.entries.contains(&name.as_str()),
            })
        }

        async fn readdir(&self, _absolute_path: &str) -> Result<Vec<String>, ToolError> {
            Ok(self.entries.iter().map(|e| (*e).to_string()).collect())
        }
    }

    /// An entry whose `stat` throws is skipped **and costs nothing** (TS
    /// `ls.ts:166-169`: the `catch` `continue`s without pushing, and the
    /// `results.length >= effectiveLimit` guard on `ls.ts:156` sits *before* the
    /// `stat` on `ls.ts:164`, so only pushed entries move the budget).
    ///
    /// With `limit: 2` and three entries where the middle one cannot be stat'd, Pi
    /// lists both stattable entries: the failing one neither appears nor consumes
    /// one of the two slots. Had it consumed a slot, the third entry would have hit
    /// the cap and the output would carry the `2 entries limit reached` notice plus
    /// `details.entryLimitReached`. Both absences are asserted.
    ///
    /// The names are chosen so the collating sort (`ls.ts:150`) leaves the failing
    /// entry in the *middle*: `a` < `a-broken` < `b`.
    #[tokio::test]
    async fn an_unstattable_entry_is_skipped_without_consuming_the_entry_budget() {
        let ops = Arc::new(FakeLsOps::new(
            // Deliberately unsorted, so the sort is what puts `a-broken` in the middle.
            vec!["b", "a-broken", "a"],
            "a-broken",
        ));
        let definition = create_ls_tool_definition(
            "/oracle/cwd",
            LsToolOptions {
                operations: Some(Arc::clone(&ops) as Arc<dyn LsOperations>),
            },
        );

        let result = definition
            .execute(
                "call_broken",
                serde_json::json!({ "path": ".", "limit": 2 }),
                CancellationToken::new(),
                Arc::new(|_| {}) as AgentToolUpdateCallback,
            )
            .await
            .expect("ls succeeds");

        let text = match &result.content[0] {
            UserContent::Text(text) => text.text.clone(),
            other => panic!("expected a text content block, got {other:?}"),
        };
        assert_eq!(
            text, "a\nb",
            "the unstattable entry must vanish, and no notice may be appended"
        );
        assert!(
            !text.contains("entries limit reached"),
            "the entry limit was never reached, so there is no notice: {text:?}"
        );
        assert_eq!(
            result.details,
            Value::Null,
            "details stays undefined: neither entryLimitReached nor truncation was set"
        );

        // The failing entry really was reached and really was stat'd — the assertion
        // above is not passing because the entry was filtered out earlier.
        let calls = ops.stat_calls();
        assert!(
            calls.iter().any(|call| call.ends_with("a-broken")),
            "the broken entry must be stat'd, not skipped before the stat: {calls:?}"
        );
        // 1 stat for the directory itself + 3 entry stats, i.e. the loop ran to the
        // end instead of breaking on the cap.
        assert_eq!(calls.len(), 4, "unexpected stat call sequence: {calls:?}");
    }

    /// `details` collapses to `null` when empty and keeps insertion order
    /// otherwise.
    #[test]
    fn details_serializes_like_json_stringify() {
        assert_eq!(LsToolDetails::default().into_value(), Value::Null);

        let details = LsToolDetails {
            entry_limit_reached: Some(3.0),
            truncation: None,
        };
        assert_eq!(
            serde_json::to_string(&details.into_value()).unwrap(),
            r#"{"entryLimitReached":3}"#
        );
    }
}
