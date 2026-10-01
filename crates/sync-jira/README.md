# pitcrew-sync-jira

Read Jira issues and epics, for Jira Cloud and Jira Data Center, safely and incrementally; field
ownership; the write-approval queue and outward writes are later briefs. This crate is read-only
and never writes to Jira.

See the [crate's own docs](src/lib.rs) ("Shape" and "Reuse, not a fork") for the architecture and
for exactly what is reused from [`pitcrew-sync-github`](../sync-github/README.md) versus added
locally. In short: the transport seam, recorded-fixture format, bounds/backoff helpers and the
`Intent`/field-ownership shape are a dependency on sync-github, not a fork of it; the JQL builder,
the Atlassian Document Format converter, the Cloud/Data Center `Deployment` trait and the Jira
wire types are specific to this crate.

## Known gaps

- `ownership::plan` produces no hub `Intent` for an epic change yet (`EpicCreated`/`EpicRenamed`/
  `EpicClosed`): epics map to workstreams, not tasks, and `plan`'s signature here only takes a
  task. `pitcrew-sync-github`'s `plan` has the identical gap for milestones, for the same reason.
  Applying either to a workstream is a later brief.
- When one project sits in a dense enough burst of updates that a single sync call's page budget
  (`Limits::max_pages`) runs out while every item fetched so far is still within
  `CURSOR_SAFETY_MARGIN_HOURS` of the old cursor (`SearchResult::stuck_window_exhausted`, see
  `JiraClient::search`'s doc), this is surfaced as a visible `SyncIssue`, but no continuation is
  persisted to resume precisely where that call left off — the next call restarts the same window
  from the top. The project does make progress (the page budget is large relative to realistic
  update bursts), just not as efficiently as a persisted mid-window continuation would allow.
  Implementing that continuation was judged not worth the added state/complexity for a case this
  narrow; the `SyncIssue` at least makes it visible rather than a silent stall.
- `JiraDataCenter`'s `startAt` pagination has no page-overlap verification (re-reading each page's
  boundary item and restarting the walk on a mismatch). A concurrent write landing mid-walk can in
  rare cases still skip or repeat an item within that one call — see `JiraDataCenter`'s own doc in
  `src/deployment.rs` for the detail (including a correction: an earlier version of that doc
  claimed this self-heals on the next sync call, which does not actually hold). `JiraCloud`'s
  token-based pagination does not share this limitation.

**Owned by stream G** — see [docs/build/streams/G.md](../../docs/build/streams/G.md).
