//! `xtask` — repo build tooling.
//!
//! Hosts `gen-catalog`, which emits `crates/pirust-coding-agent/src/catalog.rs` — the
//! **anthropic-only slice** of Pi's builtin model catalog — from the
//! `builtinCatalogFingerprint` record of `tests/fixtures/pi/cli/models.cases.jsonl`.
//!
//! This is *not* feat-008's generator. Pi's real generator
//! (`packages/ai/scripts/generate-models.ts`) walks 36 providers / 1062 models; this one
//! reproduces exactly the one provider whose `api` adapter is ported (`anthropic-messages`),
//! from data captured out of real Pi 0.80.10 rather than hand-authored. When feat-008 lands
//! its generator, this subcommand and `catalog.rs` are both superseded.
//! See `docs/analysis/02-ai.md`.
//!
//! # Usage
//!
//! ```text
//! cargo xtask gen-catalog            # regenerate crates/pirust-coding-agent/src/catalog.rs
//! cargo xtask gen-catalog --check    # exit 1 if the checked-in file is not what we'd emit
//! ```
//!
//! The emitted text is already `rustfmt`-clean and fully ordered by the fixture, so
//! regenerating an up-to-date tree is a byte-for-byte no-op.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde_json::{Map, Value};

// =============================================================================
// Entry point
// =============================================================================

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("gen-catalog") => gen_catalog(&args[1..]),
        _ => {
            eprintln!(
                "xtask: unknown task. Available: gen-catalog [--check]. \
                 See docs/analysis/00-overview.md §6"
            );
            std::process::exit(2);
        }
    };
    if let Err(error) = result {
        eprintln!("xtask: {error:#}");
        std::process::exit(1);
    }
}

/// Repo root — `CARGO_MANIFEST_DIR` is `<root>/xtask`.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask/ has a parent")
        .to_path_buf()
}

// =============================================================================
// gen-catalog
// =============================================================================

/// The fixture the catalog is derived from, relative to the repo root.
const FIXTURE: &str = "tests/fixtures/pi/cli/models.cases.jsonl";
/// The record inside [`FIXTURE`] (its `fn` field).
const RECORD: &str = "builtinCatalogFingerprint";
/// The generated file, relative to the repo root.
const OUTPUT: &str = "crates/pirust-coding-agent/src/catalog.rs";
/// The one provider the slice carries — the only ported `api` adapter.
const PROVIDER: &str = "anthropic";
/// The `api` every model in the slice must declare, so everything it advertises can stream.
const API: &str = "anthropic-messages";

fn gen_catalog(args: &[String]) -> Result<()> {
    let check_only = match args {
        [] => false,
        [flag] if flag == "--check" => true,
        _ => bail!("gen-catalog takes no arguments except --check"),
    };

    let root = repo_root();
    let record = read_record(&root.join(FIXTURE))?;
    let rendered = render(&record)?;

    let output = root.join(OUTPUT);
    let current = std::fs::read_to_string(&output).unwrap_or_default();
    // Compare after normalizing CRLF: a checkout under `core.autocrlf=true` is still current.
    if current.replace("\r\n", "\n") == rendered {
        println!("{OUTPUT}: up to date ({} bytes)", rendered.len());
        return Ok(());
    }
    if check_only {
        bail!("{OUTPUT} is stale — run `cargo xtask gen-catalog`");
    }
    std::fs::write(&output, rendered.as_bytes())
        .with_context(|| format!("write {}", output.display()))?;
    println!("{OUTPUT}: regenerated ({} bytes)", rendered.len());
    Ok(())
}

/// The single [`RECORD`] line of the fixture, as JSON.
fn read_record(path: &Path) -> Result<Value> {
    let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let mut found: Option<Value> = None;
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(line)
            .with_context(|| format!("{}:{}: not JSON", path.display(), index + 1))?;
        if value["fn"].as_str() == Some(RECORD) {
            if found.is_some() {
                bail!("{}: more than one `{RECORD}` record", path.display());
            }
            found = Some(value);
        }
    }
    found.with_context(|| format!("{}: no `{RECORD}` record", path.display()))
}

// =============================================================================
// Rendering
// =============================================================================

/// Which optional types and helpers the emitted models actually reference, so the `use` list
/// and the helper functions carry nothing unused — `-D warnings` would reject a dead one.
#[derive(Debug, Default)]
struct Needs {
    thinking_level_map: bool,
    compat_object: bool,
    header_object: bool,
}

impl Needs {
    fn any_object(&self) -> bool {
        self.compat_object || self.header_object
    }
}

/// The whole of `catalog.rs`, ready to write. Deterministic: every value and every ordering
/// comes from `record`.
fn render(record: &Value) -> Result<String> {
    let provider = &record[PROVIDER];
    let name = provider["name"]
        .as_str()
        .with_context(|| format!("{RECORD}.{PROVIDER}.name is missing"))?;
    let base_url = provider["baseUrl"]
        .as_str()
        .with_context(|| format!("{RECORD}.{PROVIDER}.baseUrl is missing"))?;
    let models = provider["models"]
        .as_array()
        .with_context(|| format!("{RECORD}.{PROVIDER}.models is missing"))?;
    if models.is_empty() {
        bail!("{RECORD}.{PROVIDER}.models is empty");
    }
    let pi_version = record["piVersion"].as_str().unwrap_or("unknown");

    // The models first: they decide the `use` list and which helpers are emitted.
    let mut needs = Needs::default();
    let mut models_fn = String::new();
    render_models_fn(&mut models_fn, models, &mut needs)?;

    let mut out = String::new();
    render_header(&mut out, pi_version, models.len());
    out.push('\n');
    let mut types = vec![
        "Api",
        "Modality",
        "Model",
        "ModelCost",
        "ModelCostRates",
        "ProviderId",
    ];
    if needs.thinking_level_map {
        types.push("ThinkingLevelMap");
    }
    out.push_str(&use_list("pirust_ai::types", &types));
    if needs.any_object() {
        // Both helper signatures name `Value`, and `object`'s return type names `Map`.
        out.push_str(&use_list("serde_json", &["Map", "Value"]));
    }
    out.push('\n');
    out.push_str(&use_list(
        "crate::models",
        &["ModelCatalog", "ProviderDescriptor"],
    ));
    render_catalog_fn(&mut out, models.len());
    render_provider_fn(&mut out, name, base_url);
    out.push_str(&models_fn);
    render_helpers(&mut out, &needs);
    Ok(out)
}

/// `use path::{A, B};` — one line while it fits `rustfmt`'s 100 columns, block-wrapped after
/// that, which is what `imports_layout = "Mixed"` (the default) produces.
fn use_list(path: &str, items: &[&str]) -> String {
    let joined = items.join(", ");
    let one_line = format!("use {path}::{{{joined}}};\n");
    if one_line.len() - 1 <= 100 {
        return one_line;
    }
    format!("use {path}::{{\n    {joined},\n}};\n")
}

fn render_header(out: &mut String, pi_version: &str, model_count: usize) {
    // Kept in the intent of the hand-written stub this replaces.
    out.push_str(&format!(
        "//! The builtin model catalog — **GENERATED, do not edit**.\n\
         //!\n\
         //! Run `cargo xtask gen-catalog` to regenerate. Every literal below comes from the\n\
         //! `{RECORD}` record of\n\
         //! `{FIXTURE}`, captured from real Pi {pi_version}.\n\
         //!\n\
         //! Pi imports a generated 36-provider / 1062-model table from\n\
         //! `@earendil-works/pi-ai/providers/all`. feat-008 owns the equivalent generator; until it\n\
         //! lands, this file carries the **anthropic-only slice** — `{API}` is the only ported\n\
         //! api adapter, so `{PROVIDER}` is the only provider whose {model_count} models can actually\n\
         //! stream, and spec §9.5's \"override the builtin `baseUrl`\" shape keeps working.\n\
         //!\n\
         //! # Why one [`ProviderDescriptor`] and not 36\n\
         //!\n\
         //! The fingerprint enumerates 36 provider ids but reduces every non-{PROVIDER} one to just\n\
         //! its id — no name, no `baseUrl`, no models. `tests/models_golden.rs` rebuilds those 35 as\n\
         //! empty-model shells so the fixture's `totalProviders` stays exact; that is a **test-side**\n\
         //! construction and deliberately not repeated here. Shells are model-invisible, so\n\
         //! `get_models`, `get_available`, `list_models` and every resolver in [`crate::models`]\n\
         //! behave identically without them — but they *are* visible to\n\
         //! `ModelRuntime::provider_ids`, `get_provider` and `configured_providers`, where they\n\
         //! would advertise 35 providers with an invented name, no `baseUrl` and nothing to stream.\n\
         //!\n\
         //! Delete this module when feat-008's generator arrives.\n",
    ));
}

fn render_catalog_fn(out: &mut String, model_count: usize) {
    out.push_str(&format!(
        "\n/// The builtin catalog `ModelRuntime::create` is handed — one provider, {model_count} models.\n\
         ///\n\
         /// A plain constructor rather than a `static`: [`Model`] and [`ProviderDescriptor`] are\n\
         /// `String`/`Vec`/`Value`-shaped, so no `const` form exists, and `ModelRuntime::create`\n\
         /// takes the catalog **by value** — a `LazyLock` would only add a clone of the same\n\
         /// allocations. There is no parsing here at any point, which is the whole point of\n\
         /// generating Rust instead of embedding the fixture JSON.\n\
         pub fn builtin_catalog() -> ModelCatalog {{\n    \
         ModelCatalog::new(vec![anthropic_provider()])\n}}\n",
    ));
}

fn render_provider_fn(out: &mut String, name: &str, base_url: &str) {
    out.push_str(&format!(
        "\n/// Pi's builtin `{PROVIDER}` provider.\n\
         ///\n\
         /// `id`, `name`, `base_url` and `models` are the fingerprint's. The three auth fields are\n\
         /// not in that record — it captures the catalog's *shape*, not its auth wiring — so they\n\
         /// come from the oracle-verified construction in `tests/models_golden.rs`:\n\
         /// [`pirust_ai::auth::ANTHROPIC_API_KEY_ENV_PRECEDENCE`] is the env order\n\
         /// `ModelRuntime::check_auth`'s last resort walks, api-key auth is inherited, and no\n\
         /// oauth method is modelled (feat-005 has no radius/oauth flow).\n\
         fn anthropic_provider() -> ProviderDescriptor {{\n    \
         ProviderDescriptor {{\n        \
         id: {}.to_string(),\n        \
         name: {}.to_string(),\n        \
         base_url: Some({}.to_string()),\n        \
         models: anthropic_models(),\n        \
         api_key_env: pirust_ai::auth::ANTHROPIC_API_KEY_ENV_PRECEDENCE\n            \
         .iter()\n            \
         .map(|name| (*name).to_string())\n            \
         .collect(),\n        \
         has_api_key_auth: true,\n        \
         has_oauth_auth: false,\n    \
         }}\n}}\n",
        rust_str(PROVIDER),
        rust_str(name),
        rust_str(base_url),
    ));
}

fn render_models_fn(out: &mut String, models: &[Value], needs: &mut Needs) -> Result<()> {
    out.push_str(&format!(
        "\n/// The {} builtin `{PROVIDER}` models, in catalog order — which is load-bearing: it seeds\n\
         /// `getModels()`' order and therefore `availableModels[0]` in step 4 of\n\
         /// `find_initial_model`.\n\
         fn anthropic_models() -> Vec<Model> {{\n    \
         vec![\n",
        models.len(),
    ));
    for (index, model) in models.iter().enumerate() {
        render_model(out, model, index, needs)?;
    }
    out.push_str("    ]\n}\n");
    Ok(())
}

fn render_model(out: &mut String, model: &Value, index: usize, needs: &mut Needs) -> Result<()> {
    let at = |field: &str| format!("{RECORD}.{PROVIDER}.models[{index}].{field}");

    let id = model["id"].as_str().with_context(|| at("id"))?;
    let name = model["name"].as_str().with_context(|| at("name"))?;
    let api = model["api"].as_str().with_context(|| at("api"))?;
    if api != API {
        bail!(
            "{}: {api:?} — the slice may only carry {API:?} models, everything else would \
             advertise a model that cannot stream",
            at("api"),
        );
    }
    let provider = model["provider"].as_str().with_context(|| at("provider"))?;
    if provider != PROVIDER {
        bail!("{}: {provider:?}, expected {PROVIDER:?}", at("provider"));
    }
    let base_url = model["baseUrl"].as_str().with_context(|| at("baseUrl"))?;
    let reasoning = model["reasoning"]
        .as_bool()
        .with_context(|| at("reasoning"))?;

    out.push_str("        Model {\n");
    let _ = writeln!(out, "            id: {}.to_string(),", rust_str(id));
    let _ = writeln!(out, "            name: {}.to_string(),", rust_str(name));
    let _ = writeln!(out, "            api: Api::from({}),", rust_str(api));
    let _ = writeln!(
        out,
        "            provider: ProviderId::from({}),",
        rust_str(provider)
    );
    let _ = writeln!(
        out,
        "            base_url: {}.to_string(),",
        rust_str(base_url)
    );
    let _ = writeln!(out, "            reasoning: {reasoning},");
    render_thinking_level_map(
        out,
        &model["thinkingLevelMap"],
        &at("thinkingLevelMap"),
        needs,
    )?;
    render_input(out, &model["input"], &at("input"))?;
    render_cost(out, &model["cost"], &at("cost"))?;
    let _ = writeln!(
        out,
        "            context_window: {},",
        rust_u64(&model["contextWindow"], &at("contextWindow"))?
    );
    let _ = writeln!(
        out,
        "            max_tokens: {},",
        rust_u64(&model["maxTokens"], &at("maxTokens"))?
    );
    render_headers(out, &model["headers"], &at("headers"), needs)?;
    render_compat(out, &model["compat"], &at("compat"), needs)?;
    out.push_str("        },\n");
    Ok(())
}

/// `thinkingLevelMap` — every one of the seven fields is emitted, so an absent key is a
/// visible `None` and no `..Default::default()` is needed.
fn render_thinking_level_map(
    out: &mut String,
    value: &Value,
    at: &str,
    needs: &mut Needs,
) -> Result<()> {
    if value.is_null() {
        out.push_str("            thinking_level_map: None,\n");
        return Ok(());
    }
    needs.thinking_level_map = true;
    let map = value
        .as_object()
        .with_context(|| format!("{at}: not an object"))?;
    // Declaration order of `ThinkingLevelMap`, not the record's key order: the record only ever
    // carries a subset, and a fixed order keeps the output stable whatever it contains.
    const LEVELS: [(&str, &str); 7] = [
        ("off", "off"),
        ("minimal", "minimal"),
        ("low", "low"),
        ("medium", "medium"),
        ("high", "high"),
        ("xhigh", "xhigh"),
        ("max", "max"),
    ];
    for key in map.keys() {
        if !LEVELS.iter().any(|(json, _)| json == key) {
            bail!("{at}: unknown thinking level {key:?}");
        }
    }
    out.push_str("            thinking_level_map: Some(ThinkingLevelMap {\n");
    for (json_key, field) in LEVELS {
        let rendered = match map.get(json_key) {
            // Absent: `Option<Option<_>>`'s outer `None`.
            None => "None".to_string(),
            // Explicit JSON `null`: the level is unsupported.
            Some(Value::Null) => "Some(None)".to_string(),
            Some(Value::String(mapped)) => format!("Some(Some({}.to_string()))", rust_str(mapped)),
            Some(other) => bail!("{at}.{json_key}: expected a string or null, got {other}"),
        };
        let _ = writeln!(out, "                {field}: {rendered},");
    }
    out.push_str("            }),\n");
    Ok(())
}

fn render_input(out: &mut String, value: &Value, at: &str) -> Result<()> {
    let items = value
        .as_array()
        .with_context(|| format!("{at}: not an array"))?;
    let mut rendered: Vec<String> = Vec::with_capacity(items.len());
    for item in items {
        let variant = match item.as_str() {
            Some("text") => "Modality::Text",
            Some("image") => "Modality::Image",
            _ => bail!("{at}: unknown modality {item}"),
        };
        rendered.push(variant.to_string());
    }
    let _ = writeln!(out, "            input: vec![{}],", rendered.join(", "));
    Ok(())
}

fn render_cost(out: &mut String, value: &Value, at: &str) -> Result<()> {
    let cost = value
        .as_object()
        .with_context(|| format!("{at}: not an object"))?;
    for key in cost.keys() {
        if !matches!(
            key.as_str(),
            "input" | "output" | "cacheRead" | "cacheWrite" | "tiers"
        ) {
            bail!("{at}: unknown cost field {key:?}");
        }
    }
    out.push_str("            cost: ModelCost {\n                rates: ModelCostRates {\n");
    for (json_key, field) in [
        ("input", "input"),
        ("output", "output"),
        ("cacheRead", "cache_read"),
        ("cacheWrite", "cache_write"),
    ] {
        let rate = cost
            .get(json_key)
            .and_then(Value::as_f64)
            .with_context(|| format!("{at}.{json_key}: not a number"))?;
        let _ = writeln!(out, "                    {field}: {},", rust_f64(rate));
    }
    out.push_str("                },\n");
    match cost.get("tiers") {
        None | Some(Value::Null) => out.push_str("                tiers: None,\n"),
        // No captured anthropic model has tiers; the fixture is the contract, so refuse rather
        // than emit an untested shape.
        Some(_) => bail!("{at}.tiers: cost tiers are not emitted yet — extend gen-catalog"),
    }
    out.push_str("            },\n");
    Ok(())
}

fn render_headers(out: &mut String, value: &Value, at: &str, needs: &mut Needs) -> Result<()> {
    if value.is_null() {
        out.push_str("            headers: None,\n");
        return Ok(());
    }
    needs.header_object = true;
    let map = value
        .as_object()
        .with_context(|| format!("{at}: not an object"))?;
    let _ = writeln!(
        out,
        "            headers: header_object({}),",
        rust_entries(map, 12)?
    );
    Ok(())
}

fn render_compat(out: &mut String, value: &Value, at: &str, needs: &mut Needs) -> Result<()> {
    if value.is_null() {
        out.push_str("            compat: None,\n");
        return Ok(());
    }
    needs.compat_object = true;
    let map = value
        .as_object()
        .with_context(|| format!("{at}: not an object"))?;
    let _ = writeln!(
        out,
        "            compat: compat_object({}),",
        rust_entries(map, 12)?
    );
    Ok(())
}

/// The helper functions the model literals call. Emitted only when referenced — the wrappers
/// exist so a one-entry literal stays inside `rustfmt`'s `fn_call_width`, i.e. so the emitted
/// text is already formatted.
fn render_helpers(out: &mut String, needs: &Needs) {
    if needs.compat_object {
        out.push_str(
            "\n/// `Model::compat` from an ordered key/value list.\n\
             fn compat_object<const N: usize>(entries: [(&str, Value); N]) -> Option<Value> {\n    \
             Some(Value::Object(object(entries)))\n}\n",
        );
    }
    if needs.header_object {
        out.push_str(
            "\n/// `Model::headers` from an ordered key/value list.\n\
             fn header_object<const N: usize>(\n    \
             entries: [(&str, Value); N],\n\
             ) -> Option<Map<String, Value>> {\n    \
             Some(object(entries))\n}\n",
        );
    }
    if needs.any_object() {
        out.push_str(
            "\n/// A `serde_json` object from an ordered key/value list. `Map` is insertion-ordered\n\
             /// here (`preserve_order`), so the argument order is the record's key order.\n\
             fn object<const N: usize>(entries: [(&str, Value); N]) -> Map<String, Value> {\n    \
             entries\n        \
             .into_iter()\n        \
             .map(|(key, value)| (key.to_string(), value))\n        \
             .collect()\n}\n",
        );
    }
}

// =============================================================================
// Literal emitters
// =============================================================================

/// A Rust string literal. Only `"` and `\` need escaping for the printable-ASCII values the
/// catalog carries; anything else is rejected rather than mis-escaped.
fn rust_str(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            c if c.is_control() => {
                // Would need `\u{…}`; no catalog string has one, so fail loudly instead.
                panic!("gen-catalog: refusing to emit control character {c:?} in {value:?}");
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// An `f64` literal. `{:?}` is the shortest round-tripping form *and* always keeps a decimal
/// point, so `10` renders as `10.0` and stays an `f64`.
fn rust_f64(value: f64) -> String {
    format!("{value:?}")
}

/// A `u64` literal, digit-grouped. Rejects a fractional or negative value: TypeBox accepts any
/// `number` for `contextWindow`/`maxTokens` but [`Model`] stores `u64`, so a non-integer in the
/// record must fail the generator rather than truncate silently.
fn rust_u64(value: &Value, at: &str) -> Result<String> {
    let number = value
        .as_u64()
        .with_context(|| format!("{at}: not a non-negative integer ({value})"))?;
    let digits = number.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            grouped.push('_');
        }
        grouped.push(digit);
    }
    Ok(grouped)
}

/// A `Value` literal — no `json!`, whose token tree `rustfmt` reformats unpredictably.
fn rust_value(value: &Value, indent: usize) -> Result<String> {
    Ok(match value {
        Value::Null => "Value::Null".to_string(),
        Value::Bool(b) => format!("Value::Bool({b})"),
        Value::Number(n) => match n.as_u64() {
            Some(u) => format!("Value::from({u}u64)"),
            None => match n.as_i64() {
                Some(i) => format!("Value::from({i}i64)"),
                None => format!(
                    "Value::from({}f64)",
                    rust_f64(n.as_f64().context("number is not representable as f64")?)
                ),
            },
        },
        Value::String(s) => format!("Value::String({}.to_string())", rust_str(s)),
        Value::Array(items) => {
            let mut rendered = Vec::with_capacity(items.len());
            for item in items {
                rendered.push(rust_value(item, indent)?);
            }
            format!("Value::Array(vec![{}])", rendered.join(", "))
        }
        Value::Object(map) => format!("Value::Object(object({}))", rust_entries(map, indent)?),
    })
}

/// The `[(key, value), …]` argument of `object` / `compat_object` / `header_object`.
///
/// One line while it fits `rustfmt`'s `fn_call_width` (60, the default — *not* `max_width`),
/// otherwise wrapped one entry per line, which is how `rustfmt` breaks an over-wide array
/// literal. Emitting the already-formatted form is what makes regeneration a no-op.
fn rust_entries(map: &Map<String, Value>, indent: usize) -> Result<String> {
    let mut entries = Vec::with_capacity(map.len());
    for (key, value) in map {
        entries.push(format!(
            "({}, {})",
            rust_str(key),
            rust_value(value, indent + 4)?
        ));
    }
    let one_line = format!("[{}]", entries.join(", "));
    const FN_CALL_WIDTH: usize = 60;
    if one_line.len() <= FN_CALL_WIDTH {
        return Ok(one_line);
    }
    let inner = " ".repeat(indent + 4);
    let close = " ".repeat(indent);
    Ok(format!(
        "[\n{}\n{close}]",
        entries
            .iter()
            .map(|entry| format!("{inner}{entry},"))
            .collect::<Vec<_>>()
            .join("\n"),
    ))
}
