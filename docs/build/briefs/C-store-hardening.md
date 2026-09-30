# Brief C · Store hardening (follow-up)

- **Stream:** C · Store. **Branch:** `s/C/store-hardening`. **Paths:** `crates/store/*`,
  `crates/store/src/**`, `crates/store/tests/**`, `crates/store/benches/**`,
  `crates/store/migrations/01*`. **Do not edit** `0001_init.sql`.
- **First read:** [README.md](README.md), then [C-open-and-log.md](C-open-and-log.md) (the
  previous brief, now merged) and the current `crates/store` code on `main`.

## Goal

Fix what the review of `s/C/open-and-log` found, before the API (stream H) starts relying on
`append` and `subscribe`. Small, targeted changes with a test for each.

## What to fix

1. **Ordered notifications.** In `append`, send the new range on the broadcast channel **before**
   releasing the connection lock. Otherwise two appenders can publish their ranges out of order
   (A commits 1..10, B commits and sends 11..15, then A sends 1..10). Document the pattern for
   the stream's hello frame: subscribe, then `latest_rev()`, then skip ranges with
   `to_rev <= N`. Test: concurrent appenders from several threads; the received ranges are
   contiguous and increasing.
2. **Immediate write transactions.** `append` uses a deferred transaction and reads
   `MAX(rev)` first. When another connection writes, WAL mode fails with SQLITE_BUSY at once and
   ignores `busy_timeout`. Use
   `transaction_with_behavior(TransactionBehavior::Immediate)`. Test: two `Store`s on one file
   appending concurrently both succeed.
3. **Migrations across processes.** Run each migration in an IMMEDIATE transaction and re-check
   `schema_migrations` inside it, skipping versions another process already applied. Test: two
   stores opening a fresh file at the same time both succeed.
4. **Type-filtered paging.** `before()` with a type filter sorts every matching row, because the
   only type index is `(type, at)`. Add `migrations/0101_events_by_type_rev.sql` with an index
   on `(type, rev)`, and a filtered `before` timing in the perf test.
5. **Embedding check.** Test that the `build.rs`-embedded list equals `load_dir` of
   `migrations/`, so the codegen step is covered.
6. **Smaller items:**
   - Error messages that print their cause twice (`{source}` in the message plus `#[source]`):
     keep only one.
   - A duplicate event id becomes a distinct error (e.g. `Error::DuplicateEvent { id }`), so a
     retrying runner can tell it apart from other failures.
   - The `EventFilter` doc says "empty matches everything", but `types([])` matches nothing. Make
     them agree; empty should match everything.
   - Clamp the broadcast capacity to a sane maximum.
   - A unit test asserting `journal_mode`, `synchronous` and `foreign_keys` after open.
   - Round-trip one value of **every** `EventBody` variant, not just the fixture's 12.
   - `RevRange::len` uses `saturating_add`. A `.sql` extension check is case-insensitive, or a
     wrongly cased file fails the build.
7. **Document the migration rules** in `crates/store/README.md` for other streams:
   - no `BEGIN` / `COMMIT`, `VACUUM` or journal pragmas;
   - tables are `STRICT`;
   - foreign keys are on during migrations (use `PRAGMA defer_foreign_keys=ON` for table
     rebuilds);
   - a merged migration is never edited.
8. **Report the perf numbers**: 10,000 events appended in batches of 100, `since(rev, 100)`, and
   filtered `before`, on this machine.

## Out of scope

Projections, NFS mode and leases (next brief), and a reader connection pool.
