# Brief K · Tray, notifications and deep links

- **Stream:** K · Desktop shell. **Branch:** `s/K/tray-notifications-links`. **Paths:**
  `apps/desktop/**`.
- **First read:** [README.md](README.md), [K-shell-and-gateway.md](K-shell-and-gateway.md) (merged,
  with its review in mind), `docs/build/streams/K.md` (work package 5),
  [the desktop gateway contract](../contracts/desktop-gateway.md) (the new "Navigation from outside
  the window" section), [API v1](../contracts/api-v1.md) (`GET /v1/me`, asks, the stream), and
  `apps/desktop/src-tauri/README.md`.

## Goal

PitCrew can sit in the background and tell the person when an agent needs them. A link from
anywhere opens the right place in the app.

## What to build

1. **Tray:** an icon with a menu:
   - "Open PitCrew", which shows and focuses the window;
   - one line per workspace with its "needs you" count, which opens that workspace's Inbox;
   - "Quit", which stops the daemon only if the app started it, as today.
   - Closing the window keeps the app in the tray. Say so once with a notification, and offer a
     setting to quit on close instead.
   - On Linux, if no tray is available (some desktops), closing the window quits, as today.
2. **"Needs you" from the gateway side.**
   - For each ready workspace, the gateway keeps its own stream subscription to the daemon (Rust
     side, with the token it already holds).
   - It tracks open asks addressed to the person, found through `GET /v1/me`, from `ask_raised`
     and `ask_answered`, after an initial `GET` of the asks.
   - **Reconnect** with `since`, as the UI does.
   - **Bound** memory and work: at most one subscription per workspace, and a bounded set of
     tracked asks.
3. **OS notifications** for new asks addressed to the person, when the window isn't focused.
   - **Title:** the asking agent and the kind (question, decision, review, approval).
   - **Body:** the ask's text, cleaned (control and bidi characters removed) and capped at 200
     characters.
   - **Clicking it** navigates to the ask's place: `gateway://navigate` to `/w/<ws>/inbox`, or to
     the task (the contract's typed `NavigateTarget`).
   - **Rate-limit** them, so a burst becomes one notification ("3 agents need you").
   - A setting turns notifications off. The tauri notification plugin is fine, with minimal
     permissions.
4. **Deep links** `pitcrew://…`:
   - Register the scheme on all three platforms.
   - Handle a link both on first launch and when the single-instance plugin forwards one.
   - Parse it strictly into the contract's allowed shapes, then emit `gateway://navigate` with a
     `NavigateTarget` and focus the window.
   - Drop anything else, logged shortened.
   - A deep link never acts. Test the parser with hostile input: other schemes, `..`, encoded
     characters, query strings, very long input, unknown shapes.
5. **Capabilities:**
   - `gateway://navigate` is emitted by the app only;
   - the webview gets no new commands, except reading or writing the two settings if you add
     commands for them (keep those minimal);
   - notification permission is requested from the Rust side.

## Acceptance

- **Tests:**
  - the deep-link parser, as a table including hostile cases;
  - "needs you" tracking against a fake daemon: raised, answered, reconnect with `since`, the
    bound, and asks for other members ignored;
  - notification rate limiting;
  - the navigate event's payload;
  - `no_token` still passes and covers the new subscription.
- **Run in WSLg against `pitcrewd serve --demo`:**
  - the tray shows counts;
  - raising an ask through the API (a device token from the daemon's token file, in your test
    script only) gives a notification;
  - `xdg-open 'pitcrew://w/<ws>/task/<id>'` focuses the app.
  - Report what WSLg supports. A missing tray in WSLg is fine; say so.
- fmt, clippy (Linux, and the Windows target with your `windres` stand-in), tests, `cargo deny`,
  and the guards all pass.

## Out of scope

Remote workspaces and pairing (after J's tunnel), the updater (stream P), and the UI's handling of
`gateway://navigate` (stream L, a follow-up; until then, test with the event alone).
