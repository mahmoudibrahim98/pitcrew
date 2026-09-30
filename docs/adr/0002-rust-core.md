# 0002. A Rust core daemon, `pitcrewd`

- **Status:** Accepted
- **Date:** 2026-09-30

## Context

The core of PitCrew runs in three places: on the user's laptop, on servers, and on HPC login or
compute nodes. It is also invoked as a hook on **every turn of every agent**. The requirements:

- one file that runs on any Linux without a runtime, a compiler, root or internet (P6);
- start-up in milliseconds, because hooks run constantly;
- fast, incremental parsing of large JSONL transcripts;
- first-class PTY and terminal-state libraries on Windows, macOS and Linux;
- a strong supply-chain story.

| | Python | TypeScript (Node/Bun) | Go | Rust |
|---|---|---|---|---|
| One static file for any Linux | No (~40 MB bundle) | Bundled runtime (50–90 MB) | Yes (~15 MB) | Yes, musl (~10–15 MB) |
| Start-up | 50–150 ms | 30–60 ms | 2–5 ms | 1–3 ms |
| JSONL parsing | 1× | 3–5× | ~10× | 10–20× |
| PTY and terminal state on 3 OSes | Weak on Windows | Native builds (`node-pty`) | Adequate | `portable-pty`, `vt100` / `alacritty_terminal` |
| Shares code with the desktop shell | No | With Electron | No | With Tauri |
| Supply-chain tooling | Weak | Large surface | Good | `cargo-deny`, `cargo-audit`, `cargo-vet` |

## Decision

The core is **Rust**: one binary, `pitcrewd`, containing the hub and runner modules
(ADR-0009), plus the agent-facing CLI `pitcrew` and the hook entry point, built as small binaries
from the same crates.

## Consequences

- The largest performance wins are design choices (events not polling, incremental parsing,
  SQLite, tmux control mode). Rust adds near-zero hook cost, one static file, and an order of
  magnitude on the first scan of a machine.
- Workspace-wide lints: no `unsafe` (except where a crate documents and isolates it), no
  `dbg!`, `unwrap` discouraged outside tests. `cargo-deny` gates licences and advisories.
- The API types are Rust and are exported to TypeScript, so the UI cannot drift.
