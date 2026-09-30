# pitcrew-runtime

Terminal runtimes: tmux control mode and the PTY supervisor, behind pitcrew-interfaces::runtime::Runtime.

**Owned by stream B** — see [docs/build/streams/B.md](../../docs/build/streams/B.md).

The current implementation provides the building blocks for the runtimes. **tmux 3.2 is the
minimum supported portable release.** `detect_tmux(path)` executes `path -V` directly and
returns a parsed `TmuxVersion` or a fallback error. It recognizes letter suffixes (`3.3a`),
development releases (`next-3.4`), and OpenBSD's separate OS numbering (`openbsd-7.4`).
OpenBSD 6.9 is the minimum base-system version: its
[manual](https://man.openbsd.org/OpenBSD-6.9/tmux.1) documents the required escaping and flow
control. Older or unrecognized versions must use the PTY runtime. Version detection checks
the executable; the runtime must also verify any already-running server it connects to.

- `control::ControlParser::feed(&[u8])` returns `Result<Vec<Notification>, DesyncError>`.
  It accepts arbitrary byte boundaries, decodes pane output once, and preserves reply lines,
  names, and unknown arguments as bytes. Reply guards must match timestamp, command number,
  and flags. Notification-looking reply lines stay literal, except `%exit`, which aborts the
  pending reply and emits `Exit`; callers must fail the outstanding command. `Other` retains
  a lossless name and whether the line began with `%`. One trailing CR is stripped from marker
  and notification lines; reply body bytes remain untouched. Input is the LF-delimited `-C`
  protocol, not the terminal wrapper emitted by `-CC`.
  `with_limits(ParserLimits)` bounds pending lines (default 1 MiB) and reply body wire bytes
  (default 4 MiB, including one LF per line so empty lines consume budget). Storage also has
  bounded per-line vector overhead. Exceeding either limit clears buffered state and latches
  a desync error. Notifications from a failing call are discarded; reconnect with a new parser.
  Consume the parser with `finish()` at EOF to check for an open reply and/or partial line.
- `command::Command` builds a single command line from `Argument` values, including distinct
  pane, window, and session IDs. `Flag` identifies trusted flags, `Format` holds deliberately
  authored static format expressions, and `FormatLiteral` doubles every `#` before quoting.
  Use `FormatLiteral` for all user values in format-expanding options: window names (`-n`,
  `rename-window`), working directories (`-c`), and displayed text (`display-message`). It
  protects one format-expansion pass; it does not make shell-command arguments safe.
  `Text` is for values whose command does not expand formats, including terminal input.
  `Command::send_literal` uses `send-keys -l`, terminates options with `--`, and never enables
  format expansion. Text arguments reject NUL; newline and other controls use octal escapes.
  The output goes directly to control-mode stdin. It is not a shell command or process argv.
  Literal newlines still reach the program in the pane as input. `send_bytes` uses `send-keys -H`
  for arbitrary bytes, including NUL and invalid UTF-8. Empty byte/key slices produce `None`.
- **Copy mode:** `-l` disables key-name parsing but still dispatches characters through a mode's
  key table. Before input, use `Command::pane_in_mode`, then `Command::cancel_copy_mode`
  (`send-keys -X cancel`) when needed, check each reply, and re-query until mode depth is zero.
  Unsupported modes or failed cancellation must prevent delivery. The future runtime must
  serialize this sequence and handle external clients changing modes concurrently.
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
The future `screen()` implementation must use a vt100 model fed by `%output`, never
`capture-pane` of an untrusted pane. The integration fixture uses capture only to synchronize
startup of its own fixed, trusted test program.

Run `cargo test -p pitcrew-runtime`. Table tests cover every supported notification and key;
property tests exercise arbitrary stream boundaries, Unicode quoting through an independent
double-quote lexer model, and replay reads against an unbounded `Vec<u8>` model.
The real-tmux test uses a random private socket,
skips with a diagnostic when tmux is absent, and cleans up its clients and server on failure.
It verifies successful and failed replies, literal window names without format side effects,
delivery after cancelling copy mode, binary input, and literal text containing quotes,
semicolons, backslashes, variables, shell-looking text, newlines, formats, and Unicode.

`TmuxRuntime`, `PtyRuntime`, and the PTY supervisor are deferred to later briefs.
