//! Port of `cli/args.ts` — the hand-rolled arg parser and `--help` renderer.
//!
//! `parseArgs` is 100% pure (no env / fs / cwd / TTY / clock), so it is a clean golden
//! target. Gated by `tests/fixtures/pi/cli/args.corpus.jsonl` and `help.*.golden`.
//!
//! # Why this is ported branch-for-branch
//!
//! [`parse_args`] is one `for` loop over argv with a long `if`/`else if` chain
//! (TS `args.ts:71-207`). **First match wins**, values are consumed by advancing the
//! index (`args[++i]`), and every value-taking branch is guarded by
//! `i + 1 < args.length`. When that guard fails the token does not error — it *falls
//! through* to the four tail branches, and which tail branch it lands in depends only on
//! the token's prefix. That fall-through is the contract, so the chain is reproduced in
//! Pi's source order rather than restructured into a table.
//!
//! The consequences pinned by the corpus (`trap:*` rows):
//! - **No `--flag=value` support for known flags.** `--model=sonnet` never matches the
//!   `arg === "--model"` branch; it lands in [`Args::unknown_flags`] as
//!   `{"model" => "sonnet"}` (TS `args.ts:89`, `:188-201`).
//! - **Long/short last-token asymmetry.** `--model` as the final token → unknown flag
//!   `{"model" => true}`, *no* diagnostic; `-t` / `-xt` / `-e` as the final token →
//!   fatal `Unknown option: -t` (tail branch T3); `-n` → `--name requires a value`
//!   (the `--name` branch owns its own `else`, TS `args.ts:98-103`).
//! - **Values are consumed blindly**: `--model --print` sets `model = "--print"`.
//! - **No `--` end-of-options handling anywhere**: `--` is just an unknown long flag
//!   with the empty name, and it eats the next token (`args.ts:188-201`).
//! - `--models` keeps empty segments, `--tools` / `--exclude-tools` filter them
//!   (`args.ts:114` vs `:120-129`).
//!
//! # Divergence from the spec's sketch
//!
//! `docs/analysis/09-cli-config-spec.md` §3.2 models the write-once boolean flags as
//! plain `bool`. This port uses `Option<bool>` for *every* `?:` field instead: the
//! oracle corpus distinguishes "key absent" from "key present and false", and a
//! `Option<bool>` field serializes to exactly Pi's JSON without a skip predicate that
//! could silently swallow a real `false`. Consumers that only want JS truthiness use
//! `args.print.unwrap_or(false)`.

use std::fmt;

use pirust_agent_core::types::ThinkingLevel;
use serde::ser::SerializeStruct;
use serde::{Deserialize, Serialize, Serializer};

/// Output mode (TS `Mode`, `args.ts:10`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Text,
    Json,
    Rpc,
}

/// `listModels?: string | true` (TS `args.ts:46`) — **three** states, not two.
///
/// Absent is modelled by the enclosing `Option`, so the field is
/// `Option<ListModels>`: `None` (flag not given), `Some(All)` (`--list-models`) and
/// `Some(Pattern(p))` (`--list-models <p>`, TS `args.ts:171-177`). `Pattern` may hold
/// the empty string — `--list-models ""` is a pattern, not `true` (corpus
/// `trap:--list-models-followed-by-empty-string`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListModels {
    /// `listModels === true`.
    All,
    /// `listModels === "<pattern>"`.
    Pattern(String),
}

impl Serialize for ListModels {
    /// Mirrors the `string | true` union: `All` is the literal `true`.
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            ListModels::All => serializer.serialize_bool(true),
            ListModels::Pattern(pattern) => serializer.serialize_str(pattern),
        }
    }
}

/// A value in [`Args::unknown_flags`] — TS `boolean | string` (`args.ts:53`).
///
/// Also the type of `ExtensionFlag.default` (TS `extensions/types.ts:1502`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum FlagValue {
    Bool(bool),
    Str(String),
}

/// Insertion-ordered `Map<string, boolean | string>` (TS `args.ts:53`).
///
/// Order is observable: `core/agent-session-services.ts:99` iterates this map to apply
/// extension flags and to build the fatal `Unknown options: ...` list, so that list is
/// in argv order. JS `Map.set` on an existing key overwrites the value but keeps the
/// key's **original** position — [`UnknownFlags::insert`] reproduces that (corpus
/// `trap:unknown-flag-repeated-key-last-value-wins-first-position-kept`).
///
/// A `Vec` of pairs rather than an `IndexMap` because the crate has no `indexmap`
/// dependency and these maps hold a handful of entries at most.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct UnknownFlags(Vec<(String, FlagValue)>);

impl UnknownFlags {
    /// JS `Map.prototype.set`: overwrite in place, otherwise append.
    pub fn insert(&mut self, key: impl Into<String>, value: FlagValue) {
        let key = key.into();
        if let Some(slot) = self.0.iter_mut().find(|(k, _)| *k == key) {
            slot.1 = value;
        } else {
            self.0.push((key, value));
        }
    }

    /// JS `Map.prototype.get`.
    pub fn get(&self, key: &str) -> Option<&FlagValue> {
        self.0.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    /// The entries, in insertion order.
    pub fn entries(&self) -> &[(String, FlagValue)] {
        &self.0
    }

    /// Iterate in insertion order (JS `Map` iteration order).
    pub fn iter(&self) -> std::slice::Iter<'_, (String, FlagValue)> {
        self.0.iter()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl<'a> IntoIterator for &'a UnknownFlags {
    type Item = &'a (String, FlagValue);
    type IntoIter = std::slice::Iter<'a, (String, FlagValue)>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

/// Severity of a parse-time diagnostic (TS `args.ts:54`: `"warning" | "error"`).
///
/// `main.ts:510-518` renders each as `Error: <message>` / `Warning: <message>` on
/// stderr and exits 1 if any is an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiagnosticKind {
    Warning,
    Error,
}

/// One entry of `Args.diagnostics` (TS `args.ts:54`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    /// TS key is `type`.
    #[serde(rename = "type")]
    pub kind: DiagnosticKind,
    pub message: String,
}

impl Diagnostic {
    /// `{ type: "warning", message }` (TS `args.ts:135-138`).
    pub fn warning(message: impl Into<String>) -> Self {
        Diagnostic {
            kind: DiagnosticKind::Warning,
            message: message.into(),
        }
    }

    /// `{ type: "error", message }` (TS `args.ts:102`, `:203`).
    pub fn error(message: impl Into<String>) -> Self {
        Diagnostic {
            kind: DiagnosticKind::Error,
            message: message.into(),
        }
    }
}

/// Parsed CLI arguments (TS `Args`, `args.ts:12-55`).
///
/// Every `?:` field is `Option`; the four always-present collections start empty
/// (TS `args.ts:64-69`). Field order matches the TS interface so the [`Serialize`] impl
/// (used by the golden suite) reads as a transcription of it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Args {
    /// `--provider <name>` (`args.ts:87-88`).
    pub provider: Option<String>,
    /// `--model <pattern>` (`args.ts:89-90`).
    pub model: Option<String>,
    /// `--api-key <key>` (`args.ts:91-92`).
    pub api_key: Option<String>,
    /// `--system-prompt <text>`, last occurrence wins (`args.ts:93-94`).
    pub system_prompt: Option<String>,
    /// `--append-system-prompt <text>`, repeatable (`args.ts:95-97`).
    ///
    /// `None` vs `Some(vec![])` is observable — TS lazily initialises with `?? []`.
    pub append_system_prompt: Option<Vec<String>>,
    /// `--thinking <level>`, only set when the level is valid (`args.ts:130-139`).
    pub thinking: Option<ThinkingLevel>,
    /// `--continue` / `-c` (`args.ts:83-84`). TS key is `continue`.
    pub r#continue: Option<bool>,
    /// `--resume` / `-r` (`args.ts:85-86`).
    pub resume: Option<bool>,
    /// `--help` / `-h` (`args.ts:74-75`).
    pub help: Option<bool>,
    /// `--version` / `-v` (`args.ts:76-77`).
    pub version: Option<bool>,
    /// `--mode <text|json|rpc>`; an invalid value is consumed and dropped silently
    /// (`args.ts:78-82`).
    pub mode: Option<Mode>,
    /// `--name` / `-n <name>` (`args.ts:98-103`).
    pub name: Option<String>,
    /// `--no-session` (`args.ts:104-105`).
    pub no_session: Option<bool>,
    /// `--session <path|id>` (`args.ts:106-107`).
    pub session: Option<String>,
    /// `--session-id <id>` (`args.ts:108-109`).
    pub session_id: Option<String>,
    /// `--fork <path|id>` (`args.ts:110-111`).
    pub fork: Option<String>,
    /// `--session-dir <dir>` (`args.ts:112-113`).
    pub session_dir: Option<String>,
    /// `--models <patterns>`: split on `,` + trim, **empty segments kept**
    /// (`args.ts:114-115`).
    pub models: Option<Vec<String>>,
    /// `--tools` / `-t <tools>`: split + trim + **drop empties** (`args.ts:120-124`).
    pub tools: Option<Vec<String>>,
    /// `--exclude-tools` / `-xt <tools>`: split + trim + drop empties
    /// (`args.ts:125-129`).
    pub exclude_tools: Option<Vec<String>>,
    /// `--no-tools` / `-nt` (`args.ts:116-117`).
    pub no_tools: Option<bool>,
    /// `--no-builtin-tools` / `-nbt` (`args.ts:118-119`).
    pub no_builtin_tools: Option<bool>,
    /// `--extension` / `-e <path>`, repeatable (`args.ts:149-151`).
    pub extensions: Option<Vec<String>>,
    /// `--no-extensions` / `-ne` (`args.ts:152-153`).
    pub no_extensions: Option<bool>,
    /// `--print` / `-p` (`args.ts:140-146`).
    pub print: Option<bool>,
    /// `--export <file>` (`args.ts:147-148`).
    pub export: Option<String>,
    /// `--no-skills` / `-ns` (`args.ts:163-164`).
    pub no_skills: Option<bool>,
    /// `--skill <path>`, repeatable (`args.ts:154-156`).
    pub skills: Option<Vec<String>>,
    /// `--prompt-template <path>`, repeatable (`args.ts:157-159`).
    pub prompt_templates: Option<Vec<String>>,
    /// `--no-prompt-templates` / `-np` (`args.ts:165-166`).
    pub no_prompt_templates: Option<bool>,
    /// `--theme <path>`, repeatable (`args.ts:160-162`).
    pub themes: Option<Vec<String>>,
    /// `--no-themes` — no short alias (`args.ts:167-168`).
    pub no_themes: Option<bool>,
    /// `--no-context-files` / `-nc` (`args.ts:169-170`).
    pub no_context_files: Option<bool>,
    /// `--list-models [search]` (`args.ts:171-177`).
    pub list_models: Option<ListModels>,
    /// `--offline` (`args.ts:184-185`).
    pub offline: Option<bool>,
    /// `--verbose` (`args.ts:178-179`).
    pub verbose: Option<bool>,
    /// `--approve` / `-a` → `true`; `--no-approve` / `-na` → `false`; last wins
    /// (`args.ts:180-183`).
    pub project_trust_override: Option<bool>,
    /// Non-flag tokens, in order (`args.ts:204-206`), plus `-p`'s swallowed message.
    pub messages: Vec<String>,
    /// `@file` tokens with the `@` stripped (`args.ts:186-187`).
    pub file_args: Vec<String>,
    /// Unknown long flags — potentially extension flags (`args.ts:52-53`, `:188-201`).
    pub unknown_flags: UnknownFlags,
    /// Parse-time diagnostics, in argv order (`args.ts:54`).
    pub diagnostics: Vec<Diagnostic>,
}

impl Serialize for Args {
    /// Emits exactly the JSON `JSON.stringify(parseArgs(argv))` would: camelCase keys,
    /// `undefined` fields omitted, `unknownFlags` as an ordered array of `[key, value]`
    /// pairs (the shape `[...map]` produces, and the shape the corpus stores).
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // `serialize_struct`'s len is advisory for JSON; count the present keys anyway.
        let mut s = serializer.serialize_struct("Args", 41)?;
        macro_rules! opt {
            ($key:literal, $field:expr) => {
                match &$field {
                    Some(value) => s.serialize_field($key, value)?,
                    None => s.skip_field($key)?,
                }
            };
        }
        opt!("provider", self.provider);
        opt!("model", self.model);
        opt!("apiKey", self.api_key);
        opt!("systemPrompt", self.system_prompt);
        opt!("appendSystemPrompt", self.append_system_prompt);
        opt!("thinking", self.thinking);
        opt!("continue", self.r#continue);
        opt!("resume", self.resume);
        opt!("help", self.help);
        opt!("version", self.version);
        opt!("mode", self.mode);
        opt!("name", self.name);
        opt!("noSession", self.no_session);
        opt!("session", self.session);
        opt!("sessionId", self.session_id);
        opt!("fork", self.fork);
        opt!("sessionDir", self.session_dir);
        opt!("models", self.models);
        opt!("tools", self.tools);
        opt!("excludeTools", self.exclude_tools);
        opt!("noTools", self.no_tools);
        opt!("noBuiltinTools", self.no_builtin_tools);
        opt!("extensions", self.extensions);
        opt!("noExtensions", self.no_extensions);
        opt!("print", self.print);
        opt!("export", self.export);
        opt!("noSkills", self.no_skills);
        opt!("skills", self.skills);
        opt!("promptTemplates", self.prompt_templates);
        opt!("noPromptTemplates", self.no_prompt_templates);
        opt!("themes", self.themes);
        opt!("noThemes", self.no_themes);
        opt!("noContextFiles", self.no_context_files);
        opt!("listModels", self.list_models);
        opt!("offline", self.offline);
        opt!("verbose", self.verbose);
        opt!("projectTrustOverride", self.project_trust_override);
        s.serialize_field("messages", &self.messages)?;
        s.serialize_field("fileArgs", &self.file_args)?;
        s.serialize_field("unknownFlags", &self.unknown_flags)?;
        s.serialize_field("diagnostics", &self.diagnostics)?;
        s.end()
    }
}

/// `VALID_THINKING_LEVELS` (TS `args.ts:57`) — this exact order feeds the warning string.
const VALID_THINKING_LEVELS: [(&str, ThinkingLevel); 7] = [
    ("off", ThinkingLevel::Off),
    ("minimal", ThinkingLevel::Minimal),
    ("low", ThinkingLevel::Low),
    ("medium", ThinkingLevel::Medium),
    ("high", ThinkingLevel::High),
    ("xhigh", ThinkingLevel::Xhigh),
    ("max", ThinkingLevel::Max),
];

/// TS `isValidThinkingLevel` (`args.ts:59-61`). Case-**sensitive**: `"HIGH"` is invalid.
pub fn is_valid_thinking_level(level: &str) -> bool {
    thinking_level_from_str(level).is_some()
}

/// The `level is ThinkingLevel` narrowing of [`is_valid_thinking_level`], as a value.
fn thinking_level_from_str(level: &str) -> Option<ThinkingLevel> {
    VALID_THINKING_LEVELS
        .iter()
        .find(|(name, _)| *name == level)
        .map(|(_, value)| *value)
}

/// `VALID_THINKING_LEVELS.join(", ")` (TS `args.ts:137`).
fn valid_thinking_levels_joined() -> String {
    VALID_THINKING_LEVELS
        .iter()
        .map(|(name, _)| *name)
        .collect::<Vec<_>>()
        .join(", ")
}

/// `split(",").map(trim)` — TS `args.ts:115` (`--models`, empty segments **kept**).
fn split_trim(value: &str) -> Vec<String> {
    value.split(',').map(|s| s.trim().to_string()).collect()
}

/// `split(",").map(trim).filter(len > 0)` — TS `args.ts:121-124` / `:126-129`.
fn split_trim_filter(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// TS `parseArgs` (`args.ts:63-210`) — pure: argv in, [`Args`] out.
///
/// No env, fs, cwd, TTY or clock is touched, verified empirically by the oracle
/// (all 129 corpus rows are byte-identical under a mutated env and a changed cwd).
///
/// The loop is a faithful transcription: `i` is advanced manually when a value is
/// consumed (`args[++i]`), each value-taking branch keeps its `i + 1 < args.length`
/// guard, and the four tail branches stay in Pi's order (`@…` before `--…` before
/// `-…` before "message") because that order decides `@--weird` (a file arg), `--`
/// (an unknown flag) and a bare `-` (`Unknown option: -`).
pub fn parse_args(args: &[String]) -> Args {
    let mut result = Args::default();

    let mut i = 0usize;
    while i < args.len() {
        let arg = args[i].as_str();

        if arg == "--help" || arg == "-h" {
            // args.ts:74-75
            result.help = Some(true);
        } else if arg == "--version" || arg == "-v" {
            // args.ts:76-77
            result.version = Some(true);
        } else if arg == "--mode" && i + 1 < args.len() {
            // args.ts:78-82 — an unrecognised mode is consumed and silently dropped.
            i += 1;
            match args[i].as_str() {
                "text" => result.mode = Some(Mode::Text),
                "json" => result.mode = Some(Mode::Json),
                "rpc" => result.mode = Some(Mode::Rpc),
                _ => {}
            }
        } else if arg == "--continue" || arg == "-c" {
            // args.ts:83-84
            result.r#continue = Some(true);
        } else if arg == "--resume" || arg == "-r" {
            // args.ts:85-86
            result.resume = Some(true);
        } else if arg == "--provider" && i + 1 < args.len() {
            // args.ts:87-88
            i += 1;
            result.provider = Some(args[i].clone());
        } else if arg == "--model" && i + 1 < args.len() {
            // args.ts:89-90 — the value is taken blindly, even `--print`.
            i += 1;
            result.model = Some(args[i].clone());
        } else if arg == "--api-key" && i + 1 < args.len() {
            // args.ts:91-92
            i += 1;
            result.api_key = Some(args[i].clone());
        } else if arg == "--system-prompt" && i + 1 < args.len() {
            // args.ts:93-94
            i += 1;
            result.system_prompt = Some(args[i].clone());
        } else if arg == "--append-system-prompt" && i + 1 < args.len() {
            // args.ts:95-97
            i += 1;
            result
                .append_system_prompt
                .get_or_insert_with(Vec::new)
                .push(args[i].clone());
        } else if arg == "--name" || arg == "-n" {
            // args.ts:98-103 — the only value-taking flag with its own diagnostic, and
            // the reason `-n` as the last token is NOT `Unknown option: -n`.
            if i + 1 < args.len() {
                i += 1;
                result.name = Some(args[i].clone());
            } else {
                result
                    .diagnostics
                    .push(Diagnostic::error("--name requires a value"));
            }
        } else if arg == "--no-session" {
            // args.ts:104-105
            result.no_session = Some(true);
        } else if arg == "--session" && i + 1 < args.len() {
            // args.ts:106-107
            i += 1;
            result.session = Some(args[i].clone());
        } else if arg == "--session-id" && i + 1 < args.len() {
            // args.ts:108-109
            i += 1;
            result.session_id = Some(args[i].clone());
        } else if arg == "--fork" && i + 1 < args.len() {
            // args.ts:110-111
            i += 1;
            result.fork = Some(args[i].clone());
        } else if arg == "--session-dir" && i + 1 < args.len() {
            // args.ts:112-113
            i += 1;
            result.session_dir = Some(args[i].clone());
        } else if arg == "--models" && i + 1 < args.len() {
            // args.ts:114-115 — NO `.filter()`: `"a,,b"` keeps the empty segment.
            i += 1;
            result.models = Some(split_trim(&args[i]));
        } else if arg == "--no-tools" || arg == "-nt" {
            // args.ts:116-117
            result.no_tools = Some(true);
        } else if arg == "--no-builtin-tools" || arg == "-nbt" {
            // args.ts:118-119
            result.no_builtin_tools = Some(true);
        } else if (arg == "--tools" || arg == "-t") && i + 1 < args.len() {
            // args.ts:120-124
            i += 1;
            result.tools = Some(split_trim_filter(&args[i]));
        } else if (arg == "--exclude-tools" || arg == "-xt") && i + 1 < args.len() {
            // args.ts:125-129
            i += 1;
            result.exclude_tools = Some(split_trim_filter(&args[i]));
        } else if arg == "--thinking" && i + 1 < args.len() {
            // args.ts:130-139 — invalid level: value still consumed, field left unset,
            // and a WARNING (not an error) is pushed.
            i += 1;
            let level = args[i].as_str();
            match thinking_level_from_str(level) {
                Some(value) => result.thinking = Some(value),
                None => result.diagnostics.push(Diagnostic::warning(format!(
                    "Invalid thinking level \"{level}\". Valid values: {}",
                    valid_thinking_levels_joined()
                ))),
            }
        } else if arg == "--print" || arg == "-p" {
            // args.ts:140-146 — optional-value lookahead. The `startsWith("---")`
            // clause means `-p ---foo` DOES swallow `---foo` as a message.
            result.print = Some(true);
            if let Some(next) = args.get(i + 1) {
                if !next.starts_with('@') && (!next.starts_with('-') || next.starts_with("---")) {
                    result.messages.push(next.clone());
                    i += 1;
                }
            }
        } else if arg == "--export" && i + 1 < args.len() {
            // args.ts:147-148 — takes ONE value; `--export a b` leaves `b` a message.
            i += 1;
            result.export = Some(args[i].clone());
        } else if (arg == "--extension" || arg == "-e") && i + 1 < args.len() {
            // args.ts:149-151
            i += 1;
            result
                .extensions
                .get_or_insert_with(Vec::new)
                .push(args[i].clone());
        } else if arg == "--no-extensions" || arg == "-ne" {
            // args.ts:152-153
            result.no_extensions = Some(true);
        } else if arg == "--skill" && i + 1 < args.len() {
            // args.ts:154-156
            i += 1;
            result
                .skills
                .get_or_insert_with(Vec::new)
                .push(args[i].clone());
        } else if arg == "--prompt-template" && i + 1 < args.len() {
            // args.ts:157-159
            i += 1;
            result
                .prompt_templates
                .get_or_insert_with(Vec::new)
                .push(args[i].clone());
        } else if arg == "--theme" && i + 1 < args.len() {
            // args.ts:160-162
            i += 1;
            result
                .themes
                .get_or_insert_with(Vec::new)
                .push(args[i].clone());
        } else if arg == "--no-skills" || arg == "-ns" {
            // args.ts:163-164
            result.no_skills = Some(true);
        } else if arg == "--no-prompt-templates" || arg == "-np" {
            // args.ts:165-166
            result.no_prompt_templates = Some(true);
        } else if arg == "--no-themes" {
            // args.ts:167-168 — no short alias.
            result.no_themes = Some(true);
        } else if arg == "--no-context-files" || arg == "-nc" {
            // args.ts:169-170
            result.no_context_files = Some(true);
        } else if arg == "--list-models" {
            // args.ts:171-177 — optional value; note there is NO `---` escape hatch
            // here, unlike `-p`. An empty-string token IS taken as the pattern.
            if i + 1 < args.len() && !args[i + 1].starts_with('-') && !args[i + 1].starts_with('@')
            {
                i += 1;
                result.list_models = Some(ListModels::Pattern(args[i].clone()));
            } else {
                result.list_models = Some(ListModels::All);
            }
        } else if arg == "--verbose" {
            // args.ts:178-179
            result.verbose = Some(true);
        } else if arg == "--approve" || arg == "-a" {
            // args.ts:180-181
            result.project_trust_override = Some(true);
        } else if arg == "--no-approve" || arg == "-na" {
            // args.ts:182-183
            result.project_trust_override = Some(false);
        } else if arg == "--offline" {
            // args.ts:184-185 (also pre-scanned from raw argv at main.ts:476)
            result.offline = Some(true);
        } else if let Some(file) = arg.strip_prefix('@') {
            // T1, args.ts:186-187 — BEFORE the `--` branch, so `@--weird` is a file arg
            // and a bare `@` yields the empty string.
            result.file_args.push(file.to_string());
        } else if let Some(rest) = arg.strip_prefix("--") {
            // T2, args.ts:188-201 — the unknown-long-flag algorithm, verbatim.
            //
            // TS scans the WHOLE token (`arg.indexOf("=")`) and then slices from 2, but
            // the first two characters are dashes so the first `=` always sits at index
            // >= 2 and scanning `rest` splits at exactly the same place: key =
            // `arg.slice(2, eqIndex)`, value = `arg.slice(eqIndex + 1)`. Hence `--=x`
            // gives the empty key and `--a=b=c` splits on the FIRST `=`.
            match rest.find('=') {
                Some(eq_index) => {
                    result.unknown_flags.insert(
                        &rest[..eq_index],
                        FlagValue::Str(rest[eq_index + 1..].into()),
                    );
                }
                None => {
                    let flag_name = rest;
                    match args.get(i + 1) {
                        // Greedily eats the next token unless it starts with `-`/`@`.
                        Some(next) if !next.starts_with('-') && !next.starts_with('@') => {
                            result
                                .unknown_flags
                                .insert(flag_name, FlagValue::Str(next.clone()));
                            i += 1;
                        }
                        _ => result
                            .unknown_flags
                            .insert(flag_name, FlagValue::Bool(true)),
                    }
                }
            }
        } else if arg.starts_with('-') && !arg.starts_with("--") {
            // T3, args.ts:202-203 — the only fatal parse diagnostic besides `--name`.
            // A bare `-` lands here, not in `messages`.
            result
                .diagnostics
                .push(Diagnostic::error(format!("Unknown option: {arg}")));
        } else if !arg.starts_with('-') {
            // T4, args.ts:204-206 — the empty string reaches here and becomes a
            // (empty) message.
            result.messages.push(arg.to_string());
        }

        i += 1;
    }

    result
}

/// The identity strings `printHelp` interpolates (TS `config.ts:489-496`).
///
/// Parameterised rather than hardcoded: production pirust renders the same template
/// with [`PIRUST`], while the golden suite renders it with Pi's values from
/// `tests/fixtures/pi/cli/help.identity.json` — which is what proves the template
/// itself is byte-exact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppIdentity<'a> {
    /// `APP_NAME` (`config.ts:489`) — 27 occurrences in the help text.
    pub app_name: &'a str,
    /// `CONFIG_DIR_NAME` (`config.ts:491`) — 2 occurrences.
    pub config_dir_name: &'a str,
    /// `ENV_AGENT_DIR` (`config.ts:495`), rendered `.padEnd(32)`.
    pub env_agent_dir: &'a str,
    /// `ENV_SESSION_DIR` (`config.ts:496`), rendered `.padEnd(32)`.
    pub env_session_dir: &'a str,
    /// `VERSION` (`config.ts:492`). Not in the help text — it is `--version`'s output
    /// (`main.ts:522`) — but it belongs to the same identity block.
    pub version: &'a str,
}

/// pirust's identity. Same template, different state directory (see the crate docs).
pub const PIRUST: AppIdentity<'static> = AppIdentity {
    app_name: "pirust",
    config_dir_name: ".pirust",
    env_agent_dir: "PIRUST_CODING_AGENT_DIR",
    env_session_dir: "PIRUST_CODING_AGENT_SESSION_DIR",
    version: env!("CARGO_PKG_VERSION"),
};

/// `ExtensionFlag.type` (TS `core/extensions/types.ts:1501`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExtensionFlagType {
    Boolean,
    String,
}

/// A CLI flag registered by an extension (TS `core/extensions/types.ts:1498-1504`).
///
/// Only [`render_help`] consumes it in feat-005; the loader lands in feat-007.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionFlag {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(rename = "type")]
    pub flag_type: ExtensionFlagType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<FlagValue>,
    pub extension_path: String,
}

/// `chalk.bold` — SGR 1, closed with SGR 22 (chalk's `boldOff`, not the blanket `0`).
///
/// Confirmed against `help.color.golden`: it is exactly 63 bytes longer than
/// `help.plain.golden` = 7 × (4 + 5) for the seven body bolds, and the `.ext.` pair
/// differ by 72 = 8 × 9 with `extensionFlagsText`'s heading included.
fn bold(text: &str, color: bool) -> String {
    if color {
        format!("\u{1b}[1m{text}\u{1b}[22m")
    } else {
        text.to_string()
    }
}

/// `extensionFlagsText` (TS `args.ts:213-222`).
///
/// `` `  --${name}${value}`.padEnd(30) + description `` — `padEnd` counts UTF-16 code
/// units and Rust's `{:<30}` counts `char`s; identical for the ASCII flag names Pi
/// accepts, and neither truncates an over-long prefix.
fn extension_flags_text(extension_flags: &[ExtensionFlag], color: bool) -> String {
    if extension_flags.is_empty() {
        return String::new();
    }
    let lines = extension_flags
        .iter()
        .map(|flag| {
            let value = if flag.flag_type == ExtensionFlagType::String {
                " <value>"
            } else {
                ""
            };
            let description = flag
                .description
                .clone()
                .unwrap_or_else(|| format!("Registered by {}", flag.extension_path));
            let prefix = format!("  --{}{}", flag.name, value);
            format!("{prefix:<30}{description}")
        })
        .collect::<Vec<_>>()
        .join("\n");
    let heading = bold("Extension CLI Flags:", color);
    format!("\n{heading}\n{lines}\n")
}

/// The exact bytes `printHelp` writes to stdout (TS `args.ts:212-390`).
///
/// The template literal spans `args.ts:223-389` — 166 content lines plus the newline
/// before its closing backtick — and `console.log` appends one more newline, so the
/// output ends with a blank line. Copied byte-for-byte; the column-33 alignment of the
/// option/env/tool descriptions is hand-written spaces in Pi, not `padEnd`, and is
/// preserved as such. The only computed padding is the two env-var names (`.padEnd(32)`)
/// and the extension-flag lines (`.padEnd(30)`).
///
/// `color` stands in for chalk's TTY detection: chalk emits no escapes when stdout is
/// not a TTY, which is why the plain goldens are escape-free.
pub fn render_help(
    identity: &AppIdentity<'_>,
    extension_flags: &[ExtensionFlag],
    color: bool,
) -> String {
    let app = identity.app_name;
    let cfg = identity.config_dir_name;
    let ext = extension_flags_text(extension_flags, color);
    // `.padEnd(32)` (args.ts:374-375): UTF-16 units in JS, `char`s here — both
    // identifiers are ASCII, so byte-identical.
    let env_agent = format!("{:<32}", identity.env_agent_dir);
    let env_session = format!("{:<32}", identity.env_session_dir);
    let body = format!(
        r#"{title} - AI coding assistant with read, bash, edit, write tools

{h_usage}
  {app} [options] [@files...] [messages...]

{h_commands}
  {app} install <source> [-l]     Install extension source and add to settings
  {app} remove <source> [-l]      Remove extension source from settings
  {app} uninstall <source> [-l]   Alias for remove
  {app} update [source|self|pi]   Update pi, extensions, or model catalogs
  {app} list                      List installed extensions from settings
  {app} config [-l]               Open TUI to enable/disable package resources (Tab switches scope)
  {app} <command> --help          Show help for install/remove/uninstall/update/list/config

{h_options}
  --provider <name>              Provider name (default: google)
  --model <pattern>              Model pattern or ID (supports "provider/id" and optional ":<thinking>")
  --api-key <key>                API key (defaults to env vars)
  --system-prompt <text>         System prompt (default: coding assistant prompt)
  --append-system-prompt <text>  Append text or file contents to the system prompt (can be used multiple times)
  --mode <mode>                  Output mode: text (default), json, or rpc
  --print, -p                    Non-interactive mode: process prompt and exit
  --continue, -c                 Continue previous session
  --resume, -r                   Select a session to resume
  --session <path|id>            Use specific session file or partial UUID
  --session-id <id>              Use exact project session ID, creating it if missing
  --fork <path|id>               Fork specific session file or partial UUID into a new session
  --session-dir <dir>            Directory for session storage and lookup
  --no-session                   Don't save session (ephemeral)
  --name, -n <name>              Set session display name
  --models <patterns>            Comma-separated model patterns for Ctrl+P cycling
                                 Supports globs (anthropic/*, *sonnet*) and fuzzy matching
  --no-tools, -nt                Disable all tools by default (built-in and extension)
  --no-builtin-tools, -nbt       Disable built-in tools by default but keep extension/custom tools enabled
  --tools, -t <tools>            Comma-separated allowlist of tool names to enable
                                 Applies to built-in, extension, and custom tools
  --exclude-tools, -xt <tools>   Comma-separated denylist of tool names to disable
                                 Applies to built-in, extension, and custom tools
  --thinking <level>             Set thinking level: off, minimal, low, medium, high, xhigh, max
  --extension, -e <path>         Load an extension file (can be used multiple times)
  --no-extensions, -ne           Disable extension discovery (explicit -e paths still work)
  --skill <path>                 Load a skill file or directory (can be used multiple times)
  --no-skills, -ns               Disable skills discovery and loading
  --prompt-template <path>       Load a prompt template file or directory (can be used multiple times)
  --no-prompt-templates, -np     Disable prompt template discovery and loading
  --theme <path>                 Load a theme file or directory (can be used multiple times)
  --no-themes                    Disable theme discovery and loading
  --no-context-files, -nc        Disable AGENTS.md and CLAUDE.md discovery and loading
  --export <file>                Export session file to HTML and exit
  --list-models [search]         List available models (with optional fuzzy search)
  --verbose                      Force verbose startup (overrides quietStartup setting)
  --approve, -a                  Trust project-local files for this run
  --no-approve, -na              Ignore project-local files for this run
  --offline                      Disable startup network operations (same as PI_OFFLINE=1)
  --help, -h                     Show this help
  --version, -v                  Show version number

Extensions can register additional flags (e.g., --plan from plan-mode extension).{ext}

{h_examples}
  # Interactive mode
  {app}

  # Interactive mode with initial prompt
  {app} "List all .ts files in src/"

  # Include files in initial message
  {app} @prompt.md @image.png "What color is the sky?"

  # Non-interactive mode (process and exit)
  {app} -p "List all .ts files in src/"

  # Multiple messages (interactive)
  {app} "Read package.json" "What dependencies do we have?"

  # Continue previous session
  {app} --continue "What did we discuss?"

  # Start a named session
  {app} --name "Refactor auth module"

  # Use different model
  {app} --provider openai --model gpt-4o-mini "Help me refactor this code"

  # Use model with provider prefix (no --provider needed)
  {app} --model openai/gpt-4o "Help me refactor this code"

  # Use model with thinking level shorthand
  {app} --model sonnet:high "Solve this complex problem"

  # Limit model cycling to specific models
  {app} --models claude-sonnet,claude-haiku,gpt-4o

  # Limit to a specific provider with glob pattern
  {app} --models "github-copilot/*"

  # Cycle models with fixed thinking levels
  {app} --models sonnet:high,haiku:low

  # Start with a specific thinking level
  {app} --thinking high "Solve this complex problem"

  # Read-only mode (no file modifications possible)
  {app} --tools read,grep,find,ls -p "Review the code in src/"

  # Disable one tool while keeping the rest available
  {app} --exclude-tools ask_question

  # Export a session file to HTML
  {app} --export ~/{cfg}/agent/sessions/--path--/session.jsonl
  {app} --export session.jsonl output.html

{h_env}
  ANTHROPIC_API_KEY                - Anthropic Claude API key
  ANTHROPIC_OAUTH_TOKEN            - Anthropic OAuth token (alternative to API key)
  ANT_LING_API_KEY                 - Ant Ling API key
  OPENAI_API_KEY                   - OpenAI GPT API key
  AZURE_OPENAI_API_KEY             - Azure OpenAI API key
  AZURE_OPENAI_BASE_URL            - Azure OpenAI/Cognitive Services base URL (e.g. https://{{resource}}.openai.azure.com)
  AZURE_OPENAI_RESOURCE_NAME       - Azure OpenAI resource name (alternative to base URL)
  AZURE_OPENAI_API_VERSION         - Azure OpenAI API version (default: v1)
  AZURE_OPENAI_DEPLOYMENT_NAME_MAP - Azure OpenAI model=deployment map (comma-separated)
  DEEPSEEK_API_KEY                 - DeepSeek API key
  NVIDIA_API_KEY                   - NVIDIA NIM API key
  GEMINI_API_KEY                   - Google Gemini API key
  GROQ_API_KEY                     - Groq API key
  CEREBRAS_API_KEY                 - Cerebras API key
  XAI_API_KEY                      - xAI Grok API key
  FIREWORKS_API_KEY                - Fireworks API key
  TOGETHER_API_KEY                 - Together AI API key
  OPENROUTER_API_KEY               - OpenRouter API key
  AI_GATEWAY_API_KEY               - Vercel AI Gateway API key
  ZAI_API_KEY                      - ZAI Coding Plan API key (Global)
  ZAI_CODING_CN_API_KEY            - ZAI Coding Plan API key (China)
  MISTRAL_API_KEY                  - Mistral API key
  MINIMAX_API_KEY                  - MiniMax API key
  MOONSHOT_API_KEY                 - Moonshot AI API key
  OPENCODE_API_KEY                 - OpenCode Zen/OpenCode Go API key
  KIMI_API_KEY                     - Kimi For Coding API key
  CLOUDFLARE_API_KEY               - Cloudflare API token (Workers AI and AI Gateway)
  CLOUDFLARE_ACCOUNT_ID            - Cloudflare account id (required for both)
  CLOUDFLARE_GATEWAY_ID            - Cloudflare AI Gateway slug (required for AI Gateway)
  XIAOMI_API_KEY                   - Xiaomi MiMo API key (api.xiaomimimo.com billing)
  XIAOMI_TOKEN_PLAN_CN_API_KEY     - Xiaomi MiMo Token Plan API key (China region)
  XIAOMI_TOKEN_PLAN_AMS_API_KEY    - Xiaomi MiMo Token Plan API key (Amsterdam region)
  XIAOMI_TOKEN_PLAN_SGP_API_KEY    - Xiaomi MiMo Token Plan API key (Singapore region)
  AWS_PROFILE                      - AWS profile for Amazon Bedrock
  AWS_ACCESS_KEY_ID                - AWS access key for Amazon Bedrock
  AWS_SECRET_ACCESS_KEY            - AWS secret key for Amazon Bedrock
  AWS_BEARER_TOKEN_BEDROCK         - Bedrock API key (bearer token)
  AWS_REGION                       - AWS region for Amazon Bedrock (e.g., us-east-1)
  {env_agent} - Config directory (default: ~/{cfg}/agent)
  {env_session} - Session storage directory (overridden by --session-dir)
  PI_PACKAGE_DIR                   - Override package directory (for Nix/Guix store paths)
  PI_OFFLINE                       - Disable startup network operations when set to 1/true/yes
  PI_TELEMETRY                     - Override install telemetry when set to 1/true/yes or 0/false/no
  PI_SHARE_VIEWER_URL              - Base URL for /share command (default: https://pi.dev/session/)

{h_tools}
  read   - Read file contents
  bash   - Execute bash commands
  edit   - Edit files with find/replace
  write  - Write files (creates/overwrites)
  grep   - Search file contents (read-only, off by default)
  find   - Find files by glob pattern (read-only, off by default)
  ls     - List directory contents (read-only, off by default)
"#,
        title = bold(app, color),
        h_usage = bold("Usage:", color),
        h_commands = bold("Commands:", color),
        h_options = bold("Options:", color),
        h_examples = bold("Examples:", color),
        h_env = bold("Environment Variables:", color),
        h_tools = bold("Built-in Tool Names:", color),
    );
    // The template literal itself ends in a newline (its last line, `args.ts:389`, is
    // empty) and `console.log` appends a second one — hence the trailing blank line in
    // every captured golden.
    format!("{body}\n")
}

/// TS `printHelp` (`args.ts:212-390`) — `console.log` of [`render_help`] to stdout.
///
/// `render_help` already carries the newline `console.log` appends, hence `print!`.
pub fn print_help(identity: &AppIdentity<'_>, extension_flags: &[ExtensionFlag], color: bool) {
    print!("{}", render_help(identity, extension_flags, color));
}

impl fmt::Display for Mode {
    /// The wire strings of TS `Mode` (`args.ts:10`).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Mode::Text => "text",
            Mode::Json => "json",
            Mode::Rpc => "rpc",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn empty_argv_yields_only_the_four_collections() {
        let args = parse_args(&[]);
        assert_eq!(args, Args::default());
        assert!(args.messages.is_empty());
        assert!(args.file_args.is_empty());
        assert!(args.unknown_flags.is_empty());
        assert!(args.diagnostics.is_empty());
    }

    #[test]
    fn unknown_flags_keep_insertion_position_on_overwrite() {
        // Deliberately inserted in reverse-alphabetical order, so a sorted map would
        // fail here and not only on the corpus row.
        let mut flags = UnknownFlags::default();
        flags.insert("b", FlagValue::Str("1".into()));
        flags.insert("a", FlagValue::Bool(true));
        flags.insert("b", FlagValue::Str("2".into()));
        assert_eq!(
            flags.entries(),
            &[
                ("b".to_string(), FlagValue::Str("2".into())),
                ("a".to_string(), FlagValue::Bool(true)),
            ]
        );
        assert_eq!(flags.get("b"), Some(&FlagValue::Str("2".into())));
        assert_eq!(flags.get("zz"), None);
        assert_eq!(flags.len(), 2);
        assert_eq!(flags.iter().count(), 2);
        assert_eq!((&flags).into_iter().count(), 2);
    }

    #[test]
    fn thinking_level_validation_is_case_sensitive() {
        assert!(is_valid_thinking_level("xhigh"));
        assert!(!is_valid_thinking_level("XHIGH"));
        assert_eq!(
            valid_thinking_levels_joined(),
            "off, minimal, low, medium, high, xhigh, max"
        );
    }

    #[test]
    fn mode_display_matches_the_wire_strings() {
        assert_eq!(Mode::Text.to_string(), "text");
        assert_eq!(Mode::Json.to_string(), "json");
        assert_eq!(Mode::Rpc.to_string(), "rpc");
    }

    #[test]
    fn known_flag_with_equals_is_an_unknown_flag() {
        let args = parse_args(&argv(&["--model=sonnet"]));
        assert_eq!(args.model, None);
        assert_eq!(
            args.unknown_flags.get("model"),
            Some(&FlagValue::Str("sonnet".into()))
        );
    }

    #[test]
    fn pirust_identity_renders_the_same_template() {
        let help = render_help(&PIRUST, &[], false);
        assert!(help.starts_with("pirust - AI coding assistant"));
        assert!(help.contains(
            "  PIRUST_CODING_AGENT_DIR          - Config directory (default: ~/.pirust/agent)\n"
        ));
        // 31 chars + padEnd(32) = one trailing space before the separator.
        assert!(help.contains("  PIRUST_CODING_AGENT_SESSION_DIR  - Session storage directory"));
        assert!(help.ends_with("off by default)\n\n"));
    }
}
