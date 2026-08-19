//! `pirust` — the headless coding-agent CLI. Port of `main.ts:473-859`'s bootstrap +
//! mode dispatch (spec: `docs/analysis/09-cli-config-spec.md` §15-17), scoped to feat-005:
//! print/json run modes only. Interactive mode, RPC mode and extensions are feat-006/007/
//! feat-012 — see `lib.rs`'s crate docs for the full scope line.
//!
//! # Speed constraints (`plan.md` #18/#19)
//!
//! `--version`, `--help`'s early diagnostics, and parse-error paths run and exit before
//! any `tokio::Runtime` is constructed and before any disk/network I/O. Everything past
//! that point — including plain `--help` and `--list-models`, which Pi itself does not
//! short-circuit (hazard §16.9) — necessarily touches disk (config paths, `auth.json`,
//! `models.json`, migrations) exactly as Pi does, so the runtime is built once we commit
//! to that path, not before.
//!
//! # What is narrowed or deferred this wave (named, not silently dropped)
//!
//! - **Step 1/4 (extension factories, bootstrap `SettingsManager`, http proxy/dispatcher):
//!   dropped entirely.** Pi's bootstrap `SettingsManager` exists only to read
//!   `httpProxy`/configure the dispatcher — both out of scope — so constructing a second,
//!   throwaway `SettingsManager` before the real one adds no behavior here.
//! - **Windows quarantine cleanup, `--export`/HTML, package/config commands,
//!   `PI_STARTUP_BENCHMARK`: not ported** (§18) — `--export` reports the same narrowed
//!   "not supported" error the spec calls for.
//! - **`--mode rpc`: not ported** (feat-012) — reported as a fatal error rather than
//!   silently falling through.
//! - **Project trust (`core/trust-manager.ts`/`core/project-trust.ts`): not ported as a
//!   store.** Pi's own non-interactive fallback is `!hasUI → false` (spec §17.5) — every
//!   run here IS non-interactive, so this wave hard-codes that same conclusion:
//!   `project_trusted = --approve/--no-approve if given, else false`, skipping the
//!   `!hasTrustRequiringProjectResources(cwd) → true` relaxation Pi grants untrusted-but-
//!   harmless directories. This is strictly *more* restrictive than Pi (a directory Pi
//!   would silently trust may here load no project settings) — the safe direction to
//!   default when the real check isn't ported (`AGENTS.md`'s "Security measures" carve-
//!   out). `resolveCliPaths`/the four resource-path lists are moot without a resource
//!   loader (feat-007).
//! - **`--help` printed slightly earlier than hazard §16.9's exact ordering.** Pi builds
//!   the *entire* runtime — including a resolved model — before printing help, tolerating
//!   a model-less `AgentSession`. `sdk.rs`'s `Agent` has no such tolerance (model is
//!   required; see its module docs), so an environment with zero configured models would
//!   otherwise make `pirust --help` exit 1 instead of printing help. Help is therefore
//!   printed right after the model runtime is built (migrations, `auth.json` creation and
//!   model-catalog probing have all already run — the side effects hazard §16.9 actually
//!   cares about) but before `sdk::create_agent_session`'s model requirement can fail.
//! - **No real `SignalRegistry`.** `print_mode::NoSignals` is used, so `SIGTERM`/`SIGHUP`
//!   fall back to the OS default (terminate, not hang) instead of Pi's graceful
//!   dispose-then-exit. Add a real registry (e.g. `tokio::signal`) when interactive mode
//!   (feat-006/007) needs the same seam.
//! - **Session persistence is message-level, end-of-turn** — see `runtime_host.rs`'s
//!   module docs.
//! - **`@file` images are not attached** — see `initial_message.rs`'s module docs.

use std::collections::BTreeSet;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::Arc;

use pirust_coding_agent::args::{self, DiagnosticKind};
use pirust_coding_agent::auth::AuthStorage;
use pirust_coding_agent::catalog;
use pirust_coding_agent::config::{ConfigEnv, VERSION};
use pirust_coding_agent::initial_message::{build_initial_message, process_file_arguments};
use pirust_coding_agent::migrations::{self, MigrationConsole};
use pirust_coding_agent::models::{CreateModelRuntimeOptions, ModelRuntime};
use pirust_coding_agent::print_mode::{self, AppMode, OutputGuard, PrintModeEnv, PrintModeOptions};
use pirust_coding_agent::runtime_host::{
    format_missing_session_cwd_error, missing_session_cwd_issue, SingleTurnRuntimeHost,
    SingleTurnSession,
};
use pirust_coding_agent::sdk::{self, CreateAgentSessionOptions};
use pirust_coding_agent::session::{
    self, HeadlessPrompts, SessionEnv, SessionIo, SessionStream, SessionStyle,
};
use pirust_coding_agent::settings::{SettingsManager, SettingsManagerCreateOptions};

/// A [`session::SessionConsole`]/[`MigrationConsole`] adapter that routes through
/// [`OutputGuard`] rather than a bare `println!`/`eprintln!` — needed because
/// `takeOverStdout` (hazard §16.12) redirects **every** `console.log` in the bootstrap,
/// not just print mode's own output, once `should_take_over_stdout` is true.
struct GuardConsole(Arc<OutputGuard>);

impl session::SessionConsole for GuardConsole {
    fn write(&mut self, stream: SessionStream, _style: SessionStyle, text: &str) {
        match stream {
            SessionStream::Stdout => self.0.log(text),
            SessionStream::Stderr => self.0.error(text),
        }
    }
}

impl MigrationConsole for GuardConsole {
    fn log(&mut self, _style: migrations::ConsoleStyle, text: &str) {
        self.0.log(text);
    }
}

/// `isTruthyEnvFlag` (`main.ts:95-98`) — `1`/`true`/`yes`, case-insensitive on the latter
/// two. Distinct from `model-runtime.ts:152`'s bare `!== undefined` check, which
/// `sdk.rs`'s stream wrapper does not need to replicate this wave (feat-005 has no
/// network refresh at startup to gate — see `models.rs`'s `ModelRuntime::create` docs).
fn is_truthy_env_flag(value: Option<String>) -> bool {
    match value {
        None => false,
        Some(v) if v.is_empty() => false,
        Some(v) => v == "1" || v.to_lowercase() == "true" || v.to_lowercase() == "yes",
    }
}

fn main() {
    let raw_args: Vec<String> = std::env::args().skip(1).collect();

    // Step 6 (`main.ts:509-519`): parseArgs; report diagnostics; exit(1) on any error.
    // Fully synchronous — no tokio runtime yet (speed constraint #18).
    let parsed = args::parse_args(&raw_args);
    for d in &parsed.diagnostics {
        let prefix = match d.kind {
            DiagnosticKind::Error => "Error",
            DiagnosticKind::Warning => "Warning",
        };
        eprintln!("{prefix}: {}", d.message);
    }
    if parsed
        .diagnostics
        .iter()
        .any(|d| d.kind == DiagnosticKind::Error)
    {
        std::process::exit(1);
    }

    // Step 7 (`main.ts:521-524`): --version wins over --help (hazard §16.8) and exits
    // before any I/O.
    if parsed.version == Some(true) {
        println!("{VERSION}");
        std::process::exit(0);
    }

    // Step 8 (`main.ts:526-538`): --export is not ported — narrowed per the module docs.
    if parsed.export.is_some() {
        eprintln!("Error: HTML export is not supported");
        std::process::exit(1);
    }

    // Everything past this point touches disk (config paths, migrations, auth.json,
    // models.json) even for `--help`/`--list-models` (hazard §16.9) — this is where the
    // real work, and the tokio runtime it needs, begins.
    let runtime = tokio::runtime::Runtime::new().expect("failed to start async runtime");
    let exit_code = runtime.block_on(run(parsed));
    std::process::exit(exit_code);
}

async fn run(parsed: args::Args) -> i32 {
    // Step 2 (`main.ts:476-480`): offline-mode env plumbing.
    let offline_mode =
        parsed.offline == Some(true) || is_truthy_env_flag(std::env::var("PIRUST_OFFLINE").ok());
    if offline_mode {
        // Safe here: single-threaded at this point in startup, before any other code
        // reads the environment concurrently.
        std::env::set_var("PIRUST_OFFLINE", "1");
    }

    let stdin_is_tty = std::io::stdin().is_terminal();
    let stdout_is_tty = std::io::stdout().is_terminal();
    let app_mode = print_mode::resolve_app_mode(&parsed, stdin_is_tty, stdout_is_tty);
    let should_take_over = print_mode::should_take_over_stdout(app_mode, &parsed);

    let guard = Arc::new(OutputGuard::from_process());
    if should_take_over {
        guard.take_over_stdout();
    }
    let mut console = GuardConsole(Arc::clone(&guard));

    // Step 10 (`main.ts:546-549`): rpc mode is feat-012, not ported.
    if app_mode == AppMode::Rpc {
        guard.error("Error: --mode rpc is not supported");
        return 1;
    }

    // Step 11 (`main.ts:551-552`).
    if let Err(exit) = session::validate_fork_flags(&parsed, &mut console) {
        return exit.code;
    }
    if let Err(exit) = session::validate_session_id_flags(&parsed, &mut console) {
        return exit.code;
    }

    let cwd = pirust_tools::path_utils::cwd();
    let config_env = ConfigEnv::from_process_env();

    // Step 12 (`main.ts:555-556`).
    let mut migration_console = GuardConsole(Arc::clone(&guard));
    if let Err(error) = migrations::run_migrations(&config_env, &cwd, &mut migration_console) {
        guard.error(&format!("Error: {error}"));
        return 1;
    }

    // Step 13 (`main.ts:558-559`): the real, trusted-by-default settings manager.
    // Project trust: see the module docs — non-interactive always resolves to
    // `--approve`/`--no-approve` if given, else untrusted.
    let project_trusted = parsed.project_trust_override.unwrap_or(false);
    let mut settings_manager = match SettingsManager::create(
        &config_env,
        std::path::Path::new(&cwd),
        SettingsManagerCreateOptions { project_trusted },
    ) {
        Ok(manager) => manager,
        Err(error) => {
            guard.error(&format!("Error: {error}"));
            return 1;
        }
    };
    for settings_error in settings_manager.drain_errors() {
        guard.error(&format!(
            "Warning: (startup session lookup, {}) {}",
            settings_error.scope, settings_error.error
        ));
    }

    // Step 15 (`main.ts:573-578`, spec §5.3): --session-dir > PIRUST_CODING_AGENT_SESSION_DIR
    // > settings. Both CLI and env forms go through the same tilde-expansion helper this
    // wave (a documented narrowing from Pi's two distinct path-normalisation functions —
    // see `plan.md`'s Wave 5 recon).
    let session_dir = match resolve_session_dir(&config_env, &parsed, &settings_manager) {
        Ok(dir) => dir,
        Err(error) => {
            guard.error(&format!("Error: {error}"));
            return 1;
        }
    };

    let session_env = SessionEnv::new(config_env.clone(), cwd.clone());
    let mut session_prompts = HeadlessPrompts;
    let mut session_manager = {
        let mut io = SessionIo {
            console: &mut console,
            prompts: &mut session_prompts,
        };
        match session::create_session_manager(
            &session_env,
            &parsed,
            &cwd,
            session_dir.as_deref(),
            &mut io,
        ) {
            Ok(manager) => manager,
            Err(exit) => return exit.code,
        }
    };

    // Step 16 (`main.ts:579-590`): non-interactive always reports and exits 1 — this wave
    // never reaches the interactive prompt branch at all.
    if let Some(issue) = missing_session_cwd_issue(&session_manager, &cwd) {
        guard.error(&format!(
            "Error: {}",
            format_missing_session_cwd_error(&issue)
        ));
        return 1;
    }

    // Step 17 (`main.ts:592-599`).
    if let Some(name) = &parsed.name {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            guard.error("Error: --name requires a non-empty value");
            return 1;
        }
        if let Err(error) = session_manager.append_session_info(trimmed) {
            guard.error(&format!("Error: {error}"));
            return 1;
        }
    }

    // Step 18 (`main.ts:602-613`): trust store + resource-path lists — stubbed, see the
    // module docs. Nothing to do here beyond what `project_trusted` above already decided.

    // Step 19 (`main.ts:615-739`): build the model runtime (services), anthropic-only.
    let auth_path = match config_env.auth_path() {
        Ok(path) => PathBuf::from(path),
        Err(error) => {
            guard.error(&format!("Error: {error}"));
            return 1;
        }
    };
    let stored_credentials: BTreeSet<String> = match AuthStorage::create(&config_env) {
        Ok(storage) => storage
            .list()
            .into_iter()
            .map(|info| info.provider_id)
            .collect(),
        Err(error) => {
            guard.error(&format!("Error: {error}"));
            return 1;
        }
    };
    let models_path = match config_env.models_path() {
        Ok(path) => path,
        Err(error) => {
            guard.error(&format!("Error: {error}"));
            return 1;
        }
    };
    let model_runtime = match ModelRuntime::create(
        &config_env,
        catalog::builtin_catalog(),
        CreateModelRuntimeOptions {
            models_path: Some(models_path.as_str()),
            stored_credentials,
            process_env: pirust_coding_agent::auth::ProcessEnv::from_process_env(),
        },
    ) {
        Ok(runtime) => runtime,
        Err(error) => {
            guard.error(&format!("Error: {error}"));
            return 1;
        }
    };

    // Step 21 (`main.ts:752-758`): --help, moved earlier per the module docs (sdk.rs
    // requires a resolved model; Pi's AgentSession does not).
    if parsed.help == Some(true) {
        args::print_help(&args::PIRUST, &[], stdout_is_tty);
        return 0;
    }

    // Step 22 (`main.ts:760-764`).
    if let Some(pattern) = &parsed.list_models {
        let search = match pattern {
            args::ListModels::All => None,
            args::ListModels::Pattern(p) => Some(p.as_str()),
        };
        let docs_path = pirust_coding_agent::config::get_docs_path();
        let providers_doc_path = docs_path
            .join("providers.md")
            .to_string_lossy()
            .into_owned();
        let models_doc_path = docs_path.join("models.md").to_string_lossy().into_owned();
        for line in pirust_coding_agent::models::list_models(
            &model_runtime,
            pirust_coding_agent::models::ListModelsOptions {
                search_pattern: search,
                providers_doc_path: &providers_doc_path,
                models_doc_path: &models_doc_path,
            },
        ) {
            match line.stream {
                pirust_coding_agent::models::OutputStream::Stderr => guard.error(&line.text),
                pirust_coding_agent::models::OutputStream::Stdout => guard.log(&line.text),
            }
        }
        return 0;
    }

    // Step 19 continued: --api-key mutates the runtime only (hazard §16.30) — sdk.rs's
    // `runtime_api_key` field carries it straight to the stream wrapper.
    let runtime_api_key = parsed.api_key.clone();

    let settings_manager = Arc::new(settings_manager);
    let session_id = session_manager.get_session_id().to_string();
    let create_result = sdk::create_agent_session(CreateAgentSessionOptions {
        cwd: &cwd,
        model_source: &model_runtime,
        auth_path,
        settings: Arc::clone(&settings_manager),
        cli_provider: parsed.provider.as_deref(),
        cli_model: parsed.model.as_deref(),
        tools: parsed.tools.as_deref(),
        no_tools: parsed.no_tools == Some(true),
        exclude_tools: parsed.exclude_tools.as_deref(),
        session_id: Some(session_id),
        runtime_api_key,
    });

    // Step 27 (`main.ts:800-803`): non-interactive with no model -> error + exit(1). This
    // is `sdk.rs`'s only failure mode (see its module docs), so it doubles as steps 26/27.
    let sdk::CreateAgentSessionResult {
        agent,
        model_fallback_message,
        model,
        thinking_level,
        ..
    } = match create_result {
        Ok(result) => result,
        Err(message) => {
            guard.error(&message);
            return 1;
        }
    };
    if let Some(message) = &model_fallback_message {
        guard.error(&format!("Warning: {message}"));
    }

    // sdk.ts:357-369 — record the resolved model/thinking level. This wave always starts
    // a fresh turn (no session restore, see `sdk.rs`'s module docs), so it is always the
    // "new session" branch.
    if let Err(error) = session_manager.append_model_change(&model.provider.0, &model.id) {
        guard.error(&format!("Warning: failed to record model change: {error}"));
    }
    if let Err(error) = session_manager.append_thinking_level_change(
        pirust_coding_agent::models::thinking_level_as_str(thinking_level),
    ) {
        guard.error(&format!(
            "Warning: failed to record thinking level: {error}"
        ));
    }

    // Step 24 (`main.ts:766-781`): piped stdin (skipped for rpc, which already exited
    // above) + @file text + `messages[0]` -> the initial prompt.
    let stdin_content = if !stdin_is_tty {
        read_piped_stdin()
    } else {
        None
    };
    let processed_files = if parsed.file_args.is_empty() {
        None
    } else {
        match process_file_arguments(&parsed.file_args, &cwd) {
            Ok(files) => Some(files),
            Err(message) => {
                guard.error(&format!("Error: {message}"));
                return 1;
            }
        }
    };
    let mut messages = parsed.messages.clone();
    let (file_text, file_images) = match processed_files {
        Some(files) => (Some(files.text), Some(files.images)),
        None => (None, None),
    };
    let initial = build_initial_message(
        &mut messages,
        file_text.as_deref(),
        file_images,
        stdin_content,
    );

    // Step 26 handled above (step 27); the interactive-only first-time-setup and theme
    // steps (25) are feat-006/007.

    let session = SingleTurnSession::new(agent, session_manager);
    let host = Arc::new(SingleTurnRuntimeHost::new(Arc::clone(&session)));

    let platform = pirust_coding_agent::config::Platform::current();

    // Step 29 (`main.ts:811-858`): interactive mode launches the TUI; rpc
    // already exited above; everything else runs print mode.
    let run_exit_code = if app_mode == AppMode::Interactive {
        run_interactive_mode().await
    } else {
        let print_output_mode = print_mode::to_print_output_mode(app_mode);
        print_mode::run_print_mode(
            host,
            PrintModeOptions {
                mode: print_output_mode,
                messages,
                initial_message: initial.initial_message,
                initial_images: initial.initial_images,
            },
            PrintModeEnv {
                guard: Arc::clone(&guard),
                signals: Arc::new(print_mode::NoSignals),
                platform,
            },
        )
        .await
    };

    guard.restore_stdout();
    run_exit_code
}

/// `runInteractiveMode` (`main.ts:811-858`) — launch the TUI and loop.
///
/// Wave 1 scaffold: prompt via the Editor, echo submissions back into the
/// TUI, quit on Ctrl+D. The real `session.prompt` turn + streaming render is
/// Wave 2 (the `host` is built above and threaded in then).
async fn run_interactive_mode() -> i32 {
    use pirust_coding_agent::interactive_mode::InteractiveMode;

    let terminal = Box::new(pirust_tui::terminal::ProcessTerminal::new());
    let mut mode = InteractiveMode::new(terminal);
    mode.run(|text: String| {
        // Wave 1: the editor round-trips text to the prompt callback.
        // Wave 2 replaces this with `session.prompt(text)` rendered into the
        // TUI's chat container.
        eprintln!("[wave1] submit: {text}");
    });
    0
}

/// `getSessionDir()`-equivalent precedence, `main.ts:573-577` + spec §5.3.
fn resolve_session_dir(
    config_env: &ConfigEnv,
    parsed: &args::Args,
    settings: &SettingsManager,
) -> Result<Option<String>, pirust_coding_agent::config::ConfigPathError> {
    if let Some(dir) = &parsed.session_dir {
        return Ok(Some(config_env.expand_tilde_path(dir)?));
    }
    if let Ok(env_dir) = std::env::var("PIRUST_CODING_AGENT_SESSION_DIR") {
        if !env_dir.is_empty() {
            return Ok(Some(config_env.expand_tilde_path(&env_dir)?));
        }
    }
    settings.get_session_dir(config_env)
}

/// `readPipedStdin()` (`main.ts:58-75`). Only called when `!stdin.isTTY`, matching Pi's
/// own early return. `.trim()` strips a leading BOM in addition to whitespace (hazard
/// §16.19) — `str::trim` does not, so it is stripped explicitly first.
fn read_piped_stdin() -> Option<String> {
    use std::io::Read;
    let mut data = String::new();
    if std::io::stdin().read_to_string(&mut data).is_err() {
        return None;
    }
    let without_bom = data.strip_prefix('\u{feff}').unwrap_or(&data);
    let trimmed = without_bom.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}
