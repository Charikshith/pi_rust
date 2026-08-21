//! v4 session context building — port of
//! `packages/agent/src/harness/session/context.ts`.
//!
//! Pure functions over the entry path: derive the session's current
//! thinking-level / model / active-tools state, apply entry transforms
//! (default: collapse everything before the latest compaction into that
//! compaction), and project entries into `AgentMessage`s.

use crate::harness::messages::{
    create_branch_summary_message, create_compaction_summary_message, AgentMessage,
};
use pirust_ai::types::Message;

use super::types::{CustomEntry, Entry};

/// `SessionContext` (context.ts:11-17).
#[derive(Debug, Clone, PartialEq)]
pub struct SessionContext {
    pub messages: Vec<AgentMessage>,
    pub thinking_level: String,
    pub model: Option<SessionContextModel>,
    pub active_tool_names: Option<Vec<String>>,
}

/// `SessionContext["model"]` element (context.ts:15).
#[derive(Debug, Clone, PartialEq)]
pub struct SessionContextModel {
    pub provider: String,
    pub model_id: String,
}

/// `ContextEntryTransform` (context.ts:19-21).
pub type ContextEntryTransform = fn(&[Entry]) -> Vec<Entry>;

/// `CustomEntryContextMessageProjector` (context.ts:23-26) — a custom entry's
/// `customType` maps to an optional list of messages (or none to skip).
pub type CustomEntryContextMessageProjector =
    fn(&CustomEntry, usize, &[Entry]) -> Option<Vec<AgentMessage>>;

/// `SessionContextBuildOptions` (context.ts:28-30).
#[derive(Debug, Clone, Default)]
pub struct SessionContextBuildOptions {
    pub entry_transforms: Option<Vec<ContextEntryTransform>>,
    pub entry_projectors:
        Option<std::collections::HashMap<String, CustomEntryContextMessageProjector>>,
}

/// `deriveSessionContextState` (context.ts:33-51) — fold the path entries into
/// the non-message session state.
fn derive_session_context_state(path_entries: &[Entry]) -> SessionContextState {
    let mut thinking_level = "off".to_string();
    let mut model: Option<SessionContextModel> = None;
    let mut active_tool_names: Option<Vec<String>> = None;

    for entry in path_entries {
        match entry {
            Entry::ThinkingLevel(e) => thinking_level = e.thinking_level.clone(),
            Entry::ModelChange(e) => {
                model = Some(SessionContextModel {
                    provider: e.provider.clone(),
                    model_id: e.model_id.clone(),
                });
            }
            Entry::Message(e) => {
                if let AgentMessage::Llm(Message::Assistant(a)) = &e.message {
                    model = Some(SessionContextModel {
                        provider: a.provider.0.clone(),
                        model_id: a.model.clone().unwrap_or_default(),
                    });
                }
            }
            Entry::ActiveTools(e) => active_tool_names = Some(e.active_tool_names.clone()),
            _ => {}
        }
    }

    SessionContextState {
        thinking_level,
        model,
        active_tool_names,
    }
}

/// Intermediate state from [`derive_session_context_state`] (context.ts:33).
struct SessionContextState {
    thinking_level: String,
    model: Option<SessionContextModel>,
    active_tool_names: Option<Vec<String>>,
}

/// `defaultContextEntryTransform` (context.ts:54-69) — find the LAST compaction
/// entry; if none, return the path unchanged; otherwise return
/// `[compaction, ...entriesAfterCompaction]`.
pub fn default_context_entry_transform(path_entries: &[Entry]) -> Vec<Entry> {
    let mut compaction_index = -1isize;
    for (index, entry) in path_entries.iter().enumerate().rev() {
        if entry.entry_type() == "compaction" {
            compaction_index = index as isize;
            break;
        }
    }
    if compaction_index < 0 {
        path_entries.to_vec()
    } else {
        let index = compaction_index as usize;
        let mut result = vec![path_entries[index].clone()];
        result.extend_from_slice(&path_entries[index + 1..]);
        result
    }
}

/// `buildContextEntries` (context.ts:71-75).
pub fn build_context_entries(
    path_entries: &[Entry],
    options: &SessionContextBuildOptions,
) -> Vec<Entry> {
    let mut entries = default_context_entry_transform(path_entries);
    for transform in options.entry_transforms.clone().unwrap_or_default() {
        entries = transform(&entries);
    }
    entries
}

/// `sessionEntryToContextMessages` (context.ts:78-97).
pub fn session_entry_to_context_messages(
    entry: &Entry,
    index: usize,
    entries: &[Entry],
    options: &SessionContextBuildOptions,
) -> Vec<AgentMessage> {
    match entry {
        Entry::Message(e) => {
            // context.ts:72 — `entry.message.stopReason === "deferred"`. The
            // port's `StopReason` enum has no `Deferred` variant (the wire
            // value is deferred-to-adapter); the raw stop reason carries it.
            if let AgentMessage::Llm(Message::Assistant(a)) = &e.message {
                if a.raw_stop_reason.as_deref() == Some("deferred") {
                    return vec![];
                }
            }
            vec![e.message.clone()]
        }
        Entry::Compaction(e) => {
            let mut messages = vec![AgentMessage::CompactionSummary(
                create_compaction_summary_message(e.summary.clone(), e.tokens_before, e.timestamp),
            )];
            messages.extend(e.retained_tail.iter().cloned());
            messages
        }
        Entry::BranchSummary(e) => {
            if e.summary.is_empty() {
                vec![]
            } else {
                vec![AgentMessage::BranchSummary(create_branch_summary_message(
                    e.summary.clone(),
                    e.from_id.clone(),
                    e.timestamp,
                ))]
            }
        }
        Entry::Custom(e) => {
            if let Some(projector) = options
                .entry_projectors
                .as_ref()
                .and_then(|map| map.get(&e.custom_type))
            {
                projector(e, index, entries).unwrap_or_default()
            } else {
                vec![]
            }
        }
        _ => vec![],
    }
}

/// `buildSessionContext` (context.ts:99-104).
pub fn build_session_context(
    path_entries: &[Entry],
    options: &SessionContextBuildOptions,
) -> SessionContext {
    let state = derive_session_context_state(path_entries);
    let context_entries = build_context_entries(path_entries, options);
    let messages = context_entries
        .iter()
        .enumerate()
        .flat_map(|(index, entry)| {
            session_entry_to_context_messages(entry, index, &context_entries, options)
        })
        .collect();
    SessionContext {
        messages,
        thinking_level: state.thinking_level,
        model: state.model,
        active_tool_names: state.active_tool_names,
    }
}
