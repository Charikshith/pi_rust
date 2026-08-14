//! Oracle for `core/tools/find.ts`.
//!
//! Four independent gates, all against bytes captured from real Pi by
//! `scripts/gen-tools-oracle.mjs`:
//!
//! 1. `tests/fixtures/pi/tools/schemas/find.json` — `parameters` must serialize
//!    byte-identically.
//! 2. `tests/fixtures/pi/tools/strings/find.json` — name / label / description /
//!    promptSnippet, plus the three `null`/`false` fields.
//! 3. The nine `"tool":"find"` rows of
//!    `tests/fixtures/pi/tools/exec.corpus.jsonl`, replayed against a `tempfile`
//!    rebuild of `exec.tree.json` with the tool's `cwd` set to the tree root —
//!    i.e. Branch B, the real `fd`. `content` is compared after the *same*
//!    order-normalization the oracle applied, `details` as exact JSON text, and
//!    error rows as exact `Error.message`.
//! 4. Branch A (a custom `operations.glob`), which the corpus cannot reach.
//!    Pinned here because its result-limit notice wording differs from Branch B's.
//!
//! A failure means the Rust port diverged from Pi; the fix is the port, never the
//! assertion.
//!
//! # Two environment facts this file pins deliberately
//!
//! **`fd` must be reachable, and a miss fails loudly.** pirust's managed directory
//! is `~/.pirust/agent/bin` and the downloader is deferred (`binaries.rs`), so on a
//! machine where only Pi has fetched `fd` there is nothing for `ensure_tool` to
//! find. [`locate_fd`] therefore looks in `PIRUST_TEST_FD`, then Pi's own
//! `~/.pi/agent/bin`, then `PATH`, and **panics** if none has it. It never copies a
//! binary into pirust's directory and the port grows no `~/.pi` fallback.
//!
//! **`fd`'s output separator is ambient.** `fd` prints `/`-separated paths when
//! `MSYSTEM` is set (win32 only; it is how MSYS2/Git Bash is detected) and native
//! `\`-separated paths otherwise. That choice is observable through Pi: with `/`
//! output `line.startsWith(searchPath)` misses and relativization goes through
//! `path.relative`, which strips a directory's trailing separator so the
//! `hadTrailingSlash` fixup re-adds exactly one — `nested/`. With `\` output the
//! fast path keeps the separator and the fixup adds a second one — `nested//`. The
//! corpus was captured from Git Bash (`nested/` in the `**` row), so the replay
//! sets `MSYSTEM` on the `fd` child to reproduce the recorded environment. Pi
//! itself passes no environment at all and inherits `process.env`; that default is
//! what production uses.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use pirust_agent_core::types::{AgentTool, AgentToolResult, AgentToolUpdateCallback};
use pirust_tools::find::{
    create_find_tool_definition, FdSpawn, FindOperations, FindToolOptions, GlobOptions,
};
use serde_json::{Map, Value};
use tokio_util::sync::CancellationToken;

/// Number of `"tool":"find"` rows in the captured corpus. Asserted so a truncated
/// fixture cannot silently weaken the suite.
const FIND_ROW_COUNT: usize = 9;

/// Env var pointing at an `fd` binary, checked first by [`locate_fd`].
const FD_ENV: &str = "PIRUST_TEST_FD";

fn fixture(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/pi/tools")
        .join(relative)
}

fn read_fixture(relative: &str) -> String {
    let path = fixture(relative);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read fixture {}: {e}", path.display()))
}

fn noop_update() -> AgentToolUpdateCallback {
    Arc::new(|_| {})
}

// ===========================================================================
// 1. Schema bytes
// ===========================================================================

#[test]
fn parameters_match_the_captured_schema_bytes() {
    let want = read_fixture("schemas/find.json").trim_end().to_string();
    let definition = create_find_tool_definition("C:\\anywhere", FindToolOptions::default());
    let got = serde_json::to_string(&definition.parameters).expect("serialize parameters");

    assert_eq!(
        got, want,
        "find parameters must be byte-identical to schemas/find.json\n  expected: {want}\n  \
         actual:   {got}"
    );
}

// ===========================================================================
// 2. Prompt strings
// ===========================================================================

#[test]
fn strings_match_the_captured_metadata() {
    let raw = read_fixture("strings/find.json");
    let want: Value = serde_json::from_str(&raw).expect("parse strings/find.json");
    let definition = create_find_tool_definition("C:\\anywhere", FindToolOptions::default());

    for (field, expected, actual) in [
        (
            "name",
            want["name"].as_str(),
            Some(definition.name.as_str()),
        ),
        (
            "label",
            want["label"].as_str(),
            Some(definition.label.as_str()),
        ),
        (
            "description",
            want["description"].as_str(),
            Some(definition.description.as_str()),
        ),
        (
            "promptSnippet",
            want["promptSnippet"].as_str(),
            definition.prompt_snippet.as_deref(),
        ),
    ] {
        assert_eq!(
            actual, expected,
            "{field} diverged\n  expected: {expected:?}\n  actual:   {actual:?}"
        );
    }

    assert!(
        want["promptGuidelines"].is_null() && definition.prompt_guidelines.is_none(),
        "find has no promptGuidelines"
    );
    assert!(
        want["executionMode"].is_null() && definition.execution_mode.is_none(),
        "find has no executionMode override"
    );
    assert_eq!(
        want["hasPrepareArguments"].as_bool(),
        Some(false),
        "the fixture says find has no prepareArguments"
    );
    assert!(definition.prepare_arguments.is_none());
}

// ===========================================================================
// 3. Execution corpus — Branch B (real `fd`)
// ===========================================================================

/// Rebuild `exec.tree.json` verbatim: all directories, then all files as UTF-8 with
/// LF newlines and no BOM (`fs::write` writes the JSON string's bytes unchanged).
/// `outAfterWrites` is deliberately ignored — it describes the state *after* the
/// `write`-tool rows, which the oracle ran last, well after every `find` row.
fn build_fixture_tree(root: &Path) {
    let tree: Value = serde_json::from_str(&read_fixture("exec.tree.json")).expect("parse tree");

    // The corpus was captured on win32 with `fd` available and no `.git` above the
    // tree; all three change what `find` does, so a mismatch must be loud.
    assert_eq!(
        tree["fdAvailable"].as_bool(),
        Some(true),
        "the corpus was captured with fd available"
    );
    assert_eq!(
        tree["insideGitRepo"].as_bool(),
        Some(false),
        "the corpus was captured outside a git repo, i.e. with `--no-require-git`"
    );

    for dir in tree["dirs"].as_array().expect("tree.dirs") {
        let relative = dir.as_str().expect("dir entry is a string");
        std::fs::create_dir_all(root.join(relative))
            .unwrap_or_else(|e| panic!("create dir {relative}: {e}"));
    }

    for (relative, content) in tree["files"].as_object().expect("tree.files") {
        let target = root.join(relative);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).expect("create parent dir");
        }
        let text = content
            .as_str()
            .unwrap_or_else(|| panic!("file {relative} content is not a string"));
        std::fs::write(&target, text.as_bytes())
            .unwrap_or_else(|e| panic!("write file {relative}: {e}"));
    }
}

/// Every `"tool":"find"` row of the corpus, in file order.
fn find_rows() -> Vec<Map<String, Value>> {
    read_fixture("exec.corpus.jsonl")
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str::<Map<String, Value>>(line)
                .unwrap_or_else(|e| panic!("deserialize corpus row failed: {e}\n  {line}"))
        })
        .filter(|row| row.get("tool").and_then(Value::as_str) == Some("find"))
        .collect()
}

/// Locate a real `fd`, or panic. See the module docs for why this cannot be a
/// silent skip.
fn locate_fd() -> String {
    if let Some(path) = std::env::var_os(FD_ENV) {
        let path = PathBuf::from(path);
        assert!(
            path.is_file(),
            "{FD_ENV} is set to {} but that is not a file",
            path.display()
        );
        return path.to_string_lossy().into_owned();
    }

    let exe = if cfg!(windows) { "fd.exe" } else { "fd" };

    // Pi's own managed directory. Read-only: nothing is copied into pirust's
    // `~/.pirust/agent/bin`, and the port itself must never look here.
    let home_env = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    if let Some(home) = std::env::var_os(home_env) {
        let candidate = PathBuf::from(home)
            .join(".pi")
            .join("agent")
            .join("bin")
            .join(exe);
        if candidate.is_file() {
            return candidate.to_string_lossy().into_owned();
        }
    }

    // PATH, in order.
    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join(exe);
            if candidate.is_file() {
                return candidate.to_string_lossy().into_owned();
            }
        }
    }

    panic!(
        "no `fd` binary found: this test replays real Pi output produced by a real fd, so it \
         cannot be skipped. Set {FD_ENV}=<path to {exe}>, or put {exe} on PATH, or let Pi \
         download it into ~/.pi/agent/bin."
    );
}

/// The spawn seam used by the replay: a located `fd` plus the `MSYSTEM` the corpus
/// was captured with (see the module docs). `fd` consults `MSYSTEM` only on win32,
/// so the value is inert elsewhere.
fn corpus_fd_spawn() -> FdSpawn {
    FdSpawn {
        binary_path: Some(locate_fd()),
        extra_env: vec![("MSYSTEM".to_string(), "MINGW64".to_string())],
    }
}

/// JS `Array.prototype.sort()`'s default comparator: `String(a) < String(b)` by
/// UTF-16 code unit.
fn utf16_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}

/// The order-normalization the oracle applied to every `find` row
/// (`scripts/gen-tools-oracle.mjs:251` `sortLines`, driven from `:1338-1346`).
///
/// `fd` walks directories in parallel, so its emission order is not reproducible;
/// Pi never sorts, so the order is external-binary noise. Both the captured text
/// and the replayed text go through this, and nothing else is touched — in
/// particular the trailing `"\n\n[…notice…]"` is never moved, and the split is on
/// the *first* `"\n\n["` exactly as the oracle does it.
fn normalize_find_text(text: &str) -> String {
    let (body, tail) = match text.find("\n\n[") {
        Some(index) => (&text[..index], &text[index..]),
        None => (text, ""),
    };
    let mut lines: Vec<&str> = body.split('\n').collect();
    lines.sort_by(|a, b| utf16_cmp(a, b));
    format!("{}{tail}", lines.join("\n"))
}

/// Apply [`normalize_find_text`] to a `content` array the way the oracle did:
/// text parts only, everything else untouched.
fn normalize_content(content: &Value) -> Value {
    let Some(parts) = content.as_array() else {
        return content.clone();
    };
    Value::Array(
        parts
            .iter()
            .map(|part| {
                let mut part = part.clone();
                let is_text = part.get("type").and_then(Value::as_str) == Some("text");
                if let (true, Some(text)) = (is_text, part.get("text").and_then(Value::as_str)) {
                    let normalized = normalize_find_text(text);
                    part["text"] = Value::String(normalized);
                }
                part
            })
            .collect(),
    )
}

#[tokio::test]
async fn exec_corpus_rows_match_pi() {
    let rows = find_rows();
    assert_eq!(
        rows.len(),
        FIND_ROW_COUNT,
        "exec.corpus.jsonl should hold all {FIND_ROW_COUNT} captured find rows"
    );

    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_string_lossy().into_owned();
    build_fixture_tree(tmp.path());
    assert_no_git_ancestor(tmp.path());

    let fd = corpus_fd_spawn();

    for (index, row) in rows.iter().enumerate() {
        let case = index + 1;
        let note = row["note"].as_str().unwrap_or("");
        let args = row["args"].clone();
        // The oracle sets `orderNormalized` on exactly the successful rows
        // (`gen-tools-oracle.mjs:1336-1337`); an error row has no `content` to sort.
        assert_eq!(
            row.get("orderNormalized")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            row["ok"].as_bool().expect("ok"),
            "case {case}: every successful find row carries orderNormalized"
        );
        // Every captured row ran with `cwd` = the tree root, recorded as the
        // placeholder `{TMPROOT}`.
        let cwd = row["cwd"]
            .as_str()
            .expect("cwd")
            .replace("{TMPROOT}", &root);
        assert_eq!(
            cwd, root,
            "case {case}: unexpected captured cwd {:?}",
            row["cwd"]
        );

        let definition = create_find_tool_definition(
            cwd.clone(),
            FindToolOptions {
                operations: None,
                fd: fd.clone(),
            },
        );
        let actual = definition
            .execute(
                &format!("call_{case}"),
                args.clone(),
                CancellationToken::new(),
                noop_update(),
            )
            .await;

        let ok = row["ok"].as_bool().expect("ok");
        if ok {
            let result: AgentToolResult = actual.unwrap_or_else(|e| {
                panic!(
                    "case {case} find ({note})\n  args:     {args}\n  \
                     expected: content {}\n  actual:   Err({e})",
                    row["content"]
                )
            });

            let want_content = serde_json::to_string(&normalize_content(&row["content"]))
                .expect("stringify content");
            let got_content = serde_json::to_string(&normalize_content(
                &serde_json::to_value(&result.content).expect("content to value"),
            ))
            .expect("serialize content");
            assert_eq!(
                got_content, want_content,
                "case {case} find ({note}): content diverged\n  args:     {args}\n  \
                 expected: {want_content}\n  actual:   {got_content}"
            );

            // `details: null` in the corpus is Pi's `details: undefined`.
            let want_details = serde_json::to_string(&row["details"]).expect("stringify details");
            let got_details = serde_json::to_string(&result.details).expect("serialize details");
            assert_eq!(
                got_details, want_details,
                "case {case} find ({note}): details diverged\n  args:     {args}\n  \
                 expected: {want_details}\n  actual:   {got_details}"
            );
        } else {
            let want = row["error"]
                .as_str()
                .expect("!ok row without an error message")
                .replace("{TMPROOT}", &root);
            let err = actual.err().unwrap_or_else(|| {
                panic!("case {case} find ({note}): Pi threw {want:?}, port returned Ok")
            });
            assert_eq!(
                err.to_string(),
                want,
                "case {case} find ({note}): error message diverged\n  args:     {args}\n  \
                 expected: Err({want})\n  actual:   Err({err})"
            );
        }
    }
}

/// `--no-require-git` is emitted only outside a git repo, and the corpus was
/// captured outside one. A `.git` anywhere above the temp tree would silently change
/// which `.gitignore` files `fd` honours, so check it rather than debug it later.
fn assert_no_git_ancestor(start: &Path) {
    let mut current = Some(start);
    while let Some(dir) = current {
        assert!(
            !dir.join(".git").exists(),
            "{} has a .git entry, so the replay would run inside a git repo while the corpus \
             was captured outside one",
            dir.display()
        );
        current = dir.parent();
    }
}

/// The `**` row is the only one that can tell the trailing-separator fixup apart
/// from dropping it, so it gets its own named guard: without it, `nested/` would be
/// reported as a plain `nested` and nothing else in the corpus would notice.
#[test]
fn the_directory_marker_row_is_still_in_the_corpus() {
    let row = find_rows()
        .into_iter()
        .find(|row| row["args"]["pattern"].as_str() == Some("**"))
        .expect("the `**` row pins fd's directory marker surviving relativization");

    let text = row["content"][0]["text"]
        .as_str()
        .expect("the `**` row has text content");
    assert!(
        text.split('\n').any(|line| line == "nested/"),
        "the captured `**` row no longer contains the `nested/` directory marker: {text:?}"
    );
}

/// The `nested/*.txt` row records a genuine win32 outcome — `fd`'s globber treats
/// only `/` as a separator while the candidate path uses `\`, so a path-shaped
/// pattern matches nothing. Guard the captured expectation so nobody "fixes" it by
/// translating separators.
#[test]
fn the_win32_path_pattern_row_is_still_empty() {
    let row = find_rows()
        .into_iter()
        .find(|row| row["args"]["pattern"].as_str() == Some("nested/*.txt"))
        .expect("the `nested/*.txt` row pins the win32 --full-path outcome");

    assert_eq!(
        row["content"][0]["text"].as_str(),
        Some("No files found matching pattern"),
        "the captured win32 outcome for a '/'-containing pattern changed"
    );
    assert!(
        row["note"]
            .as_str()
            .unwrap_or_default()
            .contains("PLATFORM-DEPENDENT OUTCOME"),
        "the row lost its platform note"
    );
}

// ===========================================================================
// 4. Branch A — a custom `operations.glob`
// ===========================================================================

/// Records what `execute` asked for and replays a fixed answer.
struct ScriptedOps {
    exists: bool,
    results: Vec<String>,
    seen: std::sync::Mutex<Vec<String>>,
}

impl ScriptedOps {
    fn new(exists: bool, results: Vec<&str>) -> Arc<Self> {
        Arc::new(Self {
            exists,
            results: results.into_iter().map(str::to_string).collect(),
            seen: std::sync::Mutex::new(Vec::new()),
        })
    }
}

#[async_trait]
impl FindOperations for ScriptedOps {
    async fn exists(&self, _absolute_path: &str) -> bool {
        self.exists
    }

    async fn glob(&self, pattern: &str, cwd: &str, options: GlobOptions<'_>) -> Vec<String> {
        self.seen.lock().expect("lock").push(format!(
            "{pattern}|{cwd}|{}|{}",
            options.ignore.join(","),
            options.limit
        ));
        self.results.clone()
    }
}

async fn run_branch_a(
    ops: Arc<ScriptedOps>,
    args: Value,
    cwd: &str,
) -> Result<AgentToolResult, pirust_agent_core::types::ToolError> {
    let definition = create_find_tool_definition(
        cwd.to_string(),
        FindToolOptions {
            operations: Some(ops),
            fd: FdSpawn::default(),
        },
    );
    definition
        .execute("call_a", args, CancellationToken::new(), noop_update())
        .await
}

#[tokio::test]
async fn branch_a_limit_notice_has_no_hint_suffix() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_string_lossy().into_owned();
    let search = tmp.path().join("src").to_string_lossy().into_owned();

    // Two results with `limit: 2` trips `relativized.length >= effectiveLimit`.
    let ops = ScriptedOps::new(
        true,
        vec![
            &format!("{search}{}a.ts", std::path::MAIN_SEPARATOR),
            &format!("{search}{}b.ts", std::path::MAIN_SEPARATOR),
        ],
    );
    let result = run_branch_a(
        Arc::clone(&ops),
        serde_json::json!({ "pattern": "*.ts", "path": "src", "limit": 2 }),
        &root,
    )
    .await
    .expect("branch A resolves");

    let want = "a.ts\nb.ts\n\n[2 results limit reached]";
    let got = serde_json::to_value(&result.content).expect("content to value");
    let got_text = got[0]["text"].as_str().expect("text content");
    assert_eq!(
        got_text, want,
        "branch A's notice must NOT carry Branch B's `Use limit=… for more, or refine pattern` \
         suffix\n  expected: {want:?}\n  actual:   {got_text:?}"
    );
    assert_eq!(
        serde_json::to_string(&result.details).expect("details"),
        r#"{"resultLimitReached":2}"#
    );

    // `ops.glob` is called with the search root, Pi's two ignore globs and the
    // unclamped effective limit (`find.ts:164-167`).
    let seen = ops.seen.lock().expect("lock").clone();
    assert_eq!(
        seen,
        vec![format!("*.ts|{search}|**/node_modules/**,**/.git/**|2")]
    );
}

#[tokio::test]
async fn branch_a_empty_and_missing_path() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_string_lossy().into_owned();

    // Empty result set -> the shared no-match text, `details` undefined.
    let result = run_branch_a(
        ScriptedOps::new(true, vec![]),
        serde_json::json!({ "pattern": "*.ts" }),
        &root,
    )
    .await
    .expect("branch A resolves");
    let content = serde_json::to_value(&result.content).expect("content to value");
    assert_eq!(
        content[0]["text"].as_str(),
        Some("No files found matching pattern")
    );
    assert_eq!(result.details, Value::Null);

    // `!exists` -> `Path not found: <resolved absolute path>`. Branch B has no such
    // check, which is why the missing-directory corpus row reports fd's stderr
    // instead.
    let err = run_branch_a(
        ScriptedOps::new(false, vec!["whatever"]),
        serde_json::json!({ "pattern": "*.ts", "path": "nope" }),
        &root,
    )
    .await
    .expect_err("branch A rejects for a missing path");
    let want = format!(
        "Path not found: {}",
        tmp.path().join("nope").to_string_lossy()
    );
    assert_eq!(err.to_string(), want);
}

/// A path that is *not* under the search root goes through `path.relative`, and a
/// `limit` below the result count still only compares `>=`.
#[tokio::test]
async fn branch_a_relativizes_outside_paths() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_string_lossy().into_owned();
    let outside = tmp.path().join("out").join("x.ts");

    let result = run_branch_a(
        ScriptedOps::new(true, vec![&outside.to_string_lossy()]),
        serde_json::json!({ "pattern": "*.ts", "path": "src" }),
        &root,
    )
    .await
    .expect("branch A resolves");

    let content = serde_json::to_value(&result.content).expect("content to value");
    assert_eq!(content[0]["text"].as_str(), Some("../out/x.ts"));
    assert_eq!(result.details, Value::Null);
}
