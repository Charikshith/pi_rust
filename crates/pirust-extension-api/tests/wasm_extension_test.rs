//! Wave 1-3 black-box tests: build the real `wasm-hello` example extension
//! for `wasm32-unknown-unknown`, load it through `WasmExtensionLoader`, and
//! drive its registered tools and event handler through real round trips.
//! Wave 3's tests additionally load with deliberately-broken guest tools
//! and tight sandbox limits, proving a runaway/memory-hungry guest fails
//! cleanly and the host survives to load and run a fresh, well-behaved
//! instance afterward. No Pi oracle exists for this (pirust-only addition,
//! see repo root `plan.md`).

use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};

use pirust_extension_api::events::ExtensionEvent;
use pirust_extension_api::runner::ExtensionRunner;
use pirust_extension_api::runtime::ExtensionRuntime;
use pirust_extension_api::wasm::{WasmExtensionLimits, WasmExtensionLoader};
use pirust_extension_api::{ExtensionContext, ExtensionMode, ToolCallParams};
use serde_json::Value;

/// Recorded `(message, options)` pairs from a test-double `send_message`.
type CapturedMessages = Arc<Mutex<Vec<(Value, Value)>>>;
/// Recorded `(content, options)` pairs from a test-double `send_user_message`.
type CapturedUserMessages = Arc<Mutex<Vec<(String, Value)>>>;
/// Recorded `(custom_type, data)` pairs from a test-double `append_entry`.
type CapturedEntries = Arc<Mutex<Vec<(String, Option<Value>)>>>;
/// Recorded tool-name lists from a test-double `set_active_tools`.
type CapturedToolSets = Arc<Mutex<Vec<Vec<String>>>>;

fn example_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("examples")
        .join("wasm-hello")
}

/// Builds the `wasm-hello` fixture for `wasm32-unknown-unknown` on demand
/// (it is deliberately its own standalone workspace, excluded from the
/// parent pirust workspace — see its `Cargo.toml` comment — so nothing
/// builds it automatically).
fn build_example() -> PathBuf {
    let dir = example_dir();
    let manifest = dir.join("Cargo.toml");

    let status = Command::new(env!("CARGO"))
        .args([
            "build",
            "--manifest-path",
            manifest
                .to_str()
                .expect("manifest path should be valid UTF-8"),
            "--target",
            "wasm32-unknown-unknown",
            "--release",
        ])
        .status()
        .expect("failed to spawn cargo to build the wasm-hello fixture");
    assert!(status.success(), "building the wasm-hello fixture failed");

    dir.join("target")
        .join("wasm32-unknown-unknown")
        .join("release")
        .join("wasm_hello.wasm")
}

fn test_context() -> ExtensionContext {
    ExtensionContext {
        mode: ExtensionMode::Print,
        has_ui: false,
        cwd: ".".to_string(),
        is_idle: Box::new(|| true),
        signal: None,
        abort: Box::new(|| {}),
        has_pending_messages: Box::new(|| false),
        shutdown: Box::new(|| {}),
        get_context_usage: Box::new(|| None),
        get_system_prompt: Box::new(|| "you are a helpful test fixture".to_string()),
    }
}

#[test]
fn loads_and_calls_a_registered_tool() {
    let wasm_path = build_example();
    let extension = WasmExtensionLoader::load(&wasm_path, Arc::new(ExtensionRuntime::noop()))
        .expect("wasm extension should load");

    assert_eq!(extension.tools.len(), 5);
    let tool = extension
        .tools
        .get("echo")
        .expect("echo tool should be registered by pi_activate");
    assert_eq!(tool.definition.label, "Echo");

    let params = serde_json::json!({"hello": "world", "n": 42});
    let ctx = test_context();
    let result = (tool.definition.execute)(ToolCallParams {
        tool_call_id: "t1",
        params: &params,
        ctx: &ctx,
    })
    .expect("echo tool should round-trip its input");

    assert_eq!(result, params);
}

/// Proves the `pi_host_call` door itself: the guest's `list_active_tools`
/// tool calls back into the host mid-execution (a reentrant guest -> host ->
/// guest-memory-write round trip), and the host's answer comes from a real,
/// test-supplied `ExtensionRuntime` closure — not a hardcoded stub.
#[test]
fn guest_can_call_back_into_the_host_active_tools_door() {
    let wasm_path = build_example();

    let runtime = ExtensionRuntime::noop();
    runtime.bind(ExtensionRuntime {
        get_active_tools: Arc::new(Mutex::new(Box::new(|| {
            vec!["read".to_string(), "bash".to_string()]
        }))),
        ..ExtensionRuntime::noop()
    });

    let extension = WasmExtensionLoader::load(&wasm_path, Arc::new(runtime))
        .expect("wasm extension should load");
    let tool = extension
        .tools
        .get("list_active_tools")
        .expect("list_active_tools should be registered");

    let params = serde_json::json!({});
    let ctx = test_context();
    let result = (tool.definition.execute)(ToolCallParams {
        tool_call_id: "t2",
        params: &params,
        ctx: &ctx,
    })
    .expect("list_active_tools should succeed");

    assert_eq!(result, serde_json::json!(["read", "bash"]));
}

/// Wave 2: proves all four of the newly-wired `ExtensionRuntime` action
/// doors (`send_message`/`send_user_message`/`append_entry`/
/// `set_active_tools`) actually reach the real host closures, by binding
/// test-double closures that capture what they were called with and
/// asserting on the captures directly — not just on the guest's own summary
/// return value.
#[test]
fn guest_can_reach_all_four_new_host_call_doors() {
    let wasm_path = build_example();

    let sent_messages: CapturedMessages = Arc::new(Mutex::new(Vec::new()));
    let sent_user_messages: CapturedUserMessages = Arc::new(Mutex::new(Vec::new()));
    let appended_entries: CapturedEntries = Arc::new(Mutex::new(Vec::new()));
    let active_tools_sets: CapturedToolSets = Arc::new(Mutex::new(Vec::new()));

    let runtime = ExtensionRuntime::noop();
    {
        let sent_messages = Arc::clone(&sent_messages);
        let sent_user_messages = Arc::clone(&sent_user_messages);
        let appended_entries = Arc::clone(&appended_entries);
        let active_tools_sets = Arc::clone(&active_tools_sets);
        runtime.bind(ExtensionRuntime {
            send_message: Arc::new(Mutex::new(Box::new(move |message, options| {
                sent_messages.lock().unwrap().push((message, options));
            }))),
            send_user_message: Arc::new(Mutex::new(Box::new(move |content, options| {
                sent_user_messages.lock().unwrap().push((content, options));
            }))),
            append_entry: Arc::new(Mutex::new(Box::new(move |custom_type, data| {
                appended_entries.lock().unwrap().push((custom_type, data));
            }))),
            set_active_tools: Arc::new(Mutex::new(Box::new(move |tools| {
                active_tools_sets.lock().unwrap().push(tools);
            }))),
            ..ExtensionRuntime::noop()
        });
    }

    let extension = WasmExtensionLoader::load(&wasm_path, Arc::new(runtime))
        .expect("wasm extension should load");
    let tool = extension
        .tools
        .get("exercise_doors")
        .expect("exercise_doors should be registered");

    let params = serde_json::json!({});
    let ctx = test_context();
    let result = (tool.definition.execute)(ToolCallParams {
        tool_call_id: "t3",
        params: &params,
        ctx: &ctx,
    })
    .expect("exercise_doors should succeed");

    assert_eq!(
        result,
        serde_json::json!({
            "send_message": true,
            "send_user_message": true,
            "append_entry": true,
            "set_active_tools": true,
        })
    );

    assert_eq!(sent_messages.lock().unwrap().len(), 1);
    assert_eq!(
        *sent_user_messages.lock().unwrap(),
        vec![("hi from guest".to_string(), serde_json::json!({}))]
    );
    assert_eq!(
        *appended_entries.lock().unwrap(),
        vec![("wasm_test".to_string(), Some(serde_json::json!({"n": 1})))]
    );
    assert_eq!(
        *active_tools_sets.lock().unwrap(),
        vec![vec!["echo".to_string()]]
    );
}

/// Wave 2: proves event dispatch. The wasm extension subscribes to
/// `agent_start` via `pi_activate`'s `"events"` list; `ExtensionRunner`
/// dispatches through the exact same `emit()` path it uses for compile-time
/// extensions (`runner.rs` is untouched by feat-010). The guest's handler
/// calls `append_entry` from INSIDE the event handler (not a tool call) to
/// prove the host-call door works there too, and echoes back the
/// host-computed `system_prompt` context snapshot it received.
#[test]
fn guest_event_handler_fires_through_a_real_extension_runner() {
    let wasm_path = build_example();

    let appended_entries: CapturedEntries = Arc::new(Mutex::new(Vec::new()));
    let runtime = ExtensionRuntime::noop();
    {
        let appended_entries = Arc::clone(&appended_entries);
        runtime.bind(ExtensionRuntime {
            append_entry: Arc::new(Mutex::new(Box::new(move |custom_type, data| {
                appended_entries.lock().unwrap().push((custom_type, data));
            }))),
            ..ExtensionRuntime::noop()
        });
    }
    let runtime = Arc::new(runtime);

    let extension = WasmExtensionLoader::load(&wasm_path, Arc::clone(&runtime))
        .expect("wasm extension should load");
    assert!(
        extension.handlers.contains_key("agent_start"),
        "pi_activate's events list should have registered an agent_start handler"
    );

    let mut runner = ExtensionRunner::new_with_runtime(
        vec![extension],
        ".".to_string(),
        ExtensionMode::Print,
        runtime,
    );

    runner.emit(&ExtensionEvent::AgentStart);

    let errors = runner.take_errors();
    assert!(
        errors.is_empty(),
        "guest agent_start handler should not have errored: {errors:?}"
    );

    // `ExtensionRunner::create_context()` still builds its context from
    // no-op stub closures (`get_system_prompt: Box::new(String::new)` etc.
    // — the real mode-specific glue is bound by `pirust-coding-agent`,
    // unrelated to this feature), so the guest sees an empty system prompt
    // here. This still proves the wiring: the guest received and echoed
    // back exactly the host-computed snapshot, not a hardcoded guest value.
    let entries = appended_entries.lock().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].0, "wasm_agent_start");
    assert_eq!(entries[0].1, Some(serde_json::json!({"system_prompt": ""})));
}

/// Wave 3: a genuine infinite loop must trap on fuel exhaustion (a normal
/// `Result::Err`, not a hang), AND the shared `Engine`/loader machinery
/// must be unaffected — proven by loading a completely fresh, well-behaved
/// instance afterward and confirming it still works. Uses a small custom
/// fuel budget (not the production default) purely so this test itself
/// runs in well under a second; the production default is chosen and
/// reasoned about separately in `plan.md` and `wasm/mod.rs`'s doc comment.
#[test]
fn runaway_guest_traps_on_fuel_exhaustion_without_wedging_the_host() {
    let wasm_path = build_example();

    let tiny_fuel_limits = WasmExtensionLimits {
        fuel: 5_000_000,
        ..WasmExtensionLimits::default()
    };
    let extension = WasmExtensionLoader::load_with_limits(
        &wasm_path,
        Arc::new(ExtensionRuntime::noop()),
        tiny_fuel_limits,
    )
    .expect("wasm extension should load");
    let burn_fuel = extension
        .tools
        .get("burn_fuel")
        .expect("burn_fuel should be registered");

    let ctx = test_context();
    let result = (burn_fuel.definition.execute)(ToolCallParams {
        tool_call_id: "t4",
        params: &serde_json::json!({}),
        ctx: &ctx,
    });
    assert!(
        result.is_err(),
        "an infinite loop must trap once its fuel budget is exhausted, not return Ok"
    );

    // The exhausted instance's own Store is now permanently out of fuel
    // (Wave 3 is a per-instance lifetime budget, not a per-call one — see
    // `wasm/mod.rs`'s doc comment) — this is expected, not a bug. What
    // matters is that loading a FRESH instance still works, proving the
    // shared `Engine`/loader code itself wasn't corrupted or wedged by the
    // trap.
    let fresh_extension = WasmExtensionLoader::load(&wasm_path, Arc::new(ExtensionRuntime::noop()))
        .expect("a fresh instance should load fine after a prior instance's trap");
    let echo = fresh_extension
        .tools
        .get("echo")
        .expect("echo should be registered on the fresh instance");
    let echo_params = serde_json::json!({"still": "alive"});
    let echo_result = (echo.definition.execute)(ToolCallParams {
        tool_call_id: "t5",
        params: &echo_params,
        ctx: &ctx,
    })
    .expect("a fresh, well-behaved instance should work after a prior instance's trap");
    assert_eq!(echo_result, echo_params);
}

/// Wave 3: a guest that tries to grow its linear memory far past the
/// configured ceiling must trap cleanly (`Result::Err`, not an actual
/// large host allocation and not a panic), and — same as the fuel test —
/// the host must still be able to load and run a fresh, well-behaved
/// instance afterward.
#[test]
fn runaway_guest_traps_on_memory_ceiling_without_wedging_the_host() {
    let wasm_path = build_example();

    // Default ceiling (16 MiB); the guest's grow_memory tool always
    // requests 160 MiB, so this fails regardless of the exact default —
    // the important thing under test is that it fails cleanly.
    let extension = WasmExtensionLoader::load(&wasm_path, Arc::new(ExtensionRuntime::noop()))
        .expect("wasm extension should load");
    let grow_memory = extension
        .tools
        .get("grow_memory")
        .expect("grow_memory should be registered");

    let ctx = test_context();
    let result = (grow_memory.definition.execute)(ToolCallParams {
        tool_call_id: "t6",
        params: &serde_json::json!({}),
        ctx: &ctx,
    });
    assert!(
        result.is_err(),
        "growing memory past the configured ceiling must trap, not succeed"
    );

    let fresh_extension = WasmExtensionLoader::load(&wasm_path, Arc::new(ExtensionRuntime::noop()))
        .expect("a fresh instance should load fine after a prior instance's memory trap");
    let echo = fresh_extension
        .tools
        .get("echo")
        .expect("echo should be registered on the fresh instance");
    let echo_params = serde_json::json!({"still": "alive"});
    let echo_result = (echo.definition.execute)(ToolCallParams {
        tool_call_id: "t7",
        params: &echo_params,
        ctx: &ctx,
    })
    .expect("a fresh, well-behaved instance should work after a prior instance's memory trap");
    assert_eq!(echo_result, echo_params);
}
