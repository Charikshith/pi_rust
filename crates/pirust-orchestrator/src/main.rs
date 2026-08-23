//! `pirust-orchestrator` — pirust port of `@earendil-works/pi-server` (a
//! transport-neutral, multi-session-multiplexing server library — see
//! `docs/analysis/04-orchestrator.md`, rewritten 2026-08-23 after real Pi
//! renamed and redesigned this package away from process-spawning).
//!
//! feat-009 Wave 6 adds this runnable binary: a `PiServer` over a real Unix
//! socket, backed by [`pirust_orchestrator::agent_service::AgentServerService`].
//!
//! **Scope (named, not silent):** this is NOT a full CLI-parity clone of
//! `pirust-coding-agent`'s `main.rs` — no `--provider`/`--model`/`--tools`
//! flags, no interactive/print modes, no session-file management. Those
//! choices are made per-session, over the wire, via `CreateSessionOptions`
//! (`model`/`thinking_level`) instead of by CLI flags, since one
//! `pirust-orchestrator` process serves many concurrent sessions over one
//! socket rather than running a single session to completion the way
//! `pirust-coding-agent` does. What this binary DOES reuse directly,
//! unmodified, from `pirust-coding-agent`: the model-runtime/settings/auth
//! bootstrap sequence (`ConfigEnv`, migrations, `SettingsManager`,
//! `AuthStorage`, `ModelRuntime::create` against the builtin catalog) — the
//! same sequence `pirust-coding-agent`'s own `--mode rpc` branch runs before
//! building its `AgentHarness`.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use pirust_coding_agent::auth::{AuthStorage, ProcessEnv};
use pirust_coding_agent::migrations::{self, StdoutConsole};
use pirust_coding_agent::models::{CreateModelRuntimeOptions, ModelRuntime};
use pirust_coding_agent::settings::{SettingsManager, SettingsManagerCreateOptions};
use pirust_orchestrator::agent_service::{AgentServerService, HarnessBuilder};
use pirust_orchestrator::listener::PiServerListener;
use pirust_orchestrator::server::{PiServer, PiServerOptions};

struct Args {
    socket_path: String,
}

fn parse_args() -> Result<Args, String> {
    let mut socket_path = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--socket" => {
                socket_path = Some(args.next().ok_or("--socket requires a path argument")?);
            }
            other => return Err(format!("unrecognized argument: {other}")),
        }
    }
    Ok(Args {
        socket_path: socket_path.ok_or("--socket <path> is required")?,
    })
}

#[cfg(unix)]
fn build_listener(path: String) -> Result<Box<dyn PiServerListener>, String> {
    pirust_orchestrator::transports::unix::create_unix_listener(
        pirust_orchestrator::transports::unix::UnixListenerOptions {
            path,
            mode: None,
            max_pending_bytes: None,
            graceful_close_timeout_ms: None,
            max_frame_length: None,
            on_error: None,
        },
    )
    .map_err(|e| e.to_string())
}

#[cfg(not(unix))]
fn build_listener(_path: String) -> Result<Box<dyn PiServerListener>, String> {
    // Named, not silent (mirrors `transports::unix::mod`'s own Windows-vs-Unix
    // split): `tokio::net::UnixListener`, and therefore `create_unix_listener`,
    // is `#[cfg(unix)]`-only. This binary must still compile and lint clean on
    // this project's own Windows dev machine, so the Unix transport is a clean
    // runtime error here rather than a missing symbol.
    Err(
        "pirust-orchestrator's Unix-domain-socket transport is only available on Unix targets"
            .to_string(),
    )
}

fn main() {
    let args = match parse_args() {
        Ok(args) => args,
        Err(message) => {
            eprintln!("Error: {message}");
            eprintln!("Usage: pirust-orchestrator --socket <path>");
            std::process::exit(2);
        }
    };

    let runtime = tokio::runtime::Runtime::new().expect("failed to start async runtime");
    let exit_code = runtime.block_on(run(args));
    std::process::exit(exit_code);
}

async fn run(args: Args) -> i32 {
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| ".".to_string());
    let config_env = pirust_coding_agent::config::ConfigEnv::from_process_env();

    let mut console = StdoutConsole;
    if let Err(error) = migrations::run_migrations(&config_env, &cwd, &mut console) {
        eprintln!("Error: {error}");
        return 1;
    }

    let settings_manager = match SettingsManager::create(
        &config_env,
        std::path::Path::new(&cwd),
        SettingsManagerCreateOptions::default(),
    ) {
        Ok(manager) => manager,
        Err(error) => {
            eprintln!("Error: {error}");
            return 1;
        }
    };
    let settings_manager = Arc::new(settings_manager);

    let auth_path = match config_env.auth_path() {
        Ok(path) => PathBuf::from(path),
        Err(error) => {
            eprintln!("Error: {error}");
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
            eprintln!("Error: {error}");
            return 1;
        }
    };
    let models_path = match config_env.models_path() {
        Ok(path) => path,
        Err(error) => {
            eprintln!("Error: {error}");
            return 1;
        }
    };
    let model_runtime = match ModelRuntime::create(
        &config_env,
        pirust_coding_agent::catalog::builtin_catalog(),
        CreateModelRuntimeOptions {
            models_path: Some(models_path.as_str()),
            stored_credentials,
            process_env: ProcessEnv::from_process_env(),
        },
    ) {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("Error: {error}");
            return 1;
        }
    };

    let listener = match build_listener(args.socket_path) {
        Ok(listener) => listener,
        Err(message) => {
            eprintln!("Error: {message}");
            return 1;
        }
    };

    let service = Arc::new(AgentServerService::new(
        Arc::new(model_runtime),
        HarnessBuilder::Real,
        auth_path,
        settings_manager,
        cwd,
    ));

    let server = match PiServer::new(
        service,
        PiServerOptions {
            listeners: vec![listener],
            max_frame_length: None,
            handshake_timeout_ms: None,
            server_id: None,
            on_error: Some(Box::new(|error| eprintln!("pirust-orchestrator: {error}"))),
        },
    ) {
        Ok(server) => server,
        Err(error) => {
            eprintln!("Error: {error}");
            return 1;
        }
    };

    if let Err(error) = server.start().await {
        eprintln!("Error: {error}");
        return 1;
    }

    tokio::signal::ctrl_c()
        .await
        .expect("failed to listen for ctrl-c");
    server.close().await;
    0
}
