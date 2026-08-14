//! Port of `core/tools/path-utils.ts` + the `utils/paths.ts` helpers it calls.
//!
//! Gated by `tests/fixtures/pi/tools/path_utils.cases.jsonl` (50 cases from real Pi).
//!
//! # Why this is bigger than the 118-line TS original
//!
//! `paths.ts` delegates the real work to `node:path`, `node:url` and `node:os`:
//! `normalizePath` calls `join`, `resolvePath` calls `isAbsolute` + `resolve`,
//! and the `file://` branch calls `fileURLToPath`. Those are the functions the
//! oracle actually captured (`C:\oracle\cwd\a\b.txt` for input `a/b.txt`), and
//! `std::path` does **not** reproduce them: `Path::join` keeps `.`/`..` segments
//! verbatim, `PathBuf` re-normalizes separators, and `Path::display()` differs
//! from Node's string output. So `node_path` below is a literal transcription of
//! Node's `lib/path.js` `win32`/`posix` `resolve`/`normalize`/`join`/`isAbsolute`
//! (plus the `normalizeString` helper all four share), and `file_url` is a
//! transcription of `fileURLToPath`'s WHATWG-URL-to-path conversion.
//!
//! Node's string algorithms only ever branch on ASCII bytes (`/`, `\`, `:`, `.`,
//! `A-Za-z`), so running them over UTF-8 bytes rather than UTF-16 code units is
//! observationally identical: a multi-byte UTF-8 sequence never contains an ASCII
//! byte, and every slice boundary we take is an ASCII byte or the string end.
//!
//! # Process ambience is a seam, not a global
//!
//! Pi reads `process.platform` (twice: the `~\` check and which `node:path`
//! flavour is loaded), `os.homedir()` and `process.cwd()`. Those three live in
//! [`PathEnv`]; [`expand_path`]/[`resolve_to_cwd`] use [`PathEnv::current`], and
//! the golden test injects the fixture's `{HOME}` / `platform` / `cwd` instead so
//! it asserts the same bytes on any machine.
//!
//! # `String.prototype.normalize("NFD")`
//!
//! [`try_nfd_variant`] (`path-utils.ts:11`) needs full Unicode NFD, which the
//! `unicode-normalization` crate provides: `UnicodeNormalization::nfd` is the
//! same canonical decomposition + combining-class reordering the ECMAScript
//! `normalize("NFD")` is specified against, so Latin Extended, Hangul syllables
//! and multi-step decompositions all come out right. `tests/path_utils_golden.rs`
//! pins that against four concrete Pi outputs
//! (`nfd_variant_matches_js_string_normalize`). No fixture case in
//! `path_utils.cases.jsonl` exercises this path.

use std::fmt;

use unicode_normalization::UnicodeNormalization;

/// `path-utils.ts:5` — `const NARROW_NO_BREAK_SPACE = "\u202F"`.
const NARROW_NO_BREAK_SPACE: char = '\u{202F}';

/// `path-utils.ts:19` — the U+2019 RIGHT SINGLE QUOTATION MARK macOS uses.
const CURLY_APOSTROPHE: &str = "\u{2019}";

// ===========================================================================
// Errors — the TS `throw` sites
// ===========================================================================

/// The exceptions `normalizePath` can propagate. `paths.ts:74-76` calls
/// `fileURLToPath`, which throws for a malformed URL, for a path containing an
/// encoded separator, and (on win32) for a `file://` path without a drive
/// letter — e.g. `expandPath("file:///foo")` throws on Windows. TS `throw`
/// therefore maps to `Err`; every other input is infallible.
///
/// `Display` reproduces Node's `err.message` verbatim so the `ok: false` /
/// `error` columns of `path_utils.cases.jsonl` stay comparable (the captured
/// corpus has no such row today — all 50 are `ok: true`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathError {
    /// `new URL(input)` threw (`ERR_INVALID_URL`).
    InvalidUrl,
    /// `ERR_INVALID_FILE_URL_PATH`; carries Node's `%s` fill-in.
    InvalidFileUrlPath(&'static str),
    /// `ERR_INVALID_FILE_URL_HOST`; carries `process.platform`.
    InvalidFileUrlHost(&'static str),
    /// `decodeURIComponent` threw `URIError: URI malformed`.
    UriMalformed,
}

impl fmt::Display for PathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidUrl => write!(f, "Invalid URL"),
            Self::InvalidFileUrlPath(what) => write!(f, "File URL path {what}"),
            Self::InvalidFileUrlHost(platform) => {
                write!(
                    f,
                    "File URL host must be \"localhost\" or empty on {platform}"
                )
            }
            Self::UriMalformed => write!(f, "URI malformed"),
        }
    }
}

impl std::error::Error for PathError {}

// ===========================================================================
// Process ambience
// ===========================================================================

/// `process.platform`, reduced to the two branches `paths.ts` distinguishes:
/// the `~\` tilde form (`paths.ts:69`) and which `node:path` flavour runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    /// `process.platform === "win32"`.
    Win32,
    /// Every other `process.platform` (`darwin`, `linux`, …).
    Posix,
}

impl Platform {
    /// The platform this binary was built for.
    pub fn current() -> Self {
        if cfg!(windows) {
            Self::Win32
        } else {
            Self::Posix
        }
    }

    /// The `process.platform` string, for [`PathError::InvalidFileUrlHost`].
    fn name(self) -> &'static str {
        match self {
            Self::Win32 => "win32",
            Self::Posix => "posix",
        }
    }

    fn is_absolute(self, path: &str) -> bool {
        match self {
            Self::Win32 => node_path::win32_is_absolute(path),
            Self::Posix => node_path::posix_is_absolute(path),
        }
    }

    fn join(self, parts: &[&str]) -> String {
        match self {
            Self::Win32 => node_path::win32_join(parts),
            Self::Posix => node_path::posix_join(parts),
        }
    }

    fn resolve(self, parts: &[&str], cwd: &str) -> String {
        match self {
            Self::Win32 => node_path::win32_resolve(parts, cwd),
            Self::Posix => node_path::posix_resolve(parts, cwd),
        }
    }
}

/// `os.homedir()`.
///
/// Node's `uv_os_homedir` reads `USERPROFILE` on Windows and `HOME` elsewhere,
/// falling back to `GetUserProfileDirectory` / `getpwuid` when unset. Only the
/// env-var half is modelled (the fallback would need a new dependency); an unset
/// variable yields `""`, exactly as `uv_os_homedir` failing would surface as an
/// `os.homedir()` throw. Tests should inject [`PathEnv::home_dir`] instead of
/// depending on this.
pub fn home_dir() -> String {
    let key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var(key).unwrap_or_default()
}

/// `process.cwd()`; `""` if the cwd is gone or not UTF-8.
pub fn cwd() -> String {
    std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The three process globals `paths.ts` reads, hoisted into an injectable value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathEnv {
    /// `process.platform`.
    pub platform: Platform,
    /// `os.homedir()` — the default for `PathInputOptions::home_dir`.
    pub home_dir: String,
    /// `process.cwd()` — `resolvePath`'s default `baseDir` and `path.resolve`'s
    /// last-resort base.
    pub cwd: String,
}

impl PathEnv {
    /// Snapshot the real process environment.
    pub fn current() -> Self {
        Self {
            platform: Platform::current(),
            home_dir: home_dir(),
            cwd: cwd(),
        }
    }
}

// ===========================================================================
// utils/paths.ts
// ===========================================================================

/// `paths.ts:9-20` — `PathInputOptions`. `expand_tilde: None` is TS `undefined`,
/// i.e. the `?? true` default at `paths.ts:66`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PathInputOptions<'a> {
    /// Trim leading/trailing whitespace before normalization.
    pub trim: bool,
    /// Expand leading `~` to a home directory. Defaults to true.
    pub expand_tilde: Option<bool>,
    /// Home directory used for `~` expansion. Defaults to [`PathEnv::home_dir`].
    pub home_dir: Option<&'a str>,
    /// Strip a leading `@`, used for CLI @file paths.
    pub strip_at_prefix: bool,
    /// Normalize unicode space variants to regular spaces.
    pub normalize_unicode_spaces: bool,
}

impl PathInputOptions<'_> {
    /// The options both exported entry points pass
    /// (`path-utils.ts:41` and `path-utils.ts:49`).
    fn pi_defaults() -> Self {
        Self {
            normalize_unicode_spaces: true,
            strip_at_prefix: true,
            ..Self::default()
        }
    }
}

/// `paths.ts:7` — `const UNICODE_SPACES = /[\u00A0\u2000-\u200A\u202F\u205F\u3000]/g`.
///
/// Note what is *not* in it: U+1680 OGHAM SPACE MARK, U+200B ZERO WIDTH SPACE,
/// U+2028/U+2029 and U+FEFF all survive `normalizeUnicodeSpaces` untouched.
fn is_unicode_space(c: char) -> bool {
    matches!(
        c,
        '\u{00A0}' | '\u{2000}'..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}'
    )
}

/// `paths.ts:60` — `normalized.replace(UNICODE_SPACES, " ")`.
fn replace_unicode_spaces(input: &str) -> String {
    input
        .chars()
        .map(|c| if is_unicode_space(c) { ' ' } else { c })
        .collect()
}

/// JS `String.prototype.trim` (`paths.ts:58`): the ECMAScript `WhiteSpace` +
/// `LineTerminator` sets. Deliberately not `str::trim`, which drops U+0085 (not
/// JS whitespace) and keeps U+FEFF (which JS does trim).
fn js_trim(input: &str) -> &str {
    let is_js_space = |c: char| {
        matches!(
            c,
            '\u{0009}'
                | '\u{000A}'
                | '\u{000B}'
                | '\u{000C}'
                | '\u{000D}'
                | '\u{0020}'
                | '\u{00A0}'
                | '\u{1680}'
                | '\u{2000}'
                ..='\u{200A}'
                    | '\u{2028}'
                    | '\u{2029}'
                    | '\u{202F}'
                    | '\u{205F}'
                    | '\u{3000}'
                    | '\u{FEFF}'
        )
    };
    input.trim_matches(is_js_space)
}

/// `paths.ts:57-79` — `normalizePath`.
///
/// Order of operations is load-bearing and matches the TS line for line:
/// optional trim → unicode-space replacement → strip **one** leading `@` →
/// tilde expansion (which `return`s early, so a `~` path never reaches the
/// `file://` branch) → `file://` conversion.
pub fn normalize_path(
    env: &PathEnv,
    input: &str,
    options: &PathInputOptions<'_>,
) -> Result<String, PathError> {
    // paths.ts:58
    let mut normalized = if options.trim {
        js_trim(input).to_string()
    } else {
        input.to_string()
    };
    // paths.ts:59-61
    if options.normalize_unicode_spaces {
        normalized = replace_unicode_spaces(&normalized);
    }
    // paths.ts:62-64 — `startsWith("@")` + `slice(1)`: exactly one `@`, so
    // `"@@x"` becomes `"@x"`.
    if options.strip_at_prefix {
        if let Some(rest) = normalized.strip_prefix('@') {
            normalized = rest.to_string();
        }
    }

    // paths.ts:66-72
    if options.expand_tilde.unwrap_or(true) {
        let home = options.home_dir.unwrap_or(&env.home_dir);
        if normalized == "~" {
            return Ok(home.to_string());
        }
        let tail = normalized.strip_prefix("~/").or(match env.platform {
            Platform::Win32 => normalized.strip_prefix("~\\"),
            Platform::Posix => None,
        });
        if let Some(tail) = tail {
            return Ok(env.platform.join(&[home, tail]));
        }
    }

    // paths.ts:74-76 — `/^file:\/\//.test(normalized)`.
    if normalized.starts_with("file://") {
        return file_url::file_url_to_path(&normalized, env.platform);
    }

    // paths.ts:78
    Ok(normalized)
}

/// `paths.ts:81-85` — `resolvePath`.
///
/// `base_dir` is TS's `baseDir`, whose default is `process.cwd()`; pass
/// `&env.cwd` for that. Note the base is itself run through `normalizePath` with
/// **empty options** (`paths.ts:83`), so a `~`-rooted or `file://` base expands
/// but its unicode spaces and leading `@` do not.
pub fn resolve_path(
    env: &PathEnv,
    input: &str,
    base_dir: &str,
    options: &PathInputOptions<'_>,
) -> Result<String, PathError> {
    let normalized = normalize_path(env, input, options)?;
    let normalized_base_dir = normalize_path(env, base_dir, &PathInputOptions::default())?;
    Ok(if env.platform.is_absolute(&normalized) {
        env.platform.resolve(&[&normalized], &env.cwd)
    } else {
        env.platform
            .resolve(&[&normalized_base_dir, &normalized], &env.cwd)
    })
}

// ===========================================================================
// core/tools/path-utils.ts
// ===========================================================================

/// `path-utils.ts:40-42` — `expandPath`, against the real process environment.
pub fn expand_path(file_path: &str) -> Result<String, PathError> {
    expand_path_in(&PathEnv::current(), file_path)
}

/// [`expand_path`] with [`PathEnv`] injected.
pub fn expand_path_in(env: &PathEnv, file_path: &str) -> Result<String, PathError> {
    normalize_path(env, file_path, &PathInputOptions::pi_defaults())
}

/// `path-utils.ts:48-50` — `resolveToCwd`. Resolve a path relative to the given
/// cwd; handles `~` expansion and absolute paths.
pub fn resolve_to_cwd(file_path: &str, cwd: &str) -> Result<String, PathError> {
    resolve_to_cwd_in(&PathEnv::current(), file_path, cwd)
}

/// [`resolve_to_cwd`] with [`PathEnv`] injected.
pub fn resolve_to_cwd_in(env: &PathEnv, file_path: &str, cwd: &str) -> Result<String, PathError> {
    resolve_path(env, file_path, cwd, &PathInputOptions::pi_defaults())
}

/// `path-utils.ts:22-29` — `fileExists`, i.e. `accessSync(p, F_OK)`.
/// True for directories too; follows symlinks.
pub fn file_exists(file_path: &str) -> bool {
    std::fs::metadata(file_path).is_ok()
}

/// `path-utils.ts:31-38` — `pathExists`, i.e. `await access(p, F_OK)`.
pub async fn path_exists(file_path: &str) -> bool {
    tokio::fs::metadata(file_path).await.is_ok()
}

/// `path-utils.ts:7-9` — `tryMacOSScreenshotPath`:
/// `filePath.replace(/ (AM|PM)\./gi, "\u202F$1.")`.
///
/// The `$1` keeps the matched case (` am.` → `\u202Fam.`), the `.` is literal,
/// and `g` restarts the scan after each 4-char match.
fn try_macos_screenshot_path(file_path: &str) -> String {
    let b = file_path.as_bytes();
    let mut out = String::with_capacity(file_path.len() + 2);
    let mut i = 0usize;
    let mut copied = 0usize;
    while i + 4 <= b.len() {
        let matched = b[i] == b' '
            && (b[i + 1].eq_ignore_ascii_case(&b'a') || b[i + 1].eq_ignore_ascii_case(&b'p'))
            && b[i + 2].eq_ignore_ascii_case(&b'm')
            && b[i + 3] == b'.';
        if matched {
            out.push_str(&file_path[copied..i]);
            out.push(NARROW_NO_BREAK_SPACE);
            out.push_str(&file_path[i + 1..i + 4]);
            i += 4;
            copied = i;
        } else {
            i += 1;
        }
    }
    out.push_str(&file_path[copied..]);
    out
}

/// `path-utils.ts:11-14` — `tryNFDVariant`: `filePath.normalize("NFD")`.
/// macOS stores filenames in NFD (decomposed) form.
fn try_nfd_variant(file_path: &str) -> String {
    file_path.nfd().collect()
}

/// `path-utils.ts:16-20` — `tryCurlyQuoteVariant`:
/// `filePath.replace(/'/g, "\u2019")`. macOS uses U+2019 in screenshot names
/// like "Capture d'écran"; users type U+0027.
fn try_curly_quote_variant(file_path: &str) -> String {
    file_path.replace('\'', CURLY_APOSTROPHE)
}

/// `path-utils.ts:52-84` — `resolveReadPath`, against the real filesystem.
pub fn resolve_read_path(file_path: &str, cwd: &str) -> Result<String, PathError> {
    resolve_read_path_in(&PathEnv::current(), file_path, cwd, file_exists)
}

/// [`resolve_read_path`] with [`PathEnv`] and the `fileExists` probe injected.
///
/// Each of the four macOS fallbacks is tried in order and accepted only when it
/// **differs from `resolved`** *and* exists; note the last one compares against
/// `resolved`, not against `nfd_variant` (`path-utils.ts:79`). Otherwise the
/// plain resolved path is returned, existing or not.
pub fn resolve_read_path_in(
    env: &PathEnv,
    file_path: &str,
    cwd: &str,
    exists: impl Fn(&str) -> bool,
) -> Result<String, PathError> {
    let resolved = resolve_to_cwd_in(env, file_path, cwd)?;

    if exists(&resolved) {
        return Ok(resolved);
    }

    // Try macOS AM/PM variant (narrow no-break space before AM/PM)
    let am_pm_variant = try_macos_screenshot_path(&resolved);
    if am_pm_variant != resolved && exists(&am_pm_variant) {
        return Ok(am_pm_variant);
    }

    // Try NFD variant (macOS stores filenames in NFD form)
    let nfd_variant = try_nfd_variant(&resolved);
    if nfd_variant != resolved && exists(&nfd_variant) {
        return Ok(nfd_variant);
    }

    // Try curly quote variant (macOS uses U+2019 in screenshot names)
    let curly_variant = try_curly_quote_variant(&resolved);
    if curly_variant != resolved && exists(&curly_variant) {
        return Ok(curly_variant);
    }

    // Try combined NFD + curly quote (for French macOS screenshots like "Capture d'écran")
    let nfd_curly_variant = try_curly_quote_variant(&nfd_variant);
    if nfd_curly_variant != resolved && exists(&nfd_curly_variant) {
        return Ok(nfd_curly_variant);
    }

    Ok(resolved)
}

/// `path-utils.ts:86-118` — `resolveReadPathAsync`. The TS duplicates the whole
/// ladder with `await pathExists(...)` instead of `fileExists(...)`; so does this.
pub async fn resolve_read_path_async(file_path: &str, cwd: &str) -> Result<String, PathError> {
    resolve_read_path_async_in(&PathEnv::current(), file_path, cwd).await
}

/// [`resolve_read_path_async`] with [`PathEnv`] injected. The existence probe is
/// always [`path_exists`] (an injectable async predicate would need a boxed
/// future; the sync twin covers the branch logic).
pub async fn resolve_read_path_async_in(
    env: &PathEnv,
    file_path: &str,
    cwd: &str,
) -> Result<String, PathError> {
    let resolved = resolve_to_cwd_in(env, file_path, cwd)?;

    if path_exists(&resolved).await {
        return Ok(resolved);
    }

    // Try macOS AM/PM variant (narrow no-break space before AM/PM)
    let am_pm_variant = try_macos_screenshot_path(&resolved);
    if am_pm_variant != resolved && path_exists(&am_pm_variant).await {
        return Ok(am_pm_variant);
    }

    // Try NFD variant (macOS stores filenames in NFD form)
    let nfd_variant = try_nfd_variant(&resolved);
    if nfd_variant != resolved && path_exists(&nfd_variant).await {
        return Ok(nfd_variant);
    }

    // Try curly quote variant (macOS uses U+2019 in screenshot names)
    let curly_variant = try_curly_quote_variant(&resolved);
    if curly_variant != resolved && path_exists(&curly_variant).await {
        return Ok(curly_variant);
    }

    // Try combined NFD + curly quote (for French macOS screenshots like "Capture d'écran")
    let nfd_curly_variant = try_curly_quote_variant(&nfd_variant);
    if nfd_curly_variant != resolved && path_exists(&nfd_curly_variant).await {
        return Ok(nfd_curly_variant);
    }

    Ok(resolved)
}

// ===========================================================================
// node:path
// ===========================================================================

/// Literal transcription of Node's `lib/path.js` (`win32` + `posix`), limited to
/// the four functions `paths.ts` uses plus the shared `normalizeString`.
///
/// Kept private: it is an implementation detail of the port, not a public
/// path library. Every function is byte-oriented; see the module docs for why
/// that is equivalent to Node's UTF-16 indexing here.
mod node_path {
    /// `String::from_utf8` for a buffer that Node's algorithms only ever cut at
    /// ASCII separators, so it cannot land mid-codepoint.
    fn utf8(bytes: Vec<u8>) -> String {
        String::from_utf8(bytes).expect("node path algorithms only slice at ASCII separators")
    }

    /// `isPathSeparator` (win32): `/` or `\`.
    fn is_win32_sep(c: u8) -> bool {
        c == b'/' || c == b'\\'
    }

    /// `isPosixPathSeparator`: `/` only.
    fn is_posix_sep(c: u8) -> bool {
        c == b'/'
    }

    /// `isWindowsDeviceRoot`: `A-Z` or `a-z`.
    fn is_windows_device_root(c: u8) -> bool {
        c.is_ascii_alphabetic()
    }

    /// Node `normalizeString`: resolve `.` / `..` inside an already-rooted tail.
    /// `allow_above_root` keeps leading `..` segments for relative results.
    fn normalize_string(
        path: &[u8],
        allow_above_root: bool,
        sep: u8,
        is_sep: fn(u8) -> bool,
    ) -> Vec<u8> {
        let mut res: Vec<u8> = Vec::new();
        let mut last_segment_length: usize = 0;
        let mut last_slash: isize = -1;
        let mut dots: isize = 0;
        let mut code: u8 = 0;

        let mut i: usize = 0;
        while i <= path.len() {
            if i < path.len() {
                code = path[i];
            } else if is_sep(code) {
                break;
            } else {
                code = b'/';
            }

            if is_sep(code) {
                if last_slash == i as isize - 1 || dots == 1 {
                    // NOOP
                } else if dots == 2 {
                    let last_two_are_dots = res.len() >= 2
                        && last_segment_length == 2
                        && res[res.len() - 1] == b'.'
                        && res[res.len() - 2] == b'.';
                    if !last_two_are_dots {
                        if res.len() > 2 {
                            match res.iter().rposition(|&c| c == sep) {
                                None => {
                                    res.clear();
                                    last_segment_length = 0;
                                }
                                Some(last_slash_index) => {
                                    res.truncate(last_slash_index);
                                    let li = res
                                        .iter()
                                        .rposition(|&c| c == sep)
                                        .map_or(-1, |v| v as isize);
                                    last_segment_length = (res.len() as isize - 1 - li) as usize;
                                }
                            }
                            last_slash = i as isize;
                            dots = 0;
                            i += 1;
                            continue;
                        } else if !res.is_empty() {
                            res.clear();
                            last_segment_length = 0;
                            last_slash = i as isize;
                            dots = 0;
                            i += 1;
                            continue;
                        }
                    }
                    if allow_above_root {
                        if !res.is_empty() {
                            res.push(sep);
                        }
                        res.extend_from_slice(b"..");
                        last_segment_length = 2;
                    }
                } else {
                    let start = (last_slash + 1) as usize;
                    if !res.is_empty() {
                        res.push(sep);
                    }
                    res.extend_from_slice(&path[start..i]);
                    last_segment_length = i - start;
                }
                last_slash = i as isize;
                dots = 0;
            } else if code == b'.' && dots != -1 {
                dots += 1;
            } else {
                dots = -1;
            }
            i += 1;
        }
        res
    }

    /// The `\\server\share` scan shared by win32 `normalize` and `resolve`.
    /// Returns `(device, root_end, matched_whole_path)`, or `None` when the
    /// double separator is not followed by a full `server` + `share` pair (in
    /// which case Node leaves `device` empty and `rootEnd` at 0).
    fn win32_unc_root(p: &[u8]) -> Option<(Vec<u8>, usize, bool)> {
        let len = p.len();
        let mut j = 2usize;
        let mut last = j;
        // Match 1 or more non-path separators
        while j < len && !is_win32_sep(p[j]) {
            j += 1;
        }
        if j >= len || j == last {
            return None;
        }
        let first_part = &p[last..j];
        last = j;
        // Match 1 or more path separators
        while j < len && is_win32_sep(p[j]) {
            j += 1;
        }
        if j >= len || j == last {
            return None;
        }
        last = j;
        // Match 1 or more non-path separators
        while j < len && !is_win32_sep(p[j]) {
            j += 1;
        }
        let mut device = Vec::from(&b"\\\\"[..]);
        device.extend_from_slice(first_part);
        device.push(b'\\');
        if j == len {
            // We matched a UNC root only
            device.extend_from_slice(&p[last..]);
            return Some((device, j, true));
        }
        if j != last {
            // We matched a UNC root with leftovers
            device.extend_from_slice(&p[last..j]);
            return Some((device, j, false));
        }
        None
    }

    /// Node `win32.isAbsolute`.
    pub(super) fn win32_is_absolute(path: &str) -> bool {
        let p = path.as_bytes();
        if p.is_empty() {
            return false;
        }
        is_win32_sep(p[0])
            || (p.len() > 2 && is_windows_device_root(p[0]) && p[1] == b':' && is_win32_sep(p[2]))
    }

    /// Node `posix.isAbsolute`.
    pub(super) fn posix_is_absolute(path: &str) -> bool {
        path.as_bytes().first() == Some(&b'/')
    }

    /// Node `win32.normalize`.
    pub(super) fn win32_normalize(path: &str) -> String {
        let p = path.as_bytes();
        let len = p.len();
        if len == 0 {
            return ".".to_string();
        }
        let mut root_end = 0usize;
        let mut device: Option<Vec<u8>> = None;
        let mut is_absolute = false;
        let code = p[0];

        if len == 1 {
            // Note: `isPosixPathSeparator` here, not `isPathSeparator`.
            return if is_posix_sep(code) {
                "\\".to_string()
            } else {
                path.to_string()
            };
        }
        if is_win32_sep(code) {
            is_absolute = true;
            if is_win32_sep(p[1]) {
                match win32_unc_root(p) {
                    Some((d, _, true)) => {
                        // Nothing left to process: return the normalized UNC root.
                        let mut out = d;
                        out.push(b'\\');
                        return utf8(out);
                    }
                    Some((d, re, false)) => {
                        device = Some(d);
                        root_end = re;
                    }
                    None => {}
                }
            } else {
                root_end = 1;
            }
        } else if is_windows_device_root(code) && p[1] == b':' {
            device = Some(p[0..2].to_vec());
            root_end = 2;
            if len > 2 && is_win32_sep(p[2]) {
                is_absolute = true;
                root_end = 3;
            }
        }

        let mut tail = if root_end < len {
            normalize_string(&p[root_end..], !is_absolute, b'\\', is_win32_sep)
        } else {
            Vec::new()
        };
        if tail.is_empty() && !is_absolute {
            tail = b".".to_vec();
        }
        if !tail.is_empty() && is_win32_sep(p[len - 1]) {
            tail.push(b'\\');
        }
        let tail = utf8(tail);
        match device {
            None => {
                if is_absolute {
                    format!("\\{tail}")
                } else {
                    tail
                }
            }
            Some(d) => {
                let d = utf8(d);
                if is_absolute {
                    format!("{d}\\{tail}")
                } else {
                    format!("{d}{tail}")
                }
            }
        }
    }

    /// Node `posix.normalize`.
    pub(super) fn posix_normalize(path: &str) -> String {
        if path.is_empty() {
            return ".".to_string();
        }
        let p = path.as_bytes();
        let is_absolute = p[0] == b'/';
        let trailing_separator = p[p.len() - 1] == b'/';

        let mut out = utf8(normalize_string(p, !is_absolute, b'/', is_posix_sep));
        if out.is_empty() {
            if is_absolute {
                return "/".to_string();
            }
            return if trailing_separator {
                "./".to_string()
            } else {
                ".".to_string()
            };
        }
        if trailing_separator {
            out.push('/');
        }
        if is_absolute {
            format!("/{out}")
        } else {
            out
        }
    }

    /// Node `win32.join`.
    pub(super) fn win32_join(args: &[&str]) -> String {
        if args.is_empty() {
            return ".".to_string();
        }
        let mut joined: Option<String> = None;
        let mut first_part: &str = "";
        for arg in args {
            if arg.is_empty() {
                continue;
            }
            match &mut joined {
                None => {
                    joined = Some((*arg).to_string());
                    first_part = arg;
                }
                Some(j) => {
                    j.push('\\');
                    j.push_str(arg);
                }
            }
        }
        let Some(mut joined) = joined else {
            return ".".to_string();
        };

        // Make sure the joined path doesn't start with two slashes, because
        // normalize() would mistake it for a UNC path — unless the first part
        // really is a UNC path (`\\x...`).
        let fb = first_part.as_bytes();
        let mut needs_replace = true;
        let mut slash_count = 0usize;
        if fb[0] == b'\\' {
            slash_count += 1;
            if fb.len() > 1 && fb[1] == b'\\' {
                slash_count += 1;
                if fb.len() > 2 {
                    if fb[2] == b'\\' {
                        slash_count += 1;
                    } else {
                        needs_replace = false;
                    }
                }
            }
        }
        if needs_replace {
            let jb = joined.as_bytes();
            while slash_count < jb.len() && is_win32_sep(jb[slash_count]) {
                slash_count += 1;
            }
            if slash_count >= 2 {
                joined = format!("\\{}", &joined[slash_count..]);
            }
        }

        win32_normalize(&joined)
    }

    /// Node `posix.join`.
    pub(super) fn posix_join(args: &[&str]) -> String {
        if args.is_empty() {
            return ".".to_string();
        }
        let mut joined: Option<String> = None;
        for arg in args {
            if arg.is_empty() {
                continue;
            }
            match &mut joined {
                None => joined = Some((*arg).to_string()),
                Some(j) => {
                    j.push('/');
                    j.push_str(arg);
                }
            }
        }
        match joined {
            None => ".".to_string(),
            Some(j) => posix_normalize(&j),
        }
    }

    /// Node `win32.resolve`.
    ///
    /// `cwd` stands in for `process.cwd()`. Node additionally consults the
    /// Windows per-drive cwd (`process.env["=C:"]`) before that fallback; the
    /// branch is reachable only when every argument is drive-relative or
    /// device-mismatched (e.g. `resolve("C:\\x", "D:y")`), which `resolvePath`
    /// never produces for the captured corpus, so `cwd` is used for both.
    pub(super) fn win32_resolve(args: &[&str], cwd: &str) -> String {
        let mut resolved_device: Vec<u8> = Vec::new();
        let mut resolved_tail: Vec<u8> = Vec::new();
        let mut resolved_absolute = false;

        let mut i = args.len() as isize - 1;
        while i >= -1 {
            let path: Vec<u8> = if i >= 0 {
                let arg = args[i as usize].as_bytes();
                // Skip empty entries
                if arg.is_empty() {
                    i -= 1;
                    continue;
                }
                arg.to_vec()
            } else if resolved_device.is_empty() {
                cwd.as_bytes().to_vec()
            } else {
                // Verify that the cwd actually points at our drive; if not,
                // default to the drive's root. (`path.slice(0, 2)` in Node.)
                let c = cwd.as_bytes();
                let same_drive = c[..c.len().min(2)].eq_ignore_ascii_case(&resolved_device);
                if !same_drive && c.len() > 2 && c[2] == b'\\' {
                    let mut root = resolved_device.clone();
                    root.push(b'\\');
                    root
                } else {
                    c.to_vec()
                }
            };

            let p = &path[..];
            let len = p.len();
            let mut root_end = 0usize;
            let mut device: Vec<u8> = Vec::new();
            let mut is_absolute = false;
            // JS `charCodeAt(0)` on an empty string is NaN, which matches
            // nothing below; 0 stands in for it.
            let code = p.first().copied().unwrap_or(0);

            // Try to match a root
            if len == 1 {
                if is_win32_sep(code) {
                    root_end = 1;
                    is_absolute = true;
                }
            } else if is_win32_sep(code) {
                is_absolute = true;
                if is_win32_sep(p[1]) {
                    if let Some((d, re, _)) = win32_unc_root(p) {
                        device = d;
                        root_end = re;
                    }
                } else {
                    root_end = 1;
                }
            } else if is_windows_device_root(code) && p[1] == b':' {
                device = p[0..2].to_vec();
                root_end = 2;
                if len > 2 && is_win32_sep(p[2]) {
                    is_absolute = true;
                    root_end = 3;
                }
            }

            if !device.is_empty() {
                if !resolved_device.is_empty() {
                    if !device.eq_ignore_ascii_case(&resolved_device) {
                        // This path points to another device, so it is not applicable
                        i -= 1;
                        continue;
                    }
                } else {
                    resolved_device = device;
                }
            }

            if resolved_absolute {
                if !resolved_device.is_empty() {
                    break;
                }
            } else {
                let mut next = p[root_end..].to_vec();
                next.push(b'\\');
                next.extend_from_slice(&resolved_tail);
                resolved_tail = next;
                resolved_absolute = is_absolute;
                if is_absolute && !resolved_device.is_empty() {
                    break;
                }
            }
            i -= 1;
        }

        let tail = utf8(normalize_string(
            &resolved_tail,
            !resolved_absolute,
            b'\\',
            is_win32_sep,
        ));
        let device = utf8(resolved_device);
        if resolved_absolute {
            format!("{device}\\{tail}")
        } else {
            let out = format!("{device}{tail}");
            if out.is_empty() {
                ".".to_string()
            } else {
                out
            }
        }
    }

    /// Node `posix.resolve`. `cwd` stands in for `process.cwd()`.
    pub(super) fn posix_resolve(args: &[&str], cwd: &str) -> String {
        let mut resolved_path = String::new();
        let mut resolved_absolute = false;

        let mut i = args.len() as isize - 1;
        while i >= 0 && !resolved_absolute {
            let path = args[i as usize];
            i -= 1;
            // Skip empty entries
            if path.is_empty() {
                continue;
            }
            resolved_path = format!("{path}/{resolved_path}");
            resolved_absolute = path.as_bytes()[0] == b'/';
        }

        if !resolved_absolute {
            resolved_path = format!("{cwd}/{resolved_path}");
            resolved_absolute = cwd.as_bytes().first() == Some(&b'/');
        }

        let path = utf8(normalize_string(
            resolved_path.as_bytes(),
            !resolved_absolute,
            b'/',
            is_posix_sep,
        ));
        if resolved_absolute {
            format!("/{path}")
        } else if !path.is_empty() {
            path
        } else {
            ".".to_string()
        }
    }
}

// ===========================================================================
// node:url — fileURLToPath
// ===========================================================================

/// `node:url`'s `fileURLToPath`, reached from `paths.ts:74-76`.
///
/// Node runs the input through the WHATWG URL parser and then converts
/// `url.hostname` + `url.pathname`. Transcribed here is the conversion half
/// (`getPathFromURLWin32` / `getPathFromURLPosix`) plus the slice of the parser
/// that a `file://` URL exercises: file-host state (including the
/// windows-drive-letter-as-host quirk and `localhost` → `""`), path state
/// (`/` **and** `\` are separators, `.`/`%2e` and `..` segments are resolved,
/// `C|` → `C:` for a leading drive letter) and query/fragment stripping.
///
/// Deliberately **not** modelled, because nothing in `path_utils.cases.jsonl`
/// or any realistic Pi input reaches it and each would need a new dependency:
/// IDNA/punycode host decoding (`domainToUnicode`), and re-applying the URL path
/// percent-encode set. The latter is a no-op for the round trip anyway: the
/// parser would encode a character only for `decodeURIComponent` to decode it
/// straight back.
mod file_url {
    use super::{PathError, Platform};

    /// Forbidden domain code points (URL spec), minus `%` which needs the
    /// "invalid percent-encoding" check the parser applies separately.
    fn is_forbidden_host_code_point(c: char) -> bool {
        matches!(
            c,
            '\0' | '\t'
                | '\n'
                | '\r'
                | ' '
                | '#'
                | '/'
                | ':'
                | '<'
                | '>'
                | '?'
                | '@'
                | '['
                | '\\'
                | ']'
                | '^'
                | '|'
        )
    }

    /// A "normalized Windows drive letter" per the URL spec: two code points,
    /// an ASCII alpha followed by `:` (or `|`, which the parser rewrites).
    fn is_windows_drive_letter(s: &str) -> bool {
        let b = s.as_bytes();
        b.len() == 2 && b[0].is_ascii_alphabetic() && (b[1] == b':' || b[1] == b'|')
    }

    /// ASCII-case-insensitive equality against a lowercase literal.
    fn eq_ascii(a: &str, lower: &str) -> bool {
        a.eq_ignore_ascii_case(lower)
    }

    fn is_single_dot(seg: &str) -> bool {
        seg == "." || eq_ascii(seg, "%2e")
    }

    fn is_double_dot(seg: &str) -> bool {
        seg == ".." || eq_ascii(seg, ".%2e") || eq_ascii(seg, "%2e.") || eq_ascii(seg, "%2e%2e")
    }

    /// JS `decodeURIComponent`: percent-decode, then require valid UTF-8.
    fn decode_uri_component(input: &str) -> Result<String, PathError> {
        let b = input.as_bytes();
        let mut out: Vec<u8> = Vec::with_capacity(b.len());
        let mut i = 0usize;
        while i < b.len() {
            if b[i] == b'%' {
                if i + 3 > b.len() {
                    return Err(PathError::UriMalformed);
                }
                let hi = (b[i + 1] as char)
                    .to_digit(16)
                    .ok_or(PathError::UriMalformed)?;
                let lo = (b[i + 2] as char)
                    .to_digit(16)
                    .ok_or(PathError::UriMalformed)?;
                out.push((hi * 16 + lo) as u8);
                i += 3;
            } else {
                out.push(b[i]);
                i += 1;
            }
        }
        String::from_utf8(out).map_err(|_| PathError::UriMalformed)
    }

    /// Percent-decode a host, then lowercase it (ASCII-only stand-in for
    /// domain-to-ASCII). Rejects forbidden host code points.
    fn parse_host(buf: &str) -> Result<String, PathError> {
        let decoded = decode_uri_component(buf).map_err(|_| PathError::InvalidUrl)?;
        if decoded.chars().any(is_forbidden_host_code_point) {
            return Err(PathError::InvalidUrl);
        }
        let lowered = decoded.to_lowercase();
        // The URL parser maps a `file:` host of "localhost" to the empty host.
        Ok(if lowered == "localhost" {
            String::new()
        } else {
            lowered
        })
    }

    /// The `(hostname, pathname)` pair the WHATWG parser would produce for a
    /// string matching `/^file:\/\//`. `pathname` keeps its percent-encoding.
    fn parse_file_url(input: &str) -> Result<(String, String), PathError> {
        let rest = input.strip_prefix("file://").ok_or(PathError::InvalidUrl)?;

        // --- file host state: read until `/`, `\`, `?` or `#`.
        let host_end = rest.find(['/', '\\', '?', '#']).unwrap_or(rest.len());
        let host_buf = &rest[..host_end];
        let (host, path_input) = if is_windows_drive_letter(host_buf) {
            // Spec: validation error, host stays empty and the buffer is
            // re-read as the first path segment (`file://C:/x` = `file:///C:/x`).
            (String::new(), rest)
        } else {
            (parse_host(host_buf)?, &rest[host_end..])
        };

        // --- strip query + fragment: neither is part of `url.pathname`.
        let path_str = &path_input[..path_input.find(['?', '#']).unwrap_or(path_input.len())];

        // --- path start state consumes exactly one leading separator; every
        // remaining `/` or `\` delimits a segment (so `file:////x` keeps `//x`).
        let body = match path_str.as_bytes().first() {
            Some(b'/' | b'\\') => &path_str[1..],
            _ => path_str,
        };

        let mut segments: Vec<String> = Vec::new();
        let mut buf_start = 0usize;
        let bytes = body.as_bytes();
        let mut i = 0usize;
        loop {
            let at_end = i == bytes.len();
            if at_end || bytes[i] == b'/' || bytes[i] == b'\\' {
                let seg = &body[buf_start..i];
                if is_double_dot(seg) {
                    // Shorten, unless the path is a lone drive letter.
                    let locked = segments.len() == 1 && is_windows_drive_letter(&segments[0]);
                    if !locked {
                        segments.pop();
                    }
                    if at_end {
                        segments.push(String::new());
                    }
                } else if is_single_dot(seg) {
                    if at_end {
                        segments.push(String::new());
                    }
                } else {
                    let mut seg = seg.to_string();
                    if segments.is_empty() && is_windows_drive_letter(&seg) {
                        // `C|` → `C:`
                        seg.replace_range(1..2, ":");
                    }
                    segments.push(seg);
                }
                if at_end {
                    break;
                }
                buf_start = i + 1;
            }
            i += 1;
        }

        Ok((host, format!("/{}", segments.join("/"))))
    }

    /// Node `fileURLToPath`.
    pub(super) fn file_url_to_path(input: &str, platform: Platform) -> Result<String, PathError> {
        let (hostname, pathname) = parse_file_url(input)?;
        match platform {
            Platform::Win32 => from_url_win32(&hostname, &pathname),
            Platform::Posix => from_url_posix(&hostname, &pathname, platform),
        }
    }

    /// Node `getPathFromURLWin32`.
    fn from_url_win32(hostname: &str, pathname: &str) -> Result<String, PathError> {
        let b = pathname.as_bytes();
        for n in 0..b.len() {
            if b[n] == b'%' {
                // `codePointAt(n + 2) | 0x20`; out of range reads as 0x20.
                let third = b.get(n + 2).map_or(0x20u32, |&c| u32::from(c) | 0x20);
                let second = b.get(n + 1).copied();
                if (second == Some(b'2') && third == 102) || (second == Some(b'5') && third == 99) {
                    return Err(PathError::InvalidFileUrlPath(
                        "must not include encoded \\ or / characters",
                    ));
                }
            }
        }
        let pathname = decode_uri_component(&pathname.replace('/', "\\"))?;
        if !hostname.is_empty() {
            // A hostname means a UNC path (`domainToUnicode` not modelled).
            return Ok(format!("\\\\{hostname}{pathname}"));
        }
        let pb = pathname.as_bytes();
        let letter = pb.get(1).map_or(0u32, |&c| u32::from(c) | 0x20);
        let sep = pb.get(2).copied();
        if !(0x61..=0x7a).contains(&letter) || sep != Some(b':') {
            return Err(PathError::InvalidFileUrlPath("must be absolute"));
        }
        Ok(pathname[1..].to_string())
    }

    /// Node `getPathFromURLPosix`.
    fn from_url_posix(
        hostname: &str,
        pathname: &str,
        platform: Platform,
    ) -> Result<String, PathError> {
        if !hostname.is_empty() {
            return Err(PathError::InvalidFileUrlHost(platform.name()));
        }
        let b = pathname.as_bytes();
        for n in 0..b.len() {
            if b[n] == b'%' {
                let third = b.get(n + 2).map_or(0x20u32, |&c| u32::from(c) | 0x20);
                if b.get(n + 1).copied() == Some(b'2') && third == 102 {
                    return Err(PathError::InvalidFileUrlPath(
                        "must not include encoded / characters",
                    ));
                }
            }
        }
        decode_uri_component(pathname)
    }
}

// ===========================================================================
// Unit tests
// ===========================================================================
//
// The oracle lives in `tests/path_utils_golden.rs`; these only pin the private
// helpers the fixture cannot reach (`resolveReadPath`'s fallback ladder) and a
// few `node:path` behaviours the fixture covers only indirectly.
#[cfg(test)]
mod tests {
    use super::*;

    fn win32_env() -> PathEnv {
        PathEnv {
            platform: Platform::Win32,
            home_dir: "C:\\oracle\\home".to_string(),
            cwd: "C:\\oracle\\cwd".to_string(),
        }
    }

    fn posix_env() -> PathEnv {
        PathEnv {
            platform: Platform::Posix,
            home_dir: "/oracle/home".to_string(),
            cwd: "/oracle/cwd".to_string(),
        }
    }

    #[test]
    fn posix_resolve_matches_node() {
        let env = posix_env();
        let r = |p: &str| resolve_to_cwd_in(&env, p, "/oracle/cwd").unwrap();
        assert_eq!(r("a/b.txt"), "/oracle/cwd/a/b.txt");
        assert_eq!(r("a/../b.txt"), "/oracle/cwd/b.txt");
        assert_eq!(r(""), "/oracle/cwd");
        assert_eq!(r(".."), "/oracle");
        assert_eq!(r("~/docs/x.txt"), "/oracle/home/docs/x.txt");
        // `~\` is win32-only (paths.ts:69).
        assert_eq!(r("~\\docs"), "/oracle/cwd/~\\docs");
        assert_eq!(r("file:///oracle/url/a%20b.txt"), "/oracle/url/a b.txt");
    }

    #[test]
    fn win32_backslash_tilde_is_platform_gated() {
        assert_eq!(
            expand_path_in(&win32_env(), "~\\docs\\x.txt").unwrap(),
            "C:\\oracle\\home\\docs\\x.txt"
        );
        assert_eq!(
            expand_path_in(&posix_env(), "~\\docs\\x.txt").unwrap(),
            "~\\docs\\x.txt"
        );
    }

    #[test]
    fn only_one_at_prefix_is_stripped() {
        assert_eq!(expand_path_in(&win32_env(), "@@x").unwrap(), "@x");
    }

    #[test]
    fn unc_and_root_relative_win32_paths() {
        let env = win32_env();
        assert_eq!(
            resolve_to_cwd_in(&env, "\\\\server\\share\\a\\..\\b", "C:\\oracle\\cwd").unwrap(),
            "\\\\server\\share\\b"
        );
        // A root-relative path borrows the drive from `process.cwd()`.
        assert_eq!(
            resolve_to_cwd_in(&env, "\\foo\\bar", "C:\\oracle\\cwd").unwrap(),
            "C:\\foo\\bar"
        );
    }

    #[test]
    fn win32_file_url_without_drive_letter_throws() {
        assert_eq!(
            expand_path_in(&win32_env(), "file:///foo/bar"),
            Err(PathError::InvalidFileUrlPath("must be absolute"))
        );
        assert_eq!(
            expand_path_in(&win32_env(), "file:///C:/a%2fb"),
            Err(PathError::InvalidFileUrlPath(
                "must not include encoded \\ or / characters"
            ))
        );
        assert_eq!(
            expand_path_in(&posix_env(), "file://example.com/x"),
            Err(PathError::InvalidFileUrlHost("posix"))
        );
    }

    #[test]
    fn file_url_localhost_and_dot_segments() {
        let env = win32_env();
        assert_eq!(
            expand_path_in(&env, "file://localhost/C:/a/./b/../c.txt").unwrap(),
            "C:\\a\\c.txt"
        );
        // `file://C:/x` — drive letter in host position is re-read as a path.
        assert_eq!(expand_path_in(&env, "file://C:/x").unwrap(), "C:\\x");
    }

    #[test]
    fn am_pm_variant_inserts_narrow_no_break_space() {
        assert_eq!(
            try_macos_screenshot_path("Screenshot at 10.03.15 AM.png"),
            "Screenshot at 10.03.15\u{202F}AM.png"
        );
        // `gi`: every occurrence, original case preserved.
        assert_eq!(
            try_macos_screenshot_path("a AM. b pm."),
            "a\u{202F}AM. b\u{202F}pm."
        );
        // No trailing dot -> no match.
        assert_eq!(try_macos_screenshot_path("at 9 AM"), "at 9 AM");
    }

    #[test]
    fn curly_and_nfd_variants() {
        assert_eq!(
            try_curly_quote_variant("Capture d'e\u{0301}cran"),
            "Capture d\u{2019}e\u{0301}cran"
        );
        assert_eq!(try_nfd_variant("caf\u{00E9}"), "cafe\u{0301}");
        assert_eq!(try_nfd_variant("na\u{00EF}ve"), "nai\u{0308}ve");
        // Already decomposed input is unchanged.
        assert_eq!(try_nfd_variant("cafe\u{0301}"), "cafe\u{0301}");
    }

    #[test]
    fn resolve_read_path_tries_variants_in_order() {
        let env = win32_env();
        let base = "C:\\oracle\\cwd\\";

        // 1. The plain resolved path wins whenever it exists.
        let got = resolve_read_path_in(&env, "a.txt", "C:\\oracle\\cwd", |p| {
            p == format!("{base}a.txt")
        })
        .unwrap();
        assert_eq!(got, format!("{base}a.txt"));

        // 2. AM/PM variant.
        let want = format!("{base}shot 9 AM.png").replace(" AM.", "\u{202F}AM.");
        let got =
            resolve_read_path_in(&env, "shot 9 AM.png", "C:\\oracle\\cwd", |p| p == want).unwrap();
        assert_eq!(got, want);

        // 3. NFD variant.
        let want = format!("{base}cafe\u{0301}.txt");
        let got = resolve_read_path_in(&env, "caf\u{00E9}.txt", "C:\\oracle\\cwd", |p| p == want)
            .unwrap();
        assert_eq!(got, want);

        // 4. Curly-apostrophe variant.
        let want = format!("{base}d\u{2019}x.txt");
        let got = resolve_read_path_in(&env, "d'x.txt", "C:\\oracle\\cwd", |p| p == want).unwrap();
        assert_eq!(got, want);

        // 5. Combined NFD + curly.
        let want = format!("{base}Capture d\u{2019}e\u{0301}cran.png");
        let got =
            resolve_read_path_in(&env, "Capture d'\u{00E9}cran.png", "C:\\oracle\\cwd", |p| {
                p == want
            })
            .unwrap();
        assert_eq!(got, want);

        // 6. Nothing exists -> the plain resolved path, unchanged.
        let got = resolve_read_path_in(
            &env,
            "Capture d'\u{00E9}cran.png",
            "C:\\oracle\\cwd",
            |_| false,
        )
        .unwrap();
        assert_eq!(got, format!("{base}Capture d'\u{00E9}cran.png"));
    }

    #[tokio::test]
    async fn async_twin_agrees_with_the_sync_one() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_string_lossy().into_owned();
        std::fs::write(dir.path().join("present.txt"), b"x").unwrap();

        let sync_hit = resolve_read_path("present.txt", &root).unwrap();
        let async_hit = resolve_read_path_async("present.txt", &root).await.unwrap();
        assert_eq!(sync_hit, async_hit);
        assert!(file_exists(&sync_hit));
        assert!(path_exists(&async_hit).await);

        let sync_miss = resolve_read_path("absent.txt", &root).unwrap();
        let async_miss = resolve_read_path_async("absent.txt", &root).await.unwrap();
        assert_eq!(sync_miss, async_miss);
        assert!(!file_exists(&sync_miss));
    }

    #[test]
    fn unicode_space_set_is_exactly_the_ts_regex() {
        let env = win32_env();
        for c in [
            '\u{00A0}', '\u{2000}', '\u{2005}', '\u{200A}', '\u{202F}', '\u{205F}', '\u{3000}',
        ] {
            assert_eq!(
                expand_path_in(&env, &format!("a{c}b")).unwrap(),
                "a b",
                "U+{:04X} should normalize",
                c as u32
            );
        }
        for c in ['\u{1680}', '\u{200B}', '\u{2028}', '\u{FEFF}'] {
            assert_eq!(
                expand_path_in(&env, &format!("a{c}b")).unwrap(),
                format!("a{c}b"),
                "U+{:04X} is outside UNICODE_SPACES",
                c as u32
            );
        }
    }

    #[test]
    fn current_env_wires_the_seams_through() {
        assert_eq!(PathEnv::current().platform, Platform::current());
        assert_eq!(expand_path("~").unwrap(), home_dir());
    }
}
