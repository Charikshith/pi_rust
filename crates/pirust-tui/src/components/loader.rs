//! Port of `packages/tui/src/components/loader.ts` — an animated spinner
//! that extends `Text`. See `docs/analysis/05-tui.md` §6.
//!
//! ## Scope decisions (documented, not silent — AGENTS.md Correctness Bar)
//!
//! - **No owned `TUI` reference, no self-ticking timer.** The TS constructor
//!   takes a live `ui: TUI` and its `setInterval`-driven animation calls
//!   `ui.requestRender()` on every frame advance — the same
//!   no-owned-event-loop story as Wave 2's `StdinBuffer::flush()` and Wave
//!   4's `TUI::request_render`/`poll()`. `Loader` here never spawns a timer
//!   and never holds a `TUI` handle (which would also be an aliasing problem
//!   — `Loader` normally lives *inside* the same component tree the `TUI` it
//!   would reference owns). Instead, [`Loader::tick`] advances the animation
//!   frame and updates the displayed text; the caller's own event loop
//!   (`feat-007`, same as every other timer named in this crate) is
//!   responsible for invoking `tick()` on the configured interval AND
//!   calling `TUI::request_render(false)` itself afterward — the two calls
//!   the TS's `updateDisplay()` makes together (`setText`, then
//!   `ui.requestRender()`) are simply split across two owners here.
//! - **`extends Text`** becomes composition: `Loader` holds a `Text` and
//!   delegates `render`/`invalidate`, prepending the TS's own hardcoded
//!   leading blank line.

use crate::components::text::Text;
use crate::tui::Component;

/// `LoaderIndicatorOptions` (loader.ts:4).
#[derive(Debug, Clone, Default)]
pub struct LoaderIndicatorOptions {
    /// Animation frames. `Some(vec![])` hides the indicator (matches the TS's
    /// "empty array to hide" contract); `None` means "use the defaults".
    pub frames: Option<Vec<String>>,
    pub interval_ms: Option<u64>,
}

const DEFAULT_INTERVAL_MS: u64 = 80;

fn default_frames() -> Vec<String> {
    ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

/// `Loader extends Text` (loader.ts:17) — see module docs for the timer and
/// `extends` adaptations.
pub struct Loader {
    text: Text,
    frames: Vec<String>,
    interval_ms: u64,
    current_frame: usize,
    render_indicator_verbatim: bool,
    spinner_color_fn: Box<dyn Fn(&str) -> String>,
    message_color_fn: Box<dyn Fn(&str) -> String>,
    message: String,
}

impl Loader {
    /// `constructor` (loader.ts:28) — `message="Loading..."`.
    pub fn new(
        spinner_color_fn: Box<dyn Fn(&str) -> String>,
        message_color_fn: Box<dyn Fn(&str) -> String>,
        message: impl Into<String>,
        indicator: Option<LoaderIndicatorOptions>,
    ) -> Self {
        let mut loader = Self {
            text: Text::new("", 1, 0),
            frames: default_frames(),
            interval_ms: DEFAULT_INTERVAL_MS,
            current_frame: 0,
            render_indicator_verbatim: false,
            spinner_color_fn,
            message_color_fn,
            message: message.into(),
        };
        loader.set_indicator(indicator);
        loader
    }

    /// `setMessage` (loader.ts:59).
    pub fn set_message(&mut self, message: impl Into<String>) {
        self.message = message.into();
        self.update_display();
    }

    /// `setIndicator` (loader.ts:64).
    pub fn set_indicator(&mut self, indicator: Option<LoaderIndicatorOptions>) {
        self.render_indicator_verbatim = indicator.is_some();
        self.frames = indicator
            .as_ref()
            .and_then(|i| i.frames.clone())
            .unwrap_or_else(default_frames);
        self.interval_ms = indicator
            .and_then(|i| i.interval_ms)
            .filter(|&ms| ms > 0)
            .unwrap_or(DEFAULT_INTERVAL_MS);
        self.current_frame = 0;
        self.update_display();
    }

    /// Advance the animation by one frame and refresh the displayed text —
    /// see module docs, "No owned `TUI` reference". Caller must also call
    /// `TUI::request_render(false)` afterward. No-op if there are 0 or 1
    /// frames (`restartAnimation`'s `frames.length <= 1` guard).
    pub fn tick(&mut self) {
        if self.frames.len() <= 1 {
            return;
        }
        self.current_frame = (self.current_frame + 1) % self.frames.len();
        self.update_display();
    }

    /// The interval a caller's own timer should use between `tick()` calls.
    pub fn interval_ms(&self) -> u64 {
        self.interval_ms
    }

    /// `updateDisplay` (loader.ts:83).
    fn update_display(&mut self) {
        let frame = self
            .frames
            .get(self.current_frame)
            .cloned()
            .unwrap_or_default();
        let rendered_frame = if self.render_indicator_verbatim {
            frame.clone()
        } else {
            (self.spinner_color_fn)(&frame)
        };
        let indicator = if !frame.is_empty() {
            format!("{rendered_frame} ")
        } else {
            String::new()
        };
        let text = format!("{indicator}{}", (self.message_color_fn)(&self.message));
        self.text.set_text(text);
    }
}

impl Component for Loader {
    /// `render` (loader.ts:43) — prepends a blank line ahead of `Text::render`.
    fn render(&mut self, width: usize) -> Vec<String> {
        let mut result = vec![String::new()];
        result.extend(self.text.render(width));
        result
    }

    fn invalidate(&mut self) {
        self.text.invalidate();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> Box<dyn Fn(&str) -> String> {
        Box::new(|s: &str| s.to_string())
    }

    #[test]
    fn default_render_has_leading_blank_line_and_default_frame() {
        let mut loader = Loader::new(identity(), identity(), "Loading...", None);
        let lines = loader.render(40);
        assert_eq!(lines[0], "");
        assert!(lines[1].contains("⠋"));
        assert!(lines[1].contains("Loading..."));
    }

    #[test]
    fn tick_advances_frame_and_wraps() {
        let mut loader = Loader::new(identity(), identity(), "x", None);
        loader.tick();
        let lines = loader.render(40);
        assert!(lines[1].contains("⠙"));
    }

    #[test]
    fn single_frame_indicator_does_not_animate_on_tick() {
        let mut loader = Loader::new(
            identity(),
            identity(),
            "x",
            Some(LoaderIndicatorOptions {
                frames: Some(vec!["*".to_string()]),
                interval_ms: None,
            }),
        );
        loader.tick();
        let lines = loader.render(40);
        assert!(lines[1].trim_start().starts_with("* "));
    }

    #[test]
    fn empty_frames_hides_indicator() {
        let mut loader = Loader::new(
            identity(),
            identity(),
            "x",
            Some(LoaderIndicatorOptions {
                frames: Some(vec![]),
                interval_ms: None,
            }),
        );
        let lines = loader.render(40);
        assert_eq!(lines[1].trim(), "x");
    }
}
