//! Proves feat-005 Wave 4 (4a-4e) wires together: `sdk::assemble_agent_session` builds
//! a real [`pirust_agent_core::agent::Agent`] — tools (feat-004), `convert_to_llm`
//! (feat-003), the system prompt (4a) and the `Agent`/loop plumbing (feat-003) — and
//! driving it one turn through a scripted [`Faux`] provider produces exactly the
//! `AssistantMessage` shape `print_mode.rs` (already done) expects to render. This is
//! not a real-network test: the provider is [`Faux`], not the Anthropic adapter (4d),
//! by design (see `sdk.rs`'s `assemble_agent_session` docs) — an integration test has no
//! business making a live API call.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use pirust_agent_core::harness::messages::AgentMessage;
use pirust_agent_core::types::ThinkingLevel;
use pirust_ai::api::{ProviderStreams, SimpleStreamOptions};
use pirust_ai::providers::faux::{faux_text_message, Faux};
use pirust_ai::types::{Context, Message, Model, StopReason};
use pirust_coding_agent::models::StaticModelSource;
use pirust_coding_agent::sdk::{assemble_agent_session, CreateAgentSessionOptions};
use pirust_coding_agent::settings::{
    InMemorySettingsStorage, SettingsManager, SettingsManagerCreateOptions,
};
use tokio_util::sync::CancellationToken;

fn settings() -> Arc<SettingsManager> {
    Arc::new(SettingsManager::from_storage(
        Arc::new(InMemorySettingsStorage::new()),
        SettingsManagerCreateOptions::default(),
    ))
}

fn one_available_model() -> StaticModelSource {
    let model = Faux::new().get_model().clone();
    let mut configured = BTreeSet::new();
    configured.insert(model.provider.0.clone());
    StaticModelSource::new(vec![model], configured)
}

/// A `StreamFn` that replays one scripted assistant message, regardless of which model
/// the loop resolved — the provider identity is 4d's concern (Anthropic), not this
/// test's; this test's concern is 4a/4c/4e's assembly plus the pre-existing agent-core
/// loop and tool registry.
fn faux_stream_fn() -> pirust_agent_core::agent_loop::StreamFn {
    Arc::new(
        move |model: Model, ctx: Context, opts: SimpleStreamOptions, _token: CancellationToken| {
            let faux = Faux::new().with_token_size(1000, 1000);
            faux.set_responses(vec![faux_text_message("hello from the canned turn").into()]);
            faux.stream_simple(&model, &ctx, Some(opts))
        },
    )
}

#[tokio::test]
async fn assembled_agent_runs_one_turn_and_produces_the_shape_print_mode_expects() {
    let model_source = one_available_model();
    let settings = settings();

    let result = assemble_agent_session(
        CreateAgentSessionOptions {
            cwd: "/proj",
            model_source: &model_source,
            auth_path: PathBuf::from("/unused/auth.json"),
            settings: settings.clone(),
            cli_provider: None,
            cli_model: None,
            tools: None,
            no_tools: false,
            exclude_tools: None,
            session_id: Some("sess-canned".to_string()),
            runtime_api_key: None,
        },
        faux_stream_fn(),
    )
    .expect("a model is available, so assembly must not fail");

    assert!(result.model_fallback_message.is_none());
    assert_eq!(result.model.provider.0, "faux");
    assert_eq!(result.thinking_level, ThinkingLevel::Off);

    // The default active tools (read/bash/edit/write) must have made it into the
    // system prompt via 4a, proving 4e wired the tool registry (feat-004) through.
    assert!(result.system_prompt.contains("Available tools"));
    assert!(result.system_prompt.contains("read:"));
    assert!(result.system_prompt.contains("bash:"));
    assert!(result.system_prompt.contains("edit:"));
    assert!(result.system_prompt.contains("write:"));
    assert!(result
        .system_prompt
        .ends_with("Current working directory: /proj"));

    let agent = result.agent;
    agent
        .prompt("hi")
        .await
        .expect("prompt must not error with a working stream_fn");
    agent.wait_for_idle().await;

    // This is the exact shape `print_mode.rs`'s text/json renderers read: the loop
    // produced a real AssistantMessage with our scripted text and a normal stop
    // reason — proving the whole 4a-4e chain (tools -> system prompt -> Agent ->
    // agent-core loop -> convert_to_llm -> provider) delivers what print_mode expects.
    let final_message = agent
        .messages()
        .into_iter()
        .rev()
        .find_map(|m| match m {
            AgentMessage::Llm(Message::Assistant(assistant)) => Some(assistant),
            _ => None,
        })
        .expect("the loop must have appended an assistant message");

    assert_eq!(final_message.stop_reason, StopReason::Stop);
    let text = final_message
        .content
        .iter()
        .find_map(|c| match c {
            pirust_ai::types::AssistantContent::Text(t) => Some(t.text.clone()),
            _ => None,
        })
        .expect("faux's scripted response is a text block");
    assert_eq!(text, "hello from the canned turn");
}
