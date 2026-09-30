-- 0203_work_sessions: sessions as the hub sees them, and dispatches.
-- Owned by stream E; filled by the `work.sessions` projection (crates/hub-work).

CREATE TABLE IF NOT EXISTS work_sessions (
  id            TEXT    PRIMARY KEY,
  rev           INTEGER NOT NULL,
  engine        TEXT    NOT NULL,
  native_id     TEXT    NOT NULL,
  machine       TEXT    NOT NULL,
  cwd           TEXT    NOT NULL,
  branch        TEXT,
  title         TEXT,
  agent         TEXT,
  workstream    TEXT,
  task          TEXT,
  link_basis    TEXT,
  state         TEXT    NOT NULL,
  status_line   TEXT,
  started       INTEGER NOT NULL,
  last_activity INTEGER NOT NULL,
  terminal      TEXT,
  parent        TEXT
) STRICT;
CREATE INDEX IF NOT EXISTS work_sessions_by_rev ON work_sessions (rev);
CREATE INDEX IF NOT EXISTS work_sessions_by_machine ON work_sessions (machine, rev);
CREATE INDEX IF NOT EXISTS work_sessions_by_workstream ON work_sessions (workstream, rev);
CREATE INDEX IF NOT EXISTS work_sessions_by_task ON work_sessions (task, rev);
CREATE INDEX IF NOT EXISTS work_sessions_by_agent ON work_sessions (agent, rev);

CREATE TABLE IF NOT EXISTS work_dispatches (
  id      TEXT    PRIMARY KEY,
  rev     INTEGER NOT NULL,
  task    TEXT    NOT NULL,
  agent   TEXT    NOT NULL,
  session TEXT,
  brief   TEXT    NOT NULL,
  started INTEGER NOT NULL,
  ended   INTEGER,           -- NULL while the dispatch is active
  outcome TEXT,
  summary TEXT
) STRICT;
CREATE INDEX IF NOT EXISTS work_dispatches_by_rev ON work_dispatches (rev);
CREATE INDEX IF NOT EXISTS work_dispatches_by_task ON work_dispatches (task, agent, ended);
CREATE INDEX IF NOT EXISTS work_dispatches_by_session ON work_dispatches (session);
