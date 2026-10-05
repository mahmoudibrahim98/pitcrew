-- 0402_work_write_retries: who asked to send a failed outward write again (api-v1.md, "Outward
-- writes", "Retrying"). Owned by stream G; filled by the `work.writes` projection from
-- `write_retry_requested`, and cleared by the `write_started` that uses it.

ALTER TABLE work_writes ADD COLUMN retry_requested_by TEXT;
