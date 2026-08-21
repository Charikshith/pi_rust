//! `xtask` — repo build tooling.
//!
//! Hosts `gen-catalog`, which emits `crates/pirust-coding-agent/src/catalog.rs` — the full
//! builtin model catalog (all 40 providers / 1345 models as of the 0.84.2 oracle) — from:
//!
//! 1. **Models**: the checked-in generator output `../pi/packages/ai/src/providers/data/*.json`
//!    (40 files, one per provider, `{api: {modelId: Model}}` nested, sha256-pinned by
//!    `data/.manifest.json`). This is what real Pi's `generate-models.ts` writes and what
//!    `providers/<provider>.models.ts` loads via `flattenModelCatalog`.
//! 2. **Provider metadata** (`name`, `baseUrl`, auth flags): the `providers` array of the
//!    `builtinCatalogFingerprint` record of `tests/fixtures/pi/cli/models.cases.jsonl`,
//!    captured from real Pi 0.84.2's `ModelRuntime.getProviders()`.
//! 3. **`api_key_env`**: the standard `envApiKeyAuth(name, [ENV])` args read from the 0.84.2
//!    provider sources (the env-var precedence each provider's inherited api-key auth
//!    consults). Not on the runtime object, so it is a checked-in table here; see
//!    [`API_KEY_ENV`].
//!
//! # Usage
//!
//! ```text
//! cargo xtask gen-catalog            # regenerate crates/pirust-coding-agent/src/catalog.rs
//! cargo xtask gen-catalog --check    # exit 1 if the checked-in file is not what we'd emit
//! ```
//!
//! The emitted text is already `rustfmt`-clean and fully ordered, so regenerating an
//! up-to-date tree is a byte-for-byte no-op.
//!
//! This is *not* a port of `generate-models.ts`'s fetch/transform logic (that network +
//! quirk pipeline stays in Pi; porting it is a separate wave). This generator consumes the
//! **output** of that pipeline — the checked-in data files — which is what the spec's §8.4
//! option (b/c) describes.

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

/// The fixture record the provider metadata comes from, relative to the repo root.
const FIXTURE: &str = "tests/fixtures/pi/cli/models.cases.jsonl";
/// The record inside [`FIXTURE`] (its `fn` field).
const RECORD: &str = "builtinCatalogFingerprint";
/// The generated file, relative to the repo root.
const OUTPUT: &str = "crates/pirust-coding-agent/src/catalog.rs";
/// Real Pi's generated catalog data, relative to the repo root (the oracle checkout).
const DATA_DIR: &str = "../pi/packages/ai/src/providers/data";

/// Per-provider `api_key_env` — the env-var precedence order each provider's inherited
/// api-key auth consults (`envApiKeyAuth(name, envVars)` args in the 0.84.2 provider
/// sources). Not exposed on the runtime object, so this table is the generator's own
/// checked-in capture of the `.ts` sources, verified this session. Providers absent here
/// have custom/non-env auth resolution (bedrock IAM, cloudflare, vertex ADC, copilot,
/// codex, kimi, openrouter oauth, xai) and get an empty env list, which matches that
/// their last-resort env check is provider-specific (deferred to the auth wave).
const API_KEY_ENV: &[(&str, &[&str])] = &[
    ("ant-ling", &["ANT_LING_API_KEY"]),
    ("anthropic", &["ANTHROPIC_OAUTH_TOKEN", "ANTHROPIC_API_KEY"]),
    ("azure-openai-responses", &["AZURE_OPENAI_API_KEY"]),
    ("baseten", &["BASETEN_API_KEY"]),
    ("cerebras", &["CEREBRAS_API_KEY"]),
    ("deepseek", &["DEEPSEEK_API_KEY"]),
    ("fireworks", &["FIREWORKS_API_KEY"]),
    ("google", &["GEMINI_API_KEY"]),
    ("groq", &["GROQ_API_KEY"]),
    ("huggingface", &["HF_TOKEN"]),
    ("minimax", &["MINIMAX_API_KEY"]),
    ("minimax-cn", &["MINIMAX_CN_API_KEY"]),
    ("mistral", &["MISTRAL_API_KEY"]),
    ("moonshotai", &["MOONSHOT_API_KEY"]),
    ("moonshotai-cn", &["MOONSHOT_API_KEY"]),
    ("nvidia", &["NVIDIA_API_KEY"]),
    ("openai", &["OPENAI_API_KEY"]),
    ("opencode", &["OPENCODE_API_KEY"]),
    ("opencode-go", &["OPENCODE_API_KEY"]),
    ("qwen-token-plan", &["QWEN_TOKEN_PLAN_API_KEY"]),
    ("qwen-token-plan-cn", &["QWEN_TOKEN_PLAN_CN_API_KEY"]),
    ("qwen-token-plan-individual", &["QWEN_TOKEN_PLAN_API_KEY"]),
    ("radius", &["RADIUS_API_KEY"]),
    ("together", &["TOGETHER_API_KEY"]),
    ("vercel-ai-gateway", &["AI_GATEWAY_API_KEY"]),
    ("xiaomi", &["XIAOMI_API_KEY"]),
    ("xiaomi-token-plan-ams", &["XIAOMI_TOKEN_PLAN_AMS_API_KEY"]),
    ("xiaomi-token-plan-cn", &["XIAOMI_TOKEN_PLAN_CN_API_KEY"]),
    ("xiaomi-token-plan-sgp", &["XIAOMI_TOKEN_PLAN_SGP_API_KEY"]),
    ("zai", &["ZAI_API_KEY"]),
    ("zai-coding-cn", &["ZAI_CODING_CN_API_KEY"]),
];

/// A provider's static metadata, from the oracle fingerprint's `providers` array.
struct ProviderMeta {
    id: String,
    name: String,
    base_url: Option<String>,
    has_api_key_auth: bool,
    has_oauth_auth: bool,
}

fn gen_catalog(args: &[String]) -> Result<()> {
    let check_only = match args {
        [] => false,
        [flag] if flag == "--check" => true,
        _ => bail!("gen-catalog takes no arguments except --check"),
    };

    let root = repo_root();
    let meta = read_provider_metadata(&root.join(FIXTURE))?;
    let models_by_provider = read_models(&root.join(DATA_DIR))?;
    let mut rendered = render(&meta, &models_by_provider)?;
    // The generator's own line-breaking is `rustfmt`-*informed* but not identical to a real
    // `rustfmt` pass (edge cases: a one-entry compat over `fn_call_width`, an over-wide
    // `base_url` string, an empty `vec!`). Running rustfmt on the emitted file makes the
    // checked-in artifact canonical and `--check` idempotent regardless of those.
    rendered = format_rust(&rendered)?;

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

/// The `providers` array of the fingerprint record — every builtin provider's static shape.
fn read_provider_metadata(path: &Path) -> Result<Vec<ProviderMeta>> {
    let record = read_record(path)?;
    let providers = record["providers"]
        .as_array()
        .with_context(|| format!("{RECORD}.providers is missing"))?;
    let mut metas = Vec::with_capacity(providers.len());
    for value in providers {
        metas.push(ProviderMeta {
            id: value["id"]
                .as_str()
                .with_context(|| format!("{RECORD}.providers[].id missing"))?
                .to_string(),
            name: value["name"]
                .as_str()
                .with_context(|| format!("{RECORD}.providers[].name missing"))?
                .to_string(),
            base_url: value["baseUrl"].as_str().map(str::to_string),
            has_api_key_auth: value["hasApiKeyAuth"]
                .as_bool()
                .with_context(|| format!("{RECORD}.providers[].hasApiKeyAuth missing"))?,
            has_oauth_auth: value["hasOauthAuth"]
                .as_bool()
                .with_context(|| format!("{RECORD}.providers[].hasOauthAuth missing"))?,
        });
    }
    if metas.is_empty() {
        bail!("{RECORD}.providers is empty");
    }
    Ok(metas)
}

/// Read every `<provider>.json` data file and flatten it the way `flattenModelCatalog` does:
/// `Object.assign({}, ...Object.values(groups))` — merge all `{api: {modelId: Model}}`
/// groups into one flat `{modelId: Model}` map per provider, in api-group order then
/// insertion order.
fn read_models(dir: &Path) -> Result<Vec<(String, Vec<Value>)>> {
    if !dir.is_dir() {
        bail!(
            "{}: not a directory — the oracle checkout ../pi must be present (0.84.2)",
            dir.display()
        );
    }
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("read {}", dir.display()))?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| {
            // Only the per-provider catalog files; `.manifest.json` is the generator's
            // integrity manifest, not a catalog.
            path.extension().is_some_and(|e| e == "json")
                && path.file_name().is_some_and(|n| n != ".manifest.json")
        })
        .collect();
    files.sort();

    let mut out = Vec::with_capacity(files.len());
    for path in files {
        let provider = path
            .file_stem()
            .expect("file stem")
            .to_string_lossy()
            .to_string();
        let text =
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        let value: Value =
            serde_json::from_str(&text).with_context(|| format!("{}: not JSON", path.display()))?;
        let groups = value
            .as_object()
            .with_context(|| format!("{}: not an object", path.display()))?;
        // flattenModelCatalog: Object.assign({}, ...Object.values(groups))
        let mut flat: Vec<Value> = Vec::new();
        for group in groups.values() {
            let models = group
                .as_object()
                .with_context(|| format!("{}: api group is not an object", path.display()))?;
            for model in models.values() {
                flat.push(model.clone());
            }
        }
        if flat.is_empty() {
            bail!("{}: no models", path.display());
        }
        out.push((provider, flat));
    }
    println!(
        "gen-catalog: {} providers loaded from {}",
        out.len(),
        dir.display()
    );
    Ok(out)
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
    cost_tier: bool,
}

impl Needs {
    fn any_object(&self) -> bool {
        self.compat_object || self.header_object
    }
}

/// Run `rustfmt` over the emitted catalog source so the checked-in file is canonical.
///
/// The workspace's `rustfmt.toml`/`Cargo.toml` settings apply (imports_layout Mixed, etc.);
/// the file parses standalone so `--edition 2021` suffices. Fall back to the unformatted
/// text if rustfmt is unavailable rather than hard-failing the whole task.
fn format_rust(source: &str) -> Result<String> {
    let bin = std::process::Command::new("rustfmt")
        .arg("--edition")
        .arg("2021")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn();
    let mut child = match bin {
        Ok(child) => child,
        Err(error) => {
            println!("gen-catalog: rustfmt unavailable ({error}); emitting unformatted text");
            return Ok(source.to_string());
        }
    };
    use std::io::Write as _;
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(source.as_bytes())
        .context("write to rustfmt stdin")?;
    let output = child.wait_with_output().context("wait for rustfmt")?;
    if !output.status.success() {
        bail!(
            "rustfmt failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    String::from_utf8(output.stdout).context("rustfmt output is not UTF-8")
}

/// The whole of `catalog.rs`, ready to write. Deterministic: every value and every ordering
/// comes from the data files and the fingerprint record.
fn render(metas: &[ProviderMeta], models_by_provider: &[(String, Vec<Value>)]) -> Result<String> {
    // Models first: they decide the `use` list and which helpers are emitted.
    let mut needs = Needs::default();
    let mut providers_fn = String::new();
    let mut total_models = 0usize;
    for meta in metas {
        let models = models_by_provider
            .iter()
            .find(|(id, _)| id == &meta.id)
            .map(|(_, models)| models.as_slice())
            .unwrap_or(&[]);
        total_models += models.len();
        render_provider(&mut providers_fn, meta, models, &mut needs)?;
    }

    let mut out = String::new();
    render_header(&mut out, metas.len(), total_models);
    out.push('\n');
    let mut types = vec![
        "Api",
        "Modality",
        "Model",
        "ModelCost",
        "ModelCostRates",
        "ProviderId",
    ];
    if needs.cost_tier {
        types.push("ModelCostTier");
    }
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
    render_catalog_fn(&mut out, metas);
    out.push_str(&providers_fn);
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

fn render_header(out: &mut String, provider_count: usize, model_count: usize) {
    out.push_str(&format!(
        "//! The builtin model catalog — **GENERATED, do not edit**.\n\
         //!\n\
         //! Run `cargo xtask gen-catalog` to regenerate. Every literal below comes from\n\
         //! real Pi 0.84.2's generated catalog data (`@earendil-works/pi-ai/providers/data/*.json`,\n\
         //! sha256-pinned by `.manifest.json`) plus the `{RECORD}` record of `{FIXTURE}`.\n\
         //!\n\
         //! {provider_count} providers / {model_count} models. Provider `name`/`baseUrl`/auth flags are the\n\
         //! fingerprint's `providers` array; `api_key_env` is the `envApiKeyAuth(name, [ENV])`\n\
         //! argument table in `xtask/src/main.rs` (the 0.84.2 provider sources).\n\
         //!\n\
         //! Pi builds this table at build time via `scripts/generate-models.ts` fetching\n\
         //! models.dev / OpenRouter / Vercel AI Gateway; here the generator consumes that\n\
         //! pipeline's **output** (the data files) rather than re-running the network fetch.\n\
         //! A model's `api` field names the wire protocol that streams it; providers whose\n\
         //! `api` adapter is not yet ported advertise models that cannot stream until the\n\
         //! feat-008 adapter waves land (matching real Pi, which advertises all of them).\n"
    ));
}

fn render_catalog_fn(out: &mut String, metas: &[ProviderMeta]) {
    out.push_str(&format!(
        "\n/// The builtin catalog `ModelRuntime::create` is handed — {provider_count} providers.\n\
         ///\n\
         /// A plain constructor rather than a `static`: [`Model`] and [`ProviderDescriptor`] are\n\
         /// `String`/`Vec`/`Value`-shaped, so no `const` form exists, and `ModelRuntime::create`\n\
         /// takes the catalog **by value** — a `LazyLock` would only add a clone of the same\n\
         /// allocations. There is no parsing here at any point, which is the whole point of\n\
         /// generating Rust instead of embedding the data JSON.\n\
         pub fn builtin_catalog() -> ModelCatalog {{\n    \
         ModelCatalog::new(vec![\n",
        provider_count = metas.len(),
    ));
    for meta in metas {
        let _ = writeln!(out, "        {}_provider(),", rust_ident(&meta.id));
    }
    out.push_str("    ])\n}\n");
}

/// A provider's `fn <id>_provider() -> ProviderDescriptor`.
fn render_provider(
    out: &mut String,
    meta: &ProviderMeta,
    models: &[Value],
    needs: &mut Needs,
) -> Result<()> {
    let provider_id = &meta.id;
    let env = API_KEY_ENV
        .iter()
        .find(|(id, _)| id == provider_id)
        .map(|(_, env)| *env)
        .unwrap_or(&[]);
    let ident = rust_ident(provider_id);
    out.push_str(&format!(
        "\n/// Pi's builtin `{provider_id}` provider — {} models.\n\
         ///\n\
         /// `name`/`baseUrl`/auth flags are the fingerprint's `providers` array; `api_key_env`\n\
         /// is the `envApiKeyAuth` table in the generator. `base_url: None` for providers whose\n\
         /// `baseUrl` is per-request (bedrock, azure) — matching the oracle.\n\
         fn {ident}_provider() -> ProviderDescriptor {{\n    \
         ProviderDescriptor {{\n        \
         id: {}.to_string(),\n        \
         name: {}.to_string(),\n        \
         base_url: {},\n        \
         models: {ident}_models(),\n        \
         api_key_env: {},\n        \
         has_api_key_auth: {},\n        \
         has_oauth_auth: {},\n    \
         }}\n}}\n",
        models.len(),
        rust_str(provider_id),
        rust_str(&meta.name),
        match &meta.base_url {
            Some(url) => format!("Some({}.to_string())", rust_str(url)),
            None => "None".to_string(),
        },
        env_list(env),
        meta.has_api_key_auth,
        meta.has_oauth_auth,
    ));
    render_models_fn(out, provider_id, models, needs)?;
    Ok(())
}

/// `api_key_env: vec![...]` — empty for providers with custom/non-env auth.
fn env_list(env: &[&str]) -> String {
    if env.is_empty() {
        "Vec::new()".to_string()
    } else {
        format!(
            "vec![{}]",
            env.iter()
                .map(|name| rust_str(name).to_string() + ".to_string()")
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

fn render_models_fn(
    out: &mut String,
    provider_id: &str,
    models: &[Value],
    needs: &mut Needs,
) -> Result<()> {
    let ident = rust_ident(provider_id);
    out.push_str(&format!(
        "\n/// The {} builtin `{provider_id}` models, in catalog order — which is load-bearing: it\n\
         /// seeds `getModels()`' order and therefore `availableModels[0]` in step 4 of\n\
         /// `find_initial_model`.\n\
         fn {ident}_models() -> Vec<Model> {{\n    \
         vec![\n",
        models.len(),
    ));
    for (index, model) in models.iter().enumerate() {
        render_model(out, provider_id, model, index, needs)?;
    }
    out.push_str("    ]\n}\n");
    Ok(())
}

fn render_model(
    out: &mut String,
    provider_id: &str,
    model: &Value,
    index: usize,
    needs: &mut Needs,
) -> Result<()> {
    let at = |field: &str| format!("{provider_id}.models[{index}].{field}");

    let id = model["id"].as_str().with_context(|| at("id"))?;
    let name = model["name"].as_str().with_context(|| at("name"))?;
    let api = model["api"].as_str().with_context(|| at("api"))?;
    let provider = model["provider"].as_str().with_context(|| at("provider"))?;
    if provider != provider_id {
        bail!(
            "{}: model.provider {provider:?} != data file {provider_id:?}",
            at("provider")
        );
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
    render_cost(out, &model["cost"], &at("cost"), needs)?;
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

fn render_cost(out: &mut String, value: &Value, at: &str, needs: &mut Needs) -> Result<()> {
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
        Some(Value::Array(tiers)) => {
            if tiers.is_empty() {
                bail!("{at}.tiers: empty array");
            }
            needs.cost_tier = true;
            out.push_str("                tiers: Some(vec![\n");
            for (index, tier) in tiers.iter().enumerate() {
                let tier_at = format!("{at}.tiers[{index}]");
                let tier = tier
                    .as_object()
                    .with_context(|| format!("{tier_at}: not an object"))?;
                for key in tier.keys() {
                    if !matches!(
                        key.as_str(),
                        "input" | "output" | "cacheRead" | "cacheWrite" | "inputTokensAbove"
                    ) {
                        bail!("{tier_at}: unknown tier field {key:?}");
                    }
                }
                out.push_str("                    ModelCostTier {\n");
                out.push_str("                        rates: ModelCostRates {\n");
                for (json_key, field) in [
                    ("input", "input"),
                    ("output", "output"),
                    ("cacheRead", "cache_read"),
                    ("cacheWrite", "cache_write"),
                ] {
                    let rate = tier
                        .get(json_key)
                        .and_then(Value::as_f64)
                        .with_context(|| format!("{tier_at}.{json_key}: not a number"))?;
                    let _ = writeln!(
                        out,
                        "                            {field}: {},",
                        rust_f64(rate)
                    );
                }
                out.push_str("                        },\n");
                let _ = writeln!(
                    out,
                    "                        input_tokens_above: {},",
                    rust_u64(
                        &tier["inputTokensAbove"],
                        &format!("{tier_at}.inputTokensAbove")
                    )?
                );
                out.push_str("                    },\n");
            }
            out.push_str("                ]),\n");
        }
        // The data is the contract, so refuse rather than emit an untested shape.
        Some(_) => bail!("{at}.tiers: expected an array — extend gen-catalog"),
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

/// A Rust identifier from a kebab-case provider id (`amazon-bedrock` → `amazon_bedrock`).
fn rust_ident(value: &str) -> String {
    value.replace('-', "_")
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
