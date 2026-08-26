---
type: template
title: "AGENTS.md / CLAUDE.md Template"
description: "Startup workflow, coding policy (Ponytail ladder), coding standards, editing discipline (surgical changes), working rules, definition of done (test-first), multi-step planning, end-of-session procedure, safety carve-outs, verification commands, and escalation"
artifact: "AGENTS.md or CLAUDE.md"
tags: [instructions, startup, workflow, done-definition, scope, coding-policy, simplicity, surgical-editing, safety, behavioral]
---

# AGENTS.md

Project harness for reliable agent-assisted development in a Rust (Cargo) codebase.
This repository is not yet initialized — bootstrap with `cargo init` before the
verification path below applies.

> **Why this structure**: This template instantiates patterns from:
> - [Lifecycle & Bootstrap](../references/lifecycle-bootstrap-pattern.md) — startup workflow, end-of-session routine, trust gates
> - [Memory Persistence](../references/memory-persistence-pattern.md) — progress.md as a session continuity artifact, two-step updates
> - [Tool Registry & Safety](../references/tool-registry-pattern.md) — verification commands as a safety gate before claiming done
> - [Context Engineering](../references/context-engineering-pattern.md) — progressive disclosure of project docs before code editing
>
> Behavioral policies are embedded directly (no external dependency required):
> - **Coding Policy** — the Ponytail ladder: YAGNI → stdlib → native → dep → one-liner → minimum. Install the full Ponytail skill for intensity levels (lite/full/ultra) and debt tracking (`/ponytail-debt`).
> - **Rust Advantage Principle** — wire/on-disk contract EXACT, internal algorithms IDIOMATIC Rust (see the project rule below): the port improves on Pi, it never mimics TS flaws.
> - **Editing Discipline** — surgical changes (Karpathy §3): touch only what the feature requires, match existing style, don't "improve" adjacent code.
> - **Definition of Done** — test-first ordering (Karpathy §4): for bugs, reproduce first; for features, write the check first.
>
> See [templates/index.md](index.md) for all available templates.

## Startup Workflow

Before writing code:

1. **Confirm working directory** with `pwd`
2. **Read this file** completely
3. **Read project docs if present** (`docs/ARCHITECTURE.md`, `docs/PRODUCT.md`, README, or equivalent)
4. **Run `./init.sh`** to verify environment is healthy
5. **Read `feature_list.json`** to see current feature state
5.5. **Read `plan.md`** if it exists — it contains the current feature's step-by-step plan with verify checks.
6. **Review recent commits** with `git log --oneline -5`
7. **State your understanding**: In one line, what the task requires.
   If multiple interpretations exist, name them. If the ambiguity is structural
   (architecture, data model, security boundary, multi-module interaction),
   **stop and ask** — the cost of the wrong answer exceeds the round-trip.
   If the ambiguity is cosmetic or the safe default is clear, proceed and name
   your assumption.
8. **Write a scope boundary** (one line each):
   - **In scope:** the single feature/behavior the request names.
   - **Out of scope:** anything not named — extra modes, flags, commands, config,
     abstractions, or "while I'm here" improvements. If a change crosses this
     line, stop and ask before building it.
9. **Check Ponytail mode**: If the Ponytail skill is installed, confirm the
   current intensity level (`lite`, `full`, `ultra`). Default to `full` if unset.
   The level governs how aggressively the ladder is applied (see Coding Policy).

## Coding Policy

Before writing any code, stop at the first rung that holds:

1. **Does this need to exist at all?** Speculative need = skip it. (YAGNI)
2. **Does the standard library already do this?** Use it.
3. **Does a native platform feature cover it?** `<input type="date">` over a
   picker lib, CSS over JS, DB constraint over app code.
4. **Does an already-installed dependency solve it?** Use it. Never add a new
   dependency for what a few lines can do.
5. **Can this be one line?** Make it one line.
6. **Only then:** write the minimum code that works.

The ladder is a reflex, not a research project. Two rungs work → take the
higher one and move on. The first lazy solution that works is the right one.

### Intensity Levels

If the Ponytail skill is installed, switch modes with `/ponytail lite|full|ultra`.
When the skill is not installed, default to **full**.

| Level | What changes |
|-------|-------------|
| **lite** | Build what's asked, but name the lazier alternative in one line. User picks. |
| **full** | The ladder enforced. Stdlib and native first. Shortest diff, shortest explanation. Default. |
| **ultra** | YAGNI extremist. Deletion before addition. Ship the one-liner and challenge the rest of the requirement in the same breath. |

Example: "Add a cache for these API responses."
- lite: "Done, cache added. FYI: `functools.lru_cache` covers this in one line if you'd rather not own a cache class."
- full: "`@lru_cache(maxsize=1000)` on the fetch function. Skipped custom cache class, add when lru_cache measurably falls short."
- ultra: "No cache until a profiler says so. When it does: `@lru_cache`. A hand-rolled TTL cache class is a bug farm with a hit rate."

## Coding Standards

- Write the **minimum code** that solves the problem. Nothing speculative.
- No abstractions that weren't requested. No interface with one implementation.
  No factory for one product. No config for a value that never changes.
- No unrequested "flexibility" or "configurability." No scaffolding "for later" —
  later can scaffold for itself.
- No error handling for scenarios the code structure makes **impossible**
  (a dict key you just set 3 lines up doesn't need a KeyError handler).
- Deletion over addition. Boring over clever. Fewest files possible.
- Two stdlib options, same size? Pick the one that's correct on edge cases.
- Mark deliberate simplifications: `// ponytail: O(n²) scan, upgrade to index if >10k rows`
- Non-trivial logic leaves ONE runnable check (assert-based demo or small test).
  Trivial one-liners need no test. No frameworks, no fixtures.

## Editing Discipline

- **Touch only what the feature requires.** Do not "improve" or reformat adjacent
  code, comments, or whitespace — even when they could be better.
- **Match the existing style.** Consistency beats your preference.
- **Don't refactor things that aren't broken.** The diff's best outcome is getting shorter.
- **If you notice unrelated dead code or issues**, mention them in `progress.md` —
  don't fix them in this diff.
- **Remove only the imports, variables, or functions that YOUR changes made unused.**
  Do NOT remove pre-existing dead code unless asked.
- **The test:** Every changed line should trace directly to the feature in
  `feature_list.json`.

## Working Rules

- **One feature at a time**: Pick exactly one unfinished feature from `feature_list.json`
- **Verification required**: Don't claim done without running verification commands
- **Update artifacts**: Before ending session, update `progress.md`, `feature_list.json`, and `session-handoff.md`
- **Stay in scope**: Don't modify files unrelated to the current feature
- **No bonus surface**: Build only what the request names. Do NOT add new CLI flags,
  commands, modes, config keys, or abstractions beyond the stated feature. If a
  useful extra occurs to you, name it in one line and ask — don't build it.
- **Leave clean state**: Next session must be able to run `./init.sh` immediately
- **Plan lifecycle**: `plan.md` is ephemeral state for the active feature only — create it before step 1, update it after each step completes, and when the feature transitions to `done`, extract the completed plan into `feature_list.json` as `evidence` and delete `plan.md`. A stale `plan.md` for a completed feature is a bug.

## Required Artifacts

- `feature_list.json` — Feature state tracker (source of truth)
- `progress.md` — Session continuity log
- `init.sh` — Standard startup and verification path
- `session-handoff.md` — Required. Fill it in before every session ends.
- `plan.md` — Current feature's step-by-step plan (generated by "Before Multi-Step Work")

## Before Multi-Step Work

State a **one-line success criterion**. If the task spans more than 2 files or
3 logical steps, add a numbered plan with a verify check per step:

```
1. [Step] → verify: [specific check]
2. [Step] → verify: [specific check]
3. [Step] → verify: [specific check]
```

For bugs: write a reproduction test FIRST, then make it pass.
For features: write the verification check FIRST, then implement.

**Save the plan to `plan.md`** before starting step 1. Update it as steps complete.

One-liners and trivial changes skip this — the task itself is the criterion.

## Definition of Done

A feature is done only when ALL of the following are true:

- [ ] Target behavior is implemented
- [ ] For bugs: a reproduction test was written FIRST, then made to pass
- [ ] For features: a verification check was written FIRST, then the code
- [ ] Required verification actually ran and passed (tests / lint / type-check)
- [ ] Evidence recorded in `feature_list.json` or `progress.md`
- [ ] **Scope trace passed**: every changed line maps to the named feature; no
      bonus flags / commands / modes / abstractions were added (see Working Rules)
- [ ] Repository remains restartable from standard startup path

## End of Session

Before ending a session:

1. Update `progress.md` with current state
2. Update `feature_list.json` with new feature status
2.5. If the feature transitions to `done`, extract the completed `plan.md` into the feature's `evidence` field in `feature_list.json`, then delete `plan.md`.
3. Update `plan.md` with current step status (mark completed steps, note blockers)
4. Record any unresolved risks or blockers
5. Fill in `session-handoff.md` (objective, completed items, verification evidence, files changed, decisions, blockers, next-step)
6. Commit with descriptive message once work is in safe state
7. Leave repo clean enough for next session to run `./init.sh` immediately

## Safety (Never Simplify Away)

- Input validation at trust boundaries
- Error handling that prevents data loss
- Security measures
- Accessibility basics
- Hardware calibration (a real clock drifts, a sensor reads off — the platform is
  never the spec ideal)
- Anything the user explicitly asked to keep

## TUI / Harness Architecture Guardrails

The interactive TUI is an active risk area. Read `docs/tui-design-audit.md`, `docs/tui-design-samples.html`, and `plan.md` before changing `InteractiveMode` or the harness boundary.

- Preserve Pi's observable behavior and persisted/wire formats, but do **not** copy JavaScript runtime mechanics into Rust when they create blocking or lifecycle problems.
- `AgentHarness` remains UI-agnostic: it owns agent execution, turn lifecycle, tool events, cancellation, and session state. `InteractiveMode` owns terminal input, focus, layout, palettes, and rendering.
- Never call `Handle::block_on` from an already-running Tokio runtime. The current `block_in_place` path is only a temporary panic workaround; the target is a non-blocking async turn state machine.
- Do not claim the TUI is customer-ready based only on component/unit tests. Require delayed-provider black-box coverage for submit, streaming, cancellation, errors, tools, resize, and clean shutdown.
- Keep cwd, session id, provider/model, reasoning level, context usage, cost, tool state, and connection/turn state visible in the interactive status surface.
- Slash-command autocomplete must use the same registered handlers as command execution; model selection and session resume must update the active session and status projection.
- Use typed lifecycle events/state, explicit turn ids and ordering, bounded/coalesced event delivery, cancellation tokens, and RAII cleanup for terminal/runtime/subscription/task ownership.
- Rust-specific goals are part of correctness: responsive async execution, bounded memory, deterministic cleanup, and measured startup/idle/streaming performance. Rust must not merely reproduce Pi's blocking behavior.
- A fresh rewrite is not authorized. Refactor the existing TUI subsystem incrementally, preserving verified harness/session behavior and adding regression tests before architectural changes.

## Verification Commands

```bash
# Full verification (recommended)
./init.sh
```

Required checks (once `Cargo.toml` exists):
- `cargo fmt --check`
- `cargo clippy --all-targets -- -D warnings`
- `cargo test`
- `cargo build`

## Correctness Bar (project rule)

This is a 1:1 port of Pi. A feature is correct only when it behaves **exactly as Pi**,
verified against **Pi as the oracle** — never against self-authored expectations:

- Prefer **golden tests** driven by real Pi artifacts. Byte-compat serde types are
  validated by `crates/pirust-ai/tests/golden.rs` against vendored fixtures + a corpus
  of real session messages (`tests/fixtures/pi/`, regenerated by `scripts/gen-*.mjs`).
- When Pi's runtime behavior contradicts its own TS type declarations (field order,
  optionality, transient fields), **the runtime wins** — replicate observed output and
  leave a comment citing the fixture.
- Naming: all Rust code is `pirust*`. Keep original names only for on-disk data / wire
  identifiers that must stay compatible with real Pi (`~/.pi`, `pi-messages`, etc.).

## Rust Advantage Principle (project rule — ALL code, existing and future)

**The port is a concept/feature port, not a syntax port.** "Porting" means: implement
Pi's *concept and feature* in Rust and **improve on it using Rust's strengths** —
never mimic TypeScript's flaws.

The split, in priority order:

1. **Wire / on-disk contract: EXACT.** The bytes that cross a boundary must be
   byte-identical to Pi — request JSON (key order, `null`-vs-missing), ids, `max_tokens`,
   file/session format, error messages that cross the wire. Verified by oracle goldens.
   This is the compatibility contract; it never yields.
2. **Internal algorithms: IDIOMATIC Rust.** Everything that doesn't cross a boundary
   must be written as if TypeScript never existed:
   - **Borrow instead of clone**: take `&[T]` / `&Context`, clone only where the
     algorithm actually rewrites. No "clone the world" to satisfy a caller.
   - **No fake JS string semantics**: char-based slicing, no `slice_utf16` ceremony
     for things that are ASCII-after-sanitize, no re-implementing JS's broken
     lone-surrogate behavior — that is a flaw, not a contract.
   - **Idiomatic stdlib**: `div_ceil`, iterators, proper error propagation with `Result`
     (never swallow errors with `unwrap_or_default` just because TS catches-and-continues
     — propagate like Pi's throws).
   - **Right tool for the data**: use `String`, `Value`, typed structs, or serde
     derives where they fit, not whatever TS used.
   - Genuine Unicode/data semantics (e.g. unpaired-surrogate detection on real input)
     stay — those are about the data, not TS.
3. **Existing code**: when a module is touched by a wave and contains TS-mimicry
   (clone-heavy signatures, fake JS string semantics, swallowed errors), **rewrite it
   to idiomatic Rust in the same wave** — do not leave a second, worse implementation
   behind. Each wave may add one such cleanup; never let the debt compound.

This rule applies to **all future feature development and every existing implementation
that needs a rewrite**. It is part of the harness, not a suggestion.

## The Oracle Audit Rule (hard requirement — prevents rework)

**Never declare code "fabricated" or "invented" based on grep absence alone.**
Grep-miss ≠ doesn't-exist. This project paid for that mistake twice: an audit on a
machine *without* the Pi oracle deleted real, verified code (`latex.rs`, `tui-main-screen.ts`
import, keybindings) and regenerated fixtures to match, causing wholesale rework.

Before you claim something in `../pi` is fabricated, missing, or wrong, you MUST:

1. **Have the oracle**: the sibling `D:\Code\AI\Agents\pi` checkout (or whatever path
   `scripts/gen-*.mjs` uses) must EXIST and be the CURRENT Pi. A machine without it is
   NOT qualified to audit — say so and stop, or pull it first.
2. **Search the real source, not absence**: `grep -rn` the actual `../pi/packages/tui/src/`
   (and `git log --all -S` across branches) for the symbol BEFORE concluding it doesn't
   exist. An unqualified `grep -r` that returns nothing is a *signal to investigate the
   search*, not a verdict.
3. **Run the oracle, don't reason about it**: `node scripts/gen-tui-oracle.mjs --check`
   must pass against current Pi BEFORE and AFTER any change. If it crashes (e.g.
   `TUI is not a constructor`), that's the first red flag that an import or fixture is
   wrong — investigate the crash, don't paper over it.
4. **Never regenerate fixtures to match a suspicion.** Fixtures come from executing real
   Pi code. If a fixture "disagrees", the fixture generator is stale or the source
   moved — regenerate from real Pi, never hand-edit a fixture to fit a claim.
5. **Deletion is the last resort**: when an audit suspects a module, the default is
   "unverifiable on this machine — flag for re-verification where the oracle exists",
   NOT "delete it". Deleting verified code to fix an audit doubt is the exact failure
   that caused the Wave 6/7 rework.

**The one acceptable form of "fabrication" finding**: a claim backed by (a) the oracle
checkout present, (b) `grep` + `git log --all -S` both empty, (c) the oracle script run
successfully against it, (d) the finding written as "flag for human review", not as
an executed deletion.

## Escalation

If you encounter:
- **Architecture decisions**: Consult project architecture docs if present, otherwise ask user
- **Unclear or over-specified requirements**: Check product/requirements docs if present,
  otherwise ask user. Question whether the spec itself is over-specified.
- **Repeated test failures**: Update progress, flag for human review
- **Scope ambiguity**: Re-read `feature_list.json` for definition of done
