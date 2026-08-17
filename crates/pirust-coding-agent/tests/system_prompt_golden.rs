//! Pi oracle for [`pirust_coding_agent::system_prompt`] (feat-005 Wave 4a).
//!
//! Replays every record of `tests/fixtures/pi/sdk/system-prompt.cases.jsonl` —
//! captured by executing real Pi's `buildSystemPrompt()` — and asserts byte-identical
//! output. The oracle pins `PI_PACKAGE_DIR` to the sentinel `C:\oracle\pkg`
//! (`scripts/gen-sdk-oracle.mjs`); this test injects the same sentinel via
//! `build_system_prompt_with_paths` rather than through `get_package_dir()`, since
//! pirust's real package dir is unrelated to Pi's and comparing it would fail for a
//! reason that is not a defect (see `config.rs`'s `get_package_dir` module docs).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use pirust_coding_agent::system_prompt::{
    build_system_prompt_with_paths, BuildSystemPromptOptions,
};
use serde_json::Value;

const README_PATH: &str = "C:\\oracle\\pkg\\README.md";
const DOCS_PATH: &str = "C:\\oracle\\pkg\\docs";
const EXAMPLES_PATH: &str = "C:\\oracle\\pkg\\examples";

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/pi/sdk/system-prompt.cases.jsonl")
}

fn load_records() -> Vec<Value> {
    let path = fixture_path();
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read fixture {}: {error}", path.display()));
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .enumerate()
        .map(|(index, line)| {
            serde_json::from_str(line)
                .unwrap_or_else(|error| panic!("line {}: {error}\n  {line}", index + 1))
        })
        .collect()
}

fn string_vec(value: &Value, key: &str) -> Option<Vec<String>> {
    value
        .get(key)?
        .as_array()?
        .iter()
        .map(|v| v.as_str().map(str::to_string))
        .collect()
}

fn tool_snippets(value: &Value) -> Option<HashMap<String, String>> {
    let obj = value.get("toolSnippets")?.as_object()?;
    Some(
        obj.iter()
            .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string()))
            .collect(),
    )
}

fn context_files(value: &Value) -> Option<Vec<(String, String)>> {
    let arr = value.get("contextFiles")?.as_array()?;
    Some(
        arr.iter()
            .map(|f| {
                (
                    f["path"].as_str().unwrap_or_default().to_string(),
                    f["content"].as_str().unwrap_or_default().to_string(),
                )
            })
            .collect(),
    )
}

#[test]
fn every_system_prompt_record_matches_pi() {
    let records = load_records();
    assert_eq!(
        records.len(),
        11,
        "fixture record count changed — update this assertion deliberately"
    );

    let mut failures = Vec::new();
    for record in &records {
        let note = record["note"].as_str().unwrap();
        let options = &record["options"];
        let expected = record["result"].as_str().unwrap();

        let cwd = options["cwd"].as_str().unwrap();
        let selected_tools = string_vec(options, "selectedTools");
        let snippets = tool_snippets(options);
        let prompt_guidelines = string_vec(options, "promptGuidelines");
        let files = context_files(options);

        let built = BuildSystemPromptOptions {
            custom_prompt: options.get("customPrompt").and_then(Value::as_str),
            selected_tools: selected_tools.as_deref(),
            tool_snippets: snippets.as_ref(),
            prompt_guidelines: prompt_guidelines.as_deref(),
            append_system_prompt: options.get("appendSystemPrompt").and_then(Value::as_str),
            cwd,
            context_files: files.as_deref(),
        };

        let actual = build_system_prompt_with_paths(
            &built,
            Path::new(README_PATH),
            Path::new(DOCS_PATH),
            Path::new(EXAMPLES_PATH),
        );
        if actual != expected {
            failures.push(format!(
                "[{note}]\n  expected: {expected:?}\n  actual:   {actual:?}"
            ));
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));
}
