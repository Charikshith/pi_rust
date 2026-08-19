# plan.md — feat-007: P6 — interactive pirust + native extension runner

> SESSION PAUSE 2026-08-19: Waves 1-4 DONE (commits b71f4f7..8d8d050).
> Next: **Wave 5 — plan-mode bundled extension**. Resume here. Full gates
> green at close (397/3, clippy/fmt clean). Live turn vs local llama.cpp
> Qwen3.5-0.8B confirmed working (`pirust -p --model
> anthropic/Qwen3.5-0.8B-Q8_0.gguf` → `ok`).

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
