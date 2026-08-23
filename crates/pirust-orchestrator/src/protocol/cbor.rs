//! Port of `packages/protocol/src/cbor/{encoder,decoder,options}.ts` — a
//! strict, definite-length RFC 8949 subset. See
//! `docs/analysis/04-orchestrator.md` §3/§6 for the full behavioral spec.
//!
//! JS has one runtime `number` type; this port mirrors that with a single
//! [`CborValue::Number`] variant (an `f64`) rather than separate int/float
//! variants, and classifies integer-vs-float **at encode time** exactly like
//! `encodeValue` does (`Number.isInteger(value) && !Object.is(value, -0)`).
//!
//! **Residual (documented, not silent):** `read_argument`'s 8-byte form computes
//! `high * UINT32_BASE + low` using exact `u64` arithmetic, whereas the real
//! TS performs that same multiply-add in native JS `number` (`f64`) space.
//! For the narrow band of raw wire arguments between `Number.MAX_SAFE_INTEGER`
//! and `read_argument`'s own `0x1fffff`-on-`high` ceiling (~9.007e15, only
//! reachable by an already-malformed/adversarial payload — legitimate data
//! never approaches this), the two implementations could theoretically round
//! differently. No case in the oracle corpus falls in that band (the
//! "unsafe positive integer" fixture uses a `high` that already exceeds the
//! ceiling and is rejected before this arithmetic runs), so this has no
//! observed behavioral effect — flagged for completeness, not a known bug.

use thiserror::Error;

pub const DEFAULT_MAX_CBOR_BYTE_LENGTH: u64 = 16 * 1024 * 1024;
pub const DEFAULT_MAX_CBOR_CONTAINER_LENGTH: u64 = 1_000_000;
pub const DEFAULT_MAX_CBOR_DEPTH: u32 = 64;

const UINT32_BASE: u64 = 0x1_0000_0000;
const MAX_UINT32: u64 = 0xffff_ffff;
/// `Number.MAX_SAFE_INTEGER` (2^53 - 1).
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

#[derive(Debug, Clone, Error)]
#[error("{0}")]
pub struct CborError(pub String);

impl CborError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

/// The protocol's JSON-compatible-plus-bytes value domain. A single `Number`
/// variant mirrors JS having one runtime number type (see module docs).
#[derive(Debug, Clone)]
pub enum CborValue {
    Null,
    Bool(bool),
    Number(f64),
    Bytes(Vec<u8>),
    Text(String),
    Array(Vec<CborValue>),
    /// Insertion-ordered, matching JS object key enumeration order — never a
    /// sorted map. String keys only (CBOR map keys are always UTF-8 text on
    /// this wire; a non-string decoded key is a decode error, not a variant).
    Map(Vec<(String, CborValue)>),
}

impl PartialEq for CborValue {
    /// Bitwise `f64` comparison (`to_bits`), not IEEE `==` — required so
    /// `-0.0 != 0.0` here the same way `Object.is(-0, 0)` is `false` in the
    /// real oracle's own round-trip assertions. No `CborValue` ever holds
    /// `NaN` (both encode and decode reject non-finite numbers), so bitwise
    /// comparison never trips on IEEE NaN-is-never-equal-to-itself either.
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (CborValue::Null, CborValue::Null) => true,
            (CborValue::Bool(a), CborValue::Bool(b)) => a == b,
            (CborValue::Number(a), CborValue::Number(b)) => a.to_bits() == b.to_bits(),
            (CborValue::Bytes(a), CborValue::Bytes(b)) => a == b,
            (CborValue::Text(a), CborValue::Text(b)) => a == b,
            (CborValue::Array(a), CborValue::Array(b)) => a == b,
            (CborValue::Map(a), CborValue::Map(b)) => a == b,
            _ => false,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct CborOptions {
    pub max_byte_length: u64,
    pub max_container_length: u64,
    pub max_depth: u32,
}

impl Default for CborOptions {
    fn default() -> Self {
        Self {
            max_byte_length: DEFAULT_MAX_CBOR_BYTE_LENGTH,
            max_container_length: DEFAULT_MAX_CBOR_CONTAINER_LENGTH,
            max_depth: DEFAULT_MAX_CBOR_DEPTH,
        }
    }
}

fn is_safe_integer(n: f64) -> bool {
    n.is_finite() && n == n.trunc() && n.abs() <= MAX_SAFE_INTEGER
}

// ============================================================================
// Encode
// ============================================================================

struct CborWriter {
    buffer: Vec<u8>,
    max_byte_length: u64,
}

impl CborWriter {
    fn new(max_byte_length: u64) -> Self {
        Self {
            buffer: Vec::new(),
            max_byte_length,
        }
    }

    fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), CborError> {
        if self.buffer.len() as u64 + bytes.len() as u64 > self.max_byte_length {
            return Err(CborError::new(format!(
                "CBOR byte length exceeds configured limit of {}",
                self.max_byte_length
            )));
        }
        self.buffer.extend_from_slice(bytes);
        Ok(())
    }

    fn write_byte(&mut self, byte: u8) -> Result<(), CborError> {
        self.write_bytes(&[byte])
    }
}

fn write_argument(writer: &mut CborWriter, major_type: u8, value: u64) -> Result<(), CborError> {
    let prefix = major_type << 5;
    if value < 24 {
        writer.write_byte(prefix | value as u8)
    } else if value <= 0xff {
        writer.write_byte(prefix | 24)?;
        writer.write_byte(value as u8)
    } else if value <= 0xffff {
        writer.write_byte(prefix | 25)?;
        writer.write_bytes(&(value as u16).to_be_bytes())
    } else if value <= MAX_UINT32 {
        writer.write_byte(prefix | 26)?;
        writer.write_bytes(&(value as u32).to_be_bytes())
    } else {
        writer.write_byte(prefix | 27)?;
        writer.write_bytes(&value.to_be_bytes())
    }
}

fn encode_text(
    writer: &mut CborWriter,
    value: &str,
    options: &CborOptions,
) -> Result<(), CborError> {
    // A Rust `str` is always valid UTF-8 by construction — TS's lossy-string
    // round-trip check (`textDecoder.decode(bytes) !== value`, guarding
    // against lone surrogates) is type-system-moot here, not silently
    // dropped: there is no `CborValue::Text` value it could ever fire on.
    let bytes = value.as_bytes();
    if bytes.len() as u64 > options.max_byte_length {
        return Err(CborError::new(format!(
            "CBOR text string length exceeds configured limit of {}",
            options.max_byte_length
        )));
    }
    write_argument(writer, 3, bytes.len() as u64)?;
    writer.write_bytes(bytes)
}

fn encode_value(
    writer: &mut CborWriter,
    value: &CborValue,
    options: &CborOptions,
    depth: u32,
) -> Result<(), CborError> {
    if depth > options.max_depth {
        return Err(CborError::new(format!(
            "CBOR nesting depth exceeds configured limit of {}",
            options.max_depth
        )));
    }
    match value {
        CborValue::Null => writer.write_byte(0xf6),
        CborValue::Bool(b) => writer.write_byte(if *b { 0xf5 } else { 0xf4 }),
        CborValue::Number(n) => encode_number(writer, *n),
        CborValue::Text(s) => encode_text(writer, s, options),
        CborValue::Bytes(bytes) => {
            if bytes.len() as u64 > options.max_byte_length {
                return Err(CborError::new(format!(
                    "CBOR byte string length exceeds configured limit of {}",
                    options.max_byte_length
                )));
            }
            write_argument(writer, 2, bytes.len() as u64)?;
            writer.write_bytes(bytes)
        }
        CborValue::Array(items) => {
            if items.len() as u64 > options.max_container_length {
                return Err(CborError::new(format!(
                    "CBOR array length exceeds configured limit of {}",
                    options.max_container_length
                )));
            }
            write_argument(writer, 4, items.len() as u64)?;
            for item in items {
                encode_value(writer, item, options, depth + 1)?;
            }
            Ok(())
        }
        CborValue::Map(entries) => {
            if entries.len() as u64 > options.max_container_length {
                return Err(CborError::new(format!(
                    "CBOR map length exceeds configured limit of {}",
                    options.max_container_length
                )));
            }
            write_argument(writer, 5, entries.len() as u64)?;
            for (key, entry_value) in entries {
                encode_text(writer, key, options)?;
                encode_value(writer, entry_value, options, depth + 1)?;
            }
            Ok(())
        }
    }
}

/// `Number.isInteger(value) && !Object.is(value, -0)` -> integer form (with
/// the safe-integer bound); everything else (genuine fractions, `-0`) ->
/// the 9-byte float64 form. Mirrors `encodeValue`'s number branch exactly.
fn encode_number(writer: &mut CborWriter, n: f64) -> Result<(), CborError> {
    if !n.is_finite() {
        return Err(CborError::new("CBOR numbers must be finite"));
    }
    let is_negative_zero = n == 0.0 && n.is_sign_negative();
    if n == n.trunc() && !is_negative_zero {
        if !is_safe_integer(n) {
            return Err(CborError::new(
                "CBOR integers must be safe JavaScript integers",
            ));
        }
        if n >= 0.0 {
            write_argument(writer, 0, n as u64)
        } else {
            write_argument(writer, 1, (-1.0 - n) as u64)
        }
    } else {
        writer.write_byte(0xfb)?;
        writer.write_bytes(&n.to_be_bytes())
    }
}

/// Encodes the protocol's strict, definite-length RFC 8949 subset.
pub fn encode_cbor(value: &CborValue) -> Result<Vec<u8>, CborError> {
    encode_cbor_with(value, &CborOptions::default())
}

pub fn encode_cbor_with(value: &CborValue, options: &CborOptions) -> Result<Vec<u8>, CborError> {
    let mut writer = CborWriter::new(options.max_byte_length);
    encode_value(&mut writer, value, options, 0)?;
    Ok(writer.buffer)
}

// ============================================================================
// Decode
// ============================================================================

struct CborReader<'a> {
    bytes: &'a [u8],
    offset: usize,
    options: CborOptions,
}

impl<'a> CborReader<'a> {
    fn decode(mut self) -> Result<CborValue, CborError> {
        let value = self.read_item(0)?;
        if self.offset != self.bytes.len() {
            return Err(CborError::new("CBOR payload contains trailing data"));
        }
        Ok(value)
    }

    fn read_item(&mut self, depth: u32) -> Result<CborValue, CborError> {
        if depth > self.options.max_depth {
            return Err(CborError::new(format!(
                "CBOR nesting depth exceeds configured limit of {}",
                self.options.max_depth
            )));
        }
        let initial = self.read_byte()?;
        let major_type = initial >> 5;
        let additional_information = initial & 0x1f;

        match major_type {
            0 => Ok(CborValue::Number(
                self.read_argument(additional_information)? as f64,
            )),
            1 => {
                let value = -1.0 - self.read_argument(additional_information)? as f64;
                if !is_safe_integer(value) {
                    return Err(CborError::new(
                        "Decoded CBOR integer is outside the safe range",
                    ));
                }
                Ok(CborValue::Number(value))
            }
            2 => {
                let length = self.read_length(
                    additional_information,
                    "byte string",
                    self.options.max_byte_length,
                )?;
                Ok(CborValue::Bytes(self.read_bytes(length as usize)?.to_vec()))
            }
            3 => {
                let length = self.read_length(
                    additional_information,
                    "text string",
                    self.options.max_byte_length,
                )?;
                let bytes = self.read_bytes(length as usize)?;
                let text = std::str::from_utf8(bytes)
                    .map_err(|_| CborError::new("CBOR text string contains invalid UTF-8"))?;
                Ok(CborValue::Text(text.to_string()))
            }
            4 => {
                let length = self.read_length(
                    additional_information,
                    "array",
                    self.options.max_container_length,
                )?;
                let mut items = Vec::new();
                for _ in 0..length {
                    items.push(self.read_item(depth + 1)?);
                }
                Ok(CborValue::Array(items))
            }
            5 => {
                let length = self.read_length(
                    additional_information,
                    "map",
                    self.options.max_container_length,
                )?;
                let mut entries: Vec<(String, CborValue)> = Vec::new();
                for _ in 0..length {
                    let key = match self.read_item(depth + 1)? {
                        CborValue::Text(text) => text,
                        _ => return Err(CborError::new("CBOR map keys must be strings")),
                    };
                    if entries.iter().any(|(existing, _)| existing == &key) {
                        return Err(CborError::new("CBOR map contains a duplicate key"));
                    }
                    let value = self.read_item(depth + 1)?;
                    entries.push((key, value));
                }
                Ok(CborValue::Map(entries))
            }
            6 => Err(CborError::new("CBOR tags are not supported")),
            7 => self.read_simple(additional_information),
            _ => Err(CborError::new("Malformed CBOR major type")),
        }
    }

    fn read_simple(&mut self, additional_information: u8) -> Result<CborValue, CborError> {
        match additional_information {
            20 => Ok(CborValue::Bool(false)),
            21 => Ok(CborValue::Bool(true)),
            22 => Ok(CborValue::Null),
            27 => {
                let bytes = self.read_bytes(8)?;
                let value =
                    f64::from_be_bytes(bytes.try_into().expect("read_bytes(8) returns 8 bytes"));
                if !value.is_finite() {
                    return Err(CborError::new("Decoded CBOR number must be finite"));
                }
                if value == value.trunc() && !is_safe_integer(value) {
                    return Err(CborError::new(
                        "Decoded CBOR integer is outside the safe range",
                    ));
                }
                Ok(CborValue::Number(value))
            }
            31 => Err(CborError::new("CBOR break marker is not supported")),
            _ => Err(CborError::new(
                "Unsupported CBOR simple value or floating-point width",
            )),
        }
    }

    fn read_length(
        &mut self,
        additional_information: u8,
        kind: &str,
        limit: u64,
    ) -> Result<u64, CborError> {
        if additional_information == 31 {
            return Err(CborError::new(format!(
                "Indefinite-length CBOR {kind}s are not supported"
            )));
        }
        let length = self.read_argument(additional_information)?;
        if length > limit {
            return Err(CborError::new(format!(
                "CBOR {kind} length exceeds configured limit of {limit}"
            )));
        }
        Ok(length)
    }

    fn read_argument(&mut self, additional_information: u8) -> Result<u64, CborError> {
        if additional_information < 24 {
            return Ok(additional_information as u64);
        }
        match additional_information {
            24 => Ok(self.read_byte()? as u64),
            25 => {
                let bytes = self.read_bytes(2)?;
                Ok(bytes[0] as u64 * 0x100 + bytes[1] as u64)
            }
            26 => {
                let bytes = self.read_bytes(4)?;
                Ok(bytes[0] as u64 * 0x1_000_000
                    + bytes[1] as u64 * 0x1_0000
                    + bytes[2] as u64 * 0x100
                    + bytes[3] as u64)
            }
            27 => {
                let high = self.read_argument(26)?;
                let low = self.read_argument(26)?;
                if high > 0x1f_ffff {
                    return Err(CborError::new(
                        "Decoded CBOR integer or length is outside the safe range",
                    ));
                }
                Ok(high * UINT32_BASE + low)
            }
            31 => Err(CborError::new(
                "Indefinite-length CBOR items are not supported",
            )),
            _ => Err(CborError::new("Malformed CBOR additional information")),
        }
    }

    fn read_byte(&mut self) -> Result<u8, CborError> {
        let byte = *self
            .bytes
            .get(self.offset)
            .ok_or_else(|| CborError::new("Truncated CBOR payload"))?;
        self.offset += 1;
        Ok(byte)
    }

    fn read_bytes(&mut self, length: usize) -> Result<&'a [u8], CborError> {
        let end = self
            .offset
            .checked_add(length)
            .filter(|&end| end <= self.bytes.len())
            .ok_or_else(|| CborError::new("Truncated CBOR payload"))?;
        let slice = &self.bytes[self.offset..end];
        self.offset = end;
        Ok(slice)
    }
}

/// Decodes exactly one item from the protocol's strict RFC 8949 subset.
pub fn decode_cbor(bytes: &[u8]) -> Result<CborValue, CborError> {
    decode_cbor_with(bytes, &CborOptions::default())
}

pub fn decode_cbor_with(bytes: &[u8], options: &CborOptions) -> Result<CborValue, CborError> {
    if bytes.len() as u64 > options.max_byte_length {
        return Err(CborError::new(format!(
            "CBOR byte length exceeds configured limit of {}",
            options.max_byte_length
        )));
    }
    CborReader {
        bytes,
        offset: 0,
        options: *options,
    }
    .decode()
}
