-- 0204_work_asks: what needs a member's answer (the Inbox).
-- Owned by stream E; filled by the `work.asks` projection (crates/hub-work).

CREATE TABLE IF NOT EXISTS work_asks (
  id          TEXT    PRIMARY KEY,
  rev         INTEGER NOT NULL,
  kind        TEXT    NOT NULL,
  from_member TEXT    NOT NULL,
  to_member   TEXT    NOT NULL,
  task        TEXT,
  session     TEXT,
  title       TEXT    NOT NULL,
  body        TEXT    NOT NULL,
  options     TEXT    NOT NULL,  -- String[] as JSON
  receipts    TEXT    NOT NULL,  -- Receipt[] as JSON
  state       TEXT    NOT NULL,
  answer      TEXT,              -- Answer as JSON
  created     INTEGER NOT NULL
) STRICT;
CREATE INDEX IF NOT EXISTS work_asks_by_rev ON work_asks (rev);
CREATE INDEX IF NOT EXISTS work_asks_by_to ON work_asks (to_member, state, rev);
CREATE INDEX IF NOT EXISTS work_asks_by_task ON work_asks (task);
