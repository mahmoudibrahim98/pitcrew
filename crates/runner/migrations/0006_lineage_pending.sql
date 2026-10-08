-- Sub-agents indexed before the runner kept the parents their transcripts name (Codex's,
-- OpenCode's, and Claude's older top-level sidechains) were stated with none. Each is looked up
-- once at the next start (`Watcher::reparent`) and leaves this list.
CREATE TABLE lineage_pending (session_id TEXT PRIMARY KEY);
INSERT INTO lineage_pending (session_id)
    SELECT session_id FROM transcripts
    WHERE discovered = 1
      AND json_valid(meta) AND json_extract(meta, '$.is_subagent') = 1
      AND (facts IS NULL OR NOT json_valid(facts)
           OR json_extract(facts, '$.parent') IS NULL
           OR json_extract(facts, '$.parent') = 'none');
