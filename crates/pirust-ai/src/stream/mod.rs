//! Async event stream — Rust port of `packages/ai/src/utils/event-stream.ts`.
//!
//! See `docs/analysis/06-anthropic-runtime-spec.md` §1. Pi's `EventStream<T, R>` is a
//! hand-rolled push/pull queue that is BOTH an async-iterable of events AND a promise of a
//! final value; it completes on the first `isComplete` event. This port maps that to
//! `tokio::sync::mpsc` (unbounded, for events) + `tokio::sync::oneshot` (for the final
//! result). The unbounded channel gives Pi's "drop after done / drain-before-done"
//! semantics for free: sending after the receiver is closed is a no-op, and buffered
//! events drain before the stream yields `None`.
//!
//! This module is a leaf (no HTTP/SSE deps) and is implemented in full here; the higher
//! layers (`sse`, `api::anthropic_messages`) drive it.

use std::pin::Pin;
use std::task::{Context as TaskContext, Poll};

use futures::Stream;
use tokio::sync::{mpsc, oneshot};

use crate::types::event::AssistantMessageEvent;
use crate::types::ids::{Api, ProviderId, StopReason};
use crate::types::message::{AssistantMessage, AssistantRole};
use crate::types::usage::{Cost, Usage};

/// Consumer side of a generic event stream (TS `EventStream<T, R>`).
///
/// Implements [`futures::Stream`] over the event channel, and exposes [`Self::result`] to
/// await the single final value. Events are delivered FIFO to exactly one consumer
/// (`mpsc` shift semantics — this is a queue, not a broadcast).
pub struct EventStream<T, R> {
    rx: mpsc::UnboundedReceiver<T>,
    result_rx: oneshot::Receiver<R>,
}

/// Producer side of a generic event stream. Holds the event sender plus a one-shot for the
/// final result and the injected `is_complete`/`extract_result` predicates (TS closures).
pub struct EventSink<T, R> {
    tx: mpsc::UnboundedSender<T>,
    result_tx: Option<oneshot::Sender<R>>,
    is_complete: Box<dyn Fn(&T) -> bool + Send>,
    extract_result: Box<dyn Fn(&T) -> R + Send>,
    done: bool,
}

/// The final result could not be produced because the producer dropped its sink without
/// ever pushing a completing event (TS: the promise would hang forever).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamClosed;

/// Construct a linked `(sink, stream)` pair (TS `new EventStream(isComplete, extractResult)`).
///
/// `is_complete` decides which event terminates the stream; `extract_result` derives the
/// final `R` from that terminal event. Both mirror the TS constructor arguments.
pub fn channel<T, R>(
    is_complete: impl Fn(&T) -> bool + Send + 'static,
    extract_result: impl Fn(&T) -> R + Send + 'static,
) -> (EventSink<T, R>, EventStream<T, R>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let (result_tx, result_rx) = oneshot::channel();
    let sink = EventSink {
        tx,
        result_tx: Some(result_tx),
        is_complete: Box::new(is_complete),
        extract_result: Box::new(extract_result),
        done: false,
    };
    let stream = EventStream { rx, result_rx };
    (sink, stream)
}

impl<T, R> EventSink<T, R> {
    /// Push one event (TS `push`, `event-stream.ts:21-36`). If already `done`, the event is
    /// dropped silently. If `is_complete(event)`, the final result is resolved from
    /// `extract_result(event)` — but the terminal event is STILL delivered to consumers.
    pub fn push(&mut self, event: T) {
        if self.done {
            return;
        }
        if (self.is_complete)(&event) {
            self.done = true;
            if let Some(tx) = self.result_tx.take() {
                let result = (self.extract_result)(&event);
                let _ = tx.send(result);
            }
        }
        // Send after resolving the result so a consumer awaiting `result()` cannot observe
        // the terminal event before the promise resolves. Send errors (receiver dropped)
        // are ignored, matching the unbounded/no-backpressure semantics.
        let _ = self.tx.send(event);
    }

    /// End the stream, optionally resolving the final result explicitly (TS `end(result?)`,
    /// `event-stream.ts:38-48`) — used by the faux provider to end with a canned message.
    /// Dropping the event sender closes the channel, so the consumer's stream yields `None`
    /// after draining any buffered events.
    pub fn end(&mut self, result: Option<R>) {
        self.done = true;
        if let (Some(result), Some(tx)) = (result, self.result_tx.take()) {
            let _ = tx.send(result);
        }
        // `tx` is dropped when `self` is dropped; callers typically drop the sink after
        // `end`. No explicit close is required for correctness.
    }
}

impl<T, R> EventStream<T, R> {
    /// Await the single final value (TS `result()`, `event-stream.ts:64-66`). Resolves once
    /// a completing event has been pushed. Returns [`StreamClosed`] if the producer dropped
    /// the sink without ever completing (TS hangs here; this port surfaces the closure).
    pub async fn result(self) -> Result<R, StreamClosed> {
        self.result_rx.await.map_err(|_| StreamClosed)
    }
}

impl<T, R> Stream for EventStream<T, R> {
    type Item = T;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Option<T>> {
        self.rx.poll_recv(cx)
    }
}

/// Producer side specialised to assistant-message events (TS `AssistantMessageEventStream`,
/// `event-stream.ts:69-83`): `is_complete` is `done`/`error`, and `extract_result` pulls the
/// final `AssistantMessage` out of either terminal variant.
pub struct AssistantMessageSink(EventSink<AssistantMessageEvent, AssistantMessage>);

/// Consumer side specialised to assistant-message events. [`Self::result`] resolves with the
/// final `AssistantMessage` on BOTH success (`done`) and stream-level error (`error`) — the
/// error is carried as a *value* (`stopReason: error|aborted`, `errorMessage`), never a
/// rejection (spec §1).
pub struct AssistantMessageEventStream(EventStream<AssistantMessageEvent, AssistantMessage>);

/// Construct a linked assistant-message `(sink, stream)` pair.
pub fn assistant_message_stream() -> (AssistantMessageSink, AssistantMessageEventStream) {
    let (sink, stream) = channel(
        |e: &AssistantMessageEvent| {
            matches!(
                e,
                AssistantMessageEvent::Done { .. } | AssistantMessageEvent::Error { .. }
            )
        },
        |e: &AssistantMessageEvent| match e {
            AssistantMessageEvent::Done { message, .. } => message.clone(),
            AssistantMessageEvent::Error { error, .. } => error.clone(),
            // Only invoked for terminal events (guaranteed by `is_complete` above).
            _ => unreachable!("extract_result called on non-terminal event"),
        },
    );
    (
        AssistantMessageSink(sink),
        AssistantMessageEventStream(stream),
    )
}

impl AssistantMessageSink {
    /// See [`EventSink::push`].
    pub fn push(&mut self, event: AssistantMessageEvent) {
        self.0.push(event);
    }

    /// See [`EventSink::end`].
    pub fn end(&mut self, result: Option<AssistantMessage>) {
        self.0.end(result);
    }
}

impl AssistantMessageEventStream {
    /// Await the final `AssistantMessage` (spec §1 Rust mapping). Unlike Pi — which hangs if
    /// neither `done` nor `error` is ever pushed — this port surfaces a synthesized `error`
    /// message (`stopReason: error`) when the producer drops without completing.
    pub async fn result(self) -> AssistantMessage {
        self.0.result().await.unwrap_or_else(|_| dropped_error())
    }
}

impl Stream for AssistantMessageEventStream {
    type Item = AssistantMessageEvent;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.0).poll_next(cx)
    }
}

/// Minimal `error` `AssistantMessage` synthesized when the producer drops its sink without
/// completing (see [`AssistantMessageEventStream::result`]).
fn dropped_error() -> AssistantMessage {
    AssistantMessage {
        role: AssistantRole::Assistant,
        content: Vec::new(),
        api: Api::from(""),
        provider: ProviderId::from(""),
        model: String::new(),
        response_model: None,
        diagnostics: None,
        usage: Usage {
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
        },
        stop_reason: StopReason::Error,
        timestamp: 0,
        response_id: None,
        error_message: Some("stream producer dropped without completing".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::content::{AssistantContent, TextContent};
    use futures::StreamExt;

    fn msg(text: &str) -> AssistantMessage {
        AssistantMessage {
            role: AssistantRole::Assistant,
            content: vec![AssistantContent::Text(TextContent::new(text))],
            api: Api::from("anthropic-messages"),
            provider: ProviderId::from("anthropic"),
            model: "claude".into(),
            response_model: None,
            diagnostics: None,
            usage: Usage {
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
            },
            stop_reason: StopReason::Stop,
            timestamp: 0,
            response_id: None,
            error_message: None,
        }
    }

    #[tokio::test]
    async fn events_deliver_in_order_and_result_resolves_on_done() {
        let (mut sink, mut stream) = assistant_message_stream();
        sink.push(AssistantMessageEvent::Start { partial: msg("") });
        sink.push(AssistantMessageEvent::TextDelta {
            content_index: 0,
            delta: "hi".into(),
            partial: msg("hi"),
        });
        sink.push(AssistantMessageEvent::Done {
            reason: StopReason::Stop,
            message: msg("hi"),
        });
        drop(sink);

        // Terminal `done` event is still yielded to the consumer.
        let mut kinds = Vec::new();
        while let Some(ev) = stream.next().await {
            kinds.push(match ev {
                AssistantMessageEvent::Start { .. } => "start",
                AssistantMessageEvent::TextDelta { .. } => "text_delta",
                AssistantMessageEvent::Done { .. } => "done",
                _ => "other",
            });
        }
        assert_eq!(kinds, ["start", "text_delta", "done"]);
    }

    #[tokio::test]
    async fn result_resolves_with_final_message() {
        let (mut sink, stream) = assistant_message_stream();
        sink.push(AssistantMessageEvent::Done {
            reason: StopReason::Stop,
            message: msg("final"),
        });
        drop(sink);
        let final_msg = stream.result().await;
        assert_eq!(
            final_msg.content,
            vec![AssistantContent::Text(TextContent::new("final"))]
        );
    }

    #[tokio::test]
    async fn events_after_done_are_dropped() {
        let (mut sink, mut stream) = assistant_message_stream();
        sink.push(AssistantMessageEvent::Done {
            reason: StopReason::Stop,
            message: msg("a"),
        });
        // Pushed after completion — must be silently dropped.
        sink.push(AssistantMessageEvent::TextDelta {
            content_index: 0,
            delta: "late".into(),
            partial: msg("late"),
        });
        drop(sink);
        let mut count = 0;
        while stream.next().await.is_some() {
            count += 1;
        }
        assert_eq!(
            count, 1,
            "only the terminal `done` event should be delivered"
        );
    }

    #[tokio::test]
    async fn dropping_sink_without_completing_surfaces_error_message() {
        let (sink, stream) = assistant_message_stream();
        drop(sink);
        let final_msg = stream.result().await;
        assert_eq!(final_msg.stop_reason, StopReason::Error);
        assert!(final_msg.error_message.is_some());
    }
}
