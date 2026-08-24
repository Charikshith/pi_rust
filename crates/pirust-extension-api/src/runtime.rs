//! Extension runtime action seams — port of `ExtensionRuntime`
//! (extensions/runner.ts:270-284) and `ExtensionActions`
//! (extensions/runner.ts:198-266).
//!
//! Pi's runner holds one shared `runtime` object that every extension API
//! references (`pi.getActiveTools()` → `this.runtime.getActiveTools()`).
//! `bindCore(actions, …)` copies the mode-provided action closures into that
//! shared object **in place** (`this.runtime.getActiveTools = actions.getActiveTools`,
//! runner.ts:326-332); extensions read through the shared object lazily at
//! call time, so a bind-after-load swap is visible to already-loaded
//! extensions. This port mirrors that exactly: each action is a mutable slot
//! (`Arc<Mutex<Box<dyn Fn…>>>`); [`ExtensionApi`] accessors hand out closures
//! that lock the slot when invoked, and [`ExtensionRunner::bind_runtime`]
//! swaps the slot contents.

use std::sync::{Arc, Mutex};

use serde_json::Value;

type SendMessageFn = Box<dyn Fn(Value, Value) + Send + Sync>;
type SendUserMessageFn = Box<dyn Fn(String, Value) + Send + Sync>;
type AppendEntryFn = Box<dyn Fn(String, Option<Value>) + Send + Sync>;
type GetActiveToolsFn = Box<dyn Fn() -> Vec<String> + Send + Sync>;
type GetAllToolsFn = Box<dyn Fn() -> Vec<String> + Send + Sync>;
type SetActiveToolsFn = Box<dyn Fn(Vec<String>) + Send + Sync>;
type AbortFn = Box<dyn Fn() + Send + Sync>;
type ShutdownFn = Box<dyn Fn() + Send + Sync>;

/// The mode-provided action closures copied into the shared runtime
/// (`ExtensionActions`, runner.ts:198-266). Every field is a mutable slot so
/// `bind_runtime` can swap it after load (Pi's in-place `bindCore`).
#[derive(Clone)]
pub struct ExtensionRuntime {
    /// `pi.sendMessage(message, options)` — send a custom message.
    pub send_message: Arc<Mutex<SendMessageFn>>,
    /// `pi.sendUserMessage(content, options)` — send a user message.
    pub send_user_message: Arc<Mutex<SendUserMessageFn>>,
    /// `pi.appendEntry(customType, data)` — persist an app-defined entry.
    pub append_entry: Arc<Mutex<AppendEntryFn>>,
    /// `pi.getActiveTools()` — names of the currently active tools.
    pub get_active_tools: Arc<Mutex<GetActiveToolsFn>>,
    /// `pi.getAllTools()` — names of all known tools.
    pub get_all_tools: Arc<Mutex<GetAllToolsFn>>,
    /// `pi.setActiveTools(toolNames)` — set the active tool list.
    pub set_active_tools: Arc<Mutex<SetActiveToolsFn>>,
    /// `ctx.abort()` (Wave 5) — abort the current agent operation. Lives
    /// here, not on a per-dispatch `ExtensionContext`, because this slot is
    /// `Arc`-shared and rebindable exactly like the five actions above —
    /// the same mechanism the wasm `pi_host_call` door needs (`HostState`
    /// already holds this whole struct).
    pub abort: Arc<Mutex<AbortFn>>,
    /// `ctx.shutdown()` (Wave 5) — gracefully shut down and exit.
    pub shutdown: Arc<Mutex<ShutdownFn>>,
}

impl ExtensionRuntime {
    /// All no-op defaults — the pre-`bindCore` state (runner.ts:286-304 has
    /// the same no-op defaults).
    pub fn noop() -> Self {
        Self {
            send_message: Arc::new(Mutex::new(Box::new(|_, _| {}))),
            send_user_message: Arc::new(Mutex::new(Box::new(|_, _| {}))),
            append_entry: Arc::new(Mutex::new(Box::new(|_, _| {}))),
            get_active_tools: Arc::new(Mutex::new(Box::new(Vec::new))),
            get_all_tools: Arc::new(Mutex::new(Box::new(Vec::new))),
            set_active_tools: Arc::new(Mutex::new(Box::new(|_| {}))),
            abort: Arc::new(Mutex::new(Box::new(|| {}))),
            shutdown: Arc::new(Mutex::new(Box::new(|| {}))),
        }
    }

    /// Replace every slot's contents — Pi's `bindCore` copy (`runner.ts:326-332`).
    /// The incoming `actions` runtime is consumed (its closures are moved into
    /// this runtime's slots), matching the copy-then-drop Pi performs.
    pub fn bind(&self, actions: ExtensionRuntime) {
        *self.send_message.lock().unwrap() = std::mem::replace(
            &mut *actions.send_message.lock().unwrap(),
            Box::new(|_, _| {}),
        );
        *self.send_user_message.lock().unwrap() = std::mem::replace(
            &mut *actions.send_user_message.lock().unwrap(),
            Box::new(|_, _| {}),
        );
        *self.append_entry.lock().unwrap() = std::mem::replace(
            &mut *actions.append_entry.lock().unwrap(),
            Box::new(|_, _| {}),
        );
        *self.get_active_tools.lock().unwrap() = std::mem::replace(
            &mut *actions.get_active_tools.lock().unwrap(),
            Box::new(Vec::new),
        );
        *self.get_all_tools.lock().unwrap() = std::mem::replace(
            &mut *actions.get_all_tools.lock().unwrap(),
            Box::new(Vec::new),
        );
        *self.set_active_tools.lock().unwrap() = std::mem::replace(
            &mut *actions.set_active_tools.lock().unwrap(),
            Box::new(|_| {}),
        );
        *self.abort.lock().unwrap() =
            std::mem::replace(&mut *actions.abort.lock().unwrap(), Box::new(|| {}));
        *self.shutdown.lock().unwrap() =
            std::mem::replace(&mut *actions.shutdown.lock().unwrap(), Box::new(|| {}));
    }
}
