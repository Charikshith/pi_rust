#![cfg(unix)]
//! Closes the "actual `pirust-orchestrator` binary run against a real Unix
//! socket" residual named in feat-009 Wave 6's evidence (`feature_list.json`):
//! every other test in this crate builds a `PiServer` in-process against a
//! library-level listener. This one spawns the REAL compiled
//! `pirust-orchestrator` binary as a child process and drives a real client
//! through a real `AF_UNIX` handshake against it.

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use pirust_orchestrator::protocol::schemas::ServerMessage;
use pirust_orchestrator::testing::client::connect_unix_test_client;

/// Kills the spawned binary even if an assertion below panics mid-test.
struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

async fn wait_for_socket(path: &Path) {
    for _ in 0..200 {
        if path.exists() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("socket file never appeared at {}", path.display());
}

#[tokio::test]
async fn real_binary_completes_a_real_hello_handshake_over_a_real_unix_socket() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket_path = dir.path().join("pirust-orchestrator.sock");
    // An isolated HOME so this test never touches a real ~/.pirust config,
    // auth, or settings file on the machine running it.
    let home_dir = dir.path().join("home");
    std::fs::create_dir_all(&home_dir).expect("create isolated home dir");

    let child = Command::new(env!("CARGO_BIN_EXE_pirust-orchestrator"))
        .arg("--socket")
        .arg(&socket_path)
        .env("HOME", &home_dir)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn the real pirust-orchestrator binary");
    let _guard = ChildGuard(child);

    wait_for_socket(&socket_path).await;

    let client = connect_unix_test_client(&socket_path)
        .await
        .expect("connect a real client over the real socket file");
    let hello = client.hello_default().await.expect("hello resolves");
    assert!(
        matches!(hello, ServerMessage::Hello(_)),
        "expected a real ServerHello from the real binary, got {hello:?}"
    );
}
