# API v1 conformance

The task tests include archive/restore, typed archival patches, authorization and their
`task_updated` events, and dispatch reads by task key/id for both token scopes. The mock's
ending-session simulation records dispatch outcomes, including a failed start's reason.

From the repository root, using Node 22.18+ and (for the daemon) Rust 1.88+:

```sh
node tests/conformance/run.mjs mock
node tests/conformance/run.mjs daemon
```

Both commands run `onboarding.test.mjs` on its own, then the same `api.test.mjs`, `scan.test.mjs`,
`files.test.mjs` and `machine-setup.test.mjs`, then `import.test.mjs` on its own, then `integrations.test.mjs` on its own
(its syncs append events, which the main suite's exact-revision checks must not see), then
`writes.test.mjs` on its own (it connects the same repository). Every phase runs, and the first
failure decides the exit code. No npm dependency is needed. The
daemon runner builds `pitcrewd` and `pitcrew-ptyd` with the locked workspace dependencies, starts a
seeded demo on an OS-assigned free loopback port, and reads its two private token files without
printing them. Each runner creates an empty temporary home and cleans up its child process and
directory even after test failures. The daemon runner deliberately refuses tmux via a regular file
in place of its socket, and runs its terminals in the `pitcrew-ptyd` it built
(`--terminal-runtime pty`, on an endpoint in its temporary folder), so that a dispatch can start
its agent's CLI: `claude`, `codex` and `opencode` are stand-ins first on the daemon's `PATH` (Unix
shell scripts that write nothing and wait until the run's folder is removed; then ptyd exits once
idle). It never runs a real agent or connects to a person's terminal. The demo watches no agent
homes, so its machine scan reports nothing; the runner passes `--scan-hold-ms 1500` so that a scan
lasts long enough for the second one `scan.test.mjs` sends meanwhile to be refused. On Windows the
daemon target is skipped (it says so and exits 0): its stand-ins are Unix shell scripts, and ptyd
there would look for `.exe` and `.cmd` names on `PATH`, where it could find a real agent CLI. CI
runs it on Linux.

To run the suite against an existing **synthetic local demo server**, set these variables and use
`node --test tests/conformance/api.test.mjs tests/conformance/scan.test.mjs`:

- `PITCREW_CONFORMANCE_URL`: its loopback HTTP base URL;
- `PITCREW_CONFORMANCE_PERSON`: device token;
- `PITCREW_CONFORMANCE_AGENT`: agent token.

The suite creates and edits projects, workstreams, tasks, asks and briefs. Use disposable state.
Tokens are only headers / WebSocket subprotocols. Query-token refusal tests use an invalid,
synthetic value. The suite itself neither imports nor inspects a server implementation.

`schema.mjs` validates required/optional response fields, nested models, enum tags, ids, dates,
integer cursors, transcript variants and recap UTF-8 spans. It permits extra fields for compatible
protocol extensions. Tests cover every API-v1 route via a successful request or a refusal/validation
path, filters, paging to completion, bad cursors, zero limits, authentication and ownership (an
agent token refused on person-only lists and on every person-only write), forged authors,
WebSocket hello, a mutation frame, and exact replay. Terminal coverage is limited to
upgrade/auth/id/size refusals; it uses no real terminal. Session launch and command success
require a runtime and are not exercised by the shared runner; body and unknown-session refusals
cover those routes. First-run successful setup is already covered by each server's own tests;
this seeded suite checks setup conflict and agent refusal.

`scan.test.mjs` covers `POST /v1/machines/{id}/scan` ("Machine scan") in a file of its own: no
token, an unknown or query token and an agent token refused; an unknown machine `404`; another
machine of the workspace `409`; the answer's frames (`application/x-ndjson`, `progress` from
`scanned: 0`, ticks that only grow to `scanned == total`, one `done` last) and its report's shape
and sums; and a second scan while one is under way `409`, the first still ending with its report.
That last case needs the server's scan to last a moment: the mock's takes about a second, and a
daemon needs `--scan-hold-ms`. It never reads a person's agent homes: the mock's report is
synthetic, and the demo daemon watches none.

`machine-setup.test.mjs` covers "Machine setup" in a file of its own: the check's shape, fixed order
and fixes (an `ok` row has none; a missing tool offers its install page; no helper row on the hub's
own machine), one row with `?row=`, the accounts' shape and rules, a sign-in's answer, the same one
while it runs, its terminal served (`101`) to a person and refused to an agent and to another
person, and not a session, `DELETE` stopping it (`204`, then `404` and its terminal gone), and the
refusals (no or unknown token, agent, another person's device token on every route, unknown and
other machines, unknown CLI, a method the CLI lacks, unknown body fields). On the daemon target the stand-in CLIs answer `--version` and
their status commands at once (not signed in), and a sign-in runs the stand-in's "login", which
waits like a session; the check also runs the runner's own `git`, `gh` and `tmux` for their
versions. The mock's are synthetic.

See [MISMATCHES.md](MISMATCHES.md) for observed differences and ambiguities. Only the daemon
runner loads `daemon-deviations.json`. A listed failure must raise exactly its recorded status
mismatch; timeouts, schema failures and different errors still fail. An unexpectedly passing case
also fails, requiring the stale exception to be removed. There are no skipped or todo tests.

The shared suite also checks read cursors for forward-only movement, project/workstream
scope isolation, future/malformed revisions, agent refusals and person isolation. The
runner supplies `PITCREW_CONFORMANCE_SECOND_PERSON`: the mock's second synthetic device
credential, or a random device credential provisioned in the daemon's temporary registry
before startup. It never modifies a registry owned by a running daemon or a real person.

files.test.mjs runs on both targets: location-root resolution, sorted lists, reads, text and
binary writes, stale revisions and exclusive creation, bad paths, Git writes, device-only
auth, file/body caps, remote refusal and outward-link refusal. The runner creates every file
and link target inside its own temporary folder. Windows daemon conformance keeps its existing
Unix-runtime skip; Rust daemon HTTP tests cover Files API routes there.

`run.mjs` runs `import.test.mjs` serially after the shared suite, since committing inclusion affects all views of its disposable hub. Both targets check dry-run/commit agreement, each mode, filters, excluded session/activity/recap reads, restoration, validation and device-only access.

Hook-install conformance writes agent configurations. It is disabled unless
`PITCREW_CONFORMANCE_SYNTHETIC_HOOKS=1`, which `run.mjs` sets only for its managed
synthetic targets. Do not set it when pointing the suite at an existing hub.

`integrations.test.mjs` covers "Integrations" and "Linking a workstream upstream": every route
refused without a token (`401`) and to an agent (`403`), unknown ids `404`, malformed connections
and links `400`, a second connection to the same repository and a secret for a `gh_cli` connection
`409`; a stored secret that is never in any answer; a project and workstream of its own linked to
milestone 1 of `example-org/demo-repo` (`workstream_linked` in the log); a sync that turns the open
issue #1 into a `todo` task of that workstream (and not #3, closed before it was first seen); the
connection's status, link title and test (with its warning about write rights); that the link's
and the sync's events reach activity and a `/v1/stream` replay only as the shared visibility rule
allows (an agent gets `403` from both; with every session excluded by `/v1/import`, they stay
visible while an excluded session's events are hidden from the same answers, and the choice is
restored to `all` afterwards); then upstream changes, which both targets must apply alike: an open
issue (#4) moved into the linked milestone becomes a task there while a closed one (#2) does not,
an issue and the milestone closing upstream move the task to `done` and ship the workstream, and a
task a person reopens stays reopened on the next sync; and removal. Both targets read a copy of
the recorded fixtures in `apps/mock-hub/fixtures` that `run.mjs` makes
(`PITCREW_CONFORMANCE_FIXTURES`), again at each sync: the test changes upstream by adding a
fixture file that sorts first. The daemon runner passes `--integration-fixtures` and puts a
stand-in `gh` (printing a synthetic credential) first on the daemon's `PATH`; the mock gets
`startServer({ integrationFixtures })`. Nothing reaches GitHub or Jira.

`writes.test.mjs` covers "Outward writes: every one approved first": the routes refused to an
agent (`403`), unknown ids `404`, malformed requests `400`; a hub task in a workstream linked to
milestone 2 that asks to create an issue (an `approval` ask with exactly what will be sent),
denied (recorded as not sent, never started), asked again and approved (sent once; the new issue
`#8` becomes the task's source; a second create is `409`); a move to `done` that the hub turns into
a proposal to close the issue, approved and sent (after reading the issue as upstream has it
now); a comment upstream refuses (`422` in the fixtures), failed, and a retry (a logged
`write_retry_requested`) that looks upstream for the earlier attempt, finds none and sends it once
more; then, with the fixture copy saying the comment is there after all, a second retry that finds
it and records it as sent without commenting again; a retry by a person the ask is not addressed
to (`403`) and of a sent write (`409`); the task's writes and their events in order; and an
approval ask an agent raised itself, which proposes nothing. The daemon sends writes in the
background, so the test polls for each outcome.
