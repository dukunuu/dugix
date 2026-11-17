CREATE TABLE chunk_embeddings (
            file_id TEXT NOT NULL,
            seq INTEGER NOT NULL,
            vec BLOB NOT NULL, identity TEXT NOT NULL DEFAULT '', location TEXT NOT NULL DEFAULT '', text TEXT NOT NULL DEFAULT '',
            PRIMARY KEY (file_id, seq)
        );
CREATE TABLE decisions (
            id INTEGER PRIMARY KEY,
            at TEXT,
            session_id TEXT,
            tool TEXT,
            action TEXT,
            class TEXT,
            summary TEXT,
            by TEXT,
            verdict TEXT,
            p_in_scope REAL,
            risk TEXT,
            user_answer TEXT,
            cost REAL
        );
CREATE VIRTUAL TABLE file_chunks USING fts5(
            file_id UNINDEXED,
            seq UNINDEXED,
            location UNINDEXED,
            text
        );
CREATE TABLE file_index_state (
            file_id TEXT PRIMARY KEY,
            mtime INTEGER NOT NULL DEFAULT 0,
            status TEXT NOT NULL DEFAULT '',
            updated_at TEXT NOT NULL
        , extractor INTEGER NOT NULL DEFAULT 0);
CREATE VIRTUAL TABLE message_chunks USING fts5(
            message_id UNINDEXED, seq UNINDEXED, text
        );
CREATE TABLE message_embeddings (
            message_id TEXT NOT NULL,
            seq INTEGER NOT NULL,
            identity TEXT NOT NULL,
            vec BLOB NOT NULL,
            PRIMARY KEY (message_id, seq, identity)
        );
CREATE TABLE message_index (
            message_id TEXT PRIMARY KEY
        );
CREATE TABLE model_prices (
            model_id TEXT PRIMARY KEY,
            backend TEXT NOT NULL,
            prompt_price REAL NOT NULL,
            completion_price REAL NOT NULL,
            cache_read_price REAL,
            cache_write_price REAL,
            updated_at TEXT NOT NULL
        );
CREATE TABLE session_masks (
            session_id TEXT PRIMARY KEY,
            mask_through INTEGER NOT NULL,
            updated_at TEXT NOT NULL
        );
CREATE TABLE session_workspaces (
            session_id TEXT PRIMARY KEY,
            root TEXT NOT NULL,
            updated_at TEXT NOT NULL
        , mode TEXT);
CREATE TABLE subagent_runs (
            id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL,
            number INTEGER NOT NULL,
            prompt TEXT NOT NULL,
            model TEXT NOT NULL,
            writable INTEGER NOT NULL,
            max_steps INTEGER NOT NULL,
            status TEXT NOT NULL,
            started_at TEXT NOT NULL,
            finished_at TEXT,
            steps INTEGER NOT NULL,
            tokens INTEGER NOT NULL,
            cost REAL,
            secs REAL NOT NULL,
            review TEXT,
            rows TEXT NOT NULL
        );
CREATE TABLE tool_events (
            id INTEGER PRIMARY KEY,
            at TEXT NOT NULL,
            session_id TEXT NOT NULL,
            tool TEXT NOT NULL,
            action TEXT NOT NULL,
            ok INTEGER NOT NULL,
            error_kind TEXT,
            latency_ms INTEGER NOT NULL,
            result_chars INTEGER NOT NULL
        );
CREATE TABLE watch_jobs (
            watch_id TEXT PRIMARY KEY,
            job_id TEXT NOT NULL
        );
CREATE TABLE web_cache (
            url_norm   TEXT PRIMARY KEY,
            url        TEXT NOT NULL,
            title      TEXT,
            text       TEXT NOT NULL,
            fetched_at TEXT NOT NULL
        );
CREATE INDEX subagent_runs_session
            ON subagent_runs(session_id, started_at);
CREATE INDEX tool_events_at ON tool_events(at);

INSERT INTO chunk_embeddings (file_id, seq, vec, identity, location, text) VALUES ('file-1', 0, X'0000803F', 'test', 'line 1', 'Synthetic embedding');
PRAGMA user_version = 0;
