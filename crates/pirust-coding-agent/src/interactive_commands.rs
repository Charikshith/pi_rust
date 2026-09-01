//! Pure logic for the interactive TUI's slash commands (port target:
//! `interactive-mode.ts`'s `dispatchCommand` + `BUILTIN_SLASH_COMMANDS`, mirrored here as
//! `crates/pirust-coding-agent/src/interactive_mode.rs:880-1035` (`dispatch_command`) and
//! `:1590-1687` (`slash_command_available` / `BUILTIN_SLASH_COMMANDS`)).
//!
//! `interactive_mode.rs` registers 27 commands in `BUILTIN_SLASH_COMMANDS` but
//! `slash_command_available` only admits 9 of them; the other 18 answer a bare "not
//! available in this session". This module supplies the missing logic as free functions
//! that take whatever capability they need as a parameter and return a [`CommandOutcome`]
//! rather than calling `self.show_notice`/`self.show_error` directly — `dispatch_command`
//! stays the only place that touches `self`.
//!
//! NOTE on provenance: unlike most of this crate, the functions below are not confirmed
//! byte-for-byte ports of a specific `pi-coding-agent` TypeScript line — that source was
//! not available to check line numbers against, so no `slash-commands.ts:N`-style
//! citation is invented here. Every citation in this file instead points at the concrete
//! pirust source (this repo) the behavior was designed against; where a command has no
//! reachable seam at all, [`unavailable_reason`] says exactly what is missing instead of
//! guessing at upstream behavior.
//!
//! # Reachability ledger (18 previously-dead commands)
//!
//! - Bucket A (fully self-contained): `export`, `copy`, `share` (`gh gist create`,
//!   shelled out to — see [`run_gist_share`] — rather than an HTTP dependency).
//! - Bucket B (pure logic here, needs a caller-supplied capability): `settings`,
//!   `scoped-models`, `changelog`, `trust`, `login`, `logout`, `reload`.
//! - Bucket C (no reachable seam today — [`unavailable_reason`] explains why): none
//!   remain.
//!   (`name` and `compact` were promoted out of this bucket earlier; `new`, `fork`,
//!   `clone`, `import` and `tree` were promoted together, once `PrintModeSession` grew
//!   `start_new_session`/`clone_session`/`fork_from`/`switch_to_session_file`
//!   (`print_mode.rs`) and `TuiRuntimeInfo` grew `branch_entries` — see the comments
//!   above their `unavailable_reason` match arms, or lack thereof, and
//!   `interactive_mode.rs`'s `dispatch_command`/`run_new_session`/`run_clone_session`/
//!   `run_fork`/`run_import`/`open_branch_picker`, which call those methods directly on
//!   `self.session`, exactly like `/name` and `/compact` already did — never through
//!   `CommandContextActions`, whatever this file's now-removed `unavailable_reason` arms
//!   for those five used to claim. `share` and `restart` were the last two: `share`
//!   turned out not to need the HTTP client the original arm assumed — shelling out to
//!   the already-installed-on-many-machines `gh` CLI needs no Rust dependency at all —
//!   and `restart` turned out not to need a session-level seam, because it was never a
//!   session operation: `std::process::Command::new(current_exe).spawn()` is a process
//!   operation, gated only on running it after the TUI's normal shutdown has already
//!   restored the terminal (`interactive_mode.rs::run_restart`,
//!   `main.rs::run_interactive_mode`).)

use std::collections::BTreeMap;
use std::io::{BufWriter, Write as _};
use std::path::{Path, PathBuf};

use pirust_agent_core::harness::messages::{bash_execution_to_text, AgentMessage};
use pirust_ai::types::{AssistantContent, Message, UserContent, UserMessageContent};

use crate::auth::{AuthStorage, AuthStorageBackend, Credential};
use crate::settings::SettingsManager;

// =============================================================================
// Command table
// =============================================================================

/// Grouping used by [`command_help_lines`] — mirrors the rough sections Pi's `/help`
/// output uses (general, session control, model selection, import/export, auth,
/// settings).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandCategory {
    General,
    Session,
    Model,
    Storage,
    Auth,
    Settings,
}

impl CommandCategory {
    /// The heading printed above this category's commands in [`command_help_lines`].
    pub const fn label(self) -> &'static str {
        match self {
            CommandCategory::General => "General",
            CommandCategory::Session => "Session",
            CommandCategory::Model => "Model",
            CommandCategory::Storage => "Import / Export",
            CommandCategory::Auth => "Auth",
            CommandCategory::Settings => "Settings",
        }
    }
}

/// One row of the slash-command table. Every field is `&'static str`/`Copy`, so the whole
/// table lives in read-only static memory with zero heap allocation and zero runtime
/// construction cost — see [`COMMANDS`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandSpec {
    /// The command name, without the leading `/` (matches `BUILTIN_SLASH_COMMANDS`'s
    /// first tuple element in `interactive_mode.rs`).
    pub name: &'static str,
    pub description: &'static str,
    pub argument_hint: Option<&'static str>,
    pub category: CommandCategory,
    /// `true` when pure logic for this command exists (either already wired into
    /// `interactive_mode.rs`'s `dispatch_command`, or newly provided by this module and
    /// waiting to be wired). `false` marks a genuine bucket-C gap — see
    /// [`unavailable_reason`]. This is a static fact about the *logic*, not a live
    /// runtime check; it does not by itself mean `slash_command_available` currently
    /// returns `true` for this name.
    pub reachable: bool,
}

/// Superset of `interactive_mode.rs`'s `BUILTIN_SLASH_COMMANDS` (27 entries there,
/// mirrored 1:1 here plus the `reachable`/`category` facts that table doesn't carry).
/// Intentionally does not replace that constant — `interactive_mode.rs` stays the
/// registration point; this is a read-only description of the same command set for
/// tooling (help text, availability messages) to consume.
pub const COMMANDS: &[CommandSpec] = &[
    CommandSpec {
        name: "help",
        description: "List available commands",
        argument_hint: None,
        category: CommandCategory::General,
        reachable: true,
    },
    CommandSpec {
        name: "hotkeys",
        description: "Show keyboard shortcuts",
        argument_hint: None,
        category: CommandCategory::General,
        reachable: true,
    },
    CommandSpec {
        name: "quit",
        description: "Exit pirust",
        argument_hint: None,
        category: CommandCategory::General,
        reachable: true,
    },
    CommandSpec {
        name: "changelog",
        description: "Show recent changelog entries",
        argument_hint: None,
        category: CommandCategory::General,
        reachable: true,
    },
    CommandSpec {
        name: "session",
        description: "Show session info",
        argument_hint: None,
        category: CommandCategory::Session,
        reachable: true,
    },
    CommandSpec {
        name: "name",
        description: "Rename the current session",
        argument_hint: Some("<name>"),
        category: CommandCategory::Session,
        // Promoted from bucket C: `PrintModeSession::set_session_name` now
        // reaches `SessionManager::append_session_info`.
        reachable: true,
    },
    CommandSpec {
        name: "new",
        description: "Start a new session",
        argument_hint: None,
        category: CommandCategory::Session,
        // Promoted from bucket C: `PrintModeSession::start_new_session`
        // (print_mode.rs:993), wired by `interactive_mode.rs::run_new_session`.
        reachable: true,
    },
    CommandSpec {
        name: "restart",
        description: "Restart the current session",
        argument_hint: None,
        category: CommandCategory::Session,
        // Promoted from bucket C: `std::process::Command::new(current_exe).spawn()`
        // (std alone — no `CommandExt::exec`, which is Unix-only) re-execs the process,
        // wired by `interactive_mode.rs::run_restart` + `main.rs::run_interactive_mode`.
        reachable: true,
    },
    CommandSpec {
        name: "fork",
        description: "Fork the session at this point",
        argument_hint: None,
        category: CommandCategory::Session,
        // Promoted from bucket C: `PrintModeSession::fork_from` (print_mode.rs:1052),
        // wired by `interactive_mode.rs::run_fork` (requires an entry id — see
        // `unavailable_reason`'s removed arm and `run_fork`'s doc comment).
        reachable: true,
    },
    CommandSpec {
        name: "clone",
        description: "Clone the session into a new one",
        argument_hint: None,
        category: CommandCategory::Session,
        // Promoted from bucket C: `PrintModeSession::clone_session` (print_mode.rs:1022),
        // wired by `interactive_mode.rs::run_clone_session`.
        reachable: true,
    },
    CommandSpec {
        name: "tree",
        description: "Browse the session's branch tree",
        argument_hint: None,
        category: CommandCategory::Session,
        // Promoted from bucket C: `TuiRuntimeInfo::branch_entries` (print_mode.rs) feeds
        // `interactive_pickers::BranchPicker`, wired by
        // `interactive_mode.rs::open_branch_picker`.
        reachable: true,
    },
    CommandSpec {
        name: "resume",
        description: "Resume a previous session",
        argument_hint: Some("[session-id]"),
        category: CommandCategory::Session,
        reachable: true,
    },
    CommandSpec {
        name: "compact",
        description: "Compact the conversation history",
        argument_hint: None,
        category: CommandCategory::Session,
        reachable: true,
    },
    CommandSpec {
        name: "reload",
        description: "Reload the session from disk",
        argument_hint: None,
        category: CommandCategory::Session,
        reachable: true,
    },
    CommandSpec {
        name: "reload-extensions",
        description: "Reload WASM extensions",
        argument_hint: None,
        category: CommandCategory::Session,
        reachable: true,
    },
    CommandSpec {
        name: "model",
        description: "Switch the active model",
        argument_hint: Some("<name>"),
        category: CommandCategory::Model,
        reachable: true,
    },
    CommandSpec {
        name: "models",
        description: "List available models",
        argument_hint: None,
        category: CommandCategory::Model,
        reachable: true,
    },
    CommandSpec {
        name: "scoped-models",
        description: "Toggle a model in the enabled-models scope",
        argument_hint: Some("<provider/model>"),
        category: CommandCategory::Model,
        reachable: true,
    },
    CommandSpec {
        name: "refresh-model-list",
        description: "Refresh the cached model list",
        argument_hint: None,
        category: CommandCategory::Model,
        reachable: true,
    },
    CommandSpec {
        name: "export",
        description: "Export the session to .jsonl or .html",
        argument_hint: Some("[path]"),
        category: CommandCategory::Storage,
        reachable: true,
    },
    CommandSpec {
        name: "import",
        description: "Import a session file",
        argument_hint: Some("<path>"),
        category: CommandCategory::Storage,
        // Promoted from bucket C: `PrintModeSession::switch_to_session_file`
        // (print_mode.rs:1086), wired by `interactive_mode.rs::run_import` (requires a
        // path — see `unavailable_reason`'s removed arm and `run_import`'s doc comment).
        reachable: true,
    },
    CommandSpec {
        name: "share",
        description: "Share the session as a hosted link",
        argument_hint: Some("confirm"),
        category: CommandCategory::Storage,
        // Promoted from bucket C: shelling out to `gh gist create` (see
        // `run_gist_share`) needs no HTTP dependency — wired by
        // `interactive_mode.rs::run_share`, gated behind an explicit `/share confirm`
        // (see `share_confirmation_notice`'s doc comment for why).
        reachable: true,
    },
    CommandSpec {
        name: "copy",
        description: "Copy the last assistant message to the clipboard",
        argument_hint: None,
        category: CommandCategory::Storage,
        reachable: true,
    },
    CommandSpec {
        name: "login",
        description: "Save an API key for a provider",
        argument_hint: Some("<provider> <api-key>"),
        category: CommandCategory::Auth,
        reachable: true,
    },
    CommandSpec {
        name: "logout",
        description: "Remove a provider's saved credentials",
        argument_hint: Some("<provider>"),
        category: CommandCategory::Auth,
        reachable: true,
    },
    CommandSpec {
        name: "trust",
        description: "Set whether this project is trusted",
        argument_hint: Some("<allow|deny>"),
        category: CommandCategory::Settings,
        reachable: true,
    },
    CommandSpec {
        name: "settings",
        description: "Show current settings",
        argument_hint: None,
        category: CommandCategory::Settings,
        reachable: true,
    },
];

/// Look up a command by name.
///
/// Chosen as a linear scan over [`COMMANDS`] rather than a `match`: the table is the
/// single source of truth for a command's name, description, argument hint, category and
/// reachability, all five of which `command_help_lines` also needs together. A `match`
/// would force those same five facts to be duplicated (or split across a match and a
/// separate description table), which is exactly the kind of drift that let 18 commands
/// go dead silently in the first place. `COMMANDS` has 27 entries — scanning it is a
/// handful of `&str` comparisons (a few nanoseconds), and `dispatch_command` runs once per
/// submitted line, not in any hot loop, so there is no measurable cost to linear-scanning
/// a `&'static` slice instead of hand-maintaining a parallel `match`.
pub fn command_spec(name: &str) -> Option<&'static CommandSpec> {
    COMMANDS.iter().find(|spec| spec.name == name)
}

/// Column pirust pads command names/hints to before the `—` separator in
/// [`command_help_lines`]. Chosen to fit every current name+hint pair except
/// `scoped-models <provider/model>`, which simply gets one space instead of aligning —
/// a cosmetic, not correctness, tradeoff.
const NAME_COL_WIDTH: usize = 28;

/// Render the full command list, grouped by [`CommandCategory`], marking any command for
/// which `available(name)` returns `false`.
///
/// Builds into a single pre-sized `String` via `push_str`, never a `Vec<String>` + `join`
/// — the output size is bounded tightly by `COMMANDS.len()`, so one allocation covers the
/// whole render.
pub fn command_help_lines(available: &dyn Fn(&str) -> bool) -> String {
    const CATEGORIES: [CommandCategory; 6] = [
        CommandCategory::General,
        CommandCategory::Session,
        CommandCategory::Model,
        CommandCategory::Storage,
        CommandCategory::Auth,
        CommandCategory::Settings,
    ];

    let mut out = String::with_capacity(COMMANDS.len() * 72 + CATEGORIES.len() * 16);
    for category in CATEGORIES {
        if !COMMANDS.iter().any(|spec| spec.category == category) {
            continue;
        }
        out.push_str(category.label());
        out.push('\n');
        for spec in COMMANDS.iter().filter(|spec| spec.category == category) {
            out.push_str("  /");
            out.push_str(spec.name);
            let mut used = 1 + spec.name.len();
            if let Some(hint) = spec.argument_hint {
                out.push(' ');
                out.push_str(hint);
                used += 1 + hint.len();
            }
            while used < NAME_COL_WIDTH {
                out.push(' ');
                used += 1;
            }
            out.push_str(" \u{2014} ");
            out.push_str(spec.description);
            if !available(spec.name) {
                // Exactly the phrasing the autocomplete dropdown uses
                // (`interactive_mode.rs`'s `set_autocomplete_provider` block),
                // so the same fact does not read as two different facts
                // depending on where the user meets it.
                out.push_str("  (unavailable in this session)");
            }
            out.push('\n');
        }
        out.push('\n');
    }
    if out.ends_with('\n') {
        out.pop();
    }
    out
}

// =============================================================================
// CommandOutcome
// =============================================================================

/// What running a command produced, decoupled from how `dispatch_command` shows it.
///
/// Extends the requested shape with one variant, [`CommandOutcome::CopyToClipboard`]: the
/// OSC 52 escape sequence a clipboard copy needs must reach the raw terminal stream
/// unmodified (no wrapping, no truncation, no re-encoding), which is a different contract
/// than `show_notice`/`show_error`'s diff-based, wrapped text compositor in
/// `interactive_mode.rs`. Folding a clipboard payload into `Notice` would risk the
/// compositor mangling the escape sequence; a dedicated variant makes the caller's
/// "write this raw to stdout, don't touch it" obligation explicit at the type level.
#[derive(Debug, Clone, PartialEq)]
pub enum CommandOutcome {
    Notice(String),
    Error(String),
    Quit,
    OpenModelPicker,
    OpenSessionPicker,
    OpenSettings,
    ToggleDebug,
    /// Raw bytes (an OSC 52 sequence) to write directly to stdout, bypassing the notice
    /// compositor. See [`copy_last_message`].
    CopyToClipboard(String),
}

// =============================================================================
// `/copy` — OSC 52 clipboard (bucket A)
// =============================================================================

/// RFC 4648 base64 alphabet (standard, padded) — the encoding OSC 52 expects.
const BASE64_TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Hand-rolled base64 (no `base64` crate — none is in `Cargo.lock`, see the crate's
/// `Cargo.toml`; this module may not add a dependency).
///
/// The output length is exactly `ceil(n/3)*4`, computed once up front so
/// `String::with_capacity` allocates exactly once; every `push` after that is into
/// pre-reserved capacity, so there is no per-chunk (re)allocation.
fn base64_encode(input: &[u8]) -> String {
    let out_len = input.len().div_ceil(3) * 4;
    let mut out = String::with_capacity(out_len);
    let mut chunks = input.chunks_exact(3);
    for chunk in &mut chunks {
        let n = ((chunk[0] as u32) << 16) | ((chunk[1] as u32) << 8) | (chunk[2] as u32);
        out.push(BASE64_TABLE[((n >> 18) & 0x3f) as usize] as char);
        out.push(BASE64_TABLE[((n >> 12) & 0x3f) as usize] as char);
        out.push(BASE64_TABLE[((n >> 6) & 0x3f) as usize] as char);
        out.push(BASE64_TABLE[(n & 0x3f) as usize] as char);
    }
    match chunks.remainder() {
        [a] => {
            let n = (*a as u32) << 16;
            out.push(BASE64_TABLE[((n >> 18) & 0x3f) as usize] as char);
            out.push(BASE64_TABLE[((n >> 12) & 0x3f) as usize] as char);
            out.push_str("==");
        }
        [a, b] => {
            let n = ((*a as u32) << 16) | ((*b as u32) << 8);
            out.push(BASE64_TABLE[((n >> 18) & 0x3f) as usize] as char);
            out.push(BASE64_TABLE[((n >> 12) & 0x3f) as usize] as char);
            out.push(BASE64_TABLE[((n >> 6) & 0x3f) as usize] as char);
            out.push('=');
        }
        _ => {}
    }
    out
}

/// Wrap base64 payload in an OSC 52 "set clipboard" sequence
/// (`\x1b]52;c;<base64>\x07`) — the terminal-level clipboard escape most modern
/// terminals (incl. Windows Terminal, iTerm2, kitty, WezTerm) honor without any OS
/// clipboard API, so no dependency is needed to implement `/copy`.
fn osc52_copy_sequence(text: &str) -> String {
    let payload = base64_encode(text.as_bytes());
    let mut out = String::with_capacity(payload.len() + 8);
    out.push_str("\u{1b}]52;c;");
    out.push_str(&payload);
    out.push('\u{07}');
    out
}

/// The last assistant message's text, joined the same way
/// `print_mode.rs::write_text_mode_result` (`:1227-1233`) renders it for stdout: each
/// `AssistantContent::Text` block gets its own `\n` in block order (so a block already
/// ending in `\n` gets a second one), then the single trailing newline is trimmed since
/// this value is a clipboard payload, not a stdout stream.
pub fn last_assistant_text(messages: &[AgentMessage]) -> Option<String> {
    for message in messages.iter().rev() {
        if let AgentMessage::Llm(Message::Assistant(assistant)) = message {
            let mut text = String::new();
            for content in &assistant.content {
                if let AssistantContent::Text(block) = content {
                    text.push_str(&block.text);
                    text.push('\n');
                }
            }
            if text.is_empty() {
                return None;
            }
            text.pop();
            return Some(text);
        }
    }
    None
}

/// `/copy` (bucket A) — copy the last assistant message via OSC 52. Self-contained: no
/// capability beyond the message list is needed.
pub fn copy_last_message(messages: &[AgentMessage]) -> CommandOutcome {
    match last_assistant_text(messages) {
        Some(text) => CommandOutcome::CopyToClipboard(osc52_copy_sequence(&text)),
        None => CommandOutcome::Error(
            "Nothing to copy yet — the assistant hasn't sent a text reply.".to_string(),
        ),
    }
}

// =============================================================================
// `/export` — .jsonl / .html (bucket A)
// =============================================================================

/// Minimal HTML entity escaping for text placed inside `<pre>` — the five characters
/// that are unsafe there (`&`, `<`, `>`, `"`, `'`).
fn html_escape(input: &str) -> String {
    let mut out = String::with_capacity(input.len() + input.len() / 8);
    for ch in input.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            other => out.push(other),
        }
    }
    out
}

/// A role label plus rendered text for one [`AgentMessage`], used by [`export_html`].
/// Reuses [`bash_execution_to_text`] for the bash-execution variant so the HTML export
/// renders bash output identically to how it would appear in the LLM context.
fn message_role_and_text(message: &AgentMessage) -> (&'static str, String) {
    match message {
        AgentMessage::Llm(Message::User(user)) => {
            ("user", user_message_content_text(&user.content))
        }
        AgentMessage::Llm(Message::Assistant(assistant)) => {
            let mut text = String::new();
            for content in &assistant.content {
                if let AssistantContent::Text(block) = content {
                    text.push_str(&block.text);
                    text.push('\n');
                }
            }
            if text.ends_with('\n') {
                text.pop();
            }
            ("assistant", text)
        }
        AgentMessage::Llm(Message::ToolResult(result)) => {
            let mut text = String::new();
            for content in &result.content {
                if let UserContent::Text(block) = content {
                    text.push_str(&block.text);
                    text.push('\n');
                }
            }
            if text.ends_with('\n') {
                text.pop();
            }
            ("tool-result", text)
        }
        AgentMessage::BashExecution(exec) => ("bash", bash_execution_to_text(exec)),
        AgentMessage::Custom(custom) => ("custom", user_message_content_text(&custom.content)),
        AgentMessage::BranchSummary(summary) => ("branch-summary", summary.summary.clone()),
        AgentMessage::CompactionSummary(summary) => ("compaction-summary", summary.summary.clone()),
    }
}

/// Flatten `UserMessageContent`'s bare-string/blocks union into plain text, joining text
/// blocks with `\n` and skipping images (an export cannot losslessly render an inline
/// image as `<pre>` text).
fn user_message_content_text(content: &UserMessageContent) -> String {
    match content {
        UserMessageContent::Text(text) => text.clone(),
        UserMessageContent::Blocks(blocks) => {
            let mut out = String::new();
            for block in blocks {
                if let UserContent::Text(text) = block {
                    out.push_str(&text.text);
                    out.push('\n');
                }
            }
            if out.ends_with('\n') {
                out.pop();
            }
            out
        }
    }
}

/// `/export <path>.jsonl` — one `AgentMessage` per line, byte-identical to how a session
/// transcript is already stored on disk (each `AgentMessage` round-trips through
/// `serde_json` elsewhere in this crate).
///
/// Builds into one `String::with_capacity` sized off `messages.len()` — no intermediate
/// `Vec<String>` — since the final size only needs to be roughly right (`String` grows
/// past it without a full reallocation strategy change) and the up-front reserve avoids
/// the small-size regrowth churn a `Vec<String>` + `join("\n")` would otherwise pay for
/// every line.
pub fn export_jsonl(messages: &[AgentMessage]) -> String {
    let mut out = String::with_capacity(messages.len() * 256 + 16);
    for message in messages {
        if let Ok(line) = serde_json::to_string(message) {
            out.push_str(&line);
            out.push('\n');
        }
    }
    out
}

/// `/export <path>.html` — a minimal, dependency-free static HTML transcript.
pub fn export_html(messages: &[AgentMessage], session_id: Option<&str>) -> String {
    let mut out = String::with_capacity(messages.len() * 512 + 512);
    out.push_str(
        "<!doctype html>\n<html><head><meta charset=\"utf-8\"><title>pirust session export",
    );
    if let Some(id) = session_id {
        out.push_str(" \u{2014} ");
        out.push_str(&html_escape(id));
    }
    out.push_str("</title></head><body>\n");
    for message in messages {
        let (role, text) = message_role_and_text(message);
        out.push_str("<section class=\"message ");
        out.push_str(role);
        out.push_str("\"><h3>");
        out.push_str(role);
        out.push_str("</h3><pre>");
        out.push_str(&html_escape(&text));
        out.push_str("</pre></section>\n");
    }
    out.push_str("</body></html>\n");
    out
}

/// Write `contents` to `path` through a `BufWriter` rather than one `std::fs::write`
/// call's implicit internal buffering choices — matches the perf mandate's explicit
/// requirement for export I/O ("no giant intermediate `Vec<String>`... write directly
/// into a `BufWriter`") and keeps the write path identical for both export formats and
/// the trust-file writer below.
fn write_text_file(contents: &str, path: &Path) -> std::io::Result<()> {
    let file = std::fs::File::create(path)?;
    let mut writer = BufWriter::new(file);
    writer.write_all(contents.as_bytes())?;
    writer.flush()
}

/// `/export [path]` (bucket A) — dispatches on the target extension: `.html` renders
/// [`export_html`], anything else (including no path, which defaults to
/// `<session-id-or-"session">.jsonl`) renders [`export_jsonl`].
pub fn export_session(
    messages: &[AgentMessage],
    session_id: Option<&str>,
    path_arg: Option<&str>,
) -> CommandOutcome {
    if messages.is_empty() {
        return CommandOutcome::Error(
            "Nothing to export yet \u{2014} the session has no messages.".to_string(),
        );
    }
    let default_stem = session_id.unwrap_or("session");
    let (contents, path): (String, PathBuf) = match path_arg {
        Some(p) if p.ends_with(".html") => (export_html(messages, session_id), PathBuf::from(p)),
        Some(p) => (export_jsonl(messages), PathBuf::from(p)),
        None => (
            export_jsonl(messages),
            PathBuf::from(format!("{default_stem}.jsonl")),
        ),
    };
    match write_text_file(&contents, &path) {
        Ok(()) => CommandOutcome::Notice(format!(
            "Exported {} message(s) to {}",
            messages.len(),
            path.display()
        )),
        Err(err) => CommandOutcome::Error(format!(
            "Failed to write export to {}: {err}",
            path.display()
        )),
    }
}

// =============================================================================
// `/share` (bucket A) — `gh gist create`, shelled out to (no HTTP dependency)
// =============================================================================

/// `/share` with no `confirm` argument — never touches `gh`, never writes a temp file.
///
/// `/share` publishes the *entire* transcript — including anything the model quoted from
/// the user's files — to a URL anyone who obtains it can read (a "secret" gist is
/// unlisted, not access-controlled: GitHub will serve it to anyone with the link). That
/// is worth gating behind an explicit confirmation, the same judgment call this file's
/// `interactive_mode.rs` counterpart already made for tool calls via `show_approval`/
/// `handle_approval_key` (`interactive_mode.rs:2021-2085`, `r`/`a`/`d` decision keys on a
/// oneshot channel the agent loop is parked on).
///
/// That oneshot shape is not reused here: it exists to block an in-flight tool call until
/// the user answers, and by the time this function runs, `dispatch_command` has already
/// finished handling `/share` synchronously — there is no call left in flight to park.
/// Requiring a second, explicit slash command instead (`/share confirm`) gets the same
/// "no accidental publish" property with none of the pending-state machinery: a bare
/// `/share` is inert by construction, and only the literal argument `confirm` reaches
/// [`run_gist_share`] (see `interactive_mode.rs::run_share`, which is the only caller of
/// either function and is what actually enforces this gate).
pub fn share_confirmation_notice() -> CommandOutcome {
    CommandOutcome::Notice(
        "/share publishes this entire conversation as a secret GitHub gist (via `gh gist \
         create`), including any file contents the model quoted. Secret gists are \
         unlisted, not private \u{2014} anyone with the link can read it. Run `/share \
         confirm` to publish, or do nothing to cancel."
            .to_string(),
    )
}

/// `/share confirm` — write the transcript to a temp file via [`export_html`] and hand it
/// to `gh gist create`.
///
/// Reuses [`export_html`] rather than inventing a third transcript format: the existing
/// self-contained, already-escaped HTML document this crate already produces for
/// `/export session.html` is exactly what a gist viewer (which renders `.html` files as
/// plain text, not live HTML — GitHub Gist has never executed gist content) should show.
///
/// Verified against `gh gist create --help` (`gh` version 2.x) rather than trusted on
/// faith: gists are secret by default, `--public` is the *opt-out* flag, and there is no
/// `--secret` flag to pass — passing a flag that does not exist would just make the
/// invocation fail — so this call passes neither `--public` nor a nonexistent `--secret`.
///
/// The temp file is removed on every return path (success, `gh` failure, and `gh` not
/// found alike) — it is scratch input to `gh`, not an artifact the user asked to keep
/// (`/export` is the command for that).
pub fn run_gist_share(messages: &[AgentMessage], session_id: Option<&str>) -> CommandOutcome {
    run_gist_share_with(messages, session_id, &real_gh_gist_create)
}

/// The actual `gh gist create` invocation, factored out to a function pointer
/// so [`run_gist_share_with`] can take a stand-in in tests (see its doc
/// comment for why: a real gist cannot be created against a scripted account,
/// and this module's existing coverage already depends on this machine
/// genuinely lacking `gh` on PATH).
fn real_gh_gist_create(path: &Path, desc: &str) -> std::io::Result<std::process::Output> {
    std::process::Command::new("gh")
        .args(["gist", "create", "--desc", desc])
        .arg(path)
        .output()
}

/// [`run_gist_share`]'s body, parameterized over the process invocation
/// (P4, `docs/tui-pending-action-plan.md`): `run_gist_share`'s success-path
/// URL parsing and non-zero-exit stderr passthrough were previously
/// untestable without a real, authenticated `gh` install — this machine has
/// neither, and CI cannot script a GitHub account either. `runner` takes the
/// already-written export path and description and returns exactly what
/// `std::process::Command::output()` would, so tests can supply a real
/// `std::process::Output` (from a genuinely-run, always-present trivial
/// command — see `tests::fake_output`) without touching `gh` at all, and the
/// parsing/error-formatting logic below runs unmodified either way.
fn run_gist_share_with(
    messages: &[AgentMessage],
    session_id: Option<&str>,
    runner: &dyn Fn(&Path, &str) -> std::io::Result<std::process::Output>,
) -> CommandOutcome {
    if messages.is_empty() {
        return CommandOutcome::Error(
            "Nothing to share yet \u{2014} the session has no messages.".to_string(),
        );
    }
    let html = export_html(messages, session_id);
    let path = std::env::temp_dir().join(format!(
        "pirust-share-{}.html",
        session_id.unwrap_or("session")
    ));
    if let Err(err) = write_text_file(&html, &path) {
        return CommandOutcome::Error(format!(
            "Failed to write the share export to {}: {err}",
            path.display()
        ));
    }
    let desc = match session_id {
        Some(id) => format!("pirust session {id}"),
        None => "pirust session export".to_string(),
    };
    let outcome = match runner(&path, &desc) {
        Ok(output) if output.status.success() => {
            // `gh gist create`'s entire useful output on success is the gist URL on
            // stdout — that is the whole point of the command, so it is the whole
            // notice.
            let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if url.is_empty() {
                CommandOutcome::Error(
                    "gh gist create exited successfully but printed no URL \u{2014} nothing \
                     to report."
                        .to_string(),
                )
            } else {
                CommandOutcome::Notice(format!("Shared as a secret gist: {url}"))
            }
        }
        Ok(output) => {
            // Pass `gh`'s own stderr through verbatim instead of inventing wording: the
            // not-authenticated case already tells the user to run `gh auth login`, and
            // duplicating that message here would just be a second place for it to go
            // stale against `gh`'s actual text.
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            CommandOutcome::Error(if stderr.is_empty() {
                format!(
                    "gh gist create exited with {:?} and printed nothing to stderr",
                    output.status.code()
                )
            } else {
                format!("gh gist create failed: {stderr}")
            })
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => CommandOutcome::Error(
            "/share needs the GitHub CLI (`gh`), which is not installed (or not on PATH) \
             on this machine. Install it from https://cli.github.com, run `gh auth \
             login`, then retry `/share confirm`."
                .to_string(),
        ),
        Err(err) => CommandOutcome::Error(format!("Failed to launch `gh gist create`: {err}")),
    };
    let _ = std::fs::remove_file(&path);
    outcome
}

// =============================================================================
// `/changelog` (bucket B — caller supplies the markdown source)
// =============================================================================

/// Keep only the first `max_entries` `## `-level headings (and everything under them) of
/// a changelog, dropping any preamble before the first heading. `max_entries == 0` is
/// treated as "no limit" the same way an unset CLI flag would be, so `/changelog` with no
/// argument still shows something instead of an empty string.
pub fn format_changelog(markdown: &str, max_entries: usize) -> String {
    let limit = if max_entries == 0 {
        usize::MAX
    } else {
        max_entries
    };
    let mut out = String::with_capacity(markdown.len().min(8192));
    let mut entries_seen = 0usize;
    for line in markdown.lines() {
        if let Some(stripped) = line.strip_prefix("## ") {
            entries_seen += 1;
            if entries_seen > limit {
                break;
            }
            out.push_str("## ");
            out.push_str(stripped);
            out.push('\n');
            continue;
        }
        if entries_seen == 0 || entries_seen > limit {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    if out.is_empty() {
        // No `## ` headings at all — fall back to the raw text rather than showing
        // nothing, since some changelog formats use a different heading level.
        out.push_str(markdown);
    }
    out
}

/// `/changelog` (bucket B) — the caller must supply a path to a changelog file.
/// pirust ships no `CHANGELOG.md` today (confirmed: no such file exists at the repo
/// root, only an unrelated one under `.agents/skills/harness-creator-v3/`), so unlike
/// every other bucket-B command here this one has no "obvious" capability to pass —
/// the caller decides what changelog (if any) `/changelog` should read.
pub fn changelog_text(path: &Path, max_entries: usize) -> CommandOutcome {
    match std::fs::read_to_string(path) {
        Ok(markdown) => CommandOutcome::Notice(format_changelog(&markdown, max_entries)),
        Err(err) => CommandOutcome::Error(format!(
            "No changelog at {} ({err}). pirust does not ship a CHANGELOG.md \u{2014} pass a real path.",
            path.display()
        )),
    }
}

// =============================================================================
// `/trust` (bucket B — new, self-authored store; no `trust.json` seam existed)
// =============================================================================

/// Path to the trust-decision store inside `agent_dir` (typically `ConfigEnv::agent_dir()`
/// — see `config.rs`). NOT a port: no `trust.json`/trust-store seam exists anywhere in
/// this crate today (`SettingsManager::is_project_trusted`/`set_project_trusted` in
/// `settings.rs:1529-1562` are in-memory only and never persist), so this is a new,
/// minimal, explicitly-labeled store rather than a port of an existing file format.
pub fn trust_store_path(agent_dir: &str) -> PathBuf {
    Path::new(agent_dir).join("trust.json")
}

/// Read the trust map, defaulting to empty on any I/O or parse failure (matches the
/// "absent file = no decisions yet" semantics `SettingsManager` uses for its own scopes).
fn read_trust_map(path: &Path) -> BTreeMap<String, bool> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|contents| serde_json::from_str(&contents).ok())
        .unwrap_or_default()
}

fn write_trust_map(path: &Path, map: &BTreeMap<String, bool>) -> std::io::Result<()> {
    let json = serde_json::to_string_pretty(map).unwrap_or_else(|_| "{}".to_string());
    write_text_file(&json, path)
}

/// `/trust <allow|deny>` (bucket B) — the caller supplies `path` (see
/// [`trust_store_path`]) and `project_cwd` (the project's working directory, the natural
/// trust key — matches `SettingsManager`'s own per-project scoping).
pub fn set_project_trust(path: &Path, project_cwd: &str, trusted: bool) -> CommandOutcome {
    let mut map = read_trust_map(path);
    map.insert(project_cwd.to_string(), trusted);
    match write_trust_map(path, &map) {
        Ok(()) => CommandOutcome::Notice(format!(
            "{project_cwd} is now {} ({}).",
            if trusted { "trusted" } else { "untrusted" },
            path.display()
        )),
        Err(err) => CommandOutcome::Error(format!(
            "Failed to write trust file {}: {err}",
            path.display()
        )),
    }
}

/// Read back a previously stored trust decision (defaults to `false`/untrusted when
/// unset, matching a fresh project's default posture).
pub fn is_project_trusted(path: &Path, project_cwd: &str) -> bool {
    read_trust_map(path)
        .get(project_cwd)
        .copied()
        .unwrap_or(false)
}

// =============================================================================
// `/login`, `/logout` (bucket B — caller supplies an `AuthStorage<B>`)
// =============================================================================

/// `/login <provider> <api-key>` (bucket B) — caller must pass a live
/// `&mut AuthStorage<B>` (production: `AuthStorage::<FileAuthStorageBackend>::create(&env)`
/// per `main.rs:302`; tests: `AuthStorage::<InMemoryAuthStorageBackend>::in_memory(...)`).
/// Generic over `AuthStorageBackend` rather than hardcoded to the file backend so the same
/// function is exercised disk-free in this module's own tests.
pub fn login_with_api_key<B: AuthStorageBackend>(
    store: &mut AuthStorage<B>,
    provider: &str,
    api_key: &str,
) -> CommandOutcome {
    if provider.trim().is_empty() {
        return CommandOutcome::Error("Usage: /login <provider> <api-key>".to_string());
    }
    if api_key.trim().is_empty() {
        return CommandOutcome::Error(format!(
            "An API key is required \u{2014} usage: /login {provider} <api-key>"
        ));
    }
    let key = api_key.to_string();
    match store.modify(provider, move |_existing| Some(Credential::api_key(key))) {
        Ok(_) => CommandOutcome::Notice(format!("Saved credentials for {provider}.")),
        Err(err) => {
            CommandOutcome::Error(format!("Failed to save credentials for {provider}: {err}"))
        }
    }
}

/// `/logout <provider>` (bucket B) — same capability as [`login_with_api_key`].
pub fn logout<B: AuthStorageBackend>(store: &mut AuthStorage<B>, provider: &str) -> CommandOutcome {
    if provider.trim().is_empty() {
        return CommandOutcome::Error("Usage: /logout <provider>".to_string());
    }
    match store.delete(provider) {
        Ok(()) => CommandOutcome::Notice(format!("Removed credentials for {provider}.")),
        Err(err) => CommandOutcome::Error(format!(
            "Failed to remove credentials for {provider}: {err}"
        )),
    }
}

// =============================================================================
// `/settings`, `/scoped-models` (bucket B — caller supplies a `&SettingsManager`)
// =============================================================================

/// A text-mode degraded substitute for Pi's interactive settings UI
/// (`pirust-tui/src/components/settings_list.rs`'s `SettingsList`, which is a stateful
/// `Rc<RefCell<dyn Component>>` tree node the TUI would have to mount — out of reach for
/// a pure function). Reads through `SettingsManager`'s existing accessors instead of
/// re-deriving them.
pub fn settings_summary(mgr: &SettingsManager) -> String {
    let mut out = String::with_capacity(192);
    out.push_str(
        "Settings (text view \u{2014} the interactive settings picker is not available here):\n",
    );
    out.push_str("  default provider: ");
    out.push_str(mgr.get_default_provider().unwrap_or("(unset)"));
    out.push('\n');
    out.push_str("  default model: ");
    out.push_str(mgr.get_default_model().unwrap_or("(unset)"));
    out.push('\n');
    out.push_str("  project trusted: ");
    out.push_str(if mgr.is_project_trusted() {
        "yes"
    } else {
        "no"
    });
    out.push('\n');
    out
}

/// `/settings` (bucket B) — caller passes the running `SettingsManager` (the interactive
/// TUI has no field holding one today; see the report for exactly where to add it).
pub fn open_settings(mgr: &SettingsManager) -> CommandOutcome {
    CommandOutcome::Notice(settings_summary(mgr))
}

/// Text rendering of `get_enabled_models()` (`settings.rs:2250-2259`), which distinguishes
/// "no scope set" (`None`, all models available) from "scoped to zero models"
/// (`Some(vec![])`) — both are rendered explicitly rather than collapsed to the same text.
pub fn scoped_models_summary(mgr: &SettingsManager) -> String {
    let mut out = String::with_capacity(128);
    match mgr.get_enabled_models() {
        Some(models) if !models.is_empty() => {
            out.push_str("Enabled models (");
            out.push_str(&models.len().to_string());
            out.push_str("):\n");
            for model in &models {
                out.push_str("  - ");
                out.push_str(model);
                out.push('\n');
            }
        }
        Some(_) => out.push_str("Enabled models: none \u{2014} every model is scoped out.\n"),
        None => out.push_str("Enabled models: all models available (no scope set).\n"),
    }
    out
}

/// `/scoped-models <provider/model>` (bucket B) — toggles `model_id` in the
/// `enabledModels` global field, writing through `SettingsManager::set_global_field`
/// (`settings.rs:1841-1847`), which persists via `self.save()`.
///
/// Note the asymmetry this preserves from `get_enabled_models`: toggling a model on when
/// no scope was previously set (`None` = "all models") narrows the scope down to just
/// that one model, rather than to "all models plus this one" (there is no such state to
/// represent — the field is either absent/all, or an explicit allowlist).
pub fn toggle_scoped_model(mgr: &mut SettingsManager, model_id: &str) -> CommandOutcome {
    if model_id.trim().is_empty() {
        return CommandOutcome::Error("Usage: /scoped-models <provider/model>".to_string());
    }
    let mut models = mgr.get_enabled_models().unwrap_or_default();
    let already_enabled = models.iter().any(|m| m == model_id);
    if already_enabled {
        models.retain(|m| m != model_id);
    } else {
        models.push(model_id.to_string());
    }
    let value = serde_json::to_value(&models).unwrap_or(serde_json::Value::Array(Vec::new()));
    mgr.set_global_field("enabledModels", value);
    if already_enabled {
        CommandOutcome::Notice(format!("Removed {model_id} from the enabled-models scope."))
    } else {
        CommandOutcome::Notice(format!("Added {model_id} to the enabled-models scope."))
    }
}

// =============================================================================
// `/reload` (bucket B — caller supplies a `&dyn PrintModeSession`)
// =============================================================================

/// `/reload` (bucket B) — `PrintModeSession::reload()` (`print_mode.rs:856`) already
/// exists and is directly callable; `dispatch_command` is synchronous, so this just adds
/// the `async` wrapper the caller can hand to `runtime.spawn`/`block_on`. Pass
/// `self.session.as_ref()` (the TUI's `Arc<dyn InteractiveSession>`, which is a
/// `PrintModeSession` by supertrait) as `session`.
pub async fn run_reload(session: &dyn crate::print_mode::PrintModeSession) -> CommandOutcome {
    session.reload().await;
    CommandOutcome::Notice("Session reloaded.".to_string())
}

// =============================================================================
// Bucket C — genuinely unreachable, precise reasons
// =============================================================================

/// Why a bucket-C command cannot be honestly implemented today. Returns `None` for
/// anything not in the bucket-C set (including commands this module doesn't touch at
/// all, like `help`).
///
/// Every reason cites the concrete seam that is missing, not a vague "not available in
/// this session" — the whole point of this function.
///
/// The `match` below has decayed to a single `_ => None` arm now that every command that
/// once lived here has been promoted out of bucket C — deliberately kept as a `match`
/// rather than collapsed to a bare `None`, because the arm comments are the ledger of
/// *why* each promotion happened and a future bucket-C command belongs as a real arm
/// right alongside them, not bolted on after an early return.
#[allow(clippy::match_single_binding)]
pub fn unavailable_reason(name: &str) -> Option<&'static str> {
    match name {
        // `/share` was bucket C when this module was written: the assumption was that
        // publishing a gist needed an HTTP client, and pirust-coding-agent's Cargo.toml
        // has none (no reqwest/hyper/ureq). That assumption was wrong — `gh gist create`
        // is a process to shell out to, not an HTTP call to make, and shelling out needs
        // no Rust dependency at all. See [`run_gist_share`] and
        // [`share_confirmation_notice`]. Hence no reason to report: it works (once `gh`
        // is installed and authenticated — see `run_gist_share`'s own error paths for
        // what happens when it is not, which is not a bucket-C gap, just a runtime
        // precondition the command reports honestly).
        //
        // `/name` was bucket C when this module was written: the only thing
        // `PrintModeSession` exposed was the *outbound* `SessionInfoChanged`
        // event, with no way for a caller to *request* a rename. It has since
        // been promoted to bucket A — `PrintModeSession::set_session_name`
        // routes to `SessionManager::append_session_info`, which always existed
        // on the store side and merely had no route out to the interactive
        // layer. Hence no reason to report: it works.
        //
        // `/new`, `/fork`, `/clone`, `/import` and `/tree` were all bucket C when this
        // module was written, and all five arms here made the same wrong claim: that
        // they needed `CommandContextActions`, which the interactive TUI never
        // constructs (print_mode.rs:783-805) — that struct is print mode's extension-
        // binding path, not something `dispatch_command` has ever gone through. The
        // real gap was that `PrintModeSession` had no `start_new_session`/
        // `clone_session`/`fork_from`/`switch_to_session_file` methods at all (`/tree`'s
        // gap was narrower: `navigate_tree` existed but nothing listed which ids were
        // valid to navigate to). Now that `print_mode.rs` provides all five —
        // `start_new_session`, `clone_session`, `fork_from`, `switch_to_session_file`,
        // and `TuiRuntimeInfo::branch_entries` for the listing `/tree` was missing —
        // `interactive_mode.rs` calls them directly on `self.session`, exactly like
        // `/name` and `/compact` above. Hence no reason to report for any of the five:
        // they work.
        // `/restart` was bucket C when this module was written: the search was for a
        // *session*-level seam (a `PrintModeSession`/`TuiRuntimeInfo`/
        // `CommandContextActions` method), and none existed, correctly — restarting the
        // whole `pirust` process was never something a session object could expose in the
        // first place. It needed a *process*-level seam instead:
        // `std::process::Command::new(std::env::current_exe()?).args(std::env::args()
        // .skip(1)).spawn()`, gated on running only after the TUI has torn down and
        // restored the terminal (`interactive_mode.rs::run_restart` sets the flag,
        // `main.rs::run_interactive_mode` re-execs once `run_async` returns — see both
        // doc comments for why the ordering is load-bearing). Hence no reason to report:
        // it works.
        //
        // `/compact` was bucket C when this module was written: the
        // deterministic half of compaction (`compaction_v4::prepare_compaction`,
        // `harness/compaction/v4.rs`) was real and working, and
        // `CompactionReason`/`CompactionStart`/`CompactionEnd` already
        // existed as vocabulary (print_mode.rs), but `PrintModeSession` —
        // the TUI's entire session capability surface — had no method that
        // reached it; the trigger existed one layer below what the TUI
        // could see. It has since been promoted to bucket A —
        // `PrintModeSession::compact` bridges the flat `Agent` message list
        // `SingleTurnSession` actually holds to that same deterministic
        // logic via `compaction_v4::prepare_compaction_from_messages`
        // (`harness/compaction/v4.rs`), and `run_compact`
        // (`interactive_mode.rs`) now spawns it as a real turn-mutually-
        // exclusive task. Hence no reason to report: it works.
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{AuthStorageData, InMemoryAuthStorageBackend};
    use crate::print_mode::{
        Cancelled, ExtensionBinding, NavigateTreeOptions, PromptOptions, SessionEventListener,
        SessionStateView, Subscription, ThrownValue,
    };
    use crate::settings::{InMemorySettingsStorage, SettingsManagerCreateOptions};
    use pirust_agent_core::harness::types::SessionHeader;
    use pirust_ai::types::{AssistantMessage, TextContent, UserMessage, UserRole};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    // -------------------------------------------------------------------------
    // Command table
    // -------------------------------------------------------------------------

    #[test]
    fn command_table_has_27_entries_matching_builtin_slash_commands() {
        assert_eq!(COMMANDS.len(), 27);
    }

    #[test]
    fn command_spec_finds_known_names_and_rejects_unknown() {
        assert_eq!(command_spec("compact").map(|s| s.name), Some("compact"));
        assert_eq!(
            command_spec("export").map(|s| s.category),
            Some(CommandCategory::Storage)
        );
        assert!(command_spec("not-a-real-command").is_none());
    }

    #[test]
    fn help_lines_group_by_category_and_mark_unavailable() {
        let text = command_help_lines(&|name| name != "compact");
        assert!(text.contains("/help"));
        assert!(text.contains("/compact"));
        assert!(text.contains("(unavailable in this session)"));
        // The one marked unavailable is compact, not help.
        let compact_line = text.lines().find(|l| l.contains("/compact")).unwrap();
        assert!(compact_line.contains("unavailable"));
        let help_line = text.lines().find(|l| l.contains("/help")).unwrap();
        assert!(!help_line.contains("unavailable"));
    }

    // -------------------------------------------------------------------------
    // base64 / OSC 52 / copy
    // -------------------------------------------------------------------------

    #[test]
    fn base64_matches_rfc4648_test_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn osc52_sequence_wraps_base64_payload() {
        let seq = osc52_copy_sequence("hi");
        assert_eq!(seq, "\u{1b}]52;c;aGk=\u{07}");
    }

    fn assistant_text_message(text: &str) -> AgentMessage {
        AgentMessage::Llm(Message::Assistant(AssistantMessage {
            role: Default::default(),
            content: vec![AssistantContent::Text(TextContent::new(text))],
            api: "anthropic-messages".into(),
            provider: "anthropic".into(),
            model: None,
            response_model: None,
            diagnostics: None,
            usage: pirust_ai::types::Usage {
                input: 0,
                output: 0,
                cache_read: 0,
                cache_write: 0,
                cache_write1h: None,
                reasoning: None,
                total_tokens: None,
                cost: pirust_ai::types::Cost {
                    input: 0.0,
                    output: 0.0,
                    cache_read: 0.0,
                    cache_write: 0.0,
                    total: 0.0,
                },
            },
            stop_reason: pirust_ai::types::StopReason::Stop,
            timestamp: 0,
            response_id: None,
            raw_stop_reason: None,
            error_message: None,
            end_turn: None,
        }))
    }

    fn user_text_message(text: &str) -> AgentMessage {
        AgentMessage::Llm(Message::User(UserMessage {
            role: UserRole::User,
            content: UserMessageContent::Text(text.to_string()),
            timestamp: 0,
        }))
    }

    #[test]
    fn last_assistant_text_finds_most_recent_assistant_text() {
        let messages = vec![
            user_text_message("hi"),
            assistant_text_message("first reply"),
            user_text_message("more"),
            assistant_text_message("second reply"),
        ];
        assert_eq!(
            last_assistant_text(&messages),
            Some("second reply".to_string())
        );
    }

    #[test]
    fn last_assistant_text_none_when_no_assistant_message() {
        let messages = vec![user_text_message("hi")];
        assert_eq!(last_assistant_text(&messages), None);
    }

    #[test]
    fn copy_last_message_produces_clipboard_outcome() {
        let messages = vec![assistant_text_message("copy me")];
        match copy_last_message(&messages) {
            CommandOutcome::CopyToClipboard(seq) => {
                assert!(seq.starts_with("\u{1b}]52;c;"));
                assert!(seq.ends_with('\u{07}'));
            }
            other => panic!("expected CopyToClipboard, got {other:?}"),
        }
    }

    #[test]
    fn copy_last_message_errors_when_nothing_to_copy() {
        let messages: Vec<AgentMessage> = vec![];
        assert!(matches!(
            copy_last_message(&messages),
            CommandOutcome::Error(_)
        ));
    }

    // -------------------------------------------------------------------------
    // export
    // -------------------------------------------------------------------------

    #[test]
    fn export_jsonl_round_trips_every_message() {
        let messages = vec![user_text_message("hi"), assistant_text_message("hello")];
        let jsonl = export_jsonl(&messages);
        let lines: Vec<&str> = jsonl.lines().collect();
        assert_eq!(lines.len(), messages.len());
        for (line, original) in lines.iter().zip(messages.iter()) {
            let parsed: AgentMessage = serde_json::from_str(line).unwrap();
            assert_eq!(&parsed, original);
        }
    }

    #[test]
    fn export_html_escapes_unsafe_text() {
        let messages = vec![user_text_message("<script>alert(1)</script>")];
        let html = export_html(&messages, Some("s1"));
        assert!(!html.contains("<script>alert"));
        assert!(html.contains("&lt;script&gt;"));
        assert!(html.starts_with("<!doctype html>"));
    }

    #[test]
    fn export_session_writes_jsonl_by_default_and_html_when_requested() {
        let dir = tempfile::tempdir().unwrap();
        let messages = vec![assistant_text_message("hello")];

        let jsonl_path = dir.path().join("out.jsonl");
        let outcome = export_session(&messages, Some("s1"), Some(jsonl_path.to_str().unwrap()));
        assert!(matches!(outcome, CommandOutcome::Notice(_)));
        let contents = std::fs::read_to_string(&jsonl_path).unwrap();
        assert_eq!(contents.lines().count(), 1);

        let html_path = dir.path().join("out.html");
        let outcome = export_session(&messages, Some("s1"), Some(html_path.to_str().unwrap()));
        assert!(matches!(outcome, CommandOutcome::Notice(_)));
        let contents = std::fs::read_to_string(&html_path).unwrap();
        assert!(contents.starts_with("<!doctype html>"));
    }

    #[test]
    fn export_session_errors_on_empty_transcript() {
        let outcome = export_session(&[], None, None);
        assert!(matches!(outcome, CommandOutcome::Error(_)));
    }

    // -------------------------------------------------------------------------
    // share
    //
    // The success path (a real `gh gist create` call that actually publishes) is
    // untestable in this environment: `gh` is not installed here, and a test suite must
    // never depend on a network call succeeding, let alone one that publishes a gist to
    // a real GitHub account, even if `gh` happened to be present. What *is* testable for
    // real, without a mock, is exactly what this machine's environment provides: the
    // confirmation gate (no `gh` call at all), the empty-session guard (same, no `gh`
    // call), and the "gh not installed" branch, which this machine hits genuinely, not
    // via a stub.
    // -------------------------------------------------------------------------

    #[test]
    fn share_confirmation_notice_names_the_confirm_argument_and_never_touches_gh() {
        // No temp file, no `gh` invocation — just a `String` build, so this needs no
        // cleanup and cannot leave anything behind on disk.
        match share_confirmation_notice() {
            CommandOutcome::Notice(text) => {
                assert!(text.contains("/share confirm"));
                assert!(text.contains("secret"));
            }
            other => panic!("expected Notice, got {other:?}"),
        }
    }

    #[test]
    fn run_gist_share_errors_on_empty_transcript_without_touching_gh_or_disk() {
        // Mirrors `export_session_errors_on_empty_transcript` above: the empty-messages
        // guard returns before `write_text_file`/`Command::new("gh")` run at all, so this
        // is safe to run with no temp-dir cleanup and no `gh` on PATH.
        let outcome = run_gist_share(&[], Some("test-session"));
        match outcome {
            CommandOutcome::Error(text) => assert!(text.contains("Nothing to share")),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn run_gist_share_reports_actionable_error_when_gh_is_absent() {
        // This machine genuinely has no `gh` on PATH — confirmed, not assumed — so this
        // exercises the real `ErrorKind::NotFound` branch, not a mock of it. If `gh` is
        // ever installed in whatever environment runs this suite, the assertions below
        // would need `gh`'s actual (also actionable) stderr instead; that gap is the
        // documented cost of not depending on a fake process boundary for `Command`.
        assert!(
            std::process::Command::new("gh")
                .arg("--version")
                .output()
                .is_err(),
            "this test assumes `gh` is not on PATH; skip/update it in an environment where it is"
        );
        let messages = vec![user_text_message("hello from a test")];
        let outcome = run_gist_share(&messages, Some("share-test-session"));
        match outcome {
            CommandOutcome::Error(text) => {
                assert!(text.contains("gh"));
                assert!(text.contains("https://cli.github.com"));
            }
            other => panic!("expected Error, got {other:?}"),
        }
        // The temp file `run_gist_share` wrote must not survive a failed share — cleanup
        // runs on every return path, including this one.
        let leftover = std::env::temp_dir().join("pirust-share-share-test-session.html");
        assert!(
            !leftover.exists(),
            "run_gist_share must remove its temp file even when `gh` is absent"
        );
    }

    // -------------------------------------------------------------------------
    // `run_gist_share_with` — the injectable-runner seam (P4,
    // `docs/tui-pending-action-plan.md`): a real, authenticated `gh` cannot be
    // scripted here or in CI, so these tests supply a genuinely-run, always-
    // present trivial command in place of `gh` to prove the URL-parsing and
    // non-zero-exit passthrough logic without ever invoking `gh` itself.
    // -------------------------------------------------------------------------

    /// A real `std::process::Output` (genuine `ExitStatus`, no hand-built
    /// platform-specific bit-packing) produced by a trivial, always-present
    /// shell invocation — stands in for `gh`'s output in the tests below.
    #[cfg(unix)]
    fn fake_output(code: i32, stdout_text: &str, stderr_text: &str) -> std::process::Output {
        std::process::Command::new("sh")
            .arg("-c")
            .arg(r#"printf '%s' "$1"; printf '%s' "$2" >&2; exit "$3""#)
            .arg("_")
            .arg(stdout_text)
            .arg(stderr_text)
            .arg(code.to_string())
            .output()
            .expect("sh must be available to build a fake Output")
    }

    #[cfg(windows)]
    fn fake_output(code: i32, stdout_text: &str, stderr_text: &str) -> std::process::Output {
        // `echo` with an empty argument prints the literal "ECHO is on."
        // (a `cmd` quirk, not a blank line) — segments are omitted entirely
        // rather than passed empty so the empty-output test case stays empty.
        let mut script = String::new();
        if !stdout_text.is_empty() {
            script.push_str(&format!("echo {stdout_text}&"));
        }
        if !stderr_text.is_empty() {
            script.push_str(&format!("echo {stderr_text} 1>&2&"));
        }
        script.push_str(&format!("exit /b {code}"));
        std::process::Command::new("cmd")
            .args(["/C", &script])
            .output()
            .expect("cmd must be available to build a fake Output")
    }

    #[test]
    fn run_gist_share_with_parses_the_gist_url_on_success() {
        let messages = vec![user_text_message("hello from a test")];
        let outcome = run_gist_share_with(&messages, Some("share-url-test"), &|_path, _desc| {
            Ok(fake_output(0, "https://gist.github.com/anonymous/deadbeef", ""))
        });
        match outcome {
            CommandOutcome::Notice(text) => {
                assert!(text.contains("Shared as a secret gist:"));
                assert!(text.contains("https://gist.github.com/anonymous/deadbeef"));
            }
            other => panic!("expected Notice, got {other:?}"),
        }
        let leftover = std::env::temp_dir().join("pirust-share-share-url-test.html");
        assert!(
            !leftover.exists(),
            "run_gist_share_with must remove its temp file on the success path too"
        );
    }

    #[test]
    fn run_gist_share_with_reports_error_when_success_output_has_no_url() {
        let messages = vec![user_text_message("hello from a test")];
        let outcome = run_gist_share_with(&messages, Some("share-empty-test"), &|_path, _desc| {
            Ok(fake_output(0, "", ""))
        });
        match outcome {
            CommandOutcome::Error(text) => assert!(text.contains("printed no URL")),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn run_gist_share_with_passes_through_nonzero_exit_stderr() {
        let messages = vec![user_text_message("hello from a test")];
        let outcome = run_gist_share_with(&messages, Some("share-fail-test"), &|_path, _desc| {
            Ok(fake_output(1, "", "not authenticated. run: gh auth login"))
        });
        match outcome {
            CommandOutcome::Error(text) => {
                assert!(text.contains("gh gist create failed:"));
                assert!(text.contains("not authenticated"));
            }
            other => panic!("expected Error, got {other:?}"),
        }
        let leftover = std::env::temp_dir().join("pirust-share-share-fail-test.html");
        assert!(
            !leftover.exists(),
            "run_gist_share_with must remove its temp file on the error path too"
        );
    }

    // -------------------------------------------------------------------------
    // changelog
    // -------------------------------------------------------------------------

    const SAMPLE_CHANGELOG: &str = "\
# Changelog

Some preamble that should be dropped.

## 1.2.0
- feature A
- feature B

## 1.1.0
- fix C

## 1.0.0
- initial release
";

    #[test]
    fn format_changelog_caps_at_max_entries_and_drops_preamble() {
        let text = format_changelog(SAMPLE_CHANGELOG, 2);
        assert!(!text.contains("preamble"));
        assert!(text.contains("## 1.2.0"));
        assert!(text.contains("## 1.1.0"));
        assert!(!text.contains("## 1.0.0"));
    }

    #[test]
    fn format_changelog_zero_means_unlimited() {
        let text = format_changelog(SAMPLE_CHANGELOG, 0);
        assert!(text.contains("## 1.0.0"));
    }

    #[test]
    fn changelog_text_reports_missing_file_precisely() {
        let outcome = changelog_text(Path::new("does-not-exist-changelog.md"), 5);
        match outcome {
            CommandOutcome::Error(message) => {
                assert!(message.contains("does not ship a CHANGELOG.md"));
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    // -------------------------------------------------------------------------
    // trust
    // -------------------------------------------------------------------------

    #[test]
    fn trust_round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trust.json");
        assert!(!is_project_trusted(&path, "/proj"));

        let outcome = set_project_trust(&path, "/proj", true);
        assert!(matches!(outcome, CommandOutcome::Notice(_)));
        assert!(is_project_trusted(&path, "/proj"));
        assert!(!is_project_trusted(&path, "/other"));

        set_project_trust(&path, "/proj", false);
        assert!(!is_project_trusted(&path, "/proj"));
    }

    #[test]
    fn trust_store_path_joins_agent_dir() {
        let path = trust_store_path("/home/user/.pirust/agent");
        assert_eq!(
            path,
            PathBuf::from("/home/user/.pirust/agent").join("trust.json")
        );
    }

    // -------------------------------------------------------------------------
    // login / logout
    // -------------------------------------------------------------------------

    #[test]
    fn login_then_logout_round_trips_through_in_memory_backend() {
        let mut store =
            AuthStorage::<InMemoryAuthStorageBackend>::in_memory(&AuthStorageData::new()).unwrap();

        let outcome = login_with_api_key(&mut store, "anthropic", "sk-test-key");
        assert!(matches!(outcome, CommandOutcome::Notice(_)));
        let saved = store.read("anthropic").expect("credential saved");
        assert_eq!(saved, Credential::api_key("sk-test-key"));

        let outcome = logout(&mut store, "anthropic");
        assert!(matches!(outcome, CommandOutcome::Notice(_)));
        assert!(store.read("anthropic").is_none());
    }

    #[test]
    fn login_rejects_empty_api_key() {
        let mut store =
            AuthStorage::<InMemoryAuthStorageBackend>::in_memory(&AuthStorageData::new()).unwrap();
        let outcome = login_with_api_key(&mut store, "anthropic", "   ");
        assert!(matches!(outcome, CommandOutcome::Error(_)));
    }

    // -------------------------------------------------------------------------
    // settings / scoped-models
    // -------------------------------------------------------------------------

    fn in_memory_settings_manager() -> SettingsManager {
        SettingsManager::from_storage(
            Arc::new(InMemorySettingsStorage::new()),
            SettingsManagerCreateOptions::default(),
        )
    }

    #[test]
    fn settings_summary_reports_unset_defaults() {
        let mgr = in_memory_settings_manager();
        let text = settings_summary(&mgr);
        assert!(text.contains("default provider: (unset)"));
        assert!(text.contains("default model: (unset)"));
    }

    #[test]
    fn scoped_models_toggle_adds_then_removes() {
        let mut mgr = in_memory_settings_manager();
        assert!(scoped_models_summary(&mgr).contains("all models available"));

        let outcome = toggle_scoped_model(&mut mgr, "anthropic/claude");
        assert!(matches!(outcome, CommandOutcome::Notice(_)));
        assert_eq!(
            mgr.get_enabled_models(),
            Some(vec!["anthropic/claude".to_string()])
        );
        assert!(scoped_models_summary(&mgr).contains("anthropic/claude"));

        let outcome = toggle_scoped_model(&mut mgr, "anthropic/claude");
        assert!(matches!(outcome, CommandOutcome::Notice(_)));
        assert_eq!(mgr.get_enabled_models(), Some(vec![]));
    }

    // -------------------------------------------------------------------------
    // reload
    // -------------------------------------------------------------------------

    struct MockSession {
        reload_calls: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl crate::print_mode::PrintModeSession for MockSession {
        fn header(&self) -> Option<SessionHeader> {
            None
        }

        async fn bind_extensions(&self, _binding: ExtensionBinding) -> Result<(), ThrownValue> {
            Ok(())
        }

        fn subscribe(&self, _listener: SessionEventListener) -> Subscription {
            Subscription::new(|| {})
        }

        async fn prompt(
            &self,
            _text: &str,
            _options: Option<PromptOptions>,
        ) -> Result<(), ThrownValue> {
            Ok(())
        }

        fn state(&self) -> SessionStateView {
            SessionStateView {
                messages: Vec::new(),
            }
        }

        async fn wait_for_idle(&self) {}

        async fn navigate_tree(
            &self,
            _target_id: &str,
            _options: Option<NavigateTreeOptions>,
        ) -> Cancelled {
            Cancelled { cancelled: false }
        }

        async fn reload(&self) {
            self.reload_calls.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn run_reload_delegates_to_session_reload() {
        let session = MockSession {
            reload_calls: AtomicUsize::new(0),
        };
        let outcome = run_reload(&session).await;
        assert_eq!(session.reload_calls.load(Ordering::SeqCst), 1);
        assert!(matches!(outcome, CommandOutcome::Notice(_)));
    }

    // -------------------------------------------------------------------------
    // bucket C
    // -------------------------------------------------------------------------

    #[test]
    fn bucket_c_is_now_empty() {
        // Bucket C had two members left when this test was last named
        // `unavailable_reason_covers_every_bucket_c_command_with_specifics` — `share` and
        // `restart` — both promoted out (see `share_is_no_longer_a_bucket_c_gap` and
        // `restart_is_no_longer_a_bucket_c_gap` below). Every other former bucket-C
        // command was promoted earlier: `name`/`compact` individually
        // (`name_is_no_longer_a_bucket_c_gap`/`compact_is_no_longer_a_bucket_c_gap`
        // below) and `new`/`fork`/`clone`/`import`/`tree` together
        // (`session_control_commands_are_no_longer_bucket_c_gaps` below). So rather than
        // collecting reasons for a named list the way this test used to, it now asserts
        // the ledger's claim directly: no name in [`COMMANDS`] gets a reason at all.
        for spec in COMMANDS {
            assert_eq!(
                unavailable_reason(spec.name),
                None,
                "{} should not report a bucket-C gap \u{2014} bucket C is empty now",
                spec.name
            );
        }
    }

    /// `/share` really shells out to `gh gist create` now (once `gh` is installed and
    /// authenticated — see `run_gist_share`'s error paths for the honest report when it
    /// is not), so it must not still be reported as a bucket-C gap. A bare `/share` still
    /// never calls `gh` — see `share_confirmation_notice` — but that is a confirmation
    /// gate, not an unreachable seam, and `unavailable_reason`/`reachable` are about the
    /// latter.
    #[test]
    fn share_is_no_longer_a_bucket_c_gap() {
        assert_eq!(unavailable_reason("share"), None);
        assert_eq!(command_spec("share").map(|s| s.reachable), Some(true));
    }

    /// `/restart` really re-execs now, via `std::process::Command` in
    /// `main.rs::run_interactive_mode`, strictly after the TUI's normal shutdown has
    /// restored the terminal — see `interactive_mode.rs::run_restart`'s doc comment for
    /// why running it any earlier would wedge the terminal instead of restarting it.
    #[test]
    fn restart_is_no_longer_a_bucket_c_gap() {
        assert_eq!(unavailable_reason("restart"), None);
        assert_eq!(command_spec("restart").map(|s| s.reachable), Some(true));
    }

    /// `/new`, `/fork`, `/clone`, `/import` and `/tree` really work now, so none of
    /// them may still be reported as a bucket-C gap — mirrors
    /// `name_is_no_longer_a_bucket_c_gap` below, one command at a time so a future
    /// regression on any single command fails with its own name rather than a
    /// combined loop assertion.
    #[test]
    fn session_control_commands_are_no_longer_bucket_c_gaps() {
        for name in ["new", "fork", "clone", "import", "tree"] {
            assert_eq!(
                unavailable_reason(name),
                None,
                "{name} should no longer report a bucket-C gap"
            );
            assert_eq!(
                command_spec(name).map(|s| s.reachable),
                Some(true),
                "{name}'s CommandSpec should report reachable: true"
            );
        }
    }

    /// `/name` really renames now, so it must not still be reported as a gap.
    ///
    /// The store side (`SessionManager::append_session_info`) always existed;
    /// what was missing was a route out to the interactive layer, which
    /// `PrintModeSession::set_session_name` now provides. Both facts this
    /// module publishes about a command — the table's `reachable` flag and
    /// `unavailable_reason` — have to agree with that.
    #[test]
    fn name_is_no_longer_a_bucket_c_gap() {
        assert_eq!(unavailable_reason("name"), None);
        assert_eq!(command_spec("name").map(|s| s.reachable), Some(true));
    }

    /// `/compact` really compacts now, so it must not still be reported as a
    /// gap. The deterministic logic (`compaction_v4::prepare_compaction`)
    /// always existed; what was missing was a route from `PrintModeSession`
    /// down to it, which `PrintModeSession::compact` +
    /// `compaction_v4::prepare_compaction_from_messages` now provide.
    #[test]
    fn compact_is_no_longer_a_bucket_c_gap() {
        assert_eq!(unavailable_reason("compact"), None);
        assert_eq!(command_spec("compact").map(|s| s.reachable), Some(true));
    }

    #[test]
    fn unavailable_reason_is_none_for_reachable_commands() {
        assert_eq!(unavailable_reason("help"), None);
        assert_eq!(unavailable_reason("export"), None);
        assert_eq!(unavailable_reason("reload"), None);
    }
}
