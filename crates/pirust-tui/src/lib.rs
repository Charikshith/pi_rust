//! `pirust-tui` — self-contained inline terminal UI library.
//!
//! pirust port of `packages/tui` (`@earendil-works/pi-tui`). See
//! `docs/analysis/05-tui.md`. Ported literally (NOT ratatui); `crossterm` is a
//! thin syscall shim only. Renderer + editor land in feat-006.

#![forbid(unsafe_code)]

pub mod utils;

pub use utils::{slice_by_column, truncate_to_width, visible_width, wrap_text_with_ansi};

/// Returns the crate name — placeholder until the renderer (feat-006) lands.
pub fn name() -> &'static str {
    "pirust-tui"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_is_stable() {
        assert_eq!(name(), "pirust-tui");
    }
}
