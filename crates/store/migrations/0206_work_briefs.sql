-- 0206_work_briefs: "Where it stands" in force, and the latest proposal per target.
-- Owned by stream E; filled by the `work.briefs` projection (crates/hub-work).
-- `target_kind` is project | workstream.

CREATE TABLE IF NOT EXISTS work_briefs (
  target_kind TEXT    NOT NULL,
  target_id   TEXT    NOT NULL,
  rev         INTEGER NOT NULL,
  text        TEXT    NOT NULL,
  next        TEXT,
  pinned      INTEGER NOT NULL,
  source      TEXT    NOT NULL,
  updated     INTEGER NOT NULL,
  receipts    TEXT    NOT NULL,  -- Receipt[] as JSON
  PRIMARY KEY (target_kind, target_id)
) STRICT;
CREATE INDEX IF NOT EXISTS work_briefs_by_rev ON work_briefs (rev);

CREATE TABLE IF NOT EXISTS work_brief_proposals (
  target_kind TEXT    NOT NULL,
  target_id   TEXT    NOT NULL,
  event       TEXT    NOT NULL,  -- the brief_proposed event's id
  rev         INTEGER NOT NULL,
  at          INTEGER NOT NULL,
  author      TEXT    NOT NULL,
  text        TEXT    NOT NULL,
  receipts    TEXT    NOT NULL,  -- Receipt[] as JSON
  PRIMARY KEY (target_kind, target_id)
) STRICT;
