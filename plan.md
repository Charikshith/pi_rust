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
5. **Wave 4 — `sdk.rs` + `print_mode.rs`.**
   → verify: print-mode text and json output shapes match Pi's for a canned turn.
6. **Wave 5 — `main.rs` bootstrap + mode dispatch.** First runnable binary.
   → verify: `pirust --version`, `--help`, `-p "hi"` all work end to end.
7. **Wave 6 — live differential + hardening.** Run real `pi -p` and `pirust -p`
   against the same Meridian endpoint, same prompt; diff the session JSONL and the
   stdout shape. Audit assertions for self-referentiality.
   → verify: `./init.sh` green; differential documented; `feature_list.json` +
   `progress.md` updated; delete this file.

## Status

- [x] 1 Wave 0 — spec + oracle (`09-cli-config-spec.md` 2560 lines; 13 fixtures, `--check` idempotent)
- [x] 2 Wave 1 — args + config (129/129 argv rows, 4 help goldens byte-identical,
      88 path comparisons + 18 tilde cases; 8/8 mutations caught; 355 tests green)
- [x] 3 Wave 2 — settings + auth + migrations (62/62 merge, 11/11 auth, 46/46 migrations)
- [x] 4 Wave 3 — session + models (9/9 session-dir + 22 lifecycle tests; 158/158 model
      records; 9/9 v1→v3 migration records; 477 tests green)
- [ ] 5 Wave 4 — sdk + print mode
- [ ] 6 Wave 5 — main bootstrap (first runnable binary)
- [ ] 7 Wave 6 — live differential + hardening
