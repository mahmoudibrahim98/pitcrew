# Brief G · Jira, read side; GitHub follow-ups

- **Stream:** G · Integrations. **Branch:** `s/G/jira-read`. **Paths:** `crates/sync-jira/**`,
  `crates/sync-github/**` (+ `Cargo.lock`).
- **First read:** [README.md](README.md), [G-github-read.md](G-github-read.md) (merged; this brief
  mirrors it), the `crates/sync-github` sources and README, `docs/build/streams/G.md`, ADR-0006,
  ADR-0007, and `ExternalRef`, `ExternalSystem`, `Task`, `Workstream` and `Mover` in
  `crates/protocol`.

## Goal

Read Jira issues and epics safely and incrementally, and turn what changed upstream into the same
kind of typed upstream changes and `plan` intents as GitHub. Like the GitHub brief, this one is
read-only and never writes to Jira.

## What to build

1. **Reuse, don't fork:**
   - Take the transport seam, the fixture format and the intent shape from `sync-github`, and
     decide how to share them:
     - a dependency of `sync-jira` on `pitcrew-sync-github`'s generic parts;
     - or moving them into a small module both crates use;
     - or, if neither is clean, a proposal for a shared crate (that needs the integrator).
   - Say which you chose, and why.
   - No HTTP client dependency, as before.
2. **Two deployments** behind one trait:
   - **Jira Cloud:** REST v3; Basic auth with e-mail plus API token. Search through
     `POST /rest/api/3/search/jql` with `nextPageToken` pagination.
   - **Jira Data Center:** REST v2; a Bearer personal access token. `/rest/api/2/search` with
     `startAt` pagination.
   - The credential never appears in `Debug`, errors or logs (tested), as for GitHub.
3. **Incremental reads:**
   - JQL `project in (…) AND updated >= "<cursor>" ORDER BY updated ASC`, built only from
     validated project keys. **Never put untrusted text into JQL.**
   - JQL times are in the account's time zone, to the minute. Read the zone once
     (`/rest/api/3/myself`), keep a small overlap window, and remove duplicates by issue id plus
     `updated`.
   - Fetch only the fields you use (`fields=`). Unknown fields are ignored.
4. **Mapping and bounds:**
   - Issue → task, with the status category (`new`, `indeterminate`, `done`), the resolution,
     the summary, the description, labels, the assignee and the parent.
   - Epic (or a parent of the epic level) → workstream.
   - **Descriptions:** v3 descriptions are ADF (Atlassian Document Format). Convert them to plain
     text with bounded depth and size; v2 descriptions are text already. Never interpret links or
     markup as instructions; this is text for people to read.
   - The same caps as GitHub: body and field lengths, items per sync, and malformed items skipped
     and counted.
5. **Upstream changes and `plan`:** the same shape and rules as GitHub.
   - Created, retitled, its description edited, status category changed (with the resolution),
     relabelled, reassigned, re-parented; an epic created, renamed or closed.
   - The field-ownership table for Jira.
   - An upstream move to `done` proposes `done` only through `can_move(.., Mover::Sync)`;
     in-progress work is never touched.
6. **Rate limits:** on a 429, honour `Retry-After`, with a capped backoff. Return `RateLimited
   { until }` rather than sleeping; never busy-loop.
7. **GitHub follow-ups** from the last review:
   - `origin.rs` `path_is_under` rejects or normalises `..` and `.` segments. Test it with a
     GitHub Enterprise base under a sub-path.
   - `body_mentions_rate_limit` looks only at the JSON `message` field, never the raw body.

## Acceptance

- **Fixture tests**, synthetic data only (`DEMO` project, fake accounts, `jira.example.com`):
  - a first full sync and an incremental one, on Cloud and on Data Center;
  - both kinds of pagination;
  - the time-zone overlap without duplicates;
  - a status category change to done, and a reopen;
  - an epic rename;
  - a re-parent;
  - a 429 with `Retry-After`;
  - a malformed issue skipped;
  - deep or huge ADF capped.
- **`plan` rules,** as for GitHub.
- **Idempotence:** a sync with no upstream change yields no changes.
- **JQL:** a project key with quotes, spaces or JQL operators is refused (tested).
- The credential never appears in `Debug`, error or log output (tested).
- No network in tests; no unwrap or expect in library code; no unsafe.

## Out of scope

The real HTTPS transport, writes and the approval queue, applying intents to the hub, routes,
and credential storage.
