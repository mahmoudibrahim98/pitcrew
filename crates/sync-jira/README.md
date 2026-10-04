# pitcrew-sync-jira

Read Jira issues and epics, for Jira Cloud and Jira Data Center, safely and incrementally; field
ownership; the write-approval queue and outward writes are later briefs. This crate is read-only
and never writes to Jira. `pitcrewd` runs it on a timer and applies its intents to the hub
(`crates/daemon/src/integrations/`); `probe::probe` is the read-only "test this connection".

See the [crate's own docs](src/lib.rs) ("Shape" and "Reuse, not a fork") for the architecture and
for exactly what is reused from [`pitcrew-sync-github`](../sync-github/README.md) versus added
locally. In short: the transport seam, recorded-fixture format, bounds/backoff helpers and the
`Intent`/field-ownership shape are a dependency on sync-github, not a fork of it; the JQL builder,
the Atlassian Document Format converter, the Cloud/Data Center `Deployment` trait and the Jira
wire types are specific to this crate.

## Known gaps

- Closed: epics → workstreams. `ownership::plan_workstream` turns an epic's change into intents for
  each workstream that links the epic (`EPIC_FIELD_OWNERSHIP`: the hub owns the name; an epic moved
  to done proposes `shipped`, unless a task of the workstream is in progress, which is a conflict
  ask). `pitcrewd` applies them (`crates/daemon/src/integrations/`, G-sync-wiring). There is no
  reopened-epic change, so a reopened epic does not move a shipped workstream back.
- When one project sits in a dense enough burst of updates that a single sync call's page budget
  (`Limits::max_pages`) runs out while every item fetched so far is still within
  `CURSOR_SAFETY_MARGIN_HOURS` of the old cursor (`SearchResult::stuck_window_exhausted`, see
  `JiraClient::search`'s doc), this is surfaced as a visible `SyncIssue`. The *next* call resumes
  from exactly the instant the stuck call last processed, with no safety margin re-subtracted
  (`ProjectState::resume_without_margin`), so it is guaranteed not to re-query the same
  already-exhausted window — round 3 review item S-3 found and fixed a real permanent-stall bug
  here, where the margin being re-subtracted every call could make a dense-enough window never
  resolve at all. What is *not* implemented is a true mid-window continuation (persisting
  `startAt`/`nextPageToken` itself, to resume a cut-short page walk precisely rather than
  re-querying from an instant and re-reading everything from there again); the no-margin resume is
  the simpler fix review offered as sufficient on its own. The residual case this does not solve:
  if more items than one call's item budget share the *exact same* `updated` instant, no cursor
  strategy can separate them, and that project would need a larger `Limits::max_items` or a
  `JiraClient::search` call site willing to override it.
- `JiraDataCenter`'s `startAt` pagination has no page-overlap verification (re-reading each page's
  boundary item and restarting the walk on a mismatch). A concurrent write landing mid-walk can in
  rare cases still skip or repeat an item within that one call — see `JiraDataCenter`'s own doc in
  `src/deployment.rs` for the detail (including a correction: an earlier version of that doc
  claimed this self-heals on the next sync call, which does not actually hold). `JiraCloud`'s
  token-based pagination does not share this limitation.

**Owned by stream G** — see [docs/build/streams/G.md](../../docs/build/streams/G.md).
