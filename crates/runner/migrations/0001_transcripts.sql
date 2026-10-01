-- Runner-side index: one row per transcript this runner has seen.
-- Offsets and sizes are u64 in Rust, stored as INTEGER (i64); transcripts never reach 2^63 bytes.
CREATE TABLE transcripts (
    session_id      TEXT PRIMARY KEY,           -- SessionId (ULID), assigned on first discovery
    engine          TEXT NOT NULL,              -- "claude", "codex", "opencode"
    path            TEXT NOT NULL,
    inner_id        TEXT NOT NULL DEFAULT '',   -- '' when the file holds one session
    cursor          TEXT NOT NULL,              -- JSON Cursor, saved after the sink accepts
    size            INTEGER NOT NULL DEFAULT 0, -- size when last read
    mtime           INTEGER NOT NULL DEFAULT 0, -- mtime (ms) when last read
    identity        TEXT,                       -- file identity (device:inode) when last read
    caught_up       INTEGER NOT NULL DEFAULT 0, -- the cursor had reached the end at that size
    generation      INTEGER NOT NULL DEFAULT 0, -- bumped each time the file is re-indexed
    discovered      INTEGER NOT NULL DEFAULT 0, -- session_discovered accepted by the sink
    emitted_through INTEGER,                    -- highest item offset whose events were accepted
    meta            TEXT,                       -- JSON SessionMeta, last known
    facts           TEXT,                       -- JSON derived state (state, open tool calls)
    UNIQUE (path, inner_id)
);
