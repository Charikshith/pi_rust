---
type: state
title: "feat-005 plan — headless pirust binary"
description: "Step-by-step plan with per-step verify checks for the active feature (feat-005, P4: first runnable pirust binary)."
artifact: "plan.md"
tags: [state, plan, active-feature, feat-005]
---

# feat-005 — headless `pirust` binary (P4)

**Success criterion (one line):** `pirust -p "…"` runs one real turn against the
Meridian endpoint, writes a session JSONL byte-compatible with Pi's, and its
argv/config/migration layers match Pi byte-for-byte under golden fixtures.

**In scope:** `cli/args.ts` (parser + verbatim `--help`), `config.ts` paths + app
identity, `migrations.ts` (all 5), `settings-manager.ts` (~45 fields + global/project
deep merge), `auth-storage.ts`, `models-store.ts` + model resolution (**anthropic api
only**), `core/session-manager.ts` (coding-agent's own 1623-line implementation),
`core/sdk.ts` wiring to `Agent`, system prompt, `modes/print-mode.ts` (text + json),
and `main.ts` bootstrap/mode dispatch.

**Correction to the earlier scope note:** `SessionRepo` is NOT part of coding-agent —
it has zero references there. The `repo-utils`/`jsonl-repo`/`memory-repo` impls belong
to `pirust-agent-core` and are off the headless critical path, so they stay a separate
side-task rather than a feat-005 wave.

**Out of scope (explicit):** RPC mode → **feat-012**. Interactive TUI → feat-006/007.
Extensions + resource-loader (skills/prompt-templates/themes/keybindings/slash-commands)
→ **stubbed**, feat-007. Also not ported: `package-manager-cli.ts` (`install/remove/
update/list/config` verbs), HTML export, telemetry, self-update / Windows quarantine,
HTTP proxy + undici dispatcher tuning, session picker & first-time-setup TUI.

Naming: `~/.pirust/agent`, `PIRUST_CODING_AGENT_DIR`, `PIRUST_CODING_AGENT_SESSION_DIR`,
`PIRUST_OFFLINE`. File **formats** stay byte-compatible with Pi; only the root differs.

Spec: `docs/analysis/09-cli-config-spec.md` (Wave 0)
Design + traps: `C:\Users\CharikshithPolimera\.claude\plans\swift-seeking-biscuit.md`

## Byte-exactness traps already found (recon)

1. **No `--flag=value` for KNOWN flags.** `--model=sonnet` does NOT set `model`; it
   lands in `unknownFlags` as `{"model" => "sonnet"}`. Biggest trap in the parser.
2. **Value-taking flag as last token**: tolerated ONLY for `--long` forms (falls through
   to the unknown-flag branch as `{"model" => true}`). The single-dash aliases `-t`,
   `-xt`, `-e` instead hit the `startsWith("-") && !startsWith("--")` branch and produce
   a **fatal** `Unknown option: -t`. `--name`/`-n` has its own error.
3. **Values are consumed blindly**: `--model --print` sets `model = "--print"`.
4. **`-p` has optional-value lookahead with a `---` clause**: `-p ---foo` takes
   `---foo` as the prompt; `-p --foo` does not.
5. **No `--` end-of-options handling.** `pi -- "hello"` yields
   `unknownFlags = {"" => "hello"}` and EMPTY `messages`.
6. **Unknown `--flags` greedily eat the next token** unless it is missing or starts
   with `-` or `@`.
7. `--models` does NOT filter empties (`"a,,b"` → `["a","","b"]`) but `--tools` /
   `--exclude-tools` DO.
8. `--version` is `-v` (not `-V`), and wins over `--help`. `--help` exits *late*,
   after the runtime is built, so extension flags can be listed.
9. `resolveAppMode`: `--mode text` is never checked, so it falls through to the TTY
   logic. **Correction:** the "piped stdin downgrades interactive→print" branch
   (`main.ts:770-772`) is **unreachable dead code** — `readPipedStdin` returns
   `undefined` when `stdin.isTTY`, and `interactive` already implies `stdin.isTTY`.
   The real mechanism is `resolveAppMode`'s `!stdinIsTTY → print` at `main.ts:540`.
10. `takeOverStdout()` replaces `process.stdout.write` with a shim forwarding to
    **stderr** in non-interactive modes; the payload goes out via a raw writer.
    The exemption is `isPlainRuntimeMetadataCommand` = `!print && mode === undefined
    && (help || listModels)`, so `pirust -p --help` and `--mode json --list-models`
    are NOT exempt.
11. **Blocking-stdin hazards to keep headless-safe:** the `--resume` session picker
    builds a TUI regardless of TTY, and the `--session` "global" branch calls
    `promptConfirm` on stdin. Both must not hang a headless run.
12. `deepMergeSettings` is **one level deep and arrays REPLACE** — its own doc comment
    says "recursively" and is wrong. Runtime wins.
13. `PIRUST_OFFLINE` has **two different truthiness rules** in Pi: `isTruthyEnvFlag`
    (`1`/`true`/`yes`) at `main.ts:95-98`, versus a bare `!== undefined` at
    `model-runtime.ts:152` — so `=0` still disables the network on the second path.
    Both must be ported.
14. `migrateSessionsFromAgentRoot` (M2) is effectively a **no-op on Windows** because of
    `file.split("/").pop() || file.split("\\").pop()`. Port literally; do not "fix".
15. **No session file exists until the first assistant message** (open flag `"wx"`).
    The Wave 6 live differential must account for this.
16. The existing scaffold binary accepts **`-V`** for version
    (`crates/pirust-coding-agent/src/main.rs:13`); Pi only has `-v`. Remove it.
17. `crates/pirust-tools/src/binaries.rs` already defines `CONFIG_DIR_NAME` /
    `ENV_AGENT_DIR` / `OFFLINE_ENV` / `AGENT_DIR_NAME` / `BIN_DIR_NAME`. `config.rs`
    must **re-export** these, not redeclare them.

## Speed constraints (bootstrap ordering — locked in, do not retrofit)

`pi`'s TUI time-to-first-frame is ~590ms vs a native-CLI floor of ~14ms (bench:
`jcode` 10.1-19.3ms, `pi` 369.6-934.8ms). That gap is almost entirely bootstrap
I/O ordering, not renderer choice. Two constraints on Wave 5/6 `main.rs`:

18. **Paint before you block.** The first frame (interactive prompt) or first
    output (print/json mode) must not wait on config load, settings merge,
    migrations, auth read, or model-catalog fetch. Render/emit first, hydrate
    those after. Do not construct a `tokio::Runtime` (or use `#[tokio::main]`)
    for `--version` / `--help` / parse-error paths — those stay fully
    synchronous, zero async-runtime init, zero disk/network I/O.
19. **Non-interactive modes never touch the TUI.** `resolveAppMode`'s dispatch
    must short-circuit to print/json/rpc *before* any terminal raw-mode setup
    or renderer init — those modes pay no TUI cost, ever.

## Steps

1. **Wave 0 — spec + oracle.** Write `docs/analysis/09-cli-config-spec.md`. Write
   `scripts/gen-cli-oracle.mjs` emitting `tests/fixtures/pi/cli/`: an argv→`Args`
   corpus (every flag, every trap above), `help.golden`, a `resolveAppMode` matrix,
   config-path table, settings deep-merge cases, the 5 migration scenarios, and the
   cwd→session-dir encoding.
   → verify: fixtures non-trivial, `--check` idempotent, traps 1–7 each have a row.
2. **Wave 1 — `args.rs` + `config.rs`.** `parseArgs` is 100% pure (no env/fs/cwd/TTY),
   so it is a clean golden target; `printHelp` must match `help.golden` verbatim.
   → verify: argv corpus byte-green; help byte-identical.
3. **Wave 2 — `settings.rs`, `auth.rs`, `migrations.rs`.**
   → verify: merge cases + all 5 migrations green; `auth.json` written `0600`.
4. **Wave 3 — `session.rs`, `models.rs`.**
   → verify: session-dir encoding golden incl. a Windows drive-letter path
   (`C:\Users\me\proj` → `--C--Users-me-proj--`); a model resolves from
   `~/.pirust/agent/models.json` through to the anthropic adapter; the "no session file
   until the first assistant message" rule holds.
5. **Wave 4 — `sdk.rs` + `print_mode.rs`.** `print_mode.rs` landed already
   (1335 lines, `tests/print_mode_golden.rs` 10/10 green). `sdk.rs` itself is
   still a doc-only stub (5 lines) — real remaining scope, now that
   `convert_to_llm` (feat-003) and model resolution (`find_initial_model`,
   `resolve_cli_model` in `models.rs`, feat-005 Wave 3) are already ported:
   - 4a. `system_prompt.rs` — port `core/system-prompt.ts` (162 lines TS).
   - 4b. `provider_attribution.rs` — port `core/provider-attribution.ts` (97 lines).
   - 4c. `auth_guidance.rs` — port `core/auth-guidance.ts` (25 lines, one formatter).
   - 4d. A thin **Anthropic-only** stream wrapper (`core/model-runtime.ts`'s
     `streamSimple`, NOT the full 587-line multi-provider `ModelRuntime` class —
     everything non-Anthropic is out of scope per feat-005's own scope note)
     wiring settings (retry/timeout/websocket) into the existing `pirust-ai`
     Anthropic adapter (feat-002).
   - 4e. `sdk.rs` — assembles a `pirust_agent_core::Agent` for **one headless
     turn**: resolved model + tools (feat-004) + system prompt + the 4d stream
     fn. Explicitly **NOT** Pi's `AgentSession` (`agent-session.ts`, 3283 lines —
     that's interactive event-bus/footer/subscribe machinery for the TUI,
     already out of scope per this feature's "Interactive TUI → feat-006/007"
     line). Extension hooks (`transformContext`/`onPayload`/`onResponse`) stay
     no-op stubs per feat-007.
   → verify: oracle fixtures per sub-module against real Pi; a canned turn
   through the assembled `Agent` produces the same `AssistantMessage` shape
   `print_mode.rs` already expects; print-mode text/json output matches Pi.
6. **Wave 5 — `main.rs` bootstrap + mode dispatch.** First runnable binary.
   Apply speed constraints #18/#19 (paint-before-block, no TUI cost on
   non-interactive modes).
   → verify: `pirust --version`, `--help`, `-p "hi"` all work end to end;
   `--version`/`--help` touch no disk/network and spin up no tokio runtime.
7. **Wave 6 — live differential + hardening.** Run real `pi -p` and `pirust -p`
   against the same Meridian endpoint, same prompt; diff the session JSONL and the
   stdout shape. Audit assertions for self-referentiality. Time-to-first-frame +
   idle RSS, `pirust` (release build) vs real `pi`, via `hyperfine` (not through
   a POSIX-shell wrapper — measure the exe directly).
   → verify: `./init.sh` green; differential documented; startup/memory numbers
   recorded in `progress.md`; `feature_list.json` updated; delete this file.

## Status

- [x] 1 Wave 0 — spec + oracle (`09-cli-config-spec.md` 2560 lines; 13 fixtures, `--check` idempotent)
- [x] 2 Wave 1 — args + config (129/129 argv rows, 4 help goldens byte-identical,
      88 path comparisons + 18 tilde cases; 8/8 mutations caught; 355 tests green)
- [x] 3 Wave 2 — settings + auth + migrations (62/62 merge, 11/11 auth, 46/46 migrations)
- [x] 4 Wave 3 — session + models (9/9 session-dir + 22 lifecycle tests; 158/158 model
      records; 9/9 v1→v3 migration records; 477 tests green)
- [x] 5a Wave 4 — print_mode.rs (10/10 golden tests green)
- [x] 5b Wave 4 — sdk.rs (4a system_prompt: 11/11 oracle cases; 4b
      provider_attribution: 10/10 oracle cases; 4c auth_guidance: unit-tested,
      no oracle per its own triviality; 4d anthropic-only stream wrapper; 4e
      sdk.rs assembly, proven end-to-end by `tests/sdk_canned_turn.rs` running a
      real turn through a scripted `Faux` provider). See `progress.md` for full
      evidence + named deferrals (blockImages filter, session restore,
      onPayload/onResponse hooks, settings-validation-error fallback).
- [x] 6 Wave 5 — main bootstrap (first runnable binary). `main.rs` wires
      args→config→settings→auth→migrations→session→models→sdk→print_mode per
      spec §15's step table. New: `runtime_host.rs` (`PrintModeSession`/
      `AgentSessionRuntimeHost` adapter over `Agent`+`SessionManager`),
      `initial_message.rs` (`buildInitialMessage` + text-only `@file`
      processing). Fixed a real Wave-4 gap found while wiring: `sdk.rs`'s
      stream closure passed an empty env map instead of the real process
      environment, silently disabling the `ANTHROPIC_API_KEY` fallback; also
      added the missing `--api-key` runtime-override plumbing (hazard §16.30).
      508 workspace tests green, fmt+clippy clean, `./init.sh` green.
      Live-verified in this environment (no configured credentials): `pirust
      --version` → `0.0.1`; `pirust --help` → full byte-identical help text,
      exit 0; `pirust -p "hi"` with zero credentials → "No models available"
      + exit 1 (correct — matches Pi with zero auth configured); with
      `ANTHROPIC_API_KEY` set to a fake key → the full pipeline runs a real
      turn, makes a genuine HTTPS request to Anthropic's API, and persists a
      byte-plausible session JSONL (header, model_change, thinking_level_change,
      user message, error-tail assistant message with the real `HTTP 401`
      body) — **not a successful live call** (no real credentials available),
      but proof the request pipeline is wired end-to-end through to Anthropic's
      real servers. See `progress.md` for full evidence, named narrowings
      (project trust, `--help` ordering vs hazard §16.9, no real
      `SignalRegistry`, message-level not event-level session persistence)
      and an open question for Wave 6 (session-file lazy-open timing vs Pi's
      "no file until first assistant message" claim).
- [ ] 7 Wave 6 — live differential + hardening
