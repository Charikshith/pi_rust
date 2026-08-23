//! feat-012 Wave 2 — the RPC command dispatch loop. Port of `rpc-mode.ts`'s
//! `handleCommand` switch (lines 386-716) over an [`RpcRuntimeHost`], scoped to
//! the commands named in `plan.md`'s Wave 2 section:
//!
//! prompt/steer/follow_up/abort, get_state, model + thinking commands,
//! steering/follow-up queue modes, compact + auto-compaction, auto-retry +
//! abort_retry, get_entries/get_tree/get_last_assistant_text/get_messages,
//! set_session_name, get_session_stats, get_commands (trivially empty — no
//! extension commands/prompt templates/skills are wired into `AgentHarness`
//! yet, feat-007 territory).
//!
//! **Explicitly out of scope this wave** (named, not silently invented): every
//! command that needs the `AgentSession`-equivalent capability the
//! architecture note at the top of `plan.md` says pirust doesn't have yet —
//! `new_session`, `bash`/`abort_bash`, `export_html`, `switch_session`,
//! `fork`/`clone`, `get_fork_messages`. These return a real (not "Unknown
//! command:") error naming the gap; `main.rs` wiring (`--mode rpc` itself) is
//! Wave 3.

use std::sync::Arc;

use pirust_agent_core::harness::messages::AgentMessage;
use pirust_agent_core::harness::session::v4::types::{
    EntryQuery, SessionStorage as V4SessionStorage,
};
use pirust_agent_core::harness::{user_message, AgentHarnessPhase};
use pirust_agent_core::types::ThinkingLevel as CoreThinkingLevel;
use pirust_ai::types::{Message, Model};
use pirust_extension_api::plan_mode_extension::assistant_text;
use serde_json::{json, Value};

use crate::rpc::host::RpcRuntimeHost;
use crate::rpc::types::{
    QueueModeSerde, RpcCommand, RpcResponse, RpcSessionState, ThinkingLevel as RpcThinkingLevel,
    ThinkingLevelSerde,
};

/// A response sink — `handle_command` calls this once for every synchronous
/// response, and (for `prompt`) again later from a spawned task once the turn
/// finishes or fails. Mirrors `rpc-mode.ts`'s closure-captured `output()`.
pub type RpcOutputFn = Arc<dyn Fn(RpcResponse) + Send + Sync>;

fn rpc_to_core_thinking(level: RpcThinkingLevel) -> CoreThinkingLevel {
    match level {
        RpcThinkingLevel::Off => CoreThinkingLevel::Off,
        RpcThinkingLevel::Minimal => CoreThinkingLevel::Minimal,
        RpcThinkingLevel::Low => CoreThinkingLevel::Low,
        RpcThinkingLevel::Medium => CoreThinkingLevel::Medium,
        RpcThinkingLevel::High => CoreThinkingLevel::High,
    }
}

/// `Xhigh`/`Max` have no RPC-wire counterpart (`rpc-types.ts`'s `ThinkingLevel`
/// only names 5 levels) and are unreachable via any RPC-set path this wave;
/// falling back to `High` is a defensive default, not an observed behavior.
fn core_to_rpc_thinking(level: CoreThinkingLevel) -> RpcThinkingLevel {
    match level {
        CoreThinkingLevel::Off => RpcThinkingLevel::Off,
        CoreThinkingLevel::Minimal => RpcThinkingLevel::Minimal,
        CoreThinkingLevel::Low => RpcThinkingLevel::Low,
        CoreThinkingLevel::Medium => RpcThinkingLevel::Medium,
        CoreThinkingLevel::High | CoreThinkingLevel::Xhigh | CoreThinkingLevel::Max => {
            RpcThinkingLevel::High
        }
    }
}

/// The thinking levels selectable via RPC for a given model. No oracle exists
/// for this exact business logic this wave; a reasoning-capable model exposes
/// the full 5-level RPC vocabulary, a non-reasoning one only `off` — a
/// documented assumption, not a fabricated Pi behavior.
fn available_thinking_levels(model: &Model) -> Vec<RpcThinkingLevel> {
    if model.reasoning {
        vec![
            RpcThinkingLevel::Off,
            RpcThinkingLevel::Minimal,
            RpcThinkingLevel::Low,
            RpcThinkingLevel::Medium,
            RpcThinkingLevel::High,
        ]
    } else {
        vec![RpcThinkingLevel::Off]
    }
}

fn thinking_level_value(level: RpcThinkingLevel) -> Value {
    serde_json::to_value(ThinkingLevelSerde(level)).unwrap_or(Value::Null)
}

fn not_supported(id: Option<String>, command: &'static str) -> RpcResponse {
    RpcResponse::error(
        id,
        command,
        format!(
            "{command} is not supported yet — needs an AgentSession-equivalent capability (feat-012, see plan.md)"
        ),
    )
}

/// Handle one already-parsed [`RpcCommand`]. Parse errors and
/// `extension_ui_response` routing happen one layer up (Wave 1's
/// [`crate::rpc::types::parse_input`]); this function only ever sees a
/// recognized command, so — unlike `rpc-mode.ts`'s runtime `default` arm —
/// there is no "unknown command" case here: the match is exhaustive at
/// compile time.
pub async fn handle_command<St>(
    host: &Arc<RpcRuntimeHost<St>>,
    id: Option<String>,
    command: RpcCommand,
    output: RpcOutputFn,
) where
    St: V4SessionStorage + Send + Sync + 'static,
{
    match command {
        // =================================================================
        // Prompting
        // =================================================================
        RpcCommand::Prompt { message, .. } => {
            // Our harness has no queueing preflight (`rpc-mode.ts`'s
            // `preflightResult` callback): busy is a real, synchronous
            // rejection rather than a queued prompt. Ack immediately
            // otherwise, matching Pi's "ack before the turn completes" shape.
            //
            // No inner `tokio::spawn` here (unlike Wave 2's first draft): the
            // caller (`rpc::run::run_rpc_mode`) already runs every stdin line
            // on its own tracked task, so awaiting the turn inline still lets
            // other commands (steer/get_state/abort) run concurrently on
            // their own lines' tasks — and, unlike a detached inner spawn, it
            // makes this task's lifetime honestly represent "is this
            // command's work done yet", which the RPC loop's stdin-end drain
            // depends on to avoid exiting mid-turn.
            if host.harness.phase() != AgentHarnessPhase::Idle {
                output(RpcResponse::error(id, "prompt", "AgentHarness is busy"));
                return;
            }
            output(RpcResponse::success(id.clone(), "prompt"));
            if let Err(e) = host.harness.prompt(&message).await {
                output(RpcResponse::error(id, "prompt", e.message));
            }
        }

        RpcCommand::Steer { message, .. } => {
            host.harness.steer(user_message(&message));
            output(RpcResponse::success(id, "steer"));
        }

        RpcCommand::FollowUp { message, .. } => {
            host.harness.follow_up(user_message(&message));
            output(RpcResponse::success(id, "follow_up"));
        }

        RpcCommand::Abort => {
            host.harness.abort();
            output(RpcResponse::success(id, "abort"));
        }

        // =================================================================
        // State
        // =================================================================
        RpcCommand::GetState => {
            let phase = host.harness.phase();
            let state = RpcSessionState {
                model: Some(serde_json::to_value(host.harness.model()).unwrap_or(Value::Null)),
                thinking_level: ThinkingLevelSerde(core_to_rpc_thinking(
                    host.harness.thinking_level(),
                )),
                is_streaming: phase == AgentHarnessPhase::Turn,
                is_compacting: phase == AgentHarnessPhase::Compaction,
                steering_mode: QueueModeSerde(host.steering_mode()),
                follow_up_mode: QueueModeSerde(host.follow_up_mode()),
                // No on-disk session file this wave (`V4SessionStorage` has no
                // path concept exposed) — omitted, matching the oracle's own
                // `get_state` fixture, which omits it for the same reason.
                session_file: None,
                session_id: host.session_id.clone(),
                session_name: host.harness.session().get_name().ok().flatten(),
                auto_compaction_enabled: host.auto_compaction_enabled(),
                message_count: host.harness.messages().len(),
                pending_message_count: host.harness.pending_message_count(),
            };
            output(RpcResponse::success_with(
                id,
                "get_state",
                serde_json::to_value(&state).unwrap_or(Value::Null),
            ));
        }

        // =================================================================
        // Model
        // =================================================================
        RpcCommand::SetModel { provider, model_id } => {
            let found = host
                .model_source
                .get_available()
                .iter()
                .find(|m| m.provider.0 == provider && m.id == model_id)
                .cloned();
            match found {
                Some(model) => {
                    host.harness.set_model(model.clone());
                    output(RpcResponse::success_with(
                        id,
                        "set_model",
                        serde_json::to_value(&model).unwrap_or(Value::Null),
                    ));
                }
                None => output(RpcResponse::error(
                    id,
                    "set_model",
                    format!("Model not found: {provider}/{model_id}"),
                )),
            }
        }

        RpcCommand::CycleModel => {
            let models = host.model_source.get_available();
            let current = host.harness.model();
            let next = if models.len() < 2 {
                None
            } else {
                let idx = models
                    .iter()
                    .position(|m| m.provider.0 == current.provider.0 && m.id == current.id);
                match idx {
                    Some(i) => models.get((i + 1) % models.len()),
                    None => models.first(),
                }
            }
            .cloned();
            match next {
                Some(model) => {
                    host.harness.set_model(model.clone());
                    output(RpcResponse::success_with(
                        id,
                        "cycle_model",
                        serde_json::to_value(&model).unwrap_or(Value::Null),
                    ));
                }
                // `success(id, "cycle_model", null)` in Pi: `null !== undefined`,
                // so the `data` key IS present with value `null` — not omitted.
                None => output(RpcResponse::success_with(id, "cycle_model", Value::Null)),
            }
        }

        RpcCommand::GetAvailableModels => {
            let models = host.model_source.get_available();
            output(RpcResponse::success_with(
                id,
                "get_available_models",
                json!({ "models": models }),
            ));
        }

        // =================================================================
        // Thinking
        // =================================================================
        RpcCommand::SetThinkingLevel { level } => {
            host.harness.set_thinking_level(rpc_to_core_thinking(level));
            output(RpcResponse::success(id, "set_thinking_level"));
        }

        RpcCommand::CycleThinkingLevel => {
            let model = host.harness.model();
            let levels = available_thinking_levels(&model);
            let current = core_to_rpc_thinking(host.harness.thinking_level());
            let next = if levels.len() < 2 {
                None
            } else {
                let idx = levels.iter().position(|l| *l == current).unwrap_or(0);
                Some(levels[(idx + 1) % levels.len()])
            };
            match next {
                Some(level) => {
                    host.harness.set_thinking_level(rpc_to_core_thinking(level));
                    output(RpcResponse::success_with(
                        id,
                        "cycle_thinking_level",
                        json!({ "level": thinking_level_value(level) }),
                    ));
                }
                None => output(RpcResponse::success_with(
                    id,
                    "cycle_thinking_level",
                    Value::Null,
                )),
            }
        }

        RpcCommand::GetAvailableThinkingLevels => {
            let model = host.harness.model();
            let levels: Vec<Value> = available_thinking_levels(&model)
                .into_iter()
                .map(thinking_level_value)
                .collect();
            output(RpcResponse::success_with(
                id,
                "get_available_thinking_levels",
                json!({ "levels": levels }),
            ));
        }

        // =================================================================
        // Queue modes
        // =================================================================
        RpcCommand::SetSteeringMode { mode } => {
            host.set_steering_mode(mode);
            output(RpcResponse::success(id, "set_steering_mode"));
        }

        RpcCommand::SetFollowUpMode { mode } => {
            host.set_follow_up_mode(mode);
            output(RpcResponse::success(id, "set_follow_up_mode"));
        }

        // =================================================================
        // Compaction
        // =================================================================
        RpcCommand::Compact { .. } => match host.harness.compact().await {
            Ok(outcome) => output(RpcResponse::success_with(
                id,
                "compact",
                json!({
                    "summary": outcome.summary,
                    "tokensBefore": outcome.tokens_before,
                    "retainedTail": outcome.retained_tail,
                }),
            )),
            Err(e) => output(RpcResponse::error(id, "compact", e.message)),
        },

        RpcCommand::SetAutoCompaction { enabled } => {
            host.set_auto_compaction_enabled(enabled);
            output(RpcResponse::success(id, "set_auto_compaction"));
        }

        // =================================================================
        // Retry — flags only; no auto-retry-after-error mechanism exists
        // anywhere in the loop/harness yet (named residual, not silent).
        // =================================================================
        RpcCommand::SetAutoRetry { enabled } => {
            host.set_auto_retry_enabled(enabled);
            output(RpcResponse::success(id, "set_auto_retry"));
        }

        RpcCommand::AbortRetry => {
            output(RpcResponse::success(id, "abort_retry"));
        }

        // =================================================================
        // Session
        // =================================================================
        RpcCommand::GetSessionStats => match host.harness.session().get_stats() {
            Ok(stats) => output(RpcResponse::success_with(
                id,
                "get_session_stats",
                json!({
                    "messageCount": stats.message_count,
                    "cachedTokens": stats.cached_tokens,
                    "uncachedTokens": stats.uncached_tokens,
                    "totalTokens": stats.total_tokens,
                    "costTotal": stats.cost_total,
                }),
            )),
            Err(e) => output(RpcResponse::error(id, "get_session_stats", e.message)),
        },

        RpcCommand::GetEntries { since } => {
            let entries = match host.harness.entries() {
                Ok(entries) => entries,
                Err(e) => {
                    output(RpcResponse::error(id, "get_entries", e.message));
                    return;
                }
            };
            let entries = match since {
                Some(since_id) => match entries.iter().position(|e| e.id() == since_id) {
                    Some(idx) => entries[idx + 1..].to_vec(),
                    None => {
                        output(RpcResponse::error(
                            id,
                            "get_entries",
                            format!("Entry not found: {since_id}"),
                        ));
                        return;
                    }
                },
                None => entries,
            };
            let leaf_id = host.harness.session().get_leaf_id().ok().flatten();
            output(RpcResponse::success_with(
                id,
                "get_entries",
                json!({ "entries": entries, "leafId": leaf_id }),
            ));
        }

        RpcCommand::GetTree => match host.harness.session().find_entries(&EntryQuery::default()) {
            Ok(entries) => {
                let leaf_id = host.harness.session().get_leaf_id().ok().flatten();
                output(RpcResponse::success_with(
                    id,
                    "get_tree",
                    json!({ "tree": entries, "leafId": leaf_id }),
                ));
            }
            Err(e) => output(RpcResponse::error(id, "get_tree", e.message)),
        },

        RpcCommand::GetLastAssistantText => {
            let text = host
                .harness
                .messages()
                .iter()
                .rev()
                .find_map(|m| match m {
                    AgentMessage::Llm(Message::Assistant(_)) => {
                        serde_json::to_value(m).ok().map(|v| assistant_text(&v))
                    }
                    _ => None,
                })
                .unwrap_or_default();
            output(RpcResponse::success_with(
                id,
                "get_last_assistant_text",
                json!({ "text": text }),
            ));
        }

        RpcCommand::SetSessionName { name } => {
            let name = name.trim().to_string();
            if name.is_empty() {
                output(RpcResponse::error(
                    id,
                    "set_session_name",
                    "Session name cannot be empty",
                ));
                return;
            }
            match host.harness.session().set_name(Some(&name)) {
                Ok(()) => output(RpcResponse::success(id, "set_session_name")),
                Err(e) => output(RpcResponse::error(id, "set_session_name", e.message)),
            }
        }

        // =================================================================
        // Messages
        // =================================================================
        RpcCommand::GetMessages => {
            let messages = host.harness.messages();
            output(RpcResponse::success_with(
                id,
                "get_messages",
                json!({ "messages": messages }),
            ));
        }

        // =================================================================
        // Commands — trivially empty: no extension commands/prompt
        // templates/skills are wired into `AgentHarness` yet (feat-007).
        // =================================================================
        RpcCommand::GetCommands => {
            output(RpcResponse::success_with(
                id,
                "get_commands",
                json!({ "commands": [] }),
            ));
        }

        // =================================================================
        // Out of scope this wave (named, see module docs) — each needs the
        // AgentSession-equivalent capability pirust doesn't have yet.
        // =================================================================
        RpcCommand::NewSession { .. } => output(not_supported(id, "new_session")),
        RpcCommand::Bash { .. } => output(not_supported(id, "bash")),
        RpcCommand::AbortBash => output(not_supported(id, "abort_bash")),
        RpcCommand::ExportHtml { .. } => output(not_supported(id, "export_html")),
        RpcCommand::SwitchSession { .. } => output(not_supported(id, "switch_session")),
        RpcCommand::Fork { .. } => output(not_supported(id, "fork")),
        RpcCommand::Clone => output(not_supported(id, "clone")),
        RpcCommand::GetForkMessages => output(not_supported(id, "get_fork_messages")),
    }
}
