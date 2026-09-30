-- 0202_work_tasks: tasks, their subtasks, dependencies and labels.
-- Owned by stream E; filled by the `work.tasks` projection (crates/hub-work).
--
-- `doc` is the whole task as the API returns it (the protocol's `Task` as JSON), so a list is one
-- indexed query. The other columns are what lists filter and sort on, and the child tables index
-- subtasks, dependencies and labels one by one. The projection keeps all of them in step.

CREATE TABLE IF NOT EXISTS work_tasks (
  id          TEXT    PRIMARY KEY,
  rev         INTEGER NOT NULL,  -- the revision that created it; lists are in this order
  project     TEXT    NOT NULL,
  key_prefix  TEXT    NOT NULL,  -- the project key in the task key: PAP in PAP-4
  number      INTEGER NOT NULL,  -- the number in the task key: 4 in PAP-4
  workstream  TEXT,
  status      TEXT    NOT NULL,
  assignee    TEXT,
  doc         TEXT    NOT NULL   -- Task as JSON
) STRICT;
-- Keys are allocated per project and never shared.
CREATE UNIQUE INDEX IF NOT EXISTS work_tasks_by_key ON work_tasks (key_prefix, number);
CREATE INDEX IF NOT EXISTS work_tasks_by_rev ON work_tasks (rev);
CREATE INDEX IF NOT EXISTS work_tasks_by_project ON work_tasks (project, rev);
CREATE INDEX IF NOT EXISTS work_tasks_by_project_number ON work_tasks (project, number);
CREATE INDEX IF NOT EXISTS work_tasks_by_workstream ON work_tasks (workstream, rev);
CREATE INDEX IF NOT EXISTS work_tasks_by_assignee ON work_tasks (assignee, rev);
CREATE INDEX IF NOT EXISTS work_tasks_by_status ON work_tasks (status, rev);

-- Checklist lines, in order. `agent` is NULL for a person's line, else the agent whose plan it
-- mirrors (SubtaskSource::AgentPlan).
CREATE TABLE IF NOT EXISTS work_subtasks (
  task     TEXT    NOT NULL REFERENCES work_tasks (id) ON DELETE CASCADE,
  position INTEGER NOT NULL,
  id       TEXT    NOT NULL,
  text     TEXT    NOT NULL,
  done     INTEGER NOT NULL,
  agent    TEXT,
  PRIMARY KEY (task, position)
) STRICT;
CREATE INDEX IF NOT EXISTS work_subtasks_by_agent ON work_subtasks (agent);

-- `task` is blocked by `blocked_by`.
CREATE TABLE IF NOT EXISTS work_task_deps (
  task       TEXT    NOT NULL REFERENCES work_tasks (id) ON DELETE CASCADE,
  position   INTEGER NOT NULL,
  blocked_by TEXT    NOT NULL,
  PRIMARY KEY (task, position)
) STRICT;
CREATE INDEX IF NOT EXISTS work_task_deps_by_blocker ON work_task_deps (blocked_by);

CREATE TABLE IF NOT EXISTS work_task_labels (
  task     TEXT    NOT NULL REFERENCES work_tasks (id) ON DELETE CASCADE,
  position INTEGER NOT NULL,
  label    TEXT    NOT NULL,
  PRIMARY KEY (task, position)
) STRICT;
CREATE INDEX IF NOT EXISTS work_task_labels_by_label ON work_task_labels (label);
