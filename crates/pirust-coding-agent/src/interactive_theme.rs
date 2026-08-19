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

/// `theme.fg(name, text)` — a `ColorFn` that wraps text in a truecolor fg
/// sequence.
pub fn fg(hex: &str) -> ColorFn {
    let hex = hex.to_string();
    Box::new(move |text: &str| {
        let (r, g, b) = hex_to_rgb(&hex);
        format!("\x1b[38;2;{r};{g};{b}m{text}\x1b[0m")
    })
}

/// `theme.bg(name, text)` — a `ColorFn` that wraps text in a truecolor bg
/// sequence.
pub fn bg(hex: &str) -> ColorFn {
    let hex = hex.to_string();
    Box::new(move |text: &str| {
        let (r, g, b) = hex_to_rgb(&hex);
        format!("\x1b[48;2;{r};{g};{b}m{text}\x1b[0m")
    })
}

fn hex_to_rgb(hex: &str) -> (u32, u32, u32) {
    let normalized = hex.strip_prefix('#').unwrap_or(hex);
    let r = u32::from_str_radix(&normalized[0..2], 16).unwrap_or(0);
    let g = u32::from_str_radix(&normalized[2..4], 16).unwrap_or(0);
    let b = u32::from_str_radix(&normalized[4..6], 16).unwrap_or(0);
    (r, g, b)
}

/// Semantic color values (dark.json `vars` + `colors` resolution) used this
/// wave.
pub mod dark {
    pub const TEXT: &str = "#d4d4d4";
    pub const GRAY: &str = "#808080";
    pub const TOOL_PENDING_BG: &str = "#282832";
    pub const TOOL_SUCCESS_BG: &str = "#283228";
    pub const TOOL_ERROR_BG: &str = "#3c2828";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fg_emits_truecolor_sequence() {
        let f = fg(dark::TEXT);
        assert_eq!(f("hi"), "\x1b[38;2;212;212;212mhi\x1b[0m");
    }

    #[test]
    fn bg_emits_truecolor_sequence() {
        let f = bg(dark::TOOL_PENDING_BG);
        assert_eq!(f("x"), "\x1b[48;2;40;40;50mx\x1b[0m");
    }
}
