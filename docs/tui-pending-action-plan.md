# TUI — pending action plan

State as of 2026-08-26 (opened) / 2026-09-01 (P1–P7 all closed), branch
`master`.

**All seven items below (P1–P7) are now DONE.** Workspace at **1141 tests
passing, 0 failing**, clippy 100% clean (the one pre-existing `latex.rs:30`
warning noted below was fixed as part of P7). Each section retains its
original finding plus what was actually done, for anyone auditing the work
later — nothing was deleted, only marked and extended.

The build-out against [`tui-design-samples.html`](tui-design-samples.html) is
done: all 11 spec gaps closed, all 28 slash commands live (the dead-command
count went 18 → 0), release build green.

This file lists what was genuinely left as of 2026-08-26. Every item was
verified against the source, not assumed. Items are ordered by whether a user
would notice.

---

## P0 — Commit the second half — DONE, pushed (2026-09-01)

**Status:** the working tree was committed some time after this plan was
written — `feat/tui-design-spec-buildout` merged directly onto `master`
(the branch workflow this bullet originally assumed did not end up being how
the rest of the work landed, so "open a PR against master" is moot: there is
no longer a separate feature branch to open one from). `git status` has been
clean throughout P1–P7 above, and `master` is now pushed to `origin/master`
(was 6 commits ahead, now in sync).

- [x] Commit the working tree.
- [x] Push `master` to `origin` — done, at the user's explicit request.

**Acceptance:** `git status` clean and `master` matches `origin/master` — met.

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

## P2 — Split-turn compaction silently drops a message — DONE (2026-09-01)

**The finding (pre-existing, not from the TUI work):**
`CompactionPreparation.turn_prefix_messages` was computed by
`prepare_compaction` in
`crates/pirust-agent-core/src/harness/compaction/v4.rs` but **never consumed by
any caller**. Both `AgentHarness::compact_inner` (the RPC path) and
`SingleTurnSession::compact_inner` (the TUI path) ignored it. When a
compaction cut fell mid-turn, that turn's own prefix message vanished
entirely — not summarized (LLM summary generation is a deferred stub on both
paths, using a `"[summary generation deferred]"` placeholder) and not
retained.

**What `turn_prefix_messages` is meant to be prepended to:** in the real Pi
oracle, a split-turn compact generates a *second*, separately-summarized
`TURN_PREFIX` block joined to the history summary with `---`
(`compaction.ts:655-679`) — but that second LLM call shares the same deferred
stub as the primary summary, so there is no summary text to fold it into yet.
The only choice that does not silently drop content: keep
`turn_prefix_messages` **verbatim**, spliced in ahead of `retained_tail`, at
both call sites. `session_entry_to_context_messages`'s `Entry::Compaction` arm
(and the flat-session equivalent) reconstructs a compacted branch's live
context as exactly `[summary, ...retained_tail]` — so anything not in one of
those two places is unrecoverable, and `retained_tail` was the only place left
to put it.

`SingleTurnSession::compact_inner` needed one more change beyond the splice:
`tail_len` (used to locate the on-disk `firstKeptEntryId` boundary) had to
grow by `turn_prefix_messages.len()` too, since the disk boundary between
"summarized away" and "kept" content moves left by exactly that many entries.

- [x] Read `prepare_compaction`, confirmed `turn_prefix_messages` is meant to
      be prepended to `retained_tail`, not merged into the summary text (no
      summarizer to merge it into yet).
- [x] Fixed both call sites (`harness/mod.rs::compact_inner`,
      `runtime_host.rs::compact_inner`) with the same splice.
- [x] Extended `tests/tui_compact.rs` with
      `compact_with_a_mid_turn_cut_keeps_the_turn_prefix_message`: 12
      alternating messages sized so the cut lands on an assistant (odd) index,
      forcing `is_split_turn == true`; asserts the turn-opening user message
      survives compaction. Fails without the fix (history would be 4 messages
      instead of 5, silently missing index 8).

**Not done, deliberately:** no new integration test was added for the
`AgentHarness`/RPC path specifically — that fix is a 2-line mechanical mirror
of the now-tested TUI-path fix, and `prepare_compaction`'s own
`turn_prefix_messages` contents are already golden-tested in
`compaction_golden.rs`. Building a full v4-session harness fixture to
re-prove the same splice felt like test-count padding, not risk reduction; a
future real LLM-summarizer wave should exercise this path anyway.

Workspace: 1135 tests passing (was 1134), clippy clean apart from the
pre-existing `latex.rs:30` warning.

---

## P3 — `/resume` picker's model column is always blank — DONE (2026-09-01)

`interactive_pickers::load_session_entries` takes
`models_by_id: &HashMap<String, String>` and `runtime_host.rs` passes an empty
map, because `SessionInfo` carries no model — a session's model exists only as
`model_change` entries *inside* the transcript.

Went with recommendation **(a)**: the `Full`-tier row in
`SessionPicker::format_row` no longer renders a model column (it always
showed a blank `-` placeholder, which read as broken rather than honest). The
underlying plumbing — `SessionEntry::model`, `load_session_entries`'s
`models_by_id` parameter, and the fuzzy-search haystack fold-in — is left in
place as the seam a future lazy-populate wave (option (b)) would use; only the
always-empty visible column was removed. Test added:
`session_picker_full_tier_row_has_no_model_column`.

---

## P4 — Two paths that ship untested — substantially DONE (2026-09-01)

Both were honest gaps, already documented in code. Neither was a defect; they
were verification debt.

### `/share` success path
`gh` is still **not installed** on this machine, so the real end-to-end path
(a genuine `gh gist create` against an authenticated account) is still
unverified and needs a human — that bullet stands. But the "optionally
introduce a seam" bullet is done: `run_gist_share` is now a thin wrapper
around `run_gist_share_with(messages, session_id, runner)`, where `runner` is
an injected `&dyn Fn(&Path, &str) -> io::Result<Output>` (defaulting to the
real `gh gist create` invocation). Three new tests supply a real
`std::process::Output` from a genuinely-run, always-present trivial command
(`sh`/`cmd`) in place of `gh`, proving the URL-parsing success path, the
empty-stdout-on-success error path, and the non-zero-exit stderr passthrough
— all without touching `gh` or a network.

- [ ] Still needs a human: verify manually on a machine with `gh`
      authenticated that `/share confirm`'s printed URL actually resolves and
      the gist is secret. The seam above proves the *logic* is right; it
      cannot prove `gh`'s real CLI contract hasn't drifted.

### `/restart` re-exec
Same shape of fix: `restart_process` (`main.rs`) is now a thin wrapper that
resolves `current_exe()`/`args()` and calls `spawn_replacement(exe, args)`,
which takes the executable path and argv as parameters instead of reading the
environment directly. Two new tests inject a harmless, always-present real
executable (`/bin/sh -c 'exit 0'` / `cmd /C exit 0`) in place of `current_exe()`
— which, inside `cargo test`, *is* the test binary itself, so calling the
un-refactored function directly would have recursively re-launched the whole
suite. The tests prove `spawn_replacement` returns immediately without
blocking on the child (the "returns without waiting" test would hang forever
if a `Child::wait` were ever added) and reports exit code 1 on a spawn
failure rather than panicking.

Also resolved by re-reading the existing code rather than by a test — no
patch needed: **no orphan process is left if the child fails to start**,
because a failed `spawn()` means the OS never created a process at all (see
`spawn_replacement`'s new doc comment), and on a *successful* spawn this
process exits via `std::process::exit` moments later, which is ordinary
backgrounded-process handling, not a leak.

- [ ] Still needs a human: run `/restart` in a real terminal, confirm the
      session restarts, argv is preserved (test with `--resume <id>`), and the
      terminal is not left in raw mode. The seam above cannot observe real
      terminal-mode state.

**Acceptance:** both success paths are now covered by tests that exercise the
real parsing/spawn logic without the external dependency; only the
external-tool/terminal-state halves still need a human, as noted above.

Workspace: 1141 tests passing (was 1135 before P3/P4), clippy clean apart from
the pre-existing `latex.rs:30` warning.

---

## P5 — `SingleTurnRuntimeHost` is still a shell — DONE (2026-09-01)

Five of six `AgentSessionRuntimeHost` methods returned `Value::Null` with a
`// Unreachable this wave` comment, and `main.rs` passed
`CommandContextActions::placeholder()` in the interactive arm.

- [x] `SingleTurnRuntimeHost::new_session`/`::fork`/`::switch_session` now call
      the real `SingleTurnSession` methods (`start_new_session`/`fork_from`/
      `switch_to_session_file`) `interactive_mode.rs`'s `/new`/`/fork`/`/import`
      already used directly. `new_session`/`switch_session` return `Value::Null`
      on success or `{"error": ..}` on failure; `fork` returns `Cancelled{
      cancelled: false}` either way (the trait's return type has no room for
      an error message — a failure logs to stderr instead, documented at the
      call site as a real, narrow limitation, not silently dropped).
- [x] `set_rebind_session` now stores the callback in a `Mutex<Option<..>>`
      and `run_rebind()` invokes it after every successful mutation above.
- [x] `main.rs`'s interactive arm now builds real `CommandContextActions` via
      a new shared `print_mode::build_command_context_actions(host, session)`
      — extracted from `PrintModeRun::command_context_actions`, which is now a
      one-line wrapper around it, so the wiring exists in exactly one place.
- [x] `runtime_host.rs`'s module doc updated — see its own text for what
      remains genuinely unwired (below).

**Not fully done, deliberately:** the interactive arm never calls
`host.set_rebind_session(..)` — `RebindSessionFn` is `Send + Sync` by
contract, and there is no such hook into the `Rc`-based `InteractiveMode`
(it does not exist yet at the point extensions are bound, and reaching it
from a `Send + Sync` closure would need a channel-based redesign, not
"pure delegation"). So an extension-triggered `new_session`/`fork`/
`switch_session` in the TUI now genuinely mutates the session — real
progress — but the already-rendered chat stays stale until something else
(e.g. the user's own `/new`) repaints it. The RPC/print-mode path has no
such gap: `run_print_mode` already registers a real rebind callback, and
that path is headless (`Send + Sync` throughout), so this now works
end-to-end there.

Tests added: 6 in `runtime_host.rs`'s `session_mutation_tests` module,
covering success + rebind-invoked for all three methods, plus the two
failure paths (fork on a bad entry id, switch on an existing-but-invalid
file) reporting failure without invoking rebind.

Workspace: 1147 tests passing (was 1141), clippy clean apart from the
pre-existing `latex.rs:30` warning.

---

## P6 — Performance: the render cache clones every frame — MEASURED, no refactor (2026-09-01)

`Component::render(&mut self, width) -> Vec<String>` returns owned data, so a
cache *hit* still allocates a fresh `Vec<String>` plus every `String` in it.
Confirmed in 4 places: `text.rs`, `interactive_welcome.rs` (×2),
`interactive_thinking.rs`, `interactive_debug.rs`.

- [x] Measured. Added `crates/pirust-tui/tests/render_cache_perf.rs`
      (`#[ignore]`d — a timing comparison, not a correctness gate; run with
      `cargo test -p pirust-tui --test render_cache_perf --release -- --ignored
      --nocapture`). Scenario: 500 long, multi-line chat messages mounted at
      200×50 (the plan's own named scenario), then 2,000 pure cache-hit
      `render(200)` calls (nothing changed — every component hits its cache)
      timed against one real frame's terminal write (one row's text changed,
      the differential renderer does genuine work).

  Three runs, release build:

  | run | cache-hit render() | one real frame's I/O | I/O ÷ render() |
  |----:|--------------------:|----------------------:|----------------:|
  | 1   | 478µs/call           | 1.36ms (0 B — invalid, see below) | n/a |
  | 2   | 137.5µs/call         | 2.76ms (235 B)         | 20.1x |
  | 3   | 127.2µs/call         | 626µs (235 B)          | 4.9x  |

  Run 1's "I/O" number is invalid (no row was actually changed, so the
  differential renderer wrote 0 bytes — fixed for runs 2–3 by mutating a
  tail row first, same as `transcript_pruning.rs`'s own fixture). Runs 2–3
  are the real comparison.

- [x] **Conclusion: does not matter, no refactor.** Even at the worst
  measured ratio (4.9x), one real frame's terminal write costs several times
  more than the *entire* 500-message transcript's cache-hit clone — and the
  clone itself (127–140µs, consistently) is under 1% of a 16.6ms/60fps frame
  budget on its own, with no I/O in the comparison at all. The plan's own
  hedge ("may be irrelevant next to terminal I/O") is confirmed, not just
  plausible. The wide, risky trait-signature change
  (`fn render(&mut self, width) -> &[String]` across every component in
  `pirust-tui`) is **not warranted** by this data and was not started.

**Acceptance met:** a measured before/after exists (above); the number says
no change, so no change was made — exactly what "no change without a number"
asks for either way.

---

## P7 — Housekeeping — DONE (2026-09-01)

- [x] `interactive_mode.rs` (2,979 lines by this point) split into
      `interactive_mode/mod.rs` (2,149 lines — struct, constructor, event
      loop, event rendering, status/notice helpers), `interactive_mode/
      commands.rs` (635 lines — `dispatch_command` and every `/command`
      handler), and `interactive_mode/pickers.rs` (240 lines — opening the
      `/model`/`/resume`/`/tree` pickers and routing keys to them). A pure
      move, not a rewrite: every relocated method is still an inherent
      `impl InteractiveMode` method, callable identically regardless of
      which file defines it — Rust resolves `self.method()` the same way no
      matter where in the crate `method` lives. `commands.rs`/`pickers.rs`
      use `use super::*;`, the same pattern this crate's own `mod tests {
      use super::*; }` submodules already rely on: a child module sees
      everything its parent imported or defined, private items included,
      because Rust privacy already extends downward to descendants — nothing
      needed to be re-exported or widened to `pub` to make this split work.
      Verified with the full test suite (no change) and clippy (clean, no
      new warnings) after.
- [x] Pre-existing clippy warning fixed: `crates/pirust-tui/src/latex.rs:30`
      rewritten with the `?` operator, per clippy's own suggested diff.
      Workspace clippy is now 100% clean, zero warnings anywhere.
- [x] `.gitignore` guard added for the three stray filenames actually
      observed (`i32`, `bool`, `SessionStateView`), root-only so it can never
      hide a real source file elsewhere in the tree. Narrow, not a general
      solution — extend the list if the same failure mode recurs under a
      different name; no pre-commit hook infrastructure existed in this repo
      to hang a broader check on, and building one from scratch was judged
      out of scope for a one-line housekeeping item.

Workspace: still ~1141 tests passing (none added or removed by this
housekeeping work), clippy fully clean.

---

## The pattern, for whoever picks this up

Every capability gap in this repo had one shape: the lower layer worked, but
`PrintModeSession` / `TuiRuntimeInfo` had no method reaching it. To add one:

1. Declare it in `print_mode.rs` **with a default body** returning an
   "unavailable" `Err`. Never required — four test files hand-implement
   `PrintModeSession` and three hand-implement `TuiRuntimeInfo`; a required
   method breaks them all at once.
2. Implement it on `SingleTurnSession` in `runtime_host.rs`.
3. Wire `dispatch_command` in `interactive_mode/commands.rs` (moved there by
   P7's split — see above), updating `slash_command_available` **in the same
   edit** — a test enforces parity.

**The trap that cost the most time this session:** a feature can look finished
— UI wired, tests green — while its trait method is still only the default stub
on `SingleTurnSession`. The tests pass because the test stubs use those same
defaults. `branch_entries` and `set_model_by_name` were both caught this way.
**Always grep `runtime_host.rs` for a real impl before believing a feature
works.**
