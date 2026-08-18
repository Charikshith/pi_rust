//! Port of `packages/tui/src/terminal-colors.ts` — OSC 11 background-color
//! response parsing and the `CSI ? 997 ; {1|2} n` color-scheme report parser.
//! See `docs/analysis/05-tui.md` §7. Internal helper — not part of
//! `index.ts`'s public surface (confirmed against §2: neither this module's
//! functions nor types appear in the top-level export list).

/// `RgbColor` (terminal-colors.ts:1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RgbColor {
    pub r: u32,
    pub g: u32,
    pub b: u32,
}

/// `TerminalColorScheme` (terminal-colors.ts:7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalColorScheme {
    Dark,
    Light,
}

fn hex_to_rgb(hex: &str) -> RgbColor {
    let normalized = hex.strip_prefix('#').unwrap_or(hex);
    RgbColor {
        r: u32::from_str_radix(&normalized[0..2], 16).unwrap_or(0),
        g: u32::from_str_radix(&normalized[2..4], 16).unwrap_or(0),
        b: u32::from_str_radix(&normalized[4..6], 16).unwrap_or(0),
    }
}

fn is_hex_digits(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// `parseOscHexChannel` (terminal-colors.ts:17) — scales an N-hex-digit
/// channel value to the 0-255 range.
fn parse_osc_hex_channel(channel: &str) -> Option<u32> {
    if !is_hex_digits(channel) {
        return None;
    }
    let max = 16u64.pow(channel.len() as u32).saturating_sub(1);
    if max == 0 {
        return None;
    }
    let value = u64::from_str_radix(channel, 16).ok()?;
    Some(((value as f64 / max as f64) * 255.0).round() as u32)
}

/// `OSC11_BACKGROUND_COLOR_RESPONSE_PATTERN` (terminal-colors.ts:28):
/// `^\x1b\]11;([^\x07\x1b]*)(?:\x07|\x1b\\)$`, matched case-insensitively.
fn match_osc11(data: &str) -> Option<&str> {
    let rest = data.strip_prefix("\x1b]11;")?;
    let (body, terminated) = if let Some(b) = rest.strip_suffix('\x07') {
        (b, true)
    } else if let Some(b) = rest.strip_suffix("\x1b\\") {
        (b, true)
    } else {
        (rest, false)
    };
    if !terminated || body.contains('\x07') || body.contains('\x1b') {
        return None;
    }
    Some(body)
}

/// `isOsc11BackgroundColorResponse` (terminal-colors.ts:31).
pub fn is_osc11_background_color_response(data: &str) -> bool {
    match_osc11(data).is_some()
}

/// `parseOsc11BackgroundColor` (terminal-colors.ts:35).
pub fn parse_osc11_background_color(data: &str) -> Option<RgbColor> {
    let value = match_osc11(data)?.trim();

    if let Some(hex) = value.strip_prefix('#') {
        if hex.len() == 6 && is_hex_digits(hex) {
            return Some(hex_to_rgb(value));
        }
        if hex.len() == 12 && is_hex_digits(hex) {
            let r = parse_osc_hex_channel(&hex[0..4])?;
            let g = parse_osc_hex_channel(&hex[4..8])?;
            let b = parse_osc_hex_channel(&hex[8..12])?;
            return Some(RgbColor { r, g, b });
        }
        return None;
    }

    // Strip an optional leading "rgb:"/"rgba:" (case-insensitive).
    let rgb_value = ["rgba:", "rgb:"]
        .iter()
        .find_map(|prefix| {
            if value.len() >= prefix.len() && value[..prefix.len()].eq_ignore_ascii_case(prefix) {
                Some(&value[prefix.len()..])
            } else {
                None
            }
        })
        .unwrap_or(value);

    let mut parts = rgb_value.split('/');
    let (red, green, blue) = (parts.next()?, parts.next()?, parts.next()?);
    let r = parse_osc_hex_channel(red)?;
    let g = parse_osc_hex_channel(green)?;
    let b = parse_osc_hex_channel(blue)?;
    Some(RgbColor { r, g, b })
}

/// `parseTerminalColorSchemeReport` (terminal-colors.ts:67):
/// `^\x1b\[\?997;(1|2)n$`.
pub fn parse_terminal_color_scheme_report(data: &str) -> Option<TerminalColorScheme> {
    let rest = data.strip_prefix("\x1b[?997;")?;
    let digit = rest.strip_suffix('n')?;
    match digit {
        "1" => Some(TerminalColorScheme::Dark),
        "2" => Some(TerminalColorScheme::Light),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex6_roundtrip() {
        assert_eq!(
            parse_osc11_background_color("\x1b]11;#1a2b3c\x07"),
            Some(RgbColor {
                r: 0x1a,
                g: 0x2b,
                b: 0x3c
            })
        );
    }

    #[test]
    fn scheme_report_light() {
        assert_eq!(
            parse_terminal_color_scheme_report("\x1b[?997;2n"),
            Some(TerminalColorScheme::Light)
        );
    }

    #[test]
    fn non_matching_is_none() {
        assert_eq!(parse_terminal_color_scheme_report("\x1b[?997;3n"), None);
        assert!(!is_osc11_background_color_response("hello"));
    }
}
