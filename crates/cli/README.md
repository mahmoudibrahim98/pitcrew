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
- **Named pipe:** opened at identification-level impersonation, and the pipe's owner SID matches
  the current user (or our token's default owner, if elevated).
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
```

A text of `-` is read from stdin.

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

Its wall time (release build, spawn to exit, 200 runs) is measured by:

```text
cargo test -p pitcrew-cli --release --test hook_timing -- --ignored --nocapture
```
