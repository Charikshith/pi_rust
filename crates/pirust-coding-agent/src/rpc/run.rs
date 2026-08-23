//! feat-012 Wave 3 — the RPC mode process loop. Port of `rpc-mode.ts`'s
//! `handleInputLine`/shutdown machinery, driving Wave 2's [`handle_command`]
//! dispatch over real stdin/stdout.
//!
//! Reads JSONL commands from stdin, dispatches each CONCURRENTLY (matching
//! `rpc-mode.ts`'s fire-and-forget `void handleInputLine(line)` — commands
//! are not serialized against each other), and streams the harness's event
//! tape (loop events + the synthesized `agent_settled`) as JSONL on stdout —
//! the same [`crate::print_mode::AgentSessionEvent`] shape `--mode json`
//! already emits (real `rpc-mode.ts` reuses the very same `toJsonEvent` from
//! `json-event.ts`, so this is not a new shape, just a second emitter for it).
//!
//! **Not forwarded** (named, not silent): the harness-own `save_point`/
//! `session_tree`/`session_compact`/`after_provider_response` events.
//! `docs/analysis/09-cli-config-spec.md` §13 names only `agent_settled` and
//! `entry_appended` as required beyond the plain loop `AgentEvent` union —
//! the other four are internal harness bookkeeping with no counterpart in
//! Pi's public RPC event vocabulary.
//!
//! **Not ported this wave** (named): `killTrackedDetachedChildren()` on
//! signal (no detached-bash-child registry exists yet to kill); extension UI
//! request/response plumbing (no pending requests exist without an extension
//! runner bound to this host).

use std::sync::Arc;

use pirust_agent_core::harness::session::v4::types::SessionStorage as V4SessionStorage;
use pirust_agent_core::harness::{HarnessEvent, HarnessListener};

use crate::print_mode::{AgentSessionEvent, OutputGuard};
use crate::rpc::host::RpcRuntimeHost;
use crate::rpc::jsonl::{read_json_lines, serialize_json_line};
use crate::rpc::mode::{handle_command, RpcOutputFn};
use crate::rpc::types::{parse_command_id, parse_input, ParsedInput, RpcResponse};
use crate::runtime_host::to_session_event;

/// Subscribe the harness's event tape straight to `guard`'s raw stdout —
/// deliberately separate from the [`RpcOutputFn`] used for command responses,
/// mirroring `rpc-mode.ts`'s single `writeRawStdout` sink for both; the
/// `OutputGuard`'s internal mutex is what keeps the two streams from
/// interleaving mid-line, not a shared Rust type.
fn install_event_forwarding<St: V4SessionStorage + Send + Sync + 'static>(
    host: &RpcRuntimeHost<St>,
    guard: Arc<OutputGuard>,
) {
    let listener: HarnessListener = Arc::new(move |event: HarnessEvent| {
        let guard = Arc::clone(&guard);
        Box::pin(async move {
            let session_event = match event {
                HarnessEvent::Loop(agent_event) => to_session_event(agent_event),
                HarnessEvent::Settled { .. } => AgentSessionEvent::AgentSettled,
                _ => return,
            };
            guard.write_raw_stdout(&serialize_json_line(&session_event));
        }) as futures::future::BoxFuture<'static, ()>
    });
    host.harness.subscribe(listener);
}

/// `handleInputLine` (`rpc-mode.ts:748-798`), split into its three outcomes:
/// a real JSON syntax error, an `extension_ui_response` (routed nowhere this
/// wave), or a command dispatched through [`handle_command`].
async fn handle_line<St: V4SessionStorage + Send + Sync + 'static>(
    host: &Arc<RpcRuntimeHost<St>>,
    line: &str,
    output: RpcOutputFn,
) {
    if let Err(parse_error) = serde_json::from_str::<serde_json::Value>(line) {
        // Wording is serde_json's own message, not V8/JSON.parse's exact text —
        // reproducing V8's parser wording from Rust is not attempted; the
        // response SHAPE (`command: "parse"`, the `Failed to parse command: `
        // prefix) is what Wave 1's oracle actually pinned.
        output(RpcResponse::error(
            None,
            "parse",
            format!("Failed to parse command: {parse_error}"),
        ));
        return;
    }
    match parse_input(line) {
        ParsedInput::Command(command) => {
            let id = parse_command_id(line);
            handle_command(host, id, command, output).await;
        }
        ParsedInput::ExtensionUiResponse(_) => {
            // No pending extension UI requests exist this wave (Wave 2's own
            // scope note) — nothing to route the response to.
        }
        ParsedInput::Unknown(label) => {
            let id = parse_command_id(line);
            let type_name = label.unwrap_or_else(|| "undefined".to_string());
            output(RpcResponse::error(
                id,
                type_name.clone(),
                format!("Unknown command: {type_name}"),
            ));
        }
    }
}

/// Run the RPC command loop to completion. Returns the process exit code:
/// `0` on stdin EOF (`rpc-mode.ts`'s default `shutdown()`), `143`/`129` on
/// `SIGTERM`/`SIGHUP` (Unix only — Windows falls back to the OS default
/// terminate, the same documented gap `print_mode::NoSignals` already
/// carries for every other mode).
pub async fn run_rpc_mode<St: V4SessionStorage + Send + Sync + 'static>(
    host: Arc<RpcRuntimeHost<St>>,
    guard: Arc<OutputGuard>,
) -> i32 {
    install_event_forwarding(&host, Arc::clone(&guard));

    let output: RpcOutputFn = {
        let guard = Arc::clone(&guard);
        Arc::new(move |response: RpcResponse| {
            guard.write_raw_stdout(&serialize_json_line(&response));
        })
    };

    let (line_tx, mut line_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    std::thread::spawn(move || {
        let _ = read_json_lines(std::io::stdin(), |line| {
            let _ = line_tx.send(line);
        });
        // `line_tx` drops here (EOF or read error) — `line_rx.recv()` then
        // yields `None`, which the loop below treats as stdin-end.
    });

    let (exit_tx, mut exit_rx) = tokio::sync::oneshot::channel::<i32>();

    #[cfg(unix)]
    {
        let guard = Arc::clone(&guard);
        tokio::spawn(async move {
            use tokio::signal::unix::{signal, SignalKind};
            let (Ok(mut term), Ok(mut hup)) = (
                signal(SignalKind::terminate()),
                signal(SignalKind::hangup()),
            ) else {
                return;
            };
            let code = tokio::select! {
                _ = term.recv() => 143,
                _ = hup.recv() => {
                    guard.flush_raw_stdout();
                    129
                }
            };
            let _ = exit_tx.send(code);
        });
    }
    // Windows has no SIGTERM/SIGHUP equivalent wired (see module docs) — keep
    // the sender alive so `exit_rx` never resolves, rather than dropping it
    // (which would make the channel immediately, spuriously ready).
    #[cfg(not(unix))]
    let _exit_tx = exit_tx;

    // Tracked (not bare `tokio::spawn`) so stdin-end can drain in-flight commands before
    // exiting — `tokio::spawn`ing and returning immediately would let `main.rs`'s
    // `std::process::exit` kill the process mid-write for any command still running.
    let mut tasks = tokio::task::JoinSet::new();

    loop {
        tokio::select! {
            line = line_rx.recv() => {
                match line {
                    Some(line) => {
                        let host = Arc::clone(&host);
                        let output = Arc::clone(&output);
                        tasks.spawn(async move { handle_line(&host, &line, output).await; });
                    }
                    None => {
                        // Graceful stdin-end shutdown (`rpc-mode.ts`'s default `shutdown()`):
                        // wait for every in-flight command to finish writing its response.
                        // Only the fast `SIGTERM` path below skips this, matching Pi's own
                        // `if (signal !== "SIGTERM") await flushRawStdout()` asymmetry.
                        while tasks.join_next().await.is_some() {}
                        return 0;
                    }
                }
            }
            Some(_) = tasks.join_next(), if !tasks.is_empty() => {}
            code = &mut exit_rx => {
                return code.unwrap_or(0);
            }
        }
    }
}
