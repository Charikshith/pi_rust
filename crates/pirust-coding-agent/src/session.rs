//! Port of `core/session-manager.ts` — session resolution and the on-disk session store.
//!
//! Note no session file exists until the first assistant message (Pi opens with `"wx"`).
//! The cwd -> session-dir encoding is an on-disk compatibility surface; gated by
//! `tests/fixtures/pi/cli/session_dir.cases.jsonl`.
//!
//! # What is ported
//!
//! | Pi | here |
//! |---|---|
//! | `CURRENT_SESSION_VERSION` (`session-manager.ts:30`) | [`CURRENT_SESSION_VERSION`] |
//! | `assertValidSessionId` (`:208-214`) | [`assert_valid_session_id`] |
//! | `createSessionId` (`:204-206`) | [`SessionIdSource::session_id`] (agent-core's `uuidv7`) |
//! | `generateId` (`:217-224`) | [`generate_id`] |
//! | `migrateV1ToV2`/`migrateV2ToV3`/`migrateToCurrentVersion` (`:227-292`) | [`migrate_session_entries`] |
//! | `parseSessionEntries` (`:295-310`) | [`parse_session_entries`] |
//! | `getLatestCompactionEntry` (`:312-319`) | [`get_latest_compaction_entry`] |
//! | `buildSessionPath` (`:330-356`) | [`build_session_path`] |
//! | `buildContextEntries` (`:414-450`) | [`build_context_entries`] |
//! | `getDefaultSessionDirPath` (`:472-477`) | [`SessionEnv::default_session_dir_path`] |
//! | `getDefaultSessionDir` (`:479-485`) | [`SessionEnv::default_session_dir`] |
//! | `loadEntriesFromFile` (`:500-542`) | [`SessionEnv::load_entries_from_file`] |
//! | `readSessionHeader` (`:544-560`) | `read_session_header` (private) |
//! | `findMostRecentSession` (`:572-592`) | [`SessionEnv::find_most_recent_session`] |
//! | `buildSessionInfo` (`:623-701`) | `build_session_info` (private) |
//! | `listSessionsFromDir` (`:747-778`) | `list_sessions_from_dir` (private) |
//! | `SessionManager` (`:791-1623`) | [`SessionManager`] + the factories on [`SessionEnv`] |
//! | `ResolvedSession` (`main.ts:143-147`) | [`ResolvedSession`] |
//! | `findLocalSessionByExactId` (`main.ts:153-161`) | [`find_local_session_by_exact_id`] |
//! | `resolveSessionPath` (`main.ts:163-189`) | [`resolve_session_path`] |
//! | `promptConfirm` (`main.ts:192-203`) | [`SessionPrompts::confirm`] |
//! | `validateForkFlags` (`main.ts:205-219`) | [`validate_fork_flags`] |
//! | `validateSessionIdFlags` (`main.ts:221-242`) | [`validate_session_id_flags`] |
//! | `openSessionOrExit`/`forkSessionOrExit` (`main.ts:244-262`) | private helpers below |
//! | `createSessionManager` (`main.ts:264-355`) | [`create_session_manager`] |
//!
//! # Entries are `serde_json::Value`, deliberately
//!
//! Pi parses every line with `JSON.parse(line) as FileEntry` — **no validation** — and
//! rewrites entries with `JSON.stringify`. So unknown entry types, unknown fields and the
//! original key order all survive a load/rewrite cycle, and JS assignment semantics
//! (`entry.version = 2` appends the key; re-assigning an existing key keeps its position)
//! are directly observable in the v1→v3 migration's output bytes. A `serde_json::Value`
//! with `preserve_order` reproduces all of that; a typed enum would silently drop unknown
//! lines and re-order keys. Hence [`FileEntry`].
//!
//! **This is also why [`pirust_agent_core::harness::types::SessionTreeEntry`] is not reused
//! as the storage type**: two of coding-agent's nine entry types have a *different key
//! order* from agent-core's, because the TS object literals list the fields differently —
//! `appendCustomEntry` (`:1051-1059`) emits `type, customType, data, id, parentId,
//! timestamp` and `appendCustomMessageEntry` (`:1106-1115`) emits `type, customType,
//! content, display, details, id, parentId, timestamp`, whereas every other entry (and all
//! of agent-core) starts `type, id, parentId, timestamp, …`. Spec §11.1 says the two
//! packages' key orders match for all nine shared types; for `custom` and `custom_message`
//! that is **not** true, and `tests/fixtures/pi/agent/entries.corpus.jsonl` (agent-core's
//! own capture) shows the agent-core order. The builders here follow the coding-agent
//! literals.
//!
//! # The three seams Pi does not have
//!
//! - **`getAgentDir()`/`process.cwd()`/`new Date()`/`randomUUID()` → [`SessionEnv`].**
//!   Pi's `SessionManager` statics read four globals; here they are fields of one value, so
//!   the golden suite can point the store at a `tempfile` dir and pin the clock and the
//!   entry ids without mutating the process environment.
//! - **`console.log(chalk.…)` → [`SessionConsole`].** Same shape as
//!   [`crate::migrations::MigrationConsole`], but the session CLI writes to *both* streams
//!   and also uses `chalk.red`, which [`crate::migrations::ConsoleStyle`] has no variant
//!   for — hence a second [`SessionStyle`]/[`SessionStream`] pair here. When a shared
//!   console module lands, the two should merge.
//! - **`process.exit(n)` → [`SessionExit`].** A library cannot exit. Every Pi
//!   `console.error(…); process.exit(1)` pair becomes "write to the console sink, then
//!   return `Err(SessionExit)`", so a test can assert the exact strings *and* the code.
//!
//! # Headless hazards (spec §17.1-17.2)
//!
//! Both interactive prompts inside `createSessionManager` are reachable regardless of TTY,
//! so both are injected through [`SessionPrompts`]:
//!
//! - **`--resume` builds a TUI session picker** (`cli/session-picker.ts:15-55`) even with
//!   stdin redirected, where it never resolves. [`SessionPrompts::select_session`] is the
//!   seam; the TUI itself is feat-006/007. A headless caller passes [`HeadlessPrompts`],
//!   whose implementation returns [`PickerUnavailable`], and [`create_session_manager`]
//!   turns that into `Error: --resume requires an interactive terminal` + exit 1 — the
//!   **intentional divergence** spec §17.1 asks for (Pi hangs).
//! - **the `--session` "global" branch calls `promptConfirm`** on stdin (`main.ts:305-313`).
//!   [`SessionPrompts::confirm`] is the seam. [`HeadlessPrompts::confirm`] returns `false`
//!   **without writing the prompt**, which reproduces what Pi already does on a piped stdin
//!   (EOF → `""` → not "y" → `Aborted.` + exit 0), mirroring how `cli/project-trust.ts`
//!   degrades in print/json modes. [`StdinPrompts`] is the interactive implementation.
//!
//! # Where `session_dir` comes from
//!
//! Every function here takes the already-resolved session directory as an `Option<&str>`
//! parameter. The three-way precedence — `--session-dir` >
//! `$PIRUST_CODING_AGENT_SESSION_DIR` > `settingsManager.getSessionDir()` — lives in
//! `main.ts:573-577` and is `main.rs`'s job (see
//! [`crate::config::ConfigEnv::sessions_dir`]); this module never reads settings and never
//! reads that env var. `None` is Pi's `undefined`, i.e. "use the default dir for this cwd";
//! `Some("")` is JS-falsy and is honoured as such — `:1442` tests `sessionDir ? … : …`
//! while `:1470` tests `sessionDir !== undefined`, and the two disagree for the empty
//! string. Reproduced, not fixed.
//!
//! # Not ported (and why)
//!
//! - `buildSessionContext` (`:457-466`): needs the runtime/context-window plumbing
//!   [`build_context_entries`] feeds, which lands with the harness driver, not here.
//! - `sessionEntryToContextMessages` (`:379-404`) — **update:** this one *is* now ported, as
//!   [`entries_to_agent_messages`], and it stays in this file rather than moving to
//!   `messages.rs`/`sdk.rs` as an earlier version of this note assumed: the projection has to
//!   read `entry_type`/`entry_id`/`entry_parent_id`/`entry_timestamp`, all private to this
//!   module (§ above), so a `messages.rs`-side function would need them re-exported for no
//!   other purpose. `core/messages.ts`'s `createCustomMessage`/`createBranchSummaryMessage`/
//!   `createCompactionSummaryMessage` now exist at
//!   [`pirust_agent_core::harness::messages`] and are what it calls.
//! - `getTree` (`:1239-1277`) and `getChildren` (`:1139-1147`): the `/tree` TUI is feat-007
//!   (spec §18) and nothing headless reads them.
//! - The ≤10-way concurrency of the metadata loads (`:705-745`). Here it is sequential.
//!   Pi's results are index-ordered regardless, so only the interleaving of the progress
//!   callback differs — the `(loaded, total)` count sequence is identical.
//! - **`migrateSessionEntries` has no oracle.** Spec §11.4 names
//!   `packages/coding-agent/test/fixtures/{before-compaction,large-session}.jsonl` as the
//!   v1→v3 oracle; **neither is vendored under `tests/fixtures/pi/`**, so the migration
//!   below is a transcription checked only structurally (its ids are random, so even with
//!   the files a byte comparison would need an injected id source).

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::Arc;

use pirust_agent_core::harness::messages::{
    create_branch_summary_message, create_compaction_summary_message, create_custom_message,
    AgentMessage,
};
use pirust_agent_core::harness::session::uuid::uuidv7;
use pirust_agent_core::harness::session::{Clock, SystemClock};
use pirust_ai::types::UserMessageContent;
use pirust_tools::path_utils::{self, PathEnv, PathInputOptions, Platform as PathPlatform};
use serde_json::{Map, Value};

use crate::config::{ConfigEnv, ConfigPathError, PathError, Platform, SESSIONS_DIR_NAME};
use crate::migrations::encode_session_dir_name;

// =============================================================================
// Constants
// =============================================================================

/// `CURRENT_SESSION_VERSION` (`session-manager.ts:30`).
pub const CURRENT_SESSION_VERSION: u64 = 3;

/// `SESSION_READ_BUFFER_SIZE` (`session-manager.ts:487`) — Pi's 1 MiB chunk size for
/// `loadEntriesFromFile`. Kept as documentation: the port reads the whole file (see
/// [`SessionEnv::load_entries_from_file`]).
pub const SESSION_READ_BUFFER_SIZE: usize = 1024 * 1024;

/// The fixed number of bytes `readSessionHeader` reads (`session-manager.ts:547-548`).
const SESSION_HEADER_PROBE_BYTES: usize = 512;

/// The suffix every session file has (`:577`, `:760`, `:1593`).
const SESSION_FILE_SUFFIX: &str = ".jsonl";

/// `assertValidSessionId`'s message (`session-manager.ts:210-212`), verbatim.
pub const INVALID_SESSION_ID_MESSAGE: &str = "Session id must be non-empty, contain only alphanumeric characters, '-', '_', and '.', and start and end with an alphanumeric character";

// =============================================================================
// Errors
// =============================================================================

/// A failure `session-manager.ts` reports by `throw`ing.
///
/// The `Display` text of the first five variants is Pi's `Error.message` verbatim (spec
/// §3.6) — `main.ts:249`/`:259` interpolate it into `` `Error: ${message}` ``.
/// [`SessionError::Io`] and [`SessionError::Path`] cover throws whose message is Node's,
/// which is **not** reproduced.
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    /// `assertValidSessionId` (`:209-213`).
    #[error("{INVALID_SESSION_ID_MESSAGE}")]
    InvalidSessionId,
    /// `setSessionFile` on a non-empty file that does not parse as a session (`:836`).
    #[error("Session file is not a valid pi session: {0}")]
    NotAPiSession(String),
    /// `forkFrom` with an empty/invalid source (`:1500`).
    #[error("Cannot fork: source session file is empty or invalid: {0}")]
    ForkSourceEmpty(String),
    /// `forkFrom` with a headerless source (`:1505`).
    #[error("Cannot fork: source session has no header: {0}")]
    ForkSourceNoHeader(String),
    /// `branch`/`appendLabelChange`/`createBranchedSession` on an unknown entry (`:1163`,
    /// `:1291`, `:1312`, `:1338`).
    #[error("Entry {0} not found")]
    EntryNotFound(String),
    /// `resolvePath`/`normalizePath` threw — a `file://` input (`utils/paths.ts:74-76`).
    #[error(transparent)]
    Path(#[from] PathError),
    /// `getAgentDir()` threw (`config.ts:520`).
    #[error(transparent)]
    Config(#[from] ConfigPathError),
    /// A filesystem call Pi lets propagate: `mkdirSync`, `statSync`, `openSync` (including
    /// the `"wx"` exclusive create), `writeFileSync`, `appendFileSync`. Node's wording
    /// (`EEXIST: file already exists, open '…'`) is not reproduced.
    #[error("{operation} {path}: {source}")]
    Io {
        /// Which operation — `mkdir`, `stat`, `open`, `write` or `append`.
        operation: &'static str,
        /// The path it was applied to.
        path: String,
        /// The underlying failure.
        #[source]
        source: std::io::Error,
    },
}

impl SessionError {
    fn io(operation: &'static str, path: &str, source: std::io::Error) -> Self {
        Self::Io {
            operation,
            path: path.to_string(),
            source,
        }
    }
}

/// `process.exit(code)`, reached after the console sink was written.
///
/// Returned instead of exiting so callers (and tests) stay in control; `main.rs` maps it to
/// `std::process::exit`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("exit({code})")]
pub struct SessionExit {
    /// The status Pi passes to `process.exit` — `1` for every error, `0` for the two
    /// user-declined paths (`Aborted.`, `No session selected`).
    pub code: i32,
}

impl SessionExit {
    /// `process.exit(1)`.
    pub const FAILURE: Self = Self { code: 1 };
    /// `process.exit(0)`.
    pub const SUCCESS: Self = Self { code: 0 };
}

// =============================================================================
// The console seam (`console.log`/`console.error` + chalk)
// =============================================================================

/// Which stream a message goes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionStream {
    /// `console.log`.
    Stdout,
    /// `console.error`.
    Stderr,
}

/// Which `chalk` helper wraps the line. Nothing here emits ANSI — this mirrors
/// [`crate::migrations::ConsoleStyle`], plus the `red` it lacks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionStyle {
    /// `chalk.red` — every fatal message.
    Red,
    /// `chalk.yellow` — `Session found in different project: …` and the
    /// `Warning: No project session found …` line.
    Yellow,
    /// `chalk.dim` — `Aborted.` and `No session selected`.
    Dim,
}

/// Where the session CLI's `console.log`/`console.error` calls go.
pub trait SessionConsole {
    /// One console call, without the trailing newline the console appends.
    fn write(&mut self, stream: SessionStream, style: SessionStyle, text: &str);
}

/// Production sink — `console.log` → stdout, `console.error` → stderr.
#[derive(Debug, Clone, Copy, Default)]
pub struct StdioConsole;

impl SessionConsole for StdioConsole {
    fn write(&mut self, stream: SessionStream, _style: SessionStyle, text: &str) {
        match stream {
            SessionStream::Stdout => println!("{text}"),
            SessionStream::Stderr => eprintln!("{text}"),
        }
    }
}

/// One captured console call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionConsoleLine {
    /// Which stream.
    pub stream: SessionStream,
    /// Which `chalk` helper.
    pub style: SessionStyle,
    /// The text, without the appended newline.
    pub text: String,
}

/// Recording sink, so the exact §3.6 strings can be asserted.
///
/// Not `#[cfg(test)]`: the golden suite is a separate crate.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CapturingConsole {
    /// Every call, in order.
    pub lines: Vec<SessionConsoleLine>,
}

impl CapturingConsole {
    /// Just the texts, in call order.
    pub fn texts(&self) -> Vec<&str> {
        self.lines.iter().map(|line| line.text.as_str()).collect()
    }
}

impl SessionConsole for CapturingConsole {
    fn write(&mut self, stream: SessionStream, style: SessionStyle, text: &str) {
        self.lines.push(SessionConsoleLine {
            stream,
            style,
            text: text.to_string(),
        });
    }
}

// =============================================================================
// The prompt seam (the two headless hazards, spec §17.1-17.2)
// =============================================================================

/// `selectSession` could not run — there is no interactive terminal to draw the picker on.
///
/// Pi has no such state: it builds the TUI unconditionally and, with stdin redirected,
/// never resolves. See the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("--resume requires an interactive terminal")]
pub struct PickerUnavailable;

/// The two loaders `selectSession` is handed (`main.ts:323-327`), as a value.
///
/// A picker implementation calls [`SessionLoaders::current`]/[`SessionLoaders::all`] to
/// fill its list, exactly as Pi's `SessionSelectorComponent` invokes its two callbacks —
/// which is what keeps the loading (and its progress reporting) lazy.
pub struct SessionLoaders<'a> {
    env: &'a SessionEnv,
    cwd: &'a str,
    session_dir: Option<&'a str>,
}

impl SessionLoaders<'_> {
    /// `(onProgress) => SessionManager.list(cwd, sessionDir, onProgress)`.
    pub fn current(
        &self,
        on_progress: Option<&mut dyn FnMut(usize, usize)>,
    ) -> Result<Vec<SessionInfo>, SessionError> {
        self.env.list(self.cwd, self.session_dir, on_progress)
    }

    /// `(onProgress) => SessionManager.listAll(sessionDir, onProgress)`.
    pub fn all(
        &self,
        on_progress: Option<&mut dyn FnMut(usize, usize)>,
    ) -> Result<Vec<SessionInfo>, SessionError> {
        self.env.list_all(self.session_dir, on_progress)
    }
}

/// The two interactive prompts `createSessionManager` can reach.
pub trait SessionPrompts {
    /// `promptConfirm(message)` (`main.ts:192-203`).
    ///
    /// The implementation owns the whole interaction, including writing
    /// `` `${message} [y/N] ` `` (no newline) to stdout, because that is what `rl.question`
    /// does. `true` only for `y`/`yes`, case-insensitively.
    fn confirm(&mut self, message: &str) -> bool;

    /// `selectSession(currentLoader, allLoader, settingsManager)`
    /// (`cli/session-picker.ts:15-55`).
    ///
    /// `Ok(None)` is Pi's `null` (cancelled). `Err(PickerUnavailable)` has no Pi
    /// counterpart — see the module docs.
    fn select_session(
        &mut self,
        loaders: &SessionLoaders<'_>,
    ) -> Result<Option<String>, PickerUnavailable>;
}

/// The non-prompting implementation a headless caller (`print`/`json`/`rpc`) passes.
///
/// `confirm` declines **without writing anything**, which is the state Pi already reaches
/// on a piped stdin; `select_session` refuses so `--resume` fails fast instead of hanging.
#[derive(Debug, Clone, Copy, Default)]
pub struct HeadlessPrompts;

impl SessionPrompts for HeadlessPrompts {
    fn confirm(&mut self, _message: &str) -> bool {
        false
    }

    fn select_session(
        &mut self,
        _loaders: &SessionLoaders<'_>,
    ) -> Result<Option<String>, PickerUnavailable> {
        Err(PickerUnavailable)
    }
}

/// The interactive `promptConfirm`: `` `${message} [y/N] ` `` on stdout, one line of stdin.
///
/// Still refuses the picker — the TUI is feat-006/007. EOF (or an unreadable stdin) yields
/// the empty answer, i.e. "no", exactly as `readline` does.
#[derive(Debug, Clone, Copy, Default)]
pub struct StdinPrompts;

impl SessionPrompts for StdinPrompts {
    fn confirm(&mut self, message: &str) -> bool {
        print!("{message} [y/N] ");
        let _ = std::io::stdout().flush();
        let mut answer = String::new();
        if std::io::stdin().read_line(&mut answer).is_err() {
            return false;
        }
        let answer = answer.trim_end_matches(['\r', '\n']).to_lowercase();
        answer == "y" || answer == "yes"
    }

    fn select_session(
        &mut self,
        _loaders: &SessionLoaders<'_>,
    ) -> Result<Option<String>, PickerUnavailable> {
        Err(PickerUnavailable)
    }
}

// =============================================================================
// The id seam (`createSessionId` / `randomUUID`)
// =============================================================================

/// The two id generators `session-manager.ts` calls.
///
/// Injectable so the golden suite can pin both, the way agent-core's
/// [`pirust_agent_core::harness::session::uuid::Uuidv7Source`] is injectable.
pub trait SessionIdSource: Send + Sync {
    /// `createSessionId()` = `uuidv7()` (`:204-206`) — the *session* id, which also lands
    /// in the file name.
    fn session_id(&self) -> String;

    /// `randomUUID()` (`:218`, `:223`) — a v4 UUID, of which `generateId` keeps the first 8
    /// hex characters as an *entry* id.
    fn random_uuid(&self) -> String;
}

/// Production [`SessionIdSource`].
///
/// `session_id` is agent-core's monotonic `uuidv7` — the same function Pi imports from
/// `@earendil-works/pi-agent-core` (`session-manager.ts:1`), reused rather than re-ported.
///
/// `random_uuid` is **not** a CSPRNG, unlike `crypto.randomUUID`: this crate may not add a
/// `rand`/`uuid` dependency, so the bits come from `RandomState` (OS-seeded SipHash keys,
/// freshly generated per instance), the nanosecond clock and a process-wide counter. The
/// value is only ever used for a collision-checked 8-hex local entry id, never for anything
/// security-bearing; swap in a CSPRNG here if that ever changes.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemIds;

impl SessionIdSource for SystemIds {
    fn session_id(&self) -> String {
        uuidv7()
    }

    fn random_uuid(&self) -> String {
        random_uuid_v4()
    }
}

/// `randomUUID()`'s shape: `xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx`, lowercase hex.
fn random_uuid_v4() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);

    let mut first = RandomState::new().build_hasher();
    first.write_u64(nanos);
    first.write_u64(seq);
    let hi = first.finish();
    let mut second = RandomState::new().build_hasher();
    second.write_u64(hi);
    second.write_u64(nanos.rotate_left(32));
    let lo = second.finish();

    let time_low = (hi >> 32) as u32;
    let time_mid = (hi >> 16) as u16;
    let time_hi = ((hi as u16) & 0x0fff) | 0x4000;
    let clock_seq = (((lo >> 48) as u16) & 0x3fff) | 0x8000;
    let node = lo & 0xffff_ffff_ffff;
    format!("{time_low:08x}-{time_mid:04x}-{time_hi:04x}-{clock_seq:04x}-{node:012x}")
}

/// `generateId(byId)` (`session-manager.ts:217-224`): up to 100 tries at
/// `randomUUID().slice(0, 8)`, falling back to a full UUID.
///
/// `taken` is the `{ has(id) }` duck type Pi passes — either the entry index or, in
/// `createBranchedSession`, an ad-hoc `Set`.
pub fn generate_id(ids: &dyn SessionIdSource, taken: &dyn Fn(&str) -> bool) -> String {
    for _ in 0..100 {
        let uuid = ids.random_uuid();
        // `slice(0, 8)` on an ASCII UUID.
        let id = uuid.get(..8).unwrap_or(uuid.as_str()).to_string();
        if !taken(&id) {
            return id;
        }
    }
    ids.random_uuid()
}

/// `assertValidSessionId(id)` (`session-manager.ts:208-214`).
///
/// The regex is `^[A-Za-z0-9](?:[A-Za-z0-9._-]*[A-Za-z0-9])?$`, so a single alphanumeric
/// character is valid (the group is optional) and the first/last character may not be `.`,
/// `_` or `-`. `A-Za-z0-9` is ASCII-only: `é1` is rejected.
pub fn assert_valid_session_id(id: &str) -> Result<(), SessionError> {
    fn alnum(c: char) -> bool {
        c.is_ascii_alphanumeric()
    }
    let mut chars = id.chars();
    let valid = match chars.next() {
        None => false,
        Some(first) if !alnum(first) => false,
        Some(_) => {
            let rest: Vec<char> = chars.collect();
            match rest.split_last() {
                // Just the one character: the optional group matched empty.
                None => true,
                Some((last, middle)) => {
                    alnum(*last)
                        && middle
                            .iter()
                            .all(|c| alnum(*c) || matches!(c, '.' | '_' | '-'))
                }
            }
        }
    };
    if valid {
        Ok(())
    } else {
        Err(SessionError::InvalidSessionId)
    }
}

// =============================================================================
// Entry representation
// =============================================================================

/// One line of a session file — the header or an entry.
///
/// `SessionHeader | SessionEntry` (`session-manager.ts:152`). Untyped on purpose; see the
/// module docs.
pub type FileEntry = Value;

/// `entry.type === "session"` (`:845`, `:1221`, `:1231`).
///
/// A non-string `type` is not `"session"` in JS either, so it counts as an entry.
pub fn is_header(entry: &FileEntry) -> bool {
    entry.get("type").and_then(Value::as_str) == Some("session")
}

/// The `type` tag, or `None` when absent/non-string.
fn entry_type(entry: &FileEntry) -> Option<&str> {
    entry.get("type").and_then(Value::as_str)
}

/// The `id` field, or `None` when absent/non-string.
///
/// Pi indexes such an entry under the JS `undefined` key, which no string lookup can hit
/// and which `generateId`'s collision check therefore ignores — so skipping it (what the
/// callers here do) is equivalent for everything ported.
fn entry_id(entry: &FileEntry) -> Option<&str> {
    entry.get("id").and_then(Value::as_str)
}

/// `entry.parentId`, or `None` for `null`/absent.
fn entry_parent_id(entry: &FileEntry) -> Option<&str> {
    entry.get("parentId").and_then(Value::as_str)
}

/// `entry.timestamp`, or `None` for `null`/absent/non-string.
///
/// Not read anywhere else in this file (`entry_head`/`entry_tail` only ever write it), but
/// [`BranchInfo`] surfaces it for a `/tree` picker, so it needs a reader.
fn entry_timestamp(entry: &FileEntry) -> Option<&str> {
    entry.get("timestamp").and_then(Value::as_str)
}

/// `e.type === "message" && e.message.role === "assistant"` (`:949`, `:1404`).
///
/// Pi would throw a `TypeError` on a `type: "message"` entry with no `message` field (only
/// reachable in a hand-edited file); this answers `false`.
fn is_assistant_message(entry: &FileEntry) -> bool {
    entry_type(entry) == Some("message")
        && entry
            .get("message")
            .and_then(|message| message.get("role"))
            .and_then(Value::as_str)
            == Some("assistant")
}

/// One `JSON.stringify(entry)` line, newline included.
fn to_line(entry: &FileEntry) -> String {
    // `serde_json::to_string` of a `Value` cannot fail: no NaN, no non-string keys.
    format!(
        "{}\n",
        serde_json::to_string(entry).expect("a serde_json::Value always serializes")
    )
}

/// `existsSync` — `false` rather than a throw when the path cannot be stat'd.
fn exists(path: &str) -> bool {
    Path::new(path).exists()
}

// =============================================================================
// The environment seam
// =============================================================================

/// Everything `session-manager.ts` reads as a global: `getAgentDir()`/`getSessionsDir()`
/// (via [`ConfigEnv`]), `process.cwd()` (`resolvePath`'s default base), `new Date()` and
/// the two id generators.
///
/// Pi's `SessionManager` statics are methods here, because they all need this ambience:
///
/// | Pi | here |
/// |---|---|
/// | `SessionManager.create(cwd, dir?, options?)` (`:1441`) | [`SessionEnv::create`] |
/// | `SessionManager.open(path, dir?, cwdOverride?)` (`:1452`) | [`SessionEnv::open`] |
/// | `SessionManager.continueRecent(cwd, dir?)` (`:1468`) | [`SessionEnv::continue_recent`] |
/// | `SessionManager.inMemory(cwd?, options?)` (`:1479`) | [`SessionEnv::in_memory`] |
/// | `SessionManager.forkFrom(src, cwd, dir?, options?)` (`:1490`) | [`SessionEnv::fork_from`] |
/// | `SessionManager.list(cwd, dir?, onProgress?)` (`:1549`) | [`SessionEnv::list`] |
/// | `SessionManager.listAll(dir?, onProgress?)` (`:1566`) | [`SessionEnv::list_all`] |
#[derive(Clone)]
pub struct SessionEnv {
    /// The path/identity ambience — `getAgentDir()` and `getSessionsDir()`.
    pub config: ConfigEnv,
    /// `process.cwd()`: `resolvePath`'s default `baseDir` (`utils/paths.ts:81`) and
    /// `node:path`'s last-resort base — which is what supplies the **drive letter** when a
    /// rooted-but-driveless path like `/home/me/proj` is resolved on win32.
    pub process_cwd: String,
    /// `createSessionId` / `randomUUID`.
    pub ids: Arc<dyn SessionIdSource>,
    /// `new Date().toISOString()` — agent-core's [`Clock`], reused.
    pub clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for SessionEnv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionEnv")
            .field("config", &self.config)
            .field("process_cwd", &self.process_cwd)
            .field("ids", &"<dyn SessionIdSource>")
            .field("clock", &"<dyn Clock>")
            .finish()
    }
}

impl SessionEnv {
    /// Snapshot the real process environment under the production identity.
    pub fn from_process_env() -> Self {
        Self::new(ConfigEnv::from_process_env(), path_utils::cwd())
    }

    /// A store with the default id source and wall clock.
    pub fn new(config: ConfigEnv, process_cwd: impl Into<String>) -> Self {
        Self {
            config,
            process_cwd: process_cwd.into(),
            ids: Arc::new(SystemIds),
            clock: Arc::new(SystemClock),
        }
    }

    // -------------------------------------------------------------------------
    // Path plumbing
    // -------------------------------------------------------------------------

    /// The `PathEnv` `path_utils` wants.
    ///
    /// **Narrowing:** an unset `os.homedir()` becomes `""` here, whereas Pi throws from
    /// `homedir()` — reachable only through a `~`-prefixed `--session-dir`/`--session` on a
    /// machine with no home directory.
    fn path_env(&self) -> PathEnv {
        PathEnv {
            platform: if self.config.platform == Platform::Win32 {
                PathPlatform::Win32
            } else {
                PathPlatform::Posix
            },
            home_dir: self.config.home_dir.clone().unwrap_or_default(),
            cwd: self.process_cwd.clone(),
        }
    }

    /// `normalizePath(input)` with no options (`utils/paths.ts:57-79`).
    pub fn normalize_path(&self, input: &str) -> Result<String, PathError> {
        path_utils::normalize_path(&self.path_env(), input, &PathInputOptions::default())
    }

    /// `resolvePath(input)` — `baseDir` defaults to `process.cwd()`
    /// (`utils/paths.ts:81-85`).
    pub fn resolve_path(&self, input: &str) -> Result<String, PathError> {
        self.resolve_path_in(input, &self.process_cwd)
    }

    /// `resolvePath(input, baseDir)` — the two-argument form (`main.ts:166`).
    pub fn resolve_path_in(&self, input: &str, base_dir: &str) -> Result<String, PathError> {
        path_utils::resolve_path(
            &self.path_env(),
            input,
            base_dir,
            &PathInputOptions::default(),
        )
    }

    /// Node's `path.join(base, ...tail)`.
    ///
    /// Same delegation `config.rs`'s private `node_join` documents: `normalizePath`'s tilde
    /// branch *is* `platform.join([home, rest])` (`utils/paths.ts:70`), so `"~/<tail>"` with
    /// `home_dir = base` yields `join` from [`pirust_tools::path_utils`]'s verified
    /// transcription — whose own `join` is private. Every `tail` used here is
    /// separator-free, so pre-joining with `/` is safe (`node:path` normalizes once, and
    /// win32 treats `/` and `\` alike).
    fn join(&self, base: &str, tail: &[&str]) -> String {
        let options = PathInputOptions {
            home_dir: Some(base),
            ..PathInputOptions::default()
        };
        path_utils::normalize_path(&self.path_env(), &format!("~/{}", tail.join("/")), &options)
            .expect("normalizePath's tilde branch returns before the file:// branch")
    }

    // -------------------------------------------------------------------------
    // session-manager.ts:468-485 — the default session directory
    // -------------------------------------------------------------------------

    /// `getDefaultSessionDirPath(cwd, agentDir = getAgentDir())`
    /// (`session-manager.ts:472-477`), the pure variant:
    ///
    /// ```text
    /// const resolvedCwd = resolvePath(cwd);
    /// const resolvedAgentDir = resolvePath(agentDir);
    /// const safePath = `--${resolvedCwd.replace(/^[/\\]/, "").replace(/[/\\:]/g, "-")}--`;
    /// return join(resolvedAgentDir, "sessions", safePath);
    /// ```
    ///
    /// **Resolve first, then encode.** [`encode_session_dir_name`] is exactly that
    /// `replace`…`replace` expression and — as its own docs say — does *not* resolve:
    /// `migrations.ts:109-112` encodes the raw `header.cwd`, this call site encodes
    /// `resolvePath(cwd)`. That single difference is what makes the two halves of
    /// `tests/fixtures/pi/cli/session_dir.cases.jsonl` disagree for the same input on
    /// win32 — `/home/user/project` → `--home-user-project--` for the migration but
    /// `--C--home-user-project--` here, because `path.win32.resolve` supplies the drive
    /// letter of [`SessionEnv::process_cwd`]. The 9 `getDefaultSessionDirPath` records pin
    /// it.
    ///
    /// `agent_dir` is Pi's optional second parameter; `None` is its `getAgentDir()` default.
    pub fn default_session_dir_path(
        &self,
        cwd: &str,
        agent_dir: Option<&str>,
    ) -> Result<String, SessionError> {
        let agent_dir = match agent_dir {
            Some(dir) => dir.to_string(),
            None => self.config.agent_dir()?,
        };
        let resolved_cwd = self.resolve_path(cwd)?;
        let resolved_agent_dir = self.resolve_path(&agent_dir)?;
        let safe_path = encode_session_dir_name(&resolved_cwd);
        Ok(self.join(&resolved_agent_dir, &[SESSIONS_DIR_NAME, &safe_path]))
    }

    /// `getDefaultSessionDir(cwd, agentDir?)` (`session-manager.ts:479-485`) — the same
    /// path, `mkdirSync(recursive)`'d when missing.
    pub fn default_session_dir(
        &self,
        cwd: &str,
        agent_dir: Option<&str>,
    ) -> Result<String, SessionError> {
        let session_dir = self.default_session_dir_path(cwd, agent_dir)?;
        if !exists(&session_dir) {
            fs::create_dir_all(&session_dir)
                .map_err(|e| SessionError::io("mkdir", &session_dir, e))?;
        }
        Ok(session_dir)
    }

    /// The directory a factory uses: `sessionDir ? normalizePath(sessionDir) :
    /// getDefaultSessionDir(cwd)` (`:1442`, `:1469`, `:1508`, `:1550`).
    ///
    /// Note the JS truthiness: `Some("")` takes the *default* branch.
    fn resolve_session_dir(
        &self,
        cwd: &str,
        session_dir: Option<&str>,
    ) -> Result<String, SessionError> {
        match session_dir.filter(|dir| !dir.is_empty()) {
            Some(dir) => Ok(self.normalize_path(dir)?),
            None => self.default_session_dir(cwd, None),
        }
    }

    // -------------------------------------------------------------------------
    // session-manager.ts:489-592 — reading files
    // -------------------------------------------------------------------------

    /// `loadEntriesFromFile(filePath)` (`session-manager.ts:500-542`).
    ///
    /// `normalizePath` (not `resolvePath`) the path, `[]` when it does not exist, then parse
    /// line by line skipping unparseable lines, and finally **validate that `entries[0]` is
    /// `{type:"session", id:<string>}`, returning `[]` if not** (`:534-541`).
    ///
    /// Pi streams 1 MiB chunks through a `StringDecoder`; this reads the file once and
    /// decodes lossily, which is what Node's utf-8 decoder does with invalid bytes (U+FFFD)
    /// — the resulting line set is identical. A CRLF file keeps its `\r` at the end of each
    /// line in both, where it is trailing JSON whitespace and thus ignored by both parsers.
    pub fn load_entries_from_file(&self, file_path: &str) -> Result<Vec<FileEntry>, SessionError> {
        let resolved = self.normalize_path(file_path)?;
        if !exists(&resolved) {
            return Ok(Vec::new());
        }
        let bytes = fs::read(&resolved).map_err(|e| SessionError::io("open", &resolved, e))?;
        let content = String::from_utf8_lossy(&bytes);
        let mut entries: Vec<FileEntry> = Vec::new();
        for line in content.split('\n') {
            if let Some(entry) = parse_session_entry_line(line) {
                entries.push(entry);
            }
        }

        // :534-541 — header validation.
        match entries.first() {
            None => Ok(entries),
            Some(header) if is_header(header) && entry_id(header).is_some() => Ok(entries),
            Some(_) => Ok(Vec::new()),
        }
    }

    /// `findMostRecentSession(sessionDir, cwd?)` (`session-manager.ts:572-592`).
    ///
    /// `*.jsonl` in that one directory whose header parses, optionally filtered by
    /// [`session_cwd_matches`], sorted by mtime **descending**; the first, or `None`.
    ///
    /// The whole body after the two path normalizations is one `try`/`catch { return null }`,
    /// so a failing `readdirSync` *or* a failing `statSync` on any candidate yields `None`.
    /// The two normalizations are outside it and propagate (hence the `Result`).
    ///
    /// Tie-breaking inherits `readdirSync` order, as in Pi: the sort is stable in V8 and in
    /// Rust alike.
    pub fn find_most_recent_session(
        &self,
        session_dir: &str,
        cwd: Option<&str>,
    ) -> Result<Option<String>, SessionError> {
        let resolved_session_dir = self.normalize_path(session_dir)?;
        let resolved_cwd = match cwd {
            Some(cwd) => Some(self.resolve_path(cwd)?),
            None => None,
        };

        let Ok(read_dir) = fs::read_dir(&resolved_session_dir) else {
            return Ok(None);
        };
        let mut candidates: Vec<(String, i64)> = Vec::new();
        for entry in read_dir.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.ends_with(SESSION_FILE_SUFFIX) {
                continue;
            }
            let path = self.join(&resolved_session_dir, &[&name]);
            let Some(header) = read_session_header(&path) else {
                continue;
            };
            if let Some(resolved_cwd) = &resolved_cwd {
                let header_cwd = session_header_cwd(&header);
                if !self.session_cwd_matches(header_cwd, resolved_cwd)? {
                    continue;
                }
            }
            // `statSync(path).mtime` inside the same try: a failure aborts the whole thing.
            let Some(mtime) = mtime_ms(&path) else {
                return Ok(None);
            };
            candidates.push((path, mtime));
        }
        candidates.sort_by_key(|candidate| std::cmp::Reverse(candidate.1));
        Ok(candidates.into_iter().next().map(|(path, _)| path))
    }

    /// `sessionCwdMatches(cwd, resolvedCwd)` (`session-manager.ts:567-569`):
    /// `cwd !== undefined && cwd !== "" && resolvePath(cwd) === resolvedCwd`.
    ///
    /// The comparison is exact and case-sensitive, on win32 too.
    pub fn session_cwd_matches(
        &self,
        cwd: Option<&str>,
        resolved_cwd: &str,
    ) -> Result<bool, SessionError> {
        match cwd.filter(|cwd| !cwd.is_empty()) {
            None => Ok(false),
            Some(cwd) => Ok(self.resolve_path(cwd)? == resolved_cwd),
        }
    }

    // -------------------------------------------------------------------------
    // session-manager.ts:747-778, 1549-1622 — listing
    // -------------------------------------------------------------------------

    /// `SessionManager.list(cwd, sessionDir?, onProgress?)`
    /// (`session-manager.ts:1549-1558`).
    ///
    /// `filterCwd = sessionDir !== undefined && dir !== getDefaultSessionDirPath(cwd)` —
    /// note it tests `!== undefined` while the directory choice above tests truthiness, so
    /// `Some("")` gives `filterCwd = false` *because* the two paths then agree, not because
    /// the flag is off. Sorted by `modified` descending.
    pub fn list(
        &self,
        cwd: &str,
        session_dir: Option<&str>,
        on_progress: Option<&mut dyn FnMut(usize, usize)>,
    ) -> Result<Vec<SessionInfo>, SessionError> {
        let dir = self.resolve_session_dir(cwd, session_dir)?;
        let filter_cwd =
            session_dir.is_some() && dir != self.default_session_dir_path(cwd, None)?;
        let resolved_cwd = self.resolve_path(cwd)?;

        let mut sessions = list_sessions_from_dir(&dir, on_progress);
        if filter_cwd {
            let mut kept = Vec::with_capacity(sessions.len());
            for session in sessions {
                if self
                    .session_cwd_matches(Some(session.cwd.as_str()), &resolved_cwd)
                    .unwrap_or(false)
                {
                    kept.push(session);
                }
            }
            sessions = kept;
        }
        sessions.sort_by_key(|session| std::cmp::Reverse(session.modified));
        Ok(sessions)
    }

    /// `SessionManager.listAll(sessionDir?, onProgress?)`
    /// (`session-manager.ts:1564-1622`).
    ///
    /// A custom directory lists only that directory. Otherwise every **immediate
    /// subdirectory** of `getSessionsDir()` is scanned for `*.jsonl` (no recursion), with
    /// `total` counted up front so the progress fraction is accurate. Sorted by `modified`
    /// descending.
    ///
    /// `normalizePath("")` is `""`, which `if (customSessionDir)` treats as falsy — so
    /// `Some("")` scans the whole store, exactly as in Pi. `getSessionsDir()` is read
    /// *before* the `try`, so a missing home directory propagates; everything after it is
    /// swallowed into an empty list.
    pub fn list_all(
        &self,
        session_dir: Option<&str>,
        mut on_progress: Option<&mut dyn FnMut(usize, usize)>,
    ) -> Result<Vec<SessionInfo>, SessionError> {
        if let Some(custom) = session_dir.filter(|dir| !dir.is_empty()) {
            let custom = self.normalize_path(custom)?;
            if !custom.is_empty() {
                let mut sessions = list_sessions_from_dir(&custom, on_progress);
                sessions.sort_by_key(|session| std::cmp::Reverse(session.modified));
                return Ok(sessions);
            }
        }

        let sessions_dir = self.config.sessions_dir()?;
        if !exists(&sessions_dir) {
            return Ok(Vec::new());
        }
        let Ok(read_dir) = fs::read_dir(&sessions_dir) else {
            return Ok(Vec::new());
        };

        let mut all_files: Vec<String> = Vec::new();
        for entry in read_dir.flatten() {
            if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let dir = self.join(&sessions_dir, &[&entry.file_name().to_string_lossy()]);
            let Ok(files) = fs::read_dir(&dir) else {
                continue;
            };
            for file in files.flatten() {
                let name = file.file_name().to_string_lossy().into_owned();
                if name.ends_with(SESSION_FILE_SUFFIX) {
                    all_files.push(self.join(&dir, &[&name]));
                }
            }
        }

        let total = all_files.len();
        let mut sessions = Vec::new();
        for (index, file) in all_files.iter().enumerate() {
            let info = build_session_info(file);
            if let Some(on_progress) = on_progress.as_deref_mut() {
                on_progress(index + 1, total);
            }
            if let Some(info) = info {
                sessions.push(info);
            }
        }
        sessions.sort_by_key(|session| std::cmp::Reverse(session.modified));
        Ok(sessions)
    }
}

// =============================================================================
// Parsing (`session-manager.ts:295-310`, `:489-497`)
// =============================================================================

/// `parseSessionEntryLine(line)` (`session-manager.ts:489-497`): `null` for a blank line or
/// a parse failure.
///
/// `line.trim()` is JS's trim, but only its emptiness is tested here and the JS/Rust
/// whitespace sets agree on every character that can make a line "blank" in practice; a
/// line of only U+0085 is non-blank to JS, and `serde_json` then rejects it just as
/// `JSON.parse` does.
pub fn parse_session_entry_line(line: &str) -> Option<FileEntry> {
    if line.trim().is_empty() {
        return None;
    }
    serde_json::from_str(line).ok()
}

/// `parseSessionEntries(content)` (`session-manager.ts:295-310`) — exported for
/// `compaction.test.ts`.
///
/// Note it `trim()`s the **whole content** first and then splits on `\n`, so a file ending
/// in a newline does not yield a trailing empty entry. Unlike
/// [`SessionEnv::load_entries_from_file`], it does **not** validate the header.
pub fn parse_session_entries(content: &str) -> Vec<FileEntry> {
    content
        .trim()
        .split('\n')
        .filter_map(parse_session_entry_line)
        .collect()
}

/// `readSessionHeader(filePath)` (`session-manager.ts:544-560`).
///
/// Reads **at most 512 bytes from offset 0 in a single `read`**, takes everything before the
/// first `\n`, and requires `type === "session"` and a string `id`. Any failure (missing
/// file, no newline within 512 bytes, invalid JSON) is `None`. A multi-byte character
/// straddling the 512-byte boundary decodes to U+FFFD in Node's `Buffer#toString` too.
fn read_session_header(file_path: &str) -> Option<FileEntry> {
    let mut file = fs::File::open(file_path).ok()?;
    let mut buffer = [0u8; SESSION_HEADER_PROBE_BYTES];
    let read = file.read(&mut buffer).ok()?;
    let text = String::from_utf8_lossy(&buffer[..read]);
    let first_line = text.split('\n').next()?;
    if first_line.is_empty() {
        return None;
    }
    let header: Value = serde_json::from_str(first_line).ok()?;
    if !is_header(&header) || entry_id(&header).is_none() {
        return None;
    }
    Some(header)
}

/// `getSessionHeaderCwd(header)` (`session-manager.ts:562-565`) — `undefined` unless the
/// value is a string.
fn session_header_cwd(header: &FileEntry) -> Option<&str> {
    header.get("cwd").and_then(Value::as_str)
}

// =============================================================================
// The v1 → v3 migration (`session-manager.ts:227-292`)
// =============================================================================

/// `migrateToCurrentVersion(entries)` (`:277-287`), which Pi also exports as the
/// void-returning `migrateSessionEntries` (`:290-292`).
///
/// Reads `header?.version ?? 1`; returns `false` when that is `>= 3`; otherwise runs
/// [v1→v2](migrate_v1_to_v2) when `< 2` and [v2→v3](migrate_v2_to_v3) when `< 3`, mutating
/// in place, and returns `true`. [`SessionManager::set_session_file`] rewrites the file
/// whenever this returns `true`.
///
/// A **non-numeric** `version` (only reachable in a hand-edited file) makes every JS
/// relational comparison false, so nothing migrates but `true` is still returned — i.e. the
/// file is rewritten unchanged. Reproduced: `None` here means "NaN".
pub fn migrate_session_entries(entries: &mut [FileEntry], ids: &dyn SessionIdSource) -> bool {
    let version = header_version(entries);

    if version.is_some_and(|version| version >= CURRENT_SESSION_VERSION as f64) {
        return false;
    }
    if version.is_some_and(|version| version < 2.0) {
        migrate_v1_to_v2(entries, ids);
    }
    if version.is_some_and(|version| version < 3.0) {
        migrate_v2_to_v3(entries);
    }
    true
}

/// `header?.version ?? 1` as a JS number; `None` models NaN.
fn header_version(entries: &[FileEntry]) -> Option<f64> {
    let header = entries.iter().find(|entry| is_header(entry));
    match header.and_then(|header| header.get("version")) {
        // Missing header, missing `version` or an explicit `null` all give `?? 1`.
        None | Some(Value::Null) => Some(1.0),
        Some(Value::Number(number)) => number.as_f64(),
        // JS coerces a string in a relational comparison; anything else is NaN here.
        Some(Value::String(text)) => text.trim().parse::<f64>().ok(),
        Some(_) => None,
    }
}

/// `migrateV1ToV2(entries)` (`session-manager.ts:227-253`).
///
/// Header: `version = 2` — a *new* key on a v1 header, so JS appends it last, which
/// `serde_json`'s `preserve_order` insert does too. Every other entry gets
/// `id = generateId(ids)` and `parentId = prevId`, forming a **linear** chain from `null`;
/// both are new keys, hence appended in that order.
///
/// `ids` is an empty `Set` that is **never added to** (`:228`), so the collision check is
/// vacuous — ported as such.
///
/// For a `compaction` entry with a numeric `firstKeptEntryIndex`: look up `entries[index]`
/// and, unless it is missing or the header, assign `firstKeptEntryId = targetEntry.id`; then
/// `delete firstKeptEntryIndex`. **Order matters** — the target's id only exists if the
/// target came *earlier*, so a forward reference assigns `undefined`, which
/// `JSON.stringify` omits; that is the `remove` below.
fn migrate_v1_to_v2(entries: &mut [FileEntry], ids: &dyn SessionIdSource) {
    let mut prev_id: Option<String> = None;

    for index in 0..entries.len() {
        if is_header(&entries[index]) {
            if let Some(header) = entries[index].as_object_mut() {
                header.insert("version".to_string(), Value::from(2));
            }
            continue;
        }

        let id = generate_id(ids, &|_| false);
        if let Some(entry) = entries[index].as_object_mut() {
            entry.insert("id".to_string(), Value::String(id.clone()));
            entry.insert(
                "parentId".to_string(),
                prev_id.clone().map_or(Value::Null, Value::String),
            );
        }
        prev_id = Some(id);

        if entry_type(&entries[index]) != Some("compaction") {
            continue;
        }
        // `typeof comp.firstKeptEntryIndex === "number"`, then `entries[thatIndex]`: a
        // fractional or out-of-range index is `undefined` in JS.
        let Some(target_index) = entries[index]
            .get("firstKeptEntryIndex")
            .and_then(Value::as_f64)
            .filter(|value| value.fract() == 0.0 && *value >= 0.0)
            .map(|value| value as usize)
        else {
            continue;
        };
        let target_id = entries
            .get(target_index)
            .filter(|target| !is_header(target))
            .map(|target| entry_id(target).map(str::to_string));
        if let Some(entry) = entries[index].as_object_mut() {
            match target_id {
                // `targetEntry && targetEntry.type !== "session"` was false: untouched.
                None => {}
                Some(Some(id)) => {
                    entry.insert("firstKeptEntryId".to_string(), Value::String(id));
                }
                // Forward reference: assigned `undefined`, which stringifies to nothing.
                Some(None) => {
                    entry.remove("firstKeptEntryId");
                }
            }
            entry.remove("firstKeptEntryIndex");
        }
    }
}

/// `migrateV2ToV3(entries)` (`session-manager.ts:256-271`): header `version = 3` (an
/// existing key on a v2 header, so it keeps its position), and every message whose
/// `role === "hookMessage"` becomes `"custom"`.
fn migrate_v2_to_v3(entries: &mut [FileEntry]) {
    for entry in entries.iter_mut() {
        if is_header(entry) {
            if let Some(header) = entry.as_object_mut() {
                header.insert("version".to_string(), Value::from(CURRENT_SESSION_VERSION));
            }
            continue;
        }
        if entry_type(entry) != Some("message") {
            continue;
        }
        let is_hook = entry
            .get("message")
            .and_then(|message| message.get("role"))
            .and_then(Value::as_str)
            == Some("hookMessage");
        if is_hook {
            if let Some(message) = entry.get_mut("message").and_then(Value::as_object_mut) {
                message.insert("role".to_string(), Value::String("custom".to_string()));
            }
        }
    }
}

// =============================================================================
// Tree traversal over entry lists (`session-manager.ts:312-450`)
// =============================================================================

/// TS `leafId?: string | null` — three distinct states (`session-manager.ts:332-343`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeafId<'a> {
    /// `undefined` — start from the **last** entry in file order.
    Unset,
    /// `null` — an empty path.
    Null,
    /// A specific id. An unknown one silently falls back to the last entry (`:343`), and
    /// the empty string is falsy in JS, so it behaves like [`LeafId::Unset`].
    Id(&'a str),
}

impl<'a> LeafId<'a> {
    /// The leaf pointer a [`SessionManager`] holds: `string | null`.
    fn from_option(leaf_id: Option<&'a str>) -> Self {
        match leaf_id {
            Some(id) => Self::Id(id),
            None => Self::Null,
        }
    }
}

/// `getLatestCompactionEntry(entries)` (`session-manager.ts:312-319`) — the **last**
/// `compaction` entry in list order, not along the path.
pub fn get_latest_compaction_entry<'a>(entries: &[&'a FileEntry]) -> Option<&'a FileEntry> {
    entries
        .iter()
        .rev()
        .find(|entry| entry_type(entry) == Some("compaction"))
        .copied()
}

/// `buildSessionPath(entries, leafId?, byId?)` (`session-manager.ts:330-356`) — the walk
/// from the leaf to the root, reversed to root-first.
///
/// Pi's optional `byId` is only a cache (`buildEntryIndex`, `:321-328`) and is rebuilt here;
/// for a `SessionManager` the two are always consistent, so this is behaviourally identical.
///
/// A cycle in `parentId` would loop forever in Pi; that is reproduced (an append-only tree
/// cannot create one, but a hand-edited file could).
pub fn build_session_path<'a>(entries: &[&'a FileEntry], leaf: LeafId<'_>) -> Vec<&'a FileEntry> {
    if leaf == LeafId::Null {
        return Vec::new();
    }
    let index: HashMap<&str, &'a FileEntry> = entries
        .iter()
        .filter_map(|entry| entry_id(entry).map(|id| (id, *entry)))
        .collect();

    let mut current = match leaf {
        LeafId::Id(id) if !id.is_empty() => index.get(id).copied(),
        _ => None,
    };
    // `leaf ??= entries[entries.length - 1]`
    current = current.or_else(|| entries.last().copied());
    let Some(mut current) = current else {
        return Vec::new();
    };

    let mut path: Vec<&'a FileEntry> = Vec::new();
    loop {
        path.push(current);
        let Some(parent) = entry_parent_id(current).and_then(|id| index.get(id).copied()) else {
            break;
        };
        current = parent;
    }
    path.reverse();
    path
}

/// `buildContextEntries(entries, leafId?, byId?)` (`session-manager.ts:414-450`).
///
/// The path, unless it contains a `compaction`: then the result is
/// `[latestCompactionOnPath, …entries from firstKeptEntryId up to the compaction,
/// …everything after the compaction]`. Note the compaction entry itself comes **first**,
/// before the kept prefix, and that `firstKeptEntryId` not being found on the path simply
/// keeps nothing from before it.
pub fn build_context_entries<'a>(
    entries: &[&'a FileEntry],
    leaf: LeafId<'_>,
) -> Vec<&'a FileEntry> {
    let path = build_session_path(entries, leaf);
    let compaction = path
        .iter()
        .rev()
        .find(|entry| entry_type(entry) == Some("compaction"))
        .copied();
    let Some(compaction) = compaction else {
        return path;
    };
    let compaction_id = entry_id(compaction);
    let Some(compaction_index) = path
        .iter()
        .position(|entry| entry_id(entry) == compaction_id)
    else {
        return path;
    };

    let first_kept = compaction.get("firstKeptEntryId").and_then(Value::as_str);
    let mut context: Vec<&'a FileEntry> = vec![compaction];
    let mut found_first_kept = false;
    for entry in path.iter().take(compaction_index) {
        if entry_id(entry) == first_kept {
            found_first_kept = true;
        }
        if found_first_kept {
            context.push(entry);
        }
    }
    context.extend(path.iter().skip(compaction_index + 1));
    context
}

/// One entry in [`SessionManager::list_branches`]'s pre-order walk of the whole entry tree.
///
/// Not a port — Pi has no `listBranches`/`getTree` on `session-manager.ts` (grepped; the
/// nearest hit, `getTree` in `scripts/gen-rpc-oracle.mjs:307`, is an unrelated RPC-oracle
/// stub). This exists so a later `/tree` picker can render every branch of a session without
/// re-deriving branch points itself. All fields borrow from the [`SessionManager`] the walk
/// was built from — clone what you need to keep past its lifetime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BranchInfo<'a> {
    /// `entry.id`.
    pub id: &'a str,
    /// `entry.parentId` — `None` for a root (or for an orphan; see the doc comment on
    /// [`SessionManager::list_branches`]).
    pub parent_id: Option<&'a str>,
    /// [`SessionManager::get_label`] for this id.
    pub label: Option<&'a str>,
    /// `entry.timestamp`.
    pub timestamp: Option<&'a str>,
    /// `true` when this entry has more than one child — i.e. history was branched *from*
    /// here, not just linearly continued.
    pub is_branch_point: bool,
    /// How many entries name this one as their `parentId`. `0` for a leaf, `1` for an
    /// ordinary link in a chain, `>1` iff [`Self::is_branch_point`].
    pub child_count: usize,
    /// Matches [`SessionManager::get_leaf_id`] — "you are here".
    pub is_current_leaf: bool,
    /// Distance from its root (a root has depth `0`), so a tree view can indent without
    /// recomputing it.
    pub depth: usize,
}

/// The walk behind [`SessionManager::list_branches`], factored out so it can be exercised
/// against a bare `entries` slice the way [`build_session_path`] and
/// [`build_context_entries`] are tested — no full `SessionManager` fixture required.
///
/// # Ordering
/// A pre-order walk from the root(s): each root is visited, then its children (each
/// recursively pre-ordered) before moving to the next root, so a caller can render the
/// result top-down and indent by [`BranchInfo::depth`] without sorting anything itself.
/// Roots are visited in file order; a parent's children are visited in the order they were
/// appended (`children` is built with one pass over `entries` in file order, so ties are
/// never reordered).
///
/// # Orphans and multiple roots
/// A "root" here is any entry whose `parentId` is absent/`null`, *or* whose `parentId` names
/// an id this session doesn't have — a partially written or hand-edited file is a real thing,
/// and dropping the entry would be worse than showing it disconnected from the tree it should
/// have belonged to. Both cases surface with `parent_id` as read from the entry (so an
/// orphan's stale `parentId` is still visible to the caller) and `depth: 0`.
///
/// # Cycles
/// A cycle in `parentId` (only reachable via a hand-edited file — an append-only tree cannot
/// create one) cannot make this loop forever or recurse unboundedly: the walk is iterative
/// (an explicit stack, not recursion, so a long linear session also can't blow the call
/// stack) and a `visited` set guarantees every id is popped and emitted **at most once** — a
/// second attempt to push an already-visited id is simply skipped. If every member of a
/// cycle is unreachable from any root (e.g. `A`'s parent is `B` and `B`'s parent is `A`, and
/// nothing outside the pair points into it), the main walk cannot reach it either; a second
/// pass over `entries` in file order seeds the walk with anything still unvisited, so that
/// cycle still gets surfaced (as its own one-entry-deep "root") instead of vanishing.
///
/// Entries with no `id` of their own are skipped, same as [`SessionManager::build_index`]
/// skips them from `by_id` (`:1852-1855`): they cannot be referenced by any `parentId`, so
/// they can be neither a parent nor a child here.
fn build_branch_list<'a>(
    entries: &[&'a FileEntry],
    leaf_id: Option<&'a str>,
    labels_by_id: &'a HashMap<String, String>,
) -> Vec<BranchInfo<'a>> {
    let by_id: HashMap<&'a str, &'a FileEntry> = entries
        .iter()
        .filter_map(|entry| entry_id(entry).map(|id| (id, *entry)))
        .collect();

    // One pass: group children by parentId (file order preserved within each group) and
    // collect roots/orphans, instead of an O(n^2) "for each entry, scan all entries".
    let mut children: HashMap<&'a str, Vec<&'a str>> = HashMap::with_capacity(by_id.len());
    let mut roots: Vec<&'a str> = Vec::with_capacity(by_id.len());
    for entry in entries {
        let Some(id) = entry_id(entry) else {
            continue;
        };
        match entry_parent_id(entry) {
            Some(parent_id) if by_id.contains_key(parent_id) => {
                children.entry(parent_id).or_default().push(id);
            }
            // No parentId, or one naming an id not in this session: both are roots.
            _ => roots.push(id),
        }
    }

    let mut out = Vec::with_capacity(by_id.len());
    let mut visited: std::collections::HashSet<&'a str> =
        std::collections::HashSet::with_capacity(by_id.len());
    // Push in reverse so the LIFO pop order matches file order.
    let mut stack: Vec<(&'a str, usize)> = roots.iter().rev().map(|&id| (id, 0)).collect();
    // Runs twice at most: the real roots, then (only if that left ids unvisited — the
    // rootless-cycle case) everything `entries` still knows about that wasn't reached.
    let mut seeded_leftovers = false;
    loop {
        while let Some((id, depth)) = stack.pop() {
            if !visited.insert(id) {
                continue;
            }
            let Some(&entry) = by_id.get(id) else {
                continue;
            };
            let kids: &[&str] = children.get(id).map(Vec::as_slice).unwrap_or(&[]);
            out.push(BranchInfo {
                id,
                parent_id: entry_parent_id(entry),
                label: labels_by_id.get(id).map(String::as_str),
                timestamp: entry_timestamp(entry),
                is_branch_point: kids.len() > 1,
                child_count: kids.len(),
                is_current_leaf: leaf_id == Some(id),
                depth,
            });
            for &child in kids.iter().rev() {
                stack.push((child, depth + 1));
            }
        }
        if seeded_leftovers {
            break;
        }
        seeded_leftovers = true;
        for entry in entries.iter().rev() {
            if let Some(id) = entry_id(entry) {
                if !visited.contains(id) {
                    stack.push((id, 0));
                }
            }
        }
    }
    out
}

/// Civil `(year, month, day)` → days since 1970-01-01 (Howard Hinnant's algorithm).
///
/// Ported, not imported: [`pirust_agent_core::harness::session`] defines the identical
/// function (`harness/session/mod.rs:91-98`) as a module-private `fn`, and its sibling
/// `iso8601_to_epoch_ms` below it is private too — nothing in that module is reachable from
/// here. Duplicated rather than made `pub(crate)` there, since that file isn't mine to edit
/// for this change.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// Parse the canonical `Date#toISOString` form (`YYYY-MM-DDTHH:MM:SS.sssZ`) into Unix
/// milliseconds — mirrors `new Date(iso).getTime()` for that format. Returns `0` for anything
/// unparseable (session entries' `timestamp` is always written via
/// [`SessionEnv::in_memory`]-style `now_iso()`/`entry_head`, so this is a defensive
/// fallback, not a real code path in practice).
///
/// Ported from `pirust_agent_core::harness::session::iso8601_to_epoch_ms`
/// (`harness/session/mod.rs:131-163`), verbatim except for the name — see the note on
/// [`days_from_civil`] for why it's copied rather than shared.
fn iso8601_to_epoch_ms(s: &str) -> i64 {
    fn parse(s: &str) -> Option<i64> {
        let s = s.strip_suffix('Z').unwrap_or(s);
        let (date, time) = s.split_once('T')?;
        let mut dp = date.split('-');
        let y: i64 = dp.next()?.parse().ok()?;
        let mo: i64 = dp.next()?.parse().ok()?;
        let d: i64 = dp.next()?.parse().ok()?;
        let (hms, frac) = match time.split_once('.') {
            Some((a, b)) => (a, Some(b)),
            None => (time, None),
        };
        let mut tp = hms.split(':');
        let hh: i64 = tp.next()?.parse().ok()?;
        let mm: i64 = tp.next()?.parse().ok()?;
        let ss: i64 = tp.next().unwrap_or("0").parse().ok()?;
        // Milliseconds: pad/truncate the fractional part to exactly 3 digits.
        let millis: i64 = match frac {
            Some(f) => {
                let mut f = f.to_string();
                f.truncate(3);
                while f.len() < 3 {
                    f.push('0');
                }
                f.parse().ok()?
            }
            None => 0,
        };
        let days = days_from_civil(y, mo, d);
        Some(days * 86_400_000 + hh * 3_600_000 + mm * 60_000 + ss * 1000 + millis)
    }
    parse(s).unwrap_or(0)
}

/// Session entries → in-context [`AgentMessage`]s: the message-projection half of
/// `sessionEntryToContextMessages` (`session-manager.ts:379-404`) — see the corrected module
/// doc note above. Lives next to [`SessionManager::get_branch`] in spirit (its natural input
/// is `get_branch`'s root-first `Vec<&FileEntry>`) but is a free function, not a method,
/// mirroring [`build_branch_list`]: pure data-in/data-out, so it is directly unit-testable
/// against hand-built `Vec<Value>` fixtures with no [`SessionManager`]/[`SessionEnv`] and no
/// I/O.
///
/// # Which of the 9 written `"type"` tags convert, and which are deliberately skipped
/// Grepped from every `entry_head("...")`/`append_custom_entry`/`append_custom_message_entry`
/// call and [`is_header`] in this file: `"session"` (header), `"message"`,
/// `"thinking_level_change"`, `"model_change"`, `"compaction"`, `"custom"`, `"session_info"`,
/// `"custom_message"`, `"label"`, `"branch_summary"`. Of those:
///
/// - `"message"` → the value stored under the entry's `"message"` key (exactly what
///   [`SessionManager::append_message`] writes it under), deserialized straight as an
///   [`AgentMessage`].
/// - `"custom_message"` → [`create_custom_message`], from the entry's `customType` / `content`
///   / `display` / `details` — the shape [`SessionManager::append_custom_message_entry`]
///   writes.
/// - `"compaction"` → [`create_compaction_summary_message`], from `summary` / `tokensBefore`.
/// - `"branch_summary"` → [`create_branch_summary_message`], from `fromId` / `summary`, **only
///   when `summary` is non-empty** — matching the guard agent-core's own canonical projection
///   applies (`pirust_agent_core::harness::session::session_entry_to_context_messages`,
///   `harness/session/mod.rs:350-396`): an empty-summary branch point contributes nothing to
///   context there either.
/// - everything else — the header, `"thinking_level_change"`, `"model_change"`, `"custom"`,
///   `"session_info"`, `"label"`, and any unrecognized future tag — contributes **zero**
///   messages.
///
/// **Correction to an earlier assumption:** bare `"custom"` entries (from
/// [`SessionManager::append_custom_entry`]) are *not* routed to [`create_custom_message`].
/// That entry shape only ever carries `customType`/`data`, never the `content`/`display` a
/// `CustomMessage` requires, and agent-core's canonical `session_entry_to_context_messages`
/// (cited above) has no match arm for a bare `"custom"` tag at all — only `"custom_message"`
/// feeds [`create_custom_message`] there. Routing `"custom"` through it would be inventing a
/// mapping Pi doesn't have, so it is skipped, matching agent-core's behavior.
///
/// # Malformed entries
/// A convertible-type entry that is missing a required field, wrongly typed, or (for
/// `"message"`) fails [`AgentMessage`] deserialization is skipped, not panicked on — a
/// truncated or hand-edited session file is a real thing. Unlike
/// [`parse_session_entry_line`]'s fully-silent `.ok()` convention, each skip here prints one
/// `eprintln!("Warning: ...")` line (id included when the entry has one), matching this
/// crate's existing `"Warning: ..."` phrasing (see `runtime_host.rs`'s
/// `"Warning: failed to ..."` lines and this file's own `SessionStream::Stderr` arm above) —
/// silently dropping a message out of context is worse than a loud one-line warning, but the
/// signature stays `-> Vec<AgentMessage>` (not a `Result`/count) since one bad entry among many
/// good ones shouldn't fail the whole conversion.
///
/// # Performance
/// `Vec::with_capacity(entries.len())`: every entry contributes at most one message, so this
/// is an exact upper bound, never a reallocation. `serde_json::from_value` *consumes* its
/// argument, so converting from a borrowed `&FileEntry` unavoidably clones the **payload**
/// value (`message`, `content`, `details` — never the whole entry, and never for entry types
/// that don't convert); `entry_type`/`entry_id`/`entry_timestamp` and the plain field reads
/// below (`summary`, `fromId`, `tokensBefore`, `customType`, `display`) are all read by
/// reference.
pub(crate) fn entries_to_agent_messages(entries: &[&FileEntry]) -> Vec<AgentMessage> {
    let mut out = Vec::with_capacity(entries.len());
    for entry in entries {
        match entry_type(entry) {
            Some("message") => match entry.get("message") {
                Some(message) => match serde_json::from_value::<AgentMessage>(message.clone()) {
                    Ok(message) => out.push(message),
                    Err(error) => eprintln!(
                        "Warning: skipping malformed \"message\" session entry (id {:?}): {error}",
                        entry_id(entry)
                    ),
                },
                None => eprintln!(
                    "Warning: skipping \"message\" session entry with no \"message\" field (id {:?})",
                    entry_id(entry)
                ),
            },
            Some("custom_message") => {
                let custom_type = entry.get("customType").and_then(Value::as_str);
                let content = entry.get("content").cloned();
                let display = entry.get("display").and_then(Value::as_bool);
                match (custom_type, content, display) {
                    (Some(custom_type), Some(content), Some(display)) => {
                        match serde_json::from_value::<UserMessageContent>(content) {
                            Ok(content) => {
                                let details = entry.get("details").cloned();
                                let timestamp =
                                    iso8601_to_epoch_ms(entry_timestamp(entry).unwrap_or(""));
                                out.push(AgentMessage::Custom(create_custom_message(
                                    custom_type.to_string(),
                                    content,
                                    display,
                                    details,
                                    timestamp,
                                )));
                            }
                            Err(error) => eprintln!(
                                "Warning: skipping malformed \"custom_message\" session entry (id {:?}): {error}",
                                entry_id(entry)
                            ),
                        }
                    }
                    _ => eprintln!(
                        "Warning: skipping \"custom_message\" session entry missing customType/content/display (id {:?})",
                        entry_id(entry)
                    ),
                }
            }
            Some("compaction") => {
                let summary = entry.get("summary").and_then(Value::as_str);
                let tokens_before = entry.get("tokensBefore").and_then(Value::as_i64);
                match (summary, tokens_before) {
                    (Some(summary), Some(tokens_before)) => {
                        let timestamp = iso8601_to_epoch_ms(entry_timestamp(entry).unwrap_or(""));
                        out.push(AgentMessage::CompactionSummary(
                            create_compaction_summary_message(
                                summary.to_string(),
                                tokens_before,
                                timestamp,
                            ),
                        ));
                    }
                    _ => eprintln!(
                        "Warning: skipping \"compaction\" session entry missing summary/tokensBefore (id {:?})",
                        entry_id(entry)
                    ),
                }
            }
            Some("branch_summary") => {
                let summary = entry.get("summary").and_then(Value::as_str);
                let from_id = entry.get("fromId").and_then(Value::as_str);
                match (summary, from_id) {
                    (Some(summary), Some(from_id)) if !summary.is_empty() => {
                        let timestamp = iso8601_to_epoch_ms(entry_timestamp(entry).unwrap_or(""));
                        out.push(AgentMessage::BranchSummary(create_branch_summary_message(
                            summary.to_string(),
                            from_id.to_string(),
                            timestamp,
                        )));
                    }
                    // Empty summary: agent-core's own projection drops these too (see doc
                    // comment above) — not malformed, so no warning.
                    (Some(_), Some(_)) => {}
                    _ => eprintln!(
                        "Warning: skipping \"branch_summary\" session entry missing summary/fromId (id {:?})",
                        entry_id(entry)
                    ),
                }
            }
            // Header ("session"), "thinking_level_change", "model_change", "custom",
            // "session_info", "label", and any unrecognized future type: not part of the
            // in-context message projection (see doc comment above).
            _ => {}
        }
    }
    out
}

// =============================================================================
// SessionInfo (`session-manager.ts:170-184`, `:623-778`)
// =============================================================================

/// `SessionInfo` (`session-manager.ts:170-184`).
///
/// Pi's `created`/`modified` are `Date`s; here they are Unix milliseconds, which is what
/// every consumer uses them for (`getTime()` comparisons). `created` is `None` when
/// `new Date(header.timestamp)` would be an Invalid Date.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionInfo {
    /// The session file's path, exactly as it was listed.
    pub path: String,
    /// `header.id`.
    pub id: String,
    /// `header.cwd`, or `""` for old sessions (`:173`).
    pub cwd: String,
    /// The latest `session_info` name, trimmed; `None` when cleared or absent.
    pub name: Option<String>,
    /// `header.parentSession`.
    pub parent_session_path: Option<String>,
    /// `new Date(header.timestamp)`; `None` for an unparseable timestamp.
    pub created: Option<i64>,
    /// Last activity: the newest user/assistant message time, else the header time, else
    /// the file's mtime (`:679-684`). The sort key for every listing.
    pub modified: i64,
    /// How many `message` entries the file has.
    pub message_count: usize,
    /// The first user message's text, or `"(no messages)"` (`:695`).
    pub first_message: String,
    /// Every user/assistant message's text, joined with `" "` (`:696`).
    pub all_messages_text: String,
}

/// `buildSessionInfo(filePath)` (`session-manager.ts:623-701`) — `None` on any throw.
///
/// The first parsed line must be the header, else `None` (`:643`). `messageCount` counts
/// **all** `message` entries; the text fields only consider `user`/`assistant` messages with
/// content.
fn build_session_info(file_path: &str) -> Option<SessionInfo> {
    let stats_mtime = mtime_ms(file_path)?;
    let bytes = fs::read(file_path).ok()?;
    let content = String::from_utf8_lossy(&bytes);

    let mut header: Option<FileEntry> = None;
    let mut message_count = 0usize;
    let mut first_message = String::new();
    let mut all_messages: Vec<String> = Vec::new();
    let mut name: Option<String> = None;
    let mut last_activity_time: Option<i64> = None;

    for line in content.split('\n') {
        // `createInterface({ crlfDelay: Infinity })` strips the `\r` of a CRLF pair.
        let line = line.strip_suffix('\r').unwrap_or(line);
        let Some(entry) = parse_session_entry_line(line) else {
            continue;
        };

        if header.is_none() {
            if !is_header(&entry) {
                return None;
            }
            header = Some(entry);
            continue;
        }

        // `entry.name?.trim() || undefined` — the latest wins, including explicit clears.
        if entry_type(&entry) == Some("session_info") {
            name = entry
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string);
        }

        if entry_type(&entry) != Some("message") {
            continue;
        }
        message_count += 1;

        if let Some(activity) = message_activity_time(&entry) {
            last_activity_time = Some(last_activity_time.unwrap_or(0).max(activity));
        }

        let Some(message) = entry.get("message") else {
            continue;
        };
        // `isMessageWithContent`: a string `role` and a `content` property.
        let Some(role) = message.get("role").and_then(Value::as_str) else {
            continue;
        };
        if message.get("content").is_none() {
            continue;
        }
        if role != "user" && role != "assistant" {
            continue;
        }
        let text = extract_text_content(message);
        if text.is_empty() {
            continue;
        }
        all_messages.push(text.clone());
        if first_message.is_empty() && role == "user" {
            first_message = text;
        }
    }

    let header = header?;
    let cwd = session_header_cwd(&header).unwrap_or("").to_string();
    let parent_session_path = header
        .get("parentSession")
        .and_then(Value::as_str)
        .map(str::to_string);
    let header_time = header
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(js_date_ms);
    let modified = match last_activity_time {
        Some(activity) if activity > 0 => activity,
        _ => header_time.unwrap_or(stats_mtime),
    };

    Some(SessionInfo {
        path: file_path.to_string(),
        id: entry_id(&header).unwrap_or("").to_string(),
        cwd,
        name,
        parent_session_path,
        created: header_time,
        modified,
        message_count,
        first_message: if first_message.is_empty() {
            "(no messages)".to_string()
        } else {
            first_message
        },
        all_messages_text: all_messages.join(" "),
    })
}

/// `getMessageActivityTime(entry)` (`session-manager.ts:609-621`): the message's own numeric
/// `timestamp`, else the entry's ISO `timestamp`, else nothing. Only `user`/`assistant`
/// messages count.
fn message_activity_time(entry: &FileEntry) -> Option<i64> {
    let message = entry.get("message")?;
    let role = message.get("role").and_then(Value::as_str)?;
    message.get("content")?;
    if role != "user" && role != "assistant" {
        return None;
    }
    if let Some(timestamp) = message.get("timestamp").and_then(Value::as_f64) {
        return Some(timestamp as i64);
    }
    entry
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(js_date_ms)
}

/// `extractTextContent(message)` (`session-manager.ts:598-607`): a string content verbatim,
/// else every `text` block joined with `" "`.
fn extract_text_content(message: &Value) -> String {
    match message.get("content") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(" "),
        // `content.filter` on a non-array throws in Pi; unreachable for real sessions.
        _ => String::new(),
    }
}

/// `listSessionsFromDir(dir, onProgress?)` (`session-manager.ts:747-778`).
///
/// `[]` when the directory is missing or unreadable. Pi's `progressOffset`/`progressTotal`
/// parameters are dead at both call sites (they always take their defaults), so they are not
/// ported.
fn list_sessions_from_dir(
    dir: &str,
    mut on_progress: Option<&mut dyn FnMut(usize, usize)>,
) -> Vec<SessionInfo> {
    if !exists(dir) {
        return Vec::new();
    }
    let Ok(read_dir) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let files: Vec<std::path::PathBuf> = read_dir
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .ends_with(SESSION_FILE_SUFFIX)
        })
        .map(|entry| entry.path())
        .collect();

    let total = files.len();
    let mut sessions = Vec::new();
    for (index, file) in files.iter().enumerate() {
        let info = build_session_info(&file.to_string_lossy());
        if let Some(on_progress) = on_progress.as_deref_mut() {
            on_progress(index + 1, total);
        }
        if let Some(info) = info {
            sessions.push(info);
        }
    }
    sessions
}

/// `statSync(path).mtime.getTime()`, or `None` when the stat fails.
fn mtime_ms(path: &str) -> Option<i64> {
    let modified = fs::metadata(path).ok()?.modified().ok()?;
    match modified.duration_since(std::time::UNIX_EPOCH) {
        Ok(since) => Some(since.as_millis() as i64),
        Err(before) => Some(-(before.duration().as_millis() as i64)),
    }
}

/// `new Date(iso).getTime()`, `None` for an Invalid Date.
///
/// **Narrowed** to the canonical `Date#toISOString` form (`YYYY-MM-DDTHH:MM:SS.sssZ`), which
/// is the only shape Pi ever writes; JS's parser also accepts many others (`2025-01-01`,
/// RFC 2822, …), so a hand-edited timestamp can be `None` here where JS yields a number.
/// The value is only ever used for ordering and display.
fn js_date_ms(iso: &str) -> Option<i64> {
    let rest = iso.strip_suffix('Z')?;
    let (date, time) = rest.split_once('T')?;
    let mut date_parts = date.split('-');
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: i64 = date_parts.next()?.parse().ok()?;
    let day: i64 = date_parts.next()?.parse().ok()?;
    if date_parts.next().is_some() {
        return None;
    }
    let (hms, millis) = match time.split_once('.') {
        Some((hms, frac)) => (hms, frac.parse::<i64>().ok()?),
        None => (time, 0),
    };
    let mut time_parts = hms.split(':');
    let hours: i64 = time_parts.next()?.parse().ok()?;
    let minutes: i64 = time_parts.next()?.parse().ok()?;
    let seconds: i64 = time_parts.next()?.parse().ok()?;
    if time_parts.next().is_some() {
        return None;
    }

    // Howard Hinnant's days_from_civil.
    let year_adjusted = if month <= 2 { year - 1 } else { year };
    let era = (if year_adjusted >= 0 {
        year_adjusted
    } else {
        year_adjusted - 399
    }) / 400;
    let year_of_era = year_adjusted - era * 400;
    let day_of_year = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;

    Some(days * 86_400_000 + hours * 3_600_000 + minutes * 60_000 + seconds * 1000 + millis)
}

// =============================================================================
// Low-level writes — the three `fs` flags Pi uses
// =============================================================================

/// `openSync(path, "wx")` + `writeFileSync(fd, …)` — **exclusive create**, so an existing
/// file is an error (`session-manager.ts:961`, `:1531`).
fn write_exclusive(path: &str, contents: &str) -> Result<(), SessionError> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| SessionError::io("open", path, e))?;
    file.write_all(contents.as_bytes())
        .map_err(|e| SessionError::io("write", path, e))
}

/// `openSync(path, "w")` + `writeFileSync(fd, …)` — truncate or create
/// (`session-manager.ts:912`).
fn write_truncate(path: &str, contents: &str) -> Result<(), SessionError> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .map_err(|e| SessionError::io("open", path, e))?;
    file.write_all(contents.as_bytes())
        .map_err(|e| SessionError::io("write", path, e))
}

/// `_rewriteFile()`'s two early-return guards plus the actual write
/// (`session-manager.ts:910-920`): a no-op when not persisting or with no file, otherwise
/// the whole buffer serialized with flag `"w"`.
///
/// A free function (not a `SessionManager` method) so
/// [`SessionManager::set_session_file`] can perform this write against a **local** entry
/// buffer and a not-yet-committed path — the write is one of the fallible steps that must
/// run before `self` is touched, not after. [`SessionManager::rewrite_file`] is a thin
/// wrapper over this for every other caller.
fn write_entries_to_file(
    persist: bool,
    file: Option<&str>,
    entries: &[FileEntry],
) -> Result<(), SessionError> {
    if !persist {
        return Ok(());
    }
    let Some(file) = file.filter(|f| !f.is_empty()) else {
        return Ok(());
    };
    let contents: String = entries.iter().map(to_line).collect();
    write_truncate(file, &contents)
}

/// `appendFileSync(path, …)` — creates the file when missing
/// (`session-manager.ts:952`, `:971`, `:1536`).
fn append_to(path: &str, contents: &str) -> Result<(), SessionError> {
    let mut file = fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)
        .map_err(|e| SessionError::io("append", path, e))?;
    file.write_all(contents.as_bytes())
        .map_err(|e| SessionError::io("append", path, e))
}

// =============================================================================
// SessionManager (`session-manager.ts:791-1434`)
// =============================================================================

/// `NewSessionOptions` (`session-manager.ts:41-44`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NewSessionOptions {
    /// An explicit session id (`--session-id`). Validated by
    /// [`assert_valid_session_id`] when present — including when it is the empty string,
    /// which is *defined* and therefore rejected.
    pub id: Option<String>,
    /// The `parentSession` header field.
    pub parent_session: Option<String>,
}

impl NewSessionOptions {
    /// `{ id }`, the only shape `main.ts` builds (`:271`, `:256`).
    pub fn with_id(id: impl Into<String>) -> Self {
        Self {
            id: Some(id.into()),
            parent_session: None,
        }
    }
}

/// Manages conversation sessions as append-only trees stored in JSONL files
/// (`session-manager.ts:791-1434`).
///
/// Built through the factories on [`SessionEnv`], never directly — Pi's constructor is
/// `private` too (`:804`).
///
/// # The persistence rule, which is the whole point
///
/// [`SessionManager::persist_entry`] (`_persist`, `:946-973`) writes **nothing to disk until
/// the tree contains an assistant message**. At that moment the entire buffer — header
/// included — is written with `"wx"` (exclusive create), and every later entry is appended
/// individually. A session that never receives an assistant reply leaves **no file at all**,
/// which is why a `-p` run that fails before the first response produces no session file
/// (spec §11.2) and is directly observable in the feat-005 live differential.
#[derive(Debug)]
pub struct SessionManager {
    env: SessionEnv,
    session_id: String,
    session_file: Option<String>,
    session_dir: String,
    cwd: String,
    persist: bool,
    flushed: bool,
    file_entries: Vec<FileEntry>,
    /// id → index into `file_entries`. Pi's `byId` holds the entry objects themselves; the
    /// two never diverge because the only in-place mutation (the v1→v3 migration) happens
    /// before `_buildIndex`.
    by_id: HashMap<String, usize>,
    labels_by_id: HashMap<String, String>,
    label_timestamps_by_id: HashMap<String, String>,
    leaf_id: Option<String>,
}

impl SessionManager {
    /// The private constructor (`session-manager.ts:804-823`).
    ///
    /// `cwd` is `resolvePath`'d, `sessionDir` is `normalizePath`'d, and the directory is
    /// created up front **only when persisting and non-empty** (`:814-816`). Then either the
    /// given file is loaded or a fresh session is started.
    fn construct(
        env: &SessionEnv,
        cwd: &str,
        session_dir: &str,
        session_file: Option<&str>,
        persist: bool,
        options: Option<&NewSessionOptions>,
    ) -> Result<Self, SessionError> {
        let mut manager = Self {
            env: env.clone(),
            session_id: String::new(),
            session_file: None,
            session_dir: env.normalize_path(session_dir)?,
            cwd: env.resolve_path(cwd)?,
            persist,
            flushed: false,
            file_entries: Vec::new(),
            by_id: HashMap::new(),
            labels_by_id: HashMap::new(),
            label_timestamps_by_id: HashMap::new(),
            leaf_id: None,
        };
        if persist && !manager.session_dir.is_empty() && !exists(&manager.session_dir) {
            fs::create_dir_all(&manager.session_dir)
                .map_err(|e| SessionError::io("mkdir", &manager.session_dir, e))?;
        }
        match session_file {
            Some(file) => manager.set_session_file(file)?,
            None => {
                manager.new_session(options)?;
            }
        }
        Ok(manager)
    }

    /// `setSessionFile(sessionFile)` (`session-manager.ts:826-859`) — the load path, used by
    /// resume and branching.
    ///
    /// Existing file → `loadEntriesFromFile`. If that yields `[]`:
    /// `statSync(path).size > 0` → [`SessionError::NotAPiSession`]; a 0-byte file instead
    /// gets a fresh session written to that exact path (`_rewriteFile`, so the header *is*
    /// on disk immediately — the one exception to the no-file-until-assistant rule) and
    /// `flushed = true`. Otherwise take `header?.id ?? createSessionId()`, run the v1→v3
    /// migration (rewriting the file when it changed anything), build the index, and set
    /// `flushed = true`. A **non-existent** path starts a fresh session and then restores the
    /// explicit path (`:854-858`) — so `--session <new-path>` writes nothing until the first
    /// assistant message.
    ///
    /// **Atomicity.** Pi's own `setSessionFile` mutates `this` incrementally as it goes —
    /// `this.sessionFile = resolved` runs before the existence check, and
    /// `this.fileEntries = loadEntriesFromFile(...)` runs before the `size > 0` check that
    /// can throw `NotAPiSessionError` (`:828-833`). Porting that shape verbatim would mean a
    /// failed `/import` or in-place `/resume` (`SingleTurnSession::switch_to_session_file` in
    /// `runtime_host.rs`) leaves the live `SessionManager` pointed at the rejected path with
    /// its entry buffer wiped, even though the caller only observes an `Err` and reasonably
    /// assumes nothing changed. So every fallible step here — resolving the path, reading
    /// and parsing the file, the v1→v3 migration, and the on-disk rewrite the 0-byte and
    /// migrated-entries branches can trigger — runs first against **local** bindings; `self`
    /// is written only once every one of them has already succeeded, as one straight-line
    /// block of infallible assignments at the end of each branch. Do not reintroduce an
    /// early `self.session_file = …` / `self.file_entries = …` ahead of a `?` — that is
    /// exactly the corruption this shape exists to prevent.
    pub fn set_session_file(&mut self, session_file: &str) -> Result<(), SessionError> {
        let resolved = self.env.resolve_path(session_file)?;

        if !exists(&resolved) {
            // `newSession(None)` has no fallible step once `options` is `None` (the only
            // `Err` it can produce, `assertValidSessionId`, is behind `Some(id)`), so it is
            // already atomic on its own here: either it fully commits, or it returns `Err`
            // before touching `self` at all.
            self.new_session(None)?;
            // Preserve the explicit path from the --session flag.
            self.session_file = Some(resolved);
            return Ok(());
        }

        let entries = self.env.load_entries_from_file(&resolved)?;

        if entries.is_empty() {
            let size = fs::metadata(&resolved)
                .map_err(|e| SessionError::io("stat", &resolved, e))?
                .len();
            if size > 0 {
                return Err(SessionError::NotAPiSession(resolved));
            }

            // 0-byte file: build the fresh header and write it to `resolved` before
            // touching `self`, so a failing write (disk full, permissions, a path that
            // vanished between the existence check and here, …) leaves the manager exactly
            // as it was instead of parked mid-reset.
            let (id, _timestamp, header) = self.build_session_header(None)?;
            let entries = vec![header];
            write_entries_to_file(self.persist, Some(&resolved), &entries)?;

            self.session_id = id;
            self.file_entries = entries;
            self.by_id.clear();
            self.labels_by_id.clear();
            self.label_timestamps_by_id.clear();
            self.leaf_id = None;
            self.session_file = Some(resolved);
            self.flushed = true;
            return Ok(());
        }

        // Existing, parseable file. The v1→v3 migration mutates in place, so it runs
        // against this LOCAL `entries` — never `self.file_entries` — and the resulting
        // rewrite is attempted before anything is committed: if the write fails, `self`
        // must not be left holding migrated entries that were never actually persisted.
        let mut entries = entries;
        let header_id = entries
            .iter()
            .find(|entry| is_header(entry))
            .and_then(entry_id)
            .map(str::to_string);
        let session_id = header_id.unwrap_or_else(|| self.env.ids.session_id());

        if migrate_session_entries(&mut entries, &*self.env.ids) {
            write_entries_to_file(self.persist, Some(&resolved), &entries)?;
        }

        // Every fallible step above has already succeeded — commit as one unit.
        self.session_id = session_id;
        self.file_entries = entries;
        self.session_file = Some(resolved);
        self.build_index();
        self.flushed = true;
        Ok(())
    }

    /// The pure, non-mutating half of `newSession` (`session-manager.ts:861-887`): validates
    /// an explicit id and builds the header entry, but commits nothing to `self`.
    ///
    /// Split out so [`SessionManager::set_session_file`]'s 0-byte-file branch can run the
    /// one fallible step (`assertValidSessionId`, in practice unreachable there since it is
    /// only ever called with `None`) and the resulting on-disk rewrite *before* touching
    /// `self`, while [`SessionManager::new_session`] itself stays a thin "compute, then
    /// commit" wrapper around it — its own behaviour is unchanged by this split.
    fn build_session_header(
        &self,
        options: Option<&NewSessionOptions>,
    ) -> Result<(String, String, FileEntry), SessionError> {
        let id = match options.and_then(|options| options.id.as_deref()) {
            Some(id) => {
                assert_valid_session_id(id)?;
                id.to_string()
            }
            None => self.env.ids.session_id(),
        };
        let timestamp = self.env.clock.now_iso();

        let mut header = Map::new();
        header.insert("type".to_string(), Value::String("session".to_string()));
        header.insert("version".to_string(), Value::from(CURRENT_SESSION_VERSION));
        header.insert("id".to_string(), Value::String(id.clone()));
        header.insert("timestamp".to_string(), Value::String(timestamp.clone()));
        header.insert("cwd".to_string(), Value::String(self.cwd.clone()));
        if let Some(parent) = options.and_then(|options| options.parent_session.as_deref()) {
            header.insert(
                "parentSession".to_string(),
                Value::String(parent.to_string()),
            );
        }

        Ok((id, timestamp, Value::Object(header)))
    }

    /// `newSession(options?)` (`session-manager.ts:861-887`).
    ///
    /// Validates an explicit id, writes the header **into the buffer only**, resets the
    /// index/labels/leaf, sets `flushed = false`, and — when persisting — computes the file
    /// name `` `${timestamp.replace(/[:.]/g, "-")}_${sessionId}.jsonl` `` inside
    /// [`SessionManager::get_session_dir`]. Returns the new file path, as Pi does.
    ///
    /// Header key order is `type, version, id, timestamp, cwd, parentSession`, with
    /// `parentSession` omitted when absent (spec §11.1). The header itself comes from
    /// [`SessionManager::build_session_header`]; that call is this method's only fallible
    /// step, and it happens before any of the mutation below.
    pub fn new_session(
        &mut self,
        options: Option<&NewSessionOptions>,
    ) -> Result<Option<String>, SessionError> {
        let (id, timestamp, header) = self.build_session_header(options)?;
        self.session_id = id;
        self.file_entries = vec![header];
        self.by_id.clear();
        self.labels_by_id.clear();
        self.label_timestamps_by_id.clear();
        self.leaf_id = None;
        self.flushed = false;

        if self.persist {
            let file_timestamp = timestamp.replace([':', '.'], "-");
            let name = format!("{file_timestamp}_{}{SESSION_FILE_SUFFIX}", self.session_id);
            self.session_file = Some(self.env.join(&self.session_dir, &[&name]));
        }
        Ok(self.session_file.clone())
    }

    /// `_buildIndex()` (`session-manager.ts:889-908`).
    ///
    /// Indexes every non-header entry, leaves `leafId` pointing at the **last** one in file
    /// order, and replays the `label` entries into the label caches (an empty/absent `label`
    /// clears them).
    fn build_index(&mut self) {
        self.by_id.clear();
        self.labels_by_id.clear();
        self.label_timestamps_by_id.clear();
        self.leaf_id = None;
        for index in 0..self.file_entries.len() {
            let entry = &self.file_entries[index];
            if is_header(entry) {
                continue;
            }
            let Some(id) = entry_id(entry).map(str::to_string) else {
                // JS stores this under the `undefined` key and clears the leaf.
                self.leaf_id = None;
                continue;
            };
            let is_label = entry_type(entry) == Some("label");
            let target = entry
                .get("targetId")
                .and_then(Value::as_str)
                .map(str::to_string);
            let label = entry
                .get("label")
                .and_then(Value::as_str)
                .filter(|label| !label.is_empty())
                .map(str::to_string);
            let timestamp = entry
                .get("timestamp")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();

            self.by_id.insert(id.clone(), index);
            self.leaf_id = Some(id);

            if is_label {
                if let Some(target) = target {
                    match label {
                        Some(label) => {
                            self.labels_by_id.insert(target.clone(), label);
                            self.label_timestamps_by_id.insert(target, timestamp);
                        }
                        None => {
                            self.labels_by_id.remove(&target);
                            self.label_timestamps_by_id.remove(&target);
                        }
                    }
                }
            }
        }
    }

    /// `_rewriteFile()` (`session-manager.ts:910-920`) — the whole buffer with flag `"w"`.
    /// A no-op when not persisting or with no file. Thin wrapper over
    /// [`write_entries_to_file`]; see that function's doc for why the write itself lives
    /// outside `impl SessionManager`.
    fn rewrite_file(&self) -> Result<(), SessionError> {
        write_entries_to_file(
            self.persist,
            self.session_file.as_deref(),
            &self.file_entries,
        )
    }

    /// `_persist(entry)` (`session-manager.ts:946-973`) — quoted in spec §11.2.
    ///
    /// ```text
    /// if (!persist || !sessionFile) return;
    /// hasAssistant = fileEntries.some(e => e.type === "message" && e.message.role === "assistant");
    /// if (!hasAssistant) { if (flushed) append(entry); else flushed = false; return; }
    /// if (!flushed) { open(file, "wx"); write every buffered entry; flushed = true; }
    /// else append(entry);
    /// ```
    ///
    /// The `else { this.flushed = false }` branch is a no-op (`flushed` is already false);
    /// it is kept because it is what Pi wrote. Note the `"wx"`: if a file somehow already
    /// exists at that path when the first assistant message arrives, this **throws** rather
    /// than clobbering it.
    ///
    /// Takes the entry's index rather than the entry, to avoid cloning it.
    fn persist_entry(&mut self, index: usize) -> Result<(), SessionError> {
        if !self.persist {
            return Ok(());
        }
        let Some(file) = self.session_file.clone().filter(|file| !file.is_empty()) else {
            return Ok(());
        };

        let has_assistant = self.file_entries.iter().any(is_assistant_message);
        if !has_assistant {
            if self.flushed {
                append_to(&file, &to_line(&self.file_entries[index]))?;
            } else {
                self.flushed = false;
            }
            return Ok(());
        }

        if !self.flushed {
            let contents: String = self.file_entries.iter().map(to_line).collect();
            write_exclusive(&file, &contents)?;
            self.flushed = true;
        } else {
            append_to(&file, &to_line(&self.file_entries[index]))?;
        }
        Ok(())
    }

    /// `_appendEntry(entry)` (`session-manager.ts:975-980`): buffer it, index it, advance the
    /// leaf, persist. Returns the entry id.
    fn append_entry(&mut self, entry: FileEntry) -> Result<String, SessionError> {
        let id = entry_id(&entry)
            .expect("every appended entry carries a generated id")
            .to_string();
        self.file_entries.push(entry);
        let index = self.file_entries.len() - 1;
        self.by_id.insert(id.clone(), index);
        self.leaf_id = Some(id.clone());
        self.persist_entry(index)?;
        Ok(id)
    }

    /// `generateId(this.byId)` for the next entry.
    fn next_entry_id(&self) -> String {
        generate_id(&*self.env.ids, &|id| self.by_id.contains_key(id))
    }

    /// The `type, id, parentId, timestamp` prefix shared by seven of the nine entry types.
    fn entry_head(&self, type_tag: &str) -> Map<String, Value> {
        let mut entry = Map::new();
        entry.insert("type".to_string(), Value::String(type_tag.to_string()));
        entry.insert("id".to_string(), Value::String(self.next_entry_id()));
        entry.insert(
            "parentId".to_string(),
            self.leaf_id.clone().map_or(Value::Null, Value::String),
        );
        entry.insert(
            "timestamp".to_string(),
            Value::String(self.env.clock.now_iso()),
        );
        entry
    }

    /// The `id, parentId, timestamp` **suffix** the two `custom*` literals use — see the
    /// module docs on their divergent key order.
    fn entry_tail(&self, entry: &mut Map<String, Value>) {
        entry.insert("id".to_string(), Value::String(self.next_entry_id()));
        entry.insert(
            "parentId".to_string(),
            self.leaf_id.clone().map_or(Value::Null, Value::String),
        );
        entry.insert(
            "timestamp".to_string(),
            Value::String(self.env.clock.now_iso()),
        );
    }

    // -------------------------------------------------------------------------
    // Appends (`session-manager.ts:982-1118`, `:1161-1182`)
    // -------------------------------------------------------------------------

    /// `appendMessage(message)` (`session-manager.ts:988-998`).
    ///
    /// `message` is Pi's `Message | CustomMessage | BashExecutionMessage`. Any `Serialize`
    /// value works, which is the composition point with
    /// [`pirust_agent_core::harness::messages::AgentMessage`]: agent-core's message
    /// serialization is byte-verified against Pi by feat-003, so passing one of those
    /// reproduces the message half of the line exactly, while this module owns the entry
    /// wrapper. A `serde_json::Value` also works, for replaying a captured corpus.
    pub fn append_message<M: serde::Serialize>(
        &mut self,
        message: &M,
    ) -> Result<String, SessionError> {
        let message = serde_json::to_value(message).map_err(|e| SessionError::Io {
            operation: "serialize",
            path: "<message>".to_string(),
            source: std::io::Error::other(e),
        })?;
        let mut entry = self.entry_head("message");
        entry.insert("message".to_string(), message);
        self.append_entry(Value::Object(entry))
    }

    /// `appendThinkingLevelChange(thinkingLevel)` (`session-manager.ts:1001-1011`).
    pub fn append_thinking_level_change(
        &mut self,
        thinking_level: &str,
    ) -> Result<String, SessionError> {
        let mut entry = self.entry_head("thinking_level_change");
        entry.insert(
            "thinkingLevel".to_string(),
            Value::String(thinking_level.to_string()),
        );
        self.append_entry(Value::Object(entry))
    }

    /// `appendModelChange(provider, modelId)` (`session-manager.ts:1014-1025`).
    pub fn append_model_change(
        &mut self,
        provider: &str,
        model_id: &str,
    ) -> Result<String, SessionError> {
        let mut entry = self.entry_head("model_change");
        entry.insert("provider".to_string(), Value::String(provider.to_string()));
        entry.insert("modelId".to_string(), Value::String(model_id.to_string()));
        self.append_entry(Value::Object(entry))
    }

    /// `appendCompaction(summary, firstKeptEntryId, tokensBefore, details?, fromHook?)`
    /// (`session-manager.ts:1028-1048`).
    ///
    /// `details`/`fromHook` are always *present in the literal*, so an absent value is
    /// `undefined` and `JSON.stringify` drops it while keeping the others' order.
    pub fn append_compaction(
        &mut self,
        summary: &str,
        first_kept_entry_id: &str,
        tokens_before: i64,
        details: Option<Value>,
        from_hook: Option<bool>,
    ) -> Result<String, SessionError> {
        let mut entry = self.entry_head("compaction");
        entry.insert("summary".to_string(), Value::String(summary.to_string()));
        entry.insert(
            "firstKeptEntryId".to_string(),
            Value::String(first_kept_entry_id.to_string()),
        );
        entry.insert("tokensBefore".to_string(), Value::from(tokens_before));
        if let Some(details) = details {
            entry.insert("details".to_string(), details);
        }
        if let Some(from_hook) = from_hook {
            entry.insert("fromHook".to_string(), Value::Bool(from_hook));
        }
        self.append_entry(Value::Object(entry))
    }

    /// `appendCustomEntry(customType, data?)` (`session-manager.ts:1051-1062`).
    ///
    /// Key order `type, customType, data, id, parentId, timestamp` — **not** the usual
    /// prefix; see the module docs.
    pub fn append_custom_entry(
        &mut self,
        custom_type: &str,
        data: Option<Value>,
    ) -> Result<String, SessionError> {
        let mut entry = Map::new();
        entry.insert("type".to_string(), Value::String("custom".to_string()));
        entry.insert(
            "customType".to_string(),
            Value::String(custom_type.to_string()),
        );
        if let Some(data) = data {
            entry.insert("data".to_string(), data);
        }
        self.entry_tail(&mut entry);
        self.append_entry(Value::Object(entry))
    }

    /// `appendSessionInfo(name)` (`session-manager.ts:1065-1076`): newlines collapse to
    /// spaces (`/[\r\n]+/g` → one space each run), then `trim()`.
    /// Every session in the store, newest first — what the TUI's `/resume`
    /// picker lists.
    ///
    /// [`SessionEnv::list_all`] is where the real work lives; this exists only
    /// because `env` is private, and the interactive layer reaches a
    /// `SessionManager` (through `SingleTurnSession`) but never a
    /// `SessionEnv`. Deliberately *not* `list()`: `/resume` should show
    /// sessions from other working directories too, which is exactly the
    /// difference between the two (`session-manager.ts:1564-1622`).
    pub fn list_sessions(&self) -> Result<Vec<SessionInfo>, SessionError> {
        self.env.list_all(None, None)
    }

    pub fn append_session_info(&mut self, name: &str) -> Result<String, SessionError> {
        let mut sanitized = String::with_capacity(name.len());
        let mut in_run = false;
        for ch in name.chars() {
            if ch == '\r' || ch == '\n' {
                if !in_run {
                    sanitized.push(' ');
                    in_run = true;
                }
            } else {
                sanitized.push(ch);
                in_run = false;
            }
        }
        let mut entry = self.entry_head("session_info");
        entry.insert(
            "name".to_string(),
            Value::String(sanitized.trim().to_string()),
        );
        self.append_entry(Value::Object(entry))
    }

    /// `appendCustomMessageEntry(customType, content, display, details?)`
    /// (`session-manager.ts:1100-1118`).
    ///
    /// Key order `type, customType, content, display, details, id, parentId, timestamp` —
    /// **not** the usual prefix; see the module docs. `content` is
    /// `string | (TextContent | ImageContent)[]`.
    pub fn append_custom_message_entry(
        &mut self,
        custom_type: &str,
        content: Value,
        display: bool,
        details: Option<Value>,
    ) -> Result<String, SessionError> {
        let mut entry = Map::new();
        entry.insert(
            "type".to_string(),
            Value::String("custom_message".to_string()),
        );
        entry.insert(
            "customType".to_string(),
            Value::String(custom_type.to_string()),
        );
        entry.insert("content".to_string(), content);
        entry.insert("display".to_string(), Value::Bool(display));
        if let Some(details) = details {
            entry.insert("details".to_string(), details);
        }
        self.entry_tail(&mut entry);
        self.append_entry(Value::Object(entry))
    }

    /// `appendLabelChange(targetId, label)` (`session-manager.ts:1161-1182`).
    ///
    /// Throws [`SessionError::EntryNotFound`] for an unknown target. `None`/empty clears the
    /// label; the entry itself is still appended (with `label` omitted, as `undefined`).
    pub fn append_label_change(
        &mut self,
        target_id: &str,
        label: Option<&str>,
    ) -> Result<String, SessionError> {
        if !self.by_id.contains_key(target_id) {
            return Err(SessionError::EntryNotFound(target_id.to_string()));
        }
        let mut entry = self.entry_head("label");
        entry.insert("targetId".to_string(), Value::String(target_id.to_string()));
        if let Some(label) = label {
            entry.insert("label".to_string(), Value::String(label.to_string()));
        }
        let timestamp = entry
            .get("timestamp")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let id = self.append_entry(Value::Object(entry))?;

        match label.filter(|label| !label.is_empty()) {
            Some(label) => {
                self.labels_by_id
                    .insert(target_id.to_string(), label.to_string());
                self.label_timestamps_by_id
                    .insert(target_id.to_string(), timestamp);
            }
            None => {
                self.labels_by_id.remove(target_id);
                self.label_timestamps_by_id.remove(target_id);
            }
        }
        Ok(id)
    }

    // -------------------------------------------------------------------------
    // Accessors (`session-manager.ts:922-944`, `:1124-1232`)
    // -------------------------------------------------------------------------

    /// `isPersisted()` (`:922-924`).
    pub fn is_persisted(&self) -> bool {
        self.persist
    }

    /// `getCwd()` (`:926-928`) — already `resolvePath`'d.
    pub fn get_cwd(&self) -> &str {
        &self.cwd
    }

    /// `getSessionDir()` (`:930-932`) — already `normalizePath`'d; `""` in memory mode.
    pub fn get_session_dir(&self) -> &str {
        &self.session_dir
    }

    /// `usesDefaultSessionDir()` (`:934-936`).
    pub fn uses_default_session_dir(&self) -> Result<bool, SessionError> {
        Ok(self.session_dir == self.env.default_session_dir_path(&self.cwd, None)?)
    }

    /// `getSessionId()` (`:938-940`).
    pub fn get_session_id(&self) -> &str {
        &self.session_id
    }

    /// `getSessionFile()` (`:942-944`) — `None` in memory mode.
    pub fn get_session_file(&self) -> Option<&str> {
        self.session_file.as_deref()
    }

    /// `getLeafId()` (`:1124-1126`).
    pub fn get_leaf_id(&self) -> Option<&str> {
        self.leaf_id.as_deref()
    }

    /// `getLeafEntry()` (`:1128-1130`).
    pub fn get_leaf_entry(&self) -> Option<&FileEntry> {
        self.leaf_id.as_deref().and_then(|id| self.get_entry(id))
    }

    /// `getEntry(id)` (`:1132-1134`).
    pub fn get_entry(&self, id: &str) -> Option<&FileEntry> {
        self.by_id
            .get(id)
            .and_then(|index| self.file_entries.get(*index))
    }

    /// `getLabel(id)` (`:1152-1154`).
    pub fn get_label(&self, id: &str) -> Option<&str> {
        self.labels_by_id.get(id).map(String::as_str)
    }

    /// `getHeader()` (`:1220-1223`).
    pub fn get_header(&self) -> Option<&FileEntry> {
        self.file_entries.iter().find(|entry| is_header(entry))
    }

    /// `getEntries()` (`:1230-1232`) — every non-header entry, in file order.
    pub fn get_entries(&self) -> Vec<&FileEntry> {
        self.file_entries
            .iter()
            .filter(|entry| !is_header(entry))
            .collect()
    }

    /// Every buffered line **including** the header — what `_rewriteFile` would write.
    /// Pi has no such accessor; it exists so a test can assert the exact bytes without
    /// forcing a flush.
    pub fn file_entries(&self) -> &[FileEntry] {
        &self.file_entries
    }

    /// `getBranch(fromId?)` (`:1189-1199`) — the walk from an entry (default: the leaf) to
    /// the root, root-first.
    pub fn get_branch(&self, from_id: Option<&str>) -> Vec<&FileEntry> {
        let start = from_id.or(self.leaf_id.as_deref());
        let mut current = start
            .filter(|id| !id.is_empty())
            .and_then(|id| self.get_entry(id));
        let mut path = Vec::new();
        while let Some(entry) = current {
            path.push(entry);
            current = entry_parent_id(entry).and_then(|id| self.get_entry(id));
        }
        path.reverse();
        path
    }

    /// Not a port — see the doc comment on [`build_branch_list`] for why (grepped for a
    /// `listBranches`/`getTree` equivalent in `session-manager.ts`; there isn't one) and for
    /// the ordering, orphan and cycle rules, which this just applies to `self`. Added so a
    /// later `/tree` picker has something to render: [`Self::get_branch`] only ever returns
    /// one root-first path, never the whole tree.
    pub fn list_branches(&self) -> Vec<BranchInfo<'_>> {
        let entries = self.get_entries();
        build_branch_list(&entries, self.leaf_id.as_deref(), &self.labels_by_id)
    }

    /// [`Self::list_branches`], filtered to the entries where history actually forks
    /// (`is_branch_point`). Convenience for a caller that only cares where a `/tree` picker
    /// needs to offer a choice, not the full linear filler in between.
    pub fn branch_points(&self) -> Vec<BranchInfo<'_>> {
        self.list_branches()
            .into_iter()
            .filter(|branch| branch.is_branch_point)
            .collect()
    }

    /// `buildContextEntries()` (`:1205-1207`) — [`build_context_entries`] from the current
    /// leaf.
    pub fn build_context_entries(&self) -> Vec<&FileEntry> {
        let entries = self.get_entries();
        build_context_entries(&entries, LeafId::from_option(self.leaf_id.as_deref()))
    }

    /// `getSessionName()` (`:1079-1090`) — the latest `session_info` name; an empty one
    /// clears it.
    pub fn get_session_name(&self) -> Option<&str> {
        for entry in self.file_entries.iter().rev() {
            if is_header(entry) || entry_type(entry) != Some("session_info") {
                continue;
            }
            return entry
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|name| !name.is_empty());
        }
        None
    }

    // -------------------------------------------------------------------------
    // Branching (`session-manager.ts:1289-1434`)
    // -------------------------------------------------------------------------

    /// `branch(branchFromId)` (`:1289-1294`) — move the leaf, keeping all history.
    pub fn branch(&mut self, branch_from_id: &str) -> Result<(), SessionError> {
        if !self.by_id.contains_key(branch_from_id) {
            return Err(SessionError::EntryNotFound(branch_from_id.to_string()));
        }
        self.leaf_id = Some(branch_from_id.to_string());
        Ok(())
    }

    /// `resetLeaf()` (`:1301-1303`) — the next append becomes a new root.
    pub fn reset_leaf(&mut self) {
        self.leaf_id = None;
    }

    /// `branchWithSummary(branchFromId, summary, details?, fromHook?)` (`:1310-1327`).
    ///
    /// `branchFromId = None` is Pi's `null` (branch from the root), which skips the existence
    /// check and records `fromId: "root"`.
    pub fn branch_with_summary(
        &mut self,
        branch_from_id: Option<&str>,
        summary: &str,
        details: Option<Value>,
        from_hook: Option<bool>,
    ) -> Result<String, SessionError> {
        if let Some(id) = branch_from_id {
            if !self.by_id.contains_key(id) {
                return Err(SessionError::EntryNotFound(id.to_string()));
            }
        }
        self.leaf_id = branch_from_id.map(str::to_string);
        let mut entry = self.entry_head("branch_summary");
        entry.insert(
            "fromId".to_string(),
            Value::String(branch_from_id.unwrap_or("root").to_string()),
        );
        entry.insert("summary".to_string(), Value::String(summary.to_string()));
        if let Some(details) = details {
            entry.insert("details".to_string(), details);
        }
        if let Some(from_hook) = from_hook {
            entry.insert("fromHook".to_string(), Value::Bool(from_hook));
        }
        self.append_entry(Value::Object(entry))
    }

    /// `createBranchedSession(leafId)` (`session-manager.ts:1334-1434`).
    ///
    /// Extracts the root→leaf path into a **new** session file: `label` entries are dropped
    /// from the path and re-chained from the resolved label map (because later entries can be
    /// children of a label), the retained entries are re-parented into a linear chain
    /// (re-assigning `parentId` keeps its original key position), and the new header carries
    /// `parentSession: <previous file>`.
    ///
    /// Persisting mode returns the new path and — crucially — **only writes the file if the
    /// path already contains an assistant message** (`:1399-1410`); otherwise it defers to
    /// `_persist`, which avoids the duplicate-header bug its comment describes. In-memory
    /// mode replaces the current session and returns `None`.
    pub fn create_branched_session(
        &mut self,
        leaf_id: &str,
    ) -> Result<Option<String>, SessionError> {
        let previous_session_file = self.session_file.clone();
        let path: Vec<FileEntry> = self
            .get_branch(Some(leaf_id))
            .into_iter()
            .cloned()
            .collect();
        if path.is_empty() {
            return Err(SessionError::EntryNotFound(leaf_id.to_string()));
        }

        let mut path_without_labels: Vec<FileEntry> = Vec::new();
        let mut path_parent_id: Option<String> = None;
        for entry in path {
            if entry_type(&entry) == Some("label") {
                continue;
            }
            let id = entry_id(&entry).map(str::to_string);
            let mut entry = entry;
            if let Some(object) = entry.as_object_mut() {
                object.insert(
                    "parentId".to_string(),
                    path_parent_id.clone().map_or(Value::Null, Value::String),
                );
            }
            path_without_labels.push(entry);
            path_parent_id = id;
        }

        let new_session_id = self.env.ids.session_id();
        let timestamp = self.env.clock.now_iso();
        let file_timestamp = timestamp.replace([':', '.'], "-");
        let new_session_file = self.env.join(
            &self.session_dir,
            &[&format!(
                "{file_timestamp}_{new_session_id}{SESSION_FILE_SUFFIX}"
            )],
        );

        let mut header = Map::new();
        header.insert("type".to_string(), Value::String("session".to_string()));
        header.insert("version".to_string(), Value::from(CURRENT_SESSION_VERSION));
        header.insert("id".to_string(), Value::String(new_session_id.clone()));
        header.insert("timestamp".to_string(), Value::String(timestamp));
        header.insert("cwd".to_string(), Value::String(self.cwd.clone()));
        if self.persist {
            if let Some(previous) = previous_session_file {
                header.insert("parentSession".to_string(), Value::String(previous));
            }
        }

        // Labels whose target survived, in `labelsById` iteration order.
        let mut path_entry_ids: std::collections::HashSet<String> = path_without_labels
            .iter()
            .filter_map(|entry| entry_id(entry).map(str::to_string))
            .collect();
        let mut labels_to_write: Vec<(String, String, String)> = Vec::new();
        for (target_id, label) in &self.labels_by_id {
            if path_entry_ids.contains(target_id) {
                labels_to_write.push((
                    target_id.clone(),
                    label.clone(),
                    self.label_timestamps_by_id
                        .get(target_id)
                        .cloned()
                        .unwrap_or_default(),
                ));
            }
        }

        let mut parent_id = path_without_labels
            .last()
            .and_then(|entry| entry_id(entry).map(str::to_string));
        let mut label_entries: Vec<FileEntry> = Vec::new();
        for (target_id, label, label_timestamp) in labels_to_write {
            let id = generate_id(&*self.env.ids, &|id| path_entry_ids.contains(id));
            let mut entry = Map::new();
            entry.insert("type".to_string(), Value::String("label".to_string()));
            entry.insert("id".to_string(), Value::String(id.clone()));
            entry.insert(
                "parentId".to_string(),
                parent_id.clone().map_or(Value::Null, Value::String),
            );
            entry.insert("timestamp".to_string(), Value::String(label_timestamp));
            entry.insert("targetId".to_string(), Value::String(target_id));
            entry.insert("label".to_string(), Value::String(label));
            path_entry_ids.insert(id.clone());
            label_entries.push(Value::Object(entry));
            parent_id = Some(id);
        }

        let mut entries = vec![Value::Object(header)];
        entries.extend(path_without_labels);
        entries.extend(label_entries);
        self.file_entries = entries;
        self.session_id = new_session_id;
        if !self.persist {
            self.build_index();
            return Ok(None);
        }

        self.session_file = Some(new_session_file.clone());
        self.build_index();
        if self.file_entries.iter().any(is_assistant_message) {
            self.rewrite_file()?;
            self.flushed = true;
        } else {
            self.flushed = false;
        }
        Ok(Some(new_session_file))
    }
}

// =============================================================================
// The factories (`session-manager.ts:1436-1541`)
// =============================================================================

impl SessionEnv {
    /// `SessionManager.create(cwd, sessionDir?, options?)` (`session-manager.ts:1441-1444`).
    pub fn create(
        &self,
        cwd: &str,
        session_dir: Option<&str>,
        options: Option<&NewSessionOptions>,
    ) -> Result<SessionManager, SessionError> {
        let dir = self.resolve_session_dir(cwd, session_dir)?;
        SessionManager::construct(self, cwd, &dir, None, true, options)
    }

    /// `SessionManager.open(path, sessionDir?, cwdOverride?)`
    /// (`session-manager.ts:1452-1461`).
    ///
    /// The cwd comes from `cwdOverride ?? header?.cwd ?? process.cwd()`, and the session dir
    /// defaults to the file's **parent** (`resolve(resolvedPath, "..")`) rather than the
    /// encoded default — so `/new` from an opened session lands beside it.
    ///
    /// Note the header is loaded twice (once here, once in `setSessionFile`), as in Pi.
    pub fn open(
        &self,
        path: &str,
        session_dir: Option<&str>,
        cwd_override: Option<&str>,
    ) -> Result<SessionManager, SessionError> {
        let resolved_path = self.resolve_path(path)?;
        let entries = self.load_entries_from_file(&resolved_path)?;
        let header_cwd = entries
            .iter()
            .find(|entry| is_header(entry))
            .and_then(|header| header.get("cwd"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let cwd = match (cwd_override, header_cwd) {
            (Some(cwd), _) => cwd.to_string(),
            (None, Some(cwd)) => cwd,
            (None, None) => self.process_cwd.clone(),
        };
        let dir = match session_dir.filter(|dir| !dir.is_empty()) {
            Some(dir) => self.normalize_path(dir)?,
            None => self.resolve_path_in("..", &resolved_path)?,
        };
        SessionManager::construct(self, &cwd, &dir, Some(&resolved_path), true, None)
    }

    /// `SessionManager.continueRecent(cwd, sessionDir?)`
    /// (`session-manager.ts:1468-1476`).
    ///
    /// `filterCwd = sessionDir !== undefined && dir !== getDefaultSessionDirPath(cwd)` —
    /// so a *custom* directory shared by several projects only continues this project's
    /// sessions, while the default directory (already cwd-specific) is not filtered. Falls
    /// back to a fresh session when nothing matches.
    pub fn continue_recent(
        &self,
        cwd: &str,
        session_dir: Option<&str>,
    ) -> Result<SessionManager, SessionError> {
        let dir = self.resolve_session_dir(cwd, session_dir)?;
        let filter_cwd =
            session_dir.is_some() && dir != self.default_session_dir_path(cwd, None)?;
        let most_recent =
            self.find_most_recent_session(&dir, if filter_cwd { Some(cwd) } else { None })?;
        SessionManager::construct(self, cwd, &dir, most_recent.as_deref(), true, None)
    }

    /// `SessionManager.inMemory(cwd = process.cwd(), options?)`
    /// (`session-manager.ts:1479-1481`) — `persist = false`, `sessionDir = ""`, no file ever.
    pub fn in_memory(
        &self,
        cwd: Option<&str>,
        options: Option<&NewSessionOptions>,
    ) -> Result<SessionManager, SessionError> {
        let cwd = cwd.unwrap_or(&self.process_cwd).to_string();
        SessionManager::construct(self, &cwd, "", None, false, options)
    }

    /// `SessionManager.forkFrom(sourcePath, targetCwd, sessionDir?, options?)`
    /// (`session-manager.ts:1490-1541`).
    ///
    /// Validates the source (non-empty **and** with a header), ensures the target directory,
    /// then writes a new header with `cwd: resolvedTargetCwd` and
    /// `parentSession: resolvedSourcePath` using flag `"wx"`, and appends every non-header
    /// source entry **verbatim** — ids, parents and timestamps included. The forked file
    /// therefore exists on disk immediately, unlike a fresh session.
    pub fn fork_from(
        &self,
        source_path: &str,
        target_cwd: &str,
        session_dir: Option<&str>,
        options: Option<&NewSessionOptions>,
    ) -> Result<SessionManager, SessionError> {
        let resolved_source_path = self.resolve_path(source_path)?;
        let resolved_target_cwd = self.resolve_path(target_cwd)?;
        let source_entries = self.load_entries_from_file(&resolved_source_path)?;
        if source_entries.is_empty() {
            return Err(SessionError::ForkSourceEmpty(resolved_source_path));
        }
        if !source_entries.iter().any(is_header) {
            return Err(SessionError::ForkSourceNoHeader(resolved_source_path));
        }

        let dir = match session_dir.filter(|dir| !dir.is_empty()) {
            Some(dir) => self.normalize_path(dir)?,
            None => self.default_session_dir(&resolved_target_cwd, None)?,
        };
        if !exists(&dir) {
            fs::create_dir_all(&dir).map_err(|e| SessionError::io("mkdir", &dir, e))?;
        }

        let new_session_id = match options.and_then(|options| options.id.as_deref()) {
            Some(id) => {
                assert_valid_session_id(id)?;
                id.to_string()
            }
            None => self.ids.session_id(),
        };
        let timestamp = self.clock.now_iso();
        let file_timestamp = timestamp.replace([':', '.'], "-");
        let new_session_file = self.join(
            &dir,
            &[&format!(
                "{file_timestamp}_{new_session_id}{SESSION_FILE_SUFFIX}"
            )],
        );

        let mut header = Map::new();
        header.insert("type".to_string(), Value::String("session".to_string()));
        header.insert("version".to_string(), Value::from(CURRENT_SESSION_VERSION));
        header.insert("id".to_string(), Value::String(new_session_id));
        header.insert("timestamp".to_string(), Value::String(timestamp));
        header.insert(
            "cwd".to_string(),
            Value::String(resolved_target_cwd.clone()),
        );
        header.insert(
            "parentSession".to_string(),
            Value::String(resolved_source_path),
        );
        write_exclusive(&new_session_file, &to_line(&Value::Object(header)))?;

        for entry in &source_entries {
            if !is_header(entry) {
                append_to(&new_session_file, &to_line(entry))?;
            }
        }

        SessionManager::construct(
            self,
            &resolved_target_cwd,
            &dir,
            Some(&new_session_file),
            true,
            None,
        )
    }
}

// =============================================================================
// Session resolution from flags (`main.ts:142-355`)
// =============================================================================

/// `ResolvedSession` (`main.ts:143-147`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedSession {
    /// The argument looked like a path; used as-is after `resolvePath(arg, cwd)`.
    Path(String),
    /// Matched a session id (exactly, or by prefix) in the current project.
    Local(String),
    /// Matched in a *different* project — the branch that prompts (spec §17.2).
    Global {
        /// The matched session file.
        path: String,
        /// That session's `cwd`, which the message quotes.
        cwd: String,
    },
    /// No match anywhere; carries the original argument for the error message.
    NotFound(String),
}

/// `findLocalSessionByExactId(sessionId, cwd, sessionDir?)` (`main.ts:153-161`) — **exact**
/// id only, no prefix matching. Used to decide whether `--session-id` names an existing
/// session (and, under `--fork`, whether it would collide).
pub fn find_local_session_by_exact_id(
    env: &SessionEnv,
    session_id: &str,
    cwd: &str,
    session_dir: Option<&str>,
) -> Result<Option<String>, SessionError> {
    let local = env.list(cwd, session_dir, None)?;
    Ok(local
        .into_iter()
        .find(|session| session.id == session_id)
        .map(|session| session.path))
}

/// `resolveSessionPath(sessionArg, cwd, sessionDir?)` (`main.ts:163-189`).
///
/// 1. Looks like a path (contains `/` or `\`, or ends in `.jsonl`) → [`ResolvedSession::Path`]
///    with `resolvePath(arg, cwd)` — note the base is the **cwd**, not `process.cwd()`.
/// 2. Otherwise, this project's sessions: an **exact** id match, else the first whose id
///    **starts with** the argument (`main.ts:172`) — that is the partial-UUID prefix match.
/// 3. Otherwise the same two-step search across *all* projects → [`ResolvedSession::Global`].
/// 4. Otherwise [`ResolvedSession::NotFound`].
///
/// "First" is after each list's `modified`-descending sort, so a prefix shared by several
/// sessions selects the most recently active one.
pub fn resolve_session_path(
    env: &SessionEnv,
    session_arg: &str,
    cwd: &str,
    session_dir: Option<&str>,
) -> Result<ResolvedSession, SessionError> {
    if session_arg.contains('/')
        || session_arg.contains('\\')
        || session_arg.ends_with(SESSION_FILE_SUFFIX)
    {
        return Ok(ResolvedSession::Path(
            env.resolve_path_in(session_arg, cwd)?,
        ));
    }

    let local = env.list(cwd, session_dir, None)?;
    let local_match = local
        .iter()
        .find(|session| session.id == session_arg)
        .or_else(|| {
            local
                .iter()
                .find(|session| session.id.starts_with(session_arg))
        });
    if let Some(local_match) = local_match {
        return Ok(ResolvedSession::Local(local_match.path.clone()));
    }

    let all = env.list_all(session_dir, None)?;
    let global_match = all
        .iter()
        .find(|session| session.id == session_arg)
        .or_else(|| {
            all.iter()
                .find(|session| session.id.starts_with(session_arg))
        });
    if let Some(global_match) = global_match {
        return Ok(ResolvedSession::Global {
            path: global_match.path.clone(),
            cwd: global_match.cwd.clone(),
        });
    }

    Ok(ResolvedSession::NotFound(session_arg.to_string()))
}

/// The console + prompt sinks `create_session_manager` needs, bundled.
pub struct SessionIo<'a> {
    /// Where `console.log`/`console.error` go.
    pub console: &'a mut dyn SessionConsole,
    /// The two interactive prompts. Pass [`HeadlessPrompts`] in print/json/rpc mode.
    pub prompts: &'a mut dyn SessionPrompts,
}

/// `console.error(chalk.red(...)); process.exit(1)`.
fn fatal(console: &mut dyn SessionConsole, text: &str) -> SessionExit {
    console.write(SessionStream::Stderr, SessionStyle::Red, text);
    SessionExit::FAILURE
}

/// The `catch` shared by `openSessionOrExit`/`forkSessionOrExit` (`main.ts:244-262`):
/// `` console.error(chalk.red(`Error: ${message}`)); process.exit(1) ``.
fn fatal_error(console: &mut dyn SessionConsole, error: &SessionError) -> SessionExit {
    fatal(console, &format!("Error: {error}"))
}

/// `parsed.<flag>` truthiness for a `boolean | undefined`.
fn flag(value: Option<bool>) -> bool {
    value == Some(true)
}

/// `parsed.<flag>` truthiness for a `string | undefined` — the empty string is **falsy**,
/// which is why `--session ""` falls through to a fresh session.
fn non_empty(value: &Option<String>) -> Option<&str> {
    value.as_deref().filter(|value| !value.is_empty())
}

/// `validateForkFlags(parsed)` (`main.ts:205-219`).
///
/// The candidate list order is fixed — `--session`, `--continue`, `--resume`,
/// `--no-session` — and they are joined with `", "` into
/// `` `Error: --fork cannot be combined with ${flags}` `` on **stderr**, then exit 1.
pub fn validate_fork_flags(
    parsed: &crate::args::Args,
    console: &mut dyn SessionConsole,
) -> Result<(), SessionExit> {
    if non_empty(&parsed.fork).is_none() {
        return Ok(());
    }
    let conflicting: Vec<&str> = [
        non_empty(&parsed.session).map(|_| "--session"),
        flag(parsed.r#continue).then_some("--continue"),
        flag(parsed.resume).then_some("--resume"),
        flag(parsed.no_session).then_some("--no-session"),
    ]
    .into_iter()
    .flatten()
    .collect();

    if conflicting.is_empty() {
        return Ok(());
    }
    Err(fatal(
        console,
        &format!(
            "Error: --fork cannot be combined with {}",
            conflicting.join(", ")
        ),
    ))
}

/// `validateSessionIdFlags(parsed)` (`main.ts:221-242`).
///
/// Gated on `parsed.sessionId !== undefined`, so `--session-id ""` **is** checked (and then
/// fails `assertValidSessionId`). Candidates: `--session`, `--continue`, `--resume` —
/// `--no-session` is *not* a conflict here (it wins later, at `main.ts:270`). The id
/// validation error is reported as `` `Error: ${message}` ``.
pub fn validate_session_id_flags(
    parsed: &crate::args::Args,
    console: &mut dyn SessionConsole,
) -> Result<(), SessionExit> {
    let Some(session_id) = parsed.session_id.as_deref() else {
        return Ok(());
    };
    let conflicting: Vec<&str> = [
        non_empty(&parsed.session).map(|_| "--session"),
        flag(parsed.r#continue).then_some("--continue"),
        flag(parsed.resume).then_some("--resume"),
    ]
    .into_iter()
    .flatten()
    .collect();

    if !conflicting.is_empty() {
        return Err(fatal(
            console,
            &format!(
                "Error: --session-id cannot be combined with {}",
                conflicting.join(", ")
            ),
        ));
    }

    if let Err(error) = assert_valid_session_id(session_id) {
        return Err(fatal(console, &format!("Error: {error}")));
    }
    Ok(())
}

/// `createSessionManager(parsed, cwd, sessionDir, settingsManager)`
/// (`main.ts:264-355`).
///
/// The branch order is load-bearing: `--no-session`/`--help`/`--list-models` →
/// `--fork` → `--session` → `--resume` → `--continue` → `--session-id` → a fresh session.
///
/// `settingsManager` is **not** a parameter here: Pi passes it only so the `--resume` picker
/// can read theme/keybinding settings, which is the injected [`SessionPrompts`]'
/// responsibility now (see the module docs on `session_dir`).
///
/// Every Pi `process.exit` becomes an `Err(SessionExit)` *after* the same text has been
/// written to `io.console`. Failures Pi leaves **uncaught** (a throwing `continueRecent`,
/// `create`, or `resolveSessionPath`) crash Node with a stack trace and exit code 1; here
/// they report `` `Error: ${message}` `` and exit 1, which is the same code and stream but
/// not the same bytes.
pub fn create_session_manager(
    env: &SessionEnv,
    parsed: &crate::args::Args,
    cwd: &str,
    session_dir: Option<&str>,
    io: &mut SessionIo<'_>,
) -> Result<SessionManager, SessionExit> {
    // main.ts:270-272
    if flag(parsed.no_session) || flag(parsed.help) || parsed.list_models.is_some() {
        let options = parsed.session_id.as_deref().map(NewSessionOptions::with_id);
        return env
            .in_memory(Some(cwd), options.as_ref())
            .map_err(|error| fatal_error(io.console, &error));
    }

    // main.ts:274-295
    if let Some(fork) = non_empty(&parsed.fork) {
        if let Some(session_id) = non_empty(&parsed.session_id) {
            let existing = find_local_session_by_exact_id(env, session_id, cwd, session_dir)
                .map_err(|error| fatal_error(io.console, &error))?;
            if existing.is_some() {
                return Err(fatal(
                    io.console,
                    &format!("Session already exists with id '{session_id}'"),
                ));
            }
        }

        let resolved = resolve_session_path(env, fork, cwd, session_dir)
            .map_err(|error| fatal_error(io.console, &error))?;
        return match resolved {
            ResolvedSession::Path(path)
            | ResolvedSession::Local(path)
            | ResolvedSession::Global { path, .. } => env
                .fork_from(
                    &path,
                    cwd,
                    session_dir,
                    parsed
                        .session_id
                        .as_deref()
                        .map(NewSessionOptions::with_id)
                        .as_ref(),
                )
                .map_err(|error| fatal_error(io.console, &error)),
            ResolvedSession::NotFound(arg) => Err(fatal(
                io.console,
                &format!("No session found matching '{arg}'"),
            )),
        };
    }

    // main.ts:297-319
    if let Some(session) = non_empty(&parsed.session) {
        let resolved = resolve_session_path(env, session, cwd, session_dir)
            .map_err(|error| fatal_error(io.console, &error))?;
        return match resolved {
            ResolvedSession::Path(path) | ResolvedSession::Local(path) => env
                .open(&path, session_dir, None)
                .map_err(|error| fatal_error(io.console, &error)),
            ResolvedSession::Global {
                path,
                cwd: session_cwd,
            } => {
                io.console.write(
                    SessionStream::Stdout,
                    SessionStyle::Yellow,
                    &format!("Session found in different project: {session_cwd}"),
                );
                if !io
                    .prompts
                    .confirm("Fork this session into current directory?")
                {
                    io.console
                        .write(SessionStream::Stdout, SessionStyle::Dim, "Aborted.");
                    return Err(SessionExit::SUCCESS);
                }
                env.fork_from(&path, cwd, session_dir, None)
                    .map_err(|error| fatal_error(io.console, &error))
            }
            ResolvedSession::NotFound(arg) => Err(fatal(
                io.console,
                &format!("No session found matching '{arg}'"),
            )),
        };
    }

    // main.ts:321-336
    if flag(parsed.resume) {
        let loaders = SessionLoaders {
            env,
            cwd,
            session_dir,
        };
        let selected = io.prompts.select_session(&loaders).map_err(|unavailable| {
            // The intentional divergence of spec §17.1 — Pi would build a TUI and hang.
            fatal(io.console, &format!("Error: {unavailable}"))
        })?;
        let Some(selected) = selected else {
            io.console.write(
                SessionStream::Stdout,
                SessionStyle::Dim,
                "No session selected",
            );
            return Err(SessionExit::SUCCESS);
        };
        return env
            .open(&selected, session_dir, None)
            .map_err(|error| fatal_error(io.console, &error));
    }

    // main.ts:338-340
    if flag(parsed.r#continue) {
        return env
            .continue_recent(cwd, session_dir)
            .map_err(|error| fatal_error(io.console, &error));
    }

    // main.ts:342-352
    if let Some(session_id) = non_empty(&parsed.session_id) {
        let existing = find_local_session_by_exact_id(env, session_id, cwd, session_dir)
            .map_err(|error| fatal_error(io.console, &error))?;
        if let Some(existing) = existing {
            return env
                .open(&existing, session_dir, None)
                .map_err(|error| fatal_error(io.console, &error));
        }
        io.console.write(
            SessionStream::Stderr,
            SessionStyle::Yellow,
            &format!(
                "Warning: No project session found with id '{session_id}'; creating a new session with that id."
            ),
        );
    }

    // main.ts:354
    env.create(
        cwd,
        session_dir,
        parsed
            .session_id
            .as_deref()
            .map(NewSessionOptions::with_id)
            .as_ref(),
    )
    .map_err(|error| fatal_error(io.console, &error))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_id_validation_matches_the_regex() {
        // `^[A-Za-z0-9](?:[A-Za-z0-9._-]*[A-Za-z0-9])?$`
        for ok in ["a", "0", "a-b", "a_b", "a.b", "a1.2_3-4z", "AZaz09"] {
            assert!(
                assert_valid_session_id(ok).is_ok(),
                "{ok:?} should be valid"
            );
        }
        for bad in [
            "", "-a", "a-", ".a", "a.", "_a", "a_", "a b", "a/b", "é1", "a\n",
        ] {
            assert!(
                assert_valid_session_id(bad).is_err(),
                "{bad:?} should be invalid"
            );
        }
        assert_eq!(
            SessionError::InvalidSessionId.to_string(),
            INVALID_SESSION_ID_MESSAGE
        );
    }

    #[test]
    fn random_uuid_has_the_crypto_random_uuid_shape() {
        let uuid = random_uuid_v4();
        assert_eq!(uuid.len(), 36);
        let parts: Vec<&str> = uuid.split('-').collect();
        assert_eq!(
            parts.iter().map(|p| p.len()).collect::<Vec<_>>(),
            [8, 4, 4, 4, 12]
        );
        assert!(uuid.chars().all(|c| c.is_ascii_hexdigit() || c == '-'));
        // version 4, variant 10xx
        assert!(parts[2].starts_with('4'));
        assert!(matches!(
            parts[3].chars().next(),
            Some('8' | '9' | 'a' | 'b')
        ));
        // `slice(0, 8)` is what `generateId` keeps.
        assert_eq!(&uuid[..8], parts[0]);
    }

    #[test]
    fn generate_id_retries_then_falls_back() {
        /// Hands out a fixed "random" UUID, so the collision path is deterministic.
        struct Fixed(&'static str);
        impl SessionIdSource for Fixed {
            fn session_id(&self) -> String {
                "session".to_string()
            }
            fn random_uuid(&self) -> String {
                self.0.to_string()
            }
        }
        let ids = Fixed("deadbeef-1111-4222-8333-444455556666");
        assert_eq!(generate_id(&ids, &|_| false), "deadbeef");
        // 100 collisions in a row -> the full UUID.
        assert_eq!(
            generate_id(&ids, &|id| id == "deadbeef"),
            "deadbeef-1111-4222-8333-444455556666"
        );
    }

    #[test]
    fn iso_timestamps_parse_like_new_date_get_time() {
        assert_eq!(js_date_ms("1970-01-01T00:00:00.000Z"), Some(0));
        assert_eq!(
            js_date_ms("2023-11-14T22:13:20.000Z"),
            Some(1_700_000_000_000)
        );
        assert_eq!(
            js_date_ms("2025-12-08T22:41:05.306Z"),
            Some(1_765_233_665_306)
        );
        // Documented narrowing: non-canonical forms are Invalid Date here.
        assert_eq!(js_date_ms("2025-12-08"), None);
        assert_eq!(js_date_ms("not a date"), None);
    }

    #[test]
    fn header_version_models_js_coercion() {
        let v1: Vec<FileEntry> = vec![serde_json::json!({"type":"session","id":"x"})];
        assert_eq!(header_version(&v1), Some(1.0));
        let v3: Vec<FileEntry> = vec![serde_json::json!({"type":"session","version":3})];
        assert_eq!(header_version(&v3), Some(3.0));
        let stringy: Vec<FileEntry> = vec![serde_json::json!({"type":"session","version":"2"})];
        assert_eq!(header_version(&stringy), Some(2.0));
        let nonsense: Vec<FileEntry> = vec![serde_json::json!({"type":"session","version":true})];
        assert_eq!(header_version(&nonsense), None);
        // No header at all is also `?? 1`.
        assert_eq!(header_version(&[]), Some(1.0));
    }

    #[test]
    fn leaf_id_has_three_states() {
        let a = serde_json::json!({"type":"message","id":"a","parentId":null});
        let b = serde_json::json!({"type":"message","id":"b","parentId":"a"});
        let entries: Vec<&FileEntry> = vec![&a, &b];

        // `null` -> empty path.
        assert!(build_session_path(&entries, LeafId::Null).is_empty());
        // `undefined` -> the last entry.
        assert_eq!(build_session_path(&entries, LeafId::Unset).len(), 2);
        // A known id.
        assert_eq!(build_session_path(&entries, LeafId::Id("a")).len(), 1);
        // An unknown id falls back to the last entry (:343), and "" is falsy.
        assert_eq!(build_session_path(&entries, LeafId::Id("zz")).len(), 2);
        assert_eq!(build_session_path(&entries, LeafId::Id("")).len(), 2);
    }

    #[test]
    fn context_entries_drop_the_summarized_prefix() {
        // path: u1 -> u2 -> u3 -> compaction(firstKept = u2) -> u4
        let u1 = serde_json::json!({"type":"message","id":"u1","parentId":null});
        let u2 = serde_json::json!({"type":"message","id":"u2","parentId":"u1"});
        let u3 = serde_json::json!({"type":"message","id":"u3","parentId":"u2"});
        let compaction = serde_json::json!({
            "type":"compaction","id":"c1","parentId":"u3","firstKeptEntryId":"u2"
        });
        let u4 = serde_json::json!({"type":"message","id":"u4","parentId":"c1"});
        let entries: Vec<&FileEntry> = vec![&u1, &u2, &u3, &compaction, &u4];

        let context = build_context_entries(&entries, LeafId::Unset);
        let ids: Vec<&str> = context.iter().filter_map(|e| entry_id(e)).collect();
        // The compaction comes FIRST, then the kept prefix, then the tail.
        assert_eq!(ids, ["c1", "u2", "u3", "u4"]);
        assert_eq!(
            entry_id(get_latest_compaction_entry(&entries).unwrap()),
            Some("c1")
        );
    }

    #[test]
    fn empty_and_malformed_lines_are_skipped_not_fatal() {
        let content = "{\"type\":\"session\",\"id\":\"a\"}\n\n{not json\n{\"type\":\"message\",\"id\":\"b\"}\n";
        let entries = parse_session_entries(content);
        assert_eq!(entries.len(), 2);
        assert_eq!(entry_id(&entries[1]), Some("b"));
    }

    #[test]
    fn list_branches_linear_session_has_no_branch_points() {
        let a = serde_json::json!({"type":"message","id":"a","parentId":null,"timestamp":"t0"});
        let b = serde_json::json!({"type":"message","id":"b","parentId":"a","timestamp":"t1"});
        let c = serde_json::json!({"type":"message","id":"c","parentId":"b","timestamp":"t2"});
        let entries: Vec<&FileEntry> = vec![&a, &b, &c];
        let mut labels = HashMap::new();
        labels.insert("b".to_string(), "checkpoint".to_string());
        let branches = build_branch_list(&entries, None, &labels);

        assert_eq!(
            branches.iter().map(|info| info.id).collect::<Vec<_>>(),
            ["a", "b", "c"]
        );
        assert!(branches.iter().all(|info| !info.is_branch_point));
        assert_eq!(
            branches.iter().map(|info| info.depth).collect::<Vec<_>>(),
            [0, 1, 2]
        );
        assert_eq!(
            branches
                .iter()
                .map(|info| info.child_count)
                .collect::<Vec<_>>(),
            [1, 1, 0]
        );
        assert_eq!(
            branches.iter().find(|info| info.id == "b").unwrap().label,
            Some("checkpoint")
        );
        assert_eq!(
            branches.iter().find(|info| info.id == "a").unwrap().label,
            None
        );
    }

    #[test]
    fn list_branches_flags_a_real_fork() {
        let root = serde_json::json!({"type":"message","id":"root","parentId":null});
        let c1 = serde_json::json!({"type":"message","id":"c1","parentId":"root"});
        let c2 = serde_json::json!({"type":"message","id":"c2","parentId":"root"});
        let entries: Vec<&FileEntry> = vec![&root, &c1, &c2];
        let labels = HashMap::new();
        let branches = build_branch_list(&entries, None, &labels);

        // Pre-order: the parent, then each child, in append order.
        assert_eq!(
            branches.iter().map(|info| info.id).collect::<Vec<_>>(),
            ["root", "c1", "c2"]
        );
        let root_info = branches.iter().find(|info| info.id == "root").unwrap();
        assert!(root_info.is_branch_point);
        assert_eq!(root_info.child_count, 2);
        assert_eq!(root_info.depth, 0);
        for child_id in ["c1", "c2"] {
            let info = branches.iter().find(|info| info.id == child_id).unwrap();
            assert!(!info.is_branch_point);
            assert_eq!(info.child_count, 0);
            assert_eq!(info.depth, 1);
        }
    }

    #[test]
    fn list_branches_depth_and_current_leaf_past_a_fork() {
        let root = serde_json::json!({"type":"message","id":"root","parentId":null});
        let mid = serde_json::json!({"type":"message","id":"mid","parentId":"root"});
        let leaf_a = serde_json::json!({"type":"message","id":"leaf_a","parentId":"mid"});
        let leaf_b = serde_json::json!({"type":"message","id":"leaf_b","parentId":"mid"});
        let entries: Vec<&FileEntry> = vec![&root, &mid, &leaf_a, &leaf_b];
        let labels = HashMap::new();
        let branches = build_branch_list(&entries, Some("leaf_b"), &labels);

        let depth_of = |id: &str| branches.iter().find(|info| info.id == id).unwrap().depth;
        assert_eq!(depth_of("root"), 0);
        assert_eq!(depth_of("mid"), 1);
        assert_eq!(depth_of("leaf_a"), 2);
        assert_eq!(depth_of("leaf_b"), 2);

        assert!(
            !branches
                .iter()
                .find(|info| info.id == "leaf_a")
                .unwrap()
                .is_current_leaf
        );
        assert!(
            branches
                .iter()
                .find(|info| info.id == "leaf_b")
                .unwrap()
                .is_current_leaf
        );
    }

    #[test]
    fn list_branches_surfaces_an_orphan_instead_of_dropping_it() {
        let root = serde_json::json!({"type":"message","id":"root","parentId":null});
        // `parentId` names an id this session doesn't have — e.g. a partially written file.
        let orphan = serde_json::json!({"type":"message","id":"orphan","parentId":"missing"});
        let entries: Vec<&FileEntry> = vec![&root, &orphan];
        let labels = HashMap::new();
        let branches = build_branch_list(&entries, None, &labels);

        assert_eq!(branches.len(), 2);
        let orphan_info = branches.iter().find(|info| info.id == "orphan").unwrap();
        // Surfaced as its own root...
        assert_eq!(orphan_info.depth, 0);
        assert!(!orphan_info.is_branch_point);
        // ...but the stale parentId is still visible to the caller.
        assert_eq!(orphan_info.parent_id, Some("missing"));
    }

    #[test]
    fn list_branches_does_not_hang_on_a_parent_id_cycle() {
        let a = serde_json::json!({"type":"message","id":"a","parentId":"b"});
        let b = serde_json::json!({"type":"message","id":"b","parentId":"a"});
        let entries: Vec<&FileEntry> = vec![&a, &b];
        let labels = HashMap::new();
        // If the walk didn't terminate, this call itself would hang the test.
        let branches = build_branch_list(&entries, None, &labels);

        let mut ids: Vec<&str> = branches.iter().map(|info| info.id).collect();
        ids.sort_unstable();
        assert_eq!(ids, ["a", "b"]);
    }

    // -------------------------------------------------------------------------
    // entries_to_agent_messages
    // -------------------------------------------------------------------------

    #[test]
    fn entries_to_agent_messages_round_trips_a_message_entry() {
        use pirust_ai::types::{Message, UserMessage, UserRole};

        let env = SessionEnv::from_process_env();
        let mut manager = env.in_memory(None, None).expect("in-memory session");
        let original = AgentMessage::Llm(Message::User(UserMessage {
            role: UserRole::User,
            content: UserMessageContent::Text("hello".to_string()),
            timestamp: 1_700_000_000_000,
        }));
        let id = manager.append_message(&original).expect("append_message");
        let entry = manager.get_entry(&id).expect("entry was just appended");

        let messages = entries_to_agent_messages(&[entry]);
        assert_eq!(messages, vec![original]);
    }

    #[test]
    fn entries_to_agent_messages_converts_branch_summary_entries() {
        let entry = serde_json::json!({
            "type": "branch_summary",
            "id": "b1",
            "parentId": null,
            "timestamp": "2024-01-01T00:00:00.000Z",
            "fromId": "a1",
            "summary": "did stuff"
        });
        let messages = entries_to_agent_messages(&[&entry]);
        assert_eq!(messages.len(), 1);
        match &messages[0] {
            AgentMessage::BranchSummary(bs) => {
                assert_eq!(bs.summary, "did stuff");
                assert_eq!(bs.from_id, "a1");
                assert_eq!(bs.timestamp, 1_704_067_200_000);
            }
            other => panic!("expected BranchSummary, got {other:?}"),
        }
    }

    #[test]
    fn entries_to_agent_messages_drops_an_empty_branch_summary() {
        // Matches agent-core's own `session_entry_to_context_messages` guard.
        let entry = serde_json::json!({
            "type": "branch_summary",
            "id": "b1",
            "parentId": null,
            "timestamp": "t",
            "fromId": "a1",
            "summary": ""
        });
        assert!(entries_to_agent_messages(&[&entry]).is_empty());
    }

    #[test]
    fn entries_to_agent_messages_converts_compaction_entries() {
        let entry = serde_json::json!({
            "type": "compaction",
            "id": "c1",
            "parentId": "p0",
            "timestamp": "2024-01-01T00:00:00.000Z",
            "summary": "compacted history",
            "firstKeptEntryId": "k1",
            "tokensBefore": 4321
        });
        let messages = entries_to_agent_messages(&[&entry]);
        assert_eq!(messages.len(), 1);
        match &messages[0] {
            AgentMessage::CompactionSummary(cs) => {
                assert_eq!(cs.summary, "compacted history");
                assert_eq!(cs.tokens_before, 4321);
                assert_eq!(cs.timestamp, 1_704_067_200_000);
            }
            other => panic!("expected CompactionSummary, got {other:?}"),
        }
    }

    #[test]
    fn entries_to_agent_messages_converts_custom_message_entries() {
        let entry = serde_json::json!({
            "type": "custom_message",
            "id": "cm1",
            "parentId": null,
            "timestamp": "2024-01-01T00:00:00.000Z",
            "customType": "note",
            "content": "hi there",
            "display": true
        });
        let messages = entries_to_agent_messages(&[&entry]);
        assert_eq!(messages.len(), 1);
        match &messages[0] {
            AgentMessage::Custom(c) => {
                assert_eq!(c.custom_type, "note");
                assert_eq!(c.content, UserMessageContent::Text("hi there".to_string()));
                assert!(c.display);
                assert_eq!(c.timestamp, 1_704_067_200_000);
            }
            other => panic!("expected Custom, got {other:?}"),
        }
    }

    #[test]
    fn entries_to_agent_messages_skips_non_projected_types() {
        // A bare "custom" entry (customType/data, no content/display) is deliberately NOT
        // routed to create_custom_message — see the doc comment's "Correction" section.
        let header = serde_json::json!({"type":"session","id":"h","timestamp":"t"});
        let custom = serde_json::json!({
            "type": "custom", "id": "x1", "parentId": null, "timestamp": "t",
            "customType": "note", "data": {"a": 1}
        });
        let label = serde_json::json!({
            "type": "label", "id": "x2", "parentId": null, "timestamp": "t",
            "targetId": "x1", "label": "checkpoint"
        });
        let thinking = serde_json::json!({
            "type": "thinking_level_change", "id": "x3", "parentId": null, "timestamp": "t",
            "thinkingLevel": "high"
        });
        let model = serde_json::json!({
            "type": "model_change", "id": "x4", "parentId": null, "timestamp": "t",
            "provider": "anthropic", "modelId": "claude"
        });
        let info = serde_json::json!({
            "type": "session_info", "id": "x5", "parentId": null, "timestamp": "t", "name": "n"
        });
        let entries: Vec<&FileEntry> = vec![&header, &custom, &label, &thinking, &model, &info];
        assert!(entries_to_agent_messages(&entries).is_empty());
    }

    #[test]
    fn entries_to_agent_messages_skips_a_malformed_message_entry_without_panicking() {
        // "message" payload has no recognizable role, so AgentMessage deserialization fails.
        let entry = serde_json::json!({
            "type": "message",
            "id": "m1",
            "parentId": null,
            "timestamp": "t",
            "message": {"nonsense": true}
        });
        assert!(entries_to_agent_messages(&[&entry]).is_empty());
    }

    #[test]
    fn entries_to_agent_messages_skips_a_compaction_entry_missing_required_fields() {
        let entry = serde_json::json!({
            "type": "compaction",
            "id": "c1",
            "parentId": null,
            "timestamp": "t",
            "summary": "no tokensBefore here"
        });
        assert!(entries_to_agent_messages(&[&entry]).is_empty());
    }

    // -------------------------------------------------------------------------
    // set_session_file — atomicity regression coverage
    // -------------------------------------------------------------------------

    /// An in-memory (`persist = false`) [`SessionEnv`] rooted in a fresh tempdir, so path
    /// resolution ("~", relative bits) has somewhere real to land without touching the
    /// actual home directory.
    fn atomicity_test_env(root: &std::path::Path) -> SessionEnv {
        SessionEnv::new(
            ConfigEnv {
                identity: crate::config::PIRUST,
                platform: Platform::current(),
                home_dir: Some(root.join("home").to_string_lossy().into_owned()),
                agent_dir_override: Some(root.join("agent").to_string_lossy().into_owned()),
            },
            root.join("project").to_string_lossy().into_owned(),
        )
    }

    /// Every field [`SessionManager::set_session_file`] can touch, read back through its
    /// public accessors, so a snapshot before/after a call can assert nothing moved.
    #[derive(Debug, PartialEq)]
    struct ManagerSnapshot {
        session_file: Option<String>,
        session_id: String,
        cwd: String,
        session_dir: String,
        entry_count: usize,
        leaf_id: Option<String>,
    }

    fn snapshot(manager: &SessionManager) -> ManagerSnapshot {
        ManagerSnapshot {
            session_file: manager.get_session_file().map(str::to_string),
            session_id: manager.get_session_id().to_string(),
            cwd: manager.get_cwd().to_string(),
            session_dir: manager.get_session_dir().to_string(),
            entry_count: manager.get_entries().len(),
            leaf_id: manager.get_leaf_id().map(str::to_string),
        }
    }

    /// Regression for the corruption bug: an existing-but-unparseable file must fail
    /// `set_session_file` WITHOUT leaving `session_file`/`file_entries` (and therefore the
    /// index built from them) reassigned to the rejected candidate.
    ///
    /// Before the fix, `self.session_file` was assigned the candidate path and
    /// `self.file_entries` was cleared before the `NotAPiSession` check ran, so this test
    /// fails on the pre-fix code: the post-call snapshot differs from the pre-call one
    /// (`session_file` becomes `Some(garbage path)` and `entry_count` drops to 0).
    #[test]
    fn set_session_file_leaves_the_manager_untouched_on_unparseable_content() {
        let root = tempfile::tempdir().expect("tempdir");
        let env = atomicity_test_env(root.path());
        let mut manager = env.in_memory(None, None).expect("in_memory session");

        let before = snapshot(&manager);
        // A fresh in-memory session has only its header entry — and `get_entries()` is
        // "every non-header entry" (see its doc comment), so `entry_count` starts at 0.
        // Since `persist = false`, there is also no session file yet. Assert both so the
        // test would actually notice a regression rather than vacuously comparing
        // `None == None`.
        assert_eq!(before.session_file, None);
        assert_eq!(before.entry_count, 0);

        let garbage_path = root.path().join("garbage.jsonl");
        fs::write(&garbage_path, b"not json\nstill not json\n").expect("write garbage file");

        let err = manager
            .set_session_file(&garbage_path.to_string_lossy())
            .expect_err("unparseable-but-existing content must be rejected");
        assert!(
            matches!(err, SessionError::NotAPiSession(_)),
            "unexpected error variant: {err:?}"
        );

        let after = snapshot(&manager);
        assert_eq!(
            after, before,
            "set_session_file must not mutate the manager when it returns Err"
        );
    }

    /// The success path is unchanged by the atomicity rework: switching to a valid,
    /// existing session file loads its entries and adopts its header id.
    #[test]
    fn set_session_file_success_path_loads_the_target_file() {
        let root = tempfile::tempdir().expect("tempdir");
        let env = atomicity_test_env(root.path());
        let mut manager = env.in_memory(None, None).expect("in_memory session");

        let valid_path = root.path().join("valid.jsonl");
        let header = serde_json::json!({
            "type": "session",
            "version": CURRENT_SESSION_VERSION,
            "id": "target-session",
            "timestamp": "2020-01-01T00:00:00.000Z",
            "cwd": env.process_cwd,
        });
        let message = serde_json::json!({
            "type": "message",
            "id": "m1",
            "parentId": null,
            "timestamp": "2020-01-01T00:00:01.000Z",
            "message": {"role": "user", "content": [{"type": "text", "text": "hi"}]}
        });
        let mut content = serde_json::to_string(&header).unwrap();
        content.push('\n');
        content.push_str(&serde_json::to_string(&message).unwrap());
        content.push('\n');
        fs::write(&valid_path, &content).expect("write valid session file");

        manager
            .set_session_file(&valid_path.to_string_lossy())
            .expect("a well-formed, existing session file must load");

        assert_eq!(manager.get_session_id(), "target-session");
        // `get_entries()` excludes the header, so only the one message entry counts.
        assert_eq!(manager.get_entries().len(), 1);
        let resolved = env.resolve_path(&valid_path.to_string_lossy()).unwrap();
        assert_eq!(manager.get_session_file(), Some(resolved.as_str()));
        assert_eq!(manager.get_leaf_id(), Some("m1"));
    }
}
