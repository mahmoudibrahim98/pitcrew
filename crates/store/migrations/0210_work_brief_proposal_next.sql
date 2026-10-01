-- 0210_work_brief_proposal_next: a proposed brief's next step.
-- Owned by stream E; filled by the `work.briefs` projection (crates/hub-work), version 2.
--
-- From version 2, `work_brief_proposals` holds each target's **pending** proposal only: the newest
-- `brief_proposed`, until a `brief_accepted` for the same target puts a newer brief in force and
-- removes it. `next` is the proposal's next step (NULL when it has none).

ALTER TABLE work_brief_proposals ADD COLUMN next TEXT;
