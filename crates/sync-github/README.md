# pitcrew-sync-github

GitHub, read side: issues, milestones and pull requests into typed upstream changes; field
ownership and `plan` (intents for the hub); a read-only `probe` for "test this connection". The
crate has no HTTP client: `pitcrewd` brings the HTTPS transport, runs the sync on a timer and applies
the intents (`crates/daemon/src/integrations/`). Outward writes and their approval queue are the
next brief (G-approval-writes).

- `ownership::plan(change, task)`: issue and pull request changes into task intents
  (`ISSUE_FIELD_OWNERSHIP`). A move to where the task already is gives nothing, so a sync read
  again after a restart asks nothing twice.
- `ownership::plan_workstream(change, linked)`: a milestone's change into intents for a workstream
  that links it (`MILESTONE_FIELD_OWNERSHIP`): the hub owns the name, and a closed milestone
  proposes `shipped` unless a task of the workstream is in progress (a conflict ask). This closes
  the earlier "milestones → workstreams" gap.
- `SyncState::opened_from_snapshot(source, milestone)`: an open issue as a first read would report
  it (`IssueOpened`), from the snapshot the last read kept. An `IssueMilestoned` change carries none
  of the issue's fields, so this is how `pitcrewd` makes a task of an issue moved into a milestone
  it follows.
- `probe::probe(transport, config)`: `GET /repos/{owner}/{repo}` for each repository, reporting
  whether it is readable and whether the credential could write (push or admin rights, a classic
  token's broad scopes).

## Known gaps

- There is no reopened-milestone change, so a reopened milestone does not move a shipped workstream
  back.

**Owned by stream G** — see [docs/build/streams/G.md](../../docs/build/streams/G.md).
