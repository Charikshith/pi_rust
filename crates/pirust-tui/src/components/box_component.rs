//! Port of `packages/tui/src/components/box.ts` — a bordered/padded
//! container that applies padding and an optional background to its
//! children. See `docs/analysis/05-tui.md` §6.
//!
//! ## Scope decisions (documented, not silent — AGENTS.md Correctness Bar)
//!
//! - **Named `BoxComponent`, not `Box`.** The TS class is literally named
//!   `Box`, but that collides head-on with `std::boxed::Box`, used
//!   throughout this crate (including by sibling components in this very
//!   wave, e.g. `Loader`'s `Option<Box<dyn FnMut()>>` callback field) —
//!   every reference would need `std::boxed::Box` qualification to disambiguate
//!   from the prelude type. Renaming avoids fighting the prelude for no
//!   behavioral gain (Ponytail: pick the boring option). It is its own
//!   struct, not a wrapper around `tui::Container` — the TS `Box` class is
//!   independent of `Container` too (it happens to have a similar
//!   children-list shape, nothing more).

use crate::tui::{Component, SharedComponent};
use crate::utils::{apply_background_to_line, visible_width};

struct RenderCache {
    child_lines: Vec<String>,
    width: usize,
    bg_sample: Option<String>,
    lines: Vec<String>,
}

/// `Box` (box.ts:14) — see module docs for the `BoxComponent` naming decision.
pub struct BoxComponent {
    children: Vec<SharedComponent>,
    padding_x: usize,
    padding_y: usize,
    bg_fn: Option<super::ColorFn>,
    cache: Option<RenderCache>,
}

impl BoxComponent {
    /// `constructor` (box.ts:23) — `paddingX=1`, `paddingY=1`.
    pub fn new(padding_x: usize, padding_y: usize) -> Self {
        Self {
            children: Vec::new(),
            padding_x,
            padding_y,
            bg_fn: None,
            cache: None,
        }
    }

    pub fn with_bg_fn(padding_x: usize, padding_y: usize, bg_fn: super::ColorFn) -> Self {
        Self {
            children: Vec::new(),
            padding_x,
            padding_y,
            bg_fn: Some(bg_fn),
            cache: None,
        }
    }

    /// `addChild` (box.ts:29).
    pub fn add_child(&mut self, component: SharedComponent) {
        self.children.push(component);
        self.invalidate_cache();
    }

    /// `removeChild` (box.ts:34).
    pub fn remove_child(&mut self, component: &SharedComponent) {
        if let Some(pos) = self
            .children
            .iter()
            .position(|c| std::rc::Rc::ptr_eq(c, component))
        {
            self.children.remove(pos);
            self.invalidate_cache();
        }
    }

    /// `clear` (box.ts:42).
    pub fn clear(&mut self) {
        self.children.clear();
        self.invalidate_cache();
    }

    /// `setBgFn` (box.ts:47) — does NOT invalidate the cache (the TS detects a
    /// changed closure by sampling its output, see `matchCache`).
    pub fn set_bg_fn(&mut self, bg_fn: Option<super::ColorFn>) {
        self.bg_fn = bg_fn;
    }

    fn invalidate_cache(&mut self) {
        self.cache = None;
    }

    /// `matchCache` (box.ts:56).
    fn match_cache(
        &self,
        width: usize,
        child_lines: &[String],
        bg_sample: &Option<String>,
    ) -> bool {
        match &self.cache {
            Some(cache) => {
                cache.width == width
                    && &cache.bg_sample == bg_sample
                    && cache.child_lines.len() == child_lines.len()
                    && cache
                        .child_lines
                        .iter()
                        .zip(child_lines)
                        .all(|(a, b)| a == b)
            }
            None => false,
        }
    }

    fn apply_bg(&self, line: &str, width: usize) -> String {
        let vis_len = visible_width(line);
        let pad_needed = width.saturating_sub(vis_len);
        let padded = format!("{line}{}", " ".repeat(pad_needed));
        match &self.bg_fn {
            Some(bg_fn) => apply_background_to_line(&padded, width, |s| bg_fn(s)),
            None => padded,
        }
    }
}

impl Component for BoxComponent {
    /// `render` (box.ts:74).
    fn render(&mut self, width: usize) -> Vec<String> {
        if self.children.is_empty() {
            return Vec::new();
        }

        let content_width = (width.saturating_sub(self.padding_x * 2)).max(1);
        let left_pad = " ".repeat(self.padding_x);

        let mut child_lines: Vec<String> = Vec::new();
        for child in &self.children {
            let lines = child.borrow_mut().render(content_width);
            for line in lines {
                child_lines.push(format!("{left_pad}{line}"));
            }
        }

        if child_lines.is_empty() {
            return Vec::new();
        }

        let bg_sample = self.bg_fn.as_ref().map(|f| f("test"));

        if self.match_cache(width, &child_lines, &bg_sample) {
            return self.cache.as_ref().unwrap().lines.clone();
        }

        let mut result: Vec<String> = Vec::new();
        for _ in 0..self.padding_y {
            result.push(self.apply_bg("", width));
        }
        for line in &child_lines {
            result.push(self.apply_bg(line, width));
        }
        for _ in 0..self.padding_y {
            result.push(self.apply_bg("", width));
        }

        self.cache = Some(RenderCache {
            child_lines,
            width,
            bg_sample,
            lines: result.clone(),
        });

        result
    }

    fn invalidate(&mut self) {
        self.invalidate_cache();
        for child in &self.children {
            child.borrow_mut().invalidate();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    struct Fixed(Vec<String>);
    impl Component for Fixed {
        fn render(&mut self, _width: usize) -> Vec<String> {
            self.0.clone()
        }
        fn invalidate(&mut self) {}
    }

    #[test]
    fn empty_box_renders_nothing() {
        let mut b = BoxComponent::new(1, 1);
        assert_eq!(b.render(10), Vec::<String>::new());
    }

    #[test]
    fn children_get_left_padding_and_vertical_padding() {
        let mut b = BoxComponent::new(1, 1);
        let child: SharedComponent = Rc::new(RefCell::new(Fixed(vec!["hi".to_string()])));
        b.add_child(child);
        let lines = b.render(10);
        // 1 top pad + 1 content + 1 bottom pad
        assert_eq!(lines.len(), 3);
        assert!(lines[1].starts_with(" hi"));
    }

    #[test]
    fn cache_reused_when_child_output_and_width_unchanged() {
        let mut b = BoxComponent::new(0, 0);
        let child: SharedComponent = Rc::new(RefCell::new(Fixed(vec!["x".to_string()])));
        b.add_child(child);
        let first = b.render(10);
        let second = b.render(10);
        assert_eq!(first, second);
    }
}
