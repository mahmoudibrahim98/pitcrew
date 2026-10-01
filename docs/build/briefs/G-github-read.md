# Brief G · GitHub, read side: client core, snapshots and upstream changes

- **Stream:** G · Integrations. **Branch:** `s/G/github-read`. **Paths:**
  `crates/sync-github/**` (+ `Cargo.lock`).
- **First read:** [README.md](README.md), then `docs/build/streams/G.md`, ADR-0006, ADR-0007
  (the `sync` mover and `can_move`), and `ExternalRef`, `ExternalSystem`, `Task`, `Workstream`,
  `Receipt` and `Mover` in `crates/protocol`.

## Goal

Read GitHub issues, pull requests and milestones for a set of repositories efficiently and
safely, and turn what changed upstream into typed **upstream changes**. This brief is read-only
and never writes to GitHub. Applying changes to the hub (stream E's commands) and the approval
queue for outward writes come in later briefs.

## What to build

1. **A transport seam.**
   - A `Transport` trait: an async `send(Request) -> Result<Response>` with method, URL,
     headers and body.
   - The crate has **no HTTP client dependency** in this brief. The real HTTPS transport is a
     dependency decision the integrator makes later, so tests use a recorded-fixture transport.
   - The token is passed to the transport per request as a header value that **never** appears
     in `Debug`, errors or logs. Test that.
2. **A client core** over the transport:
   - REST v3 with `Accept: application/vnd.github+json` and a pinned `X-GitHub-Api-Version`.
   - Pagination through `Link` headers, with a cap on pages per call.
   - **Conditional requests:** keep the `ETag` and `Last-Modified` per URL; a `304` reuses the
     cached result.
   - **Incremental reads:** issues `?state=all&since=<last seen updated_at>&sort=updated`.
     Pull requests are listed by `updated` descending, stopping at the last seen.
   - **Rate limits:** read `x-ratelimit-remaining` and `x-ratelimit-reset`. For secondary limits,
     honour `retry-after`, or back off exponentially with a cap. Never busy-loop; return a
     `RateLimited { until }` outcome instead of sleeping for minutes.
   - **Bounds:** body size caps, field length caps (titles, bodies, labels), at most N items per
     sync. Unknown fields are ignored; malformed items are skipped and counted.
3. **Snapshots and upstream changes:**
   - A serde-serialisable `SyncState` per repository: cursors, ETags, and the last snapshot of
     each tracked item's owned fields.
   - The crate is pure: `sync(state, transport) -> (new_state, Vec<UpstreamChange>)`. The caller
     persists the state; it never goes into the event log.
   - `UpstreamChange` covers:
     - an issue opened, retitled, its body edited, closed (completed or not planned), reopened,
       relabelled, reassigned, or its milestone changed;
     - a milestone created, renamed or closed;
     - a pull request opened, merged or closed, with its linked issues (closing keywords in the
       body, `owner/repo#n` and `#n`).
   - Each change carries an `ExternalRef` (`github`, `owner/repo#n`, url) and the upstream time.
4. **A field-ownership table** (a const table plus docs), deciding who owns each field:
   - title and body belong to upstream;
   - status is mirrored only through the `Sync` mover rules; an upstream close proposes `done`,
     applied only if `TaskStatus::can_move(.., Mover::Sync)` allows it, and in-progress work is
     never touched;
   - labels belong to upstream; the assignee belongs to the hub.

   Provide `fn plan(change, current: Option<&Task>) -> Vec<Intent>`, where `Intent` is an
   abstract hub action: create a task from an issue, update owned fields, propose a move with
   `Mover::Sync`, attach a pull request as a `Receipt`, or raise a conflict ask. The hub applies
   intents later.
5. **Recorded fixtures:**
   - A simple on-disk format (request line and headers, response status, headers and body)
     under `crates/sync-github/tests/fixtures/`.
   - **Synthetic data only**: an `example-org/demo-repo`, fake logins, no real tokens.
   - A small recorder helper behind an env var is fine; it must never run in CI, and it scrubs
     `Authorization`.

## Acceptance

- Fixture tests cover:
  - the first full sync;
  - an incremental sync with a `304`;
  - pagination;
  - a closed issue (completed versus not planned);
  - a reopened issue;
  - a milestone rename;
  - a merged pull request that closes `#n`;
  - rate-limit exhaustion (`RateLimited` with the reset time);
  - a secondary limit with `retry-after`;
  - a malformed item skipped;
  - an oversized body capped.
- **`plan` rules:**
  - an upstream close never moves an in-progress task;
  - a close moves `review` to `done` only when `can_move(Review, Done, Sync)` says so;
  - a title edit on the hub side is overwritten only for upstream-owned fields;
  - a conflict gives a conflict ask intent.
- **Idempotence:** re-running a sync with no upstream change yields no changes.
- The token never appears in any `Debug`, error or log output (tested).
- No network in tests; no unwrap or expect in library code; no unsafe.

## Out of scope

The real HTTPS transport and its dependency, writes to GitHub and the approval queue, Jira,
applying intents to the hub, routes, and credential storage.
