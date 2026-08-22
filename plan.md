# TUI customer-readiness and harness integration plan

Detailed audit and deferred development backlog: `docs/tui-design-audit.md`.

## Success criterion
A customer can launch `pirust`, see the cwd/session/model status, submit prompts without a runtime panic, discover slash commands, approve tools safely, cancel/recover turns, and use the TUI at 80x24 without clipped or ambiguous state.

## Scope boundary
- In scope: interactive TUI behavior, status surfaces, slash-command UX, async runtime ownership, and integration with existing AgentHarness/session events.
- Out of scope: new provider adapters, RPC, dynamic WASM extensions, redesigning the v4 persistence format, or model-management features not exposed by the current CLI.

## Architecture decision
The TUI consumes UI-neutral harness/session events. `AgentHarness` owns agent execution, turn lifecycle, tool events, cancellation, and session state; `InteractiveMode` owns rendering, input focus, palette visibility, and terminal layout. No TUI code may call `block_on` from an already-running Tokio runtime.

## Steps

1. **Reproduce and fix the nested-runtime panic** — PARTIAL: production now uses `run_async` and spawns prompt tasks; the synchronous `run` wrapper remains only for legacy tests.
   - [x] Replace production `runtime.block_on(session.prompt(...))` with an async-safe turn task boundary.
   - [x] Keep input/event draining active while a turn runs and retain deterministic subscription cleanup.
   - [x] Add async cancellation wiring: Ctrl+C/Esc aborts the active turn task and renders a cancellation notice.
   - [x] Add a dedicated delayed-provider regression test for the async path and end-to-end cancellation.
   - Verify: focused interactive smoke tests pass; full delayed-provider proof remains open.

2. **Define the harness-to-TUI state/event contract**
   - Confirm events for session start, user/assistant streaming, thinking, tool start/update/end, approval, turn end, agent settled, error, cancellation, compaction, and persistence.
   - Add only missing UI-neutral fields/seams to the harness; keep terminal rendering out of `AgentHarness`.
   - Verify: transitions are testable without a terminal and session JSONL remains unchanged.

3. **Add persistent runtime identity/status** — PARTIAL: cwd, session id, and connection/turn state now render in a persistent status line; provider/model/reasoning/context/cost still need runtime metadata seams.
   - Display cwd, session id, provider, selected model, reasoning level, context usage, token/cost estimate, tool availability, connection state, and active-turn state.
   - Keep it visible in a compact footer/header and degrade at small widths.
   - Verify: local, remote, missing-model, disconnected, streaming, cancelled, and completed states render correctly.

4. **Implement slash-command discovery and execution** — PARTIAL: submitted slash commands now stay out of the provider path and report help/unknown/unavailable states; full handler registry wiring remains.
   - Build the palette from the existing command registry, not a second hardcoded list.
   - Support `/` open, filtering, arrow navigation, Tab completion, Enter execution, Esc dismissal, help text, and scrolling.
   - Include `/help`, `/model`, `/models`, `/resume`, `/compact`, `/restart`, `/refresh-model-list`, and existing project commands.
   - Use a clear selected-row highlight, aligned descriptions, and no overlap with the prompt.
   - Verify: keyboard-only interaction, execution, unknown-command errors, narrow-terminal layout.

5. **Add model picker and cwd/session affordances**
   - `/model` shows provider, model id, context window, reasoning support, local/remote status, and current selection.
   - `/resume` shows session title, cwd, modified time, model, and resumable state.
   - Show active cwd in the header and actionable invalid-path errors.
   - Verify: model selection updates the runtime/status immediately; resume/cwd matches session data.

6. **Make customer-critical states explicit**
   - Stable layouts for first run, streaming, thinking, tools, approval, tool failure, model failure, cancellation, compaction, retry, and shutdown.
   - Expected failures become actionable inline messages, not raw panics; preserve prompt/session where safe.
   - Verify: each state has an event-driven rendering test and local-model smoke test.

7. **Harden terminal behavior** — PARTIAL: the loop now detects a terminal resize
   and re-renders; the added delayed-provider black-box tests cover submit,
   streaming, cancellation, and errors through rendered output.
   - Test 80x24, 120x40, wide terminals, resize during streaming, long tool output, multiline paste, Ctrl+C, Esc, Ctrl+D, and terminal restoration.
   - Ensure input and palette focus is always visible.
   - Verify: terminal restoration after normal exit, cancellation, model failure, and panic-safe shutdown.
8. **Verification gate and artifacts**
   - Run `cargo fmt --check`.
   - Run `cargo clippy --all-targets -- -D warnings`.
   - Run focused TUI/harness tests, then `cargo test --workspace`.
   - Run the local `Qwen3.5-0.8B-GGUF` smoke test through `llama-server` at `127.0.0.1:8080`.
   - Update `progress.md` and `feature_list.json`; document unrelated failures separately.

## Definition of done
- No nested-runtime panic on the first prompt.
- Cwd, session id, selected provider/model, reasoning, context, and connection state are visible.
- Slash commands are discoverable, keyboard-operable, aligned, scrollable, and tested.
- Model selection and session resume use existing harness/session seams.
- Tool approval, errors, cancellation, compaction, and recovery are understandable.
- Harness remains UI-agnostic and session persistence remains compatible.
- Verification passes with unrelated pre-existing failures explicitly documented.
