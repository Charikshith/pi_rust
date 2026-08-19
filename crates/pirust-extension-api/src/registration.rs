//! Registration types + `ExtensionApi` — port of `ToolDefinition`,
//! `RegisteredCommand`, `ExtensionShortcut`, `ExtensionFlag`, `ExtensionAPI`
//! (extensions/types.ts).
//!
//! Pi's `ExtensionAPI` is a function-object with `on()`, `registerTool()`,
//! `registerCommand()`, `registerShortcut()`, `registerFlag()`, `getFlag()`,
//! and the action methods. The Rust port keeps the same shape: the
//! registration methods write into the `Extension` being built; action
//! methods delegate to a runtime (Wave 6 binds real implementations).

use serde_json::Value;

use crate::context::{ExtensionCommandContext, ExtensionContext, ExtensionHandler};

/// Tool executor signature — `ToolDefinition.execute` (types.ts:505).
pub type ToolExecutor = Box<dyn Fn(ToolCallParams) -> Result<Value, String> + Send + Sync>;
/// Command argument-completions hook — `getArgumentCompletions` (types.ts:1258).
pub type ArgumentCompletionsFn = Box<dyn Fn(&str) -> Option<Vec<AutocompleteItem>> + Send + Sync>;
/// Command handler — `RegisteredCommand.handler` (types.ts:1259).
pub type CommandHandler =
    Box<dyn Fn(&str, &ExtensionCommandContext) -> Result<(), String> + Send + Sync>;
/// Shortcut handler — `ExtensionShortcut.handler` (types.ts:1336).
pub type ShortcutHandler = Box<dyn Fn(&ExtensionContext) -> Result<(), String> + Send + Sync>;

/// `ToolDefinition` (types.ts:456) — LLM-callable tool registration.
pub struct ToolDefinition {
    /// Tool name (used in LLM tool calls).
    pub name: String,
    /// Human-readable label for UI.
    pub label: String,
    /// Description for LLM.
    pub description: String,
    /// Optional one-line snippet for the system prompt's Available tools section.
    pub prompt_snippet: Option<String>,
    /// Optional guideline bullets appended to the system prompt Guidelines.
    pub prompt_guidelines: Vec<String>,
    /// Execute the tool.
    pub execute: ToolExecutor,
}

/// Parameters passed to a tool's `execute`.
pub struct ToolCallParams<'a> {
    pub tool_call_id: &'a str,
    /// Validated JSON arguments.
    pub params: &'a Value,
    /// The extension context (cwd, ui, ...).
    pub ctx: &'a ExtensionContext,
}

/// `RegisteredCommand` (types.ts:1256).
pub struct RegisteredCommand {
    pub name: String,
    pub description: Option<String>,
    pub get_argument_completions: Option<ArgumentCompletionsFn>,
    pub handler: CommandHandler,
}

/// `AutocompleteItem` (pi-tui autocomplete.ts:219).
#[derive(Debug, Clone)]
pub struct AutocompleteItem {
    pub value: String,
    pub label: String,
    pub description: Option<String>,
}

/// `ExtensionShortcut` (types.ts:1339).
pub struct ExtensionShortcut {
    pub shortcut: String,
    pub description: Option<String>,
    pub handler: ShortcutHandler,
    pub extension_path: String,
}

/// `ExtensionFlag` (types.ts:1333).
pub struct ExtensionFlag {
    pub name: String,
    pub description: Option<String>,
    pub r#type: FlagType,
    pub default: Option<FlagValue>,
    pub extension_path: String,
}

/// Flag value type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlagType {
    Boolean,
    String,
}

/// Flag value.
#[derive(Debug, Clone, PartialEq)]
pub enum FlagValue {
    Bool(bool),
    Str(String),
}

/// The API passed to extension factory functions (`ExtensionAPI`,
/// types.ts:1277).
pub struct ExtensionApi<'a> {
    /// Registration state: where `on()` and `registerX()` write.
    pub extension: &'a mut Extension,
    /// Current working directory (for sourceInfo resolution).
    pub cwd: String,
    /// Stale-instance guard: throws when this extension is stale.
    pub assert_active: Box<dyn Fn() + Send + Sync>,
}

/// A loaded extension (`Extension`, types.ts:1618): all registered items.
pub struct Extension {
    pub path: String,
    pub resolved_path: String,
    pub hidden: bool,
    /// Event handlers keyed by event type string.
    pub handlers: std::collections::HashMap<String, Vec<ExtensionHandler>>,
    pub tools: std::collections::HashMap<String, RegisteredTool>,
    pub commands: std::collections::HashMap<String, RegisteredCommand>,
    pub flags: std::collections::HashMap<String, ExtensionFlag>,
    pub shortcuts: std::collections::HashMap<String, ExtensionShortcut>,
}

/// `RegisteredTool` (types.ts:1327).
pub struct RegisteredTool {
    pub definition: ToolDefinition,
    pub source_info: SourceInfo,
}

/// `SourceInfo` (source-info.ts) — where an extension came from.
#[derive(Debug, Clone)]
pub struct SourceInfo {
    pub kind: SourceKind,
    pub path: String,
}

/// Extension source kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    Extension,
    Prompt,
    Skill,
}

impl SourceInfo {
    pub fn inline(path: &str) -> Self {
        Self {
            kind: SourceKind::Extension,
            path: path.to_string(),
        }
    }
}

/// Registration methods (`createExtensionAPI`, loader.ts:235-360).
impl ExtensionApi<'_> {
    /// `api.on(event, handler)` — register an event handler for the named
    /// event type (the `ExtensionEvent::event_type()` discriminator).
    pub fn on(&mut self, event_type: &str, handler: ExtensionHandler) {
        (self.assert_active)();
        self.extension
            .handlers
            .entry(event_type.to_string())
            .or_default()
            .push(handler);
    }

    /// Register a tool the LLM can call.
    pub fn register_tool(&mut self, tool: ToolDefinition) {
        (self.assert_active)();
        self.extension.tools.insert(
            tool.name.clone(),
            RegisteredTool {
                definition: tool,
                source_info: SourceInfo::inline(&self.cwd),
            },
        );
    }

    /// Register a custom command.
    pub fn register_command(&mut self, name: &str, command: RegisteredCommand) {
        (self.assert_active)();
        self.extension.commands.insert(name.to_string(), command);
    }

    /// Register a keyboard shortcut.
    pub fn register_shortcut(
        &mut self,
        shortcut: &str,
        handler: ShortcutHandler,
        description: Option<String>,
    ) {
        (self.assert_active)();
        self.extension.shortcuts.insert(
            shortcut.to_string(),
            ExtensionShortcut {
                shortcut: shortcut.to_string(),
                description,
                handler,
                extension_path: self.extension.path.clone(),
            },
        );
    }

    /// Register a CLI flag.
    pub fn register_flag(&mut self, flag: ExtensionFlag) {
        (self.assert_active)();
        let name = flag.name.clone();
        self.extension.flags.insert(name.clone(), flag);
    }

    /// `api.getFlag(name)` — read a registered flag's current value.
    pub fn get_flag(&self, name: &str) -> Option<FlagValue> {
        (self.assert_active)();
        self.extension
            .flags
            .get(name)
            .and_then(|f| f.default.clone())
    }
}

/// `ExtensionEvent` needs to be importable by handlers.
pub use crate::events::ExtensionEvent as _ExtensionEvent;
