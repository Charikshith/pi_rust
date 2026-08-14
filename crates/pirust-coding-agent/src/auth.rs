//! Port of `core/auth-storage.ts` — the `auth.json` store (api keys + OAuth tokens).
//!
//! Written with mode `0600`. Gated by `tests/fixtures/pi/cli/auth.json.cases.jsonl`.
//!
//! # What is ported
//!
//! | Pi (`core/auth-storage.ts`) | here |
//! |---|---|
//! | `AuthStorageData` (`:14`) | [`AuthStorageData`] |
//! | `Credential` (`packages/ai/src/auth/types.ts:17-37`) | [`Credential`] |
//! | `CredentialInfo` (`…/types.ts:40-43`) | [`CredentialInfo`] |
//! | `AUTH_FILE_WRITE_OPTIONS` (`:21`) | [`AUTH_FILE_MODE`] + [`write_credential_file`] |
//! | `AuthStorageBackend` (`:23-26`) | [`AuthStorageBackend`], [`LockResult`] |
//! | `FileAuthStorageBackend` (`:28-146`) | [`FileAuthStorageBackend`] |
//! | `InMemoryAuthStorageBackend` (`:148-166`) | [`InMemoryAuthStorageBackend`] |
//! | `AuthStorage` (`:171-255`) | [`AuthStorage`] |
//! | `readStoredCredential` (`:261-271`) | [`read_stored_credential`] |
//! | `resolveConfigValue` template half (`core/resolve-config-value.ts:145-151`) | [`resolve_config_value`] |
//!
//! # The byte contract
//!
//! Every write is `JSON.stringify(data, null, 2)` (`:190,238,247`): **two-space indent, no
//! trailing newline**, `type` first inside each entry, and top-level provider order =
//! **first-write order**, because `{ ...currentData, [provider]: next }` (`:236`) updates an
//! existing key in place and appends a new one. [`serialize_storage_data`] is the single
//! writer, `serde_json::to_string_pretty` over an order-preserving map (the crate enables
//! `serde_json/preserve_order`). All four properties are pinned by
//! `tests/auth_golden.rs` against the captured bytes, and each is caught by a mutation.
//!
//! The store therefore holds Pi's *parsed value* — [`AuthStorageData`] is
//! `serde_json::Map<String, Value>`, exactly `Record<string, Credential>` — and not a map
//! of typed [`Credential`]s. That is deliberate: `modify` rewrites the whole file, so any
//! entry the caller did **not** touch must survive with its own key order and unknown
//! fields intact, which is what JS gets for free from `JSON.parse` → spread →
//! `JSON.stringify`. [`Credential`] is the typed *view* used at the API boundary
//! (`read`/`modify`/`list`), and it round-trips unknown fields via `#[serde(flatten)]` —
//! required by `OAuthCredentials`' `[key: string]: unknown` index signature
//! (`…/types.ts:24-29`) and applied to `ApiKeyCredential` too so the boundary is lossless
//! in both directions.
//!
//! # Composition with [`pirust_ai::auth`]
//!
//! The two modules are the two halves of one story and neither duplicates the other:
//!
//! - **This module owns the on-disk store.** What `auth.json` looks like, how it is locked,
//!   merged, and re-serialized. It knows nothing about HTTP headers.
//! - **[`pirust_ai::auth`] owns "credential → request auth".** The
//!   `ANTHROPIC_OAUTH_TOKEN` → `ANTHROPIC_API_KEY` env precedence
//!   ([`pirust_ai::auth::resolve_api_key`]) and the `sk-ant-oat` → `Authorization: Bearer`
//!   vs. `X-Api-Key` decision ([`pirust_ai::auth::is_oauth_token`]).
//!
//! The seam is [`credential_api_key`]: it turns a stored [`Credential`] into
//! `resolve_api_key`'s `explicit` argument, which is precisely `ApiKeyAuth.resolve`'s
//! documented per-field merge `credential.key ?? env("ANTHROPIC_API_KEY")`
//! (`…/types.ts:175-181`). No env precedence and no header rule is re-implemented here.
//!
//! # Intentional narrowings (all documented, none silent)
//!
//! - **`!command` config values are not executed.** `resolveConfigValue`
//!   (`resolve-config-value.ts:145-151`) shells out for a value starting with `!`, with a
//!   process-lifetime cache. Spec §8.2 sets the feat-005 minimum at literals plus
//!   `$VAR`/`${VAR}`, so [`resolve_config_value`] returns `None` for a command value and
//!   [`is_command_config_value`] lets a caller tell "unsupported shell command" apart from
//!   "unset environment variable" and print a diagnostic. The escape/interpolation grammar
//!   itself (`$$`, `$!`, `${NAME}` validation, prefix matching) is ported verbatim from
//!   `parseConfigValueTemplate` (`:28-78`) and is pinned by fixture record 8.
//! - **One lock path, not two.** Pi has a sync `withLock` (10 × 20 ms `ELOCKED` retry,
//!   `:49-74`) and an async `withLockAsync` (`retries:10, factor:2, minTimeout:100,
//!   maxTimeout:10000, randomize:true`, `stale:30000`, `onCompromised`, `:111-124`).
//!   [`FileAuthStorageBackend`] implements the sync schedule for both, breaks a lock older
//!   than [`LOCK_STALE`], and has no `onCompromised` hook. Timing, not bytes; a
//!   `with_lock_async` twin can be added when an OAuth refresh flow needs it (nothing in
//!   [`pirust_ai::auth`] refreshes today).
//! - **`async` collapses to sync.** `read`/`modify`/`delete`/`list` are `async` in Pi only
//!   because `CredentialStore` (`…/types.ts:60-88`) is an async interface; the bodies
//!   `await` nothing but the caller's callback. They are plain fns here. A refresh callback
//!   that must do I/O is the reason to introduce the async twin above.
//! - **Malformed entries read as absent.** `read`/`list`/[`read_stored_credential`] return
//!   `None` for an entry that is not a valid [`Credential`] (unknown `type`, missing
//!   `refresh`, …) where JS would hand back the raw object. [`AuthStorage::raw`] exposes the
//!   unparsed value for anything that needs Pi's exact tolerance, and such an entry is
//!   still preserved verbatim on rewrite, which is the property that matters for bytes.
//! - **Non-UTF-8 `auth.json` is an error**, where `readFileSync(…, "utf-8")` would produce
//!   U+FFFD. It lands in `reload`'s swallowed-error path (`:212`), so the effect is the same
//!   "keep the last valid snapshot".
//!
//! # Not ported
//!
//! `FileModelsStore` (`core/models-store.ts:25-57`) reuses `FileAuthStorageBackend` for
//! `models-store.json`, which is why that file is also `0600`. It belongs to `models.rs`;
//! [`FileAuthStorageBackend`] is deliberately path-agnostic so it can be reused there
//! rather than copied.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::config::{ConfigEnv, ConfigPathError};

// =============================================================================
// Constants (`auth-storage.ts:21,38,50-51`, `:119`)
// =============================================================================

/// `AUTH_FILE_WRITE_OPTIONS.mode` (`:21`), re-applied by `chmodSync` after every write
/// (`:45,87,132`).
///
/// **Unix only.** Windows cannot express POSIX permissions — `fs.writeFileSync`'s `mode`
/// is ignored there and `chmodSync` only toggles the read-only attribute — which is why
/// every record of `auth.json.cases.jsonl` reports `"mode":"0666"` with
/// `"modeMeaningful":false` and `"platform":"win32"`. The fixture cannot pin this value;
/// its authority is `auth-storage.ts:21`. **The fixture should be re-derived on Linux or
/// macOS to pin the mode properly**; until then `tests/auth_golden.rs` asserts `0600` on
/// unix and skips the assertion on windows.
pub const AUTH_FILE_MODE: u32 = 0o600;

/// `mkdirSync(dir, { recursive: true, mode: 0o700 })` (`:38`) — unix only, as above.
pub const AUTH_DIR_MODE: u32 = 0o700;

/// The two bytes `ensureFileExists` seeds a fresh `auth.json` with (`:44`).
pub const EMPTY_AUTH_JSON: &str = "{}";

/// `maxAttempts` of the sync `ELOCKED` retry loop (`:50`).
pub const LOCK_MAX_ATTEMPTS: u32 = 10;

/// `delayMs` of the sync `ELOCKED` retry loop (`:51`). Pi busy-waits (`:66-69`); this
/// sleeps, which is the same 20 ms of back-off without burning a core.
pub const LOCK_RETRY_DELAY: Duration = Duration::from_millis(20);

/// `stale: 30000` from the async lock config (`:119`) — a lock directory older than this is
/// assumed to belong to a crashed process and is broken. Pi's *sync* path leaves
/// `proper-lockfile`'s own 10 s default in force; collapsing the two paths (see module
/// docs) means one value, and the more conservative one is the safer choice for a file
/// whose absence blocks login.
pub const LOCK_STALE: Duration = Duration::from_millis(30_000);

/// Value of the `type` tag for [`Credential::ApiKey`] (`…/types.ts:18`).
pub const API_KEY_TYPE: &str = "api_key";
/// Value of the `type` tag for [`Credential::OAuth`] (`…/types.ts:33`).
pub const OAUTH_TYPE: &str = "oauth";

// =============================================================================
// Schema (`packages/ai/src/auth/types.ts:17-43`, `auth-storage.ts:14`)
// =============================================================================

/// `type AuthStorageData = Record<string, Credential>` (`:14`), keyed by `Provider.id`.
///
/// A `serde_json::Map`, i.e. an `IndexMap` under `serde_json/preserve_order`, so top-level
/// provider order is insertion order exactly as a JS object's is.
pub type AuthStorageData = Map<String, Value>;

/// `ProviderEnv = Record<string, string>` (`packages/ai/src/types.ts:104`) — provider-scoped
/// environment/config values stored alongside an api key (Cloudflare account/gateway ids
/// and friends).
///
/// Held as a `serde_json::Map` rather than a `BTreeMap<String, String>` for two reasons:
/// key order survives a rewrite (a `BTreeMap` would silently alphabetize a hand-written
/// file), and a non-string value written by some other tool round-trips instead of failing
/// to parse. [`resolve_config_value`] reads values through `Value::as_str`, so a non-string
/// counts as unset — which is what JS's `env?.[name] || …` does for anything falsy.
pub type ProviderEnv = Map<String, Value>;

/// `type Credential = ApiKeyCredential | OAuthCredential` (`…/types.ts:37`) — one
/// type-tagged credential per provider, internally tagged on `type`.
///
/// Serialization order is Pi's object-literal order: the `type` tag first, then the
/// declared fields, then anything unknown. Fixture records 2 and 3 pin both variants
/// byte-for-byte.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Credential {
    /// `ApiKeyCredential` (`…/types.ts:17-21`). `key` is optional — an entry may exist to
    /// carry `env` alone — and may be a `$VAR` reference resolved by
    /// [`resolve_config_value`].
    #[serde(rename = "api_key")]
    ApiKey {
        /// `key?: string`. Absent (not `null`) when unset, as `JSON.stringify` drops
        /// `undefined`; also how [`AuthStorage::read`] reports an unresolvable `$VAR`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<String>,
        /// `env?: ProviderEnv`, and the first place [`resolve_config_value`] looks.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        env: Option<ProviderEnv>,
        /// Fields no version of Pi declares. `ApiKeyCredential` has no index signature, but
        /// `JSON.parse` → spread → `JSON.stringify` preserves them regardless, so the typed
        /// view must too.
        #[serde(flatten)]
        extra: Map<String, Value>,
    },
    /// `OAuthCredential extends OAuthCredentials` (`…/types.ts:24-34`) — stored **flat**,
    /// not nested under a `tokens` key (fixture record 3).
    #[serde(rename = "oauth")]
    OAuth {
        /// `refresh: string` — required.
        refresh: String,
        /// `access: string` — required.
        access: String,
        /// `expires: number` — epoch milliseconds. `i64`, so the captured
        /// `1730000000000` re-serializes as an integer rather than `1.73e12`.
        expires: i64,
        /// `[key: string]: unknown` (`…/types.ts:28`) — extra token fields from extension
        /// compatibility flows. **Must** round-trip: `modify` rewrites the whole file.
        #[serde(flatten)]
        extra: Map<String, Value>,
    },
}

impl Credential {
    /// An `ApiKeyCredential` with just a key — `{ type: "api_key", key }`.
    pub fn api_key(key: impl Into<String>) -> Self {
        Self::ApiKey {
            key: Some(key.into()),
            env: None,
            extra: Map::new(),
        }
    }

    /// An `ApiKeyCredential` carrying provider-scoped `env` values as well.
    pub fn api_key_with_env(key: impl Into<String>, env: ProviderEnv) -> Self {
        Self::ApiKey {
            key: Some(key.into()),
            env: Some(env),
            extra: Map::new(),
        }
    }

    /// An `OAuthCredential` — `{ type: "oauth", refresh, access, expires }`, in that order.
    pub fn oauth(refresh: impl Into<String>, access: impl Into<String>, expires: i64) -> Self {
        Self::OAuth {
            refresh: refresh.into(),
            access: access.into(),
            expires,
            extra: Map::new(),
        }
    }

    /// The `type` tag as it appears on disk — `Credential["type"]` (`…/types.ts:42`).
    pub fn credential_type(&self) -> &'static str {
        match self {
            Self::ApiKey { .. } => API_KEY_TYPE,
            Self::OAuth { .. } => OAUTH_TYPE,
        }
    }
}

/// `CredentialInfo` (`…/types.ts:40-43`) — non-secret metadata from
/// [`AuthStorage::list`].
///
/// `credential_type` is `Option<String>`, not an enum: `list` reads `credential.type`
/// (`:253`) off whatever JSON is on disk, so an unknown tag passes through verbatim and a
/// missing one yields `{ "providerId": … }` with no `type` key, exactly as
/// `JSON.stringify` renders `undefined`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialInfo {
    /// The `Provider.id` this credential is stored under.
    #[serde(rename = "providerId")]
    pub provider_id: String,
    /// `credential.type` — `"api_key"` or `"oauth"` for anything Pi wrote.
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub credential_type: Option<String>,
}

// =============================================================================
// Errors
// =============================================================================

/// Why a store operation failed. `reload` swallows every one of these (`:212`); `modify`
/// and `delete` propagate them, matching Pi, where those two reject.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    /// A filesystem call failed — `mkdirSync`/`readFileSync`/`writeFileSync`/`chmodSync`.
    #[error("auth storage I/O at {path}: {source}")]
    Io {
        /// The path being operated on.
        path: String,
        /// The underlying `std::io` error.
        source: io::Error,
    },
    /// `JSON.parse` threw (`:198`), or the value could not be serialized.
    #[error("auth storage JSON at {path}: {source}")]
    Json {
        /// The path being operated on, or `"<memory>"` for the in-memory backend.
        path: String,
        /// The underlying serde error.
        source: serde_json::Error,
    },
    /// `auth.json` parsed but is not an object, so `currentData[provider]` would throw a
    /// `TypeError` in JS. Reported instead of panicking; `reload` still swallows it.
    #[error("auth storage at {path} must contain a JSON object, found {found}")]
    NotAnObject {
        /// The path being operated on.
        path: String,
        /// The JSON kind actually found (`"array"`, `"null"`, `"number"`, …).
        found: &'static str,
    },
    /// The lock could not be taken — `proper-lockfile`'s `ELOCKED` after every retry
    /// (`:62,73`).
    #[error("could not acquire the auth storage lock at {path} after {attempts} attempts")]
    Locked {
        /// The lock directory (`<auth.json>.lock`).
        path: String,
        /// How many attempts were made ([`LOCK_MAX_ATTEMPTS`]).
        attempts: u32,
    },
    /// The `auth.json` path itself could not be composed (no home directory, bad
    /// `file://` override). From [`crate::config`]; Pi throws from `getAgentDir()`.
    #[error(transparent)]
    ConfigPath(#[from] ConfigPathError),
}

impl AuthError {
    fn io(path: &Path, source: io::Error) -> Self {
        Self::Io {
            path: path.display().to_string(),
            source,
        }
    }

    fn json(path: &str, source: serde_json::Error) -> Self {
        Self::Json {
            path: path.to_string(),
            source,
        }
    }
}

// =============================================================================
// Serialization — the single writer (`:190,238,247`)
// =============================================================================

/// `JSON.stringify(data, null, 2)` — **the** on-disk form of `auth.json`.
///
/// `to_string_pretty` is byte-identical to Node here: two-space indent, `": "` between key
/// and value, `{}` for an empty object, and **no trailing newline**. Provider order is the
/// map's own (`preserve_order`), and `expires` stays an integer (`float_roundtrip` plus
/// `i64`). The one divergence Node has no equivalent for — `JSON.stringify` emits lone
/// surrogates as-is while Rust cannot hold them in a `String` — is unreachable through
/// this API.
pub fn serialize_storage_data(data: &AuthStorageData) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(data)
}

/// `parseStorageData` (`:194-199`): empty/absent content is `{}`, otherwise `JSON.parse`.
///
/// `path` only labels errors. A non-object document is [`AuthError::NotAnObject`] rather
/// than the `TypeError` JS would raise on first property access.
fn parse_storage_data(path: &str, content: Option<&str>) -> Result<AuthStorageData, AuthError> {
    // `if (!content) return {}` — `undefined` *and* `""` are falsy in JS.
    let Some(content) = content.filter(|c| !c.is_empty()) else {
        return Ok(AuthStorageData::new());
    };
    let value: Value = serde_json::from_str(content).map_err(|e| AuthError::json(path, e))?;
    match value {
        Value::Object(map) => Ok(map),
        other => Err(AuthError::NotAnObject {
            path: path.to_string(),
            found: match other {
                Value::Null => "null",
                Value::Bool(_) => "boolean",
                Value::Number(_) => "number",
                Value::String(_) => "string",
                Value::Array(_) => "array",
                Value::Object(_) => unreachable!("matched above"),
            },
        }),
    }
}

/// `writeFileSync(path, next, { encoding: "utf-8", mode: 0o600 })` followed by
/// `chmodSync(path, 0o600)` (`:86-87,131-132`).
///
/// The `chmod` is not redundant: `OpenOptions::mode` (like Node's `mode`) applies only when
/// the file is *created*, so an `auth.json` that already exists with looser permissions is
/// tightened by the second call. On non-unix targets both are no-ops — see
/// [`AUTH_FILE_MODE`].
pub fn write_credential_file(path: &Path, contents: &str) -> Result<(), AuthError> {
    #[cfg(unix)]
    {
        use std::io::Write as _;
        use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};

        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(AUTH_FILE_MODE)
            .open(path)
            .map_err(|e| AuthError::io(path, e))?;
        file.write_all(contents.as_bytes())
            .map_err(|e| AuthError::io(path, e))?;
        fs::set_permissions(path, fs::Permissions::from_mode(AUTH_FILE_MODE))
            .map_err(|e| AuthError::io(path, e))?;
    }
    #[cfg(not(unix))]
    {
        fs::write(path, contents).map_err(|e| AuthError::io(path, e))?;
    }
    Ok(())
}

// =============================================================================
// Backends (`:16-166`)
// =============================================================================

/// `LockResult<T>` (`:16-19`) — what a locked section hands back: a value for the caller
/// and, optionally, the new file contents to persist.
///
/// `next: None` means **no write at all** (`if (next !== undefined)`, `:85`), which is how
/// `reload` reads without touching the file and how a `modify` callback that returns
/// `undefined` leaves it alone (fixture record 6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockResult<T> {
    /// Returned to the caller of `with_lock`.
    pub result: T,
    /// The exact bytes to write, or `None` to skip the write.
    pub next: Option<String>,
}

/// `AuthStorageBackend` (`:23-26`) — the read-modify-write-under-lock seam that lets
/// [`AuthStorage`] be tested without a filesystem.
///
/// Generic in the callback's return type rather than object-safe, so [`AuthStorage`] is
/// generic over the backend and everything is statically dispatched. Pi's `withLock` /
/// `withLockAsync` pair collapses to this one method (see the module docs).
pub trait AuthStorageBackend {
    /// Take the lock, hand the current contents to `f` (`None` when the file does not
    /// exist), persist `f`'s `next` if it produced one, and always release the lock —
    /// Pi's `finally` (`:90-94`).
    fn with_lock<T, F>(&self, f: F) -> Result<T, AuthError>
    where
        F: FnOnce(Option<&str>) -> Result<LockResult<T>, AuthError>;
}

/// `FileAuthStorageBackend` (`:28-146`) — `auth.json` on disk, guarded by a lock
/// directory.
#[derive(Debug, Clone)]
pub struct FileAuthStorageBackend {
    auth_path: PathBuf,
}

impl FileAuthStorageBackend {
    /// The backend for an **already-resolved** path.
    ///
    /// Pi applies `normalizePath(authPath)` in the constructor (`:32`); here that is the
    /// caller's job, via [`ConfigEnv::auth_path`] (which is composed from an already
    /// tilde-expanded `getAgentDir()`) or [`ConfigEnv::expand_tilde_path`] for a
    /// user-supplied override. [`AuthStorage::create`] and [`AuthStorage::create_at`] do
    /// exactly that, so no path logic is re-derived in this module.
    pub fn new(auth_path: impl Into<PathBuf>) -> Self {
        Self {
            auth_path: auth_path.into(),
        }
    }

    /// The file this backend reads and writes.
    pub fn auth_path(&self) -> &Path {
        &self.auth_path
    }

    /// `<auth.json>.lock` — the directory `proper-lockfile` creates via `mkdir`, kept
    /// name-compatible so a Pi and a pirust process pointed at the same file would still
    /// exclude each other.
    pub fn lock_path(&self) -> PathBuf {
        let mut name = self.auth_path.as_os_str().to_os_string();
        name.push(".lock");
        PathBuf::from(name)
    }

    /// `ensureParentDir` (`:35-40`) — `mkdirSync(dir, { recursive: true, mode: 0o700 })`.
    fn ensure_parent_dir(&self) -> Result<(), AuthError> {
        let Some(dir) = self
            .auth_path
            .parent()
            .filter(|d| !d.as_os_str().is_empty())
        else {
            return Ok(());
        };
        if dir.exists() {
            return Ok(());
        }
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt as _;
            builder.mode(AUTH_DIR_MODE);
        }
        builder.create(dir).map_err(|e| AuthError::io(dir, e))
    }

    /// `ensureFileExists` (`:42-47`) — seed the literal two bytes `{}` at mode `0600`.
    fn ensure_file_exists(&self) -> Result<(), AuthError> {
        if self.auth_path.exists() {
            return Ok(());
        }
        write_credential_file(&self.auth_path, EMPTY_AUTH_JSON)
    }

    /// `acquireLockSyncWithRetry` (`:49-74`) — [`LOCK_MAX_ATTEMPTS`] × [`LOCK_RETRY_DELAY`],
    /// retrying only the `ELOCKED` case (here: `AlreadyExists`) and rethrowing anything
    /// else immediately.
    fn acquire_lock(&self) -> Result<LockGuard, AuthError> {
        let lock_path = self.lock_path();
        for attempt in 1..=LOCK_MAX_ATTEMPTS {
            match fs::create_dir(&lock_path) {
                Ok(()) => return Ok(LockGuard { path: lock_path }),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    // `stale: 30000` (`:119`): a lock this old outlived its owner.
                    if lock_is_stale(&lock_path) && fs::remove_dir(&lock_path).is_ok() {
                        continue;
                    }
                    if attempt == LOCK_MAX_ATTEMPTS {
                        return Err(AuthError::Locked {
                            path: lock_path.display().to_string(),
                            attempts: LOCK_MAX_ATTEMPTS,
                        });
                    }
                    std::thread::sleep(LOCK_RETRY_DELAY);
                }
                Err(e) => return Err(AuthError::io(&lock_path, e)),
            }
        }
        Err(AuthError::Locked {
            path: lock_path.display().to_string(),
            attempts: LOCK_MAX_ATTEMPTS,
        })
    }
}

impl AuthStorageBackend for FileAuthStorageBackend {
    fn with_lock<T, F>(&self, f: F) -> Result<T, AuthError>
    where
        F: FnOnce(Option<&str>) -> Result<LockResult<T>, AuthError>,
    {
        // `:77-78` — both run before *every* lock acquisition, so merely constructing an
        // AuthStorage materialises `<agent-dir>/auth.json` (fixture record 1).
        self.ensure_parent_dir()?;
        self.ensure_file_exists()?;

        // `_guard`'s Drop is Pi's `finally { release() }` (`:90-94`), so an error or a
        // panic inside `f` still unlocks.
        let _guard = self.acquire_lock()?;
        let current = if self.auth_path.exists() {
            Some(
                fs::read_to_string(&self.auth_path)
                    .map_err(|e| AuthError::io(&self.auth_path, e))?,
            )
        } else {
            None
        };
        let LockResult { result, next } = f(current.as_deref())?;
        if let Some(next) = next {
            write_credential_file(&self.auth_path, &next)?;
        }
        Ok(result)
    }
}

/// Releases the lock directory on drop.
struct LockGuard {
    path: PathBuf,
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        // `release()` failures are ignored in Pi too (`:140-142`).
        let _ = fs::remove_dir(&self.path);
    }
}

/// Has this lock directory outlived [`LOCK_STALE`]? Unreadable metadata counts as fresh —
/// never break a lock on a guess.
fn lock_is_stale(lock_path: &Path) -> bool {
    fs::metadata(lock_path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .is_some_and(|age| age > LOCK_STALE)
}

/// `InMemoryAuthStorageBackend` (`:148-166`) — the same serialization with no I/O.
///
/// A `Mutex` rather than a plain field because `with_lock` takes `&self` (Pi's backend is a
/// shared object); it also keeps the backend `Send + Sync`.
#[derive(Debug, Default)]
pub struct InMemoryAuthStorageBackend {
    value: Mutex<Option<String>>,
}

impl InMemoryAuthStorageBackend {
    /// An empty backend — `value` starts `undefined` (`:149`).
    pub fn new() -> Self {
        Self::default()
    }

    /// The bytes currently held, i.e. what the file backend would have on disk. Exposed so
    /// `tests/auth_golden.rs` can compare the two serializations (fixture record 11).
    pub fn snapshot(&self) -> Option<String> {
        self.value.lock().expect("auth storage mutex").clone()
    }
}

impl AuthStorageBackend for InMemoryAuthStorageBackend {
    fn with_lock<T, F>(&self, f: F) -> Result<T, AuthError>
    where
        F: FnOnce(Option<&str>) -> Result<LockResult<T>, AuthError>,
    {
        let mut guard = self.value.lock().expect("auth storage mutex");
        let LockResult { result, next } = f(guard.as_deref())?;
        if let Some(next) = next {
            *guard = Some(next);
        }
        Ok(result)
    }
}

// =============================================================================
// process.env seam (for `resolveConfigValue`)
// =============================================================================

/// The `process.env` half of `resolveEnvConfigValue` (`resolve-config-value.ts:88-90`),
/// as a value.
///
/// Snapshot once with [`ProcessEnv::from_process_env`]; tests build a literal, so they
/// never call `std::env::set_var` (process-global, and would race `cargo test`'s threads) —
/// the same discipline [`crate::config::ConfigEnv`] follows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProcessEnv(BTreeMap<String, String>);

impl ProcessEnv {
    /// Snapshot the real environment.
    pub fn from_process_env() -> Self {
        Self(std::env::vars().collect())
    }

    /// Build one from literals — `ProcessEnv::from_pairs([("ANTHROPIC_API_KEY", "sk-…")])`.
    pub fn from_pairs<K, V, I>(pairs: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        Self(
            pairs
                .into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect(),
        )
    }

    /// `process.env[name]`.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0.get(name).map(String::as_str)
    }
}

// =============================================================================
// resolveConfigValue — template half (`core/resolve-config-value.ts:11-113,145-151`)
// =============================================================================

/// One piece of a parsed config-value template (`resolve-config-value.ts:14`).
#[derive(Debug, Clone, PartialEq, Eq)]
enum TemplatePart {
    Literal(String),
    Env(String),
}

/// `appendLiteral` (`:18-26`) — empty strings are dropped and adjacent literals coalesce.
fn append_literal(parts: &mut Vec<TemplatePart>, value: &str) {
    if value.is_empty() {
        return;
    }
    if let Some(TemplatePart::Literal(previous)) = parts.last_mut() {
        previous.push_str(value);
        return;
    }
    parts.push(TemplatePart::Literal(value.to_string()));
}

/// `ENV_VAR_NAME_RE = /^[A-Za-z_][A-Za-z0-9_]*$/` (`:11`).
fn is_env_var_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// `ENV_VAR_NAME_PREFIX_RE = /^[A-Za-z_][A-Za-z0-9_]*/` (`:12`) — the matched length, or 0.
fn env_var_name_prefix_len(rest: &str) -> usize {
    let bytes = rest.as_bytes();
    match bytes.first() {
        Some(&b) if b.is_ascii_alphabetic() || b == b'_' => {}
        _ => return 0,
    }
    bytes
        .iter()
        .take_while(|&&b| b.is_ascii_alphanumeric() || b == b'_')
        .count()
}

/// `parseConfigValueTemplate` (`:28-78`), transcribed branch for branch.
///
/// Every delimiter (`$`, `{`, `}`) and every name character is ASCII, so scanning bytes is
/// equivalent to JS scanning UTF-16 units and every slice lands on a char boundary.
fn parse_config_value_template(config: &str) -> Vec<TemplatePart> {
    let mut parts: Vec<TemplatePart> = Vec::new();
    let mut index = 0usize;

    while index < config.len() {
        let Some(offset) = config[index..].find('$') else {
            append_literal(&mut parts, &config[index..]);
            break;
        };
        let dollar = index + offset;
        append_literal(&mut parts, &config[index..dollar]);
        let next_char = config[dollar + 1..].chars().next();

        // `$$` → literal `$`, `$!` → literal `!` (`:42-46`).
        if let Some(c @ ('$' | '!')) = next_char {
            append_literal(&mut parts, &c.to_string());
            index = dollar + 2;
            continue;
        }

        // `${NAME}` (`:48-64`).
        if next_char == Some('{') {
            let Some(close_offset) = config[dollar + 2..].find('}') else {
                append_literal(&mut parts, "$");
                index = dollar + 1;
                continue;
            };
            let end = dollar + 2 + close_offset;
            let name = &config[dollar + 2..end];
            if is_env_var_name(name) {
                parts.push(TemplatePart::Env(name.to_string()));
            } else {
                // Not a valid name: the whole `${…}` stays literal (`:60`).
                append_literal(&mut parts, &config[dollar..end + 1]);
            }
            index = end + 1;
            continue;
        }

        // `$NAME` (`:66-71`).
        let matched = env_var_name_prefix_len(&config[dollar + 1..]);
        if matched > 0 {
            parts.push(TemplatePart::Env(
                config[dollar + 1..dollar + 1 + matched].to_string(),
            ));
            index = dollar + 1 + matched;
            continue;
        }

        // A bare `$` (`:73-74`).
        append_literal(&mut parts, "$");
        index = dollar + 1;
    }

    parts
}

/// `resolveEnvConfigValue` (`:88-90`) — `env?.[name] || process.env[name] || undefined`.
///
/// `||`, not `??`: an **empty** value in either map falls through, and a non-string
/// `env[name]` is falsy too (hence `as_str`).
fn resolve_env_config_value(
    name: &str,
    env: Option<&ProviderEnv>,
    process_env: &ProcessEnv,
) -> Option<String> {
    env.and_then(|e| e.get(name))
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
        .or_else(|| process_env.get(name).filter(|v| !v.is_empty()))
        .map(str::to_string)
}

/// `isCommandConfigValue` (`:130-132`) — a value starting with `!` is a shell command.
///
/// Not executed by this port (see the module docs); callers use this to tell an
/// unsupported command apart from an unset variable when [`resolve_config_value`] returns
/// `None`, and to say so in a diagnostic.
pub fn is_command_config_value(config: &str) -> bool {
    config.starts_with('!')
}

/// `resolveConfigValue(config, env)` (`:145-151`), template half only.
///
/// - literal → itself, with `$$` → `$` and `$!` → `!`
/// - `$VAR` / `${VAR}` → [`resolve_env_config_value`], **all** of which must resolve or the
///   whole value is `None` (`:109`) — the file keeps `$VAR`, the resolved credential simply
///   has no key (fixture record 8, `envmissing`)
/// - `!command` → `None` here, where Pi executes it (see the module docs)
pub fn resolve_config_value(
    config: &str,
    env: Option<&ProviderEnv>,
    process_env: &ProcessEnv,
) -> Option<String> {
    if is_command_config_value(config) {
        return None;
    }
    let mut resolved = String::new();
    for part in parse_config_value_template(config) {
        match part {
            TemplatePart::Literal(value) => resolved.push_str(&value),
            TemplatePart::Env(name) => {
                resolved.push_str(&resolve_env_config_value(&name, env, process_env)?)
            }
        }
    }
    Some(resolved)
}

// =============================================================================
// AuthStorage (`:171-255`)
// =============================================================================

/// `AuthStorage implements CredentialStore` (`:171-255`) — credential storage backed by
/// `auth.json`.
///
/// Holds the last successfully parsed snapshot in `data` (`:172`), which is what `read` and
/// `list` serve; `modify` and `delete` re-read the file under the lock first, so a
/// concurrent writer is never clobbered.
#[derive(Debug)]
pub struct AuthStorage<B: AuthStorageBackend> {
    data: AuthStorageData,
    storage: B,
    process_env: ProcessEnv,
}

impl AuthStorage<FileAuthStorageBackend> {
    /// `AuthStorage.create()` (`:180-182`) — `join(getAgentDir(), "auth.json")` from
    /// [`ConfigEnv::auth_path`].
    ///
    /// Constructing this **creates** the agent directory (`0700`) and an `auth.json`
    /// containing `{}` (`0600`) if they are absent, because `reload()` runs in the
    /// constructor (`:177`) and every lock acquisition ensures both (fixture record 1).
    pub fn create(env: &ConfigEnv) -> Result<Self, AuthError> {
        Ok(Self::from_backend(FileAuthStorageBackend::new(
            env.auth_path()?,
        )))
    }

    /// `AuthStorage.create(authPath)` with an explicit path (`:180-182`), `normalizePath`'d
    /// through [`ConfigEnv::expand_tilde_path`] as Pi's constructor does (`:32`).
    pub fn create_at(env: &ConfigEnv, auth_path: &str) -> Result<Self, AuthError> {
        Ok(Self::from_backend(FileAuthStorageBackend::new(
            env.expand_tilde_path(auth_path)?,
        )))
    }
}

impl AuthStorage<InMemoryAuthStorageBackend> {
    /// `AuthStorage.inMemory(data)` (`:188-192`) — seeds the backend with
    /// `JSON.stringify(data, null, 2)`, *the identical serialization the file backend
    /// writes*, then reloads through it.
    pub fn in_memory(data: &AuthStorageData) -> Result<Self, serde_json::Error> {
        let backend = InMemoryAuthStorageBackend::new();
        let seeded = serialize_storage_data(data)?;
        // `storage.withLock(() => ({ result: undefined, next: JSON.stringify(...) }))`.
        backend
            .with_lock(|_| {
                Ok(LockResult {
                    result: (),
                    next: Some(seeded),
                })
            })
            .expect("the in-memory backend cannot fail");
        Ok(Self::from_backend(backend))
    }
}

impl<B: AuthStorageBackend> AuthStorage<B> {
    /// `AuthStorage.fromStorage(storage)` (`:184-186`) — the private constructor plus its
    /// `reload()` (`:175-178`).
    pub fn from_backend(storage: B) -> Self {
        let mut store = Self {
            data: AuthStorageData::new(),
            storage,
            process_env: ProcessEnv::from_process_env(),
        };
        store.reload();
        store
    }

    /// Replace the `process.env` snapshot [`AuthStorage::read`] resolves `$VAR` against.
    /// Only affects `read`; nothing else in this module touches the environment.
    pub fn with_process_env(mut self, process_env: ProcessEnv) -> Self {
        self.process_env = process_env;
        self
    }

    /// The backend, for callers that need the path (or the in-memory snapshot).
    pub fn backend(&self) -> &B {
        &self.storage
    }

    /// `reload()` (`:204-215`): read under the lock into `data`.
    ///
    /// **Every** failure — lock, I/O, `JSON.parse`, non-object — leaves the previous
    /// snapshot in place and is not reported, because Pi's `catch` is empty (`:212-214`).
    /// Fixture record 10 pins exactly that: a corrupted `auth.json` still reads the last
    /// good credential.
    pub fn reload(&mut self) {
        let loaded = self.storage.with_lock(|current| {
            Ok(LockResult {
                result: current.map(str::to_string),
                next: None,
            })
        });
        if let Ok(content) = loaded {
            if let Ok(parsed) = parse_storage_data("<reload>", content.as_deref()) {
                self.data = parsed;
            }
        }
    }

    /// The raw stored JSON for a provider — Pi's `this.data[provider]` before any typing.
    ///
    /// Use this where Pi's tolerance for malformed entries matters; [`AuthStorage::read`]
    /// reports those as `None` (see the module docs).
    pub fn raw(&self, provider: &str) -> Option<&Value> {
        self.data.get(provider)
    }

    /// `read(provider)` (`:217-222`).
    ///
    /// Returns the stored credential unchanged, **except** an `api_key` with a defined
    /// `key`, which becomes `{ ...credential, key: resolveConfigValue(key, env) }`. An
    /// unresolvable reference therefore yields a credential with **no** `key` — JS assigns
    /// `undefined` and `JSON.stringify` drops it — while `env` and any unknown fields are
    /// carried over. `oauth` entries, an `api_key` without a key, and an absent provider
    /// are all returned as-is.
    ///
    /// Not `async` (see the module docs), and it never touches the file: `data` is the
    /// snapshot from the last successful [`AuthStorage::reload`].
    pub fn read(&self, provider: &str) -> Option<Credential> {
        let credential = self.credential(provider)?;
        match credential {
            Credential::ApiKey {
                key: Some(key),
                env,
                extra,
            } => Some(Credential::ApiKey {
                key: resolve_config_value(&key, env.as_ref(), &self.process_env),
                env,
                extra,
            }),
            other => Some(other),
        }
    }

    /// `modify(provider, fn)` (`:224-240`) — the only write path, serialized by the
    /// backend's lock.
    ///
    /// Re-reads the file under the lock, hands `fn` the **current** credential (`None` when
    /// the provider has no entry — fixture record 2), then:
    ///
    /// - `fn` returns `None` → **nothing is written**; the refreshed snapshot is kept and
    ///   the *unchanged* current credential is returned (fixture record 6).
    /// - `fn` returns `Some(next)` → `{ ...currentData, [provider]: next }`: an existing
    ///   provider is updated **in place**, keeping its position (record 5), a new one is
    ///   **appended** (record 4), and the whole file is rewritten with
    ///   [`serialize_storage_data`].
    ///
    /// `fn` is sync here; Pi's is async only so `Models.getAuth()` can run an OAuth refresh
    /// inside the lock (see the module docs).
    pub fn modify<F>(&mut self, provider: &str, f: F) -> Result<Option<Credential>, AuthError>
    where
        F: FnOnce(Option<Credential>) -> Option<Credential>,
    {
        // Split the borrow: the closure mutates `data` while `storage` is borrowed.
        let Self { data, storage, .. } = self;
        storage.with_lock(|content| {
            let mut current_data = parse_storage_data("<modify>", content)?;
            let current = current_data
                .get(provider)
                .and_then(|v| serde_json::from_value::<Credential>(v.clone()).ok());

            let Some(next) = f(current.clone()) else {
                // `:231-234` — refresh the snapshot, return the current value, do not write.
                *data = current_data;
                return Ok(LockResult {
                    result: current,
                    next: None,
                });
            };

            let value = serde_json::to_value(&next).map_err(|e| AuthError::json("<modify>", e))?;
            // `{ ...currentData, [provider]: next }`: `Map::insert` keeps an existing key's
            // position and appends a new one, exactly like a JS object spread.
            current_data.insert(provider.to_string(), value);
            let serialized = serialize_storage_data(&current_data)
                .map_err(|e| AuthError::json("<modify>", e))?;
            *data = current_data;
            Ok(LockResult {
                result: Some(next),
                next: Some(serialized),
            })
        })
    }

    /// `delete(provider)` (`:242-249`) — remove the entry and **always** rewrite.
    ///
    /// Deleting a provider that is not present still rewrites the file (fixture record 7),
    /// which re-serializes it and so can reformat a hand-edited `auth.json`. Remaining
    /// providers keep their relative order: the map is rebuilt by filtering, never by
    /// `serde_json::Map::remove` — which is `swap_remove` under `preserve_order` and would
    /// move the last provider into the hole.
    pub fn delete(&mut self, provider: &str) -> Result<(), AuthError> {
        let Self { data, storage, .. } = self;
        storage.with_lock(|content| {
            let current_data = parse_storage_data("<delete>", content)?;
            let remaining: AuthStorageData = current_data
                .into_iter()
                .filter(|(key, _)| key != provider)
                .collect();
            let serialized =
                serialize_storage_data(&remaining).map_err(|e| AuthError::json("<delete>", e))?;
            *data = remaining;
            Ok(LockResult {
                result: (),
                next: Some(serialized),
            })
        })
    }

    /// `list()` (`:252-254`) — `{ providerId, type }` per entry, in map order, from the
    /// in-memory snapshot. Never resolves `$VAR` values or executes anything.
    pub fn list(&self) -> Vec<CredentialInfo> {
        self.data
            .iter()
            .map(|(provider_id, value)| CredentialInfo {
                provider_id: provider_id.clone(),
                credential_type: value
                    .get("type")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            })
            .collect()
    }

    /// The stored credential as typed, with **no** `$VAR` resolution — Pi's
    /// `this.data[provider]` (`:218`), which is also what `modify` shows its callback.
    fn credential(&self, provider: &str) -> Option<Credential> {
        serde_json::from_value(self.data.get(provider)?.clone()).ok()
    }
}

// =============================================================================
// One-off read (`:261-271`)
// =============================================================================

/// `readStoredCredential(providerId, authPath)` (`:261-271`) — a one-off read with no
/// store, no lock and **no** `$VAR` resolution.
///
/// Returns `None` on *any* error (missing file, bad JSON, entry not a credential), matching
/// Pi's blanket `catch` (`:268-270`). `auth_path` is used as given; Pi's `normalizePath`
/// (`:266`) is the caller's job — see [`FileAuthStorageBackend::new`].
pub fn read_stored_credential(provider_id: &str, auth_path: &Path) -> Option<Credential> {
    let content = fs::read_to_string(auth_path).ok()?;
    let data: AuthStorageData = serde_json::from_str(&content).ok()?;
    serde_json::from_value(data.get(provider_id)?.clone()).ok()
}

// =============================================================================
// Composition with pirust_ai::auth
// =============================================================================

/// The stored credential's contribution to request auth, resolved through
/// [`pirust_ai::auth::resolve_api_key`].
///
/// This is `ApiKeyAuth.resolve`'s per-field merge — `credential.key ?? env("…")`
/// (`packages/ai/src/auth/types.ts:175-181`) — expressed by composition: the stored value
/// becomes `resolve_api_key`'s `explicit` argument, so the
/// `ANTHROPIC_OAUTH_TOKEN` → `ANTHROPIC_API_KEY` precedence stays in
/// [`pirust_ai::auth`] and is not restated here.
///
/// - [`Credential::ApiKey`] contributes its `key`, which the caller should already have run
///   through [`AuthStorage::read`] (this function does not interpolate `$VAR`).
/// - [`Credential::OAuth`] contributes its `access` token. An Anthropic access token
///   contains `sk-ant-oat`, so [`pirust_ai::auth::is_oauth_token`] classifies it and the
///   adapter sends `Authorization: Bearer` instead of `X-Api-Key`. Expiry and refresh are
///   *not* checked here — in Pi that is `Models.getAuth()`, which calls back into
///   [`AuthStorage::modify`] under the lock; no refresh flow is ported yet.
/// - `None`, or an `api_key` with no key, falls through to the ambient environment.
pub fn credential_api_key(
    credential: Option<&Credential>,
    provider_env: &BTreeMap<String, String>,
) -> Option<String> {
    let explicit = match credential {
        Some(Credential::ApiKey { key, .. }) => key.as_deref(),
        Some(Credential::OAuth { access, .. }) => Some(access.as_str()),
        None => None,
    };
    pirust_ai::auth::resolve_api_key(explicit, provider_env)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `{ "a": { "type": "api_key", "key": "1" } }` as an [`AuthStorageData`].
    fn one_api_key(provider: &str, key: &str) -> AuthStorageData {
        let mut data = AuthStorageData::new();
        data.insert(
            provider.to_string(),
            serde_json::to_value(Credential::api_key(key)).unwrap(),
        );
        data
    }

    #[test]
    fn api_key_and_oauth_serialize_with_the_tag_first() {
        // The shape fixture records 2 and 3 pin; asserted here too so a `Credential` field
        // reorder fails the unit tests as well as the goldens.
        assert_eq!(
            serde_json::to_string(&Credential::api_key("sk")).unwrap(),
            r#"{"type":"api_key","key":"sk"}"#
        );
        assert_eq!(
            serde_json::to_string(&Credential::oauth("rt", "at", 1730000000000)).unwrap(),
            r#"{"type":"oauth","refresh":"rt","access":"at","expires":1730000000000}"#
        );
    }

    #[test]
    fn unknown_fields_round_trip_through_the_typed_view() {
        // `OAuthCredentials`' index signature (`types.ts:28`) — and the same courtesy for
        // api_key, since `modify` rewrites the whole file.
        for line in [
            r#"{"type":"oauth","refresh":"rt","access":"at","expires":1,"scope":"user","nested":{"a":[1,2]}}"#,
            r#"{"type":"api_key","key":"sk","env":{"B":"2","A":"1"},"label":"work"}"#,
        ] {
            let credential: Credential = serde_json::from_str(line).unwrap();
            assert_eq!(serde_json::to_string(&credential).unwrap(), line);
        }
    }

    #[test]
    fn empty_and_absent_content_parse_as_an_empty_object() {
        // `if (!content) return {}` (`:195-197`).
        assert!(parse_storage_data("t", None).unwrap().is_empty());
        assert!(parse_storage_data("t", Some("")).unwrap().is_empty());
        assert_eq!(
            serialize_storage_data(&AuthStorageData::new()).unwrap(),
            "{}"
        );
        // A non-object document is an error rather than a JS TypeError at first access.
        assert!(matches!(
            parse_storage_data("t", Some("[]")),
            Err(AuthError::NotAnObject { found: "array", .. })
        ));
    }

    #[test]
    fn no_write_when_the_callback_returns_none() {
        // Fixture record 6 pins the *content*; this pins the stronger claim in its note —
        // "no `next` means withLockAsync skips writeFileSync entirely" — by seeding bytes
        // that any rewrite would reformat.
        let backend = InMemoryAuthStorageBackend::new();
        backend
            .with_lock(|_| {
                Ok(LockResult {
                    result: (),
                    next: Some(r#"{"a":{"type":"api_key","key":"1"}}"#.to_string()),
                })
            })
            .unwrap();
        let mut store = AuthStorage::from_backend(backend);
        let returned = store.modify("a", |_| None).unwrap();
        assert_eq!(returned, Some(Credential::api_key("1")));
        assert_eq!(
            store.backend().snapshot().as_deref(),
            Some(r#"{"a":{"type":"api_key","key":"1"}}"#),
            "a no-op modify must not rewrite the file"
        );
        // A delete, by contrast, always rewrites — hence the reformat (record 7's note).
        store.delete("absent").unwrap();
        assert_eq!(
            store.backend().snapshot().as_deref(),
            Some("{\n  \"a\": {\n    \"type\": \"api_key\",\n    \"key\": \"1\"\n  }\n}")
        );
    }

    #[test]
    fn delete_keeps_the_relative_order_of_the_rest() {
        // Guards against `Map::remove`, which is `swap_remove` under `preserve_order`.
        let mut data = one_api_key("a", "1");
        for provider in ["b", "c", "d"] {
            data.insert(
                provider.to_string(),
                serde_json::to_value(Credential::api_key(provider)).unwrap(),
            );
        }
        let mut store = AuthStorage::in_memory(&data).unwrap();
        store.delete("b").unwrap();
        assert_eq!(
            store
                .list()
                .into_iter()
                .map(|info| info.provider_id)
                .collect::<Vec<_>>(),
            ["a", "c", "d"]
        );
    }

    #[test]
    fn list_passes_through_an_unknown_type_and_omits_a_missing_one() {
        // `list()` reads `credential.type` off raw JSON (`:253`).
        let mut data = AuthStorageData::new();
        data.insert("weird".to_string(), serde_json::json!({ "type": "magic" }));
        data.insert("typeless".to_string(), serde_json::json!({ "key": "sk" }));
        let store = AuthStorage::in_memory(&data).unwrap();
        assert_eq!(
            serde_json::to_string(&store.list()).unwrap(),
            r#"[{"providerId":"weird","type":"magic"},{"providerId":"typeless"}]"#
        );
        // …and read() reports them absent while raw() still shows them (module docs).
        assert_eq!(store.read("weird"), None);
        assert!(store.raw("weird").is_some());
    }

    #[test]
    fn config_value_grammar_matches_the_ts_parser() {
        let process_env = ProcessEnv::from_pairs([("V", "from-env"), ("EMPTY", "")]);
        let resolve = |config: &str| resolve_config_value(config, None, &process_env);
        assert_eq!(resolve("sk-literal").as_deref(), Some("sk-literal"));
        assert_eq!(resolve("$V").as_deref(), Some("from-env"));
        assert_eq!(resolve("${V}").as_deref(), Some("from-env"));
        assert_eq!(resolve("Bearer $V!").as_deref(), Some("Bearer from-env!"));
        assert_eq!(resolve("$MISSING"), None);
        assert_eq!(resolve("a$MISSING$V"), None, "one missing part fails all");
        // `$$` and `$!` escapes (`:42-46`).
        assert_eq!(resolve("sk-$$literal").as_deref(), Some("sk-$literal"));
        assert_eq!(resolve("$!notacommand").as_deref(), Some("!notacommand"));
        // An unterminated `${`, an invalid name, and a bare `$` stay literal (`:50-61,73`).
        assert_eq!(resolve("${V").as_deref(), Some("${V"));
        assert_eq!(resolve("${1V}").as_deref(), Some("${1V}"));
        assert_eq!(resolve("100$ or so").as_deref(), Some("100$ or so"));
        // `||`, not `??`: an empty value falls through to "unset" (`:89`).
        assert_eq!(resolve("$EMPTY"), None);
        // Provider env wins over process env, and a non-string value is falsy.
        let mut env = ProviderEnv::new();
        env.insert(
            "V".to_string(),
            Value::String("from-credential".to_string()),
        );
        assert_eq!(
            resolve_config_value("$V", Some(&env), &process_env).as_deref(),
            Some("from-credential")
        );
        env.insert("V".to_string(), Value::Number(7.into()));
        assert_eq!(
            resolve_config_value("$V", Some(&env), &process_env).as_deref(),
            Some("from-env")
        );
        // `!command` is the documented narrowing, distinguishable from an unset variable.
        assert!(is_command_config_value("!op read op://x"));
        assert_eq!(resolve("!op read op://x"), None);
        assert!(!is_command_config_value("$!op"));
    }

    #[test]
    fn credential_api_key_composes_with_pirust_ai() {
        let no_env = BTreeMap::new();
        // The stored key becomes pirust_ai's `explicit` argument.
        assert_eq!(
            credential_api_key(Some(&Credential::api_key("sk-ant-api03-x")), &no_env).as_deref(),
            Some("sk-ant-api03-x")
        );
        // An OAuth access token is classified by pirust_ai::auth::is_oauth_token → Bearer.
        let oauth = Credential::oauth("rt", "sk-ant-oat01-x", 1);
        let resolved = credential_api_key(Some(&oauth), &no_env).unwrap();
        assert_eq!(resolved, "sk-ant-oat01-x");
        assert!(pirust_ai::auth::is_oauth_token(&resolved));
        assert!(!pirust_ai::auth::is_oauth_token("sk-ant-api03-x"));
        // No credential → the ambient env, whose precedence lives in pirust_ai::auth.
        let env: BTreeMap<String, String> = [
            (
                pirust_ai::auth::ANTHROPIC_API_KEY_ENV.to_string(),
                "sk-ant-api03-env".to_string(),
            ),
            (
                pirust_ai::auth::ANTHROPIC_OAUTH_TOKEN_ENV.to_string(),
                "sk-ant-oat01-env".to_string(),
            ),
        ]
        .into_iter()
        .collect();
        assert_eq!(
            credential_api_key(None, &env).as_deref(),
            Some("sk-ant-oat01-env")
        );
        assert_eq!(
            credential_api_key(
                Some(&Credential::ApiKey {
                    key: None,
                    env: None,
                    extra: Map::new()
                }),
                &env
            )
            .as_deref(),
            Some("sk-ant-oat01-env")
        );
    }

    #[test]
    fn in_memory_and_file_backends_serialize_identically() {
        // Fixture record 11's claim, cross-checked without a literal: the same data through
        // both backends must produce the same bytes.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("auth.json");
        let mut file_store = AuthStorage::from_backend(FileAuthStorageBackend::new(&path));
        file_store
            .modify("openai", |current| {
                assert_eq!(current, None);
                Some(Credential::api_key("sk-mem"))
            })
            .unwrap();
        let data = one_api_key("openai", "sk-mem");
        let memory_store = AuthStorage::in_memory(&data).unwrap();
        assert_eq!(
            memory_store.backend().snapshot(),
            Some(fs::read_to_string(&path).unwrap())
        );
    }

    #[test]
    fn the_lock_directory_is_released_and_breakable() {
        let dir = tempfile::tempdir().unwrap();
        let backend = FileAuthStorageBackend::new(dir.path().join("auth.json"));
        backend
            .with_lock(|_| {
                Ok(LockResult {
                    result: (),
                    next: None,
                })
            })
            .unwrap();
        assert!(
            !backend.lock_path().exists(),
            "the lock must be released on the way out"
        );
        assert_eq!(
            backend.lock_path().file_name().unwrap().to_string_lossy(),
            "auth.json.lock",
            "proper-lockfile's artifact name"
        );
        // A held lock is reported rather than waited on forever.
        fs::create_dir(backend.lock_path()).unwrap();
        assert!(matches!(
            backend.with_lock(|_| Ok(LockResult {
                result: (),
                next: None
            })),
            Err(AuthError::Locked {
                attempts: LOCK_MAX_ATTEMPTS,
                ..
            })
        ));
        fs::remove_dir(backend.lock_path()).unwrap();
        // …and an error inside the callback still releases it (Pi's `finally`, `:90-94`).
        let failed: Result<(), AuthError> = backend.with_lock(|_| {
            Err(AuthError::NotAnObject {
                path: "t".to_string(),
                found: "array",
            })
        });
        assert!(failed.is_err());
        assert!(!backend.lock_path().exists());
    }
}
