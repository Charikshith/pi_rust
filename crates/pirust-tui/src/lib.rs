//! `pirust-tui` — self-contained inline terminal UI library.
//!
//! pirust port of `packages/tui` (`@earendil-works/pi-tui`). See
//! `docs/analysis/05-tui.md`. Ported literally (NOT ratatui); `crossterm` is a
//! thin syscall shim only. Renderer + editor land in feat-006.

#![forbid(unsafe_code)]

pub mod autocomplete;
pub mod components;
pub mod editor_component;
pub mod fuzzy;
pub mod keybindings;
pub mod keys;
pub mod kill_ring;
pub mod stdin_buffer;
pub mod terminal;
pub mod terminal_colors;
pub mod terminal_image;
pub mod tui;
pub mod undo_stack;
pub mod utils;
pub mod word_navigation;

pub use autocomplete::{
    AppliedCompletion, AutocompleteItem, AutocompleteProvider, AutocompleteSuggestions,
    CombinedAutocompleteProvider, CommandOrItem, CompletionContext, SlashCommand,
};
pub use components::box_component::BoxComponent;
pub use components::cancellable_loader::CancellableLoader;
pub use components::image::{Image, ImageOptions, ImageTheme};
pub use components::input::Input;
pub use components::loader::{Loader, LoaderIndicatorOptions};
pub use components::select_list::{
    SelectItem, SelectList, SelectListLayoutOptions, SelectListTheme,
    SelectListTruncatePrimaryContext,
};
pub use components::settings_list::{
    SettingItem, SettingsList, SettingsListOptions, SettingsListTheme, SubmenuDone, SubmenuFactory,
};
pub use components::spacer::Spacer;
pub use components::text::Text;
pub use components::truncated_text::TruncatedText;
pub use editor_component::EditorComponent;
pub use fuzzy::{fuzzy_filter, fuzzy_match, FuzzyMatch};
pub use keybindings::{
    get_keybindings, set_keybindings, Keybinding, KeybindingConflict, KeybindingDefinition,
    KeybindingsManager, RawKeys,
};
pub use keys::{
    decode_kitty_printable, decode_printable_key, is_key_release, is_key_repeat,
    is_kitty_protocol_active, matches_key, parse_key, set_kitty_protocol_active,
};
pub use stdin_buffer::{StdinBuffer, StdinBufferOptions, StdinEvent};
pub use terminal::{ProcessTerminal, Terminal};
pub use tui::{
    is_focusable, Component, Container, Focusable, OverlayAnchor, OverlayId, OverlayMargin,
    OverlayMarginValue, OverlayOptions, OverlayUnfocusOptions, SharedComponent, SizeValue,
    CURSOR_MARKER, TUI,
};
pub use utils::{slice_by_column, truncate_to_width, visible_width, wrap_text_with_ansi};

/// Returns the crate name — placeholder until the renderer (feat-006) lands.
pub fn name() -> &'static str {
    "pirust-tui"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_is_stable() {
        assert_eq!(name(), "pirust-tui");
    }
}
