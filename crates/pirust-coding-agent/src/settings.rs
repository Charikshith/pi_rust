//! Port of `core/settings-manager.ts` — the ~45-field `Settings` schema, the layered
//! global+project merge, and `migrateSettings`.
//!
//! `deepMergeSettings` is ONE level deep and arrays REPLACE (its own doc comment says
//! "recursively" and is wrong — runtime wins). Gated by `settings.merge.cases.jsonl`.
//!
//! # What is ported
//!
//! | Pi | here |
//! |---|---|
//! | `Settings` + the 9 nested interfaces (`settings-manager.ts:11-129`) | [`Settings`] &co. |
//! | `deepMergeSettings` (`:132-160`) | [`deep_merge_settings`] |
//! | `migrateSettings` (`:381-440`) | [`migrate_settings`] |
//! | `SettingsScope` (`:173`) | [`SettingsScope`] |
//! | `SettingsError` (`:183-186`) | [`SettingsError`] |
//! | `SettingsStorage` (`:179-181`) | [`SettingsStorage`] |
//! | `FileSettingsStorage` (`:188-255`) | [`FileSettingsStorage`] |
//! | `InMemorySettingsStorage` (`:257-272`) | [`InMemorySettingsStorage`] |
//! | `SettingsManager` (`:274-1234`) | [`SettingsManager`] |
//! | `parseTimeoutSetting` (`:162-171`) | [`SettingsManager::get_http_idle_timeout_ms`] |
//! | `collectSettingsDiagnostics` (`main.ts:77-85`) | [`SettingsError::diagnostic_message`] |
//!
//! # The merged view is a raw JSON map, not a typed struct
//!
//! Pi's `Settings` is a TypeScript *interface*: the types are erased at runtime, so
//! `JSON.parse` hands `SettingsManager` whatever was on disk — a string where a nested
//! object was declared, an array where a scalar was, or a top-level array instead of an
//! object (`settings.merge.cases.jsonl`'s `malformed:global-is-a-json-array` pins that
//! last one). Every merge, migration and write in this module therefore runs over
//! [`serde_json::Value`] / [`SettingsMap`], exactly like Pi's runtime, which also means:
//!
//! - **on-disk key order is preserved** (the crate enables serde_json's `preserve_order`,
//!   so `Map` is `IndexMap`-backed and `persistScopedSettings`' `{...currentFileSettings}`
//!   really does keep the user's ordering, `:588`);
//! - **unknown keys round-trip** untouched, as they do through `JSON.parse`/`stringify`;
//! - **no load can fail on a type mismatch**, matching Pi, which never validates.
//!
//! [`Settings`] is a *typed lens* over that map ([`Settings::from_map`]) for consumers who
//! want fields rather than `Value`s. It is deliberately not on the load/save path: a file
//! holding `{"terminal":"nope"}` deserializes into no Rust struct, yet Pi keeps running,
//! so making the pipeline depend on the struct would invent a failure Pi does not have.
//!
//! # `undefined` vs `null` vs absent
//!
//! `deepMergeSettings` distinguishes three states and the fixture pins all three
//! (`undefined-in-project-is-SKIPPED`, `null-in-project-OVERWRITES`,
//! `absent-in-project-keeps-base`, `undefined-in-global-is-COPIED-BY-THE-SPREAD`,
//! `nested-undefined-value-inside-an-object-merge`). JSON expresses two of them; JS
//! `undefined` — a key that is **present with no value** — it cannot express at all, and
//! `serde_json::Value` has no variant for it (`Value::Null` is `null`, a *different* state
//! that `deepMergeSettings` treats as a real value and copies).
//!
//! Modelled here as the single-key sentinel object **`{"$undefined": true}`**
//! ([`js_undefined`] / [`is_js_undefined`]) — the same encoding the captured fixture uses,
//! so fixture rows are replayed verbatim with no translation step. Every JS type test in
//! this module goes through [`is_js_plain_object`], which excludes the sentinel, so a
//! sentinel behaves as `typeof x === "undefined"` and not as an object:
//!
//! | state | on disk | here |
//! |---|---|---|
//! | absent | key not written | key not in the [`SettingsMap`] |
//! | `null` | `"k": null` | `Value::Null` |
//! | `undefined` | *inexpressible* | `{"$undefined": true}` |
//!
//! [`json_stringify`] drops sentinel-valued keys and maps sentinel array elements to
//! `null`, which is exactly what `JSON.stringify` does with `undefined` — that is how
//! `setShellPath(undefined)` (`:883-887`, an assignment of `undefined` to a *present* key)
//! ends up deleting the key from the file.
//!
//! **The one divergence:** a settings file containing the literal key/value
//! `{"theme": {"$undefined": true}}` is read by Pi as a plain object and by pirust as
//! `undefined`. There is no encoding that avoids this — the state pirust must represent
//! does not exist in JSON, so any in-band marker collides with some legal document. The
//! sentinel is as narrow as it can be made (a one-key object, key `$undefined`, value
//! exactly `true`) and is unreachable through Pi's own writers, which never emit it.
//!
//! # Malformed settings files, and the V8 message divergence
//!
//! A load failure is **captured, not thrown** (`tryLoadFromStorage`, `:368-378`): the scope
//! falls back to `{}`, the error is queued per-scope and the caller *drains* it later
//! ([`SettingsManager::drain_errors`], `:654-658`) into
//! `` `(${context}, ${scope} settings) ${error.message}` `` warnings (`main.ts:77-85`).
//! A scope that failed to load is also never written back, so a broken file is not
//! clobbered (`:612-614`, `:630-632`).
//!
//! Two error shapes reach the drain, and only one is reproducible:
//!
//! - **`TypeError: Cannot use 'in' operator to search for 'queueMode' in <value>`** —
//!   `migrateSettings`' first statement is `"queueMode" in settings` (`:383`), which throws
//!   on a primitive, so `settings.json` containing `null`, `123` or `"hi"` produces this.
//!   The wording is V8's, but it is fully deterministic, so
//!   [`SettingsErrorKind::InOperatorOnPrimitive`] reproduces it **verbatim** — the fixture
//!   marks those three rows `v8Dependent: true` yet they are asserted on text.
//! - **`SyntaxError` from `JSON.parse`** — e.g. V8's
//!   `Expected property name or '}' in JSON at position 2 (line 1 column 3)`. `serde_json`
//!   reports the same defect with its own wording (`key must be a string at line 1
//!   column 3`) and there is no way to recover V8's phrasing without shipping a second JSON
//!   parser. Four fixture rows (`malformed:global-unparseable-json`,
//!   `malformed:project-unparseable-json`, `malformed:both-scopes-unparseable`,
//!   `malformed:global-truncated-object`) are therefore asserted on *structure* — which
//!   scope, how many errors, the error class, the diagnostic prefix, and that the load
//!   still succeeded with `{}` — with the message text explicitly recorded as divergent in
//!   `tests/settings_golden.rs` (four rows, five queued errors: `both-scopes-unparseable`
//!   queues two). No V8 strings are fabricated and no row is deleted.
//!
//! # Not ported
//!
//! - **The `writeQueue` promise chain** (`:286`, `:556-568`). Pi defers each write behind a
//!   promise so `save()` returns before the file is touched and a write failure surfaces
//!   later via `drainErrors()`. Writes here happen synchronously *in the same order*, with
//!   failures recorded into the same queue, so the drained sequence is identical; only the
//!   moment of the syscall differs. [`SettingsManager::flush`] is consequently a no-op that
//!   exists so callers can keep Pi's shape.
//! - **The interactive setters/getters** — `terminal.*`, `images.*`, `markdown.*`,
//!   `warnings.*`, `doubleEscapeAction`, `treeFilterMode`, `editorPaddingX`, `outputPad`,
//!   `autocompleteMaxVisible`, `showHardwareCursor`, `hideThinkingBlock`,
//!   `showCacheMissNotices`, `externalEditor`, `shellPath`, `theme`, `packages`,
//!   `extensions`, `skills`, `prompts`, `themes`, `enableSkillCommands`, `npmCommand`,
//!   `shellCommandPrefix`, `enableInstallTelemetry`, `enableAnalytics`. Spec §6.1 lists the
//!   getters feat-005 actually reads; those are implemented. The rest are one-line reads of
//!   the merged map plus a `markModified`, and both primitives are public
//!   ([`SettingsManager::get_merged_field`], [`SettingsManager::set_global_field`],
//!   [`SettingsManager::set_global_nested_field`]), so feat-006/007 adds them without
//!   touching the merge, the migration or the writer. Every field still parses, merges and
//!   round-trips.
//! - **`getExternalEditorCommand`** (`:854-864`) additionally reads `VISUAL`/`EDITOR` and
//!   `process.platform`; it belongs with the Ctrl+G editor in feat-006/007, which owns that
//!   env seam.
//! - **`proper-lockfile`'s exact retry telemetry.** [`FileSettingsStorage`] keeps Pi's
//!   10 attempts × 20 ms budget and its lock *shape* (a `<path>.lock` directory, which is
//!   what `proper-lockfile` creates), but sleeps instead of busy-waiting — Pi spins only to
//!   avoid making its callers async (`:216-219`). Spec §6.2 explicitly puts the lock-file
//!   layout outside the byte contract.
//! - **Keybindings.** `migrateSettings` does not touch keybinding names, so the
//!   `KEYBINDING_NAME_MIGRATIONS` / `KEYBINDINGS` tables transcribed in
//!   [`crate::migrations`] are not duplicated or needed here.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::{Number, Value};

use pirust_agent_core::types::ThinkingLevel;
use pirust_ai::types::{ThinkingBudgets, Transport};

use crate::config::{ConfigEnv, ConfigPathError, CONFIG_DIR_NAME, SETTINGS_FILE_NAME};

/// An order-preserving JSON object — what `JSON.parse` hands Pi and what every function in
/// this module actually merges, migrates and writes.
///
/// `serde_json::Map` is `IndexMap`-backed here (the crate enables `preserve_order`), so
/// insertion order *is* JS object key order for the string keys settings use. Divergence,
/// for completeness: JS enumerates integer-like keys first, in ascending numeric order,
/// before the insertion-ordered string keys. No `Settings` field is integer-like and no
/// fixture row has such a key, so the two orders coincide on every real document.
pub type SettingsMap = serde_json::Map<String, Value>;

// =============================================================================
// JS value semantics (`undefined`, `typeof`, spread, `String(x)`)
// =============================================================================

/// The only key of the sentinel object standing in for JS `undefined`.
pub const UNDEFINED_SENTINEL_KEY: &str = "$undefined";

/// JS `undefined` as a [`Value`] — see the module docs on the three-state modelling.
pub fn js_undefined() -> Value {
    let mut map = SettingsMap::new();
    map.insert(UNDEFINED_SENTINEL_KEY.to_string(), Value::Bool(true));
    Value::Object(map)
}

/// Is this value the `undefined` sentinel (and not merely an object that mentions it)?
pub fn is_js_undefined(value: &Value) -> bool {
    match value {
        Value::Object(map) => {
            map.len() == 1 && map.get(UNDEFINED_SENTINEL_KEY) == Some(&Value::Bool(true))
        }
        _ => false,
    }
}

/// `typeof v === "object" && v !== null && !Array.isArray(v)` — the guard
/// `deepMergeSettings` (`:145-151`) and `migrateSettings` (`:397-399`, `:418-420`) use.
///
/// `Value::Null` is not `is_object()` in serde_json, so the `!== null` half is free; the
/// sentinel is excluded because its `typeof` is `"undefined"`, not `"object"`.
pub fn is_js_plain_object(value: &Value) -> bool {
    value.is_object() && !is_js_undefined(value)
}

/// The `[key, value]` pairs a JS spread (`{...source}`) or `Object.keys(source)` yields.
///
/// Objects give their entries; arrays give `"0".."n-1"`; strings give their characters by
/// index (Pi indexes UTF-16 code units, we index `char`s — only reachable when a settings
/// file holds a bare string where an object was declared); `null`, booleans and numbers
/// have no own enumerable properties and give nothing.
fn own_enumerable_entries(source: &Value) -> Vec<(String, Value)> {
    match source {
        Value::Object(map) => map.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        Value::Array(items) => items
            .iter()
            .enumerate()
            .map(|(i, v)| (i.to_string(), v.clone()))
            .collect(),
        Value::String(text) => text
            .chars()
            .enumerate()
            .map(|(i, c)| (i.to_string(), Value::String(c.to_string())))
            .collect(),
        _ => Vec::new(),
    }
}

/// `Object.assign(target, source)`, i.e. the tail of a `{...target, ...source}` spread.
///
/// It does **not** skip `undefined` values: the fixture's
/// `nested-undefined-value-inside-an-object-merge` row pins that `terminal.showImages`
/// really does become `undefined` when the project file sets it so.
fn spread_into(target: &mut SettingsMap, source: &Value) {
    for (key, value) in own_enumerable_entries(source) {
        target.insert(key, value);
    }
}

/// `{...source}` as a fresh map.
fn spread(source: &Value) -> SettingsMap {
    let mut map = SettingsMap::new();
    spread_into(&mut map, source);
    map
}

/// JS member access `container[key]`, yielding `None` for JS `undefined`.
///
/// Property access on a primitive does not throw in JS (only `in` does), it just yields
/// `undefined`, hence the catch-all arm.
fn js_get<'a>(container: &'a Value, key: &str) -> Option<&'a Value> {
    match container {
        Value::Object(map) => map.get(key).filter(|value| !is_js_undefined(value)),
        Value::Array(items) => key
            .parse::<usize>()
            .ok()
            .and_then(|index| items.get(index))
            .filter(|value| !is_js_undefined(value)),
        _ => None,
    }
}

/// `String(value)` for the primitives that can reach a `TypeError` message (`:383`).
///
/// Numbers use serde_json's `Display`, which agrees with JS for every value a settings file
/// realistically holds. It diverges only in exponent formatting — JS prints `1e21` as
/// `1e+21` — which cannot be reached without a settings file whose entire content is a
/// number that large.
fn js_string(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => number.to_string(),
        Value::String(text) => text.clone(),
        // Objects and arrays never reach here: `in` accepts them.
        other => other.to_string(),
    }
}

/// JS truthiness — used by the getters Pi wrote with `||` rather than `??`.
fn js_truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(flag)) => *flag,
        Some(Value::Number(number)) => number.as_f64().is_some_and(|n| n != 0.0 && !n.is_nan()),
        Some(Value::String(text)) => !text.is_empty(),
        Some(value) => !is_js_undefined(value),
    }
}

/// `JSON.stringify(value, null, 2)`.
///
/// Two things beyond `to_string_pretty` (which is byte-identical to Node's two-space form —
/// `": "` after each key, `{}`/`[]` for empties, **no trailing newline**; see
/// `auth::serialize_storage_data`, which pins the same convention for `auth.json`):
/// `undefined` object entries are **dropped** and `undefined` array elements become `null`,
/// as `JSON.stringify` does.
///
/// `settings.json` takes no trailing newline — `persistScopedSettings` returns the
/// `JSON.stringify` result straight to `writeFileSync` (`:605`), with none of the `+ "\n"`
/// that `migrateKeybindingsConfigFile` adds for `keybindings.json`.
pub fn json_stringify(value: &Value) -> String {
    serde_json::to_string_pretty(&strip_undefined(value))
        .expect("a serde_json::Value always serializes")
}

/// Recursively apply `JSON.stringify`'s treatment of `undefined`.
fn strip_undefined(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .filter(|(_, entry)| !is_js_undefined(entry))
                .map(|(key, entry)| (key.clone(), strip_undefined(entry)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| {
                    if is_js_undefined(item) {
                        Value::Null
                    } else {
                        strip_undefined(item)
                    }
                })
                .collect(),
        ),
        other => other.clone(),
    }
}

// =============================================================================
// Defaults (`settings-manager.ts:11-60` comments + each getter's `??`)
// =============================================================================

/// `compaction.enabled` default (`:761`).
pub const DEFAULT_COMPACTION_ENABLED: bool = true;
/// `compaction.reserveTokens` default (`:774`).
pub const DEFAULT_COMPACTION_RESERVE_TOKENS: f64 = 16384.0;
/// `compaction.keepRecentTokens` default (`:778`).
pub const DEFAULT_COMPACTION_KEEP_RECENT_TOKENS: f64 = 20000.0;
/// `branchSummary.reserveTokens` default (`:791`).
pub const DEFAULT_BRANCH_SUMMARY_RESERVE_TOKENS: f64 = 16384.0;
/// `branchSummary.skipPrompt` default (`:792`).
pub const DEFAULT_BRANCH_SUMMARY_SKIP_PROMPT: bool = false;
/// `retry.enabled` default (`:801`).
pub const DEFAULT_RETRY_ENABLED: bool = true;
/// `retry.maxRetries` default (`:816`).
pub const DEFAULT_RETRY_MAX_RETRIES: f64 = 3.0;
/// `retry.baseDelayMs` default (`:817`).
pub const DEFAULT_RETRY_BASE_DELAY_MS: f64 = 2000.0;
/// `retry.provider.maxRetryDelayMs` default (`:838`).
pub const DEFAULT_PROVIDER_MAX_RETRY_DELAY_MS: f64 = 60000.0;
/// `DEFAULT_HTTP_IDLE_TIMEOUT_MS` (`core/http-dispatcher.ts:4`), the fallback for
/// `httpIdleTimeoutMs` (`:822`).
pub const DEFAULT_HTTP_IDLE_TIMEOUT_MS: f64 = 300_000.0;
/// `terminal.showImages` default (`:1064`).
pub const DEFAULT_SHOW_IMAGES: bool = true;
/// `terminal.imageWidthCells` default (`:1079`).
pub const DEFAULT_IMAGE_WIDTH_CELLS: u32 = 60;
/// `images.autoResize` default (`:1124`).
pub const DEFAULT_IMAGE_AUTO_RESIZE: bool = true;
/// `images.blockImages` default (`:1137`).
pub const DEFAULT_BLOCK_IMAGES: bool = false;
/// `enableSkillCommands` default (`:1050`).
pub const DEFAULT_ENABLE_SKILL_COMMANDS: bool = true;
/// `enableInstallTelemetry` default (`settings-manager.ts:941`).
pub const DEFAULT_ENABLE_INSTALL_TELEMETRY: bool = true;
/// `markdown.codeBlockIndent` default (`:1222`).
pub const DEFAULT_CODE_BLOCK_INDENT: &str = "  ";
/// `warnings.anthropicExtraUsage` default (`:59`).
pub const DEFAULT_ANTHROPIC_EXTRA_USAGE: bool = true;
/// `autocompleteMaxVisible` default (`:1212`).
pub const DEFAULT_AUTOCOMPLETE_MAX_VISIBLE: f64 = 5.0;
/// `editorPaddingX` default (`:1192`).
pub const DEFAULT_EDITOR_PADDING_X: f64 = 0.0;
/// `outputPad` default (`:1202`) — only an exact `0` yields 0.
pub const DEFAULT_OUTPUT_PAD: u8 = 1;

// =============================================================================
// The typed lens (`settings-manager.ts:11-129`)
// =============================================================================

/// `CompactionSettings` (`:11-15`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionSettings {
    /// default: `true`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// default: `16384`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reserve_tokens: Option<Number>,
    /// default: `20000`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keep_recent_tokens: Option<Number>,
}

/// `BranchSummarySettings` (`:17-20`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchSummarySettings {
    /// default: `16384` (tokens reserved for prompt + LLM response)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reserve_tokens: Option<Number>,
    /// default: `false` — when true, skips the "Summarize branch?" prompt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skip_prompt: Option<bool>,
}

/// `ProviderRetrySettings` (`:22-26`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderRetrySettings {
    /// SDK/provider request timeout in milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<Number>,
    /// SDK/provider retry attempts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_retries: Option<Number>,
    /// default: `60000` (max server-requested delay before failing)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_retry_delay_ms: Option<Number>,
}

/// `RetrySettings` (`:28-33`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetrySettings {
    /// default: `true`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// default: `3`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_retries: Option<Number>,
    /// default: `2000` (exponential backoff: 2s, 4s, 8s)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_delay_ms: Option<Number>,
    /// Nested provider-level retry knobs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<ProviderRetrySettings>,
}

/// `TerminalSettings` (`:35-40`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalSettings {
    /// default: `true` (only relevant if the terminal supports images)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub show_images: Option<bool>,
    /// default: `60` (preferred inline image width in terminal cells)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_width_cells: Option<Number>,
    /// default: `false` (clear empty rows when content shrinks)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clear_on_shrink: Option<bool>,
    /// default: `false` (OSC 9;4 terminal progress indicators)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub show_terminal_progress: Option<bool>,
}

/// `ImageSettings` (`:42-45`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageSettings {
    /// default: `true` (resize images to 2000x2000 max for model compatibility)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_resize: Option<bool>,
    /// default: `false` — when true, no image is sent to an LLM provider.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block_images: Option<bool>,
}

/// `MarkdownSettings` (`:54-56`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkdownSettings {
    /// default: `"  "`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code_block_indent: Option<String>,
}

/// `WarningSettings` (`:58-60`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WarningSettings {
    /// default: `true`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anthropic_extra_usage: Option<bool>,
}

/// `DefaultProjectTrust` (`:62`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DefaultProjectTrust {
    /// Prompt on first use — the default (`:901`).
    Ask,
    /// Trust every project without asking.
    Always,
    /// Never trust a project.
    Never,
}

/// `steeringMode` / `followUpMode` (`:89-90`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum QueueDeliveryMode {
    /// Deliver every queued message at once.
    All,
    /// Deliver one queued message per turn — the default (`:704`, `:714`).
    OneAtATime,
}

/// `doubleEscapeAction` (`:116`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DoubleEscapeAction {
    /// Fork the session.
    Fork,
    /// Open `/tree` — the default (`:1160`).
    Tree,
    /// Do nothing.
    None,
}

/// `treeFilterMode` (`:117`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TreeFilterMode {
    /// The default filter (`:1172`).
    Default,
    /// Hide tool calls.
    NoTools,
    /// User messages only.
    UserOnly,
    /// Labelled entries only.
    LabeledOnly,
    /// Everything.
    All,
}

/// The object arm of `PackageSource` (`:72-81`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageSourceFilter {
    /// The npm/git source specifier.
    pub source: String,
    /// `false` = start empty and only apply the explicit resource patterns.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub autoload: Option<bool>,
    /// Extension patterns to load from the package.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extensions: Option<Vec<String>>,
    /// Skill patterns to load from the package.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skills: Option<Vec<String>>,
    /// Prompt patterns to load from the package.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompts: Option<Vec<String>>,
    /// Theme patterns to load from the package.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub themes: Option<Vec<String>>,
}

/// `PackageSource` (`:66-81`) — a bare source string, or an object that filters which
/// resources the package contributes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PackageSource {
    /// String form: load all resources from the package.
    Source(String),
    /// Object form: filter which resources to load.
    Filtered(PackageSourceFilter),
}

/// `Settings` (`:83-129`) — all 45 fields, every one optional, in declaration order.
///
/// A **typed lens** over [`SettingsMap`], not the load/save representation; see the module
/// docs. Numeric fields are [`Number`] rather than `f64`/`u64` so that `1` re-serializes as
/// `1` and `1.5` as `1.5`: an `f64` field would emit `1.0` and break `JSON.stringify`
/// byte-compat. `extra` catches keys pirust does not model, so a `Settings` round-trips a
/// forward-compatible file.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    /// Last version whose changelog was shown.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_changelog_version: Option<String>,
    /// Provider id used when `--provider` is absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_provider: Option<String>,
    /// Model id used when `--model` is absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    /// Reasoning level; callers fall back to `"medium"` (`core/defaults.ts:3`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_thinking_level: Option<ThinkingLevel>,
    /// default: `"auto"`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transport: Option<Transport>,
    /// default: `"one-at-a-time"`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub steering_mode: Option<QueueDeliveryMode>,
    /// default: `"one-at-a-time"`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub follow_up_mode: Option<QueueDeliveryMode>,
    /// Theme name, or a `/`-containing path that `getTheme` filters out (`:729-732`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,
    /// Context-compaction knobs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compaction: Option<CompactionSettings>,
    /// Branch-summary knobs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch_summary: Option<BranchSummarySettings>,
    /// Retry/backoff knobs, including the nested provider ones.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry: Option<RetrySettings>,
    /// default: `false`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hide_thinking_block: Option<bool>,
    /// default: `false` — transcript notices for significant prompt-cache misses.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub show_cache_miss_notices: Option<bool>,
    /// Command for the Ctrl+G external editor; takes precedence over `VISUAL`/`EDITOR`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub external_editor: Option<String>,
    /// Custom shell path; supports a leading `~`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shell_path: Option<String>,
    /// default: `false`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quiet_startup: Option<bool>,
    /// default: `"ask"`; **global-scope setting only** (`:899-902`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_project_trust: Option<DefaultProjectTrust>,
    /// Prefix prepended to every bash command.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shell_command_prefix: Option<String>,
    /// argv-style command used for npm package lookup/install.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub npm_command: Option<Vec<String>>,
    /// Show a condensed changelog after an update.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub collapse_changelog: Option<bool>,
    /// default: `true` — anonymous version/update ping.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enable_install_telemetry: Option<bool>,
    /// default: `false` — opt-in analytics data sharing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enable_analytics: Option<bool>,
    /// Analytics tracking identifier, generated on first opt-in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tracking_id: Option<String>,
    /// npm/git package sources.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub packages: Option<Vec<PackageSource>>,
    /// Local extension file paths or directories.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extensions: Option<Vec<String>>,
    /// Local skill file paths or directories.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skills: Option<Vec<String>>,
    /// Local prompt-template paths or directories.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompts: Option<Vec<String>>,
    /// Local theme paths or directories.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub themes: Option<Vec<String>>,
    /// default: `true` — register skills as `/skill:name` commands.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enable_skill_commands: Option<bool>,
    /// Terminal rendering knobs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal: Option<TerminalSettings>,
    /// Image handling knobs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub images: Option<ImageSettings>,
    /// Model patterns for cycling; **absence is meaningful** (`main.ts:689`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled_models: Option<Vec<String>>,
    /// default: `"tree"`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub double_escape_action: Option<DoubleEscapeAction>,
    /// default: `"default"`; invalid values coerce to `"default"` (`:1169-1173`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tree_filter_mode: Option<TreeFilterMode>,
    /// Custom token budgets per thinking level (`ThinkingBudgetsSettings`, `:47-52`).
    ///
    /// Reuses [`pirust_ai::types::ThinkingBudgets`], which is the same
    /// `{minimal?, low?, medium?, high?}` shape, rather than declaring a second type. It
    /// types the budgets as `u64`, so a hand-written fractional budget fails to load into
    /// this lens; the raw pipeline still round-trips it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_budgets: Option<ThinkingBudgets>,
    /// default: `0`; the setter clamps to `[0, 3]` (`:1196`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub editor_padding_x: Option<Number>,
    /// default: `1`; only an exact `0` yields 0 (`:1202`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_pad: Option<Number>,
    /// default: `5`; the setter clamps to `[3, 20]` (`:1216`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub autocomplete_max_visible: Option<Number>,
    /// Show the terminal cursor while still positioning it for IME.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub show_hardware_cursor: Option<bool>,
    /// Markdown rendering knobs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub markdown: Option<MarkdownSettings>,
    /// Startup-warning toggles.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warnings: Option<WarningSettings>,
    /// Custom session storage directory (same format as `--session-dir`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_dir: Option<String>,
    /// Proxy URL applied as `HTTP_PROXY`/`HTTPS_PROXY` for Pi-managed HTTP clients.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_proxy: Option<String>,
    /// HTTP header/body idle timeout in ms; `0` disables it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_idle_timeout_ms: Option<Number>,
    /// WebSocket connect/open handshake timeout in ms; `0` disables it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub websocket_connect_timeout_ms: Option<Number>,
    /// Keys pirust does not model, kept so a `Settings` round-trips an unknown file.
    #[serde(flatten)]
    pub extra: SettingsMap,
}

impl Settings {
    /// Deserialize the typed lens from a raw settings map.
    ///
    /// Fails where Pi would not — Pi's types are erased, so `{"outputPad":"wide"}` simply
    /// flows through its getters. Nothing on the load/save path calls this.
    pub fn from_map(map: &SettingsMap) -> Result<Self, serde_json::Error> {
        serde_json::from_value(Value::Object(map.clone()))
    }

    /// Serialize back to a raw settings map.
    pub fn to_map(&self) -> SettingsMap {
        match serde_json::to_value(self).expect("Settings always serializes") {
            Value::Object(map) => map,
            _ => unreachable!("Settings serializes to a JSON object"),
        }
    }

    /// `JSON.stringify(settings, null, 2)` for this value.
    pub fn to_json_string(&self) -> String {
        json_stringify(&Value::Object(self.to_map()))
    }
}

// =============================================================================
// deepMergeSettings (`settings-manager.ts:146-164`)
// =============================================================================

/// `deepMergeSettings(base, overrides)` — **recursive for mergeable objects, arrays replace**.
///
/// 0.84.2's `deepMergeObjects` (`:146-160`): the result starts as `{...base}`; every
/// override key whose value is `undefined` is **skipped**; otherwise, when BOTH the base and
/// the override values are mergeable objects (plain object, not null, not array), the two are
/// merged **recursively**; anything else — scalars, arrays, null — the override wins
/// wholesale. The branches, in code order:
///
/// | case | result |
/// |---|---|
/// | key absent from `overrides` | base value kept (never iterated) |
/// | `overrides[key] === undefined` | **skipped** — base value kept, even when the key is otherwise unknown |
/// | `overrides[key] === null` | override wins → `null` (not mergeable) |
/// | scalar/array over anything, anything over scalar/array | override **replaces** wholesale |
/// | object over object (both plain, non-array, non-null) | **recursive** deep merge — nested keys merge, never lost |
///
/// The consequences the fixture pins:
///
/// - **`CRITICAL-two-levels-deep-IS-merged`** (0.84.2 reversal of 0.80.10's
///   "NOT-merged"): global `retry:{enabled:true,maxRetries:5,provider:{timeoutMs:1000,
///   maxRetries:2}}` plus project `retry:{provider:{maxRetries:9}}` yields
///   `retry:{enabled:true,maxRetries:5,provider:{timeoutMs:1000,maxRetries:9}}` —
///   `provider.timeoutMs` is KEPT. The one-level `{...baseValue,...overrideValue}` spread
///   of 0.80.10 is gone.
/// - **`CRITICAL-array-in-both-is-REPLACED`.** Both `Array.isArray` guards exclude arrays
///   from the object branch, so an override array wins wholesale: never concatenated, never
///   element-merged, never deduplicated. `[]` therefore *clears* a list.
/// - **`nested-undefined-value`**: an `undefined` override value is skipped at every
///   depth (the base's value survives) — never materialized as a `$undefined` marker.
///
/// Key order: the result starts as `{...base}`, so base keys come first and an overridden
/// key keeps its **base** position; keys only the override has append in override order.
///
/// Both sides are [`Value`] rather than [`SettingsMap`] because a settings file may hold a
/// top-level array (`malformed:global-is-a-json-array`): `{...[]}` is `{}` and
/// `Object.keys([])` is empty, so such a scope contributes nothing without a special case.
pub fn deep_merge_settings(base: &Value, overrides: &Value) -> SettingsMap {
    // `const result: Settings = { ...base };` (:147) — this copies a base key whose
    // value is `undefined` as a PRESENT key (fixture:
    // `undefined-in-global-is-COPIED-BY-THE-SPREAD`).
    let mut result = spread(base);

    // `for (const key of Object.keys(overrides))` (:149)
    for (key, override_value) in own_enumerable_entries(overrides) {
        // `if (overrideValue === undefined) continue;` (:151-153). The `continue` happens
        // before any assignment, so an `undefined` override never even creates the key.
        if is_js_undefined(&override_value) {
            continue;
        }

        let base_value = js_get(base, &key);

        // (:155-158) — mergeable-on-both-sides gates the recursive branch.
        if is_js_plain_object(&override_value) && base_value.is_some_and(is_js_plain_object) {
            // `deepMergeObjects(baseValue, overrideValue)` (:157) — RECURSIVE; the inner
            // result is a full [`SettingsMap`].
            let nested =
                deep_merge_settings(base_value.expect("guarded by is_some_and"), &override_value);
            result.insert(key, Value::Object(nested));
        } else {
            // "For primitives and arrays, override value wins" (:159).
            result.insert(key, override_value);
        }
    }

    result
}

/// [`deep_merge_settings`] for two maps, the shape every internal caller has.
pub fn deep_merge_maps(base: &SettingsMap, overrides: &SettingsMap) -> SettingsMap {
    deep_merge_settings(
        &Value::Object(base.clone()),
        &Value::Object(overrides.clone()),
    )
}

// =============================================================================
// migrateSettings (`settings-manager.ts:381-440`)
// =============================================================================

/// `migrateSettings(settings)` — the 4 legacy rewrites, applied on every load (`:365`) and
/// again on every write-merge (`:585-587`).
///
/// Mutates in place and returns the same object, as Pi does (the fixture records
/// `mutatesInputInPlace: true` for all 20 rows).
///
/// 1. **`queueMode` → `steeringMode`** (`:383-386`). Guarded on key *presence*, not
///    definedness, so `{"steeringMode": null}` blocks the rewrite — and because the
///    `delete` is inside the `if`, `queueMode` is then *kept*.
/// 2. **`websockets: boolean` → `transport`** (`:389-392`): `true` → `"websocket"`,
///    `false` → `"sse"`. A non-boolean `websockets` is left alone and `transport` is not set.
/// 3. **`skills: {…}` → `skills: string[]`** (`:395-413`), asymmetrically:
///    `enableSkillCommands` is hoisted only when the top-level key is absent-or-`undefined`,
///    whereas a missing or **empty** `customDirectories` **deletes** `skills` outright.
/// 4. **`retry.maxDelayMs` → `retry.provider.maxRetryDelayMs`** (`:416-437`).
///    `delete retrySettings.maxDelayMs` sits **outside** the inner `if`, so the legacy value
///    is dropped even when it is not a number or when `provider.maxRetryDelayMs` already
///    exists — lossy on purpose. `provider` is *replaced* by a new object ordered
///    `{...existing provider keys, maxRetryDelayMs}`.
///
/// Key order is load-bearing and pinned by the fixture: a rewritten key is assigned *after*
/// iteration began, so a **new** key lands at the END (`{queueMode,theme}` becomes
/// `{theme,steeringMode}`) while assignment to an **existing** key keeps its position.
/// Deletion uses `shift_remove`, not serde_json's `remove` — with `preserve_order` the
/// latter is `swap_remove` and would drag the last key into the hole.
///
/// # Errors
///
/// `"queueMode" in settings` (`:383`) throws a `TypeError` when `settings` is a primitive,
/// which is how a `settings.json` containing `null`, `123`, `true` or `"hi"` fails to load.
/// Arrays and objects are both valid `in` operands, so a top-level array passes through
/// untouched — every guard below is false for it.
pub fn migrate_settings(settings: &mut Value) -> Result<(), SettingsErrorKind> {
    if !settings.is_object() && !settings.is_array() {
        return Err(SettingsErrorKind::InOperatorOnPrimitive {
            key: "queueMode",
            value: js_string(settings),
        });
    }
    let Some(map) = settings.as_object_mut() else {
        // An array: `"queueMode" in […]` is false, `settings.websockets` is undefined, and
        // both remaining guards need a key, so nothing happens.
        return Ok(());
    };

    // 1. queueMode -> steeringMode (:383-386)
    if map.contains_key("queueMode") && !map.contains_key("steeringMode") {
        let legacy = map
            .get("queueMode")
            .cloned()
            .expect("contains_key just passed");
        map.insert("steeringMode".to_string(), legacy);
        map.shift_remove("queueMode");
    }

    // 2. websockets -> transport (:389-392)
    if !map.contains_key("transport") {
        if let Some(websockets) = map.get("websockets").and_then(Value::as_bool) {
            let transport = if websockets { "websocket" } else { "sse" };
            map.insert(
                "transport".to_string(),
                Value::String(transport.to_string()),
            );
            map.shift_remove("websockets");
        }
    }

    // 3. skills object -> skills array (:395-413)
    if map.get("skills").is_some_and(is_js_plain_object) {
        let skills = map.get("skills").cloned().expect("guarded above");
        // `skillsSettings.enableSkillCommands !== undefined` (:405)
        let hoisted = js_get(&skills, "enableSkillCommands").cloned();
        // `settings.enableSkillCommands === undefined` — absent OR explicitly undefined.
        let top_level_unset = map.get("enableSkillCommands").is_none_or(is_js_undefined);
        if let Some(hoisted) = hoisted {
            if top_level_unset {
                map.insert("enableSkillCommands".to_string(), hoisted);
            }
        }
        // (:408-412) — a non-empty array moves up; anything else deletes `skills`.
        match js_get(&skills, "customDirectories") {
            Some(Value::Array(dirs)) if !dirs.is_empty() => {
                map.insert("skills".to_string(), Value::Array(dirs.clone()));
            }
            _ => {
                map.shift_remove("skills");
            }
        }
    }

    // 4. retry.maxDelayMs -> retry.provider.maxRetryDelayMs (:416-437)
    if map.get("retry").is_some_and(is_js_plain_object) {
        let retry = map
            .get_mut("retry")
            .and_then(Value::as_object_mut)
            .expect("guarded above");
        // `typeof retrySettings.provider === "object" && retrySettings.provider !== null`
        // (:423-426) — note there is NO `Array.isArray` guard here, unlike every other
        // object test in this file, so an array `provider` IS spread (into index keys).
        let provider = retry
            .get("provider")
            .filter(|value| is_js_plain_object(value) || value.is_array())
            .cloned();
        let max_delay_is_number = retry.get("maxDelayMs").is_some_and(Value::is_number);
        // `providerSettings?.maxRetryDelayMs === undefined || … === null` (:429): an absent
        // provider short-circuits to true through the optional chain.
        let slot_is_free = provider
            .as_ref()
            .is_none_or(|provider| js_get(provider, "maxRetryDelayMs").is_none_or(Value::is_null));
        if max_delay_is_number && slot_is_free {
            let max_delay = retry
                .get("maxDelayMs")
                .cloned()
                .expect("max_delay_is_number just passed");
            // `{ ...(providerSettings ?? {}), maxRetryDelayMs: retrySettings.maxDelayMs }`
            let mut merged = provider.as_ref().map(spread).unwrap_or_default();
            merged.insert("maxRetryDelayMs".to_string(), max_delay);
            retry.insert("provider".to_string(), Value::Object(merged));
        }
        // Unconditional (:436) — outside the `if`, hence the silent drop.
        retry.shift_remove("maxDelayMs");
    }

    Ok(())
}

// =============================================================================
// Scopes, errors (`settings-manager.ts:173-186`)
// =============================================================================

/// `SettingsScope` (`:173`) — two scopes, no more.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SettingsScope {
    /// `<agentDir>/settings.json` (`:195`).
    Global,
    /// `<resolvedCwd>/<CONFIG_DIR_NAME>/settings.json` (`:196`).
    Project,
}

impl SettingsScope {
    /// The literal Pi interpolates into a diagnostic (`main.ts:83`).
    pub fn as_str(self) -> &'static str {
        match self {
            SettingsScope::Global => "global",
            SettingsScope::Project => "project",
        }
    }
}

impl std::fmt::Display for SettingsScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Every JS `Error` `SettingsManager` can queue, with Pi's `error.message` as its `Display`.
///
/// [`SettingsErrorKind::class_name`] is the `error.name` a JS consumer would read; the
/// fixture records it alongside the message.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SettingsErrorKind {
    /// `JSON.parse` threw a `SyntaxError`.
    ///
    /// **The message text is NOT Pi's.** V8 writes e.g. `Expected property name or '}' in
    /// JSON at position 2 (line 1 column 3)`; this carries serde_json's wording for the same
    /// defect. See the module docs — the fixture rows that hit this are asserted
    /// structurally.
    #[error("{0}")]
    Syntax(String),
    /// `"queueMode" in settings` on a primitive (`:383`). Reproduces V8's wording exactly.
    #[error("Cannot use 'in' operator to search for '{key}' in {value}")]
    InOperatorOnPrimitive {
        /// Always `queueMode`: it is the first `in` `migrateSettings` evaluates.
        key: &'static str,
        /// `String(settings)` — the parsed primitive.
        value: String,
    },
    /// `assertProjectTrustedForWrite` (`:534-538`).
    #[error("Project is not trusted; refusing to write project settings")]
    ProjectNotTrusted,
    /// A filesystem or lock failure from [`SettingsStorage::with_lock`]. Node's `fs` messages
    /// (`EACCES: permission denied, open '…'`) are not reproduced; this is `io::Error`'s text.
    #[error("{0}")]
    Io(String),
    /// `parseTimeoutSetting` rejected a value (`:168`) — `httpIdleTimeoutMs` or
    /// `websocketConnectTimeoutMs`. Pi throws this out of the *getter*, not at load time.
    #[error("Invalid {setting} setting: {value}")]
    InvalidTimeoutSetting {
        /// The setting name Pi interpolates.
        setting: &'static str,
        /// `String(value)`.
        value: String,
    },
}

impl SettingsErrorKind {
    /// The JS `error.name` for this error — what the fixture stores in `errors[].name`.
    pub fn class_name(&self) -> &'static str {
        match self {
            SettingsErrorKind::Syntax(_) => "SyntaxError",
            SettingsErrorKind::InOperatorOnPrimitive { .. } => "TypeError",
            // `new Error(...)` and Node's `fs` errors are both plain `Error`s.
            SettingsErrorKind::ProjectNotTrusted
            | SettingsErrorKind::Io(_)
            | SettingsErrorKind::InvalidTimeoutSetting { .. } => "Error",
        }
    }
}

/// `SettingsError` (`:183-186`) — a scope plus the error that scope produced.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("({scope} settings) {error}")]
pub struct SettingsError {
    /// Which file produced it.
    pub scope: SettingsScope,
    /// The error itself.
    pub error: SettingsErrorKind,
}

impl SettingsError {
    /// `collectSettingsDiagnostics` (`main.ts:77-85`): the exact `warning` text
    /// `` `(${context}, ${scope} settings) ${error.message}` ``.
    ///
    /// Lives here rather than in `main.rs` so the golden fixture's `diagnostics` block has
    /// one owner; the diagnostic *type* is always `"warning"`.
    pub fn diagnostic_message(&self, context: &str) -> String {
        format!("({context}, {} settings) {}", self.scope, self.error)
    }
}

// =============================================================================
// Storage (`settings-manager.ts:179-272`)
// =============================================================================

/// `SettingsStorage` (`:179-181`) — read-modify-write one scope under a lock.
///
/// `f` is handed the current file contents (`None` when the file does not exist) and returns
/// the new contents, or `None` to write nothing. It is called exactly once.
pub trait SettingsStorage: Send + Sync {
    /// `withLock(scope, fn)` (`:180`).
    fn with_lock(
        &self,
        scope: SettingsScope,
        f: &mut dyn FnMut(Option<&str>) -> Option<String>,
    ) -> std::io::Result<()>;
}

/// `InMemorySettingsStorage` (`:257-272`) — no file I/O.
///
/// `Mutex` rather than plain fields because [`SettingsStorage`] takes `&self` (Pi mutates
/// through a method on a mutable object; Rust needs the interior mutability made explicit)
/// and because `SettingsManager` is shared across tasks in the async harness.
#[derive(Debug, Default)]
pub struct InMemorySettingsStorage {
    contents: Mutex<[Option<String>; 2]>,
}

impl InMemorySettingsStorage {
    /// Both scopes empty, as Pi's constructor leaves them.
    pub fn new() -> Self {
        Self::default()
    }

    /// Seed the two scopes with raw file text — the shape the golden fixture stores
    /// (`globalRaw` / `projectRaw`, where `null` means "file absent" and `""` means "file
    /// exists but is empty").
    pub fn with_contents(global: Option<String>, project: Option<String>) -> Self {
        Self {
            contents: Mutex::new([global, project]),
        }
    }

    /// The raw text currently held for a scope.
    pub fn contents(&self, scope: SettingsScope) -> Option<String> {
        self.contents.lock().expect("settings storage mutex")[Self::index(scope)].clone()
    }

    fn index(scope: SettingsScope) -> usize {
        match scope {
            SettingsScope::Global => 0,
            SettingsScope::Project => 1,
        }
    }
}

impl SettingsStorage for InMemorySettingsStorage {
    fn with_lock(
        &self,
        scope: SettingsScope,
        f: &mut dyn FnMut(Option<&str>) -> Option<String>,
    ) -> std::io::Result<()> {
        let mut contents = self.contents.lock().expect("settings storage mutex");
        let slot = Self::index(scope);
        let next = f(contents[slot].as_deref());
        // `if (next !== undefined)` (:263) — `undefined` means "no write".
        if let Some(next) = next {
            contents[slot] = Some(next);
        }
        Ok(())
    }
}

/// `FileSettingsStorage` (`:188-255`).
///
/// Directories are created **lazily**, only when a write actually happens (`:240-243`), and
/// the lock is taken only when the file exists or a write is pending (`:233-246`) — so
/// merely constructing a `SettingsManager` never creates `~/.pirust/agent`.
#[derive(Debug, Clone)]
pub struct FileSettingsStorage {
    global_settings_path: PathBuf,
    project_settings_path: PathBuf,
}

/// `maxAttempts` for the lock (`:200`).
const LOCK_MAX_ATTEMPTS: u32 = 10;
/// `delayMs` between lock attempts (`:201`).
const LOCK_RETRY_DELAY_MS: u64 = 20;

impl FileSettingsStorage {
    /// The two absolute paths, already resolved.
    pub fn new(global_settings_path: PathBuf, project_settings_path: PathBuf) -> Self {
        Self {
            global_settings_path,
            project_settings_path,
        }
    }

    /// `new FileSettingsStorage(cwd, agentDir)` (`:192-197`).
    ///
    /// Global is [`ConfigEnv::settings_path`] — the agent dir is **not** re-derived here.
    /// Pi resolves the agent dir first and then joins the leaf; `join(resolvePath(a), leaf)`
    /// and `resolvePath(join(a, leaf))` are the same string, since `path.resolve` only
    /// anchors a relative prefix and the leaf is a plain literal.
    ///
    /// Project is `join(resolvePath(cwd), CONFIG_DIR_NAME, "settings.json")`. `cwd` is
    /// resolved with `PathBuf`, not Node's `path.join`, because both segments here are
    /// literals with no separator — the divergence [`crate::config`] documents (Node
    /// rewrites the base's separators) cannot bite.
    pub fn resolve(env: &ConfigEnv, cwd: &Path) -> Result<Self, ConfigPathError> {
        let global = PathBuf::from(env.settings_path()?);
        let project = cwd.join(CONFIG_DIR_NAME).join(SETTINGS_FILE_NAME);
        Ok(Self::new(global, project))
    }

    /// The file backing a scope.
    pub fn path(&self, scope: SettingsScope) -> &Path {
        match scope {
            SettingsScope::Global => &self.global_settings_path,
            SettingsScope::Project => &self.project_settings_path,
        }
    }

    /// `acquireLockSyncWithRetry` (`:199-224`): 10 attempts, 20 ms apart, retrying only on
    /// "already locked".
    ///
    /// The lock is a `<path>.lock` **directory**, which is what `proper-lockfile` creates —
    /// `mkdir` is atomic on every platform pirust targets. Pi busy-waits (`:216-219`) purely
    /// to keep its callers synchronous; sleeping is equivalent and cheaper.
    fn acquire_lock(path: &Path) -> std::io::Result<LockGuard> {
        let lock_path = lock_path_for(path);
        let mut last_error = None;
        for attempt in 1..=LOCK_MAX_ATTEMPTS {
            match std::fs::create_dir(&lock_path) {
                Ok(()) => return Ok(LockGuard { path: lock_path }),
                Err(error) => {
                    let locked = error.kind() == std::io::ErrorKind::AlreadyExists;
                    if !locked || attempt == LOCK_MAX_ATTEMPTS {
                        return Err(error);
                    }
                    last_error = Some(error);
                    std::thread::sleep(std::time::Duration::from_millis(LOCK_RETRY_DELAY_MS));
                }
            }
        }
        // Unreachable: the final attempt returns either way. Mirrors Pi's `:223` fallback.
        Err(last_error.unwrap_or_else(|| {
            std::io::Error::other("Failed to acquire settings lock".to_string())
        }))
    }
}

/// `join(dirname(path), basename(path) + ".lock")`, `proper-lockfile`'s layout.
fn lock_path_for(path: &Path) -> PathBuf {
    let mut lock = path.as_os_str().to_os_string();
    lock.push(".lock");
    PathBuf::from(lock)
}

/// The `release()` half of `lockfile.lockSync`, run from Pi's `finally` (`:249-253`).
struct LockGuard {
    path: PathBuf,
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir(&self.path);
    }
}

impl SettingsStorage for FileSettingsStorage {
    fn with_lock(
        &self,
        scope: SettingsScope,
        f: &mut dyn FnMut(Option<&str>) -> Option<String>,
    ) -> std::io::Result<()> {
        let path = self.path(scope);

        // `existsSync(path)` (:233) — only lock and read when there is something there.
        let file_exists = path.exists();
        let mut release = if file_exists {
            Some(FileSettingsStorage::acquire_lock(path)?)
        } else {
            None
        };
        let current = if file_exists {
            Some(std::fs::read_to_string(path)?)
        } else {
            None
        };

        let next = f(current.as_deref());
        if let Some(next) = next {
            // `if (!existsSync(dir)) mkdirSync(dir, { recursive: true })` (:241-243)
            if let Some(dir) = path.parent() {
                if !dir.as_os_str().is_empty() && !dir.exists() {
                    std::fs::create_dir_all(dir)?;
                }
            }
            if release.is_none() {
                release = Some(FileSettingsStorage::acquire_lock(path)?);
            }
            std::fs::write(path, next)?;
        }
        drop(release);
        Ok(())
    }
}

// =============================================================================
// SettingsManager (`settings-manager.ts:274-1234`)
// =============================================================================

/// `SettingsManagerCreateOptions` (`:175-177`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettingsManagerCreateOptions {
    /// `options.projectTrusted ?? true` (`:320`).
    pub project_trusted: bool,
}

impl Default for SettingsManagerCreateOptions {
    fn default() -> Self {
        Self {
            project_trusted: true,
        }
    }
}

/// `getCompactionSettings()` (`:781-787`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResolvedCompactionSettings {
    /// `compaction.enabled ?? true`.
    pub enabled: bool,
    /// `compaction.reserveTokens ?? 16384`.
    pub reserve_tokens: f64,
    /// `compaction.keepRecentTokens ?? 20000`.
    pub keep_recent_tokens: f64,
}

/// `getBranchSummarySettings()` (`:789-794`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResolvedBranchSummarySettings {
    /// `branchSummary.reserveTokens ?? 16384`.
    pub reserve_tokens: f64,
    /// `branchSummary.skipPrompt ?? false`.
    pub skip_prompt: bool,
}

/// `getRetrySettings()` (`:813-819`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResolvedRetrySettings {
    /// `retry.enabled ?? true`.
    pub enabled: bool,
    /// `retry.maxRetries ?? 3`.
    pub max_retries: f64,
    /// `retry.baseDelayMs ?? 2000`.
    pub base_delay_ms: f64,
}

/// `getProviderRetrySettings()` (`:834-840`). The first two stay optional: the SDK
/// distinguishes "unset" from any value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResolvedProviderRetrySettings {
    /// `retry.provider.timeoutMs`.
    pub timeout_ms: Option<f64>,
    /// `retry.provider.maxRetries`.
    pub max_retries: Option<f64>,
    /// `retry.provider.maxRetryDelayMs ?? 60000`.
    pub max_retry_delay_ms: f64,
}

/// `SettingsManager` (`:274-1234`) — the two scopes, their merged view, per-field write
/// tracking and the drainable error queue.
///
/// Everything is held as raw [`Value`]s, matching Pi's runtime; see the module docs.
pub struct SettingsManager {
    storage: Arc<dyn SettingsStorage>,
    global_settings: Value,
    project_settings: Value,
    settings: SettingsMap,
    project_trusted: bool,
    /// `modifiedFields` (`:280`). A `Vec` because JS `Set` iteration is insertion-ordered
    /// and `persistScopedSettings` appends new keys to the file in exactly that order.
    modified_fields: Vec<String>,
    /// `modifiedNestedFields` (`:281`), likewise insertion-ordered.
    modified_nested_fields: Vec<(String, Vec<String>)>,
    modified_project_fields: Vec<String>,
    modified_project_nested_fields: Vec<(String, Vec<String>)>,
    global_settings_load_error: Option<SettingsErrorKind>,
    project_settings_load_error: Option<SettingsErrorKind>,
    errors: Vec<SettingsError>,
}

impl std::fmt::Debug for SettingsManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SettingsManager")
            .field("global_settings", &self.global_settings)
            .field("project_settings", &self.project_settings)
            .field("settings", &self.settings)
            .field("project_trusted", &self.project_trusted)
            .field("errors", &self.errors)
            .finish_non_exhaustive()
    }
}

impl SettingsManager {
    // -------------------------------------------------------------------------
    // Construction (`:289-348`)
    // -------------------------------------------------------------------------

    /// `SettingsManager.create(cwd, agentDir, options)` (`:309-316`) — file-backed.
    pub fn create(
        env: &ConfigEnv,
        cwd: &Path,
        options: SettingsManagerCreateOptions,
    ) -> Result<Self, ConfigPathError> {
        let storage = FileSettingsStorage::resolve(env, cwd)?;
        Ok(Self::from_storage(Arc::new(storage), options))
    }

    /// `SettingsManager.fromStorage(storage, options)` (`:319-340`).
    ///
    /// Both scopes are loaded eagerly and **neither failure is thrown**: each becomes a
    /// queued [`SettingsError`], global first — the order `drainErrors` then reports.
    pub fn from_storage(
        storage: Arc<dyn SettingsStorage>,
        options: SettingsManagerCreateOptions,
    ) -> Self {
        let project_trusted = options.project_trusted;
        let (global_settings, global_error) =
            Self::try_load_from_storage(storage.as_ref(), SettingsScope::Global, true);
        let (project_settings, project_error) =
            Self::try_load_from_storage(storage.as_ref(), SettingsScope::Project, project_trusted);

        let mut errors = Vec::new();
        if let Some(error) = global_error.clone() {
            errors.push(SettingsError {
                scope: SettingsScope::Global,
                error,
            });
        }
        if let Some(error) = project_error.clone() {
            errors.push(SettingsError {
                scope: SettingsScope::Project,
                error,
            });
        }

        // `this.settings = deepMergeSettings(...)` (:305)
        let settings = deep_merge_settings(&global_settings, &project_settings);
        Self {
            storage,
            global_settings,
            project_settings,
            settings,
            project_trusted,
            modified_fields: Vec::new(),
            modified_nested_fields: Vec::new(),
            modified_project_fields: Vec::new(),
            modified_project_nested_fields: Vec::new(),
            global_settings_load_error: global_error,
            project_settings_load_error: project_error,
            errors,
        }
    }

    /// `SettingsManager.inMemory(settings, options)` (`:343-348`).
    ///
    /// The seed is migrated, stringified into the global slot, then loaded back — so it is
    /// migrated twice, exactly as in Pi (the rewrites are idempotent).
    pub fn in_memory(seed: &SettingsMap, options: SettingsManagerCreateOptions) -> Self {
        let mut initial = Value::Object(seed.clone());
        // Infallible: an object is always a valid `in` operand.
        let _ = migrate_settings(&mut initial);
        let storage = InMemorySettingsStorage::with_contents(Some(json_stringify(&initial)), None);
        Self::from_storage(Arc::new(storage), options)
    }

    /// `loadFromStorage` (`:350-366`).
    ///
    /// Untrusted project settings are **never even read** (`:351-353`), so a corrupt project
    /// file produces no diagnostic while the project is untrusted. A missing *or empty* file
    /// is `{}` — `if (!content)` (`:361`) is a JS truthiness test, so `""` short-circuits
    /// before `JSON.parse` and is not an error.
    ///
    /// The parse is **bare `JSON.parse`** (`:364`) — no `stripJsonComments`, unlike
    /// `models.json` (spec §6.2 vs. §9.1). A `//` or `/* */` comment in `settings.json` is a
    /// `SyntaxError` that costs the whole scope; `serde_json::from_str` is strict in exactly
    /// the same way, so the asymmetry is preserved for free.
    fn load_from_storage(
        storage: &dyn SettingsStorage,
        scope: SettingsScope,
        project_trusted: bool,
    ) -> Result<Value, SettingsErrorKind> {
        if scope == SettingsScope::Project && !project_trusted {
            return Ok(Value::Object(SettingsMap::new()));
        }

        let mut content: Option<String> = None;
        storage
            .with_lock(scope, &mut |current| {
                content = current.map(str::to_string);
                None
            })
            .map_err(|error| SettingsErrorKind::Io(error.to_string()))?;

        let Some(content) = content.filter(|text| !text.is_empty()) else {
            return Ok(Value::Object(SettingsMap::new()));
        };
        let mut settings: Value = serde_json::from_str(&content)
            .map_err(|error| SettingsErrorKind::Syntax(error.to_string()))?;
        migrate_settings(&mut settings)?;
        Ok(settings)
    }

    /// `tryLoadFromStorage` (`:368-378`) — the failure becomes `{}` plus an error.
    fn try_load_from_storage(
        storage: &dyn SettingsStorage,
        scope: SettingsScope,
        project_trusted: bool,
    ) -> (Value, Option<SettingsErrorKind>) {
        match Self::load_from_storage(storage, scope, project_trusted) {
            Ok(settings) => (settings, None),
            Err(error) => (Value::Object(SettingsMap::new()), Some(error)),
        }
    }

    // -------------------------------------------------------------------------
    // Scope access (`:442-477`)
    // -------------------------------------------------------------------------

    /// `getGlobalSettings()` (`:442-444`) — a clone, as `structuredClone` gives.
    ///
    /// A [`Value`] because a `settings.json` holding a top-level array is loaded as one.
    pub fn get_global_settings(&self) -> Value {
        self.global_settings.clone()
    }

    /// `getProjectSettings()` (`:446-448`).
    pub fn get_project_settings(&self) -> Value {
        self.project_settings.clone()
    }

    /// The merged view — `this.settings` (`:278`, recomputed at `:305`, `:466`, `:476`,
    /// `:504`, `:509`, `:610`, `:628`).
    pub fn merged_settings(&self) -> &SettingsMap {
        &self.settings
    }

    /// The merged view as a typed [`Settings`], for consumers that want fields.
    ///
    /// `Err` where Pi would silently carry on: see [`Settings::from_map`].
    pub fn typed_settings(&self) -> Result<Settings, serde_json::Error> {
        Settings::from_map(&self.settings)
    }

    /// `this.settings[field]` — the primitive every unported getter is one line over.
    /// `undefined` (absent or sentinel) collapses to `None`; `null` is `Some(Value::Null)`.
    pub fn get_merged_field(&self, field: &str) -> Option<&Value> {
        self.settings
            .get(field)
            .filter(|value| !is_js_undefined(value))
    }

    /// JS optional chaining `this.settings.a?.b?.c` over the merged view.
    fn merged_path(&self, path: &[&str]) -> Option<&Value> {
        let (first, rest) = path.split_first()?;
        let mut current = self.get_merged_field(first)?;
        for key in rest {
            current = js_get(current, key)?;
        }
        Some(current)
    }

    /// `isProjectTrusted()` (`:450-452`).
    pub fn is_project_trusted(&self) -> bool {
        self.project_trusted
    }

    /// `setProjectTrusted(trusted)` (`:454-477`).
    ///
    /// Revoking trust drops the project scope to `{}` **and clears its load error**, so a
    /// previously reported parse failure stops being re-reported. Granting trust re-reads the
    /// file and queues any error it finds.
    pub fn set_project_trusted(&mut self, trusted: bool) {
        if self.project_trusted == trusted {
            return;
        }

        self.project_trusted = trusted;
        self.modified_project_fields.clear();
        self.modified_project_nested_fields.clear();

        if !trusted {
            self.project_settings = Value::Object(SettingsMap::new());
            self.project_settings_load_error = None;
            self.settings = deep_merge_settings(&self.global_settings, &self.project_settings);
            return;
        }

        let (settings, error) =
            Self::try_load_from_storage(self.storage.as_ref(), SettingsScope::Project, trusted);
        self.project_settings = settings;
        self.project_settings_load_error = error.clone();
        if let Some(error) = error {
            self.record_error(SettingsScope::Project, error);
        }
        self.settings = deep_merge_settings(&self.global_settings, &self.project_settings);
    }

    /// `reload()` (`:479-505`).
    ///
    /// A scope that fails to reload keeps its **previous** in-memory value (only the success
    /// branch assigns) while still queueing the error. All four modified-field sets are
    /// cleared regardless.
    pub fn reload(&mut self) {
        let (global_settings, global_error) =
            Self::try_load_from_storage(self.storage.as_ref(), SettingsScope::Global, true);
        match global_error {
            None => {
                self.global_settings = global_settings;
                self.global_settings_load_error = None;
            }
            Some(error) => {
                self.global_settings_load_error = Some(error.clone());
                self.record_error(SettingsScope::Global, error);
            }
        }

        self.modified_fields.clear();
        self.modified_nested_fields.clear();
        self.modified_project_fields.clear();
        self.modified_project_nested_fields.clear();

        let (project_settings, project_error) = Self::try_load_from_storage(
            self.storage.as_ref(),
            SettingsScope::Project,
            self.project_trusted,
        );
        match project_error {
            None => {
                self.project_settings = project_settings;
                self.project_settings_load_error = None;
            }
            Some(error) => {
                self.project_settings_load_error = Some(error.clone());
                self.record_error(SettingsScope::Project, error);
            }
        }

        self.settings = deep_merge_settings(&self.global_settings, &self.project_settings);
    }

    /// `applyOverrides(overrides)` (`:508-510`) — merges onto the **merged view only**, so
    /// the change is never persisted and is lost on the next `save`/`reload`.
    pub fn apply_overrides(&mut self, overrides: &SettingsMap) {
        self.settings = deep_merge_maps(&self.settings, overrides);
    }

    // -------------------------------------------------------------------------
    // The error queue (`:540-543`, `:654-658`)
    // -------------------------------------------------------------------------

    /// `drainErrors()` (`:654-658`) — hand the queued errors to the caller and empty it, so
    /// a second call returns nothing until something new fails.
    pub fn drain_errors(&mut self) -> Vec<SettingsError> {
        std::mem::take(&mut self.errors)
    }

    /// `recordError(scope, error)` (`:540-543`).
    fn record_error(&mut self, scope: SettingsScope, error: SettingsErrorKind) {
        self.errors.push(SettingsError { scope, error });
    }

    /// `flush()` (`:650-652`) — awaits the write queue in Pi. Writes here are synchronous, so
    /// there is nothing to await; kept so callers can keep Pi's shape.
    pub fn flush(&self) {}

    // -------------------------------------------------------------------------
    // Write tracking + persistence (`:512-640`)
    // -------------------------------------------------------------------------

    /// `markModified(field, nestedKey?)` (`:513-521`).
    fn mark_modified(&mut self, field: &str, nested_key: Option<&str>) {
        push_unique(&mut self.modified_fields, field);
        if let Some(nested_key) = nested_key {
            push_nested(&mut self.modified_nested_fields, field, nested_key);
        }
    }

    /// `markProjectModified(field, nestedKey?)` (`:524-532`).
    fn mark_project_modified(&mut self, field: &str, nested_key: Option<&str>) {
        push_unique(&mut self.modified_project_fields, field);
        if let Some(nested_key) = nested_key {
            push_nested(&mut self.modified_project_nested_fields, field, nested_key);
        }
    }

    /// `assertProjectTrustedForWrite()` (`:534-538`).
    fn assert_project_trusted_for_write(&self) -> Result<(), SettingsErrorKind> {
        if self.project_trusted {
            Ok(())
        } else {
            Err(SettingsErrorKind::ProjectNotTrusted)
        }
    }

    /// `clearModifiedScope(scope)` (`:545-554`).
    fn clear_modified_scope(&mut self, scope: SettingsScope) {
        match scope {
            SettingsScope::Global => {
                self.modified_fields.clear();
                self.modified_nested_fields.clear();
            }
            SettingsScope::Project => {
                self.modified_project_fields.clear();
                self.modified_project_nested_fields.clear();
            }
        }
    }

    /// `persistScopedSettings` (`:578-607`) — the field-granular writer.
    ///
    /// Re-reads the file under lock, migrates it, and copies **only the modified fields**
    /// over it (for a nested field, only the modified nested keys), then writes
    /// `JSON.stringify(merged, null, 2)`. Starting from `{...currentFileSettings}` is what
    /// preserves the user's key order and any keys pirust does not know.
    ///
    /// A parse failure inside the callback aborts the write without touching the file, as
    /// Pi's `throw` does — the error then reaches the queue via the caller.
    fn persist_scoped_settings(
        storage: &dyn SettingsStorage,
        scope: SettingsScope,
        snapshot: &Value,
        modified_fields: &[String],
        modified_nested_fields: &[(String, Vec<String>)],
    ) -> Result<(), SettingsErrorKind> {
        let mut failure: Option<SettingsErrorKind> = None;
        let io = storage.with_lock(scope, &mut |current| {
            // `current ? migrateSettings(JSON.parse(current)) : {}` (:585-587) — an empty
            // file is falsy, hence `{}` without a parse.
            let current_file = match current.filter(|text| !text.is_empty()) {
                Some(text) => match serde_json::from_str::<Value>(text) {
                    Ok(mut parsed) => match migrate_settings(&mut parsed) {
                        Ok(()) => parsed,
                        Err(error) => {
                            failure = Some(error);
                            return None;
                        }
                    },
                    Err(error) => {
                        failure = Some(SettingsErrorKind::Syntax(error.to_string()));
                        return None;
                    }
                },
                None => Value::Object(SettingsMap::new()),
            };

            // `const mergedSettings: Settings = { ...currentFileSettings };` (:588)
            let mut merged = spread(&current_file);
            for field in modified_fields {
                // `snapshotSettings[field]` — absent means `undefined`, which
                // `JSON.stringify` later drops, and that is how a field is deleted.
                let value = js_get(snapshot, field)
                    .cloned()
                    .unwrap_or_else(js_undefined);
                let nested_keys = nested_keys_for(modified_nested_fields, field);
                // `typeof value === "object" && value !== null` (:591) — note there is no
                // `Array.isArray` guard here, so an array-valued nested field takes this
                // branch too.
                let value_is_object = is_js_plain_object(&value) || value.is_array();

                match nested_keys.filter(|_| value_is_object) {
                    Some(nested_keys) => {
                        // `(currentFileSettings[field] as Record<…>) ?? {}` (:593) — `??`
                        // fires only for null/undefined, so a scalar on disk gets spread.
                        let base_nested = js_get(&current_file, field).filter(|v| !v.is_null());
                        let mut merged_nested = base_nested.map(spread).unwrap_or_default();
                        for nested_key in nested_keys {
                            merged_nested.insert(
                                nested_key.clone(),
                                js_get(&value, nested_key)
                                    .cloned()
                                    .unwrap_or_else(js_undefined),
                            );
                        }
                        merged.insert(field.clone(), Value::Object(merged_nested));
                    }
                    None => {
                        merged.insert(field.clone(), value);
                    }
                }
            }

            // `JSON.stringify(mergedSettings, null, 2)` (:605) — no trailing newline.
            Some(json_stringify(&Value::Object(merged)))
        });

        if let Some(failure) = failure {
            return Err(failure);
        }
        io.map_err(|error| SettingsErrorKind::Io(error.to_string()))
    }

    /// `save()` (`:609-623`) — recompute the merged view, then persist the global scope
    /// unless it failed to load (`:612-614`), which is what stops a broken file being
    /// clobbered. A write failure lands in the error queue rather than propagating
    /// (`:565-567`).
    fn save(&mut self) {
        self.settings = deep_merge_settings(&self.global_settings, &self.project_settings);

        if self.global_settings_load_error.is_some() {
            return;
        }

        let snapshot = self.global_settings.clone();
        let modified_fields = self.modified_fields.clone();
        let modified_nested_fields = self.modified_nested_fields.clone();
        match Self::persist_scoped_settings(
            self.storage.as_ref(),
            SettingsScope::Global,
            &snapshot,
            &modified_fields,
            &modified_nested_fields,
        ) {
            Ok(()) => self.clear_modified_scope(SettingsScope::Global),
            Err(error) => self.record_error(SettingsScope::Global, error),
        }
    }

    /// `saveProjectSettings(settings)` (`:625-640`).
    ///
    /// # Errors
    ///
    /// [`SettingsErrorKind::ProjectNotTrusted`] — the assert runs *before* anything is
    /// mutated (`:626`), so an untrusted call leaves the manager untouched.
    pub fn save_project_settings(&mut self, settings: &Value) -> Result<(), SettingsErrorKind> {
        self.assert_project_trusted_for_write()?;
        self.project_settings = settings.clone();
        self.settings = deep_merge_settings(&self.global_settings, &self.project_settings);

        if self.project_settings_load_error.is_some() {
            return Ok(());
        }

        let snapshot = self.project_settings.clone();
        let modified_fields = self.modified_project_fields.clone();
        let modified_nested_fields = self.modified_project_nested_fields.clone();
        match Self::persist_scoped_settings(
            self.storage.as_ref(),
            SettingsScope::Project,
            &snapshot,
            &modified_fields,
            &modified_nested_fields,
        ) {
            Ok(()) => self.clear_modified_scope(SettingsScope::Project),
            Err(error) => self.record_error(SettingsScope::Project, error),
        }
        Ok(())
    }

    /// `updateProjectSettings(field, update)` (`:642-648`) — clone the project scope, let the
    /// caller mutate it, mark the field, save. The generic behind `setProjectPackages`,
    /// `setProjectExtensionPaths`, `setProjectSkillPaths`, `setProjectPromptTemplatePaths`
    /// and `setProjectThemePaths` (`:979-1047`), which differ only in the field they set.
    ///
    /// # Errors
    ///
    /// [`SettingsErrorKind::ProjectNotTrusted`].
    pub fn update_project_field(
        &mut self,
        field: &str,
        update: impl FnOnce(&mut SettingsMap),
    ) -> Result<(), SettingsErrorKind> {
        self.assert_project_trusted_for_write()?;
        // `structuredClone(this.projectSettings)`. A non-object project scope (only reachable
        // from a top-level-array project file) is normalized to its spread here; JS would
        // hang a named property off the array instead, which no writer or reader can then
        // use meaningfully.
        let mut project = spread(&self.project_settings);
        update(&mut project);
        self.mark_project_modified(field, None);
        self.save_project_settings(&Value::Object(project))
    }

    /// Set one global field and persist it — the primitive behind every trivial
    /// `setX(value)` in `:664-1233`.
    pub fn set_global_field(&mut self, field: &str, value: Value) {
        let mut global = spread(&self.global_settings);
        global.insert(field.to_string(), value);
        self.global_settings = Value::Object(global);
        self.mark_modified(field, None);
        self.save();
    }

    /// Set one key inside a nested global field and persist **only that key**, leaving the
    /// rest of the on-disk object alone — the primitive behind `setCompactionEnabled`,
    /// `setRetryEnabled`, `setShowImages`, `setImageWidthCells`, `setClearOnShrink`,
    /// `setShowTerminalProgress`, `setImageAutoResize` and `setBlockImages` (they all call
    /// `markModified(field, nestedKey)`).
    ///
    /// Creates the container when absent, as each setter's `if (!this.globalSettings.x) {…}`
    /// does (e.g. `:765-767`).
    pub fn set_global_nested_field(&mut self, field: &str, nested_key: &str, value: Value) {
        let mut global = spread(&self.global_settings);
        let mut container = global
            .get(field)
            .filter(|existing| is_js_plain_object(existing))
            .map(spread)
            .unwrap_or_default();
        container.insert(nested_key.to_string(), value);
        global.insert(field.to_string(), Value::Object(container));
        self.global_settings = Value::Object(global);
        self.mark_modified(field, Some(nested_key));
        self.save();
    }
}

/// JS `Set.add` over an insertion-ordered `Vec`.
fn push_unique(set: &mut Vec<String>, value: &str) {
    if !set.iter().any(|existing| existing == value) {
        set.push(value.to_string());
    }
}

/// `map.get(field).add(nestedKey)`, creating the entry on demand (`:516-519`).
fn push_nested(map: &mut Vec<(String, Vec<String>)>, field: &str, nested_key: &str) {
    match map.iter_mut().find(|(existing, _)| existing == field) {
        Some((_, keys)) => push_unique(keys, nested_key),
        None => map.push((field.to_string(), vec![nested_key.to_string()])),
    }
}

/// `modifiedNestedFields.has(field) ? modifiedNestedFields.get(field) : undefined` (`:591`).
fn nested_keys_for<'a>(map: &'a [(String, Vec<String>)], field: &str) -> Option<&'a Vec<String>> {
    map.iter()
        .find(|(existing, _)| existing == field)
        .map(|(_, keys)| keys)
}

// =============================================================================
// Getters and setters (`settings-manager.ts:660-1233`)
// =============================================================================

/// `x ?? default` for a boolean-typed field.
///
/// `??` falls back for `null`/`undefined` only, so a *non*-boolean value is returned as-is by
/// Pi. Every consumer immediately uses it in a boolean position, so its truthiness is the
/// observable behaviour, and that is what this yields.
fn nullish_bool(value: Option<&Value>, default: bool) -> bool {
    match value {
        None | Some(Value::Null) => default,
        Some(Value::Bool(flag)) => *flag,
        Some(other) => js_truthy(Some(other)),
    }
}

/// `x ?? default` for a number-typed field. A non-numeric value is coerced the way JS coerces
/// it in arithmetic (`Number(x)`), with `NaN` falling back to the default.
fn nullish_number(value: Option<&Value>, default: f64) -> f64 {
    match value {
        None | Some(Value::Null) => default,
        Some(Value::Number(number)) => number.as_f64().unwrap_or(default),
        Some(Value::String(text)) => text.trim().parse::<f64>().unwrap_or(default),
        Some(Value::Bool(flag)) => f64::from(u8::from(*flag)),
        Some(_) => default,
    }
}

/// `parseHttpIdleTimeoutMs(value)` (`core/http-dispatcher.ts:17-33`).
///
/// Lives here rather than in an `http` module because nothing else in feat-005 needs it; it
/// moves when the dispatcher is ported. `"disabled"` (case-insensitive, trimmed) is `0`, an
/// empty string is "unset", any other string is re-run as a number, and a negative or
/// non-finite number is "unset". Numbers are floored.
///
/// One divergence: JS `Number("0x10")` is `16` and `Number("Infinity")` is `Infinity`, while
/// Rust's `f64` parser rejects the first and accepts `"inf"`. A hex-literal timeout string
/// therefore becomes a throw here and `16` in Pi. `"Infinity"` is rejected either way, since
/// `Number.isFinite` filters it.
fn parse_http_idle_timeout_ms(value: Option<&Value>) -> Option<f64> {
    match value {
        Some(Value::String(text)) => {
            let trimmed = text.trim();
            if trimmed.eq_ignore_ascii_case("disabled") {
                return Some(0.0);
            }
            if trimmed.is_empty() {
                return None;
            }
            let number = trimmed.parse::<f64>().ok()?;
            parse_http_idle_timeout_ms(Some(&Value::Number(Number::from_f64(number)?)))
        }
        Some(Value::Number(number)) => {
            let number = number.as_f64()?;
            if !number.is_finite() || number < 0.0 {
                return None;
            }
            Some(number.floor())
        }
        _ => None,
    }
}

impl SettingsManager {
    /// `parseTimeoutSetting(value, settingName)` (`:162-171`).
    ///
    /// # Errors
    ///
    /// [`SettingsErrorKind::InvalidTimeoutSetting`] when the value is present but
    /// unparseable — Pi throws out of the getter, so this is not a load-time diagnostic.
    fn parse_timeout_setting(
        value: Option<&Value>,
        setting_name: &'static str,
    ) -> Result<Option<f64>, SettingsErrorKind> {
        if let Some(timeout_ms) = parse_http_idle_timeout_ms(value) {
            return Ok(Some(timeout_ms));
        }
        match value {
            None => Ok(None),
            Some(value) => Err(SettingsErrorKind::InvalidTimeoutSetting {
                setting: setting_name,
                value: js_string(value),
            }),
        }
    }

    /// `getLastChangelogVersion()` (`:660-662`).
    pub fn get_last_changelog_version(&self) -> Option<&str> {
        self.get_merged_field("lastChangelogVersion")
            .and_then(Value::as_str)
    }

    /// `setLastChangelogVersion(version)` (`:664-668`).
    pub fn set_last_changelog_version(&mut self, version: &str) {
        self.set_global_field("lastChangelogVersion", Value::String(version.to_string()));
    }

    /// `getSessionDir()` (`:670-673`) — `sessionDir ? normalizePath(sessionDir) : sessionDir`,
    /// i.e. tilde-expanded when truthy and returned untouched when falsy.
    ///
    /// Expansion is [`ConfigEnv::expand_tilde_path`] (`normalizePath` with no options), not a
    /// second implementation. A `null` value yields `None` here where Pi yields `null`; every
    /// consumer guards on truthiness (`main.ts:573-577`), so the two are indistinguishable.
    ///
    /// # Errors
    ///
    /// Propagates `normalizePath`'s `file://` failure, which Pi does not catch either.
    pub fn get_session_dir(&self, env: &ConfigEnv) -> Result<Option<String>, ConfigPathError> {
        let Some(value) = self.get_merged_field("sessionDir") else {
            return Ok(None);
        };
        if !js_truthy(Some(value)) {
            return Ok(value.as_str().map(str::to_string));
        }
        match value.as_str() {
            Some(text) => Ok(Some(env.expand_tilde_path(text)?)),
            // Truthy but not a string: Pi hands the raw value to `normalizePath`, which would
            // throw on `.startsWith`. Unreachable from any writer.
            None => Ok(None),
        }
    }

    /// `getDefaultProvider()` (`:675-677`).
    pub fn get_default_provider(&self) -> Option<&str> {
        self.get_merged_field("defaultProvider")
            .and_then(Value::as_str)
    }

    /// `getDefaultModel()` (`:679-681`).
    pub fn get_default_model(&self) -> Option<&str> {
        self.get_merged_field("defaultModel")
            .and_then(Value::as_str)
    }

    /// `setDefaultProvider(provider)` (`:683-687`).
    pub fn set_default_provider(&mut self, provider: &str) {
        self.set_global_field("defaultProvider", Value::String(provider.to_string()));
    }

    /// `setDefaultModel(modelId)` (`:689-693`).
    pub fn set_default_model(&mut self, model_id: &str) {
        self.set_global_field("defaultModel", Value::String(model_id.to_string()));
    }

    /// `setDefaultModelAndProvider(provider, modelId)` (`:695-701`) — both fields marked, one
    /// write, so the file gains both keys atomically.
    pub fn set_default_model_and_provider(&mut self, provider: &str, model_id: &str) {
        let mut global = spread(&self.global_settings);
        global.insert(
            "defaultProvider".to_string(),
            Value::String(provider.to_string()),
        );
        global.insert(
            "defaultModel".to_string(),
            Value::String(model_id.to_string()),
        );
        self.global_settings = Value::Object(global);
        self.mark_modified("defaultProvider", None);
        self.mark_modified("defaultModel", None);
        self.save();
    }

    /// `getSteeringMode()` (`:703-705`) — `|| "one-at-a-time"`, so `""` also falls back.
    ///
    /// An unrecognized non-empty string cannot be represented by [`QueueDeliveryMode`]; Pi
    /// would forward it verbatim. It becomes the default here.
    pub fn get_steering_mode(&self) -> QueueDeliveryMode {
        Self::queue_mode_or_default(self.get_merged_field("steeringMode"))
    }

    /// `getFollowUpMode()` (`:713-715`) — same `||` fallback.
    pub fn get_follow_up_mode(&self) -> QueueDeliveryMode {
        Self::queue_mode_or_default(self.get_merged_field("followUpMode"))
    }

    fn queue_mode_or_default(value: Option<&Value>) -> QueueDeliveryMode {
        if !js_truthy(value) {
            return QueueDeliveryMode::OneAtATime;
        }
        value
            .and_then(|value| serde_json::from_value(value.clone()).ok())
            .unwrap_or(QueueDeliveryMode::OneAtATime)
    }

    /// `getDefaultThinkingLevel()` (`:740-742`). An unrecognized level yields `None`, where Pi
    /// forwards the raw string.
    pub fn get_default_thinking_level(&self) -> Option<ThinkingLevel> {
        self.get_merged_field("defaultThinkingLevel")
            .and_then(|value| serde_json::from_value(value.clone()).ok())
    }

    /// `getTransport()` (`:750-752`) — `?? "auto"`.
    ///
    /// An unrecognized value cannot be represented by [`pirust_ai::types::Transport`]; Pi
    /// forwards the raw string to the AI layer. It becomes `Auto` here.
    pub fn get_transport(&self) -> Transport {
        self.get_merged_field("transport")
            .and_then(|value| serde_json::from_value(value.clone()).ok())
            .unwrap_or(Transport::Auto)
    }

    /// `getCompactionSettings()` (`:781-787`).
    pub fn get_compaction_settings(&self) -> ResolvedCompactionSettings {
        ResolvedCompactionSettings {
            enabled: nullish_bool(
                self.merged_path(&["compaction", "enabled"]),
                DEFAULT_COMPACTION_ENABLED,
            ),
            reserve_tokens: nullish_number(
                self.merged_path(&["compaction", "reserveTokens"]),
                DEFAULT_COMPACTION_RESERVE_TOKENS,
            ),
            keep_recent_tokens: nullish_number(
                self.merged_path(&["compaction", "keepRecentTokens"]),
                DEFAULT_COMPACTION_KEEP_RECENT_TOKENS,
            ),
        }
    }

    /// `getBranchSummarySettings()` (`:789-794`).
    pub fn get_branch_summary_settings(&self) -> ResolvedBranchSummarySettings {
        ResolvedBranchSummarySettings {
            reserve_tokens: nullish_number(
                self.merged_path(&["branchSummary", "reserveTokens"]),
                DEFAULT_BRANCH_SUMMARY_RESERVE_TOKENS,
            ),
            skip_prompt: nullish_bool(
                self.merged_path(&["branchSummary", "skipPrompt"]),
                DEFAULT_BRANCH_SUMMARY_SKIP_PROMPT,
            ),
        }
    }

    /// `getRetrySettings()` (`:813-819`).
    pub fn get_retry_settings(&self) -> ResolvedRetrySettings {
        ResolvedRetrySettings {
            enabled: nullish_bool(
                self.merged_path(&["retry", "enabled"]),
                DEFAULT_RETRY_ENABLED,
            ),
            max_retries: nullish_number(
                self.merged_path(&["retry", "maxRetries"]),
                DEFAULT_RETRY_MAX_RETRIES,
            ),
            base_delay_ms: nullish_number(
                self.merged_path(&["retry", "baseDelayMs"]),
                DEFAULT_RETRY_BASE_DELAY_MS,
            ),
        }
    }

    /// `getProviderRetrySettings()` (`:834-840`).
    pub fn get_provider_retry_settings(&self) -> ResolvedProviderRetrySettings {
        ResolvedProviderRetrySettings {
            timeout_ms: self
                .merged_path(&["retry", "provider", "timeoutMs"])
                .and_then(Value::as_f64),
            max_retries: self
                .merged_path(&["retry", "provider", "maxRetries"])
                .and_then(Value::as_f64),
            max_retry_delay_ms: nullish_number(
                self.merged_path(&["retry", "provider", "maxRetryDelayMs"]),
                DEFAULT_PROVIDER_MAX_RETRY_DELAY_MS,
            ),
        }
    }

    /// `getHttpIdleTimeoutMs()` (`:821-823`) — `?? DEFAULT_HTTP_IDLE_TIMEOUT_MS`.
    ///
    /// # Errors
    ///
    /// [`SettingsErrorKind::InvalidTimeoutSetting`] for a present-but-unparseable value.
    pub fn get_http_idle_timeout_ms(&self) -> Result<f64, SettingsErrorKind> {
        Ok(Self::parse_timeout_setting(
            self.get_merged_field("httpIdleTimeoutMs"),
            "httpIdleTimeoutMs",
        )?
        .unwrap_or(DEFAULT_HTTP_IDLE_TIMEOUT_MS))
    }

    /// `getWebSocketConnectTimeoutMs()` (`:842-844`) — no default; `None` means "unset".
    ///
    /// # Errors
    ///
    /// [`SettingsErrorKind::InvalidTimeoutSetting`] for a present-but-unparseable value.
    pub fn get_websocket_connect_timeout_ms(&self) -> Result<Option<f64>, SettingsErrorKind> {
        Self::parse_timeout_setting(
            self.get_merged_field("websocketConnectTimeoutMs"),
            "websocketConnectTimeoutMs",
        )
    }

    /// `setHttpIdleTimeoutMs(timeoutMs)` (`:825-832`) — validates then floors.
    ///
    /// # Errors
    ///
    /// [`SettingsErrorKind::InvalidTimeoutSetting`] for a non-finite or negative value, which
    /// Pi reports with the same `Invalid httpIdleTimeoutMs setting: …` text.
    pub fn set_http_idle_timeout_ms(&mut self, timeout_ms: f64) -> Result<(), SettingsErrorKind> {
        if !timeout_ms.is_finite() || timeout_ms < 0.0 {
            return Err(SettingsErrorKind::InvalidTimeoutSetting {
                setting: "httpIdleTimeoutMs",
                value: format_js_number(timeout_ms),
            });
        }
        let floored = Number::from_f64(timeout_ms.floor()).ok_or_else(|| {
            SettingsErrorKind::InvalidTimeoutSetting {
                setting: "httpIdleTimeoutMs",
                value: format_js_number(timeout_ms),
            }
        })?;
        self.set_global_field("httpIdleTimeoutMs", Value::Number(floored));
        Ok(())
    }

    /// `getQuietStartup()` (`:889-891`).
    pub fn get_quiet_startup(&self) -> bool {
        nullish_bool(self.get_merged_field("quietStartup"), false)
    }

    /// `getCollapseChangelog()` (`:930-932`).
    pub fn get_collapse_changelog(&self) -> bool {
        nullish_bool(self.get_merged_field("collapseChangelog"), false)
    }

    /// `getDefaultProjectTrust()` (`:899-902`).
    ///
    /// Reads **`globalSettings`, not the merged view** — a project must not be able to declare
    /// itself trusted — and coerces anything other than `"always"`/`"never"` to `"ask"`.
    pub fn get_default_project_trust(&self) -> DefaultProjectTrust {
        match js_get(&self.global_settings, "defaultProjectTrust").and_then(Value::as_str) {
            Some("always") => DefaultProjectTrust::Always,
            Some("never") => DefaultProjectTrust::Never,
            _ => DefaultProjectTrust::Ask,
        }
    }

    /// `setDefaultProjectTrust(trust)` (`:904-908`).
    pub fn set_default_project_trust(&mut self, trust: DefaultProjectTrust) {
        let value = serde_json::to_value(trust).expect("DefaultProjectTrust always serializes");
        self.set_global_field("defaultProjectTrust", value);
    }

    /// `getThinkingBudgets()` (`:1059-1061`) — passed straight through, no defaults.
    ///
    /// A budget that is not a non-negative integer yields `None` (the shared
    /// [`ThinkingBudgets`] types them as `u64`); [`SettingsManager::get_merged_field`] returns
    /// the raw value for callers that need it.
    pub fn get_thinking_budgets(&self) -> Option<ThinkingBudgets> {
        self.get_merged_field("thinkingBudgets")
            .and_then(|value| serde_json::from_value(value.clone()).ok())
    }

    /// `getEnabledModels()` (`:1149-1151`) — `undefined` and `[]` are **different** answers
    /// (`main.ts:689`), so this does not default to an empty vector.
    pub fn get_enabled_models(&self) -> Option<Vec<String>> {
        let value = self.get_merged_field("enabledModels")?;
        let items = value.as_array()?;
        Some(
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect(),
        )
    }

    /// `getImageAutoResize()` (`:1123-1125`).
    pub fn get_image_auto_resize(&self) -> bool {
        nullish_bool(
            self.merged_path(&["images", "autoResize"]),
            DEFAULT_IMAGE_AUTO_RESIZE,
        )
    }

    /// `getBlockImages()` (`:1136-1138`).
    pub fn get_block_images(&self) -> bool {
        nullish_bool(
            self.merged_path(&["images", "blockImages"]),
            DEFAULT_BLOCK_IMAGES,
        )
    }

    /// `getEnableInstallTelemetry()` (`settings-manager.ts:940-942`).
    pub fn get_enable_install_telemetry(&self) -> bool {
        nullish_bool(
            self.merged_path(&["enableInstallTelemetry"]),
            DEFAULT_ENABLE_INSTALL_TELEMETRY,
        )
    }
}

/// `String(n)` for the numbers that reach an error message. Integral values print without a
/// fractional part, as JS does (`String(5)` is `"5"`, not `"5.0"`).
fn format_js_number(value: f64) -> String {
    if value.is_nan() {
        return "NaN".to_string();
    }
    if value.is_infinite() {
        // Rust's `Display` writes `inf`/`-inf`; JS writes `Infinity`/`-Infinity`.
        return if value.is_sign_negative() {
            "-Infinity".to_string()
        } else {
            "Infinity".to_string()
        };
    }
    if value.fract() == 0.0 && value.abs() < 1e21 {
        format!("{value:.0}")
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Platform, PIRUST};

    fn manager_with(global: Option<&str>, project: Option<&str>, trusted: bool) -> SettingsManager {
        let storage = InMemorySettingsStorage::with_contents(
            global.map(str::to_string),
            project.map(str::to_string),
        );
        SettingsManager::from_storage(
            Arc::new(storage),
            SettingsManagerCreateOptions {
                project_trusted: trusted,
            },
        )
    }

    #[test]
    fn the_undefined_sentinel_is_not_an_object() {
        // The whole three-state model rests on this: `typeof undefined` is not `"object"`.
        assert!(is_js_undefined(&js_undefined()));
        assert!(!is_js_plain_object(&js_undefined()));
        // Near-misses stay real objects.
        assert!(is_js_plain_object(
            &serde_json::json!({"$undefined": false})
        ));
        assert!(is_js_plain_object(
            &serde_json::json!({"$undefined": true, "x": 1})
        ));
        assert!(!is_js_undefined(&Value::Null));
        // …and `null` is a value the merge copies, unlike `undefined`.
        let merged = deep_merge_settings(
            &serde_json::json!({"a": 1}),
            &serde_json::json!({"a": null}),
        );
        assert_eq!(merged.get("a"), Some(&Value::Null));
    }

    #[test]
    fn json_stringify_treats_undefined_as_json_stringify_does() {
        // Object entries vanish; array holes become `null` (JSON.stringify([undefined]) is
        // "[null]"). This is what makes `setShellPath(undefined)` delete the key.
        let value = serde_json::json!({
            "keep": 1,
            "drop": {"$undefined": true},
            "list": [1, {"$undefined": true}],
        });
        assert_eq!(
            json_stringify(&value),
            "{\n  \"keep\": 1,\n  \"list\": [\n    1,\n    null\n  ]\n}"
        );
    }

    #[test]
    fn settings_lens_round_trips_byte_identically() {
        // Integers must come back as integers: an `f64` field would emit `16384.0` and break
        // byte-compat with `JSON.stringify`.
        //
        // The literal is in `Settings` **declaration** order, which is what the lens emits and
        // what Pi's `JSON.stringify` emits for a freshly built object (spec §6.1). A file's own
        // order is preserved by the raw pipeline (`persistScopedSettings`), not by this lens —
        // see `tests/settings_golden.rs`, which pins that separately.
        let literal = concat!(
            "{\n",
            "  \"defaultProvider\": \"anthropic\",\n",
            "  \"defaultThinkingLevel\": \"medium\",\n",
            "  \"transport\": \"websocket-cached\",\n",
            "  \"steeringMode\": \"one-at-a-time\",\n",
            "  \"compaction\": {\n",
            "    \"enabled\": true,\n",
            "    \"reserveTokens\": 16384\n",
            "  },\n",
            "  \"retry\": {\n",
            "    \"provider\": {\n",
            "      \"maxRetryDelayMs\": 60000\n",
            "    }\n",
            "  },\n",
            "  \"defaultProjectTrust\": \"always\",\n",
            "  \"packages\": [\n",
            "    \"npm:a\",\n",
            "    {\n",
            "      \"source\": \"npm:b\",\n",
            "      \"autoload\": false,\n",
            "      \"skills\": [\n",
            "        \"x\"\n",
            "      ]\n",
            "    }\n",
            "  ],\n",
            "  \"doubleEscapeAction\": \"tree\",\n",
            "  \"treeFilterMode\": \"no-tools\",\n",
            "  \"thinkingBudgets\": {\n",
            "    \"high\": 32000\n",
            "  },\n",
            "  \"editorPaddingX\": 2,\n",
            "  \"outputPad\": 0,\n",
            "  \"httpIdleTimeoutMs\": 300000\n",
            "}",
        );
        let map: SettingsMap = serde_json::from_str(literal).expect("fixture-shaped literal");
        let settings = Settings::from_map(&map).expect("every field is well typed");
        assert_eq!(settings.to_json_string(), literal);
        assert_eq!(settings.default_provider.as_deref(), Some("anthropic"));
        assert_eq!(settings.transport, Some(Transport::WebsocketCached));
        assert_eq!(settings.tree_filter_mode, Some(TreeFilterMode::NoTools));
        assert_eq!(
            settings.default_project_trust,
            Some(DefaultProjectTrust::Always)
        );
        assert_eq!(settings.packages.as_ref().map(Vec::len), Some(2));
    }

    #[test]
    fn unknown_keys_survive_the_typed_lens() {
        let mut map = SettingsMap::new();
        map.insert("nobodyKnowsThis".to_string(), Value::from(7));
        let settings = Settings::from_map(&map).expect("unknown keys land in `extra`");
        assert_eq!(settings.extra.get("nobodyKnowsThis"), Some(&Value::from(7)));
        assert_eq!(settings.to_map(), map);
    }

    #[test]
    fn untrusted_project_settings_are_never_read_or_written() {
        let mut manager = manager_with(Some(r#"{"theme":"dark"}"#), Some("{ not json"), false);
        // No diagnostic at all: the file is not even opened (`:351-353`).
        assert!(manager.drain_errors().is_empty());
        assert_eq!(
            manager.merged_settings().get("theme"),
            Some(&Value::String("dark".to_string()))
        );
        assert_eq!(
            manager.save_project_settings(&serde_json::json!({"theme": "light"})),
            Err(SettingsErrorKind::ProjectNotTrusted)
        );
        assert_eq!(
            manager.update_project_field("skills", |settings| {
                settings.insert("skills".to_string(), Value::Array(vec![]));
            }),
            Err(SettingsErrorKind::ProjectNotTrusted)
        );
        // The refusal happens before any mutation.
        assert_eq!(manager.get_project_settings(), serde_json::json!({}));
    }

    #[test]
    fn settings_json_does_not_tolerate_json_comments() {
        // `loadFromStorage` calls bare `JSON.parse` (`:364`). `models.json` runs
        // `stripJsonComments` first; `settings.json` does NOT (spec §6.2 vs §9.1), so a
        // commented settings file loses the whole scope to a SyntaxError.
        for commented in [
            "{\n  // the theme\n  \"theme\": \"dark\"\n}",
            "{\n  /* the theme */\n  \"theme\": \"dark\"\n}",
        ] {
            let mut manager = manager_with(Some(commented), None, true);
            let drained = manager.drain_errors();
            assert_eq!(drained.len(), 1, "input: {commented}");
            assert_eq!(drained[0].scope, SettingsScope::Global);
            assert_eq!(drained[0].error.class_name(), "SyntaxError");
            assert_eq!(manager.get_global_settings(), serde_json::json!({}));
            assert_eq!(manager.merged_settings().len(), 0);
        }
        // The same bytes without the comment load fine, proving the comment is the defect.
        let manager = manager_with(Some("{\n  \"theme\": \"dark\"\n}"), None, true);
        assert_eq!(
            manager.merged_settings().get("theme"),
            Some(&Value::String("dark".to_string()))
        );
    }

    #[test]
    fn granting_trust_reads_the_project_file_and_queues_its_error() {
        let mut manager = manager_with(Some("{}"), Some("{ not json"), false);
        assert!(manager.drain_errors().is_empty());
        manager.set_project_trusted(true);
        let drained = manager.drain_errors();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].scope, SettingsScope::Project);
        assert_eq!(drained[0].error.class_name(), "SyntaxError");
        // Revoking it clears the recorded failure so it stops being re-reported.
        manager.set_project_trusted(false);
        assert!(manager.drain_errors().is_empty());
        assert_eq!(manager.get_project_settings(), serde_json::json!({}));
    }

    #[test]
    fn a_scope_that_failed_to_load_is_never_clobbered() {
        let storage = Arc::new(InMemorySettingsStorage::with_contents(
            Some("{ not json".to_string()),
            None,
        ));
        let mut manager =
            SettingsManager::from_storage(storage.clone(), SettingsManagerCreateOptions::default());
        assert_eq!(manager.drain_errors().len(), 1);
        manager.set_default_model("m");
        // `save()` returns early (`:612-614`), so the broken text is still there.
        assert_eq!(
            storage.contents(SettingsScope::Global).as_deref(),
            Some("{ not json")
        );
        // The in-memory view still reflects the change.
        assert_eq!(manager.get_default_model(), Some("m"));
        assert!(manager.drain_errors().is_empty());
    }

    #[test]
    fn a_nested_write_touches_only_the_modified_key() {
        let storage = Arc::new(InMemorySettingsStorage::with_contents(
            Some(r#"{"terminal":{"showImages":false,"imageWidthCells":40}}"#.to_string()),
            None,
        ));
        let mut manager =
            SettingsManager::from_storage(storage.clone(), SettingsManagerCreateOptions::default());
        manager.set_global_nested_field("terminal", "imageWidthCells", Value::from(80));
        assert_eq!(
            storage.contents(SettingsScope::Global).as_deref(),
            Some(concat!(
                "{\n",
                "  \"terminal\": {\n",
                // `showImages` came back from the file, untouched by this process.
                "    \"showImages\": false,\n",
                "    \"imageWidthCells\": 80\n",
                "  }\n",
                "}",
            ))
        );
    }

    #[test]
    fn apply_overrides_never_reaches_disk() {
        let storage = Arc::new(InMemorySettingsStorage::with_contents(
            Some(r#"{"theme":"dark"}"#.to_string()),
            None,
        ));
        let mut manager =
            SettingsManager::from_storage(storage.clone(), SettingsManagerCreateOptions::default());
        let mut overrides = SettingsMap::new();
        overrides.insert("theme".to_string(), Value::String("light".to_string()));
        manager.apply_overrides(&overrides);
        assert_eq!(
            manager.merged_settings().get("theme"),
            Some(&Value::String("light".to_string()))
        );
        // `applyOverrides` mutates the merged view only (`:508-510`).
        assert_eq!(
            manager.get_global_settings(),
            serde_json::json!({"theme": "dark"})
        );
        assert_eq!(
            storage.contents(SettingsScope::Global).as_deref(),
            Some(r#"{"theme":"dark"}"#)
        );
        // …and the next save recomputes the merge from the two scopes, dropping it.
        manager.set_last_changelog_version("1.0.0");
        assert_eq!(
            manager.merged_settings().get("theme"),
            Some(&Value::String("dark".to_string()))
        );
    }

    #[test]
    fn reload_keeps_the_previous_value_when_the_file_breaks() {
        let storage = Arc::new(InMemorySettingsStorage::with_contents(
            Some(r#"{"theme":"dark"}"#.to_string()),
            None,
        ));
        let mut manager =
            SettingsManager::from_storage(storage.clone(), SettingsManagerCreateOptions::default());
        storage
            .with_lock(SettingsScope::Global, &mut |_| Some("oops".to_string()))
            .expect("in-memory storage never fails");
        manager.reload();
        assert_eq!(manager.drain_errors().len(), 1);
        // Only the success branch assigns (`:482-488`).
        assert_eq!(
            manager.get_global_settings(),
            serde_json::json!({"theme": "dark"})
        );
    }

    #[test]
    fn project_settings_layer_over_global_and_trust_gates_them() {
        let mut manager = manager_with(
            Some(r#"{"defaultModel":"a","retry":{"enabled":true,"provider":{"timeoutMs":1000}}}"#),
            Some(r#"{"defaultModel":"b","retry":{"provider":{"maxRetries":9}}}"#),
            true,
        );
        assert_eq!(manager.get_default_model(), Some("b"));
        let provider = manager.get_provider_retry_settings();
        // 0.84.2 recursive merge: `retry.provider.timeoutMs` is KEPT from global and
        // merged with the project's `maxRetries`.
        assert_eq!(provider.timeout_ms, Some(1000.0));
        assert_eq!(provider.max_retries, Some(9.0));
        assert_eq!(
            provider.max_retry_delay_ms,
            DEFAULT_PROVIDER_MAX_RETRY_DELAY_MS
        );
        assert!(manager.get_retry_settings().enabled);
        // Untrusting the project restores the global values wholesale.
        manager.set_project_trusted(false);
        assert_eq!(manager.get_default_model(), Some("a"));
        assert_eq!(
            manager.get_provider_retry_settings().timeout_ms,
            Some(1000.0)
        );
    }

    #[test]
    fn getters_apply_pis_defaults() {
        let manager = manager_with(Some("{}"), None, true);
        assert_eq!(manager.get_transport(), Transport::Auto);
        assert_eq!(manager.get_steering_mode(), QueueDeliveryMode::OneAtATime);
        assert_eq!(manager.get_follow_up_mode(), QueueDeliveryMode::OneAtATime);
        assert_eq!(
            manager.get_default_project_trust(),
            DefaultProjectTrust::Ask
        );
        assert_eq!(manager.get_enabled_models(), None);
        assert!(manager.get_image_auto_resize());
        assert!(!manager.get_block_images());
        assert!(!manager.get_quiet_startup());
        assert!(!manager.get_collapse_changelog());
        assert_eq!(manager.get_thinking_budgets(), None);
        assert_eq!(
            manager.get_http_idle_timeout_ms(),
            Ok(DEFAULT_HTTP_IDLE_TIMEOUT_MS)
        );
        assert_eq!(manager.get_websocket_connect_timeout_ms(), Ok(None));
        assert_eq!(
            manager.get_compaction_settings(),
            ResolvedCompactionSettings {
                enabled: DEFAULT_COMPACTION_ENABLED,
                reserve_tokens: DEFAULT_COMPACTION_RESERVE_TOKENS,
                keep_recent_tokens: DEFAULT_COMPACTION_KEEP_RECENT_TOKENS,
            }
        );
        assert_eq!(
            manager.get_branch_summary_settings(),
            ResolvedBranchSummarySettings {
                reserve_tokens: DEFAULT_BRANCH_SUMMARY_RESERVE_TOKENS,
                skip_prompt: DEFAULT_BRANCH_SUMMARY_SKIP_PROMPT,
            }
        );

        // `steeringMode` uses `||`, so an empty string also falls back (`:704`).
        let empty = manager_with(
            Some(r#"{"steeringMode":"","followUpMode":"all"}"#),
            None,
            true,
        );
        assert_eq!(empty.get_steering_mode(), QueueDeliveryMode::OneAtATime);
        assert_eq!(empty.get_follow_up_mode(), QueueDeliveryMode::All);
    }

    #[test]
    fn default_project_trust_ignores_the_project_scope() {
        // A project must not be able to declare itself trusted (`:900` reads globalSettings).
        let manager = manager_with(
            Some("{}"),
            Some(r#"{"defaultProjectTrust":"always"}"#),
            true,
        );
        assert_eq!(
            manager.get_default_project_trust(),
            DefaultProjectTrust::Ask
        );
        assert_eq!(
            manager.merged_settings().get("defaultProjectTrust"),
            Some(&Value::String("always".to_string()))
        );
    }

    #[test]
    fn timeout_settings_parse_and_throw_like_pi() {
        let disabled = manager_with(Some(r#"{"httpIdleTimeoutMs":"disabled"}"#), None, true);
        assert_eq!(disabled.get_http_idle_timeout_ms(), Ok(0.0));
        let floored = manager_with(Some(r#"{"httpIdleTimeoutMs":1500.9}"#), None, true);
        assert_eq!(floored.get_http_idle_timeout_ms(), Ok(1500.0));
        let empty = manager_with(Some(r#"{"httpIdleTimeoutMs":"  "}"#), None, true);
        // An empty string parses as "unset", but it is not `undefined`, so Pi throws (`:167`).
        assert_eq!(
            empty.get_http_idle_timeout_ms(),
            Err(SettingsErrorKind::InvalidTimeoutSetting {
                setting: "httpIdleTimeoutMs",
                value: "  ".to_string(),
            })
        );
        let negative = manager_with(Some(r#"{"websocketConnectTimeoutMs":-1}"#), None, true);
        assert_eq!(
            negative.get_websocket_connect_timeout_ms(),
            Err(SettingsErrorKind::InvalidTimeoutSetting {
                setting: "websocketConnectTimeoutMs",
                value: "-1".to_string(),
            })
        );

        let mut manager = manager_with(Some("{}"), None, true);
        assert_eq!(
            manager.set_http_idle_timeout_ms(f64::INFINITY),
            Err(SettingsErrorKind::InvalidTimeoutSetting {
                setting: "httpIdleTimeoutMs",
                value: "Infinity".to_string(),
            })
        );
        assert!(manager.set_http_idle_timeout_ms(1200.7).is_ok());
        assert_eq!(manager.get_http_idle_timeout_ms(), Ok(1200.0));
    }

    #[test]
    fn in_memory_migrates_its_seed() {
        let mut seed = SettingsMap::new();
        seed.insert("queueMode".to_string(), Value::String("all".to_string()));
        seed.insert("websockets".to_string(), Value::Bool(true));
        let manager = SettingsManager::in_memory(&seed, SettingsManagerCreateOptions::default());
        assert_eq!(
            manager.get_global_settings(),
            serde_json::json!({"steeringMode": "all", "transport": "websocket"})
        );
        assert_eq!(manager.get_steering_mode(), QueueDeliveryMode::All);
        assert_eq!(manager.get_transport(), Transport::Websocket);
    }

    #[test]
    fn diagnostic_text_matches_main_ts() {
        let error = SettingsError {
            scope: SettingsScope::Global,
            error: SettingsErrorKind::InOperatorOnPrimitive {
                key: "queueMode",
                value: "null".to_string(),
            },
        };
        assert_eq!(
            error.diagnostic_message("startup"),
            "(startup, global settings) Cannot use 'in' operator to search for 'queueMode' in null"
        );
        assert_eq!(
            SettingsError {
                scope: SettingsScope::Project,
                error: SettingsErrorKind::ProjectNotTrusted,
            }
            .diagnostic_message("reload"),
            "(reload, project settings) Project is not trusted; refusing to write project settings"
        );
    }

    #[test]
    fn file_storage_creates_nothing_until_a_write_happens() {
        let temp = tempfile::tempdir().expect("tempdir");
        let agent_dir = temp.path().join("agent");
        let cwd = temp.path().join("work");
        let env = ConfigEnv {
            identity: PIRUST,
            platform: Platform::current(),
            home_dir: Some(temp.path().to_string_lossy().into_owned()),
            agent_dir_override: Some(agent_dir.to_string_lossy().into_owned()),
        };

        let mut manager =
            SettingsManager::create(&env, &cwd, SettingsManagerCreateOptions::default())
                .expect("paths resolve");
        // Reading two absent files must not mkdir (`:240-243`).
        assert!(!agent_dir.exists());
        assert!(!cwd.exists());
        assert!(manager.drain_errors().is_empty());
        assert_eq!(manager.merged_settings().len(), 0);

        manager.set_default_provider("anthropic");
        let global_path = agent_dir.join(SETTINGS_FILE_NAME);
        assert_eq!(
            std::fs::read_to_string(&global_path).expect("global settings written"),
            "{\n  \"defaultProvider\": \"anthropic\"\n}"
        );
        // The lock directory is released (`:249-253`).
        assert!(!lock_path_for(&global_path).exists());
        assert!(manager.drain_errors().is_empty());

        // A project write lands under `<cwd>/.pirust/settings.json` (`:196`).
        manager
            .update_project_field("skills", |settings| {
                settings.insert(
                    "skills".to_string(),
                    Value::Array(vec![Value::String("/p/s".to_string())]),
                );
            })
            .expect("project is trusted by default");
        let project_path = cwd.join(CONFIG_DIR_NAME).join(SETTINGS_FILE_NAME);
        assert_eq!(
            std::fs::read_to_string(&project_path).expect("project settings written"),
            "{\n  \"skills\": [\n    \"/p/s\"\n  ]\n}"
        );
        assert_eq!(
            manager.merged_settings().get("skills"),
            Some(&serde_json::json!(["/p/s"]))
        );

        // Re-reading from disk reproduces the merged view.
        manager.reload();
        assert!(manager.drain_errors().is_empty());
        assert_eq!(manager.get_default_provider(), Some("anthropic"));
        assert_eq!(
            manager.merged_settings().get("skills"),
            Some(&serde_json::json!(["/p/s"]))
        );
    }
}
