# Brief I · Install Claude Code's hooks in exec form (no shell)

- **Stream:** I · CLI and hooks. **Branch:** `s/I/hook-exec-form`. **Paths:** `crates/cli/**`.
- **First read:**
  - [README.md](README.md) and the root `AGENTS.md` (or `CLAUDE.md`);
  - `crates/cli/README.md` (`pitcrew hook`, `pitcrew hooks install`);
  - `crates/cli/src/install/claude.rs` (its module docs: how a hook is recognised as ours);
  - Claude Code's hooks reference (https://code.claude.com/docs/en/hooks), section "Exec form and
    shell form".
- **Suggested agent:** Codex, or any coding agent.

## Goal

`pitcrew hooks install` writes each Claude Code hook in shell form:
```json
{"type": "command", "command": "<pitcrew> hook claude <Event>", "timeout": 5}
```
Claude Code runs that through a shell: `sh` on macOS and Linux, Git Bash on Windows, or PowerShell
on Windows without Git Bash. So the path has to be quoted for whichever shell runs it, and one with a
blank can't be quoted for both Git Bash and PowerShell.

Claude Code now has an **exec form**: with `args` present, `command` is spawned directly as a program
with `args` as its argument list, and no shell is involved on any platform:
```json
{"type": "command", "command": "<absolute path to pitcrew>", "args": ["hook", "claude", "<Event>"], "timeout": 5}
```
Use the exec form wherever the installed Claude Code supports it.

## What to build

1. **Know which Claude Code supports it.**
   - Find the first Claude Code version that accepts `args` on command hooks, from its official
     changelog (`CHANGELOG.md` in `github.com/anthropics/claude-code`). Cite the line in the report.
   - **An older Claude Code that ignores `args` would run `pitcrew` with no arguments**, so never
     write the exec form for one.
2. **Pick the form at install time:**
   - add `--hook-form auto|exec|shell` (default `auto`);
   - `auto` runs the Claude Code CLI's `--version` with a short timeout, and uses the exec form only
     when it reads a version at or above the one from item 1;
   - anything else (not found, unreadable, older, or a timeout) gets the shell form, as today, and
     says so in the install's output;
   - **tests use a stand-in `claude` on a temporary `PATH`**, never a real one.
3. **The exec form's `command`:**
   - the absolute path to the running `pitcrew` executable, exactly as the OS reports it (no quoting,
     and on Windows the real `.exe`);
   - `args` is `["hook", "claude", "<Event>"]`;
   - say whether `${...}` placeholders could ever appear in the path, and if so how they're kept
     literal.
4. **Recognising our hooks (`command_is_ours`):** a hook is ours in either form. In exec form, that
   means `args` is exactly `["hook", "claude", "<Event>"]` and `command`'s file name is `pitcrew`
   (or `pitcrew.exe`).
   - **Re-running install:** moves our hooks between forms as the choice changes, and updates a
     stale path, without duplicating them.
   - **Uninstall** removes both forms.
   - **Never touch a hook that isn't ours:** a hook with extra args, another program, or a shell
     command that merely mentions pitcrew stays.
   - Extend the tests in `install/claude.rs` and the install integration tests for each case,
     including a settings file that has both forms of ours.
5. **The plan and diff output** (`plan.rs`, `install/difftext.rs`) show the exec form readably.
6. **Unchanged:** `pitcrew hook` itself still always exits 0 and prints nothing.
7. **The README:** document the two forms, `--hook-form`, and the minimum Claude Code version.

## Acceptance

- `cargo test -p pitcrew-cli --no-fail-fast`, fmt, clippy with `-D warnings`, and the guards pass.
- Every CI job passes on the pull request. Windows CI runs the install tests, and the exec form's
  path there must be the real `.exe` path.

## Out of scope

Codex's and OpenCode's hooks, and the hook's own behaviour.
