-- 0301_office_run_log: the back office's run log and saved state. Owned by stream F.
-- Both tables belong to the `office.runs` projection (crates/office): it replays the office's
-- rules over the log, so a rebuild gives the same rows.

-- What the office did: one row per action a rule produced, with its outcome. `action` is the
-- action as JSON. `outcome` is `emitted` (handed to the caller to apply), `capped` (over a cap,
-- not applied) or `refused` (it broke the "never" list or the move rules, not applied); `reason`
-- says which cap (`rule`, `global`) or which rule (e.g. `marks_done`).
CREATE TABLE IF NOT EXISTS office_runs (
  rev     INTEGER NOT NULL CHECK (rev > 0),
  seq     INTEGER NOT NULL CHECK (seq >= 0),
  event   TEXT    NOT NULL,
  at      INTEGER NOT NULL,
  rule    TEXT    NOT NULL,
  action  TEXT    NOT NULL,
  outcome TEXT    NOT NULL CHECK (outcome IN ('emitted', 'capped', 'refused')),
  reason  TEXT,
  PRIMARY KEY (rev, seq)
) STRICT;

CREATE INDEX IF NOT EXISTS office_runs_by_rule ON office_runs (rule, rev);
CREATE INDEX IF NOT EXISTS office_runs_by_outcome ON office_runs (outcome, rev);

-- The office's state after the last applied event (what it knows, rule memos, cap windows), one
-- JSON value per key, so each event rewrites only what changed.
CREATE TABLE IF NOT EXISTS office_state (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
) STRICT;
