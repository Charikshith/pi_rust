//! Port of `packages/tui/src/components/` — the ready-made component
//! library. See `docs/analysis/05-tui.md` §6.

/// A single-argument theme color/formatting function
/// (`(text: string) => string`), shared across every component that takes
/// one — factored into a `type` alias per `clippy::type_complexity`.
pub type ColorFn = Box<dyn Fn(&str) -> String>;

pub mod box_component;
pub mod cancellable_loader;
pub mod image;
pub mod input;
pub mod loader;
pub mod select_list;
pub mod settings_list;
pub mod spacer;
pub mod text;
pub mod truncated_text;
