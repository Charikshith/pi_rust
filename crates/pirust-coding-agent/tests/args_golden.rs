//! Pi-as-oracle suite for `src/args.rs` (spec: `docs/analysis/09-cli-config-spec.md` §3, §4).
//!
//! Nothing here is self-authored: every expectation is a literal value captured by
//! executing real Pi.
//!
//! - `args.corpus.jsonl` — 129 rows of `{name, note?, argv, result}` produced by calling
//!   `parseArgs(argv)` and `JSON.stringify`ing the result. 55 rows are named `trap:*`.
//!   The port is compared by serializing its [`Args`] and asserting JSON equality with
//!   the captured `result`: array order (`messages`, `fileArgs`, `unknownFlags`,
//!   `diagnostics`) is therefore significant, and a field that should be absent but is
//!   emitted (or vice versa) fails.
//! - `help.{plain,color}[.ext].golden` — `printHelp()`'s exact stdout, with and without
//!   extension flags, chalk off and on. Rendered here with **Pi's** identity from
//!   `help.identity.json`, which is what proves the template is byte-exact rather than
//!   merely self-consistent.
//!
//! Every corpus failure is collected and reported together: with 129 rows, aborting on
//! the first one hides how much actually diverged.

use std::collections::BTreeSet;
use std::path::PathBuf;

use pirust_coding_agent::args::{parse_args, render_help, AppIdentity, ExtensionFlag};
use serde::Deserialize;
use serde_json::{Map, Value};

/// Total rows in `args.corpus.jsonl`. Asserted so a shrunken fixture fails loudly.
const CORPUS_ROWS: usize = 129;

/// Rows named `trap:*` — the edge cases that carry the contract.
const CORPUS_TRAP_ROWS: usize = 55;

/// Fixture path, house style (`crates/pirust-agent-core/tests/session_golden.rs`).
fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/pi/cli")
        .join(name)
}

fn read_fixture(name: &str) -> String {
    std::fs::read_to_string(fixture(name))
        .unwrap_or_else(|e| panic!("read fixture {name}: {e} ({})", fixture(name).display()))
}

/// One `args.corpus.jsonl` row.
#[derive(Debug, Deserialize)]
struct CorpusRow {
    name: String,
    #[serde(default)]
    note: Option<String>,
    argv: Vec<String>,
    result: Value,
}

fn load_corpus() -> Vec<CorpusRow> {
    read_fixture("args.corpus.jsonl")
        .lines()
        .filter(|line| !line.trim().is_empty())
        .enumerate()
        .map(|(idx, line)| {
            serde_json::from_str(line)
                .unwrap_or_else(|e| panic!("corpus line {}: {e}\n  {line}", idx + 1))
        })
        .collect()
}

/// Identity in effect when the help goldens were captured (`help.identity.json`).
#[derive(Debug, Deserialize)]
struct HelpIdentity {
    #[serde(rename = "APP_NAME")]
    app_name: String,
    #[serde(rename = "CONFIG_DIR_NAME")]
    config_dir_name: String,
    #[serde(rename = "VERSION")]
    version: String,
    #[serde(rename = "ENV_AGENT_DIR")]
    env_agent_dir: String,
    #[serde(rename = "ENV_SESSION_DIR")]
    env_session_dir: String,
    #[serde(rename = "boldOn")]
    bold_on: String,
    #[serde(rename = "boldOff")]
    bold_off: String,
}

/// The synthetic flag list passed to `printHelp` for the `.ext.` goldens.
#[derive(Debug, Deserialize)]
struct HelpExtFlags {
    flags: Vec<ExtensionFlag>,
}

/// Per-key diff of two JSON objects, so a failure names the offending field instead of
/// dumping two 40-key blobs.
fn diff_objects(want: &Map<String, Value>, got: &Map<String, Value>) -> String {
    let keys: BTreeSet<&String> = want.keys().chain(got.keys()).collect();
    let mut out = String::new();
    for key in keys {
        let (w, g) = (want.get(key), got.get(key));
        if w == g {
            continue;
        }
        let render = |v: Option<&Value>| match v {
            Some(v) => v.to_string(),
            None => "<absent>".to_string(),
        };
        out.push_str(&format!(
            "    {key}: want {}, got {}\n",
            render(w),
            render(g)
        ));
    }
    out
}

#[test]
fn corpus_row_and_trap_counts_are_intact() {
    let corpus = load_corpus();
    assert_eq!(
        corpus.len(),
        CORPUS_ROWS,
        "args.corpus.jsonl should hold all {CORPUS_ROWS} captured cases"
    );
    let traps = corpus
        .iter()
        .filter(|row| row.name.starts_with("trap:"))
        .count();
    assert_eq!(
        traps, CORPUS_TRAP_ROWS,
        "the corpus should hold all {CORPUS_TRAP_ROWS} trap:* cases"
    );
    let unique: BTreeSet<&str> = corpus.iter().map(|row| row.name.as_str()).collect();
    assert_eq!(
        unique.len(),
        corpus.len(),
        "corpus case names must be unique"
    );
}

#[test]
fn parse_args_matches_pi_on_every_corpus_row() {
    let corpus = load_corpus();
    assert_eq!(corpus.len(), CORPUS_ROWS);

    let mut failures: Vec<String> = Vec::new();

    for (idx, row) in corpus.iter().enumerate() {
        let parsed = parse_args(&row.argv);
        let got: Value = serde_json::to_value(&parsed)
            .unwrap_or_else(|e| panic!("{}: serializing Args failed: {e}", row.name));

        if got == row.result {
            continue;
        }

        let detail = match (row.result.as_object(), got.as_object()) {
            (Some(want), Some(got)) => diff_objects(want, got),
            _ => String::new(),
        };
        failures.push(format!(
            "  line {} [{}]\n    argv:  {}\n    note:  {}\n{}    want:  {}\n    got:   {}",
            idx + 1,
            row.name,
            serde_json::to_string(&row.argv).unwrap(),
            row.note.as_deref().unwrap_or("-"),
            detail,
            serde_json::to_string(&row.result).unwrap(),
            serde_json::to_string(&got).unwrap(),
        ));
    }

    assert!(
        failures.is_empty(),
        "{} of {} corpus rows diverge from Pi:\n{}",
        failures.len(),
        corpus.len(),
        failures.join("\n")
    );
}

/// Report the first byte at which `got` diverges from `want`, with line/column and the
/// surrounding text — a 168-line dump makes a one-space drift invisible.
fn first_diff(want: &str, got: &str) -> String {
    let (wb, gb) = (want.as_bytes(), got.as_bytes());
    let n = wb.len().min(gb.len());
    let mut i = 0;
    while i < n && wb[i] == gb[i] {
        i += 1;
    }
    if i == n && wb.len() == gb.len() {
        return "identical".to_string();
    }
    let line = want[..i].matches('\n').count() + 1;
    let column = i - want[..i].rfind('\n').map_or(0, |p| p + 1) + 1;
    let window = |s: &str| {
        let start = s[..i.min(s.len())].rfind('\n').map_or(0, |p| p + 1);
        let end = s[i.min(s.len())..]
            .find('\n')
            .map_or(s.len(), |p| i.min(s.len()) + p);
        s[start..end].escape_debug().to_string()
    };
    format!(
        "want {} bytes / {} lines, got {} bytes / {} lines\n\
         first diff at byte {i} (line {line}, column {column}):\n\
         \x20 want line: {}\n\
         \x20 got  line: {}",
        wb.len(),
        want.lines().count(),
        gb.len(),
        got.lines().count(),
        window(want),
        window(got),
    )
}

#[test]
fn help_goldens_are_byte_identical_with_pis_identity() {
    let identity_json: HelpIdentity = serde_json::from_str(&read_fixture("help.identity.json"))
        .expect("parse help.identity.json");
    // chalk.bold's escapes, from the same capture as the goldens.
    assert_eq!(identity_json.bold_on, "\u{1b}[1m");
    assert_eq!(identity_json.bold_off, "\u{1b}[22m");

    let identity = AppIdentity {
        app_name: &identity_json.app_name,
        config_dir_name: &identity_json.config_dir_name,
        env_agent_dir: &identity_json.env_agent_dir,
        env_session_dir: &identity_json.env_session_dir,
        version: &identity_json.version,
    };

    let ext: HelpExtFlags =
        serde_json::from_str(&read_fixture("help.ext-flags.json")).expect("parse help.ext-flags");
    assert_eq!(
        ext.flags.len(),
        4,
        "the .ext. goldens were captured with 4 flags"
    );

    let cases: [(&str, &[ExtensionFlag], bool); 4] = [
        ("help.plain.golden", &[], false),
        ("help.color.golden", &[], true),
        ("help.plain.ext.golden", &ext.flags, false),
        ("help.color.ext.golden", &ext.flags, true),
    ];

    let mut failures: Vec<String> = Vec::new();
    for (name, flags, color) in cases {
        let want = read_fixture(name);
        let got = render_help(&identity, flags, color);
        if got != want {
            failures.push(format!("  {name}:\n{}", first_diff(&want, &got)));
        }
    }

    assert!(
        failures.is_empty(),
        "{} help golden(s) diverge:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// The colour switch must be the *only* difference between the paired goldens: 7 bolds
/// in the body, 8 once `extensionFlagsText` contributes its heading.
#[test]
fn colour_only_adds_chalk_escapes() {
    let plain = read_fixture("help.plain.golden");
    let color = read_fixture("help.color.golden");
    assert_eq!(
        color.len() - plain.len(),
        7 * ("\u{1b}[1m".len() + "\u{1b}[22m".len())
    );
    assert_eq!(
        color.replace("\u{1b}[1m", "").replace("\u{1b}[22m", ""),
        plain
    );

    let plain_ext = read_fixture("help.plain.ext.golden");
    let color_ext = read_fixture("help.color.ext.golden");
    assert_eq!(
        color_ext.len() - plain_ext.len(),
        8 * ("\u{1b}[1m".len() + "\u{1b}[22m".len())
    );
}
