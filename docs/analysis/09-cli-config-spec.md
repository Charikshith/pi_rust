# feat-005 Port Spec: `pirust-coding-agent` headless bootstrap (1:1 port of `packages/coding-agent`)

> READ-ONLY analysis. Builds on `docs/analysis/03-coding-agent.md` (feat-001 exploration),
> `06-anthropic-runtime-spec.md` (feat-002 runtime) and `07-agent-core-spec.md` (feat-003
> agent-core). All citations are `file:line` into `pi_space/pi/packages/coding-agent/src/*`
> unless another package is named. This spec is the byte- and behaviour-identity target for
> the crate `crates/pirust-coding-agent` (today a 25-line scaffold, `src/main.rs:1-25`).
>
> Ported crates confirmed available: `pirust-ai` (feat-001/002), `pirust-agent-core`
> (feat-003 — `Agent`/`AgentOptions` at `crates/pirust-agent-core/src/agent.rs:205-259`),
> `pirust-tools` (feat-004 — registry `crates/pirust-tools/src/lib.rs:111-212`, and the
> pirust identity constants already live at `crates/pirust-tools/src/binaries.rs:154-175`).
>
> **Where Pi's runtime contradicts its own types or comments, the runtime wins.** Every such
> case is called out inline ("RUNTIME WINS").

---

## Executive summary (~25 lines)

1. The headless bootstrap is five layers: `cli.ts` (20 lines; the process setup is `:12-20`) →
   `parseArgs` (`cli/args.ts:63-210`) → `main()` (`main.ts:473-859`, 387 lines) →
   `createAgentSession` (`core/sdk.ts:164-393`) → `runPrintMode` (`modes/print-mode.ts:32-159`).
2. `parseArgs` is a hand-rolled single-pass `for` loop with a 42-branch if/else chain (38
   known-flag branches + 4 tail branches) and **no** `--flag=value` support for known flags,
   **no** `--` end-of-options, and blind value consumption. It is fully pure (no I/O) and is
   the highest-value golden-test target (§3).
3. `Args` has 41 fields; two need care in Rust: `listModels?: string | true` is **3-state**
   (absent / `true` / pattern) and `unknownFlags: Map<string, boolean | string>` must
   **preserve insertion order** (it is iterated to apply extension flags,
   `core/agent-session-services.ts:99`) → `IndexMap<String, FlagValue>`.
4. `printHelp` (`cli/args.ts:212-390`) is a single 167-line template literal parameterized by
   `APP_NAME`/`CONFIG_DIR_NAME`/`ENV_AGENT_DIR`/`ENV_SESSION_DIR`. It must be reproduced
   verbatim with those five substitution points so a golden test can render Pi's identity
   (§4). **The fixture `tests/fixtures/pi/cli/help.plain.golden` does not exist yet** — §4.3
   gives the capture recipe; capturing it is a prerequisite task, not an assumption.
5. App identity is read from `package.json` `piConfig` at module load (`config.ts:479-496`);
   pirust hard-codes its own identity (`~/.pirust/agent`, `PIRUST_CODING_AGENT_DIR`,
   `PIRUST_CODING_AGENT_SESSION_DIR`, `PIRUST_OFFLINE`). **File FORMATS are unchanged** (§5).
6. `Settings` (`core/settings-manager.ts:83-129`) is 45 optional fields incl. 5 nested
   structs. Layering is exactly two scopes (global `settings.json`, project
   `<cwd>/.pi/settings.json`) merged by `deepMergeSettings` (`:132-160`), which is **one level
   deep only** — nested objects are `{...base, ...override}` (NOT recursive) and **arrays are
   replaced wholesale**, not concatenated (§6.3).
7. `migrateSettings` (`:381-440`) is 4 in-place rewrites (`queueMode`→`steeringMode`,
   `websockets`→`transport`, object-`skills`→array, `retry.maxDelayMs`→
   `retry.provider.maxRetryDelayMs`) applied on **every** load and on **every** write-merge.
8. `runMigrations` (`migrations.ts:305-315`) runs 5 migrations in a fixed order; only
   `migrateAuthToAuthJson` returns data, only `migrateExtensionSystem` returns warnings, and
   every one of them swallows its own errors. `auth.json` is written `0o600` (§7).
9. The cwd→session-dir encoding `--${cwd.replace(/^[/\\]/,"").replace(/[/\\:]/g,"-")}--`
   appears **three times** with identical semantics (`migrations.ts:112`,
   `core/session-manager.ts:475`, `packages/agent/src/harness/session/jsonl-repo.ts:35`).
   Worked examples incl. a Windows drive letter in §7.7.
10. `auth.json` is `Record<providerId, Credential>` where `Credential =
    {type:"api_key",key?,env?} | {type:"oauth",refresh,access,expires,...}`
    (`packages/ai/src/auth/types.ts:17-37`); writes are always full-file
    `JSON.stringify(merged,null,2)` under a lockfile, mode `0o600` (§8).
11. Model resolution is a 4-stage chain (`--model`/`--provider` → scoped models → saved
    default → first available) and needs only **one** `anthropic` provider entry in
    `models.json` to run against a local proxy — the minimum config is §9.5.
12. `buildSessionOptions` (`main.ts:357-453`) is the CLI→session-options translation, quoted
    in full in §10.1. `--thinking` always wins over a `:level` suffix and over scoped-model
    levels; `--api-key` is applied **outside** it (`main.ts:705-715`).
13. `new Agent({...})` (`core/sdk.ts:289-355`) wires 15 fields; 6 are extension hooks that
    stub to no-ops in feat-005 and 9 are core behaviour (§10.2 marks each).
14. `runPrintMode` (159 lines, quoted in §13) writes the agent event stream to stdout via
    `writeRawStdout` in json mode, or the final assistant text-blocks only in text mode; all
    diagnostics go to **stderr** and an errored/aborted final assistant message yields exit 1.
15. `takeOverStdout` (`core/output-guard.ts:45-70`) replaces `process.stdout.write` with a
    write to **stderr**, so every `console.log` in the bootstrap silently becomes stderr in
    non-interactive mode. Only `writeRawStdout` reaches real stdout (§13.3).
16. Two paths would BLOCK a headless run and must be made safe: `--resume` builds a TUI
    unconditionally (`main.ts:321-336` → `cli/session-picker.ts:20-54`) and the `--session`
    "global" branch calls `promptConfirm` on stdin (`main.ts:305-313`). §16.
17. `cli/project-trust.ts:7-62` already degrades correctly: every `ui.*` method returns
    `undefined`/`false` when `mode !== "interactive"`, and `notify` prints to stderr.
18. Proposed layout: 10 modules under `crates/pirust-coding-agent/src/`, 6 of which are pure
    leaves suitable for parallel subagents (§2).

---

## 1. Scope

### 1.1 IN

| Area | Pi source | Spec § |
|---|---|---|
| `parseArgs` + `printHelp` | `cli/args.ts` (390 lines) | §3, §4 |
| `resolveAppMode` / `takeOverStdout` gating | `main.ts:100-119,540-544` | §3.7, §13.3 |
| Config paths + app identity | `config.ts:466-567` | §5 |
| All 5 startup migrations | `migrations.ts` (315 lines) | §7 |
| `Settings` + global/project deep merge + `migrateSettings` | `core/settings-manager.ts` | §6 |
| `auth.json` | `core/auth-storage.ts` | §8 |
| `models.json` resolution (**anthropic api only**) | `core/model-config.ts`, `model-runtime.ts`, `model-resolver.ts` | §9 |
| Session manager (+ the `SessionRepo` impls deferred from feat-003) | `core/session-manager.ts`; `packages/agent/src/harness/session/{jsonl,memory}-repo.ts`, `repo-utils.ts` | §11 |
| `core/sdk.ts` wiring to `Agent` | `core/sdk.ts:164-393` | §10.2 |
| System prompt | `core/system-prompt.ts` | §14 |
| `@file` args + piped stdin → initial message | `cli/file-processor.ts`, `cli/initial-message.ts`, `main.ts:58-75,121-140` | §12 |
| `print-mode.ts` (text + json) | `modes/print-mode.ts` | §13 |

### 1.2 OUT (and where each goes)

| Area | Pi source | Destination |
|---|---|---|
| RPC mode + `rpc-entry.ts` + the `@file`-in-RPC guard | `modes/rpc/*`, `rpc-entry.ts`, `main.ts:546-549,811-813` | **feat-012** |
| Interactive TUI, session picker, first-time setup, startup UI/selector, theme init & watcher | `modes/interactive/*`, `cli/session-picker.ts`, `cli/startup-ui.ts`, `main.ts:563-566,581-586,782-788,814-843` | **feat-006 / feat-007** |
| Extensions + resource-loader (skills, prompt-templates, themes, keybindings, slash-commands, context files) | `core/resource-loader.ts`, `core/extensions/*`, `extensions/index.ts` | **STUBBED here → feat-007** |
| `package-manager-cli.ts` verbs (`install`/`remove`/`uninstall`/`update`/`list`/`config`) | `package-manager-cli.ts`, `main.ts:492-507` | **not ported** |
| HTML export (`--export`) | `core/export-html/*`, `main.ts:526-538` | **not ported** (keep the flag; §3.6 note) |
| Telemetry / analytics | `core/telemetry.ts`, settings `enableAnalytics`/`trackingId` | **not ported** (fields parsed & round-tripped only) |
| Self-update + Windows quarantine cleanup | `config.ts:29-355`, `utils/windows-self-update.ts`, `main.ts:482-484` | **not ported** |
| HTTP proxy + undici dispatcher | `core/http-dispatcher.ts`, `main.ts:489-490,749-750` | **not ported** (settings fields parsed only) |

**Stub contract for extensions/resource-loader.** Every call site keeps its shape but the
implementation is inert: `resourceLoader.getExtensions()` → `{extensions:[], errors:[],
runtime:{flagValues:{}, pendingProviderRegistrations:[], pendingNativeProviderRegistrations:[]}}`;
`printHelp(extensionFlags)` is called with an empty slice (so the help text's
`extensionFlagsText` is `""`, `cli/args.ts:213-222`); `applyExtensionFlagValues`
(`core/agent-session-services.ts:81-127`) still runs, which means **every** unknown flag
becomes the error `Unknown option: --foo` / `Unknown options: --a, --b` (§3.6). The system
prompt's `contextFiles`/`skills` inputs are empty vectors, and `--no-skills`/`--no-themes`/
`--no-prompt-templates`/`--no-context-files`/`--no-extensions`/`--skill`/`--theme`/
`--prompt-template`/`--extension` parse into `Args` and are then ignored.

---

## 2. Proposed Rust module layout

Under `crates/pirust-coding-agent/src/`:

| Module | Responsibility (one line) | Ports |
|---|---|---|
| `args.rs` | `Args`, `parse_args`, `print_help`, `is_valid_thinking_level`, `Mode` | `cli/args.ts` |
| `config.rs` | App identity constants + all `get_*_dir`/`get_*_path` resolvers + tilde expansion | `config.ts:466-567`, `utils/paths.ts:57-85` |
| `settings.rs` | `Settings` + nested structs, `SettingsManager` (2-scope load/merge/persist), `migrate_settings`, `deep_merge_settings` | `core/settings-manager.ts` |
| `auth.rs` | `Credential`, `AuthStorage` (locked read/modify/delete over `auth.json`, `0600`) | `core/auth-storage.ts` |
| `migrations.rs` | The 5 startup migrations + `run_migrations` + `show_deprecation_warnings` | `migrations.ts` |
| `session.rs` | `SessionManager` (jsonl tree, v1→v3 migration, list/listAll, create/open/fork/continue), `get_default_session_dir`, `assert_valid_session_id` | `core/session-manager.ts` |
| `models.rs` | `ModelConfig` (models.json parse/validate), `ModelRuntime` (anthropic-only), `resolve_cli_model`, `resolve_model_scope`, `parse_model_pattern`, `find_initial_model`, `list_models` | `core/{model-config,model-runtime,model-resolver,models-store}.ts`, `cli/list-models.ts` |
| `sdk.rs` | `create_agent_session` — builds `AgentOptions`, restores messages/thinking, computes active tool names | `core/sdk.ts` |
| `print_mode.rs` | `run_print_mode` (text + json) + `output_guard` submodule (`take_over_stdout`/`write_raw_stdout`/`flush_raw_stdout`) | `modes/print-mode.ts`, `core/output-guard.ts` |
| `main.rs` | The bootstrap: order of operations, diagnostics reporting, exit codes, `resolve_app_mode`, `build_session_options`, `create_session_manager` | `cli.ts`, `main.ts` |

Plus two small support modules that fall out of the above (both pure leaves):
`system_prompt.rs` (`core/system-prompt.ts`) and `initial_message.rs`
(`cli/{file-processor,initial-message}.ts`).

**Also in feat-005 but landing in `pirust-agent-core`** (the feat-003 deferral):
`harness/session/{repo_utils.rs, jsonl_repo.rs, memory_repo.rs}` — see §11.6. These are NOT
on the headless critical path (coding-agent's `SessionManager` is an independent
implementation and never references `SessionRepo`; verified by grep over
`packages/coding-agent/src` — zero hits).

**Parallelizable leaves (no cross-deps beyond `config.rs`):** `args.rs`, `config.rs`,
`settings.rs`, `auth.rs`, `system_prompt.rs`, `initial_message.rs`, and the three
`agent-core` repo modules.
**Sequenced integrators:** `migrations.rs` (needs `config`), `session.rs` (needs `config` +
agent-core message types), `models.rs` (needs `config`+`auth`+`pirust-ai`), `sdk.rs` (needs
`models`+`session`+`settings`+agent-core+tools), `print_mode.rs` (needs `sdk`), `main.rs` (all).

**Crates to add** to `pirust-coding-agent/Cargo.toml` (today: pirust-{ai,agent-core,tui,tools},
serde, serde_json, anyhow, tokio): `indexmap` (insertion-ordered `unknownFlags`, §3.2),
`fs4` or `fd-lock` (the `proper-lockfile` equivalent for `settings.json`/`auth.json`/
`trust.json`), `dirs` (`os.homedir()`), `globset` or `wax` (`minimatch` for `--models` globs,
§9.3), `chrono` (`new Date().toISOString()` — must emit exactly
`YYYY-MM-DDTHH:MM:SS.sssZ`), `uuid` (v4 for the 8-hex entry ids; uuidv7 comes from
agent-core), `is-terminal`/`std::io::IsTerminal` (TTY detection). Colour: Pi uses `chalk`;
all coloured output in the ported paths goes to stderr and colour is **not** part of the
byte contract (`chalk` auto-disables when not a TTY) — emit plain text.

---

## 3. `cli/args.ts` — `Args`, `parseArgs`, diagnostics

### 3.1 `Args` verbatim (`cli/args.ts:10-55`)

```ts
export type Mode = "text" | "json" | "rpc";

export interface Args {
	provider?: string;
	model?: string;
	apiKey?: string;
	systemPrompt?: string;
	appendSystemPrompt?: string[];
	thinking?: ThinkingLevel;
	continue?: boolean;
	resume?: boolean;
	help?: boolean;
	version?: boolean;
	mode?: Mode;
	name?: string;
	noSession?: boolean;
	session?: string;
	sessionId?: string;
	fork?: string;
	sessionDir?: string;
	models?: string[];
	tools?: string[];
	excludeTools?: string[];
	noTools?: boolean;
	noBuiltinTools?: boolean;
	extensions?: string[];
	noExtensions?: boolean;
	print?: boolean;
	export?: string;
	noSkills?: boolean;
	skills?: string[];
	promptTemplates?: string[];
	noPromptTemplates?: boolean;
	themes?: string[];
	noThemes?: boolean;
	noContextFiles?: boolean;
	listModels?: string | true;
	offline?: boolean;
	verbose?: boolean;
	projectTrustOverride?: boolean;
	messages: string[];
	fileArgs: string[];
	/** Unknown flags (potentially extension flags) - map of flag name to value */
	unknownFlags: Map<string, boolean | string>;
	diagnostics: Array<{ type: "warning" | "error"; message: string }>;
}
```

`VALID_THINKING_LEVELS` (`:57`) = `["off","minimal","low","medium","high","xhigh","max"]` —
this exact order and comma-space joining is used in the warning string (§3.6).

### 3.2 Rust shape

```rust
pub enum Mode { Text, Json, Rpc }

/// `listModels?: string | true` (args.ts:46) is THREE states, not two.
pub enum ListModels { Absent, All, Pattern(String) }

/// `unknownFlags: Map<string, boolean | string>` (args.ts:53).
pub enum FlagValue { Bool(bool), Str(String) }

pub struct Args {
    // Option<T> for every `?` field; bool fields stay Option<bool> ONLY where
    // tri-state matters: `projectTrustOverride` is Option<bool> (undefined /
    // true from --approve / false from --no-approve, args.ts:180-183) and
    // main.ts:605,627,631,645 branches on all three. Every other `?: boolean`
    // is only ever set to `true`, so `bool` (default false) is faithful.
    pub provider: Option<String>,
    pub model: Option<String>,
    pub api_key: Option<String>,
    pub system_prompt: Option<String>,
    pub append_system_prompt: Option<Vec<String>>,   // None vs Some(vec![]) is observable: `?? []` at :96
    pub thinking: Option<ThinkingLevel>,
    pub r#continue: bool,
    pub resume: bool,
    pub help: bool,
    pub version: bool,
    pub mode: Option<Mode>,
    pub name: Option<String>,
    pub no_session: bool,
    pub session: Option<String>,
    pub session_id: Option<String>,
    pub fork: Option<String>,
    pub session_dir: Option<String>,
    pub models: Option<Vec<String>>,
    pub tools: Option<Vec<String>>,
    pub exclude_tools: Option<Vec<String>>,
    pub no_tools: bool,
    pub no_builtin_tools: bool,
    pub extensions: Option<Vec<String>>,
    pub no_extensions: bool,
    pub print: bool,
    pub export: Option<String>,
    pub no_skills: bool,
    pub skills: Option<Vec<String>>,
    pub prompt_templates: Option<Vec<String>>,
    pub no_prompt_templates: bool,
    pub themes: Option<Vec<String>>,
    pub no_themes: bool,
    pub no_context_files: bool,
    pub list_models: ListModels,
    pub offline: bool,
    pub verbose: bool,
    pub project_trust_override: Option<bool>,
    pub messages: Vec<String>,
    pub file_args: Vec<String>,
    /// MUST preserve insertion order — iterated at core/agent-session-services.ts:99
    /// to apply extension flags and to build the `Unknown options: ...` list, whose
    /// order is therefore argv order.
    pub unknown_flags: IndexMap<String, FlagValue>,
    pub diagnostics: Vec<Diagnostic>,   // { kind: Warning|Error, message: String }
}
```

`Option<Vec<String>>` vs `Vec<String>` matters for `models`/`tools`/`excludeTools`/
`extensions`/`skills`/`promptTemplates`/`themes`: `main.ts:689` does
`parsed.models ?? settingsManager.getEnabledModels()` — an **empty** `--models ""` (which
yields `[""]`, §3.5) is not the same as absent, and `--tools ""` yields `Some(vec![])` which
disables all tools while absent keeps the defaults (`core/sdk.ts:241-246`).

`messages` is **mutated after parse**: `buildInitialMessage` does `parsed.messages.shift()`
(`cli/initial-message.ts:36`) so the first message is consumed into `initialMessage` and the
remainder is passed to print mode as `messages` (`main.ts:848`). Model this as an explicit
`take_first()` on a `&mut Args`, not as a copy.

### 3.3 The parse loop — exact branch order (`cli/args.ts:63-210`)

Single pass, `for (let i = 0; i < args.length; i++)`, `arg = args[i]`. First matching branch
wins; `args[++i]` is the "consume a value" idiom. Branches in source order:

| # | Guard | Effect |
|---|---|---|
| 1 | `arg === "--help" \|\| arg === "-h"` | `help = true` |
| 2 | `arg === "--version" \|\| arg === "-v"` | `version = true` |
| 3 | `arg === "--mode" && i+1 < len` | `mode = args[++i]` **only if** it is `"text"\|"json"\|"rpc"`; otherwise the value is consumed and silently discarded (no diagnostic) |
| 4 | `arg === "--continue" \|\| arg === "-c"` | `continue = true` |
| 5 | `arg === "--resume" \|\| arg === "-r"` | `resume = true` |
| 6 | `arg === "--provider" && i+1 < len` | `provider = args[++i]` |
| 7 | `arg === "--model" && i+1 < len` | `model = args[++i]` |
| 8 | `arg === "--api-key" && i+1 < len` | `apiKey = args[++i]` |
| 9 | `arg === "--system-prompt" && i+1 < len` | `systemPrompt = args[++i]` (last wins) |
| 10 | `arg === "--append-system-prompt" && i+1 < len` | push onto `appendSystemPrompt ?? []` |
| 11 | `arg === "--name" \|\| arg === "-n"` | if `i+1 < len` → `name = args[++i]`; **else** push error `"--name requires a value"` (the only value-taking flag with its own diagnostic) |
| 12 | `arg === "--no-session"` | `noSession = true` |
| 13 | `arg === "--session" && i+1 < len` | `session = args[++i]` |
| 14 | `arg === "--session-id" && i+1 < len` | `sessionId = args[++i]` |
| 15 | `arg === "--fork" && i+1 < len` | `fork = args[++i]` |
| 16 | `arg === "--session-dir" && i+1 < len` | `sessionDir = args[++i]` |
| 17 | `arg === "--models" && i+1 < len` | `split(",").map(trim)` — **no `.filter()`**, empty segments kept |
| 18 | `arg === "--no-tools" \|\| arg === "-nt"` | `noTools = true` |
| 19 | `arg === "--no-builtin-tools" \|\| arg === "-nbt"` | `noBuiltinTools = true` |
| 20 | `(arg === "--tools" \|\| arg === "-t") && i+1 < len` | `split(",").map(trim).filter(len>0)` |
| 21 | `(arg === "--exclude-tools" \|\| arg === "-xt") && i+1 < len` | `split(",").map(trim).filter(len>0)` |
| 22 | `arg === "--thinking" && i+1 < len` | value consumed; if valid → `thinking`; else **warning** (value still consumed) |
| 23 | `arg === "--print" \|\| arg === "-p"` | `print = true` + the `---` lookahead clause (below) |
| 24 | `arg === "--export" && i+1 < len` | `export = args[++i]` |
| 25 | `(arg === "--extension" \|\| arg === "-e") && i+1 < len` | push onto `extensions ?? []` |
| 26 | `arg === "--no-extensions" \|\| arg === "-ne"` | `noExtensions = true` |
| 27 | `arg === "--skill" && i+1 < len` | push onto `skills ?? []` |
| 28 | `arg === "--prompt-template" && i+1 < len` | push onto `promptTemplates ?? []` |
| 29 | `arg === "--theme" && i+1 < len` | push onto `themes ?? []` |
| 30 | `arg === "--no-skills" \|\| arg === "-ns"` | `noSkills = true` |
| 31 | `arg === "--no-prompt-templates" \|\| arg === "-np"` | `noPromptTemplates = true` |
| 32 | `arg === "--no-themes"` | `noThemes = true` (**no short alias**) |
| 33 | `arg === "--no-context-files" \|\| arg === "-nc"` | `noContextFiles = true` |
| 34 | `arg === "--list-models"` | optional-value lookahead (below) |
| 35 | `arg === "--verbose"` | `verbose = true` |
| 36 | `arg === "--approve" \|\| arg === "-a"` | `projectTrustOverride = true` |
| 37 | `arg === "--no-approve" \|\| arg === "-na"` | `projectTrustOverride = false` |
| 38 | `arg === "--offline"` | `offline = true` |
| — | **four tail branches, in this order:** | |
| T1 | `arg.startsWith("@")` | `fileArgs.push(arg.slice(1))` |
| T2 | `arg.startsWith("--")` | the unknown-long-flag algorithm (§3.4) |
| T3 | `arg.startsWith("-") && !arg.startsWith("--")` | error `` `Unknown option: ${arg}` `` |
| T4 | `!arg.startsWith("-")` | `messages.push(arg)` |

Tail-branch order is load-bearing: T1 before T2 means `@--foo` is a file arg; T2 before T3
means only single-dash tokens reach the "Unknown option" error; T4's redundant re-test of
`!startsWith("-")` means a bare `-` matches T3, not T4 (`-` starts with `-` and not `--`) and
so produces `Unknown option: -`.

**`-p` / `--print` lookahead (`:140-146`), quoted verbatim:**

```ts
} else if (arg === "--print" || arg === "-p") {
	result.print = true;
	const next = args[i + 1];
	if (next !== undefined && !next.startsWith("@") && (!next.startsWith("-") || next.startsWith("---"))) {
		result.messages.push(next);
		i++;
	}
}
```

So `-p` greedily swallows the next token as a message unless it is `undefined`, starts with
`@`, or starts with `-` — **except** that a token starting with `---` (three dashes) IS
swallowed. `pi -p ---x` → `messages == ["---x"]`.

**`--list-models` lookahead (`:171-177`), quoted verbatim:**

```ts
} else if (arg === "--list-models") {
	// Check if next arg is a search pattern (not a flag or file arg)
	if (i + 1 < args.length && !args[i + 1].startsWith("-") && !args[i + 1].startsWith("@")) {
		result.listModels = args[++i];
	} else {
		result.listModels = true;
	}
}
```

Note there is **no** `---` escape hatch here, unlike `-p`.

### 3.4 The unknown-long-flag algorithm (`:188-201`), quoted verbatim

```ts
} else if (arg.startsWith("--")) {
	const eqIndex = arg.indexOf("=");
	if (eqIndex !== -1) {
		result.unknownFlags.set(arg.slice(2, eqIndex), arg.slice(eqIndex + 1));
	} else {
		const flagName = arg.slice(2);
		const next = args[i + 1];
		if (next !== undefined && !next.startsWith("-") && !next.startsWith("@")) {
			result.unknownFlags.set(flagName, next);
			i++;
		} else {
			result.unknownFlags.set(flagName, true);
		}
	}
}
```

Consequences (all confirmed by reading, all must be reproduced):

- `indexOf("=")` searches the **whole token including the leading `--`**, so `--=v` gives
  key `arg.slice(2,2)` = `""` and value `"v"`. `--a=b=c` gives key `"a"`, value `"b=c"`.
- `--` (bare) has `eqIndex === -1`, `flagName === ""`, and then eats the next token if it is
  not a flag/file: `pi -- "hello"` → `unknownFlags == {"" => "hello"}` and `messages == []`.
  There is **no** end-of-options handling anywhere in `parseArgs`.
- An unknown flag greedily consumes the following token, so `pi --foo bar` gives
  `{"foo" => "bar"}` and `messages == []`; `pi --foo -- bar` gives `{"foo" => true,
  "" => "bar"}`.
- Later `set()` on the same key overwrites the value but keeps the **original** insertion
  position (JS `Map` semantics) — `IndexMap::insert` matches this exactly.

### 3.5 Complete flag table

`takes` = consumes the following argv token. `guard` = the `i+1 < args.length` requirement;
when it fails the token falls through to the tail branches (column "if value missing").

| Flag | Aliases | Takes | Target field | Default | Parse-time validation | If value missing |
|---|---|---|---|---|---|---|
| `--help` | `-h` | no | `help` | `undefined` | — | — |
| `--version` | `-v` | no | `version` | `undefined` | — | — |
| `--mode` | — | yes | `mode` | `undefined` | must be `text\|json\|rpc`, else value dropped **silently** | `unknownFlags["mode"]=true` |
| `--continue` | `-c` | no | `continue` | `undefined` | — | — |
| `--resume` | `-r` | no | `resume` | `undefined` | — | — |
| `--provider` | — | yes | `provider` | `undefined` | none (validated later, §9.4) | `unknownFlags["provider"]=true` |
| `--model` | — | yes | `model` | `undefined` | none (resolved later) | `unknownFlags["model"]=true` |
| `--api-key` | — | yes | `apiKey` | `undefined` | none | `unknownFlags["api-key"]=true` |
| `--system-prompt` | — | yes | `systemPrompt` | `undefined` | none; last occurrence wins | `unknownFlags["system-prompt"]=true` |
| `--append-system-prompt` | — | yes | `appendSystemPrompt[]` | `undefined` | none; repeatable, order preserved | `unknownFlags[...]=true` |
| `--name` | `-n` | yes | `name` | `undefined` | non-empty checked **later** (`main.ts:592-598`) | **error** `--name requires a value` |
| `--no-session` | — | no | `noSession` | `undefined` | — | — |
| `--session` | — | yes | `session` | `undefined` | none (resolved later) | `unknownFlags["session"]=true` |
| `--session-id` | — | yes | `sessionId` | `undefined` | charset checked later (`main.ts:236`) | `unknownFlags["session-id"]=true` |
| `--fork` | — | yes | `fork` | `undefined` | conflicts checked later (`main.ts:205-219`) | `unknownFlags["fork"]=true` |
| `--session-dir` | — | yes | `sessionDir` | `undefined` | none; tilde-expanded later (`main.ts:575`) | `unknownFlags["session-dir"]=true` |
| `--models` | — | yes | `models` | `undefined` | split `,` + trim, **empty segments KEPT** | `unknownFlags["models"]=true` |
| `--no-tools` | `-nt` | no | `noTools` | `undefined` | — | — |
| `--no-builtin-tools` | `-nbt` | no | `noBuiltinTools` | `undefined` | — | — |
| `--tools` | `-t` | yes | `tools` | `undefined` | split `,` + trim + **filter empties** | `--tools`→unknown flag; `-t`→**error** `Unknown option: -t` |
| `--exclude-tools` | `-xt` | yes | `excludeTools` | `undefined` | split `,` + trim + filter empties | `--exclude-tools`→unknown flag; `-xt`→**error** `Unknown option: -xt` |
| `--thinking` | — | yes | `thinking` | `undefined` | must be in `VALID_THINKING_LEVELS`, else **warning** and field stays unset | `unknownFlags["thinking"]=true` |
| `--print` | `-p` | optional | `print` + `messages[0]` | `undefined` | `---` lookahead clause (§3.3) | n/a (never errors) |
| `--export` | — | yes | `export` | `undefined` | none | `unknownFlags["export"]=true` |
| `--extension` | `-e` | yes | `extensions[]` | `undefined` | repeatable | `--extension`→unknown flag; `-e`→**error** `Unknown option: -e` |
| `--no-extensions` | `-ne` | no | `noExtensions` | `undefined` | — | — |
| `--skill` | — | yes | `skills[]` | `undefined` | repeatable | `unknownFlags["skill"]=true` |
| `--prompt-template` | — | yes | `promptTemplates[]` | `undefined` | repeatable | `unknownFlags[...]=true` |
| `--theme` | — | yes | `themes[]` | `undefined` | repeatable | `unknownFlags["theme"]=true` |
| `--no-skills` | `-ns` | no | `noSkills` | `undefined` | — | — |
| `--no-prompt-templates` | `-np` | no | `noPromptTemplates` | `undefined` | — | — |
| `--no-themes` | *(none)* | no | `noThemes` | `undefined` | — | — |
| `--no-context-files` | `-nc` | no | `noContextFiles` | `undefined` | — | — |
| `--list-models` | — | optional | `listModels` | `undefined` | lookahead: next token not starting `-`/`@` | → `true` |
| `--verbose` | — | no | `verbose` | `undefined` | — | — |
| `--approve` | `-a` | no | `projectTrustOverride = true` | `undefined` | — | — |
| `--no-approve` | `-na` | no | `projectTrustOverride = false` | `undefined` | — | — |
| `--offline` | — | no | `offline` | `undefined` | also pre-scanned raw at `main.ts:476` | — |

**Two flags are read from raw argv before `parseArgs` runs:** `--offline`
(`args.includes("--offline")`, `main.ts:476`) and the package-command verbs
(`handlePackageCommand(args, ...)` / `handleConfigCommand(args, ...)`, `main.ts:492-507`,
not ported). `rpc-entry.ts:12` prepends `["--mode","rpc"]` (feat-012).

### 3.6 Diagnostics, error strings and exit codes

Parse-time diagnostics (`Args.diagnostics`), quoted:

| Kind | String |
|---|---|
| error | `--name requires a value` |
| warning | `` `Invalid thinking level "${level}". Valid values: ${VALID_THINKING_LEVELS.join(", ")}` `` → e.g. `Invalid thinking level "hard". Valid values: off, minimal, low, medium, high, xhigh, max` |
| error | `` `Unknown option: ${arg}` `` |

Rendering (`main.ts:510-518`): each diagnostic goes to **stderr** as
`` `${d.type === "error" ? "Error" : "Warning"}: ${d.message}` `` (errors red, warnings
yellow); after printing all of them, **if any is an error → `process.exit(1)`**.

Runtime diagnostics use a different renderer, `reportDiagnostics` (`main.ts:87-93`) — also
stderr, prefix `"Error: "` / `"Warning: "` / `""` for `info`.

Every remaining diagnostic/error string in the ported bootstrap, with its stream, in
source order:

| Where | Stream | String (verbatim) | Exit |
|---|---|---|---|
| `main.ts:522` | stdout | `VERSION` | 0 |
| `main.ts:533` | stderr | `` `Error: ${message}` `` (export failure; fallback message `Failed to export session`) | 1 |
| `main.ts:536` | stdout | `` `Exported to: ${result}` `` | 0 |
| `main.ts:547` | stderr | `Error: @file arguments are not supported in RPC mode` | 1 |
| `main.ts:216` | stderr | `` `Error: --fork cannot be combined with ${conflictingFlags.join(", ")}` `` (candidates, in order: `--session`, `--continue`, `--resume`, `--no-session`) | 1 |
| `main.ts:231` | stderr | `` `Error: --session-id cannot be combined with ${conflictingFlags.join(", ")}` `` (candidates: `--session`, `--continue`, `--resume`) | 1 |
| `main.ts:239` | stderr | `` `Error: ${message}` `` where message = `Session id must be non-empty, contain only alphanumeric characters, '-', '_', and '.', and start and end with an alphanumeric character` (`core/session-manager.ts:210-212`) | 1 |
| `main.ts:278` | stderr | `` `Session already exists with id '${parsed.sessionId}'` `` | 1 |
| `main.ts:292`, `:316` | stderr | `` `No session found matching '${resolved.arg}'` `` | 1 |
| `main.ts:306` | stdout | `` `Session found in different project: ${resolved.cwd}` `` | — |
| `main.ts:307` | stdout | `Fork this session into current directory? [y/N] ` (prompt; §16) | — |
| `main.ts:309` | stdout | `Aborted.` | 0 |
| `main.ts:329` | stdout | `No session selected` | 0 |
| `main.ts:249`, `:259` | stderr | `` `Error: ${message}` `` from `SessionManager.open`/`forkFrom`; messages: `` `Session file is not a valid pi session: ${path}` `` (`session-manager.ts:836`), `` `Cannot fork: source session file is empty or invalid: ${path}` `` (`:1500`), `` `Cannot fork: source session has no header: ${path}` `` (`:1505`) | 1 |
| `main.ts:349` | stderr | `` `Warning: No project session found with id '${parsed.sessionId}'; creating a new session with that id.` `` | — |
| `main.ts:588` | stderr | `` `Stored session working directory does not exist: ${sessionCwd}\nSession file: ${sessionFile}\nCurrent working directory: ${fallbackCwd}` `` (`core/session-cwd.ts:35-38`) | 1 |
| `main.ts:595` | stderr | `Error: --name requires a non-empty value` | 1 |
| `main.ts:709` | stderr (via diagnostics) | `--api-key requires a model to be specified via --model, --provider/--model, or --models` | 1 |
| `core/agent-session-services.ts:115` | stderr (diagnostic) | `` `Extension flag "--${name}" requires a value` `` | 1 |
| `core/agent-session-services.ts:122` | stderr (diagnostic) | `` `Unknown option${n===1?"":"s"}: ${names.map(n=>`--${n}`).join(", ")}` `` — **this is where unknown flags finally become fatal** | 1 |
| `main.ts:685` | stderr (diagnostic) | `` `Failed to load extension "${path}": ${error}` `` | 1 |
| `main.ts:794` | stderr | `Hint: Start without extensions using "pi -ne".` (only when some error message contains `Failed to load extension`) | 1 |
| `main.ts:801` | stderr | `` `No models available. Use /login to log into a provider via OAuth or API key. See:\n  ${docs}/providers.md\n  ${docs}/models.md` `` (`core/auth-guidance.ts:6-16`) — only when `appMode !== "interactive"` and no model | 1 |
| `main.ts:807` | stderr | `Error: PI_STARTUP_BENCHMARK only supports interactive mode` | 1 |
| `core/model-resolver.ts:337` | stderr | `` `Warning: ${diagnostic.message}` `` where message = `` `No models match pattern "${pattern}"` `` or `` `Invalid thinking level "${suffix}" in pattern "${pattern}". Using default instead.` `` | — |
| `core/model-resolver.ts:383` | stderr | the `resolveCliModel` error, then `exit(1)` (only from `findInitialModel`) | 1 |
| `cli/list-models.ts:32` | stderr | `` `Warning: errors loading models.json:\n${loadError}` `` | — |
| `cli/list-models.ts:49` | stdout | `` `No models matching "${searchPattern}"` `` | 0 |
| `cli/file-processor.ts:37` | stderr | `` `Error: File not found: ${absolutePath}` `` | 1 |
| `cli/file-processor.ts:80` | stderr | `` `Error: Could not read file ${absolutePath}: ${message}` `` | 1 |
| `modes/print-mode.ts:136` | stderr | `` assistantMsg.errorMessage || `Request ${assistantMsg.stopReason}` `` | exitCode 1 |
| `modes/print-mode.ts:150` | stderr | the thrown error's `message` | 1 |
| `modes/print-mode.ts:57` | — | signal handler exit | 143 (SIGTERM) / 129 (SIGHUP) |

Exit-code summary: **0** = `--version`, `--help`, `--list-models`, `--export` success,
`Aborted.`, `No session selected`; **1** = every error above; **143/129** = signals in print
mode; print mode's non-zero result is assigned to `process.exitCode` (not `process.exit`) so
teardown completes (`main.ts:854-856`).

### 3.7 `resolveAppMode` and `takeOverStdout` gating (`main.ts:100-119,540-544`)

```ts
function resolveAppMode(parsed: Args, stdinIsTTY: boolean, stdoutIsTTY: boolean): AppMode {
	if (parsed.mode === "rpc") return "rpc";
	if (parsed.mode === "json") return "json";
	if (parsed.print || !stdinIsTTY || !stdoutIsTTY) return "print";
	return "interactive";
}
function toPrintOutputMode(appMode: AppMode): Exclude<Mode, "rpc"> {
	return appMode === "json" ? "json" : "text";
}
function isPlainRuntimeMetadataCommand(parsed: Args): boolean {
	return !parsed.print && parsed.mode === undefined && (parsed.help === true || parsed.listModels !== undefined);
}
```

`AppMode = "interactive" | "print" | "json" | "rpc"` (`core/project-trust.ts:12`).
`--mode text` is **not** matched by any branch, so it falls through to the TTY logic exactly
like no `--mode` at all. `stdinIsTTY`/`stdoutIsTTY` are `process.stdin.isTTY` /
`process.stdout.isTTY`, which are `undefined` (falsy) when redirected — Rust:
`std::io::stdin().is_terminal()`.

Then:

```ts
let appMode = resolveAppMode(parsed, process.stdin.isTTY, process.stdout.isTTY);
const shouldTakeOverStdout = appMode !== "interactive" && !isPlainRuntimeMetadataCommand(parsed);
if (shouldTakeOverStdout) {
	takeOverStdout();
}
```

The `--help`/`--list-models` exemption is conditional on `!parsed.print && parsed.mode ===
undefined`, so `pi -p --help` and `pi --mode json --help` **do** take over stdout (their help
text lands on stderr). This is the runtime behaviour; reproduce it.

---

## 4. `printHelp` (`cli/args.ts:212-390`)

### 4.1 Structure

One `console.log` of one template literal. Line 223 → line 389, i.e. **167 lines of body plus
a trailing newline from the literal's last line, plus the newline `console.log` appends**.
Sections in order: title line, `Usage:`, `Commands:`, `Options:`, the "Extensions can register
additional flags" line + `extensionFlagsText`, `Examples:`, `Environment Variables:`,
`Built-in Tool Names:`.

`chalk.bold(...)` appears **7 times** in the body — the title's app name plus the six section
headings `Usage:`, `Commands:`, `Options:`, `Examples:`, `Environment Variables:`,
`Built-in Tool Names:` (an 8th lives in `extensionFlagsText` at `:215`). When stdout is
not a TTY chalk emits no escapes, so the golden capture is plain text — **capture with stdout
redirected**, and set `FORCE_COLOR=0` to be safe.

### 4.2 Substitution points (the ONLY dynamic parts)

| Placeholder | Value in Pi | Occurrences |
|---|---|---|
| `APP_NAME` (`config.ts:489`) | `pi` | **27** (title, `Usage:` line, 7 `Commands:` lines, 18 `Examples:` lines) |
| `CONFIG_DIR_NAME` (`config.ts:491`) | `.pi` | 2 (`--export` example path `~/${CONFIG_DIR_NAME}/agent/sessions/...`, and `ENV_AGENT_DIR`'s description `(default: ~/${CONFIG_DIR_NAME}/agent)`) |
| `ENV_AGENT_DIR` (`config.ts:495`) | `PI_CODING_AGENT_DIR` | 1, rendered `` `  ${ENV_AGENT_DIR.padEnd(32)} - Config directory (default: ~/${CONFIG_DIR_NAME}/agent)` `` |
| `ENV_SESSION_DIR` (`config.ts:496`) | `PI_CODING_AGENT_SESSION_DIR` | 1, `` `  ${ENV_SESSION_DIR.padEnd(32)} - Session storage directory (overridden by --session-dir)` `` |
| `extensionFlagsText` | `""` when no extension flags | 1 |
| `VERSION` | not in the help text | — (it is `--version`'s output, `main.ts:522`) |

`.padEnd(32)` is JS UTF-16-unit padding; both identifiers are ASCII so Rust
`format!("{:<32}", s)` is byte-identical. Note `PI_CODING_AGENT_SESSION_DIR` is 27 chars and
`PI_CODING_AGENT_DIR` is 19, so both pad; `PIRUST_CODING_AGENT_SESSION_DIR` is **31** chars
(still pads to 32) and `PIRUST_CODING_AGENT_DIR` is 23 — pirust's rendering stays aligned, but
the golden test must substitute Pi's identity to compare.

`extensionFlagsText` (`cli/args.ts:213-222`), for when feat-007 lands:

```ts
const extensionFlagsText =
	extensionFlags && extensionFlags.length > 0
		? `\n${chalk.bold("Extension CLI Flags:")}\n${extensionFlags
				.map((flag) => {
					const value = flag.type === "string" ? " <value>" : "";
					const description = flag.description ?? `Registered by ${flag.extensionPath}`;
					return `  --${flag.name}${value}`.padEnd(30) + description;
				})
				.join("\n")}\n`
		: "";
```

### 4.3 Rust shape + golden test

```rust
pub struct AppIdentity {
    pub app_name: &'static str,        // config.ts:489
    pub config_dir_name: &'static str, // config.ts:491
    pub env_agent_dir: &'static str,   // config.ts:495
    pub env_session_dir: &'static str, // config.ts:496
    pub version: &'static str,         // config.ts:492
}
pub const PIRUST: AppIdentity = AppIdentity {
    app_name: "pirust", config_dir_name: ".pirust",
    env_agent_dir: "PIRUST_CODING_AGENT_DIR",
    env_session_dir: "PIRUST_CODING_AGENT_SESSION_DIR",
    version: env!("CARGO_PKG_VERSION"),
};
#[cfg(test)]
pub const PI: AppIdentity = AppIdentity {
    app_name: "pi", config_dir_name: ".pi",
    env_agent_dir: "PI_CODING_AGENT_DIR",
    env_session_dir: "PI_CODING_AGENT_SESSION_DIR",
    version: "0.0.0",
};

/// Renders the help body EXACTLY as `printHelp` does, minus chalk escapes.
pub fn render_help(id: &AppIdentity, extension_flags: &[ExtensionFlag]) -> String;
```

Production calls `render_help(&PIRUST, &[])`; the golden test calls
`render_help(&PI, &[])` and asserts equality with
`tests/fixtures/pi/cli/help.plain.golden`.

> **This fixture does not exist.** `tests/fixtures/pi/` currently contains only `agent/`,
> `anthropic/`, `tools/{schemas,strings}` — there is no `cli/` directory. Capturing it is a
> prerequisite task for feat-005:
> ```bash
> cd pi_space/pi/packages/coding-agent
> FORCE_COLOR=0 npx tsx src/cli.ts --help > <pirust>/tests/fixtures/pi/cli/help.plain.golden
> ```
> `--help` exits 0 and (because stdout is redirected) `resolveAppMode` returns `print`, but
> `isPlainRuntimeMetadataCommand` is true (no `-p`, no `--mode`) so stdout is **not** taken
> over and the help text lands on real stdout. Do **not** paste the help text into this spec —
> the fixture is the single source of truth. The one thing an implementer must know without
> looking: the body is 167 lines (`cli/args.ts:223-389`), its last content line is
> `  ls     - List directory contents (read-only, off by default)`, and because the template
> literal ends with a newline **and** `console.log` appends one, the output ends with a blank
> line. Every `Options:` / `Environment Variables:` / `Built-in Tool Names:` entry is a
> 2-space indent with the description starting at **column 33** (0-indexed), achieved with
> hand-written spaces, not `padEnd` (the only `padEnd`s are the two env-var names and the
> extension-flag lines) — so **copy the literal byte-for-byte from `cli/args.ts:223-389`; do
> not re-format it and do not re-derive the alignment.**

---

## 5. `config.ts` — paths and app identity

### 5.1 App identity (`config.ts:470-496`)

Pi reads its own `package.json` once at module load (`config.ts:479-485`; `ENOENT` is
tolerated, any other error is rethrown) and derives:

```ts
const piConfigName: string | undefined = pkg.piConfig?.name;
export const PACKAGE_NAME: string = pkg.name || "@earendil-works/pi-coding-agent";
export const APP_NAME: string = piConfigName || "pi";
export const APP_TITLE: string = piConfigName ? APP_NAME : "π";
export const CONFIG_DIR_NAME: string = pkg.piConfig?.configDir || ".pi";
export const VERSION: string = pkg.version || "0.0.0";
export const ENV_AGENT_DIR = `${APP_NAME.toUpperCase()}_CODING_AGENT_DIR`;
export const ENV_SESSION_DIR = `${APP_NAME.toUpperCase()}_CODING_AGENT_SESSION_DIR`;
```

**pirust divergence (intentional, explicit).** The `package.json`-derived rebranding seam is
dropped; identity is the compile-time `AppIdentity` of §4.3:

| Pi | pirust |
|---|---|
| `~/.pi/agent` | **`~/.pirust/agent`** |
| `PI_CODING_AGENT_DIR` | **`PIRUST_CODING_AGENT_DIR`** |
| `PI_CODING_AGENT_SESSION_DIR` | **`PIRUST_CODING_AGENT_SESSION_DIR`** |
| `PI_OFFLINE` | **`PIRUST_OFFLINE`** |
| `process.title = APP_NAME` (`cli.ts:12`) | not set (no portable equivalent; drop) |
| `process.env.PI_CODING_AGENT = "true"` (`cli.ts:13`) | not set (only read by Pi extensions) |

The three constants **already exist** at `crates/pirust-tools/src/binaries.rs:154-167`
(`CONFIG_DIR_NAME`, `ENV_AGENT_DIR`, `OFFLINE_ENV`) together with `AGENT_DIR_NAME = "agent"`
and `BIN_DIR_NAME = "bin"` (`:171-175`). **`config.rs` must re-export those, not redeclare
them** — two sources of truth for `~/.pirust` is a defect. `PIRUST_OFFLINE`'s other spellings
(`PI_SKIP_VERSION_CHECK`, `PI_PACKAGE_DIR`, `PI_STARTUP_BENCHMARK`, `PI_TELEMETRY`,
`PI_SHARE_VIEWER_URL`, `PI_CLEAR_ON_SHRINK`, `PI_HARDWARE_CURSOR`) belong to unported
features; do not introduce `PIRUST_` analogues for them in feat-005.

**File FORMATS are unchanged.** `settings.json`, `auth.json`, `models.json`,
`models-store.json`, `trust.json`, `keybindings.json` and the session `.jsonl` files keep
Pi's exact schema, key order and `JSON.stringify(x, null, 2)` formatting. Only the root
directory differs.

### 5.2 Path resolution table (`config.ts:498-567`)

| Function | Result | Line |
|---|---|---|
| `expandTildePath(path)` | `normalizePath(path)` (tilde expansion, see §5.3) | 498-500 |
| `getAgentDir()` | `process.env[ENV_AGENT_DIR]` → `expandTildePath(it)`; else `join(homedir(), CONFIG_DIR_NAME, "agent")` | 515-521 |
| `getCustomThemesDir()` | `join(getAgentDir(), "themes")` | 524-526 |
| `getModelsPath()` | `join(getAgentDir(), "models.json")` | 529-531 |
| `getAuthPath()` | `join(getAgentDir(), "auth.json")` | 534-536 |
| `getSettingsPath()` | `join(getAgentDir(), "settings.json")` | 539-541 |
| `getToolsDir()` | `join(getAgentDir(), "tools")` | 544-546 |
| `getBinDir()` | `join(getAgentDir(), "bin")` | 549-551 |
| `getPromptsDir()` | `join(getAgentDir(), "prompts")` | 554-556 |
| `getSessionsDir()` | `join(getAgentDir(), "sessions")` | 559-561 |
| `getDebugLogPath()` | `` join(getAgentDir(), `${APP_NAME}-debug.log`) `` | 564-566 |
| project settings | `join(resolvePath(cwd), CONFIG_DIR_NAME, "settings.json")` | `core/settings-manager.ts:196` |
| trust store | `join(resolvePath(agentDir), "trust.json")` | `core/trust-manager.ts:212` |
| models store | `join(dirname(modelsPath), "models-store.json")` | `core/model-runtime.ts:139` |

`getAgentDir()` is **re-read from the environment on every call** (no caching), so tests can
set `PIRUST_CODING_AGENT_DIR` per-case. Not ported: `getPackageDir`/`getThemesDir`/
`getExportTemplateDir`/`getInteractiveAssetsDir`/`getReadmePath`/`getDocsPath`/
`getExamplesPath`/`getChangelogPath` (`config.ts:357-464`) — except that `getReadmePath`,
`getDocsPath` and `getExamplesPath` are interpolated into the default system prompt
(`core/system-prompt.ts:75-77`) and `getDocsPath` into the no-models message
(`core/auth-guidance.ts:9-10`); §14 specifies what pirust substitutes there.

### 5.3 Tilde expansion — `normalizePath` (`utils/paths.ts:57-79`)

```ts
export function normalizePath(input: string, options: PathInputOptions = {}): string {
	let normalized = options.trim ? input.trim() : input;
	if (options.normalizeUnicodeSpaces) { normalized = normalized.replace(UNICODE_SPACES, " "); }
	if (options.stripAtPrefix && normalized.startsWith("@")) { normalized = normalized.slice(1); }
	if (options.expandTilde ?? true) {
		const home = options.homeDir ?? homedir();
		if (normalized === "~") return home;
		if (normalized.startsWith("~/") || (process.platform === "win32" && normalized.startsWith("~\\"))) {
			return join(home, normalized.slice(2));
		}
	}
	if (/^file:\/\//.test(normalized)) { return fileURLToPath(normalized); }
	return normalized;
}
```

Exact semantics to reproduce:
- Only a **leading** `~` expands, and only as the whole string or followed by `/` (plus `\` on
  win32). `~user/x` does **not** expand. `a/~/b` does not expand.
- `join(home, normalized.slice(2))` — the separator comes from `path.join`, so on Windows
  `~/x` → `C:\Users\me\x`.
- Tilde expansion happens **before** the `file://` check, so `file://` URLs are converted
  after. `fileURLToPath` failures throw (Pi does not catch here).
- `resolvePath(input, baseDir = process.cwd())` (`:81-85`) = normalize both, then
  `isAbsolute(normalized) ? resolve(normalized) : resolve(normalizedBaseDir, normalized)`.
  Note it normalizes `baseDir` too (so a `~`-prefixed base works).
- `homedir()` is libuv's: `$HOME` on POSIX, `%USERPROFILE%` on Windows (already encoded as
  `HOME_ENV_POSIX`/`HOME_ENV_WINDOWS` at `crates/pirust-tools/src/binaries.rs:178-182`).

`--session-dir` is normalized with `normalizePath` (`main.ts:575`) while `ENV_SESSION_DIR` is
normalized with `expandTildePath` (`main.ts:576`) — **the same function**, so both tilde-expand
identically. The session-dir precedence (`main.ts:573-577`):

```ts
const envSessionDir = process.env[ENV_SESSION_DIR];
const sessionDir =
	(parsed.sessionDir ? normalizePath(parsed.sessionDir) : undefined) ??
	(envSessionDir ? expandTildePath(envSessionDir) : undefined) ??
	startupSettingsManager.getSessionDir();
```

i.e. `--session-dir` > `$PIRUST_CODING_AGENT_SESSION_DIR` > `settings.sessionDir`
(also tilde-expanded, `core/settings-manager.ts:670-673`) > `undefined` (⇒ the default
encoded per-cwd directory, §7.7). Because the guards are truthiness tests, an **empty
string** in any of the three is skipped, not honoured.

---

## 6. `core/settings-manager.ts` — `Settings`, layering, merge

### 6.1 `Settings` field list (`:83-129`) — 45 fields, all optional

Order below is declaration order, which is also the JSON key order for a freshly written
in-memory settings object (`JSON.stringify` preserves literal key order). **On-disk key order
is the FILE's existing order** though, because `persistScopedSettings` starts from
`{...currentFileSettings}` and only overwrites modified fields (`:588-602`) — new keys append
in modification order. Rust: use an order-preserving representation (`IndexMap`-backed
`serde_json::Map` with the `preserve_order` feature, or a struct plus an `extra: Map` for
unknown keys) so round-tripping a user's file does not reorder it.

| Field | Type | Default (from its getter) | Getter |
|---|---|---|---|
| `lastChangelogVersion` | `string` | `undefined` | `:660` |
| `defaultProvider` | `string` | `undefined` | `:675` |
| `defaultModel` | `string` | `undefined` | `:679` |
| `defaultThinkingLevel` | `ThinkingLevel` | `undefined` (callers fall back to `"medium"` = `DEFAULT_THINKING_LEVEL`, `core/defaults.ts:3`) | `:740` |
| `transport` | `Transport` | `"auto"` | `:750-752` |
| `steeringMode` | `"all" \| "one-at-a-time"` | `"one-at-a-time"` (via `\|\|`, so `""` also falls back) | `:703-705` |
| `followUpMode` | `"all" \| "one-at-a-time"` | `"one-at-a-time"` (via `\|\|`) | `:713-715` |
| `theme` | `string` | `undefined`; `getTheme()` returns `undefined` when the value contains `/` | `:723-732` |
| `compaction` | `CompactionSettings` | see below | `:760-787` |
| `branchSummary` | `BranchSummarySettings` | see below | `:789-798` |
| `retry` | `RetrySettings` | see below | `:800-840` |
| `hideThinkingBlock` | `boolean` | `false` | `:846` |
| `showCacheMissNotices` | `boolean` | `false` | `:850` |
| `externalEditor` | `string` | `VISUAL` \|\| `EDITOR` \|\| (`win32 ? "notepad" : "nano"`); a whitespace-only setting is ignored | `:854-864` |
| `shellPath` | `string` | `undefined`; tilde-expanded when present | `:878-881` |
| `quietStartup` | `boolean` | `false` | `:889` |
| `defaultProjectTrust` | `"ask"\|"always"\|"never"` | `"ask"`; **read from `globalSettings` only, never the merged view** | `:899-902` |
| `shellCommandPrefix` | `string` | `undefined` | `:910` |
| `npmCommand` | `string[]` | `undefined` (cloned on read) | `:920-922` |
| `collapseChangelog` | `boolean` | `false` | `:930` |
| `enableInstallTelemetry` | `boolean` | `true` | `:940` |
| `enableAnalytics` | `boolean` | `false` | `:950` |
| `trackingId` | `string` | `undefined` | `:954` |
| `packages` | `PackageSource[]` | `[]` (cloned) | `:969-971` |
| `extensions` | `string[]` | `[]` (cloned) | `:985-987` |
| `skills` | `string[]` | `[]` (cloned) | `:1001-1003` |
| `prompts` | `string[]` | `[]` (cloned) | `:1017-1019` |
| `themes` | `string[]` | `[]` (cloned) | `:1033-1035` |
| `enableSkillCommands` | `boolean` | `true` | `:1049` |
| `terminal` | `TerminalSettings` | see below | `:1063-1121` |
| `images` | `ImageSettings` | see below | `:1123-1147` |
| `enabledModels` | `string[]` | `undefined` (**not** `[]` — absence is meaningful at `main.ts:689`) | `:1149` |
| `doubleEscapeAction` | `"fork"\|"tree"\|"none"` | `"tree"` | `:1159` |
| `treeFilterMode` | `"default"\|"no-tools"\|"user-only"\|"labeled-only"\|"all"` | `"default"` (invalid values coerce to `"default"`) | `:1169-1173` |
| `thinkingBudgets` | `ThinkingBudgetsSettings` | `undefined` (passed straight through) | `:1059` |
| `editorPaddingX` | `number` | `0`; setter clamps to `[0,3]` | `:1191` |
| `outputPad` | `0 \| 1` | `1` (only an exact `0` yields 0) | `:1201` |
| `autocompleteMaxVisible` | `number` | `5`; setter clamps to `[3,20]` | `:1211` |
| `showHardwareCursor` | `boolean` | `process.env.PI_CLEAR_ON_SHRINK`-style env fallback: `PI_HARDWARE_CURSOR === "1"` | `:1181` |
| `markdown` | `MarkdownSettings` | see below | `:1221` |
| `warnings` | `WarningSettings` | `{}` (shallow-cloned) | `:1225` |
| `sessionDir` | `string` | `undefined`; tilde-expanded when present | `:670-673` |
| `httpProxy` | `string` | `undefined` (unported) | — |
| `httpIdleTimeoutMs` | `number` | `DEFAULT_HTTP_IDLE_TIMEOUT_MS`; throws `` `Invalid httpIdleTimeoutMs setting: ${v}` `` on an unparseable value | `:821-823` |
| `websocketConnectTimeoutMs` | `number` | `undefined`; same throw with its own name | `:842-844` |

Nested structs (`:11-60`), with the defaults their getters apply:

```ts
interface CompactionSettings   { enabled?: boolean /*true*/; reserveTokens?: number /*16384*/; keepRecentTokens?: number /*20000*/ }
interface BranchSummarySettings{ reserveTokens?: number /*16384*/; skipPrompt?: boolean /*false*/ }
interface ProviderRetrySettings{ timeoutMs?: number; maxRetries?: number; maxRetryDelayMs?: number /*60000*/ }
interface RetrySettings        { enabled?: boolean /*true*/; maxRetries?: number /*3*/; baseDelayMs?: number /*2000*/; provider?: ProviderRetrySettings }
interface TerminalSettings     { showImages?: boolean /*true*/; imageWidthCells?: number /*60, max(1,floor(v)), non-finite→60*/;
                                 clearOnShrink?: boolean /*undefined→env PI_CLEAR_ON_SHRINK==="1"*/; showTerminalProgress?: boolean /*false*/ }
interface ImageSettings        { autoResize?: boolean /*true*/; blockImages?: boolean /*false*/ }
interface ThinkingBudgetsSettings { minimal?: number; low?: number; medium?: number; high?: number }   // no defaults
interface MarkdownSettings     { codeBlockIndent?: string /*"  "* / }
interface WarningSettings      { anthropicExtraUsage?: boolean /*true*/ }
type DefaultProjectTrust = "ask" | "always" | "never";
type PackageSource = string | { source: string; autoload?: boolean; extensions?: string[]; skills?: string[]; prompts?: string[]; themes?: string[] };
```

Only **eight** getters are on the headless path and must be correct in feat-005:
`getSessionDir`, `getDefaultProvider`, `getDefaultModel`, `getDefaultThinkingLevel`,
`getEnabledModels`, `getImageAutoResize`, `getBlockImages`, `getDefaultProjectTrust`; plus
`getSteeringMode`, `getFollowUpMode`, `getTransport`, `getThinkingBudgets`,
`getProviderRetrySettings`, `getHttpIdleTimeoutMs`, `getWebSocketConnectTimeoutMs` which are
read by `sdk.ts` (§10.2), and `getCompactionSettings`/`getBranchSummarySettings` read by
`AgentSession`. All 45 fields must still **parse and round-trip** losslessly.

### 6.2 Layering

Two scopes only (`SettingsScope = "global" | "project"`, `:173`):

- global = `<agentDir>/settings.json` (`:195`)
- project = `<resolvedCwd>/<CONFIG_DIR_NAME>/settings.json` (`:196`)

Load (`:350-378`): **if `scope === "project" && !projectTrusted` return `{}` immediately** —
untrusted project settings are never even read (`:351-353`). Otherwise read the file (missing
or empty → `{}`), `JSON.parse` (**plain JSON, no comment stripping** — unlike `models.json`,
§9.1), then `migrateSettings`. A parse failure is captured, not thrown: `tryLoadFromStorage`
(`:368-378`) returns `{settings:{}, error}` and the error is recorded per-scope and drained by
`drainErrors()` (`:654-658`) into
`` `(${context}, ${scope} settings) ${error.message}` `` warnings (`main.ts:77-85`).

The merged view is `this.settings = deepMergeSettings(globalSettings, projectSettings)`
(`:305`), recomputed on construction, `setProjectTrusted`, `reload`, `save`,
`saveProjectSettings`. `applyOverrides(overrides)` merges on top of the *merged* view only
(`:507-510`).

Writes are per-scope, queued, and field-granular: `markModified(field, nestedKey?)` records
what changed, then `persistScopedSettings` (`:578-607`) re-reads the file under lock, migrates
it, and copies **only the modified fields** (for nested fields, only the modified nested keys)
over it, emitting `JSON.stringify(mergedSettings, null, 2)` — no trailing newline. Writing
project settings while untrusted throws `Project is not trusted; refusing to write project
settings` (`:534-538`). If the scope had a load error, the save is skipped entirely
(`:612-614,630-632`) so a malformed file is never clobbered.

The lock is `proper-lockfile.lockSync(path, {realpath:false})` with 10 attempts × 20 ms busy
wait on `ELOCKED` (`:199-224`); the lock is only acquired if the file exists or a write is
needed, and the parent directory is created lazily (`:226-254`). Rust: an advisory file lock
on `<path>.lock` with the same retry budget is sufficient — the exact lock-file layout is not
part of the byte contract because pirust never shares state with Pi.

### 6.3 `deepMergeSettings` — exact semantics (`:132-160`), quoted in full

```ts
/** Deep merge settings: project/overrides take precedence, nested objects merge recursively */
function deepMergeSettings(base: Settings, overrides: Settings): Settings {
	const result: Settings = { ...base };

	for (const key of Object.keys(overrides) as (keyof Settings)[]) {
		const overrideValue = overrides[key];
		const baseValue = base[key];

		if (overrideValue === undefined) {
			continue;
		}

		// For nested objects, merge recursively
		if (
			typeof overrideValue === "object" &&
			overrideValue !== null &&
			!Array.isArray(overrideValue) &&
			typeof baseValue === "object" &&
			baseValue !== null &&
			!Array.isArray(baseValue)
		) {
			(result as Record<string, unknown>)[key] = { ...baseValue, ...overrideValue };
		} else {
			// For primitives and arrays, override value wins
			(result as Record<string, unknown>)[key] = overrideValue;
		}
	}

	return result;
}
```

**RUNTIME WINS over the doc comment.** The comment says "merge recursively"; the code does
`{...baseValue, ...overrideValue}`, which is **one level of shallow spread**. Precise rules:

| Case | Result |
|---|---|
| key absent from `overrides` | base value kept |
| `overrides[key] === undefined` (explicit `undefined`; impossible from JSON, possible from `applyOverrides`) | **skipped** — base value kept |
| `overrides[key] === null` | override wins → `null` (fails the `!== null` test, so falls to the else) |
| scalar over scalar / scalar over object / object over scalar / object over `undefined` | override replaces wholesale |
| **array** over array (or over anything) | override **replaces**; arrays are never concatenated or element-merged. `packages`, `extensions`, `skills`, `prompts`, `themes`, `enabledModels`, `npmCommand` all follow this |
| object over object (both plain, non-array, non-null) | **one-level** spread: project's keys win per-key, base's other keys survive |
| object over object, **two levels deep** (`retry.provider`) | the inner object is replaced wholesale, NOT merged. Global `retry:{enabled:false,provider:{maxRetries:5}}` + project `retry:{provider:{timeoutMs:1000}}` ⇒ `retry:{enabled:false, provider:{timeoutMs:1000}}` — `maxRetries` is **lost** |

Rust: implement over `serde_json::Map<String, Value>` (order-preserving) with exactly these
five branches, then deserialize into `Settings`; do **not** use a generic recursive JSON merge.

### 6.4 `migrateSettings` (`:381-440`) — 4 rewrites, applied on every load AND on every write-merge

```ts
// 1. queueMode -> steeringMode
if ("queueMode" in settings && !("steeringMode" in settings)) {
	settings.steeringMode = settings.queueMode;
	delete settings.queueMode;
}
```
Guard is key **presence**, not definedness, so `{"steeringMode": null}` blocks the migration
(and `queueMode` is then *kept*, because the `delete` is inside the `if`).

```ts
// 2. legacy websockets boolean -> transport enum
if (!("transport" in settings) && typeof settings.websockets === "boolean") {
	settings.transport = settings.websockets ? "websocket" : "sse";
	delete settings.websockets;
}
```
A non-boolean `websockets` is left untouched (and `transport` is not set).

```ts
// 3. old skills object format -> array
if ("skills" in settings && typeof settings.skills === "object" && settings.skills !== null && !Array.isArray(settings.skills)) {
	const skillsSettings = settings.skills as { enableSkillCommands?: boolean; customDirectories?: unknown };
	if (skillsSettings.enableSkillCommands !== undefined && settings.enableSkillCommands === undefined) {
		settings.enableSkillCommands = skillsSettings.enableSkillCommands;
	}
	if (Array.isArray(skillsSettings.customDirectories) && skillsSettings.customDirectories.length > 0) {
		settings.skills = skillsSettings.customDirectories;
	} else {
		delete settings.skills;
	}
}
```
Note the asymmetry: `enableSkillCommands` is hoisted only if the top-level key is
**absent-or-undefined**, whereas an empty/missing `customDirectories` **deletes** `skills`.

```ts
// 4. retry.maxDelayMs -> retry.provider.maxRetryDelayMs
if ("retry" in settings && typeof settings.retry === "object" && settings.retry !== null && !Array.isArray(settings.retry)) {
	const retrySettings = settings.retry as Record<string, unknown>;
	const providerSettings = typeof retrySettings.provider === "object" && retrySettings.provider !== null
		? (retrySettings.provider as Record<string, unknown>) : undefined;
	if (typeof retrySettings.maxDelayMs === "number" &&
		(providerSettings?.maxRetryDelayMs === undefined || providerSettings?.maxRetryDelayMs === null)) {
		retrySettings.provider = { ...(providerSettings ?? {}), maxRetryDelayMs: retrySettings.maxDelayMs };
	}
	delete retrySettings.maxDelayMs;
}
```
`delete retrySettings.maxDelayMs` is **outside** the inner `if`, so `maxDelayMs` is dropped
even when it is not a number or when `provider.maxRetryDelayMs` already exists — a lossy but
intentional rewrite. Also: `retrySettings.provider` is *replaced* with a new object whose
key order is `{...existing provider keys..., maxRetryDelayMs}`.

Migration is **not** persisted by loading alone; it is persisted the next time any field in
that scope is written (`persistScopedSettings` migrates `currentFileSettings` before
merging, `:585-587`). `SettingsManager.inMemory` also migrates its seed
(`:343-348`).

---

## 7. `migrations.ts` — the 5 startup migrations

### 7.0 `runMigrations` (`:305-315`), quoted in full

```ts
export function runMigrations(cwd: string): {
	migratedAuthProviders: string[];
	deprecationWarnings: string[];
} {
	const migratedAuthProviders = migrateAuthToAuthJson();
	migrateSessionsFromAgentRoot();
	migrateToolsToBin();
	migrateKeybindingsConfigFile();
	const deprecationWarnings = migrateExtensionSystem(cwd);
	return { migratedAuthProviders, deprecationWarnings };
}
```

Order is fixed and observable (M1 rewrites `settings.json`, which M5's project-dir check does
not touch, but M1 must precede any `AuthStorage` construction). It is called **once**, at
`main.ts:555`, i.e. **after** arg parsing/validation and after `takeOverStdout`, and **before**
the startup `SettingsManager` is created (`main.ts:558`) — so M1's `settings.json` rewrite is
visible to it. Failure handling: **every migration swallows its own errors**; `runMigrations`
itself has no try/catch and can only throw if `getAgentDir()` throws. Both return values are
consumed only by interactive mode (`migratedProviders` → `InteractiveMode`, `main.ts:816`;
`deprecationWarnings` → `showDeprecationWarnings` only when `appMode === "interactive"`,
`main.ts:786-788`). **In headless mode the return values are computed and discarded** — the
side effects are what matter.

### 7.1 M1 `migrateAuthToAuthJson` (`:21-73`)

- **Guard:** `if (existsSync(authPath)) return []` — a single existing `auth.json` disables
  the whole migration.
- **Transform (a), `oauth.json`:** if present, `JSON.parse`, then for each
  `[provider, cred]`: `migrated[provider] = { type: "oauth", ...(cred as object) }` — note the
  spread comes **after** `type`, so a `cred.type` in the legacy file wins. Then
  `renameSync(oauthPath, `${oauthPath}.migrated`)`. Whole block in `try {} catch {}`
  (`// Skip on error`) — a parse failure leaves `oauth.json` in place and `providers` partially
  filled.
- **Transform (b), `settings.json.apiKeys`:** if `settings.apiKeys` is a non-null object, for
  each `[provider, key]` with `!migrated[provider] && typeof key === "string"` set
  `migrated[provider] = { type: "api_key", key }`; then `delete settings.apiKeys` and
  `writeFileSync(settingsPath, JSON.stringify(settings, null, 2))` — **no mode option, no
  trailing newline, no lock**. Also in `try {} catch {}`.
- **Write:** only `if (Object.keys(migrated).length > 0)`:
  `mkdirSync(dirname(authPath), {recursive:true})` then
  ``writeFileSync(authPath, JSON.stringify(migrated, null, 2), { mode: 0o600 })``.
  **`auth.json` mode = `0o600`.** (Note `mode` only applies on creation; §8 re-chmods.)
- **Returns** `providers` — insertion order: all oauth providers first (in `oauth.json` key
  order), then the api-key providers (in `settings.json.apiKeys` key order).
- No console output.

### 7.2 M2 `migrateSessionsFromAgentRoot` (`:84-131`)

- **Guard:** `readdirSync(agentDir)` in a try/catch that `return`s on failure; then filter
  `f.endsWith(".jsonl")` (top level only, no recursion); `if (files.length === 0) return`.
- **Per file, inside `try {} catch {}` (`// Skip files that can't be migrated`):**
  read whole file utf8, take `content.split("\n")[0]`, skip if falsy/blank, `JSON.parse` it,
  skip unless `header.type === "session" && header.cwd`; compute
  `const safePath = `--${cwd.replace(/^[/\\]/, "").replace(/[/\\:]/g, "-")}--``, target dir
  `join(agentDir, "sessions", safePath)`, `mkdirSync` recursive if missing;
  `const fileName = file.split("/").pop() || file.split("\\").pop()`; `newPath = join(dir,
  fileName)`; `if (existsSync(newPath)) continue`; `renameSync(file, newPath)`.
- **RUNTIME HAZARD (reproduce as-is):** the basename extraction is
  `file.split("/").pop() || file.split("\\").pop()`. On Windows `file` is
  `C:\Users\...\x.jsonl`, `split("/")` yields one element, `.pop()` returns the whole path
  (truthy) so the `||` never fires and `fileName` is the **full path**. `join(dir, fullPath)`
  with an absolute-ish second argument produces `dir\C:\Users\...` on Node's `path.win32.join`
  — which then fails `renameSync` and is swallowed. So on Windows M2 is effectively a no-op.
  Port the expression literally (and let the rename fail) rather than "fixing" it.
- No console output. Docs reference: pi-mono issue #320, bug from v0.30.0.

### 7.3 M3 `migrateToolsToBin` (`:177-216`)

- **Guard:** `if (!existsSync(toolsDir)) return` where `toolsDir = join(agentDir,"tools")`.
- **Transform:** for `bin` in **exactly** `["fd", "rg", "fd.exe", "rg.exe"]` (this order):
  if `join(toolsDir,bin)` exists → `mkdirSync(binDir)` if missing; if the target does not
  exist → `renameSync` (errors ignored) and set `movedAny = true`; **else** (target exists)
  → `rmSync?.(oldPath, {force:true})` (errors ignored), **without** setting `movedAny`.
- **Output:** `if (movedAny)` → stdout, green:
  `Migrated managed binaries tools/ → bin/`
  (verbatim, including the U+2192 arrow).

### 7.4 M4 `migrateKeybindingsConfigFile` (`:157-172`)

- **Guard:** `configPath = join(getAgentDir(), "keybindings.json")`;
  `if (!existsSync(configPath)) return`.
- **Transform (all inside `try {} catch {}`, `// Ignore malformed files during migration`):**
  `JSON.parse`; bail if not a non-null non-array object; call
  `migrateKeybindingsConfig(parsed)` (`core/keybindings.ts:289-309`) which (a) renames legacy
  keys via `KEYBINDING_NAME_MIGRATIONS` (a 60-odd entry map of bare names → `tui.editor.*`
  etc., `core/keybindings.ts:209-270`), **dropping** a legacy key whose new name already
  exists, and (b) reorders the result into `Object.keys(KEYBINDINGS)` order followed by extras;
  `if (!migrated) return`; else
  ``writeFileSync(configPath, `${JSON.stringify(config, null, 2)}\n`, "utf-8")`` — **this one
  DOES append a trailing newline**, unlike M1's settings write.
- No console output.
- **feat-005 scope note:** the rename map and `KEYBINDINGS` belong to feat-007. Land M4 with
  the guard/parse/write shape and an **empty** rename map + identity ordering, so
  `migrated` is always `false` and no file is rewritten. Document the stub inline; feat-007
  fills the map.

### 7.5 M5 `migrateExtensionSystem` (`:257-272`) — the only one that returns warnings

```ts
function migrateExtensionSystem(cwd: string): string[] {
	const agentDir = getAgentDir();
	const projectDir = join(cwd, CONFIG_DIR_NAME);
	migrateCommandsToPrompts(agentDir, "Global");
	migrateCommandsToPrompts(projectDir, "Project");
	const warnings = [
		...checkDeprecatedExtensionDirs(agentDir, "Global"),
		...checkDeprecatedExtensionDirs(projectDir, "Project"),
	];
	return warnings;
}
```

`migrateCommandsToPrompts(baseDir, label)` (`:137-155`): guard
`existsSync(commands) && !existsSync(prompts)`; then `renameSync(commands, prompts)`; on
success log to stdout green `` `Migrated ${label} commands/ → prompts/` `` and return true; on
throw log to stdout yellow
`` `Warning: Could not migrate ${label} commands/ to prompts/: ${err instanceof Error ? err.message : err}` ``
and return false.

`checkDeprecatedExtensionDirs(baseDir, label)` (`:222-252`) collects, in this order:
1. if `<baseDir>/hooks` exists →
   `` `${label} hooks/ directory found. Hooks have been renamed to extensions.` ``
2. if `<baseDir>/tools` exists → `readdirSync` it (errors ignored) and filter out entries whose
   lowercase name is `fd`/`rg`/`fd.exe`/`rg.exe` **or** which start with `.`; if any remain →
   `` `${label} tools/ directory contains custom tools. Custom tools have been merged into extensions.` ``

So the warnings vector has at most 4 entries, ordered Global-hooks, Global-tools,
Project-hooks, Project-tools.

`showDeprecationWarnings(warnings)` (`:277-298`) — interactive only; for completeness it
prints each as `` `Warning: ${warning}` `` (yellow), then
`\nMove your extensions to the extensions/ directory.`,
`` `Migration guide: ${MIGRATION_GUIDE_URL}` ``,
`` `Documentation: ${EXTENSIONS_DOC_URL}` ``,
`\nPress any key to continue...` (dim), then blocks on a raw-mode keypress, then an empty
`console.log()`. URLs (`:11-14`):
`https://github.com/earendil-works/pi-mono/blob/main/packages/coding-agent/CHANGELOG.md#extensions-migration`
and
`https://github.com/earendil-works/pi-mono/blob/main/packages/coding-agent/docs/extensions.md`.
**Do not port the keypress wait in feat-005** (it would hang a headless run); gate it on
interactive mode exactly as `main.ts:786` does.

### 7.6 File modes summary

| Path | Mode | Where |
|---|---|---|
| `auth.json` | **`0o600`** on write, plus an explicit `chmodSync(path, 0o600)` after every write | `migrations.ts:69`; `core/auth-storage.ts:21,44-46,86-87,131-132` |
| `<agentDir>` (auth's parent) | `0o700` when created by auth storage | `core/auth-storage.ts:38` |
| `settings.json` | default (no mode passed) | `migrations.ts:60`, `core/settings-manager.ts:247` |
| `keybindings.json` | default | `migrations.ts:168` |
| `trust.json` | default | `core/trust-manager.ts:133` |
| session `.jsonl` | default; created with flag `"wx"` for new sessions (`core/session-manager.ts:961,1531`) and `"w"` for rewrites (`:912`) | — |
| session dirs / `bin` / `sessions/<enc>` | `mkdirSync(recursive: true)`, default mode | — |

On Windows `chmod` is a no-op; Rust should apply `0o600` via
`std::os::unix::fs::PermissionsExt` under `#[cfg(unix)]` and skip it elsewhere.

### 7.7 The cwd → session-dir encoding

The expression, identical in all three copies (`migrations.ts:112`,
`core/session-manager.ts:475`, `packages/agent/src/harness/session/jsonl-repo.ts:35`):

```js
const safePath = `--${cwd.replace(/^[/\\]/, "").replace(/[/\\:]/g, "-")}--`;
```

Algorithm: (1) strip **one** leading `/` or `\`; (2) replace **every** `/`, `\` and `:` with
`-`; (3) wrap in `--` … `--`. `session-manager.ts:473-476` applies it to
`resolvePath(cwd)` (absolute, symlinks NOT resolved) and joins under
`<agentDir>/sessions/`. `migrations.ts:109` applies it to the **raw `header.cwd`** string with
no `resolvePath` — a difference that only matters for already-corrupt headers.

Worked examples (agentDir = `~/.pirust/agent`):

| `cwd` | after step 1 | after step 2 | encoded dir name |
|---|---|---|---|
| `/home/me/proj` | `home/me/proj` | `home-me-proj` | `--home-me-proj--` |
| `/` | `` (empty) | `` | `----` |
| `/home/me/a-b` | `home/me/a-b` | `home-me-a-b` | `--home-me-a-b--` (collides with `/home/me/a/b` — accepted upstream) |
| `C:\Users\me\proj` | `C:\Users\me\proj` (no leading sep to strip) | `C--Users-me-proj` | `--C--Users-me-proj--` |
| `\\?\C:\x` (UNC-ish) | `\?\C:\x` → strips one leading `\` | `-?-C--x` | `---?-C--x--` |
| `/tmp/a b` | `tmp/a b` | `tmp-a b` | `--tmp-a b--` (spaces preserved) |

Note the Windows case: the drive colon becomes `-` and the `\` after it becomes `-`, giving a
**double** dash `C--Users`. Full default path for `C:\Users\me\proj`:
`C:\Users\me\.pirust\agent\sessions\--C--Users-me-proj--`.

Rust: operate on the string as bytes/chars with no path awareness — do **not** use
`Path::components()`, and do not canonicalize.

`getDefaultSessionDir(cwd, agentDir)` (`core/session-manager.ts:479-485`) additionally
`mkdirSync(recursive)` the directory if missing and returns it;
`getDefaultSessionDirPath` (`:472-477`) is the pure variant used for the equality tests at
`:1470` and `:1551` (`filterCwd = sessionDir !== undefined && dir !== getDefaultSessionDirPath(cwd)`).

---

## 8. `auth.json` (`core/auth-storage.ts`)

### 8.1 Schema

`type AuthStorageData = Record<string, Credential>` (`:14`), keyed by `Provider.id`.
`Credential` (`packages/ai/src/auth/types.ts:17-37`):

```ts
interface ApiKeyCredential { type: "api_key"; key?: string; env?: ProviderEnv }
interface OAuthCredentials { refresh: string; access: string; expires: number; [key: string]: unknown }
interface OAuthCredential extends OAuthCredentials { type: "oauth" }
type Credential = ApiKeyCredential | OAuthCredential;
```

On-disk example (formatting is exactly `JSON.stringify(data, null, 2)`, no trailing newline):

```json
{
  "anthropic": {
    "type": "api_key",
    "key": "sk-ant-api03-…"
  }
}
```

Rust: `#[serde(tag = "type")] enum Credential { #[serde(rename="api_key")] ApiKey {
key: Option<String>, env: Option<BTreeMap<String,String>> }, #[serde(rename="oauth")] OAuth {
refresh: String, access: String, expires: i64, #[serde(flatten)] extra: Map<String,Value> } }`
— the `[key: string]: unknown` index signature on OAuth means unknown keys **must** be
preserved on round-trip (`#[serde(flatten)]`), because `modify` rewrites the whole file.
Field/key order in the file must be preserved for the same reason → order-preserving map for
the top level.

### 8.2 Read / write / merge

- `AuthStorage.create(authPath?)` (`:180-182`) defaults to `join(getAgentDir(),"auth.json")`,
  `normalizePath`'d (`:32`), and calls `reload()` in the constructor (`:177`).
- `reload()` (`:204-215`) reads under lock into `this.data`; **on any failure it keeps the last
  valid in-memory snapshot** and does not report (`catch { }`).
- `read(provider)` (`:217-222`): returns the raw credential, except for `type === "api_key"`
  with a defined `key`, where it returns `{...credential, key: resolveConfigValue(credential.key,
  credential.env)}`. `resolveConfigValue` (`core/resolve-config-value.ts`) supports
  `$VAR` / `${VAR}` env interpolation (with `$$` and `$!` escapes) and `!command` shell
  execution with a process-lifetime cache. **feat-005 minimum:** support the literal case and
  `$VAR`/`${VAR}`; a `key` beginning with `!` (shell command) may be rejected with a
  diagnostic rather than executed — record that as an intentional narrowing.
- `modify(provider, fn)` (`:224-240`): read-modify-write inside `withLockAsync`; parse current
  file (`{}` when absent), call `fn(currentData[provider])`; if it returns `undefined`, only
  refresh `this.data` and return the **current** value (no write); else
  `merged = { ...currentData, [provider]: next }` — a **top-level shallow merge that replaces
  the whole per-provider credential** — and write `JSON.stringify(merged, null, 2)`. Note the
  spread means an existing key keeps its position while a new provider appends.
- `delete(provider)` (`:242-249`): parse, `delete currentData[provider]`, always write.
- `list()` (`:252-254`): `[{providerId, type}]` from the in-memory snapshot, in map order.
- Every write goes through `AUTH_FILE_WRITE_OPTIONS = { encoding: "utf-8", mode: 0o600 }`
  followed by an explicit `chmodSync(path, 0o600)` (`:86-87,131-132`).
- `ensureParentDir()` creates the parent with `mode: 0o700`; `ensureFileExists()` seeds the
  file with the two bytes `{}` at `0o600` (`:35-47`). Both run before **every** lock
  acquisition, so merely constructing an `AuthStorage` creates `~/.pirust/agent/auth.json`.
- Async lock config (`:111-124`): `retries: {retries:10, factor:2, minTimeout:100,
  maxTimeout:10000, randomize:true}`, `stale: 30000`, plus an `onCompromised` flag checked
  three times; the sync variant reuses the 10×20 ms `ELOCKED` loop.
- `readStoredCredential(providerId, authPath?)` (`:261-271`) is a one-off sync read that
  returns `undefined` on any error and does **not** resolve `$VAR` values.
- `FileModelsStore` (`core/models-store.ts:25-57`) reuses `FileAuthStorageBackend` for
  `models-store.json`, so that file is **also** `0o600` — a subtle consequence worth keeping.

---

## 9. Model resolution (`anthropic` api only)

### 9.1 `models.json` — `ModelConfig` (`core/model-config.ts:226-287`)

Top-level schema is exactly `{ providers: Record<string, ProviderConfig> }` (`:196-198`) —
a **required** `providers` key; anything else fails validation. Load
(`ModelConfig.load(path)`, `:235-274`):

1. no path → empty config, no error.
2. `normalizePath(path)`, `readFile` utf-8. `ENOENT` → empty config, **no error**. Any other
   read error → empty config + error
   `` `Failed to load models.json: ${msg}\n\nFile: ${path}` ``.
3. `JSON.parse(stripJsonComments(content))` — models.json tolerates **`//` line comments and
   trailing commas only** (unlike settings.json, which tolerates neither).
   **CORRECTION:** `stripJsonComments` (`utils/json.ts:2-6`) is exactly two regexes — `//`
   line comments and trailing commas. It does **NOT** handle `/* */`. A block comment
   survives stripping and makes `JSON.parse` fail. Note the fixture record *named*
   `json-with-comments-is-ACCEPTED` (`models.cases.jsonl` 124/143) records a **parse
   failure** at position 26 — the surviving `/` of `/*` once the `//` text is deleted and its
   newline kept. The record name is misleading; the recorded result is authoritative.
   A port that accepted block comments would accept files real Pi rejects.
   Failure → empty config + error
   `` `Failed to parse models.json: ${msg}\n\nFile: ${path}` ``.
4. TypeBox validation. Failure → empty config + error
   `` `Invalid models.json schema:\n${errors}\n\nFile: ${path}` `` where each error line is
   `` `  - ${path}: ${message}` `` and `path` is `instancePath` with the leading `/` removed
   and remaining `/` → `.` (or `"root"`), except `required` errors which append the missing
   property name (`:206-217`); if there are no formatted errors the body is
   `Unknown schema error`.
5. Success → `providers` map of deep-frozen structural clones, in file key order.

The error string is surfaced by `ModelRuntime.getError()` (`core/model-runtime.ts:328-337`),
which joins `[configError, ...compositionErrors, availabilityError]` with `"\n\n"`;
composition errors render as `` `Provider "${id}": ${error}` `` and the availability error as
`` `Availability refresh: ${msg}` ``. `--list-models` prints it as a warning to stderr.

`ProviderConfig` (`:183-194`) and its nested schemas — port only what the anthropic path
needs, but **parse and reject** the rest per the schema so error strings match:

```ts
ProviderConfigSchema = {
  name?: string(minLength 1); baseUrl?: string(minLength 1); apiKey?: string(minLength 1);
  api?: string(minLength 1); oauth?: "radius"; headers?: Record<string,string>;
  compat?: OpenAICompletionsCompat | OpenAIResponsesCompat | AnthropicMessagesCompat;
  authHeader?: boolean; models?: ModelDefinition[]; modelOverrides?: Record<string, ModelOverride>;
}
ModelDefinitionSchema = { id: string(minLength 1); name?; api?; baseUrl?; reasoning?: boolean;
  thinkingLevelMap?: Partial<Record<ThinkingLevel, string|null>>; input?: ("text"|"image")[];
  cost?: { input,output,cacheRead,cacheWrite: number; tiers?: {inputTokensAbove,…}[] };
  contextWindow?: number; maxTokens?: number; headers?: Record<string,string>; compat?; }
ModelOverrideSchema = ModelDefinition minus `id`/`api`/`baseUrl`, with all `cost` fields optional.
AnthropicMessagesCompatSchema = { supportsEagerToolInputStreaming?, supportsLongCacheRetention?,
  sendSessionAffinityHeaders?, supportsCacheControlOnTools?, forceAdaptiveThinking?,
  supportsToolReferences?: boolean }
```

`ThinkingLevelMap` and `ModelCost`/`ModelCostTier` already exist in Rust at
`crates/pirust-ai/src/types/model.rs:51-118` with the correct null-vs-absent handling —
reuse them.

### 9.2 Provider composition (what feat-005 actually needs)

Pi builds providers from three sources (`ModelRuntime.providerIds`,
`core/model-runtime.ts:185-192`): the builtin catalog
(`@earendil-works/pi-ai/providers/all`), `models.json`, and extension registrations.
`recomposeProvider` (`:194-216`) has a fast path worth reproducing exactly:

- base builtin exists **and** no `models.json` entry **and** no extension override →
  `setProvider(base)` **untouched** ("so its auth/login/stream behavior is exact", `:203`).
- otherwise `composeModelProvider(id, base, config, extension)`, whose throw is captured into
  `compositionErrors` and falls back to the untouched base (or deletes the provider).
- neither base nor config nor extension → `deleteProvider(id)`.

**feat-005 narrowing:** pirust ports **one** provider — `anthropic`, api
`anthropic-messages` (`crates/pirust-ai/src/api/anthropic_messages.rs`). The builtin catalog
collapses to a single hard-coded `anthropic` provider descriptor plus its model list; the
`radius` oauth branch (`:168-183`), `withRemoteCatalog` (network model refresh), and every
other provider id are out of scope. A `models.json` provider whose composed `api` is not
`anthropic-messages` must be **skipped with a `compositionErrors` entry**, not silently
dropped, so `getError()` still reports it.

`hasConfiguredAuth(providerId)` (`:364-366`) = the provider appeared in the availability
snapshot's `configuredProviders`, which is the set of providers for which
`models.checkAuth(id)` returned a non-`undefined` `AuthCheck` (`:249-253`). For anthropic that
means: an `auth.json` credential, or `ANTHROPIC_OAUTH_TOKEN`/`ANTHROPIC_API_KEY` in the env
(precedence per `crates/pirust-ai/src/auth/mod.rs:40-66`), or a runtime key set by `--api-key`.
`getAvailable()` = all models filtered to `configuredProviders` (`:230`);
`getModels()` = **all** models regardless of auth — and `resolveCliModel` deliberately uses
`getModels()` "so `--api-key` can be used for first-time setup" (`core/model-resolver.ts:376-378`).
`setRuntimeApiKey(provider, key)` (`:392-405`) optimistically injects
`{type:"api_key", source:"runtime API key"}` into the snapshot before refreshing.

### 9.3 `parseModelPattern` — the `:thinking` suffix (`core/model-resolver.ts:193-247`)

```
1. tryMatchModel(pattern) → if hit, return {model, thinkingLevel: undefined, warning: undefined}
2. lastColonIndex = pattern.lastIndexOf(":"); if -1 → {undefined, undefined, undefined}
3. prefix = pattern[0..lastColon], suffix = pattern[lastColon+1..]
4. if isValidThinkingLevel(suffix):
      r = parseModelPattern(prefix)              // recurse
      if r.model: return { model: r.model, thinkingLevel: r.warning ? undefined : suffix, warning: r.warning }
      else return r
5. else (invalid suffix):
      if !options.allowInvalidThinkingLevelFallback (default TRUE): return {undefined, undefined, undefined}
      r = parseModelPattern(prefix)
      if r.model: return { model: r.model, thinkingLevel: undefined,
                           warning: `Invalid thinking level "${suffix}" in pattern "${pattern}". Using default instead.` }
      else return r
```

Key points: the **full pattern is tried as a model id first**, so OpenRouter-style ids
containing `:` still resolve; recursion strips one colon-suffix at a time from the right; a
warning from an inner level **suppresses** the outer thinking level. `resolveCliModel` passes
`allowInvalidThinkingLevelFallback: false` (strict, `:445`), `resolveModelScope` uses the
default `true`.

`tryMatchModel` (`:125-155`): `findExactModelReferenceMatch` first (`:77-119` — case-insensitive
`provider/id`, then `provider`+`id` split on the **first** `/`, then bare `id`; **ambiguous
matches across providers return `undefined`**), else substring match on `id` **or** `name`
(both lowercased); then prefer "aliases" over dated versions, where `isAlias(id)` (`:63-70`) is
`id.endsWith("-latest") || !/-\d{8}$/.test(id)`; ties broken by
`sort((a,b) => b.id.localeCompare(a.id))` and taking `[0]`.

> `localeCompare` is locale-aware in Node. For the ASCII model ids in practice it matches
> byte ordering; Rust should use plain `str::cmp` **descending** and note the divergence.

`resolveModelScopeWithDiagnostics` (`:270-332`): a pattern containing `*`, `?` or `[` is a
**glob** — an optional `:level` suffix is stripped only if the suffix is a valid level, then
`minimatch(fullId, glob, {nocase:true}) || minimatch(m.id, glob, {nocase:true})`; no matches →
warning `` `No models match pattern "${pattern}"` ``; matches are appended in
`availableModels` order, de-duplicated by `modelsAreEqual`. Non-glob patterns go through
`parseModelPattern` and push its warning (if any) **and then** the no-match warning if there
is no model. `resolveModelScope` (`:334-340`) prints every diagnostic as
`` `Warning: ${message}` `` to stderr and returns the scoped models.

### 9.4 `resolveCliModel` (`:364-535`) — the `--model`/`--provider` chain

Ordered, with the exact early returns:

1. `!cliModel` → `{model: undefined, warning: undefined, error: undefined}` (so `--provider`
   alone does nothing here).
2. `availableModels = [...modelRuntime.getModels()]`; if empty → error
   `No models available. Check your installation or add models to models.json.`
3. Build `providerMap: lowercase → canonical` from all models. If `cliProvider` is set and
   unknown → error
   `` `Unknown provider "${cliProvider}". Use --list-models to see available providers/models.` ``
4. If no `--provider`: if `cliModel` contains `/` and the part before the **first** `/` is a
   known provider → `provider = canonical`, `pattern = rest`, `inferredProvider = true`.
5. If still no provider: exact (case-insensitive) match of `cliModel` against `id` or
   `provider/id` → return that model immediately.
6. If both `--provider` and a resolved provider: strip a redundant `` `${provider}/` `` prefix
   from `cliModel` (case-insensitive) into `pattern`.
7. `candidates = provider ? filter(m.provider === provider) : all`;
   `parseModelPattern(pattern, candidates, {allowInvalidThinkingLevelFallback:false})`.
8. If a model was found: when `inferredProvider` and that model's provider has **no**
   configured auth, prefer a *single* exact raw-id match on a provider that does (`:454-469`).
   Return `{model, thinkingLevel, warning, error: undefined}`.
9. If nothing found and `inferredProvider`: retry the exact-id/`provider/id` match against all
   models, then `parseModelPattern(cliModel, all, strict)`.
10. If a provider is known: split a trailing valid `:level` off `pattern` (**only when
    `cliThinking` is unset**), then `buildFallbackModel(provider, fallbackPattern, all)`
    (`:164-178` — clone the provider's `defaultModelPerProvider` model, or its first model,
    and overwrite `id` and `name` with the requested string). If the requested thinking is set
    and not `"off"`, also force `reasoning: true`. Warning:
    `` `Model "${fallbackPattern}" not found for provider "${provider}". Using custom model id.` ``
    prefixed by the inner warning + a space when one exists.
11. Otherwise error `` `Model "${display}" not found. Use --list-models to see available models.` ``
    where `display = provider ? `${provider}/${pattern}` : cliModel`.

Step 10 is what makes `--provider anthropic --model my-proxy-model` work against an
unlisted model id — **essential for the local-proxy scenario** and must be ported.

`findInitialModel` (`:551-631`) is the *no-CLI-model* fallback used inside `sdk.ts`
(`core/sdk.ts:203-210`): (1) `cliProvider && cliModel` → `resolveCliModel`, printing its error
and `process.exit(1)`; (2) `scopedModels[0]` when not continuing; (3) the saved
`defaultProvider`/`defaultModel` **if `hasConfiguredAuth`**; (4) the first available model
matching `defaultModelPerProvider` scanning `Object.keys(defaultModelPerProvider)` **in
declaration order** (`:14-51`, `amazon-bedrock, ant-ling, anthropic, openai, …`), else
`availableModels[0]`; (5) `undefined`. `thinkingLevel` defaults to `DEFAULT_THINKING_LEVEL`
(`"medium"`) except in branch (2)/(3) where the scoped/settings level wins.

**pirust narrowing:** `defaultModelPerProvider` collapses to the single entry
`anthropic: "claude-opus-4-8"` (`:17`). Keep the map (and its iteration order) as a table so
adding providers later is data-only.

### 9.5 Minimum config to run one `anthropic` provider against a local proxy

Two supported shapes. **(a) override the builtin's `baseUrl`** —
`~/.pirust/agent/models.json`:

```json
{
  "providers": {
    "anthropic": {
      "baseUrl": "http://127.0.0.1:8080/v1"
    }
  }
}
```

plus `~/.pirust/agent/auth.json` `{"anthropic":{"type":"api_key","key":"dummy"}}` (or
`ANTHROPIC_API_KEY=dummy` in the env). Every builtin anthropic model then points at the proxy.

**(b) a fully self-described provider** (no reliance on the builtin catalog):

```json
{
  "providers": {
    "anthropic": {
      "name": "Local Proxy",
      "baseUrl": "http://127.0.0.1:8080/v1",
      "api": "anthropic-messages",
      "apiKey": "dummy",
      "models": [
        {
          "id": "claude-sonnet-4-5",
          "name": "claude-sonnet-4-5",
          "reasoning": true,
          "input": ["text", "image"],
          "cost": { "input": 3, "output": 15, "cacheRead": 0.3, "cacheWrite": 3.75 },
          "contextWindow": 200000,
          "maxTokens": 64000
        }
      ]
    }
  }
}
```

Run: `pirust --provider anthropic --model claude-sonnet-4-5 -p "hello"`. The auth precedence
is `auth.json` → `ANTHROPIC_OAUTH_TOKEN` → `ANTHROPIC_API_KEY`, and the header is
`Authorization: Bearer` when the key contains `sk-ant-oat`, else `X-Api-Key`
(`crates/pirust-ai/src/auth/mod.rs:37-72`). With `PIRUST_OFFLINE=1` set,
`allowModelNetwork` is false (`core/model-runtime.ts:152` — note Pi tests
`process.env.PI_OFFLINE === undefined`, i.e. **any** value including `"0"` disables the
network) and no catalog refresh is attempted.

### 9.6 `--list-models` output (`cli/list-models.ts:29-111`)

Fixed six-column table on **stdout**, no colour: header row
`provider  model  context  max-out  thinking  images`, each cell `padEnd`'d to
`max(headerLen, max(cellLen))` and joined with **two spaces**; rows sorted by
`provider.localeCompare` then `id.localeCompare`. `context`/`max-out` use `formatTokenCount`
(`:14-24`): `>=1e6` → `` `${m}M` `` or `` `${m.toFixed(1)}M` `` when not integral; `>=1e3` →
same with `K`; else the raw integer. `thinking` = `reasoning ? "yes" : "no"`;
`images` = `input.includes("image") ? "yes" : "no"`. Empty catalog → the no-models message on
**stdout** (not stderr) and return without exiting non-zero; a search with no hits →
`` `No models matching "${searchPattern}"` `` on stdout. The fuzzy filter is pi-tui's
`fuzzyFilter(models, pattern, m => `${m.provider} ${m.id}`)` — feat-006 owns it; for feat-005
implement a case-insensitive subsequence match over that same key and flag it as an
approximation to verify against a capture.

---

## 10. The CLI → session bridge

### 10.1 `buildSessionOptions` (`main.ts:357-453`), quoted in full

```ts
function buildSessionOptions(
	parsed: Args,
	scopedModels: ScopedModel[],
	hasExistingSession: boolean,
	modelRuntime: ModelRuntime,
	settingsManager: SettingsManager,
): {
	options: CreateAgentSessionOptions;
	cliThinkingFromModel: boolean;
	diagnostics: AgentSessionRuntimeDiagnostic[];
} {
	const options: CreateAgentSessionOptions = {};
	const diagnostics: AgentSessionRuntimeDiagnostic[] = [];
	let cliThinkingFromModel = false;

	// Model from CLI
	// - supports --provider <name> --model <pattern>
	// - supports --model <provider>/<pattern>
	if (parsed.model) {
		const resolved = resolveCliModel({
			cliProvider: parsed.provider,
			cliModel: parsed.model,
			cliThinking: parsed.thinking,
			modelRuntime,
		});
		if (resolved.warning) {
			diagnostics.push({ type: "warning", message: resolved.warning });
		}
		if (resolved.error) {
			diagnostics.push({ type: "error", message: resolved.error });
		}
		if (resolved.model) {
			options.model = resolved.model;
			// Allow "--model <pattern>:<thinking>" as a shorthand.
			// Explicit --thinking still takes precedence (applied later).
			if (!parsed.thinking && resolved.thinkingLevel) {
				options.thinkingLevel = resolved.thinkingLevel;
				cliThinkingFromModel = true;
			}
		}
	}

	if (!options.model && scopedModels.length > 0 && !hasExistingSession) {
		// Check if saved default is in scoped models - use it if so, otherwise first scoped model
		const savedProvider = settingsManager.getDefaultProvider();
		const savedModelId = settingsManager.getDefaultModel();
		const savedModel = savedProvider && savedModelId ? modelRuntime.getModel(savedProvider, savedModelId) : undefined;
		const savedInScope = savedModel ? scopedModels.find((sm) => modelsAreEqual(sm.model, savedModel)) : undefined;

		if (savedInScope) {
			options.model = savedInScope.model;
			// Use thinking level from scoped model config if explicitly set
			if (!parsed.thinking && savedInScope.thinkingLevel) {
				options.thinkingLevel = savedInScope.thinkingLevel;
			}
		} else {
			options.model = scopedModels[0].model;
			// Use thinking level from first scoped model if explicitly set
			if (!parsed.thinking && scopedModels[0].thinkingLevel) {
				options.thinkingLevel = scopedModels[0].thinkingLevel;
			}
		}
	}

	// Thinking level from CLI (takes precedence over scoped model thinking levels set above)
	if (parsed.thinking) {
		options.thinkingLevel = parsed.thinking;
	}

	// Scoped models for Ctrl+P cycling
	// Keep thinking level undefined when not explicitly set in the model pattern.
	// Undefined means "inherit current session thinking level" during cycling.
	if (scopedModels.length > 0) {
		options.scopedModels = scopedModels.map((sm) => ({
			model: sm.model,
			thinkingLevel: sm.thinkingLevel,
		}));
	}

	// API key from CLI - set as a non-persistent runtime override
	// (handled by caller before createAgentSession)

	// Tools
	if (parsed.noTools) {
		options.noTools = "all";
	} else if (parsed.noBuiltinTools) {
		options.noTools = "builtin";
	}
	if (parsed.tools) {
		options.tools = [...parsed.tools];
	}
	if (parsed.excludeTools) {
		options.excludeTools = [...parsed.excludeTools];
	}

	return { options, cliThinkingFromModel, diagnostics };
}
```

Notes an implementer must not miss:

- Called from inside the `createRuntime` factory (`main.ts:696-702`) with
  `hasExistingSession = sessionManager.buildSessionContext().messages.length > 0` and
  `scopedModels = parsed.models ?? settingsManager.getEnabledModels()` resolved through
  `resolveModelScope` (empty array when the pattern list is absent or empty, `main.ts:689-691`).
- `--model` with an unresolvable value produces an **error diagnostic** here, which
  `main.ts:792-797` turns into `exit(1)` after printing.
- `parsed.thinking` overrides both the `:level` suffix and the scoped-model level, but
  `cliThinkingFromModel` still records that the suffix supplied one — used at `main.ts:729-732`:
  `cliThinkingOverride = parsed.thinking !== undefined || cliThinkingFromModel`, and if a model
  exists, `created.session.setThinkingLevel(created.session.thinkingLevel)` is called to force
  a `thinking_level_change` session entry.
- `--api-key` is handled **outside** this function (`main.ts:705-715`): it requires
  `sessionOptions.model` (else the error diagnostic in §3.6), then
  `await modelRuntime.setRuntimeApiKey(model.provider, parsed.apiKey)` followed by a redundant
  `await services.modelRuntime.getAvailable()`.
- `noTools`/`tools`/`excludeTools` are copied, not aliased. `--no-tools` beats
  `--no-builtin-tools`.

`CreateAgentSessionOptions` (`core/sdk.ts:33-80`) is the target struct: `cwd`, `agentDir`,
`modelRuntime`, `model`, `thinkingLevel`, `scopedModels`, `noTools: "all"|"builtin"`, `tools`,
`excludeTools`, `customTools`, `resourceLoader`, `sessionManager`, `settingsManager`,
`sessionStartEvent`.

### 10.2 `new Agent({...})` (`core/sdk.ts:289-355`) — 15 wired fields

| Field | Value | Kind |
|---|---|---|
| `initialState` | `{ systemPrompt: "", model, thinkingLevel, tools: [] }` — **empty prompt and no tools**; `AgentSession` fills both later | **CORE** |
| `convertToLlm` | `convertToLlmWithBlockImages` (`:251-285`): agent-core's `convertToLlm` (`packages/agent/src/harness/messages.ts:120-164`), then — **only if `settingsManager.getBlockImages()` is true, checked per call** — replace every `image` block in `user`/`toolResult` messages with `{type:"text",text:"Image reading is disabled."}` and **dedupe consecutive** identical placeholders | **CORE** |
| `streamFn` | `modelRuntime.streamSimple(model, context, {...})` with `timeoutMs = options?.timeoutMs ?? providerRetry.timeoutMs ?? (httpIdleTimeoutMs === 0 ? 2147483647 : httpIdleTimeoutMs)`, `websocketConnectTimeoutMs`, `maxRetries`, `maxRetryDelayMs`, and a `transformHeaders` callback | **CORE** (the `2147483647` sentinel is deliberate: "SDKs treat timeout=0 as 0ms", `:300-302`) |
| ↳ `transformHeaders` | `mergeProviderAttributionHeaders(model, settingsManager, options?.sessionId, requestHeaders)` then, **if any extension handles `before_provider_headers`**, `runner.emitBeforeProviderHeaders(headers ?? {})` | attribution = CORE; the runner branch = **STUB** |
| `onPayload` | passthrough unless an extension handles `before_provider_request` | **STUB** (identity) |
| `onResponse` | no-op unless an extension handles `after_provider_response` | **STUB** (no-op) |
| `sessionId` | `sessionManager.getSessionId()` | **CORE** |
| `transformContext` | `runner.emitContext(messages)`, identity when no runner | **STUB** (identity) |
| `steeringMode` | `settingsManager.getSteeringMode()` | **CORE** |
| `followUpMode` | `settingsManager.getFollowUpMode()` | **CORE** |
| `transport` | `settingsManager.getTransport()` | **CORE** (pirust: only the SSE path exists in feat-002; `"websocket"`/`"auto"` must degrade to SSE with a documented narrowing) |
| `thinkingBudgets` | `settingsManager.getThinkingBudgets()` | **CORE** |
| `maxRetryDelayMs` | `settingsManager.getProviderRetrySettings().maxRetryDelayMs` | **CORE** |

Mapping to the Rust `AgentOptions` (`crates/pirust-agent-core/src/agent.rs:205-236`):
`initialState.systemPrompt/model/thinkingLevel/tools` → `system_prompt`/`model`/
`thinking_level`/`tools`; `convertToLlm` → `convert_to_llm`; `streamFn` → `stream_fn`;
`transformContext` → `transform_context`; `sessionId` → `session_id`;
`steeringMode`/`followUpMode` → `steering_mode`/`follow_up_mode`.
**`onPayload`, `onResponse`, `transport`, `thinkingBudgets`, `maxRetryDelayMs` have no field in
the ported `AgentOptions`** — they are stream-level concerns that pirust threads through the
`stream_fn` closure into `pirust-ai`'s request builder instead. Do **not** widen
`AgentOptions` for them; document the placement in `sdk.rs`.

### 10.3 The rest of `createAgentSession` (`core/sdk.ts:164-393`)

Order of operations (all of it CORE except where noted):

1. `cwd = resolvePath(options.cwd ?? sessionManager?.getCwd() ?? process.cwd())`;
   `agentDir = options.agentDir ? resolvePath(it) : getAgentDir()`.
2. `modelRuntime = options.modelRuntime ?? await ModelRuntime.create({authPath, modelsPath})`
   where the paths are only derived when `options.agentDir` was explicit (`:169-171`).
3. `settingsManager`/`sessionManager` defaults;
   `sessionManager ?? SessionManager.create(cwd, getDefaultSessionDir(cwd, agentDir))`.
4. resourceLoader default + `reload()` — **STUB**.
5. `existingSession = sessionManager.buildSessionContext()`;
   `hasExistingSession = messages.length > 0`;
   `hasThinkingEntry = getBranch().some(e => e.type === "thinking_level_change")`.
6. Model restore: if no explicit model and the session recorded one, `getModel(provider,
   modelId)` and accept it **only if `hasConfiguredAuth`**; on failure set
   `modelFallbackMessage = `Could not restore model ${provider}/${modelId}``.
7. Still no model → `findInitialModel({scopedModels: [], isContinuing: hasExistingSession,
   defaultProvider, defaultModelId, defaultThinkingLevel, modelRuntime})`; if it also fails,
   `modelFallbackMessage = formatNoModelsAvailableMessage()`; if it succeeds **and** a fallback
   message already existed, append `` `. Using ${model.provider}/${model.id}` ``.
8. Thinking level: explicit → else if `hasExistingSession` → `hasThinkingEntry ?
   existingSession.thinkingLevel : (settings.defaultThinkingLevel ?? "medium")` → else
   `settings.defaultThinkingLevel ?? "medium"`; then `model ? clampThinkingLevel(model, level)
   : "off"`.
9. Active tools (`:240-246`), quoted:
   ```ts
   const defaultActiveToolNames: ToolName[] = ["read", "bash", "edit", "write"];
   const allowedToolNames = options.tools ?? (options.noTools === "all" ? [] : undefined);
   const excludedToolNames = options.excludeTools;
   const excludedToolNameSet = excludedToolNames ? new Set(excludedToolNames) : undefined;
   const initialActiveToolNames: string[] = (
       options.tools ? [...options.tools] : options.noTools ? [] : defaultActiveToolNames
   ).filter((name) => !excludedToolNameSet?.has(name));
   ```
   Note `allowedToolNames` is `undefined` for `noTools === "builtin"` (so extension tools stay
   allowed) while `initialActiveToolNames` is `[]` for **either** `noTools` value. The four
   defaults match `CODING_TOOL_NAMES` in `crates/pirust-tools/src/lib.rs:168-173` exactly.
10. `new Agent({...})` (§10.2).
11. If `hasExistingSession`: `agent.state.messages = existingSession.messages` and, when there
    was no `thinking_level_change` entry, `sessionManager.appendThinkingLevelChange(level)`.
    Else: `appendModelChange(model.provider, model.id)` **if a model exists**, then
    `appendThinkingLevelChange(level)`. **This ordering is part of the session-JSONL byte
    contract** — a fresh session's first two entries are `model_change` then
    `thinking_level_change`.
12. `new AgentSession({...})` — the 109 kB class is **not** in feat-005 scope as a whole; feat-005
    needs only the subset print mode touches: `prompt(text, {images})`, `subscribe(listener)`,
    `state`, `model`, `thinkingLevel`, `waitForIdle()`, `sessionManager`, `setThinkingLevel`,
    `bindExtensions` (stub). Everything else (slash commands, compaction UI, tree navigation,
    `/model` cycling) is feat-007. Call this out in `sdk.rs` so nobody tries to port
    `agent-session.ts` wholesale.

### 10.4 `AgentSessionRuntime` (`core/agent-session-runtime.ts:74-…`)

A thin owner of `{session, services, diagnostics, modelFallbackMessage}` plus
`setRebindSession`, `dispose()`, `newSession`, `fork`, `switchSession`. Print mode uses
`session`, `setRebindSession`, `dispose`, and (only from extension command actions, all
stubbed) `newSession`/`fork`/`switchSession`. feat-005 can implement it as a struct with
`session` + `services` + `diagnostics` + `dispose()`; the session-replacement methods can be
`unimplemented!()`-adjacent stubs returning an error, because nothing in the headless path
reaches them once extensions are stubbed.

---

## 11. `core/session-manager.ts` — the coding-agent session store

> **This is NOT agent-core's `Session`/`SessionStorage`.** `packages/coding-agent` has its own
> 1623-line synchronous JSONL tree implementation with its own entry types
> (`SessionHeader`, `SessionEntry`, `:32-200`) and its own v1→v3 migration
> (`:227-292`). It never imports `SessionRepo`/`SessionStorage` (grep over
> `packages/coding-agent/src`: zero hits). The overlap with feat-003's
> `harness/session/*` is real but the two must stay separate, exactly as in Pi.

### 11.1 On-disk format

Line 1 is the header, `JSON.stringify(header)` + `\n`, key order
`type, version, id, timestamp, cwd, parentSession` (`:867-874`; `parentSession` omitted when
`undefined`). `version` is always `CURRENT_SESSION_VERSION = 3` (`:30`). Subsequent lines are
`SessionEntry`s whose key orders are given in `07-agent-core-spec.md §1.4`.

> **CORRECTION (found while porting `session.rs`; the earlier claim here was WRONG).**
> coding-agent's variants match agent-core's for only **seven** of the nine shared types —
> `message`, `thinking_level_change`, `model_change`, `compaction`, `branch_summary`,
> `label`, `session_info`. The other **two differ in key order** and are therefore a
> byte-compat trap:
> * `appendCustomEntry` (`:1051-1059`) emits `type, customType, data, id, parentId, timestamp`
> * `appendCustomMessageEntry` (`:1106-1115`) emits `type, customType, content, display, details, id, parentId, timestamp` (`details` omitted when undefined)
>
> Everything else — and all of agent-core — starts `type, id, parentId, timestamp`, as
> `tests/fixtures/pi/agent/entries.corpus.jsonl` proves. Consequence: `pirust-agent-core`'s
> `SessionTreeEntry` is **not** reusable as coding-agent's entry type, and coding-agent's
> store must be an order-preserving `serde_json::Value` (which also reproduces JS assignment
> order in the v1→v3 rewrite). `crates/pirust-coding-agent/tests/session_golden.rs` pins both
> orders against that corpus.

It has
**no** `active_tools_change` and **no** `leaf` entry (leaf is the last appended entry,
`_buildIndex`, `:889-908`). Entry ids are `randomUUID().slice(0, 8)` with a 100-attempt
collision check falling back to a full UUID (`generateId`, `:217-224`) — **not** uuidv7;
uuidv7 is only used for the *session* id (`createSessionId`, `:204-206`).

File name: `` `${timestamp.replace(/[:.]/g, "-")}_${sessionId}.jsonl` `` (`:883-884`), e.g.
`2025-12-08T22-41-05-306Z_0193f2b1-....jsonl`.

### 11.2 Persistence strategy (`_persist`, `:946-973`) — subtle, must be exact

```ts
_persist(entry: SessionEntry): void {
	if (!this.persist || !this.sessionFile) return;
	const hasAssistant = this.fileEntries.some((e) => e.type === "message" && e.message.role === "assistant");
	if (!hasAssistant) {
		if (this.flushed) { appendFileSync(this.sessionFile, `${JSON.stringify(entry)}\n`); }
		else { this.flushed = false; }
		return;
	}
	if (!this.flushed) {
		const fd = openSync(this.sessionFile, "wx");
		try { for (const e of this.fileEntries) { writeFileSync(fd, `${JSON.stringify(e)}\n`); } }
		finally { closeSync(fd); }
		this.flushed = true;
	} else { appendFileSync(this.sessionFile, `${JSON.stringify(entry)}\n`); }
}
```

I.e. **nothing is written to disk until the first assistant message exists**, at which point
the entire buffered tree (header included) is written with flag `"wx"` (exclusive create — a
pre-existing file makes this throw); afterwards every entry is appended individually. A
session that never gets an assistant reply leaves **no file at all**. This is why a `-p` run
that fails before the first response produces no session file, and it is directly observable in
the feat-005 live differential.

`_rewriteFile()` (`:910-920`) rewrites everything with flag `"w"`; used after a v1→v3
migration and when initializing an empty explicit `--session` file.

### 11.3 Construction / factories

| Factory | Behaviour |
|---|---|
| `create(cwd, sessionDir?, options?)` (`:1441-1444`) | dir = `normalizePath(sessionDir)` or `getDefaultSessionDir(cwd)`; fresh session |
| `open(path, sessionDir?, cwdOverride?)` (`:1452-1461`) | load entries; `cwd = cwdOverride ?? header?.cwd ?? process.cwd()`; dir = `normalizePath(sessionDir)` or `resolve(path, "..")` |
| `continueRecent(cwd, sessionDir?)` (`:1468-1476`) | `findMostRecentSession(dir, filterCwd ? cwd : undefined)` (`:572-592`); falls back to a fresh session |
| `inMemory(cwd?, options?)` (`:1479-1481`) | `persist = false`, `sessionDir = ""` |
| `forkFrom(sourcePath, targetCwd, sessionDir?, options?)` (`:1490-1541`) | validate source non-empty + header present (error strings in §3.6); write a new header with `parentSession: resolvedSourcePath` and `cwd: resolvedTargetCwd` using flag `"wx"`, then append every non-header source entry verbatim |
| `list(cwd, sessionDir?, onProgress?)` (`:1549-1558`) | `listSessionsFromDir(dir)`, filtered by `sessionCwdMatches` only when a **custom** sessionDir differs from the default path; sorted by `modified` **descending** |
| `listAll(sessionDir?/onProgress?)` (`:1564-…`) | custom dir → that dir only; else every immediate subdirectory of `getSessionsDir()`, `*.jsonl` only, ≤10 concurrent metadata loads (`:705-707`), sorted by `modified` descending |

`setSessionFile(path)` (`:826-859`) is the load path: existing file → `loadEntriesFromFile`; if
that returns `[]` **and** `statSync(path).size > 0` → throw
`` `Session file is not a valid pi session: ${path}` ``; if it returns `[]` and the file is
0 bytes → start a fresh session at that path and `_rewriteFile()`; otherwise take
`header?.id ?? createSessionId()`, run `migrateToCurrentVersion` (rewriting the file when it
changed anything), build the index, and set `flushed = true`. A **non-existent** path →
`newSession()` then restore the explicit path (`:854-858`).

`loadEntriesFromFile` (`:500-542`) streams the file in 1 MiB chunks through a `StringDecoder`,
skipping unparseable lines silently, then **validates that `entries[0]` is
`{type:"session", id:<string>}` and returns `[]` if not.** Rust: read with a
`BufReader`, `serde_json::from_str` per line, discard `Err`s.

### 11.4 v1 → v3 migration (`:227-292`)

`migrateToCurrentVersion(entries)` reads `header?.version ?? 1`; returns false when
`>= 3`; else runs `migrateV1ToV2` when `< 2` and `migrateV2ToV3` when `< 3` (both mutate in
place) and returns true.

- **v1→v2** (`:227-253`): sets the header's `version = 2`; then for each non-header entry
  assigns `id = generateId(ids)` and `parentId = prevId` (a **linear** chain, `prevId` starts
  `null`) — note `ids` is an empty set that is **never added to**, so the collision check is
  vacuous. For `compaction` entries with a numeric `firstKeptEntryIndex`, look up
  `entries[index]` and, if it is not the header, set `firstKeptEntryId = targetEntry.id`; then
  `delete comp.firstKeptEntryIndex`. **Order matters:** the lookup uses the target's *already
  assigned* id, so a forward reference (index > current position) reads `undefined`.
- **v2→v3** (`:256-271`): sets `version = 3`; renames any `message.role === "hookMessage"` to
  `"custom"`.

The two vendored v1 fixtures
(`packages/coding-agent/test/fixtures/{before-compaction,large-session}.jsonl`, see
`07-agent-core-spec.md §Exec-summary-3`) are the **oracle for this migration** — they are
coding-agent-owned v1 files and `migrateSessionEntries` is exactly what
`coding-agent/test/compaction.test.ts:38` applies to them.

### 11.5 `assertValidSessionId` (`:208-214`)

```ts
if (!/^[A-Za-z0-9](?:[A-Za-z0-9._-]*[A-Za-z0-9])?$/.test(id)) {
	throw new Error(
		"Session id must be non-empty, contain only alphanumeric characters, '-', '_', and '.', and start and end with an alphanumeric character",
	);
}
```

Single-character ids are valid (the group is optional). Called from `validateSessionIdFlags`
(`main.ts:236`), `newSession` (`:862-864`) and `forkFrom` (`:1514-1516`).

### 11.6 The `SessionRepo` implementations deferred from feat-003

Land in `pirust-agent-core` (the trait already exists at
`crates/pirust-agent-core/src/harness/types.rs:897`):

- `repo_utils.rs` ← `packages/agent/src/harness/session/repo-utils.ts` (51 lines):
  `create_session_id()` = uuidv7; `create_timestamp()` = ISO-8601;
  `to_session(storage)`; `get_file_system_result_or_throw(result, msg)` mapping a `FileError`
  to `SessionError` with code `not_found` when the file error code is `not_found` else
  `storage`, message `` `${message}: ${result.error.message}` ``;
  `get_entries_to_fork(storage, {entryId, position})` — no `entryId` → all entries; else load
  the target (missing → `SessionError("invalid_fork_target", `Entry ${id} not found`)`);
  `position ?? "before"`: `"at"` → leaf = target id, `"before"` → the target **must** be a
  `message` with `role === "user"` (else
  `SessionError("invalid_fork_target", `Entry ${id} is not a user message`)`) and leaf =
  `target.parentId`; then `storage.getPathToRoot(leaf)`.
- `jsonl_repo.rs` ← `jsonl-repo.ts` (179 lines): `encodeCwd` (the §7.7 expression), session dir
  `join(sessionsRoot, encodeCwd(cwd))`, file name
  `` `${timestamp.replace(/[:.]/g,"-")}_${id}.jsonl` ``, and `create`/`open`/`list`/`delete`/
  `fork`. `list` skips files whose metadata load throws `SessionError` with code
  `invalid_session` and **rethrows everything else**, then sorts by `createdAt` **descending**.
  `fork` inherits `parentSessionPath ?? sourceMetadata.path` and
  `metadata ?? sourceMetadata.metadata`. `open` on a missing path throws
  `SessionError("not_found", `Session not found: ${path}`)`.
- `memory_repo.rs` ← `memory-repo.ts`.

These are **not** on the headless critical path and can be built in parallel by a separate
subagent; nothing in `pirust-coding-agent` depends on them.

---

## 12. `@file` args + piped stdin → the initial message

### 12.1 `readPipedStdin` (`main.ts:58-75`)

```ts
async function readPipedStdin(): Promise<string | undefined> {
	if (process.stdin.isTTY) { return undefined; }
	return new Promise((resolve) => {
		let data = "";
		process.stdin.setEncoding("utf8");
		process.stdin.on("data", (chunk) => { data += chunk; });
		process.stdin.on("end", () => { resolve(data.trim() || undefined); });
		process.stdin.resume();
	});
}
```

Reads to EOF, **`.trim()`s** the whole payload, and maps an all-whitespace/empty payload to
`undefined`. `trim()` is JS `String.prototype.trim` (Unicode whitespace incl. BOM `﻿`) —
Rust `str::trim` differs on `\u{FEFF}`; strip a leading BOM explicitly. Called at
`main.ts:768-773` only when `appMode !== "rpc"` (rpc owns stdin).

### 12.2 `processFileArguments` (`cli/file-processor.ts:24-87`)

For each `@`-stripped arg, in order:
1. `absolutePath = resolve(resolveReadPath(fileArg, process.cwd()))` — `resolveReadPath`
   handles `~` and macOS screenshot Unicode spaces (already ported at
   `crates/pirust-tools/src/path_utils.rs`).
2. `access()` failure → stderr `` `Error: File not found: ${absolutePath}` `` + `exit(1)`.
3. `stat().size === 0` → **skipped silently** (`continue`).
4. Image (by sniffed mime type): `processImage(content, mimeType, {autoResizeImages})`;
   on failure append `` `<file name="${absolutePath}">${processed.message}</file>\n` `` and
   continue; on success push the `ImageContent` and append either
   `` `<file name="${absolutePath}">${processed.hints.join("\n")}</file>\n` `` (when there are
   hints) or `` `<file name="${absolutePath}"></file>\n` ``.
5. Text: `` `<file name="${absolutePath}">\n${content}\n</file>\n` ``; a read error →
   stderr `` `Error: Could not read file ${absolutePath}: ${message}` `` + `exit(1)`.

`autoResizeImages` comes from `settingsManager.getImageAutoResize()` (`main.ts:778`), default
`true` (resize to 2000×2000 max). **Image processing (`utils/image-process.ts`) is a
downstream dependency**: if it is not ported in feat-005, `@image.png` must produce the
failure-message form of step 4 rather than a panic — record that as a narrowing.

### 12.3 `buildInitialMessage` (`cli/initial-message.ts:20-43`), quoted in full

```ts
export function buildInitialMessage({ parsed, fileText, fileImages, stdinContent }: InitialMessageInput): InitialMessageResult {
	const parts: string[] = [];
	if (stdinContent !== undefined) { parts.push(stdinContent); }
	if (fileText) { parts.push(fileText); }

	if (parsed.messages.length > 0) {
		parts.push(parsed.messages[0]);
		parsed.messages.shift();
	}

	return {
		initialMessage: parts.length > 0 ? parts.join("") : undefined,
		initialImages: fileImages && fileImages.length > 0 ? fileImages : undefined,
	};
}
```

Order is **stdin, then file text, then the first CLI message**, joined with the **empty
string** (no separator — the `<file>` blocks already end in `\n`, but stdin does not, so
`echo hi | pi -p "world"` yields `initialMessage === "hiworld"`). `parsed.messages` is
**mutated**: the first element is removed, and the remainder is sent as follow-up prompts by
print mode. `prepareInitialMessage` (`main.ts:121-140`) short-circuits `processFileArguments`
when `fileArgs` is empty.

---

## 13. `modes/print-mode.ts` — quoted in full (159 lines)

```ts
/**
 * Print mode (single-shot): Send prompts, output result, exit.
 *
 * Used for:
 * - `pi -p "prompt"` - text output
 * - `pi --mode json "prompt"` - JSON event stream
 */

import type { AssistantMessage, ImageContent } from "@earendil-works/pi-ai";
import type { AgentSessionRuntime } from "../core/agent-session-runtime.ts";
import { flushRawStdout, writeRawStdout } from "../core/output-guard.ts";
import { killTrackedDetachedChildren } from "../utils/shell.ts";

/**
 * Options for print mode.
 */
export interface PrintModeOptions {
	/** Output mode: "text" for final response only, "json" for all events */
	mode: "text" | "json";
	/** Array of additional prompts to send after initialMessage */
	messages?: string[];
	/** First message to send (may contain @file content) */
	initialMessage?: string;
	/** Images to attach to the initial message */
	initialImages?: ImageContent[];
}

/**
 * Run in print (single-shot) mode.
 * Sends prompts to the agent and outputs the result.
 */
export async function runPrintMode(runtimeHost: AgentSessionRuntime, options: PrintModeOptions): Promise<number> {
	const { mode, messages = [], initialMessage, initialImages } = options;
	let exitCode = 0;
	let session = runtimeHost.session;
	let unsubscribe: (() => void) | undefined;
	let disposed = false;
	const signalCleanupHandlers: Array<() => void> = [];

	const disposeRuntime = async (): Promise<void> => {
		if (disposed) return;
		disposed = true;
		unsubscribe?.();
		await runtimeHost.dispose();
	};

	const registerSignalHandlers = (): void => {
		const signals: NodeJS.Signals[] = ["SIGTERM"];
		if (process.platform !== "win32") {
			signals.push("SIGHUP");
		}

		for (const signal of signals) {
			const handler = () => {
				killTrackedDetachedChildren();
				void disposeRuntime().finally(() => {
					process.exit(signal === "SIGHUP" ? 129 : 143);
				});
			};
			process.on(signal, handler);
			signalCleanupHandlers.push(() => process.off(signal, handler));
		}
	};

	registerSignalHandlers();

	runtimeHost.setRebindSession(async () => {
		await rebindSession();
	});

	const rebindSession = async (): Promise<void> => {
		session = runtimeHost.session;
		await session.bindExtensions({
			mode: mode === "json" ? "json" : "print",
			commandContextActions: {
				waitForIdle: () => session.waitForIdle(),
				newSession: async (newSessionOptions) => runtimeHost.newSession(newSessionOptions),
				fork: async (entryId, forkOptions) => {
					const result = await runtimeHost.fork(entryId, forkOptions);
					return { cancelled: result.cancelled };
				},
				navigateTree: async (targetId, navigateOptions) => {
					const result = await session.navigateTree(targetId, {
						summarize: navigateOptions?.summarize,
						customInstructions: navigateOptions?.customInstructions,
						replaceInstructions: navigateOptions?.replaceInstructions,
						label: navigateOptions?.label,
					});
					return { cancelled: result.cancelled };
				},
				switchSession: async (sessionPath, switchOptions) => {
					return runtimeHost.switchSession(sessionPath, switchOptions);
				},
				reload: async () => {
					await session.reload();
				},
			},
			onError: (err) => {
				console.error(`Extension error (${err.extensionPath}): ${err.error}`);
			},
		});

		unsubscribe?.();
		unsubscribe = session.subscribe((event) => {
			if (mode === "json") {
				writeRawStdout(`${JSON.stringify(event)}\n`);
			}
		});
	};

	try {
		if (mode === "json") {
			const header = session.sessionManager.getHeader();
			if (header) {
				writeRawStdout(`${JSON.stringify(header)}\n`);
			}
		}

		await rebindSession();

		if (initialMessage) {
			await session.prompt(initialMessage, { images: initialImages });
		}

		for (const message of messages) {
			await session.prompt(message);
		}

		if (mode === "text") {
			const state = session.state;
			const lastMessage = state.messages[state.messages.length - 1];

			if (lastMessage?.role === "assistant") {
				const assistantMsg = lastMessage as AssistantMessage;
				if (assistantMsg.stopReason === "error" || assistantMsg.stopReason === "aborted") {
					console.error(assistantMsg.errorMessage || `Request ${assistantMsg.stopReason}`);
					exitCode = 1;
				} else {
					for (const content of assistantMsg.content) {
						if (content.type === "text") {
							writeRawStdout(`${content.text}\n`);
						}
					}
				}
			}
		}

		return exitCode;
	} catch (error: unknown) {
		console.error(error instanceof Error ? error.message : String(error));
		return 1;
	} finally {
		for (const cleanup of signalCleanupHandlers) {
			cleanup();
		}
		await disposeRuntime();
		await flushRawStdout();
	}
}
```

### 13.1 Output shapes

**json mode** — newline-delimited JSON on **real stdout**, in this order:
1. **one** header line: `JSON.stringify(sessionManager.getHeader())` — the session header
   object (`{type:"session",version,id,timestamp,cwd[,parentSession]}`), emitted **only if a
   header exists** (always true in practice) and **before** any event.
2. one line per `AgentSessionEvent`, `JSON.stringify(event)`. The event union is
   `core/agent-session.ts:136-165`: every `AgentEvent` from agent-core except `agent_end`,
   plus a widened `agent_end` (`{type,messages,willRetry}`), `agent_settled`,
   `queue_update{steering,followUp}`, `compaction_start{reason}`,
   `compaction_end{reason,result,aborted,willRetry}`, `entry_appended{entry}`,
   `session_info_changed{name}`, `thinking_level_changed{level}`, and more (read the full union
   before implementing; feat-005 only needs the agent-core subset plus `entry_appended` and
   `agent_settled` to be emitted).
   **Byte contract:** `JSON.stringify` key order = the object-literal order at each event's
   construction site, and `undefined` fields are omitted — the same rule as
   `07-agent-core-spec.md §12`.
3. Nothing else. The text-mode block is skipped, so the final assistant text is **not**
   duplicated, and an errored final assistant message does **not** set `exitCode` in json mode.

**text mode** — nothing until the run finishes, then:
- if the last message in `session.state.messages` is an `assistant` message with
  `stopReason === "error" | "aborted"` → **stderr** gets
  `` assistantMsg.errorMessage || `Request ${stopReason}` `` and `exitCode = 1`;
- otherwise every `content` block with `type === "text"` is written to **stdout** as
  `` `${content.text}\n` `` (in block order; thinking/toolCall blocks are dropped);
- if the last message is not an assistant message, **no output at all** and exit 0.

### 13.2 Streams and exit codes

| Sink | Content |
|---|---|
| real stdout (`writeRawStdout`) | json event lines; text-mode assistant text |
| stderr | everything else: all diagnostics, the assistant error message, extension errors (`` `Extension error (${path}): ${err}` ``), the caught top-level error message, **and every `console.log` in the bootstrap** because `takeOverStdout` redirected `process.stdout.write` |
| exit | `0`; `1` on assistant error/aborted (text mode only) or a thrown error; `143` SIGTERM / `129` SIGHUP |

`main.ts:846-857` assigns the result to `process.exitCode` **only when non-zero** and returns
normally (so Node drains its streams), after `stopThemeWatcher()` and `restoreStdout()`.

### 13.3 `takeOverStdout` / `writeRawStdout` (`core/output-guard.ts`)

- `takeOverStdout()` (`:45-70`) — idempotent. Captures `rawStdoutWrite` (bound
  `process.stdout.write`), `rawStderrWrite` (bound `process.stderr.write`) and the original
  `process.stdout.write`, then **replaces `process.stdout.write` with a function that writes to
  stderr**, forwarding the callback whether it arrived as arg 2 or arg 3.
- `restoreStdout()` (`:72-79`) puts the original back and clears the state.
- `isStdoutTakenOver()` (`:81-83`).
- `writeRawStdout(text)` (`:85-93`) — the **only** way to reach real stdout while taken over.
  Empty string is a no-op. Chains onto a module-level promise tail
  (`rawStdoutWriteTail = rawStdoutWriteTail.then(...)`) so writes are **strictly ordered and
  never interleaved**, and a failure calls `process.exit(1)`.
- `writeRawStdoutChunk` (`:20-43`) retries forever on `ENOBUFS`/`EAGAIN`/`EWOULDBLOCK` with a
  10 ms delay (`RAW_STDOUT_RETRY_DELAY_MS`), rethrowing anything else.
- `waitForRawStdoutBackpressure()` (`:95-103`) awaits the tail until it stops changing;
  `flushRawStdout()` (`:105-108`) does that then writes an empty chunk (a
  `write("")` with a callback = a drain barrier).

**Rust mapping.** There is no `process.stdout.write` to monkey-patch, so the takeover becomes a
**policy**: a process-global `OutputGuard` with a flag; all "console" output in the ported code
goes through helpers `log()`/`warn()`/`error()` where `log()` writes to stderr when the guard
is engaged and to stdout otherwise, and `write_raw_stdout()` always writes to the real stdout
behind a `tokio::sync::Mutex` (the promise-tail equivalent) with the same
`ENOBUFS`/`EAGAIN`/`EWOULDBLOCK` retry loop (`ErrorKind::WouldBlock` +
`raw_os_error()` checks). Every `console.log` in `main.ts`/`migrations.ts`/`list-models.ts`
must route through `log()` — a direct `println!` would break the contract.

---

## 14. The system prompt (`core/system-prompt.ts:28-162`)

Two shapes, selected by `customPrompt` (i.e. `--system-prompt`, threaded through the resource
loader as `resourceLoaderOptions.systemPrompt`, `main.ts:673`).

**Custom-prompt branch (`:46-72`):** `prompt = customPrompt`; then, in order,
`appendSection` (= `` `\n\n${appendSystemPrompt}` `` when non-empty), the `<project_context>`
block, the skills block (only when `!selectedTools || selectedTools.includes("read")`), and
finally `` `\nCurrent working directory: ${promptCwd}` ``.

**Default branch (`:74-161`):**
1. `promptCwd = cwd.replace(/\\/g, "/")` (`:39`) — **backslashes become forward slashes**, so
   the Windows cwd is emitted POSIX-style.
2. `tools = selectedTools || ["read","bash","edit","write"]`;
   `visibleTools = tools.filter(name => !!toolSnippets?.[name])`;
   `toolsList = visibleTools.length > 0 ? visibleTools.map(n => `- ${n}: ${toolSnippets[n]}`).join("\n") : "(none)"`
   — **a tool appears in "Available tools" only when the caller supplies a one-line snippet**.
3. Guidelines: de-duplicated, insertion-ordered. `Use bash for file operations like ls, rg, find`
   is added **only** when bash is present and grep/find/ls are all absent; then each
   `promptGuidelines` entry (trimmed, non-empty); then always
   `Be concise in your responses` and `Show file paths clearly when working with files`.
   Rendered as `- ` bullets joined with `\n`.
4. The fixed body (`:121-138`) — a 9-line literal starting
   `You are an expert coding assistant operating inside pi, a coding agent harness.` and
   interpolating `toolsList`, `guidelines`, and **three package paths**
   (`getReadmePath()`, `getDocsPath()`, `getExamplesPath()`).
5. `appendSection`, `<project_context>`, skills (when `read` is in `tools`),
   `` `\nCurrent working directory: ${promptCwd}` ``.

`<project_context>` block (`:145-152`, identical in both branches):

```
\n\n<project_context>\n\nProject-specific instructions and guidelines:\n\n
  for each file: <project_instructions path="${filePath}">\n${content}\n</project_instructions>\n\n
</project_context>\n
```

**pirust decisions to make explicit in `system_prompt.rs`:**
- The literal at `:121-138` mentions "pi" by name and points at Pi's shipped docs. Keep the
  wording **verbatim** (it is part of model behaviour, and the live differential compares
  outputs), and substitute the three paths with pirust's package paths — or, if pirust ships no
  docs, with the same strings Pi would produce for a source checkout. Record whichever choice
  is made; do not silently change the prose.
- `contextFiles` and `skills` are always empty in feat-005 (resource loader stubbed), so both
  optional blocks are omitted and only `appendSection` + the cwd line are dynamic.
- `toolSnippets` comes from the tool registry; with feat-004's tools the four default names
  each have a snippet, so `toolsList` is four `- name: description` lines.

---

## 15. `main()` order of operations (`main.ts:473-859`)

The sequence is observable (it decides which errors are reported first and which side effects
happen before an early exit). Ported steps only; skipped steps are marked.

| # | Line | Step |
|---|---|---|
| 1 | 474-475 | `resetTimings()`; assemble extension factories — **stub** |
| 2 | 476-480 | `offlineMode = args.includes("--offline") \|\| isTruthyEnvFlag(env.PI_OFFLINE)`; if set, **write back** `PI_OFFLINE="1"` and `PI_SKIP_VERSION_CHECK="1"` into the environment |
| 3 | 482-484 | Windows quarantine cleanup — **not ported** |
| 4 | 486-490 | `cwd = process.cwd()`; `agentDir = getAgentDir()`; a **bootstrap** `SettingsManager.create(cwd, agentDir, {projectTrusted: false})`; http proxy/dispatcher — **not ported** (but the `projectTrusted:false` bootstrap manager IS: it is why project settings can never influence proxy setup) |
| 5 | 492-507 | package/config commands — **not ported** |
| 6 | 509-519 | `parseArgs`; print diagnostics; `exit(1)` if any error |
| 7 | 521-524 | `--version` → stdout `VERSION`, `exit(0)` |
| 8 | 526-538 | `--export` → **not ported**; keep the flag parsed and emit `Error: HTML export is not supported` + `exit(1)`, documented as a narrowing |
| 9 | 540-544 | `resolveAppMode`; `takeOverStdout()` unless interactive or a plain metadata command |
| 10 | 546-549 | rpc + `@file` guard — **feat-012** |
| 11 | 551-552 | `validateForkFlags`, `validateSessionIdFlags` |
| 12 | 555-556 | `runMigrations(cwd)` |
| 13 | 558-559 | startup `SettingsManager.create(cwd, agentDir)` (**trusted** by default) + drain its errors as `(startup session lookup, <scope> settings) …` warnings |
| 14 | 563-566 | first-time setup — **feat-007** |
| 15 | 573-578 | resolve `sessionDir` (§5.3), then `createSessionManager(parsed, cwd, sessionDir, startupSettingsManager)` |
| 16 | 579-591 | missing-session-cwd check: interactive prompts, **non-interactive prints the error and `exit(1)`** |
| 17 | 592-599 | `--name`: `trim()`; empty → `exit(1)`; else `appendSessionInfo(name)` |
| 18 | 602-613 | trust store + `autoTrustOnReloadCwd` + `trustPromptMode` (`"print"` when `--help`/`--list-models`, else `appMode`) + `resolveCliPaths` for the four resource path lists (`isLocalPath(v) ? resolvePath(v, cwd) : v`, `main.ts:455-457`) |
| 19 | 615-739 | the `createRuntime` factory (services → scoped models → `buildSessionOptions` → `--api-key` → `createAgentSessionFromServices` → forced `setThinkingLevel`) |
| 20 | 741-750 | `createAgentSessionRuntime(createRuntime, {cwd: sessionManager.getCwd(), agentDir, sessionManager})` |
| 21 | 752-758 | **`--help` finally prints here** and `exit(0)` — after the whole runtime exists |
| 22 | 760-764 | `--list-models` → `listModels(modelRuntime, pattern)` and `exit(0)` |
| 23 | 766-774 | read piped stdin (skipped for rpc); downgrade interactive→print if content arrived |
| 24 | 776-781 | `prepareInitialMessage` (@files + stdin + `messages[0]`) |
| 25 | 782-788 | theme init + deprecation warnings — **feat-006/007** |
| 26 | 791-797 | `reportDiagnostics(runtime.diagnostics)`; any error → optional extension hint + `exit(1)` |
| 27 | 800-803 | non-interactive with no model → no-models message + `exit(1)` |
| 28 | 805-809 | `PI_STARTUP_BENCHMARK` guard — **not ported** (keep the error string if the env var is honoured at all; simplest is to drop both) |
| 29 | 811-858 | dispatch: rpc (**feat-012**) / interactive (**feat-007**) / else `runPrintMode(runtime, {mode: toPrintOutputMode(appMode), messages: parsed.messages, initialMessage, initialImages})`, then `stopThemeWatcher()`, `restoreStdout()`, and `process.exitCode = exitCode` when non-zero |

`createSessionManager` (`main.ts:264-355`) branch order — **first match wins**:
`noSession || help || listModels !== undefined` → `inMemory` · `fork` · `session` · `resume` ·
`continue` · `sessionId` (open existing, else warn + create) · `create`. Note that
`--help` and `--list-models` force an in-memory session, so they never touch the sessions
directory; and `--session-id` is honoured in the `inMemory` branch too
(`{id: parsed.sessionId}`).

---

## 16. § Byte-exactness hazards

A numbered list; each item is a defect if missed.

1. **No `--flag=value` for known flags.** Every known-flag branch is an exact `===` on the
   token, so `--mode=json` never matches branch 3 — it falls to the unknown-long-flag branch
   and becomes `unknownFlags["mode"] = "json"`, which `applyExtensionFlagValues` later turns
   into the fatal `Unknown option: --mode`. (`cli/args.ts:78` vs `:188-191`.)
2. **A value-taking flag as the last token is tolerated** and silently becomes an unknown
   boolean flag: `pi --model` → `unknownFlags["model"] = true` (the `i+1 < args.length` guard
   fails, the chain falls through to `:188`). The two exceptions are `--name`/`-n`, which push
   the error `--name requires a value` (`:98-103`), and the single-dash aliases `-t`/`-xt`/`-e`,
   which reach the `:202` branch and produce `Unknown option: -t` etc.
3. **Values are consumed blindly.** `result.provider = args[++i]` with no validation, so
   `pi --provider --model foo` sets `provider = "--model"` and leaves `foo` as a message
   (`:87-88`).
4. **`-p`'s `---` lookahead clause.** `(!next.startsWith("-") || next.startsWith("---"))`
   (`:143`) — a token starting with exactly three or more dashes IS taken as the prompt.
   `pi -p ---x` → `messages == ["---x"]`. `--list-models` has no such clause (`:173`).
5. **No `--` end-of-options handling anywhere.** `pi -- "hello"` →
   `unknownFlags == {"" => "hello"}` and `messages == []` (`:193-197`, `flagName = "".slice(2)`).
   A bare `--=v` gives key `""` value `"v"` (`:191`).
6. **Unknown flags greedily eat the next token** unless it starts with `-` or `@` (`:195-197`),
   so `pi --foo bar` loses `bar` as a message.
7. **`--models` keeps empty segments, `--tools`/`--exclude-tools` filter them.**
   `--models "a,,b"` → `["a","","b"]` (`:115`, no `.filter`), while `--tools "a,,b"` →
   `["a","b"]` (`:121-124`). A downstream glob resolution of `""` then produces
   `No models match pattern ""`.
8. **`-v` is `--version`, not `-V`.** (`:76`.) There is no `-V`. The current Rust scaffold
   accepts `-V` (`crates/pirust-coding-agent/src/main.rs:13`) — **that is a bug to remove.**
   Version also **beats help**: both flags can be set, but `main.ts:521` handles `version`
   before the help block at `:752`.
9. **`--help` exits LATE**, after migrations, the session manager, the model runtime and the
   whole `AgentSession` have been constructed (`main.ts:752-758`). Side effects of a
   `pi --help` run are real: migrations execute, `auth.json` and its parent directory are
   created, model availability is probed. Reproduce the ordering, not just the output.
10. **`--mode text` falls through to the TTY logic** (`resolveAppMode` matches only `rpc` and
    `json`, `main.ts:100-111`), so `pi --mode text` on a TTY without `-p` is **interactive**.
11. **Piped stdin ⇒ print mode.** *Recon correction:* the effective mechanism is
    `resolveAppMode`'s `!stdinIsTTY → "print"` (`main.ts:107`), evaluated at line 540. The
    explicit downgrade at `main.ts:770-772` (`if (stdinContent !== undefined && appMode ===
    "interactive") appMode = "print"`) is **unreachable**, because `readPipedStdin` returns
    `undefined` whenever `process.stdin.isTTY` (`main.ts:60-62`) and `appMode ===
    "interactive"` already implies `stdin.isTTY`. Port both (the second as a defensive no-op)
    but do not rely on the downgrade for correctness.
12. **`takeOverStdout` sends stdout to stderr**, with `--help`/`--list-models` exempt **only
    when** `!parsed.print && parsed.mode === undefined` (`isPlainRuntimeMetadataCommand`,
    `main.ts:117-119`). So `pi -p --help` and `pi --mode json --list-models` print their output
    to **stderr**. Every `console.log` in the bootstrap is affected, not just help.
13. **`JSON.stringify(x, null, 2)` with no trailing newline** for `settings.json`, `auth.json`,
    `models-store.json`; **with** a trailing newline for `keybindings.json`
    (`migrations.ts:168`) and `trust.json` (`core/trust-manager.ts:133`). Session JSONL lines
    are compact `JSON.stringify(entry)` + `\n`.
14. **`trust.json` keys are sorted** before writing (`core/trust-manager.ts:125-131`) and
    `null` values are written as `null` (they mean "no decision"), while `undefined` entries are
    dropped. Keys are `canonicalizePath(resolvePath(cwd))` — **symlinks resolved**, unlike
    session cwds.
15. **Session entry ids are `randomUUID().slice(0,8)`** (8 lowercase hex chars, dashes
    possible only if the slice crosses one — it does not, since a UUID's first dash is at index
    8), NOT uuidv7. The session **id** is uuidv7. (`core/session-manager.ts:204-224`.)
16. **Nothing is written to the session file until the first assistant message** (§11.2), and
    the first write uses flag `"wx"` (exclusive). A pre-existing file at that path makes the
    write throw.
17. **`.padEnd(32)` in the help text** is UTF-16-unit based; the two env var names are ASCII, so
    `format!("{:<32}", s)` matches — but if the identity is ever renamed to something
    non-ASCII this diverges.
18. **`localeCompare` in model sorting/listing** (`core/model-resolver.ts:148,152`,
    `cli/list-models.ts:55-58`) is ICU collation, not byte order. Use `str::cmp` and document
    the divergence; it is unobservable for the ASCII ids in the catalog.
19. **`readPipedStdin` uses JS `.trim()`**, which strips `\u{FEFF}` (BOM) in addition to
    Unicode whitespace; Rust `str::trim` does not. Strip the BOM explicitly
    (`main.ts:71`).
20. **`buildInitialMessage` joins with the empty string** (`cli/initial-message.ts:40`) — no
    separator between stdin content, file text and the first message.
21. **`buildInitialMessage` mutates `parsed.messages`** via `shift()` (`:36`); the remaining
    messages are the follow-up prompts. Do not copy the vector before this point.
22. **Empty `@file` args are skipped silently** (`cli/file-processor.ts:43-46`), while a missing
    one is fatal.
23. **`deepMergeSettings` is one level deep and arrays replace** (§6.3) — the doc comment
    saying "recursively" is wrong; the runtime wins.
24. **`migrateSettings` guards on key presence, not definedness**, and its `delete`s sit
    outside the inner `if` for `retry.maxDelayMs` (§6.4).
25. **`defaultProjectTrust` is read from `globalSettings`, not the merged view**
    (`core/settings-manager.ts:900`) — a project cannot grant itself trust.
26. **`getTheme()` returns `undefined` when the theme name contains `/`**
    (`core/settings-manager.ts:729-732`) while `getThemeSetting()` returns it raw.
27. **`getSteeringMode`/`getFollowUpMode` use `||`, not `??`** (`:704,714`), so an empty string
    falls back to `"one-at-a-time"`; `getTransport` uses `??` (`:751`), so `""` would be
    honoured. Same-looking getters, different semantics.
28. **`httpIdleTimeoutMs === 0` becomes `2147483647`** in the stream options
    (`core/sdk.ts:300-302`), not "no timeout".
29. **`process.env.PI_OFFLINE === undefined`** is the offline test inside `ModelRuntime`
    (`core/model-runtime.ts:152`) — so `PIRUST_OFFLINE=0` **disables** the network, whereas
    `isTruthyEnvFlag` in `main.ts:95-98` accepts only `1`/`true`/`yes` (case-insensitive on the
    latter two). Two different truthiness rules for the same variable; port both.
30. **`--api-key` mutates the model runtime, not the persisted `auth.json`**
    (`setRuntimeApiKey`, `main.ts:712`) and requires a resolved model first.
31. **The session-dir encoding is string surgery, not path logic** (§7.7): one leading
    separator stripped, then all of `/ \ :` → `-`, then wrapped. `C:\Users\me\proj` →
    `--C--Users-me-proj--` (note the double dash from `:` + `\`).
32. **`--fork` conflict list order** is `--session, --continue, --resume, --no-session`;
    `--session-id`'s is `--session, --continue, --resume` (`main.ts:208-213,224-228`). The
    joined string is part of the error message.
33. **`m.name?.toLowerCase().includes(...)`** participates in fuzzy model matching
    (`core/model-resolver.ts:135`) — matching on the display name, not just the id.
34. **`buildFallbackModel` clones a real model and overwrites `id`+`name`**
    (`core/model-resolver.ts:164-178`), inheriting `baseUrl`/`cost`/`contextWindow` from the
    provider's default model. The synthesized model is indistinguishable from a catalogued one
    downstream — including in the `model_change` session entry.
35. **The bootstrap `SettingsManager` is created with `projectTrusted: false`**
    (`main.ts:488`) and the startup one with the default `true` (`main.ts:558`). Two managers,
    two trust postures, in the same run.

---

## 17. § Headless hazards (paths that would BLOCK a non-interactive run)

1. **`--resume` builds a TUI unconditionally.** `createSessionManager` (`main.ts:321-336`)
   calls `selectSession(...)` regardless of `appMode`, and `selectSession`
   (`cli/session-picker.ts:20-54`) does `await createStartupTui(settingsManager)` →
   `SessionSelectorComponent` → `startStartupTui(...)`, resolving only from a keypress
   callback. With stdin redirected this never resolves. **feat-005 must make `--resume` fail
   fast in non-interactive modes** — e.g. `Error: --resume requires an interactive terminal`
   + `exit(1)` — and record it as an intentional divergence (Pi hangs / misbehaves here). The
   `finally { stopThemeWatcher(); }` around it (`:333-335`) is feat-006's concern.
2. **The `--session` "global" branch prompts on stdin.** `main.ts:305-313`: when the id
   resolves to a session in a *different* project it logs `Session found in different project:
   <cwd>` and calls `promptConfirm("Fork this session into current directory?")`
   (`main.ts:192-203`), a `readline` question on `process.stdin`/`process.stdout`. In print mode
   stdout is already redirected to stderr, and stdin is either a pipe (EOF → answer `""` →
   treated as "no" → `Aborted.` + `exit(0)`) or a TTY. **Port with an explicit non-interactive
   rule:** when `appMode !== "interactive"`, do not prompt — treat it as declined and exit 0
   with `Aborted.`, which is what EOF already produces in Pi. Document it.
3. **`showDeprecationWarnings` blocks on a raw-mode keypress** (`migrations.ts:288-296`). It is
   already gated on `appMode === "interactive"` (`main.ts:786`) — keep that gate; never call it
   from the headless path.
4. **`promptForMissingSessionCwd`** (`main.ts:459-467`) is likewise gated: non-interactive
   prints `MissingSessionCwdError` and exits 1 (`main.ts:587-590`). Correct as-is.
5. **`cli/project-trust.ts` already degrades correctly.** `createProjectTrustContext`
   (`:7-62`) returns a `ProjectTrustContext` whose `ui.select`/`ui.input` return `undefined` and
   whose `ui.confirm` returns `false` when `!hasUI` **or** `mode !== "interactive"`, and whose
   `ui.notify` writes to **stderr** (`console.error`) in every non-interactive mode. `mode` is
   mapped `"interactive" → "tui"`, otherwise passed through (`print`/`json`/`rpc`).
   Downstream, `resolveProjectTrusted` (`core/project-trust.ts:46-96`) short-circuits on
   `trustOverride` (`--approve`/`--no-approve`), returns `true` when
   `!hasTrustRequiringProjectResources(cwd)`, consults the trust store, then
   `defaultProjectTrust` (`always`→true, `never`→false), and finally
   `if (!projectTrustContext.hasUI) return false`. **So an untrusted project in headless mode
   silently loads no project settings/resources** — no prompt, no hang. That is the behaviour
   to reproduce.
6. **`hasTrustRequiringProjectResources`** (`core/trust-manager.ts:184-206`) walks **all
   ancestors** looking for `<dir>/.agents/skills` (excluding the user's own
   `~/.agents/skills`) in addition to checking `<cwd>/.pi/{settings.json, extensions, skills,
   prompts, themes, SYSTEM.md, APPEND_SYSTEM.md}`. It is called at `main.ts:605,625` and gates
   whether the trust machinery runs at all. Port it; a wrong answer here changes whether project
   settings are read.
7. **`AuthStorage` construction creates files.** `ensureParentDir`/`ensureFileExists`
   (`core/auth-storage.ts:35-47`) run before every lock, so even `pirust --help` creates
   `~/.pirust/agent/auth.json` containing `{}` at mode `0600`. Harmless, but tests must expect
   it.
8. **`ModelRuntime.create` performs a network refresh by default** with a 15 s abort timeout
   (`core/model-runtime.ts:156-164`). In a hermetic test run, `PIRUST_OFFLINE=1` must be set or
   the remote-catalog fetch must be absent (pirust does not port `withRemoteCatalog`, so this is
   moot — but keep `allow_model_network` as a flag so the semantics survive).

---

## 18. § Deferred

| Item | Where it goes |
|---|---|
| `--mode rpc`, `modes/rpc/*`, `rpc-entry.ts`, the `@file`-in-RPC error | **feat-012** |
| pi-tui primitives (`fuzzyFilter`, startup TUI, session selector) | **feat-006** |
| Interactive mode, first-time setup, startup selector/input, theme init + watcher, `--verbose`, `PI_STARTUP_BENCHMARK`, session picker (`--resume`'s TUI) | **feat-007** (with feat-006) |
| Extensions: `ExtensionRunner`, the ~30 lifecycle events, tool/command/shortcut/flag/provider registration, `builtInExtensions`, `applyExtensionFlagValues`' real behaviour, extension `--help` flags, `before_provider_*` / `after_provider_response` / `input` / `context` hooks | **feat-007** (stubbed here per §1.2) |
| Resource loader: skills, prompt templates, themes, keybindings map, slash commands, `AGENTS.md`/`CLAUDE.md` context files, `SYSTEM.md`/`APPEND_SYSTEM.md` | **feat-007** (stubbed here) |
| `SessionRepo` impls in agent-core (`repo_utils`, `jsonl_repo`, `memory_repo`) | **feat-005, parallel side-task** (§11.6) |
| `package-manager-cli.ts` verbs, HTML export, telemetry/analytics, self-update + Windows quarantine, HTTP proxy + undici dispatcher, `getShareViewerUrl` | **not ported** |
| `AgentSession`'s non-headless surface (compaction UI, `/tree`, `/model` cycling, scoped-model Ctrl+P, cache-stats, footer data) | **feat-007** |
| Providers other than `anthropic`; `radius` oauth; remote model catalogs; `models-store.json` refresh | later feat (feat-008) |

---

## 19. Oracles and acceptance

**Pure-layer goldens** (all offline, no network, no `pi` process needed at test time):

| Target | Oracle |
|---|---|
| `parse_args` | A table of ~60 argv vectors → expected `Args` (JSON snapshot). Must include every §16 hazard: `--mode=json`, trailing `--model`, `-t` alone, `-p ---x`, `-- hello`, `--foo bar`, `--models "a,,b"` vs `--tools "a,,b"`, `-v --help`, a bare `-`, `@--foo`. Generate the expected values by running Pi's `parseArgs` in Node and dumping the result (`Map` → array of pairs to preserve order). |
| `render_help` | `tests/fixtures/pi/cli/help.plain.golden` (§4.3 — **must be captured**) |
| `resolve_app_mode` | 4 flags × 2 TTY bits × `--print` truth table (24 cases) |
| config paths / tilde | `PIRUST_CODING_AGENT_DIR` set/unset × `~`, `~/x`, `~x`, `file://…`, absolute, relative |
| `deep_merge_settings` | The 6 cases in §6.3's table, plus the two-level `retry.provider` loss case |
| `migrate_settings` | 4 migrations × (applies / blocked-by-existing-key / malformed input) |
| migrations M1–M5 | tempdir fixtures: legacy `oauth.json` + `settings.json.apiKeys`; a stray `<agentDir>/*.jsonl` with a header cwd; `tools/{fd,rg,other}`; `commands/` with and without `prompts/`; `hooks/`. Assert file moves, contents, modes and the exact stdout strings |
| session-dir encoding | the 6 rows of §7.7, incl. `C:\Users\me\proj` |
| session JSONL | Drive `SessionManager` with fixed ids/timestamps (inject the id/clock sources) and diff bytes against a Node-generated file; plus the two vendored v1 fixtures through the v1→v3 migration |
| `parse_model_pattern` / `resolve_cli_model` | A synthetic catalogue of ~10 models covering: alias vs dated, `provider/id` ambiguity, `:high` suffix, `:bogus` suffix under both `allowInvalidThinkingLevelFallback` values, OpenRouter-style `a/b:c` ids, and the `buildFallbackModel` path |
| `print_mode` | Faux provider (`crates/pirust-ai/src/providers/faux.rs`) driving a scripted response; assert the exact stdout bytes for text mode and the NDJSON line sequence for json mode, plus stderr and exit code for the error/aborted case |

**Live differential (the acceptance gate from `feature_list.json` feat-005):** run real
`pi -p "<prompt>"` and `pirust -p "<prompt>"` against the same endpoint with the same
`models.json`/`auth.json` content (Pi's under `~/.pi`, pirust's under `~/.pirust`), then compare
(a) stdout byte-for-byte, (b) the session JSONL each wrote — modulo the known-variable fields
(session id, entry ids, timestamps, and the assistant response itself). Do the same with
`--mode json` and compare the event-line **shape** (key order per event type) rather than
values. Remember §11.2: if the run fails before the first assistant message, **neither** side
writes a session file.

---

## Appendix: file inventory (source of truth)

`packages/coding-agent/src/`: `cli.ts` (20), `rpc-entry.ts` (12, feat-012), `main.ts` (859),
`config.ts` (566), `migrations.ts` (315), `index.ts`, `package-manager-cli.ts` (not ported).
`src/cli/`: `args.ts` (390), `file-processor.ts` (87), `initial-message.ts` (43),
`list-models.ts` (111), `project-trust.ts` (62), `session-picker.ts` (55, feat-007),
`startup-ui.ts` (feat-007), `config-selector.ts` (not ported).
`src/core/`: `sdk.ts` (393), `settings-manager.ts` (1234), `session-manager.ts` (1623),
`auth-storage.ts` (271), `models-store.ts` (57), `model-config.ts` (287),
`model-runtime.ts` (587), `model-resolver.ts` (705), `model-registry.ts` (145, extension
facade — feat-007), `agent-session-services.ts` (219), `agent-session-runtime.ts` (438),
`agent-session.ts` (3283, subset only), `system-prompt.ts` (162), `output-guard.ts` (108),
`project-trust.ts` (96), `trust-manager.ts` (244), `session-cwd.ts` (59),
`auth-guidance.ts` (25), `defaults.ts` (3), `resolve-config-value.ts` (subset),
`messages.ts`, `keybindings.ts` (feat-007), `resource-loader.ts` (feat-007),
`http-dispatcher.ts` / `provider-composer.ts` / `remote-catalog-provider.ts` /
`telemetry.ts` / `timings.ts` / `export-html/` (not ported).
`src/modes/`: `index.ts` (15), `print-mode.ts` (159), `interactive/` (feat-007),
`rpc/` (feat-012).
`src/utils/`: `paths.ts` (118, subset), `image-process.ts` + `mime.ts` (§12.2),
`shell.ts` (`killTrackedDetachedChildren`), `json.ts` (`stripJsonComments`),
`windows-self-update.ts` / `child-process.ts` (not ported).
Cross-package: `packages/ai/src/auth/types.ts:17-43` (`Credential`);
`packages/agent/src/harness/session/{repo-utils,jsonl-repo,memory-repo}.ts` (§11.6).
Fixtures: `packages/coding-agent/test/fixtures/{before-compaction,large-session}.jsonl` (v1
session oracle).
Rust targets: `crates/pirust-coding-agent/src/*` (new), `crates/pirust-agent-core/src/agent.rs`
(`AgentOptions`), `crates/pirust-agent-core/src/harness/{types.rs,session/}`,
`crates/pirust-ai/src/{api/anthropic_messages.rs,types/model.rs,auth/mod.rs}`,
`crates/pirust-tools/src/lib.rs` (tool registry), `crates/pirust-tools/src/binaries.rs:154-182`
(identity constants — **reuse, do not redeclare**).
