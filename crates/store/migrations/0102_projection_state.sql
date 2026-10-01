-- 0102_projection_state: how far each projection has applied the log. Owned by stream C.
-- `version` is the projection's own version: on open, an older one is rebuilt and a newer one
-- refused. `rev` is the last revision applied to it.

CREATE TABLE IF NOT EXISTS projection_state (
  name    TEXT    PRIMARY KEY,
  version INTEGER NOT NULL CHECK (version >= 0),
  rev     INTEGER NOT NULL CHECK (rev >= 0)
) STRICT;
