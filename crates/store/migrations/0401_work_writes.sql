-- 0401_work_writes: outward writes to GitHub and Jira, each approved by a person first
-- (api-v1.md, "Outward writes"). Owned by stream G; filled by the `work.writes` projection
-- (crates/hub-work, from `write_proposed`, `ask_answered`, `write_started` and `write_finished`).

CREATE TABLE IF NOT EXISTS work_writes (
  ask          TEXT    PRIMARY KEY,   -- the approval ask, also the write's id
  rev          INTEGER NOT NULL,      -- of its `write_proposed`
  task         TEXT,
  integration  TEXT    NOT NULL,
  cause        TEXT,                  -- the event that implied it
  proposal     TEXT    NOT NULL,      -- WriteProposal as JSON
  state        TEXT    NOT NULL,      -- WriteState
  attempts     INTEGER NOT NULL CHECK (attempts >= 0),
  proposed_at  INTEGER NOT NULL,
  answered_at  INTEGER,
  answered_by  TEXT,
  finished_at  INTEGER,
  result       TEXT                   -- WriteResult as JSON
) STRICT;
CREATE INDEX IF NOT EXISTS work_writes_by_task ON work_writes (task, rev);
CREATE INDEX IF NOT EXISTS work_writes_by_state ON work_writes (state, rev);
-- Not unique: a projection never fails on what an event says. The command refuses a second
-- proposal for one cause; a second writer's duplicate is kept as it is.
CREATE INDEX IF NOT EXISTS work_writes_by_cause ON work_writes (cause);
