//! Port of `packages/protocol/src/codec.ts` — composes CBOR + framing +
//! schema validation, and (unlike TS) gets most of that validation "for
//! free" from Rust's own type system. See module docs on
//! [`super::schemas`] for the Wave 2 scope note.
//!
//! **Encode-side simplification (documented, not silent):** TS's
//! `encodeClientMessage`/`encodeServerMessage` call `parseClientMessage`/
//! `parseServerMessage` on their input FIRST, because a JS caller can pass
//! any untyped value. A Rust caller can only ever construct an
//! already-valid [`super::schemas::ClientMessage`] /
//! [`super::schemas::ServerMessage`] value in the first place, so the
//! pre-encode parse/validate step is redundant here and is skipped — TS's
//! own "validates messages before encoding" test (feeding a structurally
//! invalid raw value straight to `encodeClientMessage`) is type-system-moot
//! for this reason, the same class of divergence as several Wave 1 CBOR
//! encode-side rejections.

use super::cbor::{decode_cbor_with, encode_cbor_with, CborOptions};
use super::framing::{assert_complete_frame, encode_frame, FrameDecoder, DEFAULT_MAX_FRAME_LENGTH};
use super::schemas::{
    ClientMessage, ProtocolJson, ServerMessage, ValidationError, PROTOCOL_VERSION,
};
use thiserror::Error;

#[derive(Debug, Clone, Error, PartialEq, Eq)]
#[error("{0}")]
pub struct ProtocolValidationError(pub String);

impl ProtocolValidationError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl From<ValidationError> for ProtocolValidationError {
    fn from(error: ValidationError) -> Self {
        Self(error.0)
    }
}

/// TS's `boundedErrorMessage`: truncate to 500 chars (ASCII-safe byte
/// truncation here — every message this port ever constructs is hand-written
/// English, so byte-vs-UTF-16-codepoint slicing never diverges in practice;
/// documented, not silently assumed correct for arbitrary input).
fn bounded_error_message(message: &str) -> String {
    if message.len() <= 500 {
        message.to_string()
    } else {
        format!("{}...", &message[..497.min(message.len())])
    }
}

fn resolve_max_frame_length(max_frame_length: Option<u64>) -> u64 {
    max_frame_length.unwrap_or(DEFAULT_MAX_FRAME_LENGTH)
}

fn encode_protocol_message(
    json: &ProtocolJson,
    max_frame_length: Option<u64>,
    kind: &str,
) -> Result<Vec<u8>, ProtocolValidationError> {
    let max_frame_length = resolve_max_frame_length(max_frame_length);
    let cbor_options = CborOptions {
        max_byte_length: max_frame_length,
        ..CborOptions::default()
    };
    let wrap = |message: String| {
        ProtocolValidationError::new(format!(
            "Unable to encode {kind} protocol message: {}",
            bounded_error_message(&message)
        ))
    };

    let payload = encode_cbor_with(&json.to_cbor(), &cbor_options).map_err(|e| wrap(e.0))?;
    let frame = encode_frame(&payload).map_err(|e| wrap(e.0))?;
    assert_complete_frame(&frame, Some(max_frame_length)).map_err(|e| wrap(e.0))?;
    Ok(frame)
}

pub fn encode_client_message(
    message: &ClientMessage,
    max_frame_length: Option<u64>,
) -> Result<Vec<u8>, ProtocolValidationError> {
    encode_protocol_message(&message.to_json(), max_frame_length, "client")
}

pub fn encode_server_message(
    message: &ServerMessage,
    max_frame_length: Option<u64>,
) -> Result<Vec<u8>, ProtocolValidationError> {
    encode_protocol_message(&message.to_json(), max_frame_length, "server")
}

/// `Number.isInteger(version) && version === PROTOCOL_VERSION` — takes a raw
/// `f64` (mirrors the TS signature, which accepts any `number`), not a
/// pre-validated [`super::schemas::ClientHello::version`].
pub fn is_supported_protocol_version(version: f64) -> bool {
    version.is_finite() && version == version.trunc() && version == PROTOCOL_VERSION as f64
}

// ============================================================================
// Validated, permanently-latching decoders
// ============================================================================

fn decode_one(frame: &[u8], max_byte_length: u64) -> Result<ProtocolJson, ValidationError> {
    let cbor_options = CborOptions {
        max_byte_length,
        ..CborOptions::default()
    };
    let value = decode_cbor_with(frame, &cbor_options).map_err(|e| ValidationError(e.0))?;
    ProtocolJson::from_cbor(&value)
}

/// Port of `ValidatedMessageDecoder` + `ClientMessageDecoder`
/// (`codec.ts:88-143`). Once any `push`/`end` call fails, every subsequent
/// call also fails (`{kind} message decoder has failed`) — no partial
/// recovery, matching TS's `failed` latch exactly (distinct from
/// [`FrameDecoder`]'s own, separately-latching failed state from Wave 1).
pub struct ClientMessageDecoder {
    frames: FrameDecoder,
    max_byte_length: u64,
    failed: bool,
}

impl ClientMessageDecoder {
    pub fn new(max_frame_length: Option<u64>) -> Self {
        let max_frame_length = resolve_max_frame_length(max_frame_length);
        Self {
            frames: FrameDecoder::new(Some(max_frame_length)),
            max_byte_length: max_frame_length,
            failed: false,
        }
    }

    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<ClientMessage>, ProtocolValidationError> {
        if self.failed {
            return Err(ProtocolValidationError::new(
                "client message decoder has failed",
            ));
        }
        match self.push_inner(chunk) {
            Ok(messages) => Ok(messages),
            Err(error) => {
                self.failed = true;
                Err(error)
            }
        }
    }

    fn push_inner(&mut self, chunk: &[u8]) -> Result<Vec<ClientMessage>, ProtocolValidationError> {
        let raw_frames = self.frames.push(chunk).map_err(|e| {
            ProtocolValidationError::new(format!(
                "Invalid client protocol frame: {}",
                bounded_error_message(&e.0)
            ))
        })?;
        let mut messages = Vec::with_capacity(raw_frames.len());
        for frame in raw_frames {
            let json = decode_one(&frame, self.max_byte_length).map_err(|e| {
                ProtocolValidationError::new(format!(
                    "Invalid client protocol frame: {}",
                    bounded_error_message(&e.0)
                ))
            })?;
            messages.push(ClientMessage::parse(&json)?);
        }
        Ok(messages)
    }

    pub fn end(&mut self) -> Result<(), ProtocolValidationError> {
        if self.failed {
            return Err(ProtocolValidationError::new(
                "client message decoder has failed",
            ));
        }
        self.frames.end().map_err(|e| {
            self.failed = true;
            ProtocolValidationError::new(format!(
                "Invalid client protocol framing: {}",
                bounded_error_message(&e.0)
            ))
        })
    }
}

/// Server-side mirror of [`ClientMessageDecoder`].
pub struct ServerMessageDecoder {
    frames: FrameDecoder,
    max_byte_length: u64,
    failed: bool,
}

impl ServerMessageDecoder {
    pub fn new(max_frame_length: Option<u64>) -> Self {
        let max_frame_length = resolve_max_frame_length(max_frame_length);
        Self {
            frames: FrameDecoder::new(Some(max_frame_length)),
            max_byte_length: max_frame_length,
            failed: false,
        }
    }

    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<ServerMessage>, ProtocolValidationError> {
        if self.failed {
            return Err(ProtocolValidationError::new(
                "server message decoder has failed",
            ));
        }
        match self.push_inner(chunk) {
            Ok(messages) => Ok(messages),
            Err(error) => {
                self.failed = true;
                Err(error)
            }
        }
    }

    fn push_inner(&mut self, chunk: &[u8]) -> Result<Vec<ServerMessage>, ProtocolValidationError> {
        let raw_frames = self.frames.push(chunk).map_err(|e| {
            ProtocolValidationError::new(format!(
                "Invalid server protocol frame: {}",
                bounded_error_message(&e.0)
            ))
        })?;
        let mut messages = Vec::with_capacity(raw_frames.len());
        for frame in raw_frames {
            let json = decode_one(&frame, self.max_byte_length).map_err(|e| {
                ProtocolValidationError::new(format!(
                    "Invalid server protocol frame: {}",
                    bounded_error_message(&e.0)
                ))
            })?;
            messages.push(ServerMessage::parse(&json)?);
        }
        Ok(messages)
    }

    pub fn end(&mut self) -> Result<(), ProtocolValidationError> {
        if self.failed {
            return Err(ProtocolValidationError::new(
                "server message decoder has failed",
            ));
        }
        self.frames.end().map_err(|e| {
            self.failed = true;
            ProtocolValidationError::new(format!(
                "Invalid server protocol framing: {}",
                bounded_error_message(&e.0)
            ))
        })
    }
}
