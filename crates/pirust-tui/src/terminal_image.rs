//! Port of `packages/tui/src/terminal-image.ts` — terminal capability
//! detection (Kitty/iTerm2 image protocols, truecolor, OSC 8 hyperlinks),
//! Kitty graphics + iTerm2 inline-image encoders, and pure-Rust image-header
//! dimension parsers (PNG/JPEG/GIF/WebP). See `docs/analysis/05-tui.md` §7/§9.
//! Internal helper — not part of `index.ts`'s public surface (§2 does not
//! list a top-level re-export of this module).
//!
//! ## Scope decisions (documented, not silent — AGENTS.md Correctness Bar)
//!
//! - **`getCapabilities`/`setCapabilities`/`getCellDimensions`/
//!   `setCellDimensions` are module-level mutable state**, exactly like the
//!   TS's own module-level `let cachedCapabilities`/`let cellDimensions` —
//!   ported as `static Mutex<...>`, same pattern as `keybindings.rs`'s
//!   (Wave 3) global singleton.
//! - **`allocateImageId`'s RNG.** The TS uses `Math.random()` purely to avoid
//!   image-id collisions between independent module instances — not a
//!   security context. `rand` is not a workspace dependency and adding one
//!   for this single call site would fail the Ponytail ladder's "does an
//!   already-installed dependency solve it" rung. This port seeds a simple
//!   xorshift-style counter from `std::time::SystemTime` + a per-process
//!   `AtomicU64` counter (mixed via a multiplicative hash) — good enough
//!   collision avoidance for the same reason `Math.random()` is, with zero
//!   new dependencies.
//! - **`probeTmuxHyperlinks`'s `execSync` call is injected as a closure
//!   parameter** on `detect_capabilities`, exactly like the TS's own
//!   `tmuxForwardsHyperlink` override parameter — ported via
//!   `std::process::Command` in the default implementation, not hardcoded
//!   into the detection cascade, so tests can inject a fake probe.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

/// `ImageProtocol` (terminal-image.ts:3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageProtocol {
    Kitty,
    Iterm2,
}

/// `TerminalCapabilities` (terminal-image.ts:5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalCapabilities {
    pub images: Option<ImageProtocol>,
    pub true_color: bool,
    pub hyperlinks: bool,
}

/// `CellDimensions` (terminal-image.ts:11).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellDimensions {
    pub width_px: u32,
    pub height_px: u32,
}

/// `ImageDimensions` (terminal-image.ts:16).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageDimensions {
    pub width_px: u32,
    pub height_px: u32,
}

/// `ImageRenderOptions` (terminal-image.ts:21).
#[derive(Debug, Clone, Copy, Default)]
pub struct ImageRenderOptions {
    pub max_width_cells: Option<u32>,
    pub max_height_cells: Option<u32>,
    pub preserve_aspect_ratio: Option<bool>,
    pub image_id: Option<u32>,
    pub move_cursor: Option<bool>,
}

const DEFAULT_CELL_DIMENSIONS: CellDimensions = CellDimensions {
    width_px: 9,
    height_px: 18,
};

static CELL_DIMENSIONS: Mutex<CellDimensions> = Mutex::new(DEFAULT_CELL_DIMENSIONS);
static CACHED_CAPABILITIES: Mutex<Option<TerminalCapabilities>> = Mutex::new(None);

/// `getCellDimensions` (terminal-image.ts:36).
pub fn get_cell_dimensions() -> CellDimensions {
    *CELL_DIMENSIONS.lock().unwrap_or_else(|e| e.into_inner())
}

/// `setCellDimensions` (terminal-image.ts:40).
pub fn set_cell_dimensions(dims: CellDimensions) {
    *CELL_DIMENSIONS.lock().unwrap_or_else(|e| e.into_inner()) = dims;
}

fn env_lower(key: &str) -> String {
    std::env::var(key).unwrap_or_default().to_lowercase()
}

fn env_present(key: &str) -> bool {
    std::env::var(key).is_ok()
}

/// `probeTmuxHyperlinks` (terminal-image.ts:49) — the real, live probe.
fn probe_tmux_hyperlinks_live() -> bool {
    use std::process::Command;
    let Ok(output) = Command::new("tmux")
        .args(["display-message", "-p", "#{client_termfeatures}"])
        .output()
    else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    text.split(',')
        .any(|feature| feature.trim() == "hyperlinks")
}

/// `detectCapabilities` (terminal-image.ts:65). `tmux_forwards_hyperlink` is
/// the Rust analogue of the TS's `tmuxForwardsHyperlink` override parameter
/// — pass [`probe_tmux_hyperlinks_live`] for real detection.
pub fn detect_capabilities(tmux_forwards_hyperlink: impl FnOnce() -> bool) -> TerminalCapabilities {
    let term_program = env_lower("TERM_PROGRAM");
    let terminal_emulator = env_lower("TERMINAL_EMULATOR");
    let term = env_lower("TERM");
    let color_term = env_lower("COLORTERM");
    let has_true_color_hint = color_term == "truecolor" || color_term == "24bit";

    if env_present("TMUX") || term.starts_with("tmux") {
        return TerminalCapabilities {
            images: None,
            true_color: has_true_color_hint,
            hyperlinks: tmux_forwards_hyperlink(),
        };
    }

    if term.starts_with("screen") {
        return TerminalCapabilities {
            images: None,
            true_color: has_true_color_hint,
            hyperlinks: false,
        };
    }

    if env_present("KITTY_WINDOW_ID") || term_program == "kitty" {
        return caps(Some(ImageProtocol::Kitty), true, true);
    }
    if term_program == "ghostty" || term.contains("ghostty") || env_present("GHOSTTY_RESOURCES_DIR")
    {
        return caps(Some(ImageProtocol::Kitty), true, true);
    }
    if env_present("WEZTERM_PANE") || term_program == "wezterm" {
        return caps(Some(ImageProtocol::Kitty), true, true);
    }
    if term_program == "warpterminal"
        || env_present("WARP_SESSION_ID")
        || env_present("WARP_TERMINAL_SESSION_UUID")
    {
        return caps(Some(ImageProtocol::Kitty), true, true);
    }
    if env_present("ITERM_SESSION_ID") || term_program == "iterm.app" {
        return caps(Some(ImageProtocol::Iterm2), true, true);
    }
    if env_present("WT_SESSION") {
        return caps(None, true, true);
    }
    if term_program == "vscode" {
        return caps(None, true, true);
    }
    if term_program == "alacritty" {
        return caps(None, true, true);
    }
    if terminal_emulator == "jetbrains-jediterm" {
        return caps(None, true, false);
    }

    TerminalCapabilities {
        images: None,
        true_color: has_true_color_hint,
        hyperlinks: false,
    }
}

fn caps(images: Option<ImageProtocol>, true_color: bool, hyperlinks: bool) -> TerminalCapabilities {
    TerminalCapabilities {
        images,
        true_color,
        hyperlinks,
    }
}

/// `getCapabilities` (terminal-image.ts:127) — lazily detects and caches
/// using the real (live) tmux probe.
pub fn get_capabilities() -> TerminalCapabilities {
    let mut guard = CACHED_CAPABILITIES
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if guard.is_none() {
        *guard = Some(detect_capabilities(probe_tmux_hyperlinks_live));
    }
    guard.unwrap()
}

/// `resetCapabilitiesCache` (terminal-image.ts:134).
pub fn reset_capabilities_cache() {
    *CACHED_CAPABILITIES
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = None;
}

/// `setCapabilities` (terminal-image.ts:139).
pub fn set_capabilities(caps: TerminalCapabilities) {
    *CACHED_CAPABILITIES
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = Some(caps);
}

const KITTY_PREFIX: &str = "\x1b_G";
const ITERM2_PREFIX: &str = "\x1b]1337;File=";

/// `isImageLine` (terminal-image.ts:146).
pub fn is_image_line(line: &str) -> bool {
    line.starts_with(KITTY_PREFIX)
        || line.starts_with(ITERM2_PREFIX)
        || line.contains(KITTY_PREFIX)
        || line.contains(ITERM2_PREFIX)
}

static IMAGE_ID_COUNTER: AtomicU64 = AtomicU64::new(0);

/// `allocateImageId` (terminal-image.ts:160) — see module docs, "RNG" scope
/// decision. Range `[1, 0xffffffff]`, matching the TS's own range.
pub fn allocate_image_id() -> u32 {
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let counter = IMAGE_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    // Multiplicative mix (Knuth's constant) — spreads sequential counters and
    // a coarse time seed across the 32-bit space, avoiding collisions between
    // module instances started at the same instant, same intent as
    // `Math.random()` here (collision avoidance, not a security boundary).
    let mixed = seed.wrapping_add(counter).wrapping_mul(0x9E3779B97F4A7C15);
    ((mixed >> 32) as u32 % 0xffff_fffe) + 1
}

/// `EncodeKittyOptions` (`encodeKitty`'s inline options type, terminal-image.ts:166).
#[derive(Debug, Clone, Copy, Default)]
pub struct EncodeKittyOptions {
    pub columns: Option<u32>,
    pub rows: Option<u32>,
    pub image_id: Option<u32>,
    pub move_cursor: Option<bool>,
}

/// `encodeKitty` (terminal-image.ts:165).
pub fn encode_kitty(base64_data: &str, options: EncodeKittyOptions) -> String {
    const CHUNK_SIZE: usize = 4096;

    let mut params = vec!["a=T".to_string(), "f=100".to_string(), "q=2".to_string()];
    if options.move_cursor == Some(false) {
        params.push("C=1".to_string());
    }
    if let Some(columns) = options.columns {
        if columns != 0 {
            params.push(format!("c={columns}"));
        }
    }
    if let Some(rows) = options.rows {
        if rows != 0 {
            params.push(format!("r={rows}"));
        }
    }
    if let Some(image_id) = options.image_id {
        if image_id != 0 {
            params.push(format!("i={image_id}"));
        }
    }
    let params_joined = params.join(",");

    if base64_data.len() <= CHUNK_SIZE {
        return format!("\x1b_G{params_joined};{base64_data}\x1b\\");
    }

    let mut chunks = Vec::new();
    let mut offset = 0usize;
    let mut is_first = true;
    let bytes = base64_data.as_bytes();
    while offset < bytes.len() {
        let end = (offset + CHUNK_SIZE).min(bytes.len());
        let chunk = &base64_data[offset..end];
        let is_last = offset + CHUNK_SIZE >= bytes.len();
        if is_first {
            chunks.push(format!("\x1b_G{params_joined},m=1;{chunk}\x1b\\"));
            is_first = false;
        } else if is_last {
            chunks.push(format!("\x1b_Gm=0;{chunk}\x1b\\"));
        } else {
            chunks.push(format!("\x1b_Gm=1;{chunk}\x1b\\"));
        }
        offset += CHUNK_SIZE;
    }
    chunks.join("")
}

/// `deleteKittyImage` (terminal-image.ts:215).
pub fn delete_kitty_image(image_id: u32) -> String {
    format!("\x1b_Ga=d,d=I,i={image_id},q=2\x1b\\")
}

/// `deleteAllKittyImages` (terminal-image.ts:223).
pub fn delete_all_kitty_images() -> String {
    "\x1b_Ga=d,d=A,q=2\x1b\\".to_string()
}

/// `EncodeIterm2Options` (`encodeITerm2`'s inline options type, terminal-image.ts:229).
#[derive(Debug, Clone, Default)]
pub struct EncodeIterm2Options {
    pub width: Option<String>,
    pub height: Option<String>,
    pub name: Option<String>,
    pub preserve_aspect_ratio: Option<bool>,
    pub inline: Option<bool>,
}

/// `encodeITerm2` (terminal-image.ts:227).
pub fn encode_iterm2(base64_data: &str, options: &EncodeIterm2Options) -> String {
    let mut params = vec![format!(
        "inline={}",
        if options.inline != Some(false) { 1 } else { 0 }
    )];
    if let Some(width) = &options.width {
        params.push(format!("width={width}"));
    }
    if let Some(height) = &options.height {
        params.push(format!("height={height}"));
    }
    if let Some(name) = &options.name {
        use base64::Engine;
        let name_base64 = base64::engine::general_purpose::STANDARD.encode(name.as_bytes());
        params.push(format!("name={name_base64}"));
    }
    if options.preserve_aspect_ratio == Some(false) {
        params.push("preserveAspectRatio=0".to_string());
    }
    format!("\x1b]1337;File={}:{base64_data}\x07", params.join(";"))
}

/// `ImageCellSize` (terminal-image.ts:252).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageCellSize {
    pub columns: u32,
    pub rows: u32,
}

/// `calculateImageCellSize` (terminal-image.ts:257).
pub fn calculate_image_cell_size(
    image_dimensions: ImageDimensions,
    max_width_cells: f64,
    max_height_cells: Option<f64>,
    cell_dimensions: CellDimensions,
) -> ImageCellSize {
    let max_width = max_width_cells.floor().max(1.0);
    let max_height = max_height_cells.map(|h| h.floor().max(1.0));
    let image_width = (image_dimensions.width_px as f64).max(1.0);
    let image_height = (image_dimensions.height_px as f64).max(1.0);

    let width_scale = (max_width * cell_dimensions.width_px as f64) / image_width;
    let height_scale = match max_height {
        None => width_scale,
        Some(h) => (h * cell_dimensions.height_px as f64) / image_height,
    };
    let scale = width_scale.min(height_scale);

    let scaled_width_px = image_width * scale;
    let scaled_height_px = image_height * scale;
    let columns = (scaled_width_px / cell_dimensions.width_px as f64).ceil();
    let rows = (scaled_height_px / cell_dimensions.height_px as f64).ceil();

    let columns = 1.0f64.max(max_width.min(columns));
    let rows = match max_height {
        None => 1.0f64.max(rows),
        Some(h) => 1.0f64.max(h.min(rows)),
    };

    ImageCellSize {
        columns: columns as u32,
        rows: rows as u32,
    }
}

/// `calculateImageRows` (terminal-image.ts:283).
pub fn calculate_image_rows(
    image_dimensions: ImageDimensions,
    target_width_cells: f64,
    cell_dimensions: CellDimensions,
) -> u32 {
    calculate_image_cell_size(image_dimensions, target_width_cells, None, cell_dimensions).rows
}

fn decode_base64(base64_data: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(base64_data)
        .ok()
}

/// `getPngDimensions` (terminal-image.ts:291).
pub fn get_png_dimensions(base64_data: &str) -> Option<ImageDimensions> {
    let buffer = decode_base64(base64_data)?;
    if buffer.len() < 24 {
        return None;
    }
    if buffer[0..4] != [0x89, 0x50, 0x4e, 0x47] {
        return None;
    }
    let width = u32::from_be_bytes(buffer[16..20].try_into().ok()?);
    let height = u32::from_be_bytes(buffer[20..24].try_into().ok()?);
    Some(ImageDimensions {
        width_px: width,
        height_px: height,
    })
}

/// `getJpegDimensions` (terminal-image.ts:312).
pub fn get_jpeg_dimensions(base64_data: &str) -> Option<ImageDimensions> {
    let buffer = decode_base64(base64_data)?;
    if buffer.len() < 2 {
        return None;
    }
    if buffer[0] != 0xff || buffer[1] != 0xd8 {
        return None;
    }

    let mut offset = 2usize;
    while offset < buffer.len().saturating_sub(9) {
        if buffer[offset] != 0xff {
            offset += 1;
            continue;
        }
        let marker = buffer[offset + 1];
        if (0xc0..=0xc2).contains(&marker) {
            let height = u16::from_be_bytes(buffer[offset + 5..offset + 7].try_into().ok()?);
            let width = u16::from_be_bytes(buffer[offset + 7..offset + 9].try_into().ok()?);
            return Some(ImageDimensions {
                width_px: width as u32,
                height_px: height as u32,
            });
        }
        if offset + 3 >= buffer.len() {
            return None;
        }
        let length = u16::from_be_bytes(buffer[offset + 2..offset + 4].try_into().ok()?);
        if length < 2 {
            return None;
        }
        offset += 2 + length as usize;
    }
    None
}

/// `getGifDimensions` (terminal-image.ts:355).
pub fn get_gif_dimensions(base64_data: &str) -> Option<ImageDimensions> {
    let buffer = decode_base64(base64_data)?;
    if buffer.len() < 10 {
        return None;
    }
    let sig = std::str::from_utf8(&buffer[0..6]).ok()?;
    if sig != "GIF87a" && sig != "GIF89a" {
        return None;
    }
    let width = u16::from_le_bytes(buffer[6..8].try_into().ok()?);
    let height = u16::from_le_bytes(buffer[8..10].try_into().ok()?);
    Some(ImageDimensions {
        width_px: width as u32,
        height_px: height as u32,
    })
}

/// `getWebpDimensions` (terminal-image.ts:377).
pub fn get_webp_dimensions(base64_data: &str) -> Option<ImageDimensions> {
    let buffer = decode_base64(base64_data)?;
    if buffer.len() < 30 {
        return None;
    }
    let riff = std::str::from_utf8(&buffer[0..4]).ok()?;
    let webp = std::str::from_utf8(&buffer[8..12]).ok()?;
    if riff != "RIFF" || webp != "WEBP" {
        return None;
    }
    let chunk = std::str::from_utf8(&buffer[12..16]).ok()?;
    match chunk {
        "VP8 " => {
            let width = (u16::from_le_bytes(buffer[26..28].try_into().ok()?)) & 0x3fff;
            let height = (u16::from_le_bytes(buffer[28..30].try_into().ok()?)) & 0x3fff;
            Some(ImageDimensions {
                width_px: width as u32,
                height_px: height as u32,
            })
        }
        "VP8L" => {
            if buffer.len() < 25 {
                return None;
            }
            let bits = u32::from_le_bytes(buffer[21..25].try_into().ok()?);
            let width = (bits & 0x3fff) + 1;
            let height = ((bits >> 14) & 0x3fff) + 1;
            Some(ImageDimensions {
                width_px: width,
                height_px: height,
            })
        }
        "VP8X" => {
            let width =
                (buffer[24] as u32 | (buffer[25] as u32) << 8 | (buffer[26] as u32) << 16) + 1;
            let height =
                (buffer[27] as u32 | (buffer[28] as u32) << 8 | (buffer[29] as u32) << 16) + 1;
            Some(ImageDimensions {
                width_px: width,
                height_px: height,
            })
        }
        _ => None,
    }
}

/// `getImageDimensions` (terminal-image.ts:416).
pub fn get_image_dimensions(base64_data: &str, mime_type: &str) -> Option<ImageDimensions> {
    match mime_type {
        "image/png" => get_png_dimensions(base64_data),
        "image/jpeg" => get_jpeg_dimensions(base64_data),
        "image/gif" => get_gif_dimensions(base64_data),
        "image/webp" => get_webp_dimensions(base64_data),
        _ => None,
    }
}

/// `renderImage`'s result (terminal-image.ts:436's return type).
#[derive(Debug, Clone)]
pub struct RenderedImage {
    pub sequence: String,
    pub rows: u32,
    pub image_id: Option<u32>,
}

/// `renderImage` (terminal-image.ts:432).
pub fn render_image(
    base64_data: &str,
    image_dimensions: ImageDimensions,
    options: ImageRenderOptions,
) -> Option<RenderedImage> {
    let capabilities = get_capabilities();
    let protocol = capabilities.images?;

    let max_width = options.max_width_cells.unwrap_or(80);
    let size = calculate_image_cell_size(
        image_dimensions,
        max_width as f64,
        options.max_height_cells.map(|h| h as f64),
        get_cell_dimensions(),
    );

    match protocol {
        ImageProtocol::Kitty => {
            let sequence = encode_kitty(
                base64_data,
                EncodeKittyOptions {
                    columns: Some(size.columns),
                    rows: Some(size.rows),
                    image_id: options.image_id,
                    move_cursor: options.move_cursor,
                },
            );
            Some(RenderedImage {
                sequence,
                rows: size.rows,
                image_id: options.image_id,
            })
        }
        ImageProtocol::Iterm2 => {
            let sequence = encode_iterm2(
                base64_data,
                &EncodeIterm2Options {
                    width: Some(size.columns.to_string()),
                    height: Some("auto".to_string()),
                    preserve_aspect_ratio: Some(options.preserve_aspect_ratio.unwrap_or(true)),
                    ..Default::default()
                },
            );
            Some(RenderedImage {
                sequence,
                rows: size.rows,
                image_id: None,
            })
        }
    }
}

/// `hyperlink` (terminal-image.ts:478).
pub fn hyperlink(text: &str, url: &str) -> String {
    format!("\x1b]8;;{url}\x1b\\{text}\x1b]8;;\x1b\\")
}

/// `imageFallback` (terminal-image.ts:482).
pub fn image_fallback(
    mime_type: &str,
    dimensions: Option<ImageDimensions>,
    filename: Option<&str>,
) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(filename) = filename {
        parts.push(filename.to_string());
    }
    parts.push(format!("[{mime_type}]"));
    if let Some(dims) = dimensions {
        parts.push(format!("{}x{}", dims.width_px, dims.height_px));
    }
    format!("[Image: {}]", parts.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_image_line_detects_both_protocols() {
        assert!(is_image_line("\x1b_Gsome-kitty-data"));
        assert!(is_image_line("\x1b]1337;File=data"));
        assert!(!is_image_line("plain text"));
    }

    #[test]
    fn delete_kitty_image_matches_format() {
        assert_eq!(delete_kitty_image(42), "\x1b_Ga=d,d=I,i=42,q=2\x1b\\");
    }

    #[test]
    fn allocate_image_id_is_in_range() {
        for _ in 0..100 {
            let id = allocate_image_id();
            assert!(id >= 1);
        }
    }
}
