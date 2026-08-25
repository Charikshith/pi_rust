//! Real, data-backed `/model` and `/resume` pickers — replacing the two fake
//! pickers in `interactive_mode.rs` (`ModelPicker`/`ResumePicker`, that
//! module's lines ~103-167 and ~1352-1470 as of this writing), which show
//! only the *current* model/session and answer Enter with a hard-coded
//! "not changeable in this session" error.
//!
//! Spec: `docs/tui-design-samples.html` §4 (model picker: provider / model id
//! / context window / reasoning support / local-vs-remote, fuzzy-searchable)
//! and §6 (session picker: title / cwd / modified time / model, a stable
//! column layout, scrolling, and a clear selected-row highlight).
//!
//! This module owns the picker *widgets* (`ModelPicker`, `SessionPicker`) and
//! their *data loaders* (`load_model_entries`, `load_session_entries`). It
//! does **not** wire itself into `interactive_mode.rs` — per the task split,
//! that file is edited by the caller, not here.
//!
//! # Data sources (real, not fabricated)
//!
//! - **Models**: [`crate::models::ModelRuntime::providers`]
//!   (`crate::models`:3409) returns `&[ComposedProvider]` — the fully
//!   composed (builtin catalog + `models.json` + per-model overrides) list
//!   that `--list-models`/`list_models` (`crate::models`:3783) itself
//!   iterates. [`load_model_entries`] takes that slice directly; the caller
//!   must hold a `ModelRuntime` (built via `ModelRuntime::create`,
//!   `crate::models`:3193) and pass `runtime.providers()`. Nothing here
//!   constructs or guesses a model list.
//! - **Sessions**: [`crate::session::SessionManager::list_all`]
//!   (`crate::session`:950) / `::list` (`crate::session`:910) return
//!   `Vec<SessionInfo>` (`crate::session`:1333) with `id`, `cwd`, `name`
//!   (title), and `modified` (an `i64` millisecond timestamp — the same sort
//!   key Pi uses). [`load_session_entries`] takes that slice directly.
//!
//!   `SessionInfo` carries **no `model` field** — Pi's on-disk session
//!   format only records a model as a `model_change` entry inside the
//!   transcript itself (`crate::session::SessionManager::append_model_change`,
//!   `crate::session`:2039), not in the header or the list-time summary. Do
//!   not fabricate one: [`load_session_entries`] takes a second parameter,
//!   `models_by_id: &HashMap<String, String>`, that the caller fills in
//!   however it likes (e.g. by walking each session's `model_change`
//!   entries once, lazily, or from an in-memory cache) and looks up by
//!   `SessionInfo::id`. An empty map is fine — those rows just render with
//!   a blank model column instead of a made-up value.

use std::cmp::Ordering as CmpOrdering;
use std::collections::HashMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use pirust_tui::fuzzy::fuzzy_match;
use pirust_tui::keys::matches_key;
use pirust_tui::tui::Component;
use pirust_tui::utils::truncate_to_width;

use crate::interactive_theme::{dark, fg};
use crate::models::{locale_compare, ComposedProvider};
use crate::session::SessionInfo;

// =============================================================================
// Data: models
// =============================================================================

/// One selectable model, flattened out of a [`ComposedProvider`] list and
/// stripped down to what the picker needs to display and search. Every
/// field is an owned value (no lifetime tied to the `ModelRuntime` the
/// caller loaded it from) so the picker can outlive a single render pass
/// without borrowing the runtime.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelEntry {
    /// `ComposedProvider::id` — the machine provider id (`"anthropic"`, not
    /// `"Anthropic"`), matching the sort key `list_models` itself uses
    /// (`crate::models`:3821).
    pub provider: String,
    /// `Model::id`.
    pub model_id: String,
    /// `Model::name` — the human display name.
    pub display_name: String,
    /// `Model::context_window`, in tokens.
    pub context_window: u64,
    /// `Model::reasoning` — whether the model advertises thinking support.
    pub reasoning: bool,
    /// Whether the model's *effective* `Model::base_url` (already resolved
    /// through the local-proxy override at provider-composition time, see
    /// `crate::models::apply_models_json`'s doc comment) points at a local
    /// or private-network host. `Model` carries no explicit "is local" flag
    /// — this is a heuristic over the base URL's host, documented on
    /// [`is_local_base_url`] rather than silently assumed.
    pub local: bool,
}

/// Loopback/private-network hosts treated as "local" for [`ModelEntry::local`].
const LOCAL_HOSTS: &[&str] = &["localhost", "127.0.0.1", "::1", "0.0.0.0"];

/// Heuristic: is `base_url`'s host a loopback address or an RFC1918 private
/// range? `Model` (`pirust-ai`'s `types::model::Model`) has no dedicated
/// "local" field — the only signal available post-composition is the
/// effective `base_url` each model carries (every model's `baseUrl` is
/// `config.baseUrl ?? model.baseUrl`, i.e. the local-proxy override already
/// applied). This intentionally does not resolve DNS or consult `/etc/hosts`
/// — it is a display hint, not a security boundary.
fn is_local_base_url(base_url: &str) -> bool {
    let after_scheme = base_url.split("://").nth(1).unwrap_or(base_url);
    let host = after_scheme
        .split(['/', ':', '?'])
        .next()
        .unwrap_or(after_scheme);
    LOCAL_HOSTS.contains(&host)
        || host.starts_with("192.168.")
        || host.starts_with("10.")
        || host
            .strip_prefix("172.")
            .and_then(|rest| rest.split('.').next())
            .and_then(|second_octet| second_octet.parse::<u8>().ok())
            .is_some_and(|second_octet| (16..=31).contains(&second_octet))
}

/// Build the full, deterministically sorted model list from a live
/// `ModelRuntime`'s composed providers.
///
/// Caller contract: pass `runtime.providers()` (`crate::models::ModelRuntime::providers`,
/// `crate::models`:3409) from the same `ModelRuntime` the session's model
/// resolution uses, so the picker offers exactly the models `--list-models`
/// would. This function does no I/O and holds nothing from `providers`
/// afterward — the result is fully owned.
///
/// Sort: provider id, then model id, both via [`locale_compare`] — the same
/// ICU-root-locale-shaped comparison `list_models` sorts with
/// (`crate::models`:3821), so the picker's order matches `--list-models`'s.
pub fn load_model_entries(providers: &[ComposedProvider]) -> Vec<ModelEntry> {
    let mut entries: Vec<ModelEntry> = providers
        .iter()
        .flat_map(|provider| {
            let provider_id = provider.id.clone();
            provider.models.iter().map(move |model| ModelEntry {
                provider: provider_id.clone(),
                model_id: model.id.clone(),
                display_name: model.name.clone(),
                context_window: model.context_window,
                reasoning: model.reasoning,
                local: is_local_base_url(&model.base_url),
            })
        })
        .collect();
    entries.sort_unstable_by(|a, b| {
        locale_compare(&a.provider, &b.provider)
            .then_with(|| locale_compare(&a.model_id, &b.model_id))
    });
    entries
}

// =============================================================================
// Data: sessions
// =============================================================================

/// One resumable session, projected out of a [`SessionInfo`] plus a
/// caller-supplied model lookup (see module docs for why model is separate).
#[derive(Debug, Clone, PartialEq)]
pub struct SessionEntry {
    /// `SessionInfo::id` — the session id `SessionManager::open`/`continue_recent`
    /// (`crate::session`) would take to resume it.
    pub id: String,
    /// `SessionInfo::name` when set (the latest `session_info` rename), else
    /// the first user message's text (`SessionInfo::first_message`, which is
    /// itself already `"(no messages)"` for an empty session — Pi's own
    /// fallback text, not one invented here).
    pub title: String,
    /// `SessionInfo::cwd`.
    pub cwd: String,
    /// `SessionInfo::modified` (Pi's last-activity millisecond timestamp)
    /// converted to a `SystemTime`. `None` only when the source timestamp
    /// doesn't fit in `u64` milliseconds (i.e. is negative) — practically
    /// never, but not worth a panic over.
    pub modified: Option<SystemTime>,
    /// The session's current model, if the caller could supply one. See the
    /// module docs: `SessionInfo` alone does not carry this.
    pub model: Option<String>,
}

/// `SessionInfo::modified` is milliseconds since the Unix epoch (Pi's
/// `Date.now()`-shaped last-activity timestamp — see `crate::session`:1348).
/// `SystemTime` has no negative-offset representation here, so a negative
/// input (a corrupt or pre-epoch timestamp) maps to `None` rather than
/// panicking or silently clamping to the epoch.
fn millis_to_system_time(millis: i64) -> Option<SystemTime> {
    let millis = u64::try_from(millis).ok()?;
    UNIX_EPOCH.checked_add(Duration::from_millis(millis))
}

/// Build the session list, newest first, from a slice of [`SessionInfo`].
///
/// Caller contract: pass the result of `SessionManager::list_all(None, None)`
/// (`crate::session`:950) — or `::list` (`crate::session`:910) to scope to
/// one cwd — from the session store the `/resume` command should search.
/// Both already sort by `modified` descending; this function re-sorts
/// defensively so it does not silently depend on that caller-side guarantee.
///
/// `models_by_id` is looked up by `SessionInfo::id`; see the module docs for
/// why this is a separate parameter rather than something derived here. Pass
/// `&HashMap::new()` if the caller has no model information yet.
pub fn load_session_entries(
    infos: &[SessionInfo],
    models_by_id: &HashMap<String, String>,
) -> Vec<SessionEntry> {
    let mut ordered: Vec<&SessionInfo> = infos.iter().collect();
    ordered.sort_by_key(|info| std::cmp::Reverse(info.modified));
    ordered
        .into_iter()
        .map(|info| SessionEntry {
            id: info.id.clone(),
            title: info
                .name
                .clone()
                .unwrap_or_else(|| info.first_message.clone()),
            cwd: info.cwd.clone(),
            modified: millis_to_system_time(info.modified),
            model: models_by_id.get(&info.id).cloned(),
        })
        .collect()
}

// =============================================================================
// Shared picker plumbing
// =============================================================================

/// What a key press did, for the caller to act on. `handle_key` never acts
/// on the host `InteractiveMode` itself (no session mutation, no modal
/// close) — it only updates the picker's own state and reports what
/// happened, so the caller stays in control of everything outside this
/// module, per the task split.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerAction {
    /// The key was consumed (navigation, filter edit) with nothing further
    /// for the caller to do; re-render the picker.
    None,
    /// Esc: the caller should close the picker.
    Dismissed,
    /// Enter on a real row: the index into the *original* entries `Vec`
    /// passed to `new` (not the filtered view, which is why this is stable
    /// across filter changes) — `usize::MAX`-free by construction, since
    /// it is only ever produced from a valid `filtered` lookup.
    Selected(usize),
}

/// One cached render: a render is only rebuilt when `width`, `revision`
/// (bumped on every entries/filter mutation, not on plain scrolling), or the
/// selection/scroll position change. `Component::render`'s signature returns
/// an owned `Vec<String>`, so a cache *hit* still pays for one `Vec<String>`
/// clone — there is no way to hand back a `&[String]` through that trait —
/// but it skips the expensive part entirely: no re-filtering, re-scoring,
/// re-sorting, or column re-formatting.
struct RenderCache {
    width: usize,
    revision: u64,
    selected: usize,
    scroll: usize,
    lines: Vec<String>,
}

/// Column tiers the pickers degrade through as `width` shrinks. Optional
/// columns drop before anything gets truncated harder than necessary; the
/// last-resort primary column (model id / title) is truncated only in
/// [`ColumnTier::Minimal`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ColumnTier {
    /// Every column shown.
    Full,
    /// Secondary columns dropped; primary + one identifying column left.
    Compact,
    /// Only the primary column, aggressively truncated if needed.
    Minimal,
}

/// Split `query` into fuzzy-match tokens on whitespace/`/`, mirroring
/// `pirust_tui::fuzzy::fuzzy_filter`'s own tokenization (`fuzzy.rs`:181-185)
/// so multi-word queries keep its AND-across-tokens semantics. Deliberately
/// reimplemented rather than calling `fuzzy_filter` itself: that function
/// allocates a fresh `Vec<(&T, f64)>` *and* a fresh `Vec<&T>` on every call
/// (`fuzzy.rs`:190,210) with no way to reuse a caller-owned buffer, which is
/// exactly the per-keystroke allocation the perf mandate rules out. Calling
/// the lower-level `fuzzy_match` once per token per candidate keeps the
/// scoring identical while letting the pickers own (and reuse) the output
/// buffers.
fn split_query_tokens(query: &str) -> Vec<&str> {
    query
        .split(|c: char| c.is_whitespace() || c == '/')
        .filter(|t| !t.is_empty())
        .collect()
}

/// Score every candidate in `filtered` against `query`'s tokens (all must
/// match, scores summed — same rule as `fuzzy_filter`) using `haystacks[idx]`
/// as the searched text, writing surviving `(index, score)` pairs into
/// `scored`. `scored` is cleared first, so its backing allocation is reused
/// across calls (no reallocation once warm).
///
/// Ties broken by index (i.e. the original provider/id or modified-time
/// sort order fed into `new`/reload), not further fuzzy scoring — documented
/// here since `sort_unstable_by` is otherwise free to reorder equal-score
/// pairs arbitrarily.
fn score_candidates(
    haystacks: &[String],
    filtered: &[u32],
    query: &str,
    scored: &mut Vec<(u32, f64)>,
) {
    scored.clear();
    let tokens = split_query_tokens(query);
    if tokens.is_empty() {
        scored.extend(filtered.iter().map(|&idx| (idx, 0.0)));
        return;
    }
    for &idx in filtered {
        let haystack = &haystacks[idx as usize];
        let mut total = 0.0f64;
        let mut all_match = true;
        for token in &tokens {
            let m = fuzzy_match(token, haystack);
            if m.matches {
                total += m.score;
            } else {
                all_match = false;
                break;
            }
        }
        if all_match {
            scored.push((idx, total));
        }
    }
    scored.sort_unstable_by(|a, b| {
        a.1.partial_cmp(&b.1)
            .unwrap_or(CmpOrdering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
}

// A `clamp_index` helper used to live here. It is unnecessary: the two places
// that can move the selection out of range already close it at the source —
// `move_selection` clamps with `.min(filtered.len() - 1)` on the way up and
// `saturating_sub` on the way down, and `rescan` (the only thing that can
// *shrink* `filtered`) resets `selected` to 0. `handle_key` then reads through
// `filtered.get(selected)`, so even an out-of-range index could only ever
// produce `PickerAction::None`, never a wrong selection.

/// Slide `scroll` so `selected` stays within the `max_visible`-row viewport
/// `[scroll, scroll + max_visible)`.
fn clamp_scroll(selected: usize, scroll: usize, max_visible: usize) -> usize {
    if max_visible == 0 {
        return 0;
    }
    if selected < scroll {
        selected
    } else if selected >= scroll + max_visible {
        selected + 1 - max_visible
    } else {
        scroll
    }
}

/// `context_window` formatted compactly for a narrow column, e.g.
/// `1_000_000` → `"1.0M"`, `200_000` → `"200K"`, `8_000` → `"8K"`,
/// `512` → `"512"`.
///
/// `K` from 1,000 up, not from 10,000: `docs/tui-design-samples.html` §6's
/// model picker writes a small local model's window as `32K`, and "8K
/// context" is how model documentation states it. (An earlier version of this
/// doc comment and its test both claimed `8_000` → `"8000"`, contradicting
/// the code below and the spec; the code was right.)
fn format_context_window(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{}K", tokens / 1_000)
    } else {
        tokens.to_string()
    }
}

/// `modified` formatted relative to `now` — no calendar/timezone math (no
/// date/time crate is a dependency here, and adding one is out of scope),
/// just coarse buckets. Good enough for "how stale is this session" at a
/// glance; exact wall-clock display is a caller-side enhancement if wanted.
fn format_relative_age(modified: SystemTime, now: SystemTime) -> String {
    let age = match now.duration_since(modified) {
        Ok(d) => d,
        // `modified` is in the future (clock skew, or a just-written
        // session racing this render) — treat as "now" rather than
        // underflow-panicking on the reversed `duration_since`.
        Err(_) => Duration::ZERO,
    };
    let secs = age.as_secs();
    if secs < 60 {
        "just now".to_string()
    } else if secs < 3_600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86_400 {
        format!("{}h ago", secs / 3_600)
    } else if secs < 86_400 * 14 {
        format!("{}d ago", secs / 86_400)
    } else {
        format!("{}w ago", secs / (86_400 * 7))
    }
}

// =============================================================================
// ModelPicker
// =============================================================================

/// The real `/model` picker: fuzzy-searchable over provider / model id /
/// display name, showing context window, reasoning support, and
/// local-vs-remote, with clamped ↑/↓ navigation and a scrolled viewport.
pub struct ModelPicker {
    /// Owned once at construction (or reload); never cloned per keystroke.
    entries: Vec<ModelEntry>,
    /// `"{provider} {model_id} {display_name}"` per entry, built once
    /// alongside `entries` so filtering never re-derives searchable text.
    haystacks: Vec<String>,
    filter: String,
    /// Indices into `entries`/`haystacks` currently matching `filter`,
    /// best-scored first when `filter` is non-empty. Reused buffer: every
    /// mutation does `clear()` + `extend()`, never a fresh `Vec`.
    filtered: Vec<u32>,
    /// Scratch `(index, score)` buffer reused by [`score_candidates`] across
    /// every keystroke — cleared, not reallocated.
    scored: Vec<(u32, f64)>,
    /// Index into `filtered` (not `entries`) of the highlighted row.
    selected: usize,
    /// Index into `filtered` of the first visible row.
    scroll: usize,
    /// Rows shown at once. `Component::render` only receives a `width`, not
    /// a height (matching `pirust_tui::components::select_list::SelectList`,
    /// whose own `max_visible` is likewise constructor-set, not
    /// render-time-computed) — so the caller is responsible for sizing this
    /// from the terminal's available rows and updating it via
    /// [`ModelPicker::set_max_visible`] on resize.
    max_visible: usize,
    /// Bumped on every entries/filter mutation (not on plain scrolling) —
    /// the render-cache key that lets a cache hit skip re-filtering.
    revision: u64,
    cache: Option<RenderCache>,
}

impl ModelPicker {
    /// Build a picker over `entries` (see [`load_model_entries`]), showing
    /// `max_visible` rows at a time (clamped to at least 1).
    pub fn new(entries: Vec<ModelEntry>, max_visible: usize) -> Self {
        let haystacks = entries
            .iter()
            .map(|e| format!("{} {} {}", e.provider, e.model_id, e.display_name))
            .collect();
        let len = entries.len();
        let mut filtered = Vec::with_capacity(len);
        filtered.extend(0u32..len as u32);
        Self {
            entries,
            haystacks,
            filter: String::new(),
            filtered,
            scored: Vec::new(),
            selected: 0,
            scroll: 0,
            max_visible: max_visible.max(1),
            revision: 0,
            cache: None,
        }
    }

    /// Replace the row budget (e.g. on a terminal resize). Does not change
    /// `filter`/`selected`; only re-clamps scroll to the new viewport.
    pub fn set_max_visible(&mut self, max_visible: usize) {
        self.max_visible = max_visible.max(1);
        self.scroll = clamp_scroll(self.selected, self.scroll, self.max_visible);
        self.cache = None;
    }

    /// The current filter text.
    pub fn filter(&self) -> &str {
        &self.filter
    }

    /// How many entries currently match the filter.
    pub fn match_count(&self) -> usize {
        self.filtered.len()
    }

    /// How many entries exist in total, filter aside.
    pub fn total_count(&self) -> usize {
        self.entries.len()
    }

    /// The entry the highlighted row points at, if any (empty result set
    /// aside, this is always `Some` — `selected` is clamped on every
    /// mutation).
    pub fn selected_entry(&self) -> Option<&ModelEntry> {
        self.filtered
            .get(self.selected)
            .map(|&idx| &self.entries[idx as usize])
    }

    /// Re-score `filtered` (already-narrowed candidates when appending a
    /// character; the full `0..len` range when the filter shrank) against
    /// `query`, then rebuild `filtered` from the sorted scores. Selection
    /// clamps to the new (possibly shorter) list; scroll resets to top,
    /// matching the old pickers' behavior on a filter edit.
    fn rescan(&mut self, query: &str) {
        if query.trim().is_empty() {
            self.filtered.sort_unstable();
        } else {
            score_candidates(&self.haystacks, &self.filtered, query, &mut self.scored);
            self.filtered.clear();
            self.filtered
                .extend(self.scored.iter().map(|&(idx, _)| idx));
        }
        self.selected = 0;
        self.scroll = 0;
        self.revision += 1;
        self.cache = None;
    }

    /// Append one non-control character to the filter and rescan.
    ///
    /// Correctness note for why this only rescans the *current* `filtered`
    /// set rather than every entry: fuzzy matching requires the query to be
    /// an in-order subsequence of the haystack. If the shorter query
    /// (`Q`) was not a subsequence of some haystack, then `Q` followed by
    /// one more character can't be either — any witness subsequence for the
    /// longer query's first `|Q|` positions would itself witness `Q` — so
    /// restricting the rescan to entries that already passed the shorter
    /// query is not just an optimization, it cannot change the result. This
    /// makes a keystroke O(current `filtered` size), not O(total entries).
    pub fn push_filter_char(&mut self, ch: char) {
        if ch.is_control() {
            return;
        }
        let mut filter = std::mem::take(&mut self.filter);
        filter.push(ch);
        self.rescan(filter.trim());
        self.filter = filter;
    }

    /// Remove the last filter character and rescan. Shrinking the filter can
    /// only ever *grow* the match set, so (unlike appending) this must
    /// rescan every entry, not just the current `filtered` subset.
    pub fn pop_filter_char(&mut self) {
        let mut filter = std::mem::take(&mut self.filter);
        let popped = filter.pop().is_some();
        if popped {
            let len = self.entries.len();
            self.filtered.clear();
            self.filtered.extend(0u32..len as u32);
            self.rescan(filter.trim());
        }
        self.filter = filter;
    }

    /// Move the selection by `delta` rows, clamped to `[0, filtered.len())`
    /// — never wrapping and never running past the end, unlike the fake
    /// picker's unbounded `picker.selected += 1`.
    pub fn move_selection(&mut self, delta: i32) {
        if self.filtered.is_empty() {
            return;
        }
        let max = self.filtered.len() - 1;
        let next = if delta < 0 {
            self.selected.saturating_sub(delta.unsigned_abs() as usize)
        } else {
            (self.selected + delta as usize).min(max)
        };
        if next != self.selected {
            self.selected = next;
            self.scroll = clamp_scroll(self.selected, self.scroll, self.max_visible);
            self.cache = None;
        }
    }

    /// Route one raw input sequence: Esc dismisses, Enter selects, ↑/↓
    /// navigate (clamped), Backspace edits the filter, and any other single
    /// non-control character extends it. Everything else (multi-byte
    /// sequences that aren't a recognized key, e.g. an unmapped function
    /// key) is ignored.
    pub fn handle_key(&mut self, data: &str) -> PickerAction {
        if matches_key(data, "escape") {
            return PickerAction::Dismissed;
        }
        if matches_key(data, "enter") {
            return match self.filtered.get(self.selected) {
                Some(&idx) => PickerAction::Selected(idx as usize),
                None => PickerAction::None,
            };
        }
        if matches_key(data, "up") {
            self.move_selection(-1);
            return PickerAction::None;
        }
        if matches_key(data, "down") {
            self.move_selection(1);
            return PickerAction::None;
        }
        if matches_key(data, "backspace") {
            self.pop_filter_char();
            return PickerAction::None;
        }
        let mut chars = data.chars();
        if let (Some(ch), None) = (chars.next(), chars.next()) {
            self.push_filter_char(ch);
        }
        PickerAction::None
    }

    fn column_tier(width: usize) -> ColumnTier {
        if width >= 70 {
            ColumnTier::Full
        } else if width >= 30 {
            ColumnTier::Compact
        } else {
            ColumnTier::Minimal
        }
    }

    fn render_uncached(&self, width: usize) -> Vec<String> {
        let mut lines = Vec::with_capacity(self.max_visible + 2);
        lines.push(format!("Select model (filter: {})", self.filter));

        if self.filtered.is_empty() {
            let msg = if self.entries.is_empty() {
                "  (no models available)"
            } else {
                "  (no matching models)"
            };
            lines.push(msg.to_string());
            lines.push("↑/↓ navigate · Enter select · Esc dismiss".to_string());
            return lines;
        }

        let tier = Self::column_tier(width);
        let end = (self.scroll + self.max_visible).min(self.filtered.len());
        for row in self.scroll..end {
            let idx = self.filtered[row] as usize;
            let entry = &self.entries[idx];
            let is_selected = row == self.selected;
            lines.push(Self::format_row(entry, tier, width, is_selected));
        }

        if self.scroll > 0 || end < self.filtered.len() {
            lines.push(format!("  ({}/{})", self.selected + 1, self.filtered.len()));
        }
        lines.push("↑/↓ navigate · Enter select · Esc dismiss".to_string());
        lines
    }

    fn format_row(entry: &ModelEntry, tier: ColumnTier, width: usize, is_selected: bool) -> String {
        let marker = if is_selected { "> " } else { "  " };
        let local_flag = if entry.local { "L" } else { "R" };
        let body = match tier {
            ColumnTier::Minimal => {
                let id_width = width.saturating_sub(marker.len()).max(4);
                truncate_to_width(&entry.model_id, id_width, "…", false)
            }
            ColumnTier::Compact => {
                let provider = truncate_to_width(&entry.provider, 12, "…", true);
                let reserved = marker.len() + local_flag.len() + 1 + provider.chars().count() + 1;
                let id_width = width.saturating_sub(reserved).max(4);
                let id = truncate_to_width(&entry.model_id, id_width, "…", false);
                format!("{local_flag} {provider} {id}")
            }
            ColumnTier::Full => {
                let provider = truncate_to_width(&entry.provider, 14, "…", true);
                let ctx = format_context_window(entry.context_window);
                let reasoning = if entry.reasoning { "think" } else { "-" };
                let reserved = marker.len()
                    + local_flag.len()
                    + 1
                    + provider.chars().count()
                    + 1
                    + 8 // context column
                    + 1
                    + 5 // reasoning column
                    + 1;
                let id_width = width.saturating_sub(reserved).max(4);
                let id = truncate_to_width(&entry.model_id, id_width, "…", false);
                format!("{local_flag} {provider} {id:<id_width$} {ctx:>7} {reasoning:<5}")
            }
        };
        let line = format!("{marker}{body}");
        if is_selected {
            fg(dark::TEXT)(&line)
        } else {
            line
        }
    }
}

impl Component for ModelPicker {
    fn invalidate(&mut self) {
        self.cache = None;
    }

    fn render(&mut self, width: usize) -> Vec<String> {
        if let Some(cache) = &self.cache {
            if cache.width == width
                && cache.revision == self.revision
                && cache.selected == self.selected
                && cache.scroll == self.scroll
            {
                return cache.lines.clone();
            }
        }
        let lines = self.render_uncached(width);
        self.cache = Some(RenderCache {
            width,
            revision: self.revision,
            selected: self.selected,
            scroll: self.scroll,
            lines: lines.clone(),
        });
        lines
    }

    fn handle_input(&mut self, data: &str) {
        let _ = self.handle_key(data);
    }
}

// =============================================================================
// SessionPicker
// =============================================================================

/// The real `/resume` picker: fuzzy-searchable over title / cwd / model,
/// showing modified time, with the same clamped navigation and viewport
/// scrolling as [`ModelPicker`].
pub struct SessionPicker {
    entries: Vec<SessionEntry>,
    /// `"{title} {cwd} {model}"` per entry, built once.
    haystacks: Vec<String>,
    filter: String,
    filtered: Vec<u32>,
    scored: Vec<(u32, f64)>,
    selected: usize,
    scroll: usize,
    max_visible: usize,
    revision: u64,
    cache: Option<RenderCache>,
}

impl SessionPicker {
    /// Build a picker over `entries` (see [`load_session_entries`]), newest
    /// first as `load_session_entries` already sorted them.
    pub fn new(entries: Vec<SessionEntry>, max_visible: usize) -> Self {
        let haystacks = entries
            .iter()
            .map(|e| format!("{} {} {}", e.title, e.cwd, e.model.as_deref().unwrap_or("")))
            .collect();
        let len = entries.len();
        let mut filtered = Vec::with_capacity(len);
        filtered.extend(0u32..len as u32);
        Self {
            entries,
            haystacks,
            filter: String::new(),
            filtered,
            scored: Vec::new(),
            selected: 0,
            scroll: 0,
            max_visible: max_visible.max(1),
            revision: 0,
            cache: None,
        }
    }

    /// See [`ModelPicker::set_max_visible`].
    pub fn set_max_visible(&mut self, max_visible: usize) {
        self.max_visible = max_visible.max(1);
        self.scroll = clamp_scroll(self.selected, self.scroll, self.max_visible);
        self.cache = None;
    }

    /// The current filter text.
    pub fn filter(&self) -> &str {
        &self.filter
    }

    /// How many entries currently match the filter.
    pub fn match_count(&self) -> usize {
        self.filtered.len()
    }

    /// How many entries exist in total, filter aside.
    pub fn total_count(&self) -> usize {
        self.entries.len()
    }

    /// The entry the highlighted row points at, if any.
    pub fn selected_entry(&self) -> Option<&SessionEntry> {
        self.filtered
            .get(self.selected)
            .map(|&idx| &self.entries[idx as usize])
    }

    fn rescan(&mut self, query: &str) {
        if query.trim().is_empty() {
            self.filtered.sort_unstable();
        } else {
            score_candidates(&self.haystacks, &self.filtered, query, &mut self.scored);
            self.filtered.clear();
            self.filtered
                .extend(self.scored.iter().map(|&(idx, _)| idx));
        }
        self.selected = 0;
        self.scroll = 0;
        self.revision += 1;
        self.cache = None;
    }

    /// See [`ModelPicker::push_filter_char`] — same monotonic-subsequence
    /// argument applies verbatim.
    pub fn push_filter_char(&mut self, ch: char) {
        if ch.is_control() {
            return;
        }
        let mut filter = std::mem::take(&mut self.filter);
        filter.push(ch);
        self.rescan(filter.trim());
        self.filter = filter;
    }

    /// See [`ModelPicker::pop_filter_char`].
    pub fn pop_filter_char(&mut self) {
        let mut filter = std::mem::take(&mut self.filter);
        let popped = filter.pop().is_some();
        if popped {
            let len = self.entries.len();
            self.filtered.clear();
            self.filtered.extend(0u32..len as u32);
            self.rescan(filter.trim());
        }
        self.filter = filter;
    }

    /// See [`ModelPicker::move_selection`].
    pub fn move_selection(&mut self, delta: i32) {
        if self.filtered.is_empty() {
            return;
        }
        let max = self.filtered.len() - 1;
        let next = if delta < 0 {
            self.selected.saturating_sub(delta.unsigned_abs() as usize)
        } else {
            (self.selected + delta as usize).min(max)
        };
        if next != self.selected {
            self.selected = next;
            self.scroll = clamp_scroll(self.selected, self.scroll, self.max_visible);
            self.cache = None;
        }
    }

    /// See [`ModelPicker::handle_key`].
    pub fn handle_key(&mut self, data: &str) -> PickerAction {
        if matches_key(data, "escape") {
            return PickerAction::Dismissed;
        }
        if matches_key(data, "enter") {
            return match self.filtered.get(self.selected) {
                Some(&idx) => PickerAction::Selected(idx as usize),
                None => PickerAction::None,
            };
        }
        if matches_key(data, "up") {
            self.move_selection(-1);
            return PickerAction::None;
        }
        if matches_key(data, "down") {
            self.move_selection(1);
            return PickerAction::None;
        }
        if matches_key(data, "backspace") {
            self.pop_filter_char();
            return PickerAction::None;
        }
        let mut chars = data.chars();
        if let (Some(ch), None) = (chars.next(), chars.next()) {
            self.push_filter_char(ch);
        }
        PickerAction::None
    }

    fn column_tier(width: usize) -> ColumnTier {
        if width >= 70 {
            ColumnTier::Full
        } else if width >= 30 {
            ColumnTier::Compact
        } else {
            ColumnTier::Minimal
        }
    }

    fn render_uncached(&self, width: usize) -> Vec<String> {
        let mut lines = Vec::with_capacity(self.max_visible + 2);
        lines.push(format!("Resume a session (filter: {})", self.filter));

        if self.filtered.is_empty() {
            let msg = if self.entries.is_empty() {
                "  (no resumable sessions)"
            } else {
                "  (no matching sessions)"
            };
            lines.push(msg.to_string());
            lines.push("↑/↓ navigate · Enter resume · Esc dismiss".to_string());
            return lines;
        }

        let tier = Self::column_tier(width);
        let now = SystemTime::now();
        let end = (self.scroll + self.max_visible).min(self.filtered.len());
        for row in self.scroll..end {
            let idx = self.filtered[row] as usize;
            let entry = &self.entries[idx];
            let is_selected = row == self.selected;
            lines.push(Self::format_row(entry, tier, width, is_selected, now));
        }

        if self.scroll > 0 || end < self.filtered.len() {
            lines.push(format!("  ({}/{})", self.selected + 1, self.filtered.len()));
        }
        lines.push("↑/↓ navigate · Enter resume · Esc dismiss".to_string());
        lines
    }

    fn format_row(
        entry: &SessionEntry,
        tier: ColumnTier,
        width: usize,
        is_selected: bool,
        now: SystemTime,
    ) -> String {
        let marker = if is_selected { "> " } else { "  " };
        let body = match tier {
            ColumnTier::Minimal => {
                let title_width = width.saturating_sub(marker.len()).max(4);
                truncate_to_width(&entry.title, title_width, "…", false)
            }
            ColumnTier::Compact => {
                let cwd = truncate_to_width(&entry.cwd, 20, "…", true);
                let reserved = marker.len() + cwd.chars().count() + 3;
                let title_width = width.saturating_sub(reserved).max(4);
                let title = truncate_to_width(&entry.title, title_width, "…", false);
                format!("{title} — {cwd}")
            }
            ColumnTier::Full => {
                let cwd = truncate_to_width(&entry.cwd, 24, "…", true);
                let age = entry
                    .modified
                    .map(|m| format_relative_age(m, now))
                    .unwrap_or_else(|| "-".to_string());
                let model = entry.model.as_deref().unwrap_or("-");
                let model = truncate_to_width(model, 16, "…", true);
                let reserved = marker.len()
                    + cwd.chars().count()
                    + 3
                    + 10 // age column
                    + 1
                    + model.chars().count()
                    + 1;
                let title_width = width.saturating_sub(reserved).max(4);
                let title = truncate_to_width(&entry.title, title_width, "…", false);
                format!("{title:<title_width$} — {cwd}  {age:<10} {model}")
            }
        };
        let line = format!("{marker}{body}");
        if is_selected {
            fg(dark::TEXT)(&line)
        } else {
            line
        }
    }
}

impl Component for SessionPicker {
    fn invalidate(&mut self) {
        self.cache = None;
    }

    fn render(&mut self, width: usize) -> Vec<String> {
        if let Some(cache) = &self.cache {
            if cache.width == width
                && cache.revision == self.revision
                && cache.selected == self.selected
                && cache.scroll == self.scroll
            {
                return cache.lines.clone();
            }
        }
        let lines = self.render_uncached(width);
        self.cache = Some(RenderCache {
            width,
            revision: self.revision,
            selected: self.selected,
            scroll: self.scroll,
            lines: lines.clone(),
        });
        lines
    }

    fn handle_input(&mut self, data: &str) {
        let _ = self.handle_key(data);
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    // Straight from `pirust_ai::types`, not through `crate::models`'s
    // re-export: that re-export is private, so naming it here does not
    // compile under `cfg(test)`.
    use crate::session::SessionInfo as RealSessionInfo;
    use pirust_ai::types::model::{Modality, Model, ModelCost, ModelCostRates};
    use pirust_ai::types::{Api, ProviderId};

    // -- fixtures ------------------------------------------------------

    fn model(
        provider: &str,
        id: &str,
        name: &str,
        ctx: u64,
        reasoning: bool,
        base_url: &str,
    ) -> Model {
        Model {
            id: id.to_string(),
            name: name.to_string(),
            api: Api::from("anthropic-messages"),
            provider: ProviderId::from(provider),
            base_url: base_url.to_string(),
            reasoning,
            thinking_level_map: None,
            input: vec![Modality::Text],
            cost: ModelCost {
                rates: ModelCostRates {
                    input: 0.0,
                    output: 0.0,
                    cache_read: 0.0,
                    cache_write: 0.0,
                },
                tiers: None,
            },
            context_window: ctx,
            max_tokens: 4096,
            headers: None,
            compat: None,
        }
    }

    fn sample_providers() -> Vec<ComposedProvider> {
        vec![
            ComposedProvider {
                id: "anthropic".to_string(),
                name: "Anthropic".to_string(),
                base_url: Some("https://api.anthropic.com".to_string()),
                models: vec![
                    model(
                        "anthropic",
                        "claude-opus-4-8",
                        "Claude Opus 4.8",
                        1_000_000,
                        true,
                        "https://api.anthropic.com",
                    ),
                    model(
                        "anthropic",
                        "claude-sonnet-4-5",
                        "Claude Sonnet 4.5",
                        200_000,
                        true,
                        "https://api.anthropic.com",
                    ),
                ],
            },
            ComposedProvider {
                id: "localproxy".to_string(),
                name: "Local Proxy".to_string(),
                base_url: Some("http://localhost:8080".to_string()),
                models: vec![model(
                    "localproxy",
                    "llama-3-70b",
                    "Llama 3 70B",
                    8_192,
                    false,
                    "http://localhost:8080",
                )],
            },
        ]
    }

    fn many_model_entries(n: usize) -> Vec<ModelEntry> {
        (0..n)
            .map(|i| ModelEntry {
                provider: "prov".to_string(),
                model_id: format!("model-{i:03}"),
                display_name: format!("Model {i}"),
                context_window: 100_000,
                reasoning: i % 2 == 0,
                local: i % 3 == 0,
            })
            .collect()
    }

    fn sample_session_infos() -> Vec<RealSessionInfo> {
        vec![
            RealSessionInfo {
                path: "/sessions/a.jsonl".to_string(),
                id: "session-a".to_string(),
                cwd: "/repo/a".to_string(),
                name: Some("Refactor auth".to_string()),
                parent_session_path: None,
                created: Some(1_000),
                modified: 2_000,
                message_count: 4,
                first_message: "hello".to_string(),
                all_messages_text: "hello world".to_string(),
            },
            RealSessionInfo {
                path: "/sessions/b.jsonl".to_string(),
                id: "session-b".to_string(),
                cwd: "/repo/b".to_string(),
                name: None,
                parent_session_path: None,
                created: Some(500),
                modified: 9_000,
                message_count: 1,
                first_message: "(no messages)".to_string(),
                all_messages_text: String::new(),
            },
        ]
    }

    // -- load_model_entries ---------------------------------------------

    #[test]
    fn load_model_entries_flattens_and_sorts_deterministically() {
        let providers = sample_providers();
        let entries = load_model_entries(&providers);
        assert_eq!(entries.len(), 3);
        // anthropic < localproxy (locale_compare), and within anthropic
        // claude-opus-4-8 < claude-sonnet-4-5.
        assert_eq!(entries[0].provider, "anthropic");
        assert_eq!(entries[0].model_id, "claude-opus-4-8");
        assert_eq!(entries[1].provider, "anthropic");
        assert_eq!(entries[1].model_id, "claude-sonnet-4-5");
        assert_eq!(entries[2].provider, "localproxy");
    }

    #[test]
    fn load_model_entries_marks_local_base_url() {
        let providers = sample_providers();
        let entries = load_model_entries(&providers);
        let remote = entries
            .iter()
            .find(|e| e.model_id == "claude-opus-4-8")
            .unwrap();
        assert!(!remote.local);
        let local = entries
            .iter()
            .find(|e| e.model_id == "llama-3-70b")
            .unwrap();
        assert!(local.local);
    }

    #[test]
    fn is_local_base_url_covers_private_ranges() {
        assert!(is_local_base_url("http://localhost:1234"));
        assert!(is_local_base_url("http://127.0.0.1:11434"));
        assert!(is_local_base_url("http://192.168.1.5:8000"));
        assert!(is_local_base_url("http://10.0.0.4/v1"));
        assert!(is_local_base_url("http://172.20.0.2/v1"));
        assert!(!is_local_base_url("http://172.40.0.2/v1"));
        assert!(!is_local_base_url("https://api.anthropic.com"));
        assert!(!is_local_base_url("https://openrouter.ai/api/v1"));
    }

    // -- load_session_entries ---------------------------------------------

    #[test]
    fn load_session_entries_sorts_newest_first_and_uses_name_or_first_message() {
        let infos = sample_session_infos();
        let entries = load_session_entries(&infos, &HashMap::new());
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].id, "session-b"); // modified: 9000, newer
        assert_eq!(entries[0].title, "(no messages)"); // no name -> first_message
        assert_eq!(entries[1].id, "session-a");
        assert_eq!(entries[1].title, "Refactor auth"); // named session
    }

    #[test]
    fn load_session_entries_looks_up_model_by_id_and_leaves_unknown_blank() {
        let infos = sample_session_infos();
        let mut models = HashMap::new();
        models.insert("session-a".to_string(), "claude-opus-4-8".to_string());
        let entries = load_session_entries(&infos, &models);
        let a = entries.iter().find(|e| e.id == "session-a").unwrap();
        let b = entries.iter().find(|e| e.id == "session-b").unwrap();
        assert_eq!(a.model.as_deref(), Some("claude-opus-4-8"));
        assert_eq!(b.model, None);
    }

    #[test]
    fn load_session_entries_converts_modified_millis_to_system_time() {
        let infos = sample_session_infos();
        let entries = load_session_entries(&infos, &HashMap::new());
        let a = entries.iter().find(|e| e.id == "session-a").unwrap();
        let expected = UNIX_EPOCH + Duration::from_millis(2_000);
        assert_eq!(a.modified, Some(expected));
    }

    // -- ModelPicker: navigation clamps ------------------------------------

    #[test]
    fn model_picker_down_clamps_at_last_row_not_wraps() {
        let mut picker = ModelPicker::new(many_model_entries(3), 10);
        picker.move_selection(1);
        picker.move_selection(1);
        picker.move_selection(1); // one past the end
        picker.move_selection(1); // and again
        assert_eq!(picker.selected_entry().unwrap().model_id, "model-002");
    }

    #[test]
    fn model_picker_up_clamps_at_first_row_not_wraps() {
        let mut picker = ModelPicker::new(many_model_entries(3), 10);
        picker.move_selection(-1);
        picker.move_selection(-1);
        assert_eq!(picker.selected_entry().unwrap().model_id, "model-000");
    }

    #[test]
    fn model_picker_handle_key_enter_reports_original_index_not_filtered_index() {
        let mut picker = ModelPicker::new(many_model_entries(5), 10);
        for ch in "model-004".chars() {
            picker.push_filter_char(ch);
        }
        assert_eq!(picker.match_count(), 1);
        match picker.handle_key("\r") {
            PickerAction::Selected(idx) => assert_eq!(idx, 4),
            other => panic!("expected Selected(4), got {other:?}"),
        }
    }

    #[test]
    fn model_picker_escape_dismisses() {
        let mut picker = ModelPicker::new(many_model_entries(3), 10);
        assert_eq!(picker.handle_key("\u{1b}"), PickerAction::Dismissed);
    }

    // -- ModelPicker: fuzzy filtering --------------------------------------

    #[test]
    fn model_picker_filters_by_fuzzy_substring() {
        let mut picker = ModelPicker::new(many_model_entries(20), 10);
        for ch in "015".chars() {
            picker.push_filter_char(ch);
        }
        assert_eq!(picker.match_count(), 1);
        assert_eq!(picker.selected_entry().unwrap().model_id, "model-015");
    }

    #[test]
    fn model_picker_backspace_restores_wider_match_set() {
        let mut picker = ModelPicker::new(many_model_entries(20), 10);
        for ch in "model-01".chars() {
            picker.push_filter_char(ch);
        }
        let narrowed = picker.match_count();
        assert!(narrowed > 1); // model-010..model-019
        picker.pop_filter_char();
        picker.pop_filter_char();
        assert!(picker.match_count() > narrowed);
    }

    #[test]
    fn model_picker_no_matches_reports_no_selection() {
        let mut picker = ModelPicker::new(many_model_entries(3), 10);
        for ch in "zzz-nonexistent".chars() {
            picker.push_filter_char(ch);
        }
        assert_eq!(picker.match_count(), 0);
        assert!(picker.selected_entry().is_none());
        assert_eq!(picker.handle_key("\r"), PickerAction::None);
    }

    // -- ModelPicker: rendering / column degradation -----------------------

    #[test]
    fn model_picker_render_full_width_includes_context_and_reasoning() {
        let mut picker = ModelPicker::new(many_model_entries(3), 10);
        let lines = picker.render(80);
        let row = &lines[1];
        assert!(row.contains("100K")); // context column present
    }

    #[test]
    fn model_picker_render_narrow_width_drops_context_and_reasoning() {
        let mut picker = ModelPicker::new(many_model_entries(3), 10);
        let lines = picker.render(40);
        let row = &lines[1];
        assert!(!row.contains("100K"));
    }

    #[test]
    fn model_picker_render_very_wide_width_still_bounded_and_readable() {
        let mut picker = ModelPicker::new(many_model_entries(3), 10);
        let lines = picker.render(200);
        assert!(lines.len() >= 3);
        assert!(lines[1].contains("model-000"));
    }

    #[test]
    fn model_picker_render_scrolls_large_lists_at_small_viewport() {
        let mut picker = ModelPicker::new(many_model_entries(200), 5);
        for _ in 0..50 {
            picker.move_selection(1);
        }
        let lines = picker.render(80);
        // header + 5 visible rows + scroll indicator + footer
        assert_eq!(lines.len(), 8);
        assert!(lines.iter().any(|l| l.contains("51/200"))); // 1-based selected position
    }

    #[test]
    fn model_picker_render_cache_hits_on_unchanged_state() {
        let mut picker = ModelPicker::new(many_model_entries(3), 10);
        let first = picker.render(80);
        let second = picker.render(80);
        assert_eq!(first, second);
    }

    // -- SessionPicker -------------------------------------------------

    #[test]
    fn session_picker_navigation_clamps_and_selects_by_original_index() {
        let infos = sample_session_infos();
        let entries = load_session_entries(&infos, &HashMap::new());
        let mut picker = SessionPicker::new(entries, 10);
        picker.move_selection(1);
        picker.move_selection(1); // past the end (only 2 entries)
        assert_eq!(picker.selected_entry().unwrap().id, "session-a");
        match picker.handle_key("\r") {
            PickerAction::Selected(idx) => assert_eq!(idx, 1),
            other => panic!("expected Selected(1), got {other:?}"),
        }
    }

    #[test]
    fn session_picker_filters_by_title() {
        let infos = sample_session_infos();
        let entries = load_session_entries(&infos, &HashMap::new());
        let mut picker = SessionPicker::new(entries, 10);
        for ch in "Refactor".chars() {
            picker.push_filter_char(ch);
        }
        assert_eq!(picker.match_count(), 1);
        assert_eq!(picker.selected_entry().unwrap().id, "session-a");
    }

    #[test]
    fn session_picker_empty_list_renders_placeholder_not_panic() {
        let mut picker = SessionPicker::new(Vec::new(), 10);
        let lines = picker.render(80);
        assert!(lines.iter().any(|l| l.contains("no resumable sessions")));
    }

    // -- shared helpers -------------------------------------------------

    /// Navigation clamps at both ends, and Enter after over-scrolling selects
    /// the *last* row rather than reading past it.
    ///
    /// This is the invariant a `clamp_index` helper used to be tested for
    /// directly. Asserting it through the public surface instead is what
    /// actually matters: the fake picker this replaced did `selected += 1`
    /// with no bound, and only its renderer's `.min(len - 1)` hid the result.
    #[test]
    fn model_picker_navigation_clamps_at_both_ends() {
        let entries: Vec<ModelEntry> = (0..4)
            .map(|i| ModelEntry {
                provider: "p".to_string(),
                model_id: format!("m{i}"),
                display_name: format!("Model {i}"),
                context_window: 1000,
                reasoning: false,
                local: false,
            })
            .collect();
        let mut picker = ModelPicker::new(entries, 2);

        // Far past the end.
        for _ in 0..50 {
            picker.move_selection(1);
        }
        assert_eq!(
            picker.handle_key("\r"),
            PickerAction::Selected(3),
            "over-scrolling down must land on the last entry, not past it"
        );

        // Far past the start.
        for _ in 0..50 {
            picker.move_selection(-1);
        }
        assert_eq!(
            picker.handle_key("\r"),
            PickerAction::Selected(0),
            "over-scrolling up must land on the first entry"
        );
    }

    #[test]
    fn clamp_scroll_slides_window_to_keep_selection_visible() {
        assert_eq!(clamp_scroll(0, 0, 5), 0);
        assert_eq!(clamp_scroll(7, 0, 5), 3); // 7 - 5 + 1
        assert_eq!(clamp_scroll(2, 3, 5), 2); // selected above window -> jump up
    }

    #[test]
    fn format_context_window_formats_scale() {
        assert_eq!(format_context_window(512), "512");
        assert_eq!(format_context_window(8_000), "8K");
        assert_eq!(format_context_window(32_000), "32K");
        assert_eq!(format_context_window(200_000), "200K");
        assert_eq!(format_context_window(1_000_000), "1.0M");
    }

    #[test]
    fn format_relative_age_buckets_by_magnitude() {
        let now = UNIX_EPOCH + Duration::from_secs(1_000_000);
        assert_eq!(format_relative_age(now, now), "just now");
        assert_eq!(
            format_relative_age(now - Duration::from_secs(120), now),
            "2m ago"
        );
        assert_eq!(
            format_relative_age(now - Duration::from_secs(7_200), now),
            "2h ago"
        );
        assert_eq!(
            format_relative_age(now - Duration::from_secs(86_400 * 3), now),
            "3d ago"
        );
        assert_eq!(
            format_relative_age(now - Duration::from_secs(86_400 * 20), now),
            "2w ago"
        );
    }

    #[test]
    fn format_relative_age_future_timestamp_does_not_panic() {
        let now = UNIX_EPOCH + Duration::from_secs(1_000);
        let future = now + Duration::from_secs(1_000);
        assert_eq!(format_relative_age(future, now), "just now");
    }
}
