//! Port of `packages/tui/src/components/cancellable-loader.ts` — a `Loader`
//! that can be cancelled with Escape. See `docs/analysis/05-tui.md` §6.
//!
//! ## Scope decisions (documented, not silent — AGENTS.md Correctness Bar)
//!
//! - **`AbortController`/`AbortSignal` → `Arc<AtomicBool>`.** Rust has no
//!   stdlib cancellation-token type; the TS uses `AbortController` purely as
//!   a shared, cloneable "has this been cancelled yet" flag with a callback
//!   on transition — an `Arc<AtomicBool>` is the direct, dependency-free
//!   equivalent (`aborted()` reads it, `abort()` sets it once and fires
//!   `on_abort`). [`CancellableLoader::signal`] returns a clone of the same
//!   `Arc`, matching `get signal(): AbortSignal`'s "always the same
//!   observable state" contract.
//! - **`onAbort` as `Option<Box<dyn FnMut()>>`.** This crate's first
//!   TS-optional-callback-property port (`Loader`, this wave, has no
//!   optional callbacks of its own); later components in this wave
//!   (`Input::on_submit`/`on_escape`, `SelectList::on_select`/`on_cancel`/
//!   `on_selection_change`) all use this same `Option<Box<dyn FnMut(...)>>`
//!   idiom for consistency.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::components::loader::{Loader, LoaderIndicatorOptions};
use crate::keybindings::{get_keybindings, Keybinding};
use crate::tui::Component;

/// `CancellableLoader extends Loader` (cancellable-loader.ts:13) — see module
/// docs for the `AbortController` and `onAbort` adaptations.
pub struct CancellableLoader {
    loader: Loader,
    aborted: Arc<AtomicBool>,
    pub on_abort: Option<Box<dyn FnMut()>>,
}

impl CancellableLoader {
    pub fn new(
        spinner_color_fn: Box<dyn Fn(&str) -> String>,
        message_color_fn: Box<dyn Fn(&str) -> String>,
        message: impl Into<String>,
        indicator: Option<LoaderIndicatorOptions>,
    ) -> Self {
        Self {
            loader: Loader::new(spinner_color_fn, message_color_fn, message, indicator),
            aborted: Arc::new(AtomicBool::new(false)),
            on_abort: None,
        }
    }

    /// `get signal()` (cancellable-loader.ts:20) — a clone of the shared flag.
    pub fn signal(&self) -> Arc<AtomicBool> {
        self.aborted.clone()
    }

    /// `get aborted()` (cancellable-loader.ts:25).
    pub fn aborted(&self) -> bool {
        self.aborted.load(Ordering::SeqCst)
    }

    pub fn tick(&mut self) {
        self.loader.tick();
    }

    pub fn set_message(&mut self, message: impl Into<String>) {
        self.loader.set_message(message);
    }

    /// `dispose` (cancellable-loader.ts:37) — `Loader` has no `stop()` here
    /// since this port never starts a self-owned timer (see `loader.rs`'s
    /// module docs); kept as a no-op for TS-API-shape parity.
    pub fn dispose(&mut self) {}
}

impl Component for CancellableLoader {
    fn render(&mut self, width: usize) -> Vec<String> {
        self.loader.render(width)
    }

    fn invalidate(&mut self) {
        self.loader.invalidate();
    }

    /// `handleInput` (cancellable-loader.ts:29).
    fn handle_input(&mut self, data: &str) {
        if get_keybindings().matches(data, Keybinding::SelectCancel) {
            self.aborted.store(true, Ordering::SeqCst);
            if let Some(on_abort) = &mut self.on_abort {
                on_abort();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> Box<dyn Fn(&str) -> String> {
        Box::new(|s: &str| s.to_string())
    }

    #[test]
    fn escape_aborts_and_fires_callback() {
        let mut loader = CancellableLoader::new(identity(), identity(), "x", None);
        let fired = Arc::new(AtomicBool::new(false));
        let fired_clone = fired.clone();
        loader.on_abort = Some(Box::new(move || fired_clone.store(true, Ordering::SeqCst)));
        assert!(!loader.aborted());
        loader.handle_input("\x1b"); // escape
        assert!(loader.aborted());
        assert!(fired.load(Ordering::SeqCst));
    }

    #[test]
    fn signal_reflects_shared_state() {
        let mut loader = CancellableLoader::new(identity(), identity(), "x", None);
        let sig = loader.signal();
        assert!(!sig.load(Ordering::SeqCst));
        loader.handle_input("\x1b");
        assert!(sig.load(Ordering::SeqCst));
    }

    #[test]
    fn non_matching_input_does_not_abort() {
        let mut loader = CancellableLoader::new(identity(), identity(), "x", None);
        loader.handle_input("a");
        assert!(!loader.aborted());
    }
}
