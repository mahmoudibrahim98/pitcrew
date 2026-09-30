# 0003. Tauri 2 for the desktop shell

- **Status:** Accepted
- **Date:** 2026-09-30

## Context

The desktop app is a thin shell: a workspace registry, an SSH connection manager, a keychain, a
gateway to each workspace's daemon, notifications and an updater. All logic lives in `pitcrewd`.
Electron would work but ships ~100 MB and a Node runtime in-process.

## Decision

Use **Tauri 2** with a React 19 UI.

- Installer around 10–20 MB, lower memory, no Node in the renderer.
- Tauri's capability allow-list on every command; the webview gets only what it needs.
- Rust shared with the daemon: protocol types, SSH bootstrap, updater.
- OS keychains via the `keyring` crate; signed updates; deep links, tray, single instance.
- **The webview never holds a token.** It talks to a gateway in the Rust side, which adds the
  right device token for each workspace.

## Consequences

- Linux uses WebKitGTK, which has rendering quirks (EGL, WebGL). We test Ubuntu LTS and Fedora on
  Wayland and X11 from the first build; xterm.js falls back from WebGL to canvas.
- At milestone M1 we re-check this decision against measurements. The fallback is Electron with
  the same UI and the same daemon; only the thin shell would change.
- A strict CSP: no inline scripts, no remote origins, fonts bundled.
