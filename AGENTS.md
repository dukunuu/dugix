# AGENTS.md

Guidance for AI coding agents working in this repository. Read this before
making changes — it covers the gates, conventions, and the shape of the code.

## What this is

`nexus-chat` is a local-first terminal chat app (Rust, [ratatui]) for deep
research and multi-agent work. All state lives on the user's machine: a
SQLite database (rows scoped by *space*), files/scripts/apps in space
directories, and
model-created web apps served from `localhost:8642`. It talks to any
OpenAI-wire backend (OpenRouter, OpenAI, OpenCode Zen/Go, Codex, Mistral) through one
client. The binary is named `nexus` (package name `nexus-chat`).

## Commands

```sh
cargo build                    # build the workspace (binary: nexus)
cargo run                      # run the TUI
cargo test --workspace         # 500+ tests; none touch the network
scripts/check.sh               # THE gate: fmt + clippy + audit + tests
scripts/check.sh --fix         # auto-fix fmt/clippy, then run the gate
```

Pushing to master auto-bumps the patch version, tags `vX.Y.Z`, and releases
(crates.io + GitHub) via `.github/workflows/version-bump.yml`; a manual `v*`
tag push publishes the same way. Workflows live in `.github/workflows/`.
Release notes are generated from conventional commits since the last tag
(`scripts/release-notes.sh`).

Every commit runs `scripts/check.sh` via the local pre-commit hook — a commit
that fails the gate is rejected. **Never disable the hook, and never commit
code that fails `cargo clippy -- -D warnings -W clippy::pedantic`.**

## Conventions

- **Rust edition 2024**, `rustc 1.91+`. `cargo fmt` is mandatory.
- **Clippy at pedantic with warnings denied** — write code that satisfies it
  (explicit lifetimes where needed, `#[must_use]` on pure fns, docs on public
  items, no `unwrap()` on fallible paths). Prefer params structs over
  `too_many_arguments` allows.
- **Tests**: app-level integration tests live in `crates/core/src/app/tests.rs` and
  `crates/tui/src/ui/popups/tests.rs` (ratatui `TestBackend` snapshot style). Keep tests
  hermetic — no network, no real key material, no XDG writes (use temp dirs).
- **Commit style**: lowercase conventional prefix, imperative, em-dash
  detail. Examples from history: `feat:`, `fix:`, `design:`, `docs:`,
  `style:`, `refactor:`, `test:`, `chore:`, `build:`, `license:`. One logical
  change per commit; run `scripts/check.sh` before committing.
- **Secrets**: API keys come from `config.toml` in the config dir (`nexus status` prints it) or env
  vars (`OPENROUTER_API_KEY`, `OPENAI_API_KEY`, `OPENCODE_API_KEY`,
  `MISTRAL_API_KEY`). Never
  hardcode or commit keys.
- **cargo-audit**: gated in `scripts/check.sh`. `.cargo/audit.toml` ignores
  two advisories with written rationale — only extend that list when a
  warning is provably unreachable, and say why.

## Code map

The README has the full tree; the parts agents touch most:

| Path | What lives there |
|---|---|
| `crates/core/src/app/mod.rs` | `App` struct (domain fields only), popup state, gate plumbing, `boot()`, `snapshot()` |
| `crates/core/src/app/commands.rs` | `AppCommand` seam + `/`-command catalog (`COMMANDS`, `fuzzy_score`): `parse_command` + `execute` (the `/`-string front is `run_command`) |
| `crates/core/src/app/chat.rs` | request lifecycle: history build, streaming, tool loop |
| `crates/core/src/app/research.rs` | research pipeline: survey → plan → searchers → synthesis → critic → verifier → writer |
| `crates/core/src/app/swarm.rs`, `research_jobs.rs` | roundtable / repeating research jobs and report diffs; `/research` creates a one-off research job, `/watch <topic>` a daily one, both listed in `/jobs` (`w` toggles daily). Repeat runs use `nexus jobs daemon` or `nexus jobs run-due` from cron |
| `crates/core/src/app/usage.rs` | usage & cost analytics (`/usage`); Ctrl+G shows tool-definition context tokens and the session's total cost, requests and tokens; `nexus usage --session <id> [--json]` prints every request and the total |
| `crates/core/src/app/sessions.rs`, `spaces.rs`, `models.rs` | state backends |
| `crates/core/src/app/memory.rs` | memory glue: the frozen `## Memory` table of contents, per-message `recall` hint rows, `$EDITOR` round trip, `/memory tidy` asks the chat model to review and clean the memory with its own tool, in the conversation |
| `crates/core/src/provider/openrouter.rs` | the single client for all OpenAI-wire backends — message shapes, tool-call wire format, events are in `provider/mod.rs` |
| `crates/core/src/provider/serve.rs` | managed local-provider servers: per-runtime launch commands, endpoint liveness probes, process-tree memory, and the advisory budget behind `/local start\|stop\|status` |
| `crates/core/src/provider/local.rs`, `harmony.rs` | opt-in local runtimes: config + installed-model discovery, and the Harmony channel splitter for `gpt-oss` servers that leave framing in `content` |
| `crates/core/src/provider/embedding.rs` | file-search embeddings: `EmbedInput` (text/image), `Embedder` (OpenAI-wire or Ollama native `/api/embed`), probed `EmbedIdentity` used as the vector cache key, strict response validation; `app/embeddings.rs` selects/probes it automatically (local default only when reachable, no setting), `extract/embedding.rs` is the token-bounded chunker |
| `crates/core/src/tools.rs` | the model's tools + the `ToolExecutor` seam (`defs`/`is_read_only`/`run`) |
| `crates/core/src/design/`, `crates/core/src/doc/` | space designs (tokens, one type scale, slide layouts, drift lint) and docs (blocks, ops, Markdown, pagination in `doc/layout.rs`, page SVG, PDF via `krilla-svg`, DOCX/HTML export, DOCX import) |
| `crates/core/src/design/template/` | Office template storage/extraction and deterministic package reuse; `deck/import/pptx/template.rs` shares the bounded PPTX reader and inheritance. Word styles resolve localized ids by built-in name. |
| `crates/core/src/sheet/` | spreadsheet JSON inputs and formatting, tab-qualified A1 ranges, validation, atomic edits, IronCalc evaluation/reference rewrites (only `engine.rs` touches engine types), bounded formula/value reads, cached PNG grid previews, shared history, CSV/XLSX bytes; canvas integration, no studio grid yet |
| `crates/core/src/tools/canvas.rs`, `tools/canvas/` | shared deck/doc/sheet tool; `sheets.rs` routes sheet create/import, read, edit, view, history and export. `deck` is dispatch-only compatibility. Studio opens docs too: see the studio rows below. |
| `crates/core/src/deck/link.rs` | the studio link, protocol version 2: `path` (`deck_path` still accepted), an envelope `kind` (`deck` or `doc`), doc `Comment{block,range,quote}` and `Focus{block,selection}` (Rust names `DocComment`, `DocFocus`; `Line` maps them on the wire), and a clear "update nexus-studio" error for a peer on another version |
| `crates/studio/src/backend.rs`, `backend/doc.rs`, `backend/doc_comments.rs` | the window-free studio backend. Deck mode is `backend.rs`; doc mode (`DocSession`) is `doc.rs` — `DocApply` batches (one undo step), undo and redo written to the file, external reload as one undo step, pages served as `/pages/<rev8>-<n>.png`, designs, exports, content-addressed image import — and `doc_comments.rs` (anchors, suggestions, accept and reject all). Doc commands are `Doc*` variants; `Undo`, `Redo`, `Load` and the comment-thread commands are shared by mode |
| `crates/core/src/canvas/` | the library of a space, shared by studio, the TUI and the CLI: `library(files)` lists every deck and doc newest first as `Entry` (kind, path, title, modified, slide/block count, design, revision-keyed thumbnail; hidden folders, `assets`, symlinks and invalid files left out) and `ensure_thumbnail` draws it lazily; `files` is rename, duplicate, delete (to `<files>/.trash/`), save-a-copy with assets, `write_chosen` and the deck/doc exporters; `dialogs` is the `Dialogs` trait (native Open/Save through `rfd`, feature `native-dialogs`) with the `Scripted` fake every test uses; `settings` remembers the last export folder per space in `studio-settings.json` |
| `crates/studio/src/backend/library.rs`, `crates/studio/ui/library.js`, `library.css` | studio's Library view (a full-window layer over both modes, Cmd/Ctrl+O) and every native-dialog command (`OpenDialog`, `ExportAs`, `SaveCopy`, `PickTemplate`, `Library*`). A library file is addressed by its path inside the space and re-checked by `canvas::files::resolve`; the page never sees another path except the one a dialog just returned. Dialogs are two steps (`prepare_dialog` → a helper thread's `DialogJob::ask`, which `rfd` hands to the main thread as macOS needs → `finish_dialog`), never inside the `tao` callback or under the backend lock; thumbnails are drawn off the lock too (`Backend::thumbnail_job`). `Backend::launch` (no file given) keeps a blank deck in memory so the library is the start page |
| `crates/core/src/subagent.rs`, `crates/core/src/app/subagents.rs`, `crates/core/src/db/subagents.rs`, `crates/tui/src/ui/popups/subagents.rs` | `task` sub-agents: the loop sends `StreamEvent::Subagent` (started, batched deltas, each round with its calls and usage, finished); `App::on_subagent` keeps the run, saves it to `cache.subagent_runs` (not in incognito) and adds its cost to the parent reply; `/agents` lists and streams the transcripts. Sub-agent requests are also logged to `usage_log` as kind `subagent` by their own client |
| `crates/tui/src/ui/popups/library.rs`, `crates/tui/src/cli_library.rs` | `/library` (browse, `e` export through the Save dialog, `d` delete after a confirm; no desktop session falls back to `exports/`) and `nexus library [--kind deck\|doc] [--json]` |
| `crates/studio/src/backend/templates.rs`, `crates/studio/ui/templates.js` | shared doc/deck template import (explicit path or chooser/drop bytes), design switching, and the separate apply-after-import action; export routing in `backend/exports.rs` and `backend/doc.rs` defaults to the design's source package. |
| `crates/studio/ui/doc/` | the doc editor UI (vanilla ES modules, no build step, embedded in `backend.rs::embedded`): `inline.js` (inline Markdown <-> runs), `model.js` + `edits.js` (blocks, local ops, structural edits as pure functions), `dom.js`/`blocks.js` (runs and blocks as DOM), `editor.js` (one contenteditable surface; everything structural is intercepted), `menus.js`, `toolbar.js`, `comments.js`, `doc.js`, `doc.css`; `syntax.js` paints code blocks from the backend's `Highlight` spans with the CSS Custom Highlight API (no DOM changes), `source.js` is the Markdown source view (`DocSource`/`DocApplySource`, ⇧⌘M) and `vim.js` its vim engine, a port of `crates/tui/src/vim.rs` checked by `ui-dev/vim-unit.mjs`. No `innerHTML` anywhere (tested). Pure modules are tested with `node crates/studio/ui-dev/doc-unit.mjs` (run by `backend/ui_tests.rs` when node 22+ exists); `doc-e2e.mjs` drives it in Chromium against `doc-mock.js`. Spacing in `doc.css` equals `doc::LAYOUT` (tested) |
| `crates/tui/src/studio.rs` | the terminal half of the link: `/studio [file]` opens the current doc or deck, a canvas doc edit sends `Open` and a `DocFocus` for the first changed block, doc comments become `Comment on docs/x.doc.json · b4 "quote": text` messages (thread, revision and block text, no image), and `studio: viewing b12, selected "…"` rides on the next message |
| `crates/core/src/connectors/` | MCP connectors (docs/connectors.md): `protocol`/`transport`/`client` speak both protocol families over HTTP and stdio, `registry` tracks status and what the model may see (naming, >30-tool meta-tool, policy, rug-pull pins), `exec` runs a call through the approval flow, `import`/`secrets`/`config` cover other tools' configs, the keychain and `[connectors.*]` |
| `crates/core/src/db.rs` | SQLite (rusqlite bundled): sessions, messages, usage, citations, model prefs; schema changes are new steps appended in `crates/core/src/db/migrations.rs`; a shipped step is never edited; regenerate `crates/core/tests/fixtures/schema-snapshots/` with `NEXUS_UPDATE_SNAPSHOTS=1` and review it. |
| `crates/core/src/sync.rs` | Phase 3 merge engine: changeset types, per-table registry (cursor + apply rules), build/apply/ack, file blobs + zip bundles |
| `crates/core/src/interactive/canvas.rs`, `crates/tui/src/interactive/canvas.rs` | nexus-ui `Canvas`: core holds the shapes and `Anim` keyframes, their limits, and `Session::canvas_frame` (shapes resolved at `Session::clock`); the TUI owns a braille dot rasterizer (`Dots`, also used by Chart line/area/scatter) and draws the frame. Each panel `Entry` sets the clock from when it was first shown; `Widgets::animating` makes `events.rs` redraw at ~15 fps while a moving canvas is visible and the window has focus |
| `crates/tui/src/interactive/bold.rs`, `gallery_tests.rs` | the Bold panel look: label column, eighth-cell bars, pills, button kinds, tab geometry; chrome is `panel_chrome` in `interactive.rs`; `Theme::on` picks text for fills. `gallery_snapshots.txt` is the visual regression net (`NEXUS_UPDATE_SNAPSHOTS=1`). Layout containers (`Row`/`Grid`/`Stack`/`Spacer`) lay out in `components::container_regions`, shared by height, drawing and hit regions. Motion (glides, draw-in) is `Session` state in `crates/core/src/interactive/motion.rs`, off unless `Widgets::motion` (`/motion`, db setting `motion`) turns it on per frame in `Entry::advance_clock`. Live panels — `@Every` timers, `@Keys` bindings, clickable canvas shapes, local-only action checks — are `crates/core/src/interactive/play.rs`; the TUI ticks timers in `advance_clock` and saves panel state through the throttled `persist::flush_settled`. Number/Gauge/Heatmap/Md/Tree/Board/Diagram: core types and limits in `crates/core/src/interactive/extras.rs`, drawing in `crates/tui/src/interactive/extras.rs`; panel diagrams reach the image painter through `Widgets::diagram_placements` (painted after panels in `ui/history.rs`) |
| `crates/core/src/memory/` | the memory book: `memory_chapters` + `memories` tables (uuid ids, `deleted_at` tombstones, LWW sync), `book.rs` table of contents and editor Markdown, `recall.rs` keyword hints; `/memory tidy` asks the chat model to review and clean the memory with its own tool, in the conversation; the `memory` tool is `tools/memory_tool.rs` |
| `crates/core/src/panels.rs` | durable nexus-ui panel storage: per-panel saved state (`panel_states`) and panels pinned as space mini-apps (`pinned_panels`), the `panel_app` tool's list/read/patch; both tables sync (`sync.rs`). Records are `interactive::PanelSnapshot` (versioned, ≤48 KiB). Terminal half: `crates/tui/src/interactive/persist.rs` |
| `crates/core/src/appserver.rs` | localhost static server for model-created apps (port 8642) |
| `crates/core/src/host/` | `nexus host` HTTP/SSE API, OpenAI-wire gateway, sync/worker routes, Cloudflare setup, tunnel and sleep guard |
| `crates/tui/src/app_view.rs` | `AppView`: wraps `App` (Deref) with all view state — composer, popup chrome/caches, render state, theme, status line |
| `crates/tui/src/flows/` | popup flow methods moved out of core by 2e (`open_*_popup`, `move_*_selection`, `confirm_*`) |
| `crates/tui/src/composer.rs` | TextArea ops: editing, clipboard, slash/`@` autocomplete (catalog stays core) |
| `crates/core/src/highlight.rs` | the one tree-sitter highlighter (17 languages, Markdown with injected fences): `highlight`/`segments` give byte spans tagged with a theme-free `Role`; `Role::color_key` is the doc palette mapping. Frontends map roles to colors (`ui/style.rs::syntax_style` in the TUI). |
| `crates/tui/src/vim.rs`, `editor.rs` | vim keys over any `TextArea` (on by default, `/vim on\|off`, db setting `keymap`): `Vim::key` returns `Handled`/`Passthrough`/`Submit`/`Leave` so callers keep their own Enter/Esc; its undo is per change (snapshots via `set_lines`). `editor.rs` repaints a field's custom highlights each frame (syntax spans cached per source, the Visual selection). Used by the composer, nexus-ui `Input`/`Textarea` (`interactive/inputs.rs`) and code `Editor` (`interactive/practice.rs`, through `Session::set_code`) |
| `crates/tui/src/ui/` | ratatui rendering; `history.rs` is the main view, `popups/` one module per popup, `markdown.rs` the styled renderer |
| `crates/core/src/config.rs`, `space.rs` | credentials + spaces layout (XDG dirs) |
| `crates/core/src/filesearch.rs` | finding files to import (`/files`): `recent` (newest in Downloads/Desktop/Documents), `search` (Spotlight `mdfind`, else a time-boxed gitignore-aware walk of home; hidden paths and `~/Library` left out), `complete` for typed `/…`/`~/…` paths |

> Phase 2 status: the workspace split (2a), `ToolExecutor` seam (2b),
> command/event seam (2c), CLI on the seam (2d), and the status-event
> conversion are landed. **2e (view state extraction) is done**: `App` is
> domain-only with zero TUI deps; the TUI's `AppView` owns the composer,
> popup chrome, and render caches and feeds view feedback back via
> `AppEvent` (`ComposerSet`/`ComposerClear`/`ViewportReset`/
> `HistoryInvalidated`). Core still carries a few domain-coupled display
> fields (documented in `app/mod.rs`): `context_total`/`last_cache_rate`
> (auto-compaction), `unread`, `notifications`, the files/scripts/sessions
> caches (system-prompt inputs), and the research steer/stage state.
>
> Phase 4 host support lives in `crates/core/src/host/`: the app-actor
> HTTP/SSE API, bearer auth, `/v1` routes, hash-checked sync blob transfer,
> the OpenAI-wire gateway (every backend routes, Codex via the
> chat↔Responses translation in `provider/openrouter.rs`), and opt-in
> `cloudflared`/sleep-guard lifecycle. Registered app UUIDs are public
> capabilities; host/provider secrets must never enter snapshots, events, or
> app URLs. Keep tunnel and Cloudflare setup paths opt-in; tests must remain
> network-free and must not launch sidecars.

## Web search

`search(mode=web)` needs no search key or instance URL. Auto failover order is
LangSearch (configured key) → SearXNG (configured URL) → public keyless Exa →
the session provider's native web search → DuckDuckGo → Brave. Explicit `exa`
and `provider` settings are available; LangSearch/SearXNG remain optional.
Exa calls share a two-permit toolbox budget; tests inject loopback endpoints,
and default search endpoints use dead local ports under `cfg(test)`.

Native search uses the session's own model and backend, never another provider.
OpenRouter uses its server search tool ($0.007 per request plus tokens); record
response-reported usage/cost. Codex uses the
ChatGPT subscription Responses route; OpenAI uses billed Responses web search
(tool and token rates). Go/local skip native search. Exa ignores recency and
reports that in the result header when a filter was requested.

## Things to know before changing code

- **One UI pass**: the popups share a chrome system (`ui/popups/chrome.rs`) —
  new popups should reuse it rather than re-implementing borders/hints.
- **Streaming must not jump**: `ui/history.rs` has viewport-pinning tests for
  reasoning/tool-call growth — the viewport must stay put while content
  streams above it. There are dedicated tests for this; don't regress them.
- **Tool results are capped**: `tools.rs` caps result sizes and marks
  unchanged results with a note — keep that behavior when editing tools.
- **Batch discipline**: models are encouraged to use the `batch` tool for
  independent operations; `batch` validates nesting, sizes, and read-only
  classification (see `tools.rs` tests).
- **Model-facing text**: `crates/core/assets/prompts/base.md` is the base prompt
  bundled into every session; changes there affect all backends.
- **Skills**: `crates/core/assets/find-skills-SKILL.md` is the model-facing skill for
  discovering/installing skills. Skills are SKILL.md packs with sandboxed
  Python virtualenvs (`src/skills.rs`).
- **Local embeddings**: `local:embeddinggemma-2[:270m]` talks to Ollama's native
  `/api/embed` (`brew install ollama`, `ollama serve`, `ollama pull
  embeddinggemma-2:270m`, or the multimodal `embeddinggemma-2`). Vectors live in
  `cache.chunk_embeddings` with the `EmbedIdentity` key plus their own
  location/text, so any identity change re-embeds lazily and a vector from
  another identity is never ranked. Prefixes are applied once, inside
  `Embedder` (`document`/`query`), never in transport code. Images are embedded
  directly for search when the probe shows the runtime accepts them; scanned
  PDFs are found by name. The model views any imported PDF page with
  `files(action=pdf_page)`, rendered on demand using poppler's `pdftoppm`.
  Tests use `provider::embedding::tests::MockOllama`; never hit a real Ollama.
- **Past-chat search**: `db/chats.rs` maintains rebuildable FTS5 message chunks
  and `(message_id, seq, EmbedIdentity)` vectors in device-local `cache.db` only.
  Index persisted user/assistant content, never tool/system/reasoning text or
  incognito. Synced messages enter the same newest-first backfill; connection-local
  TEMP triggers remove cached rows on message/session deletion or message edits,
  including sync deletes. `app/chats.rs` uses the existing `AppEvent::Embed`
  channel and exactly the file embedder (local files means local chats), behind
  files, with bounded batches and progress in `/files`. `tools/chats.rs` implements
  `search mode=chats` (read-only hybrid RRF, active space only) and the `chat:` URL
  transcript that `fetch_url` pages; a separate tool would not fit the chat prompt
  token ceiling. Never in research searcher toolboxes.
- **Background jobs use the session model**: `App::session_backend` resolves
  the session's own model for titles, compaction and the `LlmDecider`;
  `/memory tidy` uses it in the conversation too.
  Embeddings (`app/embeddings.rs`: local EmbeddingGemma 2 → backend
  default → none) and media generation (`App::generation_backend`, the backend's
  `default_*_gen_model`) are computed, not configured. The removed keys
  are ignored on read and rejected as unknown on set.
  `App::embedding_model` is runtime state, not a setting. New background jobs
  call `session_backend()`; `/config` has no per-feature model rows.
- **Don't commit agent tooling**: `.claude/`, `.superpowers/`,
  `.pi-subagents/` are gitignored — leave them out of commits.

[ratatui]: https://github.com/ratatui/ratatui
