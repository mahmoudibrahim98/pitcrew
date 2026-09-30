# Stream B · Runtime

**Goal:** run agent CLIs in terminals that stream output, take input, expose the screen, and
**survive runner restarts and upgrades**: tmux through control mode, or our own PTY supervisor.

**Owns:** `crates/runtime/**`, `crates/ptyd/**`.  **Depends on:** stream 0.  **Model:** Opus-class.
**Read first:** ADR-0005, ADR-0010; `crates/interfaces/src/runtime.rs`; `Key` in
`crates/protocol/src/runner.rs`.

## Work packages

1. **tmux control-mode client.** One long-lived `tmux -C` connection per tmux server. Parse
   `%begin/%end/%error` command blocks, `%output` (octal-escaped), `%window-add`,
   `%window-close`, `%exit`. Send commands without forking. Reconnect if the client dies.
2. **`TmuxRuntime`** implementing `Runtime`: windows tagged with a PitCrew option so `list()`
   finds them after a restart; `native_target` so people can `tmux attach`; `Key` → tmux key
   names; `screen()` from `capture-pane` or a `vt100` model fed by `%output`.
3. **Replay buffer.** Per terminal, a byte ring buffer (e.g. 2 MiB) addressed by absolute
   offset; `read_output(from)` returns what is left and sets `truncated` when `from` has been
   dropped.
4. **`pitcrew-ptyd`.** A tiny, rarely-changing process that owns PTYs (`portable-pty`; ConPTY on
   Windows) and a `vt100` screen per terminal, and serves them over a local socket or named pipe
   (per-user permissions). It outlives `pitcrewd`, so upgrading the daemon never kills sessions.
   Version its small protocol separately.
5. **`PtyRuntime`** implementing `Runtime` as a client of `pitcrew-ptyd` (spawning it if absent).
6. **Detection:** pick tmux when present and usable, else PTY; report `Capability::{Tmux,Pty}`.

## Acceptance

- Integration tests start `sh -c 'printf ready; cat'` (`cmd /c` on Windows), write, read back by
  offset, resize, send `ctrl_c`, and kill. tmux tests run where tmux exists and are skipped
  otherwise (CI Linux has it).
- Restart test: start a terminal, drop the runtime, create a new one, `list()` still finds it and
  output resumes from the last offset.
- `screen()` shows a prompt drawn with cursor movement correctly (vt100, not raw bytes).
- Throughput: 50 MB of output streamed with < 5% CPU of one core in `pitcrewd`.

## Security

Terminals run as the user. The ptyd socket must be private to the user (0700 dir / pipe ACL) and
check the peer. Any `unsafe` is isolated in one module with a soundness comment.

## Do not

Parse transcripts (stream A), decide session state (stream D), or open network ports.
