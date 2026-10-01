# Brief K · Remote workspaces: add, pair, connect, prompt

- **Stream:** K · Desktop shell. **Branch:** `s/K/remote-workspaces`. **Paths:** `apps/desktop/**`.
- **First read:** [README.md](README.md), [the desktop gateway contract](../contracts/desktop-gateway.md)
  ("Remote workspaces", "Prompts"), `crates/remote/README.md` (all of it: probe, deploy,
  launchers, SLURM and site recipes, the askpass bridge and `PromptHandler`, the tunnel's
  `Connector`, `Transport`, `LinkState`), and the `apps/desktop/src-tauri` README (the registry's
  `Connection` enum, `TokenStore`, the gateway's connectors, `no_token`).

## Goal

From the desktop app, a person adds a remote machine (a server, an HPC login node, or a SLURM
compute node), sees exactly what will happen, answers SSH's questions in the app, and then works
with that machine's hub as with the local one. The connection survives network drops by itself.

## What to build

1. **The commands,** exactly as the contract says: `gateway_ssh_hosts`,
   `gateway_remote_probe`, `gateway_remote_plan`, `gateway_remote_add`, and
   `gateway_workspace_remove`, built on `pitcrew-remote`.
   - **Plans:** hold the computed actions and, for SLURM, the rendered `JobScript`. `add` submits
     that exact script, and nothing else. A plan expires after 10 minutes and is used once.
   - **Deploy:** the helper binary is the Linux `pitcrewd` for the remote's platform. Find it
     next to the app (`helpers/pitcrewd-<os>-<arch>`, which stream P will bundle), or from a
     configured path for development. Its sha256 is checked as `deploy` does. A missing helper is
     a clear error.
   - **Pairing:** read the remote hub's device token over SSH, store it in `TokenStore` (the OS
     keychain), and never send it to the webview, a log or a file.
2. **Connections:** a remote workspace in the registry stores its host, launcher, site, job
   options and remembered transport (no secrets). The gateway reaches it through a `Connector`.
   - Map `LinkState` to the workspace states, as the contract says.
   - Persist `Connector::transport()`.
   - Call `wake()` when the OS resumes from sleep, or the network changes, if Tauri or the OS
     tells you.
   - Requests and sockets for that workspace go through `Connector::connect()`, with the token
     from the keychain.
3. **Prompts:** a `PromptHandler` that emits `gateway://prompt` and waits for
   `gateway_prompt_reply`. Withdraw stale prompts with `gateway://prompt-closed`.
   - Answers are passed once and never logged or stored.
   - Text is cleaned of control characters.
   - The `pitcrew-askpass` helper binary must be found next to the app (as stream J documents);
     a missing one is a clear error.
4. **Capabilities:** the new commands and events, for the main window only. Nothing else changes.

## Acceptance

- **Tests** (the fake ssh from `crates/remote`'s test harness can be reused, or a small fake of
  your own):
  - probe, plan and add for a `direct` launcher end to end against a fake remote that runs a real
    `pitcrewd --demo` from a temp directory: the workspace appears `ready`, and `gateway_request`
    reaches it;
  - a SLURM plan returns the exact script, and `add` submits exactly that text;
  - a plan can't be reused, and an expired one is refused;
  - a password prompt round trip, and a cancelled prompt failing the add cleanly;
  - remove deletes the keychain entry (the in-memory store in tests);
  - **`no_token` covers the remote path:** the remote's token never appears in any result, event,
    error or log line.
- **Run it in WSLg** against a fake remote (a local sshd isn't available, so use the fake ssh),
  and report what you could and couldn't exercise.
- fmt, clippy (Linux and Windows target), tests, `cargo deny`, and the guards all pass.

## Out of scope

The UI for adding a machine (stream O builds the wizard against these commands), bundling the
helper binaries (stream P), and WSL machines.
