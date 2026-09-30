# PitCrew

**Your agents work on every machine, including the cluster. PitCrew keeps track.**

PitCrew is an open-source desktop app for people who run several AI coding agents (Claude Code,
Codex, OpenCode) at once, on their laptop, on servers and on HPC clusters. It gives you:

- **One place for every agent session.** Your agents stay real CLI sessions; you can still
  `tmux attach` or `claude --resume` without PitCrew.
- **Projects, workstreams and tasks** with agents as assignees. Tasks move by themselves as the
  agents work.
- **"Where it stands"** for every workstream. It is kept current from what the agents actually did,
  and every line links to its evidence.
- **One Inbox** for agent questions, decisions, reviews and approvals, answered in place.
- **Remote machines and HPC.** PitCrew runs on your laptop and connects over SSH. It sets up a small
  helper on the remote for you, with no root and no internet needed there, and SLURM-aware.
- **Import** of the sessions already on each machine, or a fresh start.

> **Status: pre-alpha.** Nothing is usable yet. The repository is being built in parallel streams;
> see [`docs/build`](docs/build/README.md).

## Architecture in one picture

```
Laptop                                        Any machine (this PC, a server, an HPC login node)
┌──────────────────────────────┐   SSH /     ┌──────────────────────────────────────────────┐
│ PitCrew desktop (Tauri)      │   local     │ pitcrewd  (one static binary)                 │
│  Projects layout · Agent     │ ──────────► │  runner: watches transcripts, runs terminals │
│  console · Inbox · keychain  │ ◄────────── │  hub:    projects, tasks, events, recaps      │
└──────────────────────────────┘             │  agents: Claude Code · Codex · OpenCode       │
                                             └──────────────────────────────────────────────┘
```

Architecture decisions are recorded in [`docs/adr`](docs/adr).

## Repository layout

| Path | What |
|---|---|
| `crates/protocol` | Shared types: domain model, events, runner ↔ hub protocol, API frames |
| `crates/interfaces` | Contracts between crates: terminal runtime and transcript source adapters |
| `crates/fixtures` | Synthetic test data: a demo workspace and sample transcripts |
| `crates/*` | The daemon's parts, one crate per build stream |
| `apps/desktop` | Tauri shell |
| `apps/ui` | React interface |
| `apps/mock-hub` | A fixture server that speaks the real API, for building the UI in parallel |
| `packages/tokens` | Design tokens |
| `docs/adr` | Architecture decisions |
| `docs/build` | How the build is organised: streams, ownership, workflow, contracts |
| `scripts/ci` | The path guard, ownership audit and scrub gate |

## Contributing

Work is split into **streams** with exclusive path ownership, so several people (and agents) can
work at once without conflicts. Read [CONTRIBUTING.md](CONTRIBUTING.md) first.

## License

[Apache-2.0](LICENSE).
