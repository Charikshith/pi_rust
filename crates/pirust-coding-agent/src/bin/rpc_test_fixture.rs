//! Test-only fixture process for `pirust-coding-agent`'s black-box RPC client
//! tests (feat-012 Wave 4). Never shipped as user-facing functionality — it
//! exists so `tests/rpc_client_test.rs` can spawn a real child process (via
//! `CARGO_BIN_EXE_rpc_test_fixture`) and exercise
//! `pirust_coding_agent::rpc::client::RpcClient` against real stdio, the way
//! Pi's own `rpc-client-process-exit.test.ts` spawns a throwaway `child.mjs`.
//!
//! Selected by the `FIXTURE_MODE` env var:
//! - `exit_after_line`: read one line from stdin, then exit with code 43
//!   (mirrors `rpc-client-process-exit.test.ts`'s fake child).
//! - `echo_clone`: for every JSONL command received, reply with a canned
//!   successful `clone` response echoing the command's `id` — enough to
//!   drive `RpcClient::clone_session` end-to-end without a real harness.

use std::io::{BufRead, Write};

fn main() {
    match std::env::var("FIXTURE_MODE").as_deref() {
        Ok("exit_after_line") => {
            let mut line = String::new();
            let _ = std::io::stdin().lock().read_line(&mut line);
            std::process::exit(43);
        }
        Ok("echo_clone") => {
            let stdin = std::io::stdin();
            let mut stdout = std::io::stdout();
            for line in stdin.lock().lines() {
                let Ok(line) = line else { break };
                if line.is_empty() {
                    continue;
                }
                let id = serde_json::from_str::<serde_json::Value>(&line)
                    .ok()
                    .and_then(|v| v.get("id").and_then(|i| i.as_str()).map(str::to_string));
                let response = serde_json::json!({
                    "id": id,
                    "type": "response",
                    "command": "clone",
                    "success": true,
                    "data": { "cancelled": false },
                });
                let _ = writeln!(stdout, "{response}");
                let _ = stdout.flush();
            }
        }
        other => {
            eprintln!("rpc_test_fixture: unknown FIXTURE_MODE {other:?}");
            std::process::exit(2);
        }
    }
}
