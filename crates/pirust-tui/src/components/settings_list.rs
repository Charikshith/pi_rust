//! Port of `packages/tui/src/components/settings-list.ts` — an interactive
//! key/value settings editor list with optional fuzzy-filtered search and
//! submenu support. See `docs/analysis/05-tui.md` §6.
//!
//! ## Scope decisions (documented, not silent — AGENTS.md Correctness Bar)
//!
//! - **`SettingItem.submenu`'s factory returns a `SharedComponent`.** The TS
//!   `submenu?: (currentValue, done) => Component` plugs directly back into
//!   this crate's `Rc<RefCell<dyn Component>>` component tree (`tui.rs`,
//!   Wave 4) — the factory closure here is
//!   `Box<dyn FnMut(&str, SubmenuDone) -> SharedComponent>` so a submenu
//!   returned from it composes into the same tree any other component does.

use std::cell::RefCell;
use std::rc::Rc;

use crate::fuzzy::fuzzy_filter;
use crate::keybindings::{get_keybindings, Keybinding};
use crate::tui::{Component, SharedComponent};
use crate::utils::{truncate_to_width, visible_width, wrap_text_with_ansi};

use super::input::Input;

/// Callback a submenu invokes to report its result and close itself
/// (`done: (selectedValue?: string) => void`, settings-list.ts:19).
pub type SubmenuDone = Box<dyn Fn(Option<String>)>;
/// `submenu?: (currentValue, done) => Component` (settings-list.ts:19).
pub type SubmenuFactory = Box<dyn FnMut(&str, SubmenuDone) -> SharedComponent>;
/// A theme formatter taking the rendered text plus whether the row is
/// selected (`(text: string, selected: boolean) => string`).
pub type SelectedTextFn = Box<dyn Fn(&str, bool) -> String>;
/// `onChange: (id: string, newValue: string) => void` (settings-list.ts:53),
/// factored into a `type` alias per `clippy::type_complexity`.
pub type OnChangeFnMut = Box<dyn FnMut(&str, &str)>;

/// `SettingItem` (settings-list.ts:7).
pub struct SettingItem {
    pub id: String,
    pub label: String,
    pub description: Option<String>,
    pub current_value: String,
    pub values: Option<Vec<String>>,
    pub submenu: Option<SubmenuFactory>,
}

/// `SettingsListTheme` (settings-list.ts:22).
pub struct SettingsListTheme {
    pub label: SelectedTextFn,
    pub value: SelectedTextFn,
    pub description: Box<dyn Fn(&str) -> String>,
    pub cursor: String,
    pub hint: Box<dyn Fn(&str) -> String>,
}

/// `SettingsListOptions` (settings-list.ts:30).
#[derive(Default)]
pub struct SettingsListOptions {
    pub enable_search: bool,
}

/// `SettingsList` (settings-list.ts:34).
pub struct SettingsList {
    items: Vec<SettingItem>,
    filtered_indices: Vec<usize>,
    theme: SettingsListTheme,
    selected_index: usize,
    max_visible: usize,
    on_change: OnChangeFnMut,
    on_cancel: Box<dyn FnMut()>,
    search_input: Option<Input>,
    search_enabled: bool,
    submenu_component: Option<SharedComponent>,
    submenu_item_index: Option<usize>,
    /// Pending result from a submenu's `done()` call, drained by the caller
    /// via [`SettingsList::drain_submenu_result`] — see module docs;
    /// the TS's `done` closure mutates `this` (the `SettingsList`) directly,
    /// which this port cannot do from inside a `Box<dyn Fn>` invoked through
    /// a `SharedComponent`'s `RefCell` borrow without a queue.
    pending_submenu_result: Rc<RefCell<Option<Option<String>>>>,
}

impl SettingsList {
    /// `constructor` (settings-list.ts:49).
    pub fn new(
        items: Vec<SettingItem>,
        max_visible: usize,
        theme: SettingsListTheme,
        on_change: OnChangeFnMut,
        on_cancel: Box<dyn FnMut()>,
        options: SettingsListOptions,
    ) -> Self {
        let filtered_indices = (0..items.len()).collect();
        let search_enabled = options.enable_search;
        Self {
            items,
            filtered_indices,
            theme,
            selected_index: 0,
            max_visible,
            on_change,
            on_cancel,
            search_input: if search_enabled {
                Some(Input::new())
            } else {
                None
            },
            search_enabled,
            submenu_component: None,
            submenu_item_index: None,
            pending_submenu_result: Rc::new(RefCell::new(None)),
        }
    }

    /// `updateValue` (settings-list.ts:70).
    pub fn update_value(&mut self, id: &str, new_value: &str) {
        if let Some(item) = self.items.iter_mut().find(|i| i.id == id) {
            item.current_value = new_value.to_string();
        }
    }

    fn current_display_indices(&self) -> Vec<usize> {
        if self.search_enabled {
            self.filtered_indices.clone()
        } else {
            (0..self.items.len()).collect()
        }
    }

    fn apply_filter(&mut self, query: &str) {
        let indices: Vec<usize> = (0..self.items.len()).collect();
        self.filtered_indices = fuzzy_filter(&indices, query, |&i| self.items[i].label.clone())
            .into_iter()
            .copied()
            .collect();
        self.selected_index = 0;
    }

    fn add_hint_line(&self, lines: &mut Vec<String>, width: usize) {
        lines.push(String::new());
        let hint_text = if self.search_enabled {
            "  Type to search · Enter/Space to change · Esc to cancel"
        } else {
            "  Enter/Space to change · Esc to cancel"
        };
        lines.push(truncate_to_width(
            &(self.theme.hint)(hint_text),
            width,
            "...",
            false,
        ));
    }

    fn render_main_list(&mut self, width: usize) -> Vec<String> {
        let mut lines = Vec::new();

        if let Some(search) = &mut self.search_input {
            if self.search_enabled {
                lines.extend(search.render(width));
                lines.push(String::new());
            }
        }

        if self.items.is_empty() {
            lines.push((self.theme.hint)("  No settings available"));
            if self.search_enabled {
                self.add_hint_line(&mut lines, width);
            }
            return lines;
        }

        let display_indices = self.current_display_indices();
        if display_indices.is_empty() {
            lines.push(truncate_to_width(
                &(self.theme.hint)("  No matching settings"),
                width,
                "...",
                false,
            ));
            self.add_hint_line(&mut lines, width);
            return lines;
        }

        let start_index = self
            .selected_index
            .saturating_sub(self.max_visible / 2)
            .min(display_indices.len().saturating_sub(self.max_visible));
        let end_index = (start_index + self.max_visible).min(display_indices.len());

        let max_label_width = self
            .items
            .iter()
            .map(|item| visible_width(&item.label))
            .max()
            .unwrap_or(0)
            .min(30);

        for (i, &item_idx) in display_indices[start_index..end_index]
            .iter()
            .enumerate()
            .map(|(offset, idx)| (start_index + offset, idx))
        {
            let item = &self.items[item_idx];
            let is_selected = i == self.selected_index;
            let prefix = if is_selected {
                self.theme.cursor.clone()
            } else {
                "  ".to_string()
            };
            let prefix_width = visible_width(&prefix);

            let label_padded = format!(
                "{}{}",
                item.label,
                " ".repeat(max_label_width.saturating_sub(visible_width(&item.label)))
            );
            let label_text = (self.theme.label)(&label_padded, is_selected);

            let separator = "  ";
            let used_width = prefix_width + max_label_width + visible_width(separator);
            let value_max_width = (width as i64 - used_width as i64 - 2).max(0) as usize;

            let value_text = (self.theme.value)(
                &truncate_to_width(&item.current_value, value_max_width, "", false),
                is_selected,
            );

            lines.push(truncate_to_width(
                &format!("{prefix}{label_text}{separator}{value_text}"),
                width,
                "...",
                false,
            ));
        }

        if start_index > 0 || end_index < display_indices.len() {
            let scroll_text = format!("  ({}/{})", self.selected_index + 1, display_indices.len());
            lines.push((self.theme.hint)(&truncate_to_width(
                &scroll_text,
                width.saturating_sub(2),
                "",
                false,
            )));
        }

        if let Some(&selected_idx) = display_indices.get(self.selected_index) {
            if let Some(desc) = &self.items[selected_idx].description {
                lines.push(String::new());
                let wrapped = wrap_text_with_ansi(desc, width.saturating_sub(4));
                for line in wrapped {
                    lines.push((self.theme.description)(&format!("  {line}")));
                }
            }
        }

        self.add_hint_line(&mut lines, width);

        lines
    }

    fn activate_item(&mut self) {
        let display_indices = self.current_display_indices();
        let Some(&item_idx) = display_indices.get(self.selected_index) else {
            return;
        };

        let has_submenu = self.items[item_idx].submenu.is_some();
        if has_submenu {
            self.submenu_item_index = Some(self.selected_index);
            let current_value = self.items[item_idx].current_value.clone();
            let pending = self.pending_submenu_result.clone();
            let done: SubmenuDone = Box::new(move |selected_value| {
                *pending.borrow_mut() = Some(selected_value);
            });
            let factory = self.items[item_idx].submenu.as_mut().unwrap();
            self.submenu_component = Some(factory(&current_value, done));
        } else if let Some(values) = self.items[item_idx].values.clone() {
            if !values.is_empty() {
                let current_index = values
                    .iter()
                    .position(|v| v == &self.items[item_idx].current_value)
                    .map(|i| i as i64)
                    .unwrap_or(-1);
                let next_index = ((current_index + 1) as usize) % values.len();
                let new_value = values[next_index].clone();
                self.items[item_idx].current_value = new_value.clone();
                (self.on_change)(&self.items[item_idx].id.clone(), &new_value);
            }
        }
    }

    fn close_submenu(&mut self) {
        self.submenu_component = None;
        if let Some(idx) = self.submenu_item_index.take() {
            self.selected_index = idx;
        }
    }

    /// Drains a result reported by the active submenu's `done()` callback
    /// (see module docs) and applies it exactly like the TS's inline
    /// closure does, then closes the submenu. A caller that owns this
    /// `SettingsList` as a `SharedComponent` should call this after each
    /// `handle_input` dispatch that might have reached the submenu.
    pub fn drain_submenu_result(&mut self) {
        let Some(result) = self.pending_submenu_result.borrow_mut().take() else {
            return;
        };
        if let Some(selected_value) = result {
            if let Some(idx) = self.submenu_item_index {
                if let Some(&item_idx) = self.current_display_indices().get(idx) {
                    self.items[item_idx].current_value = selected_value.clone();
                    let id = self.items[item_idx].id.clone();
                    (self.on_change)(&id, &selected_value);
                }
            }
        }
        self.close_submenu();
    }
}

impl Component for SettingsList {
    fn invalidate(&mut self) {
        if let Some(submenu) = &self.submenu_component {
            submenu.borrow_mut().invalidate();
        }
    }

    /// `render` (settings-list.ts:81).
    fn render(&mut self, width: usize) -> Vec<String> {
        if let Some(submenu) = &self.submenu_component {
            return submenu.borrow_mut().render(width);
        }
        self.render_main_list(width)
    }

    /// `handleInput` (settings-list.ts:168).
    fn handle_input(&mut self, data: &str) {
        if let Some(submenu) = &self.submenu_component {
            submenu.borrow_mut().handle_input(data);
            self.drain_submenu_result();
            return;
        }

        let kb = get_keybindings();
        let display_len = self.current_display_indices().len();
        if kb.matches(data, Keybinding::SelectUp) {
            drop(kb);
            if display_len == 0 {
                return;
            }
            self.selected_index = if self.selected_index == 0 {
                display_len - 1
            } else {
                self.selected_index - 1
            };
        } else if kb.matches(data, Keybinding::SelectDown) {
            drop(kb);
            if display_len == 0 {
                return;
            }
            self.selected_index = if self.selected_index + 1 >= display_len {
                0
            } else {
                self.selected_index + 1
            };
        } else if kb.matches(data, Keybinding::SelectConfirm) || data == " " {
            drop(kb);
            self.activate_item();
        } else if kb.matches(data, Keybinding::SelectCancel) {
            drop(kb);
            (self.on_cancel)();
        } else if self.search_enabled {
            drop(kb);
            let sanitized = data.replace(' ', "");
            if sanitized.is_empty() {
                return;
            }
            let query = if let Some(search) = &mut self.search_input {
                search.handle_input(&sanitized);
                search.get_value().to_string()
            } else {
                return;
            };
            self.apply_filter(&query);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme() -> SettingsListTheme {
        SettingsListTheme {
            label: Box::new(|s, _| s.to_string()),
            value: Box::new(|s, _| s.to_string()),
            description: Box::new(|s| s.to_string()),
            cursor: "> ".to_string(),
            hint: Box::new(|s| s.to_string()),
        }
    }

    fn item(id: &str, label: &str, current: &str, values: Option<Vec<&str>>) -> SettingItem {
        SettingItem {
            id: id.to_string(),
            label: label.to_string(),
            description: None,
            current_value: current.to_string(),
            values: values.map(|v| v.into_iter().map(String::from).collect()),
            submenu: None,
        }
    }

    #[test]
    fn cycles_through_values_on_confirm() {
        let changes = Rc::new(RefCell::new(Vec::new()));
        let changes_clone = changes.clone();
        let mut list = SettingsList::new(
            vec![item("a", "A", "x", Some(vec!["x", "y", "z"]))],
            5,
            theme(),
            Box::new(move |id, val| {
                changes_clone
                    .borrow_mut()
                    .push((id.to_string(), val.to_string()))
            }),
            Box::new(|| {}),
            SettingsListOptions::default(),
        );
        list.handle_input(" ");
        assert_eq!(changes.borrow()[0], ("a".to_string(), "y".to_string()));
    }

    #[test]
    fn cancel_fires_callback() {
        let cancelled = Rc::new(RefCell::new(false));
        let cancelled_clone = cancelled.clone();
        let mut list = SettingsList::new(
            vec![item("a", "A", "x", None)],
            5,
            theme(),
            Box::new(|_, _| {}),
            Box::new(move || *cancelled_clone.borrow_mut() = true),
            SettingsListOptions::default(),
        );
        list.handle_input("\x1b");
        assert!(*cancelled.borrow());
    }

    #[test]
    fn no_settings_shows_hint() {
        let mut list = SettingsList::new(
            vec![],
            5,
            theme(),
            Box::new(|_, _| {}),
            Box::new(|| {}),
            SettingsListOptions::default(),
        );
        let lines = list.render(40);
        assert!(lines[0].contains("No settings available"));
    }

    #[test]
    fn search_filters_by_label() {
        let mut list = SettingsList::new(
            vec![item("a", "Alpha", "1", None), item("b", "Beta", "2", None)],
            5,
            theme(),
            Box::new(|_, _| {}),
            Box::new(|| {}),
            SettingsListOptions {
                enable_search: true,
            },
        );
        list.handle_input("Beta");
        assert_eq!(list.current_display_indices(), vec![1]);
    }
}
