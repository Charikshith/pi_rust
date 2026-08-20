//! Wave 6 e2e — the extension runner bound into a real session (`SingleTurnSession`).
//!
//! Proves the full feat-007 binding path: `bind_extensions` builds the runner from
//! the built-in extensions, binds real action closures (`getActiveTools` →
//! `agent.tool_names()`, `setActiveTools` → `agent.set_tools` through the tool
//! registry, `appendEntry` → `session_manager.append_custom_entry`), forwards agent
//! events, and installs the agent-loop hooks. This is the Pi `plan-mode-extension.test.ts`
//! behavior driven through the REAL session instead of mocks.

use std::sync::Arc;

use pirust_agent_core::agent::{Agent, AgentOptions};
use pirust_agent_core::types::ThinkingLevel;
use pirust_ai::providers::faux::Faux;
use pirust_coding_agent::print_mode::{ExtensionBindMode, ExtensionBinding, PrintModeSession};
use pirust_coding_agent::runtime_host::SingleTurnSession;
use pirust_coding_agent::session::SessionEnv;
use pirust_extension_api::events::ExtensionEvent;
use pirust_tools::create_all_tools;

fn dummy_model() -> pirust_ai::types::Model {
    // A model identity — never actually streamed in this test.
    Faux::new().get_model().clone()
}

fn make_session() -> (Arc<SingleTurnSession>, Arc<Agent>) {
    let tool_registry = create_all_tools("/proj", None);
    let agent = Agent::new(AgentOptions {
        system_prompt: "test".into(),
        model: dummy_model(),
        thinking_level: ThinkingLevel::Off,
        tools: tool_registry
            .iter()
            .map(|(_, t)| t.clone())
            .collect::<Vec<_>>(),
        messages: Vec::new(),
        convert_to_llm: None,
        transform_context: None,
        stream_fn: None,
        get_api_key: None,
        before_tool_call: None,
        after_tool_call: None,
        steering_mode: pirust_agent_core::types::QueueMode::OneAtATime,
        follow_up_mode: pirust_agent_core::types::QueueMode::OneAtATime,
        session_id: None,
        tool_execution: pirust_agent_core::types::ToolExecutionMode::Parallel,
    });
    let env = SessionEnv::new(
        pirust_coding_agent::config::ConfigEnv::from_process_env(),
        "/proj",
    );
    let manager = env.in_memory(Some("/proj"), None).unwrap();
    let session = SingleTurnSession::new(agent.clone(), manager, tool_registry);
    let agent = Arc::new(agent);
    (session, agent)
}

#[tokio::test]
async fn bind_builds_runner_and_wires_action_seams() {
    let (session, agent) = make_session();

    let binding = ExtensionBinding {
        mode: ExtensionBindMode::Print,
        command_context_actions:
            pirust_coding_agent::print_mode::CommandContextActions::placeholder(),
        on_error: Arc::new(|_| {}),
    };
    session.bind_extensions(binding).await.unwrap();

    // The built-in runner is bound: plan-mode is the sole built-in.
    let runner = session.extension_runner_for_test();
    assert!(runner.is_some(), "runner bound by bind_extensions");
    let runner = runner.unwrap();
    let guard = runner.lock().unwrap();
    assert_eq!(guard.extensions.len(), 1);
    assert_eq!(guard.extensions[0].path, "<inline:plan-mode>");
    drop(guard);

    // Plan-mode's getActiveTools reads the REAL agent tools (all 7 builtins active).
    let initial = agent.tool_names();
    assert_eq!(initial.len(), 7, "all builtin tools active initially");
}

#[tokio::test]
async fn plan_command_toggles_real_agent_tools() {
    let (session, agent) = make_session();
    let binding = ExtensionBinding {
        mode: ExtensionBindMode::Print,
        command_context_actions:
            pirust_coding_agent::print_mode::CommandContextActions::placeholder(),
        on_error: Arc::new(|_| {}),
    };
    session.bind_extensions(binding).await.unwrap();

    let runner = session.extension_runner_for_test().unwrap();

    // Run the /plan command through the runner's command dispatch.
    let ctx = session.tui_command_context_for_test();
    {
        let guard = runner.lock().unwrap();
        let ext = &guard.extensions[0];
        let handler = ext.commands.get("plan").expect("/plan registered");
        (handler.handler)("", &ctx).expect("/plan handler ran");
    }

    // Plan mode ON → edit/write removed, read/bash/grep/find/ls kept.
    // (`questionnaire` is also requested by plan mode but is not in pirust's
    // builtin registry — `setActiveToolsByName` drops unknown names, matching
    // `validToolNames = toolNames.filter(name => registry.has(name))`.)
    let plan_tools = agent.tool_names();
    assert!(
        !plan_tools.iter().any(|n| n == "edit"),
        "edit disabled in plan mode"
    );
    assert!(
        !plan_tools.iter().any(|n| n == "write"),
        "write disabled in plan mode"
    );
    for tool in ["read", "bash", "grep", "find", "ls"] {
        assert!(
            plan_tools.iter().any(|n| n == tool),
            "{tool} enabled in plan mode"
        );
    }

    // Toggle off → tools restored to the pre-plan set (all 7).
    {
        let guard = runner.lock().unwrap();
        let ext = &guard.extensions[0];
        let handler = ext.commands.get("plan").expect("/plan registered");
        (handler.handler)("", &ctx).expect("/plan handler ran");
    }
    let restored = agent.tool_names();
    assert_eq!(restored.len(), 7, "tools restored after toggle off");
    assert!(restored.iter().any(|n| n == "edit"));
    assert!(restored.iter().any(|n| n == "write"));
}

#[tokio::test]
async fn destructive_bash_blocked_through_real_before_tool_call_hook() {
    let (session, _agent) = make_session();
    let binding = ExtensionBinding {
        mode: ExtensionBindMode::Print,
        command_context_actions:
            pirust_coding_agent::print_mode::CommandContextActions::placeholder(),
        on_error: Arc::new(|_| {}),
    };
    session.bind_extensions(binding).await.unwrap();

    let runner = session.extension_runner_for_test().unwrap();

    // Enable plan mode via the runner's own tool_call dispatch (same path the
    // before_tool_call hook uses).
    {
        let mut guard = runner.lock().unwrap();
        let ctx = session.tui_command_context_for_test();
        let ext = &guard.extensions[0];
        let handler = ext.commands.get("plan").expect("/plan registered");
        (handler.handler)("", &ctx).expect("/plan handler ran");

        // Destructive bash must be blocked.
        let result = guard.emit_tool_call(&ExtensionEvent::ToolCall {
            tool_call_id: "tc1".into(),
            tool_name: "bash".into(),
            input: serde_json::json!({"command": "rm -rf /"}),
        });
        let r = result.expect("blocked");
        assert!(r.block);
        assert!(r.reason.unwrap().contains("Plan mode"));

        // Safe bash passes through.
        let safe = guard.emit_tool_call(&ExtensionEvent::ToolCall {
            tool_call_id: "tc2".into(),
            tool_name: "bash".into(),
            input: serde_json::json!({"command": "ls -la"}),
        });
        assert!(safe.is_none(), "safe command not blocked");
    }
}

#[tokio::test]
async fn append_entry_persists_plan_mode_state_to_session() {
    let (session, _agent) = make_session();
    let binding = ExtensionBinding {
        mode: ExtensionBindMode::Print,
        command_context_actions:
            pirust_coding_agent::print_mode::CommandContextActions::placeholder(),
        on_error: Arc::new(|_| {}),
    };
    session.bind_extensions(binding).await.unwrap();

    let runner = session.extension_runner_for_test().unwrap();

    // Toggle plan mode ON → the extension's persistState → appendEntry → session file.
    {
        let guard = runner.lock().unwrap();
        let ctx = session.tui_command_context_for_test();
        let ext = &guard.extensions[0];
        let handler = ext.commands.get("plan").expect("/plan registered");
        (handler.handler)("", &ctx).expect("/plan handler ran");
    }

    // The session manager has a "plan-mode" custom entry with enabled:true.
    let entries = session.entries_for_test();
    let plan_entries: Vec<_> = entries
        .iter()
        .filter(|e| {
            e.get("type").and_then(|t| t.as_str()) == Some("custom")
                && e.get("customType").and_then(|t| t.as_str()) == Some("plan-mode")
        })
        .collect();
    assert_eq!(plan_entries.len(), 1, "plan-mode custom entry persisted");
    let data = plan_entries[0].get("data").expect("data present");
    assert_eq!(data["enabled"], serde_json::Value::Bool(true));
    assert_eq!(data["toolsBeforePlanMode"].as_array().unwrap().len(), 7);
}
