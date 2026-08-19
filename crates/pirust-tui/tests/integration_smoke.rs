//! Wave 8 integration smoke test — proves the public re-export surface
//! (mirroring `packages/tui/src/index.ts`) wires together end-to-end.
//!
//! Two checks:
//! 1. Every re-exported component renders deterministic output at a width
//!    through its `Component::render` impl.
//! 2. A `TUI` mounted with real ported components, driven through a mock
//!    `Terminal` (same pattern as `tui_golden.rs`), emits a synchronized
//!    render `write()` on `request_render(force) + poll()`.

use std::cell::RefCell;
use std::rc::Rc;

use pirust_tui::{
    // Components (index.ts exports; `BoxComponent` = TS `Box`, deliberate
    // rename — see box_component.rs module docs)
    BoxComponent,
    CancellableLoader,
    // Core TUI surface
    Component,
    Editor,
    Image,
    ImageOptions,
    Input,
    Loader,
    Markdown,
    SharedComponent,
    Spacer,
    Terminal,
    Text,
    TruncatedText,
    TUI,
};

// ---------------------------------------------------------------------------
// Mock terminal (reused pattern from tui_golden.rs)
// ---------------------------------------------------------------------------

struct MockTerminal {
    writes: Rc<RefCell<Vec<String>>>,
}

impl Terminal for MockTerminal {
    fn start(
        &mut self,
        _on_input: Box<dyn FnMut(&str) + Send>,
        _on_resize: Box<dyn FnMut() + Send>,
    ) {
    }
    fn stop(&mut self) {}
    fn drain_input(&mut self, _max_ms: u64, _idle_ms: u64) {}
    fn write(&mut self, data: &str) {
        self.writes.borrow_mut().push(data.to_string());
    }
    fn columns(&self) -> u16 {
        80
    }
    fn rows(&self) -> u16 {
        24
    }
    fn kitty_protocol_active(&self) -> bool {
        false
    }
    fn move_by(&mut self, _lines: i32) {}
    fn hide_cursor(&mut self) {}
    fn show_cursor(&mut self) {}
    fn clear_line(&mut self) {}
    fn clear_from_cursor(&mut self) {}
    fn clear_screen(&mut self) {}
    fn set_title(&mut self, _title: &str) {}
    fn set_progress(&mut self, _active: bool) {}
}

// Theme shims — the TS components take color functions; use identity.
fn id_fn() -> Box<dyn Fn(&str) -> String> {
    Box::new(|s: &str| s.to_string())
}

// ---------------------------------------------------------------------------
// Component render checks
// ---------------------------------------------------------------------------

#[test]
fn reexported_components_render_deterministic_output() {
    // Text
    let mut text = Text::new("hello world", 1, 1);
    let lines = text.render(20);
    assert!(!lines.is_empty());
    assert!(lines.iter().any(|l| l.contains("hello world")));

    // Box (BoxComponent) — empty box renders nothing (box.ts:74 early
    // return); mount a child. Box is padding+background, no border.
    let mut bx = BoxComponent::new(1, 1);
    let child: SharedComponent = Rc::new(RefCell::new(Text::new("hi", 0, 0)));
    bx.add_child(child);
    let lines = bx.render(10);
    assert!(!lines.is_empty());
    assert!(
        lines.iter().any(|l| l.contains("hi")),
        "box renders padded child content: {lines:?}"
    );

    // Spacer
    let mut spacer = Spacer::new(3);
    assert_eq!(spacer.render(10).len(), 3);

    // TruncatedText — width is visible width (ANSI resets around ellipsis
    // inflate char count; `visible_width` strips sequences).
    let mut trunc = TruncatedText::new("a very long line that gets truncated", 0, 0);
    let lines = trunc.render(10);
    assert!(!lines.is_empty());
    assert!(
        pirust_tui::visible_width(&lines[0]) <= 10,
        "truncated text visible width ≤ 10: {:?}",
        lines[0]
    );

    // Input
    let mut input = Input::new();
    let lines = input.render(20);
    assert!(!lines.is_empty());

    // Loader / CancellableLoader — render at a tick
    let mut loader = Loader::new(id_fn(), id_fn(), "Loading...", None);
    let lines = loader.render(20);
    assert!(!lines.is_empty());
    let mut cl = CancellableLoader::new(id_fn(), id_fn(), "Loading...", None);
    assert!(!cl.render(20).is_empty());

    // Markdown — needs a theme (struct of color fns).
    fn theme_fn() -> Rc<dyn Fn(&str) -> String> {
        Rc::new(|s: &str| s.to_string())
    }
    let md_theme = pirust_tui::markdown::MarkdownTheme {
        heading: theme_fn(),
        link: theme_fn(),
        link_url: theme_fn(),
        code: theme_fn(),
        code_block: theme_fn(),
        code_block_border: theme_fn(),
        quote: theme_fn(),
        quote_border: theme_fn(),
        hr: theme_fn(),
        list_bullet: theme_fn(),
        bold: theme_fn(),
        italic: theme_fn(),
        strikethrough: theme_fn(),
        underline: theme_fn(),
    };
    let mut md = Markdown::new("hi", 1, 1, md_theme, None, None);
    let lines = md.render(40);
    assert!(!lines.is_empty());

    // Editor — needs a real TUI + border color fn.
    let writes = Rc::new(RefCell::new(Vec::new()));
    let terminal = Box::new(MockTerminal {
        writes: writes.clone(),
    });
    let tui = Rc::new(RefCell::new(TUI::new(terminal, Some(false))));
    let mut editor = Editor::new(tui, id_fn(), Default::default());
    let lines = editor.render(40);
    assert!(!lines.is_empty());

    // Image — fallback text (no caps)
    let mut img = Image::new(
        "aGVsbG8=",
        "image/png",
        pirust_tui::ImageTheme {
            fallback_color: id_fn(),
        },
        ImageOptions {
            filename: Some("hello.png".to_string()),
            ..Default::default()
        },
        None,
    );
    let lines = img.render(40);
    assert!(!lines.is_empty());
    assert!(lines.iter().any(|l| l.contains("Image:")));
}

// ---------------------------------------------------------------------------
// TUI end-to-end smoke
// ---------------------------------------------------------------------------

#[test]
fn tui_renders_ported_components_through_mock_terminal() {
    let writes = Rc::new(RefCell::new(Vec::new()));
    let terminal = Box::new(MockTerminal {
        writes: writes.clone(),
    });
    let mut tui = TUI::new(terminal, Some(false));

    // Mount real ported components via the shared-component seam.
    let text: SharedComponent = Rc::new(RefCell::new(Text::new("pirust", 1, 1)));
    tui.add_child(text);

    let input: SharedComponent = Rc::new(RefCell::new(Input::new()));
    tui.add_child(input);

    // Force a render and poll (force bypasses the 16ms throttle).
    tui.request_render(true);
    assert!(tui.poll(), "force render should complete a poll");

    // The write must contain the synchronized-output open marker and the
    // rendered text content.
    let all: String = writes.borrow().concat();
    assert!(
        all.contains("\x1b[?2026h"),
        "render should open synchronized output, got: {all:?}"
    );
    assert!(
        all.contains("pirust"),
        "rendered text should appear in the write, got: {all:?}"
    );
}

// ---------------------------------------------------------------------------
// Re-export name stability (index.ts parity for what's ported)
// ---------------------------------------------------------------------------

#[test]
fn reexports_are_stable_across_the_surface() {
    let _ = pirust_tui::render_latex("x^2 + 1", &Default::default());
    let _ = pirust_tui::fuzzy_match("ab", "acb");
    let _ = pirust_tui::visible_width("héllo");
    let _ = pirust_tui::truncate_to_width("long text here", 5, "...", false);
    let _ = pirust_tui::slice_by_column("hello world", 3, 5, false);
    let _ = pirust_tui::wrap_text_with_ansi("hello", 4);
    let _ = pirust_tui::ProcessTerminal::new();
    let _ = pirust_tui::StdinBuffer::new(Default::default());
    let _ = pirust_tui::KeybindingsManager::default();
}
