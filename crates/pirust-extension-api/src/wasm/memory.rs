//! Guest linear-memory helpers + the `(ptr, len)` packing convention shared
//! by every WASM extension ABI call (see repo-root `plan.md`'s "Guest ABI"
//! section).

use wasmtime::{AsContext, AsContextMut, Caller};

use super::HostState;

/// Pack a `(ptr, len)` pair into the single `i64` every ABI function
/// returns/accepts as its packed encoding. Both halves are unsigned 32-bit
/// values.
pub(super) fn pack(ptr: u32, len: u32) -> i64 {
    (((ptr as u64) << 32) | (len as u64)) as i64
}

/// Unpack an ABI return value back into `(ptr, len)`.
pub(super) fn unpack(packed: i64) -> (u32, u32) {
    let bits = packed as u64;
    ((bits >> 32) as u32, (bits & 0xFFFF_FFFF) as u32)
}

/// Fetch the guest's exported linear memory — the `memory` export every
/// `wasm32-unknown-unknown` binary produces by default.
fn guest_memory(caller: &mut Caller<'_, HostState>) -> Result<wasmtime::Memory, String> {
    caller
        .get_export("memory")
        .and_then(wasmtime::Extern::into_memory)
        .ok_or_else(|| "wasm guest has no exported `memory`".to_string())
}

/// Read `len` bytes at `ptr` out of the guest's linear memory.
pub(super) fn read_bytes(
    caller: &mut Caller<'_, HostState>,
    ptr: u32,
    len: u32,
) -> Result<Vec<u8>, String> {
    let memory = guest_memory(caller)?;
    let start = ptr as usize;
    let end = start
        .checked_add(len as usize)
        .ok_or_else(|| "wasm guest pointer/length overflow".to_string())?;
    let data = memory.data(caller.as_context());
    data.get(start..end)
        .map(<[u8]>::to_vec)
        .ok_or_else(|| "wasm guest memory access out of bounds".to_string())
}

/// Read a UTF-8 string at `ptr`/`len`.
pub(super) fn read_string(
    caller: &mut Caller<'_, HostState>,
    ptr: u32,
    len: u32,
) -> Result<String, String> {
    let bytes = read_bytes(caller, ptr, len)?;
    String::from_utf8(bytes).map_err(|e| format!("wasm guest string was not valid UTF-8: {e}"))
}

/// Write `bytes` into guest memory at `ptr` (the guest must already have
/// allocated at least `bytes.len()` bytes there, e.g. via `pi_alloc`).
pub(super) fn write_bytes(
    caller: &mut Caller<'_, HostState>,
    ptr: u32,
    bytes: &[u8],
) -> Result<(), String> {
    let memory = guest_memory(caller)?;
    memory
        .write(caller.as_context_mut(), ptr as usize, bytes)
        .map_err(|e| format!("wasm guest memory write failed: {e}"))
}
