//! Port of `cli/initial-message.ts` (`buildInitialMessage`) and the text-file branch of
//! `cli/file-processor.ts` (`processFileArguments`), used by `main.rs`'s step 24.
//!
//! # What is out of scope this wave
//!
//! `processFileArguments`'s **image** branch — MIME detection
//! (`detectSupportedImageMimeTypeFromFile`) and resize (`processImage`) — needs an image
//! codec. This is the same residual feat-004 already named for the `read` tool
//! (`crates/pirust-tools/src/read.rs`'s module docs: "read's image PROCESSING... needs an
//! image codec"); `@file some.png` here is therefore read and emitted as a text file
//! (`<file name="...">` wrapping raw bytes lossily decoded as UTF-8) rather than attached
//! as an [`ImageContent`]. `--` `fileImages` is consequently always empty.

use pirust_ai::types::ImageContent;
use pirust_tools::path_utils::resolve_read_path;

/// `ProcessedFiles` (`file-processor.ts:13-16`), images branch always empty — see module
/// docs.
pub struct ProcessedFiles {
    pub text: String,
    pub images: Vec<ImageContent>,
}

/// `processFileArguments(fileArgs)` (`file-processor.ts:24-87`), text branch only.
///
/// Each error is fatal and reported immediately (`console.error` + `exit(1)` in Pi,
/// `Err(String)` here for `main.rs` to report and exit on) — matching hazard §16.22:
/// an *empty* file is skipped silently, a *missing* one is fatal.
pub fn process_file_arguments(file_args: &[String], cwd: &str) -> Result<ProcessedFiles, String> {
    let mut text = String::new();
    for file_arg in file_args {
        let resolved =
            resolve_read_path(file_arg, cwd).map_err(|error| format!("File not found: {error}"))?;
        let metadata =
            std::fs::metadata(&resolved).map_err(|_| format!("File not found: {resolved}"))?;
        if metadata.len() == 0 {
            continue;
        }
        let bytes = std::fs::read(&resolved)
            .map_err(|error| format!("Could not read file {resolved}: {error}"))?;
        let content = String::from_utf8_lossy(&bytes);
        text.push_str(&format!("<file name=\"{resolved}\">\n{content}\n</file>\n"));
    }
    Ok(ProcessedFiles {
        text,
        images: Vec::new(),
    })
}

/// `InitialMessageResult` (`initial-message.ts:11-14`).
pub struct InitialMessageResult {
    pub initial_message: Option<String>,
    pub initial_images: Option<Vec<ImageContent>>,
}

/// `buildInitialMessage({parsed, fileText, fileImages, stdinContent})`
/// (`initial-message.ts:20-43`).
///
/// `messages` is `parsed.messages`; the TS mutates it via `shift()` — ported as an
/// explicit `remove(0)` on the caller's `Vec` so the remaining entries are the follow-up
/// prompts `print_mode.rs` sends afterward, exactly as `sdk.ts`'s scope note for
/// `parsed.messages` documents.
pub fn build_initial_message(
    messages: &mut Vec<String>,
    file_text: Option<&str>,
    file_images: Option<Vec<ImageContent>>,
    stdin_content: Option<String>,
) -> InitialMessageResult {
    let mut parts: Vec<String> = Vec::new();
    // `stdinContent !== undefined` (`:27`) — an empty string is still `!==undefined`, so
    // it IS pushed. Contrast `fileText` below, which uses JS truthiness.
    if let Some(stdin) = stdin_content {
        parts.push(stdin);
    }
    // `if (fileText)` (`:30`) — JS truthiness: an empty string is skipped.
    if let Some(text) = file_text {
        if !text.is_empty() {
            parts.push(text.to_string());
        }
    }
    if !messages.is_empty() {
        parts.push(messages.remove(0));
    }
    InitialMessageResult {
        initial_message: if parts.is_empty() {
            None
        } else {
            Some(parts.join(""))
        },
        initial_images: file_images.filter(|images| !images.is_empty()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stdin_empty_string_is_included_but_file_text_empty_is_not() {
        let mut messages = vec!["hi".to_string()];
        let result = build_initial_message(&mut messages, Some(""), None, Some(String::new()));
        // stdin "" is pushed (parts = [""]), fileText "" is skipped, then "hi" is pushed.
        assert_eq!(result.initial_message, Some("hi".to_string()));
        assert!(messages.is_empty());
    }

    #[test]
    fn no_inputs_at_all_is_none() {
        let mut messages: Vec<String> = Vec::new();
        let result = build_initial_message(&mut messages, None, None, None);
        assert_eq!(result.initial_message, None);
        assert_eq!(result.initial_images, None);
    }

    #[test]
    fn only_first_message_is_shifted_off() {
        let mut messages = vec!["first".to_string(), "second".to_string()];
        let result = build_initial_message(&mut messages, None, None, None);
        assert_eq!(result.initial_message, Some("first".to_string()));
        assert_eq!(messages, vec!["second".to_string()]);
    }
}
