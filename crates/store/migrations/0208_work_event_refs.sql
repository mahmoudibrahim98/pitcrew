-- 0208_work_event_refs: which project, workstream, task and session each event is about, for
-- the activity filters (GET /v1/events?project=&workstream=&task=&session=).
-- Owned by stream E; filled by the `work.refs` projection (crates/hub-work).
--
-- One row per event that is about any of them; NULL where it is not. An event names some of them
-- itself (a turn names its session, a comment its task); the rest come from what was known when
-- the event was applied: a session's link, a task's workstream and project, a workstream's
-- project, a dispatch's or an ask's task and session. `work_ref_parents` holds that knowledge.
-- It repeats a little of the other work tables, because a projection never reads another's.

CREATE TABLE IF NOT EXISTS work_event_refs (
  rev        INTEGER PRIMARY KEY,  -- the event's revision
  project    TEXT,
  workstream TEXT,
  task       TEXT,
  session    TEXT
) STRICT;
CREATE INDEX IF NOT EXISTS work_event_refs_by_project ON work_event_refs (project, rev)
  WHERE project IS NOT NULL;
CREATE INDEX IF NOT EXISTS work_event_refs_by_workstream ON work_event_refs (workstream, rev)
  WHERE workstream IS NOT NULL;
CREATE INDEX IF NOT EXISTS work_event_refs_by_task ON work_event_refs (task, rev)
  WHERE task IS NOT NULL;
CREATE INDEX IF NOT EXISTS work_event_refs_by_session ON work_event_refs (session, rev)
  WHERE session IS NOT NULL;

-- What each thing belongs to, as of the latest event about it.
CREATE TABLE IF NOT EXISTS work_ref_parents (
  kind       TEXT NOT NULL,  -- workstream | task | session | dispatch | ask
  id         TEXT NOT NULL,
  project    TEXT,           -- of a workstream or a task
  workstream TEXT,           -- of a task, or a session's link
  task       TEXT,           -- a session's link, or the task of a dispatch or an ask
  session    TEXT,           -- the session of a dispatch or an ask
  link_basis TEXT,           -- why a session is linked (LinkBasis)
  PRIMARY KEY (kind, id)
) STRICT, WITHOUT ROWID;
