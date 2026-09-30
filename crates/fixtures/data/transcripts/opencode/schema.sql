-- Synthetic approximation of an OpenCode session store (SQLite), for parser tests.
-- Stream A: verify against a real store before relying on column names, and update this file
-- through a contract change if the real shape differs. Message and part payloads are JSON.
CREATE TABLE session (
  id            TEXT PRIMARY KEY,
  project_id    TEXT NOT NULL,
  parent_id     TEXT,
  directory     TEXT NOT NULL,
  title         TEXT NOT NULL,
  version       TEXT NOT NULL,
  time_created  INTEGER NOT NULL,
  time_updated  INTEGER NOT NULL
);
CREATE TABLE message (
  id            TEXT PRIMARY KEY,
  session_id    TEXT NOT NULL REFERENCES session(id),
  time_created  INTEGER NOT NULL,
  time_updated  INTEGER NOT NULL,
  data          TEXT NOT NULL
);
CREATE TABLE part (
  id            TEXT PRIMARY KEY,
  message_id    TEXT NOT NULL REFERENCES message(id),
  session_id    TEXT NOT NULL REFERENCES session(id),
  time_created  INTEGER NOT NULL,
  time_updated  INTEGER NOT NULL,
  data          TEXT NOT NULL
);
CREATE INDEX message_session ON message(session_id, time_created);
CREATE INDEX part_message ON part(message_id, time_created);
