# pitcrew-sync-github

GitHub: issues, milestones and pull requests into typed upstream changes; field ownership, both
ways, and `plan` (intents for the hub); a read-only `probe` for "test this connection"; and the
request an approved outward write is sent as (`write`). The crate has no HTTP client: `pitcrewd`
brings the HTTPS transport, runs the sync on a timer, applies the intents, and sends a write only
after a person approved it (`crates/daemon/src/integrations/`). The sync only sends `GET`.

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

- `write::send(transport, config, write)`: one approved write, each request sent once and in
  order, stopping at a refusal: `POST …/issues` (create), `POST …/issues/{n}/comments`, or an edit:
  `PATCH …/issues/{n}` with only the fields it sets (title, body, milestone, `state` and
  `state_reason`), then labels as a change, never the whole list (`POST …/issues/{n}/labels`
  with those to add, one `DELETE …/labels/{name}` per label to remove; a `404` there is already
  gone). A refusal's message is capped and stripped of hidden characters; a created issue needs a
  positive number, and its link is kept only on GitHub's web host. `tests/fixtures/writes.fixture`
  pins exactly what each write sends.
- `write::read_issue` (`GET …/issues/{n}`) reads an issue as GitHub has it now, raw, for the hub
  to compare with what an edit expects just before it is sent; `write::find_earlier` looks for an
  earlier attempt at a create (the same title and body) or a comment (the same text) created since
  a time, before either is sent again. Both are one `GET`, and neither is the sync's.
- `IssueSnapshot::title_lossless` and `body_lossless` say whether the snapshot's title and body are
  exactly what GitHub sent (nothing hidden stripped, nothing cut): only those are ever written
  back.
- The ownership tables say both directions: `FieldOwnership::outward` (`Outward::AskToSend` for the
  fields upstream owns, `AskToCloseOrReopen` for the state, `Never` for the assignee and the
  milestone's own fields), and `outward(table, field)` reads it.

## Known gaps

- There is no reopened-milestone change, so a reopened milestone does not move a shipped workstream
  back.

**Owned by stream G** — see [docs/build/streams/G.md](../../docs/build/streams/G.md).
