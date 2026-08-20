//! Bundled `plan-mode` extension — port of
//! `packages/coding-agent/examples/extensions/plan-mode/index.ts`.
//!
//! Read-only exploration mode for safe code analysis: when enabled, the
//! built-in write tools are disabled, bash is restricted to an allowlist of
//! read-only commands, and a `/plan` command / `ctrl+alt+p` shortcut toggles
//! it. The extension extracts numbered plan steps from a `Plan:` section and
//! tracks `[DONE:n]` progress during execution.
//!
//! ## State sharing
//!
//! Pi's handlers close over module-level `let` variables (`planModeEnabled`,
//! `executionMode`, `todoItems`, `toolsBeforePlanMode`). The Rust port shares
//! one [`Arc<Mutex<PlanModeStateMachine>>`] across all handler closures — the
//! same shape the Wave-4 `ExtensionHandler` (`Fn + Send + Sync`) requires.
//!
//! ## Action-method seams (Wave 6)
//!
//! Pi's extension calls `pi.getActiveTools()` / `pi.setActiveTools()` /
//! `pi.appendEntry()` / `pi.sendMessage()` and `ctx.ui.*` /
//! `ctx.sessionManager.getEntries()`. The Wave-4 `ExtensionApi`/`ExtensionContext`
//! are registration-only; action methods and the UI context are bound by
//! `pirust-coding-agent` in Wave 6. This module keeps those as explicit
//! no-op seams (`active_tools()`, `set_active_tools()`, `persist_state()`,
//! `update_status()`) so the port is line-for-line faithful and only the
//! seams change in Wave 6 — the state machine and dispatch logic stay
//! identical.

use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use crate::context::{ExtensionCommandContext, ExtensionContext, ExtensionMode};
use crate::events::ExtensionEvent;
use crate::loader::{load, InlineExtension};
use crate::plan_mode::{extract_todo_items, is_safe_command, mark_completed_steps, TodoItem};
use crate::registration::{ExtensionApi, ExtensionFlag, FlagType, FlagValue, RegisteredCommand};

/// Tools enabled in plan mode (index.ts:26).
const PLAN_MODE_TOOLS: &[&str] = &["read", "bash", "grep", "find", "ls", "questionnaire"];
/// Tools enabled in normal mode (index.ts:27).
const NORMAL_MODE_TOOLS: &[&str] = &["read", "bash", "edit", "write"];
/// Tools disabled in plan mode (index.ts:28).
const PLAN_MODE_DISABLED_TOOLS: &[&str] = &["edit", "write"];
/// Tools plan mode manages entirely (index.ts:29).
const PLAN_MANAGED_TOOLS: &[&str] = &[
    "read",
    "bash",
    "grep",
    "find",
    "ls",
    "questionnaire",
    "edit",
    "write",
];

/// `plan-mode` custom entry data persisted by `appendEntry` (index.ts:65-73).
#[derive(serde::Serialize, serde::Deserialize, Clone, PartialEq, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PlanModeState {
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub todos: Option<Vec<TodoItem>>,
    #[serde(default)]
    pub executing: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools_before_plan_mode: Option<Vec<String>>,
}

/// The mutable per-extension state (`planModeEnabled`, `executionMode`,
/// `todoItems`, `toolsBeforePlanMode` in index.ts).
#[derive(Default)]
pub struct PlanModeStateMachine {
    pub plan_mode_enabled: bool,
    pub execution_mode: bool,
    pub todo_items: Vec<TodoItem>,
    pub tools_before_plan_mode: Option<Vec<String>>,
}

impl PlanModeStateMachine {
    /// `uniqueToolNames` (index.ts:77-79) — dedupe preserving first occurrence.
    pub fn unique_tool_names(&self, tool_names: &[String]) -> Vec<String> {
        let mut seen = std::collections::HashSet::new();
        tool_names
            .iter()
            .filter(|t| seen.insert((*t).clone()))
            .cloned()
            .collect()
    }

    /// `getPlanModeTools` (index.ts:81-84).
    pub fn get_plan_mode_tools(&self, active_tool_names: &[String]) -> Vec<String> {
        self.unique_tool_names(
            &active_tool_names
                .iter()
                .filter(|name| !PLAN_MODE_DISABLED_TOOLS.contains(&name.as_str()))
                .cloned()
                .chain(PLAN_MODE_TOOLS.iter().map(|s| s.to_string()))
                .collect::<Vec<_>>(),
        )
    }

    /// `getNormalModeTools` (index.ts:86-89).
    pub fn get_normal_mode_tools(&self, active_tool_names: &[String]) -> Vec<String> {
        self.unique_tool_names(
            &NORMAL_MODE_TOOLS
                .iter()
                .map(|s| s.to_string())
                .chain(
                    active_tool_names
                        .iter()
                        .filter(|name| !PLAN_MANAGED_TOOLS.contains(&name.as_str()))
                        .cloned(),
                )
                .collect::<Vec<_>>(),
        )
    }
}

/// `pi.getActiveTools()` — seam. Wave 6 binds the real active-tool list from
/// the session runtime.
fn active_tools() -> Vec<String> {
    Vec::new()
}

/// `pi.setActiveTools(tools)` — seam. Wave 6 binds the real tool list.
fn set_active_tools(_tools: Vec<String>) {}

/// `pi.appendEntry("plan-mode", data)` — seam. Wave 6 binds the session's
/// appendEntry action.
fn persist_state(_state: &PlanModeStateMachine) {}

/// `updateStatus(ctx)` (index.ts:54-76) — footer status + todo widget. The
/// Wave-4 `ExtensionContext` has no `ui`; Wave 6 binds it.
fn update_status(_ctx: &ExtensionContext, _state: &PlanModeStateMachine) {}

/// `enablePlanModeTools` (index.ts:91-96).
fn enable_plan_mode_tools(state: &mut PlanModeStateMachine) {
    if state.tools_before_plan_mode.is_none() {
        state.tools_before_plan_mode = Some(active_tools());
    }
    let plan_tools =
        state.get_plan_mode_tools(state.tools_before_plan_mode.as_deref().unwrap_or(&[]));
    set_active_tools(plan_tools);
}

/// `restoreNormalModeTools` (index.ts:98-102).
fn restore_normal_mode_tools(state: &mut PlanModeStateMachine) {
    let current = active_tools();
    let restored = state
        .tools_before_plan_mode
        .take()
        .unwrap_or_else(|| state.get_normal_mode_tools(&current));
    set_active_tools(restored);
}

/// `togglePlanMode` (index.ts:105-114).
fn toggle_plan_mode(state: &mut PlanModeStateMachine, ctx: &ExtensionContext) {
    state.plan_mode_enabled = !state.plan_mode_enabled;
    state.execution_mode = false;
    state.todo_items.clear();

    if state.plan_mode_enabled {
        enable_plan_mode_tools(state);
    } else {
        restore_normal_mode_tools(state);
    }
    update_status(ctx, state);
    persist_state(state);
}

/// Build + register the plan-mode extension — the `planModeExtension(pi)`
/// factory (index.ts:48). `built_in_extensions()` wraps this as the
/// `plan-mode` inline extension.
pub fn plan_mode_extension() -> InlineExtension {
    InlineExtension::new(
        "plan-mode",
        Box::new(|api: &mut ExtensionApi| {
            api.register_flag(ExtensionFlag {
                name: "plan".into(),
                description: Some("Start in plan mode (read-only exploration)".into()),
                r#type: FlagType::Boolean,
                default: Some(FlagValue::Bool(false)),
                extension_path: api.extension.path.clone(),
            });

            let state = Arc::new(Mutex::new(PlanModeStateMachine::default()));

            api.register_command(
                "plan",
                RegisteredCommand {
                    name: "plan".into(),
                    description: Some("Toggle plan mode (read-only exploration)".into()),
                    get_argument_completions: None,
                    handler: Box::new({
                        let state = Arc::clone(&state);
                        move |_args, ctx| {
                            let mut s = state.lock().unwrap();
                            toggle_plan_mode(&mut s, &ctx.base);
                            Ok(())
                        }
                    }),
                },
            );

            api.register_command(
                "todos",
                RegisteredCommand {
                    name: "todos".into(),
                    description: Some("Show current plan todo list".into()),
                    get_argument_completions: None,
                    handler: Box::new({
                        let state = Arc::clone(&state);
                        move |_args, ctx| {
                            let s = state.lock().unwrap();
                            if s.todo_items.is_empty() {
                                // ctx.ui.notify("No todos. Create a plan first with /plan", "info")
                                return Ok(());
                            }
                            let list = s
                                .todo_items
                                .iter()
                                .enumerate()
                                .map(|(i, item)| {
                                    format!(
                                        "{}. {} {}",
                                        i + 1,
                                        if item.completed { "✓" } else { "○" },
                                        item.text
                                    )
                                })
                                .collect::<Vec<_>>()
                                .join("\n");
                            // ctx.ui.notify(format!("Plan Progress:\n{list}"), "info")
                            let _ = (ctx, list);
                            Ok(())
                        }
                    }),
                },
            );

            api.register_shortcut(
                "ctrl+alt+p",
                Box::new({
                    let state = Arc::clone(&state);
                    move |ctx| {
                        let mut s = state.lock().unwrap();
                        toggle_plan_mode(&mut s, ctx);
                        Ok(())
                    }
                }),
                Some("Toggle plan mode".into()),
            );

            // `tool_call` handler (index.ts:161-169): block destructive bash in plan mode.
            api.on("tool_call", Box::new({
            let state = Arc::clone(&state);
            move |event, _ctx| {
                let s = state.lock().unwrap();
                if !s.plan_mode_enabled {
                    return Ok(Value::Null);
                }
                let ExtensionEvent::ToolCall { tool_name, input, .. } = event else {
                    return Ok(Value::Null);
                };
                if tool_name != "bash" {
                    return Ok(Value::Null);
                }
                let command = input
                    .get("command")
                    .and_then(|c| c.as_str())
                    .unwrap_or("");
                if !is_safe_command(command) {
                    return Ok(json!({
                        "block": true,
                        "reason": format!(
                            "Plan mode: command blocked (not allowlisted). Use /plan to disable plan mode first.\nCommand: {command}"
                        ),
                    }));
                }
                Ok(Value::Null)
            }
        }));

            // `context` handler (index.ts:171-201): filter out stale plan-mode context.
            api.on(
                "context",
                Box::new({
                    let state = Arc::clone(&state);
                    move |event, _ctx| {
                        let s = state.lock().unwrap();
                        if s.plan_mode_enabled {
                            return Ok(Value::Null);
                        }
                        let ExtensionEvent::Context { messages } = event else {
                            return Ok(Value::Null);
                        };
                        let filtered = messages
                            .as_array()
                            .map(|arr| {
                                arr.iter()
                                    .filter(|m| {
                                        let custom_type =
                                            m.get("customType").and_then(|c| c.as_str());
                                        if custom_type == Some("plan-mode-context") {
                                            return false;
                                        }
                                        if m.get("role").and_then(|r| r.as_str()) != Some("user") {
                                            return true;
                                        }
                                        match m.get("content") {
                                            Some(Value::String(s)) => {
                                                !s.contains("[PLAN MODE ACTIVE]")
                                            }
                                            Some(Value::Array(blocks)) => !blocks.iter().any(|c| {
                                                c.get("type").and_then(|t| t.as_str())
                                                    == Some("text")
                                                    && c.get("text")
                                                        .and_then(|t| t.as_str())
                                                        .is_some_and(|t| {
                                                            t.contains("[PLAN MODE ACTIVE]")
                                                        })
                                            }),
                                            _ => true,
                                        }
                                    })
                                    .cloned()
                                    .collect::<Vec<_>>()
                            })
                            .unwrap_or_default();
                        Ok(json!({ "messages": filtered }))
                    }
                }),
            );

            // `before_agent_start` handler (index.ts:203-252): inject plan/execution context.
            api.on("before_agent_start", Box::new({
            let state = Arc::clone(&state);
            move |_event, _ctx| {
                let s = state.lock().unwrap();
                if s.plan_mode_enabled {
                    return Ok(json!({
                        "message": {
                            "customType": "plan-mode-context",
                            "content": "[PLAN MODE ACTIVE]\nYou are in plan mode - a read-only exploration mode for safe code analysis.\n\nRestrictions:\n- Built-in edit and write tools are disabled\n- Other currently active tools remain available\n- Bash is restricted to an allowlist of read-only commands\n\nAsk clarifying questions using the questionnaire tool.\nUse brave-search skill via bash for web research.\n\nCreate a detailed numbered plan under a \"Plan:\" header:\n\nPlan:\n1. First step description\n2. Second step description\n...\n\nDo NOT attempt to make changes - just describe what you would do.",
                            "display": false,
                        }
                    }));
                }
                if s.execution_mode && !s.todo_items.is_empty() {
                    let remaining = s
                        .todo_items
                        .iter()
                        .filter(|t| !t.completed)
                        .map(|t| format!("{}. {}", t.step, t.text))
                        .collect::<Vec<_>>()
                        .join("\n");
                    return Ok(json!({
                        "message": {
                            "customType": "plan-execution-context",
                            "content": format!("[EXECUTING PLAN - Full tool access enabled]\n\nRemaining steps:\n{remaining}\n\nExecute each step in order.\nAfter completing a step, include a [DONE:n] tag in your response."),
                            "display": false,
                        }
                    }));
                }
                Ok(Value::Null)
            }
        }));

            // `turn_end` handler (index.ts:254-262): track [DONE:n] progress.
            api.on(
                "turn_end",
                Box::new({
                    let state = Arc::clone(&state);
                    move |event, ctx| {
                        let mut s = state.lock().unwrap();
                        if !s.execution_mode || s.todo_items.is_empty() {
                            return Ok(Value::Null);
                        }
                        let ExtensionEvent::TurnEnd { message, .. } = event else {
                            return Ok(Value::Null);
                        };
                        if !is_assistant_message(message) {
                            return Ok(Value::Null);
                        }
                        let text = assistant_text(message);
                        if mark_completed_steps(&text, &mut s.todo_items) > 0 {
                            update_status(ctx, &s);
                        }
                        persist_state(&s);
                        Ok(Value::Null)
                    }
                }),
            );

            // `agent_end` handler (index.ts:264-330): plan extraction + next-action UI.
            api.on(
                "agent_end",
                Box::new({
                    let state = Arc::clone(&state);
                    move |event, ctx| {
                        let mut s = state.lock().unwrap();

                        // Execution-complete check (index.ts:266-279).
                        if s.execution_mode && !s.todo_items.is_empty() {
                            if s.todo_items.iter().all(|t| t.completed) {
                                let completed_list = s
                                    .todo_items
                                    .iter()
                                    .map(|t| format!("~~{}~~", t.text))
                                    .collect::<Vec<_>>()
                                    .join("\n");
                                // pi.sendMessage({customType:"plan-complete", ...},
                                //   {triggerTurn:false}) — no-op until Wave 6.
                                let _ = completed_list;
                                s.execution_mode = false;
                                s.todo_items.clear();
                                update_status(ctx, &s);
                                persist_state(&s);
                            }
                            return Ok(Value::Null);
                        }

                        // Plan extraction (index.ts:281-306).
                        if !s.plan_mode_enabled || !ctx.has_ui {
                            return Ok(Value::Null);
                        }
                        let ExtensionEvent::AgentEnd { messages } = event else {
                            return Ok(Value::Null);
                        };
                        let last_assistant = messages
                            .as_array()
                            .and_then(|arr| arr.iter().rev().find(|m| is_assistant_message(m)));
                        if let Some(last) = last_assistant {
                            let extracted = extract_todo_items(&assistant_text(last));
                            if !extracted.is_empty() {
                                s.todo_items = extracted;
                            }
                        }
                        if s.todo_items.is_empty() {
                            return Ok(Value::Null);
                        }
                        persist_state(&s);

                        // Show plan steps + next-action select (index.ts:301-330). The
                        // `ctx.ui.select` dialog is a no-op until Wave 6 (Pi's runtime
                        // also throws "not initialized" pre-bind); the plan-extraction
                        // and [DONE:n] tracking above are the oracle-verified core.
                        let _ = ctx;
                        Ok(Value::Null)
                    }
                }),
            );

            // `session_start` handler (index.ts:332-395): flag + persisted-state restore.
            let start_plan_flag = api.get_flag("plan");
            api.on(
                "session_start",
                Box::new({
                    let state = Arc::clone(&state);
                    move |_event, ctx| {
                        let mut s = state.lock().unwrap();
                        if start_plan_flag == Some(FlagValue::Bool(true)) {
                            s.plan_mode_enabled = true;
                        }
                        // ctx.sessionManager.getEntries() — no-op until Wave 6.
                        let _ = ctx;
                        if s.plan_mode_enabled {
                            enable_plan_mode_tools(&mut s);
                        }
                        update_status(ctx, &s);
                        Ok(Value::Null)
                    }
                }),
            );

            Ok(())
        }),
    )
}

/// `isAssistantMessage` (index.ts:36-38) — role + content array.
fn is_assistant_message(m: &Value) -> bool {
    m.get("role").and_then(|r| r.as_str()) == Some("assistant")
        && m.get("content").is_some_and(|c| c.is_array())
}

/// `getTextContent` (index.ts:40-44) — join text blocks with "\n".
pub fn assistant_text(message: &Value) -> String {
    message
        .get("content")
        .and_then(|c| c.as_array())
        .map(|blocks| {
            blocks
                .iter()
                .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
                .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

/// Load the plan-mode extension into an `Extension` (test seam mirroring
/// `tests/demo_extension.rs::load`).
pub fn load_plan_mode(cwd: &str) -> crate::registration::Extension {
    load(&plan_mode_extension(), cwd)
}

/// A runner with the plan-mode extension loaded (test seam).
pub fn plan_mode_runner() -> crate::runner::ExtensionRunner {
    crate::runner::ExtensionRunner::new(vec![load_plan_mode(".")], ".".into(), ExtensionMode::Tui)
}

/// Test `ExtensionContext` with `has_ui: true` (matches Pi's test harness).
pub fn tui_context() -> ExtensionContext {
    ExtensionContext {
        mode: ExtensionMode::Tui,
        has_ui: true,
        cwd: ".".into(),
        is_idle: Box::new(|| true),
        signal: None,
        abort: Box::new(|| {}),
        has_pending_messages: Box::new(|| false),
        shutdown: Box::new(|| {}),
        get_context_usage: Box::new(|| None),
        get_system_prompt: Box::new(String::new),
    }
}

/// Test `ExtensionCommandContext` wrapping [`tui_context`].
pub fn tui_command_context() -> ExtensionCommandContext {
    ExtensionCommandContext {
        base: tui_context(),
        wait_for_idle: Box::new(|| {}),
        reload: Box::new(|| {}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registers_flag_command_shortcut_and_handlers() {
        let e = load_plan_mode(".");
        assert!(e.flags.contains_key("plan"), "plan flag");
        assert!(e.commands.contains_key("plan"), "/plan command");
        assert!(e.commands.contains_key("todos"), "/todos command");
        assert!(
            e.shortcuts.contains_key("ctrl+alt+p"),
            "ctrl+alt+p shortcut"
        );
        for evt in [
            "tool_call",
            "context",
            "before_agent_start",
            "turn_end",
            "agent_end",
            "session_start",
        ] {
            assert!(e.handlers.contains_key(evt), "handler for {evt}");
        }
    }

    #[test]
    fn plan_flag_defaults_to_false() {
        let mut e = load_plan_mode(".");
        let api = ExtensionApi {
            extension: &mut e,
            cwd: ".".into(),
            assert_active: Box::new(|| {}),
        };
        assert_eq!(api.get_flag("plan"), Some(FlagValue::Bool(false)));
    }
}
