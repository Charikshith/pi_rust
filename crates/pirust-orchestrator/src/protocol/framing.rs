//! Port of `packages/protocol/src/framing.ts` — 4-byte big-endian
//! length-prefixed framing. See `docs/analysis/04-orchestrator.md` §3.

use thiserror::Error;

const FRAME_HEADER_LENGTH: usize = 4;

/// Default upper bound for one framed CBOR payload.
pub const DEFAULT_MAX_FRAME_LENGTH: u64 = 16 * 1024 * 1024;

#[derive(Debug, Clone, Error)]
#[error("{0}")]
pub struct FrameError(pub String);

impl FrameError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

/// Prefixes a payload with its unsigned 32-bit big-endian byte length.
pub fn encode_frame(payload: &[u8]) -> Result<Vec<u8>, FrameError> {
    if payload.len() > u32::MAX as usize {
        return Err(FrameError::new(
            "Frame payload exceeds the unsigned 32-bit length limit",
        ));
    }
    let mut frame = Vec::with_capacity(FRAME_HEADER_LENGTH + payload.len());
    frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    frame.extend_from_slice(payload);
    Ok(frame)
}

/// Validates that bytes contain exactly one complete frame within the
/// configured limit.
pub fn assert_complete_frame(
    frame: &[u8],
    max_frame_length: Option<u64>,
) -> Result<(), FrameError> {
    let max_frame_length = max_frame_length.unwrap_or(DEFAULT_MAX_FRAME_LENGTH);
    if frame.len() < FRAME_HEADER_LENGTH {
        return Err(FrameError::new(
            "Frame does not contain a complete length prefix",
        ));
    }
    let length = read_header(frame);
    if length > max_frame_length {
        return Err(FrameError::new(format!(
            "Frame length {length} exceeds configured limit of {max_frame_length}"
        )));
    }
    if frame.len() as u64 != FRAME_HEADER_LENGTH as u64 + length {
        return Err(FrameError::new(
            "Frame must contain exactly one complete payload",
        ));
    }
    Ok(())
}

fn read_header(frame: &[u8]) -> u64 {
    u32::from_be_bytes([frame[0], frame[1], frame[2], frame[3]]) as u64
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DecoderState {
    Open,
    Ended,
    Failed,
}

/// Incrementally splits arbitrary byte chunks into length-prefixed payloads.
///
/// Unlike the TS original's manual internal-block chunking (a pure
/// allocation-count optimization with no observable effect on output), this
/// port just grows a `Vec<u8>` directly — same behavior, simpler code
/// (Ponytail-ladder / Rust-advantage precedent already set elsewhere in this
/// codebase, e.g. feat-008 Wave 5's `openai_completions.rs` refactors).
pub struct FrameDecoder {
    max_frame_length: u64,
    header: Vec<u8>,
    expected_payload_length: Option<u64>,
    payload: Vec<u8>,
    state: DecoderState,
}

impl FrameDecoder {
    pub fn new(max_frame_length: Option<u64>) -> Self {
        Self {
            max_frame_length: max_frame_length.unwrap_or(DEFAULT_MAX_FRAME_LENGTH),
            header: Vec::with_capacity(FRAME_HEADER_LENGTH),
            expected_payload_length: None,
            payload: Vec::new(),
            state: DecoderState::Open,
        }
    }

    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<Vec<u8>>, FrameError> {
        match self.state {
            DecoderState::Ended => return Err(FrameError::new("Frame decoder has ended")),
            DecoderState::Failed => return Err(FrameError::new("Frame decoder has failed")),
            DecoderState::Open => {}
        }

        let mut frames = Vec::new();
        let mut offset = 0;
        while offset < chunk.len() {
            if self.expected_payload_length.is_none() {
                let need = FRAME_HEADER_LENGTH - self.header.len();
                let take = need.min(chunk.len() - offset);
                self.header.extend_from_slice(&chunk[offset..offset + take]);
                offset += take;
                if self.header.len() < FRAME_HEADER_LENGTH {
                    continue;
                }
                let frame_length = read_header(&self.header);
                self.header.clear();
                if frame_length > self.max_frame_length {
                    return Err(self.fail(format!(
                        "Frame length {frame_length} exceeds configured limit of {}",
                        self.max_frame_length
                    )));
                }
                if frame_length == 0 {
                    frames.push(Vec::new());
                    continue;
                }
                self.expected_payload_length = Some(frame_length);
                self.payload = Vec::with_capacity((frame_length as usize).min(1 << 20));
            }

            let expected = self.expected_payload_length.expect("just set above");
            let remaining = expected - self.payload.len() as u64;
            let take = (remaining as usize).min(chunk.len() - offset);
            self.payload
                .extend_from_slice(&chunk[offset..offset + take]);
            offset += take;
            if self.payload.len() as u64 == expected {
                frames.push(std::mem::take(&mut self.payload));
                self.expected_payload_length = None;
            }
        }
        Ok(frames)
    }

    pub fn end(&mut self) -> Result<(), FrameError> {
        match self.state {
            DecoderState::Ended => return Err(FrameError::new("Frame decoder has ended")),
            DecoderState::Failed => return Err(FrameError::new("Frame decoder has failed")),
            DecoderState::Open => {}
        }
        if !self.header.is_empty() || self.expected_payload_length.is_some() {
            return Err(self.fail("Truncated frame at end of stream"));
        }
        self.state = DecoderState::Ended;
        Ok(())
    }

    fn fail(&mut self, message: impl Into<String>) -> FrameError {
        self.state = DecoderState::Failed;
        self.header.clear();
        self.payload.clear();
        self.expected_payload_length = None;
        FrameError::new(message)
    }
}
