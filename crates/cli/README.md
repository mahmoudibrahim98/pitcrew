# pitcrew-cli

`pitcrew`, the command agents run to see and report on their work, and `pitcrew hook`, which
runs on every agent turn.

**Owned by stream I** — see [docs/build/streams/I.md](../../docs/build/streams/I.md).

## Connecting

| Variable | Meaning |
|---|---|
| `PITCREW_SOCKET` | Unix: the daemon's socket (`…/pitcrewd.sock`) or its directory. |
| `PITCREW_PIPE` | Windows: the daemon's pipe, `\\.\pipe\<name>` with a name of letters, digits, `.`, `_` and `-` (not ending in `.`), so it cannot leave the pipe namespace. Default `\\.\pipe\pitcrewd-<your SID>`. |
| `PITCREW_URL` | Loopback TCP, **development only**: `http://127.0.0.1:<port>`, `http://[::1]:<port>` or `http://localhost:<port>`. Any other host is refused. |
| `PITCREW_TOKEN` | The agent token. |
| `PITCREW_TOKEN_FILE` | Or a file holding it. On Unix it must be ours, mode 0600 or stricter, and not a symlink. |

The token is sent only after the server passes an identity check (`pitcrew_api::client`):

- **Unix socket:** the directory is ours and 0700 and the socket is ours (before connecting),
  and on Linux the peer runs as us (`SO_PEERCRED`, after).
- **Named pipe:** opened at identification-level impersonation, and the pipe's owner SID is
  exactly the current user's. The daemon names the current user as its pipe's owner, elevated or
  not; a pipe created with the default descriptor is owned by the token's default owner, which for
  an elevated process (or by policy) is the Administrators group, and is refused.
- **Loopback TCP:** no check is possible, which is why it is for development only.

Verbs check `GET /v1/host/info` (without the token) first and refuse a daemon whose protocol
range does not include ours. Then they ask `GET /v1/me` whose token it is: **only agent tokens
are accepted**; a person's (device) token is refused with exit 2.

## Verbs

All take `--json`, which prints the daemon's JSON on stdout, exactly (errors as
`{"code", "message"}` on stderr). Text output removes control characters, and the hidden set
every crate drops (`pitcrew_protocol::text::is_hidden`: bidirectional overrides, zero-width and
other invisible characters, tag characters), from everything the daemon sends. Task arguments are a key (`PAP-4`, any case) or an
id (`tsk_…` or a bare ULID); anything else is refused before a request is made.

```text
pitcrew whoami
pitcrew task list [--mine] [--status in_progress,review]
pitcrew task show <task>
pitcrew task move <task> <status>
pitcrew task plan <task>          < plan.txt   # one step per line, "[x]" for done; or a JSON array
pitcrew claim <task>                           # → in_progress; fine if it already is
pitcrew report <task> [--note <text>] [--review]
pitcrew comment <task> <text…> [--mention @member]…
pitcrew ask <@member> <title…> [--option <text>]… [--body <text>] [--task <task>] [--kind question]
pitcrew reply <ask> [<text…>] [--option <n>]   # options are numbered from 1, as `check` shows
pitcrew check                                  # asks for you, mentions, your asks and their answers
pitcrew board submit <draft>  < proposal.json  # a board draft's proposal (see below)
```

A text of `-` is read from stdin.

**`board submit`** answers the board draft an agent was started for (api-v1.md, "Board drafts";
the prompt names the draft, `drf_…`). It reads the proposal's JSON on stdin,
`{"tasks": [{"title", "status", "description"?, "evidence": [<session id>]}], "note"?}`, refuses
one over 32 KiB or that is not a JSON object before anything is sent (exit 2), and posts it with
the agent's token. Only the draft's own agent may (exit 3 otherwise), once (exit 4 after); the
daemon checks the rest (exit 2). Nothing is created until a person reviews the proposal.

| Exit | Meaning |
|---|---|
| 0 | Success |
| 1 | Anything else: an internal error, a daemon that failed the identity check, an incompatible protocol |
| 2 | Invalid: arguments, environment, or the daemon said `400 invalid` |
| 3 | Unauthorized or forbidden |
| 4 | Conflict, e.g. a move the rules do not allow |
| 5 | The daemon is unavailable |
| 6 | Not found |

## The hook

```text
pitcrew hook <engine> <event> [payload]
```

`engine` is `claude`, `codex` or `opencode`; `event` is the CLI's own event name. The payload is
the CLI's hook JSON on stdin, or the argument after the event (Codex's `notify`). It is `POST`ed
to `/v1/hooks/{engine}/{event}`.

The hook **always exits 0 and prints nothing**, even if it panics, and gives up after 500 ms
whatever the daemon does. Global flags before it (`pitcrew --json hook …`) keep it on this path. Set `PITCREW_HOOK_DEBUG=1` to see on stderr why an event was not delivered.

`pitcrew hooks install --hook-form auto|exec|shell` defaults to `auto`. It probes
`claude --version` on the installed CLI's PATH for at most 3 seconds, and writes exec form only for
Claude Code **2.1.139 or newer**. The official [2.1.139 changelog](https://github.com/anthropics/claude-code/blob/main/CHANGELOG.md#21139)
introduces the hook `args: string[]` field. Missing, old, failed, unreadable or timed-out probes
use shell form and say so in the install output. `--hook-form exec` requires a successful version probe and
refuses installation when compatibility cannot be verified; `--hook-form shell` forces the existing shell behavior.
`hooks diff` accepts the same option and previews the chosen command and arguments.

The probe checks the first Claude Code on PATH. Other installs (including IDE-bundled copies
or ones on a different PATH) may read the same settings. Use `--hook-form shell` if any of
those installs is older than 2.1.139: an older CLI ignores `args` and runs bare `pitcrew`,
whose usage exit code 2 can block UserPromptSubmit or Stop hooks. `hooks status` never runs
Claude Code; it accepts either form at the current executable path and reports each form's count.

Exec form spawns the exact absolute path reported by the OS, without quoting or converting
Windows separators (the real `pitcrew.exe`), and passes the argument array directly:

```json
{"type":"command","command":"/home/sam/Program Files/pitcrew","args":["hook","claude","Stop"],"timeout":5}
```

A filesystem path can legally contain `${...}`. Claude Code has its own placeholder expansion,
independent of shell quoting; until a literal escape is documented, installation refuses paths
containing `${` rather than risk substituting a different program. Move the executable to a
literal path to install these hooks. Ordinary spaces, quotes and dollar signs are literal in
exec form.

Shell form writes `<pitcrew> hook claude <Event>`. Claude Code runs that with a shell: Bash
(Git Bash on Windows), or PowerShell on Windows without Git Bash. Paths are quoted for POSIX
`sh` when needed; on Windows, backslashes become `/`, so a plain path works unquoted in both
shells, while a path with spaces uses Git Bash quoting.

Reinstalling updates stale paths and migrates between forms without duplicates. It preserves
user options such as timeout. Uninstall recognises both forms; extra arguments, another program,
and shell commands merely mentioning pitcrew are left untouched. Codex and OpenCode installation
is unchanged. The hook itself still exits 0 and prints nothing.

Its wall time (release build, spawn to exit, 200 runs) is measured by:

```text
cargo test -p pitcrew-cli --release --test hook_timing -- --ignored --nocapture
```

Files API error compatibility: too_large (413) maps to invalid / exit 2; unsupported (501)
maps to unavailable / exit 5, including a response without an API error body.

The public `install::Installation` library retains the same installer plans for daemon onboarding: `preview` reads supported CLI configurations without writing, `files` returns exact text, and `apply` preflights stale files, preserves backups, and skips already applied files on retry. Plans are opaque and omit configuration contents from Debug output.

Onboarding review: hook previews detect supported CLIs on PATH or through their
homes, skip conflicting engines while applying other changes, and report the
skipped engines. No-change previews cannot set the wizard's installed flag.
Desktop packages include the hook CLI beside the daemon. Safety uses snake_case
wire fields and the shared PermissionMode enum; bypass defaults are currently
refused. Unsaved safety reports `saved: false` for legacy per-task acceptance.
