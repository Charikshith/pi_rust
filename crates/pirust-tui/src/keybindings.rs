//! Port of `packages/tui/src/keybindings.ts` — the global keybinding
//! registry: semantic ids (`tui.editor.cursorUp`, ...) mapped to one or more
//! `KeyId` strings, with user-override merging and conflict detection. See
//! `docs/analysis/05-tui.md` §2/§9.
//!
//! ## Scope decisions (documented, not silent — AGENTS.md Correctness Bar)
//!
//! - **`Keybinding` is a Rust enum, unlike `KeyId` (`keys.rs`, Wave 2).**
//!   `Keybinding` (`keyof Keybindings`) is a *closed, fixed* set of 31
//!   semantic ids, unlike `KeyId`'s combinatorial modifier+key string space —
//!   exactly the kind of closed enumeration Rust enums exist for. This buys
//!   real compile-time safety for a fixed, load-bearing vocabulary with no
//!   combinatorial blowup, unlike `keys.rs`'s `Key` builder (skipped there for
//!   being speculative TS-only autocomplete sugar over an unbounded space).
//! - **`definitions` is not a constructor parameter.** The TS
//!   `KeybindingsManager` is generic over any `KeybindingDefinitions` map, but
//!   every real call site (`getKeybindings()`'s lazy default) — and this
//!   wave's own oracle — constructs it with exactly `TUI_KEYBINDINGS`. Since
//!   `Keybinding` already IS that fixed set, a `definitions` parameter that
//!   would only ever hold one value adds no tested behavior (Ponytail rung 1).
//!   [`KeybindingsManager::new`] takes only `user_bindings`; `TUI_KEYBINDINGS`
//!   is used internally. Downstream extension-package declaration-merging
//!   (TS's mechanism for *adding* new ids to `Keybindings`) has no Rust
//!   analogue and is out of this wave's scope — not named by any spec doc.
//! - **Global singleton via `LazyLock<Mutex<KeybindingsManager>>`.** The TS
//!   module-level `let globalKeybindings: KeybindingsManager | null = null`
//!   lazily constructs a default on first `getKeybindings()` call and is
//!   replaceable via `setKeybindings()`. [`get_keybindings`] returns a
//!   `MutexGuard` (derefs to `&`/`&mut KeybindingsManager`) rather than an
//!   owned value or `&'static` reference, since Rust has no equivalent of a
//!   freely-aliased mutable JS object reference — semantically the same
//!   shared, mutable singleton, just accessed through a lock guard.
//! - **Conflict-list order is not asserted exactly against the oracle.**
//!   `getConflicts()`'s claimant order in the TS follows `Map` insertion
//!   order, which in turn follows the *user-supplied config object's own key
//!   order*. That order survives the JS-side oracle computation, but is lost
//!   crossing the JSON fixture boundary (`serde_json::Value::Object` has no
//!   `preserve_order` feature enabled in this workspace, so object-key order
//!   isn't preserved on the Rust side either). This affects ONLY the relative
//!   order of tied claimants within a single conflict's `keybindings` list —
//!   never which keybindings conflict, nor per-binding key-list order (JSON
//!   *arrays* do preserve order, and `normalizeKeys`'s own dedup-preserving
//!   order is asserted exactly). The golden test sorts conflict claimant lists
//!   before comparing, documented there as an oracle-fidelity limitation, not
//!   a Rust/TS behavioral divergence.

use std::collections::{HashMap, HashSet};
use std::sync::{LazyLock, Mutex, MutexGuard};

use crate::keys::matches_key;

/// The 31 semantic keybinding ids (`Keybinding = keyof Keybindings`,
/// keybindings.ts:7-44), in the TS source's declaration order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Keybinding {
    EditorCursorUp,
    EditorCursorDown,
    EditorHistoryPrevious,
    EditorHistoryNext,
    EditorCursorLeft,
    EditorCursorRight,
    EditorCursorWordLeft,
    EditorCursorWordRight,
    EditorCursorLineStart,
    EditorCursorLineEnd,
    EditorJumpForward,
    EditorJumpBackward,
    EditorPageUp,
    EditorPageDown,
    EditorDeleteCharBackward,
    EditorDeleteCharForward,
    EditorDeleteWordBackward,
    EditorDeleteWordForward,
    EditorDeleteToLineStart,
    EditorDeleteToLineEnd,
    EditorYank,
    EditorYankPop,
    EditorUndo,
    InputNewLine,
    InputSubmit,
    InputTab,
    InputCopy,
    SelectUp,
    SelectDown,
    SelectPageUp,
    SelectPageDown,
    SelectConfirm,
    SelectCancel,
    AltScreenPageUp,
    AltScreenPageDown,
    AltScreenHalfPageUp,
    AltScreenHalfPageDown,
    AltScreenLineUp,
    AltScreenLineDown,
    AltScreenPreviousPrompt,
    AltScreenNextPrompt,
    AltScreenSearch,
    AltScreenSearchNext,
    AltScreenSearchPrevious,
    AltScreenSearchClose,
    AltScreenTop,
    AltScreenBottom,
}

impl Keybinding {
    /// All variants, in TS declaration order (matches `Object.entries`
    /// iteration order over `TUI_KEYBINDINGS`).
    pub const ALL: &'static [Keybinding] = &[
        Keybinding::EditorCursorUp,
        Keybinding::EditorCursorDown,
        Keybinding::EditorHistoryPrevious,
        Keybinding::EditorHistoryNext,
        Keybinding::EditorCursorLeft,
        Keybinding::EditorCursorRight,
        Keybinding::EditorCursorWordLeft,
        Keybinding::EditorCursorWordRight,
        Keybinding::EditorCursorLineStart,
        Keybinding::EditorCursorLineEnd,
        Keybinding::EditorJumpForward,
        Keybinding::EditorJumpBackward,
        Keybinding::EditorPageUp,
        Keybinding::EditorPageDown,
        Keybinding::EditorDeleteCharBackward,
        Keybinding::EditorDeleteCharForward,
        Keybinding::EditorDeleteWordBackward,
        Keybinding::EditorDeleteWordForward,
        Keybinding::EditorDeleteToLineStart,
        Keybinding::EditorDeleteToLineEnd,
        Keybinding::EditorYank,
        Keybinding::EditorYankPop,
        Keybinding::EditorUndo,
        Keybinding::InputNewLine,
        Keybinding::InputSubmit,
        Keybinding::InputTab,
        Keybinding::InputCopy,
        Keybinding::SelectUp,
        Keybinding::SelectDown,
        Keybinding::SelectPageUp,
        Keybinding::SelectPageDown,
        Keybinding::SelectConfirm,
        Keybinding::SelectCancel,
        Keybinding::AltScreenPageUp,
        Keybinding::AltScreenPageDown,
        Keybinding::AltScreenHalfPageUp,
        Keybinding::AltScreenHalfPageDown,
        Keybinding::AltScreenLineUp,
        Keybinding::AltScreenLineDown,
        Keybinding::AltScreenPreviousPrompt,
        Keybinding::AltScreenNextPrompt,
        Keybinding::AltScreenSearch,
        Keybinding::AltScreenSearchNext,
        Keybinding::AltScreenSearchPrevious,
        Keybinding::AltScreenSearchClose,
        Keybinding::AltScreenTop,
        Keybinding::AltScreenBottom,
    ];

    /// The TS string id (e.g. `"tui.editor.cursorUp"`).
    pub fn id(self) -> &'static str {
        match self {
            Keybinding::EditorCursorUp => "tui.editor.cursorUp",
            Keybinding::EditorCursorDown => "tui.editor.cursorDown",
            Keybinding::EditorHistoryPrevious => "tui.editor.historyPrevious",
            Keybinding::EditorHistoryNext => "tui.editor.historyNext",
            Keybinding::EditorCursorLeft => "tui.editor.cursorLeft",
            Keybinding::EditorCursorRight => "tui.editor.cursorRight",
            Keybinding::EditorCursorWordLeft => "tui.editor.cursorWordLeft",
            Keybinding::EditorCursorWordRight => "tui.editor.cursorWordRight",
            Keybinding::EditorCursorLineStart => "tui.editor.cursorLineStart",
            Keybinding::EditorCursorLineEnd => "tui.editor.cursorLineEnd",
            Keybinding::EditorJumpForward => "tui.editor.jumpForward",
            Keybinding::EditorJumpBackward => "tui.editor.jumpBackward",
            Keybinding::EditorPageUp => "tui.editor.pageUp",
            Keybinding::EditorPageDown => "tui.editor.pageDown",
            Keybinding::EditorDeleteCharBackward => "tui.editor.deleteCharBackward",
            Keybinding::EditorDeleteCharForward => "tui.editor.deleteCharForward",
            Keybinding::EditorDeleteWordBackward => "tui.editor.deleteWordBackward",
            Keybinding::EditorDeleteWordForward => "tui.editor.deleteWordForward",
            Keybinding::EditorDeleteToLineStart => "tui.editor.deleteToLineStart",
            Keybinding::EditorDeleteToLineEnd => "tui.editor.deleteToLineEnd",
            Keybinding::EditorYank => "tui.editor.yank",
            Keybinding::EditorYankPop => "tui.editor.yankPop",
            Keybinding::EditorUndo => "tui.editor.undo",
            Keybinding::InputNewLine => "tui.input.newLine",
            Keybinding::InputSubmit => "tui.input.submit",
            Keybinding::InputTab => "tui.input.tab",
            Keybinding::InputCopy => "tui.input.copy",
            Keybinding::SelectUp => "tui.select.up",
            Keybinding::SelectDown => "tui.select.down",
            Keybinding::SelectPageUp => "tui.select.pageUp",
            Keybinding::SelectPageDown => "tui.select.pageDown",
            Keybinding::SelectConfirm => "tui.select.confirm",
            Keybinding::SelectCancel => "tui.select.cancel",
            Keybinding::AltScreenPageUp => "tui.altScreen.pageUp",
            Keybinding::AltScreenPageDown => "tui.altScreen.pageDown",
            Keybinding::AltScreenHalfPageUp => "tui.altScreen.halfPageUp",
            Keybinding::AltScreenHalfPageDown => "tui.altScreen.halfPageDown",
            Keybinding::AltScreenLineUp => "tui.altScreen.lineUp",
            Keybinding::AltScreenLineDown => "tui.altScreen.lineDown",
            Keybinding::AltScreenPreviousPrompt => "tui.altScreen.previousPrompt",
            Keybinding::AltScreenNextPrompt => "tui.altScreen.nextPrompt",
            Keybinding::AltScreenSearch => "tui.altScreen.search",
            Keybinding::AltScreenSearchNext => "tui.altScreen.searchNext",
            Keybinding::AltScreenSearchPrevious => "tui.altScreen.searchPrevious",
            Keybinding::AltScreenSearchClose => "tui.altScreen.searchClose",
            Keybinding::AltScreenTop => "tui.altScreen.top",
            Keybinding::AltScreenBottom => "tui.altScreen.bottom",
        }
    }

    /// Reverse of [`Keybinding::id`]; `None` for an unrecognized id (mirrors
    /// `!(keybinding in this.definitions)`, keybindings.ts:173).
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|kb| kb.id() == id)
    }

    /// The default `KeybindingDefinition` (`TUI_KEYBINDINGS[id]`, keybindings.ts:54-134).
    pub fn definition(self) -> KeybindingDefinition {
        let (default_keys, description): (&'static [&'static str], &'static str) = match self {
            Keybinding::EditorCursorUp => (&["up"], "Move cursor up"),
            Keybinding::EditorCursorDown => (&["down"], "Move cursor down"),
            Keybinding::EditorHistoryPrevious => (&[], "Select previous prompt history entry"),
            Keybinding::EditorHistoryNext => (&[], "Select next prompt history entry"),
            Keybinding::EditorCursorLeft => (&["left", "ctrl+b"], "Move cursor left"),
            Keybinding::EditorCursorRight => (&["right", "ctrl+f"], "Move cursor right"),
            Keybinding::EditorCursorWordLeft => {
                (&["alt+left", "ctrl+left", "alt+b"], "Move cursor word left")
            }
            Keybinding::EditorCursorWordRight => (
                &["alt+right", "ctrl+right", "alt+f"],
                "Move cursor word right",
            ),
            Keybinding::EditorCursorLineStart => {
                (&["home", "ctrl+home", "ctrl+a"], "Move to line start")
            }
            Keybinding::EditorCursorLineEnd => (&["end", "ctrl+end", "ctrl+e"], "Move to line end"),
            Keybinding::EditorJumpForward => (&["ctrl+]"], "Jump forward to character"),
            Keybinding::EditorJumpBackward => (&["ctrl+alt+]"], "Jump backward to character"),
            Keybinding::EditorPageUp => (&["pageUp", "ctrl+pageUp"], "Page up"),
            Keybinding::EditorPageDown => (&["pageDown", "ctrl+pageDown"], "Page down"),
            Keybinding::EditorDeleteCharBackward => (&["backspace"], "Delete character backward"),
            Keybinding::EditorDeleteCharForward => {
                (&["delete", "ctrl+d"], "Delete character forward")
            }
            Keybinding::EditorDeleteWordBackward => {
                (&["ctrl+w", "alt+backspace"], "Delete word backward")
            }
            Keybinding::EditorDeleteWordForward => {
                (&["alt+d", "alt+delete"], "Delete word forward")
            }
            Keybinding::EditorDeleteToLineStart => (&["ctrl+u"], "Delete to line start"),
            Keybinding::EditorDeleteToLineEnd => (&["ctrl+k"], "Delete to line end"),
            Keybinding::EditorYank => (&["ctrl+y"], "Yank"),
            Keybinding::EditorYankPop => (&["alt+y"], "Yank pop"),
            Keybinding::EditorUndo => (&["ctrl+-"], "Undo"),
            Keybinding::InputNewLine => (&["shift+enter", "ctrl+j"], "Insert newline"),
            Keybinding::InputSubmit => (&["enter"], "Submit input"),
            Keybinding::InputTab => (&["tab"], "Tab / autocomplete"),
            Keybinding::InputCopy => (&["ctrl+c"], "Copy selection"),
            Keybinding::SelectUp => (&["up"], "Move selection up"),
            Keybinding::SelectDown => (&["down"], "Move selection down"),
            Keybinding::SelectPageUp => (&["pageUp"], "Selection page up"),
            Keybinding::SelectPageDown => (&["pageDown"], "Selection page down"),
            Keybinding::SelectConfirm => (&["enter"], "Confirm selection"),
            Keybinding::SelectCancel => (&["escape", "ctrl+c"], "Cancel selection"),
            Keybinding::AltScreenPageUp => (&["pageUp"], "Scroll viewport up one page"),
            Keybinding::AltScreenPageDown => (&["pageDown"], "Scroll viewport down one page"),
            Keybinding::AltScreenHalfPageUp => (&[], "Scroll viewport up half a page"),
            Keybinding::AltScreenHalfPageDown => (&[], "Scroll viewport down half a page"),
            Keybinding::AltScreenLineUp => (&[], "Scroll viewport up one line"),
            Keybinding::AltScreenLineDown => (&[], "Scroll viewport down one line"),
            Keybinding::AltScreenPreviousPrompt => {
                (&["ctrl+shift+up"], "Jump to previous semantic prompt")
            }
            Keybinding::AltScreenNextPrompt => {
                (&["ctrl+shift+down"], "Jump to next semantic prompt")
            }
            Keybinding::AltScreenSearch => (&["ctrl+shift+f"], "Search the primary scroll view"),
            Keybinding::AltScreenSearchNext => {
                (&["enter", "ctrl+g"], "Select the next search match")
            }
            Keybinding::AltScreenSearchPrevious => (
                &["shift+enter", "ctrl+shift+g"],
                "Select the previous search match",
            ),
            Keybinding::AltScreenSearchClose => (&["escape"], "Close transcript search"),
            Keybinding::AltScreenTop => (&["home"], "Scroll viewport to top"),
            Keybinding::AltScreenBottom => (&["end"], "Scroll viewport to bottom"),
        };
        KeybindingDefinition {
            default_keys,
            description,
        }
    }
}

/// `KeybindingDefinition` (keybindings.ts:46).
#[derive(Debug, Clone, Copy)]
pub struct KeybindingDefinition {
    pub default_keys: &'static [&'static str],
    pub description: &'static str,
}

/// One user-configured binding value — `KeyId | KeyId[]` (`undefined` is
/// represented by the key's absence from the map, see [`KeybindingsManager`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RawKeys {
    One(String),
    Many(Vec<String>),
}

impl RawKeys {
    fn as_list(&self) -> Vec<&str> {
        match self {
            RawKeys::One(k) => vec![k.as_str()],
            RawKeys::Many(v) => v.iter().map(String::as_str).collect(),
        }
    }
}

/// `KeybindingConflict` (keybindings.ts:136).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeybindingConflict {
    pub key: String,
    pub keybindings: Vec<Keybinding>,
}

fn dedupe_keys<'a>(keys: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut result = Vec::new();
    for k in keys {
        if seen.insert(k) {
            result.push(k.to_string());
        }
    }
    result
}

/// `KeybindingsManager` (keybindings.ts:155) — see module docs for the
/// `definitions`-parameter and singleton-access scope decisions.
#[derive(Debug, Default)]
pub struct KeybindingsManager {
    user_bindings: HashMap<String, RawKeys>,
    keys_by_id: HashMap<Keybinding, Vec<String>>,
    conflicts: Vec<KeybindingConflict>,
}

impl KeybindingsManager {
    pub fn new(user_bindings: HashMap<String, RawKeys>) -> Self {
        let mut manager = Self {
            user_bindings,
            keys_by_id: HashMap::new(),
            conflicts: Vec::new(),
        };
        manager.rebuild();
        manager
    }

    /// `rebuild` (keybindings.ts:167).
    fn rebuild(&mut self) {
        self.keys_by_id.clear();
        self.conflicts.clear();

        let mut user_claims: HashMap<String, Vec<Keybinding>> = HashMap::new();
        for (raw_id, raw_keys) in &self.user_bindings {
            let Some(kb) = Keybinding::from_id(raw_id) else {
                continue;
            };
            for key in dedupe_keys(raw_keys.as_list()) {
                let claimants = user_claims.entry(key).or_default();
                if !claimants.contains(&kb) {
                    claimants.push(kb);
                }
            }
        }

        for (key, keybindings) in &user_claims {
            if keybindings.len() > 1 {
                self.conflicts.push(KeybindingConflict {
                    key: key.clone(),
                    keybindings: keybindings.clone(),
                });
            }
        }

        for kb in Keybinding::ALL.iter().copied() {
            let keys = match self.user_bindings.get(kb.id()) {
                Some(raw) => dedupe_keys(raw.as_list()),
                None => dedupe_keys(kb.definition().default_keys.iter().copied()),
            };
            self.keys_by_id.insert(kb, keys);
        }
    }

    /// `matches` (keybindings.ts:194).
    pub fn matches(&self, data: &str, keybinding: Keybinding) -> bool {
        self.keys_by_id
            .get(&keybinding)
            .is_some_and(|keys| keys.iter().any(|key| matches_key(data, key)))
    }

    /// `getKeys` (keybindings.ts:202).
    pub fn get_keys(&self, keybinding: Keybinding) -> Vec<String> {
        self.keys_by_id
            .get(&keybinding)
            .cloned()
            .unwrap_or_default()
    }

    /// `getDefinition` (keybindings.ts:206).
    pub fn get_definition(&self, keybinding: Keybinding) -> KeybindingDefinition {
        keybinding.definition()
    }

    /// `getConflicts` (keybindings.ts:210).
    pub fn get_conflicts(&self) -> Vec<KeybindingConflict> {
        self.conflicts.clone()
    }

    /// `setUserBindings` (keybindings.ts:214).
    pub fn set_user_bindings(&mut self, user_bindings: HashMap<String, RawKeys>) {
        self.user_bindings = user_bindings;
        self.rebuild();
    }

    /// `getUserBindings` (keybindings.ts:219) — the raw, un-normalized config,
    /// including entries for unrecognized ids.
    pub fn get_user_bindings(&self) -> HashMap<String, RawKeys> {
        self.user_bindings.clone()
    }

    /// `getResolvedBindings` (keybindings.ts:223).
    pub fn get_resolved_bindings(&self) -> HashMap<String, RawKeys> {
        let mut resolved = HashMap::new();
        for kb in Keybinding::ALL.iter().copied() {
            let keys = self.keys_by_id.get(&kb).cloned().unwrap_or_default();
            let raw = if keys.len() == 1 {
                RawKeys::One(keys[0].clone())
            } else {
                RawKeys::Many(keys)
            };
            resolved.insert(kb.id().to_string(), raw);
        }
        resolved
    }
}

static GLOBAL_KEYBINDINGS: LazyLock<Mutex<KeybindingsManager>> =
    LazyLock::new(|| Mutex::new(KeybindingsManager::new(HashMap::new())));

/// `setKeybindings` (keybindings.ts:235).
pub fn set_keybindings(manager: KeybindingsManager) {
    *GLOBAL_KEYBINDINGS.lock().unwrap_or_else(|e| e.into_inner()) = manager;
}

/// `getKeybindings` (keybindings.ts:239) — see module docs for why this
/// returns a `MutexGuard` rather than an owned/`&'static` value.
pub fn get_keybindings() -> MutexGuard<'static, KeybindingsManager> {
    GLOBAL_KEYBINDINGS.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_bindings_resolve_to_their_declared_keys() {
        let mgr = KeybindingsManager::new(HashMap::new());
        assert_eq!(
            mgr.get_keys(Keybinding::EditorCursorLeft),
            vec!["left".to_string(), "ctrl+b".to_string()]
        );
    }

    #[test]
    fn user_override_replaces_default_keys() {
        let mut overrides = HashMap::new();
        overrides.insert(
            "tui.editor.undo".to_string(),
            RawKeys::One("ctrl+z".to_string()),
        );
        let mgr = KeybindingsManager::new(overrides);
        assert_eq!(
            mgr.get_keys(Keybinding::EditorUndo),
            vec!["ctrl+z".to_string()]
        );
        assert!(mgr.matches("\x1a", Keybinding::EditorUndo));
    }

    #[test]
    fn unrecognized_id_is_ignored_for_conflicts_but_kept_in_raw_storage() {
        let mut overrides = HashMap::new();
        overrides.insert(
            "tui.nonexistent.thing".to_string(),
            RawKeys::One("ctrl+q".to_string()),
        );
        let mgr = KeybindingsManager::new(overrides);
        assert!(mgr.get_conflicts().is_empty());
        assert!(mgr
            .get_user_bindings()
            .contains_key("tui.nonexistent.thing"));
    }

    #[test]
    fn duplicate_keys_within_one_binding_are_deduped() {
        let mut overrides = HashMap::new();
        overrides.insert(
            "tui.input.copy".to_string(),
            RawKeys::Many(vec![
                "ctrl+c".to_string(),
                "ctrl+c".to_string(),
                "ctrl+insert".to_string(),
            ]),
        );
        let mgr = KeybindingsManager::new(overrides);
        assert_eq!(
            mgr.get_keys(Keybinding::InputCopy),
            vec!["ctrl+c".to_string(), "ctrl+insert".to_string()]
        );
    }

    #[test]
    fn two_bindings_claiming_the_same_key_conflict() {
        let mut overrides = HashMap::new();
        overrides.insert(
            "tui.editor.undo".to_string(),
            RawKeys::One("ctrl+x".to_string()),
        );
        overrides.insert(
            "tui.editor.yank".to_string(),
            RawKeys::One("ctrl+x".to_string()),
        );
        let mgr = KeybindingsManager::new(overrides);
        let conflicts = mgr.get_conflicts();
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].key, "ctrl+x");
        assert_eq!(conflicts[0].keybindings.len(), 2);
    }

    #[test]
    fn get_keybindings_lazily_constructs_a_default() {
        assert_eq!(
            get_keybindings().get_keys(Keybinding::EditorCursorUp),
            vec!["up".to_string()]
        );
    }
}
