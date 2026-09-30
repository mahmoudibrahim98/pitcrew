# Architecture decision records

Each record states one decision: the context, what we chose, and what follows from it. Records are
never rewritten after they are accepted; a later record supersedes an earlier one.

| # | Decision | Status |
|---|---|---|
| [0001](0001-record-decisions.md) | Record architecture decisions | Accepted |
| [0002](0002-rust-core.md) | A Rust core daemon, `pitcrewd` | Accepted |
| [0003](0003-tauri-desktop.md) | Tauri 2 for the desktop shell | Accepted |
| [0004](0004-sqlite-event-log.md) | SQLite with an append-only event log; NFS-safe mode | Accepted |
| [0005](0005-terminal-runtimes.md) | tmux control mode, or our own PTY supervisor | Accepted |
| [0006](0006-auth-tokens.md) | Unix sockets and scoped bearer tokens | Accepted |
| [0007](0007-domain-model.md) | Workspace → Project → Workstream → Task → Subtask; people and agents as members | Accepted |
| [0008](0008-two-layouts.md) | Two layouts over the same data | Accepted |
| [0009](0009-hub-runner-helper.md) | Hub and runner roles; the helper is uploaded over SSH | Accepted |
| [0010](0010-real-cli-sessions.md) | Agents stay real CLI sessions | Accepted |
| [0011](0011-parallel-streams.md) | Build in parallel streams with exclusive path ownership | Accepted |

To add one: copy the shape of an existing record, take the next number, and open a
`s/0/adr-<topic>` pull request.
