# pitcrew-runtime

Terminal runtimes: tmux control mode and the PTY supervisor, behind pitcrew-interfaces::runtime::Runtime.

**Owned by stream B** — see [docs/build/streams/B.md](../../docs/build/streams/B.md).

Two runtimes implement `Runtime`: [`TmuxRuntime`](#tmuxruntime-unix) (Unix, tmux 3.2 or newer)
and [`PtyRuntime`](#ptyruntime-and-pitcrew-ptyd) (Unix and Windows, terminals owned by
`pitcrew-ptyd`). [Choosing one](#choosing-a-runtime) prefers tmux.

The building blocks come first. **tmux 3.2 is the
minimum supported portable release.** `detect_tmux(path)` executes `path -V` directly and
returns a parsed `TmuxVersion` or a fallback error. The probe is limited to two seconds and
4 KiB per output stream; a timed-out process is killed and reaped. It blocks the calling thread
for up to those two seconds, so call it from async code through `spawn_blocking`. Version parsing recognizes
letter suffixes (`3.3a`), development releases (`next-3.4`), and OpenBSD's separate OS numbering
(`openbsd-7.4`).
OpenBSD 6.9 is the minimum base-system version: its
[manual](https://man.openbsd.org/OpenBSD-6.9/tmux.1) documents the required escaping and flow
control. Older or unrecognized versions must use the PTY runtime. Version detection checks
the executable; the runtime must also verify any already-running server it connects to.

- `control::ControlParser::feed(&[u8])` returns `Result<Vec<Notification>, DesyncError>`.
  It accepts arbitrary byte boundaries, decodes pane output once, and preserves reply lines,
  names, and unknown arguments as bytes. Reply guards must match timestamp, command number,
  and flags. All notification-looking reply lines, including `%exit`, stay literal until the
  matching end/error guard. EOF during a reply reports `UnexpectedEof` through `finish()`.
  `Other` retains
  a lossless name and whether the line began with `%`. One trailing CR is stripped from marker
  and notification lines; reply body bytes remain untouched. Input is the LF-delimited `-C`
  protocol, not the terminal wrapper emitted by `-CC`.
  `with_limits(ParserLimits::new(line_limit, reply_limit))` bounds pending lines (default 1 MiB)
  and reply storage (default 4 MiB). Each body line charges its byte length, one LF, and 32 bytes
  of allocation overhead, including empty lines. Exceeding either limit clears state and latches
  a desync error. Notifications from a failing call are discarded; reconnect with a new parser.
  Consume the parser with `finish()` at EOF to check for an open reply and/or partial line.
- `command::Command` builds a single command line from `Argument` values, including distinct
  pane, window, and session IDs. `Flag` identifies trusted flags, `Format` holds deliberately
  authored static format expressions, and `FormatLiteral` doubles every `#` before quoting.
  Use `Name` for user names (`-n`, `rename-window`): it also doubles `#` and rejects all C0
  controls and DEL, which tmux can otherwise emit verbatim. Where the name is positional
  (`rename-window`), put `Flag("--")` before it, or a name starting with `-` is parsed as
  options. Use `FormatLiteral` for other user
  values in format-expanding options, such as working directories (`-c`). For displayed text
  (`display-message`), first escape `%` as `%%` as well: tmux also runs strftime (`%d` becomes
  the day), so `FormatLiteral` alone is insufficient. These protect one format-expansion pass;
  they do not make shell-command arguments safe.
  `Text` is for values whose command does not expand formats, including terminal input.
  `Command::send_literal` uses `send-keys -l`, terminates options with `--`, and never enables
  format expansion. Text arguments reject NUL; newline and other controls use octal escapes.
  The output goes directly to control-mode stdin. It is not a shell command or process argv.
  Literal newlines still reach the program in the pane as input. `send_bytes` uses `send-keys -H`
  for arbitrary bytes, including NUL and invalid UTF-8. Empty byte/key slices produce `None`.
- **Copy mode:** `-l` disables key-name parsing but still dispatches characters through a mode's
  key table. Before input, use `Command::pane_in_mode`, then `Command::cancel_copy_mode`
  (`send-keys -X cancel`) when needed, check each reply, and re-query until mode depth is zero.
  Unsupported modes or failed cancellation must prevent delivery. `TmuxRuntime` serializes this
  sequence; a person's client can still change the mode between its check and the input.
- `ReplayBuffer::new(capacity)` retains at most `capacity` bytes; `Default` retains 2 MiB.
  `append`, `end`, and `read` use absolute byte offsets. Reads before retained history start
  at the oldest byte and set `truncated`; future reads return empty data at the current end.
  `end` always identifies the end of the whole stream, even when a read is limited by `max`.
  Zero capacity and zero-length reads are supported. `starting_at(offset)` resumes numbering
  with the default capacity; `with_capacity_at(capacity, offset)` customizes both. At `u64::MAX`,
  numbering saturates and unaddressable incoming bytes are discarded without renumbering history.
- `keys::{tmux_key, pty_key, pty_key_in}` cover every protocol `Key`. `pty_key` sends arrows in
  normal cursor mode; `pty_key_in` follows application cursor mode (SS3 arrows), which
  pitcrew-ptyd tracks for each terminal.

Protocol references: [tmux control mode](https://github.com/tmux/tmux/wiki/Control-Mode) and
[tmux command parsing](https://man.openbsd.org/tmux.1#PARSING_SYNTAX).

**Untrusted pane text can forge reply guards and notifications in `capture-pane -p` output.**
Matching all guard fields is not authentication. `CommandReply::lines` is untrusted text.
`TmuxRuntime::screen()` uses a vt100 model fed by `%output`, never `capture-pane` of an
untrusted pane. The integration fixture uses capture only to synchronize
startup of its own fixed, trusted test program.

Run `cargo test -p pitcrew-runtime`. Table tests cover every supported notification and key;
property tests exercise arbitrary stream boundaries, Unicode quoting through an independent
double-quote lexer model, and replay reads against an unbounded `Vec<u8>` model.
The real-tmux test uses a random private socket (`-S /tmp/pc-<12 hex>/s` on Unix, short enough
for macOS's 104-byte limit), skips with a diagnostic when tmux is absent, and cleans up its
clients and server on failure. Unknown and unsupported versions also skip. The private socket
file and directory are removed.
It verifies successful and failed replies, literal window names without format side effects,
delivery after cancelling copy mode, binary input, and literal text containing quotes,
semicolons, backslashes, variables, shell-looking text, newlines, formats, and Unicode.

## `TmuxRuntime` (Unix)

`tmux::TmuxRuntime` implements `Runtime` over one long-lived control client (`tmux -C`) of a
tmux server that belongs to PitCrew alone.

- **A private server.** The socket is `TmuxOptions::default_socket()`: under `$TMUX_TMPDIR` or
  `$XDG_RUNTIME_DIR` when that is a private directory of the user's, else
  `/tmp/pitcrew-<uid>/tmux`. (The system removes `$XDG_RUNTIME_DIR` when the user's last login
  ends, socket included; a runner that must outlive logins, or a daemon restarted in another
  environment, should pass its own socket.) Its directory is created 0700; one that belongs to
  someone else, is a link, or is open to others is refused, never repaired, and so is anything
  at the socket path that is not a socket of the user's. Every directory above it must belong
  to root or the user, and only a sticky one (like `/tmp`) may be writable by others, as OpenSSH
  requires of its files. This is checked again before every connection (the directory can be
  replaced while PitCrew runs) and by detection. The server
  starts with `-f /dev/null`: the user's `~/.tmux.conf` does not apply, and a person's own tmux
  server and sessions are never touched. Every tmux process the runtime starts gets
  `-S <socket>`; tmux itself is found once, as an absolute path (absolute `PATH` entries only).
- **Attaching by hand:** `tmux -S <socket> attach -t pitcrew` (with the default socket in
  `/tmp`, `tmux -S /tmp/pitcrew-$(id -u)/tmux attach -t pitcrew`), then pick a window, or attach
  to one terminal with its `native_target`: `attach -t pitcrew:@12`. Window sizes are PitCrew's
  (`window-size manual`), so an attached terminal shows them as they are. tmux's default key
  bindings apply (prefix `C-b`, detach with `C-b d`).
- **The server is shared with the programs in it.** tmux gives every pane `$TMUX`, the server's
  socket. The runtime unsets it for the programs it starts (people attach from their own
  terminals, which this does not affect), but a program that finds the socket can still run any
  tmux command on that server, as the user can. The runtime limits what that can do rather than
  drawing a boundary: replies without its guard flag (ordinary hook output) are dropped, tags and
  duplicated `list-panes` rows never move or end a known terminal, and pane output is bounded
  before it is emulated. A deliberate attacker running as the same user can still disturb
  PitCrew's terminals in other ways.
- **Layout.** One session, `pitcrew`, holds every terminal as a window with one pane. The pane
  option `@pitcrew-terminal` holds the `TerminalId`, so `list()` finds terminals after a restart;
  `@pitcrew-offset` holds where its output numbering resumes. The session and server exist while
  terminals do: a short-lived `pitcrew-start` window holds a new session until its first
  terminal exists (it ends by itself after 60 seconds if PitCrew stops first). A renamed session
  is found again by its id, on the same server.
- **One connection.** Commands go to the client's stdin through `command::Command` (never a
  process per command); a writer thread owns stdin, so a tmux that stops reading costs a bounded
  queue, not a stuck caller. Replies are matched to commands in order (tmux answers one client's
  stdin in order, one `%begin`/`%end` per line). Replies to this client's own commands carry
  guard flag `1`; the command on the command line and every hook (`after-*`) reply with `0`, and
  after the first reply those are dropped, so ordinary hook output is not taken for the answer
  to one of our commands (this is not a boundary against a deliberate same-user attacker).
  However the reader thread ends, a panic included, it closes the connection, reaps the client
  and lets the runtime reattach.
- **Starting a program.** The program is found as a file on the spec's `PATH` (else the
  runtime's; absolute entries only), so a shell builtin such as `eval` is refused, as is a name
  that is not an executable file there or that starts with `-`. Then `new-window -d` with the
  name (`Name`, controls replaced by spaces), directory (`FormatLiteral`; it must exist and be
  absolute) and variables (`-e`, literal; names must be POSIX names) as single arguments, and
  `/bin/sh -c <fixed script> <program path> <args…>`: the program and arguments are positional
  parameters, never shell text. The script unsets `TMUX`, and does not `exec` the program: tmux
  3.2 destroys a pane as soon as its own process ends, and control clients then lose output not
  yet sent (about one exit in eight with `exec` in our measurement). The shell outlives the
  program by 0.2 s, and traps `INT`/`QUIT` so Ctrl-C reaches only the program.
  `TerminalInfo::pid` is therefore not the program's own process but its parent, that shell,
  which leads the process group the program and its children run in. If `start` gives up before
  tmux answers, the window tmux then makes is killed when the answer arrives, and again on the
  next connection to the same server (known by its pid and start time) unless tmux answered that
  kill. If the connection is lost before tmux answers at all, the next connection, before any
  start uses it, kills untagged panes that the wrapper started (it begins with a marker,
  `: pitcrew-wrapper;`, which `#{pane_start_command}` shows); only one runtime may use a socket.
- **Input.** `write` sends `send-keys -H` in 1 KiB commands; `send_keys` sends tmux key names.
  Both first leave copy mode (query `#{pane_in_mode}`, `send-keys -X cancel`, query again), under
  one lock, and refuse input to a pane stuck in a mode. Input (or a resize) that timed out may
  still reach the terminal: the command was sent.
- **Output and restarts.** Each terminal has a `ReplayBuffer` (2 MiB by default). Dropping the
  runtime detaches (the terminals keep running) and first stores each terminal's exact end in
  `@pitcrew-offset`; a new runtime's `list()` adopts the tagged panes and numbers their output on
  from there. While running, the stored value is kept at least 512 KiB ahead of the output (one
  `set-option` per 512 KiB to 1 MiB), so after a crash numbering resumes past every offset a
  reader saw, and an old offset reads as `truncated`. Dropping waits a few seconds at most.
- **Tags.** Within one server's life a pane id never changes owner, so a known live terminal is
  always found by its own pane, whatever its tag says now (a garbled tag does not end it, a
  copied one does not move it). A tag only adopts a pane the runtime does not know, and an id
  found on more than one pane is adopted on none. A raw newline in an option value makes
  `list-panes` print a forged extra row (seen on 3.2a): a pane id listed more than once is not
  believed, so a known terminal on it keeps its window and stays alive, and such a pane is never
  adopted. A server with another pid or start time has none of the old terminals.
- **Reconnecting.** If the control client dies, a keeper thread attaches again (50 ms, doubling
  to 2 s) while terminals are alive. Until each terminal's pane is found again, its output waits
  aside; then its numbering skips one offset, so a reader at the old end reads `truncated`
  (output printed while no client was attached is not in the stream), and the history before
  stays readable. At the start the keeper also attaches once, to record output from terminals
  of a previous run. With no server left, every terminal has ended.
- **`screen()`** comes from a `vt100` model (`screen`, shared with pitcrew-ptyd) fed from the
  replay buffer when the screen is read
  (never from `capture-pane`), so output nobody looks at costs no emulation. Each terminal's model
  has its own lock, and a size change is recorded with the offset it happened at and applied in
  order then; the reader thread never emulates, so one terminal's screen holds up no other
  terminal and no output. If more than the buffer holds arrived since the last look, the model
  starts again from the oldest byte kept; modes set before that (a scroll region, say) are lost.
  **vt100 0.16.2 bug (worth reporting upstream):** `CSI n L` (insert lines), `CSI n T` (scroll
  down) and `CSI n @` (insert characters) loop `n` times with no limit, so 128 bytes of
  `ESC[65535L` take seconds. A streaming filter (`screen/clamp.rs`) lowers the count of those, and
  of `M`, `S`, `P` and `X`, to the screen's rows or columns first, following vte's states (a CSI
  survives C0 controls, DEL and high bytes; only CAN, SUB or ESC end it). It also passes at most
  4 KiB of a string's body (OSC, DCS, SOS, PM, APC), then cancels it and drops the rest to its
  end, since vte keeps an OSC body in memory with no limit. The work per sequence is then
  bounded by the screen's size, which the API caps at 1000 by 1000, and a read is bounded too:
  if the backlog times the screen's area is past a budget, the model starts again from the last
  256 KiB. The model is at least 2 columns wide (vt100 panics drawing a wide character in 1),
  and if vt100 panics anyway, the model starts again past that output instead of failing at
  every read. `screen()` is bounded but may emulate up to that budget: call it, like every
  method here, from a blocking thread rather than an async executor's.
- **Ended programs** stay readable (`alive: false`) until 16 more have ended. `kill` sends the
  pane's process group `SIGTERM`, then `SIGKILL` after half a second, then closes the window, so
  a program that ignores `SIGHUP`, and its background jobs, end too. The group is signalled only
  if its leader is still the pane's recorded process and leads its own session, as a pane's
  process does (so a reused pid is not signalled). A process that left the group (`setsid`, a
  daemonizing program) survives. Killing an ended terminal is a no-op.
- **Bounds.** Every call is bounded by `TmuxOptions::call_timeout` (5 s; `start`:
  `start_timeout`, 15 s) and answers `Unavailable` past it. Sizes are 1 to 1000, as in the API.
- **Detection.** `tmux::detect(&options)` finds tmux, checks its version (3.2 or newer), the
  socket's directory and the socket, and that `tmux -S <socket> start-server` works; it gives
  `TmuxSupport` (with tmux's absolute path, and `capability()` `Capability::Tmux`) or
  `RuntimeError::Unavailable` with the reason. A running server must also say it is 3.2 or
  newer when the runtime attaches. `tmux::detect_async` runs detection on its own thread as a
  future any executor can await. On non-Unix systems it says tmux is unavailable.

tmux 3.2a behaviour this relies on, found while building it: a pane that closes is reported as
`%unlinked-window-close`, even from the client's own session; a pane's process leads its own
process group and session; and `respawn-pane -k` on a pane under `remain-on-exit` sometimes
crashes the 3.2a server, so the runtime never uses it.

Measured on tmux 3.2a in WSL before review round 1 (`cargo test --release -p pitcrew-runtime
--test tmux_runtime -- --ignored --nocapture`): 50 MiB of `yes` output reached the replay buffer
at about 10 MiB/s, the rate tmux itself sustains (its server used a full core). This process
used 0.73 to 0.84 s of CPU for it: about 0.016 s per MiB, 15% of one core at that rate, so
under 5% below about 3 MiB/s.

The real-tmux tests (`tests/tmux_runtime.rs`) each run their own server on a random private
socket and mark every process they start with `PITCREW_TEST_RUN=<mark>` (the runtime passes it
to tmux, whose server and panes inherit it). They run one at a time; each checks, with its
runtime dropped and before killing its server, that no control client and no runtime thread is
left, then kills its server and checks through `/proc` that nothing with its mark is left.

## `PtyRuntime` and pitcrew-ptyd

`pty::PtyRuntime` implements `Runtime` as a client of `pitcrew-ptyd` (`crates/ptyd`, see its
README), a small supervisor that owns the terminals and outlives `pitcrewd`: restarting or
upgrading the daemon never ends a session. It is for machines without a usable tmux, native
Windows above all (ConPTY there).

- **One ptyd per user**, on `PtyOptions::endpoint` (`PtyOptions::default_endpoint()`):
  - Unix: a socket, `$XDG_RUNTIME_DIR/pitcrew/ptyd` when that directory is private, else
    `/tmp/pitcrew-<uid>/ptyd`, in a directory checked by the same rules as the tmux socket's
    (0700, ours, no link, every directory above owned by root or us and not open to others
    unless sticky), before every connection. As with tmux, the system removes
    `$XDG_RUNTIME_DIR` when the user's last login ends; a runner that must outlive logins
    should pass its own endpoint.
  - Windows: the pipe `\\.\pipe\pitcrew-ptyd-<the user's SID>`.
  - `check_endpoint` is the check both sides make. Tests never use the default endpoint.
- **Each side checks the other.** On Unix the client checks the directory, then the server's
  uid (`SO_PEERCRED`); ptyd refuses a client of another uid. On Windows the client opens the
  pipe asking for identification only (`SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION`: the
  server may not act as us) and requires the pipe's owner to be the current user (another user
  cannot create a pipe owned by us); ptyd's pipe has a DACL granting the current user alone,
  refuses remote clients, and checks the user of each client's process token.
- **Starting ptyd.** `start` starts ptyd when none answers: `PtyOptions::ptyd` (by default
  `pitcrew-ptyd` next to the running executable; `PATH` is never searched), detached so it
  outlives the caller. On Unix the process started only starts the real ptyd and exits (no
  zombie is left), and the real one calls `setsid`; its log is next to the socket
  (`ptyd.log`). On Windows it is started detached, in a new process group, outside this
  process's job when the job allows it. Other calls never start ptyd: with none running there
  are no terminals (`list` is empty, a terminal is `NotFound`).
- **The connection** is run by a thread of its own (`pitcrew-pty-io`) with a small tokio
  runtime: one task reads replies and hands each to its waiting caller, another writes
  requests. Callers on any thread wait for their reply with a deadline, so a ptyd that stops
  answering costs a bounded queue (256 requests), never a stuck caller. On Windows the pipe uses
  overlapped I/O, which a read and a write in progress at once need.
- **Bounds.** Every call is bounded by `call_timeout` (5 s; `start`: `start_timeout`, 15 s,
  which includes starting ptyd; `kill`: 4 s more) and answers `Unavailable` past it. Input that
  timed out may still reach the terminal. Sizes are 1 to 1000. A `read` returns at most 4 MiB;
  writes go in pieces of 256 KiB.
- **Reconnecting and restarts.** Offsets live in ptyd, so nothing is lost when the connection
  drops or the daemon restarts: the next call connects again, `list()` finds every terminal,
  and output goes on from the last offset (nothing is `truncated` unless more than the history
  arrived meanwhile). Reads, `screen`, `info`, `list`, `resize` and `kill` try once more on a
  new connection; input and `start` do not, since they may have taken effect. `disconnect()`
  closes the connection by hand. Dropping the runtime closes it; terminals keep running.
- **Versions.** The protocol (`pty::proto`, version 1) is ptyd's own, apart from API v1. A ptyd
  of another protocol is refused with a message saying which it speaks; its terminals keep
  running (see ptyd's README).
- **Terminals.** `TerminalInfo::pid` is the program's own process (on Unix it leads its session
  and process group). Nothing can attach to a PTY by hand, so `native_target` is `None`.
  `wait_for_output(id, offset, timeout)` waits for output past an offset (ptyd waits at most
  10 s per request).
- **`screen()`** is computed in ptyd, by the same screen model as tmux's (`screen::Screened`:
  the clamp filter, its own lock per terminal, the work budget). Call it, like every method,
  from a blocking thread.

`pty::windows` holds the only `unsafe` code of this crate and of pitcrew-ptyd (Windows only):
SIDs, a pipe's owner and DACL, the client process of a pipe, and Job Objects. The crate denies
`unsafe_code`; that module alone allows it, with a `SAFETY` comment on every block.

## Choosing a runtime

`choose(&TmuxOptions, &PtyOptions)` detects tmux (`tmux::detect`) and, when it is not usable,
the PTY runtime (`pty::detect`: ptyd is a program file at its absolute path and its endpoint is
safe; nothing is started). It returns `Chosen::Tmux` or `Chosen::Pty` (with why tmux was not
used), whose `capability()` is `Capability::Tmux` or `Capability::Pty`, or `Unavailable` with
both reasons. `choose_async` runs it on its own thread as a future any executor can await, so
detection never blocks an async caller. `Chosen::into_runtime` builds the runtime.
