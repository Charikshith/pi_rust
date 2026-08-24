//! Minimal proof-of-ABI extension for feat-010 Waves 1-3. Registers five
//! tools and one event subscription — enough to prove
//! `WasmExtensionLoader`'s full round trip (`pi_alloc` -> `pi_activate` ->
//! `pi_handle`, plus the `pi_host_call` door in both directions) actually
//! works against a real compiled `wasm32-unknown-unknown` binary, not just a
//! hand-written WAT fixture. `burn_fuel` and `grow_memory` are Wave 3's
//! deliberately-broken guest tools, proving the sandbox limits actually
//! stop a runaway/memory-hungry guest rather than just existing on paper.
//!
//! Wave 5: `pi_dealloc` closes the Wave-1 "every allocation leaks" gap.
//! Ownership rule (see repo-root `plan.md`'s Guest ABI section): whoever
//! reads a `(ptr, len)` buffer LAST frees it. The host frees anything it
//! reads back from this guest (`pi_activate`/`pi_handle` results, and the
//! `op`/`payload` buffers it wrote via `pi_alloc` once `pi_handle` returns)
//! — no guest-side change needed for those. The one buffer only the GUEST
//! can free is a `pi_host_call` response: the host writes it into this
//! guest's own memory and hands back a pointer, but control returns to the
//! guest afterward, so `call_host_raw` frees it itself once parsed.

use serde_json::Value;

#[link(wasm_import_module = "env")]
extern "C" {
    fn pi_host_call(op_ptr: i32, op_len: i32, payload_ptr: i32, payload_len: i32) -> i64;
}

fn pack(ptr: u32, len: u32) -> i64 {
    (((ptr as u64) << 32) | (len as u64)) as i64
}

fn unpack(packed: i64) -> (u32, u32) {
    let bits = packed as u64;
    ((bits >> 32) as u32, (bits & 0xFFFF_FFFF) as u32)
}

fn leak_bytes(bytes: Vec<u8>) -> (u32, u32) {
    let boxed = bytes.into_boxed_slice();
    let len = boxed.len() as u32;
    let ptr = Box::into_raw(boxed).cast::<u8>() as u32;
    (ptr, len)
}

fn leak_response(value: &Value) -> i64 {
    let bytes = serde_json::to_vec(value).unwrap_or_default();
    let (ptr, len) = leak_bytes(bytes);
    pack(ptr, len)
}

/// The exact inverse of `leak_bytes`: reconstructs the original
/// `Box<[u8]>` from its raw parts and drops it, freeing the allocation.
/// Only valid on a `(ptr, len)` this instance produced itself via
/// `leak_bytes`/`pi_alloc` — never on a pointer received FROM the host
/// (that memory belongs to the host's own allocator).
#[no_mangle]
pub extern "C" fn pi_dealloc(ptr: i32, len: i32) {
    if ptr == 0 || len <= 0 {
        return;
    }
    // SAFETY: every live `ptr`/`len` pair this instance hands out was
    // produced by `leak_bytes` from a `Box<[u8]>` of exactly this length,
    // per the ABI's single-owner-frees-once contract; the caller (host or
    // this module's own `call_host_raw`) is trusted not to double-free or
    // use the pointer again afterward.
    unsafe {
        let slice = std::ptr::slice_from_raw_parts_mut(ptr as *mut u8, len as usize);
        drop(Box::from_raw(slice));
    }
}

/// Calls the one host-call door with `op` and the given raw payload range,
/// returning the host's decoded JSON response.
fn call_host_raw(op: &str, payload_ptr: i32, payload_len: i32) -> Value {
    // SAFETY: `pi_host_call` is imported from "env" per the ABI contract;
    // `op.as_ptr()`/`op.len()` describe this instance's own, currently-live
    // string buffer, valid for the duration of this call. `payload_ptr`/
    // `payload_len` (when non-zero) describe a buffer this same instance
    // leaked via `leak_bytes` just before this call, so it is also live.
    let packed = unsafe { pi_host_call(op.as_ptr() as i32, op.len() as i32, payload_ptr, payload_len) };
    let (ptr, len) = unpack(packed);
    // SAFETY: the host always returns a (ptr, len) pointing at bytes it just
    // wrote into this instance's own memory via a reentrant pi_alloc call.
    let bytes = unsafe { std::slice::from_raw_parts(ptr as *const u8, len as usize) };
    let value = serde_json::from_slice(bytes).unwrap_or(Value::Null);
    // The host has already returned control to us — we are the last (and
    // only) reader of this buffer, so we free it ourselves.
    pi_dealloc(ptr as i32, len as i32);
    value
}

/// Calls a host-call op that takes no payload (e.g. `get_active_tools`).
fn call_host(op: &str) -> Value {
    call_host_raw(op, 0, 0)
}

/// Calls a host-call op with a JSON payload (e.g. `send_message`).
fn call_host_with_payload(op: &str, payload: &Value) -> Value {
    let bytes = serde_json::to_vec(payload).unwrap_or_default();
    let (ptr, len) = leak_bytes(bytes);
    call_host_raw(op, ptr as i32, len as i32)
}

/// Allocate `len` bytes in this instance's own linear memory and return the
/// pointer. The host writes a JSON request there before calling
/// `pi_handle`.
#[no_mangle]
pub extern "C" fn pi_alloc(len: i32) -> i32 {
    if len <= 0 {
        return 0;
    }
    let (ptr, _) = leak_bytes(vec![0_u8; len as usize]);
    ptr as i32
}

/// Called once at load. Registers three tools and one event subscription.
#[no_mangle]
pub extern "C" fn pi_activate() -> i64 {
    let registration = serde_json::json!({
        "tools": [
            {
                "name": "echo",
                "label": "Echo",
                "description": "Returns its input params unchanged. Proves the Wave 1 WASM ABI round-trip."
            },
            {
                "name": "list_active_tools",
                "label": "List Active Tools",
                "description": "Calls back into the host's get_active_tools door and returns the result. Proves pi_host_call round-trips, not just pi_activate/pi_handle."
            },
            {
                "name": "exercise_doors",
                "label": "Exercise Doors",
                "description": "Calls send_message, send_user_message, append_entry, set_active_tools, abort, and shutdown via pi_host_call and reports whether each succeeded. Proves the Wave 2 + Wave 5 host-call doors."
            },
            {
                "name": "burn_fuel",
                "label": "Burn Fuel",
                "description": "Runs a genuine infinite loop. Deliberately broken: proves the Wave 3 fuel budget traps a runaway guest instead of hanging the host."
            },
            {
                "name": "grow_memory",
                "label": "Grow Memory",
                "description": "Tries to allocate far more linear memory than the configured ceiling allows. Deliberately broken: proves the Wave 3 memory limiter traps the attempt instead of exhausting host memory."
            }
        ],
        "commands": [],
        "flags": [],
        "events": ["agent_start"]
    });
    leak_response(&registration)
}

fn host_call_ok(response: &Value) -> Value {
    response.get("ok").cloned().unwrap_or(Value::Bool(false))
}

/// The single generic dispatch entrypoint. `op_ptr`/`op_len` and
/// `payload_ptr`/`payload_len` describe byte ranges the host already wrote
/// into this instance's own linear memory via `pi_alloc`, per the ABI
/// contract in the repo root `plan.md`.
#[no_mangle]
pub extern "C" fn pi_handle(op_ptr: i32, op_len: i32, payload_ptr: i32, payload_len: i32) -> i64 {
    // SAFETY: the host only ever calls `pi_handle` with a (ptr, len) pair it
    // just obtained from this same instance's `pi_alloc` and then wrote
    // `len` bytes into, per the documented ABI contract.
    let op = unsafe {
        let slice = std::slice::from_raw_parts(op_ptr as *const u8, op_len as usize);
        String::from_utf8_lossy(slice).into_owned()
    };
    // SAFETY: same contract as above, for the payload range.
    let payload = unsafe { std::slice::from_raw_parts(payload_ptr as *const u8, payload_len as usize) };

    let response = match op.as_str() {
        "tool:echo" => {
            let value: Value = serde_json::from_slice(payload).unwrap_or(Value::Null);
            serde_json::json!({"ok": true, "value": value})
        }
        "tool:list_active_tools" => {
            // Unwrap the host's own {"ok","value"/"error"} envelope so this
            // tool's result is the plain tool-name list, not a nested one.
            let host_response = call_host("get_active_tools");
            if host_response.get("ok") == Some(&Value::Bool(true)) {
                let names = host_response.get("value").cloned().unwrap_or(Value::Null);
                serde_json::json!({"ok": true, "value": names})
            } else {
                serde_json::json!({"ok": false, "error": host_response.get("error").cloned().unwrap_or(Value::Null)})
            }
        }
        "tool:exercise_doors" => {
            let send_message = call_host_with_payload(
                "send_message",
                &serde_json::json!({"message": {"text": "hello from guest"}, "options": {}}),
            );
            let send_user_message = call_host_with_payload(
                "send_user_message",
                &serde_json::json!({"content": "hi from guest", "options": {}}),
            );
            let append_entry = call_host_with_payload(
                "append_entry",
                &serde_json::json!({"custom_type": "wasm_test", "data": {"n": 1}}),
            );
            let set_active_tools = call_host_with_payload(
                "set_active_tools",
                &serde_json::json!({"tools": ["echo"]}),
            );
            let abort = call_host("abort");
            let shutdown = call_host("shutdown");

            serde_json::json!({
                "ok": true,
                "value": {
                    "send_message": host_call_ok(&send_message),
                    "send_user_message": host_call_ok(&send_user_message),
                    "append_entry": host_call_ok(&append_entry),
                    "set_active_tools": host_call_ok(&set_active_tools),
                    "abort": host_call_ok(&abort),
                    "shutdown": host_call_ok(&shutdown),
                }
            })
        }
        "tool:burn_fuel" => {
            // A genuine infinite loop, not a bounded busy-wait — proves the
            // host's fuel budget (not a guest-side cooperative check) is
            // what actually stops this. `black_box` stops the compiler from
            // proving the loop has no observable effect and eliding it.
            let mut counter: u64 = 0;
            loop {
                counter = std::hint::black_box(counter.wrapping_add(1));
            }
        }
        "tool:grow_memory" => {
            // Requests far more linear memory than any sane extension
            // (or the configured ceiling) should allow in one shot. If the
            // host's memory limiter is working, the underlying
            // `memory.grow` traps before this ever finishes — this line
            // intentionally never completes successfully against a
            // correctly configured host. `black_box` is required here:
            // without it, since only `.len()` (a compile-time-known
            // constant) is ever read back, LLVM proved the allocation and
            // its zero-fill were unobservable and deleted them entirely in
            // release builds — no real `memory.grow` ever happened, and
            // this "malicious" tool silently became a no-op that always
            // "succeeded". `black_box` forces the allocation to actually
            // occur.
            let huge = std::hint::black_box(vec![0_u8; 160 * 1024 * 1024]);
            serde_json::json!({"ok": true, "value": huge.len()})
        }
        "event:agent_start" => {
            let event_payload: Value = serde_json::from_slice(payload).unwrap_or(Value::Null);
            let system_prompt = event_payload
                .get("context")
                .and_then(|c| c.get("system_prompt"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();

            // Proves a host-call door works from INSIDE an event handler,
            // not just inside a tool call.
            let append_response = call_host_with_payload(
                "append_entry",
                &serde_json::json!({
                    "custom_type": "wasm_agent_start",
                    "data": {"system_prompt": system_prompt}
                }),
            );

            if host_call_ok(&append_response) == Value::Bool(true) {
                serde_json::json!({"ok": true, "value": "agent_start handled"})
            } else {
                serde_json::json!({"ok": false, "error": "append_entry failed inside event handler"})
            }
        }
        other => serde_json::json!({"ok": false, "error": format!("unknown op: {other}")}),
    };
    leak_response(&response)
}
