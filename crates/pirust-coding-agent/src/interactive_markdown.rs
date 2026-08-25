//! `MarkdownText` — a `pirust_tui::tui::Component` adapter around
//! `pirust_tui::markdown::Markdown`.
//!
//! The interactive chat currently pushes assistant replies through
//! `pirust_tui::components::text::Text`, which is a dumb word-wrapper: it has
//! no idea `**bold**`, fenced code, tables, or headings exist, so a reply
//! shows up with its raw markdown syntax still in it. `pirust-tui` already
//! ships a byte-exact port of the real markdown renderer
//! (`pirust_tui::markdown::Markdown`) — it was just never wired up to
//! anything in this crate. This module is that wiring: a thin `Component`
//! wrapper plus the dark-theme `MarkdownTheme` that the coding-agent side
//! owns (mirroring `getMarkdownTheme()` / `dark.json`'s `md*` keys from the
//! oracle `theme.ts`, since `pirust-tui` intentionally has no opinion about
//! color).
//!
//! Perf posture: this is the hot path for a streaming assistant reply —
//! `set_text` is called once per token delta, i.e. many times a second while
//! a response is streaming. Two things matter here:
//!   1. `Markdown` already caches its own lex/wrap output keyed on
//!      `(text, width)`, so re-rendering unchanged text is cheap. But
//!      `Markdown` does not expose whether the text you just handed it is
//!      actually new, so a caller that redraws on a timer (not just on
//!      genuine deltas) would blindly invalidate that cache every tick.
//!      `MarkdownText::set_text` closes that gap with its own before/after
//!      compare.
//!   2. `MarkdownTheme` is fourteen `Rc<dyn Fn>` closures. Building a fresh
//!      one per chat message (there can be hundreds in a long session) would
//!      be fourteen needless heap allocations per message for byte-identical
//!      behavior. Both the dark and plain themes are therefore built exactly
//!      once per thread and handed out via cheap `Rc` clones afterward — see
//!      `default_markdown_theme` / `plain_markdown_theme` below.

use std::cell::{Cell, OnceCell};
use std::rc::Rc;

use pirust_tui::markdown::{Markdown, MarkdownTheme, StyleFn};
use pirust_tui::tui::Component;

/// The `md*` palette from the oracle `dark.json` (`theme.ts`'s
/// `getMarkdownTheme()`). These are markdown-specific hexes that
/// `interactive_theme::dark` does not carry (it only exposes the colors the
/// tool-execution/chat-box wave needed), so they live here rather than
/// growing that module's surface for a single caller.
mod palette {
    /// `mdHeading` (dark.json).
    pub const HEADING: &str = "#f0c674";
    /// `mdLink` (dark.json).
    pub const LINK: &str = "#81a2be";
    /// `mdLinkUrl` -> `dimGray` (dark.json).
    pub const LINK_URL: &str = "#666666";
    /// `mdCode` -> `accent` (dark.json).
    pub const CODE: &str = "#8abeb7";
    /// `mdCodeBlock` -> `green` (dark.json).
    pub const CODE_BLOCK: &str = "#b5bd68";
    /// `mdListBullet` -> `accent` (dark.json). Same value as `CODE`, kept as
    /// its own constant so the two can diverge without a silent coupling.
    pub const LIST_BULLET: &str = "#8abeb7";
}

thread_local! {
    /// Global "should markdown color itself" switch (mirrors `NO_COLOR`
    /// handling elsewhere in the interactive mode). Defaults to enabled.
    static COLOR_ENABLED: Cell<bool> = const { Cell::new(true) };
    /// The dark theme, built once per thread on first use.
    static DARK_THEME: OnceCell<MarkdownTheme> = const { OnceCell::new() };
    /// The plain (identity) theme, built once per thread on first use.
    static PLAIN_THEME: OnceCell<MarkdownTheme> = const { OnceCell::new() };
}

/// Enables or disables ANSI color in the theme `default_markdown_theme()`
/// hands out. Intended to be set once at startup from a `NO_COLOR` /
/// `--no-color` check; changing it later only affects `MarkdownText`
/// instances constructed (or theme lookups made) afterward — existing
/// components keep whatever theme they were built with, matching how
/// `Markdown` itself has no concept of "re-theme in place".
pub fn set_color_enabled(enabled: bool) {
    COLOR_ENABLED.with(|c| c.set(enabled));
}

/// Reads the current color switch. See `set_color_enabled`.
pub fn color_enabled() -> bool {
    COLOR_ENABLED.with(|c| c.get())
}

/// Builds a `StyleFn` that wraps text in a fixed ANSI SGR open/close pair
/// (e.g. bold `\x1b[1m` / `\x1b[22m`). `open`/`close` are `&'static str`
/// literals, so the closure captures two fat-pointer-free `Copy` values —
/// no heap allocation happens until the closure is actually *called*, and
/// none happens to build the closure itself.
fn ansi_wrap(open: &'static str, close: &'static str) -> StyleFn {
    Rc::new(move |s: &str| {
        let mut out = String::with_capacity(open.len() + s.len() + close.len());
        out.push_str(open);
        out.push_str(s);
        out.push_str(close);
        out
    })
}

/// The `plain_markdown_theme` building block: a `StyleFn` that returns its
/// input unchanged (still allocates a fresh `String`, since the `Fn(&str) ->
/// String` signature demands an owned return — but that's the theme reserved
/// for the already-cold `NO_COLOR` path).
fn identity() -> StyleFn {
    Rc::new(|s: &str| s.to_string())
}

/// Builds the dark ANSI-truecolor `MarkdownTheme`, matching the oracle's
/// `getMarkdownTheme()` (`theme.ts`) resolved against `dark.json`'s `md*`
/// keys. Called at most once per thread — see `DARK_THEME`.
fn build_dark_theme() -> MarkdownTheme {
    MarkdownTheme {
        heading: Rc::from(crate::interactive_theme::fg(palette::HEADING)),
        link: Rc::from(crate::interactive_theme::fg(palette::LINK)),
        link_url: Rc::from(crate::interactive_theme::fg(palette::LINK_URL)),
        code: Rc::from(crate::interactive_theme::fg(palette::CODE)),
        code_block: Rc::from(crate::interactive_theme::fg(palette::CODE_BLOCK)),
        code_block_border: Rc::from(crate::interactive_theme::fg(
            crate::interactive_theme::dark::GRAY,
        )),
        quote: Rc::from(crate::interactive_theme::fg(
            crate::interactive_theme::dark::GRAY,
        )),
        quote_border: Rc::from(crate::interactive_theme::fg(
            crate::interactive_theme::dark::GRAY,
        )),
        hr: Rc::from(crate::interactive_theme::fg(
            crate::interactive_theme::dark::GRAY,
        )),
        list_bullet: Rc::from(crate::interactive_theme::fg(palette::LIST_BULLET)),
        // `chalk.bold`/`.italic`/`.underline`/`.strikethrough` (theme.ts)
        // are plain SGR pairs, not colors, so `interactive_theme::fg`/`bg`
        // (which only know truecolor 38;2/48;2 sequences) don't apply here.
        bold: ansi_wrap("\x1b[1m", "\x1b[22m"),
        italic: ansi_wrap("\x1b[3m", "\x1b[23m"),
        strikethrough: ansi_wrap("\x1b[9m", "\x1b[29m"),
        underline: ansi_wrap("\x1b[4m", "\x1b[24m"),
    }
}

/// Builds the plain (no-color) `MarkdownTheme`: same 14 fields, every one an
/// identity function. Called at most once per thread — see `PLAIN_THEME`.
fn build_plain_theme() -> MarkdownTheme {
    MarkdownTheme {
        heading: identity(),
        link: identity(),
        link_url: identity(),
        code: identity(),
        code_block: identity(),
        code_block_border: identity(),
        quote: identity(),
        quote_border: identity(),
        hr: identity(),
        list_bullet: identity(),
        bold: identity(),
        italic: identity(),
        strikethrough: identity(),
        underline: identity(),
    }
}

/// Returns the dark markdown theme (or the plain one, if color is disabled
/// via `set_color_enabled(false)`). Built once per thread; every subsequent
/// call is 14 `Rc` refcount bumps and one small `MarkdownTheme` struct copy
/// — no closures are reallocated, no matter how many chat messages call
/// this.
pub fn default_markdown_theme() -> MarkdownTheme {
    if color_enabled() {
        DARK_THEME.with(|cell| cell.get_or_init(build_dark_theme).clone())
    } else {
        plain_markdown_theme()
    }
}

/// Returns the identity ("no styling") markdown theme, for `NO_COLOR` mode
/// or any caller that explicitly wants unstyled markdown regardless of the
/// global color switch. Built once per thread — see `default_markdown_theme`
/// for the sharing rationale.
pub fn plain_markdown_theme() -> MarkdownTheme {
    PLAIN_THEME.with(|cell| cell.get_or_init(build_plain_theme).clone())
}

/// A `Component` that renders markdown-formatted text through
/// `pirust_tui::markdown::Markdown`, instead of the raw-text
/// `pirust_tui::components::text::Text`. Drop-in shaped: same
/// `new(text, padding_x, padding_y)` / `set_text` / `Component` surface as
/// `Text`, so swapping one for the other at a call site is a type-only
/// change.
pub struct MarkdownText {
    inner: Markdown,
    /// A copy of the text last handed to `inner`. `Markdown` has its own
    /// `text`/`cached_text` fields, but both are private (no getter) — this
    /// crate cannot ask it "did this text actually change?" without keeping
    /// a second copy to compare against. That's the only reason this exists:
    /// it is *not* a rendering cache (all rendering state still lives solely
    /// in `inner`), just the compare key for the `set_text` no-op fast path.
    text: String,
}

impl MarkdownText {
    /// `constructor` — builds a `Markdown` with the shared dark theme (or
    /// plain, if `set_color_enabled(false)` was called first) and no
    /// per-instance style overrides.
    pub fn new(text: impl Into<String>, padding_x: usize, padding_y: usize) -> Self {
        let text = text.into();
        let inner = Markdown::new(
            &text,
            padding_x,
            padding_y,
            default_markdown_theme(),
            None,
            None,
        );
        Self { inner, text }
    }

    /// The text this component currently holds. Mirrors
    /// `components::text::Text::text()`.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Updates the rendered text. This is the streaming hot path — called on
    /// every token delta while an assistant reply is in flight — so an
    /// unchanged `text` is a deliberate no-op: it skips both the redundant
    /// `String` copy and, more importantly, `Markdown::set_text`'s
    /// unconditional cache invalidation (which would otherwise force a full
    /// re-lex + re-wrap on the next `render`, for a value that didn't
    /// actually change).
    ///
    /// When the text *did* change, this reuses `self.text`'s existing
    /// allocation (`clear` + `push_str`) rather than assigning a fresh
    /// `String`. During streaming, `text` only ever grows, so the reused
    /// buffer's capacity — grown by the usual amortized doubling — needs to
    /// reallocate far less often than a hard `to_string()` would (which
    /// allocates an exact-fit buffer on every single delta).
    pub fn set_text(&mut self, text: &str) {
        if self.text == text {
            return;
        }
        self.text.clear();
        self.text.push_str(text);
        self.inner.set_text(text);
    }
}

impl Component for MarkdownText {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.inner.render(width)
    }

    fn invalidate(&mut self) {
        self.inner.invalidate();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Restores the color switch after a test that flips it, since
    /// `thread_local!` state is only guaranteed test-local when the test
    /// harness runs each `#[test]` on its own thread (the default). Being
    /// defensive here costs nothing and avoids order-dependent flakiness if
    /// that default ever changes.
    struct RestoreColor(bool);
    impl Drop for RestoreColor {
        fn drop(&mut self) {
            set_color_enabled(self.0);
        }
    }

    #[test]
    fn renders_plain_text() {
        let mut md = MarkdownText::new("hello world", 1, 0);
        let lines = md.render(40);
        assert!(lines.iter().any(|l| l.contains("hello")));
    }

    #[test]
    fn renders_bold_markdown_with_ansi() {
        let mut md = MarkdownText::new("**bold**", 1, 0);
        let lines = md.render(40);
        let joined = lines.join("\n");
        // chalk.bold semantics: ESC[1m ... ESC[22m.
        assert!(
            joined.contains("\x1b[1m"),
            "expected bold SGR in {joined:?}"
        );
        assert!(joined.contains("bold"));
    }

    #[test]
    fn set_text_updates_rendered_content() {
        let mut md = MarkdownText::new("a", 1, 0);
        assert!(md.render(40).join("\n").contains('a'));
        md.set_text("b");
        let joined = md.render(40).join("\n");
        assert!(joined.contains('b'));
        assert!(!joined.contains('a'));
    }

    #[test]
    fn set_text_is_noop_on_identical_text() {
        let mut md = MarkdownText::new("unchanged", 1, 0);
        let _ = md.render(40);
        // Same content: the compare-and-skip path must not touch `text` or
        // force `inner` to re-render differently.
        md.set_text("unchanged");
        assert_eq!(md.text(), "unchanged");
        let lines_after = md.render(40);
        assert!(lines_after.iter().any(|l| l.contains("unchanged")));
    }

    #[test]
    fn set_text_noop_preserves_buffer_capacity() {
        // A real no-op must not reallocate `text`'s buffer (clear+push_str
        // is skipped entirely when the content is identical).
        let mut md = MarkdownText::new("hello", 1, 0);
        let cap_before = md.text.capacity();
        md.set_text("hello");
        assert_eq!(md.text.capacity(), cap_before);
    }

    #[test]
    fn default_theme_is_built_once_per_thread() {
        let _guard = RestoreColor(color_enabled());
        set_color_enabled(true);
        let a = default_markdown_theme();
        let b = default_markdown_theme();
        // Same underlying closure, reused via Rc — not two fresh instances.
        assert!(Rc::ptr_eq(&a.heading, &b.heading));
        assert!(Rc::ptr_eq(&a.bold, &b.bold));
    }

    #[test]
    fn plain_theme_is_built_once_per_thread() {
        let a = plain_markdown_theme();
        let b = plain_markdown_theme();
        assert!(Rc::ptr_eq(&a.code, &b.code));
    }

    #[test]
    fn plain_theme_fields_are_identity() {
        let theme = plain_markdown_theme();
        assert_eq!((theme.bold)("x"), "x");
        assert_eq!((theme.heading)("y"), "y");
        assert_eq!((theme.link)("z"), "z");
    }

    /// The dark theme's colour fields now degrade with
    /// `interactive_a11y::active()` (they route through
    /// `interactive_theme::fg`), so the colour mode has to be pinned — under a
    /// captured test stdout the detected mode is `None`, which would correctly
    /// make `heading` the identity function.
    #[test]
    fn dark_theme_fields_are_not_identity() {
        crate::interactive_a11y::with_settings(
            crate::interactive_a11y::A11ySettings {
                color: crate::interactive_a11y::ColorMode::Truecolor,
                ..Default::default()
            },
            || {
                let theme = build_dark_theme();
                assert_ne!((theme.heading)("x"), "x");
                assert_ne!((theme.bold)("x"), "x");
            },
        );
    }

    /// With colour off, the dark theme's *colour* fields fall back to identity
    /// while the pure-SGR emphasis fields (`bold`/`italic`, which carry no
    /// colour) keep working — bold text is still legible on a monochrome
    /// terminal, so there is no reason to drop it.
    #[test]
    fn dark_theme_colour_fields_degrade_but_emphasis_survives() {
        crate::interactive_a11y::with_settings(
            crate::interactive_a11y::A11ySettings {
                color: crate::interactive_a11y::ColorMode::None,
                ..Default::default()
            },
            || {
                let theme = build_dark_theme();
                assert_eq!((theme.heading)("x"), "x");
                assert_ne!((theme.bold)("x"), "x");
            },
        );
    }

    #[test]
    fn color_disabled_switches_default_theme_to_plain() {
        let _guard = RestoreColor(color_enabled());
        set_color_enabled(false);
        let theme = default_markdown_theme();
        assert_eq!((theme.bold)("x"), "x");
        assert_eq!((theme.heading)("y"), "y");
    }

    #[test]
    fn color_enabled_round_trips() {
        let _guard = RestoreColor(color_enabled());
        set_color_enabled(false);
        assert!(!color_enabled());
        set_color_enabled(true);
        assert!(color_enabled());
    }
}
