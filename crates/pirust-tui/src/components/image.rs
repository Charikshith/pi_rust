//! Port of `packages/tui/src/components/image.ts` — renders an image via
//! Kitty graphics or iTerm2 inline images, with a text fallback. See
//! `docs/analysis/05-tui.md` §6.

use crate::terminal_image::{
    allocate_image_id, get_capabilities, get_cell_dimensions, get_image_dimensions, image_fallback,
    render_image, ImageDimensions, ImageProtocol, ImageRenderOptions,
};
use crate::tui::Component;

/// `ImageTheme` (image.ts:12).
pub struct ImageTheme {
    pub fallback_color: Box<dyn Fn(&str) -> String>,
}

/// `ImageOptions` (image.ts:16).
#[derive(Debug, Clone, Default)]
pub struct ImageOptions {
    pub max_width_cells: Option<u32>,
    pub max_height_cells: Option<u32>,
    pub filename: Option<String>,
    pub image_id: Option<u32>,
}

/// `Image` (image.ts:24).
pub struct Image {
    base64_data: String,
    mime_type: String,
    dimensions: ImageDimensions,
    theme: ImageTheme,
    options: ImageOptions,
    image_id: Option<u32>,
    cached_lines: Option<Vec<String>>,
    cached_width: Option<usize>,
}

impl Image {
    /// `constructor` (image.ts:35).
    pub fn new(
        base64_data: impl Into<String>,
        mime_type: impl Into<String>,
        theme: ImageTheme,
        options: ImageOptions,
        dimensions: Option<ImageDimensions>,
    ) -> Self {
        let base64_data = base64_data.into();
        let mime_type = mime_type.into();
        let dimensions = dimensions
            .or_else(|| get_image_dimensions(&base64_data, &mime_type))
            .unwrap_or(ImageDimensions {
                width_px: 800,
                height_px: 600,
            });
        let image_id = options.image_id;
        Self {
            base64_data,
            mime_type,
            dimensions,
            theme,
            options,
            image_id,
            cached_lines: None,
            cached_width: None,
        }
    }

    /// `getImageId` (image.ts:51).
    pub fn get_image_id(&self) -> Option<u32> {
        self.image_id
    }
}

impl Component for Image {
    /// `render` (image.ts:60).
    fn render(&mut self, width: usize) -> Vec<String> {
        if let (Some(cached), Some(cached_width)) = (&self.cached_lines, self.cached_width) {
            if cached_width == width {
                return cached.clone();
            }
        }

        let max_width = (width.saturating_sub(2))
            .max(1)
            .min(self.options.max_width_cells.unwrap_or(60) as usize);
        let cell_dimensions = get_cell_dimensions();
        let default_max_height = (((max_width as f64 * cell_dimensions.width_px as f64)
            / cell_dimensions.height_px as f64)
            .ceil() as usize)
            .max(1);
        let max_height = self
            .options
            .max_height_cells
            .map(|h| h as usize)
            .unwrap_or(default_max_height);

        let caps = get_capabilities();
        let lines: Vec<String>;

        if let Some(protocol) = caps.images {
            if protocol == ImageProtocol::Kitty && self.image_id.is_none() {
                self.image_id = Some(allocate_image_id());
            }
            let result = render_image(
                &self.base64_data,
                self.dimensions,
                ImageRenderOptions {
                    max_width_cells: Some(max_width as u32),
                    max_height_cells: Some(max_height as u32),
                    image_id: self.image_id,
                    move_cursor: Some(false),
                    ..Default::default()
                },
            );

            if let Some(result) = result {
                if let Some(image_id) = result.image_id {
                    self.image_id = Some(image_id);
                }

                if protocol == ImageProtocol::Kitty {
                    let mut ls = vec![result.sequence];
                    for _ in 0..result.rows.saturating_sub(1) {
                        ls.push(String::new());
                    }
                    lines = ls;
                } else {
                    let mut ls: Vec<String> = Vec::new();
                    for _ in 0..result.rows.saturating_sub(1) {
                        ls.push(String::new());
                    }
                    let row_offset = result.rows.saturating_sub(1);
                    let move_up = if row_offset > 0 {
                        format!("\x1b[{row_offset}A")
                    } else {
                        String::new()
                    };
                    ls.push(format!("{move_up}{}", result.sequence));
                    lines = ls;
                }
            } else {
                let fallback = image_fallback(
                    &self.mime_type,
                    Some(self.dimensions),
                    self.options.filename.as_deref(),
                );
                lines = vec![(self.theme.fallback_color)(&fallback)];
            }
        } else {
            let fallback = image_fallback(
                &self.mime_type,
                Some(self.dimensions),
                self.options.filename.as_deref(),
            );
            lines = vec![(self.theme.fallback_color)(&fallback)];
        }

        self.cached_lines = Some(lines.clone());
        self.cached_width = Some(width);

        lines
    }

    fn invalidate(&mut self) {
        self.cached_lines = None;
        self.cached_width = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal_image::{reset_capabilities_cache, set_capabilities};

    fn theme() -> ImageTheme {
        ImageTheme {
            fallback_color: Box::new(|s: &str| s.to_string()),
        }
    }

    #[test]
    fn no_image_capability_renders_fallback() {
        set_capabilities(crate::terminal_image::TerminalCapabilities {
            images: None,
            true_color: false,
            hyperlinks: false,
        });
        let mut img = Image::new(
            "",
            "image/png",
            theme(),
            ImageOptions {
                filename: Some("cat.png".to_string()),
                ..Default::default()
            },
            Some(ImageDimensions {
                width_px: 100,
                height_px: 50,
            }),
        );
        let lines = img.render(40);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("cat.png"));
        assert!(lines[0].contains("image/png"));
        reset_capabilities_cache();
    }
}
