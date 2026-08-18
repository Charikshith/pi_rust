//! Port of `packages/tui/src/components/spacer.ts` — renders N empty lines.
//! See `docs/analysis/05-tui.md` §6.

use crate::tui::Component;

/// `Spacer` (spacer.ts:6).
pub struct Spacer {
    lines: usize,
}

impl Spacer {
    /// `constructor` (spacer.ts:9) — `lines=1`.
    pub fn new(lines: usize) -> Self {
        Self { lines }
    }

    /// `setLines` (spacer.ts:13).
    pub fn set_lines(&mut self, lines: usize) {
        self.lines = lines;
    }
}

impl Component for Spacer {
    /// `render` (spacer.ts:21) — ignores `width`.
    fn render(&mut self, _width: usize) -> Vec<String> {
        vec![String::new(); self.lines]
    }

    fn invalidate(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_n_empty_lines() {
        let mut s = Spacer::new(3);
        assert_eq!(s.render(80), vec!["", "", ""]);
    }

    #[test]
    fn set_lines_changes_count() {
        let mut s = Spacer::new(1);
        s.set_lines(5);
        assert_eq!(s.render(10).len(), 5);
    }

    #[test]
    fn zero_lines_renders_nothing() {
        let mut s = Spacer::new(0);
        assert_eq!(s.render(10), Vec::<String>::new());
    }
}
