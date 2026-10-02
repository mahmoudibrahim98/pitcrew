# pitcrew-ptyd

pitcrew-ptyd: a small, rarely-updated process that owns PTYs where tmux is unavailable, so daemon upgrades do not kill sessions.

**Owned by stream B** — see [docs/build/streams/B.md](../../docs/build/streams/B.md).

It owns PitCrew's terminals on a machine without a usable tmux (native Windows above all): one
PTY per terminal through `portable-pty` (ConPTY on Windows), each with a bounded replay buffer and
a screen model. `pitcrewd` reaches it through `pitcrew_runtime::PtyRuntime` (see the runtime's
README), which starts it when needed. It outlives `pitcrewd`.

```text
pitcrew-ptyd serve --endpoint <socket or pipe> [--history <bytes>] [--idle-exit-ms <ms>] [--foreground]
pitcrew-ptyd --version
```

Exit status: 0 after an idle exit, 1 on an error, 2 for bad arguments, 3 when another ptyd
already serves the endpoint.

## Where it is

`pitcrew-ptyd` (`pitcrew-ptyd.exe` on Windows) is installed **next to `pitcrewd`**: the runtime
looks for it next to the running executable (`pty::launch::beside_current_exe`), or where
`PtyOptions::ptyd` says, never on `PATH`. Stream P bundles it with `pitcrewd`, in the same
directory. It must be built with the same `PROTOCOL` as the daemon it ships with.

## The protocol

Version 1, its own (`pitcrew_runtime::pty::proto`), apart from API v1: length-prefixed frames
of a JSON header and a raw payload. Requests (`hello`, `start`, `write`, `keys`, `resize`,
`screen`, `read`, `list`, `kill`, `info`) carry ids; each gets one reply, `ok` or `err` with a
kind (`not_found`, `exited`, `spawn`, `invalid`, `busy`, `unsupported`, `io`). A connection
starts with `hello`; another protocol is refused with a reason and the connection closed.
`write`, `keys` and `resize` from one connection are applied in order; the rest may be answered
out of order. `start` takes argv, never a shell command line.

## Terminals

- **Output** goes into a replay buffer (2 MiB by default, `--history`) addressed by absolute
  offsets, and into the runtime's screen model only when the screen is read
  (`pitcrew_runtime::screen::Screened`): the clamp filter for vt100's unbounded counts and
  strings, its own lock per terminal, and the work budget, as in the tmux runtime's review
  rounds. A reader thread per terminal records output and never emulates, so one terminal's
  flood holds up no other.
- **Input** waits in a bounded queue (4 MiB) for a writer thread per terminal, so a program that
  stops reading stalls only its own thread; past that, input is refused as `busy`.
- **What a terminal must answer.** No terminal emulator is attached, so ptyd answers cursor
  position (`CSI 6 n`, `CSI ? 6 n`, from the screen model), status (`CSI 5 n`) and device
  attributes (`CSI c`, `CSI > c`) with fixed replies. ConPTY asks for the cursor position when
  it starts and waits for the answer; TUIs ask too. Nothing a program writes is echoed back. It
  follows application cursor keys (`CSI ? 1 h`), so `keys` sends arrows as the program asked.
- **Starting a program.** The program is found as a file: a path (relative to the working
  directory), or a name on the request's `PATH`, else ptyd's own, absolute entries only (and
  `PATHEXT` on Windows), so a name is never a shell builtin. The working directory must be
  absolute and exist; variable names must be names; nothing may hold a NUL. The program gets
  ptyd's environment (on Windows, portable-pty adds the registry's), the request's variables,
  and on Unix `TERM=xterm-256color` unless the request sets it. **Windows batch files**
  (`.bat`, `.cmd`, such as npm's shims) run through `cmd.exe`, which parses their arguments
  again: an argument holding `" % ! ^ & | < > ( )` or a control character is refused.
- **Ended programs** stay readable (`alive: false`, with their exit code) until 16 more have
  ended. At most 256 terminals run at once.

## Security

- **Where it listens.** Unix: a socket in a private directory (0700, ours, checked before use
  by `pitcrew_runtime::pty::check_endpoint`, the tmux socket's rules), mode 0600. Windows: a
  named pipe whose security descriptor names the current user as owner and grants the current
  user alone (`O:<sid>D:P(A;;GA;;;<sid>)`, nothing inherited), with
  `PIPE_REJECT_REMOTE_CLIENTS`.
- **The peer check.** Unix: every client's uid (`SO_PEERCRED` / `getpeereid`) must be ours.
  Windows: the user SID of every client's process token (`GetNamedPipeClientProcessId`, then the
  process's token) must be ours. Anyone else is disconnected before a byte is read.
- **One per user and endpoint.** Unix: a lock file next to the socket (`ptyd.lock`, `flock`,
  opened without following links), taken before the socket is bound, so a stale socket is
  removed only by the lock's holder. Windows: the pipe's first instance
  (`FILE_FLAG_FIRST_PIPE_INSTANCE`). A second ptyd exits with status 3. On Windows another user
  who takes the pipe's name first only stops ptyd from starting: clients refuse a pipe that is
  not owned by the current user.
- **Bounds.** At most 32 clients. A client says hello within 10 seconds. Frames are at most
  5 MiB (1 MiB of header), `write` at most 1 MiB, `read` at most 4 MiB and waits at most
  10 seconds. At most 16 slow requests (start, screen, read, kill) of a client, and 64 of all
  clients, are in progress until their replies are written, so replies a client does not read
  stay bounded; a client that stops reading stops being read. A malformed frame ends only its
  own connection.
- **Kill** ends the program's whole tree. Unix: the program leads its session and process group
  (portable-pty starts it with `setsid`); the group gets `SIGTERM`, then `SIGKILL` after half a
  second. The program is not reaped meanwhile: the waiter thread waits for it with `waitid(…,
  WNOWAIT)` and reaps it only under the lock the kill holds, so its pid, and its group, cannot
  be reused while they are signalled. A process that left the group (`setsid`, a daemonizing
  program) survives, as with tmux. Windows: each program is put in a Job Object of its own
  (`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`) as soon as it has started, and kill terminates the job
  (and the program itself, should the job be missing); when ptyd ends, closing the jobs ends
  their programs. portable-pty cannot start a program suspended, so a process the program
  starts in the instant before it joins the job escapes it.
- **Untrusted output.** A terminal's output is the program's, so hostile: it is bounded before
  emulation (above), and the answers ptyd writes back are fixed or numbers.
- **The same user is not a boundary.** Programs run as the user and can reach the endpoint as
  the user can, as with tmux's socket.
- **`unsafe`.** None in this crate (`forbid(unsafe_code)`). The Win32 calls it needs (the pipe's
  descriptor, the client's token, Job Objects) are in `pitcrew_runtime::pty::windows`, the one
  module of either crate that allows `unsafe`, with a `SAFETY` comment on every block.

## Running

- **Detached.** On Unix, `serve` without `--foreground` starts a copy of itself with
  `--foreground` (its standard error in `ptyd.log` next to the socket, truncated at each start)
  and exits, so whoever started it has no child to reap; the copy calls `setsid`, leaving the
  starter's session. On Windows, `PtyRuntime` starts it with `--foreground`, detached
  (`DETACHED_PROCESS`, a new process group, outside the starter's job when that job allows
  breaking away); its log goes nowhere.
- **Idle exit.** With no terminal running and no client connected for a while (30 seconds;
  `--idle-exit-ms`), it exits and removes its socket. A connected `pitcrewd` keeps it running.
- **Upgrades.** ptyd outlives daemons, so a new `pitcrewd` may meet an older ptyd. One of
  another protocol is refused, with a message, and its terminals keep running until they are
  ended; then it exits when idle and the next start runs the new one. On Windows a running
  `pitcrew-ptyd.exe` cannot be replaced: an installer must expect it in use while terminals
  run.

## Tests

`cargo test -p pitcrew-ptyd`. Unit tests cover the query scanner, argument and program checks,
the arguments, and the Unix peer check (a second uid is not available here, so the comparison
is unit-tested; a real connection from this user passes). `pitcrew-runtime`'s `pty::windows`
tests cover the pipe's DACL and owner, the client's pid and user, and a Job Object (Windows
only).

The integration tests start the real binary through `PtyRuntime`, each test with its own
endpoint (a new 0700 directory in `/tmp`, or a pipe with a random name; never the default), and
mark every process with `PITCREW_TEST_RUN=<mark>` (ptyd and its terminals inherit it). Each
ends by checking that no runtime thread is left, that its ptyd exits by itself once idle, and
that nothing with its mark is left (Linux: `/proc`; macOS: `ps -E`; Windows: the ptyd and
programs it saw, by pid).

- `tests/pty_runtime.rs` (Unix, `sh -c`): write and read by offset, resize, Ctrl-C, kill of a
  program ignoring `SIGHUP` and `SIGTERM` with a background job, a restart, the screen of a
  prompt drawn by cursor movement, answered queries and arrow modes, hostile output (8200 ×
  `ESC[65535L`, a 4 MiB OSC) not slowing the screen or stopping another terminal, two
  terminals interleaving, a reconnect, ptyd outliving its client and exiting once idle, a
  second ptyd refused, hostile arguments, detection, and a throughput measurement (ignored by
  default: `cargo test -p pitcrew-ptyd --test pty_runtime -- --ignored --nocapture`).
- `tests/protocol.rs` (Unix): malformed and hostile frames against a real ptyd, and a mute,
  stopped, garbage-sending or other-protocol ptyd against `PtyRuntime`.
- `tests/pty_runtime_windows.rs` (Windows, `cmd.exe /c`, ConPTY): the same acceptance cases,
  checked on the screen model, since ConPTY redraws rather than passes bytes through.

Measured in WSL (debug build, `--ignored --nocapture`): 50 MiB of `yes` output streamed in
1.1 s (44.5 MiB/s), every byte read by the client; the client used 0.41 s of CPU (0.008 s per
MiB) and ptyd 1.29 s.
