//! Extension runner — port of `ExtensionRunner` (extensions/runner.ts).
//!
//! Dispatch semantics are ported exactly:
//! - Generic `emit()`: iterate extensions in load order, then handlers per
//!   event type; errors are captured into `emit_error`, never thrown; the
//!   session-before events short-circuit on `cancel`.
//! - `emit_tool_call`: first non-undefined result wins; `block` returns
//!   immediately.
//! - `emit_user_bash`: first non-undefined result wins (wrapped in try/catch
//!   like Pi's).
//! - `emit_input`: transforms chain; `handled` short-circuits; returns
//!   `Continue` when nothing changed.
//! - `emit_message_end`: chains replacement messages, enforcing the same-role
//!   rule with an emitted error on violation.
//! - `emit_context`: clones and chains `messages`.
//! - `emit_before_provider_request` / `emit_before_provider_headers` /
//!   `emit_after_provider_response`: passthrough chain.
//!
//! Handlers are synchronous in this port (`ExtensionHandler` returns
//! `Result<Value, String>`); Pi's are `async`. Wave 6 binds the real
//! asynchronous agent loop; until then the runner dispatches synchronously
//! and treats any handler-returned error as the Pi `emitError` path.

use serde_json::Value;

use crate::context::{
    BeforeAgentStartEventResult, ContextEventResult, ExtensionContext, ExtensionMode,
    InputEventResult, MessageEndEventResult, ResourcesDiscoverResult, ToolCallEventResult,
    ToolResultEventResult, UserBashEventResult,
};
use crate::events::ExtensionEvent;
use crate::registration::Extension;

/// `ExtensionError` (types.ts:1638).
#[derive(Debug, Clone)]
pub struct ExtensionError {
    pub extension_path: String,
    pub event: String,
    pub error: String,
    pub stack: Option<String>,
}

/// `ExtensionRunner` (runner.ts:175).
pub struct ExtensionRunner {
    pub extensions: Vec<Extension>,
    /// cwd for `createContext()`.
    pub cwd: String,
    /// Current run mode.
    pub mode: ExtensionMode,
    errors: Vec<ExtensionError>,
}

/// Combined result from all before_agent_start handlers (runner.ts:110).
pub struct BeforeAgentStartCombinedResult {
    pub messages: Vec<Value>,
    pub system_prompt: Option<String>,
}

impl ExtensionRunner {
    pub fn new(extensions: Vec<Extension>, cwd: String, mode: ExtensionMode) -> Self {
        Self {
            extensions,
            cwd,
            mode,
            errors: Vec::new(),
        }
    }

    pub fn has_handlers(&self, event_type: &str) -> bool {
        self.extensions
            .iter()
            .any(|ext| ext.handlers.get(event_type).is_some_and(|h| !h.is_empty()))
    }

    /// `runner.emitError(...)` — record a handler error.
    pub fn emit_error(&mut self, error: ExtensionError) {
        self.errors.push(error);
    }

    /// Consume recorded errors (e.g. for tests / logging).
    pub fn take_errors(&mut self) -> Vec<ExtensionError> {
        std::mem::take(&mut self.errors)
    }

    /// `emitError` captured errors from the last dispatch.
    pub fn errors(&self) -> &[ExtensionError] {
        &self.errors
    }

    fn create_context(&self) -> ExtensionContext {
        ExtensionContext {
            mode: self.mode,
            has_ui: matches!(self.mode, ExtensionMode::Tui | ExtensionMode::Rpc),
            cwd: self.cwd.clone(),
            is_idle: Box::new(|| true),
            signal: None,
            abort: Box::new(|| {}),
            has_pending_messages: Box::new(|| false),
            shutdown: Box::new(|| {}),
            get_context_usage: Box::new(|| None),
            get_system_prompt: Box::new(String::new),
        }
    }

    /// `emit()` (runner.ts:801) — generic event dispatch. Returns the
    /// short-circuit result for session-before events, else None.
    pub fn emit(&mut self, event: &ExtensionEvent) -> Option<Value> {
        let event_type = event.event_type();
        let ctx = self.create_context();
        let mut result: Option<Value> = None;

        for ext in &self.extensions {
            let Some(handlers) = ext.handlers.get(event_type) else {
                continue;
            };
            if handlers.is_empty() {
                continue;
            }
            for handler in handlers {
                match handler(event, &ctx) {
                    Ok(value) => {
                        if is_session_before(event) {
                            if value.is_object() && value.get("cancel") == Some(&Value::Bool(true))
                            {
                                return Some(value);
                            }
                            result = Some(value);
                        }
                    }
                    Err(err) => {
                        self.errors.push(ExtensionError {
                            extension_path: ext.path.clone(),
                            event: event_type.to_string(),
                            error: err,
                            stack: None,
                        });
                    }
                }
            }
        }

        result
    }

    /// `emitToolCall` (runner.ts:932).
    pub fn emit_tool_call(&mut self, event: &ExtensionEvent) -> Option<ToolCallEventResult> {
        let ctx = self.create_context();
        let mut result: Option<ToolCallEventResult> = None;

        for ext in &self.extensions {
            let Some(handlers) = ext.handlers.get("tool_call") else {
                continue;
            };
            for handler in handlers {
                match handler(event, &ctx) {
                    Ok(value) => {
                        if value.is_null() {
                            continue;
                        }
                        let parsed: ToolCallEventResult =
                            serde_json::from_value(value).unwrap_or_default();
                        if parsed.block {
                            return Some(parsed);
                        }
                        result = Some(parsed);
                    }
                    Err(err) => {
                        self.errors.push(ExtensionError {
                            extension_path: ext.path.clone(),
                            event: "tool_call".to_string(),
                            error: err,
                            stack: None,
                        });
                    }
                }
            }
        }

        result
    }

    /// `emitUserBash` (runner.ts:955).
    pub fn emit_user_bash(&mut self, event: &ExtensionEvent) -> Option<UserBashEventResult> {
        let ctx = self.create_context();
        for ext in &self.extensions {
            let Some(handlers) = ext.handlers.get("user_bash") else {
                continue;
            };
            for handler in handlers {
                match handler(event, &ctx) {
                    Ok(value) => {
                        if !value.is_null() {
                            let parsed: UserBashEventResult =
                                serde_json::from_value(value).unwrap_or_default();
                            return Some(parsed);
                        }
                    }
                    Err(err) => {
                        self.errors.push(ExtensionError {
                            extension_path: ext.path.clone(),
                            event: "user_bash".to_string(),
                            error: err,
                            stack: None,
                        });
                    }
                }
            }
        }
        None
    }

    /// `emitContext` (runner.ts:984) — clones + chains messages.
    pub fn emit_context(&mut self, messages: &Value) -> Value {
        let ctx = self.create_context();
        let mut current_messages = messages.clone();
        for ext in &self.extensions {
            let Some(handlers) = ext.handlers.get("context") else {
                continue;
            };
            for handler in handlers {
                let event = ExtensionEvent::Context {
                    messages: current_messages.clone(),
                };
                match handler(&event, &ctx) {
                    Ok(value) => {
                        if let Ok(result) = serde_json::from_value::<ContextEventResult>(value) {
                            if let Some(m) = result.messages {
                                current_messages = m;
                            }
                        }
                    }
                    Err(err) => {
                        self.errors.push(ExtensionError {
                            extension_path: ext.path.clone(),
                            event: "context".to_string(),
                            error: err,
                            stack: None,
                        });
                    }
                }
            }
        }
        current_messages
    }

    /// `emitBeforeProviderRequest` (runner.ts:1016).
    pub fn emit_before_provider_request(&mut self, payload: &Value) -> Value {
        let ctx = self.create_context();
        let mut current_payload = payload.clone();
        for ext in &self.extensions {
            let Some(handlers) = ext.handlers.get("before_provider_request") else {
                continue;
            };
            for handler in handlers {
                let event = ExtensionEvent::BeforeProviderRequest {
                    payload: current_payload.clone(),
                };
                match handler(&event, &ctx) {
                    Ok(value) => {
                        if !value.is_null() {
                            current_payload = value;
                        }
                    }
                    Err(err) => {
                        self.errors.push(ExtensionError {
                            extension_path: ext.path.clone(),
                            event: "before_provider_request".to_string(),
                            error: err,
                            stack: None,
                        });
                    }
                }
            }
        }
        current_payload
    }

    /// `emitBeforeProviderHeaders` (runner.ts:1050) — handlers mutate
    /// `headers` in place; return value ignored.
    pub fn emit_before_provider_headers(&mut self, headers: &mut Value) {
        let ctx = self.create_context();
        for ext in &self.extensions {
            let Some(handlers) = ext.handlers.get("before_provider_headers") else {
                continue;
            };
            for handler in handlers {
                let event = ExtensionEvent::BeforeProviderHeaders {
                    headers: headers.clone(),
                };
                match handler(&event, &ctx) {
                    Ok(_) => {
                        // Pi mutates headers in place; the returned event
                        // carries them back. We copy them back here.
                        if let ExtensionEvent::BeforeProviderHeaders { headers: h } = &event {
                            *headers = h.clone();
                        }
                    }
                    Err(err) => {
                        self.errors.push(ExtensionError {
                            extension_path: ext.path.clone(),
                            event: "before_provider_headers".to_string(),
                            error: err,
                            stack: None,
                        });
                    }
                }
            }
        }
    }

    /// `emitMessageEnd` (runner.ts:835).
    pub fn emit_message_end(&mut self, message: &Value) -> Option<Value> {
        let ctx = self.create_context();
        let mut current_message = message.clone();
        let mut modified = false;

        for ext in &self.extensions {
            let Some(handlers) = ext.handlers.get("message_end") else {
                continue;
            };
            for handler in handlers {
                let event = ExtensionEvent::MessageEnd {
                    message: current_message.clone(),
                };
                match handler(&event, &ctx) {
                    Ok(value) => {
                        if value.is_null() {
                            continue;
                        }
                        let result: MessageEndEventResult =
                            serde_json::from_value(value).unwrap_or_default();
                        let Some(new_message) = result.message else {
                            continue;
                        };
                        if new_message.get("role") != current_message.get("role") {
                            self.errors.push(ExtensionError {
                                extension_path: ext.path.clone(),
                                event: "message_end".to_string(),
                                error:
                                    "message_end handlers must return a message with the same role"
                                        .to_string(),
                                stack: None,
                            });
                            continue;
                        }
                        current_message = new_message;
                        modified = true;
                    }
                    Err(err) => {
                        self.errors.push(ExtensionError {
                            extension_path: ext.path.clone(),
                            event: "message_end".to_string(),
                            error: err,
                            stack: None,
                        });
                    }
                }
            }
        }
        if modified {
            Some(current_message)
        } else {
            None
        }
    }

    /// `emitToolResult` (runner.ts:877).
    pub fn emit_tool_result(&mut self, event: &ExtensionEvent) -> Option<ToolResultEventResult> {
        let ctx = self.create_context();
        let mut result: Option<ToolResultEventResult> = None;

        for ext in &self.extensions {
            let Some(handlers) = ext.handlers.get("tool_result") else {
                continue;
            };
            for handler in handlers {
                match handler(event, &ctx) {
                    Ok(value) => {
                        if value.is_null() {
                            continue;
                        }
                        let parsed: ToolResultEventResult =
                            serde_json::from_value(value).unwrap_or_default();
                        result = Some(parsed);
                    }
                    Err(err) => {
                        self.errors.push(ExtensionError {
                            extension_path: ext.path.clone(),
                            event: "tool_result".to_string(),
                            error: err,
                            stack: None,
                        });
                    }
                }
            }
        }
        result
    }

    /// `emitBeforeAgentStart` (runner.ts:1081) — chained systemPrompt.
    pub fn emit_before_agent_start(
        &mut self,
        prompt: &str,
        system_prompt: &str,
    ) -> Option<BeforeAgentStartCombinedResult> {
        let ctx = self.create_context();
        let mut current_system_prompt = system_prompt.to_string();
        let mut messages: Vec<Value> = Vec::new();
        let mut system_prompt_modified = false;

        for ext in &self.extensions {
            let Some(handlers) = ext.handlers.get("before_agent_start") else {
                continue;
            };
            for handler in handlers {
                let event = ExtensionEvent::BeforeAgentStart {
                    prompt: prompt.to_string(),
                    system_prompt: current_system_prompt.clone(),
                };
                match handler(&event, &ctx) {
                    Ok(value) => {
                        if value.is_null() {
                            continue;
                        }
                        let result: BeforeAgentStartEventResult =
                            serde_json::from_value(value).unwrap_or_default();
                        if let Some(m) = result.message {
                            messages.push(m);
                        }
                        if let Some(sp) = result.system_prompt {
                            current_system_prompt = sp;
                            system_prompt_modified = true;
                        }
                    }
                    Err(err) => {
                        self.errors.push(ExtensionError {
                            extension_path: ext.path.clone(),
                            event: "before_agent_start".to_string(),
                            error: err,
                            stack: None,
                        });
                    }
                }
            }
        }

        if !messages.is_empty() || system_prompt_modified {
            Some(BeforeAgentStartCombinedResult {
                messages,
                system_prompt: if system_prompt_modified {
                    Some(current_system_prompt)
                } else {
                    None
                },
            })
        } else {
            None
        }
    }

    /// `emitResourcesDiscover` (runner.ts:1147).
    pub fn emit_resources_discover(
        &mut self,
        cwd: &str,
        reason: crate::events::ResourceDiscoverReason,
    ) -> ResourcesDiscoverResult {
        let ctx = self.create_context();
        let mut out = ResourcesDiscoverResult::default();
        for ext in &self.extensions {
            let Some(handlers) = ext.handlers.get("resources_discover") else {
                continue;
            };
            for handler in handlers {
                let event = ExtensionEvent::ResourcesDiscover {
                    cwd: cwd.to_string(),
                    reason,
                };
                match handler(&event, &ctx) {
                    Ok(value) => {
                        if let Ok(r) = serde_json::from_value::<ResourcesDiscoverResult>(value) {
                            out.skill_paths.extend(r.skill_paths);
                            out.prompt_paths.extend(r.prompt_paths);
                            out.theme_paths.extend(r.theme_paths);
                        }
                    }
                    Err(err) => {
                        self.errors.push(ExtensionError {
                            extension_path: ext.path.clone(),
                            event: "resources_discover".to_string(),
                            error: err,
                            stack: None,
                        });
                    }
                }
            }
        }
        out
    }

    /// `emitInput` (runner.ts:1196) — transforms chain, "handled"
    /// short-circuits.
    pub fn emit_input(
        &mut self,
        text: &str,
        source: crate::events::InputSource,
    ) -> InputEventResult {
        let ctx = self.create_context();
        let mut current_text = text.to_string();

        for ext in &self.extensions {
            let Some(handlers) = ext.handlers.get("input") else {
                continue;
            };
            for handler in handlers {
                let event = ExtensionEvent::Input {
                    text: current_text.clone(),
                    source,
                };
                match handler(&event, &ctx) {
                    Ok(value) => {
                        let Ok(result) = serde_json::from_value::<InputEventResult>(value) else {
                            continue;
                        };
                        match result {
                            InputEventResult::Handled => return InputEventResult::Handled,
                            InputEventResult::Transform { text } => current_text = text,
                            InputEventResult::Continue => {}
                        }
                    }
                    Err(err) => {
                        self.errors.push(ExtensionError {
                            extension_path: ext.path.clone(),
                            event: "input".to_string(),
                            error: err,
                            stack: None,
                        });
                    }
                }
            }
        }
        if current_text != text {
            InputEventResult::Transform { text: current_text }
        } else {
            InputEventResult::Continue
        }
    }
}

fn is_session_before(event: &ExtensionEvent) -> bool {
    matches!(
        event,
        ExtensionEvent::SessionBeforeSwitch { .. }
            | ExtensionEvent::SessionBeforeFork { .. }
            | ExtensionEvent::SessionBeforeCompact { .. }
            | ExtensionEvent::SessionBeforeTree { .. }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::ExtensionHandler;
    use std::collections::HashMap;

    fn runner_with(handler: ExtensionHandler) -> ExtensionRunner {
        let mut ext = Extension {
            path: "test.ts".into(),
            resolved_path: "/abs/test.ts".into(),
            hidden: false,
            handlers: HashMap::new(),
            tools: HashMap::new(),
            commands: HashMap::new(),
            flags: HashMap::new(),
            shortcuts: HashMap::new(),
        };
        ext.handlers
            .entry("agent_start".into())
            .or_default()
            .push(handler);
        ExtensionRunner::new(vec![ext], ".".into(), ExtensionMode::Print)
    }

    #[test]
    fn emit_dispatchs_to_all_handlers() {
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = std::sync::Arc::clone(&calls);
        let handler: ExtensionHandler = Box::new(move |_e, _c| {
            counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(Value::Null)
        });
        let mut runner = runner_with(handler);
        runner.emit(&ExtensionEvent::AgentStart);
        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 1);
    }

    #[test]
    fn emit_captures_handler_errors() {
        let handler: ExtensionHandler = Box::new(|_e, _c| Err("boom".to_string()));
        let mut runner = runner_with(handler);
        runner.emit(&ExtensionEvent::AgentStart);
        let errors = runner.take_errors();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].error, "boom");
        assert_eq!(errors[0].event, "agent_start");
    }

    #[test]
    fn session_before_cancel_short_circuits() {
        let cancel: ExtensionHandler = Box::new(|_e, _c| Ok(serde_json::json!({"cancel": true})));
        let mut ext = Extension {
            path: "a.ts".into(),
            resolved_path: "/a.ts".into(),
            hidden: false,
            handlers: HashMap::new(),
            tools: HashMap::new(),
            commands: HashMap::new(),
            flags: HashMap::new(),
            shortcuts: HashMap::new(),
        };
        ext.handlers
            .entry("session_before_switch".into())
            .or_default()
            .push(cancel);
        let mut runner = ExtensionRunner::new(vec![ext], ".".into(), ExtensionMode::Print);
        let result = runner.emit(&ExtensionEvent::SessionBeforeSwitch {
            reason: crate::events::SessionSwitchReason::New,
            target_session_file: None,
        });
        assert_eq!(result.unwrap()["cancel"], true);
    }

    #[test]
    fn emit_input_transform_chains() {
        let transform: ExtensionHandler =
            Box::new(|_e, _c| Ok(serde_json::json!({"action": "transform", "text": "changed"})));
        let mut ext = Extension {
            path: "a.ts".into(),
            resolved_path: "/a.ts".into(),
            hidden: false,
            handlers: HashMap::new(),
            tools: HashMap::new(),
            commands: HashMap::new(),
            flags: HashMap::new(),
            shortcuts: HashMap::new(),
        };
        ext.handlers
            .entry("input".into())
            .or_default()
            .push(transform);
        let mut runner = ExtensionRunner::new(vec![ext], ".".into(), ExtensionMode::Print);
        let result = runner.emit_input("orig", crate::events::InputSource::Interactive);
        match result {
            InputEventResult::Transform { text } => assert_eq!(text, "changed"),
            other => panic!("expected transform, got {other:?}"),
        }
    }
}
