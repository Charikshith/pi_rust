//! Minimal interactive theme (feat-007 Wave 3).
//!
//! Port of `packages/coding-agent/src/modes/interactive/theme/theme.ts` +
//! `dark.json` — only the colors the tool-execution and chat rendering use
//! this wave. `fg(name, text)`/`bg(name, text)` wrap text in ANSI truecolor
//! sequences, matching Pi's `theme.fg`/`theme.bg` behavior. The full theme
//! (theme-controller, light/dark switching, all color keys) is a later wave.
//!
//! The values are the `dark.json` defaults (the interactive theme's default
//! when no saved theme is set): tool backgrounds are the raw `vars`, the
//! semantic colors resolve through `colors` to the `vars`.

use pirust_tui::components::ColorFn;

/// `theme.fg(name, text)` — a `ColorFn` for a foreground colour.
///
/// Delegates to [`crate::interactive_a11y::fg`] rather than emitting a
/// truecolor sequence unconditionally, because this is the single place the
/// interactive TUI produces colour: every tool box, notice, welcome line and
/// picker row goes through here. Routing it through the accessibility layer is
/// what makes `NO_COLOR`, `TERM=dumb`, a 256-colour terminal and a non-TTY
/// pipe all behave correctly, instead of each caller having to remember to ask.
///
/// The degradation ladder lives there: truecolor → nearest xterm-256 →
/// nearest of the basic 16 → identity (no escape bytes at all).
pub fn fg(hex: &str) -> ColorFn {
    crate::interactive_a11y::fg(hex)
}

/// `theme.bg(name, text)` — a `ColorFn` for a background colour. See [`fg`].
pub fn bg(hex: &str) -> ColorFn {
    crate::interactive_a11y::bg(hex)
}

/// Semantic color values (dark.json `vars` + `colors` resolution) used this
/// wave.
pub mod dark {
    pub const TEXT: &str = "#d4d4d4";
    pub const GRAY: &str = "#808080";
    pub const TOOL_PENDING_BG: &str = "#282832";
    pub const TOOL_SUCCESS_BG: &str = "#283228";
    pub const TOOL_ERROR_BG: &str = "#3c2828";
    /// `userMessageBg` (dark.json:16, `userMsgBg`) — the box background a
    /// typed user message renders on, distinguishing it from plain
    /// (unboxed) assistant text and from the tool-call boxes above.
    pub const USER_MESSAGE_BG: &str = "#343541";
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interactive_a11y::{with_settings, A11ySettings, ColorMode};

    /// The theme now consults the process-wide accessibility policy, so every
    /// test here pins it through the shared serialising helper.
    fn colored<R>(mode: ColorMode, body: impl FnOnce() -> R) -> R {
        with_settings(
            A11ySettings {
                color: mode,
                ..A11ySettings::default()
            },
            body,
        )
    }

    #[test]
    fn fg_emits_truecolor_sequence() {
        colored(ColorMode::Truecolor, || {
            assert_eq!(fg(dark::TEXT)("hi"), "\x1b[38;2;212;212;212mhi\x1b[0m");
        });
    }

    #[test]
    fn bg_emits_truecolor_sequence() {
        colored(ColorMode::Truecolor, || {
            assert_eq!(
                bg(dark::TOOL_PENDING_BG)("x"),
                "\x1b[48;2;40;40;50mx\x1b[0m"
            );
        });
    }

    /// The whole point of routing the theme through the a11y layer: a
    /// `NO_COLOR` run must emit no escape bytes at all, not colour text and
    /// hope the terminal ignores it.
    #[test]
    fn no_color_emits_no_escape_bytes() {
        colored(ColorMode::None, || {
            assert_eq!(fg(dark::TEXT)("hi"), "hi");
            assert_eq!(bg(dark::TOOL_ERROR_BG)("hi"), "hi");
        });
    }

    #[test]
    fn ansi256_quantises_instead_of_truecolor() {
        colored(ColorMode::Ansi256, || {
            let painted = fg(dark::TEXT)("hi");
            assert!(
                painted.starts_with("\x1b[38;5;"),
                "expected a 256-colour index, got {painted:?}"
            );
            assert!(painted.ends_with("mhi\x1b[0m"));
        });
    }
}
