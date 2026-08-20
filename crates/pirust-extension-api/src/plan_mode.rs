//! Plan-mode utilities — literal port of
//! `packages/coding-agent/examples/extensions/plan-mode/utils.ts`.
//!
//! Pure string/command classification functions used by the bundled
//! plan-mode extension (feat-007 Wave 5). Each function is a 1:1
//! transcription of the TS original; the JS `RegExp` semantics map to the
//! `regex` crate's default syntax (the patterns here are ASCII and need no
//! JS-vs-Rust dialect features, so the port is literal).
//!
//! Oracle: `test/plan-mode-utils.test.ts` — the Rust unit tests in this
//! module port that suite's assertions verbatim.

use fancy_regex::Regex;

/// Destructive commands blocked in plan mode (utils.ts:15-44).
fn destructive_patterns() -> Vec<Regex> {
    [
        r"\brm\b", r"\brmdir\b", r"\bmv\b", r"\bcp\b", r"\bmkdir\b", r"\btouch\b", r"\bchmod\b",
        r"\bchown\b", r"\bchgrp\b", r"\bln\b", r"\btee\b", r"\btruncate\b", r"\bdd\b",
        r"\bshred\b", r"(^|[^<])>(?!>)", r">>", r"\bnpm\s+(install|uninstall|update|ci|link|publish)",
        r"\byarn\s+(add|remove|install|publish)", r"\bpnpm\s+(add|remove|install|publish)",
        r"\bpip\s+(install|uninstall)", r"\bapt(-get)?\s+(install|remove|purge|update|upgrade)",
        r"\bbrew\s+(install|uninstall|upgrade)",
        r"\bgit\s+(add|commit|push|pull|merge|rebase|reset|checkout|branch\s+-[dD]|stash|cherry-pick|revert|tag|init|clone)",
        r"\bsudo\b", r"\bsu\b", r"\bkill\b", r"\bpkill\b", r"\bkillall\b", r"\breboot\b",
        r"\bshutdown\b", r"\bsystemctl\s+(start|stop|restart|enable|disable)",
        r"\bservice\s+\S+\s+(start|stop|restart)", r"\b(vim?|nano|emacs|code|subl)\b",
    ]
    .into_iter()
    .map(|p| Regex::new(&format!("(?i){p}")).expect("plan-mode destructive pattern is valid"))
    .collect()
}

/// Safe read-only commands allowed in plan mode (utils.ts:46-82).
fn safe_patterns() -> Vec<Regex> {
    [
        r"^\s*cat\b",
        r"^\s*head\b",
        r"^\s*tail\b",
        r"^\s*less\b",
        r"^\s*more\b",
        r"^\s*grep\b",
        r"^\s*find\b",
        r"^\s*ls\b",
        r"^\s*pwd\b",
        r"^\s*echo\b",
        r"^\s*printf\b",
        r"^\s*wc\b",
        r"^\s*sort\b",
        r"^\s*uniq\b",
        r"^\s*diff\b",
        r"^\s*file\b",
        r"^\s*stat\b",
        r"^\s*du\b",
        r"^\s*df\b",
        r"^\s*tree\b",
        r"^\s*which\b",
        r"^\s*whereis\b",
        r"^\s*type\b",
        r"^\s*env\b",
        r"^\s*printenv\b",
        r"^\s*uname\b",
        r"^\s*whoami\b",
        r"^\s*id\b",
        r"^\s*date\b",
        r"^\s*cal\b",
        r"^\s*uptime\b",
        r"^\s*ps\b",
        r"^\s*top\b",
        r"^\s*htop\b",
        r"^\s*free\b",
        r"^\s*git\s+(status|log|diff|show|branch|remote|config\s+--get)",
        r"^\s*git\s+ls-",
        r"^\s*npm\s+(list|ls|view|info|search|outdated|audit)",
        r"^\s*yarn\s+(list|info|why|audit)",
        r"^\s*node\s+--version",
        r"^\s*python\s+--version",
        r"^\s*curl\s",
        r"^\s*wget\s+-O\s*-",
        r"^\s*jq\b",
        r"^\s*sed\s+-n",
        r"^\s*awk\b",
        r"^\s*rg\b",
        r"^\s*fd\b",
        r"^\s*bat\b",
        r"^\s*eza\b",
    ]
    .into_iter()
    .map(|p| Regex::new(&format!("(?i){p}")).expect("plan-mode safe pattern is valid"))
    .collect()
}

/// `isSafeCommand` (utils.ts:84-87).
pub fn is_safe_command(command: &str) -> bool {
    // fancy_regex returns `Result` (backtracking can fail); our patterns are
    // static and valid, so an error here is a programming bug.
    let is_destructive = destructive_patterns()
        .iter()
        .any(|p| p.is_match(command).unwrap_or(false));
    let is_safe = safe_patterns()
        .iter()
        .any(|p| p.is_match(command).unwrap_or(false));
    !is_destructive && is_safe
}

/// `TodoItem` (utils.ts:89-93).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TodoItem {
    pub step: usize,
    pub text: String,
    pub completed: bool,
}

/// `cleanStepText` (utils.ts:95-117) — bold/italic, code, leading action
/// words, whitespace collapse, capitalization, 50-char truncation.
pub fn clean_step_text(text: &str) -> String {
    // `*{1,2}([^*]+)\*{1,2}` → "$1" — bold/italic; `\*` is not a regex escape
    // in Rust, so use `[*]`.
    let bold_italic = Regex::new(r"[*]{1,2}([^*]+)[*]{1,2}").expect("bold regex");
    let code = Regex::new(r"`([^`]+)`").expect("code regex");
    let action = Regex::new(
        r"(?i)^(Use|Run|Execute|Create|Write|Read|Check|Verify|Update|Modify|Add|Remove|Delete|Install)\s+(the\s+)?",
    )
    .expect("action regex");

    let mut cleaned = bold_italic.replace_all(text, "$1").to_string();
    cleaned = code.replace_all(&cleaned, "$1").to_string();
    cleaned = action.replace(&cleaned, "").to_string();
    cleaned = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    cleaned = cleaned.trim().to_string();

    if !cleaned.is_empty() {
        // `charAt(0).toUpperCase() + slice(1)` — JS uppercases one UTF-16
        // code unit. For ASCII (all the oracle cases) this equals Rust's
        // char-based uppercase; for non-ASCII astral chars the behaviors can
        // differ, but the oracle corpus is ASCII and Pi's own behavior there
        // is the UTF-16 one. ponytail: char-based uppercase, revisit only if
        // a non-BMP step text shows up.
        let mut chars = cleaned.chars();
        if let Some(first) = chars.next() {
            cleaned = first.to_uppercase().collect::<String>() + chars.as_str();
        }
    }
    if cleaned.chars().count() > 50 {
        // `slice(0, 47)` — JS slices UTF-16 code units; `chars().take(47)`
        // is grapheme-cluster-safe for ASCII and equal for the oracle cases.
        // ponytail: char-based, revisit only if a step contains a surrogate
        // pair exactly at the cut.
        cleaned = cleaned.chars().take(47).collect::<String>() + "...";
    }
    cleaned
}

/// `extractTodoItems` (utils.ts:119-139).
pub fn extract_todo_items(message: &str) -> Vec<TodoItem> {
    let mut items = Vec::new();
    let header = Regex::new(r"(?i)[*]{0,2}Plan:[*]{0,2}\s*\n").expect("plan header regex");
    let Some(header_match) = header.find(message).ok().flatten() else {
        return items;
    };
    let plan_section = &message[header_match.end()..];
    let numbered = Regex::new(r"(?m)^\s*(\d+)[.)]\s+[*]{0,2}([^*\n]+)").expect("numbered regex");

    for caps in numbered.captures_iter(plan_section) {
        let caps = caps.expect("numbered regex is valid");
        let mut text = caps.get(2).expect("numbered group 2").as_str().trim();
        // `match[2].trim().replace(/\*{1,2}$/, "").trim()`
        let trailing_stars = Regex::new(r"[*]{1,2}$").expect("trailing stars regex");
        let trimmed = trailing_stars.replace(text, "");
        text = trimmed.trim();
        if text.len() > 5
            && !text.starts_with('`')
            && !text.starts_with('/')
            && !text.starts_with('-')
        {
            let cleaned = clean_step_text(text);
            if cleaned.chars().count() > 3 {
                items.push(TodoItem {
                    step: items.len() + 1,
                    text: cleaned,
                    completed: false,
                });
            }
        }
    }
    items
}

/// `extractDoneSteps` (utils.ts:141-148).
pub fn extract_done_steps(message: &str) -> Vec<usize> {
    let mut steps = Vec::new();
    let done = Regex::new(r"(?i)\[DONE:(\d+)\]").expect("done regex");
    for caps in done.captures_iter(message) {
        let caps = caps.expect("done regex is valid");
        if let Ok(step) = caps.get(1).expect("done group 1").as_str().parse::<usize>() {
            steps.push(step);
        }
    }
    steps
}

/// `markCompletedSteps` (utils.ts:150-158) — returns the number of markers
/// found (not the number of items newly completed — matches Pi exactly).
pub fn mark_completed_steps(text: &str, items: &mut [TodoItem]) -> usize {
    let done_steps = extract_done_steps(text);
    for step in done_steps {
        if let Some(item) = items.iter_mut().find(|t| t.step == step) {
            item.completed = true;
        }
    }
    extract_done_steps(text).len()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ===== isSafeCommand — ported verbatim from test/plan-mode-utils.test.ts =====

    #[test]
    fn safe_allows_basic_read_commands() {
        assert!(is_safe_command("ls -la"));
        assert!(is_safe_command("cat file.txt"));
        assert!(is_safe_command("head -n 10 file.txt"));
        assert!(is_safe_command("tail -f log.txt"));
        assert!(is_safe_command("grep pattern file"));
        assert!(is_safe_command("find . -name '*.ts'"));
    }

    #[test]
    fn safe_allows_git_read_commands() {
        assert!(is_safe_command("git status"));
        assert!(is_safe_command("git log --oneline"));
        assert!(is_safe_command("git diff"));
        assert!(is_safe_command("git branch"));
    }

    #[test]
    fn safe_allows_npm_yarn_read_commands() {
        assert!(is_safe_command("npm list"));
        assert!(is_safe_command("npm outdated"));
        assert!(is_safe_command("yarn info react"));
    }

    #[test]
    fn safe_allows_other_safe_commands() {
        assert!(is_safe_command("pwd"));
        assert!(is_safe_command("echo hello"));
        assert!(is_safe_command("wc -l file.txt"));
        assert!(is_safe_command("du -sh ."));
        assert!(is_safe_command("df -h"));
    }

    #[test]
    fn safe_blocks_file_modification_commands() {
        assert!(!is_safe_command("rm file.txt"));
        assert!(!is_safe_command("rm -rf dir"));
        assert!(!is_safe_command("mv old new"));
        assert!(!is_safe_command("cp src dst"));
        assert!(!is_safe_command("mkdir newdir"));
        assert!(!is_safe_command("touch newfile"));
    }

    #[test]
    fn safe_blocks_git_write_commands() {
        assert!(!is_safe_command("git add ."));
        assert!(!is_safe_command("git commit -m 'msg'"));
        assert!(!is_safe_command("git push"));
        assert!(!is_safe_command("git checkout main"));
        assert!(!is_safe_command("git reset --hard"));
    }

    #[test]
    fn safe_blocks_package_manager_installs() {
        assert!(!is_safe_command("npm install lodash"));
        assert!(!is_safe_command("yarn add react"));
        assert!(!is_safe_command("pip install requests"));
        assert!(!is_safe_command("brew install node"));
    }

    #[test]
    fn safe_blocks_redirects() {
        assert!(!is_safe_command("echo hello > file.txt"));
        assert!(!is_safe_command("cat foo >> bar"));
        assert!(!is_safe_command(">file.txt"));
    }

    #[test]
    fn safe_blocks_dangerous_commands() {
        assert!(!is_safe_command("sudo rm -rf /"));
        assert!(!is_safe_command("kill -9 1234"));
        assert!(!is_safe_command("reboot"));
    }

    #[test]
    fn safe_blocks_editors() {
        assert!(!is_safe_command("vim file.txt"));
        assert!(!is_safe_command("nano file.txt"));
        assert!(!is_safe_command("code ."));
    }

    #[test]
    fn safe_requires_command_in_safe_list() {
        assert!(!is_safe_command("unknown-command"));
        assert!(!is_safe_command("my-script.sh"));
    }

    #[test]
    fn safe_handles_leading_whitespace() {
        assert!(is_safe_command("  ls -la"));
        assert!(!is_safe_command("  rm file"));
    }

    // ===== cleanStepText =====

    #[test]
    fn clean_removes_markdown_bold_italic() {
        assert_eq!(clean_step_text("**bold text**"), "Bold text");
        assert_eq!(clean_step_text("*italic text*"), "Italic text");
    }

    #[test]
    fn clean_removes_markdown_code() {
        assert_eq!(clean_step_text("run `npm install`"), "Npm install");
        assert_eq!(
            clean_step_text("check the `config.json` file"),
            "Config.json file"
        );
    }

    #[test]
    fn clean_removes_leading_action_words() {
        assert_eq!(clean_step_text("Create the new file"), "New file");
        assert_eq!(clean_step_text("Run the tests"), "Tests");
        assert_eq!(clean_step_text("Check the status"), "Status");
    }

    #[test]
    fn clean_capitalizes_first_letter() {
        assert_eq!(clean_step_text("update config"), "Config");
    }

    #[test]
    fn clean_truncates_long_text() {
        let long_text = "This is a very long step description that exceeds the maximum allowed length for display";
        let result = clean_step_text(long_text);
        assert_eq!(result.chars().count(), 50);
        assert!(result.ends_with("..."));
    }

    #[test]
    fn clean_normalizes_whitespace() {
        assert_eq!(
            clean_step_text("multiple   spaces   here"),
            "Multiple spaces here"
        );
    }

    // ===== extractTodoItems =====

    #[test]
    fn extract_numbered_items_after_plan_header() {
        let message = "Here's what we'll do:\n\nPlan:\n1. First step here\n2. Second step here\n3. Third step here";
        let items = extract_todo_items(message);
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].step, 1);
        assert_eq!(items[0].text, "First step here");
        assert!(!items[0].completed);
    }

    #[test]
    fn extract_handles_bold_plan_header() {
        let message = "**Plan:**\n1. Do something";
        let items = extract_todo_items(message);
        assert_eq!(items.len(), 1);
    }

    #[test]
    fn extract_handles_parenthesis_numbering() {
        let message = "Plan:\n1) First item\n2) Second item";
        let items = extract_todo_items(message);
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn extract_returns_empty_without_plan_header() {
        let message = "Here are some steps:\n1. First step\n2. Second step";
        let items = extract_todo_items(message);
        assert_eq!(items.len(), 0);
    }

    #[test]
    fn extract_filters_out_short_items() {
        let message = "Plan:\n1. OK\n2. This is a proper step";
        let items = extract_todo_items(message);
        assert_eq!(items.len(), 1);
        assert!(items[0].text.contains("proper"));
    }

    #[test]
    fn extract_filters_out_code_like_items() {
        let message = "Plan:\n1. `npm install`\n2. Run the build process";
        let items = extract_todo_items(message);
        assert_eq!(items.len(), 1);
    }

    // ===== extractDoneSteps =====

    #[test]
    fn done_extracts_single_marker() {
        assert_eq!(
            extract_done_steps("I've completed the first step [DONE:1]"),
            vec![1]
        );
    }

    #[test]
    fn done_extracts_multiple_markers() {
        assert_eq!(
            extract_done_steps("Did steps [DONE:1] and [DONE:2] and [DONE:3]"),
            vec![1, 2, 3]
        );
    }

    #[test]
    fn done_handles_case_insensitivity() {
        assert_eq!(
            extract_done_steps("[done:1] [DONE:2] [Done:3]"),
            vec![1, 2, 3]
        );
    }

    #[test]
    fn done_returns_empty_with_no_markers() {
        assert_eq!(extract_done_steps("No markers here"), Vec::<usize>::new());
    }

    #[test]
    fn done_ignores_malformed_markers() {
        assert_eq!(extract_done_steps("[DONE:abc] [DONE:] [DONE:1]"), vec![1]);
    }

    // ===== markCompletedSteps =====

    #[test]
    fn mark_marks_matching_items() {
        let mut items = vec![
            TodoItem {
                step: 1,
                text: "First".into(),
                completed: false,
            },
            TodoItem {
                step: 2,
                text: "Second".into(),
                completed: false,
            },
            TodoItem {
                step: 3,
                text: "Third".into(),
                completed: false,
            },
        ];
        let count = mark_completed_steps("[DONE:1] [DONE:3]", &mut items);
        assert_eq!(count, 2);
        assert!(items[0].completed);
        assert!(!items[1].completed);
        assert!(items[2].completed);
    }

    #[test]
    fn mark_returns_count_of_completed_items() {
        let mut items = vec![TodoItem {
            step: 1,
            text: "First".into(),
            completed: false,
        }];
        assert_eq!(mark_completed_steps("[DONE:1]", &mut items), 1);
        assert_eq!(mark_completed_steps("no markers", &mut items), 0);
    }

    #[test]
    fn mark_ignores_markers_for_non_existent_steps() {
        let mut items = vec![TodoItem {
            step: 1,
            text: "First".into(),
            completed: false,
        }];
        let count = mark_completed_steps("[DONE:99]", &mut items);
        assert_eq!(count, 1); // counts the marker found
        assert!(!items[0].completed); // doesn't mark anything
    }

    #[test]
    fn mark_doesnt_double_complete() {
        let mut items = vec![TodoItem {
            step: 1,
            text: "First".into(),
            completed: true,
        }];
        mark_completed_steps("[DONE:1]", &mut items);
        assert!(items[0].completed);
    }
}
