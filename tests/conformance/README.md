# API v1 conformance

From the repository root, using Node 22.18+ and (for the daemon) Rust 1.88+:

```sh
node tests/conformance/run.mjs mock
node tests/conformance/run.mjs daemon
```

Both commands run the same `api.test.mjs`. No npm dependency is needed. The daemon runner builds
`pitcrewd` and `pitcrew-ptyd` with the locked workspace dependencies, starts a seeded demo on an
OS-assigned free loopback port, and reads its two private token files without printing them. Each
runner creates an empty temporary home and cleans up its child process and directory even after
test failures. The daemon runner deliberately refuses tmux via a regular file in place of its
socket, and runs its terminals in the `pitcrew-ptyd` it built (`--terminal-runtime pty`, on an
endpoint in its temporary folder), so that a dispatch can start its agent's CLI: `claude`,
`codex` and `opencode` are stand-ins first on the daemon's `PATH` (Unix shell scripts that write
nothing and wait until the run's folder is removed; then ptyd exits once idle). It never runs a
real agent or connects to a person's terminal. The demo watches no agent homes.

To run the suite against an existing **synthetic local demo server**, set these variables and use
`node --test tests/conformance/api.test.mjs`:

- `PITCREW_CONFORMANCE_URL`: its loopback HTTP base URL;
- `PITCREW_CONFORMANCE_PERSON`: device token;
- `PITCREW_CONFORMANCE_AGENT`: agent token.

The suite creates and edits projects, workstreams, tasks, asks and briefs. Use disposable state.
Tokens are only headers / WebSocket subprotocols. Query-token refusal tests use an invalid,
synthetic value. The suite itself neither imports nor inspects a server implementation.

`schema.mjs` validates required/optional response fields, nested models, enum tags, ids, dates,
integer cursors, transcript variants and recap UTF-8 spans. It permits extra fields for compatible
protocol extensions. Tests cover every API-v1 route via a successful request or a refusal/validation
path, filters, paging to completion, bad cursors, zero limits, authentication and ownership,
forged authors, WebSocket hello, a mutation frame, and exact replay. Terminal coverage is limited
to upgrade/auth/id/size refusals; it uses no real terminal. Session launch and command success
require a runtime and are not exercised by the shared runner; body and unknown-session refusals
cover those routes. First-run successful setup is already covered by each server's own tests;
this seeded suite checks setup conflict and agent refusal.

See [MISMATCHES.md](MISMATCHES.md) for observed differences and ambiguities. Only the daemon
runner loads `daemon-deviations.json`. A listed failure must raise exactly its recorded status
mismatch; timeouts, schema failures and different errors still fail. An unexpectedly passing case
also fails, requiring the stale exception to be removed. There are no skipped or todo tests.
