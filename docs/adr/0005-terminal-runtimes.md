# 0005. tmux control mode, or our own PTY supervisor

- **Status:** Accepted
- **Date:** 2026-09-30

## Context

Agents run in real terminals (ADR-0010). The runner must start them, stream their output, type
into them, detect prompts, and survive its own restarts and upgrades. Spawning a `tmux` process
for every send-keys and capture costs hundreds of processes a minute. Windows has no tmux.

## Decision

A `Runtime` trait (`crates/interfaces/src/runtime.rs`) with two implementations (stream B):

- **tmux, through control mode** (`tmux -C`): one long-lived client per tmux server receives
  `%output`, `%window-add` and `%exit` events and sends commands without forking. Sessions survive
  runner restarts, and people can `tmux attach` themselves.
- **PTY**, where tmux is missing (Windows, some servers): `portable-pty` (ConPTY on Windows) with
  a `vt100` screen model. PTYs are owned by a tiny, rarely-updated process, `pitcrew-ptyd`, so
  upgrading the daemon does not kill sessions.

Output is addressed by **byte offset** with a replay buffer, like transcripts: a reconnecting
client asks for bytes from its last offset and receives exactly what it missed.

## Consequences

- Prompt and menu detection read the screen model, not raw bytes.
- The runner never assumes tmux; everything goes through the trait, and `FakeRuntime` lets other
  streams test without either.
