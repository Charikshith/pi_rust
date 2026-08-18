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
4. **terminal_colors.rs + terminal_image.rs + tui.rs + terminal.rs** — REVISED
   from the original plan: `tui.ts` imports `terminal-colors.ts` (OSC 11
   background-color / color-scheme parsing) and 4 exports of
   `terminal-image.ts` (`getCapabilities`, `isImageLine`, `deleteKittyImage`,
   `setCellDimensions`) directly, so those two modules move up from Wave 7 to
   this wave (ported in full — `terminal_image.rs` is self-contained pure
   logic, splitting it across two waves would be worse than porting it once).
   Wave 7 shrinks to autocomplete/native_modifiers/win_console/markdown.
   `Component`/`Container`/`Focusable` traits, the `TUI` diff-render loop
   (frame pipeline, overlay compositing incl. the focus-restore state
   machine, cursor-marker extraction, synchronized-output wrapping,
   full-redraw triggers, Kitty-image reserved-row lifecycle), `Terminal` trait
   + `ProcessTerminal` (crossterm shim: raw mode, size/resize; Windows VT
   input and the macOS native-modifier probe are stubbed fail-closed here and
   wired for real in Wave 7's win_console.rs/native_modifiers.rs). No live
   terminal I/O test — a mock `Terminal` (Rust analogue of the TS
   `@xterm/headless` test double) drives both the Rust tests AND a new oracle
   section that constructs a real Pi `TUI` against a JS-side fake `Terminal`,
   capturing the exact `write()` byte sequences for render/overlay/resize
   scenarios.
5. **components/ + autocomplete.rs + editor_component.rs** — REVISED: `editor-
   component.ts` imports `AutocompleteProvider` from `autocomplete.ts`, and
   `autocomplete.ts` turns out to have ZERO dependency on `tui.ts`/rendering
   (it only imports `fuzzy.ts`, Wave 3, plus Node `fs`/`child_process`/`path`/
   `os`) — so it moves up from Wave 7 to this wave rather than leaving a
   forward reference. `Box`, `Text`, `TruncatedText`, `Spacer`, `Loader` /
   `CancellableLoader`, `Input`, `SelectList`, `SettingsList`, `Image` are
   pure `render(width) -> Vec<String>` producers (`Input`'s cursor/grapheme
   arithmetic is UTF-16-code-unit-based, same family as Wave 3's
   `word_navigation.rs` — reuse `find_word_backward`/`find_word_forward`
   directly rather than re-deriving); `Loader`'s animation timer and
   `autocomplete.rs`'s `fd`-subprocess-based fuzzy file search are both
   timer/async-shaped in the TS with no owned event loop here yet (same
   caller-owns-the-timer story as Waves 2/4) — implement synchronously
   (blocking subprocess call, no `AbortSignal`-equivalent cancellation yet)
   and document the real async/debounce wiring as Wave 6's integration job.
   `editor_component.rs` is the `EditorComponent` seam trait. Golden-test
   rendered output against real Pi components at representative widths/
   states.
6. **editor.rs** — the 2333-line line editor (crown jewel): grapheme/word
   segmentation with atomic paste-marker segments, word-wrap + `layoutText`,
   cursor movement (sticky preferred column, 7-case vertical-move table),
   kill-ring/undo/history wiring, bracketed-paste + large-paste markers,
   character-jump mode, async autocomplete integration. **UTF-16 code-unit
   cursor-column arithmetic is the single biggest hazard here** (05-tui.md
   §9) — decide once, up front: operate on `Vec<u16>` offsets to match JS
   `.length`/`.slice`/`charCodeAt` exactly, not `char`/byte indices.
7. **native_modifiers.rs (macOS FFI), win_console.rs (Windows FFI),
   markdown.rs** — REVISED TWICE: `terminal_colors.rs`/`terminal_image.rs`
   moved to Wave 4, `autocomplete.rs` moved to Wave 5 (see above). Remaining:
   the two native-modifier FFI shims (wiring Wave 4's stubs for real) and the
   heaviest component (`Markdown`, 858 lines, via `pulldown-cmark`). FFI
   modules fail-closed on unsupported platforms exactly like the TS.
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
- [x] Wave 4 — terminal_colors.rs + terminal_image.rs + terminal.rs + tui.rs
      (revised scope, terminal-colors/image pulled up from Wave 7; 149/640/608/2050
      lines vs 73/488/531/1714 TS). Oracle-verified: 17/59/9/7 cases green via
      scripts/gen-tui-oracle.mjs's new sections + tests/{terminal_colors,
      terminal_image,terminal,tui}_golden.rs; wired into init.sh. Key design
      decisions: Component tree as Rc<RefCell<dyn Component>> (SharedComponent),
      compared via Rc::ptr_eq (makes TUI/Container intentionally !Send, matching
      JS's single-threaded object-identity semantics); OverlayHandle's TS closures
      become an OverlayId token + TUI methods; request_render's debounce is
      synchronous + caller-polled via TUI::poll() (tokio dropped entirely once
      !Send made spawning impossible — same StdinBuffer-style caller-owns-the-timer
      adaptation as Wave 2, now for a structural reason); width-overflow -> panic!
      after the same crash-log write (TS's own "crash the process" intent).
      enableWindowsVTInput/macOS native-modifier probe are documented Wave-7 stubs.
      Real bug caught by the oracle: force=true render must still go through
      process.nextTick (never synchronous), fixed with a force_pending flag.
      Named deferred residuals (no oracle exists, all documented): StdinBuffer's
      idle/fragment timeouts and OSC background/color-scheme query timeouts aren't
      wired to a real timer; resize detection polls crossterm::terminal::size()
      every 200ms instead of native SIGWINCH. crossterm added (raw-mode/size/write
      shim only, per 05-tui.md §8 — NOT its Event parser). Root re-exports for
      TUI/Terminal/Component/etc. added by the coordinator after independent
      verification (fork's lib.rs diff had left them out).
- [ ] Wave 5 — components/
- [ ] Wave 6 — editor.rs
- [ ] Wave 7 — autocomplete/terminal_colors/terminal_image/native_modifiers/win_console/markdown
- [ ] Wave 8 — lib.rs re-exports + integration smoke test + evidence
