//! Pi oracle for [`pirust_tui::terminal_image`] (feat-006 Wave 4).
//!
//! Replays every record of `tests/fixtures/pi/tui/terminal-image.cases.jsonl`
//! — captured by executing real Pi's `packages/tui/src/terminal-image.ts` —
//! and asserts identical results. Runs as a single `#[test]` so this file's
//! environment-variable mutation (for `detectCapabilities` cases) and the
//! global capabilities-cache singleton mutation cannot race with any other
//! test in this binary.

use std::path::PathBuf;

use pirust_tui::terminal_image::{
    calculate_image_cell_size, calculate_image_rows, delete_all_kitty_images, delete_kitty_image,
    detect_capabilities, encode_iterm2, encode_kitty, get_capabilities, get_gif_dimensions,
    get_image_dimensions, get_jpeg_dimensions, get_png_dimensions, get_webp_dimensions, hyperlink,
    image_fallback, is_image_line, render_image, set_capabilities, CellDimensions,
    EncodeIterm2Options, EncodeKittyOptions, ImageDimensions, ImageProtocol, ImageRenderOptions,
    TerminalCapabilities,
};
use serde_json::Value;

const ENV_IMAGE_KEYS: &[&str] = &[
    "TERM_PROGRAM",
    "TERMINAL_EMULATOR",
    "TERM",
    "COLORTERM",
    "KITTY_WINDOW_ID",
    "GHOSTTY_RESOURCES_DIR",
    "WEZTERM_PANE",
    "WARP_SESSION_ID",
    "WARP_TERMINAL_SESSION_UUID",
    "ITERM_SESSION_ID",
    "WT_SESSION",
    "TMUX",
];

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/pi/tui/terminal-image.cases.jsonl")
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

fn caps_to_value(caps: TerminalCapabilities) -> Value {
    let images = match caps.images {
        Some(ImageProtocol::Kitty) => Value::String("kitty".to_string()),
        Some(ImageProtocol::Iterm2) => Value::String("iterm2".to_string()),
        None => Value::Null,
    };
    serde_json::json!({ "images": images, "trueColor": caps.true_color, "hyperlinks": caps.hyperlinks })
}

fn dims_from_value(v: &Value) -> ImageDimensions {
    ImageDimensions {
        width_px: v["widthPx"].as_u64().unwrap() as u32,
        height_px: v["heightPx"].as_u64().unwrap() as u32,
    }
}

fn cell_dims_from_value(v: &Value) -> CellDimensions {
    CellDimensions {
        width_px: v["widthPx"].as_u64().unwrap() as u32,
        height_px: v["heightPx"].as_u64().unwrap() as u32,
    }
}

fn with_env<F: FnOnce() -> R, R>(overrides: &Value, f: F) -> R {
    let saved: Vec<(String, Option<String>)> = ENV_IMAGE_KEYS
        .iter()
        .map(|k| (k.to_string(), std::env::var(k).ok()))
        .collect();
    for k in ENV_IMAGE_KEYS {
        std::env::remove_var(k);
    }
    if let Some(obj) = overrides.as_object() {
        for (k, v) in obj {
            std::env::set_var(k, v.as_str().unwrap());
        }
    }
    let result = f();
    for k in ENV_IMAGE_KEYS {
        std::env::remove_var(k);
    }
    for (k, v) in saved {
        if let Some(v) = v {
            std::env::set_var(&k, v);
        }
    }
    result
}

#[test]
fn every_terminal_image_record_matches_pi() {
    let records = load_records();
    assert_eq!(
        records.len(),
        59,
        "fixture record count changed — update this assertion deliberately"
    );

    let mut failures = Vec::new();
    for record in &records {
        let note = record["note"].as_str().unwrap();
        let fn_name = record["fn"].as_str().unwrap();
        let args = &record["args"];
        let expected = &record["result"];

        let actual: Value = match fn_name {
            "detectCapabilities" => {
                let env_overrides = &args[0];
                let tmux_forwards = args[1].as_str() == Some("true");
                with_env(env_overrides, || {
                    caps_to_value(detect_capabilities(|| tmux_forwards))
                })
            }
            "getCapabilities" => {
                set_capabilities(TerminalCapabilities {
                    images: Some(ImageProtocol::Kitty),
                    true_color: true,
                    hyperlinks: true,
                });
                let result = caps_to_value(get_capabilities());
                pirust_tui::terminal_image::reset_capabilities_cache();
                result
            }
            "isImageLine" => Value::Bool(is_image_line(args[0].as_str().unwrap())),
            "encodeKitty" => {
                let data = args[0].as_str().unwrap();
                let opts = &args[1];
                let options = EncodeKittyOptions {
                    columns: opts["columns"].as_u64().map(|n| n as u32),
                    rows: opts["rows"].as_u64().map(|n| n as u32),
                    image_id: opts["imageId"].as_u64().map(|n| n as u32),
                    move_cursor: opts["moveCursor"].as_bool(),
                };
                Value::String(encode_kitty(data, options))
            }
            "deleteKittyImage" => {
                Value::String(delete_kitty_image(args[0].as_u64().unwrap() as u32))
            }
            "deleteAllKittyImages" => Value::String(delete_all_kitty_images()),
            "encodeITerm2" => {
                let data = args[0].as_str().unwrap();
                let opts = &args[1];
                let options = EncodeIterm2Options {
                    width: opts["width"].as_u64().map(|n| n.to_string()),
                    height: opts["height"].as_str().map(|s| s.to_string()),
                    name: opts["name"].as_str().map(|s| s.to_string()),
                    preserve_aspect_ratio: opts["preserveAspectRatio"].as_bool(),
                    inline: opts["inline"].as_bool(),
                };
                Value::String(encode_iterm2(data, &options))
            }
            "calculateImageCellSize" => {
                let dims = dims_from_value(&args[0]);
                let max_width = args[1].as_f64().unwrap();
                let max_height = args[2].as_f64();
                let cell_dims = cell_dims_from_value(&args[3]);
                let size = calculate_image_cell_size(dims, max_width, max_height, cell_dims);
                serde_json::json!({ "columns": size.columns, "rows": size.rows })
            }
            "calculateImageRows" => {
                let dims = dims_from_value(&args[0]);
                let target_width = args[1].as_f64().unwrap();
                let cell_dims = cell_dims_from_value(&args[2]);
                Value::Number(calculate_image_rows(dims, target_width, cell_dims).into())
            }
            "getPngDimensions" => {
                dims_option_to_value(get_png_dimensions(args[0].as_str().unwrap()))
            }
            "getJpegDimensions" => {
                dims_option_to_value(get_jpeg_dimensions(args[0].as_str().unwrap()))
            }
            "getGifDimensions" => {
                dims_option_to_value(get_gif_dimensions(args[0].as_str().unwrap()))
            }
            "getWebpDimensions" => {
                dims_option_to_value(get_webp_dimensions(args[0].as_str().unwrap()))
            }
            "getImageDimensions" => {
                let data = args[0].as_str().unwrap();
                let mime = args[1].as_str().unwrap();
                dims_option_to_value(get_image_dimensions(data, mime))
            }
            "renderImage" => {
                let data = args[0].as_str().unwrap();
                let dims = dims_from_value(&args[1]);
                let opts = &args[2];
                let options = ImageRenderOptions {
                    max_width_cells: opts["maxWidthCells"].as_u64().map(|n| n as u32),
                    ..Default::default()
                };
                match note {
                    "render-image-kitty" => {
                        set_capabilities(TerminalCapabilities {
                            images: Some(ImageProtocol::Kitty),
                            true_color: true,
                            hyperlinks: true,
                        });
                    }
                    "render-image-iterm2" => {
                        set_capabilities(TerminalCapabilities {
                            images: Some(ImageProtocol::Iterm2),
                            true_color: true,
                            hyperlinks: true,
                        });
                    }
                    _ => {
                        set_capabilities(TerminalCapabilities {
                            images: None,
                            true_color: true,
                            hyperlinks: true,
                        });
                    }
                }
                let result = render_image(data, dims, options);
                pirust_tui::terminal_image::reset_capabilities_cache();
                match result {
                    Some(r) => serde_json::json!({ "sequence": r.sequence, "rows": r.rows }),
                    None => Value::Null,
                }
            }
            "hyperlink" => Value::String(hyperlink(
                args[0].as_str().unwrap(),
                args[1].as_str().unwrap(),
            )),
            "imageFallback" => {
                let mime = args[0].as_str().unwrap();
                let dims = if args[1].is_null() {
                    None
                } else {
                    Some(dims_from_value(&args[1]))
                };
                let filename = args[2].as_str();
                Value::String(image_fallback(mime, dims, filename))
            }
            other => panic!("unknown fn {other}"),
        };

        // renderImage's real result also carries an `imageId` key only when
        // one was requested; neither test case here requests one, so it's
        // absent on the TS side too — compare only the fields both sides
        // definitely produce for these cases rather than a strict whole-object match.
        let matches = if fn_name == "renderImage" {
            actual["sequence"] == expected["sequence"] && actual["rows"] == expected["rows"]
                || (actual.is_null() && expected.is_null())
        } else {
            actual == *expected
        };

        if !matches {
            failures.push(format!(
                "[{note}] {fn_name}\n  expected: {expected}\n  actual:   {actual}"
            ));
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));
}

fn dims_option_to_value(dims: Option<ImageDimensions>) -> Value {
    match dims {
        Some(d) => serde_json::json!({ "widthPx": d.width_px, "heightPx": d.height_px }),
        None => Value::Null,
    }
}
