//! Port of `packages/tui/src/editor-component.ts` — the seam that lets
//! extensions provide a custom editor (vim/emacs mode) while staying
//! compatible with the core application. See `docs/analysis/05-tui.md` §2.
//! No implementer yet — `editor.rs`'s `Editor` (Wave 6) is the first.
//!
//! ## Scope decisions (documented, not silent — AGENTS.md Correctness Bar)
//!
//! - **Optional TS interface methods → default trait-method implementations**
//!   returning `None`/no-op, the same idiom `tui::Component`'s
//!   `handle_input`/`as_focusable_mut` and `autocomplete::AutocompleteProvider`
//!   (this wave) already use — an implementer only overrides what it
//!   actually supports.

use crate::autocomplete::AutocompleteProvider;
use crate::components::input::OnTextFnMut;
use crate::components::ColorFn;
use crate::tui::Component;

/// `EditorComponent extends Component` (editor-component.ts:11).
pub trait EditorComponent: Component {
    /// `getText` (editor-component.ts:17) — required.
    fn get_text(&self) -> String;
    /// `setText` (editor-component.ts:20) — required.
    fn set_text(&mut self, text: &str);

    /// `onSubmit` (editor-component.ts:30) — optional; default no-op.
    fn set_on_submit(&mut self, _callback: Option<OnTextFnMut>) {}
    /// `onChange` (editor-component.ts:33) — optional; default no-op.
    fn set_on_change(&mut self, _callback: Option<OnTextFnMut>) {}

    /// `addToHistory` (editor-component.ts:40) — optional; default no-op.
    fn add_to_history(&mut self, _text: &str) {}

    /// `insertTextAtCursor` (editor-component.ts:47) — optional; default no-op.
    fn insert_text_at_cursor(&mut self, _text: &str) {}
    /// `getExpandedText` (editor-component.ts:53) — optional; falls back to
    /// `getText()` if not overridden, matching the TS doc comment's stated
    /// fallback contract exactly.
    fn get_expanded_text(&self) -> String {
        self.get_text()
    }

    /// `setAutocompleteProvider` (editor-component.ts:60) — optional; default no-op.
    fn set_autocomplete_provider(&mut self, _provider: Option<Box<dyn AutocompleteProvider>>) {}

    /// `borderColor` (editor-component.ts:67) — optional; default: no override.
    fn set_border_color(&mut self, _color_fn: Option<ColorFn>) {}
    /// `setPaddingX` (editor-component.ts:70) — optional; default no-op.
    fn set_padding_x(&mut self, _padding: usize) {}
    /// `setAutocompleteMaxVisible` (editor-component.ts:73) — optional; default no-op.
    fn set_autocomplete_max_visible(&mut self, _max_visible: usize) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Minimal {
        text: String,
    }

    impl Component for Minimal {
        fn render(&mut self, _width: usize) -> Vec<String> {
            vec![self.text.clone()]
        }
        fn invalidate(&mut self) {}
    }

    impl EditorComponent for Minimal {
        fn get_text(&self) -> String {
            self.text.clone()
        }
        fn set_text(&mut self, text: &str) {
            self.text = text.to_string();
        }
    }

    #[test]
    fn get_expanded_text_falls_back_to_get_text_by_default() {
        let mut m = Minimal {
            text: String::new(),
        };
        m.set_text("hello");
        assert_eq!(m.get_expanded_text(), "hello");
    }

    #[test]
    fn optional_methods_are_no_ops_by_default() {
        let mut m = Minimal {
            text: String::new(),
        };
        m.add_to_history("x");
        m.insert_text_at_cursor("y");
        m.set_padding_x(4);
        m.set_autocomplete_max_visible(10);
        // No panics, no observable state change — the point of the default.
        assert_eq!(m.get_text(), "");
    }
}
