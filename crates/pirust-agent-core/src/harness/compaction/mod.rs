//! Conversation compaction — port of
//! `packages/agent/src/harness/compaction/compaction.ts`.
//!
//! Spec: `docs/analysis/07-agent-core-spec.md` §4.5 (precise compaction
//! algorithm), §11.B (compaction determinism oracle), §12 (UTF-16 length
//! gotcha). Depends on `harness::types` + `harness::session` +
//! `harness::messages` (§13).
//!
//! # What is ported here (the DETERMINISTIC pieces)
//!
//! - [`estimate_tokens`] / [`calculate_context_tokens`] / [`should_compact`] /
//!   [`estimate_context_tokens`] — token accounting (compaction.ts:118-264).
//! - [`find_turn_start_index`] / [`find_cut_point`] — cut-point selection
//!   (compaction.ts:265-381).
//! - [`prepare_compaction`] — boundary / previous-summary / cut-point / message
//!   selection (compaction.ts:545-610).
//! - [`get_last_assistant_usage`] — last valid assistant usage (compaction.ts:137-146).
//!
//! # What is DEFERRED (not byte-verifiable / needs unported modules)
//!
//! The LLM-driven summary generation — `generateSummary`, `generateTurnPrefixSummary`,
//! and `compact` (compaction.ts:459-522, 630-753) — is **model-generated** and
//! therefore not byte-verifiable (spec §11.B: "there is NO golden after-compaction
//! fixture and no byte assertion on a compacted result"). It is NOT ported here.
//!
//! [`CompactionPreparation`] intentionally omits the `fileOps` field that Pi
//! computes (compaction.ts:593-598): `extractFileOperations` /
//! `extractFileOpsFromMessage` live in the not-yet-ported `compaction/utils.ts`
//! module. File-operation extraction lands with that module (see TODO below).
//!
//! # UTF-16 length semantics (§12, compaction.ts:216)
//!
//! Pi measures string length with JS `.length`, which counts **UTF-16 code
//! units**, not Unicode scalar values (`char`) or UTF-8 bytes. Every char count
//! here therefore uses [`str::encode_utf16`]`().count()`. For a string of only
//! BMP/ASCII characters this equals the char count, but astral characters (emoji,
//! etc.) count as 2 — matching JS exactly.
//
// TODO(feat-003 harness/compaction utils): once `compaction/utils.ts` is ported,
// add the `fileOps` field to `CompactionPreparation` (extractFileOperations,
// compaction.ts:35-58) and port the LLM summary pipeline (`generateSummary` /
// `compact`) + `branch_summarization`.
//
// The v4-`Entry`-shaped port (0.84.2 oracle) lives in [`v4`] — the same
// algorithms over the v4 mutation-log entry model, with `retainedTail`.

pub mod v4;

use crate::harness::messages::{
    create_branch_summary_message, create_compaction_summary_message, create_custom_message,
    AgentMessage,
};
use crate::harness::session::{build_session_context, entry_id};
use crate::harness::types::{CompactionError, CompactionErrorCode, SessionTreeEntry};
use pirust_ai::types::{Message, StopReason, Usage, UserContent, UserMessageContent};

// ===========================================================================
// Settings (compaction.ts:100-115)
// ===========================================================================

/// Compaction thresholds and retention settings (compaction.ts:101-108).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionSettings {
    /// Enable automatic compaction decisions.
    pub enabled: bool,
    /// Tokens reserved for summary prompt and output.
    pub reserve_tokens: i64,
    /// Approximate recent-context tokens to keep after compaction.
    pub keep_recent_tokens: i64,
}

/// Default compaction settings used by the harness (compaction.ts:111-115).
pub const DEFAULT_COMPACTION_SETTINGS: CompactionSettings = CompactionSettings {
    enabled: true,
    reserve_tokens: 16384,
    keep_recent_tokens: 20000,
};

// ===========================================================================
// Token accounting (compaction.ts:118-264)
// ===========================================================================

/// Approximate characters contributed by one image block (compaction.ts:205).
const ESTIMATED_IMAGE_CHARS: i64 = 4800;

/// Calculate total context tokens from provider usage (compaction.ts:118-120):
/// `usage.totalTokens || input + output + cacheRead + cacheWrite`. The JS `||`
/// falls through when `totalTokens` is `0`/absent, so a reported `0` uses the sum.
pub fn calculate_context_tokens(usage: &Usage) -> i64 {
    match usage.total_tokens {
        Some(t) if t != 0 => t as i64,
        _ => (usage.input + usage.output + usage.cache_read + usage.cache_write) as i64,
    }
}

/// UTF-16 code-unit length of a string (§12): JS `.length` semantics.
fn utf16_len(s: &str) -> i64 {
    s.encode_utf16().count() as i64
}

/// Chars for a `(TextContent | ImageContent)[]` block list (compaction.ts:212-220).
fn chars_from_blocks(blocks: &[UserContent]) -> i64 {
    blocks
        .iter()
        .map(|b| match b {
            UserContent::Text(t) => utf16_len(&t.text),
            UserContent::Image(_) => ESTIMATED_IMAGE_CHARS,
        })
        .sum()
}

/// Chars for a `string | (TextContent | ImageContent)[]` value
/// (`estimateTextAndImageContentChars`, compaction.ts:207-221).
fn chars_from_content(content: &UserMessageContent) -> i64 {
    match content {
        UserMessageContent::Text(s) => utf16_len(s),
        UserMessageContent::Blocks(blocks) => chars_from_blocks(blocks),
    }
}

/// `JSON.stringify(value)` for tool-call arguments (`safeJsonStringify`,
/// compaction.ts:27-33). serde_json emits the same compact, no-space form as
/// `JSON.stringify`; a serialization failure maps to `"[unserializable]"`.
fn safe_json_stringify_args(args: &serde_json::Map<String, serde_json::Value>) -> String {
    serde_json::to_string(args).unwrap_or_else(|_| "[unserializable]".to_string())
}

/// Estimate token count for one message via a conservative char heuristic
/// (`estimateTokens`, compaction.ts:224-264): sum chars then `ceil(chars / 4)`.
///
/// Every representable [`AgentMessage`] role is handled. Pi's `return 0`
/// fallthrough (compaction.ts:263, for an out-of-domain "unknown" role) is
/// unreachable here: the enum is closed, so a role outside the seven handled
/// arms cannot be constructed — the zero case is statically eliminated.
pub fn estimate_tokens(message: &AgentMessage) -> i64 {
    let chars: i64 = match message {
        AgentMessage::Llm(Message::User(u)) => chars_from_content(&u.content),
        AgentMessage::Llm(Message::Assistant(a)) => {
            let mut chars = 0i64;
            for block in &a.content {
                match block {
                    pirust_ai::types::AssistantContent::Text(t) => chars += utf16_len(&t.text),
                    pirust_ai::types::AssistantContent::Thinking(th) => {
                        chars += utf16_len(&th.thinking)
                    }
                    pirust_ai::types::AssistantContent::ToolCall(tc) => {
                        chars += utf16_len(&tc.name)
                            + utf16_len(&safe_json_stringify_args(&tc.arguments))
                    }
                }
            }
            chars
        }
        AgentMessage::Custom(c) => chars_from_content(&c.content),
        AgentMessage::Llm(Message::ToolResult(tr)) => chars_from_blocks(&tr.content),
        AgentMessage::BashExecution(b) => utf16_len(&b.command) + utf16_len(&b.output),
        AgentMessage::BranchSummary(bs) => utf16_len(&bs.summary),
        AgentMessage::CompactionSummary(cs) => utf16_len(&cs.summary),
    };
    // Math.ceil(chars / 4) for non-negative `chars` (ceiling integer division).
    (chars + 3) / 4
}

/// Return usage from a valid, non-aborted/error assistant message with >0 context
/// tokens (`getAssistantUsage`, compaction.ts:121-134).
fn get_assistant_usage(msg: &AgentMessage) -> Option<&Usage> {
    if let AgentMessage::Llm(Message::Assistant(a)) = msg {
        if a.stop_reason != StopReason::Aborted
            && a.stop_reason != StopReason::Error
            && calculate_context_tokens(&a.usage) > 0
        {
            return Some(&a.usage);
        }
    }
    None
}

/// Return usage from the last valid assistant message in session entries
/// (`getLastAssistantUsage`, compaction.ts:137-146).
pub fn get_last_assistant_usage(entries: &[SessionTreeEntry]) -> Option<Usage> {
    for entry in entries.iter().rev() {
        if let SessionTreeEntry::Message { message, .. } = entry {
            if let Some(usage) = get_assistant_usage(message) {
                return Some(*usage);
            }
        }
    }
    None
}

/// Estimated context-token usage for a message list (`ContextUsageEstimate`,
/// compaction.ts:149-158).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextUsageEstimate {
    /// Estimated total context tokens.
    pub tokens: i64,
    /// Tokens reported by the most recent assistant usage block.
    pub usage_tokens: i64,
    /// Estimated tokens after the most recent assistant usage block.
    pub trailing_tokens: i64,
    /// Index of the message that provided usage, or `None` when none exists.
    pub last_usage_index: Option<usize>,
}

/// Index + usage of the last valid assistant message (`getLastAssistantUsageInfo`,
/// compaction.ts:160-166).
fn get_last_assistant_usage_info(messages: &[AgentMessage]) -> Option<(&Usage, usize)> {
    for i in (0..messages.len()).rev() {
        if let Some(usage) = get_assistant_usage(&messages[i]) {
            return Some((usage, i));
        }
    }
    None
}

/// Estimate context tokens for messages, using provider usage when available
/// (`estimateContextTokens`, compaction.ts:169-197). When a valid assistant usage
/// block exists, `tokens = usageTokens + trailingTokens` (chars after it);
/// otherwise the sum of every message's [`estimate_tokens`].
pub fn estimate_context_tokens(messages: &[AgentMessage]) -> ContextUsageEstimate {
    match get_last_assistant_usage_info(messages) {
        None => {
            let estimated: i64 = messages.iter().map(estimate_tokens).sum();
            ContextUsageEstimate {
                tokens: estimated,
                usage_tokens: 0,
                trailing_tokens: estimated,
                last_usage_index: None,
            }
        }
        Some((usage, index)) => {
            let usage_tokens = calculate_context_tokens(usage);
            let mut trailing_tokens = 0i64;
            for msg in &messages[index + 1..] {
                trailing_tokens += estimate_tokens(msg);
            }
            ContextUsageEstimate {
                tokens: usage_tokens + trailing_tokens,
                usage_tokens,
                trailing_tokens,
                last_usage_index: Some(index),
            }
        }
    }
}

/// Return whether context usage exceeds the configured compaction threshold
/// (`shouldCompact`, compaction.ts:200-203).
pub fn should_compact(
    context_tokens: i64,
    context_window: i64,
    settings: &CompactionSettings,
) -> bool {
    if !settings.enabled {
        return false;
    }
    context_tokens > context_window - settings.reserve_tokens
}

// ===========================================================================
// Cut-point selection (compaction.ts:265-381)
// ===========================================================================

/// Whether a `message` entry's role is eligible as a cut point: every role EXCEPT
/// `toolResult` (compaction.ts:270-284).
fn message_entry_is_cut_point(message: &AgentMessage) -> bool {
    !matches!(message, AgentMessage::Llm(Message::ToolResult(_)))
}

/// Indices where compaction may cut (`findValidCutPoints`, compaction.ts:265-303):
/// non-toolResult message entries, plus `branch_summary` / `custom_message` entries.
fn find_valid_cut_points(
    entries: &[SessionTreeEntry],
    start_index: usize,
    end_index: usize,
) -> Vec<usize> {
    let mut cut_points = Vec::new();
    for (i, entry) in entries.iter().enumerate().take(end_index).skip(start_index) {
        if let SessionTreeEntry::Message { message, .. } = entry {
            if message_entry_is_cut_point(message) {
                cut_points.push(i);
            }
        }
        if matches!(
            entry,
            SessionTreeEntry::BranchSummary { .. } | SessionTreeEntry::CustomMessage { .. }
        ) {
            cut_points.push(i);
        }
    }
    cut_points
}

/// Find the user-visible message that starts the turn containing an entry
/// (`findTurnStartIndex`, compaction.ts:306-320). Returns `-1` when no turn start
/// is found (matching Pi's number sentinel).
pub fn find_turn_start_index(
    entries: &[SessionTreeEntry],
    entry_index: usize,
    start_index: usize,
) -> i64 {
    for i in (start_index..=entry_index).rev() {
        let entry = &entries[i];
        if matches!(
            entry,
            SessionTreeEntry::BranchSummary { .. } | SessionTreeEntry::CustomMessage { .. }
        ) {
            return i as i64;
        }
        if let SessionTreeEntry::Message { message, .. } = entry {
            if matches!(
                message,
                AgentMessage::Llm(Message::User(_)) | AgentMessage::BashExecution(_)
            ) {
                return i as i64;
            }
        }
    }
    -1
}

/// Cut point selected for compaction (`CutPointResult`, compaction.ts:322-330).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CutPointResult {
    /// Index of the first entry retained after compaction.
    pub first_kept_entry_index: usize,
    /// Index of the turn-start entry when the cut splits a turn, otherwise `-1`.
    pub turn_start_index: i64,
    /// Whether the selected cut point splits an in-progress turn.
    pub is_split_turn: bool,
}

/// Find the compaction cut point that keeps approximately the requested recent-token
/// budget (`findCutPoint`, compaction.ts:333-381).
pub fn find_cut_point(
    entries: &[SessionTreeEntry],
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

    // Walk backwards accumulating MESSAGE-entry tokens; once past the budget, pick
    // the first cut point at/after the current index (compaction.ts:347-361).
    let mut i = end_index as i64 - 1;
    while i >= start_index as i64 {
        let idx = i as usize;
        if let SessionTreeEntry::Message { message, .. } = &entries[idx] {
            accumulated_tokens += estimate_tokens(message);
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
    // (compaction.ts:362-371).
    while cut_index > start_index {
        let prev_entry = &entries[cut_index - 1];
        if matches!(prev_entry, SessionTreeEntry::Compaction { .. }) {
            break;
        }
        if matches!(prev_entry, SessionTreeEntry::Message { .. }) {
            break;
        }
        cut_index -= 1;
    }

    let cut_entry = &entries[cut_index];
    let is_user_message = matches!(
        cut_entry,
        SessionTreeEntry::Message {
            message: AgentMessage::Llm(Message::User(_)),
            ..
        }
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

// ===========================================================================
// Entry -> message projection for compaction (compaction.ts:59-86)
// ===========================================================================

/// Parse the canonical `Date#toISOString` form (`YYYY-MM-DDTHH:MM:SS.sssZ`) into
/// Unix milliseconds — the `new Date(iso).getTime()` flip Pi's message constructors
/// perform (§1.2). Returns `0` for anything that does not parse (entry timestamps
/// are always canonical). Mirrors the private helper in `harness::session`.
fn iso8601_to_epoch_ms(s: &str) -> i64 {
    fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
        let y = if m <= 2 { y - 1 } else { y };
        let era = (if y >= 0 { y } else { y - 399 }) / 400;
        let yoe = y - era * 400;
        let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        era * 146097 + doe - 719468
    }
    fn parse(s: &str) -> Option<i64> {
        let s = s.strip_suffix('Z').unwrap_or(s);
        let (date, time) = s.split_once('T')?;
        let mut dp = date.split('-');
        let y: i64 = dp.next()?.parse().ok()?;
        let mo: i64 = dp.next()?.parse().ok()?;
        let d: i64 = dp.next()?.parse().ok()?;
        let (hms, frac) = match time.split_once('.') {
            Some((a, b)) => (a, Some(b)),
            None => (time, None),
        };
        let mut tp = hms.split(':');
        let hh: i64 = tp.next()?.parse().ok()?;
        let mm: i64 = tp.next()?.parse().ok()?;
        let ss: i64 = tp.next().unwrap_or("0").parse().ok()?;
        let millis: i64 = match frac {
            Some(f) => {
                let mut f = f.to_string();
                f.truncate(3);
                while f.len() < 3 {
                    f.push('0');
                }
                f.parse().ok()?
            }
            None => 0,
        };
        let days = days_from_civil(y, mo, d);
        Some(days * 86_400_000 + hh * 3_600_000 + mm * 60_000 + ss * 1000 + millis)
    }
    parse(s).unwrap_or(0)
}

/// Project a session-tree entry into the [`AgentMessage`] it contributes to a
/// compaction summary (`getMessageFromEntry`, compaction.ts:59-79). Custom-data,
/// change, label, session-info, and leaf entries produce nothing.
fn get_message_from_entry(entry: &SessionTreeEntry) -> Option<AgentMessage> {
    match entry {
        SessionTreeEntry::Message { message, .. } => Some(message.clone()),
        SessionTreeEntry::CustomMessage {
            custom_type,
            content,
            display,
            details,
            timestamp,
            ..
        } => Some(AgentMessage::Custom(create_custom_message(
            custom_type.clone(),
            content.clone(),
            *display,
            details.clone(),
            iso8601_to_epoch_ms(timestamp),
        ))),
        SessionTreeEntry::BranchSummary {
            summary,
            from_id,
            timestamp,
            ..
        } => Some(AgentMessage::BranchSummary(create_branch_summary_message(
            summary.clone(),
            from_id.clone(),
            iso8601_to_epoch_ms(timestamp),
        ))),
        SessionTreeEntry::Compaction {
            summary,
            tokens_before,
            timestamp,
            ..
        } => Some(AgentMessage::CompactionSummary(
            create_compaction_summary_message(
                summary.clone(),
                *tokens_before,
                iso8601_to_epoch_ms(timestamp),
            ),
        )),
        _ => None,
    }
}

/// As [`get_message_from_entry`] but skips `compaction` entries
/// (`getMessageFromEntryForCompaction`, compaction.ts:81-86).
fn get_message_from_entry_for_compaction(entry: &SessionTreeEntry) -> Option<AgentMessage> {
    if matches!(entry, SessionTreeEntry::Compaction { .. }) {
        return None;
    }
    get_message_from_entry(entry)
}

// ===========================================================================
// prepare_compaction (compaction.ts:524-610)
// ===========================================================================

/// Prepared inputs for a compaction run (`CompactionPreparation`,
/// compaction.ts:525-542).
///
/// NOTE: Pi's `fileOps` field is intentionally absent — file-operation extraction
/// lives in the not-yet-ported `compaction/utils.ts` (see the module-level TODO).
#[derive(Debug, Clone, PartialEq)]
pub struct CompactionPreparation {
    /// Entry id where retained history starts.
    pub first_kept_entry_id: String,
    /// Messages summarized into the history summary.
    pub messages_to_summarize: Vec<AgentMessage>,
    /// Prefix messages summarized separately when compaction splits a turn.
    pub turn_prefix_messages: Vec<AgentMessage>,
    /// Whether compaction splits a turn.
    pub is_split_turn: bool,
    /// Estimated context tokens before compaction.
    pub tokens_before: i64,
    /// Previous compaction summary used for iterative updates.
    pub previous_summary: Option<String>,
    /// Settings used to prepare compaction.
    pub settings: CompactionSettings,
}

/// Prepare session entries for compaction, or return `Ok(None)` when compaction is
/// not applicable (`prepareCompaction`, compaction.ts:545-610).
pub fn prepare_compaction(
    path_entries: &[SessionTreeEntry],
    settings: &CompactionSettings,
) -> Result<Option<CompactionPreparation>, CompactionError> {
    if path_entries.is_empty()
        || matches!(
            path_entries.last(),
            Some(SessionTreeEntry::Compaction { .. })
        )
    {
        return Ok(None);
    }

    // Locate the most recent compaction entry (compaction.ts:553-559).
    let mut prev_compaction_index: Option<usize> = None;
    for i in (0..path_entries.len()).rev() {
        if matches!(path_entries[i], SessionTreeEntry::Compaction { .. }) {
            prev_compaction_index = Some(i);
            break;
        }
    }

    let mut previous_summary: Option<String> = None;
    let mut boundary_start = 0usize;
    if let Some(pci) = prev_compaction_index {
        if let SessionTreeEntry::Compaction {
            summary,
            first_kept_entry_id,
            ..
        } = &path_entries[pci]
        {
            previous_summary = Some(summary.clone());
            let first_kept_index = path_entries
                .iter()
                .position(|entry| entry_id(entry) == first_kept_entry_id);
            boundary_start = first_kept_index.unwrap_or(pci + 1);
        }
    }
    let boundary_end = path_entries.len();

    let tokens_before =
        estimate_context_tokens(&build_session_context(path_entries).messages).tokens;

    let cut_point = find_cut_point(
        path_entries,
        boundary_start,
        boundary_end,
        settings.keep_recent_tokens,
    );
    let first_kept_entry = &path_entries[cut_point.first_kept_entry_index];
    let first_kept_entry_id = entry_id(first_kept_entry).to_string();
    if first_kept_entry_id.is_empty() {
        return Err(CompactionError::new(
            CompactionErrorCode::InvalidSession,
            "First kept entry has no UUID - session may need migration",
        ));
    }

    let history_end = if cut_point.is_split_turn {
        cut_point.turn_start_index as usize
    } else {
        cut_point.first_kept_entry_index
    };
    let mut messages_to_summarize: Vec<AgentMessage> = Vec::new();
    for entry in &path_entries[boundary_start..history_end] {
        if let Some(msg) = get_message_from_entry_for_compaction(entry) {
            messages_to_summarize.push(msg);
        }
    }

    let mut turn_prefix_messages: Vec<AgentMessage> = Vec::new();
    if cut_point.is_split_turn {
        for entry in
            &path_entries[cut_point.turn_start_index as usize..cut_point.first_kept_entry_index]
        {
            if let Some(msg) = get_message_from_entry_for_compaction(entry) {
                turn_prefix_messages.push(msg);
            }
        }
    }

    // TODO(feat-003 harness/compaction utils): `fileOps` (extractFileOperations,
    // compaction.ts:593-598) is computed here in Pi from `messagesToSummarize`,
    // `turnPrefixMessages`, and the previous compaction's `details` — deferred until
    // `compaction/utils.ts` is ported.

    Ok(Some(CompactionPreparation {
        first_kept_entry_id,
        messages_to_summarize,
        turn_prefix_messages,
        is_split_turn: cut_point.is_split_turn,
        tokens_before,
        previous_summary,
        settings: settings.clone(),
    }))
}
