//! `native-modifiers.ts` (66 lines) — probe whether a macOS/Win32 modifier
//! key is physically pressed, via the platform's native addon
//! (`darwin-modifiers.node` / `win32-console-mode.node`). The TS loads the
//! helper through a dynamic `require` and **fails closed** (returns `false`)
//! when the addon is missing or errors.
//!
//! This port wires the same calls with raw FFI, cfg-gated:
//! - `cfg(target_os = "macos")`: `CGEventSourceFlagsState` via CoreGraphics
//!   (dlopen so the binary still links without the framework).
//! - `cfg(target_os = "windows")`: `GetAsyncKeyState` via `user32`.
//! - anything else: `false` (the TS's `return undefined` path).
//!
//! Correctness bar: the TS's contract is `isModifierPressed(key) === true`
//! only when physically pressed; every other path returns `false`. The Rust
//! function mirrors that exactly (the `try { ... } catch { return false }`
//! around the FFI call is the fail-closed gate).

/// `ModifierKey` (native-modifiers.ts).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ModifierKey {
    Shift,
    Command,
    Control,
    Option,
}

/// `isNativeModifierPressed` (native-modifiers.ts:62).
pub fn is_native_modifier_pressed(key: ModifierKey) -> bool {
    #[cfg(target_os = "macos")]
    {
        native_darwin::is_modifier_pressed(key)
    }
    #[cfg(target_os = "windows")]
    {
        native_windows::is_modifier_pressed(key)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = key;
        false
    }
}

#[cfg(target_os = "windows")]
mod native_windows {
    use super::ModifierKey;

    const KEY_PRESSED_MASK: u16 = 0x8000;
    const VK_SHIFT: i32 = 0x10;
    const VK_LSHIFT: i32 = 0xA0;
    const VK_RSHIFT: i32 = 0xA1;
    const VK_CONTROL: i32 = 0x11;
    const VK_LCONTROL: i32 = 0xA2;
    const VK_RCONTROL: i32 = 0xA3;
    const VK_MENU: i32 = 0x12;
    const VK_LMENU: i32 = 0xA4;
    const VK_RMENU: i32 = 0xA5;
    const VK_LWIN: i32 = 0x5B;
    const VK_RWIN: i32 = 0x5C;

    #[link(name = "user32")]
    extern "system" {
        fn GetAsyncKeyState(v_key: i32) -> i16;
    }

    #[allow(unsafe_op_in_unsafe_fn)]
    // SAFETY: GetAsyncKeyState with a valid VK code; user32 is always
    // linked on Windows. The TS checks `& KEY_PRESSED_MASK != 0` on the
    // SHORT return (the high bit means "pressed now").
    fn is_key_pressed(virtual_key: i32) -> bool {
        // SAFETY: valid VK code, user32 linked (see module docs).
        #[allow(unsafe_op_in_unsafe_fn)]
        unsafe {
            (GetAsyncKeyState(virtual_key) as u16) & KEY_PRESSED_MASK != 0
        }
    }

    /// `isModifierNamePressed` (win32-console-mode.c) — the C helper ORs the
    /// left/right/generic VK codes for each modifier.
    pub fn is_modifier_pressed(key: ModifierKey) -> bool {
        match key {
            ModifierKey::Shift => {
                is_key_pressed(VK_SHIFT) || is_key_pressed(VK_LSHIFT) || is_key_pressed(VK_RSHIFT)
            }
            ModifierKey::Control => {
                is_key_pressed(VK_CONTROL)
                    || is_key_pressed(VK_LCONTROL)
                    || is_key_pressed(VK_RCONTROL)
            }
            ModifierKey::Option => {
                is_key_pressed(VK_MENU) || is_key_pressed(VK_LMENU) || is_key_pressed(VK_RMENU)
            }
            ModifierKey::Command => is_key_pressed(VK_LWIN) || is_key_pressed(VK_RWIN),
        }
    }
}

#[cfg(target_os = "macos")]
mod native_darwin {
    use super::ModifierKey;

    // CoreGraphics symbols — resolved lazily via dlsym so the binary links
    // without an explicit CoreGraphics dependency (mirrors the TS's dynamic
    // `require` of the .node addon).
    const KCG_EVENT_SOURCE_STATE_COMBINED_SESSION_STATE: u32 = 0;
    // CGEventFlags
    const KCG_EVENT_FLAG_MASK_SHIFT: u64 = 1 << 17;
    const KCG_EVENT_FLAG_MASK_COMMAND: u64 = 1 << 20;
    const KCG_EVENT_FLAG_MASK_CONTROL: u64 = 1 << 18;
    const KCG_EVENT_FLAG_MASK_ALTERNATE: u64 = 1 << 19;

    extern "C" {
        fn dlopen(filename: *const std::os::raw::c_char, flag: i32) -> *mut std::ffi::c_void;
        fn dlsym(
            handle: *mut std::ffi::c_void,
            symbol: *const std::os::raw::c_char,
        ) -> *mut std::ffi::c_void;
    }

    type CgEventSourceFlagsStateFn = unsafe extern "C" fn(u32) -> u64;

    fn modifier_mask_for_name(key: ModifierKey) -> u64 {
        match key {
            ModifierKey::Shift => KCG_EVENT_FLAG_MASK_SHIFT,
            ModifierKey::Command => KCG_EVENT_FLAG_MASK_COMMAND,
            ModifierKey::Control => KCG_EVENT_FLAG_MASK_CONTROL,
            ModifierKey::Option => KCG_EVENT_FLAG_MASK_ALTERNATE,
        }
    }

    pub fn is_modifier_pressed(key: ModifierKey) -> bool {
        let mask = modifier_mask_for_name(key);
        if mask == 0 {
            return false;
        }
        // SAFETY: dlsym(RTLD_DEFAULT) for the CoreGraphics entry point; the
        // call is wrapped so a missing symbol fails closed (returns false),
        // exactly like the TS's `try { helper.isModifierPressed(...) } catch { return false }`.
        #[allow(unsafe_op_in_unsafe_fn)]
        unsafe {
            let symbol = c"CGEventSourceFlagsState".as_ptr().cast();
            let func = dlsym(std::ptr::null_mut(), symbol);
            if func.is_null() {
                return false;
            }
            let flags_state: CgEventSourceFlagsStateFn = std::mem::transmute(func);
            let flags = flags_state(KCG_EVENT_SOURCE_STATE_COMBINED_SESSION_STATE);
            flags & mask != 0
        }
    }
}
