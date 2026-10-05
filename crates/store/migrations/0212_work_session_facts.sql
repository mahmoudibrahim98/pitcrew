-- 0212_work_session_facts: what a session's transcript says about it.
-- Owned by stream E; filled by the `work.sessions` projection (crates/hub-work), version 5.
--
-- `model` is the model the transcript last recorded (`session_discovered`, then
-- `session_updated`); `account` the CLI account home its transcript is in (`~/.claude`). Both NULL
-- until the runner states them.

ALTER TABLE work_sessions ADD COLUMN model TEXT;
ALTER TABLE work_sessions ADD COLUMN account TEXT;
