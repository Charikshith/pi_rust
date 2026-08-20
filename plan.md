# plan.md — feat-007: P6 — interactive pirust + native extension runner

> SESSION 2026-08-19 (resumed): Wave 5 DONE — plan-mode bundled extension
> (plan_mode.rs + plan_mode_extension.rs + tests; 58 tests, differential vs
> real Pi = 0 mismatches; workspace 486/3). Wave 6 DONE — extension runner
> bound into `SingleTurnSession` (real tool-toggle + appendEntry + hooks;
> workspace 490/3). Next: **Wave 7 — closeout** (evidence in
> feature_list.json; `./init.sh` green; delete plan.md). Resume here.

Source: `packages/coding-agent/src/modes/interactive/` (interactive-mode.ts 6,008
lines + components/ 9,214 lines + theme/ 1,420 lines + assets/) and the
extension host surface. Spec: `docs/analysis/03-coding-agent.md` §modes,
`docs/analysis/00-overview.md` §5 (Extension system DECIDED: Rust-native,
two loading models).

Correctness bar (per AGENTS.md): oracle-driven golden tests against real Pi
TS source. Interactive mode drives a real TUI — tmux snapshot tests mirror
Pi's own interactive test method (00-overview §8). Extensions: `Extension`
trait mirroring Pi's host surface (~30 lifecycle events + tool/command/
shortcut/flag/provider registration).

## Scope boundary (feat-007)

- **In scope:** (1) wire `pirust-tui` into `pirust-coding-agent` interactive
  mode — `pi` launch, interactive loop, prompt rendering, streaming LLM turn
  display, tool-call rendering, exit; (2) `pirust-extension-api` crate:
  `Extension` trait + built-in (compile-time) loader + `plan-mode` bundled.
- **Out of scope:** dynamic WASM extension loading (P9); RPC mode (feat-012);
  the full 40-component interactive suite (port what interactive-mode.ts
  actually exercises first); tmux snapshot infra (fixture-golden instead).

## Waves

1. **Interactive scaffold + `pi` launch** — `InteractiveMode` struct wrapping
   `pirust-tui`'s TUI/Editor/Input; raw-mode terminal init via crossterm;
   prompt line via Editor; send on Enter. Verify: manual smoke + unit tests
   for prompt/echo paths.
2. **Streaming turn display** — wire `EventStream`/agent loop output into TUI
   render (message/assistant streaming text). Verify: mock-Api integration
   test driving a turn end-to-end through the interactive loop.
3. **Tool-call rendering + autocomplete** — render tool calls/IO in the TUI
   like Pi; wire Editor autocomplete to tools/skills/commands. Verify:
   oracle cases where components have TS counterparts.
4. **`pirust-extension-api` crate** — `Extension` trait mirroring Pi host
   surface (~30 events); registration types (tool/command/shortcut/flag/
   provider); `ExtensionRunner` trait + built-in loader. Verify: unit tests
   for lifecycle event dispatch + a demo bundled extension.
5. **`plan-mode` bundled extension** — the first real built-in; exercise the
   Extension trait end-to-end. Verify: integration test running a plan-mode
   tool call through the extension runner.
6. **Closeout** — evidence in feature_list.json; `./init.sh` green; delete
   plan.md.

## Status

- [x] Wave 1 — interactive scaffold
      (`crates/pirust-coding-agent/src/interactive_mode.rs` + main.rs wiring +
      tests/interactive_mode_smoke.rs). `InteractiveMode` wraps `pirust-tui`'s
      TUI+Editor; terminal reader thread feeds raw input through a channel
      (the TUI is !Send; the callback must be Send) — the same
      caller-owns-the-loop adaptation the TUI crate documents. Editor
      on_submit routes to a prompt callback; Ctrl+D on an empty editor quits
      (Pi's handleCtrlD). Fixed a real TUI-crate bug exposed by this wiring:
      editor.render's `self.tui.borrow().terminal_rows()` panicked
      re-entrantly when the editor was mounted in the very TUI that renders
      it — replaced with a `try_borrow` + cached-rows helper
      (`Editor::terminal_rows`). main.rs launches interactive mode when
      stdin+stdout are TTYs (resolve_app_mode already decided Interactive;
      previously always fell through to print). Verify: 2 smoke tests
      (submit→prompt path, ctrl+d quit), 179/179 coding-agent tests,
      clippy/fmt clean, workspace 384/3 (3 pre-existing find tests). Wave 2
      replaces the echo prompt callback with real `session.prompt` rendering.
- [x] Wave 2 — streaming turn display
      (`interactive_mode.rs` + main.rs + tests). `InteractiveMode` now takes
      the `Arc<dyn PrintModeSession>` + `tokio::runtime::Handle`; on submit it
      blocks the loop on `session.prompt` (exactly Pi's `await
      this.session.prompt`), while the session's event subscription bridges
      agent-thread events through an mpsc channel into the main loop, which
      renders them into a chat `Container`: user line (▶ prefix), streaming
      assistant text into a live `Text` component (updates on
      `message_update`, finalized on `message_end`), spacer on `agent_end`.
      assistant_text() extracts `content[].text` blocks (trimmed), matching
      the assistant-message component's text handling; thinking/tool blocks
      ignored this wave. main.rs wires the real SingleTurnSession. Verify: 3
      smoke tests (submit→prompt, ctrl+d quit, events stream into chat),
      clippy/fmt clean, workspace 385/3.
- [x] Wave 3 — tool-call rendering + autocomplete
      (`interactive_theme.rs` + interactive_mode.rs + tests). New
      `interactive_theme.rs` port of theme.ts's fg/bg (ANSI truecolor) with
      the dark.json tool colors (pending/success/error bg, text/gray fg).
      `ToolExecutionComponent` port (simplified, faithful): tool name + args
      JSON + streaming result preview (10 lines, `... (N more lines)`
      truncation via FALLBACK_PREVIEW_LINES), bg color switches
      pending→success/error. `render_event` handles
      `tool_execution_start/update/end` (pendingTools map keyed by
      tool_call_id). Autocomplete: editor gets a
      `CombinedAutocompleteProvider` with the BUILTIN_SLASH_COMMANDS list
      (22 commands, slash-commands.ts). Verify: 5 smoke tests (+tool events
      render, +slash autocomplete suggests /model), 184/184 coding-agent,
      clippy/fmt clean, workspace 389/3.
- [x] Wave 4 — `pirust-extension-api` crate
      (new `crates/pirust-extension-api/`, 5 modules + integration test).
      `events.rs`: the full `ExtensionEvent` union (34 variants, tagged
      `{type: ...}`, camelCase serde renames) + `event_type()` discriminator
      + the reason/source enums. `context.rs`: `ExtensionContext` (mode/
      has_ui/cwd + accessor closures), `ExtensionCommandContext`, and all
      result types (`ContextEventResult`, `ToolCallEventResult`,
      `ToolResultEventResult`, `InputEventResult` (tagged `{action:...}`),
      `MessageEndEventResult`, `BeforeAgentStartEventResult`,
      `ResourcesDiscoverResult`, session-before results). `registration.rs`:
      `ToolDefinition` (execute via `ToolCallParams`), `RegisteredCommand`,
      `ExtensionShortcut`, `ExtensionFlag`, `ExtensionApi` (on/register_
      tool/command/shortcut/flag + get_flag), `Extension` object, `SourceInfo`.
      `runner.rs`: `ExtensionRunner` with Pi's exact dispatch semantics —
      generic emit (error capture, session-before cancel short-circuit),
      emit_tool_call (first result wins, block returns), emit_user_bash
      (first result wins), emit_context (clone+chain), emit_before_provider_
      request/headers, emit_message_end (same-role chained), emit_before_
      agent_start (chained systemPrompt), emit_resources_discover,
      emit_input (transform chains, handled short-circuits). Handlers are
      sync `Result<Value, String>` this wave (Pi's are async; Wave 6 binds
      the async agent loop). `loader.rs`: `InlineExtension` + `ExtensionFactory`
      + `built_in_extensions()` (empty list — plan-mode lands Wave 5).
      Workspace: registered in Cargo.toml members + workspace.dependencies.
      Verify: 6 unit tests (discriminators, tagged serialization, dispatch,
      error capture, cancel short-circuit, input transform) + 2 integration
      tests (demo extension registers + dispatches; tool executes),
      clippy/fmt clean, workspace 397/3.
- [x] Wave 5 — plan-mode bundled extension
      (`crates/pirust-extension-api/src/plan_mode.rs` +
      `plan_mode_extension.rs` + tests/plan_mode_extension.rs).
      `plan_mode.rs`: pure 1:1 port of `plan-mode/utils.ts` (isSafeCommand/
      cleanStepText/extractTodoItems/extractDoneSteps/markCompletedSteps +
      TodoItem). `plan_mode_extension.rs`: faithful port of `index.ts` —
      `plan` flag, `/plan` + `/todos` commands, `ctrl+alt+p` shortcut,
      `tool_call` (block destructive bash), `context` (filter stale
      plan-mode context), `before_agent_start` (inject plan/execution
      context), `turn_end` ([DONE:n] tracking), `agent_end` (plan
      extraction + execution-complete), `session_start` (flag + state
      restore). Shared `Arc<Mutex<PlanModeStateMachine>>` across handlers
      (Pi's closure-over-`let`). `built_in_extensions()` now returns
      plan-mode. Fixed a real Wave-4 bug: `ToolCallEventResult` fields are
      optional in Pi's TS, so `{block: true, reason}` without `terminate`
      failed to deserialize → `unwrap_or_default` silently dropped the
      `block`; added serde defaults.
      Verify: 41 unit (plan_mode 29 + extension 2 + runner 6 + events 2 +
      context 2) + 2 demo integration + 15 plan-mode integration = 58;
      DIFFERENTIAL vs real Pi Node output (46 safe + 10 clean + 6 extract +
      5 done + mark) = 0 mismatches; clippy/fmt clean; workspace 486/3
      (3 = pre-existing env-polluted find tests).
- [x] Wave 6 — bind the extension runner into `SingleTurnSession`
      (`crates/pirust-coding-agent/src/runtime_host.rs` +
      `crates/pirust-extension-api/src/runtime.rs` + `loader.rs` + `runner.rs`
      + `agent.rs` hooks). `ExtensionRuntime` = mutable action slots (Pi's
      shared `runtime` object, `bindCore` copies in place); `load_with_runtime`
      loads extensions against the runner's own runtime Arc so bind-after-load
      swaps are visible (the Wave-5 fresh-noop-arc bug that made the toggle
      tests fail). `SingleTurnSession` owns `Arc<Mutex<ExtensionRunner>>`;
      `bind_extensions` builds the runner from built-ins, binds real actions
      (`getActiveTools` → `agent.tool_names()`, `setActiveTools` → registry
      filter + `agent.set_tools`, `appendEntry` → `session_manager.append_custom_entry`),
      installs the agent-loop hooks (`transform_context`→`emit_context`,
      `before_tool_call`→`emit_tool_call` (block), `after_tool_call`→
      `emit_tool_result`), forwards agent events (`to_extension_event`), and
      emits `session_start{reason:startup}`. `Agent` gains post-hoc hook
      setters (`set_transform_context`/`set_before_tool_call`/`set_after_tool_call`,
      faithful to Pi's mutable `agent.beforeToolCall = …`) + `tool_names()`.
      `ExtensionBindMode::Tui` added (interactive mode binds `tui`,
      `has_ui:true` — plan-mode plan extraction needs it). Interactive mode
      now binds extensions at launch.
      Verify: 4 new e2e tests (`tests/wave6_binding.rs`) — runner built,
      `/plan` toggles REAL agent tools (edit/write dropped, restored on toggle),
      destructive bash blocked via the real hook, `appendEntry` persists the
      plan-mode entry with `enabled:true` + `toolsBeforePlanMode`. Workspace
      490/3, clippy 0, fmt clean, no oracle drift.
- [ ] Wave 7 — closeout: evidence in feature_list.json; `./init.sh` green;
      delete plan.md.
