# Brief 0 · Onboarding on the real API: first run and remote workspaces

- **Stream:** 0 · Contracts (integration work across L and O). **Branch:**
  `integrator/onboarding-wiring`. **Paths:** `apps/ui/src/data/**`, `apps/ui/src/shell/**`,
  `apps/ui/src/onboarding/**`, `apps/ui/e2e/**`.
- **First read:** [README.md](README.md), `docs/build/contracts/api-v1.md` ("Host and workspace",
  "The first run: `POST /v1/setup`"), `docs/build/contracts/desktop-gateway.md` (all of it,
  especially "Remote workspaces" and "Prompts"), the READMEs of `apps/ui/src/data`,
  `apps/ui/src/shell` and `apps/ui/src/onboarding` (its "Open question" and "Why the first-run
  wizard is not at a pre-workspace `/onboarding`"), and `apps/mock-hub/README.md` (fresh mode).

## Goal

A person opening a fresh hub is taken through setup and lands on Home as themselves. In the
desktop app, they can connect a remote machine (a server, a login node or a SLURM node). They see
exactly what will happen before anything changes on the remote, answer SSH's questions in the
app, and then work in that machine's workspace. **Nothing fake is shown outside tests and an
explicit development flag.**

## What to build

1. **Data** (`src/data`):
   - `setup_needed` on the workspace type.
   - A setup mutation, `POST /v1/setup`, which refreshes the workspace, `me`, members and
     machines.
   - The gateway's remote commands and events on the `Gateway` interface, desktop only:
     - `sshHosts`, `remoteProbe`, `remotePlan`, `remoteAdd(plan, onProgress)` (a `Channel`), and
       `workspaceRemove(workspace, stopHelper)`;
     - `onPrompt`, `onPromptClosed` and `replyPrompt(id, { answer } | { accept } | {})`.
     - Every incoming payload is checked, as `isWorkspace` does; a malformed one is dropped.
     - In a browser they don't exist, and the UI says so rather than failing.
2. **Shell** (`src/shell`):
   - **A redirect:** a workspace with `setup_needed` opens the first-run wizard, in the browser and
     in the desktop, without a loop (the wizard's own routes are exempt, and a finished setup
     flips the flag).
   - **The prompt dialog,** mounted once in the desktop app:
     - It shows which host is asking, and `text` as plain text (never HTML). A password,
       passphrase or code goes in a password field with autocomplete off. A host key shows its
       fingerprint, with Accept and Reject.
     - Cancel replies with neither field. Prompts from several hosts queue. A `prompt-closed`
       withdraws its prompt.
     - **The answer** lives only in the dialog's local state. It is cleared once sent, and never
       logged or put in a store, a query cache, the URL or storage.
   - **Remote workspaces in the switcher:** "Connect a remote machine…" opens the wizard below.
     "Remove workspace…" (remote ones only) asks for confirmation, with "Also stop PitCrew on
     the remote (cancels its SLURM job)" as an option.
   - The desktop's "No workspaces yet" screen offers the same connect action. If a route outside
     `/w/$ws` is needed, add the `Feature.rootRoutes` hook that the onboarding README proposes.
3. **Onboarding** (`src/onboarding`):
   - **A real `OnboardingApi`** (`createHubOnboardingApi`):
     - `setupWorkspace` sends `POST /v1/setup`. The workspace step gains your name and your
       handle (suggested from the name, editable) and this machine's name.
     - `discoverHosts` uses `sshHosts` in the desktop.
     - Every call with no backend yet is marked unavailable, and `stepsFor` leaves its step out.
       So the real first run is: Welcome, Workspace, Done, then Home. The other steps come back
       as their routes land.
     - The fake stays, for tests and for a development-only flag (e.g. `?onboarding=fake` in a
       dev build). A production build never uses it.
   - **Validation** mirrors the contract: trimmed names, lengths in code points, the handle's
     shape, and no control characters. The server's `400` and `409` appear next to the right
     field; `409` on an already set-up workspace goes to Home.
   - **The connect-a-remote wizard** (desktop only):
     1. **Host:** pick one from `sshHosts`, or type one; the contract allows typing. A typed host
        is checked first: no leading `-`, no whitespace or control characters.
     2. **Probe:** OS and architecture, whether PitCrew is there and running, and SLURM's
        version and default partition.
     3. **Launcher:** `direct`, `tmux` or `slurm`. For slurm: the site recipe, partition,
        account, QoS, time, CPUs, memory and GPUs.
     4. **Review:** the plan's `steps`, and for slurm the exact `jobScript`, verbatim, in a
        monospace block. "Nothing changes on the remote until you press Connect."
     5. **Connect:** progress from `remoteAdd`. A failure shows its step and detail. SSH's
        questions arrive through the shell's prompt dialog.
     6. **Setup:** if the new workspace has `setup_needed`, run the workspace step against it
        (through its gateway transport).
     7. **Done:** open the new workspace.
     - **An expired or used plan** (`invalid`) goes back to Review with a fresh plan. A new plan
       is never submitted without being shown.
   - Retire or rename the old "Add a machine" wizard and its palette command, so nothing offers
     the fake-backed flow in production. Note in the README what remains proposed: the machine
     check, scan, sign-in and hooks APIs.

## Acceptance

- **Vitest:**
  - the setup mutation and the remote client, over a mocked `invoke`, `Channel` and `listen`,
    including dropped malformed payloads;
  - the prompt dialog:
    - an answer is sent once and cleared;
    - `text` containing markup renders literally;
    - Cancel, withdrawal, and two queued prompts;
    - a test that the answer appears in no store or cache after the reply;
  - the redirect, with no loop;
  - the wizard against a mocked gateway:
    - probe, plan, Review showing the exact script, connect with progress, a prompt round trip,
      and the new workspace opened;
    - an expired plan going back to Review;
    - a cancelled prompt failing the connect cleanly;
    - a typed host starting with `-` refused.
- **Playwright** against the mock in fresh mode (`PITCREW_MOCK_FRESH=1`): the first run from an
  empty workspace to Home, after which `GET /v1/me` is the new person. The existing e2e suites
  still pass in demo mode.
- axe reports no violations on the new screens and the dialog, light and dark.
- `typecheck`, `lint`, `test` and `build` pass, and onboarding stays lazy-loaded.

## Out of scope

The gateway's Rust side (stream K), the daemon (stream 0), and the routes the other wizard steps
need (machine check, scan, sign-in, hooks).
