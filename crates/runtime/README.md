# pitcrew-runtime

Terminal runtimes: tmux control mode and the PTY supervisor, behind pitcrew-interfaces::runtime::Runtime.

**Owned by stream B** — see [docs/build/streams/B.md](../../docs/build/streams/B.md).

The current implementation provides the building blocks for the runtimes. **tmux 3.2 is the
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
- `keys::{tmux_key, pty_key}` cover every protocol `Key`. PTY arrows use normal cursor mode;
  application cursor mode will need terminal-state handling in the later PTY runtime.

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
tmux server that belongs to PitCrew alone. `PtyRuntime` and the PTY supervisor are later briefs.

- **A private server.** The socket is `TmuxOptions::default_socket()`, `/tmp/pitcrew-<uid>/tmux`
  (short enough for macOS's socket path limit, and the same for every PitCrew process of the
  user, so a restarted daemon finds it). Its directory is created 0700; one that belongs to
  someone else, is a link, or is open to others is refused, never repaired. The server starts
  with `-f /dev/null`: the user's `~/.tmux.conf` does not apply, and a person's own tmux server
  and sessions are never touched. Every tmux process the runtime starts gets `-S <socket>`.
- **Attaching by hand:** `tmux -S /tmp/pitcrew-$(id -u)/tmux attach -t pitcrew`, then pick a
  window, or attach to one terminal with its `native_target`: `attach -t pitcrew:@12`. Window
  sizes are PitCrew's (`window-size manual`), so an attached terminal shows them as they are.
  tmux's default key bindings apply (prefix `C-b`, detach with `C-b d`).
- **Layout.** One session, `pitcrew`, holds every terminal as a window with one pane. The pane
  option `@pitcrew-terminal` holds the `TerminalId`, so `list()` finds terminals after a restart;
  `@pitcrew-offset` holds where its output numbering resumes. The session and server exist while
  terminals do: a short-lived `pitcrew-start` window holds a new session until its first
  terminal exists (it ends by itself after 60 seconds if PitCrew stops first).
- **One connection.** Commands go to the client's stdin through `command::Command` (never a
  process per command); a writer thread owns stdin, so a tmux that stops reading costs a bounded
  queue, not a stuck caller. Replies are matched to commands in order (tmux answers one client's
  stdin in order, one `%begin`/`%end` per line). `%output` goes to the pane's terminal; output of
  a pane not recorded yet (a new window's first bytes can beat its `new-window` reply to the
  caller) is kept, bounded, and given to the terminal when it is recorded.
- **Starting a program.** `new-window -d` with the name (`Name`, controls replaced by spaces),
  directory (`FormatLiteral`; it must exist and be absolute) and variables (`-e`, literal; names
  must be POSIX names) as single arguments, then `/bin/sh -c <fixed script> <program> <args…>`:
  the program and arguments are positional parameters, never shell text, and a program name may
  not start with `-`. The script does not `exec` the program: tmux 3.2 destroys a pane as soon
  as its own process ends, and control clients then lose output not yet sent (about one exit in
  eight with `exec` in our measurement). The shell outlives the program by 0.2 s, and traps
  `INT`/`QUIT` so Ctrl-C reaches only the program. A terminal's `pid` is that shell.
- **Input.** `write` sends `send-keys -H` in 1 KiB commands; `send_keys` sends tmux key names.
  Both first leave copy mode (query `#{pane_in_mode}`, `send-keys -X cancel`, query again), under
  one lock, and refuse input to a pane stuck in a mode.
- **Output and restarts.** Each terminal has a `ReplayBuffer` (2 MiB by default). Dropping the
  runtime detaches (the terminals keep running) and first stores each terminal's exact end in
  `@pitcrew-offset`; a new runtime's `list()` adopts the tagged panes and numbers their output on
  from there. While running, the stored value is kept at least 512 KiB ahead of the output (one
  `set-option` per 512 KiB to 1 MiB), so after a crash numbering resumes past every offset a
  reader saw, and an old offset reads as `truncated`. Output printed while no client was
  attached is not in the stream.
- **Reconnecting.** If the control client dies, a keeper thread attaches again (50 ms, doubling
  to 2 s) while terminals are alive; offsets continue. At the start it also attaches once, to
  record output from terminals of a previous run. With no server left, every terminal has
  ended.
- **`screen()`** comes from a `vt100` model fed from the replay buffer when the screen is read
  or resized (never from `capture-pane`), so output nobody looks at costs no emulation. If more
  than the buffer holds arrived since the last look, the model starts again from the oldest
  byte kept; modes set before that (a scroll region, say) are lost.
- **Ended programs** stay readable (`alive: false`) until 16 more have ended. `kill` closes the
  window (tmux sends `SIGHUP`); killing an ended terminal is a no-op.
- **Bounds.** Every call is bounded by `TmuxOptions::call_timeout` (5 s; `start`:
  `start_timeout`, 15 s) and answers `Unavailable` past it. Sizes are 1 to 1000, as in the API.
- **Detection.** `tmux::detect(&options)` checks the version (3.2 or newer), the socket's
  directory, and that `tmux -S <socket> start-server` works; it gives `TmuxSupport` (whose
  `capability()` is `Capability::Tmux`) or `RuntimeError::Unavailable` with the reason.
  `tmux::detect_async` runs it on its own thread as a future any executor can await. On
  non-Unix systems it says tmux is unavailable.

tmux 3.2a behaviour this relies on, found while building it: a pane that closes is reported as
`%unlinked-window-close`, even from the client's own session; and `respawn-pane -k` on a pane
under `remain-on-exit` sometimes crashes the 3.2a server, so the runtime never uses it.

Measured on tmux 3.2a in WSL (`cargo test --release -p pitcrew-runtime --test tmux_runtime --
--ignored --nocapture`): 50 MiB of `yes` output reached the replay buffer at about 10 MiB/s,
the rate tmux itself sustains (its server used a full core). This process used 0.73 to 0.84 s
of CPU for it: about 0.016 s per MiB, 15% of one core at that rate, so under 5% below about
3 MiB/s.

The real-tmux tests (`tests/tmux_runtime.rs`) each run their own server on a random private
socket and mark every process they start with `PITCREW_TEST_RUN=<mark>` (the runtime passes it
to tmux, whose server and panes inherit it); each test kills its server and checks through
`/proc` that nothing with its mark is left.
