//! `pirust-extension-api` — the pirust port of the Pi extension host
//! surface (`packages/coding-agent/src/core/extensions/`, 3,964 lines).
//!
//! This crate mirrors the public types + runner semantics; the mode-specific
//! glue (UI context, session manager, model registry) is bound by
//! `pirust-coding-agent` in Wave 6.
//!
//! - [`events`]: `ExtensionEvent` union + discriminators
//! - [`context`]: `ExtensionContext` / `ExtensionCommandContext` + result types
//! - [`registration`]: `ToolDefinition`, `RegisteredCommand`,
//!   `ExtensionShortcut`, `ExtensionFlag`, `ExtensionApi`, `Extension`
//! - [`runner`]: `ExtensionRunner` with Pi's exact dispatch semantics
//! - [`loader`]: built-in (compile-time) loader

pub mod context;
pub mod events;
pub mod loader;
pub mod plan_mode;
pub mod plan_mode_extension;
pub mod registration;
pub mod runner;

pub use context::*;
pub use events::*;
pub use loader::*;
pub use plan_mode::*;
pub use plan_mode_extension::*;
pub use registration::*;
pub use runner::*;
