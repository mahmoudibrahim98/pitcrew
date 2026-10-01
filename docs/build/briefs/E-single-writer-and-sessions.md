# Brief E · Single writer, activity references, sessions and workspace routes

- **Stream:** E · Hub: work model. **Branch:** `s/E/single-writer-and-sessions`. **Paths:**
  `crates/hub-work/**`, `crates/store/migrations/02*` (+ `Cargo.lock`).
- **First read:** [README.md](README.md), then [E-work-core.md](E-work-core.md) (merged),
  `docs/build/streams/E.md` (work packages 2–5), `crates/hub-work` on `main` (its README), and
  `docs/build/contracts/api-v1.md` (sessions, workspace, activity).

## Goal

Close the latent issues the review of `E-work-core` found before other streams (F's back office,
G's sync and D's runner) start writing work events. Then add the read routes the desktop needs
next, and the reference index behind activity filters.

## What to build

1. **Single writer and robust projections** (from the review):
   - A `task_created` whose key clashes with an existing task must not fail the projection, or
     it stalls the log. Resolve the clash deterministically in `apply`, for example by keeping
     the first and recording the clash, and test it, including through
     `Store::open_with` catch-up.
   - Allocate the next task number by `key_prefix`, not by project. Map a uniqueness violation to
     `409 conflict`, never a 500.
   - **The single-writer rule:**
     - Document in the README and the crate docs that exactly one `Arc<WorkService>` per store
       appends work events.
     - Make `apply` deterministic under a racing writer: a `task_moved` whose `from` doesn't
       match the current status is ignored. Document this and test it.
     - Add a concurrent `create_task` test (many tasks at once: unique keys, no errors).
2. **Nits from the review:**
   - Check authorization before validation, so a forbidden agent gets 403, not 400.
   - Stamp `on_behalf_of` only for agent callers.
   - Internal errors are logged in full, but the client gets a generic message: no SQLite text,
     table or constraint names.
   - Refuse an answer that is empty text.
   - Cover all 12 device routes in the bare-mount test.
   - Widen the document-versus-columns check (project, workstream, key, number, labels,
     dependencies).
   - Add a test that pins `Task`'s serialized field set to `Tasks::VERSION`, so a protocol change
     forces a version bump.
   - Make the mock-hub parity test reproducible: commit its small dump script under
     `crates/hub-work/tests/`, or drop the ignored test.
3. **The activity reference index:**
   - A projection table, e.g. `work_event_refs(rev, project, workstream, task, session)`, filled
     for every event from the work tables. A task event knows its workstream and project; a
     session event knows its link.
   - A query: `revs_matching(filter, before_rev, limit) -> (Vec<u64>, scanned_to)`, bounded the
     same way as the contract's activity paging.
   - Stream H wires it into `GET /v1/events?project=&workstream=` later, through a small trait
     you define here (`EventRefs`), so there is no dependency cycle. Document the trait.
4. **Routes:**
   - `GET /v1/sessions?machine=&workstream=&task=&state=` and `GET /v1/sessions/{id}`, from the
     sessions projection, as in api-v1. These are device routes; check whether api-v1 marks
     them **agent**, and follow the contract.
   - `GET /v1/workspace` → `{ workspace, rev }`.
   - Tests through the routes, including the filters and 404s.
5. **Dispatch recording:**
   - `POST /v1/tasks/{id}/dispatch` as api-v1 describes: the 409 rules, assigning when
     unassigned, and choosing the machine and folder.
   - It appends `dispatch_started` and calls a `Dispatcher` trait (defined here) to start the
     session. Stream D's runner implements it later; until then a test double records calls.
   - If the dispatcher fails, append `dispatch_finished` with the failure, so no dispatch is left
     dangling.

## Acceptance

- Every item above has a test.
- Rebuild equals incremental, still over all tables including the new one.
- The 10k-task list timings stay within target.
- The routes match api-v1, including agent and device scope.
- No unwrap or expect in library code; no unsafe; migrations follow the store rules.

## Out of scope

- `TaskPatch` and `PATCH /v1/tasks`, and project and workstream creation: the contract change
  `integrator/work-edits` is in progress, and a follow-up brief adopts it.
- The back office's `Commands` trait (stream F defines it; you implement it later).
- Running sessions (stream D).
