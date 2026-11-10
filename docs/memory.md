# Memory

Memory is what nexus keeps about you between chats. It is organized like a
book: **chapters** of short **facts**, plus the space's **docs**, behind a
generated **table of contents**. The model writes it while it talks to you, a
table of contents is always in the prompt, and full chapters are opened only
when they matter.

## Goals

- **Nothing forgotten that matters, nothing loaded that doesn't.** Long
  contexts get worse as they grow, so the always-loaded part stays small.
- **No extra model calls in the normal path.** Writing, organizing and
  recalling use the chat model already running plus plain SQLite queries.
- **Cache friendly.** The system block stays byte-identical for a session;
  per-turn additions are persisted rows replayed verbatim.
- **Durable and synced.** Memory lives in `nexus.db`, not in a file, so it
  survives restores and syncs between devices.
- **No new dependencies.**

## Pieces

| Piece | What | Cost |
|---|---|---|
| Store | `memory_chapters` + `memories` tables, uuid ids, `deleted_at` tombstones, LWW sync | — |
| Table of contents | Generated at session start: pinned chapters in full, other chapters as `title (count) — description`, docs as title + headings | Cached prefix |
| `memory` tool | `read` / `add` / `update` / `delete` / `chapter`; the chat model files each fact under a chapter | One tool round on cached context |
| Hints | Before a message is sent, a keyword match against chapters and docs; up to 3 names are attached to the message ("possibly relevant: …") | ~10–30 tokens, persisted |
| Tidy | `/memory tidy` asks the chat model to review and clean the memory with its own tool, in the conversation | Only when asked |
| Editing | `/memory edit` (TUI: Ctrl+K in the space picker), `nexus memory --edit`, host document: the book as Markdown, saved back as a diff | — |

### Store

```sql
memory_chapters(id, space_id, title, description, pinned, tidied_at,
                created_at, updated_at, deleted_at)
memories(id, space_id, chapter_id, text, created_at, updated_at, deleted_at)
```

Both are durable and registered in `sync.rs` as `updated_at` LWW tables. A
delete sets `deleted_at` (and bumps `updated_at`), so it syncs like any edit.
The legacy `spaces/<name>/memory.md` is imported once
into a pinned "About you" chapter and renamed to `memory.md.imported`.

### Table of contents (system prompt)

`## Memory` is rendered when a session starts (same freeze rule as before:
mid-session writes appear next session, and the model already saw them in
the transcript). Pinned chapters are listed in full; other chapters show
count and description; docs show their title and top headings. Usage rules
live in the `memory` tool's description, not in the prompt.

### Hints (per message)

`send_message` scores chapters (title, description, facts) and docs (title,
headings) by distinct query-term overlap, skips short messages and slash
commands, drops anything already hinted this session, and keeps the top 3.
The hint is stored as a `recall` row just before the user row and replayed
as a prefix of that user message, so later requests stay byte-identical.

### Tidy

`/memory tidy` asks the chat model to review and clean the memory with its own
tool, in the conversation.

## Not doing

- Embeddings or clustering for memory: the writer files facts under
  chapters; chat search and docs cover the long tail.
- Per-reply background extraction: replaced by the tool.
- Automatic tidy: only a status hint when a chapter passes 40 facts.
