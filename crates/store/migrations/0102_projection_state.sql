-- 0102_projection_state: how far each projection has applied the log. Owned by stream C.
-- `version` is the projection's own version; a change triggers a rebuild on open.

CREATE TABLE IF NOT EXISTS projection_state (
  name    TEXT    PRIMARY KEY,
  version INTEGER NOT NULL,
  rev     INTEGER NOT NULL
) STRICT;
