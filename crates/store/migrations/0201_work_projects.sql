-- 0201_work_projects: projects and workstreams, with workstream locations.
-- Owned by stream E; filled by the `work.projects` projection (crates/hub-work).

CREATE TABLE IF NOT EXISTS work_projects (
  id       TEXT    PRIMARY KEY,
  rev      INTEGER NOT NULL,
  key      TEXT    NOT NULL,  -- ProjectKey, e.g. PAP
  name     TEXT    NOT NULL,
  status   TEXT    NOT NULL,
  lead     TEXT    NOT NULL,
  start    TEXT,
  due      TEXT,
  root     TEXT,              -- Location as JSON
  external TEXT    NOT NULL   -- ExternalRef[] as JSON
) STRICT;
CREATE INDEX IF NOT EXISTS work_projects_by_rev ON work_projects (rev);
CREATE INDEX IF NOT EXISTS work_projects_by_key ON work_projects (key);

CREATE TABLE IF NOT EXISTS work_project_members (
  project  TEXT    NOT NULL REFERENCES work_projects (id) ON DELETE CASCADE,
  position INTEGER NOT NULL,
  member   TEXT    NOT NULL,
  PRIMARY KEY (project, position)
) STRICT;
CREATE INDEX IF NOT EXISTS work_project_members_by_member ON work_project_members (member);

CREATE TABLE IF NOT EXISTS work_workstreams (
  id       TEXT    PRIMARY KEY,
  rev      INTEGER NOT NULL,
  project  TEXT    NOT NULL,
  name     TEXT    NOT NULL,
  status   TEXT    NOT NULL,
  health   TEXT    NOT NULL,
  external TEXT    NOT NULL   -- ExternalRef[] as JSON
) STRICT;
CREATE INDEX IF NOT EXISTS work_workstreams_by_rev ON work_workstreams (rev);
CREATE INDEX IF NOT EXISTS work_workstreams_by_project ON work_workstreams (project, rev);

-- Folders (and branches) whose sessions belong to a workstream.
CREATE TABLE IF NOT EXISTS work_locations (
  workstream TEXT    NOT NULL REFERENCES work_workstreams (id) ON DELETE CASCADE,
  position   INTEGER NOT NULL,
  machine    TEXT    NOT NULL,
  path       TEXT    NOT NULL,
  branch     TEXT,
  PRIMARY KEY (workstream, position)
) STRICT;
CREATE INDEX IF NOT EXISTS work_locations_by_machine ON work_locations (machine, path);
