//! feat-012 Wave 4 — black-box tests for
//! [`pirust_coding_agent::rpc::client::RpcClient`], mirroring Pi's
//! `rpc-client-clone.test.ts` and `rpc-client-process-exit.test.ts`.
//!
//! Both spawn `rpc_test_fixture` (`src/bin/rpc_test_fixture.rs`, a tiny
//! same-package test-only binary — see its own doc comment) instead of a
//! real `pirust --mode rpc` process or Pi's throwaway `child.mjs`. The
//! fixture's `FIXTURE_MODE` env var selects which of the two scripted
//! behaviors to run. This keeps both tests genuinely process-real — unlike
//! TS's `clone` test, which mocks the client's own private `send`/`getData`
//! methods rather than spawning anything — without requiring Node or a fully
//! bootable agent (models.json, a live provider, ...).

use std::collections::HashMap;

use pirust_coding_agent::rpc::client::{Cancelled, RpcClient, RpcClientError, RpcClientOptions};

fn fixture_options(mode: &str) -> RpcClientOptions {
    let mut env = HashMap::new();
    env.insert("FIXTURE_MODE".to_string(), mode.to_string());
    RpcClientOptions {
        program: env!("CARGO_BIN_EXE_rpc_test_fixture").into(),
        env,
        ..Default::default()
    }
}

/// Mirrors `rpc-client-clone.test.ts`: `clone()` sends the bare `clone`
/// command and resolves with the response's `data`.
#[tokio::test]
async fn clone_session_sends_command_and_returns_data() {
    let client = RpcClient::new(fixture_options("echo_clone"));
    client.start().await.expect("fixture process should start");

    let result = client
        .clone_session()
        .await
        .expect("fixture should reply with a successful clone response");
    assert_eq!(result, Cancelled { cancelled: false });

    client.stop().await;
}

/// Mirrors `rpc-client-process-exit.test.ts`: an in-flight request rejects
/// when the child process exits instead of replying.
#[tokio::test]
async fn rejects_in_flight_request_when_child_exits() {
    let client = RpcClient::new(fixture_options("exit_after_line"));
    client.start().await.expect("fixture process should start");

    let err = client
        .clone_session()
        .await
        .expect_err("the fixture exits instead of replying");
    match err {
        RpcClientError::ProcessExited { code, signal, .. } => {
            assert_eq!(code, "43");
            assert_eq!(signal, "null");
        }
        other => panic!("expected ProcessExited, got {other:?}"),
    }

    // The child is already gone; `stop()` must still clean up without hanging.
    client.stop().await;
}
