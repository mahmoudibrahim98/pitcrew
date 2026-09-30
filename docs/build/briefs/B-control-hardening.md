# Brief B · Control-mode hardening (follow-up)

- **Stream:** B · Runtime. **Branch:** `s/B/control-hardening`. **Paths:** `crates/runtime/**`
  only.
- **First read:** [README.md](README.md), then
  [B-control-mode-and-buffer.md](B-control-mode-and-buffer.md) (the previous brief, now merged)
  and the current `crates/runtime` code on `main`.

## Goal

Fix what the review of `s/B/control-mode-and-buffer` found, **before** the full `TmuxRuntime` is
wired in the next brief. The review traced the quoting against the tmux 3.2a source and found no
way out of `send-keys -l`. The findings below are about other commands, older tmux versions,
copy mode, and untrusted pane content.

## What to fix

1. **Format expansion through the generic command builder** (`command.rs:14-17, 46, 113-119`).
   Several tmux commands expand `#{…}` and `#(…)` formats in their arguments: `new-window -n`,
   `rename-window`, `display-message`, `-c` directories. So
   `new-window "-n" "#(touch /tmp/pwned)"` runs a shell command. `StartSpec.name` is meant to
   become a window name, so this matters as soon as `TmuxRuntime` exists.
   - Add a format-literal argument kind that escapes `#` as `##`, and use it for every
     user-controlled value passed to a format-expanding option.
   - Give trusted flags their own variant (e.g. `Flag(&'static str)`) instead of reusing `Text`.
   - Test with real tmux: a window named `#(touch <tmpfile>)` creates no file, and the name
     reads back literally.
2. **Minimum tmux version.** The quoting is correct only on tmux ≥ 3.1:
   - 3.0 has no octal escapes;
   - ≤ 2.9 splits on an argument ending in `;` (`"x;" "kill-server"` runs `kill-server`).

   Enterprise Linux 8, common on HPC login nodes, ships tmux 2.7.
   - Set the floor at **3.2**, since `%extended-output`, `%pause` and `%continue` need it.
   - Add `detect_tmux(path) -> Result<TmuxVersion, …>` that parses `tmux -V` (including forms
     like `3.3a`, `next-3.4`, `openbsd-7.4`), and refuse older versions so the caller falls back
     to the PTY runtime.
   - Document the floor in the README.
3. **Copy mode.** In 3.2a, `send-keys -l` into a pane that is in copy mode dispatches each
   character through the mode's key table: `q` exits, and `\r` can run `copy-pipe-and-cancel`.
   - Correct the "no key lookup" doc now.
   - Provide the building blocks the runtime will use: a query for `#{pane_in_mode}`, and a
     `send-keys -X cancel` command. Alternatively, deliver text with `set-buffer` +
     `paste-buffer -p -d` and document the choice.
   - Real-tmux test: with the pane in copy mode, text still arrives intact.
4. **Bounded parser memory and desync handling** (`control.rs:127-135, 141-152, 182-187`):
   - Add configurable limits on line length and on reply size. Exceeding them returns an
     explicit desync error, so the runtime can reconnect.
   - Strip one trailing `\r` on marker and notification lines. This is safe because tmux escapes
     a real CR inside `%output` as `\015`. It covers a transport through a pty (`ssh -t`).
   - `finish()` reports an unfinished reply and a partial line at EOF.
   - `%exit` arriving inside an open reply ends the reply and is reported.
5. **Pane content can forge the end of a reply.** `capture-pane -p` writes raw pane text into a
   reply, so a program can print `%end <time> <n> <flags>` and then fake notifications.
   - Compare `flags` too when closing a reply.
   - Document `CommandReply::lines` as untrusted text.
   - Note in the README that `screen()` must come from a vt100 model fed by `%output`, never from
     `capture-pane` of an untrusted pane. The next brief implements that.
6. **A real quoting test.** The property test checks only framing; it would pass even if the `$`,
   `~` or `\` escaping were removed. Add a small model of tmux's double-quote lexer (escapes,
   octal, `$`, `~`, `"`) and assert that lexing the quoted output gives back the original input
   with nothing after the closing quote. Add `FOO=bar`, `\x7f` and `%hidden x` to the real-tmux
   attack list.
7. **Smaller items:**
   - No `expect` in library code: `command.rs:132` becomes `let _ = write!(…)`, and
     `replay.rs:41` saturates instead of panicking.
   - `Notification` is `#[non_exhaustive]`; document the fields of `CommandReply` and the
     `Argument` variants.
   - `Other` keeps whether the line started with `%`, and a lossless name.
   - `ReplayBuffer::starting_at(offset)`, so a restarted runtime can resume numbering (the stream
     card's restart test needs it).
   - Add `send-keys -H` (hex) formatting for bytes that aren't valid UTF-8, because
     `Runtime::write` takes `&[u8]`.
   - `send_keys(pane, &[])` produces no command.

## Out of scope

`TmuxRuntime` itself, `pitcrew-ptyd`, `PtyRuntime` (next briefs).
