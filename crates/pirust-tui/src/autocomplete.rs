//! Port of `packages/tui/src/autocomplete.ts` — slash-command and
//! file-path autocomplete. See `docs/analysis/05-tui.md` §2/§9. Moved up
//! from Wave 7 into Wave 5 (see `plan.md`) since it has zero dependency on
//! `tui.ts`/rendering — only on `fuzzy.rs` (Wave 3) and the filesystem.
//!
//! ## Scope decisions (documented, not silent — AGENTS.md Correctness Bar)
//!
//! - **Synchronous, blocking `get_suggestions`/`apply_completion` — no
//!   `AbortSignal` equivalent.** The TS's `getSuggestions`/
//!   `walkDirectoryWithFd` are `Promise`+`AbortSignal`-based (spawns `fd`,
//!   listens for `abort` to `SIGKILL` it). This crate has no owned event
//!   loop (`tui.rs`, Wave 4, dropped `tokio`; its render loop is
//!   caller-polled). This port uses `std::process::Command::output()`
//!   (blocking, not spawn+kill) for the `fd` call — a real debounced,
//!   cancellable version is `editor.rs`'s (Wave 6) integration-layer job,
//!   the same deferred-timer story as `Loader`'s animation (this wave).
//! - **`(SlashCommand | AutocompleteItem)[]`** becomes an enum,
//!   [`CommandOrItem`], since Rust has no ad-hoc union types.
//! - **Optional trait methods** (`triggerCharacters`, `getArgumentCompletions`
//!   on `SlashCommand`; `shouldTriggerFileCompletion` on
//!   `AutocompleteProvider`) get default no-op/`None` implementations on the
//!   trait, matching `tui::Component`'s own `handle_input`/
//!   `as_focusable_mut` optional-capability pattern (Wave 4) rather than
//!   requiring every implementer to provide a stub.
//! - **`fd` presence is a test-environment fact, not a code-path decision.**
//!   `CombinedAutocompleteProvider::new` takes `fd_path: Option<&str>`
//!   exactly like the TS constructor's `fdPath: string | null` — when
//!   `None`, the fuzzy-file branch returns `[]` immediately without ever
//!   invoking `Command`, matching `getFuzzyFileSuggestions`'s own
//!   `if (!this.fdPath...) return [];` guard. The `fd` CLI-argument
//!   construction (`build_fd_args`) is a separately unit-testable pure
//!   function so its correctness does not depend on a real `fd` binary
//!   being installed in any given dev environment (this one has none — see
//!   `progress.md`).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::fuzzy::fuzzy_filter;

const PATH_DELIMITERS: &[char] = &[' ', '\t', '"', '\'', '='];

fn to_display_path(value: &str) -> String {
    value.replace('\\', "/")
}

fn escape_regex(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        if ".*+?^${}()|[]\\".contains(ch) {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// `buildFdPathQuery` (autocomplete.ts:17).
fn build_fd_path_query(query: &str) -> String {
    let normalized = to_display_path(query);
    if !normalized.contains('/') {
        return normalized;
    }
    let has_trailing_separator = normalized.ends_with('/');
    let trimmed = normalized.trim_matches('/');
    if trimmed.is_empty() {
        return normalized;
    }
    let separator_pattern = "[\\\\/]";
    let segments: Vec<String> = trimmed
        .split('/')
        .filter(|s| !s.is_empty())
        .map(escape_regex)
        .collect();
    if segments.is_empty() {
        return normalized;
    }
    let mut pattern = segments.join(separator_pattern);
    if has_trailing_separator {
        pattern.push_str(separator_pattern);
    }
    pattern
}

fn find_last_delimiter(text: &str) -> Option<usize> {
    let chars: Vec<char> = text.chars().collect();
    (0..chars.len())
        .rev()
        .find(|&i| PATH_DELIMITERS.contains(&chars[i]))
}

/// `findUnclosedQuoteStart` (autocomplete.ts:54) — returns the char index of
/// the still-open quote, or `None` if `text` has no unclosed `"`. The TS can
/// only return `null` or a non-negative index in practice (`quoteStart` is
/// only ever read once `inQuotes` has been set `true`, which also sets it).
fn find_unclosed_quote_start(text: &str) -> Option<usize> {
    let mut in_quotes = false;
    let mut quote_start: Option<usize> = None;
    for (i, ch) in text.chars().enumerate() {
        if ch == '"' {
            in_quotes = !in_quotes;
            if in_quotes {
                quote_start = Some(i);
            }
        }
    }
    if in_quotes {
        quote_start
    } else {
        None
    }
}

fn is_token_start(text: &str, index: usize) -> bool {
    if index == 0 {
        return true;
    }
    text.chars()
        .nth(index - 1)
        .is_some_and(|c| PATH_DELIMITERS.contains(&c))
}

fn char_slice_from(text: &str, char_index: usize) -> String {
    text.chars().skip(char_index).collect()
}

/// `extractQuotedPrefix` (autocomplete.ts:74).
fn extract_quoted_prefix(text: &str) -> Option<String> {
    let quote_start = find_unclosed_quote_start(text)?;

    if quote_start > 0 {
        let prev = text.chars().nth(quote_start - 1);
        if prev == Some('@') {
            if !is_token_start(text, quote_start - 1) {
                return None;
            }
            return Some(char_slice_from(text, quote_start - 1));
        }
    }

    if !is_token_start(text, quote_start) {
        return None;
    }
    Some(char_slice_from(text, quote_start))
}

struct PathPrefix {
    raw_prefix: String,
    is_at_prefix: bool,
    is_quoted_prefix: bool,
}

/// `parsePathPrefix` (autocomplete.ts:94).
fn parse_path_prefix(prefix: &str) -> PathPrefix {
    if let Some(rest) = prefix.strip_prefix("@\"") {
        return PathPrefix {
            raw_prefix: rest.to_string(),
            is_at_prefix: true,
            is_quoted_prefix: true,
        };
    }
    if let Some(rest) = prefix.strip_prefix('"') {
        return PathPrefix {
            raw_prefix: rest.to_string(),
            is_at_prefix: false,
            is_quoted_prefix: true,
        };
    }
    if let Some(rest) = prefix.strip_prefix('@') {
        return PathPrefix {
            raw_prefix: rest.to_string(),
            is_at_prefix: true,
            is_quoted_prefix: false,
        };
    }
    PathPrefix {
        raw_prefix: prefix.to_string(),
        is_at_prefix: false,
        is_quoted_prefix: false,
    }
}

// `isDirectory` (autocomplete.ts:109) is accepted by the TS `buildCompletionValue`
// but never read in its body — confirmed dead parameter, not ported (same
// category as `getFuzzyFileSuggestions`'s `isQuotedPrefix`, see module docs).
struct CompletionValueOptions {
    is_at_prefix: bool,
    is_quoted_prefix: bool,
}

/// `buildCompletionValue` (autocomplete.ts:107).
fn build_completion_value(path: &str, options: CompletionValueOptions) -> String {
    let needs_quotes = options.is_quoted_prefix || path.contains(' ');
    let prefix = if options.is_at_prefix { "@" } else { "" };
    if !needs_quotes {
        return format!("{prefix}{path}");
    }
    format!("{prefix}\"{path}\"")
}

/// `buildFdPathQuery`'s consuming CLI-arg builder — `walkDirectoryWithFd`'s
/// `args` array construction (autocomplete.ts:131-156), separated into a
/// pure function so it is unit-testable without a real `fd` binary — see
/// module docs.
pub fn build_fd_args(base_dir: &str, query: &str, max_results: usize) -> Vec<String> {
    let mut args = vec![
        "--base-directory".to_string(),
        base_dir.to_string(),
        "--max-results".to_string(),
        max_results.to_string(),
        "--type".to_string(),
        "f".to_string(),
        "--type".to_string(),
        "d".to_string(),
        "--follow".to_string(),
        "--hidden".to_string(),
        "--exclude".to_string(),
        ".git".to_string(),
        "--exclude".to_string(),
        ".git/*".to_string(),
        "--exclude".to_string(),
        ".git/**".to_string(),
    ];
    if to_display_path(query).contains('/') {
        args.push("--full-path".to_string());
    }
    if !query.is_empty() {
        args.push(build_fd_path_query(query));
    }
    args
}

#[derive(Debug, Clone)]
pub struct FdEntry {
    pub path: String,
    pub is_directory: bool,
}

/// `walkDirectoryWithFd` (autocomplete.ts:124) — see module docs, blocking
/// not `Promise`+`AbortSignal`-cancellable.
fn walk_directory_with_fd(
    base_dir: &str,
    fd_path: &str,
    query: &str,
    max_results: usize,
) -> Vec<FdEntry> {
    let args = build_fd_args(base_dir, query, max_results);
    let Ok(output) = Command::new(fd_path).args(&args).output() else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stdout = stdout.trim();
    if stdout.is_empty() {
        return Vec::new();
    }

    let mut results = Vec::new();
    for line in stdout.lines() {
        if line.is_empty() {
            continue;
        }
        let display_line = to_display_path(line);
        let has_trailing_separator = display_line.ends_with('/');
        let normalized_path = if has_trailing_separator {
            display_line[..display_line.len() - 1].to_string()
        } else {
            display_line.clone()
        };
        if normalized_path == ".git"
            || normalized_path.starts_with(".git/")
            || normalized_path.contains("/.git/")
        {
            continue;
        }
        results.push(FdEntry {
            path: display_line,
            is_directory: has_trailing_separator,
        });
    }
    results
}

/// `AutocompleteItem` (autocomplete.ts:219).
#[derive(Debug, Clone)]
pub struct AutocompleteItem {
    pub value: String,
    pub label: String,
    pub description: Option<String>,
}

/// `getArgumentCompletions?(argumentPrefix): Awaitable<AutocompleteItem[] | null>`
/// (autocomplete.ts:233) — synchronous here, see module docs.
pub type ArgumentCompletionsFn = Box<dyn Fn(&str) -> Option<Vec<AutocompleteItem>>>;

/// `SlashCommand` (autocomplete.ts:227).
pub struct SlashCommand {
    pub name: String,
    pub description: Option<String>,
    pub argument_hint: Option<String>,
    pub get_argument_completions: Option<ArgumentCompletionsFn>,
}

/// `(SlashCommand | AutocompleteItem)[]` (autocomplete.ts:274) — see module
/// docs for why this is an enum.
pub enum CommandOrItem {
    Command(SlashCommand),
    Item(AutocompleteItem),
}

impl CommandOrItem {
    fn name(&self) -> &str {
        match self {
            CommandOrItem::Command(c) => &c.name,
            CommandOrItem::Item(i) => &i.value,
        }
    }
}

/// `AutocompleteSuggestions` (autocomplete.ts:236).
#[derive(Debug, Clone)]
pub struct AutocompleteSuggestions {
    pub items: Vec<AutocompleteItem>,
    pub prefix: String,
}

/// A single line of editor text plus the cursor's line/column, mirroring the
/// TS's `(lines: string[], cursorLine: number, cursorCol: number)` triple —
/// `cursorCol` is a UTF-16 code-unit offset into `lines[cursorLine]`, same
/// convention as `word_navigation.rs`/`components::input`.
pub struct CompletionContext<'a> {
    pub lines: &'a [String],
    pub cursor_line: usize,
    pub cursor_col: usize,
}

/// The result of `applyCompletion` (autocomplete.ts:256-266).
pub struct AppliedCompletion {
    pub lines: Vec<String>,
    pub cursor_line: usize,
    pub cursor_col: usize,
}

/// `AutocompleteProvider` (autocomplete.ts:241) — see module docs for the
/// optional-method default-impl idiom.
pub trait AutocompleteProvider {
    fn trigger_characters(&self) -> Option<Vec<char>> {
        None
    }
    fn get_suggestions(
        &self,
        ctx: &CompletionContext,
        force: bool,
    ) -> Option<AutocompleteSuggestions>;
    fn apply_completion(
        &self,
        ctx: &CompletionContext,
        item: &AutocompleteItem,
        prefix: &str,
    ) -> AppliedCompletion;
    fn should_trigger_file_completion(&self, _ctx: &CompletionContext) -> bool {
        true
    }
}

fn current_line(lines: &[String], cursor_line: usize) -> &str {
    lines.get(cursor_line).map(String::as_str).unwrap_or("")
}

fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

fn utf16_to_byte_index(s: &str, utf16_idx: usize) -> usize {
    let mut utf16_pos = 0usize;
    for (byte_idx, ch) in s.char_indices() {
        if utf16_pos >= utf16_idx {
            return byte_idx;
        }
        utf16_pos += ch.len_utf16();
    }
    s.len()
}

fn slice_utf16(s: &str, start: usize, end: usize) -> &str {
    let start_b = utf16_to_byte_index(s, start);
    let end_b = utf16_to_byte_index(s, end.max(start));
    &s[start_b..end_b]
}

/// `CombinedAutocompleteProvider` (autocomplete.ts:273) — the real
/// slash-command + file-path autocomplete implementation.
pub struct CombinedAutocompleteProvider {
    commands: Vec<CommandOrItem>,
    base_path: PathBuf,
    fd_path: Option<String>,
}

struct ScopedFuzzyQuery {
    base_dir: PathBuf,
    query: String,
    display_base: String,
}

impl CombinedAutocompleteProvider {
    /// `constructor` (autocomplete.ts:278).
    pub fn new(
        commands: Vec<CommandOrItem>,
        base_path: impl Into<PathBuf>,
        fd_path: Option<String>,
    ) -> Self {
        Self {
            commands,
            base_path: base_path.into(),
            fd_path,
        }
    }

    fn expand_home_path(path: &str) -> String {
        let Some(home) = home_dir() else {
            return path.to_string();
        };
        if path == "~" {
            return home.to_string_lossy().replace('\\', "/");
        }
        if let Some(rest) = path.strip_prefix("~/") {
            let mut expanded = home.join(rest).to_string_lossy().replace('\\', "/");
            if path.ends_with('/') && !expanded.ends_with('/') {
                expanded.push('/');
            }
            return expanded;
        }
        path.to_string()
    }

    fn extract_at_prefix(&self, text: &str) -> Option<String> {
        if let Some(quoted) = extract_quoted_prefix(text) {
            if quoted.starts_with("@\"") {
                return Some(quoted);
            }
        }
        let last_delim = find_last_delimiter(text);
        let token_start = last_delim.map(|i| i + 1).unwrap_or(0);
        if text.chars().nth(token_start) == Some('@') {
            return Some(char_slice_from(text, token_start));
        }
        None
    }

    fn extract_path_prefix(&self, text: &str, force_extract: bool) -> Option<String> {
        if let Some(quoted) = extract_quoted_prefix(text) {
            return Some(quoted);
        }
        let last_delim = find_last_delimiter(text);
        let path_prefix = match last_delim {
            Some(i) => char_slice_from(text, i + 1),
            None => text.to_string(),
        };
        if force_extract {
            return Some(path_prefix);
        }
        if path_prefix.contains('/')
            || path_prefix.starts_with('.')
            || path_prefix.starts_with("~/")
        {
            return Some(path_prefix);
        }
        if path_prefix.is_empty() && text.ends_with(' ') {
            return Some(path_prefix);
        }
        None
    }

    /// `getFileSuggestions` (autocomplete.ts:559) — the plain
    /// `readdirSync`-based directory listing branch.
    fn get_file_suggestions(&self, prefix: &str) -> Vec<AutocompleteItem> {
        let parsed = parse_path_prefix(prefix);
        let raw_prefix = parsed.raw_prefix.as_str();
        let mut expanded_prefix = raw_prefix.to_string();
        if expanded_prefix.starts_with('~') {
            expanded_prefix = Self::expand_home_path(&expanded_prefix);
        }

        let is_root_prefix = raw_prefix.is_empty()
            || raw_prefix == "./"
            || raw_prefix == "../"
            || raw_prefix == "~"
            || raw_prefix == "~/"
            || raw_prefix == "/"
            || (parsed.is_at_prefix && raw_prefix.is_empty());

        let (search_dir, search_prefix): (PathBuf, String);
        if is_root_prefix || raw_prefix.ends_with('/') {
            search_dir = if raw_prefix.starts_with('~') || expanded_prefix.starts_with('/') {
                PathBuf::from(&expanded_prefix)
            } else {
                self.base_path.join(&expanded_prefix)
            };
            search_prefix = String::new();
        } else {
            let dir = Path::new(&expanded_prefix)
                .parent()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            let file = Path::new(&expanded_prefix)
                .file_name()
                .map(|f| f.to_string_lossy().to_string())
                .unwrap_or_default();
            search_dir = if raw_prefix.starts_with('~') || expanded_prefix.starts_with('/') {
                PathBuf::from(&dir)
            } else {
                self.base_path.join(&dir)
            };
            search_prefix = file;
        }

        let Ok(entries) = fs::read_dir(&search_dir) else {
            return Vec::new();
        };

        let mut suggestions: Vec<AutocompleteItem> = Vec::new();
        let search_prefix_lower = search_prefix.to_lowercase();
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.to_lowercase().starts_with(&search_prefix_lower) {
                continue;
            }

            let mut is_directory = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            if !is_directory {
                if let Ok(ft) = entry.file_type() {
                    if ft.is_symlink() {
                        is_directory = fs::metadata(entry.path())
                            .map(|m| m.is_dir())
                            .unwrap_or(false);
                    }
                }
            }

            let display_prefix = raw_prefix;
            let relative_path: String = if display_prefix.ends_with('/') {
                format!("{display_prefix}{name}")
            } else if display_prefix.contains('/') || display_prefix.contains('\\') {
                if let Some(home_rel) = display_prefix.strip_prefix("~/") {
                    let dir = Path::new(home_rel)
                        .parent()
                        .map(|p| p.to_string_lossy().to_string())
                        .unwrap_or_default();
                    if dir.is_empty() || dir == "." {
                        format!("~/{name}")
                    } else {
                        format!("~/{}", join_display(&dir, &name))
                    }
                } else if display_prefix.starts_with('/') {
                    let dir = Path::new(display_prefix)
                        .parent()
                        .map(|p| p.to_string_lossy().to_string())
                        .unwrap_or_default();
                    if dir == "/" {
                        format!("/{name}")
                    } else {
                        format!("{dir}/{name}")
                    }
                } else {
                    let dir = Path::new(display_prefix)
                        .parent()
                        .map(|p| p.to_string_lossy().to_string())
                        .unwrap_or_default();
                    let mut joined = join_display(&dir, &name);
                    if display_prefix.starts_with("./") && !joined.starts_with("./") {
                        joined = format!("./{joined}");
                    }
                    joined
                }
            } else if display_prefix.starts_with('~') {
                format!("~/{name}")
            } else {
                name.clone()
            };

            let relative_path = to_display_path(&relative_path);
            let path_value = if is_directory {
                format!("{relative_path}/")
            } else {
                relative_path
            };
            let value = build_completion_value(
                &path_value,
                CompletionValueOptions {
                    is_at_prefix: parsed.is_at_prefix,
                    is_quoted_prefix: parsed.is_quoted_prefix,
                },
            );

            suggestions.push(AutocompleteItem {
                value,
                label: format!("{name}{}", if is_directory { "/" } else { "" }),
                description: None,
            });
        }

        suggestions.sort_by(|a, b| {
            let a_is_dir = a.value.ends_with('/');
            let b_is_dir = b.value.ends_with('/');
            match (a_is_dir, b_is_dir) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => a.label.cmp(&b.label),
            }
        });

        suggestions
    }

    fn score_entry(file_path: &str, query: &str, is_directory: bool) -> i64 {
        let file_name = Path::new(file_path)
            .file_name()
            .map(|f| f.to_string_lossy().to_string())
            .unwrap_or_default();
        let lower_file_name = file_name.to_lowercase();
        let lower_query = query.to_lowercase();

        let mut score = 0i64;
        if lower_file_name == lower_query {
            score = 100;
        } else if lower_file_name.starts_with(&lower_query) {
            score = 80;
        } else if lower_file_name.contains(&lower_query) {
            score = 50;
        } else if file_path.to_lowercase().contains(&lower_query) {
            score = 30;
        }
        if is_directory && score > 0 {
            score += 10;
        }
        score
    }

    fn resolve_scoped_fuzzy_query(&self, raw_query: &str) -> Option<ScopedFuzzyQuery> {
        let normalized_query = to_display_path(raw_query);
        let slash_index = normalized_query.rfind('/')?;
        let display_base = normalized_query[..slash_index + 1].to_string();
        let query = normalized_query[slash_index + 1..].to_string();

        let base_dir = if let Some(rest) = display_base.strip_prefix("~/") {
            let home = home_dir()?;
            home.join(rest)
        } else if display_base.starts_with('/') {
            PathBuf::from(&display_base)
        } else {
            self.base_path.join(&display_base)
        };

        if !fs::metadata(&base_dir).map(|m| m.is_dir()).unwrap_or(false) {
            return None;
        }

        Some(ScopedFuzzyQuery {
            base_dir,
            query,
            display_base,
        })
    }

    fn scoped_path_for_display(display_base: &str, relative_path: &str) -> String {
        let normalized = to_display_path(relative_path);
        if display_base == "/" {
            format!("/{normalized}")
        } else {
            format!("{}{normalized}", to_display_path(display_base))
        }
    }

    /// `getFuzzyFileSuggestions` (autocomplete.ts:720) — see module docs;
    /// returns `[]` immediately if no `fd` binary path was configured or the
    /// binary is not actually runnable, matching the TS's own guard.
    fn get_fuzzy_file_suggestions(&self, query: &str) -> Vec<AutocompleteItem> {
        let Some(fd_path) = &self.fd_path else {
            return Vec::new();
        };

        let scoped_query = self.resolve_scoped_fuzzy_query(query);
        let (fd_base_dir, fd_query, scoped_display_base) = match &scoped_query {
            Some(s) => (
                s.base_dir.to_string_lossy().to_string(),
                s.query.clone(),
                Some(s.display_base.clone()),
            ),
            None => (
                self.base_path.to_string_lossy().to_string(),
                query.to_string(),
                None,
            ),
        };

        let entries = walk_directory_with_fd(&fd_base_dir, fd_path, &fd_query, 100);

        let mut scored: Vec<(FdEntry, i64)> = entries
            .into_iter()
            .map(|e| {
                let score = if !fd_query.is_empty() {
                    Self::score_entry(&e.path, &fd_query, e.is_directory)
                } else {
                    1
                };
                (e, score)
            })
            .filter(|(_, score)| *score > 0)
            .collect();
        scored.sort_by_key(|(_, score)| std::cmp::Reverse(*score));
        scored.truncate(20);

        let mut suggestions = Vec::new();
        for (entry, _) in scored {
            let path_without_slash = if entry.is_directory {
                entry.path.trim_end_matches('/').to_string()
            } else {
                entry.path.clone()
            };
            let display_path = match &scoped_display_base {
                Some(base) => Self::scoped_path_for_display(base, &path_without_slash),
                None => path_without_slash.clone(),
            };
            let entry_name = Path::new(&path_without_slash)
                .file_name()
                .map(|f| f.to_string_lossy().to_string())
                .unwrap_or_default();
            let completion_path = if entry.is_directory {
                format!("{display_path}/")
            } else {
                display_path.clone()
            };
            let value = build_completion_value(
                &completion_path,
                CompletionValueOptions {
                    is_at_prefix: true,
                    is_quoted_prefix: false,
                },
            );
            suggestions.push(AutocompleteItem {
                value,
                label: format!("{entry_name}{}", if entry.is_directory { "/" } else { "" }),
                description: Some(display_path),
            });
        }
        suggestions
    }
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

fn join_display(dir: &str, name: &str) -> String {
    if dir.is_empty() || dir == "." {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}

impl AutocompleteProvider for CombinedAutocompleteProvider {
    /// `getSuggestions` (autocomplete.ts:284).
    fn get_suggestions(
        &self,
        ctx: &CompletionContext,
        force: bool,
    ) -> Option<AutocompleteSuggestions> {
        let line = current_line(ctx.lines, ctx.cursor_line);
        let text_before_cursor = slice_utf16(line, 0, ctx.cursor_col);

        if let Some(at_prefix) = self.extract_at_prefix(text_before_cursor) {
            let parsed = parse_path_prefix(&at_prefix);
            // `isQuotedPrefix` is accepted by the TS's `getFuzzyFileSuggestions`
            // options object but never read inside its body (autocomplete.ts:720) —
            // confirmed dead parameter, not ported.
            let suggestions = self.get_fuzzy_file_suggestions(&parsed.raw_prefix);
            if suggestions.is_empty() {
                return None;
            }
            return Some(AutocompleteSuggestions {
                items: suggestions,
                prefix: at_prefix,
            });
        }

        if !force && text_before_cursor.starts_with('/') {
            let space_index = text_before_cursor.find(' ');
            match space_index {
                None => {
                    let prefix = &text_before_cursor[1..];
                    let command_items: Vec<(String, Option<String>)> = self
                        .commands
                        .iter()
                        .map(|c| {
                            let name = c.name().to_string();
                            let hint = match c {
                                CommandOrItem::Command(cmd) => cmd.argument_hint.clone(),
                                CommandOrItem::Item(_) => None,
                            };
                            let desc = match c {
                                CommandOrItem::Command(cmd) => {
                                    cmd.description.clone().unwrap_or_default()
                                }
                                CommandOrItem::Item(item) => {
                                    item.description.clone().unwrap_or_default()
                                }
                            };
                            let full_desc = match hint {
                                Some(h) if !desc.is_empty() => Some(format!("{h} — {desc}")),
                                Some(h) => Some(h),
                                None if !desc.is_empty() => Some(desc),
                                None => None,
                            };
                            (name, full_desc)
                        })
                        .collect();

                    let filtered = fuzzy_filter(&command_items, prefix, |(name, _)| name.clone());
                    if filtered.is_empty() {
                        return None;
                    }
                    let items = filtered
                        .into_iter()
                        .map(|(name, desc)| AutocompleteItem {
                            value: name.clone(),
                            label: name.clone(),
                            description: desc.clone(),
                        })
                        .collect();
                    return Some(AutocompleteSuggestions {
                        items,
                        prefix: text_before_cursor.to_string(),
                    });
                }
                Some(space_index) => {
                    let command_name = &text_before_cursor[1..space_index];
                    let argument_text = &text_before_cursor[space_index + 1..];
                    let command = self.commands.iter().find_map(|c| match c {
                        CommandOrItem::Command(cmd) if cmd.name == command_name => Some(cmd),
                        _ => None,
                    })?;
                    let get_args = command.get_argument_completions.as_ref()?;
                    let argument_suggestions = get_args(argument_text)?;
                    if argument_suggestions.is_empty() {
                        return None;
                    }
                    return Some(AutocompleteSuggestions {
                        items: argument_suggestions,
                        prefix: argument_text.to_string(),
                    });
                }
            }
        }

        let path_match = self.extract_path_prefix(text_before_cursor, force)?;
        let suggestions = self.get_file_suggestions(&path_match);
        if suggestions.is_empty() {
            return None;
        }
        Some(AutocompleteSuggestions {
            items: suggestions,
            prefix: path_match,
        })
    }

    /// `applyCompletion` (autocomplete.ts:375).
    fn apply_completion(
        &self,
        ctx: &CompletionContext,
        item: &AutocompleteItem,
        prefix: &str,
    ) -> AppliedCompletion {
        let line = current_line(ctx.lines, ctx.cursor_line);
        let prefix_len = utf16_len(prefix);
        let before_prefix = slice_utf16(line, 0, ctx.cursor_col.saturating_sub(prefix_len));
        let after_cursor = slice_utf16(line, ctx.cursor_col, utf16_len(line));
        let is_quoted_prefix = prefix.starts_with('"') || prefix.starts_with("@\"");
        let has_leading_quote_after_cursor = after_cursor.starts_with('"');
        let has_trailing_quote_in_item = item.value.ends_with('"');
        let adjusted_after_cursor =
            if is_quoted_prefix && has_trailing_quote_in_item && has_leading_quote_after_cursor {
                &after_cursor[1.min(after_cursor.len())..]
            } else {
                after_cursor
            };

        let is_slash_command = prefix.starts_with('/')
            && before_prefix.trim().is_empty()
            && !prefix[1..].contains('/');
        if is_slash_command {
            let new_line = format!("{before_prefix}/{} {adjusted_after_cursor}", item.value);
            let mut new_lines = ctx.lines.to_vec();
            if let Some(l) = new_lines.get_mut(ctx.cursor_line) {
                *l = new_line;
            }
            let cursor_col = utf16_len(before_prefix) + utf16_len(&item.value) + 2;
            return AppliedCompletion {
                lines: new_lines,
                cursor_line: ctx.cursor_line,
                cursor_col,
            };
        }

        if prefix.starts_with('@') {
            let is_directory = item.label.ends_with('/');
            let suffix = if is_directory { "" } else { " " };
            let new_line = format!(
                "{before_prefix}{}{suffix}{adjusted_after_cursor}",
                item.value
            );
            let mut new_lines = ctx.lines.to_vec();
            if let Some(l) = new_lines.get_mut(ctx.cursor_line) {
                *l = new_line;
            }
            let has_trailing_quote = item.value.ends_with('"');
            let cursor_offset = if is_directory && has_trailing_quote {
                utf16_len(&item.value) - 1
            } else {
                utf16_len(&item.value)
            };
            let cursor_col = utf16_len(before_prefix) + cursor_offset + utf16_len(suffix);
            return AppliedCompletion {
                lines: new_lines,
                cursor_line: ctx.cursor_line,
                cursor_col,
            };
        }

        let text_before_cursor = slice_utf16(line, 0, ctx.cursor_col);
        if text_before_cursor.contains('/') && text_before_cursor.contains(' ') {
            let new_line = format!("{before_prefix}{}{adjusted_after_cursor}", item.value);
            let mut new_lines = ctx.lines.to_vec();
            if let Some(l) = new_lines.get_mut(ctx.cursor_line) {
                *l = new_line;
            }
            let is_directory = item.label.ends_with('/');
            let has_trailing_quote = item.value.ends_with('"');
            let cursor_offset = if is_directory && has_trailing_quote {
                utf16_len(&item.value) - 1
            } else {
                utf16_len(&item.value)
            };
            let cursor_col = utf16_len(before_prefix) + cursor_offset;
            return AppliedCompletion {
                lines: new_lines,
                cursor_line: ctx.cursor_line,
                cursor_col,
            };
        }

        let new_line = format!("{before_prefix}{}{adjusted_after_cursor}", item.value);
        let mut new_lines = ctx.lines.to_vec();
        if let Some(l) = new_lines.get_mut(ctx.cursor_line) {
            *l = new_line;
        }
        let is_directory = item.label.ends_with('/');
        let has_trailing_quote = item.value.ends_with('"');
        let cursor_offset = if is_directory && has_trailing_quote {
            utf16_len(&item.value) - 1
        } else {
            utf16_len(&item.value)
        };
        let cursor_col = utf16_len(before_prefix) + cursor_offset;
        AppliedCompletion {
            lines: new_lines,
            cursor_line: ctx.cursor_line,
            cursor_col,
        }
    }

    /// `shouldTriggerFileCompletion` (autocomplete.ts:775).
    fn should_trigger_file_completion(&self, ctx: &CompletionContext) -> bool {
        let line = current_line(ctx.lines, ctx.cursor_line);
        let text_before_cursor = slice_utf16(line, 0, ctx.cursor_col);
        let trimmed = text_before_cursor.trim();
        !trimmed.starts_with('/') || trimmed.contains(' ')
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn build_fd_args_matches_expected_flags() {
        let args = build_fd_args("/base", "foo", 100);
        assert_eq!(args[0], "--base-directory");
        assert_eq!(args[1], "/base");
        assert!(args.contains(&"--hidden".to_string()));
        assert!(args.contains(&"foo".to_string()));
    }

    #[test]
    fn build_fd_args_adds_full_path_for_slash_query() {
        let args = build_fd_args("/base", "src/main", 100);
        assert!(args.contains(&"--full-path".to_string()));
    }

    // `@`-prefixed completions ALWAYS go through the fd-based fuzzy branch in
    // real Pi (autocomplete.ts:294's `getFuzzyFileSuggestions` call, never
    // `getFileSuggestions`) — with no `fd` on PATH in this dev environment
    // (see module docs), these two cases instead drive the plain
    // `readdirSync`-based branch via a bare (non-`@`) prefix + `force=true`
    // (the real Tab-key trigger path, `extractPathPrefix`'s
    // `forceExtract` branch).

    #[test]
    fn plain_directory_listing_finds_matching_files() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("alpha.txt"), "").unwrap();
        fs::write(dir.path().join("beta.txt"), "").unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();

        let provider = CombinedAutocompleteProvider::new(vec![], dir.path(), None);
        let lines = vec!["al".to_string()];
        let ctx = CompletionContext {
            lines: &lines,
            cursor_line: 0,
            cursor_col: 2,
        };
        let suggestions = provider.get_suggestions(&ctx, true).unwrap();
        assert_eq!(suggestions.items.len(), 1);
        assert_eq!(suggestions.items[0].label, "alpha.txt");
    }

    #[test]
    fn directories_sort_before_files() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("b_file.txt"), "").unwrap();
        fs::create_dir(dir.path().join("a_dir")).unwrap();

        let provider = CombinedAutocompleteProvider::new(vec![], dir.path(), None);
        let lines = vec![String::new()];
        let ctx = CompletionContext {
            lines: &lines,
            cursor_line: 0,
            cursor_col: 0,
        };
        let suggestions = provider.get_suggestions(&ctx, true).unwrap();
        assert_eq!(suggestions.items[0].label, "a_dir/");
    }

    #[test]
    fn slash_command_completion_filters_by_prefix() {
        let commands = vec![CommandOrItem::Command(SlashCommand {
            name: "help".to_string(),
            description: Some("Show help".to_string()),
            argument_hint: None,
            get_argument_completions: None,
        })];
        let provider = CombinedAutocompleteProvider::new(commands, ".", None);
        let lines = vec!["/he".to_string()];
        let ctx = CompletionContext {
            lines: &lines,
            cursor_line: 0,
            cursor_col: 3,
        };
        let suggestions = provider.get_suggestions(&ctx, false).unwrap();
        assert_eq!(suggestions.items[0].value, "help");
    }

    #[test]
    fn apply_completion_for_slash_command_inserts_space() {
        let provider = CombinedAutocompleteProvider::new(vec![], ".", None);
        let lines = vec!["/he".to_string()];
        let ctx = CompletionContext {
            lines: &lines,
            cursor_line: 0,
            cursor_col: 3,
        };
        let item = AutocompleteItem {
            value: "help".to_string(),
            label: "help".to_string(),
            description: None,
        };
        let result = provider.apply_completion(&ctx, &item, "/he");
        assert_eq!(result.lines[0], "/help ");
    }

    #[test]
    fn no_fd_path_returns_no_fuzzy_suggestions() {
        let provider = CombinedAutocompleteProvider::new(vec![], ".", None);
        let lines = vec!["@src/".to_string()];
        let ctx = CompletionContext {
            lines: &lines,
            cursor_line: 0,
            cursor_col: 5,
        };
        // Falls back to get_fuzzy_file_suggestions which returns [] with no fd_path.
        assert!(provider.get_suggestions(&ctx, false).is_none());
    }

    #[test]
    fn should_trigger_file_completion_false_mid_slash_command() {
        let provider = CombinedAutocompleteProvider::new(vec![], ".", None);
        let lines = vec!["/help".to_string()];
        let ctx = CompletionContext {
            lines: &lines,
            cursor_line: 0,
            cursor_col: 5,
        };
        assert!(!provider.should_trigger_file_completion(&ctx));
    }
}
