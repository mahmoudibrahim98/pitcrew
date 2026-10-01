# Brief I · Hook install and uninstall

- **Stream:** I · Agent CLI and hooks. **Branch:** `s/I/hook-install`. **Paths:**
  `crates/cli/**`.
- **First read:** [README.md](README.md), then `docs/build/streams/I.md` (work package 4),
  [I-cli-and-hooks.md](I-cli-and-hooks.md) (merged), and the current `crates/cli`.

## Goal

`pitcrew hooks status|diff|install|uninstall`, which wires `pitcrew hook` into each agent CLI's
configuration **safely**: show the exact diff first, be idempotent, and never touch what we
didn't write.

## What to build

1. **Claude Code:** the `hooks` section of the user settings file (`~/.claude/settings.json`, or
   the file under `CLAUDE_CONFIG_DIR`). Add `SessionStart`, `UserPromptSubmit`, `Stop`,
   `SessionEnd` and `Notification` entries that run `pitcrew hook claude <Event>`.
   - Preserve everything else byte-for-byte where possible: same key order and formatting
     style.
   - Mark our entries so uninstall removes only them, e.g. by the exact command path.
2. **Codex:** the `notify` setting in `~/.codex/config.toml` (or `CODEX_HOME`).
   - If the user already has a `notify`, **don't replace it**: report the conflict and offer a
     wrapper only on an explicit `--chain` flag.
   - Keep TOML comments and formatting (`toml_edit`, exact version; justify it).
3. **OpenCode:** a small plugin file in OpenCode's plugin folder that calls `pitcrew hook
   opencode <event>` on session events. Verify the plugin API and folder against the installed
   OpenCode **read-only**. Only create files we own.
4. **Behaviour:**
   - `status` shows installed, partly installed, missing or conflicting per CLI;
   - `diff` prints a unified diff of exactly what `install` would change;
   - `install` asks for confirmation unless `--yes`;
   - write atomically (temp file + rename, preserving permissions) and back up to
     `<file>.pitcrew-backup-<timestamp>` first;
   - `uninstall` reverses exactly our changes.
5. The command path written into configs is the absolute path of the current `pitcrew`
   executable, quoted correctly for each file format and OS (spaces in Windows paths!).

## Acceptance

- **Round trip:** for each CLI, `install` then `uninstall` leaves the config **byte-identical**,
  over fixtures with comments, unusual formatting and unrelated hooks.
- `install` twice is a no-op; foreign hooks are never removed; a conflicting Codex `notify` is
  reported, not overwritten.
- Paths with spaces and quotes on Windows and Unix are quoted correctly (tests).

## Out of scope

The runner consuming hooks (stream D), the OpenCode plugin's runtime behaviour beyond calling the
hook.
