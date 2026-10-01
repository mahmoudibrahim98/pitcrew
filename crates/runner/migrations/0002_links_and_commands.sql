-- Items whose events the sink accepted during a read whose cursor is not saved yet. A replay after
-- a crash skips them. Emptied for a transcript each time its cursor is saved. Keys identify an item
-- by offset, kind and id (see derive::item_key), since offsets alone are not unique.
CREATE TABLE accepted_items (
    session_id TEXT NOT NULL,
    item_key   INTEGER NOT NULL,          -- u64 key, stored with the same bits as an i64
    PRIMARY KEY (session_id, item_key)
) WITHOUT ROWID;

-- Terminals the runner started, and the session each one runs once its transcript is found.
CREATE TABLE terminals (
    terminal_id   TEXT PRIMARY KEY,       -- TerminalId (ULID)
    native_target TEXT,                   -- tmux target, e.g. "pitcrew:@12", to find it again
    session_id    TEXT UNIQUE,            -- NULL until the session is known
    engine        TEXT,                   -- NULL for a terminal linked by hand
    native_id     TEXT,                   -- the CLI's session id, when chosen or known at start
    cwd           TEXT NOT NULL DEFAULT '',
    started_at    INTEGER NOT NULL        -- ms
);

-- Outcomes of hub commands by id: a command id seen again returns its stored outcome.
CREATE TABLE commands (
    command_id TEXT PRIMARY KEY,          -- CommandId (ULID)
    outcome    TEXT NOT NULL,             -- JSON CommandOutcome
    at         INTEGER NOT NULL           -- ms
);
