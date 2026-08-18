//! Port of `migrations.ts` — the 5 one-time startup migrations plus `runMigrations`.
//!
//! M2 (`migrateSessionsFromAgentRoot`) is effectively a no-op on Windows because of
//! `file.split("/").pop() || file.split("\\").pop()`; ported literally, not "fixed".
//! Gated by `tests/fixtures/pi/cli/migrations.cases.jsonl`.
//!
//! # What is ported
//!
//! | Pi | here |
//! |---|---|
//! | `migrateAuthToAuthJson` (`migrations.ts:21-73`) | [`migrate_auth_to_auth_json`] |
//! | `migrateSessionsFromAgentRoot` (`migrations.ts:84-131`) | [`migrate_sessions_from_agent_root`] |
//! | `migrateCommandsToPrompts` (`migrations.ts:137-155`) | `migrate_commands_to_prompts` (private) |
//! | `migrateKeybindingsConfigFile` (`migrations.ts:157-172`) | [`migrate_keybindings_config_file`] |
//! | `migrateToolsToBin` (`migrations.ts:177-216`) | [`migrate_tools_to_bin`] |
//! | `checkDeprecatedExtensionDirs` (`migrations.ts:222-252`) | `check_deprecated_extension_dirs` (private) |
//! | `migrateExtensionSystem` (`migrations.ts:257-272`) | [`migrate_extension_system`] |
//! | `showDeprecationWarnings` (`migrations.ts:277-298`) | [`show_deprecation_warnings`] (minus the keypress wait) |
//! | `runMigrations` (`migrations.ts:305-315`) | [`run_migrations`] |
//! | the `safePath` encoding (`migrations.ts:112`) | [`encode_session_dir_name`] |
//! | `MIGRATION_GUIDE_URL` / `EXTENSIONS_DOC_URL` (`migrations.ts:11-14`) | [`MIGRATION_GUIDE_URL`] / [`EXTENSIONS_DOC_URL`] |
//!
//! # The two seams Pi does not have
//!
//! - **`getAgentDir()` → [`ConfigEnv`].** Every migration in Pi calls the global
//!   `getAgentDir()` (and M5 the global `CONFIG_DIR_NAME`). Here the ambience is a value,
//!   so each function takes `&ConfigEnv`: the golden suite points it at a `tempfile` dir
//!   instead of mutating `$PIRUST_CODING_AGENT_DIR`, and M5's project dir is
//!   `join(cwd, env.identity.config_dir_name)` — `.pi` under [`crate::config::PI`],
//!   `.pirust` in production.
//! - **`console.log(chalk.…)` → [`MigrationConsole`].** M3 and M5 are the only migrations
//!   that write to stdout; both take a sink so the fixture's `console` array can be
//!   compared line for line. [`StdoutConsole`] is production ([`ConsoleStyle`] carries
//!   chalk's colour choice for the presentation layer to apply — nothing here emits ANSI).
//!
//! # Error handling, exactly as Pi has it
//!
//! Pi swallows *most* failures (`catch {}` with a comment), but three writes are
//! deliberately **uncaught** and abort startup. [`MigrationError`] exists for those:
//!
//! | Pi site | uncaught? |
//! |---|---|
//! | `mkdirSync`/`writeFileSync` of `auth.json` (`migrations.ts:68-69`) | **yes** — propagates |
//! | `mkdirSync(binDir)` (`migrations.ts:193`) | **yes** — propagates |
//! | everything else (parse, rename, rm, readdir, the keybindings write) | no — swallowed |
//!
//! `runMigrations` itself has no try/catch (`migrations.ts:305-315`), so those three plus a
//! throwing `getAgentDir()` are the only ways it can fail. The `Error.message` text differs
//! from Node's (`EACCES: permission denied, open '…'`); no fixture record exercises it.
//!
//! # Platform notes (reproduced, not fixed)
//!
//! - **M2 on win32.** `mkdirSync(correctDir)` runs *before* the rename, so the encoded
//!   `sessions/--home-user-proj--/` directory **is** created; the rename then throws into
//!   the swallowing catch and the `.jsonl` stays in the agent root. Both halves are
//!   reproduced — see [`migrate_sessions_from_agent_root`] and `js_basename_bug`.
//! - **`0o600`.** `auth.json` is created with mode `0o600` under `#[cfg(unix)]`
//!   (`migrations.ts:69`); on Windows the mode is meaningless, which is why every fixture
//!   record carries `mode: "0666"` with `modeMeaningful: false`. **The fixtures should be
//!   re-derived on Linux/macOS** to turn the mode column into a real assertion.
//!
//! # Not ported
//!
//! - The **keypress wait** at the end of `showDeprecationWarnings`
//!   (`migrations.ts:288-296`): it would hang a headless run (spec §7.5). The lines around
//!   it are printed, including the trailing blank one.
//! - `chalk`'s colour *detection*. [`ConsoleStyle`] keeps the information; deciding whether
//!   a TTY gets ANSI is feat-006's.

use std::fs;
use std::path::Path;

use pirust_tools::path_utils::{self, PathEnv, PathInputOptions, Platform as PathPlatform};
use serde_json::{Map, Value};

use crate::config::{
    ConfigEnv, ConfigPathError, Platform, PROMPTS_DIR_NAME, SESSIONS_DIR_NAME, TOOLS_DIR_NAME,
};

// =============================================================================
// Constants (`migrations.ts:11-14`, `:184`, and the file-name literals)
// =============================================================================

/// `MIGRATION_GUIDE_URL` (`migrations.ts:11-12`).
pub const MIGRATION_GUIDE_URL: &str =
    "https://github.com/earendil-works/pi-mono/blob/main/packages/coding-agent/CHANGELOG.md#extensions-migration";

/// `EXTENSIONS_DOC_URL` (`migrations.ts:13-14`).
pub const EXTENSIONS_DOC_URL: &str =
    "https://github.com/earendil-works/pi-mono/blob/main/packages/coding-agent/docs/extensions.md";

/// The legacy credential file M1 consumes (`migrations.ts:24`).
const OAUTH_FILE_NAME: &str = "oauth.json";

/// Suffix M1 renames it to (`migrations.ts:41`).
const MIGRATED_SUFFIX: &str = ".migrated";

/// M4's file (`migrations.ts:158`).
const KEYBINDINGS_FILE_NAME: &str = "keybindings.json";

/// The pre-rename prompts directory (`migrations.ts:138`).
const COMMANDS_DIR_NAME: &str = "commands";

/// The deprecated extension directory M5 warns about (`migrations.ts:223`).
const HOOKS_DIR_NAME: &str = "hooks";

/// The four names M3 moves, **in this order** (`migrations.ts:184`). Compared exactly —
/// unlike `check_deprecated_extension_dirs`, which lowercases (`migrations.ts:236-239`).
const MANAGED_BINARIES: [&str; 4] = ["fd", "rg", "fd.exe", "rg.exe"];

/// The `label` arguments (`migrations.ts:262-268`).
const GLOBAL_LABEL: &str = "Global";
const PROJECT_LABEL: &str = "Project";

// =============================================================================
// Errors
// =============================================================================

/// Why a migration aborted. See the module docs' table: only three Pi sites are uncaught.
#[derive(Debug, thiserror::Error)]
pub enum MigrationError {
    /// `getAgentDir()` threw (`config.ts:520`) — no migration can run.
    #[error(transparent)]
    Path(#[from] ConfigPathError),
    /// One of Pi's **uncaught** writes failed: `auth.json` (`migrations.ts:68-69`) or
    /// `mkdirSync(binDir)` (`migrations.ts:193`).
    #[error("{operation} {path}: {source}")]
    Io {
        /// Which uncaught operation — `mkdir` or `write`.
        operation: &'static str,
        /// The path it was applied to.
        path: String,
        /// The underlying failure. Node's `Error.message` wording is not reproduced.
        #[source]
        source: std::io::Error,
    },
}

// =============================================================================
// The console seam (`console.log(chalk.…)`)
// =============================================================================

/// Which `chalk` helper Pi wraps the line in.
///
/// Kept so the presentation layer can colour identically; this module never emits ANSI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsoleStyle {
    /// `chalk.green` — the two "Migrated …" lines (`migrations.ts:144`, `:214`).
    Green,
    /// `chalk.yellow` — the migration warning and every `showDeprecationWarnings` line
    /// (`migrations.ts:149`, `:281-285`).
    Yellow,
    /// `chalk.dim` — `Press any key to continue...` (`migrations.ts:286`).
    Dim,
    /// Unstyled `console.log()` — the trailing blank line (`migrations.ts:297`).
    Plain,
}

/// Where the migrations' `console.log` calls go.
pub trait MigrationConsole {
    /// One `console.log` call: `text` is the string Pi passes to `chalk`, without a
    /// trailing newline (`console.log` adds it).
    fn log(&mut self, style: ConsoleStyle, text: &str);
}

/// Production sink — `console.log` is `println!` to stdout.
#[derive(Debug, Clone, Copy, Default)]
pub struct StdoutConsole;

impl MigrationConsole for StdoutConsole {
    fn log(&mut self, _style: ConsoleStyle, text: &str) {
        println!("{text}");
    }
}

/// One captured `console.log`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsoleLine {
    /// The `chalk` helper Pi used.
    pub style: ConsoleStyle,
    /// The text, without the newline `console.log` appends.
    pub text: String,
}

/// Recording sink, so the fixtures' `console` arrays can be compared.
///
/// Not `#[cfg(test)]`: the golden suite is a separate crate (as with
/// [`crate::config::PI`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CapturingConsole {
    /// Every line, in call order.
    pub lines: Vec<ConsoleLine>,
}

impl CapturingConsole {
    /// Just the texts, for comparison against a fixture's `console[].text`.
    pub fn texts(&self) -> Vec<&str> {
        self.lines.iter().map(|line| line.text.as_str()).collect()
    }

    /// The bytes a real `console.log` sequence would have written to stdout.
    pub fn stdout(&self) -> String {
        self.lines
            .iter()
            .map(|line| format!("{}\n", line.text))
            .collect()
    }
}

impl MigrationConsole for CapturingConsole {
    fn log(&mut self, style: ConsoleStyle, text: &str) {
        self.lines.push(ConsoleLine {
            style,
            text: text.to_string(),
        });
    }
}

// =============================================================================
// M1 — migrateAuthToAuthJson (`migrations.ts:21-73`)
// =============================================================================

/// Migrate legacy `oauth.json` + `settings.json`'s `apiKeys` into `auth.json`.
///
/// **Guard** (`:28`): `if (existsSync(authPath)) return []` — one existing `auth.json`
/// disables the whole migration, so neither source file is touched.
///
/// **Transform (a), `oauth.json`** (`:34-45`, whole block in `try {} catch {}`): parse,
/// then for each `[provider, cred]` set `migrated[provider] = { type: "oauth", ...cred }` —
/// the spread comes **after** `type`, so a legacy `cred.type` wins the *value* while `type`
/// keeps the first *position* (JS spread semantics; `serde_json`'s `preserve_order` map
/// behaves the same on re-insert). Then `renameSync(oauthPath, oauthPath + ".migrated")`.
/// A parse failure leaves `oauth.json` in place, unrenamed, with nothing written.
///
/// **Transform (b), `settings.json`** (`:48-65`, also swallowed): if `settings.apiKeys` is
/// truthy and `typeof === "object"` (so an array qualifies, `null` does not), then for each
/// `[provider, key]` with `!migrated[provider] && typeof key === "string"` set
/// `migrated[provider] = { type: "api_key", key }`; then `delete settings.apiKeys` and
/// rewrite the file with `JSON.stringify(settings, null, 2)` — **no mode, no trailing
/// newline**. The delete + rewrite are unconditional once the `apiKeys` object exists, so a
/// non-string value is silently *lost* (fixture `M1:non-string-apiKey-values-…`).
///
/// **Write** (`:67-70`): only when `migrated` is non-empty, and **not** wrapped in a
/// `catch` — `mkdirSync(dirname(authPath), {recursive:true})` then `writeFileSync(…, {mode:
/// 0o600})`. `dirname(join(agentDir, "auth.json"))` is `agentDir`, so that is what is
/// created here.
///
/// **Returns** the provider names in insertion order: `oauth.json`'s keys first, then
/// `settings.json`'s surviving `apiKeys` keys.
pub fn migrate_auth_to_auth_json(env: &ConfigEnv) -> Result<Vec<String>, MigrationError> {
    let agent_dir = env.agent_dir()?;
    let auth_path = env.auth_path()?;
    let oauth_path = join(env, &agent_dir, OAUTH_FILE_NAME);
    let settings_path = env.settings_path()?;

    // migrations.ts:28
    if exists(&auth_path) {
        return Ok(Vec::new());
    }

    let mut migrated: Map<String, Value> = Map::new();
    let mut providers: Vec<String> = Vec::new();

    // migrations.ts:34-45
    let oauth = read_json_if_exists(&oauth_path);
    if let Some(oauth) = oauth {
        for (provider, cred) in object_entries(&oauth) {
            let mut entry: Map<String, Value> = Map::new();
            entry.insert("type".to_string(), Value::String("oauth".to_string()));
            for (key, value) in object_entries(&cred) {
                entry.insert(key, value);
            }
            migrated.insert(provider.clone(), Value::Object(entry));
            providers.push(provider);
        }
        // Inside the same `try`: a failure here still leaves `migrated` filled.
        let _ = fs::rename(&oauth_path, format!("{oauth_path}{MIGRATED_SUFFIX}"));
    }

    // migrations.ts:48-65
    let settings = read_json_if_exists(&settings_path);
    if let Some(Value::Object(mut settings)) = settings {
        // `settings.apiKeys && typeof settings.apiKeys === "object"`: `null` is falsy, an
        // array is `"object"`.
        let api_keys = match settings.get("apiKeys") {
            Some(value @ (Value::Object(_) | Value::Array(_))) => Some(value.clone()),
            _ => None,
        };
        if let Some(api_keys) = api_keys {
            for (provider, key) in object_entries(&api_keys) {
                // `!migrated[provider] && typeof key === "string"` — no side effects, so
                // the two halves are order-independent.
                if migrated.contains_key(&provider) {
                    continue;
                }
                let Value::String(key) = key else { continue };
                let mut entry: Map<String, Value> = Map::new();
                entry.insert("type".to_string(), Value::String("api_key".to_string()));
                entry.insert("key".to_string(), Value::String(key));
                migrated.insert(provider.clone(), Value::Object(entry));
                providers.push(provider);
            }
            settings.remove("apiKeys");
            // Still inside the `try`, hence swallowed.
            let _ = fs::write(
                &settings_path,
                json_stringify_indent2(&Value::Object(settings)),
            );
        }
    }

    // migrations.ts:67-70 — NOT wrapped in a catch.
    if !migrated.is_empty() {
        fs::create_dir_all(&agent_dir).map_err(|source| MigrationError::Io {
            operation: "mkdir",
            path: agent_dir.clone(),
            source,
        })?;
        write_new_file_0600(
            &auth_path,
            &json_stringify_indent2(&Value::Object(migrated)),
        )
        .map_err(|source| MigrationError::Io {
            operation: "write",
            path: auth_path.clone(),
            source,
        })?;
    }

    Ok(providers)
}

// =============================================================================
// M2 — migrateSessionsFromAgentRoot (`migrations.ts:84-131`)
// =============================================================================

/// Relocate stray `<agentDir>/*.jsonl` sessions into `<agentDir>/sessions/<encoded-cwd>/`.
///
/// Fixes the v0.30.0 bug where sessions were written to the agent root
/// (pi-mono issue #320, `migrations.ts:76-82`).
///
/// **Guard** (`:88-97`): `readdirSync(agentDir)` inside a try/catch that `return`s on
/// failure; keep the entries ending in `.jsonl` (top level only, no recursion); return when
/// none remain — so a root with no loose sessions never even creates `sessions/`.
///
/// **Per file, inside `try {} catch {}`** (`:99-129`): read the whole file as utf-8, take
/// `content.split("\n")[0]`, skip a blank first line, `JSON.parse` it, skip unless
/// `header.type === "session" && header.cwd`; encode the **raw** `header.cwd` with
/// [`encode_session_dir_name`] (no `resolvePath` here — see that function); `mkdirSync` the
/// target when missing; take the file name with `js_basename_bug`; skip when the target
/// already exists; `renameSync`.
///
/// **The win32 half-effect, reproduced.** Because `mkdirSync` precedes the rename, the
/// encoded directory is created even when the rename then fails on the bogus name — which
/// is what every `platformDependent: "m2-filename-split"` fixture record shows: a new empty
/// `sessions/--home-user-proj--/` *and* the `.jsonl` still in the root.
///
/// Two `header` shapes Pi rejects by throwing rather than by a guard, and which are folded
/// into the same `continue` here: a non-string truthy `cwd` (`cwd.replace` is not a
/// function) and a whitespace-only first line (`"\u{85}"` survives JS's `trim` but not
/// `JSON.parse`). Both end as "file skipped, nothing created", identically.
pub fn migrate_sessions_from_agent_root(env: &ConfigEnv) -> Result<(), MigrationError> {
    let agent_dir = env.agent_dir()?;

    // migrations.ts:88-95
    let Ok(entries) = fs::read_dir(&agent_dir) else {
        return Ok(());
    };
    let files: Vec<String> = entries
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".jsonl"))
        .map(|name| join(env, &agent_dir, &name))
        .collect();

    // migrations.ts:97
    if files.is_empty() {
        return Ok(());
    }

    for file in &files {
        // migrations.ts:102-104
        let Ok(bytes) = fs::read(file) else { continue };
        let content = String::from_utf8_lossy(&bytes);
        let first_line = content.split('\n').next().unwrap_or("");
        if first_line.trim().is_empty() {
            continue;
        }

        // migrations.ts:106-109
        let Ok(header) = serde_json::from_str::<Value>(first_line) else {
            continue;
        };
        if header.get("type").and_then(Value::as_str) != Some("session") {
            continue;
        }
        let Some(cwd) = header
            .get("cwd")
            .and_then(Value::as_str)
            .filter(|cwd| !cwd.is_empty())
        else {
            continue;
        };

        // migrations.ts:112-118 — the mkdir happens BEFORE the rename.
        let safe_path = encode_session_dir_name(cwd);
        let sessions_dir = join(env, &agent_dir, SESSIONS_DIR_NAME);
        let correct_dir = join(env, &sessions_dir, &safe_path);
        if !exists(&correct_dir) && fs::create_dir_all(&correct_dir).is_err() {
            // `mkdirSync` throwing lands in the same catch as everything else.
            continue;
        }

        // migrations.ts:121-126
        let file_name = js_basename_bug(file);
        let new_path = join(env, &correct_dir, file_name);
        if exists(&new_path) {
            continue;
        }
        let _ = fs::rename(file, &new_path);
    }

    Ok(())
}

/// The cwd → session-directory-name encoding (`migrations.ts:112`, verbatim in
/// `core/session-manager.ts:475` and `packages/agent/src/harness/session/jsonl-repo.ts:35`).
///
/// ```text
/// `--${cwd.replace(/^[/\\]/, "").replace(/[/\\:]/g, "-")}--`
/// ```
///
/// Strip **one** leading `/` or `\`, replace **every** `/`, `\` and `:` with `-`, wrap in
/// `--`…`--`. Pure string work on chars — no `Path::components`, no canonicalization, and
/// spaces and non-ASCII pass through untouched.
///
/// **Resolution happens in the caller, not here.** `migrations.ts:109-112` encodes the raw
/// `header.cwd`; `core/session-manager.ts:473-475` encodes `resolvePath(cwd)` *before*
/// calling the same expression. That is why this function takes an already-resolved path:
/// the `getDefaultSessionDirPath` half of `tests/fixtures/pi/cli/session_dir.cases.jsonl`
/// is win32-resolved (`/home/user/proj` → `C:\home\user\proj` → `--C--home-user-proj--`)
/// while the `migrateSessionsFromAgentRoot` half of the same file is not
/// (`/home/user/project` → `--home-user-project--`). Resolving inside would break M2.
///
/// Worked examples (spec §7.7): `/home/me/proj` → `--home-me-proj--`; `/` → `----`;
/// `C:\Users\me\proj` → `--C--Users-me-proj--` (the drive colon *and* the following
/// separator both become dashes); `\\server\share\dir` → `---server-share-dir--` (only one
/// of the two leading backslashes is stripped).
pub fn encode_session_dir_name(cwd: &str) -> String {
    // `replace(/^[/\\]/, "")` — one character, at position 0 only.
    let stripped = cwd
        .strip_prefix('/')
        .or_else(|| cwd.strip_prefix('\\'))
        .unwrap_or(cwd);
    let body: String = stripped
        .chars()
        .map(|ch| {
            if matches!(ch, '/' | '\\' | ':') {
                '-'
            } else {
                ch
            }
        })
        .collect();
    format!("--{body}--")
}

/// `file.split("/").pop() || file.split("\\").pop()` (`migrations.ts:121`) — **the bug**.
///
/// On win32 `file` is `C:\…\a.jsonl`: `split("/")` yields a single element, `pop()` returns
/// the whole path (truthy), the `||` never fires, and the "file name" is an absolute path.
/// `join(correctDir, thatPath)` then produces `…\sessions\--x--\C:\…\a.jsonl`, which
/// `renameSync` rejects — swallowed, so the file stays put. On posix the first `split` finds
/// separators and the basename is correct, which is why M2 actually works there.
///
/// Ported literally; the `||` fires only for a trailing-separator path (`pop()` → `""`).
fn js_basename_bug(file: &str) -> &str {
    let by_slash = file.rsplit('/').next().unwrap_or("");
    if by_slash.is_empty() {
        file.rsplit('\\').next().unwrap_or("")
    } else {
        by_slash
    }
}

// =============================================================================
// M3 — migrateToolsToBin (`migrations.ts:177-216`)
// =============================================================================

/// Move the managed `fd`/`rg` binaries from `<agentDir>/tools/` to `<agentDir>/bin/`.
///
/// **Guard** (`:182`): `if (!existsSync(toolsDir)) return` — `bin/` is not even created.
///
/// **Transform** (`:187-211`): for each of [`MANAGED_BINARIES`] in order, when
/// `tools/<name>` exists: `mkdirSync(binDir)` if missing (**uncaught** — see
/// [`MigrationError`]); then if `bin/<name>` does **not** exist, `renameSync` (errors
/// ignored) and set `movedAny` **on success only**; else `rmSync(oldPath, {force:true})`
/// (errors ignored) **without** setting `movedAny`, leaving `bin/`'s bytes alone.
///
/// **Output** (`:213-215`): one green line when `movedAny`, verbatim including U+2192.
///
/// The name comparison is exact — `tools/RG.EXE` is *not* in the list. On a
/// case-insensitive filesystem `existsSync("tools/rg.exe")` nevertheless answers yes, so
/// win32 moves it anyway; that is what the fixture's `M3:case-sensitivity-of-the-binary-names`
/// record captured (its `after` tree is a property of NTFS, not of Pi's logic).
pub fn migrate_tools_to_bin(
    env: &ConfigEnv,
    console: &mut dyn MigrationConsole,
) -> Result<(), MigrationError> {
    let agent_dir = env.agent_dir()?;
    let tools_dir = join(env, &agent_dir, TOOLS_DIR_NAME);
    let bin_dir = env.bin_dir()?;

    // migrations.ts:182
    if !exists(&tools_dir) {
        return Ok(());
    }

    let mut moved_any = false;
    for bin in MANAGED_BINARIES {
        let old_path = join(env, &tools_dir, bin);
        let new_path = join(env, &bin_dir, bin);

        if !exists(&old_path) {
            continue;
        }
        // migrations.ts:192-194 — uncaught, so a failure aborts startup.
        if !exists(&bin_dir) {
            fs::create_dir_all(&bin_dir).map_err(|source| MigrationError::Io {
                operation: "mkdir",
                path: bin_dir.clone(),
                source,
            })?;
        }
        if exists(&new_path) {
            // migrations.ts:202-208 — target wins; `movedAny` stays false.
            let _ = fs::remove_file(&old_path);
        } else if fs::rename(&old_path, &new_path).is_ok() {
            moved_any = true;
        }
    }

    // migrations.ts:213-215
    if moved_any {
        console.log(
            ConsoleStyle::Green,
            "Migrated managed binaries tools/ → bin/",
        );
    }
    Ok(())
}

// =============================================================================
// M4 — migrateKeybindingsConfigFile (`migrations.ts:157-172`)
// =============================================================================

/// Rewrite `<agentDir>/keybindings.json` with Pi's current binding names.
///
/// **Guard** (`:158-159`): `if (!existsSync(configPath)) return`.
///
/// **Transform** (`:161-171`, all inside one `try {} catch {}` — "Ignore malformed files
/// during migration", so even the write is swallowed): `JSON.parse`; bail unless the value
/// is a non-null, non-array object; call `migrateKeybindingsConfig`
/// (`core/keybindings.ts:289-309`), which renames legacy keys, **drops** a legacy key whose
/// new name is already present, and reorders the result; `if (!migrated) return` — so a
/// file with no legacy names keeps its exact bytes, non-canonical key order included; else
/// write `` `${JSON.stringify(config, null, 2)}\n` `` — **this write does append a
/// newline**, unlike M1's.
pub fn migrate_keybindings_config_file(env: &ConfigEnv) -> Result<(), MigrationError> {
    let agent_dir = env.agent_dir()?;
    let config_path = join(env, &agent_dir, KEYBINDINGS_FILE_NAME);

    // migrations.ts:159
    if !exists(&config_path) {
        return Ok(());
    }

    // migrations.ts:162-165 — a non-object (including `null` and an array) returns.
    let Some(Value::Object(raw_config)) = read_json_if_exists(&config_path) else {
        return Ok(());
    };

    let (config, migrated) = keybindings::migrate_keybindings_config(&raw_config);
    if !migrated {
        return Ok(());
    }
    let contents = format!("{}\n", json_stringify_indent2(&Value::Object(config)));
    // Inside the `try`, hence swallowed.
    let _ = fs::write(&config_path, contents);
    Ok(())
}

// =============================================================================
// M5 — migrateExtensionSystem (`migrations.ts:257-272`)
// =============================================================================

/// Rename `commands/` → `prompts/` in both scopes, then collect the deprecation warnings.
///
/// `migrateCommandsToPrompts(agentDir, "Global")` runs **before**
/// `migrateCommandsToPrompts(projectDir, "Project")` (`:262-263`), which is why the two
/// green lines always come out Global-first; the warnings are then
/// `[...global, ...project]` (`:266-269`), each scope contributing hooks before tools — at
/// most four, in exactly that order.
///
/// `projectDir` is `join(cwd, CONFIG_DIR_NAME)` (`:259`), i.e. `<cwd>/.pi` for Pi and
/// `<cwd>/.pirust` here; the constant comes from [`ConfigEnv::identity`] so the golden
/// suite can replay Pi's.
pub fn migrate_extension_system(
    env: &ConfigEnv,
    cwd: &str,
    console: &mut dyn MigrationConsole,
) -> Result<Vec<String>, MigrationError> {
    let agent_dir = env.agent_dir()?;
    let project_dir = join(env, cwd, env.identity.config_dir_name);

    // migrations.ts:262-263 — the return values are ignored by Pi too.
    migrate_commands_to_prompts(env, &agent_dir, GLOBAL_LABEL, console);
    migrate_commands_to_prompts(env, &project_dir, PROJECT_LABEL, console);

    // migrations.ts:266-269
    let mut warnings = check_deprecated_extension_dirs(env, &agent_dir, GLOBAL_LABEL);
    warnings.extend(check_deprecated_extension_dirs(
        env,
        &project_dir,
        PROJECT_LABEL,
    ));
    Ok(warnings)
}

/// `migrateCommandsToPrompts` (`migrations.ts:137-155`).
///
/// **Guard**: `existsSync(commandsDir) && !existsSync(promptsDir)` — when both exist,
/// `commands/` is left behind and nothing is printed. Works for symlinks as well, since it
/// is one `renameSync` of the directory entry.
///
/// On success: green `` `Migrated ${label} commands/ → prompts/` `` and `true`. On throw:
/// yellow
/// `` `Warning: Could not migrate ${label} commands/ to prompts/: ${err.message}` `` and
/// `false`. The interpolated text is Node's `Error.message`; Rust's `io::Error` wording
/// differs and no fixture record exercises the failure branch.
fn migrate_commands_to_prompts(
    env: &ConfigEnv,
    base_dir: &str,
    label: &str,
    console: &mut dyn MigrationConsole,
) -> bool {
    let commands_dir = join(env, base_dir, COMMANDS_DIR_NAME);
    let prompts_dir = join(env, base_dir, PROMPTS_DIR_NAME);

    if !exists(&commands_dir) || exists(&prompts_dir) {
        return false;
    }
    match fs::rename(&commands_dir, &prompts_dir) {
        Ok(()) => {
            console.log(
                ConsoleStyle::Green,
                &format!("Migrated {label} commands/ → prompts/"),
            );
            true
        }
        Err(err) => {
            console.log(
                ConsoleStyle::Yellow,
                &format!("Warning: Could not migrate {label} commands/ to prompts/: {err}"),
            );
            false
        }
    }
}

/// `checkDeprecatedExtensionDirs` (`migrations.ts:222-252`) — warns, never moves.
///
/// 1. `<baseDir>/hooks` exists →
///    `` `${label} hooks/ directory found. Hooks have been renamed to extensions.` ``
/// 2. `<baseDir>/tools` exists → `readdirSync` it (read errors ignored) and drop entries
///    whose **lowercased** name is `fd`/`rg`/`fd.exe`/`rg.exe`, or which start with `.`
///    (`.DS_Store` and friends); if any survive →
///    `` `${label} tools/ directory contains custom tools. Custom tools have been merged into extensions.` ``
fn check_deprecated_extension_dirs(env: &ConfigEnv, base_dir: &str, label: &str) -> Vec<String> {
    let hooks_dir = join(env, base_dir, HOOKS_DIR_NAME);
    let tools_dir = join(env, base_dir, TOOLS_DIR_NAME);
    let mut warnings = Vec::new();

    // migrations.ts:227-229
    if exists(&hooks_dir) {
        warnings.push(format!(
            "{label} hooks/ directory found. Hooks have been renamed to extensions."
        ));
    }

    // migrations.ts:231-249
    if exists(&tools_dir) {
        if let Ok(entries) = fs::read_dir(&tools_dir) {
            let custom_tools = entries
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .filter(|name| {
                    let lower = name.to_lowercase();
                    !MANAGED_BINARIES.contains(&lower.as_str()) && !name.starts_with('.')
                })
                .count();
            if custom_tools > 0 {
                warnings.push(format!(
                    "{label} tools/ directory contains custom tools. Custom tools have been merged into extensions."
                ));
            }
        }
    }

    warnings
}

// =============================================================================
// showDeprecationWarnings (`migrations.ts:277-298`)
// =============================================================================

/// Print the deprecation warnings. **The keypress wait is not ported** (spec §7.5): it
/// would hang a headless run, and `main.ts:786-788` only reaches this in interactive mode.
///
/// `if (warnings.length === 0) return` (`:278`), then, one `console.log` each: every warning
/// as yellow `` `Warning: ${warning}` ``; yellow
/// `\nMove your extensions to the extensions/ directory.`;
/// yellow `` `Migration guide: ${MIGRATION_GUIDE_URL}` ``; yellow
/// `` `Documentation: ${EXTENSIONS_DOC_URL}` ``; dim `\nPress any key to continue...`.
/// Pi then awaits a key and logs one empty line (`:297`); here the empty line follows
/// immediately, which reproduces the captured stdout byte for byte (fixture
/// `showDeprecationWarnings:two-warnings`, taken from a child process fed one byte on
/// stdin) while never blocking.
pub fn show_deprecation_warnings(console: &mut dyn MigrationConsole, warnings: &[String]) {
    if warnings.is_empty() {
        return;
    }
    for warning in warnings {
        console.log(ConsoleStyle::Yellow, &format!("Warning: {warning}"));
    }
    console.log(
        ConsoleStyle::Yellow,
        "\nMove your extensions to the extensions/ directory.",
    );
    console.log(
        ConsoleStyle::Yellow,
        &format!("Migration guide: {MIGRATION_GUIDE_URL}"),
    );
    console.log(
        ConsoleStyle::Yellow,
        &format!("Documentation: {EXTENSIONS_DOC_URL}"),
    );
    console.log(ConsoleStyle::Dim, "\nPress any key to continue...");
    // migrations.ts:288-296 — the raw-mode keypress wait is deliberately absent.
    console.log(ConsoleStyle::Plain, "");
}

// =============================================================================
// runMigrations (`migrations.ts:305-315`)
// =============================================================================

/// What `runMigrations` returns (`migrations.ts:305-308`).
///
/// Both fields are consumed by interactive mode only (`main.ts:786-788`, `:816`); a
/// headless run computes and discards them — the side effects are the point.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MigrationResults {
    /// `migratedAuthProviders` — M1's return value.
    pub migrated_auth_providers: Vec<String>,
    /// `deprecationWarnings` — M5's return value.
    pub deprecation_warnings: Vec<String>,
}

/// Run all migrations, once, on startup (`main.ts:555`).
///
/// The order is fixed and observable — the fixture's `runMigrations:…-ORDER-OF-EFFECTS`
/// record pins it through the console stream (M3's `tools/ → bin/` line precedes M5's two
/// `commands/ → prompts/` lines) and through M1's `settings.json` rewrite being visible to
/// the `SettingsManager` created afterwards (`main.ts:558`):
///
/// 1. [`migrate_auth_to_auth_json`]
/// 2. [`migrate_sessions_from_agent_root`]
/// 3. [`migrate_tools_to_bin`]
/// 4. [`migrate_keybindings_config_file`]
/// 5. [`migrate_extension_system`]
///
/// No try/catch of its own: the `?`s here are the only failures Pi can propagate (see
/// [`MigrationError`]).
pub fn run_migrations(
    env: &ConfigEnv,
    cwd: &str,
    console: &mut dyn MigrationConsole,
) -> Result<MigrationResults, MigrationError> {
    let migrated_auth_providers = migrate_auth_to_auth_json(env)?;
    migrate_sessions_from_agent_root(env)?;
    migrate_tools_to_bin(env, console)?;
    migrate_keybindings_config_file(env)?;
    let deprecation_warnings = migrate_extension_system(env, cwd, console)?;
    Ok(MigrationResults {
        migrated_auth_providers,
        deprecation_warnings,
    })
}

// =============================================================================
// Internals
// =============================================================================

/// `existsSync` (`fs`): follows symlinks, and answers `false` rather than throwing when the
/// path cannot be stat'd — which is what `Path::exists` does.
fn exists(path: &str) -> bool {
    Path::new(path).exists()
}

/// `existsSync(p) ? JSON.parse(readFileSync(p, "utf-8")) : undefined`, with the `catch`
/// folded in: `None` means "absent, unreadable, or malformed", the three cases every caller
/// treats alike.
///
/// `String::from_utf8_lossy` mirrors Node's utf-8 decoder, which substitutes U+FFFD for
/// invalid bytes instead of failing (`JSON.parse` then throws, as it does in Pi).
fn read_json_if_exists(path: &str) -> Option<Value> {
    if !exists(path) {
        return None;
    }
    let bytes = fs::read(path).ok()?;
    serde_json::from_str(&String::from_utf8_lossy(&bytes)).ok()
}

/// `JSON.stringify(value, null, 2)`.
///
/// `serde_json`'s pretty printer is 2-space indented and emits `{}` / `[]` for empty
/// containers, matching JS. Known divergence: a *fractional* number renders as `1.0` where
/// JS gives `1`, and large magnitudes as `1e21` differ — `serde_json` has no
/// `Number::prototype.toString`. No fixture record contains a non-integer number; the
/// `float_roundtrip` feature keeps integers exact.
fn json_stringify_indent2(value: &Value) -> String {
    serde_json::to_string_pretty(value)
        .expect("a serde_json::Value cannot fail to serialize (no NaN, no non-string keys)")
}

/// `writeFileSync(path, contents, { mode: 0o600 })` (`migrations.ts:69`).
///
/// `mode` applies **only when the file is created** (and is masked by the umask), which is
/// `OpenOptions::create().mode()`. On Windows there is nothing to set — hence the fixtures'
/// `modeMeaningful: false`; they should be re-derived on Linux/macOS.
fn write_new_file_0600(path: &str, contents: &str) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(contents.as_bytes())
    }
    #[cfg(not(unix))]
    {
        fs::write(path, contents)
    }
}

/// `Object.entries(value)` for a JSON value, including the shapes Pi never intends but the
/// code would accept: an array yields index keys, a string yields per-character index keys,
/// and every other primitive yields nothing. Also serves as JS's object spread
/// (`{ ...cred }`), which enumerates exactly the same pairs.
///
/// The string case indexes by `char`, whereas JS indexes by UTF-16 code unit; only an
/// astral-plane character in a scalar `oauth.json` could tell the difference.
fn object_entries(value: &Value) -> Vec<(String, Value)> {
    match value {
        Value::Object(map) => map
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
        Value::Array(items) => items
            .iter()
            .enumerate()
            .map(|(index, value)| (index.to_string(), value.clone()))
            .collect(),
        Value::String(text) => text
            .chars()
            .enumerate()
            .map(|(index, ch)| (index.to_string(), Value::String(ch.to_string())))
            .collect(),
        _ => Vec::new(),
    }
}

/// `path.join(base, tail)` for this platform.
///
/// Same delegation `config.rs`'s private `node_join` documents: `normalizePath`'s tilde
/// branch *is* `platform.join([home, rest])` (`utils/paths.ts:70`), so passing `"~/<tail>"`
/// with `home_dir = base` yields Node's `join` from [`pirust_tools::path_utils`]'s verified
/// transcription — whose own `join` is private. `PathBuf::join` is **not** usable here:
/// M2's buggy `join(dir, "C:\…\a.jsonl")` must produce `dir\C:\…\a.jsonl` (which
/// `renameSync` rejects), whereas `PathBuf::join` would discard the base.
///
/// Infallible: the tilde branch returns before the only fallible (`file://`) branch.
fn join(env: &ConfigEnv, base: &str, tail: &str) -> String {
    let path_env = PathEnv {
        platform: if env.platform == Platform::Win32 {
            PathPlatform::Win32
        } else {
            PathPlatform::Posix
        },
        home_dir: String::new(),
        cwd: String::new(),
    };
    let options = PathInputOptions {
        home_dir: Some(base),
        ..PathInputOptions::default()
    };
    path_utils::normalize_path(&path_env, &format!("~/{tail}"), &options)
        .expect("normalizePath's tilde branch returns before the file:// branch")
}

/// `migrateKeybindingsConfig` (`core/keybindings.ts:289-327`) — **a temporary tenant**.
///
/// `core/keybindings.ts` belongs to feat-007 (`KEYBINDINGS`' `defaultKeys`/`description`
/// definitions, `KeybindingsManager`, the key parser). Only the two *tables* M4 needs are
/// transcribed here, and only their keys. Spec §7.4 proposed landing M4 with an empty
/// rename map, but four fixture records —
/// `M4:legacy-names-renamed-and-reordered`, `M4:array-valued-bindings-are-preserved`,
/// `M4:legacy-and-new-key-both-present-legacy-is-DROPPED` and
/// `runMigrations:needs-several-ORDER-OF-EFFECTS` — contain Pi's real output, so a stub
/// would fail the oracle. **When `keybindings.rs` lands, delete this module** and have
/// [`migrate_keybindings_config_file`] call into it; the two tables must not exist twice.
mod keybindings {
    use serde_json::{Map, Value};

    /// `KEYBINDING_NAME_MIGRATIONS` (`core/keybindings.ts:209-269`) — legacy bare name →
    /// current dotted name, in declaration order (order is irrelevant: it is a lookup).
    const NAME_MIGRATIONS: [(&str, &str); 59] = [
        ("cursorUp", "tui.editor.cursorUp"),
        ("cursorDown", "tui.editor.cursorDown"),
        ("cursorLeft", "tui.editor.cursorLeft"),
        ("cursorRight", "tui.editor.cursorRight"),
        ("cursorWordLeft", "tui.editor.cursorWordLeft"),
        ("cursorWordRight", "tui.editor.cursorWordRight"),
        ("cursorLineStart", "tui.editor.cursorLineStart"),
        ("cursorLineEnd", "tui.editor.cursorLineEnd"),
        ("jumpForward", "tui.editor.jumpForward"),
        ("jumpBackward", "tui.editor.jumpBackward"),
        ("pageUp", "tui.editor.pageUp"),
        ("pageDown", "tui.editor.pageDown"),
        ("deleteCharBackward", "tui.editor.deleteCharBackward"),
        ("deleteCharForward", "tui.editor.deleteCharForward"),
        ("deleteWordBackward", "tui.editor.deleteWordBackward"),
        ("deleteWordForward", "tui.editor.deleteWordForward"),
        ("deleteToLineStart", "tui.editor.deleteToLineStart"),
        ("deleteToLineEnd", "tui.editor.deleteToLineEnd"),
        ("yank", "tui.editor.yank"),
        ("yankPop", "tui.editor.yankPop"),
        ("undo", "tui.editor.undo"),
        ("newLine", "tui.input.newLine"),
        ("submit", "tui.input.submit"),
        ("tab", "tui.input.tab"),
        ("copy", "tui.input.copy"),
        ("selectUp", "tui.select.up"),
        ("selectDown", "tui.select.down"),
        ("selectPageUp", "tui.select.pageUp"),
        ("selectPageDown", "tui.select.pageDown"),
        ("selectConfirm", "tui.select.confirm"),
        ("selectCancel", "tui.select.cancel"),
        ("interrupt", "app.interrupt"),
        ("clear", "app.clear"),
        ("exit", "app.exit"),
        ("suspend", "app.suspend"),
        ("cycleThinkingLevel", "app.thinking.cycle"),
        ("cycleModelForward", "app.model.cycleForward"),
        ("cycleModelBackward", "app.model.cycleBackward"),
        ("selectModel", "app.model.select"),
        ("expandTools", "app.tools.expand"),
        ("toggleThinking", "app.thinking.toggle"),
        ("toggleSessionNamedFilter", "app.session.toggleNamedFilter"),
        ("externalEditor", "app.editor.external"),
        ("followUp", "app.message.followUp"),
        ("dequeue", "app.message.dequeue"),
        ("pasteImage", "app.clipboard.pasteImage"),
        ("newSession", "app.session.new"),
        ("tree", "app.session.tree"),
        ("fork", "app.session.fork"),
        ("resume", "app.session.resume"),
        ("treeFoldOrUp", "app.tree.foldOrUp"),
        ("treeUnfoldOrDown", "app.tree.unfoldOrDown"),
        ("treeEditLabel", "app.tree.editLabel"),
        ("treeToggleLabelTimestamp", "app.tree.toggleLabelTimestamp"),
        ("toggleSessionPath", "app.session.togglePath"),
        ("toggleSessionSort", "app.session.toggleSort"),
        ("renameSession", "app.session.rename"),
        ("deleteSession", "app.session.delete"),
        ("deleteSessionNoninvasive", "app.session.deleteNoninvasive"),
    ];

    /// `Object.keys(KEYBINDINGS)` (`core/keybindings.ts:64-207`) — **the order is
    /// load-bearing**, it is what `orderKeybindingsConfig` re-emits into. `KEYBINDINGS`
    /// spreads `TUI_KEYBINDINGS` first (`packages/tui/src/keybindings.ts:54-134`, 31 keys)
    /// and then declares 42 `app.*` keys, so this is those two blocks concatenated in
    /// source order. Only the keys matter here; the `defaultKeys`/`description` values
    /// (some of them `process.platform`-dependent) are feat-007's.
    const KEYBINDING_ORDER: [&str; 73] = [
        // packages/tui/src/keybindings.ts:55-117 — editor
        "tui.editor.cursorUp",
        "tui.editor.cursorDown",
        "tui.editor.cursorLeft",
        "tui.editor.cursorRight",
        "tui.editor.cursorWordLeft",
        "tui.editor.cursorWordRight",
        "tui.editor.cursorLineStart",
        "tui.editor.cursorLineEnd",
        "tui.editor.jumpForward",
        "tui.editor.jumpBackward",
        "tui.editor.pageUp",
        "tui.editor.pageDown",
        "tui.editor.deleteCharBackward",
        "tui.editor.deleteCharForward",
        "tui.editor.deleteWordBackward",
        "tui.editor.deleteWordForward",
        "tui.editor.deleteToLineStart",
        "tui.editor.deleteToLineEnd",
        "tui.editor.yank",
        "tui.editor.yankPop",
        "tui.editor.undo",
        // :118-121 — generic input
        "tui.input.newLine",
        "tui.input.submit",
        "tui.input.tab",
        "tui.input.copy",
        // :122-133 — generic selection
        "tui.select.up",
        "tui.select.down",
        "tui.select.pageUp",
        "tui.select.pageDown",
        "tui.select.confirm",
        "tui.select.cancel",
        // core/keybindings.ts:66-206 — the app's own
        "app.interrupt",
        "app.clear",
        "app.exit",
        "app.suspend",
        "app.thinking.cycle",
        "app.model.cycleForward",
        "app.model.cycleBackward",
        "app.model.select",
        "app.tools.expand",
        "app.thinking.toggle",
        "app.session.toggleNamedFilter",
        "app.editor.external",
        "app.message.copy",
        "app.message.followUp",
        "app.message.dequeue",
        "app.clipboard.pasteImage",
        "app.session.new",
        "app.session.tree",
        "app.session.fork",
        "app.session.resume",
        "app.tree.foldOrUp",
        "app.tree.unfoldOrDown",
        "app.tree.editLabel",
        "app.tree.toggleLabelTimestamp",
        "app.session.togglePath",
        "app.session.toggleSort",
        "app.session.rename",
        "app.session.delete",
        "app.session.deleteNoninvasive",
        "app.models.save",
        "app.models.enableAll",
        "app.models.clearAll",
        "app.models.toggleProvider",
        "app.models.reorderUp",
        "app.models.reorderDown",
        "app.tree.filter.default",
        "app.tree.filter.noTools",
        "app.tree.filter.userOnly",
        "app.tree.filter.labeledOnly",
        "app.tree.filter.all",
        "app.tree.filter.cycleForward",
        "app.tree.filter.cycleBackward",
    ];

    /// `migrateKeybindingsConfig` (`core/keybindings.ts:289-309`).
    ///
    /// For each entry, map the key through [`NAME_MIGRATIONS`]; a change sets `migrated`.
    /// When the key changed **and** the new name is already an own property of the *raw*
    /// config, the legacy entry is dropped (`continue`) — `migrated` is still true, so the
    /// file is rewritten with the new key's value. Values are copied verbatim; only keys
    /// migrate. The result is then reordered.
    pub fn migrate_keybindings_config(
        raw_config: &Map<String, Value>,
    ) -> (Map<String, Value>, bool) {
        let mut config: Map<String, Value> = Map::new();
        let mut migrated = false;

        for (key, value) in raw_config {
            let next_key = NAME_MIGRATIONS
                .iter()
                .find(|(legacy, _)| legacy == key)
                .map_or(key.as_str(), |(_, current)| *current);
            if next_key != key {
                migrated = true;
                if raw_config.contains_key(next_key) {
                    continue;
                }
            }
            config.insert(next_key.to_string(), value.clone());
        }

        (order_keybindings_config(&config), migrated)
    }

    /// `orderKeybindingsConfig` (`core/keybindings.ts:311-327`): known keys in
    /// [`KEYBINDING_ORDER`], then the unknown ones `sort()`ed.
    ///
    /// JS's default `sort()` compares UTF-16 code units; Rust's `sort` compares UTF-8
    /// bytes. The two agree for everything below U+FFFF that is not a surrogate — i.e. for
    /// every plausible binding name.
    fn order_keybindings_config(config: &Map<String, Value>) -> Map<String, Value> {
        let mut ordered: Map<String, Value> = Map::new();
        for keybinding in KEYBINDING_ORDER {
            if let Some(value) = config.get(keybinding) {
                ordered.insert(keybinding.to_string(), value.clone());
            }
        }

        let mut extras: Vec<&String> = config
            .keys()
            .filter(|key| !ordered.contains_key(key.as_str()))
            .collect();
        extras.sort();
        for key in extras {
            ordered.insert(key.clone(), config[key].clone());
        }

        ordered
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PI;

    /// A win32 `ConfigEnv` under Pi's identity, as the fixtures were captured.
    fn pi_win32(agent_dir: &str) -> ConfigEnv {
        ConfigEnv {
            identity: PI,
            platform: Platform::Win32,
            home_dir: Some("C:\\Users\\oracle".to_string()),
            agent_dir_override: Some(agent_dir.to_string()),
        }
    }

    #[test]
    fn session_dir_encoding_matches_the_worked_examples() {
        // spec §7.7's table, which the fixture's `encodedDirName` fields also pin.
        assert_eq!(encode_session_dir_name("/home/me/proj"), "--home-me-proj--");
        assert_eq!(encode_session_dir_name("/"), "----");
        assert_eq!(encode_session_dir_name("/home/me/a-b"), "--home-me-a-b--");
        assert_eq!(
            encode_session_dir_name("C:\\Users\\me\\proj"),
            "--C--Users-me-proj--"
        );
        assert_eq!(encode_session_dir_name("\\\\?\\C:\\x"), "---?-C--x--");
        assert_eq!(encode_session_dir_name("/tmp/a b"), "--tmp-a b--");
        // Only ONE leading separator is stripped, and only at position 0.
        assert_eq!(encode_session_dir_name("//a"), "---a--");
        assert_eq!(encode_session_dir_name("a/b:c\\d"), "--a-b-c-d--");
    }

    #[test]
    fn the_win32_basename_bug_is_reproduced_verbatim() {
        // No `/` anywhere: `pop()` returns the whole path, so the `||` never fires.
        assert_eq!(
            js_basename_bug("C:\\Users\\o\\.pi\\agent\\a.jsonl"),
            "C:\\Users\\o\\.pi\\agent\\a.jsonl"
        );
        // posix: the first split wins and the basename is correct.
        assert_eq!(js_basename_bug("/home/o/.pi/agent/a.jsonl"), "a.jsonl");
        // The `||` fires only when the first `pop()` is the empty string.
        assert_eq!(js_basename_bug("C:\\x\\"), "C:\\x\\");
        assert_eq!(js_basename_bug("/home/o/"), "/home/o/");
    }

    #[test]
    fn join_is_nodes_join_not_pathbufs() {
        let env = pi_win32("C:\\agent");
        assert_eq!(join(&env, "C:\\agent", "auth.json"), "C:\\agent\\auth.json");
        // The base's separators are normalized, as node does (config.rs's node_join note).
        assert_eq!(join(&env, "rel/agent", "bin"), "rel\\agent\\bin");
        // M2's bug: an absolute-looking tail is appended, NOT substituted.
        assert_eq!(
            join(&env, "C:\\a\\sessions\\--x--", "C:\\b\\a.jsonl"),
            "C:\\a\\sessions\\--x--\\C:\\b\\a.jsonl"
        );
        // An empty tail collapses to the base, as node's join does.
        assert_eq!(join(&env, "C:\\agent", ""), "C:\\agent");
    }

    #[test]
    fn object_entries_covers_the_shapes_js_would_enumerate() {
        let object: Value = serde_json::from_str(r#"{"b":1,"a":2}"#).unwrap();
        assert_eq!(
            object_entries(&object),
            vec![
                ("b".to_string(), Value::from(1)),
                ("a".to_string(), Value::from(2))
            ]
        );
        let array: Value = serde_json::from_str("[10,20]").unwrap();
        assert_eq!(
            object_entries(&array),
            vec![
                ("0".to_string(), Value::from(10)),
                ("1".to_string(), Value::from(20))
            ]
        );
        assert_eq!(object_entries(&Value::String("ab".into())).len(), 2);
        assert!(object_entries(&Value::from(5)).is_empty());
        assert!(object_entries(&Value::Null).is_empty());
    }

    #[test]
    fn stringify_matches_json_stringify_null_2() {
        let value: Value = serde_json::from_str(r#"{"a":{},"b":[],"c":1730000000000}"#).unwrap();
        assert_eq!(
            json_stringify_indent2(&value),
            "{\n  \"a\": {},\n  \"b\": [],\n  \"c\": 1730000000000\n}"
        );
    }

    #[test]
    fn keybindings_migration_renames_reorders_and_drops() {
        // The three shapes the M4 fixture records exercise, in miniature.
        let raw: Map<String, Value> = serde_json::from_str(
            r#"{"zzz":"f9","cycleModelForward":"ctrl+p","interrupt":"ctrl+c"}"#,
        )
        .unwrap();
        let (config, migrated) = keybindings::migrate_keybindings_config(&raw);
        assert!(migrated);
        assert_eq!(
            config.keys().collect::<Vec<_>>(),
            ["app.interrupt", "app.model.cycleForward", "zzz"]
        );

        let already: Map<String, Value> =
            serde_json::from_str(r#"{"app.model.cycleForward":"ctrl+p"}"#).unwrap();
        assert!(!keybindings::migrate_keybindings_config(&already).1);

        let both: Map<String, Value> =
            serde_json::from_str(r#"{"interrupt":"legacy","app.interrupt":"new"}"#).unwrap();
        let (config, migrated) = keybindings::migrate_keybindings_config(&both);
        assert!(migrated);
        assert_eq!(config["app.interrupt"], Value::from("new"));
        assert_eq!(config.len(), 1);
    }
}
