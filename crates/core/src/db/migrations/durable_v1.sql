CREATE TABLE IF NOT EXISTS sessions (
    id TEXT PRIMARY KEY,
    title TEXT NOT NULL,
    model TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    slug TEXT,
    space_id TEXT,
    compact_summary TEXT,
    compact_through INTEGER NOT NULL DEFAULT 0,
    web_mode INTEGER NOT NULL DEFAULT 0,
    swarm_mode INTEGER NOT NULL DEFAULT 0,
    kind TEXT NOT NULL DEFAULT 'chat',
    research_parent_id TEXT
);
CREATE TABLE IF NOT EXISTS messages (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id),
    role TEXT NOT NULL,
    content TEXT NOT NULL,
    created_at TEXT NOT NULL,
    model TEXT,
    reasoning TEXT,
    tokens INTEGER,
    secs REAL,
    cost REAL,
    phrase TEXT,
    persona TEXT
);
CREATE INDEX IF NOT EXISTS idx_messages_session
    ON messages(session_id, created_at);
CREATE TABLE IF NOT EXISTS model_prefs (
    id TEXT PRIMARY KEY,
    favorite INTEGER NOT NULL DEFAULT 0,
    last_used TEXT,
    reasoning TEXT,
    updated_at TEXT
);
CREATE TABLE IF NOT EXISTS app_settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL,
    scope TEXT NOT NULL DEFAULT 'sync',
    updated_at TEXT
);
CREATE TABLE IF NOT EXISTS spaces (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    created_at TEXT NOT NULL,
    updated_at TEXT
);
CREATE TABLE IF NOT EXISTS files (
    id TEXT PRIMARY KEY,
    space_id TEXT NOT NULL,
    name TEXT NOT NULL,
    hash TEXT NOT NULL,
    size INTEGER NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT,
    UNIQUE(space_id, name)
);
CREATE TABLE IF NOT EXISTS citations (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    sync_id     TEXT,
    space_id    TEXT NOT NULL,
    report_file TEXT NOT NULL,
    url         TEXT NOT NULL,
    title       TEXT
);
CREATE INDEX IF NOT EXISTS idx_citations_space ON citations(space_id);
CREATE TABLE IF NOT EXISTS session_sources (
    session_id TEXT NOT NULL,
    url_norm   TEXT NOT NULL,
    flag TEXT,
    updated_at TEXT,
    PRIMARY KEY (session_id, url_norm)
);
CREATE TABLE IF NOT EXISTS watches (
    id             TEXT PRIMARY KEY,
    space_id       TEXT NOT NULL,
    topic          TEXT NOT NULL,
    interval_hours INTEGER NOT NULL,
    session_id     TEXT NOT NULL,
    last_run_at    TEXT,
    updated_at     TEXT
);
CREATE TABLE IF NOT EXISTS jobs (
    id TEXT PRIMARY KEY, space_id TEXT NOT NULL, session_id TEXT NOT NULL,
    device_id TEXT NOT NULL, name TEXT NOT NULL, prompt TEXT NOT NULL,
    root TEXT NOT NULL, mode TEXT NOT NULL, trigger_kind TEXT NOT NULL,
    interval_minutes INTEGER NOT NULL, run_at TEXT, stop_command TEXT,
    max_iters INTEGER NOT NULL, max_cost_usd REAL NOT NULL,
    max_wall_minutes INTEGER NOT NULL, notify TEXT NOT NULL,
    enabled INTEGER NOT NULL, last_run_at TEXT, updated_at TEXT NOT NULL,
    kind TEXT NOT NULL DEFAULT 'agent', topic TEXT
);
CREATE TABLE IF NOT EXISTS job_runs (
    id TEXT PRIMARY KEY, job_id TEXT NOT NULL, device_id TEXT NOT NULL,
    started_at TEXT NOT NULL, finished_at TEXT, status TEXT NOT NULL,
    iters INTEGER NOT NULL, cost_usd REAL NOT NULL, branch TEXT,
    summary TEXT NOT NULL, updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS swarm_personas (
    session_id TEXT NOT NULL,
    ord        INTEGER NOT NULL,
    name       TEXT NOT NULL,
    model      TEXT NOT NULL,
    persona    TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_swarm_personas_session
    ON swarm_personas(session_id, ord);
CREATE TABLE IF NOT EXISTS usage_log (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    sync_id TEXT,
    created_at TEXT NOT NULL,
    session_id TEXT,
    space_id TEXT,
    backend TEXT NOT NULL,
    model TEXT NOT NULL,
    prompt_tokens INTEGER NOT NULL,
    completion_tokens INTEGER NOT NULL,
    cache_read_tokens INTEGER NOT NULL DEFAULT 0,
    cache_creation_tokens INTEGER NOT NULL DEFAULT 0,
    prompt_convention TEXT,
    cost REAL,
    cost_is_provider INTEGER,
    updated_at TEXT,
    kind TEXT,
    tools TEXT
);
CREATE INDEX IF NOT EXISTS idx_usage_log_created ON usage_log(created_at);
CREATE INDEX IF NOT EXISTS idx_usage_log_model ON usage_log(model);
CREATE INDEX IF NOT EXISTS idx_usage_log_session ON usage_log(session_id, created_at);
CREATE TABLE IF NOT EXISTS sync_tombstones (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    table_name TEXT NOT NULL,
    row_id TEXT NOT NULL,
    deleted_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_sync_tombstones_table ON sync_tombstones(table_name, row_id);
CREATE TABLE IF NOT EXISTS device_meta (
    device_id TEXT PRIMARY KEY,
    created_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS sync_state (
    peer_id TEXT NOT NULL,
    table_name TEXT NOT NULL,
    pull_cursor TEXT,
    push_cursor TEXT,
    last_synced_at TEXT,
    PRIMARY KEY (peer_id, table_name)
);
CREATE TABLE IF NOT EXISTS panel_states (
    session_id TEXT NOT NULL,
    panel_key TEXT NOT NULL,
    record TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (session_id, panel_key)
);
CREATE TABLE IF NOT EXISTS pinned_panels (
    id TEXT PRIMARY KEY,
    space_id TEXT NOT NULL,
    name TEXT NOT NULL,
    title TEXT NOT NULL,
    program TEXT NOT NULL,
    record TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_pinned_panels_space
    ON pinned_panels(space_id, name);
CREATE TABLE IF NOT EXISTS memory_chapters (
    id TEXT PRIMARY KEY,
    space_id TEXT NOT NULL,
    title TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    pinned INTEGER NOT NULL DEFAULT 0,
    tidied_at TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    deleted_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_memory_chapters_space
    ON memory_chapters(space_id);
CREATE TABLE IF NOT EXISTS memories (
    id TEXT PRIMARY KEY,
    space_id TEXT NOT NULL,
    chapter_id TEXT NOT NULL,
    text TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    deleted_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_memories_space ON memories(space_id);
CREATE INDEX IF NOT EXISTS idx_memories_chapter ON memories(chapter_id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_citations_sync_id ON citations(sync_id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_usage_log_sync_id ON usage_log(sync_id);
