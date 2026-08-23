#![cfg(unix)]
//! Port of `packages/server/test/unix.test.ts`'s filesystem-lifecycle
//! scenarios + a real-socket variant of `unix-connection.test.ts`'s
//! send/close ordering. `#[cfg(unix)]`-only — see `crates/pirust-orchestrator
//! /src/transports/unix/mod.rs`'s module doc. This file compiles and
//! clippy-lints clean cross-compiled to `x86_64-unknown-linux-gnu` from this
//! Windows dev machine; its actual pass/fail when RUN is unverified here
//! (named, not silent) since a cross-compiled Linux test binary cannot
//! execute on Windows. Runs wherever this crate is built on Linux/macOS/CI.

use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::sync::Arc;
use std::time::Duration;

use pirust_orchestrator::connection::ByteConnection;
use pirust_orchestrator::server::{PiServer, PiServerOptions};
use pirust_orchestrator::testing::client::connect_unix_test_client;
use pirust_orchestrator::testing::service::TestServerService;
use pirust_orchestrator::transports::unix::{
    create_unix_listener, UnixByteConnection, UnixListenerOptions,
};

fn socket_path(dir: &tempfile::TempDir, nested: bool) -> std::path::PathBuf {
    if nested {
        dir.path().join("p").join("n").join("server.sock")
    } else {
        dir.path().join("server.sock")
    }
}

fn unix_options(path: &std::path::Path) -> UnixListenerOptions {
    UnixListenerOptions {
        path: path.to_string_lossy().into_owned(),
        mode: None,
        max_pending_bytes: None,
        graceful_close_timeout_ms: None,
        max_frame_length: None,
        on_error: None,
    }
}

fn make_server(path: &std::path::Path) -> PiServer {
    let listener = create_unix_listener(unix_options(path)).expect("valid unix listener options");
    PiServer::new(
        Arc::new(TestServerService::new()) as Arc<dyn pirust_orchestrator::types::PiServerService>,
        PiServerOptions {
            listeners: vec![listener],
            max_frame_length: None,
            handshake_timeout_ms: None,
            server_id: None,
            on_error: None,
        },
    )
    .expect("valid server options")
}

#[tokio::test]
async fn rejects_a_live_listener_without_unlinking_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = socket_path(&dir, false);
    let first = make_server(&path);
    first.start().await.expect("first listener starts");
    let first_identity = tokio::fs::symlink_metadata(&path).await.unwrap();

    let second = make_server(&path);
    let err = second
        .start()
        .await
        .expect_err("second listener must reject a live socket");
    assert!(err.to_string().contains("already running"));
    let current_identity = tokio::fs::symlink_metadata(&path).await.unwrap();
    assert!(current_identity.file_type().is_socket());
    assert_eq!(current_identity.dev(), first_identity.dev());
    assert_eq!(current_identity.ino(), first_identity.ino());

    let client = connect_unix_test_client(&path)
        .await
        .expect("connects to the live listener");
    client.hello_default().await.expect("hello resolves");
    first.close().await;
}

#[tokio::test]
async fn never_unlinks_a_regular_file_at_the_configured_path() {
    let dir = tempfile::tempdir().unwrap();
    let path = socket_path(&dir, false);
    tokio::fs::write(&path, "do not remove").await.unwrap();
    tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640))
        .await
        .unwrap();

    let server = make_server(&path);
    let err = server
        .start()
        .await
        .expect_err("must refuse a non-socket path");
    assert!(err.to_string().contains("non-socket"));
    assert_eq!(
        tokio::fs::read_to_string(&path).await.unwrap(),
        "do not remove"
    );
}

#[tokio::test]
async fn creates_nested_parents_restricts_permissions_and_removes_its_own_socket() {
    let dir = tempfile::tempdir().unwrap();
    let path = socket_path(&dir, true);
    let server = make_server(&path);
    server
        .start()
        .await
        .expect("nested parent dirs are created");
    let stats = tokio::fs::symlink_metadata(&path).await.unwrap();
    assert!(stats.file_type().is_socket());
    assert_eq!(stats.permissions().mode() & 0o777, 0o600);

    server.close().await;
    assert!(
        tokio::fs::symlink_metadata(&path).await.is_err(),
        "socket file must be removed on close"
    );
}

#[tokio::test]
async fn does_not_remove_a_replacement_inode_during_shutdown() {
    let dir = tempfile::tempdir().unwrap();
    let path = socket_path(&dir, false);
    let server = make_server(&path);
    server.start().await.expect("listener starts");
    tokio::fs::remove_file(&path).await.unwrap();
    tokio::fs::write(&path, "replacement").await.unwrap();

    server.close().await;
    assert_eq!(
        tokio::fs::read_to_string(&path).await.unwrap(),
        "replacement"
    );
}

#[tokio::test]
async fn removes_a_genuinely_stale_socket_before_binding() {
    let dir = tempfile::tempdir().unwrap();
    let path = socket_path(&dir, false);
    // Simulate a crashed prior owner: bind a real listener, then drop it
    // without ever calling `close()`, leaving a dead-but-present socket
    // file at `path` — nothing is listening on it anymore.
    {
        let stale = tokio::net::UnixListener::bind(&path).unwrap();
        drop(stale);
    }
    let stale_identity = tokio::fs::symlink_metadata(&path).await.unwrap();
    assert!(stale_identity.file_type().is_socket());

    let server = make_server(&path);
    server
        .start()
        .await
        .expect("a genuinely dead socket must be replaced, not rejected");
    let live_identity = tokio::fs::symlink_metadata(&path).await.unwrap();
    assert!(live_identity.file_type().is_socket());

    let client = connect_unix_test_client(&path)
        .await
        .expect("connects to the newly bound listener");
    client.hello_default().await.expect("hello resolves");
    server.close().await;
}

/// Real-socket variant of `unix-connection.test.ts`'s "queues a final
/// protocol error behind pending output before closing." **Named
/// simplification:** TS mocks the socket to deterministically observe a
/// write staying pending; a real OS socket completes small writes near-
/// instantly, so this test instead proves the OUTCOME (both the sent chunk
/// and the final chunk arrive, in order, before EOF) rather than
/// independently forcing mid-write backpressure — the ordering guarantee
/// itself comes from `send`/`close` sharing one `tokio::sync::Mutex` around
/// the write half (see `UnixByteConnection`'s doc comment), not from
/// anything this test can directly observe over a real socket.
#[tokio::test]
async fn send_then_close_delivers_both_chunks_in_order_then_closes() {
    let (a, b) = tokio::net::UnixStream::pair().unwrap();
    let (mut peer_read, _peer_write) = b.into_split();
    let (_a_read, a_write) = a.into_split();
    let connection = Arc::new(UnixByteConnection::new(a_write, 1_000, 1024 * 1024));

    connection.send(&[1, 2, 3]).await.expect("send succeeds");
    connection
        .close(Some(&[9, 9]))
        .await
        .expect("close succeeds");

    let mut received = Vec::new();
    use tokio::io::AsyncReadExt;
    tokio::time::timeout(Duration::from_secs(2), peer_read.read_to_end(&mut received))
        .await
        .expect("peer observes EOF")
        .expect("read succeeds");
    assert_eq!(received, vec![1, 2, 3, 9, 9]);
}
