CREATE TABLE work_read_cursors (
    member TEXT NOT NULL,
    scope TEXT NOT NULL,
    rev INTEGER NOT NULL CHECK (rev >= 0),
    PRIMARY KEY (member, scope)
);
