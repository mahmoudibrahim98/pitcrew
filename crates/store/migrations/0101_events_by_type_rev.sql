-- 0101_events_by_type_rev: lets `before()` with a type filter walk back by revision without
-- sorting every matching row. Owned by stream C.

CREATE INDEX IF NOT EXISTS events_by_type_rev ON events (type, rev);
