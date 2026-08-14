//! Faux provider — Rust port of `packages/ai/src/providers/faux.ts` (offline test support).
//!
//! See `docs/analysis/06-anthropic-runtime-spec.md` §6. The faux core replays queued response
//! steps as a canonical event tape (`streamWithDeltas` `:308-401`): `start`, then per content
//! block the matching `*_start`/`*_delta`(token-chunked)/`*_end`, and finally `done` (or
//! `error` for error/aborted stop reasons), honoring cancellation. It implements the same
//! [`ProviderStreams`] contract so the event stream + types can be exercised without HTTP.
//!
//! Determinism: `splitStringByTokenSize` (`:253`) uses `Math.random` to size chunks between
//! `min` and `max` tokens. Set `min == max` (via [`Faux::with_token_size`]) and chunking is
//! fully deterministic, which is what the loop/tape tests rely on.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};

use crate::api::{ProviderStreams, SimpleStreamOptions, StreamOptions};
use crate::stream::{assistant_message_stream, AssistantMessageEventStream, AssistantMessageSink};
use crate::types::content::{AssistantContent, TextContent, ThinkingContent, ToolCall};
use crate::types::event::AssistantMessageEvent;
use crate::types::ids::{Api, CacheRetention, ProviderId, StopReason};
use crate::types::message::{
    AssistantMessage, AssistantRole, Context, Message, UserMessageContent,
};
use crate::types::model::{Modality, Model, ModelCost, ModelCostRates};
use crate::types::usage::{Cost, Usage};

const DEFAULT_API: &str = "faux";
const DEFAULT_PROVIDER: &str = "faux";
const DEFAULT_MODEL_ID: &str = "faux-1";
const DEFAULT_MODEL_NAME: &str = "Faux Model";
const DEFAULT_BASE_URL: &str = "http://localhost:0";
const DEFAULT_MIN_TOKEN_SIZE: usize = 3;
const DEFAULT_MAX_TOKEN_SIZE: usize = 5;

/// The all-zero usage literal (TS `DEFAULT_USAGE`, `faux.ts:28-35`).
fn default_usage() -> Usage {
    Usage {
        input: 0,
        output: 0,
        cache_read: 0,
        cache_write: 0,
        total_tokens: Some(0),
        cost: Cost {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
            total: 0.0,
        },
        cache_write1h: None,
        reasoning: None,
    }
}

/// Current Unix time in milliseconds (TS `Date.now()`).
fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// --- content-block builders (TS `fauxText`/`fauxThinking`/`fauxToolCall`, `:49-64`) ---

/// Build a text content block (TS `fauxText`, `:49-51`).
pub fn faux_text(text: impl Into<String>) -> TextContent {
    TextContent::new(text)
}

/// Build a thinking content block (TS `fauxThinking`, `:53-55`).
pub fn faux_thinking(thinking: impl Into<String>) -> ThinkingContent {
    ThinkingContent {
        kind: Default::default(),
        thinking: thinking.into(),
        thinking_signature: None,
        redacted: None,
    }
}

/// Build a tool-call content block (TS `fauxToolCall`, `:57-64`). Unlike Pi, the id is
/// required here (Pi defaults to a random id); tests want deterministic ids.
pub fn faux_tool_call(
    id: impl Into<String>,
    name: impl Into<String>,
    arguments: Map<String, Value>,
) -> ToolCall {
    ToolCall {
        kind: Default::default(),
        id: id.into(),
        name: name.into(),
        arguments,
        thought_signature: None,
        partial_json: None,
    }
}

/// Normalize faux assistant content (TS `normalizeFauxAssistantContent`, `:66-71`).
fn normalize_content(content: Vec<AssistantContent>) -> Vec<AssistantContent> {
    content
}

/// Options accepted by [`faux_assistant_message`] (TS `fauxAssistantMessage` options bag).
#[derive(Debug, Clone, Default)]
pub struct FauxMessageOptions {
    pub stop_reason: Option<StopReason>,
    pub error_message: Option<String>,
    pub response_id: Option<String>,
    pub timestamp: Option<i64>,
}

/// Build a faux [`AssistantMessage`] (TS `fauxAssistantMessage`, `:73-94`). Field construction
/// mirrors Pi's literal: `role, content, api, provider, model, usage, stopReason, errorMessage,
/// responseId, timestamp` — `stopReason` defaults to `stop` and `timestamp` to now.
pub fn faux_assistant_message(
    content: Vec<AssistantContent>,
    options: FauxMessageOptions,
) -> AssistantMessage {
    AssistantMessage {
        role: AssistantRole::Assistant,
        content: normalize_content(content),
        api: Api::from(DEFAULT_API),
        provider: ProviderId::from(DEFAULT_PROVIDER),
        model: DEFAULT_MODEL_ID.to_string(),
        response_model: None,
        diagnostics: None,
        usage: default_usage(),
        stop_reason: options.stop_reason.unwrap_or(StopReason::Stop),
        timestamp: options.timestamp.unwrap_or_else(now_millis),
        response_id: options.response_id,
        error_message: options.error_message,
    }
}

/// Convenience: a single-text faux assistant message (TS `fauxAssistantMessage("...")`).
pub fn faux_text_message(text: impl Into<String>) -> AssistantMessage {
    faux_assistant_message(
        vec![AssistantContent::Text(faux_text(text))],
        FauxMessageOptions::default(),
    )
}

/// Factory signature for a queued step (TS `FauxResponseFactory`, `:96-101`). Receives the
/// request context, the per-request options, the (already-incremented) call count, and the
/// resolved model, returning the message to replay.
pub type FauxResponseFactory =
    Rc<dyn Fn(&Context, Option<&StreamOptions>, usize, &Model) -> AssistantMessage>;

/// A single queued faux response step (TS `FauxResponseStep = AssistantMessage | Factory`,
/// `:103`): either a static message or a factory computed at stream time.
#[derive(Clone)]
pub enum FauxResponseStep {
    /// A canned message replayed verbatim (boxed: an `AssistantMessage` dwarfs the factory
    /// pointer, so boxing keeps the enum small).
    Message(Box<AssistantMessage>),
    /// A factory invoked with the live request state.
    Factory(FauxResponseFactory),
}

impl FauxResponseStep {
    /// Wrap a canned message as a step (boxing it for you).
    pub fn message(message: AssistantMessage) -> Self {
        FauxResponseStep::Message(Box::new(message))
    }
}

impl From<AssistantMessage> for FauxResponseStep {
    fn from(message: AssistantMessage) -> Self {
        FauxResponseStep::message(message)
    }
}

impl std::fmt::Debug for FauxResponseStep {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FauxResponseStep::Message(m) => f.debug_tuple("Message").field(m).finish(),
            FauxResponseStep::Factory(_) => f.write_str("Factory(<fn>)"),
        }
    }
}

/// Offline faux provider (TS `createFauxCore` / `fauxProvider`, `:403-538`). Holds a FIFO queue
/// of response steps consumed one per `stream` call, plus the call counter and prompt cache used
/// by [`Faux::with_usage_estimate`]. Interior mutability lets it satisfy [`ProviderStreams`]'s
/// `&self` contract while mutating queue/counter/cache per call.
#[derive(Debug)]
pub struct Faux {
    api: Api,
    provider: ProviderId,
    model: Model,
    min_token_size: usize,
    max_token_size: usize,
    responses: RefCell<VecDeque<FauxResponseStep>>,
    call_count: Cell<usize>,
    prompt_cache: RefCell<HashMap<String, String>>,
    rng_state: Cell<u64>,
}

impl Default for Faux {
    fn default() -> Self {
        Self::new()
    }
}

impl Faux {
    /// Construct an empty faux provider with Pi's default model and token sizes (`min=3`,
    /// `max=5`).
    pub fn new() -> Self {
        let api = Api::from(DEFAULT_API);
        let provider = ProviderId::from(DEFAULT_PROVIDER);
        let model = Model {
            id: DEFAULT_MODEL_ID.to_string(),
            name: DEFAULT_MODEL_NAME.to_string(),
            api: api.clone(),
            provider: provider.clone(),
            base_url: DEFAULT_BASE_URL.to_string(),
            reasoning: false,
            thinking_level_map: None,
            input: vec![Modality::Text, Modality::Image],
            cost: ModelCost {
                rates: ModelCostRates {
                    input: 0.0,
                    output: 0.0,
                    cache_read: 0.0,
                    cache_write: 0.0,
                },
                tiers: None,
            },
            context_window: 128_000,
            max_tokens: 16_384,
            headers: None,
            compat: None,
        };
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x9E37_79B9_7F4A_7C15)
            | 1;
        Self {
            api,
            provider,
            model,
            min_token_size: DEFAULT_MIN_TOKEN_SIZE,
            max_token_size: DEFAULT_MAX_TOKEN_SIZE,
            responses: RefCell::new(VecDeque::new()),
            call_count: Cell::new(0),
            prompt_cache: RefCell::new(HashMap::new()),
            rng_state: Cell::new(seed),
        }
    }

    /// Set the delta chunk sizing (TS `tokenSize`, `:406-410`). Clamped exactly like Pi:
    /// `min = max(1, min(min, max))` and `max = max(min, max)`. Using `min == max` makes delta
    /// chunking deterministic (no `Math.random`) for tests.
    #[must_use]
    pub fn with_token_size(mut self, min: usize, max: usize) -> Self {
        let min_clamped = min.min(max).max(1);
        let max_clamped = max.max(min_clamped);
        self.min_token_size = min_clamped;
        self.max_token_size = max_clamped;
        self
    }

    /// Replace the queued responses (TS `setResponses`, `:498-500`).
    pub fn set_responses(&self, responses: Vec<FauxResponseStep>) {
        *self.responses.borrow_mut() = responses.into_iter().collect();
    }

    /// Append responses to the queue (TS `appendResponses`, `:501-503`).
    pub fn append_responses(&self, responses: Vec<FauxResponseStep>) {
        self.responses.borrow_mut().extend(responses);
    }

    /// Number of responses still queued (TS `getPendingResponseCount`, `:504-506`).
    pub fn pending_response_count(&self) -> usize {
        self.responses.borrow().len()
    }

    /// The default (and only) faux model (TS `getModel()`, `:481-488`).
    pub fn get_model(&self) -> &Model {
        &self.model
    }

    /// Number of `stream`/`stream_simple` invocations so far (TS `state.callCount`).
    pub fn call_count(&self) -> usize {
        self.call_count.get()
    }

    /// One xorshift64 step, used only when `min < max` (mirrors Pi's `Math.random` chunk
    /// sizing). Deterministic paths (`min == max`) never touch it.
    fn next_rand(&self) -> u64 {
        let mut x = self.rng_state.get();
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.rng_state.set(x);
        x
    }

    /// Pick a token size in `[min, max]` (TS `splitStringByTokenSize` sizing, `:257`).
    fn pick_token_size(&self) -> usize {
        if self.min_token_size >= self.max_token_size {
            return self.min_token_size;
        }
        let span = (self.max_token_size - self.min_token_size + 1) as u64;
        self.min_token_size + (self.next_rand() % span) as usize
    }

    /// Split `text` into chunks of `tokenSize * 4` chars (TS `splitStringByTokenSize`,
    /// `:253-263`). Always yields at least one chunk (`[""]` for empty input). Operates on
    /// `char`s so multi-byte input never splits mid-codepoint.
    fn split_by_token_size(&self, text: &str) -> Vec<String> {
        let chars: Vec<char> = text.chars().collect();
        let mut chunks: Vec<String> = Vec::new();
        let mut index = 0usize;
        while index < chars.len() {
            let token_size = self.pick_token_size();
            let char_size = (token_size * 4).max(1);
            let end = (index + char_size).min(chars.len());
            chunks.push(chars[index..end].iter().collect());
            index = end;
        }
        if chunks.is_empty() {
            chunks.push(String::new());
        }
        chunks
    }

    /// Clone a resolved step into a message stamped with this provider's api/provider/model
    /// (TS `cloneMessage`, `:265-275`).
    fn clone_message(&self, mut message: AssistantMessage, model: &Model) -> AssistantMessage {
        message.api = self.api.clone();
        message.provider = self.provider.clone();
        message.model = model.id.clone();
        message
    }

    /// Estimate `usage` for `message` given the prompt `context` (TS `withUsageEstimate`,
    /// `:213-251`): `ceil(chars/4)` token estimate for prompt + output, with a prefix-based
    /// cache split when a `session_id` is present and cache retention is not `"none"`.
    pub fn with_usage_estimate(
        &self,
        mut message: AssistantMessage,
        context: &Context,
        options: Option<&StreamOptions>,
    ) -> AssistantMessage {
        let prompt_text = serialize_context(context);
        let prompt_tokens = estimate_tokens(&prompt_text);
        let output_tokens = estimate_tokens(&assistant_content_to_text(&message.content));

        let mut input = prompt_tokens;
        let mut cache_read = 0u64;
        let mut cache_write = 0u64;

        let session_id = options.and_then(|o| o.session_id.as_deref());
        let retention_is_none = matches!(
            options.and_then(|o| o.cache_retention),
            Some(CacheRetention::None)
        );

        if let Some(session_id) = session_id {
            if !retention_is_none {
                let mut cache = self.prompt_cache.borrow_mut();
                if let Some(previous_prompt) = cache.get(session_id).cloned() {
                    let cached_chars = common_prefix_len(&previous_prompt, &prompt_text);
                    cache_read = estimate_tokens(&char_slice(&previous_prompt, 0, cached_chars));
                    cache_write = estimate_tokens(&char_slice(
                        &prompt_text,
                        cached_chars,
                        char_len(&prompt_text),
                    ));
                    input = prompt_tokens.saturating_sub(cache_read);
                } else {
                    cache_write = prompt_tokens;
                }
                cache.insert(session_id.to_string(), prompt_text);
            }
        }

        message.usage = Usage {
            input,
            output: output_tokens,
            cache_read,
            cache_write,
            total_tokens: Some(input + output_tokens + cache_read + cache_write),
            cost: Cost {
                input: 0.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
                total: 0.0,
            },
            cache_write1h: None,
            reasoning: None,
        };
        message
    }

    /// Build the "no more responses" error message (TS `createErrorMessage`, `:277-289`).
    fn error_message(&self, model: &Model, error: &str) -> AssistantMessage {
        AssistantMessage {
            role: AssistantRole::Assistant,
            content: Vec::new(),
            api: self.api.clone(),
            provider: self.provider.clone(),
            model: model.id.clone(),
            response_model: None,
            diagnostics: None,
            usage: default_usage(),
            stop_reason: StopReason::Error,
            timestamp: now_millis(),
            response_id: None,
            error_message: Some(error.to_string()),
        }
    }
}

/// `ceil(len / 4)` over `char`s (TS `estimateTokens`, `:140-142`; JS `.length` counts UTF-16
/// units — identical to `char`s for the BMP text faux ever sees).
fn estimate_tokens(text: &str) -> u64 {
    let len = text.chars().count() as u64;
    len.div_ceil(4)
}

fn char_len(text: &str) -> usize {
    text.chars().count()
}

/// Collect the char-range `[start, end)` back into a `String` (JS `String.prototype.slice`).
fn char_slice(text: &str, start: usize, end: usize) -> String {
    text.chars()
        .skip(start)
        .take(end.saturating_sub(start))
        .collect()
}

/// Length of the shared leading char run (TS `commonPrefixLength`, `:204-211`).
fn common_prefix_len(a: &str, b: &str) -> usize {
    a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count()
}

/// Flatten user/tool text+image content to a string (TS `contentToText`, `:148-160`).
fn user_blocks_to_text(content: &UserMessageContent) -> String {
    match content {
        UserMessageContent::Text(text) => text.clone(),
        UserMessageContent::Blocks(blocks) => blocks
            .iter()
            .map(|block| match block {
                crate::types::content::UserContent::Text(t) => t.text.clone(),
                crate::types::content::UserContent::Image(img) => {
                    format!("[image:{}:{}]", img.mime_type, img.data.chars().count())
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

/// Flatten assistant content to a string (TS `assistantContentToText`, `:162-174`).
fn assistant_content_to_text(content: &[AssistantContent]) -> String {
    content
        .iter()
        .map(|block| match block {
            AssistantContent::Text(t) => t.text.clone(),
            AssistantContent::Thinking(t) => t.thinking.clone(),
            AssistantContent::ToolCall(tc) => {
                let args = serde_json::to_string(&tc.arguments).unwrap_or_default();
                format!("{}:{}", tc.name, args)
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Serialize a whole message to text (TS `messageToText`, `:180-188`).
fn message_to_text(message: &Message) -> String {
    match message {
        Message::User(m) => user_blocks_to_text(&m.content),
        Message::Assistant(m) => assistant_content_to_text(&m.content),
        Message::ToolResult(m) => {
            let mut parts = vec![m.tool_name.clone()];
            for block in &m.content {
                parts.push(match block {
                    crate::types::content::UserContent::Text(t) => t.text.clone(),
                    crate::types::content::UserContent::Image(img) => {
                        format!("[image:{}:{}]", img.mime_type, img.data.chars().count())
                    }
                });
            }
            parts.join("\n")
        }
    }
}

/// Role tag as emitted into the prompt fingerprint (TS `${message.role}`).
fn message_role(message: &Message) -> &'static str {
    match message {
        Message::User(_) => "user",
        Message::Assistant(_) => "assistant",
        Message::ToolResult(_) => "toolResult",
    }
}

/// Fingerprint a request context to a string (TS `serializeContext`, `:190-202`).
fn serialize_context(context: &Context) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(system) = &context.system_prompt {
        parts.push(format!("system:{system}"));
    }
    for message in &context.messages {
        parts.push(format!(
            "{}:{}",
            message_role(message),
            message_to_text(message)
        ));
    }
    if let Some(tools) = &context.tools {
        if !tools.is_empty() {
            parts.push(format!(
                "tools:{}",
                serde_json::to_string(tools).unwrap_or_default()
            ));
        }
    }
    parts.join("\n\n")
}

/// Replay a resolved message as a canonical event tape (TS `streamWithDeltas`, `:308-401`).
///
/// `is_aborted` is polled at each of Pi's abort checkpoints; the [`ProviderStreams`] path passes
/// a never-aborting probe because [`StreamOptions`] does not yet carry a cancellation signal
/// (deferred to the adapter subagent), but the machinery is ported faithfully.
fn stream_with_deltas(
    faux: &Faux,
    sink: &mut AssistantMessageSink,
    message: AssistantMessage,
    is_aborted: &dyn Fn() -> bool,
) {
    let mut partial = message.clone();
    partial.content = Vec::new();

    if is_aborted() {
        let aborted = aborted_message(&partial);
        sink.push(AssistantMessageEvent::Error {
            reason: StopReason::Aborted,
            error: aborted.clone(),
        });
        sink.end(Some(aborted));
        return;
    }

    sink.push(AssistantMessageEvent::Start {
        partial: partial.clone(),
    });

    for (index, block) in message.content.iter().enumerate() {
        if is_aborted() {
            let aborted = aborted_message(&partial);
            sink.push(AssistantMessageEvent::Error {
                reason: StopReason::Aborted,
                error: aborted.clone(),
            });
            sink.end(Some(aborted));
            return;
        }

        let content_index = index as u32;
        match block {
            AssistantContent::Thinking(thinking) => {
                partial
                    .content
                    .push(AssistantContent::Thinking(faux_thinking("")));
                sink.push(AssistantMessageEvent::ThinkingStart {
                    content_index,
                    partial: partial.clone(),
                });
                for chunk in faux.split_by_token_size(&thinking.thinking) {
                    if is_aborted() {
                        let aborted = aborted_message(&partial);
                        sink.push(AssistantMessageEvent::Error {
                            reason: StopReason::Aborted,
                            error: aborted.clone(),
                        });
                        sink.end(Some(aborted));
                        return;
                    }
                    if let Some(AssistantContent::Thinking(t)) = partial.content.get_mut(index) {
                        t.thinking.push_str(&chunk);
                    }
                    sink.push(AssistantMessageEvent::ThinkingDelta {
                        content_index,
                        delta: chunk,
                        partial: partial.clone(),
                    });
                }
                sink.push(AssistantMessageEvent::ThinkingEnd {
                    content_index,
                    content: thinking.thinking.clone(),
                    partial: partial.clone(),
                });
            }
            AssistantContent::Text(text) => {
                partial.content.push(AssistantContent::Text(faux_text("")));
                sink.push(AssistantMessageEvent::TextStart {
                    content_index,
                    partial: partial.clone(),
                });
                for chunk in faux.split_by_token_size(&text.text) {
                    if is_aborted() {
                        let aborted = aborted_message(&partial);
                        sink.push(AssistantMessageEvent::Error {
                            reason: StopReason::Aborted,
                            error: aborted.clone(),
                        });
                        sink.end(Some(aborted));
                        return;
                    }
                    if let Some(AssistantContent::Text(t)) = partial.content.get_mut(index) {
                        t.text.push_str(&chunk);
                    }
                    sink.push(AssistantMessageEvent::TextDelta {
                        content_index,
                        delta: chunk,
                        partial: partial.clone(),
                    });
                }
                sink.push(AssistantMessageEvent::TextEnd {
                    content_index,
                    content: text.text.clone(),
                    partial: partial.clone(),
                });
            }
            AssistantContent::ToolCall(tool_call) => {
                partial.content.push(AssistantContent::ToolCall(ToolCall {
                    kind: Default::default(),
                    id: tool_call.id.clone(),
                    name: tool_call.name.clone(),
                    arguments: Map::new(),
                    thought_signature: None,
                    partial_json: None,
                }));
                sink.push(AssistantMessageEvent::ToolcallStart {
                    content_index,
                    partial: partial.clone(),
                });
                let args_json = serde_json::to_string(&tool_call.arguments).unwrap_or_default();
                for chunk in faux.split_by_token_size(&args_json) {
                    if is_aborted() {
                        let aborted = aborted_message(&partial);
                        sink.push(AssistantMessageEvent::Error {
                            reason: StopReason::Aborted,
                            error: aborted.clone(),
                        });
                        sink.end(Some(aborted));
                        return;
                    }
                    // Pi does NOT mutate the partial tool-call during deltas; arguments are
                    // filled in only at `toolcall_end`.
                    sink.push(AssistantMessageEvent::ToolcallDelta {
                        content_index,
                        delta: chunk,
                        partial: partial.clone(),
                    });
                }
                if let Some(AssistantContent::ToolCall(tc)) = partial.content.get_mut(index) {
                    tc.arguments = tool_call.arguments.clone();
                }
                sink.push(AssistantMessageEvent::ToolcallEnd {
                    content_index,
                    tool_call: tool_call.clone(),
                    partial: partial.clone(),
                });
            }
        }
    }

    if matches!(message.stop_reason, StopReason::Error | StopReason::Aborted) {
        sink.push(AssistantMessageEvent::Error {
            reason: message.stop_reason,
            error: message.clone(),
        });
        sink.end(Some(message));
        return;
    }

    sink.push(AssistantMessageEvent::Done {
        reason: message.stop_reason,
        message: message.clone(),
    });
    sink.end(Some(message));
}

/// Build the aborted partial (TS `createAbortedMessage`, `:291-298`).
fn aborted_message(partial: &AssistantMessage) -> AssistantMessage {
    let mut aborted = partial.clone();
    aborted.stop_reason = StopReason::Aborted;
    aborted.error_message = Some("Request was aborted".to_string());
    aborted.timestamp = now_millis();
    aborted
}

impl Faux {
    /// Shared body of `stream`/`stream_simple` (TS `stream`, `:442-476`). Shifts one queued
    /// step, bumps the call counter, resolves the step (invoking a factory with the live
    /// state), applies [`Faux::with_usage_estimate`], then replays the tape. An empty queue
    /// produces a single `error` message.
    fn run_stream(
        &self,
        model: &Model,
        ctx: &Context,
        opts: Option<StreamOptions>,
    ) -> AssistantMessageEventStream {
        let (mut sink, stream) = assistant_message_stream();

        let step = self.responses.borrow_mut().pop_front();
        self.call_count.set(self.call_count.get() + 1);

        let Some(step) = step else {
            let mut message = self.error_message(model, "No more faux responses queued");
            message = self.with_usage_estimate(message, ctx, opts.as_ref());
            sink.push(AssistantMessageEvent::Error {
                reason: StopReason::Error,
                error: message.clone(),
            });
            sink.end(Some(message));
            return stream;
        };

        let resolved = match step {
            FauxResponseStep::Message(message) => *message,
            FauxResponseStep::Factory(factory) => {
                factory(ctx, opts.as_ref(), self.call_count.get(), model)
            }
        };

        let mut message = self.clone_message(resolved, model);
        message = self.with_usage_estimate(message, ctx, opts.as_ref());
        stream_with_deltas(self, &mut sink, message, &|| false);
        stream
    }
}

impl ProviderStreams for Faux {
    fn stream(
        &self,
        model: &Model,
        ctx: &Context,
        opts: Option<StreamOptions>,
    ) -> AssistantMessageEventStream {
        self.run_stream(model, ctx, opts)
    }

    fn stream_simple(
        &self,
        model: &Model,
        ctx: &Context,
        opts: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream {
        // TS `streamSimple` delegates straight to `stream` (`:478-479`).
        self.run_stream(model, ctx, opts.map(|o| o.base))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::message::{Context, UserMessage, UserMessageContent, UserRole};
    use futures::StreamExt;

    fn event_kind(ev: &AssistantMessageEvent) -> &'static str {
        match ev {
            AssistantMessageEvent::Start { .. } => "start",
            AssistantMessageEvent::TextStart { .. } => "text_start",
            AssistantMessageEvent::TextDelta { .. } => "text_delta",
            AssistantMessageEvent::TextEnd { .. } => "text_end",
            AssistantMessageEvent::ThinkingStart { .. } => "thinking_start",
            AssistantMessageEvent::ThinkingDelta { .. } => "thinking_delta",
            AssistantMessageEvent::ThinkingEnd { .. } => "thinking_end",
            AssistantMessageEvent::ToolcallStart { .. } => "toolcall_start",
            AssistantMessageEvent::ToolcallDelta { .. } => "toolcall_delta",
            AssistantMessageEvent::ToolcallEnd { .. } => "toolcall_end",
            AssistantMessageEvent::Done { .. } => "done",
            AssistantMessageEvent::Error { .. } => "error",
        }
    }

    fn collapse(kinds: &[&'static str]) -> Vec<&'static str> {
        let mut out: Vec<&'static str> = Vec::new();
        for &k in kinds {
            if out.last() != Some(&k) {
                out.push(k);
            }
        }
        out
    }

    fn user_context(text: &str) -> Context {
        Context {
            system_prompt: None,
            messages: vec![Message::User(UserMessage {
                role: UserRole::User,
                content: UserMessageContent::Text(text.to_string()),
                timestamp: 0,
            })],
            tools: None,
        }
    }

    fn args(pairs: &[(&str, &str)]) -> Map<String, Value> {
        let mut m = Map::new();
        for (k, v) in pairs {
            m.insert((*k).to_string(), Value::String((*v).to_string()));
        }
        m
    }

    #[tokio::test]
    async fn scripted_thinking_toolcall_text_produces_canonical_tape() {
        let faux = Faux::new().with_token_size(1, 1); // deterministic: 4-char chunks
        let tool_args = args(&[("k", "v")]);
        let resolved = faux_assistant_message(
            vec![
                AssistantContent::Thinking(faux_thinking("thinking hard about it")),
                AssistantContent::ToolCall(faux_tool_call("call_1", "read", tool_args.clone())),
                AssistantContent::Text(faux_text("here is the answer")),
            ],
            FauxMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                ..Default::default()
            },
        );
        faux.set_responses(vec![FauxResponseStep::message(resolved.clone())]);

        let stream = faux.stream(faux.get_model(), &user_context("hello"), None);
        let events: Vec<AssistantMessageEvent> = stream.collect().await;
        let kinds: Vec<&'static str> = events.iter().map(event_kind).collect();

        assert_eq!(
            collapse(&kinds),
            vec![
                "start",
                "thinking_start",
                "thinking_delta",
                "thinking_end",
                "toolcall_start",
                "toolcall_delta",
                "toolcall_end",
                "text_start",
                "text_delta",
                "text_end",
                "done",
            ]
        );

        // Multiple deltas per block prove chunking actually ran deterministically.
        assert!(kinds.iter().filter(|k| **k == "thinking_delta").count() > 1);

        let Some(AssistantMessageEvent::Done { reason, message }) = events.last() else {
            panic!("tape must end in `done`");
        };
        assert_eq!(*reason, StopReason::ToolUse);
        assert_eq!(message.stop_reason, StopReason::ToolUse);
        assert_eq!(message.content, resolved.content);
        // The terminal tool-call carries its fully-resolved arguments.
        assert_eq!(faux.call_count(), 1);
        assert_eq!(faux.pending_response_count(), 0);
    }

    #[tokio::test]
    async fn empty_queue_emits_error_message() {
        let faux = Faux::new();
        let stream = faux.stream(faux.get_model(), &user_context("hello"), None);
        let events: Vec<AssistantMessageEvent> = stream.collect().await;

        assert_eq!(events.len(), 1, "empty queue is a single-event tape");
        let AssistantMessageEvent::Error { reason, error } = &events[0] else {
            panic!("empty queue must yield an `error` event");
        };
        assert_eq!(*reason, StopReason::Error);
        assert_eq!(error.stop_reason, StopReason::Error);
        assert_eq!(
            error.error_message.as_deref(),
            Some("No more faux responses queued")
        );
        assert_eq!(faux.call_count(), 1);
    }

    #[test]
    fn usage_estimate_char_over_four() {
        let faux = Faux::new();
        // serializeContext -> "user:hello world" (16 chars) -> ceil(16/4) = 4 tokens.
        let ctx = user_context("hello world");
        let msg = faux_text_message("hi there"); // 8 chars -> ceil(8/4) = 2 tokens.
        let out = faux.with_usage_estimate(msg, &ctx, None);
        assert_eq!(out.usage.input, 4);
        assert_eq!(out.usage.output, 2);
        assert_eq!(out.usage.cache_read, 0);
        assert_eq!(out.usage.cache_write, 0);
        assert_eq!(out.usage.total_tokens, Some(6));
    }

    #[test]
    fn usage_estimate_prefix_cache_split() {
        let faux = Faux::new();
        let opts = StreamOptions {
            session_id: Some("s1".to_string()),
            ..Default::default()
        };

        // Call 1: no previous prompt -> whole prompt is a cache write.
        // "user:hello" = 10 chars -> ceil(10/4) = 3 tokens.
        let ctx1 = user_context("hello");
        let out1 = faux.with_usage_estimate(faux_text_message(""), &ctx1, Some(&opts));
        assert_eq!(out1.usage.input, 3);
        assert_eq!(out1.usage.cache_write, 3);
        assert_eq!(out1.usage.cache_read, 0);

        // Call 2: shares the "user:hello" prefix with call 1.
        // prompt = "user:hello\n\nuser:world!!" = 24 chars -> 6 tokens.
        //   common prefix "user:hello" (10 chars) -> cacheRead ceil(10/4) = 3
        //   suffix "\n\nuser:world!!" (14 chars) -> cacheWrite ceil(14/4) = 4
        //   input = 6 - 3 = 3
        let ctx2 = Context {
            system_prompt: None,
            messages: vec![
                Message::User(UserMessage {
                    role: UserRole::User,
                    content: UserMessageContent::Text("hello".to_string()),
                    timestamp: 0,
                }),
                Message::User(UserMessage {
                    role: UserRole::User,
                    content: UserMessageContent::Text("world!!".to_string()),
                    timestamp: 0,
                }),
            ],
            tools: None,
        };
        let out2 = faux.with_usage_estimate(faux_text_message(""), &ctx2, Some(&opts));
        assert_eq!(out2.usage.cache_read, 3);
        assert_eq!(out2.usage.cache_write, 4);
        assert_eq!(out2.usage.input, 3);
    }

    #[tokio::test]
    async fn factory_step_sees_incremented_call_count() {
        let faux = Faux::new();
        let factory: FauxResponseFactory = Rc::new(|_ctx, _opts, call_count, model| {
            faux_assistant_message(
                vec![AssistantContent::Text(faux_text(format!(
                    "{}:{}",
                    model.id, call_count
                )))],
                FauxMessageOptions::default(),
            )
        });
        faux.set_responses(vec![FauxResponseStep::Factory(factory)]);
        let stream = faux.stream(faux.get_model(), &user_context("hi"), None);
        let events: Vec<AssistantMessageEvent> = stream.collect().await;
        let AssistantMessageEvent::Done { message, .. } = events.last().unwrap() else {
            panic!("expected done");
        };
        assert_eq!(
            assistant_content_to_text(&message.content),
            "faux-1:1",
            "factory sees model id and the incremented call count"
        );
    }

    #[test]
    fn abort_before_start_emits_aborted_error() {
        let faux = Faux::new().with_token_size(1, 1);
        let (mut sink, stream) = assistant_message_stream();
        let msg = faux_text_message("never streamed");
        stream_with_deltas(&faux, &mut sink, msg, &|| true);
        drop(sink);
        let final_msg = futures::executor::block_on(stream.result());
        assert_eq!(final_msg.stop_reason, StopReason::Aborted);
        assert_eq!(
            final_msg.error_message.as_deref(),
            Some("Request was aborted")
        );
    }
}
