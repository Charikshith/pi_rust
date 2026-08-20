//! Integration smoke — a demo bundled extension exercising the full
//! `ExtensionApi` → `ExtensionRunner` lifecycle (feat-007 Wave 4 verify
//! step: "a demo bundled extension").

use std::collections::HashMap;

use pirust_extension_api::{
    Extension, ExtensionApi, ExtensionContext, ExtensionMode, ExtensionRunner, InlineExtension,
    RegisteredCommand, ToolCallParams, ToolDefinition,
};

/// Build a demo extension that registers a tool, a command, and an
/// `agent_start` handler, mirroring Pi's `InlineExtension` factory shape.
fn demo_extension() -> InlineExtension {
    InlineExtension::new(
        "demo",
        Box::new(|api: &mut ExtensionApi| {
            // Register a tool.
            api.register_tool(ToolDefinition {
                name: "demo_echo".into(),
                label: "Demo Echo".into(),
                description: "Echo back the input".into(),
                prompt_snippet: None,
                prompt_guidelines: Vec::new(),
                execute: Box::new(|params: ToolCallParams| {
                    let input = params
                        .params
                        .get("text")
                        .and_then(|t| t.as_str())
                        .unwrap_or("");
                    Ok(serde_json::json!({
                        "content": [{"type": "text", "text": format!("echo: {input}")}]
                    }))
                }),
            });

            // Register a command.
            api.register_command(
                "demo",
                RegisteredCommand {
                    name: "demo".into(),
                    description: Some("Demo command".into()),
                    get_argument_completions: None,
                    handler: Box::new(|_args, _ctx| Ok(())),
                },
            );

            // Subscribe to agent_start.
            api.on(
                "agent_start",
                Box::new(|_event, _ctx| Ok(serde_json::Value::Null)),
            );

            Ok(())
        }),
    )
}

/// Build an `Extension` from a factory, running it with a scratch API
/// (mirrors the loader's "load factory → extension object" step).
fn load(factory: &InlineExtension, cwd: &str) -> Extension {
    let mut ext = Extension {
        path: format!("<inline:{}>", factory.name),
        resolved_path: format!("<inline:{}>", factory.name),
        hidden: factory.hidden,
        handlers: HashMap::new(),
        tools: HashMap::new(),
        commands: HashMap::new(),
        flags: HashMap::new(),
        shortcuts: HashMap::new(),
    };
    let mut api = ExtensionApi {
        extension: &mut ext,
        cwd: cwd.to_string(),
        assert_active: Box::new(|| {}),
        runtime: std::sync::Arc::new(pirust_extension_api::runtime::ExtensionRuntime::noop()),
    };
    (factory.factory)(&mut api).expect("factory ran");
    ext
}

#[test]
fn demo_extension_registers_and_dispatches() {
    let ext = load(&demo_extension(), ".");
    assert!(ext.tools.contains_key("demo_echo"), "tool registered");
    assert!(ext.commands.contains_key("demo"), "command registered");
    assert!(
        ext.handlers.contains_key("agent_start"),
        "agent_start handler registered"
    );

    let mut runner = ExtensionRunner::new(vec![ext], ".".into(), ExtensionMode::Print);
    runner.emit(&pirust_extension_api::ExtensionEvent::AgentStart);
    assert!(runner.take_errors().is_empty(), "no dispatch errors");
}

#[test]
fn demo_extension_tool_executes() {
    let ext = load(&demo_extension(), ".");
    let tool = ext.tools.get("demo_echo").expect("tool");
    let ctx = ExtensionContext {
        mode: ExtensionMode::Print,
        has_ui: false,
        cwd: ".".into(),
        is_idle: Box::new(|| true),
        signal: None,
        abort: Box::new(|| {}),
        has_pending_messages: Box::new(|| false),
        shutdown: Box::new(|| {}),
        get_context_usage: Box::new(|| None),
        get_system_prompt: Box::new(String::new),
    };
    let result = (tool.definition.execute)(ToolCallParams {
        tool_call_id: "call_1",
        params: &serde_json::json!({"text": "hello"}),
        ctx: &ctx,
    })
    .expect("execute");
    assert_eq!(result["content"][0]["text"], "echo: hello");
}
