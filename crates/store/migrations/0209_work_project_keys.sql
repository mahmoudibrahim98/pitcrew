-- 0209_work_project_keys: project keys are unique.
-- Owned by stream E; filled by the `work.projects` projection (crates/hub-work), version 2.
--
-- `POST /v1/projects` refuses a key another project has (409), and the projection keeps the first
-- project with a key: a `project_created` whose key another project already holds is not applied.
-- Only a writer racing the hub's one WorkService, or a log imported from elsewhere, can append one.
-- This index makes the rule the table's own.
--
-- An older build did not keep keys unique. Its rows are rebuilt from the log on the next open (the
-- projection's version went up); until then, repeated keys are dropped here, keeping the earliest
-- project, so the index can be created.

DELETE FROM work_projects
 WHERE EXISTS (SELECT 1 FROM work_projects AS first
                WHERE first.key = work_projects.key
                  AND (first.rev < work_projects.rev
                       OR (first.rev = work_projects.rev AND first.id < work_projects.id)));
DELETE FROM work_project_members WHERE project NOT IN (SELECT id FROM work_projects);

-- The unique index replaces the plain one from 0201.
DROP INDEX IF EXISTS work_projects_by_key;
CREATE UNIQUE INDEX IF NOT EXISTS work_projects_by_unique_key ON work_projects (key);
