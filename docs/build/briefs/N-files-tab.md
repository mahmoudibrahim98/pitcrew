# Brief N · A Files tab on the workstream page

- **Stream:** N · Projects layout.
  **Branch:** `integrator/files-tab`.
  **Paths:**
  - `apps/ui/src/projects/**` (the tab and its tests);
  - `apps/ui/src/data/**` (a client for the files routes);
  - `apps/mock-hub/**`, only if the e2e test needs seeded files.
- **First read:**
  - [README.md](README.md) and the root `AGENTS.md` (or `CLAUDE.md`);
  - `docs/build/contracts/api-v1.md`, the files routes (#37, merged): list, read, write, revisions,
    413, 409 and 501;
  - `apps/ui/src/projects/README.md` and `workstream-page.tsx` (five tabs today);
  - `docs/build/streams/M.md` item 5 (the file viewer this starts).
- **Suggested agent:** Codex, or any coding agent.

## Goal

A person can look at, and change, the files in a workstream's folders without leaving PitCrew.

## What to build

1. **A sixth tab, "Files",** on the workstream page:
   - a location picker when the workstream has more than one location; a location on another
     machine says it isn't supported yet (the API answers 501);
   - a folder tree that loads one level at a time, sorted as the API returns it, showing links
     as links (not openable) and saying when a listing was truncated.
2. **The viewer:**
   - text as monospace, with line numbers;
   - images (`image/png`, `image/jpeg`) as images;
   - anything else as "binary, N bytes";
   - a file over the cap (413) as "too large to show (N bytes)".
   - No raw HTML is ever rendered: text stays text.
3. **Editing text:**
   - Edit, then Save, sending the revision it read;
   - a 409 shows "changed since you opened it", with Reload and Overwrite (Overwrite reloads the
     revision first, then saves);
   - unsaved changes ask before switching files or tabs.
4. **States:** loading, empty folder, errors (403 shows "not allowed", 404 shows "gone"), keyboard
   navigation, both themes.

## Tests

- Unit tests for the tree, the viewer's choices and the save/conflict flow.
- An end-to-end test against the mock hub (seed files there if needed): browse, open text, edit and
  save, hit a conflict, open an image. Axe in both themes.

## Acceptance

- The UI's checks (typecheck, lint, test, build, e2e), `npm test`, and the guards pass. Every CI job
  passes on the pull request.

## Out of scope

PDF and TeX, search, creating or deleting files and folders, and files on remote machines.
