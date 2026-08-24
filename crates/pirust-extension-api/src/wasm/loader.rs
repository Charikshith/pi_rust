//! `WasmExtensionLoader` — loads a `.wasm` file into a real `Extension`
//! whose tool closures and event handlers re-enter the guest via
//! `pi_handle`. Pirust-only addition, no Pi oracle (see repo-root
//! `plan.md`).
//!
//! Wave 2 note (named, not silent — see `plan.md`'s Wave 2 write-up for the
//! full reasoning): the three READ-ONLY `ExtensionContext` accessors
//! (`is_idle`/`has_pending_messages`/`get_system_prompt`) are exposed to a
//! wasm guest only as a plain JSON snapshot taken by the HOST before calling
//! into `pi_handle` — never as a live `pi_host_call` op. Those closures are
//! freshly built by `ExtensionRunner::create_context()` on every single
//! dispatch and carry no `Send` bound, so routing THEM through a live
//! host-call door would need a scoped "current context" slot in `HostState`
//! — out of scope here.
//!
//! Wave 5: `abort()`/`shutdown()` turned out not to need that scoped-slot
//! design at all — they route through `ExtensionRuntime`'s existing
//! `Arc<Mutex<Box<dyn Fn + Send + Sync>>>` slots (the same stable, rebindable
//! mechanism `send_message`/`get_active_tools`/etc. already use), which
//! `HostState` already holds. See `host_call`'s `"abort"`/`"shutdown"` arms.

use std::path::Path;
use std::sync::{Arc, Mutex};

use serde_json::Value;
use wasmtime::{
    Caller, Config, Engine, Instance, Linker, Module, Store, StoreLimitsBuilder, TypedFunc,
};

use crate::context::{ExtensionContext, ExtensionHandler};
use crate::registration::{Extension, RegisteredTool, SourceInfo, ToolDefinition, ToolExecutor};
use crate::runtime::ExtensionRuntime;

use super::memory::{pack, read_bytes, read_string, unpack, write_bytes};
use super::{HostState, WasmExtensionLimits};

/// Loads Rust-authored `.wasm` extensions behind the guest ABI documented in
/// the repo root `plan.md` (`pi_alloc`/`pi_activate`/`pi_handle` exports,
/// one `pi_host_call` import).
pub struct WasmExtensionLoader;

impl WasmExtensionLoader {
    /// Load `path` with the default [`WasmExtensionLimits`]. See
    /// [`Self::load_with_limits`] for the full contract.
    pub fn load(path: &Path, runtime: Arc<ExtensionRuntime>) -> Result<Extension, String> {
        Self::load_with_limits(path, runtime, WasmExtensionLimits::default())
    }

    /// Load `path`, run its `pi_activate`, and build a real [`Extension`]
    /// whose registered tools and event handlers call back into the guest
    /// via `pi_handle` — sandboxed by `limits` (Wave 3: a wasmtime fuel
    /// budget + a linear-memory ceiling, both configured on this call's own
    /// `Engine`/`Store` rather than a global default).
    pub fn load_with_limits(
        path: &Path,
        runtime: Arc<ExtensionRuntime>,
        limits: WasmExtensionLimits,
    ) -> Result<Extension, String> {
        let mut config = Config::new();
        config.consume_fuel(true);
        let engine =
            Engine::new(&config).map_err(|e| format!("failed to build wasm engine: {e}"))?;
        let module = Module::from_file(&engine, path)
            .map_err(|e| format!("failed to load wasm module {}: {e}", path.display()))?;

        let mut linker: Linker<HostState> = Linker::new(&engine);
        linker
            .func_wrap("env", "pi_host_call", host_call)
            .map_err(|e| format!("failed to register pi_host_call import: {e}"))?;

        let store_limits = StoreLimitsBuilder::new()
            .memory_size(limits.max_memory_bytes)
            .trap_on_grow_failure(true)
            .build();
        let mut store = Store::new(
            &engine,
            HostState {
                runtime,
                limits: store_limits,
            },
        );
        store.limiter(|state| &mut state.limits);
        store
            .set_fuel(limits.fuel)
            .map_err(|e| format!("failed to configure wasm fuel budget: {e}"))?;

        let instance = linker
            .instantiate(&mut store, &module)
            .map_err(|e| format!("failed to instantiate wasm extension: {e}"))?;

        let registration = call_activate(&mut store, &instance)?;

        let path_str = path.display().to_string();
        let mut extension = Extension {
            path: path_str.clone(),
            resolved_path: path_str,
            hidden: false,
            handlers: std::collections::HashMap::new(),
            tools: std::collections::HashMap::new(),
            commands: std::collections::HashMap::new(),
            flags: std::collections::HashMap::new(),
            shortcuts: std::collections::HashMap::new(),
        };

        let shared = Arc::new(WasmInstance {
            store: Mutex::new(store),
            instance,
        });

        for tool in registration.tools {
            let executor = make_tool_executor(Arc::clone(&shared), tool.name.clone());
            let source = SourceInfo::inline(&extension.path);
            extension.tools.insert(
                tool.name.clone(),
                RegisteredTool {
                    definition: ToolDefinition {
                        name: tool.name,
                        label: tool.label,
                        description: tool.description,
                        prompt_snippet: None,
                        prompt_guidelines: Vec::new(),
                        execute: executor,
                    },
                    source_info: source,
                },
            );
        }

        for event_type in registration.events {
            let handler = make_event_handler(Arc::clone(&shared), event_type.clone());
            extension
                .handlers
                .entry(event_type)
                .or_default()
                .push(handler);
        }

        Ok(extension)
    }
}

/// The `pi_activate` JSON payload's shape. `commands`/`flags` are parsed
/// defensively (so extra keys don't error) but not yet wired to anything —
/// that is Wave 4 (see `plan.md`). `events` (event-type discriminator
/// strings, matching `ExtensionEvent::event_type()`) is wired this wave.
#[derive(serde::Deserialize)]
struct ActivateResponse {
    #[serde(default)]
    tools: Vec<ActivateTool>,
    #[serde(default)]
    events: Vec<String>,
}

#[derive(serde::Deserialize)]
struct ActivateTool {
    name: String,
    label: String,
    description: String,
}

#[derive(serde::Deserialize)]
struct HandleResponse {
    ok: bool,
    #[serde(default)]
    value: Option<Value>,
    #[serde(default)]
    error: Option<String>,
}

/// A loaded guest instance, shared (behind a mutex — wasmtime's `Store`
/// needs `&mut` access for every call) by every tool closure and event
/// handler the extension registered.
struct WasmInstance {
    store: Mutex<Store<HostState>>,
    instance: Instance,
}

fn call_activate(
    store: &mut Store<HostState>,
    instance: &Instance,
) -> Result<ActivateResponse, String> {
    let activate: TypedFunc<(), i64> = instance
        .get_typed_func(&mut *store, "pi_activate")
        .map_err(|e| format!("wasm extension is missing pi_activate: {e}"))?;
    let packed = activate
        .call(&mut *store, ())
        .map_err(|e| format!("pi_activate trapped: {e}"))?;
    let (ptr, len) = unpack(packed);

    let memory = instance
        .get_memory(&mut *store, "memory")
        .ok_or_else(|| "wasm extension has no exported memory".to_string())?;
    let bytes = memory
        .data(&*store)
        .get(ptr as usize..(ptr as usize + len as usize))
        .ok_or_else(|| "pi_activate returned an out-of-bounds pointer".to_string())?
        .to_vec();
    dealloc_guest(store, instance, ptr, len);

    serde_json::from_slice(&bytes).map_err(|e| format!("pi_activate returned invalid JSON: {e}"))
}

/// Wave 5: free a `(ptr, len)` buffer via the guest's own `pi_dealloc`
/// export, if it has one. Best-effort — a guest built before Wave 5 (no
/// `pi_dealloc` export) is tolerated: the call is silently skipped and that
/// guest's allocations simply keep leaking as they always did, bounded by
/// the Wave 3 memory ceiling exactly as before.
fn dealloc_guest(store: &mut Store<HostState>, instance: &Instance, ptr: u32, len: u32) {
    if ptr == 0 || len == 0 {
        return;
    }
    if let Ok(dealloc) = instance.get_typed_func::<(i32, i32), ()>(&mut *store, "pi_dealloc") {
        let _ = dealloc.call(&mut *store, (ptr as i32, len as i32));
    }
}

/// The shared alloc-write-call(`pi_handle`)-read round trip used by both
/// tool executors and event handlers. Writes `op`/`payload` into the
/// guest's own memory via its `pi_alloc` export, calls `pi_handle`, and
/// decodes the `{"ok"/"value"/"error"}` envelope into `Result<Value, String>`
/// per `ExtensionHandler`/`ToolExecutor`'s shared shape.
fn call_guest(shared: &Arc<WasmInstance>, op: &str, payload: &[u8]) -> Result<Value, String> {
    let mut store = shared
        .store
        .lock()
        .map_err(|_| "wasm store poisoned".to_string())?;
    let op_bytes = op.as_bytes();

    let alloc: TypedFunc<i32, i32> = shared
        .instance
        .get_typed_func(&mut *store, "pi_alloc")
        .map_err(|e| format!("wasm extension is missing pi_alloc: {e}"))?;
    let memory = shared
        .instance
        .get_memory(&mut *store, "memory")
        .ok_or_else(|| "wasm extension has no exported memory".to_string())?;

    let op_ptr = alloc
        .call(&mut *store, op_bytes.len() as i32)
        .map_err(|e| format!("pi_alloc trapped: {e}"))?;
    memory
        .write(&mut *store, op_ptr as usize, op_bytes)
        .map_err(|e| format!("failed writing op into guest memory: {e}"))?;

    let payload_ptr = alloc
        .call(&mut *store, payload.len() as i32)
        .map_err(|e| format!("pi_alloc trapped: {e}"))?;
    memory
        .write(&mut *store, payload_ptr as usize, payload)
        .map_err(|e| format!("failed writing payload into guest memory: {e}"))?;

    let handle: TypedFunc<(i32, i32, i32, i32), i64> = shared
        .instance
        .get_typed_func(&mut *store, "pi_handle")
        .map_err(|e| format!("wasm extension is missing pi_handle: {e}"))?;
    let packed = handle
        .call(
            &mut *store,
            (
                op_ptr,
                op_bytes.len() as i32,
                payload_ptr,
                payload.len() as i32,
            ),
        )
        .map_err(|e| format!("pi_handle trapped: {e}"))?;
    // The guest has already read both input buffers synchronously inside
    // `pi_handle` by the time this call returns — free them now (Wave 5's
    // ownership rule: whoever allocated a buffer the guest is done reading
    // frees it).
    dealloc_guest(
        &mut store,
        &shared.instance,
        op_ptr as u32,
        op_bytes.len() as u32,
    );
    dealloc_guest(
        &mut store,
        &shared.instance,
        payload_ptr as u32,
        payload.len() as u32,
    );

    let (result_ptr, result_len) = unpack(packed);
    let bytes = memory
        .data(&*store)
        .get(result_ptr as usize..(result_ptr as usize + result_len as usize))
        .ok_or_else(|| "pi_handle returned an out-of-bounds pointer".to_string())?
        .to_vec();
    dealloc_guest(&mut store, &shared.instance, result_ptr, result_len);
    drop(store);

    let response: HandleResponse = serde_json::from_slice(&bytes)
        .map_err(|e| format!("pi_handle returned invalid JSON: {e}"))?;
    if response.ok {
        Ok(response.value.unwrap_or(Value::Null))
    } else {
        Err(response
            .error
            .unwrap_or_else(|| "wasm extension returned an error".to_string()))
    }
}

fn make_tool_executor(shared: Arc<WasmInstance>, tool_name: String) -> ToolExecutor {
    Box::new(move |params| {
        let op = format!("tool:{tool_name}");
        let payload = serde_json::to_vec(params.params)
            .map_err(|e| format!("failed to encode tool params: {e}"))?;
        call_guest(&shared, &op, &payload)
    })
}

/// Snapshot the three read-only `ExtensionContext` accessors into plain JSON
/// — computed host-side, in ordinary Rust, before ever touching the guest.
/// See this module's doc comment for why these three go through a snapshot
/// instead of a live `pi_host_call` op.
fn context_snapshot(ctx: &ExtensionContext) -> Value {
    serde_json::json!({
        "is_idle": (ctx.is_idle)(),
        "has_pending_messages": (ctx.has_pending_messages)(),
        "system_prompt": (ctx.get_system_prompt)(),
    })
}

fn make_event_handler(shared: Arc<WasmInstance>, event_type: String) -> ExtensionHandler {
    Box::new(move |event, ctx| {
        let op = format!("event:{event_type}");
        let payload = serde_json::json!({
            "event": event,
            "context": context_snapshot(ctx),
        });
        let payload_bytes = serde_json::to_vec(&payload)
            .map_err(|e| format!("failed to encode event payload: {e}"))?;
        call_guest(&shared, &op, &payload_bytes)
    })
}

/// The one host-call door every wasm extension can knock on. Wave 2 wires
/// the six `ExtensionRuntime` actions; Wave 5 adds `abort`/`shutdown` (also
/// `ExtensionRuntime` slots, see this module's doc comment).
/// `ExtensionContext`'s three read-only accessors still travel as a payload
/// snapshot instead of a host-call op (see this module's doc comment).
/// Unknown ops still fail closed.
fn host_call(
    mut caller: Caller<'_, HostState>,
    op_ptr: i32,
    op_len: i32,
    payload_ptr: i32,
    payload_len: i32,
) -> i64 {
    let op = match read_string(&mut caller, op_ptr as u32, op_len as u32) {
        Ok(op) => op,
        Err(e) => {
            return write_json_response(&mut caller, &serde_json::json!({"ok": false, "error": e}))
        }
    };

    let payload_value: Value = if payload_len > 0 {
        match read_bytes(&mut caller, payload_ptr as u32, payload_len as u32) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or(Value::Null),
            Err(e) => {
                return write_json_response(
                    &mut caller,
                    &serde_json::json!({"ok": false, "error": e}),
                )
            }
        }
    } else {
        Value::Null
    };

    let result = match op.as_str() {
        "get_active_tools" => {
            let names = (caller.data().runtime.get_active_tools.lock().unwrap())();
            serde_json::json!({"ok": true, "value": names})
        }
        "get_all_tools" => {
            let names = (caller.data().runtime.get_all_tools.lock().unwrap())();
            serde_json::json!({"ok": true, "value": names})
        }
        "send_message" => {
            let message = payload_value.get("message").cloned().unwrap_or(Value::Null);
            let options = payload_value.get("options").cloned().unwrap_or(Value::Null);
            (caller.data().runtime.send_message.lock().unwrap())(message, options);
            serde_json::json!({"ok": true, "value": Value::Null})
        }
        "send_user_message" => {
            let content = payload_value
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let options = payload_value.get("options").cloned().unwrap_or(Value::Null);
            (caller.data().runtime.send_user_message.lock().unwrap())(content, options);
            serde_json::json!({"ok": true, "value": Value::Null})
        }
        "append_entry" => {
            let custom_type = payload_value
                .get("custom_type")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let data = payload_value.get("data").cloned().filter(|v| !v.is_null());
            (caller.data().runtime.append_entry.lock().unwrap())(custom_type, data);
            serde_json::json!({"ok": true, "value": Value::Null})
        }
        "abort" => {
            (caller.data().runtime.abort.lock().unwrap())();
            serde_json::json!({"ok": true, "value": Value::Null})
        }
        "shutdown" => {
            (caller.data().runtime.shutdown.lock().unwrap())();
            serde_json::json!({"ok": true, "value": Value::Null})
        }
        "set_active_tools" => {
            let tools: Vec<String> = payload_value
                .get("tools")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            (caller.data().runtime.set_active_tools.lock().unwrap())(tools);
            serde_json::json!({"ok": true, "value": Value::Null})
        }
        other => serde_json::json!({"ok": false, "error": format!("unknown host op: {other}")}),
    };

    write_json_response(&mut caller, &result)
}

/// Write a JSON value back into the CALLING guest instance's own memory, via
/// a reentrant call to its own `pi_alloc` export. Safe: wasmtime allows a
/// host import callback to call back into other exports of the same running
/// instance (not itself, and not concurrently).
fn write_json_response(caller: &mut Caller<'_, HostState>, value: &Value) -> i64 {
    let Ok(bytes) = serde_json::to_vec(value) else {
        return pack(0, 0);
    };
    let Some(alloc_extern) = caller.get_export("pi_alloc") else {
        return pack(0, 0);
    };
    let Some(alloc_func) = alloc_extern.into_func() else {
        return pack(0, 0);
    };
    let Ok(alloc) = alloc_func.typed::<i32, i32>(&caller) else {
        return pack(0, 0);
    };
    let Ok(ptr) = alloc.call(&mut *caller, bytes.len() as i32) else {
        return pack(0, 0);
    };
    if write_bytes(caller, ptr as u32, &bytes).is_err() {
        return pack(0, 0);
    }
    pack(ptr as u32, bytes.len() as u32)
}
