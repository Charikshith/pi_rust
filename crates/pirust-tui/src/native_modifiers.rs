//! `native-modifiers.ts` (66 lines) — probe whether a macOS modifier key is
//! physically pressed, via a prebuilt native addon (`darwin-modifiers.node`)
//! loaded through `require`. Real Pi supports **macOS only**
//! (`process.platform !== "darwin"` short-circuits to `undefined`/`false`
//! on every other platform, including Windows) and only for `x64`/`arm64`
//! (`process.arch` guard). The addon itself is not part of this repository
//! checkout (it lives in a `native/darwin/prebuilds/...` directory this
//! port does not vendor), so `loadNativeModifiersHelper()` always fails to
//! find it and `isNativeModifierPressed` always returns `false` — exactly
//! the TS's own fail-closed contract when the helper can't be loaded.
//!
//! This port has no native addon to load either, so it mirrors that
//! fail-closed outcome directly rather than inventing a different native
//! probe (e.g. calling `CGEventSourceFlagsState`/`GetAsyncKeyState`
//! ourselves, which real Pi does not do).

/// `ModifierKey` (native-modifiers.ts).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ModifierKey {
    Shift,
    Command,
    Control,
    Option,
}

/// `isNativeModifierPressed` (native-modifiers.ts:51) — always `false` here;
/// see module docs.
pub fn is_native_modifier_pressed(key: ModifierKey) -> bool {
    let _ = key;
    false
}
