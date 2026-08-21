//! Oracle replay for `models.rs` against `tests/fixtures/pi/cli/models.cases.jsonl`.
//!
//! 158 records captured by executing **real Pi 0.80.10 → 0.84.2** — 111 pure-function rows
//! against a synthetic catalog the fixture itself supplies, 18 rows from a real `ModelRuntime`
//! against the real 40-provider / 1306-model builtin catalog, 6 `--list-models` renders, and
//! 3 metadata rows (`constants`, `syntheticCatalog`, `captureEnvironment`) that are *inputs*
//! rather than assertions.
//!
//! Rules this suite follows, deliberately:
//!
//! - **Nothing is asserted against a self-authored expectation.** Every `want` is read out of
//!   the fixture. Where Pi's value cannot exist in Rust (V8's `JSON.parse` wording) the
//!   envelope around it is still asserted literally and the divergence is named in the test.
//! - **The record count and the per-`fn` breakdown are asserted**, so a shrunken or re-ordered
//!   fixture fails loudly instead of silently testing less.
//! - **Failures are collected, not fatal.** Every group runs to completion and reports each
//!   mismatch as `case <n> [<fn>/<name>] <field>: want … got …`, so one bug does not hide
//!   nine others.
//! - **Nothing touches the real `~/.pirust` or `~/.pi`.** Every disk-backed case runs under
//!   `tempfile::TempDir`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use pirust_agent_core::types::ThinkingLevel;
use pirust_ai::types::Model;
use pirust_coding_agent::auth::ProcessEnv;
use pirust_coding_agent::config::{ConfigEnv, Platform, PIRUST};
use pirust_coding_agent::models::{
    find_exact_model_reference_match, find_initial_model, list_models, parse_model_pattern,
    resolve_cli_model, resolve_model_scope_with_diagnostics, CreateModelRuntimeOptions,
    FindInitialModelOptions, ListModelsOptions, ModelCatalog, ModelConfig, ModelRuntime,
    ModelSource, OutputStream, ParseModelPatternOptions, ProviderDescriptor,
    ResolveCliModelOptions, ScopedModel, StaticModelSource, DEFAULT_MODEL_PER_PROVIDER,
    DEFAULT_THINKING_LEVEL,
};
use serde_json::Value;

// =============================================================================
// Fixture plumbing
// =============================================================================

/// House style: `crates/pirust-agent-core/tests/session_golden.rs`.
fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/pi/cli/models.cases.jsonl")
}

/// One fixture line, with its 1-based line number for failure messages.
struct Case {
    /// 1-based line number, matching what an editor shows.
    index: usize,
    value: Value,
}

impl Case {
    fn kind(&self) -> &str {
        self.value["fn"].as_str().unwrap_or_default()
    }

    fn name(&self) -> &str {
        self.value["name"].as_str().unwrap_or_default()
    }

    /// `case 42 [parseModelPattern/…]` — the prefix every failure carries.
    fn label(&self) -> String {
        let name = self.name();
        if name.is_empty() {
            format!("case {} [{}]", self.index, self.kind())
        } else {
            format!("case {} [{}/{}]", self.index, self.kind(), name)
        }
    }

    fn get(&self, key: &str) -> &Value {
        self.value.get(key).unwrap_or(&Value::Null)
    }

    fn str_field(&self, key: &str) -> &str {
        self.get(key).as_str().unwrap_or_default()
    }
}

fn load_cases() -> Vec<Case> {
    let path = fixture_path();
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read fixture {}: {e}", path.display()));
    raw.lines()
        .filter(|line| !line.trim().is_empty())
        .enumerate()
        .map(|(i, line)| Case {
            index: i + 1,
            value: serde_json::from_str(line)
                .unwrap_or_else(|e| panic!("line {}: not JSON: {e}", i + 1)),
        })
        .collect()
}

fn cases_for(kind: &str) -> Vec<Case> {
    load_cases()
        .into_iter()
        .filter(|case| case.kind() == kind)
        .collect()
}

/// Collect-then-report, so one broken case does not mask the rest.
#[derive(Default)]
struct Failures {
    entries: Vec<String>,
    checked: usize,
}

impl Failures {
    fn check_value(&mut self, label: &str, field: &str, want: &Value, got: &Value) {
        self.checked += 1;
        if want != got {
            self.entries
                .push(format!("{label} {field}:\n    want {want}\n    got  {got}"));
        }
    }

    fn check_str(&mut self, label: &str, field: &str, want: &str, got: &str) {
        self.checked += 1;
        if want != got {
            self.entries.push(format!(
                "{label} {field}:\n    want {want:?}\n    got  {got:?}"
            ));
        }
    }

    fn check_usize(&mut self, label: &str, field: &str, want: usize, got: usize) {
        self.checked += 1;
        if want != got {
            self.entries
                .push(format!("{label} {field}: want {want} got {got}"));
        }
    }

    fn fail(&mut self, label: &str, message: String) {
        self.checked += 1;
        self.entries.push(format!("{label} {message}"));
    }

    /// A `null`-tolerant string comparison: the fixture writes `null` for "absent".
    fn check_opt_str(&mut self, label: &str, field: &str, want: &Value, got: Option<&str>) {
        let got_value = got.map_or(Value::Null, |s| Value::String(s.to_string()));
        self.check_value(label, field, want, &got_value);
    }

    fn finish(self, what: &str) {
        assert!(
            self.checked > 0,
            "{what}: no assertions ran — the fixture group is empty"
        );
        if !self.entries.is_empty() {
            panic!(
                "{what}: {} of {} assertions failed\n\n{}\n",
                self.entries.len(),
                self.checked,
                self.entries.join("\n")
            );
        }
    }
}

// =============================================================================
// Model comparison
// =============================================================================

/// A `Model` as JSON, normalized through the *serializer* rather than `to_value`.
///
/// `pirust_ai`'s cost/rate fields serialize through `jsnum::serialize_f64`, which writes a raw
/// JSON number; going via a string and back guarantees `1.0` is compared as the integer `1`,
/// exactly as `JSON.stringify` wrote it, instead of as a distinct `serde_json::Number` repr.
fn model_to_json(model: &Model) -> Value {
    let encoded = serde_json::to_string(model).expect("a Model always serializes");
    serde_json::from_str(&encoded).expect("its own output is JSON")
}

/// Compare a recorded model object against an actual model.
///
/// The fixture's model objects are **partial**: the pure-function records omit `cost`
/// entirely, and the runtime records spell absent fields as an explicit `null` (`headers`,
/// `compat`, `thinkingLevelMap`) where the Rust serializer omits the key. So every recorded
/// key must match, a missing actual key counts as `null`, and keys the record does not mention
/// are not checked — there is nothing to check them against.
fn check_model(
    failures: &mut Failures,
    label: &str,
    field: &str,
    want: &Value,
    got: Option<&Model>,
) {
    match (want, got) {
        (Value::Null, None) => {
            failures.checked += 1;
        }
        (Value::Null, Some(model)) => failures.fail(
            label,
            format!("{field}: want null got {}/{}", model.provider.0, model.id),
        ),
        (_, None) => failures.fail(label, format!("{field}: want {want} got null")),
        (Value::Object(expected), Some(model)) => {
            let actual = model_to_json(model);
            for (key, want_value) in expected {
                let got_value = actual.get(key).cloned().unwrap_or(Value::Null);
                failures.check_value(label, &format!("{field}.{key}"), want_value, &got_value);
            }
        }
        _ => failures.fail(label, format!("{field}: unexpected recorded shape {want}")),
    }
}

// =============================================================================
// The shared inputs: `constants` and `syntheticCatalog`
// =============================================================================

/// The `syntheticCatalog` record — "the exact model list every `catalogSource: "synthetic"`
/// record below ran against. Rebuild this verbatim."
fn synthetic_catalog() -> Vec<Model> {
    let case = cases_for("syntheticCatalog")
        .into_iter()
        .next()
        .expect("the fixture carries exactly one syntheticCatalog record");
    case.get("models")
        .as_array()
        .expect("syntheticCatalog.models is an array")
        .iter()
        .map(|value| {
            serde_json::from_value::<Model>(value.clone()).unwrap_or_else(|e| {
                panic!("syntheticCatalog model does not fit pirust_ai::Model: {e}\n  {value}")
            })
        })
        .collect()
}

/// Resolve a `"provider/id"` reference (the fixture's compact model form) against a catalog.
/// The provider is the part before the **first** slash, so `openrouter/openai/gpt-4o` works.
fn resolve_ref<'a>(reference: &str, catalog: &'a [Model]) -> &'a Model {
    let (provider, id) = reference
        .split_once('/')
        .unwrap_or_else(|| panic!("model reference {reference:?} has no provider prefix"));
    catalog
        .iter()
        .find(|m| m.provider.0 == provider && m.id == id)
        .unwrap_or_else(|| panic!("model reference {reference:?} is not in the synthetic catalog"))
}

fn thinking_from_value(value: &Value) -> Option<ThinkingLevel> {
    let level = value.as_str()?;
    Some(match level {
        "off" => ThinkingLevel::Off,
        "minimal" => ThinkingLevel::Minimal,
        "low" => ThinkingLevel::Low,
        "medium" => ThinkingLevel::Medium,
        "high" => ThinkingLevel::High,
        "xhigh" => ThinkingLevel::Xhigh,
        "max" => ThinkingLevel::Max,
        other => panic!("fixture names an unknown thinking level {other:?}"),
    })
}

fn thinking_to_value(level: Option<ThinkingLevel>) -> Value {
    match level {
        None => Value::Null,
        Some(level) => {
            Value::String(pirust_coding_agent::models::thinking_level_as_str(level).to_string())
        }
    }
}

// =============================================================================
// Fixture integrity
// =============================================================================

/// The fixture must not shrink, and no `fn` group may lose rows, or every other test in this
/// file would quietly assert less than it claims.
#[test]
fn fixture_has_all_158_records_in_the_recorded_proportions() {
    let cases = load_cases();
    assert_eq!(cases.len(), 158, "fixture record count");

    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for case in &cases {
        *counts.entry(case.kind()).or_default() += 1;
    }

    let expected: BTreeMap<&str, usize> = [
        ("ModelConfig.load", 19),
        ("ModelRuntime.create", 18),
        ("builtinCatalogFingerprint", 1),
        ("captureEnvironment", 1),
        ("constants", 1),
        ("findExactModelReferenceMatch", 19),
        ("findInitialModel", 17),
        ("listModels", 6),
        ("parseModelPattern", 35),
        ("resolveCliModel", 26),
        ("resolveModelScopeWithDiagnostics", 14),
        ("syntheticCatalog", 1),
    ]
    .into_iter()
    .collect();

    assert_eq!(counts, expected, "per-`fn` record counts");
    assert_eq!(
        expected.values().sum::<usize>(),
        158,
        "the expected breakdown must itself add up"
    );
}

/// The synthetic catalog is an *input* to 111 records; if it stops deserializing into
/// `pirust_ai::types::Model`, or changes shape, those records are no longer replaying what Pi
/// ran against.
#[test]
fn synthetic_catalog_round_trips_into_the_shared_model_type() {
    let catalog = synthetic_catalog();
    assert_eq!(catalog.len(), 15, "synthetic catalog size");
    // Spot-check the four models whose relationships the tie-break records depend on.
    assert!(catalog.iter().any(|m| m.id == "claude-sonnet-4-5"));
    assert!(catalog.iter().any(|m| m.id == "claude-sonnet-4-5-20250929"));
    assert!(catalog.iter().any(|m| m.id == "openai/gpt-4o:extended"));
    // `shared-model-id` is deliberately duplicated across two providers.
    assert_eq!(
        catalog.iter().filter(|m| m.id == "shared-model-id").count(),
        2
    );

    // Every entry must survive a serialize → parse round trip against the recorded object.
    let case = cases_for("syntheticCatalog").into_iter().next().unwrap();
    let recorded = case.get("models").as_array().unwrap().clone();
    let mut failures = Failures::default();
    for (model, want) in catalog.iter().zip(recorded.iter()) {
        check_model(
            &mut failures,
            &format!("syntheticCatalog[{}]", model.id),
            "model",
            want,
            Some(model),
        );
    }
    failures.finish("syntheticCatalog");
}

/// The `constants` record pins `DEFAULT_THINKING_LEVEL` and — critically — the **key order**
/// of `defaultModelPerProvider`, which decides step 4 of `findInitialModel`.
#[test]
fn constants_match_the_recorded_defaults() {
    let case = cases_for("constants")
        .into_iter()
        .next()
        .expect("one constants record");
    let mut failures = Failures::default();
    let label = case.label();

    failures.check_str(
        &label,
        "DEFAULT_THINKING_LEVEL",
        case.str_field("DEFAULT_THINKING_LEVEL"),
        pirust_coding_agent::models::thinking_level_as_str(DEFAULT_THINKING_LEVEL),
    );

    let want_order: Vec<&str> = case
        .get("defaultModelPerProviderKeyOrder")
        .as_array()
        .expect("defaultModelPerProviderKeyOrder is an array")
        .iter()
        .map(|v| v.as_str().expect("provider ids are strings"))
        .collect();
    let got_order: Vec<&str> = DEFAULT_MODEL_PER_PROVIDER
        .iter()
        .map(|(id, _)| *id)
        .collect();
    failures.check_usize(&label, "keyOrder.len", want_order.len(), got_order.len());
    for (position, want) in want_order.iter().enumerate() {
        let got = got_order.get(position).copied().unwrap_or_default();
        failures.check_str(&label, &format!("keyOrder[{position}]"), want, got);
    }

    let want_map = case
        .get("defaultModelPerProvider")
        .as_object()
        .expect("defaultModelPerProvider is an object");
    failures.check_usize(
        &label,
        "defaultModelPerProvider.len",
        want_map.len(),
        DEFAULT_MODEL_PER_PROVIDER.len(),
    );
    for (provider, want_model) in want_map {
        let got = pirust_coding_agent::models::default_model_for_provider(provider);
        failures.check_opt_str(&label, &format!("default[{provider}]"), want_model, got);
    }

    failures.finish("constants");
}

// =============================================================================
// findExactModelReferenceMatch — 19 records
// =============================================================================

#[test]
fn find_exact_model_reference_match_matches_all_19_records() {
    let catalog = synthetic_catalog();
    let mut failures = Failures::default();

    for case in cases_for("findExactModelReferenceMatch") {
        let label = case.label();
        let input = case.str_field("input");
        let got = find_exact_model_reference_match(input, &catalog);
        check_model(
            &mut failures,
            &format!("{label} input={input:?}"),
            "result",
            case.get("result"),
            got,
        );
    }

    failures.finish("findExactModelReferenceMatch");
}

// =============================================================================
// parseModelPattern — 35 records
// =============================================================================

#[test]
fn parse_model_pattern_matches_all_35_records() {
    let catalog = synthetic_catalog();
    let mut failures = Failures::default();

    for case in cases_for("parseModelPattern") {
        let label = case.label();
        let input = case.str_field("input");
        let options = match case.get("options").get("allowInvalidThinkingLevelFallback") {
            Some(Value::Bool(false)) => ParseModelPatternOptions::STRICT,
            // `options: null` is Pi's absent argument, whose default is `true`.
            _ => ParseModelPatternOptions::LENIENT,
        };

        let got = parse_model_pattern(input, &catalog, options);
        let result = case.get("result");
        let scoped_label = format!(
            "{label} input={input:?} strict={}",
            options == ParseModelPatternOptions::STRICT
        );

        check_model(
            &mut failures,
            &scoped_label,
            "result.model",
            result.get("model").unwrap_or(&Value::Null),
            got.model.as_ref(),
        );
        failures.check_value(
            &scoped_label,
            "result.thinkingLevel",
            result.get("thinkingLevel").unwrap_or(&Value::Null),
            &thinking_to_value(got.thinking_level),
        );
        failures.check_opt_str(
            &scoped_label,
            "result.warning",
            result.get("warning").unwrap_or(&Value::Null),
            got.warning.as_deref(),
        );
    }

    failures.finish("parseModelPattern");
}

// =============================================================================
// resolveCliModel — 26 records
// =============================================================================

/// Build the [`StaticModelSource`] a record describes.
///
/// `configuredProviders` is either the string `"ALL"` or an explicit list. Two readings of an
/// **empty** list were possible; the fixture settles it:
///
/// - it does **not** filter `getModels()`. Record 80 (`configuredProviders: ["anthropic"]`)
///   still resolves `xiaomi/mimo-v2.5-pro` through provider inference, which is only possible
///   if xiaomi's models are in `getModels()`.
/// - so record 82 (`empty-catalog-is-an-ERROR`, `configuredProviders: []`) can only produce
///   `No models available.` if the *catalog itself* was empty. An empty list therefore means
///   "no models and no configured providers", which is exactly what that one record needs and
///   what no other record contradicts.
fn source_for(case: &Case, catalog: &[Model]) -> StaticModelSource {
    let configured = case.get("configuredProviders");
    let (models, providers): (Vec<Model>, BTreeSet<String>) = match configured {
        Value::String(all) if all == "ALL" => (
            catalog.to_vec(),
            catalog.iter().map(|m| m.provider.0.clone()).collect(),
        ),
        Value::Array(list) if list.is_empty() => (Vec::new(), BTreeSet::new()),
        Value::Array(list) => (
            catalog.to_vec(),
            list.iter()
                .map(|v| {
                    v.as_str()
                        .expect("configuredProviders entries are strings")
                        .to_string()
                })
                .collect(),
        ),
        // No `configuredProviders` key at all (the scope records) — everything is available.
        Value::Null => (
            catalog.to_vec(),
            catalog.iter().map(|m| m.provider.0.clone()).collect(),
        ),
        other => panic!("unexpected configuredProviders shape: {other}"),
    };

    let mut source = StaticModelSource::new(models, providers);
    if let Value::Array(refs) = case.get("availableOverride") {
        source = source.with_available(
            refs.iter()
                .map(|v| {
                    resolve_ref(
                        v.as_str().expect("availableOverride entries are strings"),
                        catalog,
                    )
                    .clone()
                })
                .collect(),
        );
    }
    source
}

#[test]
fn resolve_cli_model_matches_all_26_records() {
    let catalog = synthetic_catalog();
    let mut failures = Failures::default();

    for case in cases_for("resolveCliModel") {
        let label = case.label();
        let source = source_for(&case, &catalog);
        let input = case.get("input");

        let got = resolve_cli_model(
            ResolveCliModelOptions {
                cli_provider: input.get("cliProvider").and_then(Value::as_str),
                cli_model: input.get("cliModel").and_then(Value::as_str),
                cli_thinking: thinking_from_value(input.get("cliThinking").unwrap_or(&Value::Null)),
            },
            &source,
        );

        let result = case.get("result");
        check_model(
            &mut failures,
            &label,
            "result.model",
            result.get("model").unwrap_or(&Value::Null),
            got.model.as_ref(),
        );
        failures.check_value(
            &label,
            "result.thinkingLevel",
            result.get("thinkingLevel").unwrap_or(&Value::Null),
            &thinking_to_value(got.thinking_level),
        );
        failures.check_opt_str(
            &label,
            "result.warning",
            result.get("warning").unwrap_or(&Value::Null),
            got.warning.as_deref(),
        );
        failures.check_opt_str(
            &label,
            "result.error",
            result.get("error").unwrap_or(&Value::Null),
            got.error.as_deref(),
        );
    }

    failures.finish("resolveCliModel");
}

// =============================================================================
// findInitialModel — 17 records
// =============================================================================

#[test]
fn find_initial_model_matches_all_17_records() {
    let catalog = synthetic_catalog();
    let mut failures = Failures::default();

    for case in cases_for("findInitialModel") {
        let label = case.label();
        let source = source_for(&case, &catalog);
        let input = case.get("input");

        let scoped_models: Vec<ScopedModel> = input
            .get("scopedModels")
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .map(|entry| ScopedModel {
                        model: resolve_ref(
                            entry["model"].as_str().expect("scoped model reference"),
                            &catalog,
                        )
                        .clone(),
                        thinking_level: thinking_from_value(
                            entry.get("thinkingLevel").unwrap_or(&Value::Null),
                        ),
                    })
                    .collect()
            })
            .unwrap_or_default();

        let outcome = find_initial_model(
            FindInitialModelOptions {
                cli_provider: input.get("cliProvider").and_then(Value::as_str),
                cli_model: input.get("cliModel").and_then(Value::as_str),
                scoped_models: &scoped_models,
                is_continuing: input
                    .get("isContinuing")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                default_provider: input.get("defaultProvider").and_then(Value::as_str),
                default_model_id: input.get("defaultModelId").and_then(Value::as_str),
                default_thinking_level: thinking_from_value(
                    input.get("defaultThinkingLevel").unwrap_or(&Value::Null),
                ),
            },
            &source,
        );

        // Every record has `exitCode: null` and `console: []`, i.e. Pi never reached its
        // `process.exit(1)`. Assert that, so a port that started erroring here is caught.
        failures.check_value(&label, "exitCode", case.get("exitCode"), &Value::Null);
        failures.check_value(
            &label,
            "console",
            case.get("console"),
            &Value::Array(Vec::new()),
        );

        let result = case.get("result");
        match outcome {
            Err(error) => failures.fail(
                &label,
                format!("resolveCliModel errored where Pi did not exit: {error}"),
            ),
            Ok(initial) => {
                check_model(
                    &mut failures,
                    &label,
                    "result.model",
                    result.get("model").unwrap_or(&Value::Null),
                    initial.model.as_ref(),
                );
                failures.check_value(
                    &label,
                    "result.thinkingLevel",
                    result.get("thinkingLevel").unwrap_or(&Value::Null),
                    &thinking_to_value(Some(initial.thinking_level)),
                );
                failures.check_opt_str(
                    &label,
                    "result.fallbackMessage",
                    result.get("fallbackMessage").unwrap_or(&Value::Null),
                    initial.fallback_message.as_deref(),
                );
            }
        }
    }

    failures.finish("findInitialModel");
}

/// Step 1 **discards** the thinking level `resolveCliModel` parsed. Pinned on its own so
/// mutation (b) — "keep the parsed thinking level in step 1" — fails here by name.
#[test]
fn find_initial_model_step1_discards_the_parsed_thinking_level() {
    let catalog = synthetic_catalog();
    let source = StaticModelSource::new(
        catalog.clone(),
        catalog.iter().map(|m| m.provider.0.clone()).collect(),
    );
    // Record `step1:cli-pair-wins`: `--provider anthropic --model claude-sonnet-4-5:high`,
    // with `defaultThinkingLevel: "low"` in settings, still starts at `medium`.
    let initial = find_initial_model(
        FindInitialModelOptions {
            cli_provider: Some("anthropic"),
            cli_model: Some("claude-sonnet-4-5:high"),
            scoped_models: &[],
            is_continuing: false,
            default_provider: None,
            default_model_id: None,
            default_thinking_level: Some(ThinkingLevel::Low),
        },
        &source,
    )
    .expect("resolvable");
    assert_eq!(
        initial.model.map(|m| m.id),
        Some("claude-sonnet-4-5".into())
    );
    assert_eq!(initial.thinking_level, ThinkingLevel::Medium);
    assert_eq!(initial.thinking_level, DEFAULT_THINKING_LEVEL);
}

// =============================================================================
// resolveModelScopeWithDiagnostics — 14 records
// =============================================================================

#[test]
fn resolve_model_scope_with_diagnostics_matches_all_14_records() {
    let catalog = synthetic_catalog();
    let mut failures = Failures::default();

    for case in cases_for("resolveModelScopeWithDiagnostics") {
        let label = case.label();
        let source = source_for(&case, &catalog);
        let patterns: Vec<String> = case
            .get("input")
            .as_array()
            .expect("scope input is an array of patterns")
            .iter()
            .map(|v| v.as_str().expect("patterns are strings").to_string())
            .collect();

        let got = resolve_model_scope_with_diagnostics(&patterns, &source);
        let result = case.get("result");

        let want_scoped = result
            .get("scopedModels")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        failures.check_usize(
            &label,
            "scopedModels.len",
            want_scoped.len(),
            got.scoped_models.len(),
        );
        for (position, want) in want_scoped.iter().enumerate() {
            let field = format!("scopedModels[{position}]");
            match got.scoped_models.get(position) {
                None => failures.fail(&label, format!("{field}: missing, want {want}")),
                Some(scoped) => {
                    // The fixture stores the model as a compact `provider/id` reference.
                    failures.check_str(
                        &label,
                        &format!("{field}.model"),
                        want["model"].as_str().unwrap_or_default(),
                        &format!("{}/{}", scoped.model.provider.0, scoped.model.id),
                    );
                    failures.check_value(
                        &label,
                        &format!("{field}.thinkingLevel"),
                        want.get("thinkingLevel").unwrap_or(&Value::Null),
                        &thinking_to_value(scoped.thinking_level),
                    );
                }
            }
        }

        let want_diagnostics = result
            .get("diagnostics")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        failures.check_usize(
            &label,
            "diagnostics.len",
            want_diagnostics.len(),
            got.diagnostics.len(),
        );
        for (position, want) in want_diagnostics.iter().enumerate() {
            let field = format!("diagnostics[{position}]");
            // Every recorded diagnostic is `type: "warning"`; assert the fixture still says so
            // rather than silently ignoring a new kind.
            failures.check_str(
                &label,
                &format!("{field}.type"),
                "warning",
                want["type"].as_str().unwrap_or_default(),
            );
            match got.diagnostics.get(position) {
                None => failures.fail(&label, format!("{field}: missing, want {want}")),
                Some(diagnostic) => {
                    failures.check_str(
                        &label,
                        &format!("{field}.message"),
                        want["message"].as_str().unwrap_or_default(),
                        &diagnostic.message,
                    );
                    failures.check_str(
                        &label,
                        &format!("{field}.pattern"),
                        want["pattern"].as_str().unwrap_or_default(),
                        &diagnostic.pattern,
                    );
                }
            }
        }
    }

    failures.finish("resolveModelScopeWithDiagnostics");
}

/// Scope de-duplication is by **model only**, so the second pattern's level is dropped.
/// Pinned on its own so mutation (d) — "de-duplicate by (model, thinkingLevel)" — fails here
/// by name. Values from record 112.
#[test]
fn model_scope_deduplicates_by_model_alone() {
    let catalog = synthetic_catalog();
    let source = StaticModelSource::new(
        catalog.clone(),
        catalog.iter().map(|m| m.provider.0.clone()).collect(),
    );
    let result = resolve_model_scope_with_diagnostics(
        &[
            "claude-sonnet-4-5:high".to_string(),
            "claude-sonnet-4-5:low".to_string(),
        ],
        &source,
    );
    assert_eq!(result.scoped_models.len(), 1);
    assert_eq!(result.scoped_models[0].model.id, "claude-sonnet-4-5");
    assert_eq!(
        result.scoped_models[0].thinking_level,
        Some(ThinkingLevel::High)
    );
    assert!(result.diagnostics.is_empty());
}

// =============================================================================
// Disk-backed cases: models.json under a TempDir
// =============================================================================

/// A `ConfigEnv` whose agent dir is inside `dir`. Never the user's real `~/.pirust`.
fn temp_env(dir: &std::path::Path) -> ConfigEnv {
    ConfigEnv {
        identity: PIRUST,
        platform: Platform::current(),
        home_dir: Some(dir.to_string_lossy().into_owned()),
        agent_dir_override: Some(dir.join("agent").to_string_lossy().into_owned()),
    }
}

/// Write the record's `modelsJson` into `<tmp>/agent/models.json` and return the path Pi would
/// have passed to `ModelConfig.load` — i.e. `getModelsPath()`.
///
/// `modelsJson: null` means the file is **not** created, which is the `missing-file` case.
fn write_models_json(dir: &std::path::Path, contents: Option<&str>) -> String {
    let agent = dir.join("agent");
    std::fs::create_dir_all(&agent).expect("create the temp agent dir");
    let path = agent.join("models.json");
    if let Some(contents) = contents {
        std::fs::write(&path, contents).expect("write models.json");
    }
    path.to_string_lossy().into_owned()
}

/// The recorded `File: …` suffix every config/parse/schema error carries.
///
/// Returns `(message, recorded_path)`. The recorded path is a `{TMPROOT}`-redacted absolute
/// path, so it can never equal a real temp path; the tests assert its *shape* and compare the
/// message half.
fn split_file_suffix(error: &str) -> (String, String) {
    match error.rsplit_once("\n\nFile: ") {
        Some((message, path)) => (message.to_string(), path.to_string()),
        None => (error.to_string(), String::new()),
    }
}

/// Compare an error against its recorded form, substituting the real path.
///
/// V8 divergence: an error whose message begins `Failed to parse models.json: ` embeds V8's
/// `JSON.parse` diagnostic (`Unexpected end of JSON input`, `` Expected property name or '}' in
/// JSON at position 26 (line 4 column 5) ``). Rust cannot produce that text, so only the
/// envelope — the prefix and the `\n\nFile: {path}` suffix — is asserted, and the recorded
/// message is asserted to be non-empty so a fixture that lost it fails. **Five records land
/// here and only three carry the `v8Dependent` marker**: 125/144 (`malformed-json`) and 158
/// (`listModels`) are marked; 124/143 (`json-with-comments-is-ACCEPTED`) are not, even though
/// they are equally V8-dependent. Flagged upstream; treated identically here.
const PARSE_ERROR_PREFIX: &str = "Failed to parse models.json: ";

fn check_error(
    failures: &mut Failures,
    label: &str,
    field: &str,
    want: &Value,
    got: Option<&str>,
    actual_path: &str,
) {
    let Some(recorded) = want.as_str() else {
        failures.check_opt_str(label, field, want, got);
        return;
    };
    let Some(got) = got else {
        failures.fail(label, format!("{field}: want {recorded:?} got null"));
        return;
    };

    let (want_message, want_path) = split_file_suffix(recorded);
    let (got_message, got_path) = split_file_suffix(got);

    // The recorded path is redacted; assert its shape so a reshaped fixture is noticed.
    if !want_path.is_empty() {
        if !want_path.starts_with("{TMPROOT}") {
            failures.fail(
                label,
                format!("{field}: recorded path is not {{TMPROOT}}-redacted: {want_path:?}"),
            );
        }
        if !want_path.ends_with("models.json") {
            failures.fail(
                label,
                format!("{field}: recorded path does not end at models.json: {want_path:?}"),
            );
        }
        failures.check_str(label, &format!("{field} File:"), actual_path, &got_path);
    }

    if want_message.starts_with(PARSE_ERROR_PREFIX) {
        // V8-dependent body: assert the envelope only.
        failures.check_str(
            label,
            &format!("{field} envelope"),
            PARSE_ERROR_PREFIX,
            got_message
                .get(..PARSE_ERROR_PREFIX.len())
                .unwrap_or(&got_message),
        );
        if want_message.len() <= PARSE_ERROR_PREFIX.len() {
            failures.fail(
                label,
                format!("{field}: recorded V8 message is empty — fixture regressed"),
            );
        }
        if got_message.len() <= PARSE_ERROR_PREFIX.len() {
            failures.fail(
                label,
                format!("{field}: no parse diagnostic was produced at all"),
            );
        }
        return;
    }

    failures.check_str(
        label,
        &format!("{field} message"),
        &want_message,
        &got_message,
    );
}

// =============================================================================
// ModelConfig.load — 19 records
// =============================================================================

#[test]
fn model_config_load_matches_all_19_records() {
    let mut failures = Failures::default();

    for case in cases_for("ModelConfig.load") {
        let label = case.label();
        let dir = tempfile::tempdir().expect("tempdir");
        let models_json = case.get("modelsJson").as_str().map(str::to_string);
        let path = write_models_json(dir.path(), models_json.as_deref());
        let env = temp_env(dir.path());

        // `no-path-at-all` exercises the `if (!modelsJsonPath)` guard; every other record
        // passes a path (which may point at a file that does not exist).
        let argument = if case.name() == "no-path-at-all" {
            None
        } else {
            Some(path.as_str())
        };

        let config = match ModelConfig::load(&env, argument) {
            Ok(config) => config,
            Err(error) => {
                failures.fail(&label, format!("load returned Err: {error}"));
                continue;
            }
        };

        check_error(
            &mut failures,
            &label,
            "error",
            case.get("error"),
            config.get_error(),
            &path,
        );

        let want_ids: Vec<&str> = case
            .get("providerIds")
            .as_array()
            .expect("providerIds is an array")
            .iter()
            .map(|v| v.as_str().unwrap_or_default())
            .collect();
        let got_ids: Vec<&str> = config.provider_ids().iter().map(String::as_str).collect();
        failures.check_value(
            &label,
            "providerIds",
            &Value::Array(
                want_ids
                    .iter()
                    .map(|s| Value::String((*s).into()))
                    .collect(),
            ),
            &Value::Array(got_ids.iter().map(|s| Value::String((*s).into())).collect()),
        );

        // The stored provider is Pi's `deepFreeze(structuredClone(provider))` — unknown keys
        // and all, which is why `provider-with-an-unknown-field` keeps `totallyUnknownField`.
        let want_providers = case
            .get("providers")
            .as_object()
            .expect("providers is an object")
            .clone();
        for (provider_id, want) in &want_providers {
            let got = config
                .get_provider_raw(provider_id)
                .cloned()
                .unwrap_or(Value::Null);
            failures.check_value(&label, &format!("providers.{provider_id}"), want, &got);
        }
        failures.check_usize(
            &label,
            "providers.len",
            want_providers.len(),
            config.provider_ids().len(),
        );
    }

    failures.finish("ModelConfig.load");
}

/// `stripJsonComments` handles `//` and trailing commas but **not** `/* */`.
///
/// The fixture record named `json-with-comments-is-ACCEPTED` records a *parse failure*, so this
/// asserts the same split directly: a `//` comment loads, a `/* */` block comment does not.
#[test]
fn models_json_strips_line_comments_but_not_block_comments() {
    let dir = tempfile::tempdir().expect("tempdir");
    let env = temp_env(dir.path());

    let accepted = "{\n  // a line comment\n  \"providers\": {\n    \"anthropic\": { \"baseUrl\": \"http://x\" },\n  }\n}\n";
    let path = write_models_json(dir.path(), Some(accepted));
    let config = ModelConfig::load(&env, Some(&path)).expect("load");
    assert_eq!(config.get_error(), None, "`//` and a trailing comma load");
    assert_eq!(config.provider_ids(), ["anthropic"]);

    let rejected = "{\n  \"providers\": {\n    /* a block comment */\n    \"anthropic\": { \"baseUrl\": \"http://x\" }\n  }\n}\n";
    let path = write_models_json(dir.path(), Some(rejected));
    let config = ModelConfig::load(&env, Some(&path)).expect("load");
    let error = config
        .get_error()
        .expect("a `/* */` block comment is a PARSE ERROR, as the oracle records");
    assert!(
        error.starts_with("Failed to parse models.json: "),
        "{error}"
    );
    assert!(config.provider_ids().is_empty());
}

// =============================================================================
// The builtin catalog — supplied by the fixture, never embedded
// =============================================================================

/// The `builtinCatalogFingerprint` record, rebuilt as a [`ModelCatalog`].
///
/// This is where the 18 `ModelRuntime.create` records get their base layer: **Pi's real
/// anthropic provider**, with its real 13 models, name and `baseUrl`, straight out of the
/// capture. The other providers are id-only shells with empty model lists — the fingerprint
/// enumerates their ids but reduces each to just that, so their model lists are not driven
/// through these runtime records. (The full real catalog now lives in the generated
/// `catalog.rs`; this helper predates it and is kept to replay the recorded rows against a
/// known fixed base rather than the generator's output.)
///
/// `api_key_env` carries anthropic's real env precedence so `check_auth`'s last resort is
/// exercised; the capture ran with every provider credential var **absent**
/// (`captureEnvironment`), which the tests reproduce with an empty [`ProcessEnv`].
fn builtin_catalog() -> (ModelCatalog, usize) {
    let case = cases_for("builtinCatalogFingerprint")
        .into_iter()
        .next()
        .expect("one builtinCatalogFingerprint record");

    let anthropic_models: Vec<Model> = case.value["anthropic"]["models"]
        .as_array()
        .expect("the fingerprint enumerates anthropic's models")
        .iter()
        .map(|value| {
            serde_json::from_value::<Model>(value.clone()).unwrap_or_else(|e| {
                panic!("builtin anthropic model does not fit pirust_ai::Model: {e}\n  {value}")
            })
        })
        .collect();
    let anthropic_model_count = anthropic_models.len();

    let providers: Vec<ProviderDescriptor> = case
        .get("providerIds")
        .as_array()
        .expect("providerIds is an array")
        .iter()
        .map(|value| {
            let id = value
                .as_str()
                .expect("provider ids are strings")
                .to_string();
            if id == "anthropic" {
                ProviderDescriptor {
                    id,
                    name: case.value["anthropic"]["name"]
                        .as_str()
                        .expect("anthropic name")
                        .to_string(),
                    base_url: case.value["anthropic"]["baseUrl"]
                        .as_str()
                        .map(str::to_string),
                    models: anthropic_models.clone(),
                    api_key_env: pirust_ai::auth::ANTHROPIC_API_KEY_ENV_PRECEDENCE
                        .iter()
                        .map(|name| (*name).to_string())
                        .collect(),
                    has_api_key_auth: true,
                    has_oauth_auth: false,
                }
            } else {
                // Model list unknown: the fingerprint reduces every non-anthropic provider to
                // its id. See `model_runtime_totals` for how `totals.models` is still checked.
                ProviderDescriptor {
                    id: id.clone(),
                    name: id,
                    base_url: None,
                    models: Vec::new(),
                    api_key_env: Vec::new(),
                    has_api_key_auth: true,
                    has_oauth_auth: false,
                }
            }
        })
        .collect();

    (ModelCatalog::new(providers), anthropic_model_count)
}

/// The `builtinCatalogFingerprint` record is explicitly "a drift signal, not a contract" — the
/// catalog is generated and grows every Pi release. So this test asserts what *is* a contract:
///
/// - the 1306 real model fields round-trip through `pirust_ai::types::Model`;
/// - a runtime with **no** `models.json` leaves the provider untouched (`recomposeProvider`'s
///   fast path), so name, `baseUrl` and all 13 anthropic models come through unchanged;
/// - with no credentials anywhere, `availableModels` is 0.
///
/// `totalProviders: 40` and `totalModels: 1306` are **recorded, not required**: they are
/// printed on failure of the derived checks but never asserted as equalities.
#[test]
fn builtin_catalog_composes_untouched_when_there_is_no_models_json() {
    let case = cases_for("builtinCatalogFingerprint")
        .into_iter()
        .next()
        .expect("one record");
    let (catalog, anthropic_count) = builtin_catalog();
    let dir = tempfile::tempdir().expect("tempdir");
    let env = temp_env(dir.path());

    let runtime = ModelRuntime::create(
        &env,
        catalog,
        CreateModelRuntimeOptions {
            models_path: None,
            stored_credentials: BTreeSet::new(),
            process_env: ProcessEnv::from_pairs(Vec::<(String, String)>::new()),
        },
    )
    .expect("create");

    let mut failures = Failures::default();
    let label = case.label();

    let anthropic = runtime
        .get_provider("anthropic")
        .expect("the builtin anthropic provider survives with no models.json");
    failures.check_str(
        &label,
        "anthropic.name",
        case.value["anthropic"]["name"].as_str().unwrap_or_default(),
        &anthropic.name,
    );
    failures.check_opt_str(
        &label,
        "anthropic.baseUrl",
        &case.value["anthropic"]["baseUrl"],
        anthropic.base_url.as_deref(),
    );
    let want_models = case.value["anthropic"]["models"]
        .as_array()
        .expect("models")
        .clone();
    failures.check_usize(
        &label,
        "anthropic.models.len",
        want_models.len(),
        anthropic.models.len(),
    );
    for (position, want) in want_models.iter().enumerate() {
        check_model(
            &mut failures,
            &label,
            &format!("anthropic.models[{position}]"),
            want,
            anthropic.models.get(position),
        );
    }

    // Availability with a credential-free environment.
    failures.check_usize(
        &label,
        "availableModels",
        case.get("availableModels").as_u64().unwrap_or_default() as usize,
        runtime.get_available().len(),
    );

    // Drift signals, reported but not required.
    let recorded_providers = case.get("totalProviders").as_u64().unwrap_or_default();
    let recorded_models = case.get("totalModels").as_u64().unwrap_or_default();
    assert!(
        recorded_providers > 0 && recorded_models > 0,
        "the fingerprint must still carry its drift numbers ({recorded_providers} providers / \
         {recorded_models} models at pi {}); they are NOT asserted as equalities because the \
         catalog is generated and version-dependent — only the anthropic block above is a \
         contract, and it accounts for {anthropic_count} of them",
        case.str_field("piVersion"),
    );

    failures.finish("builtinCatalogFingerprint");
}

// =============================================================================
// ModelRuntime.create — 18 records, against the real builtin anthropic provider
// =============================================================================

/// Compare a console/`getError` string that may embed V8's `JSON.parse` diagnostic.
///
/// Outside that diagnostic the comparison is literal; the `File: …` tail is compared against
/// the real temp path, since the recorded one is `{TMPROOT}`-redacted.
fn check_text_with_v8_tolerance(
    failures: &mut Failures,
    label: &str,
    field: &str,
    want: &str,
    got: &str,
    actual_path: &str,
) {
    let Some(head_end) = want.find(PARSE_ERROR_PREFIX) else {
        failures.check_str(label, field, want, got);
        return;
    };
    let head = &want[..head_end + PARSE_ERROR_PREFIX.len()];
    if !got.starts_with(head) {
        failures.fail(
            label,
            format!("{field}: want prefix {head:?}\n    got  {got:?}"),
        );
        return;
    }
    // The recorded body is V8's; only the `\n\nFile: {path}` tail is reproducible.
    let (_, want_path) = split_file_suffix(want);
    let (_, got_path) = split_file_suffix(got);
    if want_path.is_empty() {
        failures.checked += 1;
        return;
    }
    failures.check_str(label, &format!("{field} File:"), actual_path, &got_path);
}

#[test]
fn model_runtime_create_matches_all_18_records() {
    let mut failures = Failures::default();
    let baseline_models = builtin_catalog().1;
    let fingerprint = cases_for("builtinCatalogFingerprint")
        .into_iter()
        .next()
        .expect("one fingerprint record");
    let recorded_baseline_models = fingerprint
        .get("totalModels")
        .as_u64()
        .expect("totalModels") as i64;

    for case in cases_for("ModelRuntime.create") {
        let label = case.label();
        let dir = tempfile::tempdir().expect("tempdir");
        let models_json = case.get("modelsJson").as_str().map(str::to_string);
        let path = write_models_json(dir.path(), models_json.as_deref());
        let env = temp_env(dir.path());
        let (catalog, _) = builtin_catalog();

        let runtime = match ModelRuntime::create(
            &env,
            catalog,
            CreateModelRuntimeOptions {
                models_path: Some(path.as_str()),
                // `captureEnvironment`: no credential file, no credential env var.
                stored_credentials: BTreeSet::new(),
                process_env: ProcessEnv::from_pairs(Vec::<(String, String)>::new()),
            },
        ) {
            Ok(runtime) => runtime,
            Err(error) => {
                failures.fail(&label, format!("create returned Err: {error}"));
                continue;
            }
        };

        check_error(
            &mut failures,
            &label,
            "getError",
            case.get("getError"),
            runtime.get_error().as_deref(),
            &path,
        );

        let totals = case.get("totals");
        failures.check_usize(
            &label,
            "totals.providers",
            totals["providers"].as_u64().unwrap_or_default() as usize,
            runtime.providers().len(),
        );
        failures.check_usize(
            &label,
            "totals.availableModels",
            totals["availableModels"].as_u64().unwrap_or_default() as usize,
            runtime.get_available().len(),
        );
        failures.check_value(
            &label,
            "totals.configuredProviders",
            &totals["configuredProviders"],
            &Value::Array(
                runtime
                    .configured_providers()
                    .iter()
                    .map(|id| Value::String(id.clone()))
                    .collect(),
            ),
        );

        // `totals.models` counts all 1306 real builtin models; this test-side fixture holds
        // only the 13 anthropic ones (the fingerprint enumerates the rest as id shells), so
        // the *delta* from the no-models.json baseline is what is comparable — and it is exactly
        // what composition controls: +3 for three inline models, +1 for one appended id, 0
        // when a provider is replaced in place or deleted.
        let want_delta = totals["models"].as_i64().unwrap_or_default() - recorded_baseline_models;
        let got_delta = runtime.get_models().len() as i64 - baseline_models as i64;
        failures.checked += 1;
        if want_delta != got_delta {
            failures.entries.push(format!(
                "{label} totals.models delta: want {want_delta} got {got_delta} \
                 (recorded {} vs baseline {recorded_baseline_models}; ours {} vs baseline \
                 {baseline_models})",
                totals["models"],
                runtime.get_models().len()
            ));
        }

        for (provider_id, want) in case
            .get("reportedProviders")
            .as_object()
            .expect("reportedProviders is an object")
        {
            let field = format!("reportedProviders.{provider_id}");
            let composed = runtime.get_provider(provider_id);
            failures.check_value(
                &label,
                &format!("{field}.present"),
                &want["present"],
                &Value::Bool(composed.is_some()),
            );
            failures.check_opt_str(
                &label,
                &format!("{field}.name"),
                &want["name"],
                composed.map(|p| p.name.as_str()),
            );
            failures.check_opt_str(
                &label,
                &format!("{field}.baseUrl"),
                &want["baseUrl"],
                composed.and_then(|p| p.base_url.as_deref()),
            );
            failures.check_value(
                &label,
                &format!("{field}.hasConfiguredAuth"),
                &want["hasConfiguredAuth"],
                &Value::Bool(runtime.has_configured_auth(provider_id)),
            );
            failures.check_usize(
                &label,
                &format!("{field}.modelCount"),
                want["modelCount"].as_u64().unwrap_or_default() as usize,
                composed.map_or(0, |p| p.models.len()),
            );
            for (position, want_model) in want["models"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .enumerate()
            {
                check_model(
                    &mut failures,
                    &label,
                    &format!("{field}.models[{position}]"),
                    want_model,
                    composed.and_then(|p| p.models.get(position)),
                );
            }
        }
    }

    failures.finish("ModelRuntime.create");
}

/// The Meridian local-proxy shape, pinned on its own: every builtin anthropic model keeps its
/// id/name/cost/contextWindow and takes the overridden `baseUrl`, `hasConfiguredAuth` goes true
/// from `apiKey`, and **`headers` stays null** — the provider-level header is applied at request
/// time by `resolveConfiguredModelHeaders`, never copied onto a model.
///
/// Mutation (e) — "copy `headers` onto the composed model objects" — fails here by name.
#[test]
fn meridian_local_proxy_keeps_model_headers_null() {
    let case = cases_for("ModelRuntime.create")
        .into_iter()
        .find(|c| c.name() == "meridian-local-proxy")
        .expect("the meridian-local-proxy record");
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_models_json(dir.path(), case.get("modelsJson").as_str());
    let env = temp_env(dir.path());
    let (catalog, _) = builtin_catalog();

    let runtime = ModelRuntime::create(
        &env,
        catalog,
        CreateModelRuntimeOptions {
            models_path: Some(path.as_str()),
            stored_credentials: BTreeSet::new(),
            process_env: ProcessEnv::from_pairs(Vec::<(String, String)>::new()),
        },
    )
    .expect("create");

    assert_eq!(runtime.get_error(), None);
    assert!(
        runtime.has_configured_auth("anthropic"),
        "apiKey configures it"
    );
    let anthropic = runtime.get_provider("anthropic").expect("present");
    assert_eq!(anthropic.name, "Anthropic", "the builtin name is kept");
    assert_eq!(anthropic.base_url.as_deref(), Some("http://127.0.0.1:3456"));
    assert_eq!(anthropic.models.len(), 13);
    for model in &anthropic.models {
        assert_eq!(
            model.base_url, "http://127.0.0.1:3456",
            "{} kept the old baseUrl",
            model.id
        );
        assert_eq!(
            model.headers, None,
            "{} must NOT carry the provider header",
            model.id
        );
    }
    // Costs and context windows are untouched by a baseUrl/apiKey/headers override.
    let opus = anthropic
        .models
        .iter()
        .find(|m| m.id == "claude-opus-4-8")
        .expect("claude-opus-4-8");
    assert_eq!(opus.context_window, 1_000_000);
    assert_eq!(opus.max_tokens, 128_000);
    assert_eq!(opus.cost.rates.input, 5.0);
}

/// An unrecognised `api` composes without error and the model **is** listed and available; it
/// only fails at stream time. Mutation (c) — "reject an unknown `api` at load time" — fails
/// here by name.
#[test]
fn an_unknown_api_composes_and_only_fails_at_stream_time() {
    let case = cases_for("ModelRuntime.create")
        .into_iter()
        .find(|c| c.name() == "unknown-api-value-COMPOSES-FINE")
        .expect("the unknown-api record");
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_models_json(dir.path(), case.get("modelsJson").as_str());
    let env = temp_env(dir.path());
    let (catalog, _) = builtin_catalog();

    let runtime = ModelRuntime::create(
        &env,
        catalog,
        CreateModelRuntimeOptions {
            models_path: Some(path.as_str()),
            stored_credentials: BTreeSet::new(),
            process_env: ProcessEnv::from_pairs(Vec::<(String, String)>::new()),
        },
    )
    .expect("create");

    assert_eq!(
        runtime.get_error(),
        None,
        "no composition error at load time"
    );
    assert!(runtime.composition_errors().is_empty());
    let provider = runtime
        .get_provider("oracle-badapi")
        .expect("still present");
    assert_eq!(provider.models.len(), 1);
    assert_eq!(provider.models[0].api.0, "not-a-real-api");
    assert!(runtime.has_configured_auth("oracle-badapi"));
    assert_eq!(runtime.get_available().len(), 1);
    // The one place the value is ever rejected (`provider-composer.ts:462`).
    assert_eq!(
        pirust_coding_agent::models::api_provider_error(&provider.models[0].api.0),
        "No API provider registered for api: not-a-real-api"
    );
}

// =============================================================================
// listModels — 6 records
// =============================================================================

/// `getDocsPath()` is deferred by spec §5.2, and the capture redacted it to `{PIPKG}`. Passing
/// the redacted strings back in is what makes the no-models message comparable byte for byte —
/// the doc paths are inputs to `formatNoModelsAvailableMessage`, not something this module
/// derives.
const PROVIDERS_DOC: &str = "{PIPKG}\\docs\\providers.md";
const MODELS_DOC: &str = "{PIPKG}\\docs\\models.md";

#[test]
fn list_models_renders_all_6_records_byte_for_byte() {
    let mut failures = Failures::default();

    for case in cases_for("listModels") {
        let label = case.label();
        let dir = tempfile::tempdir().expect("tempdir");
        let path = write_models_json(dir.path(), case.get("modelsJson").as_str());
        let env = temp_env(dir.path());
        let (catalog, _) = builtin_catalog();

        let runtime = match ModelRuntime::create(
            &env,
            catalog,
            CreateModelRuntimeOptions {
                models_path: Some(path.as_str()),
                stored_credentials: BTreeSet::new(),
                process_env: ProcessEnv::from_pairs(Vec::<(String, String)>::new()),
            },
        ) {
            Ok(runtime) => runtime,
            Err(error) => {
                failures.fail(&label, format!("create returned Err: {error}"));
                continue;
            }
        };

        failures.check_usize(
            &label,
            "availableCount",
            case.get("availableCount").as_u64().unwrap_or_default() as usize,
            runtime.get_available().len(),
        );

        let lines = list_models(
            &runtime,
            ListModelsOptions {
                search_pattern: case.get("searchPattern").as_str(),
                providers_doc_path: PROVIDERS_DOC,
                models_doc_path: MODELS_DOC,
            },
        );

        let want_output = case
            .get("output")
            .as_array()
            .expect("output is an array")
            .clone();
        failures.check_usize(&label, "output.len", want_output.len(), lines.len());
        for (position, want) in want_output.iter().enumerate() {
            let field = format!("output[{position}]");
            match lines.get(position) {
                None => failures.fail(&label, format!("{field}: missing, want {want}")),
                Some(line) => {
                    let want_stream = want["stream"].as_str().unwrap_or_default();
                    let got_stream = match line.stream {
                        OutputStream::Stdout => "stdout",
                        OutputStream::Stderr => "stderr",
                    };
                    failures.check_str(&label, &format!("{field}.stream"), want_stream, got_stream);
                    check_text_with_v8_tolerance(
                        &mut failures,
                        &label,
                        &format!("{field}.text"),
                        want["text"].as_str().unwrap_or_default(),
                        &line.text,
                        &path,
                    );
                }
            }
        }

        // `stdout` is the concatenation of every stdout line plus a trailing newline, which is
        // what `console.log` produces. Column widths are computed from the data, so this is the
        // byte-for-byte check the table really needs.
        let got_stdout: String = lines
            .iter()
            .filter(|line| line.stream == OutputStream::Stdout)
            .map(|line| format!("{}\n", line.text))
            .collect();
        check_text_with_v8_tolerance(
            &mut failures,
            &label,
            "stdout",
            case.str_field("stdout"),
            &got_stdout,
            &path,
        );
    }

    failures.finish("listModels");
}

/// `formatTokenCount` (`cli/list-models.ts:14-24`), with the four values the
/// `list:three-custom-models` record's note names and the `.5` boundary that separates JS's
/// `toFixed` from Rust's `{:.1}`.
#[test]
fn format_token_count_matches_js_tofixed() {
    use pirust_coding_agent::models::format_token_count;
    assert_eq!(format_token_count(1_000_000), "1M");
    assert_eq!(format_token_count(128_000), "128K");
    assert_eq!(format_token_count(8_192), "8.2K");
    assert_eq!(format_token_count(900), "900");
    assert_eq!(format_token_count(16_384), "16.4K");
    assert_eq!(format_token_count(64_000), "64K");
    assert_eq!(format_token_count(200_000), "200K");
    // JS `toFixed` rounds half away from zero; `format!("{:.1}")` rounds half to even and
    // would answer "1.2K" here.
    assert_eq!(format_token_count(1_250), "1.3K");
    assert_eq!(format_token_count(1_049_000), "1.0M");
    assert_eq!(format_token_count(999), "999");
    assert_eq!(format_token_count(0), "0");
}

/// The three quirks the fixture names in prose, asserted as standalone regressions so a
/// mutation that breaks one produces a *pointed* failure rather than a diff inside 35 rows.
///
/// Every expected value here is lifted from a named fixture record, not invented.
#[test]
fn parse_model_pattern_quirks_are_pinned_individually() {
    let catalog = synthetic_catalog();

    // Record 42: `findExactModelReferenceMatch` rejects the empty string, but the substring
    // fallback uses `id.includes("")` — true for every model — so the alias tie-break picks one.
    let empty = parse_model_pattern("", &catalog, ParseModelPatternOptions::LENIENT);
    let picked = empty.model.expect("the empty pattern MATCHES a model");
    assert_eq!(picked.provider.0, "vercel-ai-gateway");
    assert_eq!(picked.id, "zai/glm-5.1");

    // Record 39: the LAST colon-suffix wins.
    let last_wins = parse_model_pattern(
        "claude-sonnet-4-5:high:low",
        &catalog,
        ParseModelPatternOptions::LENIENT,
    );
    assert_eq!(last_wins.thinking_level, Some(ThinkingLevel::Low));
    assert_eq!(last_wins.warning, None);

    // Record 38: an inner invalid-level warning SUPPRESSES the outer level and propagates.
    let suppressed = parse_model_pattern(
        "openai/gpt-4o:bogus:high",
        &catalog,
        ParseModelPatternOptions::LENIENT,
    );
    assert_eq!(suppressed.model.map(|m| m.id), Some("openai/gpt-4o".into()));
    assert_eq!(suppressed.thinking_level, None);
    assert_eq!(
        suppressed.warning.as_deref(),
        Some("Invalid thinking level \"bogus\" in pattern \"openai/gpt-4o:bogus\". Using default instead.")
    );
}
