# Threat model

A living document, owned by stream Q. Every stream that lands code named in a "Where" cell, and
every review that finds a security issue, updates it.

- **Last reviewed:** 2026-09-30, against `main` at `f365897`, plus the open branches named below.
- **Status:** **In place** (on `main`, with a test) · **Partial** (some of it on `main`) ·
  **Planned** (in an open branch, not on `main`) · **Open** (a gap: the owner must act; listed
  again in [Open items](#7-open-items)).
- **Owners** are streams, as in `docs/build/ownership.json`. Code references are paths on `main`
  unless marked with a branch.

## 1. Scope

In scope: the desktop shell and its webview UI, `pitcrewd` in both roles (hub and runner), the
CLI and hooks, the runner protocol, the remote helper and the SSH layer, and the build and
release pipeline.

Out of scope, by assumption:

- malware already running as the user, root or an administrator, and physical access;
- the agent CLIs themselves (Claude Code, Codex, OpenCode) and the model providers;
- the operating system's own isolation between users.

## 2. Assets

| Id | Asset | Why it matters |
|---|---|---|
| A1 | Bearer tokens: **device** (a person) and **agent** | A device token takes every action a person can. |
| A2 | Transcripts and agent output | They hold prompts, code, command output and sometimes secrets. |
| A3 | Agent control | Starting sessions, typing into them and approving work is code execution as the user. |
| A4 | Source code and working trees on every machine | Agents read and write them. |
| A5 | Credentials on remote machines | SSH keys, passwords and one-time codes, the CLIs' account homes, cluster accounts. |
| A6 | The event log and workspace state | Authorship (who did what) and decisions must be true; the log must stay available. |
| A7 | The helper binary, installers and updates | Whatever they contain runs on every machine. |

## 3. Actors

| Id | Actor | Can | Assumed not to |
|---|---|---|---|
| U1 | **Another user on a shared machine** (an HPC login node hosts hundreds) | Connect to any socket they can reach, create files in shared folders, read other users' process arguments | Read files in the user's private folders |
| U2 | **Malicious content**: transcript text, issue bodies, file contents, web pages | Reach agents as prompt injection, and reach our parsers and the UI as arbitrary bytes | Run code directly |
| U3 | **The agent itself**, confused or injected | Everything the user's OS account can do if it has a shell; call the API with its agent token | Hold the device token, unless it can read it (see T10) |
| U4 | **A stolen or leaked token** | Everything its scope allows, from anywhere that reaches the transport | Survive rotation |
| U5 | **A compromised dependency** (crate, npm package, GitHub Action) | Run code at build time or in the shipped app | |
| U6 | **A malicious or compromised remote host**, or someone in the middle of a first connection | Answer SSH and every probe with anything; run anything as the user there | Reach the desktop other than through SSH output and the forwarded socket |
| U7 | **A web page in the user's browser** | Send requests to `localhost`, rebind DNS | Read responses from another origin, set `Authorization` on a WebSocket |

## 4. Trust boundaries

```text
 ┌──────────────┐ B1 Tauri IPC   ┌──────────────────┐ B2 unix socket / named pipe /
 │ webview (UI) │───────────────▶│ desktop gateway  │    SSH-forwarded socket, bearer header
 └──────────────┘ capabilities   │ (adds the token) │──────────────────────────────┐
                                 └───────┬──────────┘                              ▼
                                         │ B8 system OpenSSH, askpass     ┌──────────────────┐
                                         ▼                                │ pitcrewd: hub    │
                                 ┌──────────────────┐   B3 runner        │ (API, auth, log) │
                                 │ remote helper    │◀── protocol ──────▶│                  │
                                 │ (pitcrewd runner)│   (JSON lines)     └────────▲─────────┘
                                 └───────┬──────────┘                             │ B6 hooks,
                          B4 spawn, env, │ B5 tmux -C / PTY                      │ agent token
                                         ▼                                        │
                                 ┌──────────────────┐  writes   ┌─────────────┐   │
                                 │ agent CLIs       │──────────▶│ transcripts │   │
                                 │ (in tmux or PTY) │───────────┼─────────────┼───┘
                                 └──────────────────┘           └──────┬──────┘
                                                              B7 ingest reads them
```

| Id | Boundary | What crosses | Who is trusted |
|---|---|---|---|
| B1 | webview ↔ desktop gateway | Tauri commands; rendered text from B2 | The gateway. The webview renders attacker-controlled text and never holds a token. |
| B2 | gateway, CLI, hooks ↔ daemon API | HTTP and WebSocket over a local transport, bearer tokens | The daemon, once the client has checked who serves the socket |
| B3 | hub ↔ runner | The runner protocol (`crates/protocol/src/runner.rs`) | Both sides belong to the user; the bytes are still parsed as untrusted |
| B4 | runner ↔ agent CLIs | Process spawn: argv, environment, working directory | The runner. CLIs run with the user's rights. |
| B5 | runner ↔ tmux and PTY | tmux control-mode commands and notifications; terminal bytes | tmux. Pane content is attacker-controlled. |
| B6 | agent CLIs and hooks → daemon | Hook payloads, agent-scoped API calls | Nothing: agents are U3 |
| B7 | transcripts on disk → ingest | JSONL files written by the CLIs | Nothing: content is U2 |
| B8 | desktop ↔ SSH ↔ remote helper | SSH, askpass prompts, the helper upload, a forwarded socket or stdio bridge | The user's own SSH config and known hosts |
| B9 | source → build → release → users | Dependencies, CI, installers, updates, the helper binary | Pinned, reviewed inputs only |

## 5. Threats and controls

### 5.1 Local transport and other users (B2)

| Id | Threat | Control | Where | Tested by | Owner | Status |
|---|---|---|---|---|---|---|
| T1 | Another user connects to the daemon and uses its API | Unix: a socket (0600) in a 0700 directory; every accepted connection's peer uid (`SO_PEERCRED`) must equal the daemon's, else it is dropped; a token is still required. Windows: a named pipe whose protected DACL grants only the current user's SID, with remote clients rejected. | `crates/api/src/listener/unix.rs` (`UnixSocket::bind`, `accept`), `crates/api/src/listener/pipe.rs`, `crates/api/src/listener/pipe_security.rs` (`PipeSecurity::current_user_only`) | `unix.rs` tests `connections_from_another_uid_are_dropped`; `crates/api/tests/unix_socket.rs`; `pipe.rs` test `only_the_current_user_has_access` (reads the DACL back); `crates/api/tests/named_pipe.rs` | H | In place |
| T2 | **Socket and pipe squatting**: another user creates the directory, socket or pipe first, and the daemon or a client uses theirs, handing over a token | Server: the directory is created 0700 and its owner checked before anything else; an existing one must already be ours and private (refused, never re-permissioned); a stale socket is removed only if it is ours and dead; `Drop` removes the socket only if its `(dev, ino)` is still ours. Windows: the pipe is named after the user's SID and created with `FILE_FLAG_FIRST_PIPE_INSTANCE`. Client: `check_unix_socket` (directory ours and 0700, socket ours) before connecting and `check_unix_peer` after; on Windows `check_pipe_server` (the serving process runs as the current user). | `crates/auth/src/private.rs` (`create_private_dir`, `check_private_dir`), `unix.rs` (`remove_stale`, `Drop`), `crates/api/src/listener/mod.rs` (`default_pipe_name`), `crates/api/src/client.rs` | `unix.rs` tests `a_live_socket_is_not_replaced_and_a_stale_one_is`, `something_else_in_the_way_is_left_alone`, `an_open_directory_is_refused_before_anything_is_touched`, `drop_leaves_a_socket_that_replaced_ours`; `client.rs` tests | H (checks), K and I (callers) | Partial: no client calls the checks yet (O1) |
| T3 | Another user plants or edits the token registry | The state directory is private (owner and mode checked); `tokens.json` is opened with `O_NOFOLLOW` and must be ours and not writable by group or others; writes go to a new, uniquely named temporary file, then a rename; an exclusive lock makes the daemon the single writer. | `crates/auth/src/store.rs` (`FileTokenStore::open`, `persist`), `crates/auth/src/private.rs` (`open_private_file`, `create_new_private_file`, `ExclusiveLock`) | `store.rs` tests `a_registry_others_can_write_is_refused`, `a_symlinked_registry_is_refused`, `an_open_state_directory_is_refused`, `only_one_store_may_open_a_registry`, `malformed_registries_are_errors` | H | In place on Unix. Windows relies on the directory's inherited ACL (O2). |
| T4 | Development TCP is reached by other local users, or by a web page through DNS rebinding | Only when asked, and only on loopback (checked in `Bound::bind`; `Bound` cannot be built another way); the `Host` header must name `localhost`, `127.0.0.1`, `[::1]` or `*.localhost`; a bearer token is still required. There are no cookies, so a cross-site request carries nothing. | `crates/api/src/listener/mod.rs` (`Bound::bind`, `local_host_only`, `is_local_host`) | `listener/mod.rs` tests `local_hosts`, `tcp_must_be_loopback`; `crates/api/tests/websocket.rs` `development_tcp_refuses_foreign_host_headers` | H; 0 (the daemon must never enable it by default) | In place |
| T5 | `GET /v1/host/info` answers without a token (host name, OS, versions) | Needed to detect version skew before authenticating. On the private transports only the same user reaches it. | `crates/api/src/lib.rs` (`router`), `crates/api/src/host.rs` | `crates/api/tests/auth.rs` `host_info_needs_no_token` | H | Accepted |

### 5.2 Tokens and scopes (B1, B2, B6)

| Id | Threat | Control | Where | Tested by | Owner | Status |
|---|---|---|---|---|---|---|
| T6 | A token leaks through a URL, a log or a handler | `Authorization: Bearer` only, never the query string; WebSockets offer `pitcrew.bearer.<token>` as a subprotocol; two credentials are ambiguous and refused; after verification both headers are removed, so no handler or log downstream sees them; rejections never quote the token; `SecretToken`'s `Debug` shows only the prefix; logs name tokens by `TokenId`. | `crates/api/src/auth.rs` (`authenticate`, `bearer_token`, `scrub`), `crates/auth/src/token.rs` (`SecretToken`) | `crates/api/tests/auth.rs` (`a_token_in_the_query_only_is_unauthorized`, `two_authorization_headers_are_unauthorized`, `unauthorized_responses_carry_a_bearer_challenge`); `auth.rs` unit tests; `token.rs` `debug_never_prints_the_token`; fuzz `api_request` | H | In place |
| T7 | A token is stolen from disk or from another user's view of the process list | Only SHA-256 hashes of 256-bit random tokens are stored, compared in constant time over every entry. Device tokens live in the OS keychain (ADR-0003). Agent tokens reach hooks through a private file or the environment, never argv, which other users can read on shared machines. | `crates/auth/src/token.rs` (`TokenHash`, `ct_eq`), `crates/auth/src/store.rs` (`verify`); keychain: `apps/desktop` (K, not built); hook token delivery: `crates/cli` (branch `s/I/cli-and-hooks`) | `store.rs` `the_registry_round_trips_and_never_holds_a_raw_token`, `unknown_well_formed_tokens_fail` | H, K, I | Partial (O3) |
| T8 | **Token in a build**: a developer's real token is inlined into the UI bundle | The UI reads `VITE_PITCREW_TOKEN` only in development; `vite build` fails when it is set, in every mode; only the needed variables reach the bundle. The webview never holds a token (ADR-0003): the gateway adds it. | `apps/ui/src/data/config.ts`, `apps/ui/vite.config.ts` (branch `s/L/skeleton-and-data`) | `config.test.ts` and a real build with the variable set (branch) | L, K | Planned; CI check O4 |
| T9 | A leaked token stays valid | `TokenStore::revoke` and `rotate`; every request verifies against the live registry. | `crates/auth/src/store.rs` | `store.rs` `revoke_and_rotate` | H (routes), K and L (Settings → Security) | Partial: no API route or UI yet (O5) |
| T10 | **An agent acts as the person** | Two scopes. Routes are device-only unless added with `RouterParts::agent`, so a forgotten mark fails closed; `device_only` and `Person` answer 403 to agents. The API layer inserts the `Caller`; domain crates stamp `author` and `on_behalf_of` from it, never from a body. `TaskStatus::can_move` never lets an agent move a task to done or canceled. Agent writes are limited to the agent's own tasks, in the domain handlers. | `crates/api/src/lib.rs` (`RouterParts`, `router`), `crates/auth/src/http.rs` (`Authenticated`, `Person`, `device_only`), `crates/protocol/src/api.rs` (`Caller`), `crates/protocol/src/model.rs` (`TaskStatus::can_move`); stamping and ownership: `crates/hub-work` (E, not built) | `crates/api/tests/auth.rs` `an_agent_token_on_a_device_only_route_is_forbidden`; `http.rs` tests; `crates/protocol/tests/contract.rs` `task_move_rules`; fuzz `api_request` | H, E, 0 | Partial (O6). **Residual:** an agent with a shell runs as the same OS user, so it can edit `tokens.json` or read a token it finds; the split holds only for agents confined to the API (O22). |
| T11 | Person-only actions leave no trace | An audit log of person-only actions: decisions, approvals of outward writes, ending sessions, settings, skip-permissions (ADR-0006). | Not built | | E, H | Open (O7) |

### 5.3 Agents, hooks and prompt injection (B4, B6)

| Id | Threat | Control | Where | Tested by | Owner | Status |
|---|---|---|---|---|---|---|
| T12 | **Prompt injection** through transcripts, issues, files or web pages makes an agent approve, decide or send something outward | Agent tokens cannot take person-only actions (T10). Decisions, approvals and outward writes need a device token and a click in the Inbox. Authors are stamped by the host. Agent text is shown as information and never authorises anything. | T10's layers; the Inbox and outward sync: `crates/hub-work`, `crates/office`, `crates/sync-*` (E, F, G, not built) | | E, F, G | Partial (O8) |
| T13 | An agent floods the hook intake, or sends huge or malformed bodies | An agent route with a token; the body is at most 1 MiB and must be a JSON object; the engine must be known and the event name match `[A-Za-z][A-Za-z0-9_-]{0,63}`; a bounded channel drops and counts rather than blocking. | `crates/api/src/hooks.rs` | `crates/api/tests/hooks.rs` (`bad_requests_are_invalid`, `hooks_need_a_token`, `a_full_channel_still_answers_202_and_counts_the_drop`); fuzz `api_request` | H | In place. The payload is untrusted in the runner's `HookSink` too (O9). |
| T14 | Hook installation changes agent configurations badly | Installed only after the person sees the diff; idempotent; never replaces what it did not write; uninstallable. | `crates/cli` (branch `s/I/cli-and-hooks`) | (branch) | I | Planned |
| T15 | Agents launched with too much power | The default permission mode is the CLI's own. `bypass_permissions` needs a per-workspace opt-in, with a warning and an audit entry; the runner answers `CommandOutcome::Rejected` when policy forbids. | `crates/protocol/src/runner.rs` (`RunnerCommand::StartSession.permission_mode`, `CommandOutcome::Rejected`); enforcement in `crates/hub-work` and `crates/runner` (not built) | | D, E | Open (O10) |
| T16 | Command injection into tmux or a shell through session names, paths or typed text | tmux commands are built from typed arguments and quoted for tmux's own parser (no shell); text goes through `send-keys -l --`; NUL is refused and control bytes become octal escapes. Branch B adds escaping of `#{…}`/`#(…)` formats and refuses control characters in names. Remote commands are quoted with POSIX single quotes, and host names that look like options are refused (branch J). | `crates/runtime/src/command.rs` (`Command`, `quote_argument`, `send_literal`); branch `s/B/control-hardening` (`FormatLiteral`, names); branch `s/J/ssh-connection` `crates/remote/src/quote.rs` (`sh_quote`, `remote_command`, `validate_host`) | `crates/runtime/tests/command.rs`, `crates/runtime/tests/tmux.rs` (real tmux); branch J: quoting property test against a POSIX lexer model | B, J | Partial: quoting in place; names and formats Planned (O15) |

### 5.4 Webview and UI (B1)

| Id | Threat | Control | Where | Tested by | Owner | Status |
|---|---|---|---|---|---|---|
| T17 | **XSS** from attacker-controlled text: transcripts, issues, file names, branch names | A strict CSP: no inline scripts, no remote origins, bundled fonts. Markdown without raw HTML; sanitised links with `rel=noopener`; file previews sandboxed. Tauri capabilities give the webview only the commands it needs. | `apps/desktop` (K, a stub on `main`); `apps/ui` (branches L, M, N) | L's build check: no inline script in `dist` (branch) | K, L, M, N | Planned (O11) |
| T18 | Terminal escape sequences in agent output: clipboard writes (OSC 52), links (OSC 8), titles | Not yet decided. | `apps/ui/src/console` (branch `s/M/console-components`) | | M | Open (O12) |
| T19 | The webview obtains a token, or calls commands it should not | The gateway adds the token per workspace; the webview never sees it. A capability allow-list on every command. | `apps/desktop` (K, not built) | | K | Planned |

### 5.5 Parsers and untrusted bytes (B3, B5, B7)

| Id | Threat | Control | Where | Tested by | Owner | Status |
|---|---|---|---|---|---|---|
| T20 | **Hostile transcripts** crash or exhaust the ingest: huge lines, invalid UTF-8, deep nesting, huge fields, many records | Lines over 16 MiB are skipped unread; a partial line is carried in the cursor only up to 64 KiB; every copied field is capped (`bound.rs`) and implausible facts dropped; skip reports are capped; `serde_json`'s recursion limit applies; cursor arithmetic is checked; a page holds at most `limit + 1` records. | `crates/ingest/src/lines.rs`, `bound.rs`, `jsonl.rs`, `claude/`, `codex/` | `crates/ingest/tests/claude.rs`, `codex.rs` (property tests, 200 MB runs); `parse.rs` tests `every_payload_is_capped`, `oversized_facts_are_dropped_or_cut`; fuzz `claude_parse_line`, `codex_parse_line`, `adapter_read`, `adapter_cursor` | A | In place. Residual: a full `serde_json::Value` per line (O13); Claude discovery follows symlinked folders (O14). |
| T21 | **Pane content forges tmux notifications**, or desynchronises the parser | Inside a reply every line is data until the guard with the same time and number, including `%exit`, `%begin` and `%output` lines; unknown records are kept as `Other`; output is decoded once. Pane content that can guess a guard can still end a reply early, so `capture-pane` replies of untrusted panes must not be trusted. Branch B adds limits (1 MiB lines, 4 MiB replies, a latched desync error). | `crates/runtime/src/control.rs` (`ControlParser`) | `crates/runtime/tests/control.rs` `only_matching_guards_finish_a_reply`, `arbitrary_chunking_matches_whole_stream`; fuzz `tmux_control` | B | In place for forging; limits Planned (O15) |
| T22 | A hostile runner or hub sends malformed or huge protocol lines | Typed decoding: unknown variants and fields of the wrong type are errors; a version range check (`is_compatible`). `decode_line` takes a whole line, so the reader must cap the line first. Runners are trusted by transport (in-process, or the user's forwarded socket): `Hello` carries no credential. | `crates/protocol/src/runner.rs` (`decode_line`, `encode_line`), `crates/protocol/src/version.rs` | `crates/protocol/tests/contract.rs`; fuzz `runner_decode_line` | 0, D, J | Partial (O16, O17) |
| T23 | Hostile JSON reaches the desktop, or a corrupt row the store | Typed decoding of `StreamFrame`, `Event` and `TranscriptPage`; the store turns undecodable rows into `Error::Corrupt`, not a panic. | `crates/protocol/src/{api,events,transcript}.rs`, `crates/store/src/store.rs` (`RawRow::decode`) | `crates/protocol/tests/contract.rs`, `crates/store/tests/event_log.rs`; fuzz `api_json` | 0, C | In place |
| T24 | Huge WebSocket messages from a client | The delta stream ignores client messages, but axum's defaults still accept messages up to 64 MiB. | `crates/api/src/stream.rs` (`session`) | | H | Open (O18) |

### 5.6 Files and storage (B7)

| Id | Threat | Control | Where | Tested by | Owner | Status |
|---|---|---|---|---|---|---|
| T25 | **Path traversal** through the runner's file API | Allowed roots are the project locations only; paths are canonicalised and symlink escapes rejected; sizes are capped; writes only inside a location, with a backup. It must be mounted like any nested router (see R1). | `crates/runner` (D, not built) | | D | Open (O19) |
| T26 | SQL injection into the event store | Parameterised queries only; type filters are bound parameters. | `crates/store/src/store.rs` (`before_query`) | `crates/store/tests/event_log.rs` | C | In place |
| T27 | Ingest reads outside the CLIs' homes | Adapters only open files for reading. Codex discovery does not follow symlinks; Claude discovery does (O14). The homes come from the user's own environment (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`). | `crates/ingest/src/codex/mod.rs` (`walk`), `crates/ingest/src/claude/mod.rs` (`discover`) | `crates/ingest/tests/codex.rs` | A | Partial |

### 5.7 Remote machines and SSH (B8)

`crates/remote` is not on `main` yet. References below are to branch `s/J/ssh-connection` and
are **pending** until it merges.

| Id | Threat | Control | Where | Tested by | Owner | Status |
|---|---|---|---|---|---|---|
| T28 | A man in the middle, or an unknown host key, on the first connection | System OpenSSH with the user's config; `StrictHostKeyChecking=ask`, with new host keys shown in a trust dialog through the askpass bridge; without a prompt handler, `BatchMode` fails instead of prompting. | `crates/remote/src/ssh.rs` (`Ssh::args`) (pending) | `crates/remote/tests/fake_ssh.rs`, `real_sshd.rs` (pending) | J | Planned |
| T29 | SSH secrets (passwords, one-time codes, passphrases) leak | An askpass bridge over a private socket (a pipe on Windows): both ends prove a per-call key with HMAC before anything is asked; answers are never stored or logged, and `Secret` hides its value. | `crates/remote/src/askpass/` (pending) | unit tests there (pending) | J | Planned |
| T30 | Agent or X11 forwarding exposes the laptop to the remote | Agent forwarding, X11 forwarding, local commands and forwardings from the config are off. | `crates/remote/src/ssh.rs` (pending) | (pending) | J | Planned |
| T31 | Another local user hijacks a ControlMaster socket | Control sockets live in a private 0700 directory, checked before use. | `crates/remote/src/ssh.rs` (`control_path`), `crates/remote/src/private.rs` (pending) | `ssh.rs` `control_paths_are_checked` (pending) | J | Planned |
| T32 | A malicious remote answers probes, `ssh -G` or SLURM commands with hostile output | Probe output is delimited by markers and parsed defensively; everything a remote reports is untrusted text for the UI (T17) and the hub (T22). | `crates/remote/src/probe.rs` (`probe::parse`), `ssh.rs` (`parse_resolved`), `config.rs` (`list_hosts_in`) (pending) | unit tests there (pending); fuzz targets proposed (§8.4) | J | Planned |
| T33 | A tampered helper runs on the remote | The desktop uploads the helper itself, checks its sha256 against a value compiled into the desktop, runs `--version`, switches atomically and removes old versions. Nothing is downloaded or built on the remote; no root is needed. | Not built (J deploys, P builds and hashes) | | J, P | Planned |

### 5.8 Supply chain, build and release (B9)

| Id | Threat | Control | Where | Tested by | Owner | Status |
|---|---|---|---|---|---|---|
| T34 | A compromised or vulnerable dependency | `cargo-deny` in CI: licences, RustSec advisories, yanked crates, crates.io only, no git sources; lockfiles and `--locked`; pnpm waits a day before installing a new version (`minimumReleaseAge`), runs install scripts only for listed packages, and saves exact versions; Dependabot, grouped weekly. | `deny.toml`, `.github/workflows/ci.yml` (`deny` job), `pnpm-workspace.yaml`, `.npmrc`, `.github/dependabot.yml` | CI | 0 | In place. `fuzz/Cargo.lock` is outside `cargo-deny` (dev only, never shipped; the nightly job can check it). |
| T35 | A compromised workflow or Action | Actions pinned by commit SHA; `permissions: contents: read`; `persist-credentials: false`; on pull requests the guard scripts come from the base branch. | `.github/workflows/ci.yml`, `scripts/ci/` | CI | 0 | In place; `zizmor` proposed (O21) |
| T36 | A tampered installer or update | Code signing on all three OSes, macOS notarisation, a signed Tauri updater, an SBOM and build-provenance attestations per release. | `packaging/`, `.github/workflows/release*.yml` (P, branch `s/P/bench-and-release`) | | P, K | Planned |
| T37 | Private data lands in the public repository | A scrub gate with hashed words and secret patterns, on files and commit metadata; fixtures are synthetic; `private/` folders are ignored. | `scripts/ci/scrub-gate.mjs`, `.github/scrub/hashes.txt`, `.gitignore` | `scripts/ci/test/` | 0 | In place |

### 5.9 Availability

| Id | Threat | Control | Where | Tested by | Owner | Status |
|---|---|---|---|---|---|---|
| T38 | A slow or stuck stream client exhausts the daemon's memory | A bounded queue per client (frames × page), a send timeout, then the client is dropped and resumes with `since`. | `crates/api/src/stream.rs` (`pump`, `StreamConfig`) | `stream.rs` tests | H | In place |
| T39 | Losing contact with a machine kills or forgets its sessions | Machines and sessions become `unverifiable` or `unreachable` and keep their last known state (ADR-0009). | `crates/protocol/src/model.rs` (`Liveness`, `SessionState`); runner and remote (not built) | | D, J | Planned |

## 6. Findings from reviews

What reviews of merged or in-review branches found, and where each stands.

| Id | Finding | Fix | Where | Tested by | Owner | Status |
|---|---|---|---|---|---|---|
| R1 | **Nested-router authentication bypass.** Authentication and the device-only guard were applied with `route_layer`, which does not wrap the fallback of a nested router. A nested router with its own fallback (a file server, for instance) was reachable with no token, or with an agent token on a device-only route. | Both are applied with `layer`, which covers nested fallbacks. `router()` documents that nothing may be merged into the returned router. An empty `RouterParts` no longer panics. | `crates/api/src/lib.rs` (`router`), `crates/auth/src/http.rs` (`device_only`, `require_device`) | `crates/api/tests/auth.rs` `nested_fallbacks_are_authenticated_and_scoped`, `an_empty_router_parts_serves_host_info`; fuzz `api_request` (the app nests a device router with a fallback) | H | Fixed on `main` |
| R2 | **tmux format and `%exit` forging.** A version of the parser that ended an open reply on `%exit` let pane content (a title, a window name) inject notifications, including `%output` for another pane. Names could hold control characters that split lines, and `#{…}`/`#(…)` formats. | Reply bodies stay opaque until their own guard (as on `main`); names refuse C0 and DEL; format literals are escaped. | `crates/runtime/src/control.rs`; branch `s/B/control-hardening` (`command.rs`) | `crates/runtime/tests/control.rs`; fuzz `tmux_control` property 2, which fails if a body line escapes its reply | B | `main` is not affected; the hardening branch must keep `%exit` as body text before it merges (O15) |
| R3 | **Token in the build.** `VITE_PITCREW_TOKEN` from a developer's `.env.local` was inlined by `vite build`, in production and in other modes. | Read only in development; the build fails when it is set; only the needed keys reach the bundle. | branch `s/L/skeleton-and-data` (`apps/ui/src/data/config.ts`, `apps/ui/vite.config.ts`) | `config.test.ts`; a real build with the variable set (branch) | L | Fixed on the branch, not merged (O4) |
| R4 | **Unbounded parser inputs.** Uncapped session facts plus a clone-and-compare of all facts on every line let a small crafted file hang `read_from`; `read_page` kept every turn-duration record; several item payloads were uncapped; the tmux parser had no line or reply limit. | Ingest: facts capped or dropped, `set_first`/`set_latest` instead of cloning, at most `limit + 1` records per page, every payload capped. tmux: `ParserLimits` with a latched desync error (branch B). | `crates/ingest/src/bound.rs`, `jsonl.rs`, `claude/mod.rs`; branch `s/B/control-hardening` (`control.rs`) | ingest tests above; fuzz targets in §8 | A, B | Ingest fixed on `main`; tmux Planned (O15) |
| R5 | **Socket and pipe squatting.** Directories were made private after creation, so a shared group could plant a socket or a registry in the window; existing files and sockets were trusted without an owner check; the pipe was named after the user name; clients had no way to check who served a pipe; the loopback check for development TCP could be skipped. | See T2, T3 and T4: owner checks before use, `O_NOFOLLOW`, a SID-named pipe, client checks, an opaque `Bound`. | `crates/auth/src/private.rs`, `crates/api/src/listener/`, `crates/api/src/client.rs` | the tests under T2–T4 | H | Fixed on `main`; clients must adopt the checks (O1) |
| R6 | An agent with shell access runs as the same OS user and could write a device-scope hash into `tokens.json`. | Accept and document: scopes protect against agents confined to the API, not against code running as the user. | ADR-0006 | | 0, Q | Open (O22) |

## 7. Open items

| Id | What | Owner |
|---|---|---|
| O1 | The desktop gateway and the CLI call `check_unix_socket` + `check_unix_peer` (Unix) or `check_pipe_server` (Windows) before sending any token. | K, I |
| O2 | Windows: put the state directory under the user's profile and verify its owner, or set a protected DACL on it, before opening `tokens.json`. | H, K |
| O3 | Device tokens in the OS keychain; agent tokens for hooks through a 0600 file or the environment, never argv. | K, I |
| O4 | Merge the UI token guard (R3), and add a CI check that fails if the built UI contains `pcd_`, `pca_` or a development token. | L, 0 |
| O5 | API routes and a Settings page to list, revoke and rotate tokens. | H, L |
| O6 | Domain handlers stamp `author` and `on_behalf_of` from `Caller` and limit agent writes to the agent's own tasks and sessions. | E |
| O7 | The audit log of person-only actions. | E, H |
| O8 | The Inbox as the only path for decisions, approvals and outward writes; the back office's "never" list. | E, F, G |
| O9 | The runner's `HookSink` treats payloads as untrusted: a hook may only update sessions its caller owns. | D |
| O10 | An agent token can never start or dispatch a session with a stronger permission mode than policy allows; `bypass_permissions` needs the workspace opt-in. | D, E |
| O11 | A CSP check on the built UI in CI; markdown without raw HTML; sanitised links. | K, L, M, 0 |
| O12 | Terminal: disable OSC 52 clipboard writes by default, allow only `http(s)` links, treat titles as text. | M |
| O13 | Typed, borrowed per-line structs instead of a full `serde_json::Value` (memory amplification on long lines). | A |
| O14 | Claude discovery: do not follow symlinked project folders out of the home. | A |
| O15 | Merge the tmux hardening (limits, names, formats) with `%exit` kept as body text; then adapt the `tmux_control` target to `feed` returning `Result`. | B, Q |
| O16 | The runner protocol reader caps a line (for example 16 MiB) before `decode_line`, and bounds `Events` batches and `TerminalOutput`. | D, J, 0 |
| O17 | Authenticate runners to hubs before a runner can attach over anything but the user's own transport. | 0, J |
| O18 | Set small `max_message_size` / `max_frame_size` on the delta stream's WebSocket, and bounded ones on the terminal WebSocket. | H |
| O19 | The runner's file API: roots, canonicalisation, symlink escapes, size caps, backups; a Q review before merge. | D |
| O20 | Add the nightly fuzz workflow (§8.3). | 0 |
| O21 | CodeQL and `zizmor` for the workflows; the fuzz lockfile under `cargo-deny` in the nightly job. | 0 |
| O22 | Record the residual risk of R6 in ADR-0006. | 0 |

## 8. Fuzzing

`fuzz/` is a `cargo-fuzz` project with its own workspace (the root excludes it). It needs a
nightly toolchain.

```sh
python3 fuzz/seed.py                 # seed fuzz/corpus/<target>/ from crates/fixtures
cargo +nightly fuzz run adapter_read -- -dict=fuzz/dict/claude.dict -max_total_time=60
```

A crash leaves its input in `fuzz/artifacts/<target>/`; `cargo +nightly fuzz fmt <target> <file>`
prints it and `cargo +nightly fuzz tmin <target> <file>` minimises it. Turn it into a regression
test in the owning stream's crate.

### 8.1 Targets

| Target | Code under test | Untrusted source | Checks besides "no panic" | Dictionary |
|---|---|---|---|---|
| `claude_parse_line` | `ingest::claude::parse_line` | a transcript line (B7) | every payload within its cap; items carry the line's offset; items survive a JSON round trip; facts bounded | `claude.dict` |
| `codex_parse_line` | `ingest::codex::parse_line` | a rollout line (B7) | as above, plus at most 100 file edits per patch | `codex.dict` |
| `adapter_read` | `ClaudeAdapter` / `CodexAdapter` `read_from` and `read_page` on file bytes | a transcript file (B7) | reading in two parts equals reading at once; paging backwards equals reading forwards; pages always make progress; offsets ordered and in range | `claude.dict` or `codex.dict` |
| `adapter_cursor` | `read_from` with an arbitrary cursor state and offset | a stored or stale cursor | an error only past the end of the file; the new cursor stays in the file; a second read finds nothing new | `claude.dict` |
| `tmux_control` | `runtime::ControlParser::feed`, with arbitrary chunking | tmux output and pane content (B5) | chunking never changes the result; lines inside a reply never escape it; escaped `%output` decodes exactly | `tmux.dict` |
| `runner_decode_line` | `protocol::runner::decode_line` into `RunnerToHub`, `HubToRunner`, `RunnerCommand`, `CommandOutcome` | the runner connection (B3) | what decodes encodes to one line and decodes back unchanged | `protocol.dict` |
| `api_json` | `serde_json` into `Event`, `StreamFrame`, `TranscriptPage`, `TranscriptItem`, `HostInfo`, `ApiError` | the hub's API and delta stream (B2) | round trip unchanged | `protocol.dict` |
| `api_request` | the whole API app: `pitcrew_api::router`, authentication, hooks, the delta stream route, a nested device router with a fallback | any client of the socket (B2, B6) | a 2xx beyond host info only with a minted token in an authentication header; device routes only for the device token; the caller seen is the token's; every 401 has a `Bearer` challenge | `http.dict` |

The round-trip checks use `serde_json`'s `float_roundtrip` feature, so they cannot fail on the
last bit of a float. `fuzz/src/lib.rs` copies the ingest caps (they are crate-private); if the
caps change, change them there too.

### 8.2 Limits of the current targets

- Inputs are small (libFuzzer's default maximum is about 4 KiB, or the largest seed), so the
  64 KiB carried-line and 16 MiB line thresholds are only reached by stream A's own tests.
- `tmux_control` targets `feed` as it is on `main`; branch B changes it to return a `Result`
  (O15).
- Not fuzzed yet: the OpenCode adapter (branch `s/A/opencode-adapter`), the store's row decoding
  (private), `TokenHash::from_hex` and the registry parser (private), the command quoting in
  `crates/runtime/src/command.rs` against a tmux lexer model.

### 8.3 Nightly CI (proposal)

Stream 0 owns `.github/workflows`; the workflow for it is in stream Q's report. It seeds the
corpora, runs each target for 5 minutes with its dictionary, keeps the corpus in the Actions
cache, and uploads crash artifacts.

### 8.4 Targets to add as streams land

- `crates/remote` (J): `probe::parse` on arbitrary output; `config::list_hosts_in` on arbitrary
  config text (with `Include`); `quote::sh_quote`/`remote_command` against `sh_split` (a
  round-trip property); `askpass::classify`.
- `crates/runner` (D): the protocol line reader (with its cap), the file API's path checks.
- `crates/ingest` OpenCode (A): the SQLite reader on arbitrary databases.
- Hook payloads into the runner's session state (D).

## 9. Coverage of the security table

The plan's security table and the ADRs' security commitments (ADR-0003, ADR-0006, ADR-0009,
`SECURITY.md`), and where each is covered here.

| Commitment | Covered by |
|---|---|
| Other users on a shared machine: private socket or pipe, 0600 state, peer uid check | T1–T4, R5 |
| A stolen or leaked token: headers only, redacted logs, keychain, rotation | T6–T9 |
| Agents acting as you: scoped tokens enforced in the host, audit log for person-only actions | T10, T11, R6 |
| Prompt injection: authors stamped by the host, agent text never authorises, Inbox for irreversible actions | T12, T13 |
| XSS in the webview: CSP, no raw HTML, sanitised links, capabilities, sandboxed previews | T17–T19 |
| Path traversal via the file API | T25, T27 |
| Supply chain: `cargo-deny`, `cargo-audit`, pnpm hardening, Dependabot, SBOM, provenance, pinned Actions, CodeQL, `zizmor` | T34, T35, T36, O21 |
| A tampered update, installer or remote helper | T33, T36 |
| SSH: host keys, no agent forwarding, askpass, nothing stored | T28–T32 |
| Hooks altering agent configs | T14 |
| Agents launched with too much power | T15 |
| Parser crashes on hostile input: fuzzing, capped lines and records | T20–T24, R4, §8 |
| The webview never holds a token (ADR-0003) | T8, T19 |
| No listening TCP ports on shared machines (ADR-0006, `SECURITY.md`) | T1, T4 |
| Authors stamped by the daemon (ADR-0006, `SECURITY.md`) | T10 |
| The remote helper needs no root and no internet, and is checksummed (ADR-0009, `SECURITY.md`) | T33 |
