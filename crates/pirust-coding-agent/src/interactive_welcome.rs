//! First-run/ready and missing-configuration TUI screens
//! (`docs/tui-design-samples.html` §1 "First-run / ready state").
//!
//! The spec's two customer-facing samples for this section are:
//!
//! - **Empty workspace** — a welcome block: `Pi Rust · {cwd}`, a "Ready to
//!   help" line, `Model: … · Provider: …`, `Tools: …`, and a hint line
//!   (`Enter submit · Ctrl+C cancel · /help commands`). [`WelcomeScreen`] /
//!   [`welcome_lines`].
//! - **Missing configuration** — an actionable in-TUI error naming the
//!   concrete file to fix, never a stack trace, with `[Open setup help]
//!   [Quit]` choices. [`SetupHelpScreen`] / [`setup_help_lines`].
//!
//! Neither screen is wired into [`crate::interactive_mode::InteractiveMode`]
//! by this module — that seam (`InteractiveMode::new`,
//! `interactive_mode.rs:399-520`) is owned by a sibling change. This module
//! only supplies the [`pirust_tui::tui::Component`]s and the pure line
//! builders they wrap, matching `Text`'s own render-time width cache
//! (`pirust-tui/src/components/text.rs:76-134`): lines are rebuilt once per
//! distinct width and `.clone()`d on a repeat width, never recomputed from
//! scratch on every frame.
//!
//! ## Tool names are not on the TUI's session seam
//!
//! [`TuiRuntimeStatus`] (`print_mode.rs:672-691`) is the *only* projection
//! `interactive_mode.rs` reads for status rendering (`session_status`,
//! `interactive_mode.rs:1501-1533`), and it has no tool-name field — only
//! `tools_enabled: bool`. The actual enabled-tool set is assembled in
//! `sdk.rs`'s `initial_active_tool_names` / `selected_tool_names`
//! (`sdk.rs:146`, `sdk.rs:409-429`) when the tool registry is built, and is
//! never threaded back through `TuiRuntimeInfo`/`InteractiveSession`
//! (`interactive_mode.rs:59-60`). So [`WelcomeScreen`] and [`welcome_lines`]
//! **accept** `tool_names: &[&str]` from the caller rather than reaching for
//! a list that does not exist on this seam — the caller (wherever it builds
//! the tool registry) is expected to pass the names it already resolved.
//!
//! ## Reusing `auth_guidance.rs`
//!
//! [`crate::auth_guidance::format_no_models_available_message`] already
//! produces the exact actionable "no models" text this screen needs (login
//! help + `providers.md`/`models.md` doc paths) — [`SetupHelpScreen::new`]
//! and [`SetupHelpScreen::from_process_env`] call it rather than duplicating
//! its wording. This module adds only what `auth_guidance.rs` does not
//! already own: the concrete `models.json` path (from
//! [`crate::config::ConfigEnv::models_path`]) and the two-choice picker.

use pirust_tui::keys::matches_key;
use pirust_tui::tui::Component;
use pirust_tui::utils::{truncate_to_width, visible_width};

use crate::interactive_theme::{self, dark};

// =============================================================================
// Welcome screen (spec: "Empty workspace" sample)
// =============================================================================

/// Below this column count the `Pi Rust · {cwd}` header line is dropped —
/// the first line cut when space is scarce. It is the least essential line
/// in the block: the persistent status band (`session_status`,
/// `interactive_mode.rs:1526-1531`) already prints `cwd: {cwd}` on every
/// frame, so losing the header here costs nothing but a repeated label.
const MIN_WIDTH_FOR_HEADER: usize = 60;

/// Below this column count `Model: … · Provider: …` splits onto two lines
/// instead of being squeezed (and truncated into mush) onto one.
const MIN_WIDTH_FOR_COMBINED_MODEL_LINE: usize = 48;

/// Below this column count the blank spacer row before the hint line is
/// dropped, trading whitespace for one more usable row — relevant to the
/// spec's 80×24 usability floor as much as the column width is.
const MIN_WIDTH_FOR_SPACER: usize = 30;

const READY_LINE: &str = "Ready to help with this project.";
const HINT_LINE: &str = "Enter submit \u{b7} Ctrl+C cancel \u{b7} /help commands";
const ELLIPSIS: &str = "...";

/// Build the plain-text welcome block (no ANSI), degrading tier by tier as
/// `width` shrinks rather than word-wrapping any single line into several —
/// the customer's own "stays usable at 80×24" bar. See the `MIN_WIDTH_FOR_*`
/// constants for exactly what is dropped and why. Pure and terminal-free, so
/// tests assert directly on the returned strings.
pub fn welcome_lines(
    cwd: &str,
    model_name: &str,
    provider: &str,
    tool_names: &[&str],
    width: usize,
) -> Vec<String> {
    let width = width.max(1);
    let mut lines: Vec<String> = Vec::with_capacity(8);

    if width >= MIN_WIDTH_FOR_HEADER {
        let mut header = String::with_capacity("Pi Rust \u{b7} ".len() + cwd.len());
        header.push_str("Pi Rust");
        if !cwd.is_empty() {
            header.push_str(" \u{b7} ");
            header.push_str(cwd);
        }
        lines.push(truncate_to_width(&header, width, ELLIPSIS, false));
        lines.push(String::new());
    }

    lines.push(truncate_to_width(READY_LINE, width, ELLIPSIS, false));

    let mut model_line = String::with_capacity("Model: ".len() + model_name.len());
    model_line.push_str("Model: ");
    model_line.push_str(model_name);
    let mut provider_line = String::with_capacity("Provider: ".len() + provider.len());
    provider_line.push_str("Provider: ");
    provider_line.push_str(provider);

    if width >= MIN_WIDTH_FOR_COMBINED_MODEL_LINE {
        let mut combined =
            String::with_capacity(model_line.len() + provider_line.len() + " \u{b7} ".len());
        combined.push_str(&model_line);
        combined.push_str(" \u{b7} ");
        combined.push_str(&provider_line);
        lines.push(truncate_to_width(&combined, width, ELLIPSIS, false));
    } else {
        lines.push(truncate_to_width(&model_line, width, ELLIPSIS, false));
        lines.push(truncate_to_width(&provider_line, width, ELLIPSIS, false));
    }

    lines.push(tool_names_line(tool_names, width));

    if width >= MIN_WIDTH_FOR_SPACER {
        lines.push(String::new());
    }
    lines.push(truncate_to_width(HINT_LINE, width, ELLIPSIS, false));

    lines
}

/// `Tools: {joined names}` built with a single `push_str` pass (one
/// allocation for the line, sized up front) rather than a `.join(", ")`
/// followed by a second `format!` allocation. When the full roster would
/// overflow `width`, trailing names are dropped and replaced with a
/// `"+N more"` counter instead of truncating a name mid-word.
fn tool_names_line(tool_names: &[&str], width: usize) -> String {
    const PREFIX: &str = "Tools: ";
    const NONE: &str = "Tools: (none enabled)";
    // Budget reserved for a trailing "+N more" counter so it never itself
    // gets clipped away by the final `truncate_to_width` safety net.
    const MORE_BUDGET: usize = 9; // " +999 more" worst case, rounded down a hair.

    if tool_names.is_empty() {
        return truncate_to_width(NONE, width, ELLIPSIS, false);
    }

    let mut capacity = PREFIX.len();
    for name in tool_names {
        capacity += name.len() + 2;
    }
    let mut line = String::with_capacity(capacity);
    line.push_str(PREFIX);

    let mut shown = 0usize;
    for (i, name) in tool_names.iter().enumerate() {
        let sep_len = if i > 0 { 2 } else { 0 };
        let remaining_after = tool_names.len() - i - 1;
        let reserve = if remaining_after > 0 { MORE_BUDGET } else { 0 };
        if shown > 0 && visible_width(&line) + sep_len + name.len() + reserve > width {
            break;
        }
        if i > 0 {
            line.push_str(", ");
        }
        line.push_str(name);
        shown += 1;
    }

    let remaining = tool_names.len() - shown;
    if remaining > 0 {
        line.push_str(" +");
        line.push_str(&remaining.to_string());
        line.push_str(" more");
    }

    truncate_to_width(&line, width, ELLIPSIS, false)
}

/// `fg`/`bg` closures allocate on every call (`interactive_theme::fg`,
/// `interactive_theme.rs:17-23`), so they are built once per render and
/// reused across lines rather than reconstructed per line.
struct WelcomeColors {
    normal: pirust_tui::components::ColorFn,
    dim: pirust_tui::components::ColorFn,
}

impl WelcomeColors {
    fn new() -> Self {
        Self {
            normal: interactive_theme::fg(dark::TEXT),
            dim: interactive_theme::fg(dark::GRAY),
        }
    }

    /// The "Ready to help…" line renders at normal brightness (the sample's
    /// `assistant` class); every other line — header, model/provider,
    /// tools, hint — is dim (the sample's `dim` class). Blank spacer rows
    /// are left untouched: wrapping empty text in a color sequence would
    /// only add bytes with nothing visible to color.
    fn apply(&self, line: &str) -> String {
        if line.is_empty() {
            String::new()
        } else if line == READY_LINE {
            (self.normal)(line)
        } else {
            (self.dim)(line)
        }
    }
}

/// The first-run/ready welcome block (spec: "Empty workspace" sample) — cwd,
/// model, provider, and the enabled tool names, ending in the
/// `Enter submit · Ctrl+C cancel · /help commands` hint line.
///
/// Static once constructed: nothing here changes turn-over-turn, so
/// [`Component::render`] caches by width exactly like [`pirust_tui::components::text::Text`]
/// does, and only re-derives the colored lines when the terminal is resized.
pub struct WelcomeScreen {
    cwd: String,
    model_name: String,
    provider: String,
    tool_names: Vec<String>,
    /// Set by [`Self::dismiss`] once the first prompt is submitted; makes
    /// `render` return no rows at all.
    dismissed: bool,
    cached_width: Option<usize>,
    cached_lines: Option<Vec<String>>,
}

impl WelcomeScreen {
    /// Build the block from already-owned display strings. `tool_names` is
    /// copied once here (each name is typically a short static tool id) so
    /// the component can outlive whatever built the tool registry.
    pub fn new(
        cwd: impl Into<String>,
        model_name: impl Into<String>,
        provider: impl Into<String>,
        tool_names: &[&str],
    ) -> Self {
        Self {
            cwd: cwd.into(),
            model_name: model_name.into(),
            provider: provider.into(),
            tool_names: tool_names.iter().map(|s| (*s).to_string()).collect(),
            dismissed: false,
            cached_width: None,
            cached_lines: None,
        }
    }

    /// Build the block from the same [`crate::print_mode::TuiRuntimeStatus`]
    /// projection `session_status` (`interactive_mode.rs:1501-1533`) draws
    /// its provider/model segments from, so this block and the persistent
    /// status line never disagree about which model is active. `tool_names`
    /// still must come from the caller — see the module docs on why it is
    /// not reachable from `status` itself.
    pub fn from_status(
        cwd: impl Into<String>,
        status: &crate::print_mode::TuiRuntimeStatus,
        tool_names: &[&str],
    ) -> Self {
        Self::new(
            cwd,
            status.model_name.clone(),
            status.provider.clone(),
            tool_names,
        )
    }

    /// Stop rendering. Called once the user submits their first prompt.
    ///
    /// This block is a *first-run* affordance — it answers "what am I talking
    /// to, and where?" before there is any conversation to look at. Once work
    /// starts it is pure cost: seven rows of an 80×24 terminal, permanently,
    /// repeating a cwd/model the status line already shows on every frame.
    /// The spec's own acceptance bar is that 80×24 stays usable with "no
    /// essential information hidden", and a caught regression made the point
    /// concretely: with this block mounted, a tool result's
    /// `… (N more lines)` truncation hint was pushed off the bottom of a
    /// 24-row terminal.
    ///
    /// Dismissing is one-way and idempotent. `cached_lines` is dropped so the
    /// text itself is freed, not merely hidden.
    ///
    /// The early return matters: callers invoke this on *every* session event
    /// (any activity makes a first-run block stale, however it arrived), so
    /// after the first call this must not keep clearing a cache that is
    /// already empty — that would mark the component dirty on every event and
    /// defeat the render cache it is trying to free.
    pub fn dismiss(&mut self) {
        if self.dismissed {
            return;
        }
        self.dismissed = true;
        self.cached_width = None;
        self.cached_lines = None;
    }

    /// Whether [`Self::dismiss`] has been called.
    pub fn is_dismissed(&self) -> bool {
        self.dismissed
    }
}

impl Component for WelcomeScreen {
    fn render(&mut self, width: usize) -> Vec<String> {
        if self.dismissed {
            return Vec::new();
        }
        if let (Some(cached_width), Some(cached_lines)) = (self.cached_width, &self.cached_lines) {
            if cached_width == width {
                return cached_lines.clone();
            }
        }

        let tool_refs: Vec<&str> = self.tool_names.iter().map(String::as_str).collect();
        let plain = welcome_lines(
            &self.cwd,
            &self.model_name,
            &self.provider,
            &tool_refs,
            width,
        );
        let colors = WelcomeColors::new();
        let colored: Vec<String> = plain.iter().map(|line| colors.apply(line)).collect();

        self.cached_width = Some(width);
        self.cached_lines = Some(colored.clone());
        colored
    }

    fn invalidate(&mut self) {
        self.cached_width = None;
        self.cached_lines = None;
    }
}

// =============================================================================
// Missing-configuration screen (spec: "Missing configuration" sample)
// =============================================================================

/// Which of the two choices is currently focused. Only two states, so
/// `Tab`/`Shift+Tab`/arrows all just flip between them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupChoice {
    /// `[Open setup help]` — the caller opens `auth_guidance`'s referenced
    /// docs (`providers.md`/`models.md`) or an equivalent help surface.
    OpenHelp,
    /// `[Quit]` — the caller exits the TUI.
    Quit,
}

impl SetupChoice {
    /// Flip to the other choice — with exactly two options "next" and
    /// "previous" are the same operation, so `Tab`, `Shift+Tab`, `Left` and
    /// `Right` all route here.
    fn toggled(self) -> Self {
        match self {
            SetupChoice::OpenHelp => SetupChoice::Quit,
            SetupChoice::Quit => SetupChoice::OpenHelp,
        }
    }
}

/// Visible glyph marking the focused choice. The spec requires focus to
/// never be color-only, so the marker is baked directly into the plain text
/// returned by [`setup_help_lines`] — it survives even when nothing
/// downstream applies color at all (e.g. a piped/non-ANSI terminal).
const FOCUS_GLYPH: &str = "\u{25b8} ";
const NO_FOCUS_GLYPH: &str = "  ";

fn choice_label(label: &str, focused: bool) -> String {
    let glyph = if focused { FOCUS_GLYPH } else { NO_FOCUS_GLYPH };
    let mut out = String::with_capacity(glyph.len() + label.len());
    out.push_str(glyph);
    out.push_str(label);
    out
}

/// Truncate `text` keeping its **tail** (prefixing an ellipsis instead of
/// suffixing one) — used only for the `models.json` path line, where the
/// trailing `agent/models.json` segment is the part worth keeping visible
/// when the leading `C:\Users\...\` prefix does not fit.
fn truncate_path_left(text: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }
    if visible_width(text) <= max_width {
        return text.to_string();
    }
    let ellipsis = '\u{2026}';
    let budget = max_width.saturating_sub(1);
    let tail: String = {
        let mut rev: Vec<char> = text.chars().rev().take(budget).collect();
        rev.reverse();
        rev.into_iter().collect()
    };
    let mut out = String::with_capacity(tail.len() + ellipsis.len_utf8());
    out.push(ellipsis);
    out.push_str(&tail);
    out
}

/// The short, numbered "what to do" list — all three steps are always
/// shown; only their column width is ever adjusted. Unlike the welcome
/// block, nothing here is optional: the spec requires the actionable
/// message, the concrete path, the numbered list, *and* the two choices to
/// all be present at once.
const WHAT_TO_DO: [&str; 3] = [
    "Add a provider entry to models.json (see the docs above).",
    "Or run /login to authenticate a supported provider.",
    "Restart pirust \u{2014} it reloads models.json automatically.",
];

/// Build the plain-text missing-configuration screen (no ANSI): the
/// actionable heading, `guidance` (the reused `auth_guidance.rs` message),
/// the concrete `models_path`, the numbered what-to-do list, and the
/// `focus`-marked two-choice picker. Pure and terminal-free — `guidance` is
/// passed in rather than computed here so tests never depend on
/// `get_docs_path()`'s environment-sensitive output.
pub fn setup_help_lines(
    models_path: &str,
    guidance: &str,
    focus: SetupChoice,
    width: usize,
) -> Vec<String> {
    let width = width.max(1);
    let mut lines: Vec<String> = Vec::with_capacity(12 + guidance.lines().count());

    lines.push(truncate_to_width(
        "Could not start a model",
        width,
        ELLIPSIS,
        false,
    ));
    lines.push(String::new());

    for line in guidance.lines() {
        if line.is_empty() {
            lines.push(String::new());
        } else {
            lines.push(truncate_to_width(line, width, ELLIPSIS, false));
        }
    }
    lines.push(String::new());

    lines.push(truncate_to_width(
        "Add a provider in:",
        width,
        ELLIPSIS,
        false,
    ));
    let indent = "  ";
    let path_budget = width.saturating_sub(indent.len()).max(1);
    let mut path_line = String::with_capacity(indent.len() + models_path.len());
    path_line.push_str(indent);
    path_line.push_str(&truncate_path_left(models_path, path_budget));
    lines.push(path_line);
    lines.push(String::new());

    lines.push(truncate_to_width("What to do:", width, ELLIPSIS, false));
    for (i, step) in WHAT_TO_DO.iter().enumerate() {
        let mut item = String::with_capacity(6 + step.len());
        item.push_str("  ");
        item.push_str(&(i + 1).to_string());
        item.push_str(". ");
        item.push_str(step);
        lines.push(truncate_to_width(&item, width, ELLIPSIS, false));
    }
    lines.push(String::new());

    let open = choice_label("[Open setup help]", focus == SetupChoice::OpenHelp);
    let quit = choice_label("[Quit]", focus == SetupChoice::Quit);
    let mut choices = String::with_capacity(open.len() + quit.len() + 2);
    choices.push_str(&open);
    choices.push_str("  ");
    choices.push_str(&quit);
    lines.push(truncate_to_width(&choices, width, ELLIPSIS, false));

    lines
}

/// The missing-configuration screen (spec: "Missing configuration" sample):
/// an actionable, in-TUI error naming the concrete `models.json` path to
/// fix — never a stack trace — with `[Open setup help]`/`[Quit]` choices.
///
/// `interactive_theme::dark` currently defines no dedicated error/highlight
/// foreground (only `TOOL_*_BG` block backgrounds and `TEXT`/`GRAY`
/// foregrounds — `interactive_theme.rs:46-54`), and this module may not add
/// one (single-file constraint). [`Component::render`] therefore reuses
/// `TOOL_ERROR_BG` — the same background a failed tool execution already
/// renders on (`interactive_mode.rs:335`) — as the heading's error
/// indicator, and colors the focused choice with `TEXT` vs. `GRAY` as a
/// **supplementary** signal on top of the glyph [`setup_help_lines`] already
/// bakes into the plain text; the glyph alone satisfies "focus must not be
/// color-only".
pub struct SetupHelpScreen {
    models_path: String,
    guidance: String,
    focus: SetupChoice,
    cached_width: Option<usize>,
    cached_lines: Option<Vec<String>>,
}

impl SetupHelpScreen {
    /// Build the screen from an already-resolved `models.json` path. The
    /// actionable body text is `auth_guidance.rs`'s own message — computed
    /// once here, not duplicated.
    pub fn new(models_path: impl Into<String>) -> Self {
        Self {
            models_path: models_path.into(),
            guidance: crate::auth_guidance::format_no_models_available_message(),
            focus: SetupChoice::OpenHelp,
            cached_width: None,
            cached_lines: None,
        }
    }

    /// Resolve the real `models.json` path via
    /// [`crate::config::ConfigEnv::models_path`] (`config.ts:529-531`'s
    /// port, `config.rs:431-434`) — the same accessor `main.rs:313-319`
    /// uses to boot the model runtime — then build the screen from it.
    pub fn from_process_env() -> Result<Self, crate::config::ConfigPathError> {
        let env = crate::config::ConfigEnv::from_process_env();
        let models_path = env.models_path()?;
        Ok(Self::new(models_path))
    }

    /// The currently focused choice.
    pub fn focus(&self) -> SetupChoice {
        self.focus
    }

    fn set_focus(&mut self, focus: SetupChoice) {
        if self.focus != focus {
            self.focus = focus;
            self.invalidate();
        }
    }

    /// Route a raw input sequence: `Tab`/`Shift+Tab`/`Left`/`Right` move
    /// focus (returns `None`), `Enter` confirms the focused choice, and
    /// `Escape` always maps to [`SetupChoice::Quit`] regardless of focus —
    /// matching every other modal in this crate, where Esc is the universal
    /// "back out" key. Key names are exactly `pirust_tui::keys::matches_key`'s
    /// (`keys.rs:722-728`): `"escape"`, `"enter"`, `"tab"`, `"shift+tab"`,
    /// `"left"`, `"right"`.
    pub fn handle_key(&mut self, data: &str) -> Option<SetupChoice> {
        if matches_key(data, "escape") {
            return Some(SetupChoice::Quit);
        }
        if matches_key(data, "enter") {
            return Some(self.focus);
        }
        if matches_key(data, "tab")
            || matches_key(data, "shift+tab")
            || matches_key(data, "left")
            || matches_key(data, "right")
        {
            self.set_focus(self.focus.toggled());
        }
        None
    }
}

impl Component for SetupHelpScreen {
    fn render(&mut self, width: usize) -> Vec<String> {
        if let (Some(cached_width), Some(cached_lines)) = (self.cached_width, &self.cached_lines) {
            if cached_width == width {
                return cached_lines.clone();
            }
        }

        let plain = setup_help_lines(&self.models_path, &self.guidance, self.focus, width);
        let error_bg = interactive_theme::bg(dark::TOOL_ERROR_BG);
        let dim = interactive_theme::fg(dark::GRAY);
        let normal = interactive_theme::fg(dark::TEXT);

        let mut colored: Vec<String> = Vec::with_capacity(plain.len());
        for line in &plain {
            if line.is_empty() {
                colored.push(String::new());
                continue;
            }
            if line == "Could not start a model" {
                colored.push(error_bg(line));
            } else if line == "Add a provider in:" {
                colored.push(dim(line));
            } else if line.contains(FOCUS_GLYPH) || line.contains(NO_FOCUS_GLYPH) {
                // The choices line: color it as a whole, on top of the
                // glyph the plain text already carries.
                colored.push(normal(line));
            } else {
                colored.push(line.clone());
            }
        }

        self.cached_width = Some(width);
        self.cached_lines = Some(colored.clone());
        colored
    }

    fn invalidate(&mut self) {
        self.cached_width = None;
        self.cached_lines = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOOLS: [&str; 7] = ["read", "write", "edit", "bash", "grep", "find", "ls"];

    fn assert_fits(lines: &[String], width: usize) {
        for line in lines {
            assert!(
                visible_width(line) <= width,
                "line {line:?} exceeds width {width}"
            );
        }
    }

    // -------------------------------------------------------------------
    // welcome_lines
    // -------------------------------------------------------------------

    #[test]
    fn welcome_lines_wide_shows_everything_on_expected_lines() {
        let lines = welcome_lines("~/project", "Claude Sonnet 4.5", "anthropic", &TOOLS, 200);
        assert_fits(&lines, 200);
        assert!(lines.iter().any(|l| l == "Pi Rust \u{b7} ~/project"));
        assert!(lines.iter().any(|l| l == READY_LINE));
        assert!(lines
            .iter()
            .any(|l| l == "Model: Claude Sonnet 4.5 \u{b7} Provider: anthropic"));
        let tools_line = lines
            .iter()
            .find(|l| l.starts_with("Tools:"))
            .expect("tools line present");
        for name in TOOLS {
            assert!(tools_line.contains(name), "{tools_line:?} missing {name}");
        }
        assert!(!tools_line.contains("more"));
        assert!(lines.iter().any(|l| l == HINT_LINE));
    }

    #[test]
    fn welcome_lines_80_columns_stays_usable() {
        let lines = welcome_lines("~/project", "Claude Sonnet 4.5", "anthropic", &TOOLS, 80);
        assert_fits(&lines, 80);
        // At 80 columns the header and combined model/provider line still fit.
        assert!(lines.iter().any(|l| l.starts_with("Pi Rust")));
        assert!(lines
            .iter()
            .any(|l| l.starts_with("Model:") && l.contains("Provider:")));
        assert!(lines.iter().any(|l| l.starts_with("Enter submit")));
    }

    #[test]
    fn welcome_lines_40_columns_drops_header_and_splits_model_line() {
        let lines = welcome_lines("~/project", "Claude Sonnet 4.5", "anthropic", &TOOLS, 40);
        assert_fits(&lines, 40);
        assert!(
            !lines.iter().any(|l| l.starts_with("Pi Rust")),
            "header should be dropped at 40 columns: {lines:?}"
        );
        assert!(lines
            .iter()
            .any(|l| l == "Ready to help with this project."));
        assert!(lines
            .iter()
            .any(|l| l.starts_with("Model:") && !l.contains("Provider:")));
        assert!(lines.iter().any(|l| l.starts_with("Provider:")));
        let tools_line = lines
            .iter()
            .find(|l| l.starts_with("Tools:"))
            .expect("tools line present");
        assert!(tools_line.contains("more"), "{tools_line:?}");
        assert!(lines.iter().any(|l| l.starts_with("Enter submit")));
    }

    #[test]
    fn tool_names_line_empty_says_none_enabled() {
        assert_eq!(tool_names_line(&[], 80), "Tools: (none enabled)");
    }

    #[test]
    fn tool_names_line_always_shows_at_least_one_name() {
        let line = tool_names_line(&TOOLS, 10);
        assert!(line.starts_with("Tools:"));
        assert!(visible_width(&line) <= 10);
    }

    // -------------------------------------------------------------------
    // WelcomeScreen (Component)
    // -------------------------------------------------------------------

    #[test]
    fn welcome_screen_caches_by_width() {
        let mut screen = WelcomeScreen::new("~/project", "Sonnet", "anthropic", &TOOLS);
        let first = screen.render(80);
        let second = screen.render(80);
        assert_eq!(first, second);
        let resized = screen.render(40);
        assert_ne!(first, resized);
    }

    #[test]
    fn welcome_screen_invalidate_forces_a_rebuild_next_render() {
        let mut screen = WelcomeScreen::new("~/project", "Sonnet", "anthropic", &TOOLS);
        let first = screen.render(80);
        screen.invalidate();
        let second = screen.render(80);
        assert_eq!(
            first, second,
            "content is identical, only the cache was busted"
        );
    }

    #[test]
    fn welcome_screen_from_status_reuses_provider_and_model_name() {
        let status = crate::print_mode::TuiRuntimeStatus {
            provider: "anthropic".to_string(),
            model: "claude-sonnet-4-5".to_string(),
            model_name: "Claude Sonnet 4.5".to_string(),
            context_window: 200_000,
            reasoning_supported: true,
            thinking_level: "medium".to_string(),
            context_tokens: 0,
            cost: 0.0,
            tools_enabled: true,
        };
        let mut screen = WelcomeScreen::from_status("~/project", &status, &TOOLS);
        let lines = screen.render(200);
        let plain: Vec<String> = lines.iter().map(|l| strip_ansi_for_test(l)).collect();
        assert!(plain
            .iter()
            .any(|l| l.contains("Claude Sonnet 4.5") && l.contains("anthropic")));
    }

    // -------------------------------------------------------------------
    // setup_help_lines / SetupHelpScreen
    // -------------------------------------------------------------------

    const GUIDANCE: &str = "No models available. Use /login to log into a provider via OAuth or API key. See:\n  docs/providers.md\n  docs/models.md";
    const MODELS_PATH: &str = "C:\\Users\\me\\.pirust\\agent\\models.json";

    #[test]
    fn setup_help_lines_names_the_concrete_path_and_never_a_stack_trace() {
        let lines = setup_help_lines(MODELS_PATH, GUIDANCE, SetupChoice::OpenHelp, 80);
        assert_eq!(lines[0], "Could not start a model");
        assert!(lines.iter().any(|l| l.contains(MODELS_PATH)));
        assert!(lines.iter().any(|l| l == "Add a provider in:"));
        for stack_marker in ["panic", "unwrap", "RUST_BACKTRACE", "at src/"] {
            assert!(
                !lines.iter().any(|l| l.contains(stack_marker)),
                "screen leaked a stack-trace-looking token: {stack_marker}"
            );
        }
    }

    #[test]
    fn setup_help_lines_has_a_three_step_numbered_list() {
        let lines = setup_help_lines(MODELS_PATH, GUIDANCE, SetupChoice::OpenHelp, 80);
        for n in 1..=3 {
            let marker = format!("  {n}. ");
            assert!(
                lines.iter().any(|l| l.starts_with(&marker)),
                "missing numbered step {n} in {lines:?}"
            );
        }
    }

    #[test]
    fn setup_help_lines_marks_focus_with_a_visible_glyph() {
        let open_focused = setup_help_lines(MODELS_PATH, GUIDANCE, SetupChoice::OpenHelp, 80);
        let choices = open_focused.last().unwrap();
        assert!(choices.starts_with(FOCUS_GLYPH.trim_end()));
        assert!(choices.contains("[Open setup help]"));
        assert!(choices.contains("[Quit]"));

        let quit_focused = setup_help_lines(MODELS_PATH, GUIDANCE, SetupChoice::Quit, 80);
        let choices = quit_focused.last().unwrap();
        assert!(!choices.starts_with(FOCUS_GLYPH.trim_end()));
        assert!(choices.trim_end().ends_with("[Quit]"));
    }

    #[test]
    fn setup_help_lines_fits_narrow_widths() {
        let lines = setup_help_lines(MODELS_PATH, GUIDANCE, SetupChoice::OpenHelp, 40);
        assert_fits(&lines, 40);
    }

    #[test]
    fn truncate_path_left_keeps_the_tail() {
        let truncated = truncate_path_left(MODELS_PATH, 20);
        assert!(truncated.starts_with('\u{2026}'));
        assert!(truncated.ends_with("models.json"));
        assert!(visible_width(&truncated) <= 20);
    }

    #[test]
    fn handle_key_tab_toggles_focus_and_enter_confirms() {
        let mut screen = SetupHelpScreen::new(MODELS_PATH);
        assert_eq!(screen.focus(), SetupChoice::OpenHelp);
        assert_eq!(screen.handle_key("\r"), Some(SetupChoice::OpenHelp));

        assert_eq!(screen.handle_key("\t"), None);
        assert_eq!(screen.focus(), SetupChoice::Quit);
        assert_eq!(screen.handle_key("\r"), Some(SetupChoice::Quit));

        // A second Tab flips back.
        assert_eq!(screen.handle_key("\t"), None);
        assert_eq!(screen.focus(), SetupChoice::OpenHelp);
    }

    #[test]
    fn handle_key_escape_always_quits() {
        let mut screen = SetupHelpScreen::new(MODELS_PATH);
        assert_eq!(screen.handle_key("\x1b"), Some(SetupChoice::Quit));
        // Even after moving focus to OpenHelp, Escape still quits.
        screen.handle_key("\t");
        assert_eq!(screen.focus(), SetupChoice::Quit);
        screen.handle_key("\t");
        assert_eq!(screen.focus(), SetupChoice::OpenHelp);
        assert_eq!(screen.handle_key("\x1b"), Some(SetupChoice::Quit));
    }

    #[test]
    fn setup_help_screen_render_reflects_focus_change_after_invalidate() {
        let mut screen = SetupHelpScreen::new(MODELS_PATH);
        let before = screen.render(80);
        screen.handle_key("\t"); // Tab: OpenHelp -> Quit, calls invalidate internally.
        let after = screen.render(80);
        assert_ne!(before, after);
    }

    #[test]
    fn setup_help_screen_from_process_env_resolves_a_real_path() {
        // `ConfigEnv::from_process_env` only fails when the home directory
        // cannot be determined at all; on a normal dev/CI machine this
        // succeeds and yields a screen whose path line is non-empty.
        if let Ok(screen) = SetupHelpScreen::from_process_env() {
            assert!(!screen.models_path.is_empty());
        }
    }

    /// Minimal ANSI stripper for assertions that only care about content,
    /// not color — good enough for `\x1b[...m` sequences this module emits.
    fn strip_ansi_for_test(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\u{1b}' {
                for esc_c in chars.by_ref() {
                    if esc_c == 'm' {
                        break;
                    }
                }
                continue;
            }
            out.push(c);
        }
        out
    }
}
