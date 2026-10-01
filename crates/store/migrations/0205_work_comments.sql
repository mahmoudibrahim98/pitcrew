-- 0205_work_comments: comments on tasks and workstreams, with mentions.
-- Owned by stream E; filled by the `work.comments` projection (crates/hub-work).

CREATE TABLE IF NOT EXISTS work_comments (
  event        TEXT    PRIMARY KEY,  -- the comment_posted event's id
  rev          INTEGER NOT NULL,
  at           INTEGER NOT NULL,
  author       TEXT    NOT NULL,
  on_behalf_of TEXT,
  task         TEXT,
  workstream   TEXT,
  text         TEXT    NOT NULL
) STRICT;
CREATE INDEX IF NOT EXISTS work_comments_by_task ON work_comments (task, rev);
CREATE INDEX IF NOT EXISTS work_comments_by_workstream ON work_comments (workstream, rev);

CREATE TABLE IF NOT EXISTS work_comment_mentions (
  comment  TEXT    NOT NULL REFERENCES work_comments (event) ON DELETE CASCADE,
  position INTEGER NOT NULL,
  member   TEXT    NOT NULL,
  PRIMARY KEY (comment, position)
) STRICT;
CREATE INDEX IF NOT EXISTS work_comment_mentions_by_member ON work_comment_mentions (member);
