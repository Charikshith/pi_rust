# TUI Design Audit — Development Backlog

## Status

The interactive TUI is **not customer-ready**. The first prompt previously panicked because the TUI called `Handle::block_on` from inside an existing Tokio runtime. The temporary `block_in_place` workaround prevents that specific panic, but the TUI still blocks while a model turn runs.

## Core architectural correction

Replace the synchronous turn path:

```text
TUI loop → block_on(session.prompt()) → frozen TUI
```

with an async event-driven path:

```text
async TUI loop
  ├── process terminal input
  ├── drain AgentHarness/session events
  ├── render the current frame
  ├── spawn/await the active model turn
  └── process cancellation and shutdown
```

`AgentHarness` must remain UI-agnostic. It owns agent execution, turn lifecycle, tool events, cancellation, and session state. `InteractiveMode` owns terminal input, focus, palette state, layout, and rendering.

## Confirmed design flaws

### Critical

1. **Blocking model turns**
   - `interactive_mode.rs::run_turn()` blocks while awaiting the model.
   - The UI cannot process input or render live events during a long response/tool call.
   - `block_in_place` avoids the nested-runtime panic but does not solve the freeze.

2. **Fragile runtime ownership**
   - The TUI receives a Tokio runtime handle and controls blocking behavior.
   - The workaround assumes a multithreaded runtime; current-thread embedding can panic again.
   - The TUI should not block or own runtime execution.

3. **Slash commands are not fully dispatched**
   - Autocomplete suggestions exist, but submitted slash commands are not yet a complete command-dispatch path.
   - `/model`, `/resume`, `/compact`, `/restart`, and similar commands need explicit handlers.

4. **Cancellation is not wired end-to-end**
   - Keyboard cancellation must flow through a cancellation token into the agent loop, provider stream, and active tools.
   - Ctrl+C/Esc must work while a turn is active.

5. **Prompt errors are discarded**
   - The prompt result is ignored.
   - Provider, authentication, tool, cancellation, and model errors must become visible actionable TUI states.

### Major UX gaps

6. **Missing persistent runtime identity**
   Always show:
   - cwd
   - session id
   - provider
   - selected model
   - reasoning level
   - context usage
   - token/cost estimate
   - tool availability
   - connection/turn state

7. **Model picker is not wired**
   - `/model` needs a searchable picker.
   - Show provider, model id, context window, reasoning support, local/remote state, and current selection.
   - Selection must update the active session/runtime and status display immediately.

8. **Tool approval is incomplete**
   Destructive tools need:
   - exact command
   - cwd
   - risk warning
   - Run once
   - Always allow
   - Deny
   - timeout
   - exit status
   - output truncation
   - diff preview for file edits

9. **Tool state cleanup is unclear**
   - Completed entries in `pending_tools` must be finalized and removed or retained intentionally as history.
   - Long sessions must not accumulate stale active-tool state.

10. **Resize handling is ignored**
    - The terminal resize callback is currently empty.
    - Layout, wrapping, footer, palette, and status panels must recompute on resize.
    - Test at 80x24, 120x40, and wide terminals.

11. **Rendering is incomplete**
    - Thinking, markdown, images, tool calls, tool results, and rich errors need dedicated rendering states.
    - Text extraction must not silently discard thinking, images, redacted content, or other assistant blocks.
    - Avoid accidental content changes from unconditional trimming.

### Verification gaps

12. **Smoke tests check invocation, not usability**
    Existing tests prove that `prompt()` is called, but do not prove live responsiveness, streamed rendering, cancellation, resize, or error display.

13. **No terminal snapshot/golden coverage**
    Add real rendered-output assertions, ideally tmux or an equivalent terminal transcript/snapshot harness.

14. **No delayed end-to-end hello test**
    Add a test that launches the interactive path with a delayed fake provider, types `hello`, submits, observes streamed output, receives the final response, and exits cleanly.

## Required implementation order

1. Build the non-blocking async turn runner.
2. Add active-turn state, duplicate-submit prevention, completion, failure, and cancellation handling.
3. Connect AgentHarness/session lifecycle events to TUI state.
4. Add persistent cwd/session/model/status display.
5. Add slash-command dispatch and palette behavior.
6. Add `/model` and `/resume` flows.
7. Add tool approval, diff preview, error, retry, and compaction states.
8. Add resize and small-terminal behavior.
9. Add delayed-provider end-to-end tests and terminal snapshots.
10. Run fmt, clippy, focused tests, workspace tests, build, and local llama-server smoke testing.

## Customer-ready acceptance criteria

- Typing and submitting `hello` never panics or freezes the interface.
- Streaming output appears while the model is working.
- The user can cancel an active turn.
- Errors are visible and actionable.
- cwd, session, provider, model, reasoning, context, and connection state are visible.
- Slash commands are discoverable, keyboard-operable, and actually dispatched.
- Model selection and session resume work.
- Destructive tools require clear approval.
- The TUI remains usable at 80x24 and after resize.
- Terminal state is restored after normal exit, cancellation, errors, and shutdown.
- Tests validate behavior and rendered output, not merely function invocation.

## Additional architectural risks

These are separate from the confirmed nested-runtime/frozen-loop defect and should be verified before calling the implementation production-ready.

### 15. No single source of truth for UI state

The TUI keeps local state (`streaming_text`, `pending_tools`, quit flag) while the session/harness keeps its own turn, model, tool, and persistence state. Without a defined projection/reconciliation rule, the screen can disagree with the session after retries, cancellation, compaction, or errors.

**Required direction:** derive display state from an explicit event/state model; local UI state should only contain transient presentation data.

### 16. Event ordering and late-event hazards

Events arrive through a channel from another execution context. A cancelled or completed turn can still deliver late events. Without a turn id/sequence number, a late assistant update can modify the next turn's message or tool component.

**Required direction:** attach turn/operation ids, reject stale events, and test reordered/late events.

### 17. Event-channel backpressure is undefined

An unbounded channel can grow without limit when a provider emits quickly, a tool produces large output, or rendering falls behind. A bounded channel needs an explicit policy: coalesce stream updates, truncate tool output, or preserve terminal events over deltas.

### 18. Subscription lifetime is leaked

The subscription is intentionally forgotten with `mem::forget`. That avoids an early unsubscribe but makes ownership unclear and prevents deterministic cleanup. A session should own the subscription guard or `InteractiveMode` should retain and drop it explicitly.

### 19. Turn lifecycle is not modeled as a state machine

Boolean/optional fields are insufficient for states such as queued, running, awaiting approval, retrying, cancelling, cancelled, failed, completed, and compacting. Invalid transitions can otherwise accept input or render contradictory status.

**Required direction:** define a small explicit turn state machine and test legal/illegal transitions.

### 20. Startup and shutdown ownership is split

`main`, `InteractiveMode`, `TUI`, `ProcessTerminal`, `OutputGuard`, and the async runtime each own part of startup/shutdown. A panic, provider failure, Ctrl+C, or terminal error can leave cursor/raw-mode/stdout state inconsistent.

**Required direction:** one session guard owns terminal restoration, stdout restoration, task cancellation, subscription cleanup, and runtime shutdown in a documented order.

### 21. Fire-and-forget tasks need shutdown semantics

Any spawned prompt/tool task must have a cancellation token, a join/abort policy, and an error-reporting path. Dropping a task handle must not silently abandon a network request, child process, file write, or session flush.

### 22. Command registry and capabilities can drift

The palette, actual command dispatcher, model actions, and extension commands can each expose different command sets. A command visible to the user must have a capability/handler or be clearly marked unavailable.

**Required direction:** generate palette entries from registered handlers and expose availability/reason text.

### 23. Cwd/security boundary is underspecified

The displayed cwd is not enough. Tools need a clear policy for relative paths, symlinks, parent traversal, project roots, shell working directory, and commands that change directory internally. The UI must show the actual execution cwd for each tool.

### 24. Approval policy is not centralized

Tool approval should be a harness policy, not only a TUI prompt. Headless mode, future RPC mode, extensions, and retries must use the same trusted decision boundary; otherwise a tool can bypass the visual approval screen.

### 25. Persistence timing is unclear

The UI can show a response before the session entry is durably written. A crash at that point can make the user-visible conversation differ from the resumed session. Save points need explicit events and status, especially around tool calls and compaction.

### 26. Retry semantics can duplicate visible work

A provider retry may emit partial events before retrying. If the UI does not receive a retry boundary and message identity, it may display duplicated assistant text, duplicate tool calls, or incorrect token/cost totals.

### 27. Model status can become stale

The catalog, configured model, session model, and actual stream model are distinct sources. The status corner must identify which one is active and update after model changes, fallback, retry, or provider routing.

### 28. Observability is missing from the interaction contract

A customer-facing error needs a correlation/request id, elapsed time, provider/model, retry count, and a path to debug details. Raw stack traces should be hidden by default but available in a log or diagnostic panel.

### 29. The test seams are too implementation-oriented

Tests directly manipulate channels and call `poll()` rather than exercising the public interaction contract. This can leave failures in terminal focus, scheduling, resize, and actual byte rendering undetected.

**Required direction:** add black-box transcript tests alongside unit tests.

### 30. Feature completion is being measured by component presence

Having an editor, autocomplete provider, tool component, and event enum does not prove the user workflow is complete. Acceptance must be defined by scenarios: hello, slow response, tool approval, tool failure, cancel, resume, model switch, resize, and restart.

## Rust-specific architecture goals

The port should preserve Pi's behavior and file/wire compatibility, but it should not preserve implementation mistakes that came from JavaScript's runtime model. Rust is an opportunity to improve execution architecture without changing user-visible semantics.

### Benefits we should actively capture

1. **Responsive async execution**
   - Use Tokio tasks and cancellation rather than blocking the TUI thread.
   - Keep model streaming, tool execution, terminal input, and rendering independently schedulable.

2. **Bounded memory**
   - Bound event queues.
   - Coalesce high-frequency streaming deltas.
   - Truncate tool output at the producer boundary.
   - Avoid retaining completed tool components or duplicate message snapshots unnecessarily.

3. **Predictable ownership and cleanup**
   - Use RAII guards for raw terminal mode, stdout takeover, subscriptions, child processes, and temporary resources.
   - Avoid `mem::forget` for lifecycle objects.
   - Ensure spawned tasks have cancellation and join/abort semantics.

4. **Low overhead state representation**
   - Use enums for turn/event state rather than loosely related booleans.
   - Use stable ids and compact references for active turns/tools.
   - Keep persisted Pi-compatible JSON at the boundary, not as the internal state representation.

5. **Concurrency safety**
   - Make event ordering and cancellation explicit with typed messages, turn ids, sequence numbers, and bounded channels.
   - Let the compiler enforce `Send`/`Sync` boundaries instead of recreating shared mutable JavaScript objects.

6. **Performance measurement**
   - Benchmark startup time, idle memory, streaming throughput, tool latency, context compaction, and long-session memory growth.
   - Compare Rust against a fair Pi baseline, not an unbundled development Node runner.
   - Set regression budgets before claiming a speed or memory advantage.

### Rust benefits we must not assume automatically

- Rust does not guarantee low memory if the implementation retains full event/message histories, uses unbounded queues, or leaks subscriptions.
- Rust does not guarantee responsiveness if the UI thread blocks on futures.
- Rust does not guarantee speed if every stream update causes a full terminal redraw or if polling sleeps are used instead of event notification.
- Rust does not eliminate security risks; cwd boundaries, shell execution, approvals, and path validation remain explicit responsibilities.
- Rust's `unsafe` avoidance does not replace lifecycle tests, cancellation tests, or terminal restoration tests.

### Porting rule

Preserve Pi's **observable contract**—CLI behavior, session formats, model semantics, event meaning, and user workflows. Do not preserve JavaScript-specific mechanics when Rust has a safer or more efficient equivalent. In particular:

```text
Pi behavior compatibility ≠ Pi implementation compatibility
```

The TUI should be a Rust-native async event loop with Pi-compatible behavior, not a synchronous JavaScript event-loop imitation.

## Fresh-start decision

A fresh rewrite is not recommended. Continue from the existing code, but treat `InteractiveMode` as a subsystem boundary that needs a deliberate refactor rather than a series of local patches.

Recommended sequence:

1. Freeze the current behavior with black-box tests.
2. Replace the blocking turn loop with the async state machine.
3. Keep `AgentHarness` UI-agnostic and expose typed lifecycle/cancellation events.
4. Add one state projection for TUI status, commands, model, cwd, and session data.
5. Add bounded queues, RAII cleanup, and task cancellation.
6. Add terminal transcript/snapshot tests and delayed-provider tests.
7. Benchmark speed and memory before and after the refactor.

## Related files

- `plan.md`
- `feature_list.json` → `feat-013`
- `docs/tui-design-samples.html`
- `crates/pirust-coding-agent/src/interactive_mode.rs`
- `crates/pirust-agent-core/src/harness/`
- `crates/pirust-coding-agent/tests/interactive_mode_smoke.rs`
