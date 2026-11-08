# Editor: vim keys and tree-sitter highlighting

Every place you type text in nexus behaves like a small code editor. You get
modal vim keys, and the text is highlighted by tree-sitter.

## Decisions (user, 2026-11-08)

- **Vim is on by default.** `/vim off` turns it off and `/vim on` turns it back
  on. The setting is stored as the db setting `keymap` (`vim` or `standard`),
  the same way `/theme` stores the background mode, so it syncs with the
  space's other settings.
- **Tree-sitter highlights everything that shows code:**
  - the TUI composer (Markdown, plus the language of each fenced block);
  - nexus-ui text fields and the code `Editor`;
  - code blocks in chat history (replacing tui-markdown's syntect path);
  - studio doc code blocks and their exports.
- **Studio docs get a Markdown source mode.** A toggle swaps the page for the
  doc as Markdown. That view is a code editor with vim keys and tree-sitter
  highlighting. The rich block editor itself keeps its current keys.

## One highlighter, in core

`crates/core/src/highlight.rs` is the only place that knows about
tree-sitter. It serves the TUI, studio, and the exports.

- `highlight(source: &str, lang: &str) -> Vec<Span>`, where
  `Span { range: Range<usize> /* bytes */, role: Role }`. The spans are
  sorted, do not overlap, and only cover highlighted text.
- `Role` is a small, theme-free vocabulary: `Keyword`, `String`, `Comment`,
  `Function`, `Type`, `Number`, `Constant`, `Operator`, `Punctuation`,
  `Property`, `Variable`, `Tag`, `Attribute`, `Escape`, and, for Markdown,
  `Heading`, `Emphasis`, `Strong`, `Link`, `Code`, `Quote`, `ListMarker`.
  Each frontend maps roles to its own colors. The TUI uses `ui/style.rs`;
  studio and the exports use the doc's design palette.
- The languages are bash, c, cpp, css, go, html, java, javascript, json,
  markdown (block and inline grammars, with injections into fenced code),
  python, ruby, rust, toml, typescript, tsx and yaml. Aliases such as
  `sh`/`zsh`/`shell`, `js`, `ts`, `py`, `rs`, `yml` and `c++` resolve through
  `language(name)`. An unknown language gets no spans.
- Each language's `HighlightConfiguration` is built once and cached in a
  `OnceLock`. Input over 512 KiB, or input that takes longer than about 100 ms
  to parse, gets no spans. Highlighting is decoration, so it never blocks
  typing.

## Vim engine (TUI)

`crates/tui/src/vim.rs` is a modal layer over `tui_textarea::TextArea`. The
composer, nexus-ui `Input`/`Textarea`, and the nexus-ui code `Editor` all use
it.

- **Modes:** Normal, Insert, Visual (`v`), Visual line (`V`), and a pending
  operator or count (`d`, `c`, `y`, `g`, numbers).
- **Supported keys:**
  - motions: `h j k l`, `w b e`, `W B E`, `0 ^ $`, `gg G`, `{ }`;
  - inserting: `i a I A o O`;
  - editing: `x X`, `D C`, `dd cc yy`, operator plus motion (`dw`, `c$`, `y}`), `p P`, `J`, `r<char>`, `~`;
  - undo and redo: `u`, Ctrl+R;
  - counts, and `.` repeat of the last change.
- Each field starts in **Insert** mode, so typing works right away. Esc
  switches Insert to Normal. A second Esc in Normal mode does what Esc does
  today: it stops the stream or clears the composer in chat, and leaves the
  field in a panel.
- In Normal mode, Enter submits the composer and does nothing else elsewhere.
  In Insert mode, keys behave exactly as they do today, including Ctrl+A/Z/Y,
  Shift+arrow selection, and clipboard sync.
- The composer border and the focused field show `NORMAL`, `INSERT`,
  `VISUAL` or `V-LINE` as dim metadata, using the existing style roles.
- The function `vim::key(&mut VimState, &mut TextArea, KeyEvent) -> Outcome`
  returns `Handled`, `Passthrough` (the caller's existing handling applies),
  or `Submit`/`Leave` for Enter and Esc in Normal mode. It is pure state over
  the buffer, so its tests are key sequences checked against
  `(lines, cursor)`.

## Highlighting in the TUI

- **Composer and fields:** after each edit, the frontend clears
  `TextArea::custom_highlight`, then adds one range per span, with each byte
  range converted to `(row, col)`. The composer highlights as `markdown`.
  `Textarea` fields also highlight as `markdown`. A single-line `Input` gets
  no highlighting. The code `Editor` highlights its own language.
- **Chat history:** fenced code blocks render with core spans instead of
  tui-markdown's syntect output. This keeps the boxed layout, wrapping, and
  the language label. `cards.rs` reads, and `practice.rs`, which already go
  through `ui::markdown::render`, pick this up automatically.

## Studio

- **Highlight IPC:** the studio backend command `highlight { lang, source }`
  returns `[[start, end, role]]`, with UTF-16 offsets for JS. Doc code blocks
  in the rich editor are painted from these spans, recomputed per block after
  a pause in typing.
- **Exports:** HTML gets `<span class="hl-…">` with design-palette CSS. PDF
  (page SVG) and DOCX color the code runs with the same palette.
- **Markdown source mode:** the top bar gets a Source toggle. Turning it on
  renders the doc with `doc::markdown::render`, which is lossless because it
  uses `nexus-block` fences, into a source editor. The editor is a
  `<textarea>` over a highlighted `<pre>`, with the vim engine in
  `ui/doc/vim.js` covering the same command set as the Rust engine. Edits are
  parsed back with `doc::markdown::parse` after a pause and saved as one undo
  step. Blocks whose content did not change keep their ids, so comment
  anchors survive. If the parse fails, the editor shows the error and leaves
  the doc unchanged.

## Status

All six phases landed on 2026-11-08:
- E1, the core highlighter: 78dc514.
- E2, chat code blocks: 3e8ba3d.
- E3, vim in the composer: a55c5df.
- E4, fields, code editors and live colors: 42ccb8d.
- E5, studio code blocks and exports: d1ac8c9.
- E6, the source view: eb4abff.

Notes from building it:

- **Startup cost:** a grammar's query compiles on first use, so the
  highlighter builds a Markdown source's injected languages before its time
  budget starts. A first-time compile therefore never blanks the result.
- **Undo:** tui-textarea's own history cannot group a vim change. The engine
  keeps its own snapshots instead: one entry per change, restored with
  `set_lines`. Ctrl+Z in Insert mode is still the widget's own undo.
- **Code editors:** the nexus-ui code `Editor` keeps its value and caret in
  the core session. Vim runs on a temporary TextArea built from them and
  writes back with `Session::set_code`. Insert-mode typing keeps the editor's
  smart indent. The Visual selection is not painted there yet.
- **Studio code blocks:** they are painted through `CSS.highlights`, because
  rewriting the contenteditable DOM would move the caret and break undo.
- **Source view:** the colored layer under the textarea draws vim's block
  cursor and Visual selection, since the textarea's own selection is not
  reliably visible. Studio has no access to the TUI's `keymap` setting, so
  the vim toggle is remembered per device in `localStorage`. Esc in Normal
  mode stays in the view; ⇧⌘M or "Back to editing" leaves it.
- **Ids in source edits:** `doc::markdown::apply_source` reuses the id of an
  unchanged block, or of the same kind of block in the same place. Comments
  therefore stay anchored to paragraphs edited in the source view.

## Phases

1. **E1 (core highlighter):** `highlight.rs`, the grammars, and the roles, with
   tests.
2. **E2 (chat history code blocks):** move to core spans and drop the
   syntect dependency.
3. **E3 (vim engine and composer):** `/vim`, the `keymap` setting, and the mode
   indicator.
4. **E4 (live highlighting and fields):** composer and field highlighting,
   plus vim in nexus-ui `Input`/`Textarea`/`Editor`.
5. **E5 (studio highlighting):** the highlight IPC, doc code blocks, and the
   exports.
6. **E6 (studio Markdown source mode):** the toggle and source mode, built on
   `vim.js`.
