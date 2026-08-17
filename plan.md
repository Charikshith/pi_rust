# plan.md — feat-006: P5 — pirust-tui literal port

Source: `packages/tui` (`@earendil-works/pi-tui`, ~12,200 lines / 28 files).
Spec: `docs/analysis/05-tui.md`. Crate: `crates/pirust-tui` (currently a name-only
stub). Cadence decision already locked in `progress.md`: **checkpoint per phase**
— implement one wave, verify, report, pause for the next "go ahead".

Correctness bar (per `AGENTS.md`): oracle-driven golden tests against real Pi
TS source (same `register()` bare-specifier-alias + node type-stripping pattern
as `scripts/gen-sdk-oracle.mjs`/`gen-cli-oracle.mjs`), not self-authored
expectations. `pirust-tui` has no network/FS surface for most of its logic, so
oracle scripts import `packages/tui/src/*.ts` straight from `../pi` and execute
the real functions — no server, no offline env vars needed.

## Waves (dependency order, mirrors 05-tui.md §9's proposed layout)

1. **utils.rs** — `visible_width`, `truncate_to_width`, `slice_by_column`,
   `slice_with_width`, `extract_segments`, `wrap_text_with_ansi`,
   `normalize_terminal_output`, `extract_ansi_code`, `is_whitespace_char`,
   `is_punctuation_char`, `apply_background_to_line`, `AnsiCodeTracker`,
   `cjk_break_regex`-equivalent classifier. Foundational — every renderer/
   component call goes through `visibleWidth`/wrapping. UTF-16 code-unit
   offsets are NOT this file's hazard (it's grapheme/width, not cursor-col
   arithmetic) but `Intl.Segmenter` grapheme parity IS — needs emoji/ZWJ/
   regional-indicator/Thai-Lao-AM/combining-mark oracle cases.
   Verify: `scripts/gen-tui-oracle.mjs` (utils section) + `utils_golden.rs`.
2. **keys.rs + stdin_buffer.rs** — Kitty CSI-u / xterm modifyOtherKeys / legacy
   key parsing tables (`matches_key`, `parse_key`, `decode_printable_key`,
   `decode_kitty_printable`, `is_key_release`, `is_key_repeat`); escape-sequence
   splitter + bracketed-paste framing. Pure byte-sequence -> value functions,
   oracle-testable without a real terminal.
   Verify: oracle cases per encoding + trap sequences (split escapes, WezTerm
   double-escape, mouse SGR, high-byte meta) + `keys_golden.rs`/`stdin_buffer_golden.rs`.
3. **kill_ring.rs, undo_stack.rs, word_navigation.rs, keybindings.rs, fuzzy.rs**
   — small self-contained pure modules (46/28/117/244/137 TS lines). Unit +
   light oracle tests where behavior is non-obvious (kill-ring accumulate/
   prepend merge rules, fuzzy-match scoring).
4. **tui.rs + terminal.rs** — `Component`/`Container`/`Focusable` traits, the
   `TUI` diff-render loop (frame pipeline, overlay compositing, cursor-marker
   extraction, synchronized-output wrapping, full-redraw triggers), `Terminal`
   trait + `ProcessTerminal` (crossterm shim: raw mode, size/resize, VT enable
   on Windows). This is the render engine; no live terminal I/O test — a mock
   `Terminal` (Rust analogue of the TS `@xterm/headless` test double) drives it.
5. **components/** — `Box`, `Text`, `TruncatedText`, `Spacer`, `Loader` /
   `CancellableLoader`, `Input`, `SelectList`, `SettingsList`, `Image`,
   `editor_component.rs` (the `EditorComponent` seam trait). Each is a pure
   `render(width) -> Vec<String>` producer; golden-test rendered output against
   real Pi components at representative widths/states.
6. **editor.rs** — the 2333-line line editor (crown jewel): grapheme/word
   segmentation with atomic paste-marker segments, word-wrap + `layoutText`,
   cursor movement (sticky preferred column, 7-case vertical-move table),
   kill-ring/undo/history wiring, bracketed-paste + large-paste markers,
   character-jump mode, async autocomplete integration. **UTF-16 code-unit
   cursor-column arithmetic is the single biggest hazard here** (05-tui.md
   §9) — decide once, up front: operate on `Vec<u16>` offsets to match JS
   `.length`/`.slice`/`charCodeAt` exactly, not `char`/byte indices.
7. **autocomplete.rs, terminal_colors.rs, terminal_image.rs (Kitty/iTerm2),
   native_modifiers.rs (macOS FFI), win_console.rs (Windows FFI), markdown.rs**
   — remaining terminal-feature integrations and the heaviest component
   (`Markdown`, 858 lines, via `pulldown-cmark`). FFI modules fail-closed on
   unsupported platforms exactly like the TS.
8. **lib.rs re-exports + final integration** — mirror `index.ts`'s public
   surface; a smoke test wiring `TUI` + a couple of components through a mock
   `Terminal` end-to-end (no live stdio — feat-007 wires this into the
   interactive `pirust` binary). Update `feature_list.json` evidence, delete
   this file.

## Out of scope (feat-007's job, not this feature's)

Wiring `pirust-tui` into `pirust-coding-agent`'s interactive mode; extensions;
plan-mode; slash-command autocomplete providers backed by real tools/skills.
This feature only builds the library and proves it against Pi's own TUI
source, standalone.

## Status

- [x] Wave 1 — utils.rs (crates/pirust-tui/src/utils.rs, ~1360 lines incl. docs/tests
      vs 1209 TS lines;
      99/99 oracle cases green via scripts/gen-tui-oracle.mjs + tests/utils_golden.rs;
      wired into init.sh; documented gaps: RGI-emoji matching, Default_Ignorable
      property, cjkBreakRegex script property — all heuristic approximations, see
      utils.rs module docs)
- [x] Wave 2 — keys.rs + stdin_buffer.rs (crates/pirust-tui/src/{keys,stdin_buffer}.rs,
      1307/450 lines vs 1401/434 TS; 306/23 oracle cases green via
      scripts/gen-tui-oracle.mjs's keys/stdin-buffer sections +
      tests/{keys,stdin_buffer}_golden.rs; wired into init.sh. Scope decisions:
      KeyId/Key builder not ported (TS-compile-time-only, zero runtime behavior);
      Kitty protocol state as static AtomicBool; _lastEventType confirmed dead
      state (zero readers in ../pi) and not ported; StdinBuffer::process returns
      Vec<StdinEvent> instead of EventEmitter callbacks, timer-driven flush()
      scheduling deferred to Wave 4 (tui.rs) as documented caller responsibility.)
- [x] Wave 3 — kill_ring/undo_stack/word_navigation/keybindings/fuzzy
      (crates/pirust-tui/src/{kill_ring,undo_stack,word_navigation,keybindings,fuzzy}.rs;
      kill_ring/undo_stack unit-tested only, per triviality; word_navigation/keybindings/
      fuzzy oracle-verified, 29/8/19 cases green via scripts/gen-tui-oracle.mjs +
      tests/{word_navigation,keybindings,fuzzy}_golden.rs; wired into init.sh. Real bug
      found+fixed: find_word_backward's cursor baseline must stay unclamped, matching
      Pi. Documented gap: unicode-segmentation lacks Intl.Segmenter's CJK dictionary
      segmentation — one named oracle case (forward-cjk-text) explicitly excluded with
      citation, not silently dropped. word_navigation.rs cursor offsets are UTF-16
      code units by design, pre-empting Wave 6's editor.rs hazard. Keybinding ported as
      a closed Rust enum (fixed 31-id set); global singleton via LazyLock<Mutex<_>>.)
- [ ] Wave 4 — tui.rs + terminal.rs
- [ ] Wave 5 — components/
- [ ] Wave 6 — editor.rs
- [ ] Wave 7 — autocomplete/terminal_colors/terminal_image/native_modifiers/win_console/markdown
- [ ] Wave 8 — lib.rs re-exports + integration smoke test + evidence
