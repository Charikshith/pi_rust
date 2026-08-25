//! Accessibility capability-detection and policy for the interactive TUI.
//!
//! `docs/tui-design-samples.html` promises "Accessibility — Keyboard-only
//! operation, visible focus, no color-only meaning, readable contrast,
//! reduced animation option" but nothing in the crate implemented any of it.
//! This module is the single place that (a) detects what the current
//! terminal/environment can and should be asked to render, and (b) exposes
//! that decision as a cheap, process-wide, lock-free-to-read policy that the
//! rest of the TUI (spinners, box-drawing glyphs, tool-state colors) consults
//! on every render.
//!
//! Keyboard-only operation and visible focus are layout/input concerns that
//! belong to `interactive_mode.rs`'s key-handling and `pirust-tui`'s
//! rendering loop, not this module — this module supplies the three things
//! that *are* pure capability/policy: color degradation, motion reduction,
//! and glyph substitution, plus the "never rely on color alone" text-label
//! helper. Wiring `interactive_mode.rs` to call into this module is left to
//! the caller of this task, per instructions.
//!
//! ## Where a future `settings.json` override slots in
//!
//! `settings.rs` already has small `#[derive(... Serialize, Deserialize)]`
//! `#[serde(rename_all = "camelCase")]` structs of `Option<bool>` fields for
//! per-area overrides (see `TerminalSettings` at `settings.rs:433-446`,
//! `ImageSettings` at `:449-458`). A future `AccessibilitySettings` struct
//! (e.g. `reduced_motion: Option<bool>`, `ascii_only: Option<bool>`,
//! `verbose_state: Option<bool>`) would follow that exact shape, get threaded
//! through `SettingsManager` the same way, and — once resolved — call
//! [`set_active`] once at startup (and again on hot-reload) to override the
//! env-detected [`A11ySettings`]. This module deliberately does not read
//! `settings.rs` itself (out of scope for this task; `settings.rs` is 2810
//! lines and not to be edited here).
//!
//! ## Color-capability detection: reused vs. built fresh
//!
//! `pirust-tui/src/terminal_colors.rs` (149 lines) only parses OSC 11
//! background-color and `CSI ? 997 n` dark/light-scheme responses — it has no
//! `NO_COLOR`/`COLORTERM`/`TERM` logic at all, so there was nothing to reuse
//! there. `pirust-tui/src/terminal_image.rs::detect_capabilities` (used by
//! `interactive_theme.rs`'s sibling code paths) *does* read `COLORTERM` and
//! `TERM` (`terminal_image.rs:119-123`), but only to decide `true_color:
//! bool` as one field of an image-protocol-detection struct — it has no
//! `NO_COLOR`/`FORCE_COLOR` handling, no `ColorMode` taxonomy
//! (Truecolor/256/16/None), and is a private, image-focused function not
//! meant to be a general color-depth API. Rather than duplicate its private
//! internals or repurpose an image-detection function for an unrelated
//! concern, this module implements its own minimal `COLORTERM`/`TERM`
//! reading, following the same env-var conventions (`COLORTERM=truecolor|24bit`,
//! `TERM` substring matching) `terminal_image.rs` already established, and
//! then *layers* the parts that did not exist anywhere: `NO_COLOR`,
//! `FORCE_COLOR`/`CLICOLOR_FORCE`, TTY detection, and the ANSI-256/16
//! quantization needed to actually degrade a truecolor hex value.
//!
//! ## `PIRUST_*` env-var convention
//!
//! The codebase's existing `PIRUST_*` vars (`PIRUST_CODING_AGENT_DIR`,
//! `PIRUST_CODING_AGENT_SESSION_DIR`, `PIRUST_OFFLINE` — `config.rs:145,174`,
//! `binaries.rs:161,167`; `PIRUST_TELEMETRY` — `provider_attribution.rs:28`)
//! are all `PIRUST_` + `SCREAMING_SNAKE_CASE`, and boolean flags are read
//! with `isTruthyEnvFlag`-style parsing: `v == "1" || v.lower() == "true" ||
//! v.lower() == "yes"` (`provider_attribution.rs:19-24`, mirrored privately
//! in `main.rs:100`). `PIRUST_REDUCED_MOTION`, `PIRUST_ASCII`, and
//! `PIRUST_A11Y` (below) follow that exact convention; [`is_truthy`] in this
//! module duplicates the tiny parsing helper rather than importing it, since
//! both existing copies are private (`fn`, no `pub`) to their own modules.
//!
//! ## `active()` storage: a packed `AtomicU32`, not `OnceLock<A11ySettings>`
//!
//! [`A11ySettings`] is 5 meaningful bits of state (2-bit `ColorMode` + 3
//! bools). A `OnceLock<A11ySettings>` would make [`set_active`] impossible
//! to implement — `OnceLock::set` fails once initialized, and there is no
//! safe way to overwrite it, which the "future settings.json override" path
//! above needs (detect-from-env now, override-from-settings or hot-reload
//! later). Packing into one `AtomicU32` gives: (1) a single lock-free load
//! on every render-hot-path read ([`active`]), (2) a single lock-free store
//! on override ([`set_active`]), (3) no heap allocation, ever, and (4) no
//! risk of readers observing a torn/half-updated struct, since the whole
//! value updates atomically in one instruction. `Ordering::Relaxed` is
//! sufficient on both sides: the packed `u32` is a self-contained value with
//! no pointee to synchronize (unlike, say, publishing a pointer), so there is
//! nothing for an `Acquire`/`Release` pair to protect beyond what the atomic
//! read/write itself already guarantees.

use std::io::IsTerminal;
use std::sync::atomic::{AtomicU32, Ordering};

pub use pirust_tui::components::ColorFn;

// ---------------------------------------------------------------------------
// ColorMode / A11ySettings
// ---------------------------------------------------------------------------

/// How much color the active terminal/environment is allowed to receive.
/// Ordered from richest to none so `as u32` gives a natural "degrade
/// downward" ordering, though callers should match on the variant rather
/// than rely on the discriminant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorMode {
    /// 24-bit `ESC[38;2;r;g;bm` sequences (what `interactive_theme.rs::fg`
    /// always emits today, unconditionally).
    Truecolor,
    /// 8-bit `ESC[38;5;Nm` palette index (216-color cube + 24-step grayscale
    /// ramp, indices 16-255).
    Ansi256,
    /// The 8 (or aixterm-extended 16) basic `ESC[3Xm`/`ESC[9Xm` colors.
    Ansi16,
    /// No color at all: emit plain text, zero escape bytes. Required by
    /// `NO_COLOR` (see [`detect_from`]) and by screen readers / pipes that
    /// mangle escape sequences.
    None,
}

/// The accessibility policy in effect for the current process. `Copy` and 5
/// bits of real information — designed to be read on every frame without
/// allocating, locking, or even branching much.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct A11ySettings {
    /// Color depth to degrade all themed output to. See [`fg`]/[`colorize`].
    pub color: ColorMode,
    /// Suppress spinners and other continuous animation; render a static
    /// marker instead (e.g. a fixed `*` rather than a braille spinner
    /// cycling every 80ms). Screen readers re-announce changing text on
    /// every update, so a fast-changing spinner glyph is read out
    /// continuously; this also matters for `prefers-reduced-motion`-style
    /// vestibular-disorder accommodation, and for `TERM=dumb`/non-TTY
    /// outputs that cannot usefully animate at all.
    pub reduced_motion: bool,
    /// Replace box-drawing/emoji glyphs (`◇ ▸ ▾ ⚠ ✓ ✗ ♻ ⟳ ▌`) with plain
    /// ASCII equivalents. Some terminals and most screen readers either
    /// mangle these Unicode glyphs or read them out as verbose Unicode
    /// names. See [`glyph`].
    pub ascii_only: bool,
    /// Never let color be the *only* signal for a state (WCAG 2.1 SC 1.4.1
    /// "Use of Color": color must not be the sole visual means of conveying
    /// information). When set, every stateful render should prefix a short
    /// text/word marker (`[error]`, `[ok]`, `[running]`) in addition to any
    /// color. See [`state_label`]. Defaults to `true`: the cost of a few
    /// extra ASCII bytes is negligible and the accessibility benefit is
    /// unconditional, so unlike the other three fields this is not gated on
    /// terminal capability — only an explicit override can turn it off.
    pub verbose_state: bool,
}

impl Default for A11ySettings {
    /// The conservative "fully capable, fully verbose" default: truecolor,
    /// no reduced motion, no ASCII substitution, but state labels always on
    /// (see the `verbose_state` doc). This is what you get from
    /// `A11ySettings::default()` directly; process startup should call
    /// [`detect`] instead, which reads the real environment.
    fn default() -> Self {
        A11ySettings {
            color: ColorMode::Truecolor,
            reduced_motion: false,
            ascii_only: false,
            verbose_state: true,
        }
    }
}

// ---------------------------------------------------------------------------
// Detection
// ---------------------------------------------------------------------------

/// `NO_COLOR`'s own spec (<https://no-color.org>): "command-line software...
/// should not add ANSI color to output... if the `NO_COLOR` environment
/// variable is present, **regardless of its value**." We deliberately treat
/// `NO_COLOR=""` (present but empty — which `std::env::var` still returns
/// `Ok("")` for, distinct from unset) the same as `NO_COLOR=1`: presence
/// alone is the signal, not truthiness. This is tested in
/// [`tests::no_color_empty_value_still_disables_color`].
fn no_color_present(env: &dyn Fn(&str) -> Option<String>) -> bool {
    env("NO_COLOR").is_some()
}

/// `isTruthyEnvFlag`-equivalent (`provider_attribution.rs:19-24`,
/// `main.rs:100`, both private): `"1"`, case-insensitive `"true"`, or
/// case-insensitive `"yes"`.
fn is_truthy(value: &str) -> bool {
    value == "1" || value.eq_ignore_ascii_case("true") || value.eq_ignore_ascii_case("yes")
}

fn env_truthy(env: &dyn Fn(&str) -> Option<String>, key: &str) -> bool {
    env(key).is_some_and(|v| is_truthy(&v))
}

/// Base color mode from `COLORTERM`/`TERM`, ignoring `NO_COLOR`/`FORCE_COLOR`
/// (those are layered on by the caller). Mirrors the convention already
/// established in `pirust-tui/src/terminal_image.rs:119-123`
/// (`COLORTERM=truecolor|24bit` ⇒ true-color hint), extended with the
/// `256color`/`dumb` `TERM` cases this module additionally needs.
fn color_from_term(env: &dyn Fn(&str) -> Option<String>) -> ColorMode {
    let colorterm = env("COLORTERM").unwrap_or_default().to_ascii_lowercase();
    if colorterm == "truecolor" || colorterm == "24bit" {
        return ColorMode::Truecolor;
    }
    let term = env("TERM").unwrap_or_default().to_ascii_lowercase();
    if term == "dumb" {
        return ColorMode::None;
    }
    if term.contains("256color") {
        return ColorMode::Ansi256;
    }
    // A set-but-unrecognized TERM (e.g. "xterm", "screen", "vt100") or an
    // entirely absent TERM (common on Windows terminals, which support ANSI
    // via the console API rather than terminfo — see
    // `pirust-tui/src/win_console.rs`) both fall back to the conservative
    // basic-16 assumption; the TTY check in `detect_from` handles the case
    // where there is no real interactive terminal at all.
    ColorMode::Ansi16
}

/// Detects the accessibility policy from an injectable env source and TTY
/// flag, so it is unit-testable without mutating process-global environment
/// state. **Do not** call `std::env::set_var`/`remove_var` from tests to
/// exercise this: `cargo test`'s default harness runs tests on multiple
/// threads within one process, so mutating real env vars races with every
/// other test in this binary (including ones in other files) and produces
/// flaky, hard-to-reproduce failures. Always go through this injectable
/// form in tests; [`detect`] is the thin real-environment wrapper used by
/// production code.
///
/// Precedence (highest wins for the field it affects):
/// 1. `NO_COLOR` present (any value, even empty) ⇒ `ColorMode::None`, and
///    also implies `reduced_motion` + `ascii_only` (a screen-reader-hostile
///    or minimal terminal that can't do color usually can't animate or
///    render box-drawing glyphs well either — no-color.org's own examples
///    section documents pairing `NO_COLOR` with other "reduce frills" flags).
/// 2. `FORCE_COLOR`/`CLICOLOR_FORCE` (truthy) ⇒ un-does `NO_COLOR`'s color
///    downgrade specifically (color re-derives from `COLORTERM`/`TERM`,
///    defaulting to `Ansi16` if that alone would still say `None`) but does
///    *not* re-enable motion/glyphs — an operator forcing color back on
///    hasn't necessarily also asked for animation or Unicode glyphs back.
/// 3. `COLORTERM`/`TERM` (`color_from_term`) sets the color depth otherwise.
/// 4. `TERM=dumb` also implies `reduced_motion` + `ascii_only`, same
///    reasoning as `NO_COLOR` above.
/// 5. `PIRUST_REDUCED_MOTION` / `PIRUST_ASCII` explicitly set `reduced_motion`
///    / `ascii_only` (truthy = on, present-but-falsy = off), overriding
///    whatever steps 1-4 computed. `PIRUST_A11Y` is a single combined
///    override: truthy forces `reduced_motion` + `ascii_only` + `color =
///    None`; explicitly falsy (e.g. `PIRUST_A11Y=0`) forces all
///    accessibility affordances including `verbose_state` off. Individual
///    `PIRUST_REDUCED_MOTION`/`PIRUST_ASCII` are read after `PIRUST_A11Y` so
///    they can still fine-tune on top of it.
/// 6. Not a TTY (`is_tty == false`) ⇒ `ColorMode::None` + `reduced_motion`
///    (there is no human on the other end to render color or animation
///    for, and most non-TTY consumers — log files, pipes into `less -R`
///    without `-R`, CI logs — mangle raw escape codes). This is the last,
///    highest-precedence step for `color`/`reduced_motion` because a
///    non-interactive sink should never receive escapes no matter what
///    `FORCE_COLOR` says... except `FORCE_COLOR`/`CLICOLOR_FORCE` is the one
///    documented, intentional escape hatch (`CLICOLOR_FORCE`'s own
///    convention, e.g. used by `cargo`/`ripgrep`, exists specifically to
///    force color into non-TTY sinks such as `less -R` or CI log
///    collectors that *do* render ANSI), so it is honored here too.
pub fn detect_from(env: &dyn Fn(&str) -> Option<String>, is_tty: bool) -> A11ySettings {
    let no_color = no_color_present(env);
    let forced_on = env_truthy(env, "FORCE_COLOR") || env_truthy(env, "CLICOLOR_FORCE");

    let mut color = color_from_term(env);
    if no_color {
        color = ColorMode::None;
    }
    if forced_on && color == ColorMode::None {
        color = color_from_term(env);
        if color == ColorMode::None {
            color = ColorMode::Ansi16;
        }
    }

    let term_dumb = env("TERM").as_deref() == Some("dumb");
    let mut reduced_motion = no_color || term_dumb;
    let mut ascii_only = no_color || term_dumb;
    let mut verbose_state = true;

    if !is_tty {
        reduced_motion = true;
        if !forced_on {
            color = ColorMode::None;
        }
    }

    if let Some(v) = env("PIRUST_A11Y") {
        let on = is_truthy(&v);
        reduced_motion = on;
        ascii_only = on;
        verbose_state = on;
        if on {
            color = ColorMode::None;
        }
    }
    if let Some(v) = env("PIRUST_REDUCED_MOTION") {
        reduced_motion = is_truthy(&v);
    }
    if let Some(v) = env("PIRUST_ASCII") {
        ascii_only = is_truthy(&v);
    }

    A11ySettings {
        color,
        reduced_motion,
        ascii_only,
        verbose_state,
    }
}

/// Real-environment wrapper around [`detect_from`]. TTY detection reuses the
/// same `std::io::IsTerminal` standard-library trait `main.rs:53,160-161`
/// already uses (`std::io::stdout().is_terminal()`) — no new dependency.
/// Checks stdout specifically since that is the stream the TUI actually
/// renders to.
pub fn detect() -> A11ySettings {
    detect_from(
        &|key| std::env::var(key).ok(),
        std::io::stdout().is_terminal(),
    )
}

// ---------------------------------------------------------------------------
// Process-wide accessor
// ---------------------------------------------------------------------------

/// 2 bits: `ColorMode` (`00`=Truecolor `01`=Ansi256 `10`=Ansi16 `11`=None).
const COLOR_MASK: u32 = 0b11;
const REDUCED_MOTION_BIT: u32 = 1 << 2;
const ASCII_ONLY_BIT: u32 = 1 << 3;
const VERBOSE_STATE_BIT: u32 = 1 << 4;
/// Every real packed value only ever sets the low 5 bits, so `u32::MAX` is
/// safe to use as an "uninitialized" sentinel distinguishable from any real
/// [`A11ySettings`] encoding.
const UNINIT: u32 = u32::MAX;

fn pack(s: A11ySettings) -> u32 {
    let mut bits = match s.color {
        ColorMode::Truecolor => 0,
        ColorMode::Ansi256 => 1,
        ColorMode::Ansi16 => 2,
        ColorMode::None => 3,
    };
    if s.reduced_motion {
        bits |= REDUCED_MOTION_BIT;
    }
    if s.ascii_only {
        bits |= ASCII_ONLY_BIT;
    }
    if s.verbose_state {
        bits |= VERBOSE_STATE_BIT;
    }
    bits
}

fn unpack(bits: u32) -> A11ySettings {
    let color = match bits & COLOR_MASK {
        0 => ColorMode::Truecolor,
        1 => ColorMode::Ansi256,
        2 => ColorMode::Ansi16,
        _ => ColorMode::None,
    };
    A11ySettings {
        color,
        reduced_motion: bits & REDUCED_MOTION_BIT != 0,
        ascii_only: bits & ASCII_ONLY_BIT != 0,
        verbose_state: bits & VERBOSE_STATE_BIT != 0,
    }
}

static ACTIVE: AtomicU32 = AtomicU32::new(UNINIT);

/// The process-wide accessibility policy. Lazily initialized from
/// [`detect`] on first call (real env read happens at most once per
/// process; subsequent calls, and calls after [`set_active`], are a single
/// lock-free atomic load — no allocation, no syscall, safe to call every
/// frame in the render hot path). A benign race is possible if two threads
/// both observe "uninitialized" concurrently: both compute the same
/// deterministic [`detect`] result and store it, so the outcome is
/// unaffected, just (rarely) a few wasted `env::var` reads.
pub fn active() -> A11ySettings {
    let raw = ACTIVE.load(Ordering::Relaxed);
    if raw == UNINIT {
        let settings = detect();
        set_active(settings);
        settings
    } else {
        unpack(raw)
    }
}

/// Overrides the process-wide policy (e.g. once a future `settings.json`
/// `AccessibilitySettings` override resolves, or on hot-reload — see the
/// module doc comment). A single lock-free atomic store.
pub fn set_active(settings: A11ySettings) {
    ACTIVE.store(pack(settings), Ordering::Relaxed);
}

/// Run `body` with `settings` in force, restoring the previous value after.
///
/// [`set_active`] writes a process-global atomic, so two tests in different
/// modules that both need a specific [`ColorMode`] will race under cargo's
/// threaded harness — a flake, not a real failure, but indistinguishable from
/// one. This serialises every such caller on one lock and restores what was
/// there before, so callers cannot leak a mode into an unrelated test.
///
/// Not `#[cfg(test)]`: `interactive_theme` and `interactive_markdown` are
/// separate modules and need the *same* lock, which a test-only item in this
/// module cannot give them. It is cheap and harmless in a release build.
///
/// The lock is poison-tolerant — a panicking test must not wedge every later
/// one behind a poisoned mutex.
pub fn with_settings<R>(settings: A11ySettings, body: impl FnOnce() -> R) -> R {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let previous = ACTIVE.load(Ordering::Relaxed);
    set_active(settings);
    let result = body();
    ACTIVE.store(previous, Ordering::Relaxed);
    result
}

// ---------------------------------------------------------------------------
// Glyph substitution
// ---------------------------------------------------------------------------

/// Returns `ascii` in place of `fancy` when [`active`]'s `ascii_only` is
/// set. Both arguments are `&'static str` and the return is always one of
/// them unchanged — zero allocation. Callers pass the box-drawing/emoji
/// glyph and its ASCII fallback, e.g.
/// `glyph("\u{25c7}", "*")` for `◇`, `glyph("\u{26a0}", "!")` for `⚠`.
pub fn glyph(fancy: &'static str, ascii: &'static str) -> &'static str {
    if active().ascii_only {
        ascii
    } else {
        fancy
    }
}

// ---------------------------------------------------------------------------
// "Never rely on color alone" state labels (WCAG 2.1 SC 1.4.1)
// ---------------------------------------------------------------------------

/// Maps a state name to a short, static text prefix so state is legible
/// even with color entirely stripped (piped output, `NO_COLOR`, a screen
/// reader that doesn't announce SGR colors at all). Recognizes both the
/// generic vocabulary (`ok`/`error`/`warning`/`info`/`pending`) and the
/// concrete words `session_status` already renders for `TurnState`
/// (`interactive_mode.rs:1507-1515`: `ready`, `running`, `approval`,
/// `cancelling`, `cancelled`, `complete`, `error`), so callers can pass
/// either vocabulary through unchanged. Unknown input maps to a neutral
/// `[state]` marker rather than panicking or returning empty — callers
/// should always have *something* to show when `verbose_state` is on.
///
/// This function performs the mapping unconditionally; whether to prepend
/// its result is the caller's call, driven by `active().verbose_state`
/// (which defaults to `true` — see [`A11ySettings::verbose_state`]).
pub fn state_label(state: &str) -> &'static str {
    match state {
        "ok" | "success" | "complete" | "completed" | "done" => "[ok]",
        "error" | "fail" | "failed" => "[error]",
        "warn" | "warning" => "[warning]",
        "info" => "[info]",
        "running" | "in_progress" => "[running]",
        "pending" | "ready" | "idle" | "queued" => "[pending]",
        "approval" | "awaiting_approval" => "[approval]",
        "cancelling" => "[cancelling]",
        "cancelled" | "canceled" => "[cancelled]",
        _ => "[state]",
    }
}

// ---------------------------------------------------------------------------
// Color degradation
// ---------------------------------------------------------------------------

/// Duplicated from `interactive_theme.rs`'s private (non-`pub`) `hex_to_rgb`
/// rather than imported — that copy is not exported and returns a
/// `(u32,u32,u32)` tuple sized for direct `format!` interpolation, whereas
/// the quantization functions below need `u8` for integer-arithmetic
/// distance math. Same 6-hex-digit `#rrggbb` parsing convention either way.
fn hex_to_rgb(hex: &str) -> (u8, u8, u8) {
    let normalized = hex.strip_prefix('#').unwrap_or(hex);
    let byte = |slice: &str| u8::from_str_radix(slice, 16).unwrap_or(0);
    let r = normalized.get(0..2).map(byte).unwrap_or(0);
    let g = normalized.get(2..4).map(byte).unwrap_or(0);
    let b = normalized.get(4..6).map(byte).unwrap_or(0);
    (r, g, b)
}

/// The 6 xterm-256 cube coordinate values. Index `i` in a channel maps to
/// `CUBE_STEPS[i]`; the cube occupies palette indices 16-231 as
/// `16 + 36*r + 6*g + b` over `r,g,b in 0..6`. This exact stepping (and the
/// grayscale ramp in [`rgb_to_ansi256`]) is the standard xterm-256
/// quantization scheme, documented e.g. at
/// <https://jonasjacek.github.io/colors/> and implemented identically by
/// `chalk`/`ansi-styles`' `rgbToAnsi256`.
const CUBE_STEPS: [i32; 6] = [0, 95, 135, 175, 215, 255];

fn nearest_cube_index(channel: u8) -> usize {
    let v = channel as i32;
    let mut best_idx = 0usize;
    let mut best_dist = i32::MAX;
    for (idx, &step) in CUBE_STEPS.iter().enumerate() {
        let dist = (v - step).abs();
        if dist < best_dist {
            best_dist = dist;
            best_idx = idx;
        }
    }
    best_idx
}

fn squared_distance(a: (i32, i32, i32), b: (i32, i32, i32)) -> i32 {
    let dr = a.0 - b.0;
    let dg = a.1 - b.1;
    let db = a.2 - b.2;
    dr * dr + dg * dg + db * db
}

/// Quantizes a truecolor `r,g,b` to the nearest xterm-256 palette index
/// (16-231 6x6x6 cube, or 232-255 24-step grayscale ramp — whichever is
/// closer by squared Euclidean distance). Pure integer arithmetic, no
/// floats, no allocation. See [`CUBE_STEPS`] doc for the scheme's
/// provenance; tested against known reference values in
/// [`tests::ansi256_known_values`].
pub fn rgb_to_ansi256(r: u8, g: u8, b: u8) -> u8 {
    let ri = nearest_cube_index(r);
    let gi = nearest_cube_index(g);
    let bi = nearest_cube_index(b);
    let cube_rgb = (CUBE_STEPS[ri], CUBE_STEPS[gi], CUBE_STEPS[bi]);
    let cube_dist = squared_distance((r as i32, g as i32, b as i32), cube_rgb);
    let cube_index = 16 + 36 * ri as i32 + 6 * gi as i32 + bi as i32;

    // 24-step grayscale ramp: value = 8 + 10*i for i in 0..24 (covers 8..=238),
    // palette index = 232 + i.
    let avg = (r as i32 + g as i32 + b as i32) / 3;
    let gray_idx = ((avg - 8).max(0) / 10).min(23);
    let gray_val = 8 + 10 * gray_idx;
    let gray_dist = squared_distance(
        (r as i32, g as i32, b as i32),
        (gray_val, gray_val, gray_val),
    );
    let gray_index = 232 + gray_idx;

    if gray_dist < cube_dist {
        gray_index as u8
    } else {
        cube_index as u8
    }
}

/// The 16 basic ANSI colors' commonly-documented default RGB values (the
/// xterm default palette — indices 0-7 normal, 8-15 "aixterm" bright
/// extension). Used only to find the *nearest* basic color to an arbitrary
/// hex; the actual terminal renders its own configured palette for these
/// indices, which is the whole point of degrading to them.
const ANSI16: [(u8, u8, u8); 16] = [
    (0, 0, 0),
    (205, 0, 0),
    (0, 205, 0),
    (205, 205, 0),
    (0, 0, 238),
    (205, 0, 205),
    (0, 205, 205),
    (229, 229, 229),
    (127, 127, 127),
    (255, 0, 0),
    (0, 255, 0),
    (255, 255, 0),
    (92, 92, 255),
    (255, 0, 255),
    (0, 255, 255),
    (255, 255, 255),
];

/// Quantizes to the nearest of the 16 basic ANSI colors, returning its
/// palette index (0-15). Pure integer squared-distance search over a fixed
/// 16-entry table, no floats, no allocation.
pub fn rgb_to_ansi16(r: u8, g: u8, b: u8) -> u8 {
    let target = (r as i32, g as i32, b as i32);
    let mut best_idx = 0u8;
    let mut best_dist = i32::MAX;
    for (idx, &(cr, cg, cb)) in ANSI16.iter().enumerate() {
        let dist = squared_distance(target, (cr as i32, cg as i32, cb as i32));
        if dist < best_dist {
            best_dist = dist;
            best_idx = idx as u8;
        }
    }
    best_idx
}

/// SGR foreground code for a basic-16 index: `30-37` for 0-7, `90-97`
/// (aixterm bright extension, supported by effectively every modern
/// terminal) for 8-15.
fn ansi16_fg_code(index: u8) -> u32 {
    if index < 8 {
        30 + index as u32
    } else {
        90 + (index - 8) as u32
    }
}

/// SGR background code for a basic-16 index: `40-47` / `100-107`.
fn ansi16_bg_code(index: u8) -> u32 {
    if index < 8 {
        40 + index as u32
    } else {
        100 + (index - 8) as u32
    }
}

/// Builds a [`ColorFn`] (identical type to `pirust_tui::components::ColorFn`
/// and `interactive_theme::fg`'s return type) that wraps text in a
/// foreground color sequence degraded to [`active`]'s current
/// [`ColorMode`]: `Truecolor` emits the same `38;2;r;g;b` sequence
/// `interactive_theme.rs::fg` always emits; `Ansi256`/`Ansi16` emit the
/// quantized equivalents; `None` returns the *identity* closure — no escape
/// bytes are allocated or emitted at all, not even an empty pair, so
/// `NO_COLOR`/non-TTY output is byte-for-byte plain text.
pub fn fg(hex: &str) -> ColorFn {
    let (r, g, b) = hex_to_rgb(hex);
    match active().color {
        ColorMode::Truecolor => {
            Box::new(move |text: &str| format!("\x1b[38;2;{r};{g};{b}m{text}\x1b[0m"))
        }
        ColorMode::Ansi256 => {
            let idx = rgb_to_ansi256(r, g, b);
            Box::new(move |text: &str| format!("\x1b[38;5;{idx}m{text}\x1b[0m"))
        }
        ColorMode::Ansi16 => {
            let code = ansi16_fg_code(rgb_to_ansi16(r, g, b));
            Box::new(move |text: &str| format!("\x1b[{code}m{text}\x1b[0m"))
        }
        ColorMode::None => Box::new(|text: &str| text.to_string()),
    }
}

/// Background-color counterpart to [`fg`]; see its doc for the degradation
/// rules.
pub fn bg(hex: &str) -> ColorFn {
    let (r, g, b) = hex_to_rgb(hex);
    match active().color {
        ColorMode::Truecolor => {
            Box::new(move |text: &str| format!("\x1b[48;2;{r};{g};{b}m{text}\x1b[0m"))
        }
        ColorMode::Ansi256 => {
            let idx = rgb_to_ansi256(r, g, b);
            Box::new(move |text: &str| format!("\x1b[48;5;{idx}m{text}\x1b[0m"))
        }
        ColorMode::Ansi16 => {
            let code = ansi16_bg_code(rgb_to_ansi16(r, g, b));
            Box::new(move |text: &str| format!("\x1b[{code}m{text}\x1b[0m"))
        }
        ColorMode::None => Box::new(|text: &str| text.to_string()),
    }
}

/// Convenience one-shot form of [`fg`] for call sites that don't need to
/// reuse the closure across multiple strings.
pub fn colorize(hex: &str, text: &str) -> String {
    fg(hex)(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_map(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |key| {
            pairs
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| v.to_string())
        }
    }

    fn no_env(_key: &str) -> Option<String> {
        None
    }

    /// `active()`/`set_active()` share one process-wide `static ACTIVE`
    /// (by design — see the module doc comment on why it's not per-thread).
    /// `cargo test`'s default harness runs tests in parallel threads *within
    /// one process*, so any two tests that both call `set_active` race on
    /// that shared static exactly like the module doc warns real env
    /// mutation would race — observed directly: this suite was flaky before
    /// this lock was added (`fg_ansi256_emits_38_5_sequence` intermittently
    /// read a state left behind by a concurrently-running `set_active` in
    /// another test). Every test below that calls `set_active`/`active`
    /// takes this lock first to serialize just those tests against each
    /// other; the pure `detect_from` tests need no lock and still run fully
    /// in parallel.
    fn lock_active() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    // -- detect_from precedence --------------------------------------------

    #[test]
    fn tty_no_env_defaults_to_ansi16_full_capability() {
        let s = detect_from(&no_env, true);
        assert_eq!(s.color, ColorMode::Ansi16);
        assert!(!s.reduced_motion);
        assert!(!s.ascii_only);
        assert!(s.verbose_state);
    }

    #[test]
    fn colorterm_truecolor_wins() {
        let env = env_map(&[("COLORTERM", "truecolor")]);
        let s = detect_from(&env, true);
        assert_eq!(s.color, ColorMode::Truecolor);
    }

    #[test]
    fn colorterm_24bit_also_truecolor() {
        let env = env_map(&[("COLORTERM", "24bit")]);
        assert_eq!(detect_from(&env, true).color, ColorMode::Truecolor);
    }

    #[test]
    fn term_256color_substring_match() {
        let env = env_map(&[("TERM", "xterm-256color")]);
        assert_eq!(detect_from(&env, true).color, ColorMode::Ansi256);
    }

    #[test]
    fn term_dumb_is_none_and_implies_reduced_motion_and_ascii() {
        let env = env_map(&[("TERM", "dumb")]);
        let s = detect_from(&env, true);
        assert_eq!(s.color, ColorMode::None);
        assert!(s.reduced_motion);
        assert!(s.ascii_only);
    }

    /// no-color.org: "regardless of its value" — an empty `NO_COLOR=""` must
    /// still disable color, same as `NO_COLOR=1`. `env_map` returning
    /// `Some("")` here (not `None`) is exactly what `std::env::var` would
    /// return for a set-but-empty var, distinguishing "present" from "unset".
    #[test]
    fn no_color_empty_value_still_disables_color() {
        let env = env_map(&[("NO_COLOR", "")]);
        let s = detect_from(&env, true);
        assert_eq!(s.color, ColorMode::None);
        assert!(s.reduced_motion);
        assert!(s.ascii_only);
    }

    #[test]
    fn no_color_with_truthy_value_also_disables_color() {
        let env = env_map(&[("NO_COLOR", "1")]);
        assert_eq!(detect_from(&env, true).color, ColorMode::None);
    }

    #[test]
    fn force_color_overrides_no_color() {
        let env = env_map(&[("NO_COLOR", "1"), ("FORCE_COLOR", "1")]);
        let s = detect_from(&env, true);
        assert_eq!(s.color, ColorMode::Ansi16);
        // Motion/glyph reduction from NO_COLOR is not undone by FORCE_COLOR.
        assert!(s.reduced_motion);
        assert!(s.ascii_only);
    }

    #[test]
    fn clicolor_force_behaves_like_force_color() {
        let env = env_map(&[("NO_COLOR", "1"), ("CLICOLOR_FORCE", "yes")]);
        assert_eq!(detect_from(&env, true).color, ColorMode::Ansi16);
    }

    #[test]
    fn non_tty_is_none_and_reduced_motion() {
        let s = detect_from(&no_env, false);
        assert_eq!(s.color, ColorMode::None);
        assert!(s.reduced_motion);
    }

    #[test]
    fn force_color_rescues_non_tty() {
        let env = env_map(&[("FORCE_COLOR", "1"), ("COLORTERM", "truecolor")]);
        let s = detect_from(&env, false);
        assert_eq!(s.color, ColorMode::Truecolor);
        // reduced_motion still forced by non-TTY.
        assert!(s.reduced_motion);
    }

    #[test]
    fn pirust_reduced_motion_overrides() {
        let env = env_map(&[("PIRUST_REDUCED_MOTION", "true")]);
        assert!(detect_from(&env, true).reduced_motion);
    }

    #[test]
    fn pirust_ascii_overrides() {
        let env = env_map(&[("PIRUST_ASCII", "yes")]);
        assert!(detect_from(&env, true).ascii_only);
    }

    #[test]
    fn pirust_a11y_true_forces_everything_on() {
        let env = env_map(&[("PIRUST_A11Y", "1")]);
        let s = detect_from(&env, true);
        assert_eq!(s.color, ColorMode::None);
        assert!(s.reduced_motion);
        assert!(s.ascii_only);
        assert!(s.verbose_state);
    }

    #[test]
    fn pirust_a11y_false_forces_verbose_state_off_too() {
        let env = env_map(&[("PIRUST_A11Y", "0")]);
        let s = detect_from(&env, true);
        assert!(!s.reduced_motion);
        assert!(!s.ascii_only);
        assert!(!s.verbose_state);
    }

    #[test]
    fn pirust_reduced_motion_fine_tunes_after_pirust_a11y() {
        // PIRUST_A11Y=0 turns everything off, but PIRUST_REDUCED_MOTION=1
        // still layers on top per the documented precedence.
        let env = env_map(&[("PIRUST_A11Y", "0"), ("PIRUST_REDUCED_MOTION", "1")]);
        let s = detect_from(&env, true);
        assert!(s.reduced_motion);
        assert!(!s.ascii_only);
    }

    // -- active()/set_active() ------------------------------------------

    #[test]
    fn set_active_then_active_round_trips() {
        let _guard = lock_active();
        let custom = A11ySettings {
            color: ColorMode::Ansi256,
            reduced_motion: true,
            ascii_only: true,
            verbose_state: false,
        };
        set_active(custom);
        assert_eq!(active(), custom);

        let other = A11ySettings {
            color: ColorMode::None,
            reduced_motion: false,
            ascii_only: false,
            verbose_state: true,
        };
        set_active(other);
        assert_eq!(active(), other);
    }

    #[test]
    fn pack_unpack_round_trips_all_color_modes() {
        for color in [
            ColorMode::Truecolor,
            ColorMode::Ansi256,
            ColorMode::Ansi16,
            ColorMode::None,
        ] {
            for reduced_motion in [true, false] {
                for ascii_only in [true, false] {
                    for verbose_state in [true, false] {
                        let s = A11ySettings {
                            color,
                            reduced_motion,
                            ascii_only,
                            verbose_state,
                        };
                        assert_eq!(unpack(pack(s)), s);
                    }
                }
            }
        }
    }

    // -- glyph / state_label ------------------------------------------------

    #[test]
    fn glyph_picks_ascii_when_ascii_only() {
        let _guard = lock_active();
        set_active(A11ySettings {
            color: ColorMode::Truecolor,
            reduced_motion: false,
            ascii_only: true,
            verbose_state: true,
        });
        assert_eq!(glyph("\u{25c7}", "*"), "*");
    }

    #[test]
    fn glyph_picks_fancy_when_not_ascii_only() {
        let _guard = lock_active();
        set_active(A11ySettings {
            color: ColorMode::Truecolor,
            reduced_motion: false,
            ascii_only: false,
            verbose_state: true,
        });
        assert_eq!(glyph("\u{25c7}", "*"), "\u{25c7}");
    }

    #[test]
    fn state_label_covers_turn_state_words() {
        assert_eq!(state_label("running"), "[running]");
        assert_eq!(state_label("error"), "[error]");
        assert_eq!(state_label("complete"), "[ok]");
        assert_eq!(state_label("approval"), "[approval]");
        assert_eq!(state_label("cancelling"), "[cancelling]");
        assert_eq!(state_label("cancelled"), "[cancelled]");
        assert_eq!(state_label("ready"), "[pending]");
        assert_eq!(state_label("nonsense"), "[state]");
    }

    // -- color quantization --------------------------------------------------

    #[test]
    fn ansi256_known_values() {
        // Pure black and pure white land on the cube corners 16 and 231
        // (jonasjacek.github.io/colors/ #16 Grey0, #231 Grey100).
        assert_eq!(rgb_to_ansi256(0, 0, 0), 16);
        assert_eq!(rgb_to_ansi256(255, 255, 255), 231);
        // #808080 (128,128,128) exactly matches the grayscale ramp at index
        // 244 (Grey50), which beats the nearest cube corner.
        assert_eq!(rgb_to_ansi256(128, 128, 128), 244);
    }

    #[test]
    fn ansi256_pure_red_lands_in_cube() {
        // (255,0,0): r maps to cube step 255 (idx 5), g/b map to step 0
        // (idx 0) => 16 + 36*5 + 6*0 + 0 = 196.
        assert_eq!(rgb_to_ansi256(255, 0, 0), 196);
    }

    #[test]
    fn ansi16_pure_colors_map_to_expected_index() {
        assert_eq!(rgb_to_ansi16(0, 0, 0), 0);
        assert_eq!(rgb_to_ansi16(255, 255, 255), 15);
        assert_eq!(rgb_to_ansi16(255, 0, 0), 9); // bright red is closer than dark red
    }

    #[test]
    fn ansi16_fg_and_bg_codes_use_aixterm_bright_range() {
        assert_eq!(ansi16_fg_code(0), 30);
        assert_eq!(ansi16_fg_code(7), 37);
        assert_eq!(ansi16_fg_code(8), 90);
        assert_eq!(ansi16_fg_code(15), 97);
        assert_eq!(ansi16_bg_code(0), 40);
        assert_eq!(ansi16_bg_code(15), 107);
    }

    // -- fg()/bg()/colorize() degrade correctly ------------------------------

    #[test]
    fn fg_truecolor_matches_interactive_theme_format() {
        let _guard = lock_active();
        set_active(A11ySettings {
            color: ColorMode::Truecolor,
            reduced_motion: false,
            ascii_only: false,
            verbose_state: true,
        });
        let f = fg("#d4d4d4");
        assert_eq!(f("hi"), "\x1b[38;2;212;212;212mhi\x1b[0m");
    }

    #[test]
    fn fg_none_mode_is_identity_no_escape_bytes() {
        let _guard = lock_active();
        set_active(A11ySettings {
            color: ColorMode::None,
            reduced_motion: true,
            ascii_only: true,
            verbose_state: true,
        });
        let f = fg("#d4d4d4");
        let out = f("plain");
        assert_eq!(out, "plain");
        assert!(!out.contains('\x1b'));
    }

    #[test]
    fn fg_ansi256_emits_38_5_sequence() {
        let _guard = lock_active();
        set_active(A11ySettings {
            color: ColorMode::Ansi256,
            reduced_motion: false,
            ascii_only: false,
            verbose_state: true,
        });
        let out = fg("#808080")("x");
        assert_eq!(out, "\x1b[38;5;244mx\x1b[0m");
    }

    #[test]
    fn colorize_matches_fg() {
        let _guard = lock_active();
        set_active(A11ySettings {
            color: ColorMode::Truecolor,
            reduced_motion: false,
            ascii_only: false,
            verbose_state: true,
        });
        assert_eq!(colorize("#d4d4d4", "hi"), fg("#d4d4d4")("hi"));
    }
}
