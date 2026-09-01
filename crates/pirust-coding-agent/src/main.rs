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

    // Step 11 (`main.ts:551-552`).
    if let Err(exit) = session::validate_fork_flags(&parsed, &mut console) {
        return exit.code;
    }
    if let Err(exit) = session::validate_session_id_flags(&parsed, &mut console) {
        return exit.code;
    }

    // feat-012: `@file` args are not supported in RPC mode (feature_list.json's own scope
    // line for this guard) — RPC's initial content comes entirely from `prompt` commands
    // over the wire, never from CLI-supplied file arguments.
    if app_mode == AppMode::Rpc && !parsed.file_args.is_empty() {
        guard.error("Error: @file args are not supported in RPC mode");
        return 1;
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
    //
    // Steps 15-17 (session-dir resolution, the v3 `SessionManager`, the missing-cwd check,
    // `--name`) are SKIPPED for `--mode rpc` (feat-012 Wave 3): RPC sessions are built on
    // `AgentHarness`'s v4 session tree, not the v3 `SessionManager` every other mode uses —
    // see `sdk::assemble_agent_harness_session`'s module docs for why the two don't share a
    // session file this wave. `session_manager` is therefore `None` for RPC and `Some` (and
    // only ever unwrapped) for every other mode.
    let mut session_manager = if app_mode == AppMode::Rpc {
        None
    } else {
        let session_dir = match resolve_session_dir(&config_env, &parsed, &settings_manager) {
            Ok(dir) => dir,
            Err(error) => {
                guard.error(&format!("Error: {error}"));
                return 1;
            }
        };

        let session_env = SessionEnv::new(config_env.clone(), cwd.clone());
        let mut session_prompts = HeadlessPrompts;
        let session_manager = {
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

        // Step 16 (`main.ts:579-590`): non-interactive always reports and exits 1 — this
        // wave never reaches the interactive prompt branch at all.
        if let Some(issue) = missing_session_cwd_issue(&session_manager, &cwd) {
            guard.error(&format!(
                "Error: {}",
                format_missing_session_cwd_error(&issue)
            ));
            return 1;
        }

        Some(session_manager)
    };

    // Step 17 (`main.ts:592-599`). The empty-value check applies to every mode; the actual
    // append is a no-op for `--mode rpc` (no `session_manager` this wave, see above).
    if let Some(name) = &parsed.name {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            guard.error("Error: --name requires a non-empty value");
            return 1;
        }
        if let Some(session_manager) = session_manager.as_mut() {
            if let Err(error) = session_manager.append_session_info(trimmed) {
                guard.error(&format!("Error: {error}"));
                return 1;
            }
        }
    }

    // Step 18 (`main.ts:602-613`): trust store + resource-path lists — stubbed, see the
    // module docs. Nothing to do here beyond what `project_trusted` above already decided.

    // Step 19 (`main.ts:615-739`): build the model runtime (services), with the full
    // generated builtin catalog (40 providers / 1306 models); only the anthropic-messages api
    // adapter streams yet (feat-008 adapter waves land later).
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
            // `docs/tui-design-samples.html` §1 "Missing configuration": in an
            // interactive terminal this must be an actionable in-TUI screen
            // naming the file to edit, not a bare line on stderr — the spec's
            // acceptance bar is "actionable error · no stack trace · exit
            // remains available". Non-interactive callers (pipes, --print, CI)
            // still get the plain stderr line, which is what they can parse.
            if app_mode == AppMode::Interactive && stdout_is_tty {
                guard.restore_stdout();
                return run_setup_help_screen(&models_path, &format!("{error}"));
            }
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

    // feat-012 Wave 3: `--mode rpc` builds an `AgentHarness` over an in-memory v4 session
    // (no on-disk RPC session file yet — see `sdk::assemble_agent_harness_session`'s module
    // docs) and hands off to the RPC command loop instead of print/interactive mode.
    if app_mode == AppMode::Rpc {
        let rpc_session_id = pirust_agent_core::harness::session::uuid::create_session_id();
        let created_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let storage = Arc::new(
            pirust_agent_core::harness::session::v4::memory::InMemorySessionStorage::new(
                pirust_agent_core::harness::session::v4::types::SessionMetadata {
                    id: rpc_session_id.clone(),
                    created_at,
                    parent_session_id: None,
                },
            ),
        );
        let v4_session = pirust_agent_core::harness::session::v4::session::Session::new(storage);

        let harness_result = sdk::create_agent_harness_session(
            CreateAgentSessionOptions {
                cwd: &cwd,
                model_source: &model_runtime,
                auth_path,
                settings: Arc::clone(&settings_manager),
                cli_provider: parsed.provider.as_deref(),
                cli_model: parsed.model.as_deref(),
                tools: parsed.tools.as_deref(),
                no_tools: parsed.no_tools == Some(true),
                exclude_tools: parsed.exclude_tools.as_deref(),
                session_id: Some(rpc_session_id),
                runtime_api_key,
            },
            v4_session,
        );
        let (harness, _tool_registry, _model, _thinking_level) = match harness_result {
            Ok(result) => result,
            Err(message) => {
                guard.error(&message);
                return 1;
            }
        };

        let model_source: Arc<dyn pirust_coding_agent::models::ModelSource + Send + Sync> =
            Arc::new(model_runtime);
        let host = Arc::new(pirust_coding_agent::rpc::host::RpcRuntimeHost::new(
            Arc::new(harness),
            model_source,
        ));
        let exit_code = pirust_coding_agent::rpc::run::run_rpc_mode(host, Arc::clone(&guard)).await;
        guard.restore_stdout();
        return exit_code;
    }

    let mut session_manager =
        session_manager.expect("session_manager is Some for every non-rpc mode");
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
        tool_registry,
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

    let session = SingleTurnSession::new(agent, session_manager, tool_registry);
    let host = Arc::new(SingleTurnRuntimeHost::new(Arc::clone(&session)));

    let platform = pirust_coding_agent::config::Platform::current();

    // Step 29 (`main.ts:811-858`): interactive mode launches the TUI; rpc
    // already exited above; everything else runs print mode.
    let run_exit_code = if app_mode == AppMode::Interactive {
        // The `/model` picker's catalogue. Built here because this is the only
        // scope that holds the `ModelRuntime` — `SingleTurnSession` gets an
        // `Agent`, and `Agent::model()` is the single model in use, not the
        // list. Composed once at startup rather than on each `/model` press:
        // the composition is already done, and re-walking every provider on a
        // keypress would be pure waste.
        let model_entries =
            pirust_coding_agent::interactive_pickers::load_model_entries(model_runtime.providers());
        // `TuiRuntimeInfo::set_model_by_name` (`runtime_host.rs`) resolves a `/model` picker
        // selection back into a real `pirust_ai::types::Model` to hand to `Agent::set_model` —
        // `SingleTurnSession` has no such catalogue of its own (it only holds the one `Agent`
        // already in use), so it is handed the same flattened `ComposedProvider::models` list
        // `model_entries` above was itself built from, right after construction, same as
        // `model_entries`. This mirrors `model_entries`'s own doc comment: the only scope that
        // holds `model_runtime` is this interactive arm, so this is where the catalogue has to
        // be built and handed off.
        let model_catalog: Vec<pirust_ai::types::Model> = model_runtime
            .providers()
            .iter()
            .flat_map(|provider| provider.models.iter().cloned())
            .collect();
        session.set_model_catalog(model_catalog);
        run_interactive_mode(session, model_entries).await
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

/// The "Missing configuration" screen (`docs/tui-design-samples.html` §1).
///
/// A deliberately tiny TUI: no session, no agent, no model — there *is* no
/// model, which is the whole point, so `InteractiveMode` cannot be used here.
/// It mounts one component, pumps keys until the user picks `[Open setup help]`
/// or `[Quit]`, and restores the terminal on the way out.
///
/// Runs on the current thread with a plain blocking read loop rather than the
/// async machinery: there is nothing to await, and a `tokio` loop here would
/// only add a way to get the shutdown wrong.
///
/// Returns 1 — the configuration really is missing, and a script that
/// mistakenly reaches this path should still see a failure.
fn run_setup_help_screen(models_path: &str, error: &str) -> i32 {
    use pirust_coding_agent::interactive_welcome::{SetupChoice, SetupHelpScreen};
    use pirust_tui::terminal::Terminal;
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::mpsc;

    let mut terminal: Box<dyn Terminal> = Box::new(pirust_tui::terminal::ProcessTerminal::new());
    let (tx, rx) = mpsc::channel::<String>();
    terminal.start(
        Box::new(move |data: &str| {
            let _ = tx.send(data.to_string());
        }),
        Box::new(|| {}),
    );

    let tui = Rc::new(RefCell::new(pirust_tui::tui::TUI::new(
        terminal,
        Some(false),
    )));
    tui.borrow_mut().start();

    let screen = Rc::new(RefCell::new(SetupHelpScreen::new(models_path)));
    tui.borrow_mut()
        .add_child(Rc::clone(&screen) as pirust_tui::tui::SharedComponent);
    tui.borrow_mut().request_render(true);
    tui.borrow_mut().poll();

    let mut help_path: Option<String> = None;
    loop {
        match rx.recv_timeout(std::time::Duration::from_millis(200)) {
            Ok(data) => {
                // Ctrl+C and Ctrl+D quit here too. A screen whose only job is
                // to report a misconfiguration must never be a trap.
                if pirust_tui::keys::matches_key(&data, "ctrl+c")
                    || pirust_tui::keys::matches_key(&data, "ctrl+d")
                {
                    break;
                }
                match screen.borrow_mut().handle_key(&data) {
                    Some(SetupChoice::Quit) => break,
                    Some(SetupChoice::OpenHelp) => {
                        help_path = Some(models_path.to_string());
                        break;
                    }
                    None => {}
                }
                tui.borrow_mut().request_render(false);
                tui.borrow_mut().poll();
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                tui.borrow_mut().poll();
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    tui.borrow_mut().stop();

    // Printed *after* the TUI has restored the terminal, so it survives on
    // screen instead of being wiped by the teardown.
    eprintln!("Error: {error}");
    if let Some(path) = help_path {
        eprintln!("Add a provider to: {path}");
        eprintln!("Docs: {}", get_docs_path_display());
    }
    1
}

/// Where the provider/model docs live, for the setup screen's closing hint.
/// Falls back to a plain description rather than failing — this runs on an
/// error path and must not introduce a second error.
fn get_docs_path_display() -> String {
    let docs = pirust_coding_agent::config::get_docs_path();
    docs.join("providers.md").to_string_lossy().into_owned()
}

/// `runInteractiveMode` (`main.ts:811-858`) — launch the TUI and loop.
///
/// The TUI loop runs asynchronously; model turns are spawned so input and
/// session events remain live while the provider responds.
/// (user line, streaming assistant text, separator). Quit on Ctrl+D.
///
/// `bindCurrentSessionExtensions` (interactive-mode.ts:1858-1860) — the
/// interactive session binds extensions with `mode: "tui"` before the loop.
async fn run_interactive_mode(
    session: Arc<SingleTurnSession>,
    model_entries: Vec<pirust_coding_agent::interactive_pickers::ModelEntry>,
) -> i32 {
    use pirust_coding_agent::interactive_mode::{InteractiveMode, InteractiveSession};
    use pirust_coding_agent::print_mode::{AgentSessionRuntimeHost, PrintModeSession};
    use pirust_coding_agent::runtime_host::SingleTurnRuntimeHost;

    // Bind the extension runner (plan-mode + tool blocking active in the TUI).
    //
    // `command_context_actions` (P5, `docs/tui-pending-action-plan.md`) is now real,
    // not `CommandContextActions::placeholder()`: an extension's `newSession`/`fork`/
    // `switchSession` call genuinely reaches `SingleTurnRuntimeHost`, which mutates this
    // same `session`'s `Agent`/`SessionManager` — the identical `PrintModeSession`
    // methods `/new`/`/fork`/`/import` already call from `interactive_mode.rs`.
    //
    // `host.set_rebind_session` is deliberately never called here, unlike
    // `print_mode.rs::run_print_mode`: that callback is `Send + Sync` by contract
    // (`RebindSessionFn`), and there is no such hook into the `Rc`-based
    // `InteractiveMode` below — it does not exist yet at this point in the function, and
    // even once built could not be reached from a `Send + Sync` closure without a
    // channel-based redesign (see `runtime_host.rs`'s module doc for the full
    // reasoning). So an extension-triggered swap here genuinely mutates the session, but
    // the already-rendered chat stays stale until something else (e.g. the user's own
    // `/new`) repaints it — a real, named limitation, not a silent gap.
    let host: Arc<dyn AgentSessionRuntimeHost> =
        Arc::new(SingleTurnRuntimeHost::new(Arc::clone(&session)));
    let session_dyn = Arc::clone(&session) as Arc<dyn PrintModeSession>;
    let binding = pirust_coding_agent::print_mode::ExtensionBinding {
        mode: pirust_coding_agent::print_mode::ExtensionBindMode::Tui,
        command_context_actions: pirust_coding_agent::print_mode::build_command_context_actions(
            &host,
            &session_dyn,
        ),
        on_error: Arc::new(|_| {}),
    };
    if let Err(error) = session.bind_extensions(binding).await {
        eprintln!(
            "Warning: failed to bind extensions: {}",
            error.console_message()
        );
    }

    let runtime = tokio::runtime::Handle::current();
    let terminal = Box::new(pirust_tui::terminal::ProcessTerminal::new());
    let session: Arc<dyn InteractiveSession> = session;
    let mut mode = InteractiveMode::new(terminal, session, runtime);
    mode.set_model_entries(model_entries);
    mode.run_async().await;

    // `/restart` (`interactive_mode.rs::run_restart`) never spawns anything itself — it
    // only sets `restart_requested` and quits through the exact same path `/quit` uses.
    // This is deliberately the *only* place that ever calls `restart_process`, and it
    // must run here, after the `.await` above has returned, not from inside
    // `dispatch_command` or `run_async`: by this point `run_async`'s loop has already
    // broken out of its `self.quit` check (interactive_mode.rs's `run_async`, the
    // `if self.quit.load(..) { break; }` at the top of its loop) and `mode` — including
    // the `TUI`/`Terminal` it owns — has been through its normal teardown, which is what
    // takes the terminal back out of raw mode and off the alternate screen. Spawning a
    // second `pirust` process any earlier, while this one still owns the terminal in raw
    // mode, would leave two processes racing to read the same stdin and write the same
    // stdout — not a restart, a wedged terminal (the same class of hazard this crate's
    // Ctrl+C/Esc fixes exist to avoid). `mode` is a plain local `let mut mode`, so it is
    // still alive and its flag is still readable here even though `run_async` took
    // `&mut self` — the borrow ends when the `.await` above completes.
    if mode.restart_requested() {
        return restart_process();
    }
    0
}

/// Re-exec the current `pirust` invocation in place of this process.
///
/// Only ever called from [`run_interactive_mode`], strictly after the TUI has already
/// torn down — see that call site's comment for why that ordering is load-bearing.
///
/// Spawns the replacement and returns immediately, **without** calling `Child::wait`.
/// This process's only remaining job once the child exists is to get out of the way: the
/// terminal is a single resource, and holding this process alive (blocked on the child)
/// buys nothing, since nothing here still needs to run afterward — `main`'s caller
/// (`fn main`) does nothing with the exit code but pass it straight to
/// `std::process::exit`. Not calling `Child::wait` is also why this cannot use the
/// Unix-only `exec`-replace trick (`std::os::unix::process::CommandExt::exec`, which
/// really would collapse two processes into one) even if this file only ever ran on
/// Unix: `spawn` + let-the-parent-exit is the one approach that is both cross-platform
/// (works on Windows, where `exec` does not exist at all) and never blocks this process
/// waiting on the new one.
///
/// `std::env::args().skip(1)` reproduces the original argv byte-for-byte, so flags like
/// `--resume <id>` carry over — they live in argv, never in a stream that gets consumed.
/// The one input that genuinely cannot be reproduced this way is piped stdin
/// (`read_piped_stdin`, below): a pipe is drained once and a re-exec's stdin would just
/// see immediate EOF instead of the original content. That is not guarded here because
/// it cannot arise here: `resolve_app_mode` (`print_mode.rs:1196-1211`) only ever chooses
/// `AppMode::Interactive` when `stdin_is_tty` was already `true` (its line 1207:
/// `!stdin_is_tty || !stdout_is_tty` forces `AppMode::Print` instead) — so any process
/// that ever reaches `run_interactive_mode`, and therefore any session where `/restart`
/// is reachable at all, was never reading piped stdin to begin with. The guard the task
/// anticipated is already enforced one layer up, before this function's caller's caller
/// even runs; duplicating it here would just be dead code checking a condition that
/// cannot be false at this call site.
fn restart_process() -> i32 {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(err) => {
            eprintln!("Error: /restart could not resolve the current executable: {err}");
            return 1;
        }
    };
    let args: Vec<String> = std::env::args().skip(1).collect();
    spawn_replacement(&exe, &args)
}

/// [`restart_process`]'s body once the executable path and argv are resolved
/// — split out purely so a test can supply a harmless real executable
/// instead of `current_exe()` (P4, `docs/tui-pending-action-plan.md`: inside
/// `cargo test`, `current_exe()` **is** the test binary itself — spawning it
/// unmodified would recursively re-launch the whole suite, not exercise a
/// harmless child).
fn spawn_replacement(exe: &std::path::Path, args: &[String]) -> i32 {
    match std::process::Command::new(exe).args(args).spawn() {
        // Exit 0: the restart was successfully *scheduled*, which is the only thing this
        // process can promise — whatever the new process's own exit code eventually is
        // belongs to that process, not this one, exactly as a shell would treat any other
        // backgrounded-then-detached command.
        //
        // No orphan/zombie risk from not calling `Child::wait` here: on a failed `spawn`
        // the OS never created a process at all, so there is nothing to leak; on a
        // successful `spawn`, this process (per the module doc above) exits via
        // `std::process::exit` moments later, and the kernel reaps or reparents the
        // child exactly as it would for any other backgrounded process whose parent
        // exits — the same shape as a shell backgrounding a command with `&` and quitting.
        Ok(_child) => 0,
        Err(err) => {
            eprintln!("Error: /restart failed to launch {}: {err}", exe.display());
            1
        }
    }
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

#[cfg(test)]
mod restart_tests {
    use super::spawn_replacement;

    /// A harmless, always-present real executable + args that exits 0
    /// immediately — stands in for the restarted `pirust` binary.
    #[cfg(unix)]
    fn harmless_command() -> (std::path::PathBuf, Vec<String>) {
        (
            std::path::PathBuf::from("/bin/sh"),
            vec!["-c".to_string(), "exit 0".to_string()],
        )
    }

    #[cfg(windows)]
    fn harmless_command() -> (std::path::PathBuf, Vec<String>) {
        (
            std::path::PathBuf::from("cmd"),
            vec!["/C".to_string(), "exit".to_string(), "0".to_string()],
        )
    }

    /// Proves the two properties `restart_process` actually needs: a
    /// successful spawn reports exit code 0, and the call returns
    /// immediately rather than blocking on the child (this test would hang
    /// forever if `spawn_replacement` ever grew a `Child::wait`).
    #[test]
    fn spawn_replacement_launches_and_returns_without_waiting() {
        let (exe, args) = harmless_command();
        let code = spawn_replacement(&exe, &args);
        assert_eq!(code, 0, "a successful spawn must report exit code 0");
    }

    #[test]
    fn spawn_replacement_reports_an_error_when_the_executable_does_not_exist() {
        let exe = std::path::PathBuf::from(if cfg!(windows) {
            "pirust-definitely-not-a-real-binary.exe"
        } else {
            "/definitely/not/a/real/binary"
        });
        let code = spawn_replacement(&exe, &[]);
        assert_eq!(
            code, 1,
            "a failed spawn must report exit code 1, not panic or hang"
        );
    }
}
