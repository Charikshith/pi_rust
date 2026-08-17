---
type: template
title: "Progress Log Template"
description: "Session progress log for agent continuity"
artifact: "progress.md"
tags: [state, progress, continuity, session, tracking, verification-plan]
---

# Session Progress Log

## Current State

**Last Updated:** 2026-08-17
**Active Feature:** feat-006 — `pirust-tui` literal port (IN PROGRESS — Waves 1-2 of 8
done: utils.rs, keys.rs, stdin_buffer.rs, all oracle-verified; see plan.md for the
remaining wave breakdown).
Cadence: checkpoint per phase — one wave, verify, report, pause.
**Next feature:** feat-006 Wave 3 (kill_ring/undo_stack/word_navigation/keybindings/fuzzy).
**Project:** 1:1 Rust replica of the Pi Agent Harness (pi_space/pi, ~100K LOC TS).
**Naming:** all Rust code is `pirust*`; original names kept only for on-disk/wire compat.

## Status

### What's Done

- [x] Analyzed all 5 source packages via parallel exploration agents → `docs/analysis/0{1..5}-*.md`
- [x] Wrote master report: `docs/analysis/00-overview.md` (architecture diagram, components, key findings, Rust crate/dep mapping, phased port order P0–P9, risk register)
- [x] Encoded the port roadmap as feat-000..feat-010 in `feature_list.json`
- [x] **feat-000 (P0) DONE** — Cargo workspace scaffold, 7 members, gate green
- [x] **Renamed all crates/binaries pi-* → pirust-\*** (binary `pi` → `pirust`); on-disk/wire names (~/.pi, pi-messages) kept for compat
- [x] **feat-011 DONE** — golden-fixture harness (Pi as oracle): scripts/gen-{golden,message-corpus,model-corpus,event-corpus,rarefields-corpus}.mjs + tests/fixtures/pi/; crates/pirust-ai/tests/golden.rs (5 suites)
- [x] **feat-001 (P0) DONE & ACCEPTED** — pirust-ai type model, verified against authentic Pi at 3 tiers: BYTE-IDENTICAL (1901 real session messages + 1 fixture), SEMANTIC (1062 real Models, all 12 event variants from Pi's faux provider), TYPE-FIDELITY (rare optional fields). Oracle-forced fixes: jsnum.rs (JS number fmt), serde_json float_roundtrip, errorMessage-after-timestamp order, optional partialJson+totalTokens, explicit role tags + untagged Message. 26 tests, fmt+clippy clean. Residual documented: persisted byte-order of provider-gated optionals → feat-002/003.
- [x] **Generated Pi model catalog** (ran Pi's network generator; reverted the 5 tracked .models.ts it rewrote, kept git-ignored data/*.json for corpus regen) — Pi repo left clean.
- [x] **feat-002 (P1) DONE & ACCEPTED (ORCHESTRATED)** — pirust-ai runtime, Anthropic end-to-end. Modules stream/sse/json_repair/auth/api.anthropic_messages/http + §4e type reorder. Verified byte-identical vs Pi's literal bytes across 5 authentic oracle scenarios (tests/anthropic_golden.rs). 65 workspace tests, fmt+clippy clean. All Rust code + fixes written by subagents; orchestrator captured oracle, verified, caught+delegated a self-referential-test weakness. Pinned feat-001 residual (responseId/cacheWrite1h/reasoning order). Deferred: faux stub->feat-003; outbound-request-shape not oracle-verified.
- [x] **Orchestration model proven**: spec agent -> oracle-capture agent -> scaffold agent -> 3 parallel leaf agents -> integrator agent -> hardening agent; orchestrator ran all gates between.
- [x] **feat-003 (P2) DONE & ACCEPTED (ORCHESTRATED, 6 waves)** — pirust-agent-core: types/session-tree/uuid/compaction/loop/Agent/AgentHarness + faux (in pirust-ai). Verified vs Pi: 17 v3 entries + 2 headers byte-identical, UUIDv7 4 vectors byte-exact, 9 compaction cases, loop + full harness tapes vs loop-echo.json, buildSessionContext structure. 111 workspace tests, fmt+clippy clean, ./init.sh green. Waves: oracle+scaffold -> [types, uuid, faux] -> session -> compaction -> agent_loop -> Agent+AgentHarness. One session-limit agent failure resumed cleanly (no rework). Deferred: LLM summary gen, proxy, skills/prompt-templates/system-prompt, real Node env, SessionRepo, v1->v3 migration -> feat-005/007.

- [x] **feat-004 (P3) DONE & ACCEPTED (ORCHESTRATED, 7 waves)** — `pirust-tools`: all 7 built-in
      tools (read/bash/edit/write/grep/find/ls) + shared infra (truncate, path_utils,
      output_accumulator, mutation_queue, binaries, edit_diff) + the `index.ts` registry.
      Oracle: `scripts/gen-tools-oracle.mjs` drives Pi's real tool modules offline →
      `tests/fixtures/pi/tools/` (7 schemas + 7 strings, 71 truncate, 56 edit-diff, 10
      prepare, 52 exec, 50 path, 21 accumulator); `--check` wired into `init.sh`.
      All 7 schemas byte-identical incl. TypeBox key order; `edit_diff` is a literal
      jsdiff-8.0.4 port (no Rust diff crate). 331 tests, 2 ignored, fmt+clippy clean,
      `./init.sh` green with no fixture drift. **Found+fixed an agent-core bug**:
      `build_llm_tools` sent `label()` as the provider-facing tool `description`;
      `AgentTool` gained `description()`. ~45 mutations applied; the 3 survivors were
      closed with new oracle rows + a fake-`LsOperations` unit test.
- [x] **Orchestration model held under failure** — 3 subagents died mid-task (2 API errors,
      1 session limit). Two had already written working code+tests and only lost their
      mutation-test step, which I re-ran as a dedicated audit agent; one was resumed via
      `SendMessage`. Scaffolding shared files (`lib.rs`, `Cargo.toml`, module stubs)
      myself before fan-out is what kept 5 concurrent agents from colliding.
- [x] **feat-005 Wave 4 (sdk.rs) DONE** — `print_mode.rs` was already complete
      (1335 lines, 10/10 golden). Built the remaining sub-waves: **4a**
      `system_prompt.rs` (`buildSystemPrompt`, skills section explicitly omitted —
      always empty pre-feat-007, documented not silent); **4b**
      `provider_attribution.rs` (`mergeProviderAttributionHeaders` +
      `isInstallTelemetryEnabled` folded in); **4c** `auth_guidance.rs` (trivial,
      unit-tested only, no oracle per its own triviality); **4d** an Anthropic-only
      `StreamFn` wrapper in `sdk.rs` resolving auth/headers/timeout/retry per call;
      **4e** `sdk::{create_agent_session, assemble_agent_session}` assembling one
      headless-turn `Agent` — tools (feat-004) + `convert_to_llm` (feat-003) +
      system prompt (4a) + the 4d stream fn, explicitly NOT Pi's 3283-line
      `AgentSession` (interactive-only event-bus machinery, out of scope).
      Also added: `config::get_package_dir/get_readme_path/get_docs_path/get_examples_path`
      (Bun-binary-equivalent adaptation — Pi's package-dir walk has no pirust
      analogue, see module docs), `settings::get_enable_install_telemetry`.
      ORACLE: `scripts/gen-sdk-oracle.mjs` drives real Pi's `buildSystemPrompt` (11
      cases, incl. custom-prompt/append/context-files/Windows-cwd/bash-only-
      guideline) and `mergeProviderAttributionHeaders` (10 cases, incl.
      openrouter/nvidia/cloudflare/opencode + header-source override vs append) into
      `tests/fixtures/pi/sdk/`; both byte/structurally identical. **4e verified** by
      `tests/sdk_canned_turn.rs`: assembles a real `Agent` via `assemble_agent_session`
      with a scripted `Faux` `StreamFn` (not the Anthropic adapter — an integration
      test has no business making a live call) and drives one real turn through the
      actual agent-core loop, proving tools→system-prompt→Agent→loop→convert_to_llm→
      provider produces exactly the `AssistantMessage` shape `print_mode.rs` expects
      (`StopReason::Stop`, scripted text, default read/bash/edit/write tools present
      in the rendered prompt). 3 new golden/integration suites, ~470+ workspace tests
      total, fmt+clippy -D warnings clean, `./init.sh` green with no fixture drift.
      DEFERRED (named, not silent): `blockImages` message filtering (`sdk.ts:250-285`,
      cheap to add later, no multimodal session exercises it yet); session-restore
      model/thinking-level (`is_continuing` always `false` this wave — `session.rs`
      owns restore, not exercised here); `onPayload`/`onResponse` extension hooks
      (no slot on `pirust_ai`'s `StreamOptions` yet, matches its own feat-002
      TODO — feat-007's job alongside the extension host); settings-validation
      errors from `get_http_idle_timeout_ms`/`get_websocket_connect_timeout_ms`
      inside the stream wrapper fall back to the default rather than surfacing as a
      stream error event (main.rs/Wave 5 is the natural place to validate settings
      upfront, before a turn starts).

- [x] **feat-005 Wave 5 (main.rs bootstrap) DONE** — first runnable `pirust` binary.
      Replaced the scaffold stub with the real bootstrap per spec §15's 32-step
      table: parseArgs → diagnostics/`--version`/`--export` (sync, no I/O, no
      tokio — speed constraint #18) → offline-env → TTY probes →
      `resolveAppMode`/`takeOverStdout` → fork/session-id flag validation →
      migrations → trusted `SettingsManager` → session-dir resolution →
      `create_session_manager` (already fully built in Wave 3 incl. the §17.1
      headless `--resume` fail-fast) → missing-session-cwd check (new, small,
      ported from `core/session-cwd.ts`) → `--name` → `ModelRuntime::create` →
      `--help`/`--list-models` early exits → `sdk::create_agent_session` →
      model/thinking-level session entries → piped-stdin/`@file`/initial-message
      assembly (new) → `print_mode::run_print_mode`.
      NEW FILES: `runtime_host.rs` (`SingleTurnSession`/`SingleTurnRuntimeHost`
      implementing `print_mode.rs`'s `PrintModeSession`/`AgentSessionRuntimeHost`
      traits over a real `Agent`+`SessionManager` — this is the piece that did
      NOT exist yet: `print_mode.rs` was built against Pi's `AgentSession`
      abstraction, which `sdk.rs` deliberately never builds; `main.rs`'s job
      this wave included supplying the missing bridge); `initial_message.rs`
      (`buildInitialMessage` + text-only `processFileArguments`, image branch
      deferred — same residual as feat-004's `read` tool, needs an image codec).
      FOUND + FIXED a real Wave-4 gap while wiring: `sdk.rs`'s stream closure
      passed `&BTreeMap::new()` instead of the real process environment to
      `credential_api_key`, silently disabling the `ANTHROPIC_API_KEY`/
      `ANTHROPIC_OAUTH_TOKEN` env-var auth fallback since the day it landed;
      also added the `--api-key` runtime-override field (hazard §16.30) that
      `CreateAgentSessionOptions` was missing entirely. 508 workspace tests
      green (was ~470+), fmt+clippy -D warnings clean, `./init.sh` green, no
      oracle-freshness drift.
      LIVE-VERIFIED in this environment (no configured credentials — `auth.json`
      is `{}`, `ANTHROPIC_API_KEY` unset): `pirust --version` → `0.0.1`, exit 0;
      `pirust --help` → the full byte-identical help text (already golden-
      tested), exit 0; `pirust -p "hi"` → "No models available. Use /login..."
      + exit 1 (correct — no provider is configured, so Pi would show the same
      thing). With `ANTHROPIC_API_KEY` set to a **fake** key: the full pipeline
      resolved a real anthropic model, built the `Agent`, ran a real turn, made
      a genuine HTTPS request to `https://api.anthropic.com`, got back
      Anthropic's real `401 authentication_error` body, synthesized the correct
      error-tail `AssistantMessage`, and persisted a session JSONL with header +
      `model_change` + `thinking_level_change` + the user message + the error
      assistant message — all byte-plausible against the golden shapes Waves
      1-4 already pinned. **Not a successful live call** (no real credentials
      were available in this environment) but definitive proof the request
      pipeline reaches Anthropic's real servers end-to-end.
      NARROWED (named, not silent — see `main.rs`'s own module docs for the
      full list): project trust hard-codes to `--approve`/`--no-approve` else
      untrusted (stricter than Pi's `!hasTrustRequiringProjectResources → true`
      relaxation — the safe direction, `core/trust-manager.ts` not ported);
      `--help` prints right after the model runtime builds rather than after
      the full `AgentSession` (Pi tolerates a model-less session, `sdk.rs`'s
      `Agent` does not — an environment with zero models would otherwise turn
      `pirust --help` into exit 1); `print_mode::NoSignals` used, so
      SIGTERM/SIGHUP fall back to OS-default terminate instead of Pi's graceful
      dispose-then-exit; session persistence is message-level (diffed after
      each `wait_for_idle()`) not event-level streaming.
      OPEN QUESTION FOR WAVE 6: a session file was created (via the unconditional
      `model_change` write, matching `sdk.ts:364-368`'s own ordering) even
      though the only prompt in the manual test errored out — this is
      consistent with `session-manager.ts` writing `model_change` before any
      prompt in Pi too, so it is likely correct, not a divergence; the live
      differential should confirm this against a real `pi` run rather than
      taking this reasoning on faith.
- [x] **feat-005 (P4) DONE & ACCEPTED — Wave 6 (live differential + hardening)**. No real
      Anthropic credentials existed in this environment, so the differential ran against a
      local `llama-server` (llama.cpp, `Qwen3.5-0.8B`) that implements a genuine
      Anthropic-Messages-compatible `/v1/messages` endpoint, via the SAME `models.json`
      `baseUrl`-override mechanism already built in Wave 3 (no new code needed to point at
      it). Real `pi` was run unmodified from its own TypeScript source (`../pi`, no `dist/`
      build exists) via a throwaway Node ESM resolve-hook runner (same alias-mapping
      pattern the existing oracle scripts already use for `@earendil-works/pi-*` workspace
      specifiers) — nothing inside the `../pi` checkout was touched; `git -C ../pi status`
      stayed clean throughout.
  - **Scenarios run against both real `pi` and `pirust`, same cwd, same
    `--provider anthropic --model <local model>`:** (a) text mode provoking a real `bash`
    tool call; (b) `--mode json` provoking real `write` then `read` tool calls.
  - **Session JSONL structure: full parity.** Entry types (`session`, `model_change`,
    `thinking_level_change`, `message` ×N) and assistant-message field order
    (`role,content,api,provider,model,usage,stopReason,timestamp,responseId`) are
    byte-identical in *shape* between real `pi` and `pirust` for both scenarios (values
    differ only in ids/timestamps/model-generated text, as expected).
  - **`--mode json` event-type vocabulary: found ONE real gap, fixed, then verified full
    parity.** Real `pi`'s json stream ends with `{"type":"agent_settled"}`; `pirust`'s
    did not — a genuine missing feature, not a rendering nuance. Root cause:
    `print_mode.rs`'s `AgentSessionEvent::AgentSettled` variant existed (Wave 4/5) and is
    documented as "emitted once per prompt, after the last `agent_end`", and
    `docs/analysis/09-cli-config-spec.md` §13 explicitly names `agent_settled` (and
    `entry_appended`) as required beyond the plain agent-core `AgentEvent` subset — but
    `runtime_host.rs`'s `SingleTurnSession` (the `AgentSession`-substitute bridge, since
    `sdk.rs` deliberately never builds the real 3283-line `AgentSession`) never
    *constructed* one: `to_session_event` is a pure 1:1 `AgentEvent`→`AgentSessionEvent`
    map, and `AgentSettled` has no `AgentEvent` counterpart to map from — it must be
    synthesized. **Fix:** `SingleTurnSession` now keeps the `subscribe()`-registered
    listener in a stored `Mutex<Option<SessionEventListener>>`, and `prompt()` invokes it
    once more with `AgentSettled` after `wait_for_idle()` (this wave's sequential
    `session.prompt()` calls have no queue/retry machinery of their own — see
    `subscribe`'s own note that `will_retry` is always `false` — so "idle" here always
    means "settled"). Re-verified: `print_mode_golden.rs` 10/10 still green (the existing
    fixtures already modeled `agent_settled` correctly; the fix just makes the runtime
    honor it), and a fresh live run's json-event-type set-diff against real `pi`'s is now
    **empty in both directions** for the write/read scenario. `entry_appended` (the other
    §13-named event) legitimately stays deferred — it fires only through a loaded
    extension (feat-007), which does not exist yet; correctly absent from BOTH real `pi`'s
    and `pirust`'s output in these non-extension scenarios (confirmed, not assumed).
  - **Self-referentiality audit:** the `agent_settled` gap above IS the audit's finding —
    the type-level modeling (Wave 4/5) was correct and presumably oracle-informed, but the
    runtime wiring was never exercised against a real end-to-end run, so a real, oracle-
    verifiable gap shipped silently under "10/10 golden". Spot-checked `sdk_canned_turn.rs`
    separately: it intentionally uses a `Faux` stream fn and asserts internal-wiring
    correctness (tools→prompt→loop→convert_to_llm), not Pi byte-compat — correctly scoped,
    not mislabeled as an oracle test. `system_prompt_golden.rs`/`provider_attribution_golden.rs`
    confirmed still oracle-generated (non-trivial fixture counts from Wave 4).
  - **Timing** (release build, `[profile.release]` lto/strip already in `Cargo.toml`; no
    `hyperfine` installed, used PowerShell `Measure-Command` instead — 10 runs each,
    first-run cold-cache outliers visible but ignored): `pirust --version` steady-state
    **~8-10ms**, `--help` **~9-10ms** — squarely in the earlier-reported `jcode`
    near-native floor (10.1-19.3ms), i.e. the LTO/paint-before-block work already done
    this session achieved its goal. Real `pi --version` via the unbundled Node
    resolve-hook runner: **~2.1-2.7s** — NOT a fair comparison to the original
    benchmark's 590ms `pi` figure, since that number presumably came from a proper
    bundled `dist/cli.js`, and this environment has none (`npm run build` was not run, to
    avoid writing into the `../pi` checkout — `dist/` is gitignored there so it would have
    been safe, but was judged out of scope for this wave). Flagged as a residual: a real
    apples-to-apples `pirust` vs `pi` timing comparison needs either a built `pi` dist or
    a documented adjustment for the resolve-hook overhead.
  - **Memory:** `pirust`'s release binary exits in ~8-10ms, too fast to sample a
    meaningful "idle" working set (`Get-Process`/`Measure-Command` sampling raced the
    exit and returned nothing). Real `pi` under Node: **~125.7MB** working set sampled
    mid-run. `pirust.exe` (release) is **127KB** on disk (from the `[profile.release]`
    change earlier this session). Rough first data point only, per the brief — not a
    rigorous benchmark suite.
  - **Constraints honored:** nothing committed/pushed; `../pi` checkout untouched and
    clean throughout; no new tooling installed (checked for `hyperfine`, absent, used the
    PowerShell fallback instead of installing it); `pirust-tui`/extensions/feat-006/007
    territory untouched; `./init.sh` fully green (47 suites, 0 failures, no oracle drift)
    after the fix and a `cargo fmt` pass.
  - **Verdict: feat-005 closes.** The acceptance bar (`AGENTS.md` "Correctness Bar" +
    this feature's own ACCEPTANCE line: pure-layer goldens + a live differential
    comparing session JSONL and stdout shape) is met — pure-layer goldens all green
    (Waves 0-5), the live differential ran for real, found exactly one real gap, and that
    gap is now fixed and re-verified with zero remaining structural divergence in either
    tested scenario. The timing/memory numbers are a real (if rough) first data point,
    not a blocker — they were never part of the ACCEPTANCE line's own bar.

### Decisions locked (user, this session)

- Extensions: **Rust-native**, two loaders (built-in compile-time = P6; dynamic WASM = P9). Embedded-JS engine rejected.
- On-disk state: **full byte-compat** with ~/.pi (auth/settings/session JSONL/UUIDv7). Golden tests required.
- Cadence: **checkpoint per phase** — implement one phase, verify, report, pause.
- **State dir is `~/.pirust`** (not `~/.pi`): `PIRUST_CODING_AGENT_DIR`, `PIRUST_OFFLINE`,
  `~/.pirust/agent/bin/{rg,fd}`. File *formats* stay byte-compatible with Pi; only the
  directory root follows the pirust naming convention. One constant each, in `binaries.rs`.
- `.gitattributes` marks `tests/fixtures/**`, `*.golden`, `*.jsonl` as `-text` —
  `core.autocrlf=true` on this machine would otherwise corrupt every byte-exact golden.

### What's Next (with verification per step)

1. **feat-005 is DONE** (Wave 6 evidence above). A real Anthropic-credential run of the
   same differential (this session used a local llama.cpp server instead — see
   evidence) would still be worth doing whenever real credentials are available, as a
   confirmation rather than a blocker.
2. feat-006 (P5) `pirust-tui` literal port, now IN PROGRESS (Waves 1-2/8 done — see below).
3. Residual named in Wave 6: a real `pirust` vs `pi` timing comparison needs a built
   `pi` `dist/cli.js` (or a documented adjustment for this session's unbundled-Node
   resolve-hook overhead, ~2.1-2.7s, which is not representative of a real install).

### feat-006 Wave 1 (utils.rs) — DONE

Ported `packages/tui/src/utils.ts` (1209 TS lines) → `crates/pirust-tui/src/utils.rs`
(~1360 lines incl. docs/tests): `visibleWidth`, `truncateToWidth`, `sliceByColumn`/
`sliceWithWidth`, `extractSegments`, `wrapTextWithAnsi`, `normalizeTerminalOutput`,
`extractAnsiCode`, `isWhitespaceChar`/`isPunctuationChar`, `applyBackgroundToLine`,
`AnsiCodeTracker` (SGR + OSC-8 hyperlink state). New oracle `scripts/gen-tui-oracle.mjs`
drives real `../pi/packages/tui/src/utils.ts` directly (Node 24 type-stripping, no
alias hook needed — utils.ts has zero internal Pi-package imports) → 99 cases in
`tests/fixtures/pi/tui/utils.cases.jsonl`, all green via `crates/pirust-tui/tests/
utils_golden.rs`; wired into `init.sh`'s `--check` gate. New deps: `unicode-segmentation`,
`unicode-width`, `unicode-properties` (workspace + pirust-tui). fmt+clippy -D warnings
clean; full workspace 515+ tests green.

Documented (not silent) approximation gaps — all named in `utils.rs`'s module docs,
each with a one-line reason:
- **RGI_Emoji matching**: TS tests `/^\p{RGI_Emoji}$/v` against Unicode's official
  curated emoji-sequence table (thousands of entries); no Rust crate in this tree has
  that table, so this port uses a heuristic (known emoji code-point blocks + known
  combinators: ZWJ, VS15/16, skin-tone modifiers, keycap, emoji tags). Covers every
  oracle case (plain/ZWJ-family/skin-tone/flag-pair/VS16 emoji) but isn't byte-exact
  for the full Unicode corpus.
- **`Default_Ignorable_Code_Point`**: approximated as Control ∪ Format ∪ Mark general
  categories — covers all practical zero-width chars, not the full derived property.
- **`cjkBreakRegex`**: TS uses `Script_Extensions` (Han/Hiragana/Katakana/Hangul/
  Bopomofo); this port uses the standard block-range approximation instead.

The perf-only `widthCache` (bounded FIFO `Map`, zero effect on any return value) was
intentionally not ported — same-input-same-output makes it unobservable.

### feat-006 Wave 2 (keys.rs + stdin_buffer.rs) — DONE

Ported `packages/tui/src/keys.ts` (1401 TS lines) → `crates/pirust-tui/src/keys.rs`
(1307 lines) and `packages/tui/src/stdin-buffer.ts` (434 TS lines) →
`crates/pirust-tui/src/stdin_buffer.rs` (450 lines). `keys.rs`: `matches_key`,
`parse_key`, `decode_kitty_printable`/`decode_printable_key`, `is_key_release`/
`is_key_repeat`, `set_kitty_protocol_active`/`is_kitty_protocol_active`, plus every
private Kitty CSI-u / xterm modifyOtherKeys / legacy-sequence helper. `stdin_buffer.rs`:
full escape-sequence completeness detection (CSI/OSC/DCS/APC/SS3/old-mouse/SGR-mouse),
the bracketed-paste state machine, the WezTerm double-escape split, and Kitty-CSI-u
duplicate-codepoint suppression. `scripts/gen-tui-oracle.mjs` extended with `keys`/
`stdin-buffer` sections (still driving real `../pi` TS source, no reimplementation) →
306 + 23 cases in `tests/fixtures/pi/tui/{keys,stdin-buffer}.cases.jsonl`, all green
via new `tests/{keys,stdin_buffer}_golden.rs`; wired into `init.sh`'s existing
`--check` gate. fmt+clippy -D warnings clean, full `./init.sh` green.

Scope decisions (documented in `keys.rs`/`stdin_buffer.rs` module docs, not silent):
- **`KeyId`/`Key` builder not ported** — TS-compile-time-only autocomplete sugar with
  zero runtime behavior; `matches_key`/`parse_key` take/return plain `&str`/`String`.
- **Kitty protocol state as a `static AtomicBool`** — safe under
  `#![forbid(unsafe_code)]`, direct analogue of the TS module-level `let`.
- **`_lastEventType`/`parseEventType`/`KeyEventType` confirmed dead state and not
  ported** — repo-wide grep of `../pi` found zero readers outside the TS's own write
  site; the `:<event>` suffix is still shape-parsed so malformed sequences are still
  rejected, its value is just discarded.
- **`StdinBuffer::process` returns `Vec<StdinEvent>` instead of firing `EventEmitter`
  callbacks.** The TS's `setTimeout`-driven auto-flush is redesigned as
  caller-scheduled: `flush()` itself has TS-identical semantics, but *when* to call it
  after `timeoutMs` of inactivity is deferred to Wave 4 (`tui.rs`), which owns the
  event loop — this crate gains no async-runtime dependency for this file.
- One TS-side redundancy found and documented rather than duplicated:
  `isCompleteCsiSequence`'s manual mouse-SGR fallback is behaviorally identical to the
  regex check preceding it in the TS; implemented once in the Rust port.

No other Rust/TS divergence found — all 329 new oracle cases matched on the first run.

## Blockers / Risks

- [ ] Dynamic JS extension loading (jiti) has no clean Rust 1:1 — strategy decision needed (see 00-overview §4.4 / §5).
- [ ] Generated model catalog JSON was absent in checkout (build-time/git-ignored) — must port generator or obtain output (feat-008).
- [ ] UTF-16 offset semantics (editor + compaction cut points) — fidelity hazard; needs golden tests. **Confirmed NOT an issue for feat-006 Wave 1** (utils.rs operates on visible-column offsets throughout, never UTF-16 code units) — will be the central hazard in Wave 6 (editor.rs).
- [ ] feat-006 Wave 1's three documented approximation gaps above (RGI_Emoji, Default_Ignorable, cjkBreakRegex) — safe/non-blocking, but worth a follow-up diff against the real Unicode emoji-sequences data if a suitable crate appears later.

## Decisions Made

- **Do NOT map TUI onto ratatui** — Pi uses an inline line-diff renderer, behaviorally incompatible with ratatui's alt-screen grid. Port literally; crossterm as thin syscall shim. (00-overview §4.7)
- **Extract UI-free tool logic into pi-tools crate** — decouples tool correctness from the TUI port; lets headless modes land first. (00-overview §5)
- **6-crate workspace + xtask**, port order by dependency direction: ai → agent-core → tui → coding-agent → orchestrator.

## Files Modified This Session

- `docs/analysis/*.md` — 6 analysis docs (created)
- `Cargo.toml`, `crates/*/Cargo.toml`, `crates/*/src/*.rs`, `xtask/*` — workspace scaffold (created)
- `feature_list.json`, `plan.md`, `progress.md`, `init.sh`, `AGENTS.md`, `.gitignore` — harness wired to the port

## Evidence of Completion (feat-000)

- [x] `cargo fmt --check` clean
- [x] `cargo clippy --all-targets -- -D warnings` — no issues
- [x] `cargo test` — 5 passed
- [x] `cargo build` — all 7 members compile; `pi --version` → `pi 0.0.1`

## Notes for Next Session

The whole port is P0–P9 (feature_list.json). P0 landed. Before feat-001, get the user
to settle the extension strategy — it materially changes the coding-agent architecture.
Read `docs/analysis/00-overview.md` first each session; it routes to per-package detail.
