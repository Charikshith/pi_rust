//! feat-009 Wave 6 end-to-end proof: a real `AgentServerService` (backed by
//! a real `AgentHarness`, not `crate::testing::service`'s in-memory double)
//! driven through the actual wire protocol — hello, create, attach, prompt —
//! over `DuplexTransport` (Wave 5), asserting a real assistant-shaped
//! transcript item comes back out. Per plan.md's own stated Wave 6 test
//! strategy, the provider is a scripted `Faux` through a real `AgentHarness`
//! turn, NOT an oracle replay (no Pi oracle exists for this pirust-side
//! addition — see `crate::agent_service`'s own module doc).

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use pirust_ai::api::{ProviderStreams, SimpleStreamOptions};
use pirust_ai::providers::faux::{faux_text_message, Faux};
use pirust_ai::types::{Context, Model};
use pirust_coding_agent::models::StaticModelSource;
use pirust_coding_agent::settings::{
    InMemorySettingsStorage, SettingsManager, SettingsManagerCreateOptions,
};
use pirust_orchestrator::agent_service::{AgentServerService, HarnessBuilder};
use pirust_orchestrator::protocol::schemas::{
    AssistantContent, AssistantTranscriptItem, Command, CommandResult, ResponseEnvelope,
    TranscriptItem,
};
use pirust_orchestrator::server::{PiServer, PiServerOptions};
use pirust_orchestrator::testing::duplex::DuplexTransport;
use pirust_orchestrator::types::PiServerService;
use tokio_util::sync::CancellationToken;

const CANNED_REPLY: &str = "hello from the agent harness";

fn model_source() -> StaticModelSource {
    let model = Faux::new().get_model().clone();
    let mut configured = BTreeSet::new();
    configured.insert(model.provider.0.clone());
    StaticModelSource::new(vec![model], configured)
}

/// Mirrors `pirust-coding-agent/tests/sdk_canned_turn.rs`'s `faux_stream_fn`
/// — a scripted, deterministic provider double, not a live network call.
fn faux_stream_fn() -> pirust_agent_core::agent_loop::StreamFn {
    Arc::new(
        move |model: Model, ctx: Context, opts: SimpleStreamOptions, _token: CancellationToken| {
            let faux = Faux::new().with_token_size(1000, 1000);
            faux.set_responses(vec![faux_text_message(CANNED_REPLY).into()]);
            faux.stream_simple(&model, &ctx, Some(opts))
        },
    )
}

fn settings() -> Arc<SettingsManager> {
    Arc::new(SettingsManager::from_storage(
        Arc::new(InMemorySettingsStorage::new()),
        SettingsManagerCreateOptions::default(),
    ))
}

#[tokio::test]
async fn hello_create_attach_prompt_returns_a_real_assistant_transcript_item() {
    let service = Arc::new(AgentServerService::new(
        Arc::new(model_source()),
        HarnessBuilder::Faux(faux_stream_fn()),
        PathBuf::from("/unused/auth.json"),
        settings(),
        "/proj".to_string(),
    )) as Arc<dyn PiServerService>;

    let transport = DuplexTransport::new("agent-service-e2e");
    let server = PiServer::new(
        service,
        PiServerOptions {
            listeners: vec![transport.listener()],
            max_frame_length: None,
            handshake_timeout_ms: None,
            server_id: None,
            on_error: None,
        },
    )
    .expect("valid server options");
    server.start().await.expect("server starts");

    let client = transport.connect();
    client.hello_default().await.expect("hello resolves");

    let create_response = client
        .request(
            Command::Create {
                cwd: None,
                name: None,
                model: None,
                thinking_level: None,
            },
            None,
        )
        .await
        .expect("create request resolves");
    let session_id = match create_response {
        ResponseEnvelope::Success {
            result: CommandResult::Create { session },
            ..
        } => session.id,
        other => panic!("expected a successful create response, got {other:?}"),
    };

    let attach_response = client
        .request(
            Command::Attach {
                session_id: session_id.clone(),
            },
            None,
        )
        .await
        .expect("attach request resolves");
    assert!(
        matches!(
            attach_response,
            ResponseEnvelope::Success {
                result: CommandResult::Attach { .. },
                ..
            }
        ),
        "expected a successful attach response, got {attach_response:?}"
    );

    let prompt_response = client
        .request(
            Command::Prompt {
                session_id: session_id.clone(),
                text: "hi".to_string(),
            },
            None,
        )
        .await
        .expect("prompt request resolves");
    let session = match prompt_response {
        ResponseEnvelope::Success {
            result: CommandResult::Prompt { session },
            ..
        } => session,
        other => panic!("expected a successful prompt response, got {other:?}"),
    };

    // This is the proof that a real `AgentHarness` ran a real turn (through
    // AgentServerService/AgentPiSessionRuntime) and its result was converted
    // and delivered through the actual wire protocol, not a stub.
    let assistant_text = session
        .transcript
        .iter()
        .find_map(|item| match item {
            TranscriptItem::Assistant(AssistantTranscriptItem::Complete { common, .. }) => {
                common.content.iter().find_map(|c| match c {
                    AssistantContent::Text { text } => Some(text.clone()),
                    _ => None,
                })
            }
            _ => None,
        })
        .expect("the transcript must contain a complete assistant text item");
    assert_eq!(assistant_text, CANNED_REPLY);

    server.close().await;
}
