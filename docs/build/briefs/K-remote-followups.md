# Brief K · Remote workspaces: races, an ssh version gate, and names

- **Stream:** K · Desktop shell. **Branch:** `s/K/remote-followups`. **Paths:** `apps/desktop/**`.
- **First read:** [README.md](README.md), [the desktop gateway contract](../contracts/desktop-gateway.md)
  ("Remote workspaces", "Prompts"), and the `apps/desktop/src-tauri` README ("Remote workspaces").

## Goal

The last review of remote workspaces found narrow races and a recovery gap. Close them, make sure
the prompt rule holds on every ssh PitCrew may run, and keep remote workspaces' names current.

## What to build

1. **A cancel can't be lost.** `add` takes the plan and releases the plans lock before it records
   its cancel sender, so a `gateway_remote_cancel` in between finds neither and the add runs to
   the end.
   - Record the running add while still holding the plans lock, and take the locks in the same
     order in `cancel`. Or keep both maps under one mutex.
2. **Retry or re-pair racing with remove.**
   - `remove` takes the registry entry out before it closes the link.
   - `reconnect` and `follow` insert a link only if the record still exists, checked under the
     links lock.
   - `pair` checks its claim again after `tokens.set`, and deletes the token if the claim is gone.
   - `unclaim` restores the previous entry only if the current one is still its own
     (`Arc::ptr_eq` on the connector).
3. **The local workspace comes back.** After `set_local` was refused because a remote held the
   local id, removing that remote doesn't bring the local workspace back. Keep the refused id and
   name, and apply them again when the remote is removed (or have the follower retry on a registry
   change).
4. **An ssh version gate for prompts.**
   - The prompt-kind rule relies on OpenSSH 8.4 or newer, which marks the server's text with
     `(user@host)`.
   - Check `ssh -V` once per ssh path, and refuse to answer prompts from an older ssh with a clear
     error ("ssh 8.4 or newer is needed to sign in from the app").
   - Report what Windows' built-in OpenSSH does with askpass, as far as you can tell without a
     Windows run.
5. **Coalesce retries.** Several `gateway_workspace_retry` calls during one attempt make one more
   attempt, not one per click.
6. **Names:**
   - Refresh a remote workspace's name from its hub's `GET /v1/workspace` on each connect, cleaned
     and capped as at pairing, so a name set during first-run setup shows in the switcher.
   - Add `gateway_local_host() → { name }`, this computer's host name, cleaned, for onboarding's
     machine-name default. Main window only. Add it to the contract's "Workspaces" section in
     your report as a proposal; the integrator commits the contract.
7. **Tidying:** make `Registry::insert` test-only, or remove it if `claim_remote` and
   `set_local` cover every caller.

## Acceptance

- Tests:
  - a cancel racing the start of an add, using a hook in the add to pause between the two steps;
  - retry during a remove leaves no link, tunnel or token behind;
  - the local workspace back after the remote is removed;
  - an old ssh (a fake that reports 8.1) gets no prompt answered;
  - retries coalesced;
  - a name change on the hub shows after a reconnect;
  - `gateway_local_host` is main window only.
- Check each new test against a mutation of the code it guards.
- fmt, clippy (Linux, and one Windows-target run), the desktop tests, `cargo deny` for the desktop
  manifest, `npm test`, and the guards all pass.

## Out of scope

The UI (it already handles retry and cancel), bundling (stream P), and native Windows runs.
