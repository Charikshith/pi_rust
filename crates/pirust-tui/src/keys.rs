//! Port of `packages/tui/src/keys.ts` — keyboard input handling: Kitty keyboard
//! protocol, xterm `modifyOtherKeys`, and legacy escape sequences unified behind
//! `matches_key`/`parse_key`. See `docs/analysis/05-tui.md` §2/§4/§9.
//!
//! ## Scope decisions (documented, not silent — AGENTS.md Correctness Bar)
//!
//! - **`KeyId`/`Key` builder not ported.** TS's `KeyId` is an elaborate literal-type
//!   union plus a `Key` builder object that exists purely for TS-compile-time
//!   autocomplete/typo-catching (`Key.ctrl("c")` just returns the string `"ctrl+c"`
//!   at runtime — zero behavior of its own). Rust has no equivalent compile-time
//!   surface to preserve here, and the Ponytail ladder (`AGENTS.md`) says skip
//!   speculative surface. [`matches_key`]/[`parse_key`] take/return plain
//!   `&str`/`String`, matching what the TS functions actually consume/produce at
//!   runtime. A later wave (`keybindings.rs`) may add named constants for
//!   ergonomics if it needs them — that is that wave's call, not this one's.
//! - **`_lastEventType`/`parseEventType`/`KeyEventType` not ported — confirmed dead
//!   state.** The TS module parses an `eventType` (press/repeat/release) out of
//!   Kitty CSI-u sequences and stashes it in a module-global `_lastEventType`, and
//!   stores it on `ParsedKittySequence`/`ParsedModifyOtherKeysSequence`. A repo-wide
//!   grep of `../pi` for `_lastEventType` turns up zero readers outside the write
//!   site in `keys.ts` itself, and neither `matchesKittySequence`'s caller nor
//!   `formatParsedKey` ever reads the `eventType` field — `isKeyRelease`/
//!   `isKeyRepeat` use direct substring checks on `data` instead (see below). This
//!   is write-only dead state with no effect on any exported function's return
//!   value; porting it would add code with no observable behavior (Ponytail rung
//!   1: "does this need to exist at all?"). The `:<event>` suffix in Kitty
//!   sequences is still *parsed for shape* (so malformed sequences are still
//!   correctly rejected) but its value is discarded.
//! - **Kitty protocol state is real, externally-observable process state**
//!   (`ProcessTerminal` sets it after protocol negotiation; `matches_key`/
//!   `parse_key` read it on every call) — ported as a `static AtomicBool`, the
//!   direct Rust analogue of the TS module-level `let _kittyProtocolActive`,
//!   defaulting to inactive exactly like the TS module-load default.

use std::sync::atomic::{AtomicBool, Ordering};

// =============================================================================
// Global Kitty Protocol State
// =============================================================================

static KITTY_PROTOCOL_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Set the global Kitty keyboard protocol state (`setKittyProtocolActive`, keys.ts:31).
pub fn set_kitty_protocol_active(active: bool) {
    KITTY_PROTOCOL_ACTIVE.store(active, Ordering::Relaxed);
}

/// Query whether Kitty keyboard protocol is currently active (`isKittyProtocolActive`, keys.ts:38).
pub fn is_kitty_protocol_active() -> bool {
    KITTY_PROTOCOL_ACTIVE.load(Ordering::Relaxed)
}

// =============================================================================
// Constants
// =============================================================================

fn is_symbol_key_char(c: char) -> bool {
    matches!(
        c,
        '`' | '-'
            | '='
            | '['
            | ']'
            | '\\'
            | ';'
            | '\''
            | ','
            | '.'
            | '/'
            | '!'
            | '@'
            | '#'
            | '$'
            | '%'
            | '^'
            | '&'
            | '*'
            | '('
            | ')'
            | '_'
            | '+'
            | '|'
            | '~'
            | '{'
            | '}'
            | ':'
            | '<'
            | '>'
            | '?'
    )
}

const MOD_SHIFT: i64 = 1;
const MOD_ALT: i64 = 2;
const MOD_CTRL: i64 = 4;
const MOD_SUPER: i64 = 8;
const LOCK_MASK: i64 = 64 + 128; // Caps Lock + Num Lock

const CP_ESCAPE: i64 = 27;
const CP_TAB: i64 = 9;
const CP_ENTER: i64 = 13;
const CP_SPACE: i64 = 32;
const CP_BACKSPACE: i64 = 127;
const CP_KP_ENTER: i64 = 57414; // Numpad Enter (Kitty protocol)

const ARROW_UP: i64 = -1;
const ARROW_DOWN: i64 = -2;
const ARROW_RIGHT: i64 = -3;
const ARROW_LEFT: i64 = -4;

const FN_DELETE: i64 = -10;
const FN_INSERT: i64 = -11;
const FN_PAGE_UP: i64 = -12;
const FN_PAGE_DOWN: i64 = -13;
const FN_HOME: i64 = -14;
const FN_END: i64 = -15;

fn normalize_kitty_functional_codepoint(codepoint: i64) -> i64 {
    match codepoint {
        57399 => 48,
        57400 => 49,
        57401 => 50,
        57402 => 51,
        57403 => 52,
        57404 => 53,
        57405 => 54,
        57406 => 55,
        57407 => 56,
        57408 => 57,
        57409 => 46,
        57410 => 47,
        57411 => 42,
        57412 => 45,
        57413 => 43,
        57415 => 61,
        57416 => 44,
        57417 => ARROW_LEFT,
        57418 => ARROW_RIGHT,
        57419 => ARROW_UP,
        57420 => ARROW_DOWN,
        57421 => FN_PAGE_UP,
        57422 => FN_PAGE_DOWN,
        57423 => FN_HOME,
        57424 => FN_END,
        57425 => FN_INSERT,
        57426 => FN_DELETE,
        other => other,
    }
}

fn normalize_shifted_letter_identity_codepoint(codepoint: i64, modifier: i64) -> i64 {
    let effective_modifier = modifier & !LOCK_MASK;
    if (effective_modifier & MOD_SHIFT) != 0 && (65..=90).contains(&codepoint) {
        codepoint + 32
    } else {
        codepoint
    }
}

fn char_from_cp(cp: i64) -> Option<char> {
    if cp < 0 {
        return None;
    }
    char::from_u32(cp as u32)
}

fn legacy_key_sequences(key: &str) -> Option<&'static [&'static str]> {
    Some(match key {
        "up" => &["\x1b[A", "\x1bOA"],
        "down" => &["\x1b[B", "\x1bOB"],
        "right" => &["\x1b[C", "\x1bOC"],
        "left" => &["\x1b[D", "\x1bOD"],
        "home" => &["\x1b[H", "\x1bOH", "\x1b[1~", "\x1b[7~"],
        "end" => &["\x1b[F", "\x1bOF", "\x1b[4~", "\x1b[8~"],
        "insert" => &["\x1b[2~"],
        "delete" => &["\x1b[3~"],
        "pageUp" => &["\x1b[5~", "\x1b[[5~"],
        "pageDown" => &["\x1b[6~", "\x1b[[6~"],
        "clear" => &["\x1b[E", "\x1bOE"],
        "f1" => &["\x1bOP", "\x1b[11~", "\x1b[[A"],
        "f2" => &["\x1bOQ", "\x1b[12~", "\x1b[[B"],
        "f3" => &["\x1bOR", "\x1b[13~", "\x1b[[C"],
        "f4" => &["\x1bOS", "\x1b[14~", "\x1b[[D"],
        "f5" => &["\x1b[15~", "\x1b[[E"],
        "f6" => &["\x1b[17~"],
        "f7" => &["\x1b[18~"],
        "f8" => &["\x1b[19~"],
        "f9" => &["\x1b[20~"],
        "f10" => &["\x1b[21~"],
        "f11" => &["\x1b[23~"],
        "f12" => &["\x1b[24~"],
        _ => return None,
    })
}

fn legacy_shift_sequences(key: &str) -> Option<&'static [&'static str]> {
    Some(match key {
        "up" => &["\x1b[a"],
        "down" => &["\x1b[b"],
        "right" => &["\x1b[c"],
        "left" => &["\x1b[d"],
        "clear" => &["\x1b[e"],
        "insert" => &["\x1b[2$"],
        "delete" => &["\x1b[3$"],
        "pageUp" => &["\x1b[5$"],
        "pageDown" => &["\x1b[6$"],
        "home" => &["\x1b[7$"],
        "end" => &["\x1b[8$"],
        _ => return None,
    })
}

fn legacy_ctrl_sequences(key: &str) -> Option<&'static [&'static str]> {
    Some(match key {
        "up" => &["\x1bOa"],
        "down" => &["\x1bOb"],
        "right" => &["\x1bOc"],
        "left" => &["\x1bOd"],
        "clear" => &["\x1bOe"],
        "insert" => &["\x1b[2^"],
        "delete" => &["\x1b[3^"],
        "pageUp" => &["\x1b[5^"],
        "pageDown" => &["\x1b[6^"],
        "home" => &["\x1b[7^"],
        "end" => &["\x1b[8^"],
        _ => return None,
    })
}

fn legacy_sequence_key_id(data: &str) -> Option<&'static str> {
    Some(match data {
        "\x1bOA" => "up",
        "\x1bOB" => "down",
        "\x1bOC" => "right",
        "\x1bOD" => "left",
        "\x1bOH" => "home",
        "\x1bOF" => "end",
        "\x1b[E" => "clear",
        "\x1bOE" => "clear",
        "\x1bOe" => "ctrl+clear",
        "\x1b[e" => "shift+clear",
        "\x1b[2~" => "insert",
        "\x1b[2$" => "shift+insert",
        "\x1b[2^" => "ctrl+insert",
        "\x1b[3$" => "shift+delete",
        "\x1b[3^" => "ctrl+delete",
        "\x1b[[5~" => "pageUp",
        "\x1b[[6~" => "pageDown",
        "\x1b[a" => "shift+up",
        "\x1b[b" => "shift+down",
        "\x1b[c" => "shift+right",
        "\x1b[d" => "shift+left",
        "\x1bOa" => "ctrl+up",
        "\x1bOb" => "ctrl+down",
        "\x1bOc" => "ctrl+right",
        "\x1bOd" => "ctrl+left",
        "\x1b[5$" => "shift+pageUp",
        "\x1b[6$" => "shift+pageDown",
        "\x1b[7$" => "shift+home",
        "\x1b[8$" => "shift+end",
        "\x1b[5^" => "ctrl+pageUp",
        "\x1b[6^" => "ctrl+pageDown",
        "\x1b[7^" => "ctrl+home",
        "\x1b[8^" => "ctrl+end",
        "\x1bOP" => "f1",
        "\x1bOQ" => "f2",
        "\x1bOR" => "f3",
        "\x1bOS" => "f4",
        "\x1b[11~" => "f1",
        "\x1b[12~" => "f2",
        "\x1b[13~" => "f3",
        "\x1b[14~" => "f4",
        "\x1b[[A" => "f1",
        "\x1b[[B" => "f2",
        "\x1b[[C" => "f3",
        "\x1b[[D" => "f4",
        "\x1b[[E" => "f5",
        "\x1b[15~" => "f5",
        "\x1b[17~" => "f6",
        "\x1b[18~" => "f7",
        "\x1b[19~" => "f8",
        "\x1b[20~" => "f9",
        "\x1b[21~" => "f10",
        "\x1b[23~" => "f11",
        "\x1b[24~" => "f12",
        "\x1bb" => "alt+left",
        "\x1bf" => "alt+right",
        "\x1bp" => "alt+up",
        "\x1bn" => "alt+down",
        _ => return None,
    })
}

fn matches_legacy_sequence(data: &str, sequences: &[&str]) -> bool {
    sequences.contains(&data)
}

fn matches_legacy_modifier_sequence(data: &str, key: &str, modifier: i64) -> bool {
    if modifier == MOD_SHIFT {
        legacy_shift_sequences(key).is_some_and(|s| matches_legacy_sequence(data, s))
    } else if modifier == MOD_CTRL {
        legacy_ctrl_sequences(key).is_some_and(|s| matches_legacy_sequence(data, s))
    } else {
        false
    }
}

// =============================================================================
// Kitty / modifyOtherKeys sequence parsing
// =============================================================================

/// `shiftedKey` is deliberately not carried here: `matchesKittySequence` and
/// `formatParsedKey` (the only two consumers of a parsed Kitty sequence) never
/// read it in the TS source either — `decodeKittyPrintable` re-parses via
/// `parse_csi_u` directly instead, where the shifted key IS consumed.
struct ParsedKittySequence {
    codepoint: i64,
    base_layout_key: Option<i64>,
    modifier: i64,
}

struct ParsedModifyOtherKeysSequence {
    codepoint: i64,
    modifier: i64,
}

fn parse_digits(s: &str) -> Option<i64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// Parses `\x1b[<codepoint>[:<shifted>[:<base>]][;<mod>[:<event>]]u` — the Kitty
/// CSI-u format (`parseKittySequence`'s csiUMatch branch, keys.ts:598, reused
/// verbatim by `KITTY_CSI_U_REGEX`, keys.ts:1333, since both patterns are
/// textually identical in the TS source). Returns
/// `(codepoint, shifted_key, base_layout_key, mod_value)` where `mod_value` is
/// still 1-indexed (caller subtracts 1). The trailing `:<event>` is validated for
/// shape only; its value is discarded (dead state — see module docs).
fn parse_csi_u(data: &str) -> Option<(i64, Option<i64>, Option<i64>, i64)> {
    let inner = data.strip_prefix("\x1b[")?.strip_suffix('u')?;
    let (key_part, mod_part) = match inner.find(';') {
        Some(idx) => (&inner[..idx], Some(&inner[idx + 1..])),
        None => (inner, None),
    };

    let mut key_segs = key_part.split(':');
    let codepoint = parse_digits(key_segs.next()?)?;
    let shifted_key = match key_segs.next() {
        None | Some("") => None,
        Some(s) => Some(parse_digits(s)?),
    };
    let base_layout_key = match key_segs.next() {
        None => None,
        Some(s) => Some(parse_digits(s)?),
    };
    if key_segs.next().is_some() {
        return None;
    }

    let mod_value = match mod_part {
        None => 1,
        Some(mp) => {
            let mut mod_segs = mp.split(':');
            let m = parse_digits(mod_segs.next()?)?;
            if let Some(evt) = mod_segs.next() {
                parse_digits(evt)?;
            }
            if mod_segs.next().is_some() {
                return None;
            }
            m
        }
    };

    Some((codepoint, shifted_key, base_layout_key, mod_value))
}

/// Arrow keys with modifier: `\x1b[1;<mod>[:<event>][ABCD]` (keys.ts:610).
fn parse_arrow_with_modifier(data: &str) -> Option<(i64, i64)> {
    let rest = data.strip_prefix("\x1b[1;")?;
    let last = rest.chars().last()?;
    let arrow_cp = match last {
        'A' => ARROW_UP,
        'B' => ARROW_DOWN,
        'C' => ARROW_RIGHT,
        'D' => ARROW_LEFT,
        _ => return None,
    };
    let body = &rest[..rest.len() - 1];
    let mut segs = body.split(':');
    let mod_value = parse_digits(segs.next()?)?;
    if let Some(evt) = segs.next() {
        parse_digits(evt)?;
    }
    if segs.next().is_some() {
        return None;
    }
    Some((arrow_cp, mod_value))
}

/// Functional keys: `\x1b[<num>[;<mod>][:<event>]~` (keys.ts:620). Note the
/// `:<event>` group is NOT nested inside the `;<mod>` group in the TS regex — it
/// is a separate optional group that always comes after it, so `<num>:<event>~`
/// (no `;<mod>` at all) is also valid, with mod defaulting to 1.
fn parse_functional_key(data: &str) -> Option<(i64, i64)> {
    let rest = data.strip_prefix("\x1b[")?.strip_suffix('~')?;
    let (before_colon, event_part) = match rest.find(':') {
        Some(idx) => (&rest[..idx], Some(&rest[idx + 1..])),
        None => (rest, None),
    };
    let (num_str, mod_str) = match before_colon.find(';') {
        Some(idx) => (&before_colon[..idx], Some(&before_colon[idx + 1..])),
        None => (before_colon, None),
    };
    let key_num = parse_digits(num_str)?;
    let mod_value = match mod_str {
        Some(m) => parse_digits(m)?,
        None => 1,
    };
    if let Some(evt) = event_part {
        parse_digits(evt)?;
    }
    let codepoint = match key_num {
        2 => FN_INSERT,
        3 => FN_DELETE,
        5 => FN_PAGE_UP,
        6 => FN_PAGE_DOWN,
        7 => FN_HOME,
        8 => FN_END,
        _ => return None,
    };
    Some((codepoint, mod_value))
}

/// Home/End with modifier: `\x1b[1;<mod>[:<event>][HF]` (keys.ts:641).
fn parse_home_end_with_modifier(data: &str) -> Option<(i64, i64)> {
    let rest = data.strip_prefix("\x1b[1;")?;
    let last = rest.chars().last()?;
    let cp = match last {
        'H' => FN_HOME,
        'F' => FN_END,
        _ => return None,
    };
    let body = &rest[..rest.len() - 1];
    let mut segs = body.split(':');
    let mod_value = parse_digits(segs.next()?)?;
    if let Some(evt) = segs.next() {
        parse_digits(evt)?;
    }
    if segs.next().is_some() {
        return None;
    }
    Some((cp, mod_value))
}

fn parse_kitty_sequence(data: &str) -> Option<ParsedKittySequence> {
    if let Some((codepoint, _shifted_key, base_layout_key, mod_value)) = parse_csi_u(data) {
        return Some(ParsedKittySequence {
            codepoint,
            base_layout_key,
            modifier: mod_value - 1,
        });
    }
    if let Some((codepoint, mod_value)) = parse_arrow_with_modifier(data) {
        return Some(ParsedKittySequence {
            codepoint,
            base_layout_key: None,
            modifier: mod_value - 1,
        });
    }
    if let Some((codepoint, mod_value)) = parse_functional_key(data) {
        return Some(ParsedKittySequence {
            codepoint,
            base_layout_key: None,
            modifier: mod_value - 1,
        });
    }
    if let Some((codepoint, mod_value)) = parse_home_end_with_modifier(data) {
        return Some(ParsedKittySequence {
            codepoint,
            base_layout_key: None,
            modifier: mod_value - 1,
        });
    }
    None
}

fn matches_kitty_sequence(data: &str, expected_codepoint: i64, expected_modifier: i64) -> bool {
    let Some(parsed) = parse_kitty_sequence(data) else {
        return false;
    };
    let actual_mod = parsed.modifier & !LOCK_MASK;
    let expected_mod = expected_modifier & !LOCK_MASK;
    if actual_mod != expected_mod {
        return false;
    }

    let normalized_codepoint = normalize_shifted_letter_identity_codepoint(
        normalize_kitty_functional_codepoint(parsed.codepoint),
        parsed.modifier,
    );
    let normalized_expected_codepoint = normalize_shifted_letter_identity_codepoint(
        normalize_kitty_functional_codepoint(expected_codepoint),
        expected_modifier,
    );

    if normalized_codepoint == normalized_expected_codepoint {
        return true;
    }

    if let Some(base) = parsed.base_layout_key {
        if base == expected_codepoint {
            let is_latin_letter = (97..=122).contains(&normalized_codepoint);
            let is_known_symbol =
                char_from_cp(normalized_codepoint).is_some_and(is_symbol_key_char);
            if !is_latin_letter && !is_known_symbol {
                return true;
            }
        }
    }

    false
}

/// `\x1b[27;<mod>;<codepoint>~` — xterm `modifyOtherKeys` (keys.ts:697).
fn parse_modify_other_keys_sequence(data: &str) -> Option<ParsedModifyOtherKeysSequence> {
    let rest = data.strip_prefix("\x1b[27;")?.strip_suffix('~')?;
    let idx = rest.find(';')?;
    let mod_value = parse_digits(&rest[..idx])?;
    let codepoint = parse_digits(&rest[idx + 1..])?;
    Some(ParsedModifyOtherKeysSequence {
        codepoint,
        modifier: mod_value - 1,
    })
}

fn matches_modify_other_keys(data: &str, expected_keycode: i64, expected_modifier: i64) -> bool {
    match parse_modify_other_keys_sequence(data) {
        Some(p) => p.codepoint == expected_keycode && p.modifier == expected_modifier,
        None => false,
    }
}

fn env_truthy(name: &str) -> bool {
    std::env::var(name).map(|v| !v.is_empty()).unwrap_or(false)
}

fn is_windows_terminal_session() -> bool {
    env_truthy("WT_SESSION")
        && !env_truthy("SSH_CONNECTION")
        && !env_truthy("SSH_CLIENT")
        && !env_truthy("SSH_TTY")
}

/// Raw 0x08 (BS) is ambiguous in legacy terminals — see keys.ts:721-734's own doc
/// comment for the Windows Terminal Ctrl+Backspace heuristic.
fn matches_raw_backspace(data: &str, expected_modifier: i64) -> bool {
    if data == "\x7f" {
        return expected_modifier == 0;
    }
    if data != "\x08" {
        return false;
    }
    if is_windows_terminal_session() {
        expected_modifier == MOD_CTRL
    } else {
        expected_modifier == 0
    }
}

// =============================================================================
// Generic Key Matching
// =============================================================================

fn raw_ctrl_char(key: char) -> Option<char> {
    let c = key.to_ascii_lowercase();
    let code = c as u32;
    if (97..=122).contains(&code) || matches!(c, '[' | '\\' | ']' | '_') {
        return char::from_u32(code & 0x1f);
    }
    if c == '-' {
        return char::from_u32(31);
    }
    None
}

fn matches_printable_modify_other_keys(
    data: &str,
    expected_keycode: i64,
    expected_modifier: i64,
) -> bool {
    if expected_modifier == 0 {
        return false;
    }
    let Some(parsed) = parse_modify_other_keys_sequence(data) else {
        return false;
    };
    if parsed.modifier != expected_modifier {
        return false;
    }
    normalize_shifted_letter_identity_codepoint(parsed.codepoint, parsed.modifier)
        == normalize_shifted_letter_identity_codepoint(expected_keycode, expected_modifier)
}

fn format_key_name_with_modifiers(key_name: &str, modifier: i64) -> Option<String> {
    let effective_mod = modifier & !LOCK_MASK;
    let supported_modifier_mask = MOD_SHIFT | MOD_CTRL | MOD_ALT | MOD_SUPER;
    if (effective_mod & !supported_modifier_mask) != 0 {
        return None;
    }
    let mut mods: Vec<&str> = Vec::new();
    if effective_mod & MOD_SHIFT != 0 {
        mods.push("shift");
    }
    if effective_mod & MOD_CTRL != 0 {
        mods.push("ctrl");
    }
    if effective_mod & MOD_ALT != 0 {
        mods.push("alt");
    }
    if effective_mod & MOD_SUPER != 0 {
        mods.push("super");
    }
    Some(if mods.is_empty() {
        key_name.to_string()
    } else {
        format!("{}+{key_name}", mods.join("+"))
    })
}

struct ParsedKeyId {
    key: String,
    ctrl: bool,
    shift: bool,
    alt: bool,
    super_mod: bool,
}

fn parse_key_id(key_id: &str) -> Option<ParsedKeyId> {
    let lower = key_id.to_lowercase();
    let parts: Vec<&str> = lower.split('+').collect();
    let key = (*parts.last()?).to_string();
    if key.is_empty() {
        return None;
    }
    Some(ParsedKeyId {
        key,
        ctrl: parts.contains(&"ctrl"),
        shift: parts.contains(&"shift"),
        alt: parts.contains(&"alt"),
        super_mod: parts.contains(&"super"),
    })
}

fn matches_single_char_key(data: &str, key: &str, modifier: i64) -> bool {
    let mut chars = key.chars();
    let Some(kc) = chars.next() else {
        return false;
    };
    if chars.next().is_some() {
        return false;
    }
    if !(kc.is_ascii_lowercase() || kc.is_ascii_digit() || is_symbol_key_char(kc)) {
        return false;
    }

    let codepoint = kc as i64;
    let raw_ctrl = raw_ctrl_char(kc);
    let is_letter = kc.is_ascii_lowercase();
    let is_digit = kc.is_ascii_digit();
    let kitty_active = is_kitty_protocol_active();

    if modifier == MOD_CTRL + MOD_ALT && !kitty_active {
        if let Some(rc) = raw_ctrl {
            if data == format!("\x1b{rc}") {
                return true;
            }
        }
    }

    if modifier == MOD_ALT
        && !kitty_active
        && (is_letter || is_digit || is_symbol_key_char(kc))
        && data == format!("\x1b{kc}")
    {
        return true;
    }

    if modifier == MOD_CTRL {
        if let Some(rc) = raw_ctrl {
            if data == rc.to_string() {
                return true;
            }
        }
        return matches_kitty_sequence(data, codepoint, MOD_CTRL)
            || matches_printable_modify_other_keys(data, codepoint, MOD_CTRL);
    }

    if modifier == MOD_SHIFT + MOD_CTRL {
        return matches_kitty_sequence(data, codepoint, MOD_SHIFT + MOD_CTRL)
            || matches_printable_modify_other_keys(data, codepoint, MOD_SHIFT + MOD_CTRL);
    }

    if modifier == MOD_SHIFT {
        if is_letter && data == kc.to_uppercase().to_string() {
            return true;
        }
        return matches_kitty_sequence(data, codepoint, MOD_SHIFT)
            || matches_printable_modify_other_keys(data, codepoint, MOD_SHIFT);
    }

    if modifier != 0 {
        return matches_kitty_sequence(data, codepoint, modifier)
            || matches_printable_modify_other_keys(data, codepoint, modifier);
    }

    data == key || matches_kitty_sequence(data, codepoint, 0)
}

/// Match input data against a key identifier string (`matchesKey`, keys.ts:820).
///
/// Supported key identifiers mirror the TS source: single keys (`"escape"`,
/// `"tab"`, `"enter"`, `"backspace"`, `"delete"`, `"home"`, `"end"`, `"space"`),
/// arrows (`"up"`/`"down"`/`"left"`/`"right"`), and modifier combinations
/// (`"ctrl+c"`, `"shift+tab"`, `"alt+enter"`, `"super+k"`, `"shift+ctrl+p"`, etc).
pub fn matches_key(data: &str, key_id: &str) -> bool {
    let Some(parsed) = parse_key_id(key_id) else {
        return false;
    };
    let ParsedKeyId {
        key,
        ctrl,
        shift,
        alt,
        super_mod,
    } = parsed;
    let mut modifier = 0i64;
    if shift {
        modifier |= MOD_SHIFT;
    }
    if alt {
        modifier |= MOD_ALT;
    }
    if ctrl {
        modifier |= MOD_CTRL;
    }
    if super_mod {
        modifier |= MOD_SUPER;
    }
    let kitty_active = is_kitty_protocol_active();

    match key.as_str() {
        "escape" | "esc" => {
            if modifier != 0 {
                return false;
            }
            data == "\x1b"
                || matches_kitty_sequence(data, CP_ESCAPE, 0)
                || matches_modify_other_keys(data, CP_ESCAPE, 0)
        }
        "space" => {
            if !kitty_active {
                if modifier == MOD_CTRL && data == "\x00" {
                    return true;
                }
                if modifier == MOD_ALT && data == "\x1b " {
                    return true;
                }
            }
            if modifier == 0 {
                return data == " "
                    || matches_kitty_sequence(data, CP_SPACE, 0)
                    || matches_modify_other_keys(data, CP_SPACE, 0);
            }
            matches_kitty_sequence(data, CP_SPACE, modifier)
                || matches_modify_other_keys(data, CP_SPACE, modifier)
        }
        "tab" => {
            if modifier == MOD_SHIFT {
                return data == "\x1b[Z"
                    || matches_kitty_sequence(data, CP_TAB, MOD_SHIFT)
                    || matches_modify_other_keys(data, CP_TAB, MOD_SHIFT);
            }
            if modifier == 0 {
                return data == "\t" || matches_kitty_sequence(data, CP_TAB, 0);
            }
            matches_kitty_sequence(data, CP_TAB, modifier)
                || matches_modify_other_keys(data, CP_TAB, modifier)
        }
        "enter" | "return" => {
            if modifier == MOD_SHIFT {
                if matches_kitty_sequence(data, CP_ENTER, MOD_SHIFT)
                    || matches_kitty_sequence(data, CP_KP_ENTER, MOD_SHIFT)
                {
                    return true;
                }
                if matches_modify_other_keys(data, CP_ENTER, MOD_SHIFT) {
                    return true;
                }
                if kitty_active {
                    return data == "\x1b\r" || data == "\n";
                }
                return false;
            }
            if modifier == MOD_ALT {
                if matches_kitty_sequence(data, CP_ENTER, MOD_ALT)
                    || matches_kitty_sequence(data, CP_KP_ENTER, MOD_ALT)
                {
                    return true;
                }
                if matches_modify_other_keys(data, CP_ENTER, MOD_ALT) {
                    return true;
                }
                if !kitty_active {
                    return data == "\x1b\r";
                }
                return false;
            }
            if modifier == 0 {
                return data == "\r"
                    || (!kitty_active && data == "\n")
                    || data == "\x1bOM"
                    || matches_kitty_sequence(data, CP_ENTER, 0)
                    || matches_kitty_sequence(data, CP_KP_ENTER, 0);
            }
            matches_kitty_sequence(data, CP_ENTER, modifier)
                || matches_kitty_sequence(data, CP_KP_ENTER, modifier)
                || matches_modify_other_keys(data, CP_ENTER, modifier)
        }
        "backspace" => {
            if modifier == MOD_ALT {
                if data == "\x1b\x7f" || data == "\x1b\x08" {
                    return true;
                }
                return matches_kitty_sequence(data, CP_BACKSPACE, MOD_ALT)
                    || matches_modify_other_keys(data, CP_BACKSPACE, MOD_ALT);
            }
            if modifier == MOD_CTRL {
                if matches_raw_backspace(data, MOD_CTRL) {
                    return true;
                }
                return matches_kitty_sequence(data, CP_BACKSPACE, MOD_CTRL)
                    || matches_modify_other_keys(data, CP_BACKSPACE, MOD_CTRL);
            }
            if modifier == 0 {
                return matches_raw_backspace(data, 0)
                    || matches_kitty_sequence(data, CP_BACKSPACE, 0)
                    || matches_modify_other_keys(data, CP_BACKSPACE, 0);
            }
            matches_kitty_sequence(data, CP_BACKSPACE, modifier)
                || matches_modify_other_keys(data, CP_BACKSPACE, modifier)
        }
        "insert" => {
            if modifier == 0 {
                return matches_legacy_sequence(data, legacy_key_sequences("insert").unwrap())
                    || matches_kitty_sequence(data, FN_INSERT, 0);
            }
            if matches_legacy_modifier_sequence(data, "insert", modifier) {
                return true;
            }
            matches_kitty_sequence(data, FN_INSERT, modifier)
        }
        "delete" => {
            if modifier == 0 {
                return matches_legacy_sequence(data, legacy_key_sequences("delete").unwrap())
                    || matches_kitty_sequence(data, FN_DELETE, 0);
            }
            if matches_legacy_modifier_sequence(data, "delete", modifier) {
                return true;
            }
            matches_kitty_sequence(data, FN_DELETE, modifier)
        }
        "clear" => {
            if modifier == 0 {
                return matches_legacy_sequence(data, legacy_key_sequences("clear").unwrap());
            }
            matches_legacy_modifier_sequence(data, "clear", modifier)
        }
        "home" => {
            if modifier == 0 {
                return matches_legacy_sequence(data, legacy_key_sequences("home").unwrap())
                    || matches_kitty_sequence(data, FN_HOME, 0);
            }
            if matches_legacy_modifier_sequence(data, "home", modifier) {
                return true;
            }
            matches_kitty_sequence(data, FN_HOME, modifier)
        }
        "end" => {
            if modifier == 0 {
                return matches_legacy_sequence(data, legacy_key_sequences("end").unwrap())
                    || matches_kitty_sequence(data, FN_END, 0);
            }
            if matches_legacy_modifier_sequence(data, "end", modifier) {
                return true;
            }
            matches_kitty_sequence(data, FN_END, modifier)
        }
        "pageup" => {
            if modifier == 0 {
                return matches_legacy_sequence(data, legacy_key_sequences("pageUp").unwrap())
                    || matches_kitty_sequence(data, FN_PAGE_UP, 0);
            }
            if matches_legacy_modifier_sequence(data, "pageUp", modifier) {
                return true;
            }
            matches_kitty_sequence(data, FN_PAGE_UP, modifier)
        }
        "pagedown" => {
            if modifier == 0 {
                return matches_legacy_sequence(data, legacy_key_sequences("pageDown").unwrap())
                    || matches_kitty_sequence(data, FN_PAGE_DOWN, 0);
            }
            if matches_legacy_modifier_sequence(data, "pageDown", modifier) {
                return true;
            }
            matches_kitty_sequence(data, FN_PAGE_DOWN, modifier)
        }
        "up" => {
            if modifier == MOD_ALT {
                return data == "\x1bp" || matches_kitty_sequence(data, ARROW_UP, MOD_ALT);
            }
            if modifier == 0 {
                return matches_legacy_sequence(data, legacy_key_sequences("up").unwrap())
                    || matches_kitty_sequence(data, ARROW_UP, 0);
            }
            if matches_legacy_modifier_sequence(data, "up", modifier) {
                return true;
            }
            matches_kitty_sequence(data, ARROW_UP, modifier)
        }
        "down" => {
            if modifier == MOD_ALT {
                return data == "\x1bn" || matches_kitty_sequence(data, ARROW_DOWN, MOD_ALT);
            }
            if modifier == 0 {
                return matches_legacy_sequence(data, legacy_key_sequences("down").unwrap())
                    || matches_kitty_sequence(data, ARROW_DOWN, 0);
            }
            if matches_legacy_modifier_sequence(data, "down", modifier) {
                return true;
            }
            matches_kitty_sequence(data, ARROW_DOWN, modifier)
        }
        "left" => {
            if modifier == MOD_ALT {
                return data == "\x1b[1;3D"
                    || (!kitty_active && data == "\x1bB")
                    || data == "\x1bb"
                    || matches_kitty_sequence(data, ARROW_LEFT, MOD_ALT);
            }
            if modifier == MOD_CTRL {
                return data == "\x1b[1;5D"
                    || matches_legacy_modifier_sequence(data, "left", MOD_CTRL)
                    || matches_kitty_sequence(data, ARROW_LEFT, MOD_CTRL);
            }
            if modifier == 0 {
                return matches_legacy_sequence(data, legacy_key_sequences("left").unwrap())
                    || matches_kitty_sequence(data, ARROW_LEFT, 0);
            }
            if matches_legacy_modifier_sequence(data, "left", modifier) {
                return true;
            }
            matches_kitty_sequence(data, ARROW_LEFT, modifier)
        }
        "right" => {
            if modifier == MOD_ALT {
                return data == "\x1b[1;3C"
                    || (!kitty_active && data == "\x1bF")
                    || data == "\x1bf"
                    || matches_kitty_sequence(data, ARROW_RIGHT, MOD_ALT);
            }
            if modifier == MOD_CTRL {
                return data == "\x1b[1;5C"
                    || matches_legacy_modifier_sequence(data, "right", MOD_CTRL)
                    || matches_kitty_sequence(data, ARROW_RIGHT, MOD_CTRL);
            }
            if modifier == 0 {
                return matches_legacy_sequence(data, legacy_key_sequences("right").unwrap())
                    || matches_kitty_sequence(data, ARROW_RIGHT, 0);
            }
            if matches_legacy_modifier_sequence(data, "right", modifier) {
                return true;
            }
            matches_kitty_sequence(data, ARROW_RIGHT, modifier)
        }
        "f1" | "f2" | "f3" | "f4" | "f5" | "f6" | "f7" | "f8" | "f9" | "f10" | "f11" | "f12" => {
            if modifier != 0 {
                return false;
            }
            matches_legacy_sequence(data, legacy_key_sequences(key.as_str()).unwrap())
        }
        _ => matches_single_char_key(data, &key, modifier),
    }
}

fn format_parsed_key(
    codepoint: i64,
    modifier: i64,
    base_layout_key: Option<i64>,
) -> Option<String> {
    let normalized_codepoint = normalize_kitty_functional_codepoint(codepoint);
    let identity_codepoint =
        normalize_shifted_letter_identity_codepoint(normalized_codepoint, modifier);

    let is_latin_letter = (97..=122).contains(&identity_codepoint);
    let is_digit = (48..=57).contains(&identity_codepoint);
    let is_known_symbol = char_from_cp(identity_codepoint).is_some_and(is_symbol_key_char);
    let effective_codepoint = if is_latin_letter || is_digit || is_known_symbol {
        identity_codepoint
    } else {
        base_layout_key.unwrap_or(identity_codepoint)
    };

    let key_name: Option<String> = if effective_codepoint == CP_ESCAPE {
        Some("escape".to_string())
    } else if effective_codepoint == CP_TAB {
        Some("tab".to_string())
    } else if effective_codepoint == CP_ENTER || effective_codepoint == CP_KP_ENTER {
        Some("enter".to_string())
    } else if effective_codepoint == CP_SPACE {
        Some("space".to_string())
    } else if effective_codepoint == CP_BACKSPACE {
        Some("backspace".to_string())
    } else if effective_codepoint == FN_DELETE {
        Some("delete".to_string())
    } else if effective_codepoint == FN_INSERT {
        Some("insert".to_string())
    } else if effective_codepoint == FN_HOME {
        Some("home".to_string())
    } else if effective_codepoint == FN_END {
        Some("end".to_string())
    } else if effective_codepoint == FN_PAGE_UP {
        Some("pageUp".to_string())
    } else if effective_codepoint == FN_PAGE_DOWN {
        Some("pageDown".to_string())
    } else if effective_codepoint == ARROW_UP {
        Some("up".to_string())
    } else if effective_codepoint == ARROW_DOWN {
        Some("down".to_string())
    } else if effective_codepoint == ARROW_LEFT {
        Some("left".to_string())
    } else if effective_codepoint == ARROW_RIGHT {
        Some("right".to_string())
    } else if (48..=57).contains(&effective_codepoint) || (97..=122).contains(&effective_codepoint)
    {
        char_from_cp(effective_codepoint).map(|c| c.to_string())
    } else if char_from_cp(effective_codepoint).is_some_and(is_symbol_key_char) {
        char_from_cp(effective_codepoint).map(|c| c.to_string())
    } else {
        None
    };

    format_key_name_with_modifiers(&key_name?, modifier)
}

/// Parse input data and return the key identifier if recognized (`parseKey`, keys.ts:1251).
pub fn parse_key(data: &str) -> Option<String> {
    if let Some(kitty) = parse_kitty_sequence(data) {
        return format_parsed_key(kitty.codepoint, kitty.modifier, kitty.base_layout_key);
    }
    if let Some(moks) = parse_modify_other_keys_sequence(data) {
        return format_parsed_key(moks.codepoint, moks.modifier, None);
    }

    let kitty_active = is_kitty_protocol_active();
    if kitty_active && (data == "\x1b\r" || data == "\n") {
        return Some("shift+enter".to_string());
    }

    if let Some(id) = legacy_sequence_key_id(data) {
        return Some(id.to_string());
    }

    if data == "\x1b" {
        return Some("escape".to_string());
    }
    if data == "\x1c" {
        return Some("ctrl+\\".to_string());
    }
    if data == "\x1d" {
        return Some("ctrl+]".to_string());
    }
    if data == "\x1f" {
        return Some("ctrl+-".to_string());
    }
    if data == "\x1b\x1b" {
        return Some("ctrl+alt+[".to_string());
    }
    if data == "\x1b\x1c" {
        return Some("ctrl+alt+\\".to_string());
    }
    if data == "\x1b\x1d" {
        return Some("ctrl+alt+]".to_string());
    }
    if data == "\x1b\x1f" {
        return Some("ctrl+alt+-".to_string());
    }
    if data == "\t" {
        return Some("tab".to_string());
    }
    if data == "\r" || (!kitty_active && data == "\n") || data == "\x1bOM" {
        return Some("enter".to_string());
    }
    if data == "\x00" {
        return Some("ctrl+space".to_string());
    }
    if data == " " {
        return Some("space".to_string());
    }
    if data == "\x7f" {
        return Some("backspace".to_string());
    }
    if data == "\x08" {
        return Some(
            if is_windows_terminal_session() {
                "ctrl+backspace"
            } else {
                "backspace"
            }
            .to_string(),
        );
    }
    if data == "\x1b[Z" {
        return Some("shift+tab".to_string());
    }
    if !kitty_active && data == "\x1b\r" {
        return Some("alt+enter".to_string());
    }
    if !kitty_active && data == "\x1b " {
        return Some("alt+space".to_string());
    }
    if data == "\x1b\x7f" || data == "\x1b\x08" {
        return Some("alt+backspace".to_string());
    }
    if !kitty_active && data == "\x1bB" {
        return Some("alt+left".to_string());
    }
    if !kitty_active && data == "\x1bF" {
        return Some("alt+right".to_string());
    }
    if !kitty_active {
        let chars: Vec<char> = data.chars().collect();
        if chars.len() == 2 && chars[0] == '\x1b' {
            let code = chars[1] as i64;
            if (1..=26).contains(&code) {
                if let Some(c) = char_from_cp(code + 96) {
                    return Some(format!("ctrl+alt+{c}"));
                }
            }
            let key = chars[1];
            if (97..=122).contains(&code) || (48..=57).contains(&code) || is_symbol_key_char(key) {
                return Some(format!("alt+{key}"));
            }
        }
    }
    if data == "\x1b[A" {
        return Some("up".to_string());
    }
    if data == "\x1b[B" {
        return Some("down".to_string());
    }
    if data == "\x1b[C" {
        return Some("right".to_string());
    }
    if data == "\x1b[D" {
        return Some("left".to_string());
    }
    if data == "\x1b[H" || data == "\x1bOH" {
        return Some("home".to_string());
    }
    if data == "\x1b[F" || data == "\x1bOF" {
        return Some("end".to_string());
    }
    if data == "\x1b[3~" {
        return Some("delete".to_string());
    }
    if data == "\x1b[5~" {
        return Some("pageUp".to_string());
    }
    if data == "\x1b[6~" {
        return Some("pageDown".to_string());
    }

    let chars: Vec<char> = data.chars().collect();
    if chars.len() == 1 {
        let code = chars[0] as i64;
        if (1..=26).contains(&code) {
            return char_from_cp(code + 96).map(|c| format!("ctrl+{c}"));
        }
        if (32..=126).contains(&code) {
            return Some(data.to_string());
        }
    }

    None
}

// =============================================================================
// Kitty CSI-u Printable Decoding
// =============================================================================

const KITTY_PRINTABLE_ALLOWED_MODIFIERS: i64 = MOD_SHIFT | LOCK_MASK;

/// Decode a Kitty CSI-u sequence into a printable character, if applicable
/// (`decodeKittyPrintable`, keys.ts:1350). Only accepts plain or Shift-modified
/// keys; rejects Ctrl/Alt and unsupported modifier combinations.
pub fn decode_kitty_printable(data: &str) -> Option<String> {
    let (codepoint, shifted_key, _base_layout_key, mod_value) = parse_csi_u(data)?;
    let modifier = mod_value - 1;

    if (modifier & !KITTY_PRINTABLE_ALLOWED_MODIFIERS) != 0 {
        return None;
    }
    if (modifier & (MOD_ALT | MOD_CTRL)) != 0 {
        return None;
    }

    let mut effective_codepoint = codepoint;
    if (modifier & MOD_SHIFT) != 0 {
        if let Some(sk) = shifted_key {
            effective_codepoint = sk;
        }
    }
    effective_codepoint = normalize_kitty_functional_codepoint(effective_codepoint);
    if effective_codepoint < 32 {
        return None;
    }

    char_from_cp(effective_codepoint).map(|c| c.to_string())
}

fn decode_modify_other_keys_printable(data: &str) -> Option<String> {
    let parsed = parse_modify_other_keys_sequence(data)?;
    let modifier = parsed.modifier & !LOCK_MASK;
    if (modifier & !MOD_SHIFT) != 0 {
        return None;
    }
    if parsed.codepoint < 32 {
        return None;
    }
    char_from_cp(parsed.codepoint).map(|c| c.to_string())
}

/// `decodePrintableKey` (keys.ts:1399).
pub fn decode_printable_key(data: &str) -> Option<String> {
    decode_kitty_printable(data).or_else(|| decode_modify_other_keys_printable(data))
}

// =============================================================================
// Key Release / Repeat Detection
// =============================================================================

/// Check if the last parsed key event was a key release (`isKeyRelease`, keys.ts:527).
/// Only meaningful when Kitty keyboard protocol with flag 2 is active. Direct
/// substring checks on `data`, matching the TS source exactly — see module docs
/// for why this does NOT consult any parsed `eventType` state.
pub fn is_key_release(data: &str) -> bool {
    if data.contains("\x1b[200~") {
        return false;
    }
    [":3u", ":3~", ":3A", ":3B", ":3C", ":3D", ":3H", ":3F"]
        .iter()
        .any(|p| data.contains(p))
}

/// Check if the last parsed key event was a key repeat (`isKeyRepeat`, keys.ts:557).
pub fn is_key_repeat(data: &str) -> bool {
    if data.contains("\x1b[200~") {
        return false;
    }
    [":2u", ":2~", ":2A", ":2B", ":2C", ":2D", ":2H", ":2F"]
        .iter()
        .any(|p| data.contains(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_key_plain_ctrl_c() {
        assert!(matches_key("\x03", "ctrl+c"));
        assert!(!matches_key("c", "ctrl+c"));
    }

    #[test]
    fn parse_key_plain_letter() {
        assert_eq!(parse_key("a"), Some("a".to_string()));
    }

    #[test]
    fn kitty_protocol_state_roundtrips() {
        set_kitty_protocol_active(true);
        assert!(is_kitty_protocol_active());
        set_kitty_protocol_active(false);
        assert!(!is_kitty_protocol_active());
    }

    #[test]
    fn is_key_release_ignores_paste_content() {
        assert!(!is_key_release("\x1b[200~90:62:3F:A5\x1b[201~"));
        assert!(is_key_release("\x1b[97;5:3u"));
    }
}
