//! JavaScript-compatible number formatting.
//!
//! serde_json (via ryu) serializes `f64` in the shortest form, which for small/large
//! magnitudes uses scientific notation earlier than JavaScript does — e.g. `0.000005`
//! becomes `5e-6`. Pi produces its JSON with `JSON.stringify`, whose numbers follow the
//! ECMAScript `Number::toString` rules. For byte-identical output (verified against
//! real Pi fixtures, feat-011) any `f64` that appears on the wire must be formatted with
//! [`js_number`] via the [`serialize_f64`] `serialize_with` hook.
//!
//! Deserialization is unaffected — fields stay `f64` and parse normally.

use serde::Serializer;
use serde_json::value::RawValue;

/// Formats `v` exactly as ECMAScript `Number.prototype.toString` (base 10) would.
///
/// Implements the spec's fixed-vs-exponential thresholds: fixed notation for
/// `1e-6 <= |v| < 1e21`, scientific otherwise. Rust's `{:e}` gives the shortest
/// significant digits; this reconstructs the ECMAScript spacing/formatting from them.
pub fn js_number(v: f64) -> String {
    // JSON has no NaN/Infinity; callers only pass finite values. Be defensive anyway.
    if !v.is_finite() {
        return "null".to_string();
    }
    if v == 0.0 {
        return "0".to_string(); // covers -0.0 (-0.0 == 0.0)
    }

    let neg = v < 0.0;
    let a = v.abs();

    // Shortest scientific form, e.g. "5e-6", "7.902124999999999e-2", "1e0".
    let sci = format!("{a:e}");
    let (mant, exp_str) = sci.split_once('e').expect("scientific form contains 'e'");
    let exp: i32 = exp_str.parse().expect("valid exponent");
    let digits: String = mant.chars().filter(|c| *c != '.').collect();
    let k = digits.len() as i32; // number of significant digits
                                 // ECMAScript: value = s × 10^(n-k); Rust's mantissa has one digit before '.', so
                                 // value = digits × 10^(exp-(k-1)) ⇒ n = exp + 1.
    let n = exp + 1;
    let s = digits.as_str();

    let mut out = String::new();
    if neg {
        out.push('-');
    }
    if k <= n && n <= 21 {
        // integer with trailing zeros
        out.push_str(s);
        for _ in 0..(n - k) {
            out.push('0');
        }
    } else if 0 < n && n <= 21 {
        // digits with a decimal point inside
        out.push_str(&s[..n as usize]);
        out.push('.');
        out.push_str(&s[n as usize..]);
    } else if -6 < n && n <= 0 {
        // 0.<zeros><digits>
        out.push_str("0.");
        for _ in 0..(-n) {
            out.push('0');
        }
        out.push_str(s);
    } else {
        // exponential: d[.ddd]e±exp
        out.push_str(&s[..1]);
        if k > 1 {
            out.push('.');
            out.push_str(&s[1..]);
        }
        out.push('e');
        let e = n - 1;
        if e >= 0 {
            out.push('+');
            out.push_str(&e.to_string());
        } else {
            out.push('-');
            out.push_str(&(-e).to_string());
        }
    }
    out
}

/// serde `serialize_with` hook: emit an `f64` using [`js_number`] as a raw JSON number.
pub fn serialize_f64<S>(v: &f64, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    // RawValue writes its text verbatim into serde_json output (js_number always
    // produces a valid JSON number literal).
    let raw = RawValue::from_string(js_number(*v)).map_err(serde::ser::Error::custom)?;
    serde::Serialize::serialize(&raw, serializer)
}

#[cfg(test)]
mod tests {
    use super::js_number;

    #[test]
    fn matches_ecmascript_tostring() {
        // (value, expected) pairs cross-checked against Node `String(x)` / JSON.stringify.
        assert_eq!(js_number(0.0), "0");
        assert_eq!(js_number(-0.0), "0");
        assert_eq!(js_number(2.0), "2");
        assert_eq!(js_number(10.0), "10");
        assert_eq!(js_number(1.5), "1.5");
        assert_eq!(js_number(0.1), "0.1");
        assert_eq!(js_number(0.030275), "0.030275");
        assert_eq!(js_number(0.00381875), "0.00381875");
        assert_eq!(js_number(0.07902124999999999), "0.07902124999999999");
        assert_eq!(js_number(0.00004), "0.00004");
        assert_eq!(js_number(0.000005), "0.000005"); // NOT 5e-6
        assert_eq!(js_number(0.0000005), "5e-7"); // exponential kicks in below 1e-6
        assert_eq!(js_number(1e21), "1e+21"); // exponential on the large side, with '+'
        assert_eq!(js_number(1e-7), "1e-7");
        assert_eq!(js_number(123.0), "123");
        assert_eq!(js_number(-0.5), "-0.5");
    }
}
