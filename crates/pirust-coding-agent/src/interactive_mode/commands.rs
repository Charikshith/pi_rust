//! Slash-command handlers for `InteractiveMode` — `dispatch_command` and
//! every `/name`, `/compact`, `/new`, `/export`, `/settings`, ... handler it
//! routes to.
//!
//! Split out of `interactive_mode.rs` (P7, `docs/tui-pending-action-plan.md`
//! housekeeping — a pure code move, no behavior change: every method here is
//! still an inherent `impl InteractiveMode` method, callable exactly as
//! before regardless of which file defines it). `use super::*` (rather than
//! a curated import list) deliberately mirrors this crate's own established
//! `mod tests { use super::*; ... }` convention: a child module of
//! `interactive_mode` sees everything `mod.rs` itself imported or defined,
//! private items included, because Rust privacy already extends downward to
//! descendant modules — nothing here needed to be re-exported to get here.

use super::*;

impl InteractiveMode {
    /// Dispatch a submitted slash command. Unknown commands get an actionable
    /// error; known-but-unimplemented commands report why. Commands that
    /// require a picker open the palette/model/resume modals.
    pub(super) fn dispatch_command(&mut self, text: &str) {
        let parts: Vec<&str> = text.split_whitespace().collect();
        let command = parts
            .first()
            .map(|c| c.trim_start_matches('/').to_ascii_lowercase())
            .unwrap_or_default();
        let arg = parts.get(1).copied();
        match command.as_str() {
            "help" => self.show_help(),
            "hotkeys" => self.show_hotkeys(),
            "session" => self.show_session_info(),
            "name" => self.set_session_name(arg),
            "model" => self.open_model_picker(),
            "models" => self.show_models_list(),
            "resume" => self.open_resume_picker(),
            "compact" => self.run_compact(),
            "new" => self.run_new_session(),
            "clone" => self.run_clone_session(),
            "fork" => self.run_fork(arg),
            "import" => self.run_import(arg),
            "tree" => self.open_branch_picker(),
            "restart" => self.run_restart(),
            "refresh-model-list" => self.refresh_models(),
            "reload-extensions" => self.reload_extensions(),
            "debug" => self.toggle_debug_panel(),
            "thinking" => self.toggle_thinking(arg),
            "export" => self.run_export(arg),
            "copy" => self.run_copy(),
            "trust" => self.run_trust(arg),
            "changelog" => self.run_changelog(arg),
            "settings" => self.run_settings(),
            "scoped-models" => self.run_scoped_models(arg),
            "share" => self.run_share(arg),
            "quit" => {
                self.quit.store(true, Ordering::Relaxed);
            }
            _ => {
                if BUILTIN_SLASH_COMMANDS
                    .iter()
                    .any(|(name, _, _)| *name == command)
                {
                    // A precise reason beats a flat "not available": it names
                    // the missing seam, so the answer is actionable instead of
                    // just a refusal.
                    match crate::interactive_commands::unavailable_reason(&command) {
                        Some(reason) => self.show_error(format!("/{command}: {reason}")),
                        None => {
                            self.show_error(format!("/{command} is not available in this session"))
                        }
                    }
                } else {
                    self.show_error(format!("Unknown command: /{command}"));
                }
            }
        }
    }

    /// `/help` — the registered command list, with the same availability
    /// marking the autocomplete dropdown uses (audit #22).
    pub(super) fn show_help(&mut self) {
        // Grouped by category with aligned columns, rather than the flat
        // 28-line dump this used to print.
        let help = crate::interactive_commands::command_help_lines(&slash_command_available);
        self.show_notice(help);
    }

    /// `/hotkeys` — the keyboard shortcuts the TUI implements.
    pub(super) fn show_hotkeys(&mut self) {
        self.show_notice(
            "Ctrl+D    quit (empty editor)\n\
             Ctrl+C    cancel the active turn (twice within 500ms quits)\n\
             Esc       cancel the active turn, or close an open picker\n\
             Ctrl+O    expand/collapse the latest reasoning block\n\
             r / a / d resolve a tool-approval prompt (run once / always / deny)\n\
             /         open the command palette\n\
             ↑ / ↓     move in a picker · Enter select · Esc dismiss\n\
             /debug    show the debug panel · /thinking [on|off] all reasoning",
        );
    }

    /// `/session` — session id, cwd, model, context, cost.
    pub(super) fn show_session_info(&mut self) {
        let header = self.session.header();
        let mut lines = Vec::new();
        match &header {
            Some(h) => {
                lines.push(format!("Session: {}", h.id));
                lines.push(format!("cwd: {}", h.cwd));
                if let Some(name) = h.metadata.as_ref().and_then(|m| m.get("name")) {
                    if let Some(name) = name.as_str() {
                        lines.push(format!("Name: {name}"));
                    }
                }
            }
            None => lines.push("Session: unavailable".to_string()),
        }
        if let Some(status) = &self.runtime_status {
            lines.push(format!("Model: {} / {}", status.provider, status.model));
            lines.push(format!(
                "Context: {} / {} tokens · cost ${:.4}",
                status.context_tokens, status.context_window, status.cost
            ));
        }
        self.show_notice(lines.join("\n"));
    }

    /// `/name <name>` — set the session display name.
    ///
    /// Reports honestly that nothing was renamed. This used to answer
    /// "Session renamed to: X" for any argument, but there is no rename on
    /// `PrintModeSession`, so nothing was written anywhere and the next
    /// `/session` still showed the old name. Claiming a write that did not
    /// happen is worse than saying the command is not wired yet, so
    /// `slash_command_available` reports it unavailable too and both `/help`
    /// and the autocomplete dropdown mark it as such.
    pub(super) fn set_session_name(&mut self, arg: Option<&str>) {
        let name = arg.map(str::trim).filter(|n| !n.is_empty());
        match name {
            None => self.show_error("Usage: /name <name>"),
            // Echoes the name the store actually wrote, not the argument:
            // `append_session_info` collapses newline runs to single spaces
            // and trims, so the two can differ.
            Some(name) => match self.session.set_session_name(name) {
                Ok(written) => self.show_notice(format!("Session renamed to: {written}")),
                Err(error) => self.show_error(format!("Could not rename the session: {error}")),
            },
        }
    }

    /// `/models` — the active provider's model + the configured list headline.
    pub(super) fn show_models_list(&mut self) {
        match &self.runtime_status {
            Some(status) => {
                self.show_notice(format!(
                    "Provider: {} · Model: {} ({} — {})\nContext: {} tokens · reasoning: {}\nUse /model to switch",
                    status.provider,
                    status.model_name,
                    status.model,
                    if status.reasoning_supported { "reasoning" } else { "no reasoning" },
                    status.context_window,
                    if status.reasoning_supported { "supported" } else { "not supported" },
                ));
            }
            None => self.show_error("No model is configured for this session"),
        }
    }

    /// `/compact` — run the harness compaction seam
    /// (`SingleTurnSession::compact`, `runtime_host.rs`) as a spawned task,
    /// reusing `active_turn`/`TurnState` so a compaction and a prompt turn
    /// can never run concurrently.
    ///
    /// Slash commands normally dispatch even mid-turn — `dispatch_command`'s
    /// own comment explains why: they're UI-local and instant, so only
    /// prompts queue. A `/compact` racing a live prompt turn is not one of
    /// those safe-to-run-mid-turn commands: both sides read and rewrite
    /// `Agent`'s message list (`Agent::messages`/`set_messages`,
    /// agent.rs:345,360), so this guards explicitly rather than inheriting
    /// the generic mid-turn dispatch behaviour.
    pub(super) fn run_compact(&mut self) {
        if self.active_turn.is_some() {
            self.show_error(
                "Cannot compact while a request is in progress; try again once it finishes.",
            );
            return;
        }
        self.turn_state = TurnState::Running;
        self.is_compacting = true;
        self.refresh_status();
        self.repaint();

        let session = Arc::clone(&self.session);
        self.active_turn = Some(self.runtime.spawn(async move {
            session
                .compact(crate::print_mode::CompactionReason::Manual)
                .await
                .map_err(crate::print_mode::ThrownValue::Error)
        }));
    }

    /// Guard shared by every session-mutation command below (`/new`, `/clone`,
    /// `/fork`, `/import`, and the branch picker's `Selected` action): each of
    /// their real implementations is expected to call `Agent::set_messages`
    /// (`agent.rs:360`), the same call a live turn's `prompt()` future is
    /// concurrently reading/writing — the identical hazard `run_compact`
    /// guards against above, for the identical reason. Returns `true` if the
    /// caller already reported the refusal and should stop.
    pub(super) fn refuse_while_turn_active(&mut self, verb: &str) -> bool {
        if self.active_turn.is_some() {
            self.show_error(format!(
                "Cannot {verb} while a request is in progress; try again once it finishes."
            ));
            true
        } else {
            false
        }
    }

    /// Clear the on-screen chat and per-turn state after a command that
    /// points the live session at different content than what is currently
    /// rendered (`/new`, `/fork`, `/import`, in-place `/resume`, and the
    /// branch picker's `Selected` action).
    ///
    /// `/clone` does NOT call this: it branches at the *current* leaf
    /// (`PrintModeSession::clone_session`'s doc comment), so the transcript
    /// already on screen is exactly what the new file holds — nothing to
    /// invalidate.
    ///
    /// This does not replay the new/changed session's own history back into
    /// the chat: nothing in this crate renders a batch `Vec<AgentMessage>`
    /// into chat components — the container is built up incrementally from
    /// live `AgentSessionEvent`s as a turn streams (`render_event`). An empty
    /// transcript is therefore the honest result of a session swap; a stale
    /// or fabricated one is not.
    pub(super) fn reset_transcript_view(&mut self) {
        self.chat.borrow_mut().clear();
        self.pending_prompts.clear();
        self.finish_turn(TurnState::Idle);
    }

    /// `/new` — discard the transcript and start a fresh, empty session in
    /// place (`PrintModeSession::start_new_session`, `print_mode.rs:993`).
    pub(super) fn run_new_session(&mut self) {
        if self.refuse_while_turn_active("start a new session") {
            return;
        }
        match self.session.start_new_session() {
            Ok(()) => {
                self.reset_transcript_view();
                self.show_notice("Started a new session.");
            }
            Err(error) => self.show_error(format!("Could not start a new session: {error}")),
        }
    }

    /// `/clone` — duplicate the session at its current position into a new
    /// session file, and switch the live session to it
    /// (`PrintModeSession::clone_session`, `print_mode.rs:1022`).
    pub(super) fn run_clone_session(&mut self) {
        if self.refuse_while_turn_active("clone the session") {
            return;
        }
        match self.session.clone_session() {
            Ok(Some(path)) => self.show_notice(format!("Cloned session to {path}")),
            Ok(None) => self.show_notice("Cloned session (in-memory; no file was written)"),
            Err(error) => self.show_error(format!("Could not clone the session: {error}")),
        }
    }

    /// `/fork <entry-id>` — branch a new session off an earlier point in this
    /// session's history and switch the live session to it
    /// (`PrintModeSession::fork_from`, `print_mode.rs:1052`). Requires an
    /// explicit entry id: guessing a fork point is worse than refusing it —
    /// `/tree` is what lists the ids that are valid to fork from.
    pub(super) fn run_fork(&mut self, arg: Option<&str>) {
        let Some(entry_id) = arg.map(str::trim).filter(|s| !s.is_empty()) else {
            self.show_error(
                "Usage: /fork <entry-id> — /tree lists the entry ids you can fork from",
            );
            return;
        };
        if self.refuse_while_turn_active("fork the session") {
            return;
        }
        self.fork_to(entry_id);
    }

    /// Shared by [`Self::run_fork`] and [`Self::handle_branch_picker_key`]:
    /// call `PrintModeSession::fork_from`, reset the transcript view on
    /// success (the branched entries are not replayed — see
    /// [`Self::reset_transcript_view`]), and report the outcome.
    pub(super) fn fork_to(&mut self, entry_id: &str) {
        match self.session.fork_from(entry_id) {
            Ok(Some(path)) => {
                self.reset_transcript_view();
                self.show_notice(format!("Forked session to {path}"));
            }
            Ok(None) => {
                self.reset_transcript_view();
                self.show_notice("Forked session (in-memory; no file was written)");
            }
            Err(error) => self.show_error(format!("Could not fork from {entry_id}: {error}")),
        }
    }

    /// `/import <path>` — load a different session file into the live
    /// session (`PrintModeSession::switch_to_session_file`,
    /// `print_mode.rs:1086`). Requires the path; there is no default file to
    /// guess.
    pub(super) fn run_import(&mut self, arg: Option<&str>) {
        let Some(path) = arg.map(str::trim).filter(|s| !s.is_empty()) else {
            self.show_error("Usage: /import <path>");
            return;
        };
        if self.refuse_while_turn_active("import a session") {
            return;
        }
        match self.session.switch_to_session_file(path) {
            Ok(()) => {
                self.reset_transcript_view();
                self.show_notice(format!("Imported session from {path}"));
            }
            Err(error) => self.show_error(format!("Could not import {path}: {error}")),
        }
    }

    /// `/refresh-model-list` — re-read the runtime's model status.
    pub(super) fn refresh_models(&mut self) {
        self.refresh_status();
        self.show_notice("Model list refreshed");
    }

    /// Render a [`CommandOutcome`] from [`crate::interactive_commands`] into
    /// the chat. One place, so every wired command reports consistently.
    pub(super) fn apply_outcome(&mut self, outcome: CommandOutcome) {
        match outcome {
            CommandOutcome::Notice(text) => self.show_notice(text),
            CommandOutcome::Error(text) => self.show_error(text),
            CommandOutcome::Quit => self.quit.store(true, Ordering::Relaxed),
            CommandOutcome::OpenModelPicker => self.open_model_picker(),
            CommandOutcome::OpenSessionPicker => self.open_resume_picker(),
            // No `interactive_commands` function actually returns `OpenSettings` today —
            // `open_settings` returns `Notice(settings_summary(..))` directly (see
            // `Self::run_settings`) — but the arm must still be honest, not the old
            // "the TUI does not hold a SettingsManager" placeholder, which stopped being
            // true once `Self::settings_manager` was added. If anything ever does
            // construct this variant, route it through the same real path `/settings` uses.
            CommandOutcome::OpenSettings => self.run_settings(),
            CommandOutcome::ToggleDebug => self.toggle_debug_panel(),
            // OSC 52: written straight to the terminal, not into the
            // transcript — it is a control sequence, not text to display.
            CommandOutcome::CopyToClipboard(sequence) => {
                self.tui.borrow_mut().write_raw(&sequence);
                self.show_notice("Copied the last assistant message to the clipboard");
            }
        }
    }

    /// `/export [path]` — write the transcript as JSONL (default) or HTML.
    pub(super) fn run_export(&mut self, arg: Option<&str>) {
        let state = self.session.state();
        let session_id = self.session.header().map(|h| h.id);
        let outcome = crate::interactive_commands::export_session(
            &state.messages,
            session_id.as_deref(),
            arg,
        );
        self.apply_outcome(outcome);
    }

    /// `/copy` — put the last assistant message on the clipboard via OSC 52,
    /// which works over SSH and needs no platform clipboard binding.
    pub(super) fn run_copy(&mut self) {
        let state = self.session.state();
        let outcome = crate::interactive_commands::copy_last_message(&state.messages);
        self.apply_outcome(outcome);
    }

    /// `/restart` — request a re-exec of the whole `pirust` process.
    ///
    /// Does **not** call `std::process::Command::spawn` itself. It only flips
    /// `restart_requested` and then quits through the *exact* path `/quit` already uses
    /// (`self.quit.store(true, ..)`, the `"quit"` arm above) — reusing that path is the
    /// point, not an implementation detail: it is what guarantees `run_async`'s normal
    /// exit, `Drop`, and `TUI::stop` (which restores the terminal out of raw mode) all
    /// run before anything re-execs. The actual `Command::new(current_exe).spawn()` lives
    /// one layer up, in `main.rs::run_interactive_mode`, strictly *after* `run_async`
    /// returns. Doing it here instead — synchronously, mid-keystroke-handling, with the
    /// terminal still in raw mode and the TUI still holding the alternate screen — would
    /// start a second process fighting the first over the same console, which is a wedged
    /// terminal, not a restart. See `run_interactive_mode`'s doc comment for the rest of
    /// this seam (argv round-trip, spawn-vs-wait, error reporting).
    pub(super) fn run_restart(&mut self) {
        // No `show_notice` here, deliberately: `/quit`'s own arm prints nothing either,
        // because anything drawn into the chat container now would be wiped the instant
        // `TUI::stop` leaves the alternate screen a few iterations later (the same fact
        // `run_setup_help_screen`'s doc comment in `main.rs` calls out for its own
        // post-teardown `eprintln!`). `main.rs::run_interactive_mode` is where a restart
        // failure gets reported, specifically because stderr survives past teardown and a
        // chat notice would not.
        self.restart_requested = true;
        self.quit.store(true, Ordering::Relaxed);
    }

    /// `/share [confirm]` — publish the transcript as a secret GitHub gist via `gh gist
    /// create` (no HTTP client dependency needed — see
    /// `interactive_commands::run_gist_share`'s doc comment).
    ///
    /// A bare `/share` never calls `gh`: it publishes the *entire* conversation,
    /// including anything the model quoted from the user's files, to a URL anyone who
    /// gets it can read (secret gists are unlisted, not access-controlled). That is worth
    /// a confirmation step, so `/share` alone only explains what would happen and names
    /// the exact command — `/share confirm` — that actually publishes. See
    /// `interactive_commands::share_confirmation_notice`'s doc comment for why this repo's
    /// existing `r`/`a`/`d` tool-approval prompt shape (`show_approval`/
    /// `handle_approval_key`) was not reused for this instead.
    pub(super) fn run_share(&mut self, arg: Option<&str>) {
        let confirmed = arg
            .map(str::trim)
            .is_some_and(|a| a.eq_ignore_ascii_case("confirm"));
        if !confirmed {
            self.apply_outcome(crate::interactive_commands::share_confirmation_notice());
            return;
        }
        let state = self.session.state();
        let session_id = self.session.header().map(|h| h.id);
        let outcome =
            crate::interactive_commands::run_gist_share(&state.messages, session_id.as_deref());
        self.apply_outcome(outcome);
    }

    /// `/trust [on|off]` — record this project's trust decision.
    pub(super) fn run_trust(&mut self, arg: Option<&str>) {
        let trusted = match arg.map(str::to_ascii_lowercase).as_deref() {
            None | Some("on") | Some("yes") | Some("trust") => true,
            Some("off") | Some("no") | Some("revoke") => false,
            Some(other) => {
                self.show_error(format!("Usage: /trust [on|off] (got {other:?})"));
                return;
            }
        };
        let cwd = match self.session.header().map(|h| h.cwd) {
            Some(cwd) => cwd,
            None => {
                self.show_error("/trust needs a session cwd, and this session reports none");
                return;
            }
        };
        let config = crate::config::ConfigEnv::from_process_env();
        let agent_dir = match config.agent_dir() {
            Ok(dir) => dir,
            Err(error) => {
                self.show_error(format!(
                    "/trust could not resolve the agent directory: {error}"
                ));
                return;
            }
        };
        let path = crate::interactive_commands::trust_store_path(&agent_dir);
        let outcome = crate::interactive_commands::set_project_trust(&path, &cwd, trusted);
        self.apply_outcome(outcome);
    }

    /// Lazily build and cache this TUI's own `SettingsManager` — see the
    /// [`Self::settings_manager`] field's doc comment for why it is a second,
    /// independent `SettingsManager` rather than a reference to `main.rs`'s.
    ///
    /// Cached rather than rebuilt on every `/settings`/`/scoped-models` call: rebuilding
    /// would re-read the on-disk files each time, which (a) is needless I/O for a value
    /// that only this same TUI process ever writes, and (b) would silently discard any
    /// unsaved in-memory write this manager already made (`SettingsManager::set_global_field`
    /// mutates `self.settings`/`modified_fields` before it persists — a fresh `create` call
    /// would replace that in-memory state with whatever was last flushed to disk).
    ///
    /// `cwd` and the trust decision come from the same seams `/trust` uses just above:
    /// the session header's `cwd`, and `is_project_trusted` read from the same trust-store
    /// file `/trust` itself writes — so a project the user has already trusted via `/trust`
    /// is honestly reported as trusted here too, rather than defaulting to `true` (Pi's own
    /// default, `settings.rs:1277`) or `false` regardless of the real decision on disk.
    pub(super) fn settings_manager(&mut self) -> Result<&mut SettingsManager, String> {
        if self.settings_manager.is_none() {
            let cwd = match self.session.header().map(|h| h.cwd) {
                Some(cwd) => cwd,
                None => {
                    return Err(
                        "Settings need a session cwd, and this session reports none".to_string()
                    )
                }
            };
            let env = crate::config::ConfigEnv::from_process_env();
            let agent_dir = env.agent_dir().map_err(|error| {
                format!("Could not resolve the agent directory for settings: {error}")
            })?;
            let trust_path = crate::interactive_commands::trust_store_path(&agent_dir);
            let project_trusted =
                crate::interactive_commands::is_project_trusted(&trust_path, &cwd);
            let options = SettingsManagerCreateOptions { project_trusted };
            let mgr = SettingsManager::create(&env, std::path::Path::new(&cwd), options)
                .map_err(|error| format!("Could not open settings for {cwd}: {error}"))?;
            self.settings_manager = Some(mgr);
        }
        Ok(self
            .settings_manager
            .as_mut()
            .expect("just set to Some above"))
    }

    /// `/settings` — a text rendering of the TUI's own `SettingsManager`
    /// ([`crate::interactive_commands::open_settings`]). Read-only: this command never
    /// writes anything, so there is no "takes effect later" caveat to report.
    pub(super) fn run_settings(&mut self) {
        match self.settings_manager() {
            Ok(mgr) => {
                let outcome = crate::interactive_commands::open_settings(mgr);
                self.apply_outcome(outcome);
            }
            Err(error) => self.show_error(error),
        }
    }

    /// `/scoped-models [provider/model]` — with no argument, show the current
    /// `enabledModels` scope; with one, toggle that model in it.
    ///
    /// The write really lands on disk (`toggle_scoped_model` ends in
    /// `SettingsManager::set_global_field` → `save()`), but nothing in this process reads
    /// `enabledModels` back afterward: confirmed by grep — the interactive TUI's model
    /// list (`InteractiveMode::model_entries`) is set once, at startup, from
    /// `main.rs::run_interactive_mode`'s `load_model_entries(model_runtime.providers())`,
    /// which never consults `SettingsManager::get_enabled_models`, and nothing else in this
    /// crate does either. So — unlike a command that mutates state the running session
    /// still reads — a toggle here has **no live effect on this session's model list or
    /// cycling**; it only takes effect the next time `pirust` starts and re-reads settings.
    /// The success text says so plainly rather than implying an immediate effect.
    pub(super) fn run_scoped_models(&mut self, arg: Option<&str>) {
        let model_id = arg.map(str::trim).filter(|s| !s.is_empty());
        let mgr = match self.settings_manager() {
            Ok(mgr) => mgr,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        let outcome = match model_id {
            None => CommandOutcome::Notice(crate::interactive_commands::scoped_models_summary(mgr)),
            Some(model_id) => {
                match crate::interactive_commands::toggle_scoped_model(mgr, model_id) {
                    CommandOutcome::Notice(text) => CommandOutcome::Notice(format!(
                        "{text} Saved to settings; this session does not re-read \
                         `enabledModels`, so it takes effect on the next `pirust` run, \
                         not this one."
                    )),
                    other => other,
                }
            }
        };
        self.apply_outcome(outcome);
    }

    /// `/changelog [path]` — pirust ships no `CHANGELOG.md`, so the path is
    /// required rather than guessed. Saying so beats reading a stale file from
    /// whatever directory the binary happens to sit in.
    pub(super) fn run_changelog(&mut self, arg: Option<&str>) {
        /// How many entries to show — enough to be useful, short enough not to
        /// bury the transcript.
        const MAX_ENTRIES: usize = 5;

        let Some(path) = arg else {
            self.show_error(
                "Usage: /changelog <path-to-CHANGELOG.md> — pirust does not ship one, \
                 so there is no default to read",
            );
            return;
        };
        let outcome =
            crate::interactive_commands::changelog_text(std::path::Path::new(path), MAX_ENTRIES);
        self.apply_outcome(outcome);
    }

    /// `/debug` — show or hide the debug panel.
    ///
    /// The panel reads the same bounded ring buffer the panic report does, so
    /// turning it on costs nothing extra: the events were already being
    /// recorded, they just were not on screen.
    pub(super) fn toggle_debug_panel(&mut self) {
        self.debug_panel.borrow_mut().toggle();
        let visible = self.debug_panel.borrow().is_visible();
        let path = std::env::var(crate::interactive_debug::DEBUG_LOG_ENV).ok();
        if visible {
            let sink = match &path {
                Some(path) => format!(" · also logging to {path}"),
                None => format!(
                    " · set {}=<path> to also write a log file",
                    crate::interactive_debug::DEBUG_LOG_ENV
                ),
            };
            self.show_notice(format!("Debug panel on{sink}"));
        } else {
            self.show_notice("Debug panel off");
        }
        self.repaint();
    }

    /// `/thinking [on|off]` — expand or collapse reasoning blocks.
    ///
    /// Ctrl+O toggles the most recent block; this sets *all* of them at once,
    /// which is what you want after the fact when reading back a long session.
    pub(super) fn toggle_thinking(&mut self, arg: Option<&str>) {
        let expanded = match arg.map(str::to_ascii_lowercase).as_deref() {
            Some("on") | Some("expand") | Some("show") => true,
            Some("off") | Some("collapse") | Some("hide") => false,
            None => true,
            Some(other) => {
                self.show_error(format!("Usage: /thinking [on|off] (got {other:?})"));
                return;
            }
        };
        self.thinking.toggle_all(expanded);
        self.show_notice(if expanded {
            "Reasoning blocks expanded (Ctrl+O toggles the latest)"
        } else {
            "Reasoning blocks collapsed (Ctrl+O toggles the latest)"
        });
        self.repaint();
    }

    /// `/reload-extensions` (Wave 5) — rescan `<agent_dir>/extensions/*.wasm`
    /// for extensions not already loaded, without restarting `pirust`.
    pub(super) fn reload_extensions(&mut self) {
        match self.session.reload_wasm_extensions() {
            Ok(0) => self.show_notice("No new extensions found"),
            Ok(count) => self.show_notice(format!("Loaded {count} new extension(s)")),
            Err(error) => self.show_error(format!("Could not reload extensions: {error}")),
        }
    }
}
