//! Extended-thinking rendering for the interactive TUI.
//!
//! `interactive_mode.rs`'s `assistant_text()` filters an assistant message's
//! `content[]` array down to `type == "text"` blocks and silently drops
//! everything else — including the model's reasoning. That reasoning is a
//! real, separately-tagged content block (`crates/pirust-ai/src/types/content.rs`
//! `ThinkingContent`, tag `#[serde(rename = "thinking")]`, fields `thinking:
//! String`, `thinkingSignature: Option<String>`, `redacted: Option<bool>` —
//! confirmed by that file's own round-trip test:
//! `r#"{"type":"thinking","thinking":"hmm"}"#`). A repo-wide grep of
//! `pirust-ai`/`pirust-agent-core` found no separate `"redacted_thinking"`
//! block *type* — redaction here is the boolean `redacted` flag on the same
//! `"thinking"` block, not a distinct discriminant. [`thinking_text`] still
//! matches on a literal `"redacted_thinking"` type defensively (that IS the
//! Anthropic Messages API's own block name), since the message `Value` this
//! module reads is untyped JSON, not the `ThinkingContent` struct itself, and
//! a future provider adapter could plausibly emit it before this crate's
//! types catch up — but as of this wave that arm is unreached in practice.
//!
//! `docs/tui-design-samples.html`'s "Thinking enabled" sample is the design
//! target: a dim one-line "Thinking" header, collapsed by default, with the
//! live reasoning visible a line at a time while streaming, and
//! "press Ctrl+O to expand" to see the rest. [`ThinkingComponent`] implements
//! that panel; [`ThinkingRegistry`] is the bookkeeping a global Ctrl+O
//! handler needs to find *which* panel to toggle without `interactive_mode.rs`
//! threading a reference through every event branch that might create one.
//! Wiring `render_event`/the Ctrl+O key handler up to these types is left to
//! the caller — this module only owns the component and its data extraction,
//! per the task boundary (no edits to `interactive_mode.rs`).

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use pirust_tui::tui::Component;
use pirust_tui::utils::wrap_text_with_ansi;

use crate::interactive_a11y;
use crate::interactive_theme::{dark, fg};

/// Upper bound on retained thinking text, in bytes.
///
/// Extended reasoning is not bounded by anything upstream — a single turn at
/// a high thinking level can stream hundreds of KB of prose before the model
/// ever emits a text block. Keeping the whole trace verbatim for the rest of
/// the process would mean one verbose turn permanently inflates the chat's
/// memory footprint (nothing else ever calls `set_text`/`push_delta` to
/// shrink it back down — the component just sits mounted in the chat
/// history). 64 KiB is generous for what a human will actually read in an
/// expanded panel (on the order of a thousand lines of prose) while keeping
/// worst-case memory for *any* single thinking block a small, fixed constant
/// no matter how long the model deliberates. When the cap is exceeded the
/// oldest bytes are dropped and the expanded view says so (see
/// `body_text`'s `"… earlier reasoning trimmed"` line) rather than silently
/// losing the start of the trace with no indication anything is missing.
const MAX_RETAINED_BYTES: usize = 64 * 1024;

/// A collapsible panel for one assistant turn's reasoning/thinking text.
///
/// Collapsed (the default) it renders a single dim summary line — a live
/// line count and, while still streaming, the most recent line of reasoning
/// so the user has something to watch besides a static "thinking…" spinner.
/// Expanded (`Ctrl+O`, via [`ThinkingRegistry`]) it renders the full
/// retained text, dimmed and indented, under a `▾ Thinking` header.
///
/// `push_delta` is the hot path — called once per streamed thinking token —
/// so it is amortised O(delta.len()): one `String` buffer, `push_str`
/// (never `format!`/rebuild), and an incremental newline count rather than a
/// full re-scan of the buffer on every call. Rendered lines are cached by
/// width so repeated `render()` calls between deltas (e.g. a redraw
/// triggered by some *other* component) are a `Vec<String>` clone, not a
/// re-wrap.
pub struct ThinkingComponent {
    /// The retained tail of the reasoning text (bounded by
    /// `MAX_RETAINED_BYTES`; see that constant's docs).
    text: String,
    /// Count of `\n` bytes currently in `text`. Maintained incrementally by
    /// `push_delta`/`trim_to_cap` rather than recomputed on every render —
    /// with reasoning traces potentially tens of KB long, a
    /// `text.matches('\n').count()` per streamed token would turn an O(1)
    /// append into an O(n) scan on the hottest path this component has.
    raw_newlines: usize,
    /// Set once the front of `text` has been dropped to respect the cap, so
    /// the expanded view can say so instead of silently showing a partial
    /// trace as if it were the whole thing.
    trimmed: bool,
    /// User-controlled: `Ctrl+O` (via the owning [`ThinkingRegistry`]) flips
    /// this. Collapsed by default per the design spec.
    expanded: bool,
    /// True from construction until `finish()`. Governs the collapsed
    /// summary's wording (`"Thinking… (N lines)"` vs `"Thought for N
    /// lines"`) and whether the live last-line preview is shown.
    streaming: bool,
    /// Render cache: `Some(width)` alongside the lines wrapped for exactly
    /// that width. Every mutator that can change what should be on screen
    /// (`push_delta`, `set_text`, `finish`, `toggle`, `set_expanded`) clears
    /// this via `invalidate`; a `render()` call with nothing changed and the
    /// same width is a clone of already-wrapped lines, not a re-wrap.
    cached_width: Option<usize>,
    cached_lines: Option<Vec<String>>,
}

impl ThinkingComponent {
    /// A fresh, collapsed, streaming (not yet `finish()`ed) panel with no
    /// text — callers should check [`Self::is_empty`] before mounting one,
    /// since an empty "Thinking… (0 lines)" panel with nothing to show is
    /// noise, not signal.
    pub fn new() -> Self {
        Self {
            text: String::new(),
            raw_newlines: 0,
            trimmed: false,
            expanded: false,
            streaming: true,
            cached_width: None,
            cached_lines: None,
        }
    }

    /// Append streamed thinking text. Hot path — see the struct docs for
    /// the amortised-cost argument.
    pub fn push_delta(&mut self, delta: &str) {
        if delta.is_empty() {
            return;
        }
        self.text.reserve(delta.len());
        self.text.push_str(delta);
        self.raw_newlines += delta.bytes().filter(|&b| b == b'\n').count();
        self.trim_to_cap();
        self.invalidate();
    }

    /// Replace the retained text wholesale (e.g. a `message_end` snapshot
    /// that supersedes everything streamed so far). No-op — no cache
    /// invalidation, no allocation beyond the comparison — if the text is
    /// already exactly this.
    pub fn set_text(&mut self, text: &str) {
        if text == self.text {
            return;
        }
        self.text.clear();
        self.text.reserve(text.len());
        self.text.push_str(text);
        self.raw_newlines = self.text.bytes().filter(|&b| b == b'\n').count();
        self.trimmed = false;
        self.trim_to_cap();
        self.invalidate();
    }

    /// Drop the oldest bytes once `text` exceeds `MAX_RETAINED_BYTES`.
    ///
    /// Runs to a fixed point after every mutation, so `text.len()` is always
    /// `<= MAX_RETAINED_BYTES` afterward — meaning this only does real work
    /// (an O(retained) `replace_range` shift) once per cap's worth of bytes
    /// pushed, not on every `push_delta` call. Total shifting work across an
    /// entire streamed trace is therefore bounded by the total bytes
    /// streamed, preserving `push_delta`'s amortised O(delta.len()) bound.
    fn trim_to_cap(&mut self) {
        if self.text.len() <= MAX_RETAINED_BYTES {
            return;
        }
        let excess = self.text.len() - MAX_RETAINED_BYTES;
        // `excess` may land mid-codepoint; walk forward to the next char
        // boundary so `replace_range` never panics on a UTF-8 split.
        let mut cut = excess;
        while cut < self.text.len() && !self.text.is_char_boundary(cut) {
            cut += 1;
        }
        if cut == 0 {
            return;
        }
        let dropped_newlines = self.text.as_bytes()[..cut]
            .iter()
            .filter(|&&b| b == b'\n')
            .count();
        self.raw_newlines = self.raw_newlines.saturating_sub(dropped_newlines);
        self.text.replace_range(..cut, "");
        self.trimmed = true;
    }

    /// Mark the turn's reasoning as complete: switches the collapsed summary
    /// from "Thinking… (N lines)" to "Thought for N lines" and drops the
    /// live last-line preview.
    pub fn finish(&mut self) {
        if !self.streaming {
            return;
        }
        self.streaming = false;
        self.invalidate();
    }

    /// Whether there is anything to show at all. Callers should skip
    /// mounting a `ThinkingComponent` in the chat when this is true rather
    /// than rendering an empty "Thinking… (0 lines)" placeholder.
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Flip collapsed/expanded — the `Ctrl+O` action.
    pub fn toggle(&mut self) {
        self.expanded = !self.expanded;
        self.invalidate();
    }

    /// Set collapsed/expanded directly (e.g. `ThinkingRegistry::toggle_all`).
    pub fn set_expanded(&mut self, expanded: bool) {
        if self.expanded != expanded {
            self.expanded = expanded;
            self.invalidate();
        }
    }

    pub fn is_expanded(&self) -> bool {
        self.expanded
    }

    /// Lines currently retained in `text`, `0` when empty. This counts lines
    /// in the *retained* (possibly trimmed) window, so once trimming has
    /// happened the number can understate the turn's true total reasoning
    /// length — an accepted trade-off of the bounded buffer (see
    /// `MAX_RETAINED_BYTES`), and the reason the trimmed note exists
    /// separately from this count.
    fn total_lines(&self) -> usize {
        if self.text.is_empty() {
            0
        } else {
            self.raw_newlines + 1
        }
    }

    /// Build the (uncolored, unwrapped) text to display for the current
    /// state. Only called on a cache miss.
    fn body_text(&self) -> String {
        let total_lines = total_lines_label(self.total_lines());
        if self.expanded {
            let mut s = String::with_capacity(self.text.len() + 32);
            s.push_str("▾ Thinking");
            if self.trimmed {
                s.push_str("\n… earlier reasoning trimmed");
            }
            s.push('\n');
            for line in self.text.split('\n') {
                s.push_str("  ");
                s.push_str(line);
                s.push('\n');
            }
            s.pop(); // drop the trailing newline the loop always adds
            s
        } else if self.streaming {
            let mut s = format!("▸ Thinking… ({total_lines})");
            // The live last-line preview repaints on every streamed token —
            // exactly the kind of continuous motion `reduced_motion` exists
            // to suppress (screen readers re-announce it on every change;
            // vestibular-disorder accommodation wants it still). The header
            // above still updates (it is real progress, not decoration), just
            // without the constantly-rewriting tail line.
            if !interactive_a11y::active().reduced_motion {
                let last = self.text.rsplit('\n').next().unwrap_or("").trim();
                if !last.is_empty() {
                    s.push('\n');
                    s.push_str("  ");
                    s.push_str(last);
                }
            }
            s
        } else {
            format!("▸ Thought for {total_lines}")
        }
    }

    fn render_uncached(&self, width: usize) -> Vec<String> {
        let body = self.body_text();
        let colorize = fg(dark::GRAY);
        let colored = colorize(&body);
        wrap_text_with_ansi(&colored, width)
    }
}

impl Default for ThinkingComponent {
    fn default() -> Self {
        Self::new()
    }
}

/// `"N line"` / `"N lines"` — split out of `body_text` purely so the
/// singular/plural branch isn't duplicated across the streaming/finished
/// arms.
fn total_lines_label(n: usize) -> String {
    if n == 1 {
        "1 line".to_string()
    } else {
        format!("{n} lines")
    }
}

impl Component for ThinkingComponent {
    fn render(&mut self, width: usize) -> Vec<String> {
        if let (Some(cached_width), Some(cached_lines)) = (self.cached_width, &self.cached_lines) {
            if cached_width == width {
                return cached_lines.clone();
            }
        }
        let width = width.max(1);
        let lines = self.render_uncached(width);
        self.cached_width = Some(width);
        self.cached_lines = Some(lines.clone());
        lines
    }

    fn invalidate(&mut self) {
        self.cached_width = None;
        self.cached_lines = None;
    }
}

/// Extract the assistant's reasoning text from a message `Value` — the
/// thinking-block mirror of `interactive_mode.rs`'s `assistant_text` (which
/// does the same walk for `type == "text"` blocks and drops everything
/// else). See the module docs for the block shape this was verified
/// against (`crates/pirust-ai/src/types/content.rs`'s `ThinkingContent`).
///
/// A block with `redacted: true` (safety-filtered; the real payload is an
/// opaque `thinkingSignature`, not human-readable text) renders as
/// `"[redacted]"` rather than being dropped silently — matching how a
/// redacted block is still a *fact* about the turn ("the model reasoned
/// here, but it's hidden"), not nothing.
pub fn thinking_text(message: &serde_json::Value) -> String {
    let Some(content) = message.get("content").and_then(|c| c.as_array()) else {
        return String::new();
    };
    content
        .iter()
        .filter_map(|block| match block.get("type").and_then(|t| t.as_str()) {
            Some("thinking") => {
                let redacted = block
                    .get("redacted")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                if redacted {
                    return Some("[redacted]".to_string());
                }
                block
                    .get("thinking")
                    .and_then(|t| t.as_str())
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
                    .map(str::to_string)
            }
            // Not observed anywhere in this codebase (see module docs) but
            // handled defensively: it is the Anthropic Messages API's own
            // block type for a redacted thinking segment, so an adapter
            // that passes provider JSON through with less normalization
            // than `pirust-ai`'s typed model could plausibly emit it.
            Some("redacted_thinking") => Some("[redacted]".to_string()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Tracks the `ThinkingComponent`s mounted so far in the current session so
/// a single global `Ctrl+O` handler can act on "the" thinking panel (the
/// most recent turn's) without `interactive_mode.rs` threading a reference
/// through every branch of `render_event` that might create one.
///
/// Holds only `Weak` handles: the chat container's child list is the real
/// owner of each `Rc<RefCell<ThinkingComponent>>`, and a registry that held
/// strong references would keep every turn's reasoning text alive for the
/// life of the process even after it scrolls out of anything the user can
/// reach — silently defeating `MAX_RETAINED_BYTES`'s whole point at the
/// session level. Dead entries are pruned lazily (on the next
/// register/toggle) rather than eagerly, since nothing needs the list's
/// size to be exact between calls.
#[derive(Default)]
pub struct ThinkingRegistry {
    components: Vec<Weak<RefCell<ThinkingComponent>>>,
}

impl ThinkingRegistry {
    pub fn new() -> Self {
        Self {
            components: Vec::new(),
        }
    }

    /// Register a newly mounted panel as the most recent one — call this
    /// once per `ThinkingComponent` a caller creates, right after wrapping
    /// it in the `Rc<RefCell<..>>` that also goes into the chat.
    pub fn register(&mut self, component: &Rc<RefCell<ThinkingComponent>>) {
        self.prune();
        self.components.push(Rc::downgrade(component));
    }

    fn prune(&mut self) {
        self.components.retain(|w| w.strong_count() > 0);
    }

    /// The plain `Ctrl+O` action: toggle only the most recently registered
    /// still-alive panel (what the model is thinking about *right now*, or
    /// most recently was). A no-op if nothing is registered or every
    /// registered panel has already been dropped.
    pub fn toggle_latest(&mut self) {
        self.prune();
        if let Some(component) = self.components.last().and_then(Weak::upgrade) {
            component.borrow_mut().toggle();
        }
    }

    /// Force every still-alive panel in the session to the same
    /// expanded/collapsed state (e.g. a future "expand all thinking"
    /// command). `toggle_latest` is the one wired to plain `Ctrl+O`; this is
    /// for a broader action a caller may want later.
    pub fn toggle_all(&mut self, expanded: bool) {
        self.prune();
        for weak in &self.components {
            if let Some(component) = weak.upgrade() {
                component.borrow_mut().set_expanded(expanded);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ---- ThinkingComponent: basic state ----------------------------------

    #[test]
    fn new_component_is_empty_and_collapsed_and_streaming() {
        let t = ThinkingComponent::new();
        assert!(t.is_empty());
        assert!(!t.is_expanded());
        assert!(t.streaming);
    }

    #[test]
    fn push_delta_accumulates_and_counts_lines_incrementally() {
        let mut t = ThinkingComponent::new();
        t.push_delta("first line\n");
        t.push_delta("second line\n");
        t.push_delta("third, no newline yet");
        assert_eq!(t.text, "first line\nsecond line\nthird, no newline yet");
        assert_eq!(t.raw_newlines, 2);
        assert_eq!(t.total_lines(), 3);
    }

    #[test]
    fn push_delta_empty_is_a_true_no_op() {
        let mut t = ThinkingComponent::new();
        t.push_delta("hello");
        let before_width = t.cached_width;
        // Force a render so a cache exists, then confirm an empty delta
        // does not invalidate it (would show up as a `render()` panic-free
        // re-wrap either way, but the point is `push_delta("")` must not
        // even touch the buffer).
        let _ = t.render(40);
        t.push_delta("");
        assert_eq!(t.text, "hello");
        assert_eq!(before_width, None); // sanity: hadn't rendered before the call above
    }

    #[test]
    fn set_text_is_a_no_op_when_unchanged() {
        let mut t = ThinkingComponent::new();
        t.set_text("same");
        let _ = t.render(40);
        assert!(t.cached_lines.is_some());
        t.set_text("same");
        // Cache must survive an identical `set_text` — it is documented as
        // a no-op, i.e. must not invalidate.
        assert!(t.cached_lines.is_some());
    }

    #[test]
    fn set_text_replaces_wholesale_and_recomputes_lines() {
        let mut t = ThinkingComponent::new();
        t.push_delta("a\nb\nc");
        assert_eq!(t.total_lines(), 3);
        t.set_text("just one line");
        assert_eq!(t.total_lines(), 1);
        assert_eq!(t.text, "just one line");
    }

    #[test]
    fn finish_flips_streaming_and_is_idempotent() {
        let mut t = ThinkingComponent::new();
        assert!(t.streaming);
        t.finish();
        assert!(!t.streaming);
        t.finish(); // must not panic or toggle back
        assert!(!t.streaming);
    }

    #[test]
    fn toggle_and_set_expanded_flip_state() {
        let mut t = ThinkingComponent::new();
        assert!(!t.is_expanded());
        t.toggle();
        assert!(t.is_expanded());
        t.toggle();
        assert!(!t.is_expanded());
        t.set_expanded(true);
        assert!(t.is_expanded());
        t.set_expanded(true); // idempotent
        assert!(t.is_expanded());
    }

    // ---- ThinkingComponent: render content --------------------------------

    /// Pinned via `with_settings`: this asserts the *non-reduced-motion* live
    /// preview, which `interactive_a11y::active()` would not deterministically
    /// give by default (a non-TTY test run detects `reduced_motion = true`).
    #[test]
    fn collapsed_streaming_render_shows_header_and_last_line() {
        interactive_a11y::with_settings(
            interactive_a11y::A11ySettings {
                reduced_motion: false,
                ..interactive_a11y::A11ySettings::default()
            },
            || {
                let mut t = ThinkingComponent::new();
                t.push_delta("looking at the code\nnarrowing it down");
                let lines = t.render(80);
                let joined = lines.join("\n");
                assert!(joined.contains("Thinking…"));
                assert!(joined.contains("(2 lines)"));
                assert!(joined.contains("narrowing it down"));
                // The header still says "Thinking…", not "Thought for" — streaming.
                assert!(!joined.contains("Thought for"));
            },
        );
    }

    /// `reduced_motion` must suppress the constantly-rewriting last-line
    /// preview while still showing real progress (the line count).
    #[test]
    fn reduced_motion_suppresses_live_last_line_preview() {
        interactive_a11y::with_settings(
            interactive_a11y::A11ySettings {
                reduced_motion: true,
                ..interactive_a11y::A11ySettings::default()
            },
            || {
                let mut t = ThinkingComponent::new();
                t.push_delta("looking at the code\nnarrowing it down");
                let joined = t.render(80).join("\n");
                assert!(joined.contains("Thinking…"));
                assert!(joined.contains("(2 lines)"));
                assert!(!joined.contains("narrowing it down"));
            },
        );
    }

    #[test]
    fn collapsed_finished_render_drops_live_preview() {
        let mut t = ThinkingComponent::new();
        t.push_delta("looking at the code\nnarrowing it down");
        t.finish();
        let lines = t.render(80);
        let joined = lines.join("\n");
        assert!(joined.contains("Thought for"));
        assert!(joined.contains("2 lines"));
        assert!(!joined.contains("narrowing it down"));
    }

    #[test]
    fn singular_line_count_is_grammatical() {
        let mut t = ThinkingComponent::new();
        t.push_delta("only one line");
        t.finish();
        let joined = t.render(80).join("\n");
        assert!(joined.contains("1 line)") || joined.contains("1 line"));
        assert!(!joined.contains("1 lines"));
    }

    #[test]
    fn expanded_render_shows_full_indented_text() {
        let mut t = ThinkingComponent::new();
        t.push_delta("alpha\nbeta");
        t.set_expanded(true);
        let joined = t.render(80).join("\n");
        assert!(joined.contains("Thinking")); // "▾ Thinking" header
        assert!(joined.contains("alpha"));
        assert!(joined.contains("beta"));
    }

    #[test]
    fn render_cache_hits_on_unchanged_width() {
        let mut t = ThinkingComponent::new();
        t.push_delta("hello world");
        let first = t.render(60);
        let second = t.render(60);
        assert_eq!(first, second);
    }

    #[test]
    fn render_cache_misses_on_width_change() {
        // Pinned `reduced_motion: false` so the live last-line preview (long
        // enough to wrap differently at each width) is actually present —
        // otherwise the two renders would both collapse to the same
        // one-line header and this test would stop testing the cache.
        interactive_a11y::with_settings(
            interactive_a11y::A11ySettings {
                reduced_motion: false,
                ..interactive_a11y::A11ySettings::default()
            },
            || {
                let mut t = ThinkingComponent::new();
                t.push_delta("a somewhat long line of reasoning text that will wrap");
                let narrow = t.render(20);
                let wide = t.render(80);
                // Different wrap widths must not be silently served from the
                // same cache entry.
                assert_ne!(narrow, wide);
            },
        );
    }

    #[test]
    fn is_empty_reflects_buffer_state() {
        let mut t = ThinkingComponent::new();
        assert!(t.is_empty());
        t.push_delta("x");
        assert!(!t.is_empty());
    }

    // ---- Bounded retention --------------------------------------------------

    #[test]
    fn trim_to_cap_bounds_memory_and_flags_trimming() {
        let mut t = ThinkingComponent::new();
        // Push well past the cap in chunks (as real streaming would).
        let chunk = "x".repeat(4096);
        for _ in 0..20 {
            t.push_delta(&chunk);
        }
        assert!(t.text.len() <= MAX_RETAINED_BYTES);
        assert!(t.trimmed);
    }

    #[test]
    fn trim_to_cap_adjusts_line_count_for_dropped_lines() {
        let mut t = ThinkingComponent::new();
        // Every pushed line is short and newline-terminated so the dropped
        // prefix definitely contains whole lines to discount.
        let line = "reasoning step\n".repeat(200); // ~3000 bytes
        for _ in 0..40 {
            t.push_delta(&line); // ~120,000 bytes total, well past the cap
        }
        assert!(t.text.len() <= MAX_RETAINED_BYTES);
        // raw_newlines must reflect only what's retained, not the full
        // history ever streamed (which would wildly overcount).
        let counted = t.text.bytes().filter(|&b| b == b'\n').count();
        assert_eq!(t.raw_newlines, counted);
    }

    #[test]
    fn expanded_render_notes_trimming_when_it_happened() {
        let mut t = ThinkingComponent::new();
        let chunk = "y".repeat(4096);
        for _ in 0..20 {
            t.push_delta(&chunk);
        }
        t.set_expanded(true);
        let joined = t.render(80).join("\n");
        assert!(joined.contains("earlier reasoning trimmed"));
    }

    #[test]
    fn no_trim_note_when_under_cap() {
        let mut t = ThinkingComponent::new();
        t.push_delta("short");
        t.set_expanded(true);
        let joined = t.render(80).join("\n");
        assert!(!joined.contains("trimmed"));
    }

    // ---- thinking_text extraction ------------------------------------------

    #[test]
    fn thinking_text_extracts_thinking_blocks_only() {
        let message = json!({
            "role": "assistant",
            "content": [
                {"type": "thinking", "thinking": "let me check the file"},
                {"type": "text", "text": "Here's the answer."},
                {"type": "toolCall", "id": "1", "name": "read", "arguments": {}},
            ]
        });
        assert_eq!(thinking_text(&message), "let me check the file");
    }

    #[test]
    fn thinking_text_joins_multiple_thinking_blocks_with_newline() {
        let message = json!({
            "content": [
                {"type": "thinking", "thinking": "step one"},
                {"type": "thinking", "thinking": "step two"},
            ]
        });
        assert_eq!(thinking_text(&message), "step one\nstep two");
    }

    #[test]
    fn thinking_text_trims_and_skips_blank_blocks() {
        let message = json!({
            "content": [
                {"type": "thinking", "thinking": "  padded  "},
                {"type": "thinking", "thinking": "   "},
            ]
        });
        assert_eq!(thinking_text(&message), "padded");
    }

    #[test]
    fn thinking_text_renders_redacted_flag_as_placeholder() {
        let message = json!({
            "content": [
                {"type": "thinking", "thinking": "", "redacted": true, "thinkingSignature": "opaque"},
            ]
        });
        assert_eq!(thinking_text(&message), "[redacted]");
    }

    #[test]
    fn thinking_text_handles_redacted_thinking_type_defensively() {
        let message = json!({
            "content": [
                {"type": "redacted_thinking", "data": "opaque"},
            ]
        });
        assert_eq!(thinking_text(&message), "[redacted]");
    }

    #[test]
    fn thinking_text_empty_when_no_content_array() {
        let message = json!({"role": "assistant"});
        assert_eq!(thinking_text(&message), "");
    }

    #[test]
    fn thinking_text_empty_when_no_thinking_blocks() {
        let message = json!({"content": [{"type": "text", "text": "hi"}]});
        assert_eq!(thinking_text(&message), "");
    }

    // ---- ThinkingRegistry ---------------------------------------------------

    #[test]
    fn registry_toggle_latest_toggles_only_the_most_recent() {
        let mut registry = ThinkingRegistry::new();
        let first = Rc::new(RefCell::new(ThinkingComponent::new()));
        let second = Rc::new(RefCell::new(ThinkingComponent::new()));
        registry.register(&first);
        registry.register(&second);

        registry.toggle_latest();

        assert!(!first.borrow().is_expanded());
        assert!(second.borrow().is_expanded());
    }

    #[test]
    fn registry_toggle_all_affects_every_alive_component() {
        let mut registry = ThinkingRegistry::new();
        let first = Rc::new(RefCell::new(ThinkingComponent::new()));
        let second = Rc::new(RefCell::new(ThinkingComponent::new()));
        registry.register(&first);
        registry.register(&second);

        registry.toggle_all(true);

        assert!(first.borrow().is_expanded());
        assert!(second.borrow().is_expanded());

        registry.toggle_all(false);
        assert!(!first.borrow().is_expanded());
        assert!(!second.borrow().is_expanded());
    }

    #[test]
    fn registry_prunes_dropped_components_without_panicking() {
        let mut registry = ThinkingRegistry::new();
        {
            let scoped = Rc::new(RefCell::new(ThinkingComponent::new()));
            registry.register(&scoped);
        } // `scoped` dropped: only weak ref remains, now dangling.

        // Neither call should panic even though every registered handle is
        // dead; toggle_latest should simply have nothing to do.
        registry.toggle_latest();
        registry.toggle_all(true);

        let alive = Rc::new(RefCell::new(ThinkingComponent::new()));
        registry.register(&alive);
        registry.toggle_latest();
        assert!(alive.borrow().is_expanded());
    }

    #[test]
    fn registry_default_is_empty_and_safe_to_toggle() {
        let mut registry = ThinkingRegistry::default();
        registry.toggle_latest(); // no-op, must not panic
        registry.toggle_all(true); // no-op, must not panic
    }
}
