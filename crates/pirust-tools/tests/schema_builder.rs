//! Byte oracle for the TypeBox key-order schema builder (`definition::schema`).
//!
//! Every one of Pi's seven built-in tools declares its `parameters` with TypeBox
//! and ships `JSON.stringify(parameters)` to the provider verbatim. The fixtures
//! in `tests/fixtures/pi/tools/schemas/` are those literal bytes, captured from
//! real Pi by `scripts/gen-tools-oracle.mjs`.
//!
//! This test rebuilds all seven schemas with the shared helpers and asserts the
//! serialized bytes are unchanged. It is deliberate belt-and-braces: each tool
//! module asserts its own schema too, but proving the *builder* can express all
//! seven (including `edit`'s nested array-of-objects and `ls`'s absent
//! `required`) pins the ordering contract in one place. A failure here means the
//! builder diverged from TypeBox — fix the builder, never the assertion.

use std::path::PathBuf;

use pirust_tools::definition::schema::{
    array_prop, boolean_prop, number_prop, object_schema, optional, required, string_prop,
};
use serde_json::Value;

/// Workspace-root fixtures dir (`CARGO_MANIFEST_DIR` = crates/pirust-tools).
fn fixture(tool: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/pi/tools/schemas")
        .join(format!("{tool}.json"));
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read fixture {}: {e}", path.display()));
    // The capture script writes one JSON line plus a trailing newline; strip only
    // that line ending so the comparison is against Pi's exact stringify output.
    let trimmed = raw.strip_suffix('\n').unwrap_or(&raw);
    let trimmed = trimmed.strip_suffix('\r').unwrap_or(trimmed);
    trimmed.to_string()
}

/// Report the first byte position at which `got` diverges from `want`.
fn first_diff(want: &str, got: &str) -> String {
    let wb = want.as_bytes();
    let gb = got.as_bytes();
    let n = wb.len().min(gb.len());
    let mut i = 0;
    while i < n && wb[i] == gb[i] {
        i += 1;
    }
    format!(
        "first diff at byte {i}:\n  want …{}\n  got  …{}",
        &want[i..want.len().min(i + 60)],
        &got[i..got.len().min(i + 60)],
    )
}

fn assert_matches_pi(tool: &str, schema: &Value) {
    let want = fixture(tool);
    let got = serde_json::to_string(schema).expect("serialize schema");
    assert_eq!(
        got,
        want,
        "{tool}.json is not byte-identical\n{}",
        first_diff(&want, &got)
    );
}

/// `read.ts:19-27` — one required + two optional numbers.
fn read_schema() -> Value {
    object_schema([
        required(
            "path",
            string_prop("Path to the file to read (relative or absolute)"),
        ),
        optional(
            "offset",
            number_prop("Line number to start reading from (1-indexed)"),
        ),
        optional("limit", number_prop("Maximum number of lines to read")),
    ])
}

/// `bash.ts` — required command + optional timeout.
fn bash_schema() -> Value {
    object_schema([
        required("command", string_prop("Bash command to execute")),
        optional(
            "timeout",
            number_prop("Timeout in seconds (optional, no default timeout)"),
        ),
    ])
}

/// `edit.ts:33-56` — nested `Type.Array(Type.Object(...))`: the inner object
/// carries its own `required`, and the array's `description` follows `items`.
fn edit_schema() -> Value {
    object_schema([
        required(
            "path",
            string_prop("Path to the file to edit (relative or absolute)"),
        ),
        required(
            "edits",
            array_prop(
                object_schema([
                    required(
                        "oldText",
                        string_prop(concat!(
                            "Exact text for one targeted replacement. It must be unique in the ",
                            "original file and must not overlap with any other edits[].oldText in ",
                            "the same call."
                        )),
                    ),
                    required(
                        "newText",
                        string_prop("Replacement text for this targeted edit."),
                    ),
                ]),
                concat!(
                    "One or more targeted replacements. Each edit is matched against the original ",
                    "file, not incrementally. Do not include overlapping or nested edits. If two ",
                    "changes touch the same block or nearby lines, merge them into one edit instead."
                ),
            ),
        ),
    ])
}

/// `write.ts` — both properties required.
fn write_schema() -> Value {
    object_schema([
        required(
            "path",
            string_prop("Path to the file to write (relative or absolute)"),
        ),
        required("content", string_prop("Content to write to the file")),
    ])
}

/// `grep.ts` — seven properties interleaving string/boolean/number.
fn grep_schema() -> Value {
    object_schema([
        required(
            "pattern",
            string_prop("Search pattern (regex or literal string)"),
        ),
        optional(
            "path",
            string_prop("Directory or file to search (default: current directory)"),
        ),
        optional(
            "glob",
            string_prop("Filter files by glob pattern, e.g. '*.ts' or '**/*.spec.ts'"),
        ),
        optional(
            "ignoreCase",
            boolean_prop("Case-insensitive search (default: false)"),
        ),
        optional(
            "literal",
            boolean_prop("Treat pattern as literal string instead of regex (default: false)"),
        ),
        optional(
            "context",
            number_prop("Number of lines to show before and after each match (default: 0)"),
        ),
        optional(
            "limit",
            number_prop("Maximum number of matches to return (default: 100)"),
        ),
    ])
}

/// `find.ts` — required glob pattern + two optionals.
fn find_schema() -> Value {
    object_schema([
        required(
            "pattern",
            string_prop(
                "Glob pattern to match files, e.g. '*.ts', '**/*.json', or 'src/**/*.spec.ts'",
            ),
        ),
        optional(
            "path",
            string_prop("Directory to search in (default: current directory)"),
        ),
        optional(
            "limit",
            number_prop("Maximum number of results (default: 1000)"),
        ),
    ])
}

/// `ls.ts:14-17` — both properties optional, so TypeBox emits **no** `required`.
fn ls_schema() -> Value {
    object_schema([
        optional(
            "path",
            string_prop("Directory to list (default: current directory)"),
        ),
        optional(
            "limit",
            number_prop("Maximum number of entries to return (default: 500)"),
        ),
    ])
}

#[test]
fn read_schema_matches_pi() {
    assert_matches_pi("read", &read_schema());
}

#[test]
fn bash_schema_matches_pi() {
    assert_matches_pi("bash", &bash_schema());
}

#[test]
fn edit_schema_matches_pi() {
    assert_matches_pi("edit", &edit_schema());
}

#[test]
fn write_schema_matches_pi() {
    assert_matches_pi("write", &write_schema());
}

#[test]
fn grep_schema_matches_pi() {
    assert_matches_pi("grep", &grep_schema());
}

#[test]
fn find_schema_matches_pi() {
    assert_matches_pi("find", &find_schema());
}

#[test]
fn ls_schema_matches_pi() {
    assert_matches_pi("ls", &ls_schema());
}

/// All seven at once, so a missing fixture cannot silently pass.
#[test]
fn all_seven_builtin_schemas_match_pi() {
    let all: [(&str, Value); 7] = [
        ("read", read_schema()),
        ("bash", bash_schema()),
        ("edit", edit_schema()),
        ("write", write_schema()),
        ("grep", grep_schema()),
        ("find", find_schema()),
        ("ls", ls_schema()),
    ];
    assert_eq!(all.len(), 7);
    for (tool, schema) in &all {
        assert_matches_pi(tool, schema);
    }
}
