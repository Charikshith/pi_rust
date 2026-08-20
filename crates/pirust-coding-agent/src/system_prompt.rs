//! Port of `core/system-prompt.ts` — system prompt construction.
//!
//! # Skills are out of scope this wave
//!
//! Pi's `buildSystemPrompt` appends a skills section (`formatSkillsForPrompt`) when the
//! `read` tool is active and the caller passed non-empty `skills`. `skills.ts` is not
//! ported — extensions/`ResourceLoader` are feat-007 — and this wave's only caller
//! (`sdk.rs`, headless one-shot turns) always has an empty skill set. The branch is
//! therefore omitted rather than stubbed with an always-false condition; when feat-007
//! wires real skills, port `format_skills_for_prompt` and restore the two call sites
//! noted below (`:65-67`, `:155-157`).

use std::collections::HashMap;
use std::path::Path;

use crate::config::{get_docs_path, get_examples_path, get_readme_path};

/// `BuildSystemPromptOptions` (`:8-25`).
#[derive(Debug, Clone, Default)]
pub struct BuildSystemPromptOptions<'a> {
    /// Custom system prompt (replaces the default template).
    pub custom_prompt: Option<&'a str>,
    /// Tools to include. Default: `[read, bash, edit, write]`.
    pub selected_tools: Option<&'a [String]>,
    /// One-line tool snippets keyed by tool name.
    pub tool_snippets: Option<&'a HashMap<String, String>>,
    /// Additional guideline bullets appended to the defaults.
    pub prompt_guidelines: Option<&'a [String]>,
    /// Text appended to the system prompt.
    pub append_system_prompt: Option<&'a str>,
    /// Working directory.
    pub cwd: &'a str,
    /// Pre-loaded project context files, `(path, content)`.
    pub context_files: Option<&'a [(String, String)]>,
}

/// `buildSystemPrompt(options)` (`:28-162`), using the real `get_readme_path` /
/// `get_docs_path` / `get_examples_path` (production package dir).
pub fn build_system_prompt(options: &BuildSystemPromptOptions<'_>) -> String {
    build_system_prompt_with_paths(
        options,
        &get_readme_path(),
        &get_docs_path(),
        &get_examples_path(),
    )
}

/// Core of `buildSystemPrompt`, with the three package-relative paths injected — this is
/// what the golden test drives, since `get_package_dir()`'s real value (derived from
/// `current_exe()`) is not the sentinel the oracle captured Pi with (module docs on
/// [`crate::config::get_package_dir`]).
pub fn build_system_prompt_with_paths(
    options: &BuildSystemPromptOptions<'_>,
    readme_path: &Path,
    docs_path: &Path,
    examples_path: &Path,
) -> String {
    let prompt_cwd = options.cwd.replace('\\', "/");
    let append_section = options
        .append_system_prompt
        .map(|s| format!("\n\n{s}"))
        .unwrap_or_default();
    let context_files = options.context_files.unwrap_or(&[]);

    if let Some(custom_prompt) = options.custom_prompt {
        let mut prompt = custom_prompt.to_string();
        prompt.push_str(&append_section);
        push_context_files(&mut prompt, context_files);
        prompt.push_str(&format!("\nCurrent working directory: {prompt_cwd}\n"));
        return prompt;
    }

    let default_tools = [
        "read".to_string(),
        "bash".to_string(),
        "edit".to_string(),
        "write".to_string(),
    ];
    let tools: &[String] = options.selected_tools.unwrap_or(&default_tools);
    let empty_snippets = HashMap::new();
    let tool_snippets = options.tool_snippets.unwrap_or(&empty_snippets);
    let visible_tools: Vec<&String> = tools
        .iter()
        .filter(|name| tool_snippets.contains_key(name.as_str()))
        .collect();
    let tools_list = if visible_tools.is_empty() {
        "(none)".to_string()
    } else {
        visible_tools
            .iter()
            .map(|name| format!("- {name}: {}", tool_snippets[name.as_str()]))
            .collect::<Vec<_>>()
            .join("\n")
    };

    let has_bash = tools.iter().any(|t| t == "bash");
    let has_grep = tools.iter().any(|t| t == "grep");
    let has_find = tools.iter().any(|t| t == "find");
    let has_ls = tools.iter().any(|t| t == "ls");
    // `hasRead` (`:101`) gated the skills-section append (`:155-157`), which is deferred
    // (see module docs) — nothing else reads it, so it is not computed.

    fn add_guideline(guideline: String, list: &mut Vec<String>) {
        if !list.contains(&guideline) {
            list.push(guideline);
        }
    }

    let mut guidelines_list: Vec<String> = Vec::new();
    if has_bash && !has_grep && !has_find && !has_ls {
        add_guideline(
            "Use bash for file operations like ls, rg, find".to_string(),
            &mut guidelines_list,
        );
    }
    for guideline in options.prompt_guidelines.unwrap_or(&[]) {
        let normalized = guideline.trim();
        if !normalized.is_empty() {
            add_guideline(normalized.to_string(), &mut guidelines_list);
        }
    }
    add_guideline(
        "Be concise in your responses".to_string(),
        &mut guidelines_list,
    );
    add_guideline(
        "Show file paths clearly when working with files".to_string(),
        &mut guidelines_list,
    );
    let guidelines = guidelines_list
        .iter()
        .map(|g| format!("- {g}"))
        .collect::<Vec<_>>()
        .join("\n");

    let mut prompt = format!(
        "You are an expert coding assistant operating inside pi, a coding agent harness. You help users by reading files, executing commands, editing code, and writing new files.\n\nAvailable tools:\n{tools_list}\n\nIn addition to the tools above, you may have access to other custom tools depending on the project.\n\nGuidelines:\n{guidelines}\n\nPi documentation (read only when the user asks about pi itself, its SDK, extensions, themes, skills, or TUI):\n- Main documentation: {}\n- Additional docs: {}\n- Examples: {} (extensions, custom tools, SDK)\n- When reading pi docs or examples, resolve docs/... under Additional docs and examples/... under Examples, not the current working directory\n- When asked about: extensions (docs/extensions.md, examples/extensions/), themes (docs/themes.md), skills (docs/skills.md), prompt templates (docs/prompt-templates.md), TUI components (docs/tui.md), keybindings (docs/keybindings.md), SDK integrations (docs/sdk.md), custom providers (docs/custom-provider.md), adding models (docs/models.md), pi packages (docs/packages.md), environment variables (docs/environment-variables.md)\n- When working on pi topics, read the docs and examples, and follow .md cross-references before implementing\n- Always read pi .md files completely and follow links to related docs (e.g., tui.md for TUI API details)",
        readme_path.display(),
        docs_path.display(),
        examples_path.display(),
    );

    prompt.push_str(&append_section);
    push_context_files(&mut prompt, context_files);
    prompt.push_str(&format!("\nCurrent working directory: {prompt_cwd}"));
    prompt
}

/// `<project_context>` block append (`:53-61` / `:144-152`), shared by both branches.
fn push_context_files(prompt: &mut String, context_files: &[(String, String)]) {
    if context_files.is_empty() {
        return;
    }
    prompt.push_str("\n\n<project_context>\n\n");
    prompt.push_str("Project-specific instructions and guidelines:\n\n");
    for (path, content) in context_files {
        prompt.push_str(&format!(
            "<project_instructions path=\"{path}\">\n{content}\n</project_instructions>\n\n"
        ));
    }
    prompt.push_str("</project_context>\n");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts<'a>(cwd: &'a str) -> BuildSystemPromptOptions<'a> {
        BuildSystemPromptOptions {
            cwd,
            ..Default::default()
        }
    }

    #[test]
    fn default_tools_produce_the_bash_only_guideline() {
        let prompt = build_system_prompt_with_paths(
            &opts("/proj"),
            Path::new("/pkg/README.md"),
            Path::new("/pkg/docs"),
            Path::new("/pkg/examples"),
        );
        assert!(prompt.contains("Use bash for file operations like ls, rg, find"));
        assert!(prompt.ends_with("Current working directory: /proj"));
    }

    #[test]
    fn custom_prompt_short_circuits_the_default_template() {
        let options = BuildSystemPromptOptions {
            custom_prompt: Some("Custom."),
            cwd: "/proj",
            ..Default::default()
        };
        let prompt = build_system_prompt_with_paths(
            &options,
            Path::new("/pkg/README.md"),
            Path::new("/pkg/docs"),
            Path::new("/pkg/examples"),
        );
        assert_eq!(prompt, "Custom.\nCurrent working directory: /proj\n");
    }

    #[test]
    fn windows_cwd_backslashes_become_forward_slashes() {
        let prompt = build_system_prompt_with_paths(
            &opts("C:\\Users\\me\\proj"),
            Path::new("/pkg/README.md"),
            Path::new("/pkg/docs"),
            Path::new("/pkg/examples"),
        );
        assert!(prompt.ends_with("Current working directory: C:/Users/me/proj"));
    }
}
