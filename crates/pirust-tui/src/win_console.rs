//! `win32-console-mode.c` — Windows console-mode FFI: add
//! `ENABLE_VIRTUAL_TERMINAL_INPUT` (0x0200) to the stdin console handle so
//! the terminal sends VT sequences for modified keys (e.g. `\x1b[Z` for
//! Shift+Tab). Without this, ReadConsoleInputW discards modifier state.
//!
//! The TS's `enableWindowsVTInput()` (terminal.ts:366) is a thin wrapper:
//! platform-gate + `try { require("win32-console-mode.node").enableVirtualTerminalInput() } catch {}`
//! — fail closed on every error path. This port is the same, with raw FFI:
//! `GetStdHandle(STD_INPUT_HANDLE)` → `GetConsoleMode` → `SetConsoleMode`.
//!
//! Correctness bar: the C helper returns a boolean (`enabled`); the TS
//! ignores the return value entirely. Fail-closed = the function never
//! panics and never blocks (stdin handle missing → returns false → caller
//! proceeds without VT input).

/// `enableWindowsVTInput` (win32-console-mode.c) — returns whether the
/// console mode was successfully updated (the TS discards this; kept for
/// parity + tests).
#[cfg(target_os = "windows")]
pub fn enable_windows_vt_input() -> bool {
    const STD_INPUT_HANDLE: i32 = -10;
    const ENABLE_VIRTUAL_TERMINAL_INPUT: u32 = 0x0200;

    #[link(name = "kernel32")]
    extern "system" {
        fn GetStdHandle(n_std_handle: i32) -> *mut std::ffi::c_void;
        fn GetConsoleMode(h_console_handle: *mut std::ffi::c_void, lp_mode: *mut u32) -> i32;
        fn SetConsoleMode(h_console_handle: *mut std::ffi::c_void, dw_mode: u32) -> i32;
    }

    // SAFETY: standard Win32 calls on the real stdin handle; failures (not a
    // console, invalid handle) return FALSE/INVALID_HANDLE_VALUE and we
    // return false without panicking — the fail-closed contract.
    #[allow(unsafe_op_in_unsafe_fn)]
    unsafe {
        let handle = GetStdHandle(STD_INPUT_HANDLE);
        if handle.is_null() || handle == std::ptr::without_provenance_mut(u32::MAX as usize) {
            return false;
        }
        let mut mode: u32 = 0;
        if GetConsoleMode(handle, &mut mode) == 0 {
            return false;
        }
        SetConsoleMode(handle, mode | ENABLE_VIRTUAL_TERMINAL_INPUT) != 0
    }
}

/// Non-Windows: no-op, exactly like the TS's `if (process.platform !== "win32") return;`.
#[cfg(not(target_os = "windows"))]
pub fn enable_windows_vt_input() -> bool {
    false
}
