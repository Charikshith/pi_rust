//! Pi oracle for [`pirust_tui::components::image::Image`] (feat-006 Wave 5).
//!
//! Replays every record of `tests/fixtures/pi/tui/image.cases.jsonl` —
//! captured by executing real Pi's `packages/tui/src/components/image.ts`
//! against forced capability outcomes (`kitty`/`iterm2`/none) — and asserts
//! identical `render(width)` byte sequences, including the Kitty/iTerm2
//! cursor-movement escape construction.
//!
//! Every case's `imageId` (when the Kitty branch allocates one) is passed
//! explicitly in the fixture rather than left to `allocateImageId()`'s
//! `Math.random()` — see `scripts/gen-tui-oracle.mjs`'s `img()` helper's own
//! comment — so this test never needs to special-case a nondeterministic
//! field.

use std::path::PathBuf;

use pirust_tui::components::image::{Image, ImageOptions, ImageTheme};
use pirust_tui::terminal_image::{
    reset_capabilities_cache, set_capabilities, set_cell_dimensions, CellDimensions,
    ImageDimensions, ImageProtocol, TerminalCapabilities,
};
use pirust_tui::tui::Component;
use serde_json::Value;

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/pi/tui/image.cases.jsonl")
}

fn load_records() -> Vec<Value> {
    let path = fixture_path();
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read fixture {}: {error}", path.display()));
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .enumerate()
        .map(|(index, line)| {
            serde_json::from_str(line)
                .unwrap_or_else(|error| panic!("line {}: {error}\n  {line}", index + 1))
        })
        .collect()
}

fn parse_protocol(v: &Value) -> Option<ImageProtocol> {
    match v.as_str() {
        Some("kitty") => Some(ImageProtocol::Kitty),
        Some("iterm2") => Some(ImageProtocol::Iterm2),
        _ => None,
    }
}

#[test]
fn every_image_record_matches_pi() {
    let records = load_records();
    assert_eq!(
        records.len(),
        4,
        "fixture record count changed — update this assertion deliberately"
    );

    let mut failures = Vec::new();
    for record in &records {
        let note = record["note"].as_str().unwrap();
        let caps_images = parse_protocol(&record["capsImages"]);
        let base64_data = record["base64Data"].as_str().unwrap();
        let mime_type = record["mimeType"].as_str().unwrap();
        let width = record["width"].as_u64().unwrap() as usize;
        let dims = record.get("dims").and_then(|d| {
            if d.is_null() {
                None
            } else {
                Some(ImageDimensions {
                    width_px: d["widthPx"].as_u64().unwrap() as u32,
                    height_px: d["heightPx"].as_u64().unwrap() as u32,
                })
            }
        });
        let options = &record["options"];
        let image_options = ImageOptions {
            max_width_cells: options
                .get("maxWidthCells")
                .and_then(|v| v.as_u64())
                .map(|v| v as u32),
            max_height_cells: options
                .get("maxHeightCells")
                .and_then(|v| v.as_u64())
                .map(|v| v as u32),
            filename: options
                .get("filename")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            image_id: options
                .get("imageId")
                .and_then(|v| v.as_u64())
                .map(|v| v as u32),
        };
        let expected: Vec<String> = record["result"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();

        reset_capabilities_cache();
        set_capabilities(TerminalCapabilities {
            images: caps_images,
            true_color: true,
            hyperlinks: true,
        });
        set_cell_dimensions(CellDimensions {
            width_px: 9,
            height_px: 18,
        });

        let theme = ImageTheme {
            fallback_color: Box::new(|s: &str| format!("<fb>{s}</fb>")),
        };
        let mut image = Image::new(base64_data, mime_type, theme, image_options, dims);
        let actual = image.render(width);

        reset_capabilities_cache();

        if actual != expected {
            failures.push(format!(
                "[{note}]\n  expected: {expected:?}\n  actual:   {actual:?}"
            ));
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));
}
