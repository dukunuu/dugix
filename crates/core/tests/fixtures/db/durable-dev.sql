CREATE TABLE app_settings (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            , scope TEXT NOT NULL DEFAULT 'sync', updated_at TEXT);
CREATE TABLE chunk_embeddings (
                file_id TEXT NOT NULL,
                seq INTEGER NOT NULL,
                vec BLOB NOT NULL,
                PRIMARY KEY (file_id, seq)
            );
CREATE TABLE citations (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                space_id    TEXT NOT NULL,
                report_file TEXT NOT NULL,
                url         TEXT NOT NULL,
                title       TEXT
            , sync_id TEXT);
CREATE TABLE device_meta (
                device_id TEXT PRIMARY KEY,
                created_at TEXT NOT NULL
            );
CREATE VIRTUAL TABLE file_chunks USING fts5(
                file_id UNINDEXED,
                seq UNINDEXED,
                location UNINDEXED,
                text
            );
CREATE TABLE files (
                id TEXT PRIMARY KEY,
                space_id TEXT NOT NULL,
                name TEXT NOT NULL,
                hash TEXT NOT NULL,
                size INTEGER NOT NULL,
                created_at TEXT NOT NULL, mtime INTEGER NOT NULL DEFAULT 0, updated_at TEXT,
                UNIQUE(space_id, name)
            );
CREATE TABLE job_runs (
                id TEXT PRIMARY KEY, job_id TEXT NOT NULL, device_id TEXT NOT NULL,
                started_at TEXT NOT NULL, finished_at TEXT, status TEXT NOT NULL,
                iters INTEGER NOT NULL, cost_usd REAL NOT NULL, branch TEXT,
                summary TEXT NOT NULL, updated_at TEXT NOT NULL
            );
CREATE TABLE jobs (
                id TEXT PRIMARY KEY, space_id TEXT NOT NULL, session_id TEXT NOT NULL,
                device_id TEXT NOT NULL, name TEXT NOT NULL, prompt TEXT NOT NULL,
                root TEXT NOT NULL, mode TEXT NOT NULL, trigger_kind TEXT NOT NULL,
                interval_minutes INTEGER NOT NULL, run_at TEXT, stop_command TEXT,
                max_iters INTEGER NOT NULL, max_cost_usd REAL NOT NULL,
                max_wall_minutes INTEGER NOT NULL, notify TEXT NOT NULL,
                enabled INTEGER NOT NULL, last_run_at TEXT, updated_at TEXT NOT NULL,
                kind TEXT NOT NULL DEFAULT 'agent', topic TEXT
            );
CREATE TABLE memories (
            id          TEXT PRIMARY KEY,
            space_id    TEXT NOT NULL,
            chapter_id  TEXT NOT NULL,
            text        TEXT NOT NULL,
            created_at  TEXT NOT NULL,
            updated_at  TEXT NOT NULL,
            deleted_at  TEXT
        );
CREATE TABLE memory_chapters (
            id          TEXT PRIMARY KEY,
            space_id    TEXT NOT NULL,
            title       TEXT NOT NULL,
            description TEXT NOT NULL DEFAULT '',
            pinned      INTEGER NOT NULL DEFAULT 0,
            tidied_at   TEXT,
            created_at  TEXT NOT NULL,
            updated_at  TEXT NOT NULL,
            deleted_at  TEXT
        );
CREATE TABLE messages (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL REFERENCES sessions(id),
                role TEXT NOT NULL,
                content TEXT NOT NULL,
                created_at TEXT NOT NULL
            , model TEXT, reasoning TEXT, tokens INTEGER, secs REAL, phrase TEXT, persona TEXT, cost REAL);
CREATE TABLE model_prefs (
                id TEXT PRIMARY KEY,
                favorite INTEGER NOT NULL DEFAULT 0,
                last_used TEXT
            , reasoning TEXT, updated_at TEXT);
CREATE TABLE model_prices (
                model_id TEXT PRIMARY KEY,
                backend TEXT NOT NULL,
                prompt_price REAL NOT NULL,
                completion_price REAL NOT NULL,
                updated_at TEXT NOT NULL
            );
CREATE TABLE panel_states (
            session_id TEXT NOT NULL,
            panel_key  TEXT NOT NULL,
            record     TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            PRIMARY KEY (session_id, panel_key)
        );
CREATE TABLE pinned_panels (
            id         TEXT PRIMARY KEY,
            space_id   TEXT NOT NULL,
            name       TEXT NOT NULL,
            title      TEXT NOT NULL,
            program    TEXT NOT NULL,
            record     TEXT NOT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
CREATE TABLE session_sources (
                session_id TEXT NOT NULL,
                url_norm   TEXT NOT NULL, flag TEXT, updated_at TEXT,
                PRIMARY KEY (session_id, url_norm)
            );
CREATE TABLE sessions (
                id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                model TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            , slug TEXT, space_id TEXT, compact_summary TEXT, compact_through INTEGER NOT NULL DEFAULT 0, web_mode INTEGER NOT NULL DEFAULT 0, swarm_mode INTEGER NOT NULL DEFAULT 0, kind TEXT NOT NULL DEFAULT 'chat', research_parent_id TEXT);
CREATE TABLE spaces (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL UNIQUE,
                created_at TEXT NOT NULL
            , updated_at TEXT);
CREATE TABLE swarm_personas (
                session_id TEXT NOT NULL,
                ord        INTEGER NOT NULL,
                name       TEXT NOT NULL,
                model      TEXT NOT NULL,
                persona    TEXT NOT NULL
            );
CREATE TABLE sync_state (
                peer_id TEXT NOT NULL,
                table_name TEXT NOT NULL,
                pull_cursor TEXT,
                push_cursor TEXT,
                last_synced_at TEXT,
                PRIMARY KEY (peer_id, table_name)
            );
CREATE TABLE sync_tombstones (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                table_name TEXT NOT NULL,
                row_id TEXT NOT NULL,
                deleted_at TEXT NOT NULL
            );
CREATE TABLE usage_log (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                created_at TEXT NOT NULL,
                session_id TEXT,
                space_id TEXT,
                backend TEXT NOT NULL,
                model TEXT NOT NULL,
                prompt_tokens INTEGER NOT NULL,
                completion_tokens INTEGER NOT NULL,
                cache_read_tokens INTEGER NOT NULL DEFAULT 0,
                cache_creation_tokens INTEGER NOT NULL DEFAULT 0,
                cost REAL
            , cost_is_provider INTEGER, sync_id TEXT, updated_at TEXT, prompt_convention TEXT, kind TEXT, tools TEXT);
CREATE TABLE watches (
                id             TEXT PRIMARY KEY,
                space_id       TEXT NOT NULL,
                topic          TEXT NOT NULL,
                interval_hours INTEGER NOT NULL,
                session_id     TEXT NOT NULL,
                last_run_at    TEXT
            , updated_at TEXT);
CREATE TABLE web_cache (
                url_norm   TEXT PRIMARY KEY,
                url        TEXT NOT NULL,
                title      TEXT,
                text       TEXT NOT NULL,
                fetched_at TEXT NOT NULL
            );
CREATE INDEX idx_citations_space ON citations(space_id);
CREATE UNIQUE INDEX idx_citations_sync_id ON citations(sync_id);
CREATE INDEX idx_memories_chapter ON memories(chapter_id);
CREATE INDEX idx_memories_space ON memories(space_id);
CREATE INDEX idx_memory_chapters_space
            ON memory_chapters(space_id);
CREATE INDEX idx_messages_session
                ON messages(session_id, created_at);
CREATE INDEX idx_pinned_panels_space
            ON pinned_panels(space_id, name);
CREATE INDEX idx_swarm_personas_session
                ON swarm_personas(session_id, ord);
CREATE INDEX idx_sync_tombstones_table ON sync_tombstones(table_name, row_id);
CREATE INDEX idx_usage_log_created ON usage_log(created_at);
CREATE INDEX idx_usage_log_model ON usage_log(model);
CREATE INDEX idx_usage_log_session ON usage_log(session_id, created_at);
CREATE UNIQUE INDEX idx_usage_log_sync_id ON usage_log(sync_id);

INSERT INTO spaces (id, name, created_at) VALUES ('default', 'default', '2026-01-01'), ('space-1', 'work', '2026-01-02');
INSERT INTO sessions (id, title, model, created_at, updated_at, space_id) VALUES ('session-1', 'Synthetic session', 'test/model', '2026-01-02', '2026-01-02', 'space-1');
INSERT INTO messages (id, session_id, role, content, created_at) VALUES ('message-1', 'session-1', 'user', 'Synthetic message', '2026-01-02');
INSERT INTO app_settings (key, value) VALUES ('helper_model', 'old/model'), ('vim', 'true');
INSERT INTO usage_log (sync_id, created_at, session_id, space_id, backend, model, prompt_tokens, completion_tokens) VALUES ('usage-1', '2026-01-02', 'session-1', 'space-1', 'test', 'test/model', 10, 2);
PRAGMA user_version = 3;
