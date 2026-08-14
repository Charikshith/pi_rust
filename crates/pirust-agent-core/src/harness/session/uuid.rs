//! UUIDv7 with injectable clock/randomness — byte-exact port of
//! `packages/agent/src/harness/session/uuid.ts`.
//!
//! Spec: `docs/analysis/07-agent-core-spec.md` §7 (exact 16-byte layout +
//! monotonic counter). `[LEAF, standalone]` (§13, wave 0).
//!
//! Pi injects NOTHING — it reads `globalThis.crypto`/`Math.random` and
//! `Date.now()` from module globals, and keeps `lastTimestamp`/`sequence` as
//! module-level state. This port keeps the byte layout identical but adds an
//! injection seam ([`Uuidv7Source`]) so the layout can be asserted against the
//! authentic Pi vectors in `tests/fixtures/pi/agent/uuidv7-vectors.json`.
//!
//! The convenience [`uuidv7`] / [`create_session_id`] / [`generate_entry_id`]
//! functions drive a single process-global generator over [`SystemSource`],
//! matching Pi's global-state semantics (the same monotonic counter is shared
//! across all calls).

use std::fmt::Write as _;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// Injectable clock + randomness source for deterministic UUIDv7 tests (§7).
///
/// Mirrors the two module globals Pi reads directly: `Date.now()` and
/// `crypto.getRandomValues`.
pub trait Uuidv7Source {
    /// Milliseconds since the Unix epoch (the v7 timestamp field, `Date.now()`).
    fn now_ms(&self) -> u64;
    /// Fills the 16-byte entropy buffer (`crypto.getRandomValues`).
    fn fill_random(&self, buf: &mut [u8; 16]);
}

/// Default source: system clock + OS CSPRNG (via `getrandom`).
///
/// `getrandom::fill` is the Rust analogue of `crypto.getRandomValues`; the
/// `Math.random()` fallback in `uuid.ts:10-12` is intentionally not ported —
/// the OS entropy source is always available here.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemSource;

impl Uuidv7Source for SystemSource {
    fn now_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }

    fn fill_random(&self, buf: &mut [u8; 16]) {
        getrandom::fill(buf).expect("getrandom: system entropy source unavailable");
    }
}

/// Stateful monotonic UUIDv7 generator (`lastTimestamp` + `sequence` from
/// `uuid.ts:1-2`) over an injectable [`Uuidv7Source`].
#[derive(Debug)]
pub struct Uuidv7Generator<S: Uuidv7Source> {
    source: S,
    /// `lastTimestamp` (ms). `None` is the `-Infinity` initial value: any
    /// observed timestamp compares greater, so the first call takes the
    /// fresh-seed branch.
    last_timestamp: Option<u64>,
    /// `sequence` — a JS uint32; `wrapping_add` reproduces `(x + 1) >>> 0`.
    sequence: u32,
}

impl<S: Uuidv7Source> Uuidv7Generator<S> {
    /// Creates a generator with `lastTimestamp = -Infinity`, `sequence = 0`.
    pub const fn with_source(source: S) -> Self {
        Self {
            source,
            last_timestamp: None,
            sequence: 0,
        }
    }

    /// Generates the next UUIDv7 string, advancing the monotonic state.
    ///
    /// Byte-exact port of `uuidv7()` (`uuid.ts:15-49`).
    pub fn generate(&mut self) -> String {
        let mut random = [0u8; 16];
        self.source.fill_random(&mut random);
        let timestamp = self.source.now_ms();

        // `timestamp > lastTimestamp` (None == -Infinity => always fresh).
        let fresh = self.last_timestamp.is_none_or(|last| timestamp > last);
        if fresh {
            // sequence = random[6]*2^24 + random[7]*2^16 + random[8]*2^8 + random[9]
            self.sequence = (u32::from(random[6]) << 24)
                | (u32::from(random[7]) << 16)
                | (u32::from(random[8]) << 8)
                | u32::from(random[9]);
            self.last_timestamp = Some(timestamp);
        } else {
            // sequence = (sequence + 1) >>> 0; if it wrapped to 0, lastTimestamp++.
            self.sequence = self.sequence.wrapping_add(1);
            if self.sequence == 0 {
                let bumped = self.last_timestamp.unwrap_or(0).wrapping_add(1);
                self.last_timestamp = Some(bumped);
            }
        }

        let ts = self.last_timestamp.unwrap_or(0);
        let seq = self.sequence;

        let mut bytes = [0u8; 16];
        // 48-bit lastTimestamp, big-endian (`ts / 2^N & 0xff`; equivalent to `ts >> 8*(5-i)`).
        bytes[0] = (ts >> 40) as u8;
        bytes[1] = (ts >> 32) as u8;
        bytes[2] = (ts >> 24) as u8;
        bytes[3] = (ts >> 16) as u8;
        bytes[4] = (ts >> 8) as u8;
        bytes[5] = ts as u8;
        // version 7 nibble + sequence high bits.
        bytes[6] = 0x70 | (((seq >> 28) & 0x0f) as u8);
        bytes[7] = ((seq >> 20) & 0xff) as u8;
        // variant (0b10) + sequence bits.
        bytes[8] = 0x80 | (((seq >> 14) & 0x3f) as u8);
        bytes[9] = ((seq >> 6) & 0xff) as u8;
        bytes[10] = (((seq & 0x3f) << 2) as u8) | (random[10] & 0x03);
        bytes[11] = random[11];
        bytes[12] = random[12];
        bytes[13] = random[13];
        bytes[14] = random[14];
        bytes[15] = random[15];

        format_uuid(&bytes)
    }
}

/// Formats 16 bytes as lowercase hex with dashes at `8-4-4-4-12`
/// (`formatUuid`, `uuid.ts:51-54`).
fn format_uuid(bytes: &[u8; 16]) -> String {
    let mut out = String::with_capacity(36);
    for (i, byte) in bytes.iter().enumerate() {
        if matches!(i, 4 | 6 | 8 | 10) {
            out.push('-');
        }
        // Infallible: writing to a String never errors.
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// Process-global generator over [`SystemSource`], mirroring Pi's module-level
/// `lastTimestamp`/`sequence` globals (one shared monotonic counter).
static SYSTEM_GENERATOR: Mutex<Uuidv7Generator<SystemSource>> =
    Mutex::new(Uuidv7Generator::with_source(SystemSource));

/// Generates a UUIDv7 string using the shared system generator (`uuidv7()`).
pub fn uuidv7() -> String {
    SYSTEM_GENERATOR
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .generate()
}

/// Full session id (`createSessionId`, `repo-utils.ts:12-14`) = full uuidv7.
pub fn create_session_id() -> String {
    uuidv7()
}

/// Short entry id (`generateEntryId`, `jsonl-storage.ts:36-44` /
/// `memory-storage.ts:28-36`) = `uuidv7().slice(-8)` (last 8 hex chars = the
/// pure-random tail, bytes 12..15).
///
/// Pi wraps this in a ≤100-attempt collision-retry loop that falls back to the
/// full uuid; that retry is owned by the storage layer (which knows existing
/// ids), so only the `slice(-8)` derivation lives here.
pub fn generate_entry_id() -> String {
    let id = uuidv7();
    // `id` is 36 ASCII bytes, so byte-slicing the last 8 chars is safe.
    id[id.len() - 8..].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic source that replays one fixed `(now_ms, 16 bytes)` vector.
    struct FixedSource {
        now_ms: u64,
        bytes: [u8; 16],
    }

    impl Uuidv7Source for FixedSource {
        fn now_ms(&self) -> u64 {
            self.now_ms
        }
        fn fill_random(&self, buf: &mut [u8; 16]) {
            *buf = self.bytes;
        }
    }

    #[derive(serde::Deserialize)]
    struct Vector {
        label: String,
        #[serde(rename = "nowMs")]
        now_ms: u64,
        #[serde(rename = "randomBytes")]
        random_bytes: Vec<u8>,
        id: String,
        #[serde(rename = "shortId")]
        short_id: String,
    }

    #[derive(serde::Deserialize)]
    struct Vectors {
        vectors: Vec<Vector>,
    }

    fn load_vectors() -> Vec<Vector> {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/pi/agent/uuidv7-vectors.json"
        );
        let raw = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {path}: {e}"));
        serde_json::from_str::<Vectors>(&raw)
            .expect("parse uuidv7-vectors.json")
            .vectors
    }

    /// Byte-exact acceptance gate: drive every authentic Pi vector through ONE
    /// generator instance in array order, so the shared `lastTimestamp` /
    /// `sequence` state carries — the `monotonic-same-now` vector (reusing
    /// `nowMs=1000`) must therefore take the `sequence + 1` branch.
    #[test]
    fn matches_pi_vectors_byte_for_byte() {
        let vectors = load_vectors();
        assert!(vectors.len() >= 2, "need the monotonic pair");

        // A single shared generator == Pi's module globals.
        let mut generator: Option<Uuidv7Generator<FixedSource>> = None;
        for v in &vectors {
            let bytes: [u8; 16] = v
                .random_bytes
                .as_slice()
                .try_into()
                .expect("randomBytes must be 16 long");
            let source = FixedSource {
                now_ms: v.now_ms,
                bytes,
            };
            // Swap in this vector's source while carrying the counter state.
            let mut generator_state = match generator.take() {
                Some(prev) => Uuidv7Generator {
                    source,
                    last_timestamp: prev.last_timestamp,
                    sequence: prev.sequence,
                },
                None => Uuidv7Generator::with_source(source),
            };

            let id = generator_state.generate();
            assert_eq!(id, v.id, "id mismatch for vector '{}'", v.label);
            assert_eq!(
                &id[id.len() - 8..],
                v.short_id,
                "shortId mismatch for vector '{}'",
                v.label
            );
            generator = Some(generator_state);
        }
    }

    /// Explicitly exercise the monotonic (`sequence + 1`) path through one
    /// instance: two calls with the same `now_ms` must differ only in the
    /// sequence-derived / random-tail bytes, and the second must increment.
    #[test]
    fn monotonic_same_now_increments_sequence() {
        let mut gen = Uuidv7Generator::with_source(FixedSource {
            now_ms: 1000,
            bytes: [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
        });
        let first = gen.generate();
        let seq_after_first = gen.sequence;
        // Same timestamp -> takes the (sequence + 1) >>> 0 branch.
        gen.source.bytes = [
            16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31,
        ];
        let second = gen.generate();
        assert_eq!(gen.sequence, seq_after_first.wrapping_add(1));
        assert_ne!(first, second);
        // Same 48-bit timestamp prefix (bytes 0..5 => first two hex groups).
        assert_eq!(&first[..13], &second[..13]);
    }

    /// Version nibble (byte 6 high) == 7 and variant (byte 8 top bits) == 0b10.
    #[test]
    fn version_and_variant_bits() {
        let mut gen = Uuidv7Generator::with_source(FixedSource {
            now_ms: 1_765_233_665_292,
            bytes: [0xAA; 16],
        });
        let id = gen.generate();
        let hex: String = id.chars().filter(|c| *c != '-').collect();
        let byte6 = u8::from_str_radix(&hex[12..14], 16).unwrap();
        let byte8 = u8::from_str_radix(&hex[16..18], 16).unwrap();
        assert_eq!(byte6 >> 4, 0x7, "version nibble must be 7");
        assert_eq!(byte8 >> 6, 0b10, "variant top bits must be 0b10");
    }

    /// `generate_entry_id` is the last 8 hex chars of a full uuidv7.
    #[test]
    fn entry_id_is_short_tail() {
        let id = create_session_id();
        assert_eq!(id.len(), 36);
        let entry = generate_entry_id();
        assert_eq!(entry.len(), 8);
        assert!(entry.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
