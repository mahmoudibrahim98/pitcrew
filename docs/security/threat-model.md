# Threat model

A living document, owned by stream Q. Every stream that lands code named in a "Where" cell, and
every review that finds a security issue, updates it.

- **Last reviewed:** 2026-10-01, against `main` at `b251180` (which merged `s/M/terminal`,
  `s/L/desktop-transport` and `integrator/daemon-wiring` during the review), plus the open
  branches `s/K/shell-and-gateway` (`fe38305`), `s/J/slurm` (`5f6e770`), `s/G/jira-read`
  (`3f1a224`) and `s/B/control-hardening` (`e77c7a0`). The fuzz runs in §8 are against `main` at
  `fe76853`. The previous review was against `f365897` (2026-09-30).
- **Status:** **In place** (on `main`, with a test) · **Partial** (some of it on `main`) ·
  **Planned** (in an open branch, not on `main`) · **Open** (a gap: the owner must act; listed
  again in [Open items](#7-open-items)).
- **Owners** are streams, as in `docs/build/ownership.json`. Code references are paths on `main`
  unless marked with a branch.

## 1. Scope

In scope: the desktop shell and its webview UI, `pitcrewd` in both roles (hub and runner), the
CLI and hooks, the runner protocol, the remote helper and the SSH layer, batch jobs on clusters,
the read side of the GitHub and Jira integrations, the back office, and the build and release
pipeline.

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
| A5 | Credentials on remote machines and for trackers | SSH keys, passwords and one-time codes, the CLIs' account homes, cluster accounts, GitHub and Jira tokens. |
| A6 | The event log and workspace state | Authorship (who did what) and decisions must be true; the log must stay available. |
| A7 | The helper binary, installers and updates | Whatever they contain runs on every machine. |

## 3. Actors

| Id | Actor | Can | Assumed not to |
|---|---|---|---|
| U1 | **Another user on a shared machine** (an HPC login node hosts hundreds) | Connect to any socket they can reach, create files in shared folders, read other users' process arguments, see the batch queue | Read files in the user's private folders |
| U2 | **Malicious content**: transcript text, issue bodies, file contents, web pages | Reach agents as prompt injection, and reach our parsers and the UI as arbitrary bytes | Run code directly |
| U3 | **The agent itself**, confused or injected | Everything the user's OS account can do if it has a shell; call the API with its agent token | Hold the device token, unless it can read it (see T10) |
| U4 | **A stolen or leaked token** | Everything its scope allows, from anywhere that reaches the transport | Survive rotation |
| U5 | **A compromised dependency** (crate, npm package, GitHub Action) | Run code at build time or in the shipped app | |
| U6 | **A malicious or compromised remote host**, or someone in the middle of a first connection; also a hostile GitHub Enterprise server, or a proxy in front of one | Answer SSH, every probe and every API call with anything; run anything as the user there | Reach the desktop other than through SSH output, the forwarded socket and API responses |
| U7 | **A web page in the user's browser** | Send requests to `localhost`, rebind DNS | Read responses from another origin, set `Authorization` on a WebSocket |

## 4. Trust boundaries

```text
 ┌──────────────┐ B1 Tauri IPC   ┌──────────────────┐ B2 unix socket / named pipe /
 │ webview (UI) │───────────────▶│ desktop gateway  │    SSH-forwarded socket, bearer header
 └──────────────┘ capabilities   │ (adds the token) │──────────────────────────────┐
                                 └───────┬──────────┘                              ▼
                                         │ B8 system OpenSSH, askpass     ┌──────────────────┐
                                         ▼                                │ pitcrewd: hub    │ B10 HTTPS
                                 ┌──────────────────┐   B3 runner        │ (API, auth, log, │──────────▶ GitHub,
                                 │ remote helper    │◀── protocol ──────▶│  back office B12)│            Jira
                                 │ (pitcrewd runner)│   (JSON lines)     └────────▲─────────┘
                                 └──┬────┬──────────┘                             │ B6 hooks,
                 B11 sbatch, squeue │    │ B4 spawn, env; B5 tmux -C / PTY       │ agent token
                                    ▼    ▼                                        │
                     ┌────────────┐ ┌──────────────────┐  writes   ┌─────────────┐   │
                     │ SLURM job  │ │ agent CLIs       │──────────▶│ transcripts │   │
                     │ (a node)   │ │ (in tmux or PTY) │───────────┼─────────────┼───┘
                     └────────────┘ └──────────────────┘           └──────┬──────┘
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
| B7 | transcripts on disk → ingest | JSONL files and the OpenCode database written by the CLIs | Nothing: content is U2 |
| B8 | desktop ↔ SSH ↔ remote helper | SSH, askpass prompts, the helper upload, a forwarded socket or stdio bridge | The user's own SSH config and known hosts |
| B9 | source → build → release → users | Dependencies, CI, installers, updates, the helper binary | Pinned, reviewed inputs only |
| B10 | hub ↔ GitHub and Jira | HTTPS reads with the user's tracker credentials; issue, pull request and milestone text | Nothing upstream: text and links are U2, and a GitHub Enterprise server or a proxy in front of it may be U6 |
| B11 | remote helper ↔ the batch scheduler (SLURM) and compute nodes | Job scripts, `sbatch`, `squeue`, `scancel`, `sacct`, a socket on the node | The user's own cluster account; other users' jobs and shared folders are U1 |
| B12 | the back office (`@office`) inside the daemon ↔ the work model | The actions its rules emit, applied in-process | Nothing: the office is an agent (U3), and the hub checks each of its actions again |

## 5. Threats and controls

### 5.1 Local transport and other users (B2)

| Id | Threat | Control | Where | Tested by | Owner | Status |
|---|---|---|---|---|---|---|
| T1 | Another user connects to the daemon and uses its API | Unix: a socket (0600) in a 0700 directory; every accepted connection's peer uid (`SO_PEERCRED`) must equal the daemon's, else it is dropped; a token is still required. Windows: a named pipe whose protected DACL grants only the current user's SID, with remote clients rejected. | `crates/api/src/listener/unix.rs` (`UnixSocket::bind`, `accept`), `crates/api/src/listener/pipe.rs`, `crates/api/src/listener/pipe_security.rs` (`PipeSecurity::current_user_only`) | `unix.rs` tests `connections_from_another_uid_are_dropped`; `crates/api/tests/unix_socket.rs`; `pipe.rs` test `the_current_user_owns_it_and_alone_has_access` (reads the DACL back); `crates/api/tests/named_pipe.rs` | H | In place |
| T2 | **Socket and pipe squatting**: another user creates the directory, socket or pipe first, and the daemon or a client uses theirs, handing over a token | Server: the directory is created 0700 and its owner checked before anything else; an existing one must already be ours and private (refused, never re-permissioned); a stale socket is removed only if it is ours and dead; `Drop` removes the socket only if its `(dev, ino)` is still ours. Windows: the pipe is named after the user's SID and created with `FILE_FLAG_FIRST_PIPE_INSTANCE`. Client: `check_unix_socket` (directory ours and 0700, socket ours) before connecting and the peer's uid after; on Windows the pipe is opened with `SECURITY_IDENTIFICATION` and `check_pipe_server` compares its owner SID with the user's. The CLI refuses a `PITCREW_PIPE` that is not a plain local pipe name (a `\\.\pipe\..\UNC\…` name once reached SMB). | `crates/auth/src/private.rs` (`create_private_dir`, `check_private_dir`), `unix.rs` (`remove_stale`, `Drop`), `crates/api/src/listener/mod.rs` (`default_pipe_name`), `crates/api/src/client.rs`; `crates/cli/src/transport.rs` (`connect_unix`, `connect_pipe`, `check_peer`), `crates/cli/src/config.rs` (`is_local_pipe_name`) | `unix.rs` tests `a_live_socket_is_not_replaced_and_a_stale_one_is`, `something_else_in_the_way_is_left_alone`, `an_open_directory_is_refused_before_anything_is_touched`, `drop_leaves_a_socket_that_replaced_ours`; `client.rs` tests; `crates/cli/tests/verbs.rs` `a_socket_in_an_open_directory_never_gets_the_token`; `crates/cli/tests/hook.rs` `over_a_socket_only_a_private_one_gets_the_token`; `config.rs` `only_plain_local_pipe_names` | H (checks), I and K (callers) | Partial: the CLI checks (In place); the desktop on branch K (T44, O1). Residual: the CLI's peer-uid check runs on Linux only (macOS and BSD keep the directory and owner checks), and an elevated administrator can plant a pipe whose owner passes. |
| T3 | Another user plants or edits the token registry | The state directory is private (owner and mode checked); `tokens.json` is opened with `O_NOFOLLOW` and must be ours and not writable by group or others; writes go to a new, uniquely named temporary file, then a rename; an exclusive lock makes the daemon the single writer. | `crates/auth/src/store.rs` (`FileTokenStore::open`, `persist`), `crates/auth/src/private.rs` (`open_private_file`, `create_new_private_file`, `ExclusiveLock`) | `store.rs` tests `a_registry_others_can_write_is_refused`, `a_symlinked_registry_is_refused`, `an_open_state_directory_is_refused`, `only_one_store_may_open_a_registry`, `malformed_registries_are_errors` | H | In place on Unix. Windows relies on the directory's inherited ACL (O2). |
| T4 | Development TCP is reached by other local users, or by a web page through DNS rebinding | Only when asked, and only on loopback (checked in `Bound::bind`; `Bound` cannot be built another way); the `Host` header must name `localhost`, `127.0.0.1`, `[::1]` or `*.localhost`; a bearer token is still required. There are no cookies, so a cross-site request carries nothing. `pitcrewd` listens privately by default and takes `tcp:` only for loopback. On TCP only, it adds CORS for `http://localhost[:port]`, `http://127.0.0.1[:port]` and the Tauri origins: preflights and WebSocket upgrades from other origins get 403; other requests pass to the token check without CORS headers; no `Allow-Credentials`. | `crates/api/src/listener/mod.rs` (`Bound::bind`, `local_host_only`, `is_local_host`); `crates/daemon/src/cli.rs` (`ServeArgs`), `crates/daemon/src/cors.rs` (`cors`, `is_allowed_origin`) | `listener/mod.rs` tests `local_hosts`, `tcp_must_be_loopback`; `crates/api/tests/websocket.rs` `development_tcp_refuses_foreign_host_headers`; `cors.rs` `only_local_and_tauri_origins_are_allowed`; `cli.rs` `listen_takes_private_or_loopback_tcp`; `crates/daemon/tests/serve.rs` `demo_serves_the_work_model_with_real_tokens` | H, 0 (the daemon must never enable it by default) | In place. No test covers the 403 on a WebSocket upgrade from another origin (O33). |
| T5 | `GET /v1/host/info` answers without a token (host name, OS, versions) | Needed to detect version skew before authenticating. On the private transports only the same user reaches it. | `crates/api/src/lib.rs` (`router`), `crates/api/src/host.rs` | `crates/api/tests/auth.rs` `host_info_needs_no_token` | H | Accepted |

### 5.2 Tokens and scopes (B1, B2, B6)

| Id | Threat | Control | Where | Tested by | Owner | Status |
|---|---|---|---|---|---|---|
| T6 | A token leaks through a URL, a log or a handler | `Authorization: Bearer` only, never the query string; WebSockets offer `pitcrew.bearer.<token>` as a subprotocol; two credentials are ambiguous and refused; after verification both headers are removed, so no handler or log downstream sees them; rejections never quote the token; `SecretToken`'s `Debug` shows only the prefix; logs name tokens by `TokenId`. The CLI's request and client types redact the token in `Debug`. | `crates/api/src/auth.rs` (`authenticate`, `bearer_token`, `scrub`), `crates/auth/src/token.rs` (`SecretToken`), `crates/cli/src/http.rs`, `crates/cli/src/client.rs` | `crates/api/tests/auth.rs` (`a_token_in_the_query_only_is_unauthorized`, `two_authorization_headers_are_unauthorized`, `unauthorized_responses_carry_a_bearer_challenge`); `auth.rs` unit tests; `token.rs` `debug_never_prints_the_token`; CLI `debug_never_shows_the_token`; fuzz `api_request` | H, I | In place |
| T7 | A token is stolen from disk or from another user's view of the process list | Only SHA-256 hashes of 256-bit random tokens are stored, compared in constant time over every entry. The daemon writes the device token to `<state>/device.token` (0600); the OS keychain (ADR-0003) is on branch K and not used yet. The CLI takes the agent token from `PITCREW_TOKEN` or a private file (`PITCREW_TOKEN_FILE`; on Unix opened `O_NOFOLLOW`, ours, not readable by others, at most 4 KiB of visible ASCII), never argv; installed hooks carry no token on their command lines. | `crates/auth/src/token.rs` (`TokenHash`, `ct_eq`), `crates/auth/src/store.rs` (`verify`), `crates/daemon/src/state.rs` (`write_token`, `read_token`), `crates/cli/src/config.rs` (`token_from_env`, `read_private_file`) | `store.rs` `the_registry_round_trips_and_never_holds_a_raw_token`, `unknown_well_formed_tokens_fail`; `config.rs` `a_token_file_must_be_private`; `crates/cli/tests/verbs.rs` `a_token_file_open_to_others_is_refused`; `state.rs` `token_files_are_private`; `crates/daemon/tests/serve.rs` `token_show_path_prints_the_path_never_the_token` | H, K, I, 0 | Partial: nothing mints and delivers agent tokens to the agents it starts yet; on Windows a token file is checked only as a regular file (O2, O3). |
| T8 | **Token in a build**: a developer's real token is inlined into the UI bundle | The UI reads `VITE_PITCREW_TOKEN` only in development and only named keys; `vite build` fails while it is set, in every mode. The webview never holds a token (ADR-0003): the gateway adds it (T40). | `apps/ui/src/data/config.ts` (`resolveToken`), `apps/ui/vite.config.ts` (`noTokenInBuilds`) | `apps/ui/tests/config.test.ts` ("refuses a … build while VITE_PITCREW_TOKEN is set in an .env file", "refuses a build while VITE_PITCREW_TOKEN is set in the shell", "builds with no token, no other env values and no inline script") | L, K | In place. A CI check of the built UI for token prefixes is still open (O4). |
| T9 | A leaked token stays valid | `TokenStore::revoke` and `rotate`; every request verifies against the live registry. | `crates/auth/src/store.rs` | `store.rs` `revoke_and_rotate` | H (routes), K and L (Settings → Security) | Partial: no API route or UI yet (O5) |
| T10 | **An agent acts as the person** | Two scopes. Routes are device-only unless added with `RouterParts::agent`, so a forgotten mark fails closed; `device_only` and `Person` answer 403 to agents. The API layer inserts the `Caller`; the work model stamps `author` from it and `on_behalf_of` only for agents, never from a body (`WorkService::by`). Agents write only to their own tasks (assignee, or an active dispatch) and asks; person-only handlers refuse agents even when mounted without the guard. `TaskStatus::can_move` never lets an agent move a task to done or canceled. | `crates/api/src/lib.rs` (`RouterParts`, `router`), `crates/auth/src/http.rs` (`Authenticated`, `Person`, `device_only`), `crates/protocol/src/api.rs` (`Caller`), `crates/protocol/src/model.rs` (`TaskStatus::can_move`); `crates/hub-work/src/service.rs` (`WorkService::by`), `commands.rs` (`require_person`, `require_own_task`), `query.rs` (`is_own_task`) | `crates/api/tests/auth.rs` `an_agent_token_on_a_device_only_route_is_forbidden`; `http.rs` tests; `crates/protocol/tests/contract.rs` `task_move_rules`; `crates/hub-work/tests/rules.rs` (`agents_write_only_to_their_own_tasks`, `device_handlers_refuse_agents_even_when_mounted_without_the_guard`, `a_forbidden_agent_hears_403_before_any_400`); `tests/single_writer.rs` `only_agents_act_on_behalf_of_someone`; fuzz `api_request` | H, E, 0 | In place for tasks and asks; sessions wait on the runner's hook sink (O9). **Residual:** an agent with a shell runs as the same OS user, so it can edit `tokens.json` or read a token it finds; the split holds only for agents confined to the API (O22). |
| T11 | Person-only actions leave no trace | An audit log of person-only actions: decisions, approvals of outward writes, ending sessions, settings, skip-permissions (ADR-0006). | Not built | | E, H | Open (O7) |

### 5.3 Agents, hooks and prompt injection (B4, B6, B12)

| Id | Threat | Control | Where | Tested by | Owner | Status |
|---|---|---|---|---|---|---|
| T12 | **Prompt injection** through transcripts, issues, files or web pages makes an agent approve, decide or send something outward | Agent tokens cannot take person-only actions (T10). Decisions, approvals and outward writes need a device token and a click in the Inbox. Authors are stamped by the host. Agent text is shown as information and never authorises anything. The back office never sends anything outward (T49). | T10's layers; `crates/office/src/guard.rs`, `crates/hub-work/src/office.rs`; the Inbox and outward writes (G, later) | `crates/office/tests/never.rs` `never_asks_to_send_anything_outward`, `crafted_approval_asks_never_lead_to_one` | E, F, G | Partial: the Inbox as the only path for outward writes is not built; the tracker crates only read (O8). |
| T13 | An agent floods the hook intake, or sends huge or malformed bodies | An agent route with a token; the body is at most 1 MiB and must be a JSON object; the engine must be known and the event name match `[A-Za-z][A-Za-z0-9_-]{0,63}`; a bounded channel drops and counts rather than blocking; the sink runs on the blocking pool and a panic in it is caught. | `crates/api/src/hooks.rs` | `crates/api/tests/hooks.rs` (`bad_requests_are_invalid`, `hooks_need_a_token`, `a_full_channel_still_answers_202_and_counts_the_drop`, `a_blocking_or_panicking_sink_never_stalls_requests`); fuzz `api_request` | H | In place. The payload is untrusted in the runner's `HookSink` too (O9). |
| T14 | Hook installation changes agent configurations badly | `pitcrew hooks install` shows the diff and asks unless `--yes` (`--json` needs `--yes`); re-reads the file before writing and refuses if it changed; backs it up (0600, at most five) and writes atomically through a 0600 temporary file, through a symlink rather than over it. A hook is ours only when its command is a single program word naming `pitcrew` plus our marker, never a multi-word command that ends with our path. A foreign Codex `notify` is never replaced unless `--chain`, which records its exact text and runs the original with no shell (`PITCREW_CHAINED` stops loops); uninstall restores it. | `crates/cli/src/install/` (`mod.rs` `apply_change`, `is_single_shell_word`; `claude.rs`, `codex.rs`, `opencode.rs`, `jsontext.rs`), `crates/cli/src/hook.rs` (`run_chained`) | `install/mod.rs` `apply_change_refuses_a_file_that_changed_since_it_was_read`, `apply_change_backs_up_and_writes_atomically`; `claude.rs` `a_multi_word_command_ending_in_our_path_is_never_claimed_as_ours`; `codex.rs` `a_foreign_notify_is_reported_not_overwritten`, `install_refuses_to_chain_when_notify_already_names_pitcrew`; `crates/cli/tests/hook.rs` `runs_the_recorded_original_with_the_exact_payload_codex_passed`; fuzz `cli_hooks` | I | In place |
| T15 | Agents launched with too much power | The default permission mode is the CLI's own. `bypass_permissions` needs a per-workspace opt-in, with a warning and an audit entry; the runner answers `CommandOutcome::Rejected` when policy forbids. Today dispatch is person-only (`require_person`) and takes the persona's permission mode. | `crates/protocol/src/runner.rs` (`RunnerCommand::StartSession.permission_mode`, `CommandOutcome::Rejected`); `crates/hub-work/src/dispatch.rs`; enforcement in `crates/runner` (not built) | `crates/hub-work/tests/dispatch.rs` | D, E | Open (O10): no opt-in, warning or audit; agents cannot dispatch. |
| T16 | Command injection into tmux or a shell through session names, paths or typed text | tmux commands are built from typed arguments and quoted for tmux's own parser (no shell); text goes through `send-keys -l --`; NUL is refused and control bytes become octal escapes. Branch B adds escaping of `#{…}`/`#(…)` formats and refuses control characters in names. Remote commands are POSIX-quoted, then sent inside a fixed `/bin/sh -c 'eval "$(printf …)"'` wrapper of octal escapes that sh, bash, zsh, fish, csh and tcsh all pass unchanged; xonsh login shells are refused; host names are limited to `A-Z a-z 0-9 . _ : % [ ] @ -` and may not look like options. | `crates/runtime/src/command.rs` (`Command`, `quote_argument`, `send_literal`); branch `s/B/control-hardening` (`FormatLiteral`, names); `crates/remote/src/quote.rs` (`sh_quote`, `posix_command`, `remote_command`, `validate_host`), `crates/remote/src/probe.rs` (`is_unsafe_shell`) | `crates/runtime/tests/command.rs`, `crates/runtime/tests/tmux.rs` (real tmux); `quote.rs` proptests `quoting_round_trips`, `wrapped_commands_are_shell_neutral_for_hostile_input`; `crates/remote/tests/login_shells.rs` `hostile_argv_survives_every_login_shell`; `crates/remote/tests/fake_ssh.rs` `hostile_hosts_never_reach_ssh`; fuzz `remote_quote` | B, J | Partial: remote quoting In place; tmux names and formats Planned (O15) |

### 5.4 Webview and UI (B1)

| Id | Threat | Control | Where | Tested by | Owner | Status |
|---|---|---|---|---|---|---|
| T17 | **XSS** from attacker-controlled text: transcripts, issues, file names, branch names | A strict CSP: no inline scripts, no remote origins, bundled fonts (T43). Markdown without raw HTML; links only `http`, `https` and `mailto`, with `rel="noopener noreferrer"`; a markdown parser that stays linear on hostile text; file previews sandboxed. Tauri capabilities give the webview only the commands it needs. | `apps/ui/src/console/render/links.tsx` (`safeHref`), `apps/ui/src/console/render/markdown-parse.ts`; branch K `apps/desktop/src-tauri/tauri.conf.json`, `capabilities/main.json` | `apps/ui/src/console/tests/markdown.test.ts` ("keeps raw HTML as text", "allows http, https and mailto only", "stays linear on tens of thousands of closers with no opener, which stay text"); `apps/ui/tests/config.test.ts` "builds with no token, no other env values and no inline script" | K, L, M, N | Partial: the console on `main`; the CSP on branch K (O11) |
| T18 | Terminal escape sequences in agent output: clipboard writes (OSC 52), links (OSC 8), titles | See T47. | `apps/ui/src/console/terminal/` | see T47 | M | In place |
| T19 | The webview obtains a token, or calls commands it should not | The gateway adds the token per workspace; the webview never sees it. A capability allow-list on every command. See T40–T43. | branch K `apps/desktop/src-tauri/` | see T40–T43 | K | Planned |

### 5.5 Parsers and untrusted bytes (B3, B5, B7)

| Id | Threat | Control | Where | Tested by | Owner | Status |
|---|---|---|---|---|---|---|
| T20 | **Hostile transcripts** crash or exhaust the ingest: huge lines, invalid UTF-8, deep nesting, huge fields, many records | Lines over 16 MiB are skipped unread; a partial line is carried in the cursor only up to 64 KiB; every copied field is capped (`bound.rs`) and implausible facts dropped; skip reports are capped; `serde_json`'s recursion limit applies; cursor arithmetic is checked; a page holds at most `limit + 1` records. OpenCode: the database is opened read-only, payloads over 16 MiB are skipped unread, positions are capped at 2^53 − 1, the cursor remembers at most 4,096 open parts, and a damaged store is skipped until it changes. | `crates/ingest/src/lines.rs`, `bound.rs`, `jsonl.rs`, `claude/`, `codex/`, `opencode/` (`mod.rs`, `store.rs`, `parse.rs`) | `crates/ingest/tests/claude.rs`, `codex.rs` (property tests, 200 MB runs), `opencode.rs` (`malformed_and_huge_payloads_are_skipped`, `a_damaged_store_read_unlocked_is_unreadable_and_skipped`); `parse.rs` tests `every_payload_is_capped`, `oversized_facts_are_dropped_or_cut`; fuzz `claude_parse_line`, `codex_parse_line`, `adapter_read`, `adapter_cursor`, `opencode_store` | A | In place. Residual: a full `serde_json::Value` per line (O13). |
| T21 | **Pane content forges tmux notifications**, or desynchronises the parser | Inside a reply every line is data until the guard with the same time and number, including `%exit`, `%begin` and `%output` lines; unknown records are kept as `Other`; output is decoded once. Pane content that can guess a guard can still end a reply early, so `capture-pane` replies of untrusted panes must not be trusted. Branch B adds limits (1 MiB lines, 4 MiB replies, a latched desync error). | `crates/runtime/src/control.rs` (`ControlParser`) | `crates/runtime/tests/control.rs` `only_matching_guards_finish_a_reply`, `arbitrary_chunking_matches_whole_stream`; fuzz `tmux_control` | B | In place for forging; limits Planned (O15) |
| T22 | A hostile runner or hub sends malformed or huge protocol lines | Typed decoding: unknown variants and fields of the wrong type are errors; a version range check (`is_compatible`). `decode_line` takes a whole line, so the reader must cap the line first. Runners are trusted by transport (in-process, or the user's forwarded socket): `Hello` carries no credential. | `crates/protocol/src/runner.rs` (`decode_line`, `encode_line`), `crates/protocol/src/version.rs` | `crates/protocol/tests/contract.rs`; fuzz `runner_decode_line` | 0, D, J | Partial: no reader exists on `main` yet (O16, O17) |
| T23 | Hostile JSON reaches the desktop, a corrupt row the store, or a hostile export file an import | Typed decoding of `StreamFrame`, `Event` and `TranscriptPage`; the store turns undecodable rows into `Error::Corrupt`, not a panic; `Store::import` decodes each line as an `Event` and appends in batches. | `crates/protocol/src/{api,events,transcript}.rs`, `crates/store/src/store.rs` (`RawRow::decode`, `Store::import`), `crates/store/src/maintenance.rs` (`decode_line`) | `crates/protocol/tests/contract.rs`, `crates/store/tests/event_log.rs`, `crates/store/tests/maintenance.rs`; fuzz `api_json`, `store_import` | 0, C | In place, except that a failed import keeps the batches before the bad line (R7, O31). |
| T24 | Huge WebSocket messages from a client | The delta stream accepts client messages of at most 4 KiB (`MAX_INBOUND`, applied as `max_message_size` and `max_frame_size`); the terminal WebSocket caps inbound messages at 1 MiB (`max_inbound`). Both close with 1009. | `crates/api/src/stream.rs`, `crates/api/src/terminal.rs` | `crates/api/tests/stream.rs` `a_message_over_4_kib_closes_with_1009`; `crates/api/tests/terminal.rs` `a_message_over_max_inbound_closes_with_1009`; fuzz `api_terminal` | H | In place |

### 5.6 Files and storage (B7)

| Id | Threat | Control | Where | Tested by | Owner | Status |
|---|---|---|---|---|---|---|
| T25 | **Path traversal** through the runner's file API | Allowed roots are the project locations only; paths are canonicalised and symlink escapes rejected; sizes are capped; writes only inside a location, with a backup. It must be mounted like any nested router (see R1). The gateway's own path check (T41) does not decode `%2F`, so the route must canonicalise what it receives. | `crates/runner` (D, not built) | | D | Open (O19, O24) |
| T26 | SQL injection into the event store | Parameterised queries only; type filters are bound parameters. | `crates/store/src/store.rs` (`before_query`) | `crates/store/tests/event_log.rs` | C | In place |
| T27 | Ingest reads outside the CLIs' homes | Adapters only open files for reading. Claude and Codex discovery do not follow symlinks; OpenCode discovery lists regular files only. The homes come from the user's own environment (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`, `XDG_DATA_HOME`). | `crates/ingest/src/codex/mod.rs` (`walk`), `crates/ingest/src/claude/mod.rs` (`discover`), `crates/ingest/src/opencode/mod.rs` (`discover`) | `crates/ingest/tests/codex.rs`; `crates/ingest/tests/claude.rs` `discovery_does_not_follow_links_out_of_the_projects_root` | A | In place |

### 5.7 Remote machines and SSH (B8)

`crates/remote` is on `main`. Its test suites have not run on Windows yet (the crate's README).

| Id | Threat | Control | Where | Tested by | Owner | Status |
|---|---|---|---|---|---|---|
| T28 | A man in the middle, or an unknown host key, on the first connection | System OpenSSH with the user's config; `StrictHostKeyChecking=ask`, with new host keys shown in a trust dialog through the askpass bridge; without a prompt handler, `BatchMode` fails instead of prompting. | `crates/remote/src/ssh.rs` (`Ssh::args`) | `crates/remote/tests/fake_ssh.rs` (`exact_argument_list`, `host_key_prompt`, `updated_host_keys_are_a_yes_no_question`); `crates/remote/tests/real_sshd.rs` `runs_quotes_and_probes_a_real_host` | J | In place |
| T29 | SSH secrets (passwords, one-time codes, passphrases) leak | An askpass bridge over a private socket (a pipe on Windows): both ends prove a per-call key with HMAC before anything is asked; answers are never stored or logged, and `Secret` hides its value. An answer of the wrong type is refused; a cancel or an app exit stops ssh rather than letting it send an empty password. The prompt class is only a hint: text marked as the server's never becomes a passphrase or host-key dialog. | `crates/remote/src/askpass/` (`mod.rs` `classify`, `server.rs`, `client.rs`) | `askpass/mod.rs` `hmac_matches_openssl`, `secrets_do_not_print`, `answers_of_the_wrong_type_are_refused`, `prompts_are_classified`; `fake_ssh.rs` `askpass_round_trip`, `cancel_sends_no_empty_credential`, `quitting_mid_prompt_sends_nothing`, `a_failed_handshake_fails_the_call`; fuzz `remote_askpass` | J | In place |
| T30 | Agent or X11 forwarding exposes the laptop to the remote | `ForwardAgent=no`, `ForwardX11=no`, `PermitLocalCommand=no`, `ClearAllForwardings=yes`, `RemoteCommand=none`. | `crates/remote/src/ssh.rs` (`Ssh::args`) | `fake_ssh.rs` `exact_argument_list` | J | In place |
| T31 | Another local user hijacks a ControlMaster socket | Control sockets live in a private 0700 directory (`$XDG_RUNTIME_DIR/pitcrew-ssh`, then `/tmp/pitcrew-ssh-<uid>`, then `~/.pitcrew/s`), checked before use; a squatted directory falls back to the next; names ssh would expand are refused. | `crates/remote/src/ssh.rs` (`control_path`), `crates/remote/src/private.rs` (`pick_runtime_dir`, `ensure_private_dir`, `check_private_dir`) | `ssh.rs` `control_paths_are_checked`; `private.rs` `creates_0700_and_refuses_open_dirs`, `a_squatted_dir_falls_back`, `names_ssh_would_expand_are_refused_for_control_path`, `lengths_are_checked_for_the_sockets` | J | In place |
| T32 | A malicious remote answers probes, `ssh -G` or SLURM commands with hostile output | Reports are delimited by markers carrying a random tag for each call; a repeated key is an error; probe output is capped (1 MiB, 30 s) and `ssh -G` too (1 MiB, 10 s); error lines lose control characters and server text after ssh's own terminal line is ignored. The ssh config listing follows at most 16 levels and 256 files of `Include`, skipping cycles. Everything a remote reports is untrusted text for the UI (T17) and the hub (T22). | `crates/remote/src/probe.rs` (`parse`), `crates/remote/src/report.rs`, `ssh.rs` (`parse_resolved`, `classify_failure`), `config.rs` (`list_hosts_in`) | `probe.rs` `markers_from_another_call_do_not_count`, `a_repeated_key_is_an_error`; `ssh.rs` `server_text_cannot_steer_the_error`, `error_lines_lose_control_characters`; `config.rs` `include_cycles_are_skipped`, `fan_out_is_bounded`; `fake_ssh.rs` `probe_is_bounded`; fuzz `remote_probe`, `remote_ssh_config` | J | In place |
| T33 | A tampered helper runs on the remote | The desktop uploads the helper itself and checks its sha256 (`Helper::new` refuses bytes that do not match, or over 256 MiB); the helper script uploads under `umask 077`, checks size, sha256 and `--version`, switches atomically and keeps two versions. Nothing is downloaded or built on the remote; no root is needed. | `crates/remote/src/helper/deploy.rs` (`Helper::new`, `deploy`), `crates/remote/src/helper/helper.sh` | `deploy.rs` `helpers_are_checked_before_sending`, `versions_are_checked`; `crates/remote/tests/deploy.rs` `a_hash_mismatch_removes_the_upload`, `the_upload_is_never_readable_by_others`, `an_interrupted_upload_leaves_nothing_in_place`, `the_way_to_the_root_is_checked` | J, P, K | Partial: the expected sha256 compiled into the desktop is K's (not built). |

### 5.8 Supply chain, build and release (B9)

| Id | Threat | Control | Where | Tested by | Owner | Status |
|---|---|---|---|---|---|---|
| T34 | A compromised or vulnerable dependency | `cargo-deny` in CI: licences, RustSec advisories, yanked crates, crates.io only, no git sources; lockfiles and `--locked`; pnpm waits a day before installing a new version (`minimumReleaseAge`), runs install scripts only for listed packages, and saves exact versions; Dependabot, grouped weekly. | `deny.toml`, `.github/workflows/ci.yml` (`deny` job), `pnpm-workspace.yaml`, `.npmrc`, `.github/dependabot.yml` | CI | 0 | In place. The fuzz workspace is outside the root check: `fuzz/deny.toml` applies the same policy plus an NCSA exception for `libfuzzer-sys` (never shipped), and the nightly fuzz workflow runs it. |
| T35 | A compromised workflow or Action | Actions pinned by commit SHA; `permissions: contents: read`; `persist-credentials: false`; on pull requests the guard scripts come from the base branch. | `.github/workflows/ci.yml`, `.github/workflows/fuzz.yml`, `scripts/ci/` | CI | 0 | In place; `zizmor` proposed (O21) |
| T36 | A tampered installer or update | Release builds on `v*` tags with `permissions: {}` and pinned actions; every artefact is re-verified against `SHA256SUMS`; a CycloneDX SBOM and build-provenance attestations per release. Code signing on all three OSes, macOS notarisation and a signed Tauri updater are still to come. | `.github/workflows/release.yml`, `packaging/` (`sbom.sh`, `sha256sums.sh`, `verify.sh`, `sign.sh`) | `packaging/test.sh` in the release workflow | P, K | Partial: `packaging/sign.sh` is a placeholder that skips without secrets; no Tauri bundle or updater signing yet. |
| T37 | Private data lands in the public repository | A scrub gate with hashed words and secret patterns, on files and commit metadata; fixtures are synthetic; `private/` folders are ignored. | `scripts/ci/scrub-gate.mjs`, `.github/scrub/hashes.txt`, `.gitignore` | `scripts/ci/test/` | 0 | In place |

### 5.9 Availability

| Id | Threat | Control | Where | Tested by | Owner | Status |
|---|---|---|---|---|---|---|
| T38 | A slow or stuck stream client exhausts the daemon's memory | A bounded queue per client (frames × page), a send timeout, then the client is dropped and resumes with `since`. | `crates/api/src/stream.rs` (`pump`, `StreamConfig`) | `stream.rs` tests | H | In place |
| T39 | Losing contact with a machine kills or forgets its sessions | Machines and sessions become `unverifiable` or `unreachable` and keep their last known state (ADR-0009). | `crates/protocol/src/model.rs` (`Liveness`, `SessionState`); runner and remote (not built) | | D, J | Planned |

### 5.10 Desktop gateway (B1, B2)

The contract is `docs/build/contracts/desktop-gateway.md`. On `main`, `apps/desktop` is still a
stub; the gateway is on branch `s/K/shell-and-gateway` (in progress). The UI's side is on `main`
(`apps/ui/src/data/gateway.ts`, `transport.ts`, `desktop.tsx`). Paths in this table are under
`apps/desktop/src-tauri/` on the K branch unless they say otherwise.

| Id | Threat | Control | Where | Tested by | Owner | Status |
|---|---|---|---|---|---|---|
| T40 | **The webview obtains a token** (XSS, a hostile page) | The gateway holds it. `DeviceToken` has no `Display` or `Serialize` and a redacted `Debug`. It is read from the daemon's token file on every connection (`read_token_file`; on Unix `O_NOFOLLOW`, then owner and mode checked), only after the endpoint check (T44), and goes only into the outgoing request as a sensitive header. The handshake crates' logs are silenced. In the desktop the UI reads no token and no config. | `src/token.rs`, `src/gateway/http.rs` (`send`), `src/gateway/socket.rs` (`open`), `src/logging.rs` (`SILENCED`); on `main` `apps/ui/src/data/gateway.ts` | `token.rs` `tokens_are_checked_and_hidden`, `a_bad_token_file_is_refused_without_echoing_it`, `open_or_linked_token_files_are_refused` (Unix); `logging.rs` `handshake_crates_are_silenced`; on `main` `apps/ui/tests/desktop-no-token.test.tsx` | K, L | Planned (the UI's side In place). Open: the contract's end-to-end "no token reaches the webview" test (O23); on Windows the token file gets no owner or ACL check (O2). |
| T41 | The webview reaches routes it should not through the gateway (traversal, odd methods, sockets to anything) | Request paths must start `/v1/`, be printable ASCII up to 8 KiB, and hold no `\`, `#`, control character, `//` or `.`/`..` segment (`%2e` decoded). Methods are GET, POST, PATCH, PUT and DELETE. Sockets go only to `/v1/stream` and `/v1/sessions/{id}/terminal` with a plain id. | `src/gateway/path.rs` (`check_request_path`, `check_socket_path`), `src/gateway/mod.rs` (`parse_method`) | `path.rs` `request_paths`, `socket_paths`, `send_limits_are_api_v1s`; `mod.rs` `methods` | K | Planned. Residual: `%2F` and double encoding (`%252e`) pass the gateway; the daemon decodes path parameters itself, so a route that takes a path must canonicalise it (T25, O24). |
| T42 | The daemon floods the webview, or the webview floods the daemon | Request bodies 1 MiB, responses 32 MiB, 120 s per request. Each socket may hold 8 MiB the webview has not acknowledged (`Budget`), then it closes with 1013. A daemon frame over 8 MiB closes it with 1009, and so does a webview message over 4 KiB (stream) or 1 MiB (terminal). | `src/gateway/mod.rs` (`Limits`), `src/gateway/socket.rs` (`Budget`, `SocketKind::send_limit`), `src/commands.rs` (`ChannelSink::probe`) | `socket.rs` `the_budget_counts_what_the_webview_has_not_taken`, `upgrade_errors_map_to_the_contract` | K | Planned. The brief's tests against a fake daemon (order, 1006, 1009, 1013, pings) are not written (O23). |
| T43 | Injected script calls the gateway's commands, or takes the window elsewhere | CSP `default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self'; font-src 'self'; connect-src ipc: http://ipc.localhost; object-src 'none'; base-uri 'none'; form-action 'none'; frame-src 'none'; frame-ancestors 'none'; worker-src 'none'; media-src 'none'; manifest-src 'none'`. `withGlobalTauri: false`, `freezePrototype: true`. One capability: the five gateway commands, and event listen and unlisten; no shell, fs, http or opener plugin. Navigation only to the app's own origin; new windows denied; downloads refused; devtools only in debug builds. | `tauri.conf.json`, `capabilities/main.json`, `src/app.rs` (`main_window`), `build.rs` | `app.rs` `only_the_apps_own_origin_is_allowed` | K, L | Planned. Open: `core:event:allow-listen` is not scoped to the gateway's events (O25); a CSP check on the built app (O11). |
| T44 | The gateway hands the token to a squatted socket or pipe | Before the token is read: on Unix `check_unix_socket`, then `check_unix_peer`; on Windows the pipe is opened with `SECURITY_IDENTIFICATION` and checked with `check_pipe_server` (H's checks in `crates/api/src/client.rs`). | `src/daemon/endpoint.rs` (`connect_unix`, `connect_pipe`), `src/daemon/mod.rs` (`LocalConnector::connect`) | `endpoint.rs` `our_own_socket_connects`, `an_open_directory_is_untrusted` (Unix) | K, H | Planned: closes O1 for the desktop when merged. |
| T45 | The desktop starts a planted `pitcrewd` | `locate` tries the configured path, then the folder of the desktop's own executable, then `PATH` without relative entries. | `src/daemon/locate.rs` | `locate.rs` `configured_then_beside_then_path`, `relative_path_entries_are_skipped` | K | Planned. Residual: an absolute `PATH` folder that others can write to is still searched. |

### 5.11 Terminal WebSocket (B1, B2, B5)

`GET /v1/sessions/{id}/terminal` carries keystrokes in and terminal output out. Output is pane
content, so it is attacker-controlled (U2).

| Id | Threat | Control | Where | Tested by | Owner | Status |
|---|---|---|---|---|---|---|
| T46 | A client sends hostile keystrokes or control messages | A device route. Binary frames are keystrokes, written as they are. Text frames must be `{"type":"resize"}` with sizes 1 to 1000, else 1007; other types are ignored. A message or frame over `max_inbound` (1 MiB) closes with 1009. Input waits in a queue of 16 messages, then the socket is not read. Every call into the runtime is bounded (64 at once, 5 s each). | `crates/api/src/terminal.rs` (`routes`, `control`, `session_loop`, `Calls`, `TerminalConfig`) | `crates/api/tests/terminal.rs` `input_resize_unknown_and_malformed_control`, `a_message_over_max_inbound_closes_with_1009`, `sizes_are_1_to_1000_on_the_query_and_in_resize`, `a_stalled_runtime_answers_503_or_closes_with_1011`; fuzz `api_terminal` | H | In place. The daemon serves the route through `NoRunner` (`crates/daemon/src/no_runner.rs`): no live terminal until stream D. |
| T47 | **Program output writes the clipboard (OSC 52), sets titles, or plants links** | xterm.js with `allowProposedApi: false`. OSC 0, 1, 2 and 52 are swallowed (`SWALLOWED_OSC`). No clipboard and no web-links addon. Links open only for absolute `http(s)` URLs of at most 2,048 characters (`MAX_LINK_LENGTH`), on a modifier-click, through the console's opener. Only the person's own selection is copied. | `apps/ui/src/console/terminal/options.ts` (`terminalOptions`, `linkHandler`, `SWALLOWED_OSC`, `MAX_LINK_LENGTH`), `controller.ts` (`TerminalController`) | `apps/ui/src/console/tests/terminal-view.test.tsx` "starts xterm with options that keep hostile output in its place", "opens only http and https links, on a modifier-click, through the console opener"; `terminal-options.test.ts` "only absolute http and https URLs"; Playwright `tests/e2e/terminal.spec.ts` "program output cannot set the title or the clipboard" | M | In place. Residual: bracketed paste is xterm's default; a paste is capped at 64 KiB. |
| T48 | A client floods keystrokes, or a slow client holds the daemon's memory | Client: at most 64 KiB per send, buffered, or queued while disconnected (`INPUT_LIMIT`); output flow control with high and low water marks. Server: T46's limits; 64 frames queued per client, then 1013. | `apps/ui/src/console/terminal/socket.ts` (`INPUT_LIMIT`), `controller.ts`; `crates/api/src/terminal.rs` (`queue_frames`, `send_timeout`) | `apps/ui/src/console/tests/terminal-socket.test.ts` "while disconnected, refuses more than 64 KiB at once"; `crates/api/tests/terminal.rs` `a_client_that_stops_reading_is_closed_with_1013` | M, H | In place. Residual: up to 16 MiB of queued input per client at the server's defaults. |

### 5.12 The back office, `@office` (B12)

| Id | Threat | Control | Where | Tested by | Owner | Status |
|---|---|---|---|---|---|---|
| T49 | **The back office oversteps**: sends something outward, marks work done, answers a person, or acts on a stale view | It is an agent member (`Config::office`) acting in-process, with **no token**; its events are authored by it on behalf of its owner. Its own guard allows only `TaskMoved`, `AskAnswered` and comments on a task or workstream; needs receipts; never raises an approval; moves a task only from its current status as `can_move` allows, and to done only with the task's automatic acceptance; answers only open questions to agents. **The hub checks every action again**: the event kinds, `can_move` from the current status, answers only to the office's own questions and mentions, known targets, receipts, no approvals. | `crates/office/src/guard.rs` (`check_shape`, `check`); `crates/hub-work/src/office.rs` (`BackOffice`, `OfficeCommands`, `WorkService::office_commands`, `evidence`) | `crates/office/tests/never.rs` (`never_asks_to_send_anything_outward`, `never_marks_done_without_automatic_acceptance`, `never_answers_a_person`, `apply_checks_again`, `crafted_logs_never_break_the_rules`); `crates/hub-work/tests/office.rs` (`nothing_bypasses_can_move`, `actions_the_hub_refuses_are_refused_and_logged_as_refused`, `the_office_answers_only_its_own_questions_and_mentions`, `the_office_runs_only_as_an_agent_with_its_run_log`) | E, F | In place. `pitcrewd serve` runs the office unless `--no-office`, as the workspace's agent `@office` (`crates/daemon/src/office.rs`, `member`), with no token minted for it; `crates/daemon/tests/office.rs` `an_action_the_hub_refuses_is_logged_and_appends_nothing`. |
| T50 | A crash or a replay applies an office action twice, or the office floods the log | The office ignores revisions it has seen and uses event time, never the clock. The run log (`RunLog`, projection `office.runs`) records actions inside each append's transaction. Each event id is derived from the log, revision and entry (`Origin`); an action whose first event is already in the log is a replay and appends nothing (`in_log`). Caps: 20 actions per rule and 60 in all per hour of event time, 64 per rule per event. | `crates/office/src/office.rs` (`Office::on_event`), `caps.rs`, `runlog.rs`; `crates/hub-work/src/office.rs` (`Origin`, `in_log`, `WorkService::run_office`) | `crates/office/tests/props.rs` (`replay_is_deterministic_and_idempotent`, `caps_hold_under_a_flood`); `crates/office/tests/run_log.rs`; `crates/hub-work/tests/office.rs` `re_running_a_range_after_a_crash_applies_each_action_once`; `crates/daemon/tests/office.rs` `a_restart_does_not_duplicate_office_actions` | E, F, 0 | In place. Residual: the caps live only in the office; the hub does not check them again (O26). A refused action still shows as `emitted` in `office_runs` (hub-work README). |

### 5.13 SLURM jobs (B11)

All on branch `s/J/slurm` (not merged), under `crates/remote/src/helper/`. The `squeue` and
`sacct` lines are read by the shell helper (`helper.sh`), not by Rust.

| Id | Threat | Control | Where | Tested by | Owner | Status |
|---|---|---|---|---|---|---|
| T51 | **The job script changes** between the desktop and `sbatch` (another user, a race, a cut upload) | `JobSpec::render` makes the only `JobScript`. The launcher sends its length, sha256 and job name as arguments and the text on stdin. `pc_slurm_submit` writes it under the private root (`umask 077`, noclobber) while holding the launch lock, checks the length, the first and last marker lines and the sha256 before `sbatch`, unsets `SBATCH_*`, and records `run/slurm.json` atomically. Extra `#SBATCH` lines must pass an allowlist (`check_sbatch_option`: long options, plain values; `--job-name`, `--chdir`, `--output`, `--wrap`, `--uid` and more refused). On the node, `job.sh` starts only if `run/slurm.json` names its own `SLURM_JOB_ID`. | `slurm/spec.rs` (`JobSpec`, `JobScript::sha256`, `check_sbatch_option`), `slurm/mod.rs` (`SlurmLauncher`), `helper.sh` (`pc_slurm_submit`), `slurm/job.sh` | `tests/deploy.rs` `slurm_scripts_arrive_whole_or_not_at_all`, `slurm_jobs_check_where_they_run`; `spec.rs` `options_are_checked` | J | Planned |
| T52 | **PitCrew cancels or reads someone else's job** | A job is ours only if `squeue` reports our uid and the job name we recorded (`pc_queue`). `scancel` runs only for ours, by id, right after that check (`pc_slurm_stop`). `sacct` output is filtered by id, uid and name (`pc_acct`). | `helper.sh` (`pc_queue`, `pc_slurm_stop`, `pc_acct`) | `tests/deploy.rs` `slurm_never_touches_other_jobs` | J | Planned. Residual: a job id reused between the check and `scancel`. |
| T53 | Another user on the compute node reaches the helper's socket | The socket is in `<root>/run/` or, node-local, in `${TMPDIR:-/tmp}/pitcrew-<job>.<pid>/`, created 0700 (`pc_private`) after `pc_safe_way`; paths over 100 bytes are refused. The daemon still checks every peer (T1). | `slurm/job.sh` | `tests/deploy.rs` `slurm_socket_on_node_local_tmpdir` | J | Planned. How the desktop reaches the node (a tunnel, `srun --overlap`) is not built. |
| T54 | **A site recipe runs code on the cluster** | Recipes are trusted input: the built-in `generic()`, or the user's own `~/.pitcrew/sites/*.toml` on the laptop, read strictly (unknown keys and wrong types refused, 64 KiB at most, values checked by `Site::check`). `modules_init` is a path that `job.sh` sources, so a recipe can run anything as the user on the cluster, by design. | `slurm/site.rs` (`Site::from_toml`, `load_sites`) | `site.rs` `recipes_are_read_strictly`, `the_example_recipe_reads` | J | Planned. A recipe copied from elsewhere is code: the docs and the UI must say so (O27). |

### 5.14 GitHub and Jira reads (B10)

| Id | Threat | Control | Where | Tested by | Owner | Status |
|---|---|---|---|---|---|---|
| T55 | Upstream text is hostile: huge, deeply nested, or crafted for the UI | Caps: 20 pages and 2,000 items per call, 5 MiB per page, titles of 512 and bodies of 65,536 characters, 50 labels of 100. Jira: 1,000 items, documents at most 32 deep and 20,000 nodes. All of it is untrusted text for the UI (T17). | `crates/sync-github/src/bounds.rs`, `change.rs`; branch `s/G/jira-read` `crates/sync-jira/src/adf.rs` | `crates/sync-github/tests/bounds_and_skips.rs`; fuzz `github_sync` | G | In place for GitHub; Jira Planned. Open: control and direction-changing characters are kept, and `html_url` is kept as sent, in any scheme (R10, O30). |
| T56 | **A tracker credential leaks**: to another host through a pagination link, or into logs | `AuthToken` and `JiraAuth` never print (redacted `Debug`, no `Display`); `Request`'s `Debug` redacts `Authorization`; errors carry URLs, not headers. GitHub follows `Link: rel="next"` only when `is_trusted_next_url` accepts it (same scheme, host and port; path under the base), and reports one it ignores. Jira never follows a URL from the server. | `crates/sync-github/src/transport.rs`, `origin.rs` (`is_trusted_next_url`), `client.rs` (`GithubClient::list`); branch G `crates/sync-jira/src/auth.rs` | `transport.rs` `auth_token_debug_never_shows_the_value`, `request_debug_redacts_authorization_only`; `tests/pagination_link_safety.rs` `an_untrusted_link_header_is_never_followed_and_is_reported`; fuzz `github_sync` | G | Partial: the origin check and a WHATWG URL parser disagree (R8, R9, O29). No HTTPS transport exists yet; the one chosen must resolve URLs the way the check does. |
| T57 | JQL injection through a project key or the stored cursor | `ProjectRef::new` refuses anything but `[A-Z][A-Z0-9]{1,9}`; the query is percent-encoded. | branch G `crates/sync-jira/src/jql.rs` (`incremental_query`, `ProjectRef`) | `a_key_with_quotes_is_rejected`, `a_key_with_jql_operators_is_rejected`, proptest `no_accepted_key_ever_contains_a_quote` | G | Planned. Open: the cursor read back from stored state goes into the JQL unchecked (O28). |
| T58 | Rate limits are ignored (the account is blocked) or misread (a sync stops for good) | GitHub: a 403 or 429 is a rate limit only with `x-ratelimit-remaining: 0` and a reset time, a `retry-after`, or a secondary-limit message; the backoff starts at 2 s and doubles to 300 s, kept in state; any other 403 is an error. Jira: a 429 with `Retry-After`, or the same backoff. | `crates/sync-github/src/client.rs`, `bounds.rs` (`backoff_secs`); branch G `crates/sync-jira` | `crates/sync-github/tests/rate_limits.rs` | G | In place for GitHub; Jira Planned. On `main` a 403 whose body merely contains "rate limit" counts as one; branch G reads only the JSON `message`. |

### 5.15 Store leases on network filesystems (B7)

| Id | Threat | Control | Where | Tested by | Owner | Status |
|---|---|---|---|---|---|---|
| T59 | Two hosts write one SQLite file on NFS and corrupt the log | On a network or unknown filesystem the store uses `journal_mode=DELETE`, `locking_mode=EXCLUSIVE` and a single-host lease: `<db>.lease.<gen>` files created exclusively with a hard link (0600 on Unix), renewed every third of the ttl and checked again before every append, rebuild and import (`Error::LeaseLost`). A live lease refuses the open (`Error::Leased`). | `crates/store/src/lease.rs` (`LeaseGuard::acquire`, `LeaseGuard::check`), `crates/store/src/fs_kind.rs` (`detect`) | `crates/store/tests/network_lease.rs` (`a_second_open_in_network_mode_fails_with_leased`, `a_taken_over_lease_fails_the_next_append_immediately`, `three_threads_racing_an_expired_lease_never_give_two_holders`); `crates/store/tests/fs_mode.rs` | C | In place. **Residual risk** (C's README, "Residual limits"): NFS attribute caching can hide a newer generation from the old owner for a while, and "expired" is each host's own clock; re-listing before each write and a ttl with margin only narrow both. A filesystem without hard links cannot take the lease at all. On Windows the lease file inherits the folder's ACL, and a dead process is noticed only by expiry. |

## 6. Findings from reviews

What reviews of merged or in-review branches found, and where each stands. R7 to R10 and R24 come
from this round's fuzzing and review (stream Q); R11 to R23 are the findings the merged briefs,
their review follow-ups and the crate READMEs record.

| Id | Finding | Fix | Where | Tested by | Owner | Status |
|---|---|---|---|---|---|---|
| R1 | **Nested-router authentication bypass.** Authentication and the device-only guard were applied with `route_layer`, which does not wrap the fallback of a nested router. A nested router with its own fallback (a file server, for instance) was reachable with no token, or with an agent token on a device-only route. | Both are applied with `layer`, which covers nested fallbacks. `router()` documents that nothing may be merged into the returned router. An empty `RouterParts` no longer panics. | `crates/api/src/lib.rs` (`router`), `crates/auth/src/http.rs` (`device_only`, `require_device`) | `crates/api/tests/auth.rs` `nested_fallbacks_are_authenticated_and_scoped`, `an_empty_router_parts_serves_host_info`; fuzz `api_request` (the app nests a device router with a fallback) | H | Fixed on `main` |
| R2 | **tmux format and `%exit` forging.** A version of the parser that ended an open reply on `%exit` let pane content (a title, a window name) inject notifications, including `%output` for another pane. Names could hold control characters that split lines, and `#{…}`/`#(…)` formats. | Reply bodies stay opaque until their own guard (as on `main`); names refuse C0 and DEL; format literals are escaped. | `crates/runtime/src/control.rs`; branch `s/B/control-hardening` (`command.rs`) | `crates/runtime/tests/control.rs`; fuzz `tmux_control` property 2, which fails if a body line escapes its reply | B | `main` is not affected. Branch B (`e77c7a0`, fixes requested) keeps `%exit` as body text. Merge pending (O15). |
| R3 | **Token in the build.** `VITE_PITCREW_TOKEN` from a developer's `.env.local` was inlined by `vite build`, in production and in other modes. | Read only in development; the build fails when it is set; only the needed keys reach the bundle. | `apps/ui/src/data/config.ts`, `apps/ui/vite.config.ts` | `apps/ui/tests/config.test.ts` | L | Fixed on `main` (`cdcdea2`). A CI check of `dist` is still open (O4). |
| R4 | **Unbounded parser inputs.** Uncapped session facts plus a clone-and-compare of all facts on every line let a small crafted file hang `read_from`; `read_page` kept every turn-duration record; several item payloads were uncapped; the tmux parser had no line or reply limit. | Ingest: facts capped or dropped, `set_first`/`set_latest` instead of cloning, at most `limit + 1` records per page, every payload capped. tmux: `ParserLimits` with a latched desync error (branch B). | `crates/ingest/src/bound.rs`, `jsonl.rs`, `claude/mod.rs`; branch `s/B/control-hardening` (`control.rs`) | ingest tests above; fuzz targets in §8 | A, B | Ingest fixed on `main`; tmux Planned (O15) |
| R5 | **Socket and pipe squatting.** Directories were made private after creation, so a shared group could plant a socket or a registry in the window; existing files and sockets were trusted without an owner check; the pipe was named after the user name; clients had no way to check who served a pipe; the loopback check for development TCP could be skipped. | See T2, T3 and T4: owner checks before use, `O_NOFOLLOW`, a SID-named pipe, client checks, an opaque `Bound`. | `crates/auth/src/private.rs`, `crates/api/src/listener/`, `crates/api/src/client.rs`, `crates/cli/src/transport.rs` | the tests under T2–T4 | H, I | Fixed on `main`; the CLI uses the checks; the desktop does on branch K (O1) |
| R6 | An agent with shell access runs as the same OS user and could write a device-scope hash into `tokens.json`. | Accept and document: scopes protect against agents confined to the API, not against code running as the user. | ADR-0006 | | 0, Q | Open (O22) |
| R7 | **An import is not whole or nothing.** `Store::import` appends in batches of 1,000 lines, each committed, so a bad line (undecodable, or not UTF-8) after the first batch fails the import and leaves the earlier batches in the store, which then refuses a second import (`NotEmpty`). Its own doc comment says so; the store's contract for an import says whole or nothing. | Decode and check every line before appending, or append all batches in one transaction. | `crates/store/src/store.rs` (`Store::import`) | fuzz `store_import` (finding; reproduction in §8.1) | C | Open (O31) |
| R8 | **A `next` link can send the GitHub token to another host.** `origin::parse` takes the host from after the last `@` before the first `/`, `?` or `#`. A WHATWG parser (the `url` crate, which reqwest and ureq use) also ends the authority at `\` for `http(s)`. `https://attacker.example\@api.github.com/x` passes `is_trusted_next_url` for the base `https://api.github.com`, but such a client sends it, with `Authorization`, to `attacker.example`. Latent: there is no HTTPS transport yet. | Parse with the same parser the transport uses and compare its result, and refuse `\`, userinfo and anything but the query changing; or follow `next` only when it equals the current URL except for the query. | `crates/sync-github/src/origin.rs` (`parse`, `is_trusted_next_url`) | fuzz `github_sync` (finding; reproduction in §8.1) | G | Open (O29) |
| R9 | **A `next` link can leave the API base path.** `path_is_under` compares raw segments, so `/api/v3/../../x`, `/api/v3/%2e%2e/%2e%2e/x` and `/api/v3/..\..\x` are "under" `/api/v3`, while a WHATWG parser (and many servers and proxies) resolve them to `/x`: the token goes to another path on the same host, outside the API (another application behind the same proxy). Branch `s/G/jira-read` (`def2ce7`) resolves `.` and `..` but not `%2e` or `\`. | As R8: compare the transport parser's normalised path, or refuse `%2e`, `\` and dot segments outright. | `crates/sync-github/src/origin.rs` (`path_is_under`) | fuzz `github_sync` (finding; reproduction in §8.1) | G | Open (O29) |
| R10 | **Upstream links are kept as sent.** `html_url` from the server becomes `ExternalRef.url` unchanged, in any scheme (`javascript:`, `data:`) and of any length, and titles and bodies keep control and direction-changing characters. The UI must not trust them (T17). | Keep only `https` URLs on the expected host, capped; clean text as the recap's `clean` does. | `crates/sync-github/src/change.rs` (`issue_ref`, `pull_ref`, `milestone_ref`) | (review; a reproduction in §8.1) | G | Open (O30) |
| R11 | `TokenHash::from_hex` accepted a leading `+`; the pipe server was identified by its process id, which can be recycled. | Hex digits only; the pipe's owner SID (or the token's default owner) is compared instead. | `crates/auth/src/token.rs` (`TokenHash::from_hex`), `crates/api/src/client.rs` (`check_pipe_server`), `crates/api/src/listener/pipe_security.rs` | `token.rs` `hex_hashes_are_hex_digits_only`; `pipe_security.rs` `a_pipe_with_default_security_has_the_token_owner_and_passes` | H | Fixed on `main`. Residual: an elevated administrator can plant a pipe that passes. |
| R12 | Activity filters walked event bodies without a depth limit and matched ids as substrings; a panic in the route sent its message (SQL text, paths) to the client. | Key-only matching to depth 32; a generic message. | `crates/api/src/activity.rs` (`KeyMatch`, `MAX_DEPTH`) | `activity.rs` `the_walk_stops_at_max_depth`, `filters_match_by_key_not_by_substring`; `crates/api/tests/activity.rs` `an_index_that_fails_or_panics_is_a_500_without_its_detail`; fuzz `api_activity` | H | Fixed on `main` |
| R13 | Terminal WebSocket: a plain GET resized the terminal before the upgrade check; sizes were unbounded; a stalled runtime piled up threads; no keepalive; the hook sink could block an async worker or die on a panic. | Sizes 1 to 1000; the upgrade checked first; bounded calls; pings with a deadline that waits while input backs up; the sink on the blocking pool. | `crates/api/src/terminal.rs`, `crates/api/src/hooks.rs` | `crates/api/tests/terminal.rs` `a_get_without_an_upgrade_never_resizes`, `keepalive_pings_and_closes_a_client_that_stops_answering`; `crates/api/tests/hooks.rs` `a_blocking_or_panicking_sink_never_stalls_requests` | H | Fixed on `main` |
| R14 | CLI: `PITCREW_PIPE=\\.\pipe\..\UNC\host\share\x` reached SMB and could leak credentials; the HTTP client overflowed on a huge chunk size and read unbounded heads and trailers; the token showed in `Debug`; `task plan` worked with a device token; `.`/`..` task references; control, bidi and invisible characters reached the terminal. | Plain local pipe names only; one cap over everything read; redacted `Debug`; agent tokens only; references are keys or ids; output cleaned. | `crates/cli/src/config.rs` (`is_local_pipe_name`), `http.rs`, `client.rs`, `verbs.rs`, `display.rs` | `config.rs` `only_plain_local_pipe_names`; `http.rs` `a_huge_chunk_size_is_refused_without_overflow`; `crates/cli/tests/verbs.rs` `a_persons_token_is_refused_and_nothing_changes`; `display.rs` `escapes_and_bidi_controls_are_removed` | I | Fixed on `main` |
| R15 | Hook install, rounds 1 to 3: uninstall deleted the user's hooks; look-alike and multi-word commands were claimed as ours; the Codex chain could loop; the quote check looked only at the first and last byte; backups were not private; the original notifier was lost without a token. | Ownership per hook by program name; `is_single_shell_word`; private backups; `PITCREW_CHAINED`; the chain runs with no daemon. | `crates/cli/src/install/`, `crates/cli/src/hook.rs` (`run_chained`) | T14's tests; `crates/cli/tests/hook.rs` `runs_the_original_even_with_no_token_and_no_daemon_configured`; fuzz `cli_hooks` | I | Fixed on `main` |
| R16 | Work model: validation errors told forbidden agents more than 403; `on_behalf_of` was stamped for persons; SQLite text reached 500s; a key clash or stale move stalled projections; label checks were quadratic inside the command lock; a task could be dispatched twice. | 404, 403, 400, 409 in that order; `WorkService::by`; a fixed internal message; deterministic projections with recorded clashes; checks before the lock; 409 on a second dispatch. | `crates/hub-work/src/` (`service.rs`, `commands.rs`, `projection/`, `edits.rs`, `dispatch.rs`) | `crates/hub-work/tests/single_writer.rs` `errors_from_the_store_never_leak_to_clients`; `tests/edits.rs` `long_lists_are_checked_without_quadratic_work`; `tests/dispatch.rs` `an_agent_is_dispatched_on_a_task_once_at_a_time` | E | Fixed on `main` |
| R17 | The back office could answer asks addressed to others, re-runs were not idempotent, and `apply` checked only the shape. | Its own asks only; derived event ids with `append_new`; the hub checks every action again (T49, T50). | `crates/hub-work/src/office.rs`, `crates/office/src/commands.rs` | `crates/hub-work/tests/office.rs` `re_running_a_range_after_a_crash_applies_each_action_once`; `crates/office/tests/never.rs` `apply_checks_again` | E, F | Fixed on `main` |
| R18 | The NFS lease could let two hubs both write (rounds 1 and 2), and a rebuild did not check it. | Generation-numbered lease files created exclusively; every write checks the lease. | `crates/store/src/lease.rs`, `crates/store/src/store.rs` (`check_lease`) | `crates/store/tests/network_lease.rs` `a_taken_over_lease_fails_rebuild_immediately`, `three_threads_racing_an_expired_lease_never_give_two_holders` | C | Fixed on `main`; residual risk in T59 |
| R19 | The runner's state directory was not private; several runners could share it; a newer index was accepted. | 0700 directory, a lock, a version check. | `crates/runner/src/store.rs` | `store.rs` `the_state_directory_is_private`, `one_runner_per_state_directory`, `an_index_from_a_newer_runner_is_refused` | D | Fixed on `main`. Residual: an existing open directory only causes a warning. |
| R20 | The scan lost results on a panic and missed the home exclusion with other letter cases; a damaged OpenCode store was retried forever. | Panics caught per unit; case-folded comparison; a damaged store is skipped until it changes. | `crates/ingest/src/scan.rs`, `crates/ingest/src/opencode/` | `scan.rs` `a_panicking_unit_is_caught_and_counted_unreadable_not_lost`; `crates/ingest/tests/opencode.rs` `a_damaged_store_read_unlocked_is_unreadable_and_skipped` | A | Fixed on `main` |
| R21 | GitHub pagination followed `Link: next` to any host; any 403 was read as a rate limit, hiding a revoked token. | `is_trusted_next_url`; explicit rate-limit signals only. | `crates/sync-github/src/origin.rs`, `client.rs` | `tests/pagination_link_safety.rs`; `client.rs` `a_plain_403_with_no_rate_limit_signal_is_a_client_error` | G | Fixed on `main`; see R8 and R9 for what the check still misses. |
| R22 | Remote: POSIX quoting was unsafe under fish and csh login shells, and under xonsh; host characters reached `ProxyCommand %h`; a cancel or an app exit made ssh send empty passwords; server text in ssh's log steered error classification; the network-filesystem check was a denylist; `Include` cycles; deploy round 1 (a group-writable path to the root, `.bashrc` eating stdin, lost locks, look-alike tools). | The octal wrapper and the xonsh refusal; `validate_host`; group kill or a Job Object; stop at ssh's terminal line; an allowlist; cycle skip and caps; path, length and lock checks in the helper. | `crates/remote/src/` (`quote.rs`, `probe.rs`, `askpass/`, `ssh.rs`, `config.rs`, `helper/helper.sh`) | `crates/remote/tests/login_shells.rs`, `fake_ssh.rs`, `deploy.rs` (T16, T28–T33) | J | Fixed on `main`; Windows untested |
| R23 | The console's markdown parser was super-linear on hostile text, so a transcript could freeze the webview. | Budgeted matching and a depth cap. | `apps/ui/src/console/render/markdown-parse.ts` | `apps/ui/src/console/tests/markdown.test.ts` ("stays linear on …") | M | Fixed on `main` |
| R24 | Low. `pitcrew hooks install` on a Codex `config.toml` that starts with a UTF-8 BOM writes it back without the BOM, and `uninstall` does not restore it (Claude's installer keeps a BOM). The TOML means the same. | Split the BOM off and put it back, as `claude.rs` does (`split_bom`, `with_bom`). | `crates/cli/src/install/codex.rs` | fuzz `cli_hooks` (seed `codex-08`: flags `0x01`, then `﻿model = "bom"\n`) | I | Open (O34) |

## 7. Open items

| Id | What | Owner |
|---|---|---|
| O1 | The desktop gateway calls `check_unix_socket` + `check_unix_peer` (Unix) or `check_pipe_server` (Windows) before sending any token. The CLI does (its peer-uid check is Linux only). | K (branch, T44) |
| O2 | Windows: put the state directory under the user's profile and verify its owner, or set a protected DACL on it, before opening `tokens.json` or `device.token`; the gateway and CLI check the token file's owner. | H, K, I, 0 |
| O3 | Device tokens in the OS keychain; agent tokens minted and delivered to the agents PitCrew starts, through a 0600 file or the environment, never argv. | K, D, 0 |
| O4 | A CI check that fails if the built UI contains `pcd_`, `pca_` or a development token. (The token guard itself is merged.) | L, 0 |
| O5 | API routes and a Settings page to list, revoke and rotate tokens. | H, L |
| O6 | Closed: the work model stamps authors from the caller and limits agents to their own tasks and asks. Sessions are O9. | E |
| O7 | The audit log of person-only actions. | E, H |
| O8 | The Inbox as the only path for decisions, approvals and outward writes. (The back office's "never" list is built: T49.) | E, F, G |
| O9 | The runner's `HookSink` treats payloads as untrusted: a hook may only update sessions its caller owns. | D |
| O10 | An agent token can never start or dispatch a session with a stronger permission mode than policy allows; `bypass_permissions` needs the workspace opt-in, a warning and an audit entry. | D, E |
| O11 | A CSP check on the built desktop app and UI in CI. (Markdown without raw HTML and sanitised links are on `main`.) | K, L, 0 |
| O12 | Closed: the terminal swallows OSC 52 and titles and opens only `http(s)` links (T47). | M |
| O13 | Typed, borrowed per-line structs instead of a full `serde_json::Value` (memory amplification on long lines). | A |
| O14 | Closed: Claude discovery no longer follows links. | A |
| O15 | Merge the tmux hardening (limits, names, formats; `%exit` stays body text); then change the `tmux_control` target to `feed` returning `Result` (an error on a small input is a finding). | B, Q |
| O16 | The runner protocol reader caps a line (for example 16 MiB) before `decode_line`, and bounds `Events` batches and `TerminalOutput`. | D, J, 0 |
| O17 | Authenticate runners to hubs before a runner can attach over anything but the user's own transport. | 0, J |
| O18 | Closed: oversized messages close both WebSockets with 1009, with tests. | H |
| O19 | The runner's file API: roots, canonicalisation, symlink escapes, size caps, backups; a Q review before merge. | D |
| O20 | Closed: the nightly fuzz workflow runs, with `cargo deny` on `fuzz/deny.toml`. Its target list is O32. | 0 |
| O21 | CodeQL and `zizmor` for the workflows; `cargo-audit` (or keep `cargo-deny` advisories as the one source). | 0 |
| O22 | Record the residual risk of R6 in ADR-0006. | 0 |
| O23 | The gateway's tests the K brief and contract ask for: against a fake daemon (order, 1006, 1009, 1013, pings), "no token reaches the webview" end to end, cleanup on window close, and the supervisor. | K |
| O24 | The gateway also refuses `%2F` and double-encoded dots in a route, or the contract says every route that takes a path canonicalises it (the files API must, O19). | K, D |
| O25 | Scope the webview's `core:event:allow-listen` to the gateway's own events. | K |
| O26 | Decide whether the hub enforces the back office's caps too, or document that only the office does; log a hub-refused action as refused in `office_runs`. | E, F |
| O27 | Say in the docs and the UI that a site recipe is code that runs on the cluster, and show where each recipe came from. | J |
| O28 | Check the stored Jira cursor again (`JiraTimestamp`) before it goes into the JQL. | G |
| O29 | Fix R8 and R9 before any HTTPS transport is added: compare URLs with the transport's own parser, refuse `\`, userinfo, `%2e` and dot segments, or follow `next` only when it differs from the current URL in the query alone. Add the inputs in §8.1 as tests. | G |
| O30 | Keep only `https` links on the expected host from upstream, with a length cap, and remove control and direction-changing characters from upstream text before it is stored (R10). | G |
| O31 | Make `Store::import` whole or nothing (R7), with the reproduction in §8.1 as a test. | C |
| O32 | The nightly fuzz workflow lists its targets by hand: add the 11 new ones (or read them from `fuzz/Cargo.toml`) and fit the time budget (§8.3). | 0 |
| O33 | A test that a WebSocket upgrade from a foreign origin gets 403 on development TCP. | 0 |
| O34 | Keep a Codex config's leading BOM through install and uninstall (R24). | I |

## 8. Fuzzing

`fuzz/` is a `cargo-fuzz` project with its own workspace (the root excludes it). It needs a
nightly toolchain.

```sh
python3 fuzz/seed.py                 # seed fuzz/corpus/<target>/ from crates/fixtures and the crates' test data
cargo +nightly fuzz run adapter_read -- -dict=fuzz/dict/claude.dict -max_total_time=60
```

A crash leaves its input in `fuzz/artifacts/<target>/`; `cargo +nightly fuzz fmt <target> <file>`
prints it and `cargo +nightly fuzz tmin <target> <file>` minimises it. Turn it into a regression
test in the owning stream's crate.

- Run `cargo fuzz` from the repository (it sets `-artifact_prefix` to `fuzz/artifacts/`). A fuzz
  binary run directly writes `crash-*` files to its working directory unless given
  `-artifact_prefix=`.
- Under `strace` or `gdb`, pass `-detect_leaks=0`: LeakSanitizer cannot run under ptrace, and
  libFuzzer then records a "crash" of the empty input (`crash-da39a3ee…`) at exit. That artifact
  is not a finding; every target passes the empty input.
- Targets that need files (`adapter_*`, `opencode_store`, `remote_ssh_config`, `cli_hooks`,
  `store_import`) write them to a scratch folder: `PITCREW_FUZZ_TMP` if set, else `/dev/shm`
  (about twice as fast as a disk), else the temporary folder. `remote_ssh_config` skips inputs
  whose `Include` could reach outside its scratch home, so a run never reads the machine's files.
- Known findings fail their target until they are fixed. `PITCREW_FUZZ_SKIP_KNOWN=1` relaxes the
  checks for the open findings in §6 (R7, R8, R9), so a run can look past them.

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
| `remote_probe` | `remote::probe::parse` | a remote machine's probe output (B8) | a report counts only between this call's two markers; the facts agree (network filesystem, SLURM, tmux); printing the facts and parsing them again gives the same facts | `remote.dict` |
| `remote_ssh_config` | `remote::list_hosts_in` on up to five files with `Include` | the user's ssh config (B8) | every host is concrete and passes `validate_host`; no host twice; the same result twice | `remote.dict` |
| `remote_quote` | `remote::quote::sh_quote`, `posix_command`, `remote_command`, `validate_host` | argv for remote commands (B4, B8) | every word splits back exactly in a POSIX shell model with nothing expanded; the wrapper is read alike by POSIX, fish and csh login-shell models and decodes to the command line; accepted host names cannot be read as options | `remote.dict` |
| `remote_askpass` | `remote::askpass::classify` | ssh prompts, partly the server's (B8) | ssh's hints win; server text is only ever a password or a one-time code; host-key and passphrase classes need their words | `remote.dict` |
| `opencode_store` | `OpenCodeAdapter` `discover`, `read`, `read_page` on database bytes, or on row writes applied to OpenCode's schema | an OpenCode store (B7) | item caps; positions within `MAX_POSITION`, in order; a second read finds nothing new; pages join, make progress and together equal the full read | `opencode.dict` |
| `api_terminal` | the terminal WebSocket route over an in-memory connection, `max_inbound` 200 | a client of the socket (B1, B2) | keystrokes and resizes reach the terminal exactly and in order; a malformed control message closes with 1007, an oversized message with 1009, and nothing after it arrives | `api.dict` |
| `api_activity` | the activity route, with and without a fake index, over logs of up to 1,200 events | a client's query (B2) | 200 or 400 `invalid`, never 500; pages oldest first within `limit`; paging back ends and returns exactly the events that match every filter | `api.dict` |
| `github_sync` | `sync_github::sync::sync` (twice, the second from the first's state) against a scripted server; `link_header::next_link` | GitHub or a server in front of it (B10) | **no request leaves the API base** as a WHATWG parser resolves it; bounded requests; the token never in errors or the outcome; caps on titles, bodies and labels; state and changes round-trip | `github.dict` |
| `cli_hooks` | `pitcrew hooks install` and `uninstall` (`pitcrew_cli::run`) on Claude, Codex (with and without `--chain`) and OpenCode files | the person's own agent configs (B6) | a refusal changes nothing; nothing that isn't ours is dropped or changed; a second install changes nothing; uninstall leaves nothing of ours; install then uninstall gives the original bytes back | `cli.dict` |
| `store_import` | `Store::import`, then `export` | an export file (B7) | whole or nothing; what is imported exports back as the same events; the integrity check passes | `protocol.dict` |
| `recap_blocks` | `recap::blocks`, `BlockBuilder`, `draft_line`, `draft_paragraph`, `standing`, `propose_workstream` | event text from agents and transcripts (B6, B7) | batching never changes the blocks; byte-identical twice; caps; every fact keeps its receipts and every receipt is in the input; every summary and proposal verifies | `protocol.dict` |

The round-trip checks use `serde_json`'s `float_roundtrip` feature, so they cannot fail on the
last bit of a float. `fuzz/src/lib.rs` copies the ingest caps (they are crate-private); if the
caps change, change them there too. `fuzz/src/shell.rs` holds the shell models, written
independently of `crates/remote`'s own test-only ones.

**Last local run** (2026-10-01, `main` at `fe76853`, 60 s per new target, one job, WSL on a
shared 14-core machine with other builds running, the corpus copied to the Linux side; the eight
earlier targets were rebuilt but not run again):

| Target | exec/s | Result |
|---|---|---|
| `remote_probe` | 10,066 | no failure |
| `remote_ssh_config` | 1,919 | no failure |
| `remote_quote` | 2,949 | no failure |
| `remote_askpass` | 7,562 | no failure |
| `opencode_store` | 31 | no failure |
| `api_terminal` | 197 | no failure |
| `api_activity` | 81 | no failure |
| `github_sync` | 396 | no failure in 60 s; the R8 and R9 inputs below fail it |
| `cli_hooks` | 188 | a seed failed the round trip: Codex's installer drops a leading BOM (R24); the check now compares Codex files without it; then no failure |
| `store_import` | 7 | **R7** after 10 runs; with `PITCREW_FUZZ_SKIP_KNOWN=1`, no other failure |
| `recap_blocks` | 217 | no failure |

Reproductions are in `fuzz/regressions/<target>/` (run one with
`cargo +nightly fuzz run <target> fuzz/regressions/<target>/<file>`; each fails until its finding
is fixed):

- R7, `store_import/r7-bad-line-after-a-batch`: byte 125 (1,000 valid event lines), then
  `not an event`. The import fails and the store keeps 1,000 events.
- R8, `github_sync/r8-backslash-before-at`: base `https://api.github.com`, a first response with
  `Link: <https://attacker.example\@api.github.com/repos/example-org/demo-repo/milestones?page=2>; rel="next"`.
  The next request, with the token, goes to `attacker.example`.
- R9, `github_sync/r9-encoded-dot-segments`, `r9-backslash-dot-segments`,
  `r9-plain-dot-segments`: base `https://ghe.example.com/api/v3`, `Link` paths
  `/api/v3/%2e%2e/%2e%2e/admin`, `/api/v3/..\..\admin` and `/api/v3/../../admin`, all resolving
  to `/admin`. Branch `s/G/jira-read` (`def2ce7`) refuses only the third.
- R10 (no fuzz check yet): a milestone, issue or pull request with
  `"html_url":"javascript:alert(1)"` becomes `ExternalRef { url: Some("javascript:alert(1)") }`.

The seeds are 331 files, 300 KiB in all.

### 8.2 Limits of the current targets

- Inputs are small (libFuzzer's default maximum is about 4 KiB, or the largest seed), so the
  64 KiB carried-line and 16 MiB line thresholds are only reached by stream A's own tests;
  `store_import` reaches the 1,000-line import batches only through its repetition byte.
- `tmux_control` targets `feed` as it is on `main`; branch B changes it to return a `Result`
  (O15).
- `adapter_read`, `opencode_store`, `cli_hooks` and `store_import` are slow: each input writes
  files (and for the store, opens a database) before anything is checked.
- `opencode_store` instruments PitCrew's code, not SQLite's: mutated database bytes mostly fail
  SQLite's own checks, so the row-writing mode does most of the work.
- `cli_hooks` needs debug assertions (the default for `cargo fuzz build`): without them the CLI
  ignores the test override of its executable path, and the target checks nothing.
- `github_sync` uses the `url` crate as the model of an HTTPS client; a transport built on
  another parser (for example `http::Uri`) may resolve some URLs differently again.
- `api_terminal` runs one connection per input, so it finds protocol and ordering problems, not
  races between clients.
- Not fuzzed: the store's row decoding and the token registry parser (private), the command
  quoting in `crates/runtime/src/command.rs` against a tmux lexer model, `ssh -G` parsing
  (`parse_resolved`, private), the askpass wire messages (private), and the CLI's HTTP client
  (`crates/cli/src/http.rs`).

### 8.3 Nightly CI

`.github/workflows/fuzz.yml` (stream 0) runs every night: it seeds the corpora, runs each target
for 5 minutes with its dictionary, keeps the corpus in the Actions cache, uploads crash
artifacts, and runs `cargo deny` on the fuzz workspace. **It does not pick targets up from the
directory**: its matrix lists the first eight by hand, so the 11 new targets do not run until it
is changed (O32).

The budget with 19 targets: 19 jobs of 5 minutes of fuzzing each fit the jobs' 45-minute timeout
and GitHub's 20 concurrent jobs, but every job builds the whole fuzz workspace (about 8 minutes
cold here, 1.5 GB of target directory), and 19 per-target caches of that size exceed the
repository's 10 GB Actions cache, so most builds would be cold. Proposed for stream 0:

- one job builds every target once (`cargo +nightly fuzz build`) and uploads the binaries; the
  fuzz jobs download them instead of building;
- the matrix comes from `cargo fuzz list` (so new targets are picked up), with each target's
  dictionary in a small table, or a default per name prefix;
- the slow targets (`opencode_store`, `store_import`, `api_activity`) keep 5 minutes; the rest can
  share jobs (for example three targets of 5 minutes per job);
- the nightly run sets `PITCREW_FUZZ_SKIP_KNOWN=1`, so an open finding does not hide new ones,
  and a separate step runs `fuzz/regressions/` without it, as a reminder that does not fail the
  workflow.

### 8.4 Targets to add as streams land

- `crates/remote` SLURM (J, branch `s/J/slurm`): `Site::from_toml` on arbitrary recipe text
  (strict keys, 64 KiB cap; property: a recipe that loads passes `Site::check`),
  `parse_wall_time`, `JobExit::parse` (`sacct`'s `ExitCode`), and `check_sbatch_option`
  (property: an accepted option is a long option from the allowlist with a plain value). The
  `squeue` and `sacct` lines are parsed by `helper.sh`, not Rust; the Rust side reads the
  helper's report with `parse_status` (private: the seam to ask for is a `#[doc(hidden)] pub`
  function, or fuzz it through `report::parse`).
- `crates/sync-jira` (G, branch `s/G/jira-read`): `adf::adf_to_text` (depth and node caps),
  `ProjectRef::new` and `jql::incremental_query` (no accepted key or cursor breaks out of its
  quotes), and the search response pages, through a scripted transport as `github_sync` does.
- The desktop gateway (K, branch `s/K/shell-and-gateway`): `check_request_path` and
  `check_socket_path` (property: an accepted path, decoded as the daemon decodes it, stays under
  `/v1/` with no dot segment).
- `crates/runner` (D): the protocol line reader (with its cap), the file API's path checks, and
  hook payloads into the runner's session state.
- `crates/remote`: `ssh::parse_resolved` and the askpass wire messages, once there is a public
  seam for each.
- The recap wire types (`pitcrew_protocol::recap`, merged after `fe76853`) in `api_json`'s round
  trip, and the recap routes once stream H serves them.

## 9. Coverage of the security table

The plan's security table and the ADRs' security commitments (ADR-0003, ADR-0006, ADR-0009,
`SECURITY.md`), and where each is covered here.

| Commitment | Covered by |
|---|---|
| Other users on a shared machine: private socket or pipe, 0600 state, peer uid check | T1–T4, T44, R5, R11 |
| A stolen or leaked token: headers only, redacted logs, keychain, rotation | T6–T9, T40, T56 |
| Agents acting as you: scoped tokens enforced in the host, audit log for person-only actions | T10, T11, T49, R6, R16 |
| Prompt injection: authors stamped by the host, agent text never authorises, Inbox for irreversible actions | T12, T13, T49, T50 |
| XSS in the webview: CSP, no raw HTML, sanitised links, capabilities, sandboxed previews | T17–T19, T41–T43, T47, R10, R23 |
| Path traversal via the file API | T25, T27, T41 |
| Supply chain: `cargo-deny`, `cargo-audit`, pnpm hardening, Dependabot, SBOM, provenance, pinned Actions, CodeQL, `zizmor` | T34, T35, T36, O21 |
| A tampered update, installer or remote helper | T33, T36, T45 |
| SSH: host keys, no agent forwarding, askpass, nothing stored | T28–T32, R22 |
| Hooks altering agent configs | T14, R15 |
| Agents launched with too much power | T15 |
| Parser crashes on hostile input: fuzzing, capped lines and records | T20–T24, T32, T55, R4, §8 |
| The webview never holds a token (ADR-0003) | T8, T19, T40 |
| No listening TCP ports on shared machines (ADR-0006, `SECURITY.md`) | T1, T4 |
| Authors stamped by the daemon (ADR-0006, `SECURITY.md`) | T10, T49 |
| The remote helper needs no root and no internet, and is checksummed (ADR-0009, `SECURITY.md`) | T33 |
| Batch jobs: the script that runs is the one sent; other users' jobs are never touched (ADR-0009) | T51–T54 |
| The terminal: hostile output stays in the terminal; input is bounded | T46–T48, R13 |
| Trackers: credentials stay with their host; upstream text is untrusted; no write without the person | T55–T58, R8–R10, R21, O8 |
| The back office acts only within its rules, and the hub checks them | T49, T50, R17 |
| The event log stays whole: one writer on network filesystems, imports whole or not at all | T59, R7, R18 |
