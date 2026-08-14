//! Inline SSE decoder — Rust port of the Server-Sent-Events decoding that lives INLINE in
//! `packages/ai/src/api/anthropic-messages.ts:292-482` (there is no separate `utils/*sse*`
//! file in Pi).
//!
//! See `docs/analysis/06-anthropic-runtime-spec.md` §3. The decoder is line-based, CR/LF/CRLF
//! aware, joins multi-line `data:` fields with `\n`, flushes on blank lines, and ignores
//! `[DONE]`/comments/unknown events. A hand-rolled UTF-8 buffer (matching Pi's
//! `TextDecoder{stream:true}`) is preferred over `eventsource-stream`, whose framing differs.
//!
//! Ported functions (all VERBATIM in behavior from `anthropic-messages.ts`):
//! - `flushSseEvent`      (`:313-327`) → [`flush_event`]
//! - `decodeSseLine`      (`:329-353`) → [`decode_line`]
//! - `nextLineBreakIndex` (`:355-365`) → [`next_line_break_index`] (private helper)
//! - `consumeLine`        (`:367-382`) → [`consume_line`] (private helper)
//! - `iterateSseMessages` (`:384-441`) → [`iterate_sse_messages`]
//! - `iterateAnthropicEvents` allow-set filter (`:454-461`) → [`iterate_anthropic_events`]

use std::collections::VecDeque;
use std::pin::Pin;

use bytes::Bytes;
use futures::{Stream, StreamExt};

/// The 6 canonical Anthropic message stream event types (TS set at `anthropic-messages.ts:304-311`).
/// Any SSE whose `event` is not one of these is skipped (`ping`, `[DONE]`, `proxy.stats`, …).
pub const ANTHROPIC_MESSAGE_EVENTS: [&str; 6] = [
    "message_start",
    "message_delta",
    "message_stop",
    "content_block_start",
    "content_block_delta",
    "content_block_stop",
];

/// A fully-decoded Server-Sent Event (TS `ServerSentEvent`, `:292-296`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerSentEvent {
    /// The `event:` field value, or `None` when only `data:` lines were present.
    pub event: Option<String>,
    /// The concatenated `data:` payload (multiple `data:` lines joined with `\n`).
    pub data: String,
    /// The raw source lines that produced this event (for diagnostics).
    pub raw: Vec<String>,
}

/// In-progress decoder accumulator (TS `SseDecoderState`, `:298-302`). One `data:` line per
/// `Vec` entry; joined with `\n` on flush.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SseDecoderState {
    pub event: Option<String>,
    pub data: Vec<String>,
    pub raw: Vec<String>,
}

impl SseDecoderState {
    /// A fresh, empty decoder state.
    pub fn new() -> Self {
        Self::default()
    }
}

/// Errors surfaced while iterating an SSE byte stream.
#[derive(Debug, thiserror::Error)]
pub enum SseError {
    /// The request was aborted via the caller's cancellation signal (TS `:395-397`).
    ///
    /// Abort/`signal` handling is NOT performed inside [`iterate_sse_messages`] — Pi's abort
    /// check lives in the same async generator, but in the port the cancellation signal is
    /// owned by the higher event-stream layer (see spec §1), so this variant is surfaced from
    /// there. It is retained here as the canonical error value for that layer to emit.
    #[error("Request was aborted")]
    Aborted,
    /// An `event: error` SSE was received; the payload is the raw `data` (TS `:455-457`).
    #[error("{0}")]
    ServerError(String),
    /// Transport-level failure while reading the byte stream.
    #[error("transport error: {0}")]
    Transport(String),
}

/// Flush the accumulator into an event, or `None` for a collapsing blank run (TS
/// `flushSseEvent`, `:313-327`). Resets `state`.
pub fn flush_event(state: &mut SseDecoderState) -> Option<ServerSentEvent> {
    // TS guard is `if (!state.event && state.data.length === 0)`. `!state.event` is JS-falsy:
    // true for BOTH `null` AND the empty string `""`. So an `event:` line with an empty value
    // does not, on its own, produce an event — we must treat `Some("")` as falsy here.
    let event_empty = match state.event.as_deref() {
        None => true,
        Some(value) => value.is_empty(),
    };
    if event_empty && state.data.is_empty() {
        return None;
    }

    // NB: the emitted `event` still carries the raw value (which may be `Some("")`), matching
    // TS `event: state.event`. Only the *guard* above collapses the empty string.
    let event = ServerSentEvent {
        event: state.event.take(),
        data: state.data.join("\n"),
        raw: std::mem::take(&mut state.raw),
    };
    state.data.clear();
    Some(event)
}

/// Decode one already-split line into the accumulator, emitting an event on a blank line
/// (TS `decodeSseLine`, `:329-353`). Handles the single-leading-space strip and the
/// comment (`:`-prefixed) and `event`/`data` field rules.
pub fn decode_line(line: &str, state: &mut SseDecoderState) -> Option<ServerSentEvent> {
    if line.is_empty() {
        return flush_event(state);
    }

    // NB: comment lines are still pushed to `raw` (the push happens before the `:`-prefix
    // check in TS `:334`).
    state.raw.push(line.to_string());
    if line.starts_with(':') {
        return None;
    }

    // Split on the FIRST `:`. No colon → the whole line is the field name, value is "".
    // (`:` / ` ` are ASCII, so byte indices from `find`/`strip_prefix` are char boundaries.)
    let (field_name, mut value) = match line.find(':') {
        Some(index) => (&line[..index], &line[index + 1..]),
        None => (line, ""),
    };
    // Strip exactly one leading space (TS `:342-344`).
    if let Some(stripped) = value.strip_prefix(' ') {
        value = stripped;
    }

    match field_name {
        "event" => state.event = Some(value.to_string()),
        "data" => state.data.push(value.to_string()),
        _ => {}
    }

    None
}

/// Index of the first `\r` or `\n` in `text`, or `None` (TS `nextLineBreakIndex`, `:355-365`).
fn next_line_break_index(text: &str) -> Option<usize> {
    let cr = text.find('\r');
    let lf = text.find('\n');
    match (cr, lf) {
        (None, None) => None,
        (Some(index), None) | (None, Some(index)) => Some(index),
        (Some(a), Some(b)) => Some(a.min(b)),
    }
}

/// Split off the first complete line, treating `\r\n` as a single break (TS `consumeLine`,
/// `:367-382`). Returns `(line, rest)`, or `None` when `text` has no line break yet.
fn consume_line(text: &str) -> Option<(String, String)> {
    let break_index = next_line_break_index(text)?;
    let bytes = text.as_bytes();
    let mut next_index = break_index + 1;
    if bytes[break_index] == b'\r' && bytes.get(next_index) == Some(&b'\n') {
        next_index += 1;
    }
    Some((
        text[..break_index].to_string(),
        text[next_index..].to_string(),
    ))
}

/// Incremental UTF-8 decoder mirroring `TextDecoder{stream:true}`: it buffers a trailing
/// incomplete multi-byte sequence across chunk boundaries and emits U+FFFD for genuinely
/// invalid bytes (Anthropic streams are well-formed UTF-8, but this keeps the framing exact).
#[derive(Default)]
struct Utf8StreamDecoder {
    pending: Vec<u8>,
}

impl Utf8StreamDecoder {
    /// Streaming decode of one byte chunk. Complete UTF-8 is returned; a partial trailing
    /// sequence is retained for the next call.
    fn decode(&mut self, chunk: &[u8]) -> String {
        self.pending.extend_from_slice(chunk);
        let mut out = String::new();
        loop {
            match std::str::from_utf8(&self.pending) {
                Ok(valid) => {
                    out.push_str(valid);
                    self.pending.clear();
                    return out;
                }
                Err(err) => {
                    let valid_up_to = err.valid_up_to();
                    if valid_up_to > 0 {
                        out.push_str(
                            std::str::from_utf8(&self.pending[..valid_up_to])
                                .expect("valid_up_to marks a valid UTF-8 boundary"),
                        );
                    }
                    match err.error_len() {
                        // Incomplete sequence at the end of the buffer: keep the tail so the
                        // next chunk can complete it (do not split a multi-byte char).
                        None => {
                            self.pending.drain(..valid_up_to);
                            return out;
                        }
                        // Genuinely invalid bytes: emit the replacement char and skip them,
                        // matching TextDecoder's non-fatal behavior.
                        Some(bad_len) => {
                            out.push('\u{FFFD}');
                            self.pending.drain(..valid_up_to + bad_len);
                        }
                    }
                }
            }
        }
    }

    /// Final flush (TS `decoder.decode()` with no args, `:416`): a leftover incomplete
    /// sequence becomes a single replacement char.
    fn flush(&mut self) -> String {
        if self.pending.is_empty() {
            String::new()
        } else {
            self.pending.clear();
            "\u{FFFD}".to_string()
        }
    }
}

/// The stateful line/UTF-8 buffer shared by [`iterate_sse_messages`]. Bundles the incremental
/// UTF-8 decoder, the partial-line `String` buffer, and the [`SseDecoderState`] accumulator so
/// the exact TS loop body (`:404-437`) can run over any chunking of the input.
struct SseLineBuffer {
    decoder: Utf8StreamDecoder,
    buffer: String,
    state: SseDecoderState,
}

impl SseLineBuffer {
    fn new() -> Self {
        Self {
            decoder: Utf8StreamDecoder::default(),
            buffer: String::new(),
            state: SseDecoderState::new(),
        }
    }

    /// Feed one byte chunk; append any completed events to `out` (TS `:404-413`).
    fn push_chunk(&mut self, chunk: &[u8], out: &mut Vec<ServerSentEvent>) {
        self.buffer.push_str(&self.decoder.decode(chunk));
        self.drain_complete_lines(out);
    }

    /// End of stream: final UTF-8 flush, drain remaining complete lines, decode a trailing
    /// partial line, then a trailing `flush_event` (TS `:416-437`).
    fn finish(&mut self, out: &mut Vec<ServerSentEvent>) {
        self.buffer.push_str(&self.decoder.flush());
        self.drain_complete_lines(out);

        if !self.buffer.is_empty() {
            let line = std::mem::take(&mut self.buffer);
            if let Some(event) = decode_line(&line, &mut self.state) {
                out.push(event);
            }
        }

        if let Some(event) = flush_event(&mut self.state) {
            out.push(event);
        }
    }

    fn drain_complete_lines(&mut self, out: &mut Vec<ServerSentEvent>) {
        while let Some((line, rest)) = consume_line(&self.buffer) {
            self.buffer = rest;
            if let Some(event) = decode_line(&line, &mut self.state) {
                out.push(event);
            }
        }
    }
}

/// Adapt a byte stream into a stream of decoded SSE events (TS `iterateSseMessages`,
/// `:384-441`), handling CRLF framing, incremental UTF-8 decoding, and the trailing flush.
///
/// The input models Pi's `ReadableStream<Uint8Array>`: a stream of byte chunks. A transport
/// error from the input is forwarded once and terminates the stream. (Abort/`signal` handling
/// is intentionally omitted here — see [`SseError::Aborted`].)
pub fn iterate_sse_messages<S>(
    body: S,
) -> Pin<Box<dyn Stream<Item = Result<ServerSentEvent, SseError>> + Send>>
where
    S: Stream<Item = Result<Bytes, SseError>> + Send + 'static,
{
    struct IterState<S> {
        body: Pin<Box<S>>,
        buf: SseLineBuffer,
        ready: VecDeque<ServerSentEvent>,
        input_done: bool,
        finished: bool,
    }

    let init = IterState {
        body: Box::pin(body),
        buf: SseLineBuffer::new(),
        ready: VecDeque::new(),
        input_done: false,
        finished: false,
    };

    Box::pin(futures::stream::unfold(init, |mut st| async move {
        loop {
            if let Some(event) = st.ready.pop_front() {
                return Some((Ok(event), st));
            }
            if st.finished {
                return None;
            }
            if st.input_done {
                // Stream drained: run the trailing flush (TS `:416-437`) once.
                let mut out = Vec::new();
                st.buf.finish(&mut out);
                st.ready.extend(out);
                st.finished = true;
                continue;
            }
            match st.body.next().await {
                Some(Ok(bytes)) => {
                    let mut out = Vec::new();
                    st.buf.push_chunk(&bytes, &mut out);
                    st.ready.extend(out);
                }
                Some(Err(err)) => {
                    st.finished = true;
                    return Some((Err(err), st));
                }
                None => {
                    st.input_done = true;
                }
            }
        }
    }))
}

/// True when `event` is one of the 6 canonical Anthropic message events. `None`/unknown → false
/// (TS `ANTHROPIC_MESSAGE_EVENTS.has(sse.event ?? "")`, `:459`).
fn is_anthropic_message_event(event: Option<&str>) -> bool {
    matches!(event, Some(name) if ANTHROPIC_MESSAGE_EVENTS.contains(&name))
}

/// Anthropic-layer filter over [`iterate_sse_messages`] (TS `iterateAnthropicEvents`,
/// `:443-482`), keeping ONLY the allow-set portion of that function:
/// - an `event: error` SSE becomes [`SseError::ServerError`] (TS `:455-457`);
/// - any event NOT in [`ANTHROPIC_MESSAGE_EVENTS`] is dropped — `ping`, `proxy.stats`,
///   `[DONE]`, unknown types — and there is NO reliance on `[DONE]` (TS `:459-461`);
/// - allowed events pass through unchanged.
///
/// Signature refinement vs. Pi: this yields the filtered [`ServerSentEvent`]s rather than
/// `RawMessageStreamEvent`. The JSON parse (`parseJsonWithRepair`), the
/// `sawMessageStart`/`sawMessageEnd` bookkeeping, and the "ended before message_stop" check
/// (TS `:463-481`) belong to the api-layer state machine (spec §4b), not the SSE decoder, and
/// are ported there. This function is the exact decode + allow-set boundary between the two.
pub fn iterate_anthropic_events<S>(
    body: S,
) -> Pin<Box<dyn Stream<Item = Result<ServerSentEvent, SseError>> + Send>>
where
    S: Stream<Item = Result<Bytes, SseError>> + Send + 'static,
{
    Box::pin(iterate_sse_messages(body).filter_map(|result| async move {
        match result {
            Err(err) => Some(Err(err)),
            Ok(sse) => {
                // `event: error` is checked BEFORE the allow-set (TS `:455`), so it surfaces
                // as an error rather than being silently dropped.
                if sse.event.as_deref() == Some("error") {
                    Some(Err(SseError::ServerError(sse.data)))
                } else if is_anthropic_message_event(sse.event.as_deref()) {
                    Some(Ok(sse))
                } else {
                    None
                }
            }
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT_BASIC_SSE: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/pi/anthropic/text-basic.sse"
    );

    /// The oracle event tape for `text-basic.sse` (event name + exact `data` payload).
    fn expected_text_basic() -> Vec<(&'static str, &'static str)> {
        vec![
            (
                "message_start",
                r#"{"type":"message_start","message":{"id":"msg_test","usage":{"input_tokens":12,"output_tokens":0,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}}"#,
            ),
            (
                "content_block_start",
                r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
            ),
            (
                "content_block_delta",
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}"#,
            ),
            (
                "content_block_stop",
                r#"{"type":"content_block_stop","index":0}"#,
            ),
            (
                "message_delta",
                r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"input_tokens":12,"output_tokens":5,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}"#,
            ),
            ("message_stop", r#"{"type":"message_stop"}"#),
        ]
    }

    fn assert_matches_text_basic(events: &[ServerSentEvent]) {
        let expected = expected_text_basic();
        assert_eq!(events.len(), expected.len(), "event count");
        for (event, (name, data)) in events.iter().zip(expected) {
            assert_eq!(event.event.as_deref(), Some(name));
            assert_eq!(event.data, data);
        }
    }

    /// Drive [`iterate_sse_messages`] over an explicit sequence of byte chunks.
    async fn decode_chunks(chunks: Vec<Vec<u8>>) -> Vec<ServerSentEvent> {
        let body = futures::stream::iter(
            chunks
                .into_iter()
                .map(|c| Ok::<_, SseError>(Bytes::from(c))),
        );
        iterate_sse_messages(body)
            .map(|r| r.expect("no transport/error events in fixture"))
            .collect()
            .await
    }

    // ---- decode_line / flush_event unit behavior (byte-for-byte with decodeSseLine) --------

    #[test]
    fn blank_line_flushes_and_strips_single_leading_space() {
        let mut state = SseDecoderState::new();
        assert!(decode_line("event: message_start", &mut state).is_none());
        // Exactly one leading space is stripped; further spaces are preserved.
        assert!(decode_line("data:  two-leading", &mut state).is_none());
        let event = decode_line("", &mut state).expect("blank line flushes");
        assert_eq!(event.event.as_deref(), Some("message_start"));
        assert_eq!(event.data, " two-leading");
        assert_eq!(
            event.raw,
            vec!["event: message_start", "data:  two-leading"]
        );
        // State reset after flush.
        assert_eq!(state, SseDecoderState::new());
    }

    #[test]
    fn multi_line_data_joins_with_newline() {
        let mut state = SseDecoderState::new();
        decode_line("data: line-1", &mut state);
        decode_line("data: line-2", &mut state);
        let event = flush_event(&mut state).expect("has data");
        assert_eq!(event.event, None);
        assert_eq!(event.data, "line-1\nline-2");
    }

    #[test]
    fn comment_line_is_ignored_but_recorded_in_raw() {
        let mut state = SseDecoderState::new();
        assert!(decode_line(": this is a comment", &mut state).is_none());
        // Comment recorded in raw but produced neither event nor data.
        assert_eq!(state.raw, vec![": this is a comment"]);
        assert_eq!(state.event, None);
        assert!(state.data.is_empty());
        // A comment alone does not flush into an event.
        assert!(flush_event(&mut state).is_none());
    }

    #[test]
    fn line_without_colon_is_a_field_with_empty_value() {
        let mut state = SseDecoderState::new();
        // No colon → field name is the whole line, value "" → "data" pushes an empty string.
        decode_line("data", &mut state);
        let event = flush_event(&mut state).expect("data present (empty string)");
        assert_eq!(event.data, "");
    }

    #[test]
    fn empty_event_value_alone_is_falsy_and_does_not_flush() {
        // Mirrors JS `!state.event` treating "" as falsy (TS `:314`).
        let mut state = SseDecoderState::new();
        decode_line("event:", &mut state); // sets event = Some("")
        assert_eq!(state.event.as_deref(), Some(""));
        assert!(flush_event(&mut state).is_none());
    }

    #[test]
    fn unknown_field_names_are_ignored() {
        let mut state = SseDecoderState::new();
        decode_line("id: 42", &mut state);
        decode_line("retry: 1000", &mut state);
        // Neither `event` nor `data` populated → no event.
        assert!(flush_event(&mut state).is_none());
    }

    // ---- iterate_sse_messages over real fixture bytes --------------------------------------

    #[tokio::test]
    async fn text_basic_fixture_single_chunk() {
        let bytes = std::fs::read(TEXT_BASIC_SSE).expect("read fixture");
        let events = decode_chunks(vec![bytes]).await;
        assert_matches_text_basic(&events);
    }

    #[tokio::test]
    async fn text_basic_fixture_split_at_awkward_boundaries() {
        let bytes = std::fs::read(TEXT_BASIC_SSE).expect("read fixture");

        // 1-byte chunks: every split lands mid-line / mid-field.
        let one_byte: Vec<Vec<u8>> = bytes.iter().map(|b| vec![*b]).collect();
        assert_matches_text_basic(&decode_chunks(one_byte).await);

        // 7-byte chunks: irregular splits across line breaks and field delimiters.
        let seven_byte: Vec<Vec<u8>> = bytes.chunks(7).map(<[u8]>::to_vec).collect();
        assert_matches_text_basic(&decode_chunks(seven_byte).await);
    }

    #[tokio::test]
    async fn utf8_multibyte_char_split_across_chunks() {
        // Mixed 2/3/4-byte code points in the `data:` payload. LF framing is used on purpose:
        // Pi's `consumeLine` cannot look across chunk boundaries, so a lone `\r` at a boundary
        // is a complete break and the orphaned `\n` becomes a spurious flush — a faithful but
        // separate CR/LF artifact. Here we isolate multi-byte-char reassembly.
        let payload = "café — 世界 🎉 last";
        let raw = format!("event: content_block_delta\ndata: {payload}\n\n");

        // Feed byte-by-byte so every multi-byte sequence is split mid-char.
        let one_byte: Vec<Vec<u8>> = raw.as_bytes().iter().map(|b| vec![*b]).collect();
        let events = decode_chunks(one_byte).await;

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event.as_deref(), Some("content_block_delta"));
        assert_eq!(events[0].data, payload);
    }

    // ---- iterate_anthropic_events allow-set filtering --------------------------------------

    #[tokio::test]
    async fn anthropic_allow_set_drops_unknown_events() {
        // Interleave canonical events with events that must be filtered out. Note there is no
        // `[DONE]` terminator and the stream still decodes fully.
        let body = "\
event: ping\n\
data: {}\n\
\n\
event: message_start\n\
data: {\"type\":\"message_start\"}\n\
\n\
event: proxy.stats\n\
data: {\"tokens\":1}\n\
\n\
event: content_block_delta\n\
data: {\"type\":\"content_block_delta\"}\n\
\n\
data: [DONE]\n\
\n\
event: message_stop\n\
data: {\"type\":\"message_stop\"}\n\
\n";

        let stream = futures::stream::iter(std::iter::once(Ok::<_, SseError>(Bytes::from(
            body.as_bytes().to_vec(),
        ))));
        let events: Vec<ServerSentEvent> = iterate_anthropic_events(stream)
            .map(|r| r.expect("no error events"))
            .collect()
            .await;

        let names: Vec<Option<&str>> = events.iter().map(|e| e.event.as_deref()).collect();
        assert_eq!(
            names,
            vec![
                Some("message_start"),
                Some("content_block_delta"),
                Some("message_stop"),
            ]
        );
    }

    #[tokio::test]
    async fn anthropic_error_event_surfaces_as_error() {
        let body = "event: error\ndata: overloaded_error\n\n";
        let stream = futures::stream::iter(std::iter::once(Ok::<_, SseError>(Bytes::from(
            body.as_bytes().to_vec(),
        ))));
        let results: Vec<Result<ServerSentEvent, SseError>> =
            iterate_anthropic_events(stream).collect().await;

        assert_eq!(results.len(), 1);
        match &results[0] {
            Err(SseError::ServerError(msg)) => assert_eq!(msg, "overloaded_error"),
            other => panic!("expected ServerError, got {other:?}"),
        }
    }
}
