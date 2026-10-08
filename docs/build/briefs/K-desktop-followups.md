# Brief K · Desktop follow-ups: open files on this computer, notification settings, hook removal

- **Stream:** K · Desktop shell.
  **Branch:** `integrator/desktop-followups`.
  **Paths:** `apps/desktop/src-tauri/**` (commands, capabilities, notification preferences),
  `apps/ui/src/files/**` and the file views from 0-files-everywhere (#63), the Settings pages from
  0-settings (#64), `crates/cli/**` (hook uninstall), `crates/daemon/**` and `crates/hub-work/**`
  (only the hook-removal route), `crates/protocol/**` (regenerate `packages/protocol-ts`),
  `apps/mock-hub/**`, `tests/conformance/**`, `docs/build/contracts/api-v1.md` and
  `docs/build/contracts/desktop-gateway.md`, the threat model, and the READMEs of what you touch.
  Mechanical edits outside the paths are fine; list them. If one item needs a substantive change
  outside the paths, skip that item, list it under Not done, and finish the rest. Never stop the
  whole brief.
- **Start after #63 (files everywhere) and #64 (Settings) merge.** This brief finishes what each of
  them deferred.
- **First read:**
  - [README.md](README.md) and the root `AGENTS.md` (or `CLAUDE.md`);
  - [0-files-everywhere.md](0-files-everywhere.md), [0-settings.md](0-settings.md) and their PRs'
    "Not done" lists;
  - `apps/desktop/src-tauri/src/commands.rs`, `capabilities/main.json`, `notify/` and
    `preferences.rs`;
  - [I-hook-install.md](I-hook-install.md), [I-hook-exec-form.md](I-hook-exec-form.md) and how
    `pitcrew hooks install` writes each CLI's settings today.
- **Suggested agent:** an Opus-class agent.

## Goal

Three things were deferred by decision from #63 and #64. Each needs a careful desktop or local
surface, so they come together here.

## What to build

1. **Open a file on this computer.**
   - In the file views, add "Reveal in folder" and "Open in editor", shown only in the desktop app
     and only for files on this computer's machine.
   - Both go through **one Tauri command** that takes a workstream id and a path relative to one
     of its locations. The command:
     - resolves the path and checks it stays inside that location after symlinks;
     - refuses anything else;
     - never accepts a free-form absolute path from the web view.
   - Allow it in `capabilities/main.json` for the main window only.
   - **Reveal:** Explorer `/select,` on Windows, `open -R` on macOS, and the file manager on the
     parent folder on Linux.
   - **Open in editor:** the editor configured in Settings (a program plus fixed arguments, chosen
     from detected editors or entered once). The default is the system's default application.
   - Always argv, never a shell. Failures are shown in plain words.
2. **Desktop notification settings**, a Settings section that shows only in the desktop app:
   - which events notify: an agent asks, a session finishes, a session fails, a write needs
     approval;
   - quiet hours;
   - per workspace on or off;
   - a "Send a test notification" button.

   Stored in the desktop's preferences. The existing notifier honours them.
3. **Remove hooks.**
   - `pitcrew hooks install` records what it changed per CLI, in an uninstall plan kept in the
     state directory: the file, the entries added, and a hash of the file after install.
   - `pitcrew hooks uninstall [--dry-run] [--engine …]` reverses exactly those entries. If the
     file changed since, it removes only PitCrew's own entries and reports what it left.
   - Settings' "Remove" calls a device-only route that runs it for the hub's machine and shows the
     dry-run first.
   - Agent and reader tokens get 403.
4. **Tests:**
   - the command's path checks: `..`, symlinks out, absolute paths, Windows `\\?\` and UNC paths;
   - argv building per platform;
   - notification preferences honoured;
   - uninstall round-trips on temporary homes for Claude Code, Codex and OpenCode, including a
     file edited after install;
   - the route's 403s;
   - mock parity and conformance.

## Acceptance

- In the desktop app, a file in a local workstream can be revealed and opened in the configured
  editor, and nothing outside the workstream's locations can be.
- Hooks installed by `pitcrew hooks install` are removed cleanly from Settings, with a dry-run
  shown first.
- fmt, clippy with `-D warnings` (workspace and the desktop crate), the tests of the crates
  touched, `cargo test -p pitcrew-protocol --features ts`, `npm test`, the UI's checks, both
  conformance targets, and the guards pass. Every CI job passes on the pull request.
