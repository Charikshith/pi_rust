---
type: template
title: "Progress Log Template"
description: "Session progress log for agent continuity"
artifact: "progress.md"
tags: [state, progress, continuity, session, tracking, verification-plan]
---

# Session Progress Log

## Current State

**Last Updated:** 2026-07-28
**Active Feature:** feat-005 — headless `pirust` binary (NEXT; not started).
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

1. **feat-005 (P4) headless `pirust` binary** — coding-agent CLI/args (hand-rolled parser),
   layered config (~45 fields) + 5 startup migrations, print/json/rpc run modes. First
   runnable binary. → verify: `pirust -p "…"` runs a real turn end-to-end against a live
   provider, and the on-disk session JSONL it writes stays byte-compatible (the feat-003
   golden suites already pin the format). This is also the phase that finally enables a
   **live side-by-side diff against real `pi`**, which no fixture can substitute for.
   Carries the feat-004 deferrals: the rg/fd downloader, and bash's
   `commandPrefix`/`spawnHook`/`shellPath` config plumbing.
2. feat-006 (P5) `pirust-tui` literal port → then feat-007 wires tools+TUI together.

## Blockers / Risks

- [ ] Dynamic JS extension loading (jiti) has no clean Rust 1:1 — strategy decision needed (see 00-overview §4.4 / §5).
- [ ] Generated model catalog JSON was absent in checkout (build-time/git-ignored) — must port generator or obtain output (feat-008).
- [ ] UTF-16 offset semantics (editor + compaction cut points) — fidelity hazard; needs golden tests.

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
