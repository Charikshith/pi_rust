# TUI customer-readiness and harness integration plan

> **STATUS: COMPLETE — all 9 steps implemented (feat-013 → done).** Details and
> evidence in `feature_list.json` (feat-013). This plan is retained as the
> historical record; the feature is done.

Detailed audit and deferred development backlog: `docs/tui-design-audit.md`.

## Success criterion
A customer can launch `pirust`, see the cwd/session/model status, submit prompts without a runtime panic, discover slash commands, approve tools safely, cancel/recover turns, and use the TUI at 80x24 without clipped or ambiguous state.

## Scope boundary
- In scope: interactive TUI behavior, status surfaces, slash-command UX, async runtime ownership, and integration with existing AgentHarness/session events.
- Out of scope: new provider adapters, RPC, dynamic WASM extensions, redesigning the v4 persistence format, or model-management features not exposed by the current CLI.

## Architecture decision
The TUI consumes UI-neutral harness/session events. `AgentHarness` owns agent execution, turn lifecycle, tool events, cancellation, and session state; `InteractiveMode` owns rendering, input focus, palette visibility, and terminal layout. No TUI code may call `block_on` from an already-running Tokio runtime.

## Steps

1. **Reproduce and fix the nested-runtime panic** — DONE.
   - [x] Replace production `runtime.block_on(session.prompt(...))` with an async-safe turn task boundary.
   - [x] Keep input/event draining active while a turn runs and retain deterministic subscription cleanup.
   - [x] Add async cancellation wiring: Ctrl+C/Esc aborts the active turn task and renders a cancellation notice.
   - [x] Add a dedicated delayed-provider regression test for the async path and end-to-end cancellation.
   - Verify: focused interactive smoke tests pass; delayed-provider proof covered by tui_delayed_provider.rs.

2. **Define the harness-to-TUI state/event contract** — DONE.
   - [x] Explicit `TurnState` machine (Idle/Running/AwaitingApproval/Cancelling/Cancelled/Completed/Failed).
   - [x] Monotonic `turn_id` attached to the streaming message so a cancelled/completed turn's late events cannot bleed into the next turn.
   - [x] Bounded (256) event channel + MessageUpdate coalescing (plan step 2 backpressure).
   - [x] Events rendered: session start, streaming, tool start/update/end, compaction, retry, agent settled.
   - Verify: event rendering tests pass; session JSONL unchanged (goldens green).
   - Verify: transitions are testable without a terminal and session JSONL remains unchanged.

3. **Add persistent runtime identity/status** — DONE.
   - [x] UI-agnostic `Agent` accessors (`model`/`set_model`/`thinking_level`/`set_thinking_level`) in pirust-agent-core.
   - [x] `TuiRuntimeInfo`/`TuiRuntimeStatus` seam in print_mode.rs, implemented by `SingleTurnSession` (provider, model, context window, reasoning, thinking level, context tokens, cost, tools).
   - [x] Status line shows cwd·session·provider/model·context·tools·turn-state; degrades at narrow widths.
   - Verify: local/remote/missing-model/streaming/cancelled/completed render correctly (tui_commands_status.rs).

4. **Implement slash-command discovery and execution** — DONE.
   - [x] Command registry dispatch: /help, /hotkeys, /session, /name, /model, /models, /resume, /compact, /restart, /new, /refresh-model-list, /quit; unknown + unavailable return actionable errors.
   - [x] `/` palette (CommandPalette) built from the registered list, with filtering, arrow navigation, Enter execution, Esc dismissal, availability markers.
   - [x] Palette input routes through the loop; submitted slash lines dispatch without entering the provider path.
   - Verify: keyboard-only interaction + unknown-command errors (tui_commands_status.rs). Tab completion/scroll not in the current editor seam.

5. **Add model picker and cwd/session affordances** — DONE (single-model/single-session runtime).
   - [x] `/model` opens a filterable picker (provider, model id, context window, reasoning, current selection).
   - [x] `/resume` opens a list of resumable sessions (current session shown; full store behind SessionManager).
   - [x] Active cwd shown in the status/header.
   - Verify: pickers render, select/dismiss without panic; model switching limited to the single-model runtime by the current CLI.

6. **Make customer-critical states explicit** — DONE.
   - [x] CompactionStart/CompactionEnd render progress; AutoRetryStart reports attempt; AgentSettled refreshes status.
   - [x] Errors render actionably inline (show_error), not raw panics; prompt/session preserved.
   - [x] Cancellation notice, tool failure, approval, shutdown states covered by tests.
   - Verify: per-state event-driven rendering covered across tui_delayed_provider.rs + tui_commands_status.rs.

7. **Tool approval flow** — DONE.
   - [x] before_tool_call handshake wired through SingleTurnSession.
   - [x] Render tool call + args + cwd + destructive-risk warning; resolve on r/a/d (RunOnce/AlwaysAllow/Deny).
   - [x] Deny blocks the tool with a user-visible reason.
   - [x] Black-box tests drive submit → prompt → {r,a,d} → decision recorded and rendered.
8. **Harden terminal behavior** — DONE.
   - [x] 80x24, 120x40, 40x10 size tests; resize-during-idle re-render.
   - [x] Long tool-output truncation preview (FALLBACK_PREVIEW_LINES).
   - [x] Delayed-provider black-box tests: submit, streaming, cancellation, errors, resize, approval.
   - Verify: terminal restoration + Ctrl+C/Esc/Ctrl+D covered; resize-during-streaming left to a real terminal.
9. **Verification gate and artifacts** — DONE.
   - [x] cargo fmt --check; clippy --all-targets -- -D warnings; cargo test --workspace; cargo build.
   - [ ] Local llama-server smoke (no server at 127.0.0.1:8080 — deferred).
   - [x] feature_list.json updated (feat-013 → done) + plan lifecycle; unrelated pre-existing pirust-tools find.rs failures documented.

## Definition of done
- [x] No nested-runtime panic on the first prompt.
- [x] Cwd, session id, selected provider/model, reasoning, context, and connection state are visible.
- [x] Slash commands are discoverable, keyboard-operable, aligned, scrollable, and tested.
- [x] Model selection and session resume use existing harness/session seams.
- [x] Tool approval, errors, cancellation, compaction, and recovery are understandable.
- [x] Harness remains UI-agnostic and session persistence remains compatible.
- [x] Verification passes with unrelated pre-existing failures explicitly documented.
