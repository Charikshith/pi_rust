# TUI — pending action plan

State as of 2026-08-26, branch `feat/tui-design-spec-buildout`.

The build-out against [`tui-design-samples.html`](tui-design-samples.html) is
done: all 11 spec gaps closed, all 28 slash commands live (the dead-command
count went 18 → 0), workspace at **1131 tests passing, 0 failing**, clippy clean
apart from one pre-existing warning, release build green.

This file lists what is genuinely left. Every item below was verified against
the source, not assumed. Items are ordered by whether a user would notice.

---

## P0 — Commit the second half

**Status:** ~10 modified files plus `tests/tui_compact.rs` are uncommitted.
First half is committed as `2b0a12b`.

Nothing else here can be reviewed cleanly until this lands.

- [ ] Commit the working tree on `feat/tui-design-spec-buildout`
- [ ] Push and open a PR against `master` (still untouched at `cf5ea2f`)

**Acceptance:** `git status` clean; CI green on the PR.

---

## P1 — Accessibility promise is half-kept — DONE (2026-09-01)

**The finding:** `interactive_a11y::A11ySettings` detects four flags. Only
`ascii_only` (via `glyph`) and `color` (via `interactive_theme::fg/bg`) were
consumed. `reduced_motion` and `verbose_state` appeared **nowhere outside
`interactive_a11y.rs`**.

**What was actually wired, once traced to real call sites:**

- `Loader`/`CancellableLoader` (`pirust-tui`) turned out to be dead code —
  never instantiated anywhere in `pirust-coding-agent`, so there was no live
  spinner to gate. The one real, shipping animation is
  `interactive_thinking.rs`'s collapsed streaming view, which rewrites its
  last-line preview on every `push_delta`. `reduced_motion` now suppresses
  that preview, keeping the static `"▸ Thinking… (N lines)"` header (real
  progress, not decoration).
- The one genuine "no colour-only meaning" violation was
  `ToolExecutionComponent::bg_hex` in `interactive_mode.rs`: a tool box's
  pending/success/error state was conveyed **only** by background colour.
  `format_tool_execution` now prefixes `interactive_a11y::state_label` when
  `verbose_state` is on, driven by a new `state_word()` that both `bg_hex`
  and the label consult (so colour and label can't disagree).
  `show_error`/the status line were checked too: both were already plain,
  uncolored text with no color-only gap, so left untouched — adding a
  redundant label there would have been scope creep, not a fix.
- Two existing `interactive_thinking.rs` tests assumed `reduced_motion:
  false`, which is *not* the real default in a non-TTY `cargo test` process
  (`detect_from`'s non-TTY rule forces it true) — both now pin the setting
  via `with_settings` instead of relying on ambient state.

**Tests added:** `interactive_thinking::tests::reduced_motion_suppresses_live_last_line_preview`,
`interactive_mode::tool_execution_component_tests::{verbose_state_labels_pending_success_and_error, verbose_state_off_omits_the_label}`.
Workspace: 1134 tests passing (was 1131), clippy clean apart from the
pre-existing `latex.rs:30` warning.

---

## P2 — Split-turn compaction silently drops a message

**The finding (pre-existing, not from this work):**
`CompactionPreparation.turn_prefix_messages` is computed by
`prepare_compaction` in
`crates/pirust-agent-core/src/harness/compaction/v4.rs` and **never consumed by
any caller**. Both `AgentHarness::compact_inner` (the RPC path) and the new
`SingleTurnSession::compact_inner` ignore it. When a compaction cut falls
mid-turn, that message's content is lost.

This affects RPC mode too, so it is not a TUI bug and should not be fixed as
one.

- [ ] Read `prepare_compaction` and determine what `turn_prefix_messages` is
      meant to be prepended to.
- [ ] Fix both call sites together, or fix it inside `prepare_compaction` so no
      caller can forget.
- [ ] Add a test that compacts with a cut deliberately placed mid-turn and
      asserts the prefix survives. The existing `tests/tui_compact.rs` test
      deliberately avoids this case — extend it rather than replacing it.

**Acceptance:** a mid-turn cut loses no message content, proven by a test that
fails before the fix.

**Size:** medium, and it touches `pirust-agent-core`. Needs care: `v4.rs` has a
byte-parity golden suite behind it.

---

## P3 — `/resume` picker's model column is always blank

`interactive_pickers::load_session_entries` takes
`models_by_id: &HashMap<String, String>` and `runtime_host.rs` passes an empty
map, because `SessionInfo` carries no model — a session's model exists only as
`model_change` entries *inside* the transcript. Filling the column naively means
opening every session file just to draw a list.

Options, cheapest first:

- [ ] **(a)** Leave it blank and drop the column. Honest, zero cost, and nobody
      misses a column they never saw.
- [ ] **(b)** Populate lazily: only read the transcript for the rows currently
      in the viewport, cached by session id.
- [ ] **(c)** Record the model in the session header at creation so listing is
      free. Best long-term, but it is an on-disk format change and the format
      is byte-compatible with Pi — check `session_golden.rs` before touching it.

**Recommendation:** (a) now, (b) if users ask. Do not do (c) casually.

**Acceptance:** either the column is gone, or it is populated without a
full-store read on every `/resume`.

**Size:** (a) trivial, (b) small, (c) do not.

---

## P4 — Two paths that ship untested

Both are honest gaps, already documented in code. Neither is a defect; they are
verification debt.

### `/share` success path
`gh` is **not installed** on this machine, so gist creation, URL parsing, and
the not-authenticated stderr passthrough were never exercised. The
not-installed path *is* tested against a real `Command` call.

- [ ] Verify manually on a machine with `gh` authenticated: run `/share`, then
      `/share confirm`, confirm the printed URL resolves and the gist is secret.
- [ ] Optionally introduce a seam so the `gh` invocation can be faked (inject
      the program name, or a trait with one method) and test URL parsing and
      the non-zero-exit passthrough without the binary.

### `/restart` re-exec
The flag-and-clean-shutdown path is tested; the actual
`current_exe()` + `spawn()` is not, because tests must not spawn processes.

- [ ] Verify manually: `/restart` in a real terminal, confirm the session
      restarts, argv is preserved (test with `--resume <id>`), and the terminal
      is not left in raw mode.
- [ ] Confirm no orphan process is left if the child fails to start.

**Acceptance:** both manually verified once and the result recorded here.

**Size:** small, but needs a human at a terminal.

---

## P5 — `SingleTurnRuntimeHost` is still a shell

Five of six `AgentSessionRuntimeHost` methods still return `Value::Null` with a
`// Unreachable this wave` comment, and `main.rs` still passes
`CommandContextActions::placeholder()` in the interactive arm.

**This does not affect the TUI's own commands** — they call `PrintModeSession`
directly, which is why `/new`, `/fork`, `/clone`, `/import` work today. It does
mean **an extension cannot drive** `new_session`/`fork`/`switch_session`, and
`set_rebind_session`'s callback is stored and never invoked.

The underlying work is now all done, so this is pure delegation:

- [ ] Point the host's `new_session`/`fork`/`switch_session` at the real
      `SingleTurnSession` methods that already exist.
- [ ] Make `set_rebind_session` store and invoke the callback.
- [ ] Build real `CommandContextActions` in `main.rs`'s interactive arm instead
      of `placeholder()`.
- [ ] Update the module doc at the top of `runtime_host.rs`, which currently
      states the callback is dead by construction.

**Prerequisite already met:** the `subscribe` listener leak is fixed, so calling
the rebind path more than once no longer duplicates `--json` output. That fix
was landed specifically to unblock this.

**Acceptance:** an extension-invoked `new_session` swaps the session and the TUI
re-renders against it.

**Size:** medium. Files: `runtime_host.rs`, `main.rs`.

---

## P6 — Performance: the render cache clones every frame

`Component::render(&mut self, width) -> Vec<String>` returns owned data, so a
cache *hit* still allocates a fresh `Vec<String>` plus every `String` in it.
Confirmed in 4 places: `text.rs`, `interactive_welcome.rs` (×2),
`interactive_thinking.rs`, `interactive_debug.rs`. Several modules document this
honestly as unavoidable *under the current trait signature* — which is the point.

- [ ] Measure first. Add a bench or instrument a long transcript at 200×50.
      This may be irrelevant next to terminal I/O; do not refactor on a hunch.
- [ ] If it matters, change the trait to `fn render(&mut self, width) -> &[String]`
      (or add a `render_cached` returning a borrow). This is a **wide** change
      across `pirust-tui` and every component.
- [ ] The differential renderer already avoids redundant *writes*; this is
      purely about allocation, so the win shows up as CPU/allocator pressure on
      long sessions, not as visible lag.

**Acceptance:** a measured before/after. No change without a number.

**Size:** large and risky. Do not start without P0–P5 done.

---

## P7 — Housekeeping

- [ ] `interactive_mode.rs` is **2,915 lines**. It has accumulated command
      handlers, picker lifecycles, event rendering, and the loop. Consider
      extracting the slash-command handlers into `interactive_mode/commands.rs`
      and the picker lifecycle into `interactive_mode/pickers.rs`. Mechanical,
      but do it in one commit that moves code without changing it.
- [ ] Pre-existing clippy warning: `crates/pirust-tui/src/latex.rs:30`
      ("may be rewritten with the `?` operator"). Untouched by this work.
- [ ] Subagent shell redirects left ~15 stray 0-byte files in the repo root
      during this session (`i32`, `bool`, `SessionStateView`, …). All cleaned,
      but worth a `.gitignore` guard or a pre-commit check if agents keep
      running here.

---

## The pattern, for whoever picks this up

Every capability gap in this repo had one shape: the lower layer worked, but
`PrintModeSession` / `TuiRuntimeInfo` had no method reaching it. To add one:

1. Declare it in `print_mode.rs` **with a default body** returning an
   "unavailable" `Err`. Never required — four test files hand-implement
   `PrintModeSession` and three hand-implement `TuiRuntimeInfo`; a required
   method breaks them all at once.
2. Implement it on `SingleTurnSession` in `runtime_host.rs`.
3. Wire `dispatch_command` in `interactive_mode.rs`, updating
   `slash_command_available` **in the same edit** — a test enforces parity.

**The trap that cost the most time this session:** a feature can look finished
— UI wired, tests green — while its trait method is still only the default stub
on `SingleTurnSession`. The tests pass because the test stubs use those same
defaults. `branch_entries` and `set_model_by_name` were both caught this way.
**Always grep `runtime_host.rs` for a real impl before believing a feature
works.**
