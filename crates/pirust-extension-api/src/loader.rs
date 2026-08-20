//! Built-in (compile-time) extension loader.
//!
//! Pi loads extensions dynamically (loader.ts: dynamic import). The Rust
//! port's first loader is compile-time: extensions are Rust closures given
//! an `ExtensionApi` at startup. Dynamic/WASM loading is out of scope
//! (feat-007 scope boundary; P9).
//!
//! `InlineExtension` mirrors Pi's `InlineExtension` (types.ts:1591):
//! a factory function plus an optional display name and hidden flag. The
//! built-in list mirrors `packages/coding-agent/src/extensions/index.ts`.

use crate::registration::ExtensionApi;

/// `ExtensionFactory` (types.ts:1589) — sync factory taking the API.
pub type ExtensionFactory = Box<dyn Fn(&mut ExtensionApi) -> Result<(), String> + Send + Sync>;

/// `InlineExtension` (types.ts:1591).
pub struct InlineExtension {
    /// Display name shown as `<inline:name>` in the startup Extensions list.
    pub name: String,
    pub factory: ExtensionFactory,
    /// Omit this extension from the startup Extensions list.
    pub hidden: bool,
}

impl InlineExtension {
    pub fn new(name: impl Into<String>, factory: ExtensionFactory) -> Self {
        Self {
            name: name.into(),
            factory,
            hidden: false,
        }
    }

    /// `hidden: true` — for internal built-ins not shown to the user.
    pub fn hidden(name: impl Into<String>, factory: ExtensionFactory) -> Self {
        Self {
            name: name.into(),
            factory,
            hidden: true,
        }
    }
}

/// The built-in extensions list (mirrors `builtInExtensions`,
/// extensions/index.ts).
pub fn built_in_extensions() -> Vec<InlineExtension> {
    vec![crate::plan_mode_extension::plan_mode_extension()]
}

/// Run an `InlineExtension` factory against a fresh [`crate::registration::Extension`],
/// returning the loaded extension. Mirrors the loader's "load factory → extension
/// object" step (used by tests and the built-in loader).
pub fn load(factory: &InlineExtension, cwd: &str) -> crate::registration::Extension {
    let mut ext = crate::registration::Extension {
        path: format!("<inline:{}>", factory.name),
        resolved_path: format!("<inline:{}>", factory.name),
        hidden: factory.hidden,
        handlers: std::collections::HashMap::new(),
        tools: std::collections::HashMap::new(),
        commands: std::collections::HashMap::new(),
        flags: std::collections::HashMap::new(),
        shortcuts: std::collections::HashMap::new(),
    };
    let mut api = crate::registration::ExtensionApi {
        extension: &mut ext,
        cwd: cwd.to_string(),
        assert_active: Box::new(|| {}),
    };
    (factory.factory)(&mut api).expect("extension factory ran");
    ext
}
