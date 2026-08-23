//! Port of the transport-agnostic scenarios from `packages/server/test/
//! conformance.test.ts` and `packages/server/test/listener.test.ts`, driven
//! over `pirust_orchestrator::testing::duplex::DuplexTransport` instead of a
//! real Unix socket — see `crates/pirust-orchestrator/src/transports/unix/
//! mod.rs`'s module doc for why. This IS real, executable coverage of
//! `PiServer`'s connection/session state machine over genuine async byte
//! I/O; it does not exercise Unix-domain-socket-specific filesystem
//! behavior (`tests/unix_transport.rs`, `#[cfg(unix)]`, covers that
//! separately and is unverified on this Windows dev machine).

use std::sync::Arc;
use std::time::Duration;

use pirust_orchestrator::protocol::schemas::{
    Command, ProtocolErrorCode, ResponseEnvelope, ServerEvent, ServerMessage, PROTOCOL_VERSION,
};
use pirust_orchestrator::server::{PiServer, PiServerOptions};
use pirust_orchestrator::testing::duplex::DuplexTransport;
use pirust_orchestrator::testing::service::TestServerService;
use pirust_orchestrator::types::PiServerService;

fn options(
    listeners: Vec<Box<dyn pirust_orchestrator::listener::PiServerListener>>,
) -> PiServerOptions {
    PiServerOptions {
        listeners,
        max_frame_length: None,
        handshake_timeout_ms: None,
        server_id: None,
        on_error: None,
    }
}

async fn start_server(service: TestServerService) -> (PiServer, DuplexTransport) {
    let transport = DuplexTransport::new("duplex-0");
    let server = PiServer::new(
        Arc::new(service) as Arc<dyn PiServerService>,
        options(vec![transport.listener()]),
    )
    .expect("valid server options");
    server.start().await.expect("server starts");
    (server, transport)
}

#[tokio::test]
async fn accepts_a_hello_and_returns_a_matching_snapshot() {
    let service = TestServerService::new();
    service.seed_default();
    let (server, transport) = start_server(service).await;
    let client = transport.connect();

    let hello = client.hello_default().await.expect("hello resolves");
    match hello {
        ServerMessage::Hello(h) => assert_eq!(h.snapshot.sessions.len(), 1),
        other => panic!("expected hello, got {other:?}"),
    }
    server.close().await;
}

#[tokio::test]
async fn rejects_unsupported_version_and_closes() {
    let (server, transport) = start_server(TestServerService::new()).await;
    let client = transport.connect();

    let response = client
        .hello((PROTOCOL_VERSION + 1) as i64)
        .await
        .expect("hello_error resolves");
    match response {
        ServerMessage::HelloError(e) => assert_eq!(e.error.code, ProtocolErrorCode::Version),
        other => panic!("expected hello_error, got {other:?}"),
    }
    client.wait_for_close().await;
    server.close().await;
}

#[tokio::test]
async fn rejects_a_request_sent_before_hello() {
    let (server, transport) = start_server(TestServerService::new()).await;
    let client = transport.connect();

    let first_error = client.next(|m| matches!(m, ServerMessage::HelloError(_)));
    client
        .send_message(
            pirust_orchestrator::protocol::schemas::ClientMessage::Request(
                pirust_orchestrator::protocol::schemas::RequestEnvelope {
                    id: "too-early".to_string(),
                    request: Command::List,
                },
            ),
        )
        .await
        .expect("send succeeds even though the server will reject it");
    match first_error.await.expect("hello_error resolves") {
        ServerMessage::HelloError(e) => assert_eq!(e.error.code, ProtocolErrorCode::InvalidRequest),
        other => panic!("expected hello_error, got {other:?}"),
    }
    client.wait_for_close().await;
    server.close().await;
}

#[tokio::test]
async fn rejects_a_second_hello_after_the_first() {
    let (server, transport) = start_server(TestServerService::new()).await;
    let client = transport.connect();
    client.hello_default().await.expect("first hello resolves");

    let duplicate_error = client.next(|m| matches!(m, ServerMessage::HelloError(_)));
    client
        .send_message(
            pirust_orchestrator::protocol::schemas::ClientMessage::Hello(
                pirust_orchestrator::protocol::schemas::ClientHello {
                    version: PROTOCOL_VERSION as i64,
                },
            ),
        )
        .await
        .expect("send succeeds");
    match duplicate_error.await.expect("hello_error resolves") {
        ServerMessage::HelloError(e) => assert_eq!(e.error.code, ProtocolErrorCode::InvalidRequest),
        other => panic!("expected hello_error, got {other:?}"),
    }
    client.wait_for_close().await;
    server.close().await;
}

#[tokio::test]
async fn closes_a_connection_that_never_completes_hello() {
    let transport = DuplexTransport::new("duplex-0");
    let server = PiServer::new(
        Arc::new(TestServerService::new()) as Arc<dyn PiServerService>,
        PiServerOptions {
            listeners: vec![transport.listener()],
            max_frame_length: None,
            handshake_timeout_ms: Some(20),
            server_id: None,
            on_error: None,
        },
    )
    .expect("valid options");
    server.start().await.expect("server starts");
    let client = transport.connect();

    tokio::time::timeout(Duration::from_secs(2), client.wait_for_close())
        .await
        .expect("connection is closed once the handshake timeout elapses");
    assert!(client
        .messages()
        .iter()
        .any(|m| matches!(m, ServerMessage::HelloError(e) if e.error.code == ProtocolErrorCode::InvalidRequest)));
    server.close().await;
}

#[tokio::test]
async fn closes_on_a_malformed_frame_before_hello() {
    let (server, transport) = start_server(TestServerService::new()).await;
    let client = transport.connect();

    let error = client.next(|m| matches!(m, ServerMessage::HelloError(_)));
    // A single 0xff byte can never be a valid length-prefixed CBOR frame.
    client.send_bytes(&[0xff]).await.expect("raw bytes send");
    match error.await.expect("hello_error resolves") {
        ServerMessage::HelloError(e) => assert_eq!(e.error.code, ProtocolErrorCode::InvalidRequest),
        other => panic!("expected hello_error, got {other:?}"),
    }
    client.wait_for_close().await;
    server.close().await;
}

#[tokio::test]
async fn request_ordering_slow_list_does_not_block_a_faster_attach() {
    let service = Arc::new(TestServerService::new());
    service.seed("first", None, None, None, None);
    let transport = DuplexTransport::new("duplex-0");
    let server = PiServer::new(
        service.clone() as Arc<dyn PiServerService>,
        options(vec![transport.listener()]),
    )
    .expect("valid options");
    server.start().await.expect("server starts");
    let client = transport.connect();
    client.hello_default().await.expect("hello resolves");

    let delay = service.delay_next_list();
    let slow = client.request(Command::List, Some("slow".to_string()));
    tokio::pin!(slow);
    tokio::select! {
        _ = &mut slow => panic!("list must not resolve before it is released"),
        _ = delay.wait_entered() => {}
    }
    let fast = client
        .request(
            Command::Attach {
                session_id: "first".to_string(),
            },
            Some("fast".to_string()),
        )
        .await
        .expect("attach resolves while list is delayed");
    match fast {
        ResponseEnvelope::Success { id, .. } => assert_eq!(id, "fast"),
        ResponseEnvelope::Failure { .. } => panic!("attach must succeed"),
    }
    assert!(
        !client
            .messages()
            .iter()
            .any(|m| matches!(m, ServerMessage::Response(ResponseEnvelope::Success { id, .. } | ResponseEnvelope::Failure { id, .. }) if id == "slow")),
        "the slow response must not have arrived yet"
    );
    delay.release();
    let slow_response = slow.await.expect("list eventually resolves");
    match slow_response {
        ResponseEnvelope::Success { id, .. } => assert_eq!(id, "slow"),
        ResponseEnvelope::Failure { .. } => panic!("list must succeed"),
    }
    server.close().await;
}

#[tokio::test]
async fn delivers_session_progress_events_to_attached_clients() {
    let service = Arc::new(TestServerService::new());
    service.seed("first", None, None, None, None);
    let transport = DuplexTransport::new("duplex-0");
    let server = PiServer::new(
        service.clone() as Arc<dyn PiServerService>,
        options(vec![transport.listener()]),
    )
    .expect("valid options");
    server.start().await.expect("server starts");
    let client = transport.connect();
    client.hello_default().await.expect("hello resolves");
    client
        .request(
            Command::Attach {
                session_id: "first".to_string(),
            },
            None,
        )
        .await
        .expect("attach succeeds");

    let progress_event =
        client.next(|m| matches!(m, ServerMessage::Event(e) if matches!(&e.event, ServerEvent::SessionProgress { .. })));
    let runtime = service.latest_runtime("first");
    runtime.emit_progress(
        pirust_orchestrator::protocol::schemas::TranscriptProgress::AssistantDelta {
            message_id: "assistant-1".to_string(),
            content_index: 0,
            kind: pirust_orchestrator::protocol::schemas::ContentDeltaKind::Text,
            delta: "hello".to_string(),
        },
    );
    let event = tokio::time::timeout(Duration::from_secs(2), progress_event)
        .await
        .expect("progress event arrives")
        .expect("event resolves");
    match event {
        ServerMessage::Event(e) => match e.event {
            ServerEvent::SessionProgress { session_id, .. } => assert_eq!(session_id, "first"),
            other => panic!("expected session_progress, got {other:?}"),
        },
        other => panic!("expected event, got {other:?}"),
    }
    server.close().await;
}

#[tokio::test]
async fn disconnects_attached_clients_when_a_runtime_reports_a_terminal_error() {
    let service = Arc::new(TestServerService::new());
    service.seed("terminal", None, None, None, None);
    let transport = DuplexTransport::new("duplex-0");
    let server = PiServer::new(
        service.clone() as Arc<dyn PiServerService>,
        options(vec![transport.listener()]),
    )
    .expect("valid options");
    server.start().await.expect("server starts");
    let client = transport.connect();
    client.hello_default().await.expect("hello resolves");
    client
        .request(
            Command::Attach {
                session_id: "terminal".to_string(),
            },
            None,
        )
        .await
        .expect("attach succeeds");

    let runtime = service.latest_runtime("terminal");
    runtime.emit_error(pirust_orchestrator::errors::PiServerError::session_locked(
        Some("lock ownership lost".to_string()),
        None,
    ));
    tokio::time::timeout(Duration::from_secs(2), client.wait_for_close())
        .await
        .expect("client is disconnected after a terminal runtime error");
    server.close().await;
}

#[tokio::test]
async fn gracefully_closes_connections_and_disposes_attached_sessions() {
    let service = Arc::new(TestServerService::new());
    service.seed("first", None, None, None, None);
    let transport = DuplexTransport::new("duplex-0");
    let server = PiServer::new(
        service.clone() as Arc<dyn PiServerService>,
        options(vec![transport.listener()]),
    )
    .expect("valid options");
    server.start().await.expect("server starts");
    let client = transport.connect();
    client.hello_default().await.expect("hello resolves");
    client
        .request(
            Command::Attach {
                session_id: "first".to_string(),
            },
            None,
        )
        .await
        .expect("attach succeeds");
    let runtime = service.latest_runtime("first");

    server.close().await;
    tokio::time::timeout(Duration::from_secs(2), client.wait_for_close())
        .await
        .expect("close disconnects the client");
    assert_eq!(runtime.dispose_count(), 1);
    assert_eq!(server.addresses().await, Vec::<String>::new());
}

#[tokio::test]
async fn listener_composition_starts_and_closes_every_listener() {
    let first = DuplexTransport::new("first");
    let second = DuplexTransport::new("second");
    let server = PiServer::new(
        Arc::new(TestServerService::new()) as Arc<dyn PiServerService>,
        options(vec![first.listener(), second.listener()]),
    )
    .expect("valid options");
    server.start().await.expect("server starts");
    let mut addresses = server.addresses().await;
    addresses.sort();
    assert_eq!(addresses, vec!["first".to_string(), "second".to_string()]);
    server.close().await;
    assert_eq!(server.addresses().await, Vec::<String>::new());
}
