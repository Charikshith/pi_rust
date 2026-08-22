//! Strict LF-only JSONL framing — port of `modes/rpc/jsonl.ts` (58 lines).
//!
//! Framing is LF-only. Payload strings may contain other Unicode separators
//! such as U+2028 and U+2029, so a line reader must split on `\n` only (Node's
//! readline splits on additional Unicode separators and is intentionally NOT
//! used by Pi; see jsonl.ts:4-9). A trailing `\r` on a line is stripped
//! (jsonl.ts:26) so CRLF input is tolerated.

use std::io::Read;

/// Serialize a single strict JSONL record: compact JSON + `\n`
/// (jsonl.ts:10-12).
pub fn serialize_json_line(value: &impl serde::Serialize) -> String {
    // serde_json compact == JSON.stringify for the values this protocol emits
    // (byte-compat verified by the golden suites in pirust-ai / pirust-agent-core).
    let mut out = serde_json::to_string(value).expect("RPC JSONL value must serialize");
    out.push('\n');
    out
}

/// Incremental UTF-8 line splitter — the analogue of `attachJsonlLineReader`'s
/// `StringDecoder("utf8")` + `\n` scan (jsonl.ts:21-57).
///
/// Feed arbitrary byte chunks; complete lines are returned per call. Incomplete
/// multi-byte sequences at a chunk boundary are buffered exactly like
/// `StringDecoder.write`, so a code point split across chunks is handled.
#[derive(Default)]
pub struct JsonLineSplitter {
    bytes: Vec<u8>,
}

impl JsonLineSplitter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Push one raw chunk; returns every COMPLETE line (without the `\n`),
    /// each with any trailing `\r` already stripped.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<String> {
        self.bytes.extend_from_slice(chunk);
        let mut lines = Vec::new();
        while let Some(idx) = self.bytes.iter().position(|&b| b == b'\n') {
            let mut line: Vec<u8> = self.bytes.drain(..=idx).collect();
            line.pop(); // '\n'
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            lines.push(String::from_utf8_lossy(&line).into_owned());
        }
        lines
    }

    /// Flush at end-of-stream: emit a final unterminated line if any remains
    /// (jsonl.ts:43-49 — `decoder.end()` then emit the remainder).
    pub fn finish(&mut self) -> Option<String> {
        if self.bytes.is_empty() {
            return None;
        }
        let line = std::mem::take(&mut self.bytes);
        let mut line = line;
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        Some(String::from_utf8_lossy(&line).into_owned())
    }
}

/// Read a stream to EOF, invoking `on_line` per complete line — the blocking
/// analogue of [`crate::rpc`] `attachJsonlLineReader`. Returns once the reader
/// is exhausted (the final partial line, if any, is emitted first).
pub fn read_json_lines<R: Read>(
    mut reader: R,
    mut on_line: impl FnMut(String),
) -> std::io::Result<()> {
    let mut splitter = JsonLineSplitter::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        for line in splitter.push(&buf[..n]) {
            on_line(line);
        }
    }
    if let Some(line) = splitter.finish() {
        on_line(line);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialize_appends_lf_only() {
        assert_eq!(
            serialize_json_line(&serde_json::json!({"a":1})),
            "{\"a\":1}\n"
        );
    }

    #[test]
    fn u2028_inside_string_is_not_a_separator() {
        // jsonl.ts:4-9 — U+2028/U+2029 are valid inside JSON strings; framing
        // must NOT split there (readline would).
        let v = serde_json::json!({"text": "a\u{2028}b\u{2029}c"});
        let line = serialize_json_line(&v);
        let mut s = JsonLineSplitter::new();
        assert_eq!(s.push(line.as_bytes()).len(), 1);
        assert_eq!(s.finish(), None);
    }

    #[test]
    fn crlf_is_stripped() {
        let mut s = JsonLineSplitter::new();
        assert_eq!(
            s.push(b"{\"a\":1}\r\n{\"a\":2}\n"),
            vec!["{\"a\":1}", "{\"a\":2}"]
        );
    }

    #[test]
    fn utf8_code_point_split_across_chunks() {
        // "é" is 2 bytes; split between them — StringDecoder semantics.
        let mut s = JsonLineSplitter::new();
        assert_eq!(s.push(b"{\"t\":\"\xc3"), Vec::<String>::new());
        assert_eq!(s.push(b"\xa9\"}\n"), vec![format!("{{\"t\":\"{}\"}}", 'é')]);
    }

    #[test]
    fn final_unterminated_line_flushes() {
        let mut s = JsonLineSplitter::new();
        assert_eq!(s.push(b"abc"), Vec::<String>::new());
        assert_eq!(s.finish().as_deref(), Some("abc"));
        assert_eq!(s.finish(), None);
    }

    #[test]
    fn empty_lines_are_emitted() {
        // Two adjacent \n means an EMPTY command line — Pi feeds it to
        // JSON.parse("") and answers with a parse error; the reader itself
        // must surface the empty line (jsonl.ts:38 slices before the \n).
        let mut s = JsonLineSplitter::new();
        assert_eq!(s.push(b"a\n\nb\n"), vec!["a", "", "b"]);
    }

    #[test]
    fn read_json_lines_drains_reader() {
        let input = "one\ntwo\r\nthree";
        let mut seen = Vec::new();
        read_json_lines(input.as_bytes(), |l| seen.push(l)).unwrap();
        assert_eq!(seen, vec!["one", "two", "three"]);
    }
}
