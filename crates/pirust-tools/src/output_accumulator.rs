//! Port of `core/tools/output-accumulator.ts` — bash's streaming output buffer.
//!
//! Gated by `tests/fixtures/pi/tools/output_accumulator.cases.jsonl` (21 cases).
//!
//! [`OutputAccumulator`] is what the bash tool pushes every stdout/stderr chunk
//! into while a process runs. It keeps memory bounded in two independent ways:
//! raw bytes are held in memory only until a limit trips, after which they are
//! spilled to a temp file (and every later chunk goes straight there); and the
//! decoded text is kept only as a rolling *tail*, re-trimmed once it grows past
//! 4× `maxBytes`. [`OutputAccumulator::snapshot`] turns that tail into the
//! truncated view the TUI streams, while reporting the *global* counters.
//!
//! Ported items (TS `output-accumulator.ts`):
//! - `OutputAccumulatorOptions` (`:7-11`)   → [`OutputAccumulatorOptions`]
//! - `OutputSnapshot`           (`:13-17`)  → [`OutputSnapshot`]
//! - `defaultTempFilePath`      (`:19-22`)  → [`default_temp_file_path`]
//! - `byteLength`               (`:24-26`)  → `str::len`: a Rust `&str` is UTF-8,
//!   so `Buffer.byteLength(s, "utf-8") == s.len()` (same identity `truncate.rs` uses)
//! - `OutputAccumulator`        (`:35-222`) → [`OutputAccumulator`], with
//!   `append` (`:64-78`), `finish` (`:80-89`), `snapshot` (`:91-119`),
//!   `closeTempFile` (`:121-142`), `getLastLineBytes` (`:144-146`),
//!   `appendDecodedText` (`:148-177`), `trimTail` (`:179-194`),
//!   `getSnapshotText` (`:196-203`), `shouldUseTempFile` (`:205-209`) and
//!   `ensureTempFile` (`:211-221`)
//!
//! # The streaming UTF-8 decoder
//!
//! Pi decodes with one long-lived `new TextDecoder()` and calls
//! `decode(chunk, { stream: true })` (TS `:70`), then `decode()` with no argument
//! from `finish()` (TS `:85`). That is load-bearing: process output splits on
//! arbitrary byte boundaries, so a multi-byte character routinely straddles two
//! chunks. [`Utf8StreamDecoder`] reproduces it — an incomplete trailing sequence
//! is *retained* and completed by the next chunk (fixture scenario
//! `split-multibyte`), never turned into U+FFFD.
//!
//! **Incomplete vs. invalid.** WHATWG's UTF-8 decoder emits U+FFFD only for bytes
//! that *cannot* start/continue a valid sequence; a truncated-but-still-plausible
//! prefix is held back until end-of-stream, where the final flush emits exactly one
//! U+FFFD for it. `std::str::from_utf8` draws the same line: `error_len() == None`
//! means "incomplete at the end of the buffer" (hold), `Some(n)` means "`n` bytes
//! are genuinely ill-formed" (one U+FFFD, skip them). Rust's `error_len` implements
//! the Unicode "maximal subpart" rule, which is precisely the substitution count
//! WHATWG's state machine produces — e.g. `E0 80` yields two U+FFFDs in both, and
//! `F0 80 80 80` yields four.
//!
//! # Deliberate divergences
//!
//! - **Temp filename prefix.** Pi defaults to `"pi-output"` and bash passes
//!   `"pi-bash"`; this port defaults to [`DEFAULT_TEMP_FILE_PREFIX`]
//!   (`"pirust-output"`) and pirust callers pass `"pirust-bash"`. The temp file
//!   name is not a Pi-compat wire format — nothing parses it, only the *presence*
//!   of a path is contractual (which is why the oracle fixture normalizes it to
//!   `{TMPFILE}`) — so the rename keeps pirust's spill files distinguishable from a
//!   concurrently running Pi's.
//! - **Random id source.** Pi uses `randomBytes(8).toString("hex")`. No RNG crate is
//!   in this crate's dependency set, so [`default_temp_file_path`] derives the same
//!   *shape* (16 lowercase hex digits + `.log`) from `RandomState`'s
//!   system-seeded hasher mixed with the pid, a monotonic counter and the clock.
//! - **Error propagation.** Pi's `append` *throws* on a finished accumulator
//!   (TS `:65-67`); here it returns [`AppendAfterFinish`]. Write/open failures are
//!   surfaced like Node's `WriteStream` `"error"` event: recorded, then reported by
//!   [`OutputAccumulator::close_temp_file`] (Pi's promise rejection, TS `:130-133`),
//!   never by `append`.

use std::collections::hash_map::RandomState;
use std::fs::File;
use std::hash::{BuildHasher, Hasher};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::truncate::{
    truncate_tail, TruncatedBy, TruncationOptions, TruncationResult, DEFAULT_MAX_BYTES,
    DEFAULT_MAX_LINES,
};

/// Default temp-file prefix. Pi's is `"pi-output"` (TS `:61`) — see the module docs
/// for why this port renames it.
pub const DEFAULT_TEMP_FILE_PREFIX: &str = "pirust-output";

/// Construction options (TS `OutputAccumulatorOptions`, `:7-11`).
///
/// `None` means "apply Pi's `??` default" (TS `:58-61`): [`DEFAULT_MAX_LINES`],
/// [`DEFAULT_MAX_BYTES`], [`DEFAULT_TEMP_FILE_PREFIX`]. `u64` limits for the same
/// reason as [`TruncationOptions`] — callers pass `Number.MAX_SAFE_INTEGER`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct OutputAccumulatorOptions {
    /// Line budget for snapshots and the spill trigger (TS `:8`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_lines: Option<u64>,
    /// Byte budget for snapshots and the spill trigger (TS `:9`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_bytes: Option<u64>,
    /// Prefix of the spill file's basename (TS `:10`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temp_file_prefix: Option<String>,
}

/// [`OutputAccumulator::snapshot`] argument (TS `{ persistIfTruncated?: boolean }`,
/// `:91`).
///
/// Pi tests the field for JS truthiness (TS `:110`), so an absent field and `false`
/// behave identically — hence a plain `bool` with `serde(default)` rather than an
/// `Option`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SnapshotOptions {
    /// Create the spill file if the snapshot reports truncation (TS `:110-112`).
    pub persist_if_truncated: bool,
}

/// One streaming view of the accumulated output (TS `OutputSnapshot`, `:13-17`).
///
/// `content` duplicates `truncation.content`, exactly as Pi returns it (TS `:115`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputSnapshot {
    /// The truncated tail to display (TS `:14`).
    pub content: String,
    /// Truncation bookkeeping, with global counters — see [`OutputAccumulator::snapshot`]
    /// (TS `:15`).
    pub truncation: TruncationResult,
    /// Path of the spill file holding the *complete* raw output, once one exists
    /// (TS `:16`, optional). Skipped when serializing to mirror Pi's `undefined`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub full_output_path: Option<PathBuf>,
}

/// `append` was called after `finish` (TS `throw new Error(...)`, `:66`). The
/// message is Pi's, verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("Cannot append to a finished output accumulator")]
pub struct AppendAfterFinish;

/// `join(tmpdir(), `${prefix}-${randomBytes(8).toString("hex")}.log`)`
/// (TS `defaultTempFilePath`, `:19-22`).
///
/// The id is 16 lowercase hex digits (8 bytes' worth), like Pi's. Its *source*
/// differs — see the module docs' divergence note.
fn default_temp_file_path(prefix: &str) -> PathBuf {
    /// Guarantees distinct ids within a process even if the clock is coarse.
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    // `RandomState::new()` is seeded from system randomness (and re-keyed per
    // thread), so this is not merely a hash of predictable inputs.
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
    hasher.write_u32(std::process::id());
    hasher.write_u128(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos()),
    );
    let id = hasher.finish();

    // `std::env::temp_dir()` is `os.tmpdir()`'s equivalent (`TMPDIR`/`TMP`/`TEMP`,
    // else `/tmp`).
    std::env::temp_dir().join(format!("{prefix}-{id:016x}.log"))
}

/// Incremental UTF-8 decoder mirroring `new TextDecoder()` +
/// `decode(chunk, { stream: true })` (TS `:40`, `:70`, `:85`).
///
/// Two behaviours are reproduced beyond plain validation:
/// - a trailing *incomplete* sequence is retained for the next chunk, and only the
///   end-of-stream [`flush`](Self::flush) turns it into a single U+FFFD, whereas
///   genuinely ill-formed bytes become U+FFFD immediately (see the module docs);
/// - a leading U+FEFF is dropped. `new TextDecoder()` leaves `ignoreBOM` at
///   `false`, which per the Encoding Standard means "strip the BOM": the *first*
///   code point of the stream is discarded when it is U+FEFF. Pi therefore never
///   counts a BOM in `totalDecodedBytes`, and neither does this port.
#[derive(Debug, Default)]
struct Utf8StreamDecoder {
    /// Trailing bytes of an incomplete sequence, carried to the next chunk.
    pending: Vec<u8>,
    /// Whether the stream's first code point has been seen (BOM sniffing done).
    bom_seen: bool,
}

impl Utf8StreamDecoder {
    /// `decoder.decode(chunk, { stream: true })`.
    fn decode(&mut self, chunk: &[u8]) -> String {
        self.pending.extend_from_slice(chunk);
        let mut out = String::new();
        loop {
            match std::str::from_utf8(&self.pending) {
                Ok(valid) => {
                    out.push_str(valid);
                    self.pending.clear();
                    break;
                }
                Err(err) => {
                    let valid_up_to = err.valid_up_to();
                    out.push_str(
                        std::str::from_utf8(&self.pending[..valid_up_to])
                            .expect("valid_up_to marks a valid UTF-8 boundary"),
                    );
                    match err.error_len() {
                        // Incomplete sequence at the end of the buffer: keep the
                        // tail so the next chunk can complete it. This is the whole
                        // point of `{ stream: true }`.
                        None => {
                            self.pending.drain(..valid_up_to);
                            break;
                        }
                        // Genuinely ill-formed bytes: one U+FFFD, skip them.
                        Some(bad_len) => {
                            out.push('\u{FFFD}');
                            self.pending.drain(..valid_up_to + bad_len);
                        }
                    }
                }
            }
        }
        self.strip_bom(out)
    }

    /// `decoder.decode()` with no argument (TS `:85`): end-of-stream. A retained
    /// incomplete sequence becomes exactly one U+FFFD.
    fn flush(&mut self) -> String {
        let out = if self.pending.is_empty() {
            String::new()
        } else {
            self.pending.clear();
            '\u{FFFD}'.to_string()
        };
        self.strip_bom(out)
    }

    /// Drop the stream's leading U+FEFF (see the type docs). Any first code point —
    /// U+FFFD included — closes BOM sniffing, matching the Encoding Standard's
    /// "BOM seen" flag.
    fn strip_bom(&mut self, out: String) -> String {
        if self.bom_seen || out.is_empty() {
            return out;
        }
        self.bom_seen = true;
        if out.starts_with('\u{FEFF}') {
            return out['\u{FEFF}'.len_utf8()..].to_string();
        }
        out
    }
}

/// Incrementally tracks streaming output with bounded memory
/// (TS `OutputAccumulator`, `:28-222`).
///
/// Appended chunks are decoded with a streaming UTF-8 decoder, only a decoded tail
/// is kept for display snapshots, and a temp file is opened once the full output
/// needs preserving.
#[derive(Debug)]
pub struct OutputAccumulator {
    // ---- configuration (TS `:36-39`) ----
    max_lines: u64,
    max_bytes: u64,
    /// `Math.max(maxBytes * 2, 1)` (TS `:60`). `saturating_mul` because `maxBytes`
    /// may legitimately be `Number.MAX_SAFE_INTEGER`-sized; JS would have produced a
    /// float there, and either way the rolling tail is then effectively unbounded.
    max_rolling_bytes: u64,
    temp_file_prefix: String,

    // ---- state (TS `:40-55`) ----
    decoder: Utf8StreamDecoder,
    raw_chunks: Vec<Vec<u8>>,
    tail_text: String,
    tail_bytes: u64,
    tail_starts_at_line_boundary: bool,
    total_raw_bytes: u64,
    total_decoded_bytes: u64,
    completed_lines: u64,
    total_lines: u64,
    current_line_bytes: u64,
    has_open_line: bool,
    finished: bool,

    temp_file_path: Option<PathBuf>,
    /// Pi's `tempFileStream` (TS `:55`). `BufWriter` stands in for Node's
    /// `WriteStream`'s internal buffering; `close_temp_file` flushes it.
    temp_file_writer: Option<BufWriter<File>>,
    /// First open/write failure, replayed by [`Self::close_temp_file`] the way Node
    /// emits a stream `"error"` event (TS `:130-133`).
    temp_file_error: Option<io::Error>,
}

impl Default for OutputAccumulator {
    fn default() -> Self {
        Self::new(OutputAccumulatorOptions::default())
    }
}

impl OutputAccumulator {
    /// TS `constructor` (`:57-62`).
    pub fn new(options: OutputAccumulatorOptions) -> Self {
        let max_bytes = options.max_bytes.unwrap_or(DEFAULT_MAX_BYTES);
        Self {
            max_lines: options.max_lines.unwrap_or(DEFAULT_MAX_LINES),
            max_bytes,
            max_rolling_bytes: max_bytes.saturating_mul(2).max(1),
            temp_file_prefix: options
                .temp_file_prefix
                .unwrap_or_else(|| DEFAULT_TEMP_FILE_PREFIX.to_string()),

            decoder: Utf8StreamDecoder::default(),
            raw_chunks: Vec::new(),
            tail_text: String::new(),
            tail_bytes: 0,
            tail_starts_at_line_boundary: true,
            total_raw_bytes: 0,
            total_decoded_bytes: 0,
            completed_lines: 0,
            total_lines: 0,
            current_line_bytes: 0,
            has_open_line: false,
            finished: false,

            temp_file_path: None,
            temp_file_writer: None,
            temp_file_error: None,
        }
    }

    /// Feed one raw chunk (TS `append`, `:64-78`).
    ///
    /// The decode happens *before* the spill check, so a chunk that pushes the
    /// counters over a limit opens the temp file within the same call and the
    /// already-buffered chunks are flushed ahead of it, in order.
    ///
    /// # Errors
    /// [`AppendAfterFinish`] once [`Self::finish`] has run (TS `:65-67`). I/O
    /// failures are not reported here — see [`Self::close_temp_file`].
    pub fn append(&mut self, data: &[u8]) -> Result<(), AppendAfterFinish> {
        if self.finished {
            return Err(AppendAfterFinish);
        }

        self.total_raw_bytes += data.len() as u64;
        let text = self.decoder.decode(data);
        self.append_decoded_text(&text);

        if self.temp_file_writer.is_some() || self.should_use_temp_file() {
            self.ensure_temp_file();
            // TS `this.tempFileStream?.write(data)`: a no-op if the stream was
            // already closed by `closeTempFile` (the path survives, so
            // `ensureTempFile` above returned early without reopening).
            self.write_to_temp_file(data);
        } else if !data.is_empty() {
            self.raw_chunks.push(data.to_vec());
        }

        Ok(())
    }

    /// End of stream (TS `finish`, `:80-89`). Idempotent: flushes the decoder's
    /// dangling bytes once, then opens the spill file if a limit was crossed.
    pub fn finish(&mut self) {
        if self.finished {
            return;
        }
        self.finished = true;
        let text = self.decoder.flush();
        self.append_decoded_text(&text);
        if self.should_use_temp_file() {
            self.ensure_temp_file();
        }
    }

    /// Build a streaming view of the output so far (TS `snapshot`, `:91-119`).
    ///
    /// The window is the rolling tail — [`truncate_tail`] over
    /// [`Self::get_snapshot_text`] supplies `content`, `output_lines`,
    /// `output_bytes`, `last_line_partial` and `first_line_exceeds_limit`.
    /// Everything describing the *whole* run is then overwritten from the
    /// accumulator's global counters (TS `:96-108`), which is why a snapshot can
    /// report `truncated = true` while the tail window itself needed no truncating
    /// (fixture `byte-limit-and-trimtail` step 6: the tail is empty, yet
    /// `totalBytes = 367 > 40`). In that case `truncatedBy` falls back to the global
    /// comparison — `"bytes"` if the byte limit is over, else `"lines"` (TS `:98`).
    ///
    /// Side-effect free except for `persist_if_truncated`, which creates the spill
    /// file when the snapshot is truncated (TS `:110-112`).
    pub fn snapshot(&mut self, options: SnapshotOptions) -> OutputSnapshot {
        let tail_truncation = truncate_tail(
            self.get_snapshot_text(),
            TruncationOptions {
                max_lines: Some(self.max_lines),
                max_bytes: Some(self.max_bytes),
            },
        );

        let truncated =
            self.total_lines > self.max_lines || self.total_decoded_bytes > self.max_bytes;
        let truncated_by = if truncated {
            // TS `??`: keep the tail window's verdict when it has one.
            tail_truncation
                .truncated_by
                .or(Some(if self.total_decoded_bytes > self.max_bytes {
                    TruncatedBy::Bytes
                } else {
                    TruncatedBy::Lines
                }))
        } else {
            None
        };

        let truncation = TruncationResult {
            truncated,
            truncated_by,
            total_lines: self.total_lines,
            total_bytes: self.total_decoded_bytes,
            max_lines: self.max_lines,
            max_bytes: self.max_bytes,
            ..tail_truncation
        };

        if options.persist_if_truncated && truncation.truncated {
            self.ensure_temp_file();
        }

        OutputSnapshot {
            content: truncation.content.clone(),
            truncation,
            full_output_path: self.temp_file_path.clone(),
        }
    }

    /// Close the spill file (TS `closeTempFile`, `:121-142`). A no-op without one.
    ///
    /// Async for the same reason Pi returns a promise: Node's `stream.end()` settles
    /// on `"finish"`. The flush + close syscalls run on a blocking worker so they
    /// never stall the runtime.
    ///
    /// # Errors
    /// The first open/write failure recorded during `append`/`ensure_temp_file`
    /// (Node's `"error"` event, TS `:130-133`), else a failure of the final flush.
    pub async fn close_temp_file(&mut self) -> io::Result<()> {
        let Some(mut writer) = self.temp_file_writer.take() else {
            return Ok(());
        };
        // An earlier failure would have reached Pi's promise first ("error" is
        // emitted before "finish"), so it wins over the flush result.
        let recorded = self.temp_file_error.take();

        let flushed = tokio::task::spawn_blocking(move || writer.flush())
            .await
            .map_err(io::Error::other)?;

        match recorded {
            Some(err) => Err(err),
            None => flushed,
        }
    }

    /// Byte length of the still-open last line (TS `getLastLineBytes`, `:144-146`).
    /// Bash's byte-limit footer uses it.
    pub fn get_last_line_bytes(&self) -> u64 {
        self.current_line_bytes
    }

    /// Path of the spill file, once one exists. Not in Pi (whose `tempFilePath` is
    /// private and read only through `snapshot`); exposed here so a caller can clean
    /// the file up without taking a snapshot.
    pub fn temp_file_path(&self) -> Option<&Path> {
        self.temp_file_path.as_deref()
    }

    /// Fold newly decoded text into the tail and the line/byte counters
    /// (TS `appendDecodedText`, `:148-177`).
    fn append_decoded_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }

        let bytes = text.len() as u64;
        self.total_decoded_bytes += bytes;
        self.tail_text.push_str(text);
        self.tail_bytes += bytes;
        // 4× `maxBytes`: `trimTail` then cuts back to `maxRollingBytes` (2×), so the
        // O(n) rebuild is amortized (TS `:157-159`).
        if self.tail_bytes > self.max_rolling_bytes.saturating_mul(2) {
            self.trim_tail();
        }

        // TS scans with `indexOf("\n")`, keeping the count and the LAST index.
        match text.rfind('\n') {
            None => {
                self.current_line_bytes += bytes;
                self.has_open_line = true;
            }
            Some(last_newline) => {
                self.completed_lines += text.matches('\n').count() as u64;
                let tail = &text[last_newline + 1..];
                self.current_line_bytes = tail.len() as u64;
                self.has_open_line = !tail.is_empty();
            }
        }
        self.total_lines = self.completed_lines + u64::from(self.has_open_line);
    }

    /// Cut the rolling tail back to `maxRollingBytes` (TS `trimTail`, `:179-194`).
    ///
    /// The cut point is advanced past UTF-8 continuation bytes (`b & 0xC0 == 0x80`),
    /// so it always lands on a character boundary — the same move `truncate.rs`'s
    /// `truncate_string_to_bytes_from_end` makes, and what keeps Pi's
    /// `buffer.subarray(start).toString("utf-8")` from minting a U+FFFD.
    fn trim_tail(&mut self) {
        let len = self.tail_text.len() as u64;
        if len <= self.max_rolling_bytes {
            self.tail_bytes = len;
            return;
        }

        let bytes = self.tail_text.as_bytes();
        let mut start = bytes.len() - self.max_rolling_bytes as usize;
        while start < bytes.len() && (bytes[start] & 0xc0) == 0x80 {
            start += 1;
        }

        // TS `start === 0 ? this.tailStartsAtLineBoundary : buffer[start - 1] === 0x0a`.
        // `start` is >= 1 here (the `len <= maxRollingBytes` case returned above), so
        // the "unchanged" arm is unreachable; kept for exactness.
        self.tail_starts_at_line_boundary = if start == 0 {
            self.tail_starts_at_line_boundary
        } else {
            bytes[start - 1] == b'\n'
        };
        self.tail_text = self.tail_text[start..].to_string();
        self.tail_bytes = self.tail_text.len() as u64;
    }

    /// The tail as a snapshot should see it (TS `getSnapshotText`, `:196-203`).
    ///
    /// When [`Self::trim_tail`] cut mid-line, the leading partial line is dropped
    /// along with its newline; a tail with no newline at all is returned unchanged.
    fn get_snapshot_text(&self) -> &str {
        if self.tail_starts_at_line_boundary {
            return &self.tail_text;
        }
        match self.tail_text.find('\n') {
            None => &self.tail_text,
            Some(first_newline) => &self.tail_text[first_newline + 1..],
        }
    }

    /// Whether the full output must be preserved on disk
    /// (TS `shouldUseTempFile`, `:205-209`). All three comparisons are strict, so
    /// output sitting exactly *on* a limit does not spill.
    fn should_use_temp_file(&self) -> bool {
        self.total_raw_bytes > self.max_bytes
            || self.total_decoded_bytes > self.max_bytes
            || self.total_lines > self.max_lines
    }

    /// Open the spill file and flush the in-memory raw chunks into it, once
    /// (TS `ensureTempFile`, `:211-221`).
    ///
    /// Pi sets `tempFilePath` *before* `createWriteStream`, and that call is lazy —
    /// an open failure surfaces later as a stream `"error"`. So the path is recorded
    /// even when the open fails, and the failure is deferred to
    /// [`Self::close_temp_file`].
    fn ensure_temp_file(&mut self) {
        if self.temp_file_path.is_some() {
            return;
        }
        let path = default_temp_file_path(&self.temp_file_prefix);
        self.temp_file_path = Some(path.clone());
        match File::create(&path) {
            Ok(file) => self.temp_file_writer = Some(BufWriter::new(file)),
            Err(err) => self.record_temp_file_error(err),
        }
        for chunk in std::mem::take(&mut self.raw_chunks) {
            self.write_to_temp_file(&chunk);
        }
    }

    /// TS `this.tempFileStream?.write(data)` (`:74`, `:218`): fire-and-forget.
    fn write_to_temp_file(&mut self, data: &[u8]) {
        let Some(writer) = self.temp_file_writer.as_mut() else {
            return;
        };
        if let Err(err) = writer.write_all(data) {
            self.record_temp_file_error(err);
        }
    }

    /// Keep the FIRST failure, like a stream whose `"error"` event fires once.
    fn record_temp_file_error(&mut self, err: io::Error) {
        if self.temp_file_error.is_none() {
            self.temp_file_error = Some(err);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(max_lines: u64, max_bytes: u64) -> OutputAccumulatorOptions {
        OutputAccumulatorOptions {
            max_lines: Some(max_lines),
            max_bytes: Some(max_bytes),
            temp_file_prefix: Some("pirust-oa-unit".to_string()),
        }
    }

    /// Deletes an accumulator's spill file when the test ends, *including* on panic,
    /// so a failing assertion never litters the temp dir.
    ///
    /// Declare it BEFORE the accumulator: locals drop in reverse declaration order,
    /// so the accumulator (and its file handle) is dropped first.
    #[derive(Default)]
    struct SpillGuard(std::cell::RefCell<Option<PathBuf>>);

    impl SpillGuard {
        /// Record whatever spill file `acc` has opened so far.
        fn watch(&self, acc: &OutputAccumulator) {
            *self.0.borrow_mut() = acc.temp_file_path().map(Path::to_path_buf);
        }
    }

    impl Drop for SpillGuard {
        fn drop(&mut self) {
            if let Some(path) = self.0.borrow().as_ref() {
                let _ = std::fs::remove_file(path);
            }
        }
    }

    /// The behaviour `{ stream: true }` exists for: a multi-byte character split
    /// across chunk boundaries must survive. Two- and three-way splits, plus a
    /// 4-byte character fed one byte at a time.
    #[test]
    fn multibyte_char_split_across_chunks_is_preserved() {
        // 2-byte char split 1+1 (the same split the oracle fixture captures).
        let mut decoder = Utf8StreamDecoder::default();
        assert_eq!(decoder.decode(&[0xc3]), "");
        assert_eq!(decoder.decode(&[0xa9]), "é");
        assert_eq!(decoder.flush(), "");

        // 3-byte char split 1+1+1.
        let mut decoder = Utf8StreamDecoder::default();
        assert_eq!(decoder.decode(&[0xe4]), "");
        assert_eq!(decoder.decode(&[0xb8]), "");
        assert_eq!(decoder.decode(&[0x96]), "世");

        // 3-byte char split 2+1, with surrounding ASCII in the same chunks.
        let mut decoder = Utf8StreamDecoder::default();
        assert_eq!(decoder.decode(&[b'a', 0xe4, 0xb8]), "a");
        assert_eq!(decoder.decode(&[0x96, b'b']), "世b");

        // 4-byte char, one byte per chunk.
        let mut decoder = Utf8StreamDecoder::default();
        for byte in "🎉".as_bytes() {
            let out = decoder.decode(&[*byte]);
            assert!(out.is_empty() || out == "🎉");
        }

        // Full-stream equivalence: any chunking of mixed-width text decodes to the
        // same string as decoding it whole.
        let text = "caf\u{e9} — 世界 🎉 last";
        for size in 1..=6 {
            let mut decoder = Utf8StreamDecoder::default();
            let mut out = String::new();
            for chunk in text.as_bytes().chunks(size) {
                out.push_str(&decoder.decode(chunk));
            }
            out.push_str(&decoder.flush());
            assert_eq!(out, text, "chunk size {size}");
        }
    }

    /// Incomplete (hold, then one U+FFFD at end-of-stream) vs. genuinely invalid
    /// (immediate U+FFFD, maximal-subpart count) — see the module docs.
    #[test]
    fn incomplete_sequences_are_held_but_invalid_ones_become_replacement_chars() {
        // Incomplete: held back, never substituted mid-stream.
        let mut decoder = Utf8StreamDecoder::default();
        assert_eq!(decoder.decode(&[b'x', 0xe4, 0xb8]), "x");
        assert_eq!(
            decoder.flush(),
            "\u{FFFD}",
            "one U+FFFD for the dangling head"
        );
        assert_eq!(decoder.flush(), "", "flush is idempotent");

        // Invalid: substituted immediately, without waiting for more bytes.
        let mut decoder = Utf8StreamDecoder::default();
        assert_eq!(decoder.decode(&[0xff, b'a']), "\u{FFFD}a");

        // `C3 28`: `28` cannot continue the sequence, so `C3` is one maximal subpart
        // and `(` decodes normally.
        let mut decoder = Utf8StreamDecoder::default();
        assert_eq!(decoder.decode(&[0xc3, 0x28]), "\u{FFFD}(");

        // `E0 80`: `80` is outside `E0`'s A0..BF continuation range → two U+FFFDs,
        // matching WHATWG's "prepend and reprocess" step.
        let mut decoder = Utf8StreamDecoder::default();
        assert_eq!(decoder.decode(&[0xe0, 0x80]), "\u{FFFD}\u{FFFD}");

        // `F0 80 80 80`: four maximal subparts → four U+FFFDs.
        let mut decoder = Utf8StreamDecoder::default();
        assert_eq!(
            decoder.decode(&[0xf0, 0x80, 0x80, 0x80]),
            "\u{FFFD}\u{FFFD}\u{FFFD}\u{FFFD}"
        );
    }

    /// `new TextDecoder()` leaves `ignoreBOM = false`, so a leading U+FEFF is
    /// stripped — including when it arrives split across chunks — and only the first
    /// one.
    #[test]
    fn leading_byte_order_mark_is_stripped_once() {
        let mut decoder = Utf8StreamDecoder::default();
        assert_eq!(decoder.decode("\u{FEFF}hi".as_bytes()), "hi");
        // A later BOM is ordinary content.
        assert_eq!(decoder.decode("\u{FEFF}".as_bytes()), "\u{FEFF}");

        let mut decoder = Utf8StreamDecoder::default();
        assert_eq!(decoder.decode(&[0xef, 0xbb]), "");
        assert_eq!(decoder.decode(&[0xbf, b'a']), "a");

        // The accumulator's byte counters therefore exclude it, as Pi's do.
        let mut acc = OutputAccumulator::new(opts(10, 4096));
        acc.append("\u{FEFF}ok\n".as_bytes()).unwrap();
        assert_eq!(acc.snapshot(SnapshotOptions::default()).content, "ok\n");
        assert_eq!(acc.total_decoded_bytes, 3);
    }

    #[test]
    fn append_after_finish_is_rejected() {
        let mut acc = OutputAccumulator::new(opts(10, 4096));
        acc.append(b"a\n").unwrap();
        acc.finish();
        assert_eq!(acc.append(b"b\n"), Err(AppendAfterFinish));
        assert_eq!(
            AppendAfterFinish.to_string(),
            "Cannot append to a finished output accumulator"
        );
        // Idempotent: a second finish must not double-flush the decoder.
        acc.finish();
        assert_eq!(acc.snapshot(SnapshotOptions::default()).content, "a\n");
    }

    /// Defaults straight from TS `:58-61`.
    #[test]
    fn constructor_defaults_match_pi() {
        let acc = OutputAccumulator::default();
        assert_eq!(acc.max_lines, 2000);
        assert_eq!(acc.max_bytes, 51200);
        assert_eq!(acc.max_rolling_bytes, 102400);
        assert_eq!(acc.temp_file_prefix, "pirust-output");

        // `Math.max(maxBytes * 2, 1)` — the floor matters only at `maxBytes = 0`.
        let zero = OutputAccumulator::new(opts(10, 0));
        assert_eq!(zero.max_rolling_bytes, 1);
    }

    /// Pins the two `trimTail` thresholds *structurally*, against the TS source
    /// (`:157` fires at `tailBytes > maxRollingBytes * 2`, `:186` cuts back to
    /// `maxRollingBytes`), because the oracle fixture cannot: in every fixture step
    /// where a wrong threshold would trim early, the snapshot is dominated by the
    /// byte-limit truncation of a long last line, so the visible content is
    /// identical either way. Only the internal window size differs.
    #[test]
    fn trim_tail_fires_at_four_times_max_bytes_and_cuts_back_to_two() {
        // maxBytes 10 → maxRollingBytes 20, trim trigger 40.
        let cleanup = SpillGuard::default();
        let mut acc = OutputAccumulator::new(opts(1_000, 10));
        acc.append(&b"a".repeat(40)).unwrap();
        // 40 raw bytes > maxBytes, so `append` has already spilled.
        cleanup.watch(&acc);
        assert_eq!(
            (acc.tail_bytes, acc.tail_text.len()),
            (40, 40),
            "exactly ON the trigger must not trim (TS uses a strict `>`)"
        );

        acc.append(b"a").unwrap();
        assert_eq!(
            (acc.tail_bytes, acc.tail_text.len()),
            (20, 20),
            "one byte over the trigger trims back to maxRollingBytes, not to the trigger"
        );
        // The global counters are untouched by trimming — only the window shrinks.
        assert_eq!(acc.total_decoded_bytes, 41);
    }

    /// `trimTail`'s continuation-byte skip (TS `:187-189`), which the all-ASCII
    /// fixture never exercises. Without it the cut lands inside a 3-byte character
    /// and Pi's `buffer.subarray(start).toString("utf-8")` would mint a U+FFFD (this
    /// port would panic on a non-char-boundary slice).
    #[test]
    fn trim_tail_advances_the_cut_off_utf8_continuation_bytes() {
        // maxBytes 10 → trigger 40, cut back to 20. 14 × 3-byte chars = 42 bytes, so
        // the raw cut at 42 - 20 = 22 lands mid-character (22 % 3 == 1) and must be
        // advanced to 24.
        let cleanup = SpillGuard::default();
        let mut acc = OutputAccumulator::new(opts(1_000, 10));
        acc.append("世".repeat(14).as_bytes()).unwrap();
        cleanup.watch(&acc);
        assert_eq!(acc.tail_bytes, 18, "cut advanced from 22 to 24 of 42");
        assert_eq!(acc.tail_text, "世".repeat(6));
        // A mid-line cut also clears the line-boundary flag (TS `:191`).
        assert!(!acc.tail_starts_at_line_boundary);

        // Cutting exactly at a newline sets the flag instead: 20 lines of "a\n" is
        // 40 bytes (no trim), 21 lines is 42 → cut at 22, right after a `\n`.
        let cleanup = SpillGuard::default();
        let mut acc = OutputAccumulator::new(opts(1_000, 10));
        acc.append(&b"a\n".repeat(21)).unwrap();
        cleanup.watch(&acc);
        assert!(acc.tail_starts_at_line_boundary);
        assert_eq!(acc.tail_text, "a\n".repeat(10));
    }

    /// `snapshot({ persistIfTruncated: true })` (TS `:110-112`) is provably a no-op
    /// in Pi: `truncated` is `totalLines > maxLines || totalDecodedBytes > maxBytes`,
    /// a strict subset of `shouldUseTempFile()`'s three-way `||`, and both counters
    /// only ever change inside `append`/`finish` — which call `ensureTempFile()`
    /// themselves whenever that predicate holds. So a truncated snapshot always
    /// finds the spill file already open. This asserts the implication rather than a
    /// self-authored path value; the fixture's two `persistIfTruncated` rows note the
    /// same thing ("append() already spills").
    #[test]
    fn a_truncated_snapshot_always_already_has_a_spill_file() {
        for (max_lines, max_bytes, chunk) in [
            (2u64, 4096u64, "a\nb\nc\n"),
            (1_000, 4, "abcdefgh\n"),
            (1_000, 4, "no-newline-at-all"),
            (0, 4096, "x"),
        ] {
            let cleanup = SpillGuard::default();
            let mut acc = OutputAccumulator::new(opts(max_lines, max_bytes));
            acc.append(chunk.as_bytes()).unwrap();
            cleanup.watch(&acc);
            let before = acc.snapshot(SnapshotOptions::default());
            assert!(
                before.truncation.truncated,
                "{chunk:?} should be truncated at {max_lines}/{max_bytes}"
            );
            assert!(
                before.full_output_path.is_some(),
                "{chunk:?}: append() must have spilled before any persist request"
            );

            let after = acc.snapshot(SnapshotOptions {
                persist_if_truncated: true,
            });
            assert_eq!(after, before, "{chunk:?}: persisting changed the snapshot");
        }
    }

    /// `finish()`'s two remaining jobs (TS `:85-88`), neither of which the fixture
    /// can pin — its `split-multibyte` scenario reaches `finish()` with an empty
    /// decoder, and every other scenario has already spilled during `append`:
    /// 1. the decoder flush turns dangling bytes into one U+FFFD, which counts
    ///    towards `totalDecodedBytes` like any other text;
    /// 2. those bytes can be what pushes the accumulator over a limit, so `finish()`
    ///    re-checks `shouldUseTempFile()` and opens the spill file itself.
    #[tokio::test]
    async fn finish_flushes_the_decoder_and_can_spill_because_of_it() {
        // maxLines 1: `"a\n"` plus a dangling lead byte is one complete line and no
        // open one, so nothing has crossed a limit yet.
        let cleanup = SpillGuard::default();
        let mut acc = OutputAccumulator::new(opts(1, 4096));
        acc.append(b"a\n").unwrap();
        acc.append(&[0xe4]).unwrap();
        let before = acc.snapshot(SnapshotOptions::default());
        assert_eq!(before.content, "a\n", "the lead byte is still held back");
        assert_eq!(before.truncation.total_bytes, 2);
        assert_eq!(before.truncation.total_lines, 1);
        assert!(!before.truncation.truncated);
        assert!(
            before.full_output_path.is_none(),
            "nothing crossed a limit yet"
        );

        acc.finish();
        cleanup.watch(&acc);
        // The flush's U+FFFD is ordinary text: 3 bytes, and it OPENS a second line,
        // which is what crosses `maxLines`.
        assert_eq!(acc.get_last_line_bytes(), 3);
        let after = acc.snapshot(SnapshotOptions::default());
        assert_eq!(after.truncation.total_bytes, 5, "U+FFFD is 3 UTF-8 bytes");
        assert_eq!(after.truncation.total_lines, 2);
        assert!(after.truncation.truncated, "2 lines > maxLines 1");
        assert_eq!(after.truncation.truncated_by, Some(TruncatedBy::Lines));
        // The one-line tail window is the U+FFFD line itself.
        assert_eq!(after.content, "\u{FFFD}");
        let path = after
            .full_output_path
            .expect("finish() must open the spill file the flush made necessary");

        acc.close_temp_file().await.unwrap();
        // The RAW bytes are spilled — the substitution happens only in the decoded
        // view (TS `:74` writes `data`, never the decoded text).
        assert_eq!(std::fs::read(&path).unwrap(), [b'a', b'\n', 0xe4]);
        std::fs::remove_file(&path).unwrap();
    }

    /// The spill file must hold EVERY raw byte, in arrival order: the chunks
    /// buffered before the threshold tripped, then the chunk that tripped it, then
    /// everything after.
    #[tokio::test]
    async fn spill_file_holds_all_raw_bytes_in_order() {
        let cleanup = SpillGuard::default();
        let mut acc = OutputAccumulator::new(opts(3, 4096));
        for chunk in ["a\n", "b\n", "c\n", "d\n", "e\n"] {
            acc.append(chunk.as_bytes()).unwrap();
        }
        acc.finish();
        cleanup.watch(&acc);
        let path = acc
            .temp_file_path()
            .expect("spilled past maxLines")
            .to_path_buf();
        acc.close_temp_file().await.unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), b"a\nb\nc\nd\ne\n");
        // Shape of Pi's `${prefix}-${randomBytes(8).toString("hex")}.log`.
        let name = path.file_name().unwrap().to_str().unwrap();
        let id = name
            .strip_prefix("pirust-oa-unit-")
            .and_then(|rest| rest.strip_suffix(".log"))
            .expect("prefix-id.log");
        assert_eq!(id.len(), 16, "8 random bytes as hex");
        assert!(id
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));

        std::fs::remove_file(&path).unwrap();
    }

    /// Ids must differ per accumulator (Pi's are 8 random bytes).
    #[test]
    fn temp_file_paths_are_unique() {
        let mut paths = std::collections::HashSet::new();
        for _ in 0..64 {
            let cleanup = SpillGuard::default();
            let mut acc = OutputAccumulator::new(opts(0, 4096));
            acc.append(b"x\n").unwrap();
            cleanup.watch(&acc);
            let path = acc.temp_file_path().expect("spilled").to_path_buf();
            assert!(paths.insert(path.clone()), "duplicate temp path {path:?}");
        }
    }

    /// `closeTempFile` on an accumulator that never spilled resolves silently
    /// (TS `:122-124`), and is safe to call twice.
    #[tokio::test]
    async fn close_temp_file_without_a_spill_is_a_noop() {
        let mut acc = OutputAccumulator::new(opts(10, 4096));
        acc.append(b"small\n").unwrap();
        acc.finish();
        assert!(acc.temp_file_path().is_none());
        acc.close_temp_file().await.unwrap();
        acc.close_temp_file().await.unwrap();
    }
}
