//! Picker lifecycle for `InteractiveMode` — opening the `/model`, `/resume`
//! and `/tree` pickers, routing keys to them, and applying their selections.
//!
//! Split out of `interactive_mode.rs` (P7, `docs/tui-pending-action-plan.md`
//! housekeeping — a pure code move, no behavior change: every method here is
//! still an inherent `impl InteractiveMode` method, callable exactly as
//! before regardless of which file defines it).

use std::cell::RefCell;
use std::rc::Rc;

use pirust_tui::tui::SharedComponent;

use crate::interactive_pickers::{
    BranchPicker, ModelPicker as PickerModelPicker, PickerAction, SessionPicker,
};

use super::InteractiveMode;

impl InteractiveMode {
    /// Open the `/model` picker.
    pub(super) fn open_model_picker(&mut self) {
        let picker = Rc::new(RefCell::new(PickerModelPicker::new(
            self.model_entries.clone(),
            self.picker_viewport_rows(),
        )));
        self.model_picker = Some(Rc::clone(&picker));
        self.show_modal_component(picker as SharedComponent);
    }

    /// Route a key to the model picker.
    ///
    /// All the navigation, fuzzy filtering and clamping now lives in the
    /// picker itself; this only has to act on the [`PickerAction`] it reports.
    pub(super) fn handle_model_picker_key(&mut self, data: &str) {
        let Some(picker) = self.model_picker.clone() else {
            return;
        };
        let action = picker.borrow_mut().handle_key(data);
        match action {
            PickerAction::None => self.repaint(),
            PickerAction::Dismissed => {
                self.model_picker = None;
                self.hide_modal();
            }
            PickerAction::Selected(index) => {
                let chosen = self.model_entries.get(index).cloned();
                self.model_picker = None;
                self.hide_modal();
                match chosen {
                    // `TuiRuntimeInfo::set_model_by_name` (print_mode.rs) mutates
                    // the running `Agent`'s model in place — see its doc comment
                    // for why this is a name lookup rather than passing a real
                    // `Model` (which `ModelEntry` does not carry).
                    Some(entry) => match self
                        .session
                        .set_model_by_name(&entry.provider, &entry.model_id)
                    {
                        Ok(()) => self.show_notice(format!(
                            "Switched to {} / {}",
                            entry.provider, entry.model_id
                        )),
                        Err(error) => self.show_error(format!(
                            "Could not switch to {} / {}: {error}",
                            entry.provider, entry.model_id
                        )),
                    },
                    None => self.show_error("No model is available to select"),
                }
                self.refresh_status();
            }
        }
    }

    /// Open the `/resume` picker over the real session store.
    pub(super) fn open_resume_picker(&mut self) {
        let entries = self.session.session_entries();
        let picker = Rc::new(RefCell::new(SessionPicker::new(
            entries,
            self.picker_viewport_rows(),
        )));
        self.resume_picker = Some(Rc::clone(&picker));
        self.show_modal_component(picker as SharedComponent);
    }

    /// Open the `/tree` picker over `TuiRuntimeInfo::branch_entries`
    /// (`print_mode.rs`).
    ///
    /// An empty list renders a plain notice instead of an empty picker —
    /// "no branches" is the honest state for a session with no fork points
    /// yet, not an error and not a picker with a header and zero rows.
    pub(super) fn open_branch_picker(&mut self) {
        let entries = self.session.branch_entries();
        if entries.is_empty() {
            self.show_notice("No branches — this session has no fork points yet");
            return;
        }
        let picker = Rc::new(RefCell::new(BranchPicker::new(
            entries,
            self.picker_viewport_rows(),
        )));
        self.branch_picker = Some(Rc::clone(&picker));
        self.show_modal_component(picker as SharedComponent);
    }

    /// How many list rows a picker may occupy.
    ///
    /// The pickers render inside the bottom band, above the editor, so every
    /// row they take is a row of transcript pushed off-screen. `RESERVED`
    /// covers the picker's own header and hint lines plus the editor and
    /// status line beneath it; the clamp keeps the list usable at 80×24 (the
    /// spec's floor) without letting a 200-model list swallow a tall terminal.
    pub(super) fn picker_viewport_rows(&self) -> usize {
        /// Rows the band needs for everything that is not list content.
        const RESERVED: usize = 8;
        /// Never show fewer than this, even on a very short terminal.
        const MIN_ROWS: usize = 3;
        /// Never show more than this, however tall the terminal.
        const MAX_ROWS: usize = 15;

        let rows = self.tui.borrow().terminal_rows() as usize;
        rows.saturating_sub(RESERVED).clamp(MIN_ROWS, MAX_ROWS)
    }

    /// Route a key to the session picker. `Selected` resumes the chosen
    /// session in place via [`Self::resume_to`] — the same
    /// `switch_to_session_file` seam `/import` (`run_import`) already uses,
    /// now reachable from the picker because `SessionEntry::path`
    /// (`interactive_pickers.rs`) carries a real session file path through
    /// from `SessionInfo::path` (`session.rs:1705`).
    pub(super) fn handle_resume_picker_key(&mut self, data: &str) {
        let Some(picker) = self.resume_picker.clone() else {
            return;
        };
        let action = picker.borrow_mut().handle_key(data);
        match action {
            PickerAction::None => self.repaint(),
            PickerAction::Dismissed => {
                self.resume_picker = None;
                self.hide_modal();
            }
            PickerAction::Selected(_) => {
                let chosen = picker
                    .borrow()
                    .selected_entry()
                    .map(|entry| entry.path.clone());
                self.resume_picker = None;
                self.hide_modal();
                match chosen {
                    Some(path) => {
                        if !self.refuse_while_turn_active("resume a session") {
                            self.resume_to(&path);
                        }
                    }
                    None => self.show_error("No session is available to resume"),
                }
            }
        }
    }

    /// Shared by [`Self::handle_resume_picker_key`]: switch the live session
    /// onto `path` in place (`PrintModeSession::switch_to_session_file`,
    /// `print_mode.rs:1086` — the same call `run_import` makes for
    /// `/import`), then bring the screen back in sync with the swapped-in
    /// session.
    ///
    /// `path` is `SessionEntry::path`, sourced from `SessionInfo::path`
    /// (`session.rs:1705`), which is populated from a real directory listing
    /// (`build_session_info`/`list_sessions_from_dir`, `session.rs`) — never
    /// synthesized here. An empty `path` would only mean a malformed store
    /// entry slipped through, so it is refused rather than handed to
    /// `switch_to_session_file`, which would otherwise fail on it anyway but
    /// with a far less useful error.
    ///
    /// On success the on-screen transcript belongs to the *old* session and
    /// is stale, so it is cleared the same way `/new`/`/import` clear theirs
    /// (`reset_transcript_view`, which also resets turn state to `Idle`).
    /// The status line is refreshed too: the session id, cwd, and token
    /// counts it reports all just changed under it. On failure nothing here
    /// is touched — no transcript clear, no status refresh, no success
    /// notice — the switch never happened.
    pub(super) fn resume_to(&mut self, path: &str) {
        if path.is_empty() {
            self.show_error(
                "This session has no file path on record, so it cannot be resumed in place.",
            );
            return;
        }
        match self.session.switch_to_session_file(path) {
            Ok(()) => {
                self.reset_transcript_view();
                self.refresh_status();
                self.show_notice(format!("Resumed session from {path}"));
            }
            Err(error) => self.show_error(format!("Could not resume session from {path}: {error}")),
        }
    }

    /// Route a key to the `/tree` branch picker. `Selected` forks a new
    /// session from the chosen entry via `PrintModeSession::fork_from`
    /// (print_mode.rs:1052) — the same seam `run_fork`/`fork_to` use for
    /// `/fork <entry-id>` — rather than `navigate_tree`
    /// (print_mode.rs:876): `navigate_tree` is print mode's own
    /// `session.navigateTree` pass-through and carries no documented
    /// contract for rebuilding `Agent`'s in-memory transcript to match the
    /// new position, whereas `fork_from`'s doc comment explicitly spells out
    /// the `SessionManager::create_branched_session` +
    /// `entries_to_agent_messages` + `Agent::set_messages` chain its real
    /// implementer is expected to follow — the one seam on this trait
    /// guaranteed to leave the screen consistent with the store afterward.
    pub(super) fn handle_branch_picker_key(&mut self, data: &str) {
        let Some(picker) = self.branch_picker.clone() else {
            return;
        };
        let action = picker.borrow_mut().handle_key(data);
        match action {
            PickerAction::None => self.repaint(),
            PickerAction::Dismissed => {
                self.branch_picker = None;
                self.hide_modal();
            }
            PickerAction::Selected(_) => {
                let chosen = picker
                    .borrow()
                    .selected_entry()
                    .map(|entry| entry.id.clone());
                self.branch_picker = None;
                self.hide_modal();
                match chosen {
                    Some(id) => {
                        if !self.refuse_while_turn_active("switch branches") {
                            self.fork_to(&id);
                        }
                    }
                    None => self.show_error("No branch is available to select"),
                }
            }
        }
    }
}
