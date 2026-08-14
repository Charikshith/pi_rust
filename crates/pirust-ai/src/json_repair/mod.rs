//! Lenient JSON repair + parsing — Rust port of `packages/ai/src/utils/json-parse.ts`.
//!
//! See `docs/analysis/06-anthropic-runtime-spec.md` §2. Pi has an exact 3-stage fallback:
//! `JSON.parse` → `repairJson`+parse → `partial-json` parse → `partial-json(repairJson)` →
//! `{}`. `repairJson` is a precise left-to-right character scanner (rules in the spec). The
//! `partial-json` dep (v0.1.7, `Allow.ALL`) tolerates truncated JSON; it has no drop-in Rust
//! crate, so a small tolerant parser matching its object/array/string/number/keyword-prefix
//! behavior is hand-rolled here (see [`PartialParser`]). `serde_json`'s `preserve_order` keeps
//! object key insertion order, matching JS object key order.
//!
//! Ported symbols:
//! - `repairJson` (`json-parse.ts:32-83`) → [`repair_json`]
//! - `parseJsonWithRepair` (`json-parse.ts:85-95`) → [`parse_json_with_repair`]
//! - `parseStreamingJson` (`json-parse.ts:104-124`) → [`parse_streaming_json`]
//! - `partial-json` `_parseJSON` w/ `Allow.ALL` (`node_modules/partial-json/dist/index.js`)
//!   → [`PartialParser`] (`partial_parse`)

use serde_json::{Map, Value};

/// Valid JSON escape follow-characters (TS `VALID_JSON_ESCAPES`, `json-parse.ts:3`).
const VALID_JSON_ESCAPES: &[char] = &['"', '\\', '/', 'b', 'f', 'n', 'r', 't', 'u'];

/// True for raw control characters, codepoint `0x00..=0x1f` (TS `isControlCharacter`, `:5-8`).
fn is_control_character(ch: char) -> bool {
    (ch as u32) <= 0x1f
}

/// Escape a raw control character (TS `escapeControlCharacter`, `:10-25`): `\b \f \n \r \t`
/// for those five, else `\uXXXX` zero-padded to four lowercase hex digits.
fn escape_control_character(ch: char) -> String {
    match ch {
        '\u{8}' => "\\b".to_string(), // backspace
        '\u{c}' => "\\f".to_string(), // form feed
        '\n' => "\\n".to_string(),
        '\r' => "\\r".to_string(),
        '\t' => "\\t".to_string(),
        _ => format!("\\u{:04x}", ch as u32),
    }
}

/// Repair a not-quite-valid JSON string via a single left-to-right scan (TS `repairJson`,
/// `json-parse.ts:32-83`): escapes lone/invalid backslashes and raw control characters,
/// preserves valid escapes and `\uXXXX` sequences.
pub fn repair_json(json: &str) -> String {
    let chars: Vec<char> = json.chars().collect();
    let mut repaired = String::new();
    let mut in_string = false;
    let mut index = 0usize;

    while index < chars.len() {
        let ch = chars[index];

        // Outside a string: copy verbatim; a quote opens string mode. (`:39-45`)
        if !in_string {
            repaired.push(ch);
            if ch == '"' {
                in_string = true;
            }
            index += 1;
            continue;
        }

        // Inside a string: a quote closes it. (`:47-51`)
        if ch == '"' {
            repaired.push(ch);
            in_string = false;
            index += 1;
            continue;
        }

        // Backslash handling. (`:53-77`)
        if ch == '\\' {
            match chars.get(index + 1) {
                // Trailing backslash at EOF → double it. (`:55-58`)
                None => {
                    repaired.push_str("\\\\");
                    index += 1;
                    continue;
                }
                Some(&next) => {
                    // `\uXXXX` with four hex digits → keep verbatim, advance past all six. (`:60-67`)
                    if next == 'u' && index + 6 <= chars.len() {
                        let digits = &chars[index + 2..index + 6];
                        if digits.iter().all(|d| d.is_ascii_hexdigit()) {
                            repaired.push('\\');
                            repaired.push('u');
                            for d in digits {
                                repaired.push(*d);
                            }
                            index += 6;
                            continue;
                        }
                    }

                    // Valid escape (`" \ / b f n r t u`) → keep the pair. (`:69-73`)
                    if VALID_JSON_ESCAPES.contains(&next) {
                        repaired.push('\\');
                        repaired.push(next);
                        index += 2;
                        continue;
                    }

                    // Invalid escape (e.g. `\H`) → double the lone backslash, do NOT
                    // consume the following char (it is reprocessed). (`:75`)
                    repaired.push_str("\\\\");
                    index += 1;
                    continue;
                }
            }
        }

        // Raw control char → escape it; any other char → copy verbatim. (`:79`)
        if is_control_character(ch) {
            repaired.push_str(&escape_control_character(ch));
        } else {
            repaired.push(ch);
        }
        index += 1;
    }

    repaired
}

/// Parse, falling back to a single `repair_json` attempt on failure (TS `parseJsonWithRepair`,
/// `:85-95`). Returns the parse error only when repair produced no change.
pub fn parse_json_with_repair(json: &str) -> Result<Value, serde_json::Error> {
    match serde_json::from_str::<Value>(json) {
        Ok(value) => Ok(value),
        Err(error) => {
            let repaired = repair_json(json);
            if repaired != json {
                // Return the repaired parse directly, propagating its own error if any.
                serde_json::from_str::<Value>(&repaired)
            } else {
                Err(error)
            }
        }
    }
}

/// Best-effort parse of a possibly-truncated JSON fragment, never failing — falls back
/// through repair and the tolerant partial parser to `{}` (TS `parseStreamingJson`,
/// `:104-124`). Used for the live per-delta tool-argument view.
pub fn parse_streaming_json(partial_json: &str) -> Value {
    // 1. Falsy / whitespace-only → `{}`. (`:105-107`)
    if partial_json.trim().is_empty() {
        return empty_object();
    }

    // 2. Strict-then-repair. (`:110`)
    if let Ok(value) = parse_json_with_repair(partial_json) {
        return value;
    }

    // 3. Tolerant partial parse of the raw input; `result ?? {}`. (`:112-114`)
    if let Ok(value) = partial_parse(partial_json) {
        return or_empty_object(value);
    }

    // 4. Tolerant partial parse of the repaired input; `result ?? {}`. (`:116-118`)
    if let Ok(value) = partial_parse(&repair_json(partial_json)) {
        return or_empty_object(value);
    }

    // 5. Give up → `{}`. (`:119-121`)
    empty_object()
}

fn empty_object() -> Value {
    Value::Object(Map::new())
}

/// Replicates JS `result ?? {}`: only `null`/`undefined` coalesce to `{}`; any other value
/// (including `false`, `0`, `""`) passes through.
fn or_empty_object(value: Value) -> Value {
    if value.is_null() {
        empty_object()
    } else {
        value
    }
}

/// Hand-rolled port of `partial-json` v0.1.7 `_parseJSON` with `Allow.ALL`
/// (`node_modules/partial-json/dist/index.js`). Tolerates truncation by returning the
/// structure accumulated so far. Errors are collapsed to `()` because, under `Allow.ALL`,
/// container parsing never rethrows — any sub-error just yields the partial container — so
/// the error kind (PartialJSON vs MalformedJSON) is irrelevant to control flow; a thrown
/// error only ever escapes at the top level, where the caller catches it.
struct PartialParser {
    chars: Vec<char>,
    length: usize,
    index: usize,
}

impl PartialParser {
    fn new(input: &str) -> Self {
        let chars: Vec<char> = input.chars().collect();
        let length = chars.len();
        Self {
            chars,
            length,
            index: 0,
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.index).copied()
    }

    fn remaining(&self) -> usize {
        self.length - self.index
    }

    fn remaining_str(&self) -> String {
        self.chars[self.index..].iter().collect()
    }

    /// `jsonString.substring(index, index + s.len()) === s` (clamped).
    fn full_match(&self, s: &str) -> bool {
        let pat: Vec<char> = s.chars().collect();
        if self.index + pat.len() > self.length {
            return false;
        }
        self.chars[self.index..self.index + pat.len()] == pat[..]
    }

    /// Keyword-prefix completion: `length - index < s.len() && s.startsWith(rest)`.
    fn prefix_partial(&self, s: &str) -> bool {
        self.remaining() < s.chars().count() && s.starts_with(&self.remaining_str())
    }

    fn last_index_of(&self, needle: char) -> Option<usize> {
        self.chars.iter().rposition(|&c| c == needle)
    }

    /// `skipBlank` (`index.js:212-216`): skip ` \n\r\t`.
    fn skip_blank(&mut self) {
        while self.index < self.length {
            match self.chars[self.index] {
                ' ' | '\n' | '\r' | '\t' => self.index += 1,
                _ => break,
            }
        }
    }

    /// `parseAny` (`index.js:55-90`) with `Allow.ALL`.
    fn parse_any(&mut self) -> Result<Value, ()> {
        self.skip_blank();
        if self.index >= self.length {
            return Err(()); // markPartialJSON("Unexpected end of input")
        }
        match self.chars[self.index] {
            '"' => self.parse_str(),
            '{' => self.parse_obj(),
            '[' => self.parse_arr(),
            _ => {
                // Keyword full-match or (Allow) prefix completion. (`:65-88`)
                if self.full_match("null") || self.prefix_partial("null") {
                    self.index += 4;
                    return Ok(Value::Null);
                }
                if self.full_match("true") || self.prefix_partial("true") {
                    self.index += 4;
                    return Ok(Value::Bool(true));
                }
                if self.full_match("false") || self.prefix_partial("false") {
                    self.index += 5;
                    return Ok(Value::Bool(false));
                }
                // Infinity / -Infinity / NaN are not representable as a serde_json number;
                // they never appear in tool-call JSON. Consume like JS and yield Null.
                if self.full_match("Infinity") || self.prefix_partial("Infinity") {
                    self.index += 8;
                    return Ok(Value::Null);
                }
                if self.full_match("-Infinity")
                    || (self.remaining() > 1
                        && self.remaining() < 9
                        && "-Infinity".starts_with(&self.remaining_str()))
                {
                    self.index += 9;
                    return Ok(Value::Null);
                }
                if self.full_match("NaN") || self.prefix_partial("NaN") {
                    self.index += 3;
                    return Ok(Value::Null);
                }
                self.parse_num()
            }
        }
    }

    /// `parseStr` (`index.js:91-117`) with `Allow.STR`.
    fn parse_str(&mut self) -> Result<Value, ()> {
        let start = self.index;
        let mut escape = false;
        self.index += 1; // skip initial quote

        while self.index < self.length
            && (self.chars[self.index] != '"' || (escape && self.chars[self.index - 1] == '\\'))
        {
            escape = if self.chars[self.index] == '\\' {
                !escape
            } else {
                false
            };
            self.index += 1;
        }

        if self.index < self.length && self.chars[self.index] == '"' {
            // Found the closing quote: substring(start, ++index - escape).
            self.index += 1;
            let end = self.index - usize::from(escape);
            let literal: String = self.chars[start..end].iter().collect();
            serde_json::from_str::<Value>(&literal).map_err(|_| ())
        } else {
            // Unterminated (Allow.STR): close it and retry; on invalid trailing escape,
            // cut back to the last backslash. (`:107-114`)
            let cut = self.index - usize::from(escape);
            let mut literal: String = self.chars[start..cut].iter().collect();
            literal.push('"');
            match serde_json::from_str::<Value>(&literal) {
                Ok(value) => Ok(value),
                Err(_) => {
                    // jsonString.lastIndexOf("\\") is a global search.
                    if let Some(bs) = self.last_index_of('\\') {
                        if bs >= start {
                            let mut fallback: String = self.chars[start..bs].iter().collect();
                            fallback.push('"');
                            return serde_json::from_str::<Value>(&fallback).map_err(|_| ());
                        }
                    }
                    Err(())
                }
            }
        }
    }

    /// `parseObj` (`index.js:118-153`) with `Allow.OBJ`.
    fn parse_obj(&mut self) -> Result<Value, ()> {
        self.index += 1; // skip initial brace
        self.skip_blank();
        let mut obj = Map::new();

        while self.peek() != Some('}') {
            self.skip_blank();
            if self.index >= self.length {
                return Ok(Value::Object(obj)); // Allow.OBJ
            }

            let key = match self.parse_str() {
                Ok(Value::String(key)) => key,
                // Any failure (or non-string key) → return the object so far. (`:145-149`)
                _ => return Ok(Value::Object(obj)),
            };

            self.skip_blank();
            self.index += 1; // skip colon

            match self.parse_any() {
                Ok(value) => {
                    obj.insert(key, value);
                }
                Err(()) => return Ok(Value::Object(obj)), // Allow.OBJ (`:134-138`)
            }

            self.skip_blank();
            if self.peek() == Some(',') {
                self.index += 1; // skip comma
            }
        }

        self.index += 1; // skip final brace
        Ok(Value::Object(obj))
    }

    /// `parseArr` (`index.js:154-174`) with `Allow.ARR`.
    fn parse_arr(&mut self) -> Result<Value, ()> {
        self.index += 1; // skip initial bracket
        let mut arr = Vec::new();

        while self.peek() != Some(']') {
            match self.parse_any() {
                Ok(value) => arr.push(value),
                Err(()) => return Ok(Value::Array(arr)), // Allow.ARR
            }
            self.skip_blank();
            if self.peek() == Some(',') {
                self.index += 1; // skip comma
            }
        }

        self.index += 1; // skip final bracket
        Ok(Value::Array(arr))
    }

    /// `parseNum` (`index.js:175-211`) with `Allow.NUM`.
    fn parse_num(&mut self) -> Result<Value, ()> {
        // Whole-string number at position 0. (`:176-190`)
        if self.index == 0 {
            let whole: String = self.chars.iter().collect();
            if whole == "-" {
                return Err(());
            }
            if let Ok(value) = serde_json::from_str::<Value>(&whole) {
                return Ok(value);
            }
            // NUM allowed: drop a dangling exponent.
            if let Some(e) = self.last_index_of('e') {
                let sub: String = self.chars[0..e].iter().collect();
                if let Ok(value) = serde_json::from_str::<Value>(&sub) {
                    return Ok(value);
                }
            }
            return Err(());
        }

        let start = self.index;
        if self.peek() == Some('-') {
            self.index += 1;
        }
        while let Some(c) = self.peek() {
            if c == ',' || c == ']' || c == '}' {
                break;
            }
            self.index += 1;
        }
        // NUM allowed → no partial-JSON error on running to EOF.

        let sub: String = self.chars[start..self.index].iter().collect();
        match serde_json::from_str::<Value>(&sub) {
            Ok(value) => Ok(value),
            Err(_) => {
                if sub == "-" {
                    return Err(());
                }
                // Drop a dangling exponent: substring(start, lastIndexOf("e")).
                if let Some(e) = self.last_index_of('e') {
                    if e > start && e <= self.length {
                        let sub2: String = self.chars[start..e].iter().collect();
                        if let Ok(value) = serde_json::from_str::<Value>(&sub2) {
                            return Ok(value);
                        }
                    }
                }
                Err(())
            }
        }
    }
}

/// Port of `partial-json` `parse(jsonString)` (default `Allow.ALL`): trims, rejects empty,
/// then runs the tolerant recursive descent. Errors collapse to `()`.
fn partial_parse(input: &str) -> Result<Value, ()> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(());
    }
    let mut parser = PartialParser::new(trimmed);
    parser.parse_any()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // The authentic oracle: malformed streamed tool JSON with a literal backslash-H (`\H`,
    // an invalid escape) and a raw TAB inside a string. Cross-checked against
    // `tests/fixtures/pi/anthropic/toolcall-repair.expected.json` → `final.content[0].arguments`.
    // In Rust source `"A\\H"` is the 3 chars A, backslash, H; `"col1\tcol2"` contains a real tab.
    const GOLDEN_INPUT: &str = "{\"path\":\"A\\H\",\"text\":\"col1\tcol2\"}";

    #[test]
    fn repair_json_golden_oracle() {
        // `\H` → `\\H`, raw tab → `\t`.
        assert_eq!(
            repair_json(GOLDEN_INPUT),
            "{\"path\":\"A\\\\H\",\"text\":\"col1\\tcol2\"}"
        );
    }

    #[test]
    fn parse_json_with_repair_golden_oracle() {
        let value = parse_json_with_repair(GOLDEN_INPUT).expect("repaired parse succeeds");
        assert_eq!(value, json!({ "path": "A\\H", "text": "col1\tcol2" }));
        // path is exactly the 3 chars A, backslash, H.
        assert_eq!(value["path"].as_str().unwrap(), "A\\H");
        assert_eq!(value["path"].as_str().unwrap().len(), 3);
        // text contains a real tab.
        assert_eq!(value["text"].as_str().unwrap(), "col1\tcol2");
    }

    #[test]
    fn parse_streaming_json_golden_oracle() {
        // Stage 2 (strict-then-repair) handles the golden case.
        assert_eq!(
            parse_streaming_json(GOLDEN_INPUT),
            json!({ "path": "A\\H", "text": "col1\tcol2" })
        );
    }

    #[test]
    fn strict_valid_json_passthrough() {
        let input = r#"{"a":1,"b":[true,null,"x"],"c":{"d":2.5}}"#;
        // repair_json leaves valid JSON untouched.
        assert_eq!(repair_json(input), input);
        let expected = json!({"a":1,"b":[true,null,"x"],"c":{"d":2.5}});
        assert_eq!(parse_json_with_repair(input).unwrap(), expected);
        assert_eq!(parse_streaming_json(input), expected);
    }

    #[test]
    fn preserves_valid_escapes_and_unicode() {
        // Valid escapes and a proper \uXXXX survive repair unchanged.
        let input = r#"{"s":"a\nb\t\"q\"\\zé"}"#;
        assert_eq!(repair_json(input), input);
        let value = parse_json_with_repair(input).unwrap();
        assert_eq!(value["s"].as_str().unwrap(), "a\nb\t\"q\"\\z\u{00e9}");
    }

    #[test]
    fn invalid_unicode_escape_left_intact() {
        // `\uZZZZ` is not four hex digits, but `u` is itself a valid escape char (`:69`), so
        // repair falls through to the valid-escape branch and leaves `\u` unchanged (Pi does
        // not double it). The result is still not valid JSON — repair only fixes what its
        // rules cover; parsing it fails, which is faithful to Pi.
        assert_eq!(repair_json(r#"{"s":"\uZZZZ"}"#), r#"{"s":"\uZZZZ"}"#);
    }

    #[test]
    fn trailing_backslash_at_eof_is_doubled() {
        // Trailing backslash inside a string with no following char → `\\`.
        assert_eq!(repair_json("\"abc\\"), "\"abc\\\\");
    }

    #[test]
    fn truncated_object_completes_via_streaming() {
        // Strict + repair both fail (unterminated string); the tolerant parser (stage 3)
        // returns the object accumulated so far, dropping the dangling `"te` key.
        assert_eq!(
            parse_streaming_json(r#"{"path":"/tmp/x","te"#),
            json!({ "path": "/tmp/x" })
        );
    }

    #[test]
    fn truncated_value_and_unterminated_string_complete() {
        // Object missing a value after the colon → keep prior keys.
        assert_eq!(parse_streaming_json(r#"{"a":1,"b":"#), json!({ "a": 1 }));
        // Unterminated string value gets closed.
        assert_eq!(parse_streaming_json(r#"{"a":"hel"#), json!({ "a": "hel" }));
    }

    #[test]
    fn truncated_array_completes() {
        assert_eq!(parse_streaming_json("[1,2,3"), json!([1, 2, 3]));
        // The second object was opened and its key `"b"` read, but no value followed, so
        // partial-json emits an empty `{}` for it.
        assert_eq!(
            parse_streaming_json(r#"[{"a":1},{"b"#),
            json!([{ "a": 1 }, {}])
        );
    }

    #[test]
    fn keyword_prefixes_complete() {
        assert_eq!(parse_streaming_json(r#"{"ok":tr"#), json!({ "ok": true }));
        assert_eq!(parse_streaming_json(r#"{"x":nu"#), json!({ "x": null }));
    }

    #[test]
    fn empty_and_whitespace_yield_empty_object() {
        assert_eq!(parse_streaming_json(""), json!({}));
        assert_eq!(parse_streaming_json("   \n\t "), json!({}));
    }

    #[test]
    fn parse_json_with_repair_unrepairable_errors() {
        // Garbage that repair cannot fix (repair leaves it unchanged) still errors.
        assert!(parse_json_with_repair("not json").is_err());
    }

    #[test]
    fn control_characters_are_escaped() {
        // A raw newline inside a string becomes `\n`.
        let input = "{\"s\":\"line1\nline2\"}";
        assert_eq!(repair_json(input), r#"{"s":"line1\nline2"}"#);
        assert_eq!(
            parse_json_with_repair(input).unwrap()["s"]
                .as_str()
                .unwrap(),
            "line1\nline2"
        );
    }
}
