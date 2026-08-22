//! Port of `core/sdk.ts` — assembles a [`pirust_agent_core`] `Agent` from resolved
//! settings, model, tools and session, plus the system prompt.
//!
//! # Scope: one headless turn, not `AgentSession`
//!
//! Pi's `createAgentSession` returns an `AgentSession` (`core/agent-session.ts`, 3283
//! lines) — an event-bus-driven object with subscribe/footer-data/idle-wait/model-
//! cycling machinery for the **interactive TUI**. None of that applies to a headless
//! `-p`/`--json` run, which executes one turn and exits. This module therefore builds
//! only what a one-shot turn needs: a resolved [`pirust_agent_core::agent::Agent`] wired
//! to the Anthropic adapter. `main.rs` (Wave 5) drives it.
//!
//! # What is out of scope this wave (named, not silently dropped)
//!
//! - **Extension-runner hooks** (`transformContext`/`onPayload`/`onResponse`,
//!   `transformHeaders`): `AgentOptions` has a `transform_context` slot (left `None` =
//!   identity), but `onPayload`/`onResponse` have no slot at all yet — `pirust_ai`'s
//!   provider options (`StreamOptions`) don't carry those callbacks either (see its own
//!   `TODO(feat-002 api)`). Wiring them is feat-007's job alongside the extension host.
//! - **`blockImages` message filtering** (`sdk.ts:250-285`): `convert_to_llm` is used
//!   unfiltered. Restore the image→placeholder filter when a real multimodal session
//!   exercises it.
//! - **Session restore** (`sdk.ts:182-199,357-369`): no existing-session model/thinking-
//!   level restore — `session.rs` owns session state and this wave always starts a fresh
//!   turn. `is_continuing` is always `false`.
//! - **`ResourceLoader`** (custom system prompt, skills, context files, extensions):
//!   stubbed to `None`/empty per feat-007.
//! - **Non-Anthropic providers**: the stream wrapper only calls
//!   `pirust_ai::api::anthropic_messages::stream_simple` — feat-005 is anthropic-only.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

use pirust_agent_core::agent::{Agent, AgentOptions};
use pirust_agent_core::harness::messages::convert_to_llm;
use pirust_agent_core::types::{AgentTool, ThinkingLevel};
use pirust_ai::api::{anthropic_messages, SimpleStreamOptions, StreamOptions};
use pirust_ai::stream::{assistant_message_stream, AssistantMessageEventStream};
use pirust_ai::types::event::AssistantMessageEvent;
use pirust_ai::types::ids::StopReason;
use pirust_ai::types::message::AssistantMessage;
use pirust_ai::types::usage::{Cost, Usage};
use pirust_ai::types::Model;
use pirust_tools::{create_all_tool_definitions, create_all_tools, ToolName, ToolRecord};
use tokio_util::sync::CancellationToken;

use crate::auth::{credential_api_key, read_stored_credential};
use crate::auth_guidance::format_no_models_available_message;
use crate::models::{find_initial_model, FindInitialModelOptions, ModelSource};
use crate::provider_attribution::merge_provider_attribution_headers;
use crate::settings::SettingsManager;
use crate::system_prompt::{build_system_prompt, BuildSystemPromptOptions};

/// The builtin tools active by default when neither `--tools` nor `--no-tools` is given
/// (`sdk.ts:240`).
const DEFAULT_ACTIVE_TOOLS: [&str; 4] = ["read", "bash", "edit", "write"];

/// `createAgentSession(options)`'s inputs, narrowed to feat-005 (see module docs).
pub struct CreateAgentSessionOptions<'a> {
    /// Working directory for project-local discovery.
    pub cwd: &'a str,
    /// The composed model/auth view `main.rs` built (catalog + `models.json` + stored
    /// credential presence). Used only for resolution (`find_initial_model`); the actual
    /// credential *value* streamed with comes from `auth_path` below.
    pub model_source: &'a dyn ModelSource,
    /// `~/.pirust/agent/auth.json` (or the `--agent-dir`-relative equivalent).
    pub auth_path: PathBuf,
    pub settings: Arc<SettingsManager>,
    /// `--provider`.
    pub cli_provider: Option<&'a str>,
    /// `--model`.
    pub cli_model: Option<&'a str>,
    /// `--tools` (allowlist). `None` means "use the default active set unless
    /// `no_tools`".
    pub tools: Option<&'a [String]>,
    /// `--no-tools all` or `--no-tools builtin` (`sdk.ts:56`) — both collapse to an empty
    /// *initial* active set; the "all" vs "builtin" distinction only changes the
    /// extension/custom-tool registry gate, which is out of scope this wave (no custom
    /// tools yet).
    pub no_tools: bool,
    /// `--exclude-tools`.
    pub exclude_tools: Option<&'a [String]>,
    /// Forwarded to the provider as `StreamOptions.session_id` and to
    /// `mergeProviderAttributionHeaders`' opencode-session header.
    pub session_id: Option<String>,
    /// `--api-key` (`main.ts:712`, hazard §16.30): a **runtime-only** override that takes
    /// precedence over `auth.json` and never touches disk — matching
    /// `ModelRuntime.check_auth`'s "runtime API key" step, which this wave's `models.rs`
    /// equivalent (`set_runtime_api_key`) also ranks first.
    pub runtime_api_key: Option<String>,
}

/// `CreateAgentSessionResult`, narrowed: no `extensionsResult` (feat-007) and no
/// `session` (`AgentSession`, out of scope — see module docs).
pub struct CreateAgentSessionResult {
    pub agent: Agent,
    /// The full tool registry (`create_all_tools` output) — the `_toolRegistry`
    /// Pi's `setActiveToolsByName` filters through (agent-session.ts:936-955).
    /// `main.rs` hands it to the session so extension `setActiveTools` can map
    /// names → tools.
    pub tool_registry: ToolRecord,
    /// `formatNoModelsAvailableMessage()` when no model resolved; `None` otherwise. Pi's
    /// other fallback text ("Could not restore model …") only fires on session restore,
    /// which is out of scope this wave.
    pub model_fallback_message: Option<String>,
    /// The model actually resolved and wired into `agent`. `Agent` exposes no public
    /// getter for its constructor-time state (`messages()`/`is_streaming()` are the
    /// only readers), so callers that need to display or persist the resolved model
    /// (e.g. `main.rs` writing the session's model-change entry) read it here instead.
    pub model: Model,
    pub thinking_level: ThinkingLevel,
    /// The system prompt built by 4a and wired into `agent`'s initial state.
    pub system_prompt: String,
}

/// `settings.rs`'s [`crate::settings::QueueDeliveryMode`] and `agent-core`'s
/// [`pirust_agent_core::types::QueueMode`] are the same two-value enum ported from two
/// different TS sites (`settings-manager.ts` vs `agent.ts`) — no shared type exists to
/// reuse, so this maps between them.
fn to_agent_queue_mode(
    mode: crate::settings::QueueDeliveryMode,
) -> pirust_agent_core::agent::AgentQueueMode {
    match mode {
        crate::settings::QueueDeliveryMode::All => pirust_agent_core::agent::AgentQueueMode::All,
        crate::settings::QueueDeliveryMode::OneAtATime => {
            pirust_agent_core::agent::AgentQueueMode::OneAtATime
        }
    }
}

fn parse_tool_name(name: &str) -> Option<ToolName> {
    match name {
        "read" => Some(ToolName::Read),
        "bash" => Some(ToolName::Bash),
        "edit" => Some(ToolName::Edit),
        "write" => Some(ToolName::Write),
        "grep" => Some(ToolName::Grep),
        "find" => Some(ToolName::Find),
        "ls" => Some(ToolName::Ls),
        _ => None,
    }
}

/// `initialActiveToolNames` (`sdk.ts:244-246`).
fn initial_active_tool_names(
    tools: Option<&[String]>,
    no_tools: bool,
    exclude_tools: Option<&[String]>,
) -> Vec<String> {
    let base: Vec<String> = if let Some(list) = tools {
        list.to_vec()
    } else if no_tools {
        Vec::new()
    } else {
        DEFAULT_ACTIVE_TOOLS.iter().map(|s| s.to_string()).collect()
    };
    match exclude_tools {
        Some(excluded) => base.into_iter().filter(|n| !excluded.contains(n)).collect(),
        None => base,
    }
}

/// `getSupportedThinkingLevels(model)` (`packages/ai/src/models.ts:663-671`).
fn supported_thinking_levels(model: &Model) -> Vec<ThinkingLevel> {
    if !model.reasoning {
        return vec![ThinkingLevel::Off];
    }
    let map = model.thinking_level_map.clone().unwrap_or_default();
    const EXTENDED: [ThinkingLevel; 7] = [
        ThinkingLevel::Off,
        ThinkingLevel::Minimal,
        ThinkingLevel::Low,
        ThinkingLevel::Medium,
        ThinkingLevel::High,
        ThinkingLevel::Xhigh,
        ThinkingLevel::Max,
    ];
    EXTENDED
        .iter()
        .copied()
        .filter(|level| {
            let mapped = match level {
                ThinkingLevel::Off => &map.off,
                ThinkingLevel::Minimal => &map.minimal,
                ThinkingLevel::Low => &map.low,
                ThinkingLevel::Medium => &map.medium,
                ThinkingLevel::High => &map.high,
                ThinkingLevel::Xhigh => &map.xhigh,
                ThinkingLevel::Max => &map.max,
            };
            match mapped {
                Some(None) => false, // explicit `null` — unsupported
                None => matches!(
                    level,
                    ThinkingLevel::Off
                        | ThinkingLevel::Minimal
                        | ThinkingLevel::Low
                        | ThinkingLevel::Medium
                        | ThinkingLevel::High
                ),
                Some(Some(_)) => true,
            }
        })
        .collect()
}

/// `clampThinkingLevel(model, level)` (`packages/ai/src/models.ts:674-693`). Lives here
/// rather than in `pirust-ai`: this wave's only consumer. Move it if a second one
/// appears (e.g. an interactive `/model` command, feat-007).
fn clamp_thinking_level(model: &Model, level: ThinkingLevel) -> ThinkingLevel {
    const EXTENDED: [ThinkingLevel; 7] = [
        ThinkingLevel::Off,
        ThinkingLevel::Minimal,
        ThinkingLevel::Low,
        ThinkingLevel::Medium,
        ThinkingLevel::High,
        ThinkingLevel::Xhigh,
        ThinkingLevel::Max,
    ];
    let available = supported_thinking_levels(model);
    if available.contains(&level) {
        return level;
    }
    let Some(requested_index) = EXTENDED.iter().position(|&l| l == level) else {
        return available.first().copied().unwrap_or(ThinkingLevel::Off);
    };
    for candidate in &EXTENDED[requested_index..] {
        if available.contains(candidate) {
            return *candidate;
        }
    }
    for candidate in EXTENDED[..requested_index].iter().rev() {
        if available.contains(candidate) {
            return *candidate;
        }
    }
    available.first().copied().unwrap_or(ThinkingLevel::Off)
}

/// The `streamFn` closure sdk.ts builds inline (`:296-325`), dispatched by `model.api`
/// (the feat-008 routing seam). Resolves the credential fresh on every call
/// ("important for expiring tokens", `agent-loop.ts:445`'s comment on `get_api_key` —
/// done here instead of via that hook because header/timeout resolution needs the same
/// settings read anyway).
fn build_stream_fn(
    auth_path: PathBuf,
    settings: Arc<SettingsManager>,
    session_id: Option<String>,
    runtime_api_key: Option<String>,
) -> pirust_agent_core::agent_loop::StreamFn {
    Arc::new(move |model, context, options, _token: CancellationToken| {
        // `--api-key` wins outright (hazard §16.30) — no stored-credential lookup, no
        // ambient-env fallback, matching `ModelRuntime.check_auth`'s ranking.
        let provider = model.provider.0.as_str();
        let env: BTreeMap<String, String> = std::env::vars().collect();
        let api_key = match &runtime_api_key {
            Some(key) => Some(key.clone()),
            None => {
                let credential = read_stored_credential(provider, &auth_path);
                credential_api_key(credential.as_ref(), &env)
                    .or_else(|| pirust_ai::auth::resolve_env_api_key(provider, &env))
            }
        };
        let headers =
            merge_provider_attribution_headers(&model, &settings, session_id.as_deref(), &[]);

        let retry = settings.get_provider_retry_settings();
        let http_idle_timeout_ms = settings
            .get_http_idle_timeout_ms()
            .unwrap_or(crate::settings::DEFAULT_HTTP_IDLE_TIMEOUT_MS);
        // "SDKs treat timeout=0 as 0ms (immediate timeout), not 'no timeout'" (`sdk.ts:300-302`).
        let effective_timeout_ms = if http_idle_timeout_ms == 0.0 {
            2_147_483_647.0
        } else {
            http_idle_timeout_ms
        };
        let timeout_ms = retry.timeout_ms.unwrap_or(effective_timeout_ms);
        let websocket_connect_timeout_ms =
            settings.get_websocket_connect_timeout_ms().unwrap_or(None);

        let opts = SimpleStreamOptions {
            base: StreamOptions {
                api_key,
                headers,
                timeout_ms: Some(timeout_ms as u64),
                websocket_connect_timeout_ms: websocket_connect_timeout_ms.map(|v| v as u64),
                max_retries: retry.max_retries.map(|v| v as u32),
                max_retry_delay_ms: Some(retry.max_retry_delay_ms as u64),
                session_id: session_id.clone(),
                ..options.base
            },
            ..options
        };
        match model.api.0.as_str() {
            pirust_ai::types::ids::known_api::ANTHROPIC_MESSAGES => {
                anthropic_messages::stream_simple(&model, &context, Some(opts))
            }
            pirust_ai::types::ids::known_api::OPENAI_COMPLETIONS => {
                pirust_ai::api::openai_completions::stream_simple(&model, &context, Some(opts))
            }
            other => provider_error_stream(
                &model,
                &format!("No API provider registered for api: {other}"),
            ),
        }
    })
}

/// A single-event error stream for an unknown `model.api` (mirrors agent-core's private
/// `error_stream`, which is not exported).
fn provider_error_stream(model: &Model, message: &str) -> AssistantMessageEventStream {
    let (mut sink, stream) = assistant_message_stream();
    let error = AssistantMessage {
        role: pirust_ai::types::message::AssistantRole::Assistant,
        content: Vec::new(),
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: Some(model.id.clone()),
        response_model: None,
        diagnostics: None,
        usage: Usage {
            input: 0,
            output: 0,
            cache_read: 0,
            cache_write: 0,
            total_tokens: Some(0),
            cost: Cost {
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
                total: 0.0,
            },
            cache_write1h: None,
            reasoning: None,
        },
        stop_reason: StopReason::Error,
        timestamp: now_millis(),
        response_id: None,
        raw_stop_reason: None,
        error_message: Some(message.to_string()),
        end_turn: None,
    };
    sink.push(AssistantMessageEvent::Error {
        reason: StopReason::Error,
        error,
    });
    sink.end(None);
    stream
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// `createAgentSession(options)` (`sdk.ts:164-393`), narrowed per the module docs.
pub fn create_agent_session(
    options: CreateAgentSessionOptions<'_>,
) -> Result<CreateAgentSessionResult, String> {
    let stream_fn = build_stream_fn(
        options.auth_path.clone(),
        options.settings.clone(),
        options.session_id.clone(),
        options.runtime_api_key.clone(),
    );
    assemble_agent_session(options, stream_fn)
}

/// The pure core of [`create_agent_session`], with the provider [`StreamFn`] injected
/// rather than built from `auth.json`/settings — this is what
/// `tests/sdk_canned_turn.rs` drives with a [`pirust_ai::providers::faux::Faux`] stream
/// function, proving 4a-4e wire together without a real network call.
pub fn assemble_agent_session(
    options: CreateAgentSessionOptions<'_>,
    stream_fn: pirust_agent_core::agent_loop::StreamFn,
) -> Result<CreateAgentSessionResult, String> {
    let cwd = options.cwd;

    // --- tools (sdk.ts:240-246, then AgentSession's _rebuildSystemPrompt :1009-1023) ---
    let active_names =
        initial_active_tool_names(options.tools, options.no_tools, options.exclude_tools);
    let defs = create_all_tool_definitions(cwd, None);
    let impls = create_all_tools(cwd, None);

    let mut tools: Vec<Arc<dyn AgentTool>> = Vec::new();
    let mut tool_snippets: HashMap<String, String> = HashMap::new();
    let mut prompt_guidelines: Vec<String> = Vec::new();
    let mut selected_tool_names: Vec<String> = Vec::new();

    for name in &active_names {
        let Some(tool_name) = parse_tool_name(name) else {
            // Not a builtin: `_toolRegistry.has(name)` would only be true for a
            // custom/extension tool, which is feat-007 — dropped, matching
            // `validToolNames = toolNames.filter(name => registry.has(name))`.
            continue;
        };
        if let Some((_, def)) = defs.iter().find(|(t, _)| *t == tool_name) {
            if let Some(snippet) = &def.prompt_snippet {
                tool_snippets.insert(name.clone(), snippet.clone());
            }
            if let Some(guidelines) = &def.prompt_guidelines {
                prompt_guidelines.extend(guidelines.iter().cloned());
            }
        }
        if let Some((_, tool)) = impls.iter().find(|(t, _)| *t == tool_name) {
            tools.push(tool.clone());
        }
        selected_tool_names.push(name.clone());
    }

    let system_prompt = build_system_prompt(&BuildSystemPromptOptions {
        custom_prompt: None,
        selected_tools: Some(&selected_tool_names),
        tool_snippets: Some(&tool_snippets),
        prompt_guidelines: Some(&prompt_guidelines),
        append_system_prompt: None,
        cwd,
        context_files: None,
    });
    let result_system_prompt = system_prompt.clone();

    // --- model (sdk.ts:187-238) ---
    let resolved = find_initial_model(
        FindInitialModelOptions {
            cli_provider: options.cli_provider,
            cli_model: options.cli_model,
            scoped_models: &[],
            is_continuing: false,
            default_provider: options.settings.get_default_provider(),
            default_model_id: options.settings.get_default_model(),
            default_thinking_level: options.settings.get_default_thinking_level(),
        },
        options.model_source,
    )?;

    let model = resolved.model;
    let model_fallback_message = if model.is_none() {
        Some(format_no_models_available_message())
    } else {
        None
    };
    let thinking_level = match &model {
        None => ThinkingLevel::Off,
        Some(m) => clamp_thinking_level(m, resolved.thinking_level),
    };
    let Some(model) = model else {
        return Err(model_fallback_message.unwrap_or_default());
    };
    let result_model = model.clone();

    // --- assemble (sdk.ts:289-355) ---
    let settings = options.settings;

    let agent_options = AgentOptions {
        system_prompt,
        model,
        thinking_level,
        tools,
        messages: Vec::new(),
        convert_to_llm: Some(Arc::new(move |messages| {
            Box::pin(async move { convert_to_llm(&messages) })
        })),
        transform_context: None,
        stream_fn: Some(stream_fn),
        get_api_key: None,
        before_tool_call: None,
        after_tool_call: None,
        steering_mode: to_agent_queue_mode(settings.get_steering_mode()),
        follow_up_mode: to_agent_queue_mode(settings.get_follow_up_mode()),
        session_id: options.session_id,
        tool_execution: pirust_agent_core::types::ToolExecutionMode::Parallel,
    };

    Ok(CreateAgentSessionResult {
        agent: Agent::new(agent_options),
        tool_registry: impls,
        model_fallback_message,
        model: result_model,
        thinking_level,
        system_prompt: result_system_prompt,
    })
}
