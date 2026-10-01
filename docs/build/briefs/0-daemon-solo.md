# Brief 0 · `pitcrewd` for the solo case: store, work model and API in one process

- **Stream:** 0 · Composition root (run as integration work). **Branch:**
  `integrator/daemon-solo`. **Paths:** `crates/daemon/**` (+ `Cargo.lock`). Other crates are
  read-only; if one needs a change, stop and describe it.
- **First read:** [README.md](README.md), `docs/build/streams/0.md`, ADR-0004, ADR-0006,
  ADR-0009, `docs/build/contracts.md`, and on `main` the READMEs of `crates/api` (especially
  "For the composition root"), `crates/auth`, `crates/store` and `crates/hub-work`.

## Goal

Milestone: the desktop UI runs against the **real** daemon instead of the mock hub. `pitcrewd`
opens the store, runs the work model, and serves API v1 with real tokens. The runner, terminals
and remote machines join later; their briefs are in progress.

## What to build

1. **Command line** (clap, already a workspace dependency):
   - `pitcrewd serve`:
     - `--state-dir`: defaults to the platform's local data folder via `directories`, never a
       synced or roaming one.
     - `--listen private`: the private socket or pipe, the default. `--listen tcp:127.0.0.1:<port>`
       is for development. The API crate's DNS-rebinding and loopback checks stay on.
   - `--demo` seeds the demo workspace into an **empty** store; it refuses a non-empty one.
   - `pitcrewd --version` prints the version and protocol range.
2. **Wiring:**
   - open the store, using `FsMode::Auto` once stream C's NFS brief lands; for now the default;
   - register the hub-work projections;
   - build `RouterParts` with the work routes (agent and device), the stream's `EventSource`
     backed by the store's `subscribe` and `since`, the activity route, and the hooks route. Until
     the runner link exists, the hook sink records hooks at debug level, with their bodies
     redacted.
   - The terminals route answers per the contract when nothing is attached (404 or 503, as the
     API crate documents).
3. **Tokens** (`pitcrew-auth`):
   - On first start, mint the desktop's device token into the token store under the state dir
     (private file).
   - `pitcrewd token show-path` prints **where** it is, never the token itself.
   - In development, the UI reads the token from that file through an env var or a Vite dev
     config. Document exactly how. Tokens never go in URLs or logs.
4. **Lifecycle:**
   - Ctrl+C or SIGTERM shuts down gracefully: WebSockets close with 1001 and the store closes
     cleanly.
   - Only one daemon per state dir: use an exclusive lock, and give a clear error if another
     one holds it.
   - Logs go to stderr with `tracing` and a level from `PITCREW_LOG`.
5. **Parity check against the mock hub:**
   - Run the mock hub's HTTP tests (`apps/mock-hub/test/http.test.ts`) against
     `pitcrewd serve --demo --listen tcp:…`, if they can target another base URL; otherwise a
     small Node script that replays their requests against both servers.
   - Report every difference as one of: a missing route (expected for runner routes), a contract
     mismatch (a bug for E, H or 0), or a deliberate difference documented by E.
6. **The UI against the daemon:**
   - Run the UI dev server pointed at the daemon and the shell's e2e suite (Playwright,
     `PLAYWRIGHT_CHANNEL=msedge`, your own ports 47460–47469).
   - Report what passes, and what fails with its cause.
   - Don't edit the UI; describe what it needs, e.g. a config option for the token.

## Acceptance

- `pitcrewd serve --demo` starts in under 1 s on this machine, and `GET /v1/host/info` answers.
- `GET /v1/tasks` with the device token returns the demo tasks.
- A move through the API appears on `GET /v1/stream`.
- The parity report and the UI e2e report are in your final message.
- Integration tests in `crates/daemon/tests` start the binary on a temp state dir and a free
  port, then exercise the token, the work routes, the stream and shutdown.
- fmt, clippy `-D warnings` (Linux and Windows target), `cargo test --workspace`, and
  `cargo deny check` all pass.

## Out of scope

- The runner link, terminals and agent tokens for real sessions: stream D's hub-link wires
  these in later.
- Remote machines, the Tauri shell, auto-start, and installers.
