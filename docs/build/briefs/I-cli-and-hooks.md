# Brief I · CLI transport, verbs and the hook

- **Stream:** I · Agent CLI and hooks. **Branch:** `s/I/cli-and-hooks`. **Paths:**
  `crates/cli/**` only.
- **First read:** [README.md](README.md), then `docs/build/streams/I.md`, ADR-0006, ADR-0010,
  `docs/build/contracts/api-v1.md` (routes marked **agent**, and "Hooks"), and
  `crates/api/src/client.rs` on `main` (server-identity checks).

## Goal

`pitcrew`, the command agents run to see and report on their work, and `pitcrew hook`, which
runs on **every** agent turn and must cost almost nothing.

## What to build

1. **Transport.** HTTP/1.1 to the daemon over:
   - the unix socket (`PITCREW_SOCKET`);
   - the named pipe on Windows (`PITCREW_PIPE`);
   - loopback TCP for development only (`PITCREW_URL`, refused unless the host is
     `127.0.0.1`, `::1` or `localhost`).

   The token comes from `PITCREW_TOKEN`, or from `PITCREW_TOKEN_FILE`, which must be private
   (0600 on Unix).
   - **Keep start-up tiny:** prefer a small blocking client with no async runtime. Measure it.
   - **Check the server before sending the token.** On Unix: the socket directory is private and
     the peer is the same user. On Windows: the pipe server is the same user, opened at
     identification-level impersonation (`SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION`).
     Reuse the checks in `pitcrew_api::client`. If depending on `pitcrew-api` is too heavy for
     the CLI, say so in your report and propose moving the checks to `pitcrew-auth`.
2. **Verbs** (all with `--json`; human output is short and plain):
   - `whoami`;
   - `task list [--mine] [--status …]`, `task show <id|key>`, `task move <key> <status>`;
   - `task plan <key>`, which reads a plan from stdin and replaces your own `agent_plan`
     subtasks;
   - `claim <key>` (move to in progress), `report <key> --review|--note <text>` (comment, and
     move to review when asked);
   - `comment <key> <text> [--mention @x]`, `ask <@member> <title> [--option …]`,
     `reply <ask> <text|--option n>`;
   - `check`: your open asks and recent mentions.

   Map API errors to clear messages and exit codes (e.g. 2 invalid, 3 forbidden, 4 conflict,
   5 unavailable).
3. **`pitcrew hook <engine> <event>`:**
   - reads the CLI's hook JSON from stdin (cap 1 MiB) and `POST`s it to
     `/v1/hooks/{engine}/{event}`;
   - uses short connect and write timeouts;
   - **always exits 0 and prints nothing** unless the engine's hook contract needs output;
   - never blocks the agent when the daemon is down.

## Acceptance

- **Verb tests:** against an in-process fake server (a small `TcpListener` thread with canned
  responses is enough), cover each verb and each error mapping. Also run the verbs once by hand
  against `npm run mock-hub` with `PITCREW_URL=http://127.0.0.1:47317` and
  `PITCREW_TOKEN=dev-agent-token`, and paste the output in your report.
- **Hook timing** (release build): up to 10 ms with a server up, up to 5 ms with none listening.
  Report p50 and p99 over 200 runs.
- A `PITCREW_URL` pointing at a non-loopback host is refused; a token file readable by others is
  refused on Unix.

## Out of scope

Installing and uninstalling hooks in agent configs (next brief), the OpenCode plugin.
