-- 0200_work_directory: machines, members, personas and teams, as the hub knows them.
-- Owned by stream E; filled by the `work.directory` projection (crates/hub-work).
-- Every table has `rev`, the revision of the event that first created the row; lists are
-- returned in that order. JSON columns hold protocol types serialized with serde.

CREATE TABLE IF NOT EXISTS work_machines (
  id       TEXT    PRIMARY KEY,
  rev      INTEGER NOT NULL,
  name     TEXT    NOT NULL,
  kind     TEXT    NOT NULL,
  info     TEXT,             -- MachineInfo as JSON
  liveness TEXT    NOT NULL
) STRICT;
CREATE INDEX IF NOT EXISTS work_machines_by_rev ON work_machines (rev);

CREATE TABLE IF NOT EXISTS work_members (
  id      TEXT    PRIMARY KEY,
  rev     INTEGER NOT NULL,
  kind    TEXT    NOT NULL,  -- human | agent
  handle  TEXT    NOT NULL,
  name    TEXT    NOT NULL,
  owner   TEXT,              -- the owning person, for agents
  persona TEXT
) STRICT;
CREATE INDEX IF NOT EXISTS work_members_by_rev ON work_members (rev);
CREATE INDEX IF NOT EXISTS work_members_by_owner ON work_members (owner);

CREATE TABLE IF NOT EXISTS work_personas (
  id              TEXT    PRIMARY KEY,
  rev             INTEGER NOT NULL,
  name            TEXT    NOT NULL,
  engine          TEXT    NOT NULL,
  model           TEXT,
  instructions    TEXT,
  permission_mode TEXT    NOT NULL
) STRICT;
CREATE INDEX IF NOT EXISTS work_personas_by_rev ON work_personas (rev);

CREATE TABLE IF NOT EXISTS work_teams (
  id   TEXT    PRIMARY KEY,
  rev  INTEGER NOT NULL,
  name TEXT    NOT NULL,
  lead TEXT    NOT NULL
) STRICT;
CREATE INDEX IF NOT EXISTS work_teams_by_rev ON work_teams (rev);

CREATE TABLE IF NOT EXISTS work_team_members (
  team     TEXT    NOT NULL REFERENCES work_teams (id) ON DELETE CASCADE,
  position INTEGER NOT NULL,
  member   TEXT    NOT NULL,
  PRIMARY KEY (team, position)
) STRICT;
CREATE INDEX IF NOT EXISTS work_team_members_by_member ON work_team_members (member);
