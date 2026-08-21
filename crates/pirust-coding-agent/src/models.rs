//! Port of `core/models-store.ts` + model resolution — `models.json`, custom-provider
//! merging, `--model`/`--provider`/settings precedence, and the `:thinking` suffix.
//!
//! feat-005 targets a single `anthropic` provider (the only ported api adapter); the
//! remaining ~10 apis and ~35 providers land in feat-008.
//!
//! Gated by `tests/fixtures/pi/cli/models.cases.jsonl` — 158 records captured from real Pi
//! 0.80.10, 18 of which ran a real `ModelRuntime` against the real builtin catalog. Every
//! literal string, tie-break and quirk below is that fixture's, not a self-authored
//! expectation.
//!
//! # What is ported
//!
//! | Pi | here |
//! |---|---|
//! | `DEFAULT_THINKING_LEVEL` (`core/defaults.ts:3`) | [`DEFAULT_THINKING_LEVEL`] |
//! | `defaultModelPerProvider` (`core/model-resolver.ts:14-51`) | [`DEFAULT_MODEL_PER_PROVIDER`] |
//! | `isAlias` (`model-resolver.ts:63-70`) | [`is_alias`] |
//! | `findExactModelReferenceMatch` (`:77-119`) | [`find_exact_model_reference_match`] |
//! | `tryMatchModel` (`:125-155`) | [`try_match_model`] |
//! | `buildFallbackModel` (`:164-178`) | [`build_fallback_model`] |
//! | `parseModelPattern` (`:193-246`) | [`parse_model_pattern`] |
//! | `resolveModelScopeWithDiagnostics` (`:270-332`) | [`resolve_model_scope_with_diagnostics`] |
//! | `resolveModelScope` (`:334-340`) | [`resolve_model_scope`] |
//! | `resolveCliModel` (`:364-535`) | [`resolve_cli_model`] |
//! | `findInitialModel` (`:551-631`) | [`find_initial_model`] |
//! | `restoreModelFromSession` (`:636-705`) | [`restore_model_from_session`] |
//! | `stripJsonComments` (`utils/json.ts:2-6`) | [`strip_json_comments`] |
//! | `ModelConfig.load` (`core/model-config.ts:235-274`) | [`ModelConfig::load`] |
//! | `formatValidationPath` (`model-config.ts:206-217`) | [`ValidationError::format_path`] |
//! | `mergeCompat` (`core/provider-composer.ts:78-98`) | [`merge_compat`] |
//! | `applyModelOverride` (`:100-122`) | [`apply_model_override`] |
//! | `modelFromJson` (`:124-159`) | [`model_from_json`] |
//! | `applyModelsJson` (`:161-199`) | [`apply_models_json`] |
//! | `composeModelProvider` (`:412-499`) | [`compose_model_provider`] |
//! | `resolveConfiguredModelHeaders` (`:501-512`) | [`resolve_configured_model_headers`] |
//! | `configuredRequestAuthStatus` (`:534-548`) | [`configured_request_auth_status`] |
//! | `ModelRuntime.create` + snapshot accessors (`core/model-runtime.ts:92-587`) | [`ModelRuntime`] |
//! | `InMemoryCodingAgentModelsStore` (`core/models-store.ts:8-22`) | [`InMemoryCodingAgentModelsStore`] |
//! | `FileModelsStore` (`core/models-store.ts:25-57`) | [`FileModelsStore`] |
//! | `listModels` + `formatTokenCount` (`cli/list-models.ts:14-111`) | [`list_models`], [`format_token_count`] |
//! | `fuzzyMatch`/`fuzzyFilter` (`packages/tui/src/fuzzy.ts:12-137`) | [`fuzzy_match`], [`fuzzy_filter`] |
//! | `getProviderLoginHelp` (`core/auth-guidance.ts:6-16`) | [`format_no_models_available_message`] |
//!
//! # Composition — what this module does *not* re-implement
//!
//! - **[`pirust_ai::types::Model`]** is *the* model type. It was byte-verified against 1062
//!   real Pi models; nothing here declares a second one. `ThinkingLevelMap`, `ModelCost`,
//!   `ModelCostTier` and `Modality` likewise come from `pirust-ai`, which is why
//!   `models.json`'s `cost` block deserializes straight into the struct the composed model
//!   carries.
//! - **[`crate::config::ConfigEnv`]** supplies `getModelsPath()` and the `normalizePath` that
//!   [`ModelConfig::load`] applies to its argument (`model-config.ts:237`). No path logic is
//!   re-derived here.
//! - **[`crate::auth`]** supplies [`ProcessEnv`], `resolve_config_value` (the `$VAR` / `!cmd`
//!   template resolver behind `models.json`'s `apiKey`), `is_command_config_value`, and
//!   `FileAuthStorageBackend` — which [`FileModelsStore`] reuses verbatim, because
//!   `models-store.json` is the same locked, `0600`, `JSON.stringify(x, null, 2)` file shape
//!   as `auth.json` (Pi builds it from the same `FileAuthStorageBackend`,
//!   `models-store.ts:29`).
//! - **`crate::args::is_valid_thinking_level`** is the accepted-suffix predicate
//!   (`cli/args.ts:59-61`); [`thinking_level_from_str`] only adds the value mapping, and a
//!   unit test pins the two against each other so the sets cannot drift.
//!
//! # The catalog is an input, never a global
//!
//! Pi imports `@earendil-works/pi-ai/providers/all` — a *generated* 36-provider /
//! 1062-model table — straight into `ModelRuntime.create` (`model-runtime.ts:32,141-145`).
//! Here it is a parameter: [`ModelCatalog`], built by the caller. feat-008 owns the
//! generator; until it lands, `main.rs` decides what to pass. Consequences worth knowing:
//!
//! - the golden suite feeds it the fixture's own `builtinCatalogFingerprint` record, so the
//!   18 `ModelRuntime.create` rows are replayed against *Pi's real anthropic provider*
//!   rather than a hand-written stand-in;
//! - the 35 other providers are modelled as [`ProviderDescriptor`]s with an empty model list,
//!   so `totals.providers` is exact while `totals.models` can only be checked as a *delta*
//!   from the fingerprint baseline (see `tests/models_golden.rs`);
//! - nothing in this module reads `std::env` except through [`ProcessEnv`], which the caller
//!   snapshots.
//!
//! # Deliberate divergences (all fixture-verified)
//!
//! - **`localeCompare`.** Pi's alias tie-break is `sort((a, b) => b.id.localeCompare(a.id))`
//!   (`model-resolver.ts:148,152`) and `--list-models` sorts by `provider.localeCompare` then
//!   `id.localeCompare` (`list-models.ts:55-57`) — ICU root-locale collation, not byte order.
//!   [`locale_compare`] approximates it; its limits are documented there. Spec §9.3 suggested
//!   plain `str::cmp`; that is *wrong* for the real catalog (`MiniMax-M2.7` and `kimi-k2.6`
//!   inverta) even though it happens to pass all 158 records, so the approximation is used.
//! - **V8 `JSON.parse` messages.** Five records carry a literal V8 diagnostic. Rust cannot
//!   reproduce that text; [`ModelConfig::load`] emits `serde_json`'s message inside the same
//!   `` `Failed to parse models.json: {msg}\n\nFile: {path}` `` envelope, and the golden test
//!   asserts the envelope plus the structural outcome. Only three of the five are marked
//!   `v8Dependent` in the fixture — see [`ModelConfig::load`].
//! - **TypeBox validation messages.** Four are oracle-verified; the rest of the table is
//!   best-effort and labelled as such on [`ValidationError`].
//! - **Fractional `contextWindow`/`maxTokens`.** TypeBox accepts any `number`;
//!   [`pirust_ai::types::Model`] stores `u64`. A fractional value truncates here where Pi
//!   would keep `1.5`. Unreachable from any real catalog, unexercised by the fixture.
//!
//! # Not ported
//!
//! - **`ModelRegistry`** (`core/model-registry.ts`) — the synchronous facade exposed to
//!   *extensions*, which feat-005 does not have. Every method is a one-line delegation to
//!   [`ModelRuntime`]; it belongs with the extension host (feat-007), not a stub now.
//! - **Extension providers** (`registerProvider` / `registerNativeProvider` /
//!   `applyExtension`, `provider-composer.ts:201-228`, `model-runtime.ts:533-586`).
//!   [`compose_model_provider`] therefore composes exactly two layers — builtin and
//!   `models.json` — and every `extension?.x ?? config?.x` in Pi collapses to `config?.x`.
//!   Each such site says so.
//! - **`radius` oauth providers** (`model-runtime.ts:168-183`) and **`withRemoteCatalog`**
//!   (network catalog refresh). With `PIRUST_OFFLINE` set — which is how the fixture was
//!   captured — `allowModelNetwork` is false and no refresh is attempted; the
//!   `oauth: "radius"` guard in [`apply_models_json`] survives so its error string does, but
//!   no radius provider can be constructed.
//! - **`stream` / `streamSimple` / `login` / `logout`.** The composed provider's `stream` is
//!   where an unknown `api` finally fails, with
//!   `` `No API provider registered for api: {api}` `` (`provider-composer.ts:462`) —
//!   [`api_provider_error`] returns exactly that string, so the *load-time* tolerance of an
//!   unknown `api` is testable without a transport.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap};

use pirust_agent_core::types::ThinkingLevel;
use pirust_ai::types::{
    Api, Modality, Model, ModelCost, ModelCostRates, ModelCostTier, ProviderId, ThinkingLevelMap,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value};

use crate::auth::{AuthStorageBackend, LockResult, ProcessEnv};

// =============================================================================
// Constants (`core/defaults.ts:3`, `core/model-resolver.ts:14-51`)
// =============================================================================

/// `DEFAULT_THINKING_LEVEL` (`core/defaults.ts:3`) — `"medium"`.
///
/// [`find_initial_model`] returns this in every branch except the scoped/settings ones; the
/// fixture's `constants` record pins the literal.
pub const DEFAULT_THINKING_LEVEL: ThinkingLevel = ThinkingLevel::Medium;

/// `defaultModelPerProvider` (`core/model-resolver.ts:14-51`).
///
/// **The order is load-bearing.** [`find_initial_model`]'s step 4 scans
/// `Object.keys(defaultModelPerProvider)` (`:617`) and takes the *first* pair present in
/// `getAvailable()`, so this is the key order of the TS object literal — captured verbatim as
/// the fixture's `constants.defaultModelPerProviderKeyOrder`, **not** alphabetised and
/// **not** the order of the available list. [`build_fallback_model`] also reads it, to choose
/// which of a provider's models to clone.
///
/// Spec §9.4 suggests narrowing this to the single `anthropic` entry; the fixture keeps all
/// 36, and `step4:scans-defaultModelPerProvider-IN-KEY-ORDER` only means something with the
/// full table, so the full table is kept. Adding a provider stays data-only.
pub const DEFAULT_MODEL_PER_PROVIDER: [(&str, &str); 40] = [
    ("amazon-bedrock", "us.anthropic.claude-opus-4-6-v1"),
    ("ant-ling", "Ring-2.6-1T"),
    ("anthropic", "claude-opus-4-8"),
    ("openai", "gpt-5.5"),
    ("azure-openai-responses", "gpt-5.4"),
    ("openai-codex", "gpt-5.5"),
    ("radius", "auto"),
    ("nvidia", "nvidia/nemotron-3-super-120b-a12b"),
    ("deepseek", "deepseek-v4-pro"),
    ("google", "gemini-3.1-pro-preview"),
    ("google-vertex", "gemini-3.1-pro-preview"),
    ("github-copilot", "gpt-5.4"),
    ("openrouter", "moonshotai/kimi-k2.6"),
    ("vercel-ai-gateway", "zai/glm-5.1"),
    ("xai", "grok-4.6"),
    ("groq", "openai/gpt-oss-120b"),
    ("cerebras", "gpt-oss-120b"),
    ("zai", "glm-5.3"),
    ("zai-coding-cn", "glm-5.3"),
    ("mistral", "devstral-medium-latest"),
    ("minimax", "MiniMax-M2.7"),
    ("minimax-cn", "MiniMax-M2.7"),
    ("moonshotai", "kimi-k2.6"),
    ("moonshotai-cn", "kimi-k2.6"),
    ("huggingface", "moonshotai/Kimi-K2.6"),
    ("fireworks", "accounts/fireworks/models/kimi-k2p6"),
    ("together", "moonshotai/Kimi-K2.6"),
    ("baseten", "zai-org/GLM-5.2"),
    ("opencode", "kimi-k2.6"),
    ("opencode-go", "kimi-k2.6"),
    ("kimi-coding", "kimi-for-coding"),
    ("cloudflare-workers-ai", "@cf/moonshotai/kimi-k2.6"),
    (
        "cloudflare-ai-gateway",
        "workers-ai/@cf/moonshotai/kimi-k2.6",
    ),
    ("qwen-token-plan", "qwen3.7-max"),
    ("qwen-token-plan-cn", "qwen3.7-max"),
    ("qwen-token-plan-individual", "qwen3.8-max"),
    ("xiaomi", "mimo-v2.5-pro"),
    ("xiaomi-token-plan-cn", "mimo-v2.5-pro"),
    ("xiaomi-token-plan-ams", "mimo-v2.5-pro"),
    ("xiaomi-token-plan-sgp", "mimo-v2.5-pro"),
];

/// `defaultModelPerProvider[provider]` (`model-resolver.ts:168`), by linear scan.
///
/// 36 entries: a scan is cheaper than a map and keeps [`DEFAULT_MODEL_PER_PROVIDER`] the one
/// source of both the lookup and the iteration order.
pub fn default_model_for_provider(provider: &str) -> Option<&'static str> {
    DEFAULT_MODEL_PER_PROVIDER
        .iter()
        .find(|(id, _)| *id == provider)
        .map(|(_, model)| *model)
}

/// Leaf of the dynamic-catalog store — `join(dirname(modelsPath), "models-store.json")`
/// (`model-runtime.ts:139`), or `join(getAgentDir(), …)` (`models-store.ts:28`).
pub const MODELS_STORE_FILE_NAME: &str = "models-store.json";

/// `` `No API provider registered for api: ${model.api}` `` (`provider-composer.ts:462`).
///
/// The **only** place an unknown `api` value is ever rejected. Load-time composition accepts
/// it (fixture `unknown-api-value-COMPOSES-FINE`: no composition error, the model *is*
/// listed and counted as available), and the failure surfaces at stream time. A port that
/// validated `api` at load would diverge on that record, which is why this string lives here
/// rather than in a validator.
pub fn api_provider_error(api: &str) -> String {
    format!("No API provider registered for api: {api}")
}

// =============================================================================
// Thinking-level suffixes (`cli/args.ts:57-61`)
// =============================================================================

/// The value behind `crate::args::is_valid_thinking_level` (`cli/args.ts:59-61`).
///
/// `args` owns the accepted set (and the `--thinking` diagnostics); this only maps an
/// already-accepted string to its [`ThinkingLevel`]. `thinking_levels_agree_with_args`
/// asserts the two never disagree, so there is no second list to keep in sync.
///
/// Case-**sensitive**, like Pi: `":HIGH"` is not a thinking suffix, it is part of a model id.
pub fn thinking_level_from_str(level: &str) -> Option<ThinkingLevel> {
    let parsed = match level {
        "off" => ThinkingLevel::Off,
        "minimal" => ThinkingLevel::Minimal,
        "low" => ThinkingLevel::Low,
        "medium" => ThinkingLevel::Medium,
        "high" => ThinkingLevel::High,
        "xhigh" => ThinkingLevel::Xhigh,
        "max" => ThinkingLevel::Max,
        _ => return None,
    };
    debug_assert!(crate::args::is_valid_thinking_level(level));
    Some(parsed)
}

/// The wire spelling of a [`ThinkingLevel`] — the inverse of [`thinking_level_from_str`].
pub fn thinking_level_as_str(level: ThinkingLevel) -> &'static str {
    match level {
        ThinkingLevel::Off => "off",
        ThinkingLevel::Minimal => "minimal",
        ThinkingLevel::Low => "low",
        ThinkingLevel::Medium => "medium",
        ThinkingLevel::High => "high",
        ThinkingLevel::Xhigh => "xhigh",
        ThinkingLevel::Max => "max",
    }
}

// =============================================================================
// Collation — `String.prototype.localeCompare`
// =============================================================================

/// `a.localeCompare(b)` in Node: **ICU root-locale collation**, not byte order.
///
/// Two call sites depend on it: the alias tie-break in [`try_match_model`]
/// (`model-resolver.ts:148,152`, descending) and `--list-models`' row sort
/// (`list-models.ts:55-57`, ascending). Unlike `pirust_tools::ls::sort_entries` — which
/// solves the same problem for `ls`, and whose approach and ASCII weight table this borrows —
/// Pi does **not** lowercase the operands here, so a case level is needed.
///
/// # What this reproduces
///
/// Three levels, in order:
///
/// 1. **Primary**: each ASCII character maps to a weight from `ASCII_PRIMARY_ORDER`, the
///    two cases of a letter folded onto one weight. That table is not a guess — it is
///    `[...chars].sort((a, b) => a.toLowerCase().localeCompare(b.toLowerCase()))` over
///    `U+0020..U+007E`, run on the *host* ICU (the one Pi runs on), lifted verbatim from
///    `pirust_tools::ls` (`ls.rs:355-356`) where it is private. Punctuation sorts below
///    digits, which sort below letters — nothing like codepoint order, where `_` and `:` both
///    sit above every digit.
/// 2. **Case**: on an otherwise-equal primary run, lowercase before uppercase, the direction
///    ICU's tertiary level takes (`"a".localeCompare("A") === -1`).
/// 3. **Tiebreak**: codepoint order.
///
/// So it is faithful for strings of printable ASCII in either case — which is every model id,
/// model name and provider id in the captured 1062-model catalog, and every value in all 158
/// fixture records.
///
/// # What it does not reproduce
///
/// - **Any non-ASCII character** is ordered by codepoint *after* every ASCII weight. `ls.rs`
///   does better: it NFD-decomposes and drops combining marks so `éclair` lands in the `e`
///   bucket. That needs `unicode-normalization`, which this crate does not depend on, and
///   adding a dependency is not this port's call — so accented names sort after `z` here.
///   `ls.rs`'s own residual gaps (`ø`/`æ`/`ß`, secondary accent order, non-Latin scripts,
///   primary-ignorables beyond the C0 controls) apply here too, on top of that.
/// - **Contractions and expansions** — irrelevant at the root locale, noted for completeness.
/// - **Numeric collation**, off in both, matching `localeCompare`'s default.
///
/// Closing the gap needs `icu_collator` (ICU4X) at root locale and default strength, at which
/// point this function and `ls.rs`'s should both be deleted in its favour, and the duplicated
/// weight table with them.
pub fn locale_compare(a: &str, b: &str) -> Ordering {
    primary_key(a)
        .cmp(&primary_key(b))
        .then_with(|| case_key(a).cmp(&case_key(b)))
        .then_with(|| a.cmp(b))
}

/// ICU root-locale primary order of the printable ASCII range, case-folded.
///
/// Lifted verbatim from `pirust_tools::ls` (`ls.rs:355-356`), which derived it from the host
/// ICU. Duplicated only because it is private there and `pirust-tools` is not this task's to
/// edit; it belongs in one shared place the next time either module is touched.
const ASCII_PRIMARY_ORDER: &[u8] =
    b" _-,;:!?.'\"()[]{}@*/\\&#%`^+<=>|~$0123456789abcdefghijklmnopqrstuvwxyz";

/// [`ASCII_PRIMARY_ORDER`] inverted into a byte-indexed lookup. `0` = "no primary weight":
/// a C0 control or DEL, which ICU treats as primary-ignorable.
static ASCII_PRIMARY_WEIGHT: [u8; 128] = build_ascii_primary_weight();

const fn build_ascii_primary_weight() -> [u8; 128] {
    let mut table = [0u8; 128];
    let mut i = 0;
    while i < ASCII_PRIMARY_ORDER.len() {
        let byte = ASCII_PRIMARY_ORDER[i];
        // +1 so weight 0 stays reserved for the primary-ignorables.
        table[byte as usize] = (i + 1) as u8;
        // Fold the uppercase half of each letter onto the same weight.
        if byte.is_ascii_lowercase() {
            table[byte.to_ascii_uppercase() as usize] = (i + 1) as u8;
        }
        i += 1;
    }
    table
}

/// Primary weights of `s`, skipping primary-ignorables.
fn primary_key(s: &str) -> Vec<u32> {
    s.chars().filter_map(primary_weight).collect()
}

/// The primary weight of one character, or `None` when it is primary-ignorable.
fn primary_weight(c: char) -> Option<u32> {
    let code = c as u32;
    if code < 128 {
        let weight = ASCII_PRIMARY_WEIGHT[code as usize];
        return (weight != 0).then_some(u32::from(weight));
    }
    // Past ASCII: ordered by codepoint after every ASCII weight. Documented gap.
    Some(ASCII_PRIMARY_ORDER.len() as u32 + 1 + code)
}

/// Case weights of `s`: `1` for an ASCII uppercase letter, `0` for anything else. Compared
/// only when the primary keys tie, so it never outranks a later letter difference — as ICU's
/// level ordering requires.
fn case_key(s: &str) -> Vec<u8> {
    s.chars()
        .filter(|c| primary_weight(*c).is_some())
        .map(|c| u8::from(c.is_ascii_uppercase()))
        .collect()
}

// =============================================================================
// The runtime seam — what the resolvers actually need from a `ModelRuntime`
// =============================================================================

/// The five `ModelRuntime` reads the resolvers perform, as one trait.
///
/// Pi passes the whole `ModelRuntime` into `resolveCliModel`, `findInitialModel`,
/// `resolveModelScopeWithDiagnostics`, `restoreModelFromSession` and `listModels`, but
/// touches only `getError`, `getModels`, `getAvailable`, `getModel` and `hasConfiguredAuth`
/// (`model-resolver.ts:378,602-613`, `list-models.ts:30-35`). Narrowing to those makes the
/// resolvers replayable against the fixture's synthetic catalog — [`StaticModelSource`] —
/// without constructing a runtime, which is how 111 of the 158 records were captured.
///
/// `getAvailable` is `async` in Pi purely because a *network* catalog refresh may be pending
/// (`model-runtime.ts:307-322`). With no network in feat-005 there is nothing to await, so it
/// is synchronous here; feat-008 reintroduces the await with the refresh.
pub trait ModelSource {
    /// `getError()` (`model-runtime.ts:328-337`) — config error, then composition errors, then
    /// the availability error, joined with `"\n\n"`.
    fn get_error(&self) -> Option<String>;
    /// `getModels()` (`:295-297`) — **every** model, regardless of auth. `resolveCliModel`
    /// deliberately uses this one "so `--api-key` can be used for first-time setup"
    /// (`model-resolver.ts:376-378`).
    fn get_models(&self) -> &[Model];
    /// `getAvailable()` (`:307-322`) — models whose provider has configured auth.
    fn get_available(&self) -> &[Model];
    /// `getModel(providerId, modelId)` (`:299-301`) — exact, case-sensitive.
    fn get_model(&self, provider: &str, model_id: &str) -> Option<&Model>;
    /// `hasConfiguredAuth(providerId)` (`:364-366`) — the provider is in the availability
    /// snapshot's `configuredProviders`.
    fn has_configured_auth(&self, provider: &str) -> bool;
}

/// A [`ModelSource`] over literal lists — the shape the fixture's pure-function records
/// describe (`catalogSource: "synthetic"` plus `configuredProviders`, and for
/// `findInitialModel` an `availableOverride`).
///
/// Not `#[cfg(test)]`: `tests/models_golden.rs` is a separate crate and could not see it
/// otherwise. It is also the seam `main.rs` can use to inject a fixed catalog before feat-008
/// lands.
#[derive(Debug, Clone, Default)]
pub struct StaticModelSource {
    models: Vec<Model>,
    available: Vec<Model>,
    configured_providers: BTreeSet<String>,
    error: Option<String>,
}

impl StaticModelSource {
    /// Every model, with `available` derived as Pi derives it —
    /// `all.filter(m => configuredProviders.has(m.provider))` (`model-runtime.ts:230`).
    pub fn new(models: Vec<Model>, configured_providers: BTreeSet<String>) -> Self {
        let available = models
            .iter()
            .filter(|m| configured_providers.contains(&m.provider.0))
            .cloned()
            .collect();
        Self {
            models,
            available,
            configured_providers,
            error: None,
        }
    }

    /// Replace the derived availability list — the fixture's `availableOverride`, which
    /// stands in for a snapshot the capture harness pinned directly.
    #[must_use]
    pub fn with_available(mut self, available: Vec<Model>) -> Self {
        self.available = available;
        self
    }

    /// Set what `getError()` reports.
    #[must_use]
    pub fn with_error(mut self, error: Option<String>) -> Self {
        self.error = error;
        self
    }
}

impl ModelSource for StaticModelSource {
    fn get_error(&self) -> Option<String> {
        self.error.clone()
    }

    fn get_models(&self) -> &[Model] {
        &self.models
    }

    fn get_available(&self) -> &[Model] {
        &self.available
    }

    fn get_model(&self, provider: &str, model_id: &str) -> Option<&Model> {
        self.models
            .iter()
            .find(|m| m.provider.0 == provider && m.id == model_id)
    }

    fn has_configured_auth(&self, provider: &str) -> bool {
        self.configured_providers.contains(provider)
    }
}

/// `modelsAreEqual(a, b)` (`packages/ai/src/models.ts:699-705`) — **provider and id only**.
///
/// This is what makes scope de-duplication drop a second thinking level for the same model
/// (`--models "x:high,x:low"` keeps `high`); see [`resolve_model_scope_with_diagnostics`].
pub fn models_are_equal(a: &Model, b: &Model) -> bool {
    a.id == b.id && a.provider == b.provider
}

// =============================================================================
// model-resolver.ts:63-155 — exact reference matching and fuzzy fallback
// =============================================================================

/// `isAlias(id)` (`model-resolver.ts:63-70`) — `` id.endsWith("-latest") || !/-\d{8}$/.test(id) ``.
///
/// So `claude-haiku-4-5-latest` is an alias, `claude-sonnet-4-5` is an alias (no date), and
/// `claude-sonnet-4-5-20250929` is not. A `-latest` suffix wins even with a date in front.
pub fn is_alias(id: &str) -> bool {
    if id.ends_with("-latest") {
        return true;
    }
    !ends_with_date_suffix(id)
}

/// `` /-\d{8}$/ `` — a hyphen then exactly eight ASCII digits at the end.
fn ends_with_date_suffix(id: &str) -> bool {
    let bytes = id.as_bytes();
    if bytes.len() < 9 {
        return false;
    }
    let tail = &bytes[bytes.len() - 9..];
    tail[0] == b'-' && tail[1..].iter().all(u8::is_ascii_digit)
}

/// `findExactModelReferenceMatch(modelReference, availableModels)`
/// (`model-resolver.ts:77-119`) — three passes, each rejecting ambiguity outright.
///
/// 1. **Canonical.** `` `${provider}/${id}` `` compared case-insensitively to the trimmed
///    reference. Exactly one hit wins; **two or more return `None`**, not the first. This
///    pass runs *before* any provider/id split, which is why `openai/gpt-4o` resolves to
///    openrouter's literal-slash id, and `openrouter/openai/gpt-4o` resolves at all.
/// 2. **Split on the first `/`.** Both halves are trimmed (`anthropic /claude-sonnet-4-5`
///    works), and the pass is skipped unless *both* are non-empty — so `/id` and `provider/`
///    fall through it. Again one hit wins, several return `None`.
/// 3. **Bare id**, case-insensitive. One hit wins; an id shared by two providers
///    (`shared-model-id`) returns `None`, which is what sends [`try_match_model`] into its
///    substring fallback.
///
/// The guard is `if (!trimmedReference) return undefined` (`:82-84`): an empty or
/// whitespace-only reference matches nothing *here*. That matters — the substring fallback in
/// [`try_match_model`] has no such guard and `id.includes("")` is true for every model.
pub fn find_exact_model_reference_match<'a>(
    model_reference: &str,
    available_models: &'a [Model],
) -> Option<&'a Model> {
    let trimmed_reference = js_trim(model_reference);
    if trimmed_reference.is_empty() {
        return None;
    }

    let normalized_reference = trimmed_reference.to_lowercase();

    let canonical_matches: Vec<&Model> = available_models
        .iter()
        .filter(|model| {
            format!("{}/{}", model.provider.0, model.id).to_lowercase() == normalized_reference
        })
        .collect();
    if canonical_matches.len() == 1 {
        return Some(canonical_matches[0]);
    }
    if canonical_matches.len() > 1 {
        return None;
    }

    if let Some(slash_index) = trimmed_reference.find('/') {
        let provider = js_trim(&trimmed_reference[..slash_index]);
        let model_id = js_trim(&trimmed_reference[slash_index + 1..]);
        if !provider.is_empty() && !model_id.is_empty() {
            let provider_lower = provider.to_lowercase();
            let model_id_lower = model_id.to_lowercase();
            let provider_matches: Vec<&Model> = available_models
                .iter()
                .filter(|model| {
                    model.provider.0.to_lowercase() == provider_lower
                        && model.id.to_lowercase() == model_id_lower
                })
                .collect();
            if provider_matches.len() == 1 {
                return Some(provider_matches[0]);
            }
            if provider_matches.len() > 1 {
                return None;
            }
        }
    }

    let id_matches: Vec<&Model> = available_models
        .iter()
        .filter(|model| model.id.to_lowercase() == normalized_reference)
        .collect();
    if id_matches.len() == 1 {
        Some(id_matches[0])
    } else {
        None
    }
}

/// `String.prototype.trim` — JS trims *WhiteSpace* ∪ *LineTerminator*, which is not
/// `char::is_whitespace`: JS also trims `U+FEFF`, and does *not* trim `U+0085`.
fn js_trim(s: &str) -> &str {
    s.trim_matches(is_js_whitespace)
}

/// JS `WhiteSpace ∪ LineTerminator` (ECMA-262 §12.2): tab, LF, VT, FF, CR, space, NBSP,
/// ZWNBSP/BOM, LS, PS, plus every Unicode `Zs`.
fn is_js_whitespace(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n'
            | '\u{b}'
            | '\u{c}'
            | '\r'
            | ' '
            | '\u{a0}'
            | '\u{feff}'
            | '\u{2028}'
            | '\u{2029}'
    ) || matches!(
        c,
        '\u{1680}' | '\u{2000}'..='\u{200a}' | '\u{202f}' | '\u{205f}' | '\u{3000}'
    )
}

/// `tryMatchModel(modelPattern, availableModels)` (`model-resolver.ts:125-155`).
///
/// [`find_exact_model_reference_match`] first; failing that a **substring** pass over the
/// lowercased `id` **or** `name` (`:132-136`) — note the pattern is *not* trimmed here, only
/// lowercased. Then:
///
/// - aliases ([`is_alias`]) beat dated versions, however many of each there are;
/// - within the winning group, `sort((a, b) => b.id.localeCompare(a.id))` and take `[0]`, the
///   **highest**-collating id;
/// - the sort is stable in both runtimes (`Array#sort` since ES2019, `slice::sort_by`
///   always), so two models with *equal* ids keep catalog order — the only thing deciding
///   `shared-model-id` between groq and cerebras.
///
/// **Quirk with a fixture record of its own:** an empty pattern reaches this function (the
/// exact pass rejected it) and `"x".includes("")` is true, so *every* model matches and the
/// alias tie-break picks one. `parseModelPattern("")` therefore returns a model, and so does
/// `":high"`.
pub fn try_match_model<'a>(
    model_pattern: &str,
    available_models: &'a [Model],
) -> Option<&'a Model> {
    if let Some(exact_match) = find_exact_model_reference_match(model_pattern, available_models) {
        return Some(exact_match);
    }

    let needle = model_pattern.to_lowercase();
    let matches: Vec<&Model> = available_models
        .iter()
        .filter(|m| {
            // `m.name?.toLowerCase().includes(...)` (`:135`) — `name` is non-optional in the
            // Rust `Model` and every captured model has one, so the optional chain cannot
            // change the outcome.
            m.id.to_lowercase().contains(&needle) || m.name.to_lowercase().contains(&needle)
        })
        .collect();

    if matches.is_empty() {
        return None;
    }

    let aliases: Vec<&Model> = matches
        .iter()
        .copied()
        .filter(|m| is_alias(&m.id))
        .collect();
    let mut group = if aliases.is_empty() {
        matches
            .iter()
            .copied()
            .filter(|m| !is_alias(&m.id))
            .collect::<Vec<&Model>>()
    } else {
        aliases
    };

    // `b.id.localeCompare(a.id)` — descending.
    group.sort_by(|a, b| locale_compare(&b.id, &a.id));
    group.first().copied()
}

// =============================================================================
// model-resolver.ts:157-246 — parseModelPattern
// =============================================================================

/// `ParsedModelResult` (`model-resolver.ts:157-162`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ParsedModelResult {
    /// The matched model, if any.
    pub model: Option<Model>,
    /// The level from an explicit `:<level>` suffix — `None` when absent *or* suppressed by
    /// an inner warning.
    pub thinking_level: Option<ThinkingLevel>,
    /// `` `Invalid thinking level "{suffix}" in pattern "{pattern}". Using default instead.` ``
    pub warning: Option<String>,
}

/// `options` of `parseModelPattern` (`model-resolver.ts:196`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseModelPatternOptions {
    /// `allowInvalidThinkingLevelFallback` — **defaults to `true`** (`:228`).
    ///
    /// `resolveModelScope` takes the default (warn, then retry the prefix);
    /// [`resolve_cli_model`] passes `false` (`:445`), which makes an invalid suffix fail
    /// outright "to avoid accidentally resolving to a different model" (`:230-231`) — no model
    /// *and no warning*.
    pub allow_invalid_thinking_level_fallback: bool,
}

impl Default for ParseModelPatternOptions {
    fn default() -> Self {
        Self::LENIENT
    }
}

impl ParseModelPatternOptions {
    /// What [`resolve_cli_model`] passes (`model-resolver.ts:444-446`).
    pub const STRICT: Self = Self {
        allow_invalid_thinking_level_fallback: false,
    };
    /// What `resolveModelScope` passes — i.e. nothing (`:314`).
    pub const LENIENT: Self = Self {
        allow_invalid_thinking_level_fallback: true,
    };
}

/// `parseModelPattern(pattern, availableModels, options)` (`model-resolver.ts:193-246`).
///
/// The full pattern is tried as a model **first**, so an id that genuinely contains a colon
/// (`openai/gpt-4o:extended`) resolves with no thinking level at all. Only on a miss is the
/// **last** colon split off and the prefix retried, one suffix per recursion frame.
///
/// Two consequences the fixture names explicitly, and which are easy to get backwards:
///
/// - **The last suffix wins.** `"claude-sonnet-4-5:high:low"` → `low`. The recursion strips
///   `:low` first; the inner frame resolves `…:high` and returns `high`; the outer frame then
///   *overwrites* it with its own `low` (`:219-223`). A port where the first suffix won would
///   answer `high`.
/// - **An inner warning suppresses the outer level.** `"openai/gpt-4o:bogus:high"` → model
///   resolved, `thinkingLevel: None`, and the inner `bogus` warning propagated, because the
///   outer frame writes `result.warning ? undefined : suffix` (`:221`).
///
/// A prefix that resolves to nothing returns the inner result *unchanged* (`:225,244`), so
/// `"nope:high"` yields no model, no level and no warning.
pub fn parse_model_pattern(
    pattern: &str,
    available_models: &[Model],
    options: ParseModelPatternOptions,
) -> ParsedModelResult {
    if let Some(exact_match) = try_match_model(pattern, available_models) {
        return ParsedModelResult {
            model: Some(exact_match.clone()),
            thinking_level: None,
            warning: None,
        };
    }

    let Some(last_colon_index) = pattern.rfind(':') else {
        return ParsedModelResult::default();
    };

    let prefix = &pattern[..last_colon_index];
    let suffix = &pattern[last_colon_index + 1..];

    if let Some(level) = thinking_level_from_str(suffix) {
        let result = parse_model_pattern(prefix, available_models, options);
        if result.model.is_some() {
            return ParsedModelResult {
                thinking_level: if result.warning.is_some() {
                    None
                } else {
                    Some(level)
                },
                ..result
            };
        }
        return result;
    }

    if !options.allow_invalid_thinking_level_fallback {
        return ParsedModelResult::default();
    }

    let result = parse_model_pattern(prefix, available_models, options);
    if result.model.is_some() {
        return ParsedModelResult {
            model: result.model,
            thinking_level: None,
            warning: Some(format!(
                "Invalid thinking level \"{suffix}\" in pattern \"{pattern}\". Using default instead."
            )),
        };
    }
    result
}

// =============================================================================
// model-resolver.ts:164-178 — buildFallbackModel
// =============================================================================

/// `buildFallbackModel(provider, modelId, availableModels)` (`model-resolver.ts:164-178`).
///
/// Clone one of the provider's existing models and rename it, so `--provider anthropic
/// --model my-proxy-model` yields a usable model for an id the catalog has never heard of.
/// This is what makes the local-proxy scenario work without listing models in `models.json`
/// (spec §9.4 step 10).
///
/// Which model is cloned: the provider's [`DEFAULT_MODEL_PER_PROVIDER`] entry if that id is
/// present, else `providerModels[0]` — i.e. the first in catalog order. Only `id` and `name`
/// are overwritten (`:174-176`); `api`, `baseUrl`, `reasoning`, `input`, `cost`,
/// `contextWindow`, `maxTokens`, `thinkingLevelMap`, `headers` and `compat` are inherited
/// wholesale, which the fixture pins by expecting anthropic's `claude-opus-4-8` numbers
/// (`contextWindow: 1000000`, `maxTokens: 128000`) on the fallback.
///
/// `None` when the provider has no models at all.
pub fn build_fallback_model(
    provider: &str,
    model_id: &str,
    available_models: &[Model],
) -> Option<Model> {
    let provider_models: Vec<&Model> = available_models
        .iter()
        .filter(|m| m.provider.0 == provider)
        .collect();
    let first = *provider_models.first()?;

    let base_model = match default_model_for_provider(provider) {
        Some(default_id) => provider_models
            .iter()
            .copied()
            .find(|m| m.id == default_id)
            .unwrap_or(first),
        None => first,
    };

    Some(Model {
        id: model_id.to_string(),
        name: model_id.to_string(),
        ..base_model.clone()
    })
}

// =============================================================================
// model-resolver.ts:342-535 — resolveCliModel
// =============================================================================

/// `ResolveCliModelResult` (`model-resolver.ts:342-351`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ResolveCliModelResult {
    /// The resolved model. Always `None` when `error` is set.
    pub model: Option<Model>,
    /// A level *parsed* from `<pattern>:<level>`. Not applied here — the caller applies it.
    pub thinking_level: Option<ThinkingLevel>,
    /// Non-fatal note, e.g. the custom-model-id fallback.
    pub warning: Option<String>,
    /// Fatal, CLI-displayable message. `main.rs` prints it red and exits 1.
    pub error: Option<String>,
}

/// The `options` object of `resolveCliModel` (`model-resolver.ts:364-369`), minus the
/// runtime — which is a separate argument here so the whole thing stays `Copy`-cheap.
#[derive(Debug, Clone, Copy, Default)]
pub struct ResolveCliModelOptions<'a> {
    /// `--provider`.
    pub cli_provider: Option<&'a str>,
    /// `--model`.
    pub cli_model: Option<&'a str>,
    /// `--thinking`. Only consulted by step 10, to decide whether a trailing `:<level>` is
    /// split off the fallback id.
    pub cli_thinking: Option<ThinkingLevel>,
}

/// `resolveCliModel(options)` (`model-resolver.ts:364-535`) — the `--model`/`--provider`
/// chain, in Pi's exact branch order.
///
/// 1. No `--model` → everything `None`. **`--provider` alone does nothing here**; it is
///    [`find_initial_model`] that requires both.
/// 2. `getModels()` (all models, *not* just authenticated ones — see [`ModelSource`]); empty
///    → error `No models available. Check your installation or add models to models.json.`
///    This guard fires **before** `--provider` is validated.
/// 3. A lowercase→canonical provider map over all models; an unknown `--provider` → error
///    `` `Unknown provider "{p}". Use --list-models to see available providers/models.` ``
/// 4. With no `--provider`: if `--model` contains `/` and the part before the **first** `/`
///    is a known provider, adopt it and keep the rest as the pattern (`inferredProvider`).
/// 5. Still no provider → an exact case-insensitive match of the *whole* `--model` against
///    `id` or `provider/id` returns immediately.
/// 6. Both `--provider` and a resolved provider → strip a redundant `` `{provider}/` ``
///    prefix, case-insensitively.
/// 7. [`parse_model_pattern`] over the provider's models (or all), **strict** — so
///    `:bogus` is not stripped and does not warn.
/// 8. On a hit: when the provider was *inferred* and has no configured auth, prefer a
///    **single** exact raw-id match on a provider that does. (`xiaomi/mimo-v2.5-pro` →
///    commandcode's literally-named model, but only when exactly one authenticated raw match
///    exists and xiaomi itself is unauthenticated.)
/// 9. On a miss with an inferred provider: retry the exact match against all models, then
///    [`parse_model_pattern`] over all models — this is how `openai/gpt-4o:extended` lands on
///    openrouter.
/// 10. With any provider resolved: split a trailing *valid* `:<level>` off the pattern **only
///     when `--thinking` is unset**, then [`build_fallback_model`]. A requested level other
///     than `off` also forces `reasoning: true`. The warning is
///     `` `Model "{pattern}" not found for provider "{provider}". Using custom model id.` ``,
///     prefixed by the inner warning plus a space when there is one.
/// 11. Otherwise error `` `Model "{display}" not found. Use --list-models to see available
///     models.` `` with `display = provider ? `{provider}/{pattern}` : cliModel`.
pub fn resolve_cli_model(
    options: ResolveCliModelOptions<'_>,
    source: &dyn ModelSource,
) -> ResolveCliModelResult {
    let ResolveCliModelOptions {
        cli_provider,
        cli_model,
        cli_thinking,
    } = options;

    // 1. `if (!cliModel)` (`:372-374`) — JS truthiness, so `--model ""` is also a no-op.
    let Some(cli_model) = cli_model.filter(|m| !m.is_empty()) else {
        return ResolveCliModelResult::default();
    };

    let available_models = source.get_models();
    if available_models.is_empty() {
        return ResolveCliModelResult {
            error: Some(
                "No models available. Check your installation or add models to models.json."
                    .to_string(),
            ),
            ..Default::default()
        };
    }

    // 3. `providerMap: lowercase → canonical` (`:388-391`). Later models overwrite earlier
    // ones, as `Map.set` does; only case-variant duplicates could notice.
    let mut provider_map: HashMap<String, String> = HashMap::new();
    for m in available_models {
        provider_map.insert(m.provider.0.to_lowercase(), m.provider.0.clone());
    }

    let mut provider: Option<String> = cli_provider
        .filter(|p| !p.is_empty())
        .and_then(|p| provider_map.get(&p.to_lowercase()).cloned());
    if let Some(cli_provider) = cli_provider.filter(|p| !p.is_empty()) {
        if provider.is_none() {
            return ResolveCliModelResult {
                error: Some(format!(
                    "Unknown provider \"{cli_provider}\". Use --list-models to see available providers/models."
                )),
                ..Default::default()
            };
        }
    }

    let mut pattern = cli_model.to_string();
    let mut inferred_provider = false;

    // 4. `provider/model` inference (`:410-421`).
    if provider.is_none() {
        if let Some(slash_index) = cli_model.find('/') {
            let maybe_provider = &cli_model[..slash_index];
            if let Some(canonical) = provider_map.get(&maybe_provider.to_lowercase()) {
                provider = Some(canonical.clone());
                pattern = cli_model[slash_index + 1..].to_string();
                inferred_provider = true;
            }
        }
    }

    // 5. Exact match with no provider inference at all (`:459-497`). 0.84.2:
    // ambiguous bare ids are REJECTED with an error (not silently passed to the
    // substring fallback), with an authenticated-provider preference: a sole
    // authenticated match wins; 0 or 2+ authenticated matches produce the
    // ambiguity error.
    if provider.is_none() {
        let lower = cli_model.to_lowercase();
        let exact_matches: Vec<&Model> = available_models
            .iter()
            .filter(|m| {
                m.id.to_lowercase() == lower
                    || format!("{}/{}", m.provider.0, m.id).to_lowercase() == lower
            })
            .collect();
        if exact_matches.len() == 1 {
            return ResolveCliModelResult {
                model: Some(exact_matches[0].clone()),
                ..Default::default()
            };
        }
        if exact_matches.len() > 1 {
            let authenticated: Vec<&Model> = exact_matches
                .iter()
                .copied()
                .filter(|m| source.has_configured_auth(&m.provider.0))
                .collect();
            if authenticated.len() == 1 {
                return ResolveCliModelResult {
                    model: Some(authenticated[0].clone()),
                    ..Default::default()
                };
            }
            let mut matches: Vec<String> = exact_matches
                .iter()
                .map(|m| format!("{}/{}", m.provider.0, m.id))
                .collect();
            matches.sort_by(|a, b| locale_compare(a, b));
            let matches = matches.join(", ");
            let auth_hint = if authenticated.is_empty() {
                "No matching provider is authenticated."
            } else {
                "More than one matching provider is authenticated."
            };
            return ResolveCliModelResult {
                error: Some(format!(
                    "Model \"{cli_model}\" is ambiguous across providers: {matches}. {auth_hint} \
                     Use --provider or provider/model."
                )),
                ..Default::default()
            };
        }
    }

    // 6. Strip a redundant `{provider}/` prefix (`:435-441`).
    if let (Some(cli_provider), Some(resolved)) =
        (cli_provider.filter(|p| !p.is_empty()), provider.as_deref())
    {
        let _ = cli_provider;
        let prefix = format!("{resolved}/");
        if cli_model.to_lowercase().starts_with(&prefix.to_lowercase()) {
            pattern = cli_model[prefix.len()..].to_string();
        }
    }

    // 7. Strict parse over the candidate set (`:443-446`).
    let candidates: Vec<Model> = match provider.as_deref() {
        Some(p) => available_models
            .iter()
            .filter(|m| m.provider.0 == p)
            .cloned()
            .collect(),
        None => available_models.to_vec(),
    };
    let parsed = parse_model_pattern(&pattern, &candidates, ParseModelPatternOptions::STRICT);

    // 8. The authenticated-raw-id preference (`:448-471`).
    if let Some(model) = parsed.model.clone() {
        if inferred_provider {
            let cli_model_lower = cli_model.to_lowercase();
            let raw_exact_matches: Vec<&Model> = available_models
                .iter()
                .filter(|m| m.id.to_lowercase() == cli_model_lower && !models_are_equal(m, &model))
                .collect();
            if !raw_exact_matches.is_empty() && !source.has_configured_auth(&model.provider.0) {
                let authenticated: Vec<&Model> = raw_exact_matches
                    .into_iter()
                    .filter(|m| source.has_configured_auth(&m.provider.0))
                    .collect();
                if authenticated.len() == 1 {
                    return ResolveCliModelResult {
                        model: Some(authenticated[0].clone()),
                        ..Default::default()
                    };
                }
            }
        }
        return ResolveCliModelResult {
            model: Some(model),
            thinking_level: parsed.thinking_level,
            warning: parsed.warning,
            error: None,
        };
    }

    // 9. The inferred-provider fallback to the full raw input (`:477-497`).
    if inferred_provider {
        if let Some(exact) = find_exact_id_or_canonical(cli_model, available_models) {
            return ResolveCliModelResult {
                model: Some(exact.clone()),
                ..Default::default()
            };
        }
        let fallback = parse_model_pattern(
            cli_model,
            available_models,
            ParseModelPatternOptions::STRICT,
        );
        if fallback.model.is_some() {
            return ResolveCliModelResult {
                model: fallback.model,
                thinking_level: fallback.thinking_level,
                warning: fallback.warning,
                error: None,
            };
        }
    }

    // 10. The custom-model-id fallback (`:499-526`).
    if let Some(resolved_provider) = provider.as_deref() {
        let mut fallback_pattern = pattern.clone();
        let mut fallback_thinking: Option<ThinkingLevel> = None;
        if cli_thinking.is_none() {
            if let Some(last_colon) = pattern.rfind(':') {
                if let Some(level) = thinking_level_from_str(&pattern[last_colon + 1..]) {
                    fallback_pattern = pattern[..last_colon].to_string();
                    fallback_thinking = Some(level);
                }
            }
        }

        if let Some(fallback_model) =
            build_fallback_model(resolved_provider, &fallback_pattern, available_models)
        {
            let requested_thinking = cli_thinking.or(fallback_thinking);
            let model = match requested_thinking {
                Some(level) if level != ThinkingLevel::Off => Model {
                    reasoning: true,
                    ..fallback_model
                },
                _ => fallback_model,
            };
            let not_found = format!(
                "Model \"{fallback_pattern}\" not found for provider \"{resolved_provider}\". Using custom model id."
            );
            let fallback_warning = match parsed.warning.as_deref() {
                Some(warning) => format!("{warning} {not_found}"),
                None => not_found,
            };
            return ResolveCliModelResult {
                model: Some(model),
                thinking_level: fallback_thinking,
                warning: Some(fallback_warning),
                error: None,
            };
        }
    }

    // 11. `:528-534`.
    let display = match provider.as_deref() {
        Some(p) => format!("{p}/{pattern}"),
        None => cli_model.to_string(),
    };
    ResolveCliModelResult {
        model: None,
        thinking_level: None,
        warning: parsed.warning,
        error: Some(format!(
            "Model \"{display}\" not found. Use --list-models to see available models."
        )),
    }
}

/// `availableModels.find(m => m.id.toLowerCase() === lower || `${m.provider}/${m.id}`
/// .toLowerCase() === lower)` — the identical predicate at `model-resolver.ts:427-429` and
/// `:479-481`, hoisted so the two sites cannot drift.
///
/// Unlike [`find_exact_model_reference_match`] this is `find`, not `filter`: the **first**
/// match wins and ambiguity is not rejected.
fn find_exact_id_or_canonical<'a>(reference: &str, models: &'a [Model]) -> Option<&'a Model> {
    let lower = reference.to_lowercase();
    models.iter().find(|m| {
        m.id.to_lowercase() == lower || format!("{}/{}", m.provider.0, m.id).to_lowercase() == lower
    })
}

// =============================================================================
// model-resolver.ts:537-631 — findInitialModel
// =============================================================================

/// `InitialModelResult` (`model-resolver.ts:537-541`).
#[derive(Debug, Clone, PartialEq)]
pub struct InitialModelResult {
    /// The selected model, or `None` when nothing is available (step 5).
    pub model: Option<Model>,
    /// Never optional: [`DEFAULT_THINKING_LEVEL`] unless a scoped or settings level applies.
    pub thinking_level: ThinkingLevel,
    /// Always `None` from this function — only [`restore_model_from_session`] sets one. Kept
    /// because it is part of Pi's return shape and `sdk.ts` reads the field either way.
    pub fallback_message: Option<String>,
}

/// The `options` object of `findInitialModel` (`model-resolver.ts:551-560`).
///
/// `defaultProvider` / `defaultModelId` / `defaultThinkingLevel` are `settings.defaultProvider`
/// / `.defaultModel` / `.defaultThinkingLevel` in Pi. They are **parameters** here rather than
/// a `crate::settings` read: this module must not depend on the settings layer, and `main.rs`
/// does the wiring (Wave 5).
#[derive(Debug, Clone, Default)]
pub struct FindInitialModelOptions<'a> {
    /// `--provider`.
    pub cli_provider: Option<&'a str>,
    /// `--model`.
    pub cli_model: Option<&'a str>,
    /// Already-resolved `--models` scope.
    pub scoped_models: &'a [ScopedModel],
    /// `--continue` / `--resume`: suppresses step 2 so the session's own model can be restored.
    pub is_continuing: bool,
    /// `settings.defaultProvider`.
    pub default_provider: Option<&'a str>,
    /// `settings.defaultModel`.
    pub default_model_id: Option<&'a str>,
    /// `settings.defaultThinkingLevel`.
    pub default_thinking_level: Option<ThinkingLevel>,
}

/// `findInitialModel(options)` (`model-resolver.ts:551-631`) — five precedence steps.
///
/// 1. **`--provider` *and* `--model`** (both, `:576`) → [`resolve_cli_model`]. Its `error`
///    becomes this function's `Err`, which Pi prints red and follows with `process.exit(1)`
///    (`:583-585`); returning it keeps the exit in `main.rs` where the process lives. A
///    resolution that yields *neither* model nor error (a strict invalid `:suffix`) falls
///    through to step 2 instead of exiting.
///
///    **Quirk:** this step **discards the parsed thinking level** and returns
///    [`DEFAULT_THINKING_LEVEL`] (`:587`). `--provider anthropic --model
///    claude-sonnet-4-5:high` therefore starts at `medium`, not `high` — the fixture's
///    `step1:cli-pair-wins` record exists for exactly this.
/// 2. **`scopedModels[0]`** when the scope is non-empty *and* not continuing. Its level is
///    `scoped.thinkingLevel ?? defaultThinkingLevel ?? DEFAULT_THINKING_LEVEL` (`:595`) — the
///    only place `settings.defaultThinkingLevel` and a scope level compete.
/// 3. **The saved settings default**, but only when `defaultProvider` *and* `defaultModelId`
///    are both set, the model still exists, **and** its provider has configured auth
///    (`:601-603`). `defaultThinkingLevel` applies here too.
/// 4. **The first [`DEFAULT_MODEL_PER_PROVIDER`] pair present in `getAvailable()`**, scanning
///    that table's **key order** — not the available list's order (`:617-623`) — else
///    `availableModels[0]`. Always [`DEFAULT_THINKING_LEVEL`].
/// 5. Nothing available → `model: None`, [`DEFAULT_THINKING_LEVEL`].
pub fn find_initial_model(
    options: FindInitialModelOptions<'_>,
    source: &dyn ModelSource,
) -> Result<InitialModelResult, String> {
    let FindInitialModelOptions {
        cli_provider,
        cli_model,
        scoped_models,
        is_continuing,
        default_provider,
        default_model_id,
        default_thinking_level,
    } = options;

    // 1. `if (cliProvider && cliModel)` (`:576`) — JS truthiness on both.
    if let (Some(cli_provider), Some(cli_model)) = (
        cli_provider.filter(|p| !p.is_empty()),
        cli_model.filter(|m| !m.is_empty()),
    ) {
        // Note: no `cliThinking` is forwarded (`:577-581`), so step 10 of resolveCliModel
        // always takes its suffix-splitting branch here.
        let resolved = resolve_cli_model(
            ResolveCliModelOptions {
                cli_provider: Some(cli_provider),
                cli_model: Some(cli_model),
                cli_thinking: None,
            },
            source,
        );
        if let Some(error) = resolved.error {
            return Err(error);
        }
        if let Some(model) = resolved.model {
            // `resolved.thinkingLevel` is deliberately dropped (`:587`).
            return Ok(InitialModelResult {
                model: Some(model),
                thinking_level: DEFAULT_THINKING_LEVEL,
                fallback_message: None,
            });
        }
    }

    // 2. `scopedModels[0]` (`:592-598`).
    if let Some(first) = scoped_models.first() {
        if !is_continuing {
            return Ok(InitialModelResult {
                model: Some(first.model.clone()),
                thinking_level: first
                    .thinking_level
                    .or(default_thinking_level)
                    .unwrap_or(DEFAULT_THINKING_LEVEL),
                fallback_message: None,
            });
        }
    }

    // 3. The saved settings default (`:601-610`).
    if let (Some(default_provider), Some(default_model_id)) = (
        default_provider.filter(|p| !p.is_empty()),
        default_model_id.filter(|m| !m.is_empty()),
    ) {
        if let Some(found) = source.get_model(default_provider, default_model_id) {
            if source.has_configured_auth(&found.provider.0) {
                return Ok(InitialModelResult {
                    model: Some(found.clone()),
                    thinking_level: default_thinking_level.unwrap_or(DEFAULT_THINKING_LEVEL),
                    fallback_message: None,
                });
            }
        }
    }

    // 4. The defaultModelPerProvider scan, in table order (`:613-627`).
    let available_models = source.get_available();
    if !available_models.is_empty() {
        for (provider, default_id) in DEFAULT_MODEL_PER_PROVIDER {
            if let Some(matched) = available_models
                .iter()
                .find(|m| m.provider.0 == provider && m.id == default_id)
            {
                return Ok(InitialModelResult {
                    model: Some(matched.clone()),
                    thinking_level: DEFAULT_THINKING_LEVEL,
                    fallback_message: None,
                });
            }
        }
        return Ok(InitialModelResult {
            model: Some(available_models[0].clone()),
            thinking_level: DEFAULT_THINKING_LEVEL,
            fallback_message: None,
        });
    }

    // 5. `:630`.
    Ok(InitialModelResult {
        model: None,
        thinking_level: DEFAULT_THINKING_LEVEL,
        fallback_message: None,
    })
}

/// `restoreModelFromSession(...)` (`model-resolver.ts:636-705`).
///
/// The console writes (`:650,659,665,694`) are returned as `messages` instead of printed, so
/// the library stays quiet and `main.rs` decides about colour and `--quiet`; `shouldPrintMessages`
/// therefore becomes the caller's decision to render them or not, and is not a parameter.
///
/// The fallback chain is: the restored model when it exists *and* still has configured auth;
/// else `currentModel`; else the same [`DEFAULT_MODEL_PER_PROVIDER`] scan as step 4 of
/// [`find_initial_model`]; else nothing. `reason` is `model no longer exists` when
/// `getModel` missed and `no auth configured` when it did not (`:656`).
///
/// No fixture record covers this function — it is reached only from the session-restore path,
/// which `session.rs` owns — so it is ported for completeness and marked as unverified.
pub fn restore_model_from_session(
    saved_provider: &str,
    saved_model_id: &str,
    current_model: Option<&Model>,
    source: &dyn ModelSource,
) -> (Option<Model>, Option<String>, Vec<String>) {
    let restored_model = source.get_model(saved_provider, saved_model_id).cloned();
    let has_configured_auth = restored_model
        .as_ref()
        .is_some_and(|m| source.has_configured_auth(&m.provider.0));

    if let Some(restored) = restored_model.clone() {
        if has_configured_auth {
            return (
                Some(restored),
                None,
                vec![format!("Restored model: {saved_provider}/{saved_model_id}")],
            );
        }
    }

    let reason = if restored_model.is_none() {
        "model no longer exists"
    } else {
        "no auth configured"
    };
    let mut messages = vec![format!(
        "Warning: Could not restore model {saved_provider}/{saved_model_id} ({reason})."
    )];

    if let Some(current) = current_model {
        messages.push(format!(
            "Falling back to: {}/{}",
            current.provider.0, current.id
        ));
        return (
            Some(current.clone()),
            Some(format!(
                "Could not restore model {saved_provider}/{saved_model_id} ({reason}). Using {}/{}.",
                current.provider.0, current.id
            )),
            messages,
        );
    }

    let available_models = source.get_available();
    if !available_models.is_empty() {
        let fallback_model = DEFAULT_MODEL_PER_PROVIDER
            .iter()
            .find_map(|(provider, default_id)| {
                available_models
                    .iter()
                    .find(|m| m.provider.0 == *provider && m.id == *default_id)
            })
            .unwrap_or(&available_models[0]);
        messages.push(format!(
            "Falling back to: {}/{}",
            fallback_model.provider.0, fallback_model.id
        ));
        return (
            Some(fallback_model.clone()),
            Some(format!(
                "Could not restore model {saved_provider}/{saved_model_id} ({reason}). Using {}/{}.",
                fallback_model.provider.0, fallback_model.id
            )),
            messages,
        );
    }

    (None, None, messages)
}

// =============================================================================
// model-resolver.ts:248-340 — model scope
// =============================================================================

/// `ScopedModel` (`model-resolver.ts:53-57`).
#[derive(Debug, Clone, PartialEq)]
pub struct ScopedModel {
    /// The model itself.
    pub model: Model,
    /// The level only when the pattern named one (`model:high`); `None` otherwise.
    pub thinking_level: Option<ThinkingLevel>,
}

/// `ModelScopeDiagnostic` (`model-resolver.ts:259-263`). `type` is the literal `"warning"`
/// for every diagnostic Pi emits, so it is not modelled as a field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelScopeDiagnostic {
    /// The human-readable message, unprefixed.
    pub message: String,
    /// The pattern that produced it.
    pub pattern: String,
}

/// `ResolveModelScopeResult` (`model-resolver.ts:265-268`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ResolveModelScopeResult {
    /// Models in the order they were matched, de-duplicated.
    pub scoped_models: Vec<ScopedModel>,
    /// Every warning, in emission order.
    pub diagnostics: Vec<ModelScopeDiagnostic>,
}

/// `resolveModelScopeWithDiagnostics(patterns, modelRuntime)` (`model-resolver.ts:270-332`),
/// over `getAvailable()`.
///
/// A pattern containing `*`, `?` or `[` takes the **glob** branch (`:280`):
///
/// - a trailing `:<level>` is stripped **only if the suffix is a valid level** (`:286-292`),
///   so `anthropic/*:bogus` keeps `:bogus` in the glob and therefore matches nothing;
/// - matching is `minimatch(`{provider}/{id}`, glob, {nocase:true}) || minimatch(id, glob,
///   {nocase:true})` (`:296-299`) — see [`minimatch_nocase`] for the subset implemented. The
///   second arm is why a bare `*sonnet*` works at all: `*` never crosses a `/`, so a
///   one-segment glob can never match a two-segment `provider/id`.
///
/// Anything else goes through [`parse_model_pattern`] with the **lenient** options, so an
/// invalid `:suffix` both warns *and* still resolves. Its warning is pushed first, then the
/// no-match warning if there is no model (`:316-323`) — a pattern can therefore produce two
/// diagnostics.
///
/// **De-duplication is by `modelsAreEqual`, i.e. by model alone** (`:307,326`). So
/// `--models "x:high,x:low"` yields one entry at `high` and the second level is *silently
/// dropped*; keying on `(model, thinkingLevel)` would produce two entries and diverge.
pub fn resolve_model_scope_with_diagnostics(
    patterns: &[String],
    source: &dyn ModelSource,
) -> ResolveModelScopeResult {
    let available_models = source.get_available();
    let mut scoped_models: Vec<ScopedModel> = Vec::new();
    let mut diagnostics: Vec<ModelScopeDiagnostic> = Vec::new();

    for pattern in patterns {
        if pattern.contains('*') || pattern.contains('?') || pattern.contains('[') {
            let mut glob_pattern = pattern.as_str();
            let mut thinking_level: Option<ThinkingLevel> = None;

            if let Some(colon_idx) = pattern.rfind(':') {
                if let Some(level) = thinking_level_from_str(&pattern[colon_idx + 1..]) {
                    thinking_level = Some(level);
                    glob_pattern = &pattern[..colon_idx];
                }
            }

            let matching_models: Vec<&Model> = available_models
                .iter()
                .filter(|m| {
                    let full_id = format!("{}/{}", m.provider.0, m.id);
                    minimatch_nocase(&full_id, glob_pattern)
                        || minimatch_nocase(&m.id, glob_pattern)
                })
                .collect();

            if matching_models.is_empty() {
                diagnostics.push(ModelScopeDiagnostic {
                    message: format!("No models match pattern \"{pattern}\""),
                    pattern: pattern.clone(),
                });
                continue;
            }

            for model in matching_models {
                if !scoped_models
                    .iter()
                    .any(|sm| models_are_equal(&sm.model, model))
                {
                    scoped_models.push(ScopedModel {
                        model: model.clone(),
                        thinking_level,
                    });
                }
            }
            continue;
        }

        let parsed =
            parse_model_pattern(pattern, available_models, ParseModelPatternOptions::LENIENT);

        if let Some(warning) = parsed.warning {
            diagnostics.push(ModelScopeDiagnostic {
                message: warning,
                pattern: pattern.clone(),
            });
        }

        let Some(model) = parsed.model else {
            diagnostics.push(ModelScopeDiagnostic {
                message: format!("No models match pattern \"{pattern}\""),
                pattern: pattern.clone(),
            });
            continue;
        };

        if !scoped_models
            .iter()
            .any(|sm| models_are_equal(&sm.model, &model))
        {
            scoped_models.push(ScopedModel {
                model,
                thinking_level: parsed.thinking_level,
            });
        }
    }

    ResolveModelScopeResult {
        scoped_models,
        diagnostics,
    }
}

/// `resolveModelScope(patterns, modelRuntime)` (`model-resolver.ts:334-340`) — the same
/// resolution with every diagnostic rendered as `` `Warning: {message}` ``.
///
/// Pi writes those to stderr in `chalk.yellow`; they are *returned* here so the library never
/// touches a stream and `main.rs` keeps sole ownership of colour and `NO_COLOR`.
pub fn resolve_model_scope(
    patterns: &[String],
    source: &dyn ModelSource,
) -> (Vec<ScopedModel>, Vec<String>) {
    let result = resolve_model_scope_with_diagnostics(patterns, source);
    let warnings = result
        .diagnostics
        .iter()
        .map(|d| format!("Warning: {}", d.message))
        .collect();
    (result.scoped_models, warnings)
}

// =============================================================================
// minimatch — the subset `resolveModelScopeWithDiagnostics` reaches
// =============================================================================

/// `minimatch(path, pattern, { nocase: true })` for the features model-scope globs use.
///
/// **`/` is a separator.** Both sides are split on it and matched segment by segment, which
/// is the whole reason `resolveModelScopeWithDiagnostics` tries `id` as well as
/// `provider/id`: a one-segment pattern like `*sonnet*` can never match the two-segment
/// `anthropic/claude-sonnet-4-5`, because `*` does not cross a separator.
///
/// # Implemented
///
/// - `*` (zero or more non-`/` characters), `?` (exactly one), `[...]` character classes with
///   ranges and a leading `!`/`^` negation, and `\` escapes;
/// - `**` as a whole segment: matches zero or more segments (globstar);
/// - `nocase: true` — both sides lowercased before comparison;
/// - `dot: false` (minimatch's default): a segment pattern that does not itself start with
///   `.` will not match a name that does. Unreachable for model ids, implemented for
///   fidelity;
/// - an unterminated `[` is a literal `[`, as minimatch treats it.
///
/// # Not implemented (each would need a fixture record to port safely)
///
/// - **Brace expansion** (`{a,b}`), which minimatch applies *before* matching. Reachable in
///   principle — `*{a,b}` contains a `*`, so it would take the glob branch — and it would
///   silently fail to match here where Pi expands it. `{` alone does not trigger the glob
///   branch, so this needs a `*`/`?`/`[` in the same pattern.
/// - **Extglob** (`+(a|b)`, `!(a)`, `@(a|b)`, `?(a)`), **negated patterns** (a leading `!`),
///   **comments** (a leading `#`), and the `windowsPathsNoEscape` / `matchBase` /
///   `partial` options.
/// - **POSIX classes** inside brackets (`[[:alpha:]]`).
pub fn minimatch_nocase(path: &str, pattern: &str) -> bool {
    let path_segments: Vec<String> = path.split('/').map(str::to_lowercase).collect();
    let pattern_segments: Vec<String> = pattern.split('/').map(str::to_lowercase).collect();
    match_segments(&path_segments, &pattern_segments)
}

/// Segment-list matching, with `**` consuming zero or more segments.
fn match_segments(path: &[String], pattern: &[String]) -> bool {
    match pattern.split_first() {
        None => path.is_empty(),
        Some((head, rest)) if head == "**" => {
            // Zero segments, then one, then two, …
            (0..=path.len()).any(|skip| match_segments(&path[skip..], rest))
        }
        Some((head, rest)) => match path.split_first() {
            Some((name, tail)) if match_segment(name, head) => match_segments(tail, rest),
            _ => false,
        },
    }
}

/// One segment against one segment pattern.
fn match_segment(name: &str, pattern: &str) -> bool {
    // `dot: false` — a leading `.` must be matched literally.
    if name.starts_with('.') && !pattern.starts_with('.') {
        return false;
    }
    glob_match(name.as_bytes(), pattern.as_bytes())
}

/// Backtracking wildcard match over one segment. ASCII-only metacharacters, so byte scanning
/// lands on char boundaries for every `*`/`?`/`[`/`\` decision; a `?` or a bracket range does
/// consume a single **byte**, which for a multi-byte character diverges from JS's single
/// UTF-16 unit. Unreachable for model ids and noted rather than solved.
fn glob_match(name: &[u8], pattern: &[u8]) -> bool {
    if pattern.is_empty() {
        return name.is_empty();
    }
    match pattern[0] {
        b'*' => {
            // Collapse a run of `*` and try every split point.
            let rest = &pattern[1..];
            (0..=name.len()).any(|i| glob_match(&name[i..], rest))
        }
        b'?' => !name.is_empty() && glob_match(&name[1..], &pattern[1..]),
        b'[' => match parse_bracket(pattern) {
            Some((class, consumed)) => {
                !name.is_empty()
                    && class.matches(name[0])
                    && glob_match(&name[1..], &pattern[consumed..])
            }
            // Unterminated `[` is a literal.
            None => !name.is_empty() && name[0] == b'[' && glob_match(&name[1..], &pattern[1..]),
        },
        b'\\' if pattern.len() > 1 => {
            !name.is_empty() && name[0] == pattern[1] && glob_match(&name[1..], &pattern[2..])
        }
        literal => !name.is_empty() && name[0] == literal && glob_match(&name[1..], &pattern[1..]),
    }
}

/// A parsed `[...]` bracket expression.
struct BracketClass {
    negated: bool,
    /// Inclusive byte ranges; a single character is `(c, c)`.
    ranges: Vec<(u8, u8)>,
}

impl BracketClass {
    fn matches(&self, byte: u8) -> bool {
        let hit = self
            .ranges
            .iter()
            .any(|(lo, hi)| byte >= *lo && byte <= *hi);
        hit != self.negated
    }
}

/// Parse `[...]` at the start of `pattern`, returning the class and how many bytes it spans.
/// `None` when unterminated.
fn parse_bracket(pattern: &[u8]) -> Option<(BracketClass, usize)> {
    let mut i = 1;
    let negated = matches!(pattern.get(i), Some(b'!' | b'^'));
    if negated {
        i += 1;
    }
    let mut ranges: Vec<(u8, u8)> = Vec::new();
    // A `]` in first position is a literal `]`, as POSIX and minimatch agree.
    let mut first = true;
    while i < pattern.len() {
        if pattern[i] == b']' && !first {
            return Some((BracketClass { negated, ranges }, i + 1));
        }
        first = false;
        let lo = pattern[i];
        if pattern.get(i + 1) == Some(&b'-') && pattern.get(i + 2).is_some_and(|c| *c != b']') {
            ranges.push((lo, pattern[i + 2]));
            i += 3;
        } else {
            ranges.push((lo, lo));
            i += 1;
        }
    }
    None
}

// =============================================================================
// utils/json.ts:2-6 — stripJsonComments
// =============================================================================

/// `stripJsonComments(input)` (`utils/json.ts:2-6`), the pre-parse pass
/// [`ModelConfig::load`] applies and `settings.json` does **not**.
///
/// Two regex replacements, both string-literal-aware:
///
/// 1. `` /"(?:\\.|[^"\\])*"|\/\/[^\n]*/g `` → keep string literals, delete `//` comments to
///    end of line;
/// 2. `` /"(?:\\.|[^"\\])*"|,(\s*[}\]])/g `` → keep string literals, delete a comma that is
///    followed only by whitespace and a `}` or `]` (trailing commas).
///
/// **It does not strip `/* */` block comments.** The function's own doc comment says so
/// ("Strip `//` line comments and trailing commas"), and the fixture confirms it the hard
/// way: the record *named* `json-with-comments-is-ACCEPTED` records a **parse failure** at
/// exactly the position of the surviving `/*`. Adding block-comment support here would make
/// pirust accept a file real Pi rejects.
///
/// Deleting rather than blanking the comment matters for the V8 error positions Pi reports
/// (the captured message says `line 4 column 5`, which only lines up once the `//` text is
/// gone but its newline is kept) — so the transformation is byte-for-byte the same even
/// though the resulting diagnostic text cannot be.
pub fn strip_json_comments(input: &str) -> String {
    strip_trailing_commas(&strip_line_comments(input))
}

/// Pass 1: `` /"(?:\\.|[^"\\])*"|\/\/[^\n]*/g ``.
fn strip_line_comments(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'"' {
            let (literal, next) = scan_json_string(input, i);
            out.push_str(literal);
            i = next;
            continue;
        }
        if bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'/') {
            // `[^\n]*` — the newline itself is not consumed.
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        let ch = input[i..].chars().next().expect("valid boundary");
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Pass 2: `` /"(?:\\.|[^"\\])*"|,(\s*[}\]])/g `` → the captured tail replaces the whole
/// match, i.e. the comma is dropped and the whitespace kept.
fn strip_trailing_commas(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'"' {
            let (literal, next) = scan_json_string(input, i);
            out.push_str(literal);
            i = next;
            continue;
        }
        if bytes[i] == b',' {
            // `\s*` is JS's regex whitespace class; every byte it can match is ASCII in
            // practice for a JSON file, and `char::is_whitespace` is a superset that cannot
            // change the outcome for the `[}\]]` lookahead below.
            let mut j = i + 1;
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            if matches!(bytes.get(j), Some(b'}' | b']')) {
                // Drop the comma, keep the whitespace run and the closer.
                out.push_str(&input[i + 1..=j]);
                i = j + 1;
                continue;
            }
        }
        let ch = input[i..].chars().next().expect("valid boundary");
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// `"(?:\\.|[^"\\])*"` starting at `start` (which must be the opening quote). Returns the
/// matched slice and the index just past it. An unterminated literal matches nothing in the
/// regex, so the quote is emitted alone and scanning continues after it.
fn scan_json_string(input: &str, start: usize) -> (&str, usize) {
    let bytes = input.as_bytes();
    let mut i = start + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' if i + 1 < bytes.len() => {
                // `\\.` — `.` excludes newlines in JS, so a backslash-newline breaks the match.
                if bytes[i + 1] == b'\n' {
                    return (&input[start..start + 1], start + 1);
                }
                i += 2;
            }
            b'"' => return (&input[start..=i], i + 1),
            _ => i += 1,
        }
    }
    (&input[start..start + 1], start + 1)
}

// =============================================================================
// model-config.ts:10-204 — the models.json schema, as serde lenses
// =============================================================================

/// `ModelDefinitionSchema` (`model-config.ts:148-161`) — one entry of a provider's `models`.
///
/// A **lens** over already-validated JSON, in the style `settings.rs` established: the schema
/// check happens on the `Value` (so Pi's exact TypeBox messages survive), and this struct only
/// reads the fields composition needs. Unknown keys are ignored rather than rejected, because
/// TypeBox's `Type.Object` is open — the fixture's
/// `provider-with-an-unknown-field-is-a-SCHEMA-ERROR` record is, despite its name, a **pass**.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelsJsonModel {
    /// Required, `minLength: 1`.
    pub id: String,
    /// Defaults to `id` in [`model_from_json`].
    pub name: Option<String>,
    /// Falls back to the provider's `api`, then the builtin's.
    pub api: Option<String>,
    /// Falls back to the provider's `baseUrl`, then the builtin's.
    pub base_url: Option<String>,
    /// Defaults to `false`.
    pub reasoning: Option<bool>,
    /// Carried through verbatim, `null`-vs-absent preserved by [`ThinkingLevelMap`].
    pub thinking_level_map: Option<ThinkingLevelMap>,
    /// Defaults to `["text"]`.
    pub input: Option<Vec<Modality>>,
    /// Defaults to all-zero rates.
    pub cost: Option<ModelCost>,
    /// Defaults to `128000`. `f64` because TypeBox accepts any `number`; see the module docs
    /// on the truncation that follows.
    pub context_window: Option<f64>,
    /// Defaults to `16384`.
    pub max_tokens: Option<f64>,
    /// **Never copied onto the model** — see [`model_from_json`].
    pub headers: Option<Map<String, Value>>,
    /// Opaque until feat-008 types the per-api unions.
    pub compat: Option<Value>,
}

/// `ModelOverrideSchema` (`model-config.ts:163-181`) — `ModelDefinition` minus `id`, `api`
/// and `baseUrl`, with every `cost` field optional.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelsJsonModelOverride {
    /// Replaces `name`.
    pub name: Option<String>,
    /// Replaces `reasoning`.
    pub reasoning: Option<bool>,
    /// **Merged** onto the existing map, not replacing it.
    pub thinking_level_map: Option<ThinkingLevelMap>,
    /// Replaces `input` wholesale.
    pub input: Option<Vec<Modality>>,
    /// Per-field overlay.
    pub cost: Option<ModelsJsonCostOverride>,
    /// Replaces `contextWindow`.
    pub context_window: Option<f64>,
    /// Replaces `maxTokens`.
    pub max_tokens: Option<f64>,
    /// Accepted by the schema but **ignored by `applyModelOverride`** — request-time only.
    pub headers: Option<Map<String, Value>>,
    /// Merged by [`merge_compat`].
    pub compat: Option<Value>,
}

/// The all-optional `cost` of `ModelOverrideSchema` (`model-config.ts:168-176`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelsJsonCostOverride {
    /// Input rate.
    pub input: Option<f64>,
    /// Output rate.
    pub output: Option<f64>,
    /// Cache-read rate.
    pub cache_read: Option<f64>,
    /// Cache-write rate.
    pub cache_write: Option<f64>,
    /// Replaces the tier list wholesale.
    pub tiers: Option<Vec<ModelCostTier>>,
}

/// `ProviderConfigSchema` (`model-config.ts:183-194`) — one `providers` entry.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelsJsonProvider {
    /// Display name; wins over the builtin's.
    pub name: Option<String>,
    /// Overrides the builtin's `baseUrl` **and every model's** — the local-proxy lever.
    pub base_url: Option<String>,
    /// A literal key, a `$VAR` template or a `!command` (resolved by `crate::auth`).
    pub api_key: Option<String>,
    /// Provider-level `api`, the middle of `definition.api ?? provider.api ?? builtin.api`.
    pub api: Option<String>,
    /// `Type.Literal("radius")` — the one unported oauth flow.
    pub oauth: Option<String>,
    /// Request headers. **Not copied onto models** — see [`resolve_configured_model_headers`].
    pub headers: Option<Map<String, Value>>,
    /// Merged into every model's `compat` by [`apply_models_json`].
    pub compat: Option<Value>,
    /// Forces `Authorization: Bearer <key>` at request time.
    pub auth_header: Option<bool>,
    /// Custom models, upserted onto the builtin list by id.
    pub models: Option<Vec<ModelsJsonModel>>,
    /// Per-model-id patches, applied **last** of all layers.
    pub model_overrides: Option<BTreeMap<String, ModelsJsonModelOverride>>,
}

// =============================================================================
// model-config.ts:196-217 — validation
// =============================================================================

/// One TypeBox validation failure, in the shape `formatValidationPath`
/// (`model-config.ts:206-217`) consumes.
///
/// # Which messages are oracle-verified
///
/// Exactly four, each with its own fixture record:
///
/// | record | rendered line |
/// |---|---|
/// | `missing-providers-key-is-a-SCHEMA-ERROR` | `  - providers: must have required properties providers` |
/// | `empty-string-baseUrl-is-a-SCHEMA-ERROR` | `  - providers.anthropic.baseUrl: must not have fewer than 1 characters` |
/// | `model-without-an-id-is-a-SCHEMA-ERROR` | `  - providers.oracle-x.models.0.id: must have required properties id` |
/// | `providers-as-an-array-is-a-SCHEMA-ERROR` | `  - providers: must be object` |
///
/// Everything else this module can emit (`must be string`, `must be boolean`, `must be
/// number`, `must be array`, `must be equal to …`) is **best-effort TypeBox wording, not
/// verified against the oracle**. So is the *order* of two or more simultaneous errors: no
/// record produces more than one line, so the traversal order below (declared-property order,
/// depth first, type-before-required-before-children) is a reasonable reading of
/// `Compile(...).Errors()` rather than a proven one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError {
    /// TypeBox's `instancePath`, e.g. `/providers/anthropic/baseUrl`.
    pub instance_path: String,
    /// The localized message, e.g. `must be object`.
    pub message: String,
    /// `params.requiredProperties[0]` for a `required` error; `None` otherwise.
    pub required_property: Option<String>,
}

impl ValidationError {
    /// `formatValidationPath(error)` (`model-config.ts:206-217`): strip the leading `/`,
    /// turn the rest into a dotted path, fall back to `root` — and for a `required` error
    /// append the missing property name (`` `${basePath}.${prop}` ``, or just `prop` when the
    /// base path is empty).
    pub fn format_path(&self) -> String {
        let base_path = self
            .instance_path
            .strip_prefix('/')
            .unwrap_or(&self.instance_path)
            .replace('/', ".");
        if let Some(required) = &self.required_property {
            return if base_path.is_empty() {
                required.clone()
            } else {
                format!("{base_path}.{required}")
            };
        }
        if base_path.is_empty() {
            "root".to_string()
        } else {
            base_path
        }
    }

    /// `` `  - ${formatValidationPath(error)}: ${error.message}` `` (`model-config.ts:263`).
    pub fn format_line(&self) -> String {
        format!("  - {}: {}", self.format_path(), self.message)
    }

    fn required(instance_path: &str, property: &str) -> Self {
        Self {
            instance_path: instance_path.to_string(),
            message: format!("must have required properties {property}"),
            required_property: Some(property.to_string()),
        }
    }

    fn type_error(instance_path: &str, kind: &str) -> Self {
        Self {
            instance_path: instance_path.to_string(),
            message: format!("must be {kind}"),
            required_property: None,
        }
    }

    fn min_length(instance_path: &str, min: usize) -> Self {
        Self {
            instance_path: instance_path.to_string(),
            message: format!("must not have fewer than {min} characters"),
            required_property: None,
        }
    }

    fn literal(instance_path: &str, expected: &str) -> Self {
        Self {
            instance_path: instance_path.to_string(),
            message: format!("must be equal to {expected}"),
            required_property: None,
        }
    }
}

/// `validateModelsConfig.Check` / `.Errors` (`model-config.ts:199,259-265`) for
/// `ModelsConfigSchema = Type.Object({ providers: Type.Record(Type.String(), ProviderConfig) })`.
///
/// **`api` is deliberately not constrained beyond `string(minLength 1)`.** An unrecognised
/// value composes fine, produces no error, and the model *is* listed and counted available —
/// the failure only surfaces at stream time via [`api_provider_error`]. Adding an allow-list
/// here would break the `unknown-api-value-COMPOSES-FINE` record.
///
/// The `compat` union (`model-config.ts:127-131`: OpenAICompletions | OpenAIResponses |
/// AnthropicMessages, all three open objects with all-optional properties) is checked only as
/// "is an object". Because the arms are open and fully optional, *any* object matches at least
/// one of them unless a declared key carries the wrong type — so the only gap is a
/// wrong-typed known compat key, which Pi would reject and this accepts. Transcribing ~35
/// compat fields with zero fixture coverage would be guesswork; feat-008 owns typed compat and
/// should close it then.
pub fn validate_models_config(root: &Value) -> Vec<ValidationError> {
    let mut errors: Vec<ValidationError> = Vec::new();

    let Some(root_obj) = root.as_object() else {
        errors.push(ValidationError::type_error("", "object"));
        return errors;
    };
    if !root_obj.contains_key("providers") {
        errors.push(ValidationError::required("", "providers"));
    }
    if let Some(providers) = root_obj.get("providers") {
        match providers.as_object() {
            None => errors.push(ValidationError::type_error("/providers", "object")),
            Some(map) => {
                for (id, provider) in map {
                    validate_provider(&format!("/providers/{id}"), provider, &mut errors);
                }
            }
        }
    }
    errors
}

/// `ProviderConfigSchema` (`model-config.ts:183-194`).
fn validate_provider(path: &str, value: &Value, errors: &mut Vec<ValidationError>) {
    let Some(obj) = value.as_object() else {
        errors.push(ValidationError::type_error(path, "object"));
        return;
    };
    // Declaration order, as TypeBox traverses it.
    for key in ["name", "baseUrl", "apiKey", "api"] {
        if let Some(v) = obj.get(key) {
            validate_non_empty_string(&format!("{path}/{key}"), v, errors);
        }
    }
    if let Some(v) = obj.get("oauth") {
        if v.as_str() != Some("radius") {
            errors.push(ValidationError::literal(&format!("{path}/oauth"), "radius"));
        }
    }
    if let Some(v) = obj.get("headers") {
        validate_string_record(&format!("{path}/headers"), v, errors);
    }
    if let Some(v) = obj.get("compat") {
        validate_compat(&format!("{path}/compat"), v, errors);
    }
    if let Some(v) = obj.get("authHeader") {
        if !v.is_boolean() {
            errors.push(ValidationError::type_error(
                &format!("{path}/authHeader"),
                "boolean",
            ));
        }
    }
    if let Some(v) = obj.get("models") {
        match v.as_array() {
            None => errors.push(ValidationError::type_error(
                &format!("{path}/models"),
                "array",
            )),
            Some(items) => {
                for (index, item) in items.iter().enumerate() {
                    validate_model_definition(&format!("{path}/models/{index}"), item, errors);
                }
            }
        }
    }
    if let Some(v) = obj.get("modelOverrides") {
        match v.as_object() {
            None => errors.push(ValidationError::type_error(
                &format!("{path}/modelOverrides"),
                "object",
            )),
            Some(map) => {
                for (id, item) in map {
                    validate_model_override(&format!("{path}/modelOverrides/{id}"), item, errors);
                }
            }
        }
    }
}

/// `ModelDefinitionSchema` (`model-config.ts:148-161`).
fn validate_model_definition(path: &str, value: &Value, errors: &mut Vec<ValidationError>) {
    let Some(obj) = value.as_object() else {
        errors.push(ValidationError::type_error(path, "object"));
        return;
    };
    if !obj.contains_key("id") {
        errors.push(ValidationError::required(path, "id"));
    }
    for key in ["id", "name", "api", "baseUrl"] {
        if let Some(v) = obj.get(key) {
            validate_non_empty_string(&format!("{path}/{key}"), v, errors);
        }
    }
    validate_shared_model_fields(path, obj, errors);
    if let Some(v) = obj.get("cost") {
        validate_cost(&format!("{path}/cost"), v, true, errors);
    }
}

/// `ModelOverrideSchema` (`model-config.ts:163-181`).
fn validate_model_override(path: &str, value: &Value, errors: &mut Vec<ValidationError>) {
    let Some(obj) = value.as_object() else {
        errors.push(ValidationError::type_error(path, "object"));
        return;
    };
    if let Some(v) = obj.get("name") {
        validate_non_empty_string(&format!("{path}/name"), v, errors);
    }
    validate_shared_model_fields(path, obj, errors);
    if let Some(v) = obj.get("cost") {
        validate_cost(&format!("{path}/cost"), v, false, errors);
    }
}

/// The fields `ModelDefinitionSchema` and `ModelOverrideSchema` share verbatim.
fn validate_shared_model_fields(
    path: &str,
    obj: &Map<String, Value>,
    errors: &mut Vec<ValidationError>,
) {
    if let Some(v) = obj.get("reasoning") {
        if !v.is_boolean() {
            errors.push(ValidationError::type_error(
                &format!("{path}/reasoning"),
                "boolean",
            ));
        }
    }
    if let Some(v) = obj.get("thinkingLevelMap") {
        validate_thinking_level_map(&format!("{path}/thinkingLevelMap"), v, errors);
    }
    if let Some(v) = obj.get("input") {
        match v.as_array() {
            None => errors.push(ValidationError::type_error(
                &format!("{path}/input"),
                "array",
            )),
            Some(items) => {
                for (index, item) in items.iter().enumerate() {
                    if !matches!(item.as_str(), Some("text" | "image")) {
                        errors.push(ValidationError::literal(
                            &format!("{path}/input/{index}"),
                            "text",
                        ));
                    }
                }
            }
        }
    }
    for key in ["contextWindow", "maxTokens"] {
        if let Some(v) = obj.get(key) {
            if !v.is_number() {
                errors.push(ValidationError::type_error(
                    &format!("{path}/{key}"),
                    "number",
                ));
            }
        }
    }
    if let Some(v) = obj.get("headers") {
        validate_string_record(&format!("{path}/headers"), v, errors);
    }
    if let Some(v) = obj.get("compat") {
        validate_compat(&format!("{path}/compat"), v, errors);
    }
}

/// `ModelCostSchema` (`model-config.ts:143-146`) — the four rates are **required** in a
/// `ModelDefinition` and all optional in a `ModelOverride`.
fn validate_cost(
    path: &str,
    value: &Value,
    rates_required: bool,
    errors: &mut Vec<ValidationError>,
) {
    let Some(obj) = value.as_object() else {
        errors.push(ValidationError::type_error(path, "object"));
        return;
    };
    for key in ["input", "output", "cacheRead", "cacheWrite"] {
        match obj.get(key) {
            None if rates_required => errors.push(ValidationError::required(path, key)),
            Some(v) if !v.is_number() => errors.push(ValidationError::type_error(
                &format!("{path}/{key}"),
                "number",
            )),
            _ => {}
        }
    }
    if let Some(tiers) = obj.get("tiers") {
        match tiers.as_array() {
            None => errors.push(ValidationError::type_error(
                &format!("{path}/tiers"),
                "array",
            )),
            Some(items) => {
                for (index, item) in items.iter().enumerate() {
                    let tier_path = format!("{path}/tiers/{index}");
                    let Some(tier) = item.as_object() else {
                        errors.push(ValidationError::type_error(&tier_path, "object"));
                        continue;
                    };
                    for key in [
                        "inputTokensAbove",
                        "input",
                        "output",
                        "cacheRead",
                        "cacheWrite",
                    ] {
                        match tier.get(key) {
                            None => errors.push(ValidationError::required(&tier_path, key)),
                            Some(v) if !v.is_number() => errors.push(ValidationError::type_error(
                                &format!("{tier_path}/{key}"),
                                "number",
                            )),
                            _ => {}
                        }
                    }
                }
            }
        }
    }
}

/// `ThinkingLevelMapSchema` (`model-config.ts:55-63`) — the seven levels, each `string | null`.
fn validate_thinking_level_map(path: &str, value: &Value, errors: &mut Vec<ValidationError>) {
    let Some(obj) = value.as_object() else {
        errors.push(ValidationError::type_error(path, "object"));
        return;
    };
    for (key, v) in obj {
        if !(v.is_string() || v.is_null()) {
            errors.push(ValidationError::type_error(
                &format!("{path}/{key}"),
                "string",
            ));
        }
    }
}

/// `Type.Record(Type.String(), Type.String())`.
fn validate_string_record(path: &str, value: &Value, errors: &mut Vec<ValidationError>) {
    let Some(obj) = value.as_object() else {
        errors.push(ValidationError::type_error(path, "object"));
        return;
    };
    for (key, v) in obj {
        if !v.is_string() {
            errors.push(ValidationError::type_error(
                &format!("{path}/{key}"),
                "string",
            ));
        }
    }
}

/// `ProviderCompatSchema` (`model-config.ts:127-131`) — object-shape only; see
/// [`validate_models_config`] for why the union arms are not transcribed.
fn validate_compat(path: &str, value: &Value, errors: &mut Vec<ValidationError>) {
    if !value.is_object() {
        errors.push(ValidationError::type_error(path, "object"));
    }
}

/// `Type.String({ minLength: 1 })`.
fn validate_non_empty_string(path: &str, value: &Value, errors: &mut Vec<ValidationError>) {
    match value.as_str() {
        None => errors.push(ValidationError::type_error(path, "string")),
        // `minLength` counts UTF-16 code units in TypeBox; only emptiness matters at 1.
        Some("") => errors.push(ValidationError::min_length(path, 1)),
        Some(_) => {}
    }
}

// =============================================================================
// model-config.ts:226-287 — ModelConfig
// =============================================================================

/// `ModelConfig` (`model-config.ts:226-287`) — one immutable, credential-blind load of
/// `models.json`.
///
/// Providers are kept **in file key order**, which is what
/// [`ModelRuntime`]'s `providerIds()` appends to the builtin order. Both a raw
/// [`Value`] and a typed [`ModelsJsonProvider`] lens are retained per provider: the raw copy is
/// Pi's `deepFreeze(structuredClone(provider))` (`:271`) and preserves unknown keys, which the
/// oracle keeps too; the lens is what composition reads.
#[derive(Debug, Clone, Default)]
pub struct ModelConfig {
    provider_ids: Vec<String>,
    raw: Map<String, Value>,
    typed: HashMap<String, ModelsJsonProvider>,
    error: Option<String>,
}

impl ModelConfig {
    /// `ModelConfig.load(modelsJsonPath)` (`model-config.ts:235-274`).
    ///
    /// 1. No path (or an empty one — `if (!modelsJsonPath)`, `:236`) → empty config, no error;
    ///    the path is never touched.
    /// 2. `normalizePath(path)` — delegated to [`crate::config::ConfigEnv::expand_tilde_path`],
    ///    which can fail exactly where Pi's `fileURLToPath` throws. Pi does **not** catch that
    ///    (it is outside the `try`), so it surfaces as `Err` here rather than as a config
    ///    error string.
    /// 3. `readFile`. **`ENOENT` is not an error** (`:242`): an absent `models.json` yields an
    ///    empty config with `getError() === undefined`. Any other read failure →
    ///    `` `Failed to load models.json: {msg}\n\nFile: {path}` ``, where `{msg}` is Node's
    ///    `error.message` and here is `std::io::Error`'s `Display` — different text, same
    ///    envelope.
    /// 4. `JSON.parse(stripJsonComments(content))` → on failure
    ///    `` `Failed to parse models.json: {msg}\n\nFile: {path}` ``. `{msg}` is V8's wording
    ///    in Pi (`Unexpected end of JSON input`; `` Expected property name or '}' in JSON at
    ///    position 26 (line 4 column 5) ``) and `serde_json`'s here. **Five fixture records
    ///    depend on that text and only three are marked `v8Dependent`** — 124/143
    ///    (`json-with-comments-is-ACCEPTED`) are equally V8-dependent and are missing the
    ///    marker; flagged upstream. The golden test asserts the envelope and the structural
    ///    outcome for all five.
    /// 5. Schema validation → `` `Invalid models.json schema:\n{lines}\n\nFile: {path}` ``,
    ///    each line [`ValidationError::format_line`], or the body `Unknown schema error` when
    ///    the check failed but produced no formatted line (`:264`).
    /// 6. Success → the providers map, in file order.
    ///
    /// Every failure path yields an **empty** config, so no provider is overlaid and every
    /// builtin is used untouched — which is why a malformed `models.json` still leaves all 14
    /// anthropic models listed, just unconfigured.
    pub fn load(
        env: &crate::config::ConfigEnv,
        models_json_path: Option<&str>,
    ) -> Result<Self, crate::config::ConfigPathError> {
        let Some(models_json_path) = models_json_path.filter(|p| !p.is_empty()) else {
            return Ok(Self::default());
        };
        let path = env.expand_tilde_path(models_json_path)?;

        let content = match std::fs::read_to_string(&path) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default())
            }
            Err(error) => {
                return Ok(Self::with_error(format!(
                    "Failed to load models.json: {error}\n\nFile: {path}"
                )))
            }
        };

        let parsed: Value = match serde_json::from_str(&strip_json_comments(&content)) {
            Ok(parsed) => parsed,
            Err(error) => {
                return Ok(Self::with_error(format!(
                    "Failed to parse models.json: {error}\n\nFile: {path}"
                )))
            }
        };

        let errors = validate_models_config(&parsed);
        if !errors.is_empty() {
            let body = errors
                .iter()
                .map(ValidationError::format_line)
                .collect::<Vec<String>>()
                .join("\n");
            let body = if body.is_empty() {
                "Unknown schema error".to_string()
            } else {
                body
            };
            return Ok(Self::with_error(format!(
                "Invalid models.json schema:\n{body}\n\nFile: {path}"
            )));
        }

        Ok(Self::from_validated(&parsed))
    }

    /// The post-validation half of [`ModelConfig::load`], exposed so a caller that already
    /// holds the parsed JSON (or a test) can build a config without touching disk.
    ///
    /// # Panics
    ///
    /// Never: a `providers` value that is not an object cannot reach here, because
    /// [`validate_models_config`] rejects it first. A malformed input simply yields an empty
    /// config.
    pub fn from_validated(parsed: &Value) -> Self {
        let mut config = Self::default();
        let Some(providers) = parsed.get("providers").and_then(Value::as_object) else {
            return config;
        };
        for (provider_id, provider) in providers {
            config.provider_ids.push(provider_id.clone());
            config.raw.insert(provider_id.clone(), provider.clone());
            // Validation ran first, so this cannot fail on a schema-valid input; an unknown
            // key is ignored rather than rejected, matching TypeBox's open objects.
            let typed =
                serde_json::from_value::<ModelsJsonProvider>(provider.clone()).unwrap_or_default();
            config.typed.insert(provider_id.clone(), typed);
        }
        config
    }

    fn with_error(error: String) -> Self {
        Self {
            error: Some(error),
            ..Self::default()
        }
    }

    /// `getProvider(providerId)` (`model-config.ts:276-278`), as the typed lens.
    pub fn get_provider(&self, provider_id: &str) -> Option<&ModelsJsonProvider> {
        self.typed.get(provider_id)
    }

    /// The deep-frozen structural clone Pi stores (`model-config.ts:271`), unknown keys and
    /// all. The fixture's `ModelConfig.load` records assert against exactly this.
    pub fn get_provider_raw(&self, provider_id: &str) -> Option<&Value> {
        self.raw.get(provider_id)
    }

    /// `getProviderIds()` (`:280-282`) — **file key order**.
    pub fn provider_ids(&self) -> &[String] {
        &self.provider_ids
    }

    /// `getError()` (`:284-286`).
    pub fn get_error(&self) -> Option<&str> {
        self.error.as_deref()
    }
}

// =============================================================================
// provider-composer.ts:78-199 — the composition layers
// =============================================================================

/// `mergeCompat(base, override)` (`provider-composer.ts:78-98`).
///
/// A shallow `{...base, ...override}` — so the base's key order is kept and the override's new
/// keys append, which `serde_json`'s `preserve_order` reproduces because re-inserting an
/// existing key keeps its position. Then the three known **nested** objects
/// (`openRouterRouting`, `vercelGatewayRouting`, `chatTemplateKwargs`) are merged one level
/// deeper, whenever either side has an object there.
///
/// `override === undefined` returns `base` untouched, *including* when the base is also
/// undefined (`:82`).
///
/// Divergence: JS's `typeof x === "object" && x !== null` is true for **arrays** too, so Pi
/// would spread an array member of those three keys into an index-keyed object. `is_object()`
/// here excludes arrays, leaving the array in place. No schema allows an array there.
pub fn merge_compat(base: Option<&Value>, override_value: Option<&Value>) -> Option<Value> {
    let Some(override_value) = override_value else {
        return base.cloned();
    };

    let mut merged = Map::new();
    if let Some(base_obj) = base.and_then(Value::as_object) {
        for (key, value) in base_obj {
            merged.insert(key.clone(), value.clone());
        }
    }
    if let Some(override_obj) = override_value.as_object() {
        for (key, value) in override_obj {
            merged.insert(key.clone(), value.clone());
        }
    }

    for key in [
        "openRouterRouting",
        "vercelGatewayRouting",
        "chatTemplateKwargs",
    ] {
        let base_value = base.and_then(|b| b.get(key));
        let override_nested = override_value.get(key);
        let base_is_object = base_value.is_some_and(Value::is_object);
        let override_is_object = override_nested.is_some_and(Value::is_object);
        if base_is_object || override_is_object {
            let mut nested = Map::new();
            if let Some(obj) = base_value.and_then(Value::as_object) {
                for (k, v) in obj {
                    nested.insert(k.clone(), v.clone());
                }
            }
            if let Some(obj) = override_nested.and_then(Value::as_object) {
                for (k, v) in obj {
                    nested.insert(k.clone(), v.clone());
                }
            }
            merged.insert(key.to_string(), Value::Object(nested));
        }
    }

    Some(Value::Object(merged))
}

/// `applyModelOverride(model, override)` (`provider-composer.ts:100-122`) — the **topmost**
/// user-config layer, applied after custom-model upserts.
///
/// Only the listed keys change. Two details the fixture pins:
///
/// - `thinkingLevelMap` is **merged** (`{...model.map, ...override.map}`, `:106`), not
///   replaced, so an override naming only `max` keeps the model's other levels; an explicit
///   `null` in the override does overwrite, which [`ThinkingLevelMap`]'s
///   `Option<Option<String>>` fields preserve.
/// - **`headers` is not applied.** `ModelOverrideSchema` accepts it, `applyModelOverride`
///   ignores it, and it reaches the wire only through
///   [`resolve_configured_model_headers`] at request time. `model.headers` therefore stays
///   whatever the base had — `null` for every builtin.
pub fn apply_model_override(model: &Model, override_value: &ModelsJsonModelOverride) -> Model {
    let thinking_level_map = match &override_value.thinking_level_map {
        Some(patch) => Some(merge_thinking_level_map(
            model.thinking_level_map.as_ref(),
            patch,
        )),
        None => model.thinking_level_map.clone(),
    };

    let cost = match &override_value.cost {
        Some(patch) => ModelCost {
            rates: ModelCostRates {
                input: patch.input.unwrap_or(model.cost.rates.input),
                output: patch.output.unwrap_or(model.cost.rates.output),
                cache_read: patch.cache_read.unwrap_or(model.cost.rates.cache_read),
                cache_write: patch.cache_write.unwrap_or(model.cost.rates.cache_write),
            },
            tiers: patch.tiers.clone().or_else(|| model.cost.tiers.clone()),
        },
        None => model.cost.clone(),
    };

    Model {
        name: override_value
            .name
            .clone()
            .unwrap_or_else(|| model.name.clone()),
        reasoning: override_value.reasoning.unwrap_or(model.reasoning),
        thinking_level_map,
        input: override_value
            .input
            .clone()
            .unwrap_or_else(|| model.input.clone()),
        cost,
        context_window: override_value
            .context_window
            .map_or(model.context_window, js_number_to_u64),
        max_tokens: override_value
            .max_tokens
            .map_or(model.max_tokens, js_number_to_u64),
        compat: merge_compat(model.compat.as_ref(), override_value.compat.as_ref()),
        ..model.clone()
    }
}

/// `{ ...model.thinkingLevelMap, ...override.thinkingLevelMap }` (`provider-composer.ts:106`).
///
/// Per field, the patch wins when the key is *present* — including when it is present and
/// `null`, which is `Some(None)` and means "this level is unsupported".
fn merge_thinking_level_map(
    base: Option<&ThinkingLevelMap>,
    patch: &ThinkingLevelMap,
) -> ThinkingLevelMap {
    let base = base.cloned().unwrap_or_default();
    ThinkingLevelMap {
        off: patch.off.clone().or(base.off),
        minimal: patch.minimal.clone().or(base.minimal),
        low: patch.low.clone().or(base.low),
        medium: patch.medium.clone().or(base.medium),
        high: patch.high.clone().or(base.high),
        xhigh: patch.xhigh.clone().or(base.xhigh),
        max: patch.max.clone().or(base.max),
    }
}

/// TypeBox's `Type.Number()` is any JS number; [`pirust_ai::types::Model`] stores `u64`.
///
/// Truncates toward zero and clamps at 0, which is what `as u64` does for a finite
/// non-negative value. Reachable only from a hand-written `models.json` with a fractional or
/// negative `contextWindow`/`maxTokens` — and a **non-positive** one is rejected earlier by
/// [`model_from_json`]'s own guard, so only the fractional case can get here. Documented in
/// the module header as an accepted divergence.
fn js_number_to_u64(value: f64) -> u64 {
    if !value.is_finite() || value <= 0.0 {
        return 0;
    }
    value as u64
}

/// `modelFromJson(providerId, definition, providerConfig, defaults)`
/// (`provider-composer.ts:124-159`).
///
/// The three-level fallbacks are `definition.api ?? providerConfig.api ?? defaults?.api` and
/// the same for `baseUrl` (`:130,136`), where `defaults` is the model this definition replaces
/// (or the provider's *first* model when it is a new id). Missing either is a **composition**
/// error, not a schema error:
///
/// - `` `Provider {id}, model {modelId}: no "api" specified. Set at provider or model level.` ``
/// - `` `Provider {id}: "baseUrl" is required when defining custom models.` ``
/// - `` `Provider {id}, model {modelId}: invalid contextWindow` `` when `<= 0`
/// - `` `Provider {id}, model {modelId}: invalid maxTokens` `` when `<= 0`
///
/// Everything unspecified gets a **fixed default, not the base model's value** (`:144-158`):
/// `name := id`, `reasoning := false`, `input := ["text"]`, `cost := all zeros`,
/// `contextWindow := 128000`, `maxTokens := 16384`, and `headers := undefined` *always* —
/// even when the definition declares `headers`. That is why upserting `{"id":
/// "claude-sonnet-4-5"}` onto the builtin replaces a 1M-context reasoning model with a
/// 128K-context non-reasoning one, exactly as the `models-array-UPSERTS-onto-a-builtin-id`
/// record records.
pub fn model_from_json(
    provider_id: &str,
    definition: &ModelsJsonModel,
    provider_config: &ModelsJsonProvider,
    defaults: Option<&Model>,
) -> Result<Model, String> {
    let api = definition
        .api
        .clone()
        .or_else(|| provider_config.api.clone())
        .or_else(|| defaults.map(|d| d.api.0.clone()))
        .ok_or_else(|| {
            format!(
                "Provider {provider_id}, model {}: no \"api\" specified. Set at provider or model level.",
                definition.id
            )
        })?;

    let base_url = definition
        .base_url
        .clone()
        .or_else(|| provider_config.base_url.clone())
        .or_else(|| defaults.map(|d| d.base_url.clone()))
        .ok_or_else(|| {
            format!("Provider {provider_id}: \"baseUrl\" is required when defining custom models.")
        })?;

    if definition.context_window.is_some_and(|v| v <= 0.0) {
        return Err(format!(
            "Provider {provider_id}, model {}: invalid contextWindow",
            definition.id
        ));
    }
    if definition.max_tokens.is_some_and(|v| v <= 0.0) {
        return Err(format!(
            "Provider {provider_id}, model {}: invalid maxTokens",
            definition.id
        ));
    }

    Ok(Model {
        id: definition.id.clone(),
        name: definition
            .name
            .clone()
            .unwrap_or_else(|| definition.id.clone()),
        api: Api(api),
        provider: ProviderId(provider_id.to_string()),
        base_url,
        reasoning: definition.reasoning.unwrap_or(false),
        thinking_level_map: definition.thinking_level_map.clone(),
        input: definition
            .input
            .clone()
            .unwrap_or_else(|| vec![Modality::Text]),
        cost: definition.cost.clone().unwrap_or(ModelCost {
            rates: ModelCostRates {
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
            },
            tiers: None,
        }),
        context_window: definition.context_window.map_or(128_000, js_number_to_u64),
        max_tokens: definition.max_tokens.map_or(16_384, js_number_to_u64),
        // `headers: undefined` (`:156`) — unconditionally. See the doc comment.
        headers: None,
        compat: merge_compat(provider_config.compat.as_ref(), definition.compat.as_ref()),
    })
}

/// `applyModelsJson(providerId, baseModels, config)` (`provider-composer.ts:161-199`).
///
/// With no `models.json` entry the base list is returned untouched. Otherwise:
///
/// 1. `oauth` without `baseUrl` → `` `Provider {id}: "baseUrl" is required when "oauth" is
///    set.` ``
/// 2. The **"must specify something" guard** (`:171-184`): with no `models`, no `baseUrl`, no
///    `headers`, no `compat`, no non-empty `modelOverrides`, no `apiKey`, no `oauth` and
///    `authHeader === undefined`, the entry is rejected with
///    `` `Provider {id}: must specify "baseUrl", "headers", "compat", "modelOverrides", or
///    "models".` ``. Note the JS truthiness: `"headers": {}` and `"compat": {}` are objects and
///    therefore **satisfy** the guard, while `"models": []` does not (`!config.models?.length`)
///    and `"modelOverrides": {}` does not (`Object.keys(...).length > 0`).
/// 3. Every base model gets `baseUrl := config.baseUrl ?? model.baseUrl` — this is the whole
///    of the local-proxy override — and `compat := mergeCompat(model.compat, config.compat)`.
///    A `radius` provider keeps its own per-model `baseUrl` (`:188`).
/// 4. Each `models` entry is **upserted by id**: an existing id is replaced *in place*, so it
///    keeps its position in the list; a new id is appended. `defaults` is the model being
///    replaced, or `models[0]` for a new id (`:193`).
///
/// `modelOverrides` are *not* applied here — [`compose_model_provider`] applies them last.
pub fn apply_models_json(
    provider_id: &str,
    base_models: &[Model],
    config: Option<&ModelsJsonProvider>,
) -> Result<Vec<Model>, String> {
    let Some(config) = config else {
        return Ok(base_models.to_vec());
    };

    if config.oauth.is_some() && config.base_url.is_none() {
        return Err(format!(
            "Provider {provider_id}: \"baseUrl\" is required when \"oauth\" is set."
        ));
    }

    let has_overrides = config
        .model_overrides
        .as_ref()
        .is_some_and(|m| !m.is_empty());
    let has_models = config.models.as_ref().is_some_and(|m| !m.is_empty());
    if !has_models
        && config.base_url.is_none()
        && config.headers.is_none()
        && config.compat.is_none()
        && !has_overrides
        && config.api_key.is_none()
        && config.oauth.is_none()
        && config.auth_header.is_none()
    {
        return Err(format!(
            "Provider {provider_id}: must specify \"baseUrl\", \"headers\", \"compat\", \"modelOverrides\", or \"models\"."
        ));
    }

    let is_radius = config.oauth.as_deref() == Some("radius");
    let mut models: Vec<Model> = base_models
        .iter()
        .map(|model| Model {
            base_url: if is_radius {
                model.base_url.clone()
            } else {
                config
                    .base_url
                    .clone()
                    .unwrap_or_else(|| model.base_url.clone())
            },
            compat: merge_compat(model.compat.as_ref(), config.compat.as_ref()),
            ..model.clone()
        })
        .collect();

    for definition in config.models.iter().flatten() {
        let existing_index = models.iter().position(|model| model.id == definition.id);
        let defaults_index = existing_index.or(if models.is_empty() { None } else { Some(0) });
        let defaults = defaults_index.map(|i| models[i].clone());
        let model = model_from_json(provider_id, definition, config, defaults.as_ref())?;
        match existing_index {
            Some(index) => models[index] = model,
            None => models.push(model),
        }
    }

    Ok(models)
}

// =============================================================================
// provider-composer.ts:399-548 — provider composition
// =============================================================================

/// One entry of the builtin catalog — Pi's `Provider` (`packages/ai`), narrowed to what
/// composition and auth-probing read.
///
/// **This is an input.** See the module docs on why the 36-provider / 1062-model table is not
/// embedded here.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProviderDescriptor {
    /// `provider.id`.
    pub id: String,
    /// `provider.name` — the display name `--list-models` and `/model` show.
    pub name: String,
    /// `provider.baseUrl`.
    pub base_url: Option<String>,
    /// `provider.getModels()`.
    pub models: Vec<Model>,
    /// Env vars this provider's inherited api-key auth consults, in precedence order — for
    /// anthropic, [`pirust_ai::auth::ANTHROPIC_API_KEY_ENV_PRECEDENCE`]. Drives
    /// [`ModelRuntime::check_auth`]'s last resort.
    pub api_key_env: Vec<String>,
    /// Whether `provider.auth.apiKey` exists (`composeApiKeyAuth`'s `inherited`).
    pub has_api_key_auth: bool,
    /// Whether `provider.auth.oauth` exists.
    pub has_oauth_auth: bool,
}

/// The builtin catalog `ModelRuntime.create` is handed, in place of Pi's
/// `builtinProviderCatalog.builtinProviders()` (`model-runtime.ts:141-145`).
///
/// Order is preserved: it seeds `providerIds()`, which decides `getModels()`' order and
/// therefore `availableModels[0]` in step 4 of [`find_initial_model`].
#[derive(Debug, Clone, Default)]
pub struct ModelCatalog {
    providers: Vec<ProviderDescriptor>,
}

impl ModelCatalog {
    /// Build a catalog from an ordered provider list.
    pub fn new(providers: Vec<ProviderDescriptor>) -> Self {
        Self { providers }
    }

    /// Every builtin provider, in catalog order.
    pub fn providers(&self) -> &[ProviderDescriptor] {
        &self.providers
    }

    /// One builtin provider by id.
    pub fn get(&self, provider_id: &str) -> Option<&ProviderDescriptor> {
        self.providers.iter().find(|p| p.id == provider_id)
    }
}

/// The result of composing the builtin and `models.json` layers — Pi's returned `Provider`
/// object (`provider-composer.ts:468-498`), minus the closures feat-005 cannot use.
#[derive(Debug, Clone, PartialEq)]
pub struct ComposedProvider {
    /// `id: providerId`.
    pub id: String,
    /// `extension?.name ?? config?.name ?? base?.name ?? extension?.oauth?.name ?? providerId`
    /// (`:470`) — with no extensions, `config?.name ?? base?.name ?? providerId`.
    pub name: String,
    /// `extension?.baseUrl ?? config?.baseUrl ?? base?.baseUrl` (`:471`).
    pub base_url: Option<String>,
    /// `getModels()` — all layers applied, `modelOverrides` last.
    pub models: Vec<Model>,
}

/// `composeModelProvider(providerId, base, modelConfig, extension)`
/// (`provider-composer.ts:412-499`), without the extension layer.
///
/// `getModels()` is evaluated **eagerly** here, as Pi does at `:440` ("Validate eagerly so
/// registration/reload reports structural errors immediately"), so a broken `models.json`
/// entry surfaces at load rather than at first use.
///
/// The auth-method check (`:441-443`) is reproduced because its throw is a composition error:
/// `composeApiKeyAuth` returns nothing only when the base has no inherited api-key auth, no
/// `apiKey` is configured, **and** some oauth method exists (`:303`); if no oauth exists
/// either, an api-key login is fabricated. So the
/// `` `Provider {id}: no authentication method configured.` `` throw needs an oauth-only base
/// with no configured key — unreachable for the ported providers, kept for fidelity.
pub fn compose_model_provider(
    provider_id: &str,
    base: Option<&ProviderDescriptor>,
    model_config: &ModelConfig,
) -> Result<ComposedProvider, String> {
    let config = model_config.get_provider(provider_id);

    let base_models: &[Model] = base.map_or(&[], |b| b.models.as_slice());
    let models = apply_models_json(provider_id, base_models, config)?;
    // `applyExtension` with no extension is the identity (`provider-composer.ts:206`).
    let models: Vec<Model> = models
        .iter()
        .map(|model| {
            match config
                .and_then(|c| c.model_overrides.as_ref())
                .and_then(|overrides| overrides.get(&model.id))
            {
                Some(override_value) => apply_model_override(model, override_value),
                None => model.clone(),
            }
        })
        .collect();

    let has_oauth =
        config.is_some_and(|c| c.oauth.is_some()) || base.is_some_and(|b| b.has_oauth_auth);
    let inherited_api_key = base.is_some_and(|b| b.has_api_key_auth);
    let configured_api_key = config.and_then(|c| c.api_key.as_deref());
    let has_api_key = inherited_api_key || configured_api_key.is_some() || !has_oauth;
    if !has_api_key && !has_oauth {
        return Err(format!(
            "Provider {provider_id}: no authentication method configured."
        ));
    }

    Ok(ComposedProvider {
        id: provider_id.to_string(),
        name: config
            .and_then(|c| c.name.clone())
            .or_else(|| base.map(|b| b.name.clone()))
            .unwrap_or_else(|| provider_id.to_string()),
        base_url: config
            .and_then(|c| c.base_url.clone())
            .or_else(|| base.and_then(|b| b.base_url.clone())),
        models,
    })
}

/// `rawModelHeaders(model, config, extension)` (`provider-composer.ts:384-397`) —
/// `{...modelOverrides[id].headers, ...definition.headers, ...extensionModel.headers}`, or
/// `None` when that is empty.
fn raw_model_headers(
    model: &Model,
    config: Option<&ModelsJsonProvider>,
) -> Option<Map<String, Value>> {
    let config = config?;
    let mut headers = Map::new();
    if let Some(override_headers) = config
        .model_overrides
        .as_ref()
        .and_then(|o| o.get(&model.id))
        .and_then(|o| o.headers.as_ref())
    {
        for (k, v) in override_headers {
            headers.insert(k.clone(), v.clone());
        }
    }
    if let Some(definition_headers) = config
        .models
        .iter()
        .flatten()
        .find(|entry| entry.id == model.id)
        .and_then(|entry| entry.headers.as_ref())
    {
        for (k, v) in definition_headers {
            headers.insert(k.clone(), v.clone());
        }
    }
    (!headers.is_empty()).then_some(headers)
}

/// `resolveConfiguredModelHeaders(model, config, extension, env)`
/// (`provider-composer.ts:501-512`) — the **request-time** half of header handling.
///
/// This is why `model.headers` stays `null` through composition even for the Meridian shape
/// (`{"anthropic":{"baseUrl":…,"apiKey":"x","headers":{"x-meridian-agent":"pi"}}}`): the
/// provider-level `headers` never touch a model object, they are resolved per request. The
/// fixture asserts `headers: null` on all 14 composed anthropic models for exactly that shape,
/// so copying them onto the models would diverge.
///
/// Values go through `crate::auth::resolve_config_value`, so `$VAR` and `${VAR}` templates
/// work. Pi's `resolveHeadersOrThrow` throws when a referenced variable is unset; here an
/// unresolvable header value is **dropped** and reported by the returned `Vec<String>` of
/// names, because this module has no throw channel to a request that has not been built yet.
pub fn resolve_configured_model_headers(
    model: &Model,
    config: Option<&ModelsJsonProvider>,
    env: &ProcessEnv,
) -> (Option<Map<String, Value>>, Vec<String>) {
    let Some(raw) = raw_model_headers(model, config) else {
        return (None, Vec::new());
    };
    let mut resolved = Map::new();
    let mut unresolved = Vec::new();
    for (name, value) in &raw {
        let Some(template) = value.as_str() else {
            unresolved.push(name.clone());
            continue;
        };
        match crate::auth::resolve_config_value(template, None, env) {
            Some(v) => {
                resolved.insert(name.clone(), Value::String(v));
            }
            None => unresolved.push(name.clone()),
        }
    }
    ((!resolved.is_empty()).then_some(resolved), unresolved)
}

/// `AuthStatus` (`provider-composer.ts:70-74`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthStatus {
    /// Whether a credential is available at all.
    pub configured: bool,
    /// `"stored" | "runtime" | "environment" | "fallback" | "models_json_key" |
    /// "models_json_command"`.
    pub source: Option<String>,
    /// The env var names, joined with `", "`, when `source == "environment"`.
    pub label: Option<String>,
}

/// `configuredRequestAuthStatus(config, extension)` (`provider-composer.ts:534-548`).
///
/// `None` when no `apiKey` is configured at all. Otherwise: a `!command` is
/// `models_json_command`; a value referencing env vars is `environment` (with the names as the
/// label) when they all resolve and `{configured: false}` when they do not; a literal is
/// `models_json_key` — `fallback` in Pi is the extension-supplied case, unreachable here.
///
/// Unexercised by the fixture (nothing asserts `getProviderAuthStatus`), so it is ported for
/// completeness and flagged as unverified.
pub fn configured_request_auth_status(
    config: Option<&ModelsJsonProvider>,
    env: &ProcessEnv,
) -> Option<AuthStatus> {
    let value = config.and_then(|c| c.api_key.as_deref())?;
    if crate::auth::is_command_config_value(value) {
        return Some(AuthStatus {
            configured: true,
            source: Some("models_json_command".to_string()),
            label: None,
        });
    }
    let names = config_value_env_var_names(value);
    if !names.is_empty() {
        return Some(
            if crate::auth::resolve_config_value(value, None, env).is_some() {
                AuthStatus {
                    configured: true,
                    source: Some("environment".to_string()),
                    label: Some(names.join(", ")),
                }
            } else {
                AuthStatus {
                    configured: false,
                    source: None,
                    label: None,
                }
            },
        );
    }
    Some(AuthStatus {
        configured: true,
        source: Some("models_json_key".to_string()),
        label: None,
    })
}

/// `getConfigValueEnvVarNames(value)` (`core/resolve-config-value.ts:117-127`) — the `$VAR`
/// and `${VAR}` names a template references, in order, de-duplicated.
///
/// `crate::auth` owns the template *parser* but keeps it private, and `resolve_config_value`
/// only answers "did everything resolve". This re-scans for the names, which is the one piece
/// of that grammar duplicated here; it should move into `crate::auth` (as a `pub fn`) the next
/// time that module is touched, and this deleted.
fn config_value_env_var_names(config: &str) -> Vec<String> {
    if crate::auth::is_command_config_value(config) {
        return Vec::new();
    }
    let bytes = config.as_bytes();
    let mut names: Vec<String> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'$' {
            i += 1;
            continue;
        }
        match bytes.get(i + 1) {
            // `$$` → literal `$`, `$!` → literal `!` (`resolve-config-value.ts:42-46`).
            Some(b'$' | b'!') => i += 2,
            Some(b'{') => {
                let Some(close) = config[i + 2..].find('}') else {
                    i += 1;
                    continue;
                };
                let end = i + 2 + close;
                let name = &config[i + 2..end];
                if is_env_var_name(name) && !names.iter().any(|n| n == name) {
                    names.push(name.to_string());
                }
                i = end + 1;
            }
            _ => {
                let len = env_var_name_prefix_len(&config[i + 1..]);
                if len == 0 {
                    i += 1;
                    continue;
                }
                let name = &config[i + 1..i + 1 + len];
                if !names.iter().any(|n| n == name) {
                    names.push(name.to_string());
                }
                i += 1 + len;
            }
        }
    }
    names
}

/// `` ENV_VAR_NAME_RE = /^[A-Za-z_][A-Za-z0-9_]*$/ `` (`resolve-config-value.ts:11`).
fn is_env_var_name(name: &str) -> bool {
    env_var_name_prefix_len(name) == name.len() && !name.is_empty()
}

/// `` ENV_VAR_NAME_PREFIX_RE = /^[A-Za-z_][A-Za-z0-9_]*/ `` (`:12`) — the matched length, or 0.
fn env_var_name_prefix_len(rest: &str) -> usize {
    let bytes = rest.as_bytes();
    match bytes.first() {
        Some(&b) if b.is_ascii_alphabetic() || b == b'_' => {}
        _ => return 0,
    }
    bytes
        .iter()
        .take_while(|&&b| b.is_ascii_alphanumeric() || b == b'_')
        .count()
}

// =============================================================================
// model-runtime.ts:92-587 — ModelRuntime
// =============================================================================

/// `AuthCheck.type` (`packages/ai`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthKind {
    /// A plain API key, however it was sourced.
    ApiKey,
    /// An OAuth credential.
    OAuth,
}

/// `AuthCheck` (`packages/ai`) — the non-`undefined` return of `models.checkAuth(providerId)`,
/// which is precisely what puts a provider into `configuredProviders`
/// (`model-runtime.ts:249-253`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthCheck {
    /// `api_key` or `oauth`.
    pub kind: AuthKind,
    /// Human-readable provenance: `runtime API key`, `stored credential`,
    /// `configured API key`, or the env var name.
    pub source: String,
}

/// `CreateModelRuntimeOptions` (`model-runtime.ts:58-68`), narrowed to feat-005.
///
/// Dropped, with reasons: `credentials` / `authPath` (the credential *store* is
/// `crate::auth::AuthStorage`; only the set of provider ids it holds matters here, hence
/// `stored_credentials`), `modelsStore` / `modelsStorePath` (the dynamic catalog is only
/// written by a network refresh), `allowModelNetwork` / `modelRefreshTimeoutMs` /
/// `catalogBaseUrl` (no network in feat-005 — Pi's default is
/// `process.env.PI_OFFLINE === undefined`, i.e. **any** value, `"0"` included, disables it).
#[derive(Debug, Clone, Default)]
pub struct CreateModelRuntimeOptions<'a> {
    /// `join(getAgentDir(), "models.json")` in Pi (`:134`). `None` is Pi's `modelsPath: null`
    /// — no file is read and no store path is derived. `main.rs` passes
    /// `crate::config::ConfigEnv::models_path`.
    pub models_path: Option<&'a str>,
    /// Provider ids that have a credential in `auth.json` — `credentials.list()` mapped to
    /// `entry.providerId` (`:258`).
    pub stored_credentials: BTreeSet<String>,
    /// The environment the inherited api-key checks and `$VAR` templates read.
    pub process_env: ProcessEnv,
}

/// `ModelRuntime` (`model-runtime.ts:92-587`) — the composed provider set plus its
/// availability snapshot.
///
/// Synchronous throughout: every `await` in Pi's version exists for the network catalog
/// refresh, which feat-005 does not have (see [`ModelSource`]).
#[derive(Debug, Clone)]
pub struct ModelRuntime {
    catalog: ModelCatalog,
    config: ModelConfig,
    models_path: Option<String>,
    providers: Vec<ComposedProvider>,
    composition_errors: Vec<(String, String)>,
    all: Vec<Model>,
    available: Vec<Model>,
    configured_providers: Vec<String>,
    auth: HashMap<String, AuthCheck>,
    stored_credentials: BTreeSet<String>,
    runtime_api_keys: BTreeSet<String>,
    process_env: ProcessEnv,
}

impl ModelRuntime {
    /// `ModelRuntime.create(options)` (`model-runtime.ts:131-166`).
    ///
    /// Load `models.json`, compose every provider, then refresh availability. Pi additionally
    /// calls `configureRadiusProviders()` and `refresh({allowNetwork})` with a 15 s abort
    /// timer; neither has an effect without the radius flow or a network, so both are
    /// omitted — see the module docs.
    ///
    /// The builtin catalog is the first argument rather than a module-level import.
    pub fn create(
        env: &crate::config::ConfigEnv,
        catalog: ModelCatalog,
        options: CreateModelRuntimeOptions<'_>,
    ) -> Result<Self, crate::config::ConfigPathError> {
        let config = ModelConfig::load(env, options.models_path)?;
        let mut runtime = Self {
            catalog,
            config,
            models_path: options.models_path.map(str::to_string),
            providers: Vec::new(),
            composition_errors: Vec::new(),
            all: Vec::new(),
            available: Vec::new(),
            configured_providers: Vec::new(),
            auth: HashMap::new(),
            stored_credentials: options.stored_credentials,
            runtime_api_keys: BTreeSet::new(),
            process_env: options.process_env,
        };
        runtime.rebuild_providers();
        runtime.refresh_availability();
        Ok(runtime)
    }

    /// `reloadConfig()` (`model-runtime.ts:506-511`) — re-read `models.json`, recompose,
    /// refresh.
    pub fn reload_config(
        &mut self,
        env: &crate::config::ConfigEnv,
    ) -> Result<(), crate::config::ConfigPathError> {
        self.config = ModelConfig::load(env, self.models_path.as_deref())?;
        self.rebuild_providers();
        self.refresh_availability();
        Ok(())
    }

    /// `providerIds()` (`model-runtime.ts:185-192`) — `new Set([...builtins, ...config])`, so
    /// **builtin order first** and `models.json`-only providers appended in file order. Set
    /// semantics: a `models.json` entry for an existing builtin does not move it.
    pub fn provider_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = Vec::new();
        for provider in self.catalog.providers() {
            if !ids.contains(&provider.id) {
                ids.push(provider.id.clone());
            }
        }
        for id in self.config.provider_ids() {
            if !ids.contains(id) {
                ids.push(id.clone());
            }
        }
        ids
    }

    /// `rebuildProviders()` + `recomposeProvider(id)` (`model-runtime.ts:194-223`).
    ///
    /// The fast path at `:202-206` is reproduced exactly: a builtin with **no** `models.json`
    /// entry is used **untouched**, "so its auth/login/stream behavior is exact". Otherwise
    /// [`compose_model_provider`] runs and its error is captured into `compositionErrors`,
    /// after which the provider **falls back to the untouched builtin** — or is deleted when
    /// there is no builtin to fall back to (`:211-215`).
    ///
    /// That fallback is what makes `{"anthropic": {}}` a *reported error with all 14 builtin
    /// models still listed*, while `{"oracle-noapi": {...}}` is a reported error with the
    /// provider gone.
    fn rebuild_providers(&mut self) {
        self.providers.clear();
        self.composition_errors.clear();

        for provider_id in self.provider_ids() {
            let base = self.catalog.get(&provider_id).cloned();
            let has_config = self.config.get_provider(&provider_id).is_some();

            let Some(base) = base else {
                if !has_config {
                    // `deleteProvider` (`:198-200`).
                    continue;
                }
                match compose_model_provider(&provider_id, None, &self.config) {
                    Ok(composed) => self.providers.push(composed),
                    Err(error) => self.composition_errors.push((provider_id, error)),
                }
                continue;
            };

            if !has_config {
                self.providers.push(untouched(&base));
                continue;
            }

            match compose_model_provider(&provider_id, Some(&base), &self.config) {
                Ok(composed) => self.providers.push(composed),
                Err(error) => {
                    self.composition_errors.push((provider_id, error));
                    self.providers.push(untouched(&base));
                }
            }
        }

        self.update_model_snapshot();
    }

    /// `updateModelSnapshot()` (`model-runtime.ts:225-232`) — `all` is every provider's models
    /// concatenated in provider order; `available` filters it to `configuredProviders`.
    fn update_model_snapshot(&mut self) {
        self.all = self
            .providers
            .iter()
            .flat_map(|p| p.models.iter().cloned())
            .collect();
        self.available = self
            .all
            .iter()
            .filter(|m| self.configured_providers.contains(&m.provider.0))
            .cloned()
            .collect();
    }

    /// `runAvailabilityRefresh()` (`model-runtime.ts:234-262`) — `checkAuth` every composed
    /// provider, and the ones that answer become `configuredProviders`, **in provider order**
    /// (which is the order the fixture's `totals.configuredProviders` records).
    fn refresh_availability(&mut self) {
        let mut auth = HashMap::new();
        let mut configured = Vec::new();
        for provider_id in self
            .providers
            .iter()
            .map(|p| p.id.clone())
            .collect::<Vec<_>>()
        {
            if let Some(check) = self.check_auth(&provider_id) {
                auth.insert(provider_id.clone(), check);
                configured.push(provider_id);
            }
        }
        self.auth = auth;
        self.configured_providers = configured;
        self.update_model_snapshot();
    }

    /// `models.checkAuth(providerId)` for a composed provider — `composeApiKeyAuth`'s `check`
    /// (`provider-composer.ts:316-332`) plus the runtime-key shortcut
    /// (`model-runtime.ts:392-405`), in Pi's order:
    ///
    /// 1. a runtime key from `--api-key` → `runtime API key`;
    /// 2. a stored `auth.json` credential → `stored credential`. Pi delegates to the builtin's
    ///    own `check` when there is one, which for anthropic reports the same thing;
    /// 3. a configured `apiKey`: a `!command` counts as configured **without running it**
    ///    (`:322`); otherwise every `$VAR` it references must be defined → `configured API
    ///    key`;
    /// 4. the builtin's inherited env-var check → the first
    ///    [`ProviderDescriptor::api_key_env`] name that is set, reported as its own name.
    ///
    /// Divergences, both unexercised by the fixture (which was captured with **every** provider
    /// credential env var absent, and with literal `apiKey` values):
    ///
    /// - step 3 asks `crate::auth::resolve_config_value` whether the template resolves, which
    ///   treats an env var set to the **empty string** as unset; Pi's `check` only asks whether
    ///   it is *defined*. A `$VAR=""` would be "configured" in Pi and not here;
    /// - step 4's `source` string is the env var name; Pi's builtin providers choose their own
    ///   wording. Nothing asserts it.
    pub fn check_auth(&self, provider_id: &str) -> Option<AuthCheck> {
        if self.runtime_api_keys.contains(provider_id) {
            return Some(AuthCheck {
                kind: AuthKind::ApiKey,
                source: "runtime API key".to_string(),
            });
        }

        let base = self.catalog.get(provider_id);
        let config = self.config.get_provider(provider_id);

        if self.stored_credentials.contains(provider_id) {
            return Some(AuthCheck {
                kind: AuthKind::ApiKey,
                source: "stored credential".to_string(),
            });
        }

        if let Some(raw_key) = config.and_then(|c| c.api_key.as_deref()) {
            if crate::auth::is_command_config_value(raw_key) {
                return Some(AuthCheck {
                    kind: AuthKind::ApiKey,
                    source: "configured API key".to_string(),
                });
            }
            return crate::auth::resolve_config_value(raw_key, None, &self.process_env).map(|_| {
                AuthCheck {
                    kind: AuthKind::ApiKey,
                    source: "configured API key".to_string(),
                }
            });
        }

        base.and_then(|b| {
            b.api_key_env
                .iter()
                .find(|name| self.process_env.get(name).is_some_and(|v| !v.is_empty()))
                .map(|name| AuthCheck {
                    kind: AuthKind::ApiKey,
                    source: name.clone(),
                })
        })
    }

    /// `setRuntimeApiKey(providerId, apiKey)` (`model-runtime.ts:392-405`) — optimistically
    /// mark the provider configured, then refresh. The key itself lives in the credential
    /// store, not here; only its presence affects availability.
    pub fn set_runtime_api_key(&mut self, provider_id: &str) {
        self.runtime_api_keys.insert(provider_id.to_string());
        self.stored_credentials.insert(provider_id.to_string());
        self.refresh_availability();
    }

    /// `getProviders()` (`model-runtime.ts:287-289`).
    pub fn providers(&self) -> &[ComposedProvider] {
        &self.providers
    }

    /// `getProvider(providerId)` (`:291-293`).
    pub fn get_provider(&self, provider_id: &str) -> Option<&ComposedProvider> {
        self.providers.iter().find(|p| p.id == provider_id)
    }

    /// The `models.json` snapshot this runtime composed from.
    pub fn config(&self) -> &ModelConfig {
        &self.config
    }

    /// `compositionErrors` (`model-runtime.ts:99`), in provider order.
    pub fn composition_errors(&self) -> &[(String, String)] {
        &self.composition_errors
    }

    /// The availability snapshot's `configuredProviders` (`model-runtime.ts:250-253`), in
    /// provider order — which is the order the fixture's `totals.configuredProviders` records.
    pub fn configured_providers(&self) -> &[String] {
        &self.configured_providers
    }

    /// `isUsingOAuth(providerId)` (`:360-362`).
    pub fn is_using_oauth(&self, provider_id: &str) -> bool {
        self.auth
            .get(provider_id)
            .is_some_and(|check| check.kind == AuthKind::OAuth)
    }

    /// `getProviderAuthStatus(providerId)` (`model-runtime.ts:416-426`).
    pub fn get_provider_auth_status(&self, provider_id: &str) -> AuthStatus {
        if self.runtime_api_keys.contains(provider_id) {
            return AuthStatus {
                configured: true,
                source: Some("runtime".to_string()),
                label: None,
            };
        }
        if self.stored_credentials.contains(provider_id) {
            return AuthStatus {
                configured: true,
                source: Some("stored".to_string()),
                label: None,
            };
        }
        if let Some(configured) =
            configured_request_auth_status(self.config.get_provider(provider_id), &self.process_env)
        {
            return configured;
        }
        match self.auth.get(provider_id) {
            Some(check) => AuthStatus {
                configured: true,
                source: Some("environment".to_string()),
                label: Some(check.source.clone()),
            },
            None => AuthStatus {
                configured: false,
                source: None,
                label: None,
            },
        }
    }
}

/// `setProvider(base)` — the untouched-builtin fast path (`model-runtime.ts:204`).
fn untouched(base: &ProviderDescriptor) -> ComposedProvider {
    ComposedProvider {
        id: base.id.clone(),
        name: base.name.clone(),
        base_url: base.base_url.clone(),
        models: base.models.clone(),
    }
}

impl ModelSource for ModelRuntime {
    /// `getError()` (`model-runtime.ts:328-337`) — the config error, then every composition
    /// error as `` `Provider "{id}": {error}` ``, joined with `"\n\n"`. The availability error
    /// cannot occur without a network.
    fn get_error(&self) -> Option<String> {
        let mut errors: Vec<String> = Vec::new();
        if let Some(config_error) = self.config.get_error() {
            errors.push(config_error.to_string());
        }
        for (provider_id, error) in &self.composition_errors {
            errors.push(format!("Provider \"{provider_id}\": {error}"));
        }
        (!errors.is_empty()).then(|| errors.join("\n\n"))
    }

    fn get_models(&self) -> &[Model] {
        &self.all
    }

    fn get_available(&self) -> &[Model] {
        &self.available
    }

    fn get_model(&self, provider: &str, model_id: &str) -> Option<&Model> {
        self.all
            .iter()
            .find(|m| m.provider.0 == provider && m.id == model_id)
    }

    fn has_configured_auth(&self, provider: &str) -> bool {
        self.configured_providers.iter().any(|p| p == provider)
    }
}

// =============================================================================
// packages/tui/src/fuzzy.ts:12-137 — the filter behind `--list-models <pattern>`
// =============================================================================

/// `fuzzyMatch(query, text)` (`packages/tui/src/fuzzy.ts:12-93`) — subsequence match with a
/// **lower-is-better** score, or `None` when it does not match.
///
/// Transcribed rather than approximated (spec §9.6 expected an approximation): the algorithm
/// is small, deterministic and locale-free. Consecutive runs earn `-5 * runLength`
/// cumulatively, gaps cost `+2` per skipped character, a match at a word boundary
/// (`` /[\s\-_./:]/ `` before it, or index 0) earns `-10`, every match adds `+0.1 * index`,
/// and an exact whole-string match earns `-100`. On a miss, an `abc123` ⇄ `123abc` swap is
/// retried and, if it matches, scores `+5`.
///
/// `pirust-tui` will own this when feat-006 ports the picker; this copy should be deleted then.
pub fn fuzzy_match(query: &str, text: &str) -> Option<f64> {
    let query_lower = query.to_lowercase();
    let text_lower = text.to_lowercase();

    if let Some(score) = fuzzy_match_query(&query_lower, &text_lower) {
        return Some(score);
    }

    let swapped = swap_alpha_numeric(&query_lower)?;
    fuzzy_match_query(&swapped, &text_lower).map(|score| score + 5.0)
}

/// `matchQuery(normalizedQuery)` (`fuzzy.ts:16-68`).
fn fuzzy_match_query(query: &str, text: &str) -> Option<f64> {
    if query.is_empty() {
        return Some(0.0);
    }
    // `normalizedQuery.length > textLower.length` (`:21`) — UTF-16 units in JS; char counts
    // here. Only a shortcut, so the difference cannot change a match/no-match outcome.
    let query_chars: Vec<char> = query.chars().collect();
    let text_chars: Vec<char> = text.chars().collect();
    if query_chars.len() > text_chars.len() {
        return None;
    }

    let mut query_index = 0usize;
    let mut score = 0.0f64;
    let mut last_match_index: Option<usize> = None;
    let mut consecutive_matches = 0i64;

    for (i, ch) in text_chars.iter().enumerate() {
        if query_index >= query_chars.len() {
            break;
        }
        if *ch != query_chars[query_index] {
            continue;
        }
        let is_word_boundary = i == 0
            || matches!(
                text_chars[i - 1],
                ' ' | '\t' | '\n' | '\r' | '\u{b}' | '\u{c}' | '-' | '_' | '.' | '/' | ':'
            );

        if last_match_index == Some(i.wrapping_sub(1)) && i > 0 {
            consecutive_matches += 1;
            score -= consecutive_matches as f64 * 5.0;
        } else {
            consecutive_matches = 0;
            if let Some(last) = last_match_index {
                score += (i - last - 1) as f64 * 2.0;
            }
        }

        if is_word_boundary {
            score -= 10.0;
        }
        score += i as f64 * 0.1;

        last_match_index = Some(i);
        query_index += 1;
    }

    if query_index < query_chars.len() {
        return None;
    }
    if query == text {
        score -= 100.0;
    }
    Some(score)
}

/// `` /^(?<letters>[a-z]+)(?<digits>[0-9]+)$/ `` ⇄ `` /^(?<digits>[0-9]+)(?<letters>[a-z]+)$/ ``
/// (`fuzzy.ts:75-85`) — `gpt4` also tries `4gpt`, and vice versa.
fn swap_alpha_numeric(query: &str) -> Option<String> {
    let letters_then_digits = query.find(|c: char| c.is_ascii_digit()).filter(|split| {
        *split > 0
            && query[..*split].chars().all(|c| c.is_ascii_lowercase())
            && query[*split..].chars().all(|c| c.is_ascii_digit())
    });
    if let Some(split) = letters_then_digits {
        return Some(format!("{}{}", &query[split..], &query[..split]));
    }
    let digits_then_letters = query
        .find(|c: char| c.is_ascii_lowercase())
        .filter(|split| {
            *split > 0
                && query[..*split].chars().all(|c| c.is_ascii_digit())
                && query[*split..].chars().all(|c| c.is_ascii_lowercase())
        });
    digits_then_letters.map(|split| format!("{}{}", &query[split..], &query[..split]))
}

/// `fuzzyFilter(items, query, getText)` (`fuzzy.ts:99-137`).
///
/// A blank query returns everything. Otherwise the query is split on `` /[\s/]+/ `` and
/// **every** token must match; scores are summed and the survivors sorted ascending. The sort
/// is stable in both runtimes, so equal scores keep input order.
pub fn fuzzy_filter<'a, T, F>(items: &'a [T], query: &str, get_text: F) -> Vec<&'a T>
where
    F: Fn(&T) -> String,
{
    if query.trim().is_empty() {
        return items.iter().collect();
    }
    let tokens: Vec<&str> = query
        .trim()
        .split(|c: char| c.is_whitespace() || c == '/')
        .filter(|t| !t.is_empty())
        .collect();
    if tokens.is_empty() {
        return items.iter().collect();
    }

    let mut results: Vec<(&T, f64)> = Vec::new();
    for item in items {
        let text = get_text(item);
        let mut total_score = 0.0f64;
        let mut all_match = true;
        for token in &tokens {
            match fuzzy_match(token, &text) {
                Some(score) => total_score += score,
                None => {
                    all_match = false;
                    break;
                }
            }
        }
        if all_match {
            results.push((item, total_score));
        }
    }

    results.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(Ordering::Equal));
    results.into_iter().map(|(item, _)| item).collect()
}

// =============================================================================
// cli/list-models.ts:14-111 — the --list-models table
// =============================================================================

/// Which stream a line of `--list-models` output goes to. The distinction is load-bearing:
/// the `models.json` warning goes to **stderr** and everything else — including the
/// no-models message — to **stdout**.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputStream {
    /// `console.log`.
    Stdout,
    /// `console.error`.
    Stderr,
}

/// One `console.log`/`console.error` call, as a value.
///
/// Returned instead of written so the library never owns a stream; `main.rs` renders these,
/// which is also where `chalk`/`NO_COLOR` lives. `text` carries **no** ANSI codes — the
/// fixture was captured with `NO_COLOR=1`, so its `output` array is exactly these strings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsoleLine {
    /// Target stream.
    pub stream: OutputStream,
    /// The line, without its trailing newline.
    pub text: String,
}

/// The arguments of `listModels` that are not the runtime (`cli/list-models.ts:29`), plus the
/// two documentation paths `formatNoModelsAvailableMessage` needs.
#[derive(Debug, Clone, Copy, Default)]
pub struct ListModelsOptions<'a> {
    /// The optional fuzzy search pattern.
    pub search_pattern: Option<&'a str>,
    /// `join(getDocsPath(), "providers.md")`.
    pub providers_doc_path: &'a str,
    /// `join(getDocsPath(), "models.md")`.
    pub models_doc_path: &'a str,
}

/// `formatTokenCount(count)` (`cli/list-models.ts:14-24`).
///
/// `>= 1e6` → `` `{n}M` `` when `n` is integral, else `` `{n.toFixed(1)}M` ``; `>= 1e3` → the
/// same with `K`; otherwise the raw integer. So `1000000` → `1M`, `128000` → `128K`,
/// `8192` → `8.2K`, `16384` → `16.4K`, `900` → `900`.
///
/// The rounding is done in **integer** arithmetic, not `format!("{:.1}")`. `Number.prototype
/// .toFixed` rounds half **away from zero** on the exact value while Rust's formatter rounds
/// half to **even**, so a count of `1250` is `1.3K` in Pi and would be `1.2K` via `{:.1}`.
/// Since the value is always `count / 1000` or `count / 1_000_000`, `(count + 50) / 100` and
/// `(count + 50_000) / 100_000` give the tenths exactly, with no float involved at all.
pub fn format_token_count(count: u64) -> String {
    if count >= 1_000_000 {
        // `millions % 1 === 0` (`list-models.ts:17`).
        if count.is_multiple_of(1_000_000) {
            return format!("{}M", count / 1_000_000);
        }
        let tenths = (count + 50_000) / 100_000;
        return format!("{}.{}M", tenths / 10, tenths % 10);
    }
    if count >= 1_000 {
        // `thousands % 1 === 0` (`:21`).
        if count.is_multiple_of(1_000) {
            return format!("{}K", count / 1_000);
        }
        let tenths = (count + 50) / 100;
        return format!("{}.{}K", tenths / 10, tenths % 10);
    }
    count.to_string()
}

/// `getProviderLoginHelp()` (`core/auth-guidance.ts:6-12`).
///
/// The two paths are **parameters** because they come from `getDocsPath()`, which resolves
/// against Pi's *installed package directory* — explicitly deferred by spec §5.2, and a
/// property of the machine rather than of this logic. `main.rs` composes them (§14); the
/// golden test substitutes the fixture's `{PIPKG}\docs\…` values.
pub fn format_provider_login_help(providers_doc_path: &str, models_doc_path: &str) -> String {
    format!(
        "Use /login to log into a provider via OAuth or API key. See:\n  {providers_doc_path}\n  {models_doc_path}"
    )
}

/// `formatNoModelsAvailableMessage()` (`core/auth-guidance.ts:14-16`) — `No models available. `
/// then [`format_provider_login_help`], on **stdout**.
pub fn format_no_models_available_message(
    providers_doc_path: &str,
    models_doc_path: &str,
) -> String {
    format!(
        "No models available. {}",
        format_provider_login_help(providers_doc_path, models_doc_path)
    )
}

/// `listModels(modelRuntime, searchPattern)` (`cli/list-models.ts:29-111`).
///
/// Order of business:
///
/// 1. `getError()` non-empty → `` `Warning: errors loading models.json:\n{error}` `` on
///    **stderr**, *before* anything else. The table still renders from whatever loaded.
/// 2. `getAvailable()` empty → [`format_no_models_available_message`] on **stdout**, and
///    return. Note stdout, not stderr, and no non-zero exit.
/// 3. A search pattern → [`fuzzy_filter`] over `` `{provider} {id}` ``. No hits →
///    `` `No models matching "{pattern}"` `` on stdout, and return.
/// 4. Sort by `provider.localeCompare` then `id.localeCompare` (see [`locale_compare`]) — the
///    fuzzy filter's score order is therefore discarded.
/// 5. Six columns, each `padEnd`'d to `max(headerLen, max(cellLen))` and joined with **two
///    spaces**. The header row is padded too, and so is the last column, so data rows carry
///    **trailing spaces**. Widths come from the rows, so the whole table is data-dependent and
///    must be compared byte for byte.
pub fn list_models(source: &dyn ModelSource, options: ListModelsOptions<'_>) -> Vec<ConsoleLine> {
    let mut out: Vec<ConsoleLine> = Vec::new();

    if let Some(load_error) = source.get_error() {
        out.push(ConsoleLine {
            stream: OutputStream::Stderr,
            text: format!("Warning: errors loading models.json:\n{load_error}"),
        });
    }

    let models = source.get_available();
    if models.is_empty() {
        out.push(ConsoleLine {
            stream: OutputStream::Stdout,
            text: format_no_models_available_message(
                options.providers_doc_path,
                options.models_doc_path,
            ),
        });
        return out;
    }

    // `if (searchPattern)` — JS truthiness, so an empty pattern skips the filter entirely.
    let mut filtered: Vec<&Model> = match options.search_pattern.filter(|p| !p.is_empty()) {
        Some(pattern) => fuzzy_filter(models, pattern, |m| format!("{} {}", m.provider.0, m.id)),
        None => models.iter().collect(),
    };

    if filtered.is_empty() {
        let pattern = options.search_pattern.unwrap_or_default();
        out.push(ConsoleLine {
            stream: OutputStream::Stdout,
            text: format!("No models matching \"{pattern}\""),
        });
        return out;
    }

    filtered.sort_by(|a, b| {
        locale_compare(&a.provider.0, &b.provider.0).then_with(|| locale_compare(&a.id, &b.id))
    });

    let rows: Vec<[String; 6]> = filtered
        .iter()
        .map(|m| {
            [
                m.provider.0.clone(),
                m.id.clone(),
                format_token_count(m.context_window),
                format_token_count(m.max_tokens),
                if m.reasoning { "yes" } else { "no" }.to_string(),
                if m.input.contains(&Modality::Image) {
                    "yes"
                } else {
                    "no"
                }
                .to_string(),
            ]
        })
        .collect();

    let headers = [
        "provider", "model", "context", "max-out", "thinking", "images",
    ];
    let widths: Vec<usize> = (0..6)
        .map(|column| {
            rows.iter()
                .map(|row| js_length(&row[column]))
                .chain(std::iter::once(js_length(headers[column])))
                .max()
                .unwrap_or(0)
        })
        .collect();

    let join_row = |cells: [&str; 6]| -> String {
        cells
            .iter()
            .enumerate()
            .map(|(i, cell)| pad_end(cell, widths[i]))
            .collect::<Vec<String>>()
            .join("  ")
    };

    out.push(ConsoleLine {
        stream: OutputStream::Stdout,
        text: join_row(headers),
    });
    for row in &rows {
        let cells: [&str; 6] = [
            row[0].as_str(),
            row[1].as_str(),
            row[2].as_str(),
            row[3].as_str(),
            row[4].as_str(),
            row[5].as_str(),
        ];
        out.push(ConsoleLine {
            stream: OutputStream::Stdout,
            text: join_row(cells),
        });
    }

    out
}

/// `String.prototype.padEnd(width)` — pads with spaces, and **never truncates**.
fn pad_end(value: &str, width: usize) -> String {
    let len = js_length(value);
    if len >= width {
        return value.to_string();
    }
    format!("{value}{}", " ".repeat(width - len))
}

/// `String.prototype.length` — UTF-16 code units, not bytes and not chars. Every cell in the
/// captured tables is ASCII, but a non-BMP character in a model name would be 2 here and 1
/// under `chars().count()`, so the faithful measure is used.
fn js_length(value: &str) -> usize {
    value.encode_utf16().count()
}

// =============================================================================
// core/models-store.ts:1-57 — the dynamic provider-catalog store
// =============================================================================

/// `ModelsStoreEntry` (`packages/ai/src/models-store.ts:3-7`) — one provider's refreshed
/// catalog.
///
/// `checkedAt` is a [`Number`], not an `f64`: it round-trips a Unix timestamp as the integer
/// `1730000000` rather than re-emitting `1730000000.0`, which is the byte-compat rule
/// `settings.rs` established for every numeric field written back to disk.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelsStoreEntry {
    /// The provider's models, as fetched.
    pub models: Vec<Model>,
    /// Unix timestamp of the last completed remote check.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checked_at: Option<Number>,
}

/// Why a [`ModelsStore`] operation failed.
#[derive(Debug, thiserror::Error)]
pub enum ModelsStoreError {
    /// The locked read/write itself failed — `crate::auth`'s error, reused rather than
    /// redeclared, because the backend is `crate::auth::FileAuthStorageBackend`.
    #[error(transparent)]
    Storage(#[from] crate::auth::AuthError),
    /// `JSON.parse` / `JSON.stringify` failed. Pi throws the same way (`models-store.ts:33`).
    #[error("models-store.json is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
}

/// `ModelsStore` (`packages/ai/src/models-store.ts:10-14`) — persistent catalogs keyed by
/// provider id.
///
/// `&self` throughout, as Pi's async methods are; the in-memory implementation takes a `Mutex`
/// for it, exactly as `crate::auth::InMemoryAuthStorageBackend` does.
pub trait ModelsStore {
    /// `read(providerId)`.
    fn read(&self, provider_id: &str) -> Result<Option<ModelsStoreEntry>, ModelsStoreError>;
    /// `write(providerId, entry)`.
    fn write(&self, provider_id: &str, entry: &ModelsStoreEntry) -> Result<(), ModelsStoreError>;
    /// `delete(providerId)`.
    fn delete(&self, provider_id: &str) -> Result<(), ModelsStoreError>;
}

/// `InMemoryCodingAgentModelsStore` (`core/models-store.ts:8-22`) — the store used when there
/// is no `models.json` path to derive a file from.
///
/// Note the asymmetry with `pi-ai`'s own `InMemoryModelsStore`, which `structuredClone`s on
/// both read and write (`packages/ai/src/models-store.ts:26-33`): the coding-agent's version
/// clones on neither, handing out the stored object itself. Rust's `Clone` on read makes that
/// difference unobservable.
#[derive(Debug, Default)]
pub struct InMemoryCodingAgentModelsStore {
    entries: std::sync::Mutex<BTreeMap<String, ModelsStoreEntry>>,
}

impl InMemoryCodingAgentModelsStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

impl ModelsStore for InMemoryCodingAgentModelsStore {
    fn read(&self, provider_id: &str) -> Result<Option<ModelsStoreEntry>, ModelsStoreError> {
        Ok(self
            .entries
            .lock()
            .expect("models store mutex poisoned")
            .get(provider_id)
            .cloned())
    }

    fn write(&self, provider_id: &str, entry: &ModelsStoreEntry) -> Result<(), ModelsStoreError> {
        self.entries
            .lock()
            .expect("models store mutex poisoned")
            .insert(provider_id.to_string(), entry.clone());
        Ok(())
    }

    fn delete(&self, provider_id: &str) -> Result<(), ModelsStoreError> {
        self.entries
            .lock()
            .expect("models store mutex poisoned")
            .remove(provider_id);
        Ok(())
    }
}

/// `FileModelsStore` (`core/models-store.ts:25-57`) — "locked JSON-backed storage for
/// dynamically refreshed provider catalogs".
///
/// Pi builds it from `new FileAuthStorageBackend(path)` (`:29`), the *same* class that backs
/// `auth.json`, so `models-store.json` inherits its whole contract: a `<path>.lock` directory,
/// a `0700` parent, a `0600` file seeded with `{}`, and `JSON.stringify(x, null, 2)` on write.
/// [`crate::auth::FileAuthStorageBackend`] is therefore reused verbatim — its docs say it is
/// deliberately path-agnostic for exactly this — and no second locking or file-mode
/// implementation exists here.
///
/// The stored document is `Record<providerId, ModelsStoreEntry>`, held as a
/// [`Map`] so `preserve_order` keeps Pi's key order across a read-modify-write.
#[derive(Debug, Clone)]
pub struct FileModelsStore {
    storage: crate::auth::FileAuthStorageBackend,
}

impl FileModelsStore {
    /// The store at an **already-resolved** path — `join(getAgentDir(),
    /// "models-store.json")` in Pi (`models-store.ts:28`), or
    /// `join(dirname(modelsPath), "models-store.json")` when `ModelRuntime` derives it
    /// (`model-runtime.ts:139`). Path composition is the caller's, as with
    /// [`crate::auth::FileAuthStorageBackend::new`].
    pub fn new(path: impl Into<std::path::PathBuf>) -> Self {
        Self {
            storage: crate::auth::FileAuthStorageBackend::new(path),
        }
    }

    /// The file this store reads and writes.
    pub fn path(&self) -> &std::path::Path {
        self.storage.auth_path()
    }

    /// `parse(content)` (`models-store.ts:32-34`) — `content ? JSON.parse(content) : {}`, so
    /// an empty file is an empty document but malformed JSON throws.
    ///
    /// Returns [`crate::auth::AuthError`] rather than [`ModelsStoreError`] because it runs
    /// *inside* `with_lock`, whose callback error type is fixed by the backend. That is also
    /// why the failure is reported as `AuthError::Json` with this store's own path.
    fn parse(&self, content: Option<&str>) -> Result<Map<String, Value>, crate::auth::AuthError> {
        match content.filter(|c| !c.is_empty()) {
            None => Ok(Map::new()),
            Some(content) => {
                serde_json::from_str(content).map_err(|source| crate::auth::AuthError::Json {
                    path: self.path().display().to_string(),
                    source,
                })
            }
        }
    }
}

impl ModelsStore for FileModelsStore {
    fn read(&self, provider_id: &str) -> Result<Option<ModelsStoreEntry>, ModelsStoreError> {
        let raw = self.storage.with_lock(|content| {
            let parsed = self.parse(content)?;
            Ok(LockResult {
                result: parsed.get(provider_id).cloned(),
                next: None,
            })
        })?;
        match raw {
            None => Ok(None),
            Some(value) => Ok(Some(serde_json::from_value(value)?)),
        }
    }

    fn write(&self, provider_id: &str, entry: &ModelsStoreEntry) -> Result<(), ModelsStoreError> {
        let encoded = serde_json::to_value(entry)?;
        self.storage.with_lock(|content| {
            let mut current = self.parse(content)?;
            current.insert(provider_id.to_string(), encoded);
            Ok(LockResult {
                result: (),
                next: Some(stringify_pretty(&current)),
            })
        })?;
        Ok(())
    }

    fn delete(&self, provider_id: &str) -> Result<(), ModelsStoreError> {
        self.storage.with_lock(|content| {
            let mut current = self.parse(content)?;
            current.remove(provider_id);
            Ok(LockResult {
                result: (),
                next: Some(stringify_pretty(&current)),
            })
        })?;
        Ok(())
    }
}

/// `JSON.stringify(value, null, 2)` — two-space indent, which is `to_string_pretty`'s default.
///
/// # Panics
///
/// Never: the input is always a [`Map`] built from values that were themselves produced by
/// `serde_json`, so serialization cannot fail.
fn stringify_pretty(value: &Map<String, Value>) -> String {
    serde_json::to_string_pretty(value).expect("a serde_json::Map always serializes")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thinking_levels_agree_with_args() {
        for level in ["off", "minimal", "low", "medium", "high", "xhigh", "max"] {
            assert!(crate::args::is_valid_thinking_level(level));
            let parsed = thinking_level_from_str(level).expect("accepted by args");
            assert_eq!(thinking_level_as_str(parsed), level);
        }
        for level in ["", "OFF", "High", "none", "medium ", "thinking"] {
            assert_eq!(
                crate::args::is_valid_thinking_level(level),
                thinking_level_from_str(level).is_some(),
                "{level:?}"
            );
            assert!(thinking_level_from_str(level).is_none(), "{level:?}");
        }
    }

    #[test]
    fn default_model_per_provider_keeps_the_literals_key_order() {
        // The fixture's `constants.defaultModelPerProviderKeyOrder`, which step 4 of
        // findInitialModel scans in exactly this order.
        assert_eq!(DEFAULT_MODEL_PER_PROVIDER.len(), 40);
        assert_eq!(DEFAULT_MODEL_PER_PROVIDER[0].0, "amazon-bedrock");
        assert_eq!(DEFAULT_MODEL_PER_PROVIDER[1].0, "ant-ling");
        assert_eq!(DEFAULT_MODEL_PER_PROVIDER[2].0, "anthropic");
        assert_eq!(
            DEFAULT_MODEL_PER_PROVIDER[35].0,
            "qwen-token-plan-individual"
        );
        assert_eq!(DEFAULT_MODEL_PER_PROVIDER[39].0, "xiaomi-token-plan-sgp");
        // NOT alphabetical: `openai` precedes `azure-openai-responses`.
        assert_eq!(DEFAULT_MODEL_PER_PROVIDER[3].0, "openai");
        assert_eq!(
            default_model_for_provider("anthropic"),
            Some("claude-opus-4-8")
        );
        assert_eq!(default_model_for_provider("nope"), None);
    }

    #[test]
    fn is_alias_reads_the_date_suffix() {
        assert!(is_alias("claude-sonnet-4-5"));
        assert!(is_alias("claude-haiku-4-5-latest"));
        assert!(!is_alias("claude-sonnet-4-5-20250929"));
        // Nine chars is the minimum for `-` + 8 digits.
        assert!(!is_alias("-20250929"));
        assert!(is_alias("20250929"));
        assert!(is_alias("claude-2025092"));
        assert!(is_alias("claude-202509290"));
        // `-latest` wins even with a date in front of it.
        assert!(is_alias("claude-20250929-latest"));
    }

    #[test]
    fn locale_compare_is_icu_shaped_not_byte_shaped() {
        // Case: lowercase first at the tertiary level, unlike byte order.
        assert_eq!(locale_compare("a", "A"), Ordering::Less);
        assert!("A" < "a", "what plain str::cmp would have said");
        // …but a later primary difference outranks the case level, as ICU levels require.
        assert_eq!(locale_compare("Ab", "aa"), Ordering::Greater);
        // `_` leads ASCII punctuation in ICU; in bytes it sits above every digit.
        assert_eq!(locale_compare("a_", "a1"), Ordering::Less);
        assert!("a1" < "a_");
        // `:` sorts below digits in ICU, above them in bytes — reachable for OpenRouter ids.
        assert_eq!(locale_compare("a:", "a1"), Ordering::Less);
        assert!("a1" < "a:");
        // The real-catalog case that made this worth doing: two `defaultModelPerProvider`
        // ids that byte order and ICU order rank oppositely.
        assert_eq!(
            locale_compare("MiniMax-M2.7", "kimi-k2.6"),
            Ordering::Greater
        );
        assert!("MiniMax-M2.7" < "kimi-k2.6");
        // Prefixes still order shortest-first, and equality is equality.
        assert_eq!(locale_compare("claude", "claude-x"), Ordering::Less);
        assert_eq!(locale_compare("x", "x"), Ordering::Equal);
    }

    #[test]
    fn locale_compare_documented_gap_non_ascii_sorts_after_z() {
        // Real ICU folds `é` onto `e`, so `éclair` would precede `zebra`. This port does not
        // (no `unicode-normalization` dependency); asserted in the direction it actually goes
        // so an ICU4X swap fails here rather than silently.
        assert_eq!(locale_compare("\u{e9}clair", "zebra"), Ordering::Greater);
    }

    #[test]
    fn js_trim_is_not_rust_trim() {
        assert_eq!(js_trim("  x  "), "x");
        assert_eq!(js_trim("\u{feff}x"), "x", "JS trims the BOM, Rust does not");
        assert_eq!(
            js_trim("\u{85}x"),
            "\u{85}x",
            "JS does not trim NEL, Rust does"
        );
    }

    #[test]
    fn strip_json_comments_strips_only_what_pi_strips() {
        // `//` to end of line, newline preserved.
        assert_eq!(strip_json_comments("{\n // c\n}\n"), "{\n \n}\n");
        // Trailing commas before `}` and `]`, whitespace kept.
        assert_eq!(strip_json_comments("{\"a\":1,\n}"), "{\"a\":1\n}");
        assert_eq!(strip_json_comments("[1,2, ]"), "[1,2 ]");
        // String literals are untouched — including ones that look like comments.
        assert_eq!(
            strip_json_comments("{\"url\":\"http://x//y\"}"),
            "{\"url\":\"http://x//y\"}"
        );
        assert_eq!(
            strip_json_comments("{\"a\":\"x, \"}"),
            "{\"a\":\"x, \"}",
            "a comma inside a literal is not a trailing comma"
        );
        // Escapes inside literals do not end them.
        assert_eq!(
            strip_json_comments(r#"{"a":"q\"// not a comment"}"#),
            r#"{"a":"q\"// not a comment"}"#
        );
        // Block comments SURVIVE — which is what makes them a parse error, as the oracle
        // records for `json-with-comments-is-ACCEPTED`.
        assert_eq!(
            strip_json_comments("{/* c */}"),
            "{/* c */}",
            "stripJsonComments does not handle block comments"
        );
    }

    #[test]
    fn strip_json_comments_reproduces_the_captured_v8_position() {
        // The oracle recorded `line 4 column 5` (position 26) for this exact input, which only
        // lines up if the `//` text is deleted and its newline kept.
        let input = "{\n  // a line comment\n  \"providers\": {\n    /* a block comment */\n    \"anthropic\": { \"baseUrl\": \"http://127.0.0.1:3456\", \"apiKey\": \"x\" }\n  }\n}\n";
        let stripped = strip_json_comments(input);
        // V8 points at the offending token, i.e. the `/` that opens the surviving block comment.
        assert_eq!(stripped.as_bytes()[26], b'/');
        // Position 26 is byte 4 of line 4 (0-based), i.e. column 5 in V8's 1-based reporting.
        let line_4_start = stripped
            .char_indices()
            .filter(|(_, c)| *c == '\n')
            .nth(2)
            .expect("three newlines before line 4")
            .0
            + 1;
        assert_eq!(26 - line_4_start, 4);
    }

    #[test]
    fn minimatch_treats_slash_as_a_separator() {
        // `*` never crosses `/`, which is the whole reason the scope resolver tries both the
        // `provider/id` and the bare `id` form.
        assert!(minimatch_nocase("anthropic/claude-opus-4-8", "anthropic/*"));
        assert!(!minimatch_nocase("anthropic/claude-sonnet-4-5", "*sonnet*"));
        assert!(minimatch_nocase("claude-sonnet-4-5", "*sonnet*"));
        // nocase.
        assert!(minimatch_nocase("anthropic/claude-opus-4-8", "ANTHROPIC/*"));
        // `?` is exactly one character; `[...]` is a class, with ranges and negation.
        assert!(minimatch_nocase(
            "claude-haiku-4-5-latest",
            "claude-?aiku-4-5-latest"
        ));
        assert!(minimatch_nocase("claude-sonnet-4-5", "claude-[sh]*"));
        assert!(!minimatch_nocase("claude-opus-4-8", "claude-[sh]*"));
        assert!(minimatch_nocase("a1", "a[0-9]"));
        assert!(minimatch_nocase("ax", "a[!0-9]"));
        assert!(!minimatch_nocase("a1", "a[!0-9]"));
        // An invalid thinking suffix stays part of the glob and matches nothing.
        assert!(!minimatch_nocase(
            "anthropic/claude-opus-4-8",
            "anthropic/*:bogus"
        ));
        // `**` spans segments.
        assert!(minimatch_nocase("a/b/c", "a/**/c"));
        assert!(minimatch_nocase("a/c", "a/**/c"));
        // `dot: false`.
        assert!(!minimatch_nocase(".hidden", "*"));
        assert!(minimatch_nocase(".hidden", ".*"));
        // An unterminated `[` is a literal.
        assert!(minimatch_nocase("a[b", "a[b"));
        // Documented gap: brace expansion is not implemented, so this would match in Pi.
        assert!(
            !minimatch_nocase("ab", "*{b,c}"),
            "brace expansion is a documented gap"
        );
    }

    #[test]
    fn merge_compat_is_shallow_with_three_nested_exceptions() {
        let base = serde_json::json!({
            "supportsStore": true,
            "openRouterRouting": { "zdr": true, "order": ["a"] },
        });
        let over = serde_json::json!({
            "supportsStore": false,
            "openRouterRouting": { "order": ["b"] },
            "thinkingFormat": "openai",
        });
        let merged = merge_compat(Some(&base), Some(&over)).expect("merged");
        assert_eq!(merged["supportsStore"], serde_json::json!(false));
        assert_eq!(merged["thinkingFormat"], serde_json::json!("openai"));
        // Nested: `zdr` survives, `order` is replaced.
        assert_eq!(merged["openRouterRouting"]["zdr"], serde_json::json!(true));
        assert_eq!(
            merged["openRouterRouting"]["order"],
            serde_json::json!(["b"])
        );
        // Base key order is preserved, override's new keys append.
        let keys: Vec<&String> = merged.as_object().unwrap().keys().collect();
        assert_eq!(
            keys,
            ["supportsStore", "openRouterRouting", "thinkingFormat"]
        );
        // `override === undefined` returns the base untouched, undefined base included.
        assert_eq!(merge_compat(Some(&base), None), Some(base.clone()));
        assert_eq!(merge_compat(None, None), None);
    }

    #[test]
    fn config_value_env_var_names_reads_both_template_forms() {
        assert_eq!(config_value_env_var_names("literal"), Vec::<String>::new());
        assert_eq!(config_value_env_var_names("$FOO"), ["FOO"]);
        assert_eq!(config_value_env_var_names("${FOO}"), ["FOO"]);
        assert_eq!(config_value_env_var_names("a${FOO}b$BAR"), ["FOO", "BAR"]);
        assert_eq!(config_value_env_var_names("$FOO$FOO"), ["FOO"], "de-duped");
        assert_eq!(config_value_env_var_names("$$FOO"), Vec::<String>::new());
        assert_eq!(config_value_env_var_names("!echo hi"), Vec::<String>::new());
        assert_eq!(config_value_env_var_names("${1BAD}"), Vec::<String>::new());
    }

    #[test]
    fn fuzzy_filter_requires_every_token_to_match() {
        let items = ["oracle-local oracle-large", "oracle-local oracle-small"];
        assert_eq!(
            fuzzy_filter(&items, "large", |s| (*s).to_string()),
            [&items[0]]
        );
        // Both tokens must match.
        assert_eq!(
            fuzzy_filter(&items, "oracle large", |s| (*s).to_string()),
            [&items[0]]
        );
        assert!(fuzzy_filter(&items, "zzz", |s| (*s).to_string()).is_empty());
        // A blank query returns everything, untouched.
        assert_eq!(fuzzy_filter(&items, "   ", |s| (*s).to_string()).len(), 2);
        // `/` is a token separator, so `provider/model` searches work.
        assert_eq!(
            fuzzy_filter(&items, "oracle/large", |s| (*s).to_string()),
            [&items[0]]
        );
        // The alphanumeric swap retry (`gpt4` also tries `4gpt`).
        assert!(fuzzy_match("4gpt", "gpt4 turbo").is_some());
    }

    #[test]
    fn file_models_store_round_trips_under_a_lock() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(MODELS_STORE_FILE_NAME);
        let store = FileModelsStore::new(&path);

        assert_eq!(store.read("anthropic").expect("read"), None);

        let entry = ModelsStoreEntry {
            models: Vec::new(),
            checked_at: Some(Number::from(1_730_000_000_u64)),
        };
        store.write("anthropic", &entry).expect("write");
        assert_eq!(store.read("anthropic").expect("read"), Some(entry.clone()));

        // `JSON.stringify(x, null, 2)`, and an integer timestamp — not `1730000000.0`.
        let raw = std::fs::read_to_string(&path).expect("read the file");
        assert!(raw.contains("\"checkedAt\": 1730000000"), "{raw}");
        assert!(raw.starts_with("{\n  \"anthropic\": {"), "{raw}");

        // A second provider appends; the first keeps its position (`preserve_order`).
        store.write("openai", &entry).expect("write");
        let raw = std::fs::read_to_string(&path).expect("read the file");
        assert!(
            raw.find("\"anthropic\"") < raw.find("\"openai\""),
            "key order: {raw}"
        );

        store.delete("anthropic").expect("delete");
        assert_eq!(store.read("anthropic").expect("read"), None);
        assert!(store.read("openai").expect("read").is_some());

        // The lock directory is always released.
        assert!(!path.with_extension("json.lock").exists());
    }

    #[test]
    fn in_memory_models_store_is_the_same_contract() {
        let store = InMemoryCodingAgentModelsStore::new();
        let entry = ModelsStoreEntry {
            models: Vec::new(),
            checked_at: None,
        };
        assert_eq!(store.read("anthropic").expect("read"), None);
        store.write("anthropic", &entry).expect("write");
        assert_eq!(store.read("anthropic").expect("read"), Some(entry));
        store.delete("anthropic").expect("delete");
        assert_eq!(store.read("anthropic").expect("read"), None);
    }
}
