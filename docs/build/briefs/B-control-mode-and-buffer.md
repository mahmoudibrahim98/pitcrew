# Brief B · tmux control-mode parser and replay buffer

- **Stream:** B · Runtime. **Branch:** `s/B/control-mode-and-buffer`. **Paths:**
  `crates/runtime/**` only (leave `crates/ptyd` for a later brief).
- **First read:** [README.md](README.md), then `docs/build/streams/B.md`, ADR-0005,
  `crates/interfaces/src/runtime.rs` (note `OutputChunk`), and `Key` in
  `crates/protocol/src/runner.rs`.

## Goal

The pure building blocks of the terminal runtime, fully tested: a **tmux control-mode protocol
parser**, **safe command formatting**, the **offset-addressed replay buffer**, and **key
mapping**. This is work packages 1 and 3 of your card. The full `TmuxRuntime` and the PTY
supervisor come in later briefs.

## What to build

1. **Control-mode parser** (`tmux -C` output), incremental: `feed(&[u8]) -> Vec<Notification>`.
   It handles input split at any byte, including in the middle of a line or an escape.
   - Command replies: `%begin <time> <number> <flags>` … `%end` or `%error` with the same
     number, collecting the lines between.
   - `%output %<pane> <data>`: decode tmux's octal escapes (`\ooo`, including `\134` for a
     backslash) into raw bytes.
   - `%extended-output`, `%window-add`, `%window-close`, `%unlinked-window-add`,
     `%unlinked-window-close`, `%window-renamed`, `%session-changed`, `%sessions-changed`,
     `%layout-change`, `%pause`, `%continue`, `%exit [reason]`.
   - Any other notification is kept as `Other { name, args }`, never an error.
2. **Command formatting.** Build tmux commands from typed arguments with correct quoting. Text
   sent with `send-keys -l` must never be able to run another tmux command or a shell: test with
   `;`, `\`, quotes, `$(…)`, newlines, `#{…}` formats and non-ASCII.
3. **`ReplayBuffer`:**
   - a ring buffer addressed by absolute byte offset, with a configurable capacity (default
     2 MiB);
   - `append(&[u8])`, `end()`, and `read(from, max) -> OutputChunk`, with `truncated: true` when
     `from` was already dropped;
   - exact semantics as documented on `OutputChunk`.
4. **Key mapping:** `Key` → tmux key name (`Enter`, `Escape`, `C-c`, …), and `Key` → bytes for a
   PTY (`\r`, `\x1b`, `\x03`, arrow-key escape sequences, …).

## Acceptance

- **Table tests** for every notification above, including tricky escapes.
- **Property test:** splitting the same control-mode stream at arbitrary points yields identical
  notifications.
- **Property test** for `ReplayBuffer` against a simple `Vec<u8>` model: same bytes, same
  `truncated` decisions.
- **Quoting tests** show that no input escapes `send-keys -l`.
- **Integration test with real tmux**, skipped cleanly when tmux is absent (WSL has tmux 3.2a):
  start `tmux -C new-session -d -s pitcrew-test-<random>`, run a command, parse the `%begin/%end`
  reply and some `%output`, then kill the session. Use a private socket (`-L <random>`) so the
  user's tmux is never touched.

## Out of scope

`TmuxRuntime` wiring, `pitcrew-ptyd`, `PtyRuntime`, and anything outside `crates/runtime`. No
`unsafe`.
