-- 0001_init: the event log and schema bookkeeping. Owned by stream 0.
-- Migration number ranges belong to streams (docs/build/ownership.md):
--   00xx stream 0 · 01xx C (store) · 02xx E (work) · 03xx F (recap, office)
--   04xx G (sync) · 05xx O (import)
-- Rules: never edit a migration after it is merged; add a new one. Every table is STRICT.

CREATE TABLE IF NOT EXISTS meta (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
) STRICT;

-- The append-only log. `rev` is the hub's local, gap-free order (StreamFrame revisions);
-- `id` is the event's ULID. Nothing is updated or deleted here.
CREATE TABLE IF NOT EXISTS events (
  rev          INTEGER PRIMARY KEY,
  id           TEXT    NOT NULL UNIQUE,
  at           INTEGER NOT NULL,
  workspace    TEXT    NOT NULL,
  author       TEXT    NOT NULL,
  on_behalf_of TEXT,
  type         TEXT    NOT NULL,
  data         TEXT    NOT NULL
) STRICT;

CREATE INDEX IF NOT EXISTS events_by_type ON events (type, at);
CREATE INDEX IF NOT EXISTS events_by_author ON events (author, at);

CREATE TRIGGER IF NOT EXISTS events_no_update BEFORE UPDATE ON events
BEGIN SELECT RAISE(ABORT, 'events are append-only'); END;
CREATE TRIGGER IF NOT EXISTS events_no_delete BEFORE DELETE ON events
BEGIN SELECT RAISE(ABORT, 'events are append-only'); END;

INSERT OR IGNORE INTO meta (key, value) VALUES ('protocol', '1');
