# API v1 conformance

From the repository root, using Node 22.18+ and (for the daemon) Rust 1.88+:

```sh
node tests/conformance/run.mjs mock
node tests/conformance/run.mjs daemon
```

Both commands run the same `api.test.mjs` and `scan.test.mjs`. No npm dependency is needed. The
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
