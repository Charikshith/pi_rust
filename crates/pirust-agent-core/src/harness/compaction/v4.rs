//! v4-shaped conversation compaction — port of
//! `packages/agent/src/harness/compaction/compaction.ts` operating on the v4
//! `Entry` model (`packages/agent/src/harness/session/types.ts`).
//!
//! The 0.84.2 oracle's `prepareCompaction` consumes v4 `Entry`s (not the v3
//! `SessionTreeEntry` tree) and produces a `retainedTail` stored on the v4
//! `CompactionEntry`. This module is that exact shape.
//!
//! Deterministic pieces ported here:
//! - [`prepare_compaction`] — boundary / previous-summary / cut-point / message
//!   selection (compaction.ts:616-690) including `virtualRetainedEntries`
//!   (compaction.ts:630-643) and `retainedTail` (compaction.ts:665-680).
//! - [`find_cut_point`] / [`find_valid_cut_points`] / [`find_turn_start_index`]
//!   (compaction.ts:312-405).
//! - [`get_message_from_entry`] / [`get_message_from_entry_for_compaction`]
//!   (compaction.ts:59-96).
//!
//! Token estimation reuses the v3 module's [`super::estimate_tokens`] /
//! [`super::estimate_context_tokens`] — they operate on `AgentMessage`, which
//! both entry models carry identically.
//!
//! # What is DEFERRED (same as v3)
//!
//! The LLM-driven summary generation (`generateSummary`, `compact`) is
//! model-generated and not byte-verifiable; `fileOps` (`utils.ts` —
//! `extractFileOperations` / `computeFileLists` / `formatFileOperations`) is
//! likewise deferred until `compaction/utils.ts` is ported. `previous_summary`
//! is carried (deterministic input to the deferred summarizer).

use crate::harness::messages::{
    create_branch_summary_message, create_compaction_summary_message, AgentMessage,
};
use crate::harness::session::v4::context::build_session_context;
use crate::harness::session::v4::types::{CompactionEntry, Entry, MessageEntry};

use super::{estimate_context_tokens, estimate_tokens, CompactionError, CompactionSettings};

/// `getMessageFromEntry` (compaction.ts:59-72) — project a v4 entry into the
/// [`AgentMessage`] it contributes to a compaction summary. Message entries pass
/// through; branch_summary / compaction map to their message variants; all
/// change/custom entries produce nothing.
pub fn get_message_from_entry(entry: &Entry) -> Option<AgentMessage> {
    match entry {
        Entry::Message(e) => Some(e.message.clone()),
        Entry::BranchSummary(e) => Some(AgentMessage::BranchSummary(
            create_branch_summary_message(e.summary.clone(), e.from_id.clone(), e.timestamp),
        )),
        Entry::Compaction(e) => Some(AgentMessage::CompactionSummary(
            create_compaction_summary_message(e.summary.clone(), e.tokens_before, e.timestamp),
        )),
        _ => None,
    }
}

/// `getMessageFromEntryForCompaction` (compaction.ts:74-77) — like
/// [`get_message_from_entry`] but skips compaction entries.
fn get_message_from_entry_for_compaction(entry: &Entry) -> Option<AgentMessage> {
    match entry {
        Entry::Compaction(_) => None,
        _ => get_message_from_entry(entry),
    }
}

/// `findValidCutPoints` (compaction.ts:312-345) — indices where compaction may
/// cut: every non-toolResult message role, plus `branch_summary` entries.
fn find_valid_cut_points(entries: &[Entry], start_index: usize, end_index: usize) -> Vec<usize> {
    let mut cut_points = Vec::new();
    for (i, entry) in entries.iter().enumerate().take(end_index).skip(start_index) {
        if let Entry::Message(e) = entry {
            let role_is_cut = !matches!(
                e.message,
                AgentMessage::Llm(pirust_ai::types::Message::ToolResult(_))
            );
            if role_is_cut {
                cut_points.push(i);
            }
        }
        if matches!(entry, Entry::BranchSummary(_)) {
            cut_points.push(i);
        }
    }
    cut_points
}

/// `findTurnStartIndex` (compaction.ts:347-363) — the user-visible message that
/// starts the turn containing an entry, or `-1` when none is found.
pub fn find_turn_start_index(entries: &[Entry], entry_index: usize, start_index: usize) -> i64 {
    for i in (start_index..=entry_index).rev() {
        let entry = &entries[i];
        if matches!(entry, Entry::BranchSummary(_)) {
            return i as i64;
        }
        if let Entry::Message(e) = entry {
            if matches!(
                e.message,
                AgentMessage::Llm(pirust_ai::types::Message::User(_))
                    | AgentMessage::BashExecution(_)
            ) {
                return i as i64;
            }
        }
    }
    -1
}

/// Cut point selected for compaction (`CutPointResult`, compaction.ts:365-373).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CutPointResult {
    /// Index of the first entry retained after compaction.
    pub first_kept_entry_index: usize,
    /// Index of the turn-start entry when the cut splits a turn, otherwise `-1`.
    pub turn_start_index: i64,
    /// Whether the selected cut point splits an in-progress turn.
    pub is_split_turn: bool,
}

/// `findCutPoint` (compaction.ts:375-405) — find the cut point that keeps
/// approximately the requested recent-token budget.
pub fn find_cut_point(
    entries: &[Entry],
    start_index: usize,
    end_index: usize,
    keep_recent_tokens: i64,
) -> CutPointResult {
    let cut_points = find_valid_cut_points(entries, start_index, end_index);

    if cut_points.is_empty() {
        return CutPointResult {
            first_kept_entry_index: start_index,
            turn_start_index: -1,
            is_split_turn: false,
        };
    }

    let mut accumulated_tokens = 0i64;
    let mut cut_index = cut_points[0];

    // Walk backwards accumulating MESSAGE-entry tokens; once past the budget,
    // pick the first cut point at/after the current index (compaction.ts:380-390).
    let mut i = end_index as i64 - 1;
    while i >= start_index as i64 {
        let idx = i as usize;
        if let Entry::Message(e) = &entries[idx] {
            accumulated_tokens += estimate_tokens(&e.message);
            if accumulated_tokens >= keep_recent_tokens {
                for &c in &cut_points {
                    if c >= idx {
                        cut_index = c;
                        break;
                    }
                }
                break;
            }
        }
        i -= 1;
    }

    // Walk left while the previous entry is neither compaction nor message
    // (compaction.ts:392-399).
    while cut_index > start_index {
        let prev_entry = &entries[cut_index - 1];
        if matches!(prev_entry, Entry::Compaction(_)) {
            break;
        }
        if matches!(prev_entry, Entry::Message(_)) {
            break;
        }
        cut_index -= 1;
    }

    let cut_entry = &entries[cut_index];
    let is_user_message = matches!(
        cut_entry,
        Entry::Message(e) if matches!(e.message, AgentMessage::Llm(pirust_ai::types::Message::User(_)))
    );
    let turn_start_index = if is_user_message {
        -1
    } else {
        find_turn_start_index(entries, cut_index, start_index)
    };

    CutPointResult {
        first_kept_entry_index: cut_index,
        turn_start_index,
        is_split_turn: !is_user_message && turn_start_index != -1,
    }
}

/// Prepared inputs for a compaction run (`CompactionPreparation`,
/// compaction.ts:596-610).
///
/// NOTE: Pi's `fileOps` field is intentionally absent — file-operation
/// extraction lives in the not-yet-ported `compaction/utils.ts` (same deferral
/// as the v3 module).
#[derive(Debug, Clone, PartialEq)]
pub struct CompactionPreparation {
    /// Messages summarized into the history summary.
    pub messages_to_summarize: Vec<AgentMessage>,
    /// Prefix messages summarized separately when compaction splits a turn.
    pub turn_prefix_messages: Vec<AgentMessage>,
    /// Recent messages retained after compaction and stored on the compaction
    /// entry (`retainedTail`).
    pub retained_tail: Vec<AgentMessage>,
    /// Whether compaction splits a turn.
    pub is_split_turn: bool,
    /// Estimated context tokens before compaction.
    pub tokens_before: i64,
    /// Previous compaction summary used for iterative updates.
    pub previous_summary: Option<String>,
    /// Settings used to prepare compaction.
    pub settings: CompactionSettings,
}

/// `prepareCompaction` (compaction.ts:616-690) — prepare session entries for
/// compaction, or return `Ok(None)` when compaction is not applicable.
pub fn prepare_compaction(
    path_entries: &[Entry],
    settings: &CompactionSettings,
) -> Result<Option<CompactionPreparation>, CompactionError> {
    if path_entries.is_empty() || matches!(path_entries.last(), Some(Entry::Compaction(_))) {
        return Ok(None);
    }

    // Locate the most recent compaction entry (compaction.ts:625-630).
    let mut prev_compaction_index = -1isize;
    for (i, entry) in path_entries.iter().enumerate().rev() {
        if matches!(entry, Entry::Compaction(_)) {
            prev_compaction_index = i as isize;
            break;
        }
    }

    let mut previous_summary: Option<String> = None;
    let mut compactable_entries: Vec<Entry> = path_entries.to_vec();
    if prev_compaction_index >= 0 {
        let pci = prev_compaction_index as usize;
        if let Entry::Compaction(prev) = &path_entries[pci] {
            previous_summary = Some(prev.summary.clone());
            // Virtual retained entries re-materialize the previous compaction's
            // retainedTail so the cut-point walk sees them as real messages
            // (compaction.ts:630-643).
            let virtual_retained: Vec<Entry> = prev
                .retained_tail
                .iter()
                .enumerate()
                .map(|(index, message)| {
                    Entry::Message(MessageEntry {
                        id: format!("{}:retained:{}", prev.id, index),
                        seq: prev.seq,
                        parent_id: if index == 0 {
                            Some(prev.id.clone())
                        } else {
                            Some(format!("{}:retained:{}", prev.id, index - 1))
                        },
                        timestamp: message_timestamp(message),
                        message: message.clone(),
                        terminate: None,
                    })
                })
                .collect();
            compactable_entries = virtual_retained;
            compactable_entries.extend_from_slice(&path_entries[pci + 1..]);
        }
    }
    let boundary_end = compactable_entries.len();

    let tokens_before =
        estimate_context_tokens(&build_session_context(path_entries, &Default::default()).messages)
            .tokens;

    let cut_point = find_cut_point(
        &compactable_entries,
        0,
        boundary_end,
        settings.keep_recent_tokens,
    );
    let history_end = if cut_point.is_split_turn {
        cut_point.turn_start_index as usize
    } else {
        cut_point.first_kept_entry_index
    };

    let mut messages_to_summarize: Vec<AgentMessage> = Vec::new();
    for entry in &compactable_entries[0..history_end] {
        if let Some(msg) = get_message_from_entry_for_compaction(entry) {
            messages_to_summarize.push(msg);
        }
    }

    let mut turn_prefix_messages: Vec<AgentMessage> = Vec::new();
    if cut_point.is_split_turn {
        for entry in &compactable_entries
            [cut_point.turn_start_index as usize..cut_point.first_kept_entry_index]
        {
            if let Some(msg) = get_message_from_entry_for_compaction(entry) {
                turn_prefix_messages.push(msg);
            }
        }
    }

    let mut retained_tail: Vec<AgentMessage> = Vec::new();
    for entry in &compactable_entries[cut_point.first_kept_entry_index..boundary_end] {
        if let Some(msg) = get_message_from_entry_for_compaction(entry) {
            retained_tail.push(msg);
        }
    }

    // TODO(feat-003 harness/compaction utils): `fileOps` (extractFileOperations,
    // compaction.ts:678-687) is computed here in Pi — deferred until
    // `compaction/utils.ts` is ported.

    Ok(Some(CompactionPreparation {
        messages_to_summarize,
        turn_prefix_messages,
        retained_tail,
        is_split_turn: cut_point.is_split_turn,
        tokens_before,
        previous_summary,
        settings: settings.clone(),
    }))
}

/// Synthesize a v4 `Entry` path from a flat live message list and run
/// [`prepare_compaction`] over it.
///
/// # Why this exists
///
/// `prepare_compaction` is written against the v4 session-tree `Entry` model
/// (`harness/session/v4/types.rs`), because that's the persisted shape the
/// 0.84.2 oracle's RPC compaction path operates on. The interactive TUI's
/// `SingleTurnSession` (`pirust-coding-agent/src/runtime_host.rs`), by
/// contrast, holds conversation state as a flat `Vec<AgentMessage>` on
/// [`crate::agent::Agent`] (`agent.rs:345` `messages()` / `agent.rs:360`
/// `set_messages()`) — there is no `Entry` tree at all in that harness. This
/// function bridges the two: it wraps each live message in a minimal,
/// linearly-chained `Entry` so `prepare_compaction`'s boundary / cut-point /
/// token-budget logic — which is exactly what we want reused, not
/// reimplemented — can run unmodified.
///
/// # Synthesis rules
///
/// Entry `i` gets `id = "live:{i}"`, `seq = i`, and `parent_id =
/// Some("live:{i-1}")` (or `None` for `i == 0`) — a straight-line chain
/// matching message order, since the flat live list has no branching.
/// `AgentMessage::CompactionSummary` messages become `Entry::Compaction`
/// (so `prepare_compaction`'s "already compacted, nothing to do" and
/// "previous summary" logic see them as compaction boundaries, not plain
/// messages); every other variant, including `AgentMessage::BranchSummary`,
/// becomes a plain `Entry::Message`. Mapping `BranchSummary` to a message
/// entry rather than `Entry::BranchSummary` is a deliberate simplification:
/// `BranchSummary` is a v3 tree-navigation concept, and `SingleTurnSession`
/// (a single, non-branching turn sequence) never produces one — so the two
/// representations are behaviorally identical for every input this function
/// will actually see.
///
/// The synthesized `Entry::Compaction`'s `retained_tail` is always left
/// empty — this is correct, not a shortcut. `prepare_compaction` only reads
/// a previous compaction's `retained_tail` to *re-materialize* messages that
/// would otherwise be missing from the entry path (see
/// `compactable_entries` construction above). Here, nothing is missing: the
/// flat live list already carries that same tail as literal, subsequent
/// `AgentMessage`s / `Entry::Message`s right after the compaction entry.
/// Populating `retained_tail` too would make `build_session_context`
/// double-count that tail (see
/// `harness/session/v4/context.rs::session_entry_to_context_messages`'s
/// `Entry::Compaction` arm, which re-emits `[summary] + retained_tail`).
///
/// Because entries are synthesized 1:1, in order, from `messages`, the
/// resulting `CompactionPreparation::retained_tail` is guaranteed to be a
/// value-for-value suffix of `messages` — callers may safely recover *which*
/// original message index starts the retained tail by counting from the
/// end, without needing to match on `Entry` ids.
pub fn prepare_compaction_from_messages(
    messages: &[AgentMessage],
    settings: &CompactionSettings,
) -> Result<Option<CompactionPreparation>, CompactionError> {
    let entries: Vec<Entry> = messages
        .iter()
        .enumerate()
        .map(|(index, message)| {
            let id = format!("live:{index}");
            let parent_id = (index > 0).then(|| format!("live:{}", index - 1));
            let timestamp = message_timestamp(message);
            match message {
                AgentMessage::CompactionSummary(cs) => Entry::Compaction(CompactionEntry {
                    id,
                    seq: index as i64,
                    parent_id,
                    timestamp,
                    summary: cs.summary.clone(),
                    retained_tail: Vec::new(),
                    tokens_before: cs.tokens_before,
                    details: None,
                    usage: None,
                }),
                _ => Entry::Message(MessageEntry {
                    id,
                    seq: index as i64,
                    parent_id,
                    timestamp,
                    message: message.clone(),
                    terminate: None,
                }),
            }
        })
        .collect();
    prepare_compaction(&entries, settings)
}

/// The message's `timestamp` (used for virtual retained entries; compaction.ts
/// uses `message.timestamp` directly). Message timestamps are Unix ms.
fn message_timestamp(message: &AgentMessage) -> i64 {
    match message {
        AgentMessage::Llm(pirust_ai::types::Message::User(u)) => u.timestamp,
        AgentMessage::Llm(pirust_ai::types::Message::Assistant(a)) => a.timestamp,
        AgentMessage::Llm(pirust_ai::types::Message::ToolResult(t)) => t.timestamp,
        AgentMessage::BashExecution(b) => b.timestamp,
        AgentMessage::Custom(c) => c.timestamp,
        AgentMessage::BranchSummary(b) => b.timestamp,
        AgentMessage::CompactionSummary(c) => c.timestamp,
    }
}
