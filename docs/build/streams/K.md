# Stream K · Desktop shell

**Goal:** the Tauri app people install: workspaces, a gateway to each workspace's daemon, the
keychain, the local helper, notifications, tray, deep links and updates.

**Owns:** `apps/desktop/**` (replace the stub in `apps/desktop/src-tauri`).
**Depends on:** stream 0; J's connection interface; H's API contract.  **Model:** Opus-class.
**Read first:** ADR-0003, ADR-0006, ADR-0009.

## Work packages

1. **Tauri 2 app** in `apps/desktop/src-tauri`, loading the UI from `apps/ui` (dev server in
   development, the built `dist` in releases). Strict CSP, minimal capabilities.
2. **Workspace registry:** workspaces, their primary machine, and connection settings, stored in
   the app's data directory. Combined Inbox counts across workspaces.
3. **Gateway:** the webview talks only to the gateway (Tauri commands or a custom protocol),
   which forwards to each workspace's daemon over its socket or tunnel and **adds the device
   token from the keychain**. The webview never sees a token.
4. **Local helper:** start and supervise `pitcrewd` for local workspaces; pair (mint the device
   token) on first run.
5. **Notifications, tray, single instance, deep links** (`pitcrew://w/<ws>/task/<id>`).
6. **Updater** configuration (signing keys and feed come from stream P).

## Acceptance

- The app starts in < 1.5 s to interactive; idle memory with three workspaces < 300 MB.
- A test proves no token reaches the webview (gateway strips and injects).
- Runs on Windows, macOS, and Linux (Ubuntu LTS and Fedora, Wayland and X11).
- Linux CI needs WebKitGTK packages: ask the integrator to add them to `ci.yml`.

## Do not

Put business logic in the shell (it belongs in `pitcrewd`); enable remote URLs in the webview.
