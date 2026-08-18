//! Port of `packages/tui/src/components/select-list.ts` — a scrollable
//! single-select list with a two-column primary/description layout. See
//! `docs/analysis/05-tui.md` §6.

use crate::keybindings::{get_keybindings, Keybinding};
use crate::tui::Component;
use crate::utils::{truncate_to_width, visible_width};

const DEFAULT_PRIMARY_COLUMN_WIDTH: usize = 32;
const PRIMARY_COLUMN_GAP: usize = 2;
const MIN_DESCRIPTION_WIDTH: usize = 10;

/// `text.replace(/[\r\n]+/g, " ").trim()` (select-list.ts:9) — collapse each
/// RUN of `\r`/`\n` to a single space (preserving other whitespace exactly),
/// then trim the ends.
fn normalize_to_single_line(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut in_run = false;
    for ch in text.chars() {
        if ch == '\r' || ch == '\n' {
            if !in_run {
                result.push(' ');
                in_run = true;
            }
        } else {
            result.push(ch);
            in_run = false;
        }
    }
    result.trim().to_string()
}

fn clamp(value: usize, min: usize, max: usize) -> usize {
    value.max(min).min(max)
}

/// `SelectItem` (select-list.ts:12).
#[derive(Debug, Clone)]
pub struct SelectItem {
    pub value: String,
    pub label: String,
    pub description: Option<String>,
}

/// `SelectListTheme` (select-list.ts:18).
pub struct SelectListTheme {
    pub selected_prefix: Box<dyn Fn(&str) -> String>,
    pub selected_text: Box<dyn Fn(&str) -> String>,
    pub description: Box<dyn Fn(&str) -> String>,
    pub scroll_info: Box<dyn Fn(&str) -> String>,
    pub no_match: Box<dyn Fn(&str) -> String>,
}

/// `SelectListTruncatePrimaryContext` (select-list.ts:26).
pub struct SelectListTruncatePrimaryContext<'a> {
    pub text: &'a str,
    pub max_width: usize,
    pub column_width: usize,
    pub item: &'a SelectItem,
    pub is_selected: bool,
}

/// `layout.truncatePrimary` override (select-list.ts:37), factored into a
/// `type` alias per `clippy::type_complexity`.
pub type TruncatePrimaryFn = Box<dyn Fn(&SelectListTruncatePrimaryContext) -> String>;
/// An `Option<&SelectItem>`-argument `FnMut` callback, factored into a
/// `type` alias per `clippy::type_complexity`.
pub type OnItemFnMut = Box<dyn FnMut(&SelectItem)>;

/// `SelectListLayoutOptions` (select-list.ts:34).
#[derive(Default)]
pub struct SelectListLayoutOptions {
    pub min_primary_column_width: Option<usize>,
    pub max_primary_column_width: Option<usize>,
    pub truncate_primary: Option<TruncatePrimaryFn>,
}

/// `SelectList` (select-list.ts:40).
pub struct SelectList {
    items: Vec<SelectItem>,
    filtered_items: Vec<SelectItem>,
    selected_index: usize,
    max_visible: usize,
    theme: SelectListTheme,
    layout: SelectListLayoutOptions,
    pub on_select: Option<OnItemFnMut>,
    pub on_cancel: Option<Box<dyn FnMut()>>,
    pub on_selection_change: Option<OnItemFnMut>,
}

impl SelectList {
    /// `constructor` (select-list.ts:52).
    pub fn new(
        items: Vec<SelectItem>,
        max_visible: usize,
        theme: SelectListTheme,
        layout: SelectListLayoutOptions,
    ) -> Self {
        Self {
            items: items.clone(),
            filtered_items: items,
            selected_index: 0,
            max_visible,
            theme,
            layout,
            on_select: None,
            on_cancel: None,
            on_selection_change: None,
        }
    }

    /// `setFilter` (select-list.ts:60).
    pub fn set_filter(&mut self, filter: &str) {
        let filter_lower = filter.to_lowercase();
        self.filtered_items = self
            .items
            .iter()
            .filter(|item| item.value.to_lowercase().starts_with(&filter_lower))
            .cloned()
            .collect();
        self.selected_index = 0;
    }

    /// `setSelectedIndex` (select-list.ts:66).
    pub fn set_selected_index(&mut self, index: usize) {
        let max = self.filtered_items.len().saturating_sub(1);
        self.selected_index = index.min(max);
    }

    /// `getSelectedItem` (select-list.ts:225).
    pub fn get_selected_item(&self) -> Option<&SelectItem> {
        self.filtered_items.get(self.selected_index)
    }

    fn notify_selection_change(&mut self) {
        if let Some(item) = self.filtered_items.get(self.selected_index).cloned() {
            if let Some(cb) = &mut self.on_selection_change {
                cb(&item);
            }
        }
    }

    fn get_display_value(item: &SelectItem) -> &str {
        if item.label.is_empty() {
            &item.value
        } else {
            &item.label
        }
    }

    fn get_primary_column_bounds(&self) -> (usize, usize) {
        let raw_min = self
            .layout
            .min_primary_column_width
            .or(self.layout.max_primary_column_width)
            .unwrap_or(DEFAULT_PRIMARY_COLUMN_WIDTH);
        let raw_max = self
            .layout
            .max_primary_column_width
            .or(self.layout.min_primary_column_width)
            .unwrap_or(DEFAULT_PRIMARY_COLUMN_WIDTH);
        (raw_min.min(raw_max).max(1), raw_min.max(raw_max).max(1))
    }

    fn get_primary_column_width(&self) -> usize {
        let (min, max) = self.get_primary_column_bounds();
        let widest_primary = self.filtered_items.iter().fold(0usize, |widest, item| {
            widest.max(visible_width(Self::get_display_value(item)) + PRIMARY_COLUMN_GAP)
        });
        clamp(widest_primary, min, max)
    }

    fn truncate_primary(
        &self,
        item: &SelectItem,
        is_selected: bool,
        max_width: usize,
        column_width: usize,
    ) -> String {
        let display_value = Self::get_display_value(item);
        let truncated = match &self.layout.truncate_primary {
            Some(f) => f(&SelectListTruncatePrimaryContext {
                text: display_value,
                max_width,
                column_width,
                item,
                is_selected,
            }),
            None => truncate_to_width(display_value, max_width, "", false),
        };
        truncate_to_width(&truncated, max_width, "", false)
    }

    fn render_item(
        &self,
        item: &SelectItem,
        is_selected: bool,
        width: usize,
        description_single_line: Option<&str>,
        primary_column_width: usize,
    ) -> String {
        let prefix = if is_selected { "→ " } else { "  " };
        let prefix_width = visible_width(prefix);

        if let Some(desc) = description_single_line {
            if width > 40 {
                let effective_primary_column_width = primary_column_width
                    .min(width.saturating_sub(prefix_width).saturating_sub(4))
                    .max(1);
                let max_primary_width = effective_primary_column_width
                    .saturating_sub(PRIMARY_COLUMN_GAP)
                    .max(1);
                let truncated_value = self.truncate_primary(
                    item,
                    is_selected,
                    max_primary_width,
                    effective_primary_column_width,
                );
                let truncated_value_width = visible_width(&truncated_value);
                let spacing_len = effective_primary_column_width
                    .saturating_sub(truncated_value_width)
                    .max(1);
                let spacing = " ".repeat(spacing_len);
                let description_start = prefix_width + truncated_value_width + spacing.len();
                let remaining_width = (width as i64 - description_start as i64 - 2).max(0) as usize;

                if remaining_width > MIN_DESCRIPTION_WIDTH {
                    let truncated_desc = truncate_to_width(desc, remaining_width, "", false);
                    if is_selected {
                        return (self.theme.selected_text)(&format!(
                            "{prefix}{truncated_value}{spacing}{truncated_desc}"
                        ));
                    }
                    let desc_text = (self.theme.description)(&format!("{spacing}{truncated_desc}"));
                    return format!("{prefix}{truncated_value}{desc_text}");
                }
            }
        }

        let max_width = width.saturating_sub(prefix_width).saturating_sub(2).max(1);
        let truncated_value = self.truncate_primary(item, is_selected, max_width, max_width);
        if is_selected {
            (self.theme.selected_text)(&format!("{prefix}{truncated_value}"))
        } else {
            format!("{prefix}{truncated_value}")
        }
    }
}

impl Component for SelectList {
    fn invalidate(&mut self) {}

    /// `render` (select-list.ts:74).
    fn render(&mut self, width: usize) -> Vec<String> {
        let mut lines = Vec::new();

        if self.filtered_items.is_empty() {
            lines.push((self.theme.no_match)("  No matching commands"));
            return lines;
        }

        let primary_column_width = self.get_primary_column_width();

        let start_index = self
            .selected_index
            .saturating_sub(self.max_visible / 2)
            .min(self.filtered_items.len().saturating_sub(self.max_visible));
        let end_index = (start_index + self.max_visible).min(self.filtered_items.len());

        for i in start_index..end_index {
            let item = self.filtered_items[i].clone();
            let is_selected = i == self.selected_index;
            let description_single_line = item.description.as_deref().map(normalize_to_single_line);
            lines.push(self.render_item(
                &item,
                is_selected,
                width,
                description_single_line.as_deref(),
                primary_column_width,
            ));
        }

        if start_index > 0 || end_index < self.filtered_items.len() {
            let scroll_text = format!(
                "  ({}/{})",
                self.selected_index + 1,
                self.filtered_items.len()
            );
            lines.push((self.theme.scroll_info)(&truncate_to_width(
                &scroll_text,
                width.saturating_sub(2),
                "",
                false,
            )));
        }

        lines
    }

    /// `handleInput` (select-list.ts:112).
    fn handle_input(&mut self, key_data: &str) {
        let kb = get_keybindings();
        if kb.matches(key_data, Keybinding::SelectUp) {
            drop(kb);
            self.selected_index = if self.selected_index == 0 {
                self.filtered_items.len().saturating_sub(1)
            } else {
                self.selected_index - 1
            };
            self.notify_selection_change();
        } else if kb.matches(key_data, Keybinding::SelectDown) {
            drop(kb);
            self.selected_index = if self.selected_index + 1 >= self.filtered_items.len() {
                0
            } else {
                self.selected_index + 1
            };
            self.notify_selection_change();
        } else if kb.matches(key_data, Keybinding::SelectConfirm) {
            drop(kb);
            if let Some(item) = self.filtered_items.get(self.selected_index).cloned() {
                if let Some(cb) = &mut self.on_select {
                    cb(&item);
                }
            }
        } else if kb.matches(key_data, Keybinding::SelectCancel) {
            drop(kb);
            if let Some(cb) = &mut self.on_cancel {
                cb();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme() -> SelectListTheme {
        SelectListTheme {
            selected_prefix: Box::new(|s| s.to_string()),
            selected_text: Box::new(|s| s.to_string()),
            description: Box::new(|s| s.to_string()),
            scroll_info: Box::new(|s| s.to_string()),
            no_match: Box::new(|s| s.to_string()),
        }
    }

    fn items(n: usize) -> Vec<SelectItem> {
        (0..n)
            .map(|i| SelectItem {
                value: format!("item{i}"),
                label: format!("Item {i}"),
                description: None,
            })
            .collect()
    }

    #[test]
    fn empty_filtered_list_shows_no_match() {
        let mut list = SelectList::new(vec![], 5, theme(), SelectListLayoutOptions::default());
        let lines = list.render(40);
        assert_eq!(lines, vec!["  No matching commands".to_string()]);
    }

    #[test]
    fn down_wraps_to_top() {
        let mut list = SelectList::new(items(3), 5, theme(), SelectListLayoutOptions::default());
        list.set_selected_index(2);
        list.handle_input("\x1b[B"); // down arrow
        assert_eq!(list.selected_index, 0);
    }

    #[test]
    fn up_wraps_to_bottom() {
        let mut list = SelectList::new(items(3), 5, theme(), SelectListLayoutOptions::default());
        list.handle_input("\x1b[A"); // up arrow
        assert_eq!(list.selected_index, 2);
    }

    #[test]
    fn confirm_fires_on_select_with_current_item() {
        let mut list = SelectList::new(items(3), 5, theme(), SelectListLayoutOptions::default());
        let selected = std::rc::Rc::new(std::cell::RefCell::new(None));
        let selected_clone = selected.clone();
        list.on_select = Some(Box::new(move |item| {
            *selected_clone.borrow_mut() = Some(item.value.clone());
        }));
        list.handle_input("\r"); // enter
        assert_eq!(selected.borrow().as_deref(), Some("item0"));
    }

    #[test]
    fn set_filter_narrows_and_resets_selection() {
        let mut list = SelectList::new(items(3), 5, theme(), SelectListLayoutOptions::default());
        list.set_selected_index(2);
        list.set_filter("item1");
        assert_eq!(list.filtered_items.len(), 1);
        assert_eq!(list.selected_index, 0);
    }

    #[test]
    fn scroll_indicator_appears_when_items_exceed_max_visible() {
        let mut list = SelectList::new(items(10), 3, theme(), SelectListLayoutOptions::default());
        let lines = list.render(40);
        assert_eq!(lines.len(), 4); // 3 visible + scroll indicator
    }
}
