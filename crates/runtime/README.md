# pitcrew-runtime

Terminal runtimes: tmux control mode and the PTY supervisor, behind pitcrew-interfaces::runtime::Runtime.

**Owned by stream B** — see [docs/build/streams/B.md](../../docs/build/streams/B.md).

The current implementation provides the pure building blocks for the runtimes:

- `control::ControlParser::feed(&[u8])` returns complete command replies and notifications.
  It accepts arbitrary byte boundaries, decodes pane output once, and preserves reply lines,
  names, and unknown arguments as bytes. Command replies close only on a matching timestamp
  and command number. Notifications cannot occur inside reply blocks; notification-looking
  reply lines stay literal. Unknown or malformed records become `Other`. Incomplete lines and
  replies remain buffered; create a new parser when reconnecting. Input is the LF-delimited
  `-C` protocol, not the terminal wrapper emitted by `-CC`.
- `command::Command` builds a single command line from `Argument` values, including distinct
  pane, window, and session IDs. `Command::send_literal` uses `send-keys -l`, terminates option
  parsing with `--`, and never enables format expansion. Arguments reject NUL because tmux
  cannot represent it in text; newline and other control bytes are encoded with octal escapes.
  The output goes directly to control-mode stdin. It is not a shell command or process argv.
  Literal newlines still reach the program in the pane as input.
- `ReplayBuffer::new(capacity)` retains at most `capacity` bytes; `Default` retains 2 MiB.
  `append`, `end`, and `read` use absolute byte offsets. Reads before retained history start
  at the oldest byte and set `truncated`; future reads return empty data at the current end.
  `end` always identifies the end of the whole stream, even when a read is limited by `max`.
  Zero capacity and zero-length reads are supported.
- `keys::{tmux_key, pty_key}` cover every protocol `Key`. PTY arrows use normal cursor mode;
  application cursor mode will need terminal-state handling in the later PTY runtime.

Protocol references: [tmux control mode](https://github.com/tmux/tmux/wiki/Control-Mode) and
[tmux command parsing](https://man.openbsd.org/tmux.1#PARSING_SYNTAX).

Run `cargo test -p pitcrew-runtime`. Table tests cover every supported notification and key;
property tests exercise arbitrary stream boundaries, Unicode command framing, and replay
reads against an unbounded `Vec<u8>` model. The real-tmux test uses a random private socket,
skips with a diagnostic when tmux is absent, and cleans up its clients and server on failure.
It verifies successful and failed replies, pane output, and literal input containing quotes,
semicolons, backslashes, variables, shell-looking text, newlines, formats, and Unicode.

`TmuxRuntime`, `PtyRuntime`, and the PTY supervisor are deferred to later briefs.
