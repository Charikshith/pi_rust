# feat-008 (P7) — Remaining AI providers + catalog generator

## PREREQUISITE (user decision, 2026-08-21): ORACLE UPGRADE 0.80.10 → 0.84.2

The `../pi` checkout is v0.84.2 (760 commits past the locked 0.80.10 oracle that
feat-001..007 fixtures were captured against). User decision: **advance the oracle**
to the current checkout — regenerate/verify ALL existing fixtures against it before
porting feat-008. This is a distinct prerequisite wave:

- **Oracle-upgrade Wave A — fix stale oracle scripts against 0.84.2** (each
  `scripts/gen-*.mjs` that crashes or drifts): confirmed broken today —
  `gen-agent-oracle` (uuid.ts moved to `ai/src/utils/uuid.ts`),
  `gen-cli-oracle` (crash), `gen-printmode-oracle` (crash),
  `gen-model-corpus` (data/*.json now grouped `{api:{modelId:model}}`,
  script assumes flat), plus drift in `gen-anthropic-oracle`/others.
- **Oracle-upgrade Wave B — regenerate all fixtures** and fix every Rust test
  that changes (serde types, new/moved fields, provider counts). Re-run the full
  workspace gate until green.
- **Oracle-upgrade Wave C — close out**: update `feature_list.json` evidence notes
  (piVersion 0.80.10 → 0.84.2) and any version-stamped docs.

**DONE (commit da63f5c, 2026-08-21):**
- pirust-ai: AssistantMessage + anthropic adapter → 0.84.2 (rawStopReason/endTurn,
  model-from-wire optional); model corpus 1306; catalog.rs 13 models;
  DEFAULT_MODEL_PER_PROVIDER 40; resolve_cli_model ambiguity error;
  deepMergeSettings recursive; help template (auth/--use-theme/--tui-mode/new env);
  pi-telemetry alias + NODE_NO_WARNINGS + C:\oracle cwd in oracle scripts;
  clippy large_enum_variant allows. Full workspace green except 3 pre-existing
  env-polluted pirust-tools find tests (C:\Users\Chakri\.git above temp).

**REMAINING (0.84.2):**
1. `gen-printmode-oracle`: NODE_NO_WARNINGS in its child harvests + regenerate
   printmode fixtures (large drift).
2. **v4 session port (BIG)** — 0.84.2 replaced the v3 tree JSONL with a
   mutation-log format. DONE so far (commit a352516): `harness/session/v4/`
   types.rs (Entry/LaneRecord/SessionMutation/records/queries) + state.rs
   (SessionState replay) + codec.rs (header/mutation byte format), oracle-
   verified byte-identical against Pi's real codec.ts (25-record fixture,
   `v4_codec_golden.rs`). STILL TO GO: JsonlSessionStorage (storage.ts incl.
   torn-tail repair + atomic publish), JsonlSessionRepo (repo.ts:
   create/open/list/fork, session-id validation, dir naming), v4 `Session`/
   `memory.ts`/`context.ts`, then the v3→v4 replacement of the SessionStorage
   trait + harness + coding-agent session.rs wiring, then rework
   `gen-agent-oracle` against the v4 APIs.
3. `gen-agent-oracle` rework against the v4 session APIs once the port lands.

## feat-008 waves (on the 0.84.2 baseline)

### Wave 0 — Oracle + catalog data (foundation)
Real `pirust` resolves and streams through at least the key remaining adapters
(openai-completions, openai-responses, google, mistral-conversations) into a
correct `AssistantMessage`, **byte-verified against real Pi as oracle**; the
full 36-provider / 1062-model builtin catalog is served from real Pi's
generated data (fingerprint: 36 providers / 1062 models total), and
`cargo xtask gen-catalog` emits/regenerates it. Every ported adapter follows
the established oracle pattern (golden fixtures captured from real Pi via
`scripts/gen-*.mjs`, `--check` wired into `./init.sh`).

## Scope
- **IN:** the remaining api adapters + provider registry described below,
  OAuth flows, and the catalog-generator port (`xtask gen-catalog` producing
  the full multi-provider `catalog.rs`).
- **OUT (deferred, named):** image-generation providers / `images.ts`
  surface (separate concern, not listed in feat-008); the `feat-009`
  orchestrator and `feat-012` RPC (their own later features); `feat-010`
  dynamic WASM. OAuth kept behind a `cli`/`native` feature flag per
  `docs/analysis/02-ai.md` §4 (local HTTP loopback server; only `login`/
  `refresh`/`toAuth` core in the default crate).

## Cadence
Checkpoint per **phase** (user's locked decision): one wave, verify, report,
pause. Orchestration model as proven in feat-002/003/004: orchestrator
captures Pi oracle, delegates port work to subagents, runs gates between.

## Waves

### Wave 0 — Oracle + catalog data (foundation)
- Capture the **full** 36-provider / 1062-model catalog from real Pi's
  generator. Run `node packages/ai/scripts/generate-models.ts --data-only`
  in `../pi/packages/ai` (network-backed; models.dev confirmed reachable) to
  produce the git-ignored `data/*.json` files; capture the resulting catalog
  into a **committed** fixture (`builtinCatalogFull` in
  `tests/fixtures/pi/cli/models.cases.jsonl` — replacing/augmenting the
  current anthropic-only `builtinCatalogFingerprint`), so the data is
  reproducible offline without the network.
- Extend an oracle capture (`scripts/gen-cli-oracle.mjs` or a new
  `scripts/gen-providers-oracle.mjs`) that loads real Pi's `builtinProviders()`
  and snapshots each provider's `{id, name, baseUrl, auth apiKey env order,
  models[]}` shape — the data the `xtask` catalog generator needs.
- Verify: `node <oracle> --check` green; fixture byte-match against real Pi.

### Wave 1 — Catalog generator (`xtask gen-catalog`, full catalog)
- Replace the anthropic-only slice with a real generator emitting the full
  36-provider / 1062-model `catalog.rs` (all `ProviderDescriptor`s + `Model`s),
  from the Wave-0 fixture. Supersede `crates/pirust-coding-agent/src/catalog.rs`.
- Verify: `cargo xtask gen-catalog --check` green; regenerating a fresh tree
  is a byte no-op; `tests/models_golden.rs` `builtinCatalogFingerprint` still
  green (or re-anchored to the new full record); 36 providers / 1062 models
  resolve from an empty `~/.pirust`.

### Wave 2 — OpenAI completions adapter (`api/openai-completions.rs`)
- Port `openai-completions.ts` (1609 LOC) + `openai-prompt-cache.ts` +
  `transform-messages.ts` deps as needed. Provider group: deepseek, together,
  mistral?, groq, fireworks, cerebras, opencode, minimax*, moonshotai, etc.
  (all `api: "openai-completions"`).
- Oracle: `scripts/gen-providers-oracle.mjs` captures real Pi's
  `openAICompletionsApi` under canned SSE (the feat-002 pattern against Pi's
  real adapter); golden test drives the Rust adapter with the same canned SSE
  → byte-identical final `AssistantMessage`.
- Verify: golden green; `cargo fmt/clippy/test`; `./init.sh` green.

### Wave 3 — OpenAI responses adapter (`api/openai-responses.rs`)
- Port `openai-responses.ts` (380) + `openai-responses-shared.ts` (792) +
  `azure-openai-responses.ts` (337) + `github-copilot-headers.ts` (37) +
  `openai-prompt-cache.ts`. Providers: openai, azure-openai-responses,
  github-copilot, zai*, etc.
- Oracle + golden as Wave 2.
- Verify: golden green; gates green; `./init.sh` green.

### Wave 4 — Google adapters
- Port `google-generative-ai.ts` (525) + `google-shared.ts` (452) +
  `google-vertex.ts` (597). Providers: google, google-vertex.
- Oracle + golden (Vertex SigV4 auth can be stubbed to handler-token —
  verify against Pi's behavior for the auth method exercised).
- Verify: golden green; gates green; `./init.sh` green.

### Wave 5 — Mistral conversations (`api/mistral-conversations.rs`)
- Port `mistral-conversations.ts` (934). Provider: mistral.
- Oracle + golden.
- Verify: golden green; gates green; `./init.sh` green.

### Wave 6 — AWS Bedrock (`api/bedrock-converse-stream.rs`)
- Port `bedrock-converse-stream.ts` (1233). Needs **SigV4** request signing
  (docs/analysis/02-ai.md lists `awsc-smithy-client`/`aws-sigv4` as candidate
  deps; per Ponytail ladder prefer an already-installed crate, else add the
  smallest sigv4 crate, else gated feature). Provider: amazon-bedrock).
- Oracle + golden (sigv4 signing deterministic given fixed region/date →
  fixtures). Auth stays behind feature gate? Bedrock uses static creds, not
  the loopback OAuth — keep in default crate but sign with stdlib/crate.
- Verify: golden green; gates green; `./init.sh` green.

### Wave 7 — OAuth flows (`auth/oauth/*`, `auth/resolve.rs` completion)
- Port `auth/types.ts`/`resolve.ts`/`helpers.ts`/`credential-store.ts`
  completion + OAuth: `pkce.ts`, `device-code.ts`, and the loopback-server
  flows (_anthropic, github-copilot, openai-codex, kimi-coding, openrouter,
  xai, radius_) behind a `cli` feature using `tiny_http`/`axum`/`hyper` for
  the local callback server (2-ai §4 mapping). Non-loopback helpers
  (token refresh, `toAuth`) in the default crate.
- Oracle: Pi's `auth/` behavior captured offline (stored credential →
  resolve → refresh decision) via `scripts/gen-providers-oracle.mjs`.
- Verify: resolve/refresh goldens green; feature-gated loopback server
  unit-tested; `./init.sh` green.

### Wave 8 — Provider registry + wiring into `pirust`
- Replace `faux`-only `providers/mod.rs` with a registry of all built-in
  providers (`builtin_providers()`, `builtin_models()` analog of Pi's
  `all.ts`), each binding its adapter (Waves 2-6) + auth (Wave 7). Wire
  `ModelRuntime::create`/model resolution (`models.rs`) to the full catalog
  (Wave 1) + full provider registry, so `pirust -p --provider <X> --model
  ...` resolves and streams through every ported provider.
- Oracle: live differential — real `pi -p` vs `pirust -p` against local
  llama-server for one openai-completions and one openai-responses provider
  (the feat-005 Wave-6 pattern); session JSONL shape parity.
- Verify: full `./init.sh` green; `ModelRuntime.create` 18-row goldens still
  pass (re-anchored to 36-provider catalog); live differential shape-parity.

## Definition of Done (all required)
- [ ] Every ported adapter verified against real Pi as oracle (golden SSE
      replay → byte-identical final AssistantMessage), not self-authored.
- [ ] Full 36-provider / 1062-model catalog served; `xtask gen-catalog`
      regenerates it and `--check` is green; fixture committed (offline-reproducible).
- [ ] All 5 pure-layer + adapter goldens green; 0 oracle drift.
- [ ] Live differential (openai-completions + openai-responses) shows session
      JSONL + stdout shape parity vs real pi.
- [ ] `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`, `cargo build` all green.
- [ ] `./init.sh` green, repo restartable; `../pi` oracle checkout left clean.
- [ ] `plan.md` closed out into `feature_list.json` evidence; deferred list
      named (images providers, RPC, orchestrator, WASM).

## Residuals to name at close (not silent)
- models.dev/Nvidia/OpenRouter data is network-fetched at generation time;
  the committed fixture makes the *built catalog* reproducible, but
  regenerating fresh `data/*.json` still needs network (matches Pi).
- OAuth loopback flows are CLI-only behind `cli` feature (Node `node:http`
  analog); default crate exposes resolve/refresh/toAuth only.
- Bedrock SigV4 signing dep choice is resolved on the ladder at Wave 6.
- Catalog fingerprint is version-dependent (drift signal not contract) —
  already stated in the existing fixture note.
