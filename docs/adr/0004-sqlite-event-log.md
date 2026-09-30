# 0004. SQLite with an append-only event log; NFS-safe mode

- **Status:** Accepted
- **Date:** 2026-09-30

## Context

Every page in PitCrew answers "what is the state of the work, and what happened lately?". Several
writers (runners on different machines, agents, the back office, later other people) change the
same workspace. JSON files rewritten in place race and lose history. HPC home directories are
often on NFS, where SQLite's WAL mode is unsafe because it needs shared memory.

## Decision

- **One SQLite store per workspace**, on the workspace's primary machine, with versioned,
  forward-only migrations. The daemon refuses to open a store newer than itself.
- **Every change is an event** in an append-only `events` table: a ULID id, time, author,
  `on_behalf_of`, a type and a JSON payload (`crates/protocol/src/events.rs`). Triggers reject
  updates and deletes. Pages, recaps, the Inbox and search are **projections** of the log.
- Each event also has a local, gap-free `rev`, which is what the UI's delta stream counts.
- **Migration ranges belong to streams** (0000–0099 stream 0, 0100 C, 0200 E, 0300 F, 0400 G,
  0500 O), so parallel work never collides on a migration number.
- **NFS:** the daemon detects a network filesystem. It prefers node-local disk for the store
  (configurable state directory). Otherwise it uses `journal_mode=DELETE` with
  `locking_mode=EXCLUSIVE` under a single-host lease.
- Runners keep their own small SQLite (transcript offsets, parse caches). **Transcripts are never
  copied to the hub**; runners send derived events.

## Consequences

- History, receipts and "what changed since you last looked" come for free from the log.
- Projections must be rebuildable from the log; tests do exactly that.
- Full-text search uses FTS5 over events, tasks, briefs and comments, and transcript text only
  where sharing allows it.
