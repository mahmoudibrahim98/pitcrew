-- 0207_work_task_clashes: task_created events refused because another task held their key.
-- Owned by stream E; filled by the `work.tasks` projection (crates/hub-work), version 2.
--
-- Task keys are unique (work_tasks_by_key). A task_created whose key another task already holds
-- would break that index and stall the log, so the projection keeps the first task and records
-- the refused event here instead. Only a writer racing the hub's one WorkService (or a log
-- imported from elsewhere) can produce one.

CREATE TABLE IF NOT EXISTS work_task_clashes (
  rev        INTEGER PRIMARY KEY,  -- the refused task_created
  task       TEXT    NOT NULL,     -- the task it would have created or re-stated
  key_prefix TEXT    NOT NULL,
  number     INTEGER NOT NULL,
  holder     TEXT    NOT NULL      -- the task that holds the key
) STRICT;
CREATE INDEX IF NOT EXISTS work_task_clashes_by_task ON work_task_clashes (task);
