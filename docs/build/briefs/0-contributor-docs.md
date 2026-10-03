# Brief 0 · Docs for contributors: architecture and getting started

- **Stream:** 0 · Contracts (root docs and `docs/*.md` are stream 0's). **Branch:**
  `integrator/contributor-docs`.
  **Paths:** `docs/architecture.md` and `docs/getting-started.md` (both new), `README.md` and
  `CONTRIBUTING.md` (links and short fixes only).
- **First read:**
  - [README.md](README.md) and the root `AGENTS.md` (or `CLAUDE.md`);
  - the root `README.md` and `CONTRIBUTING.md`;
  - every crate's and app's README;
  - `docs/build/ownership.md` and `docs/build/contracts/*.md`;
  - `docs/security/threat-model.md` (trust boundaries).
- **Suggested agent:** Codex, or any coding agent.

## Goal

The repository is public. Someone new should understand how PitCrew fits together, and get it
building and running, without reading 90 briefs.

## What to write

1. **`docs/architecture.md`:**
   - **the processes and how they talk:** the desktop app (webview, then gateway), `pitcrewd` (API,
     hub, runner, office), `pitcrew-ptyd`, tmux, the agent CLIs and their hooks, the remote helper
     over SSH (tunnel, launchers, SLURM), and `pitcrew` (the agent CLI);
   - **a crate and app map:** one line each, and which stream owns it;
   - **the data flow:** transcripts, then adapters, events, the store, projections, and the API
     with its delta stream to the UI;
   - **trust boundaries:** in a few sentences each, linking the threat model rather than repeating
     it;
   - **one diagram** as a mermaid block (GitHub renders it).
2. **`docs/getting-started.md`:**
   - prerequisites per OS (Linux, macOS, Windows), taken from the CI workflows and the READMEs;
   - build and test: cargo, the UI with pnpm, the guards;
   - run the demo: `pitcrewd serve --demo` with a fresh `--state-dir`, the UI dev server against it
     and against the mock hub, and the desktop app in dev mode;
   - where to read next: the briefs README for status, and the contracts.
3. **Verify every command you write** in the environment, and say in the report which ones you could
   not run (e.g. macOS, Windows, the desktop app without a display).
4. **Link the new docs** from `README.md` and `CONTRIBUTING.md`.

## Rules

- Describe what exists today. Where something is planned but not built (see the stream cards),
  say "not built yet" or leave it out; never present it as working.
- Synthetic examples only (`/home/sam`, `example.com`).

## Acceptance

- Every command in `getting-started.md` was run, or is marked as not verified, with the reason.
- Mermaid renders (valid syntax), and links resolve.
- The guards pass, and every CI job passes on the pull request.

## Out of scope

User-facing product docs, and changes to code.
