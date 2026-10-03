# Brief 0 · Files API: list, read and write inside a workstream's folders

- **Stream:** 0 · Contracts (contract, runner, daemon route, mock hub and conformance together).
  **Branch:** `integrator/files-api`.
  **Paths:**
  - `docs/build/contracts/api-v1.md`, `docs/security/threat-model.md`;
  - `crates/protocol/**` (types, runner commands; regenerate `packages/protocol-ts`);
  - `crates/runner/**` (the file operations themselves, with their tests);
  - `crates/hub-work/**` (only to resolve a workstream's location for the route), `crates/daemon/**`;
  - `apps/mock-hub/**`, `tests/conformance/**`;
  - the READMEs of the crates touched.
- **First read:**
  - [README.md](README.md) and the root `AGENTS.md` (or `CLAUDE.md`);
  - `docs/build/streams/D.md` (item 6 and its acceptance line) and `M.md` (item 5, the file viewer
    this API will serve);
  - `api-v1.md` (errors, the machine scan section as a model for a device-only route);
  - `crates/runner/README.md`, `crates/protocol/src/runner.rs`;
  - `docs/security/threat-model.md` (T27, T69: how ingest already refuses links and swapped files).
- **Suggested agent:** Codex, on a Windows machine if possible, since Windows path rules matter.

## Goal

The UI will show and edit files in a workstream's folders (the workbench's file viewer, a later
brief). This brief builds the API under it: list a folder, read a file, write a file, only inside the
folders a workstream names as its locations, on the hub's own machine.

## What to build

1. **The contract first,** in `api-v1.md`:
   - a root is named by a workstream and the index of one of its locations, never by a path from the
     client: for example `GET /v1/workstreams/{id}/files?loc=0&path=src` (list),
     `GET /v1/workstreams/{id}/files/content?loc=0&path=src/main.rs` (read),
     `PUT` on the same (write). Pick the shape; keep it small and say why;
   - `path` is relative to the root, with `/` separators;
   - **device tokens only:** an agent token gets 403 (agents already have the folder);
   - **read** returns the size, a media type, a revision (a hash of the bytes), and the content as
     UTF-8 text or base64. Cap the size (say 8 MiB); a bigger file gets 413 with its size, so the
     viewer can say so;
   - **list** returns names, kinds (file, folder, link), sizes and modification times, sorted, with
     a cap on the number of entries (say 5,000) and a `truncated` flag;
   - **write** takes the content and the revision the client read (or "must not exist" for a new
     file). A different revision on disk gets 409 with the current one. Cap the body;
   - errors: 400 for a bad path, 404 for an unknown workstream, location or file, 403 for a path that
     resolves outside the root, 501 for a location on another machine (see 4).
2. **The runner does the file work** (`crates/runner`), with the rules of D.6:
   - **refuse before touching the disk:** absolute paths, `..`, empty or `.` components, NUL,
     backslashes; on Windows also drive letters, UNC and `\\?\` prefixes, `:` (alternate data
     streams), and reserved device names (`CON`, `NUL`, `COM1`...);
   - **links:** walk the path one component at a time without following links. A symbolic link,
     junction or other reparse point inside the root may be listed (as a link) but never followed
     for a read or a write. Then check, after opening, that the file opened is the one checked (for
     example by comparing the canonical path, or the file id, with the root), so a swap between the
     check and the open is refused (the same idea as T69);
   - **writes:** refuse a file with more than one hard link, and anything under `.git/`. Write to a
     temporary file in the same folder, then rename over the target, keeping its permissions. Before
     replacing a file, back up the old bytes in the daemon's state folder (owner-only modes, a bounded
     number per file and a total size cap; say which);
   - **privacy:** never log paths or contents; log a count and a reason.
3. **The daemon** wires the routes: resolve the workstream's location, check it is on this machine,
   and call the runner's file operations. Keep the work off the async threads.
4. **Other machines:** a location on a remote or WSL machine answers 501 for now. If the remote link
   already carries runner commands, say in the PR how this would extend to it; don't build it.
5. **The mock hub** implements the routes over an in-memory tree; **conformance** covers both
   targets: list, read, write with the right and the wrong revision, 403 for an agent, `..` and an
   absolute path refused, 413, 501. On the daemon target, a link pointing out of the root must be
   refused (create it in the test's temporary folder; skip only where the OS refuses to create one,
   and say so).
6. **The threat model:** a row for the files API (squatting, links, hard links, swaps, backups'
   modes, `.git`), numbered after the highest row in use.

## Tests

- Table tests for the path rules, including the Windows-only cases on Windows.
- A link (symbolic link, and a junction on Windows) pointing outside: listed as a link, never read
  or written through; a file swapped for a link between check and open is refused.
- Writes: the revision check, the backup and its cap, the hard-link refusal, the permissions kept.

## Acceptance

- fmt, clippy with `-D warnings`, the tests of the crates touched, `cargo test -p pitcrew-protocol
  --features ts` (commit the regenerated `packages/protocol-ts`), `npm test`, the conformance suite
  against both targets, and the guards pass. Every CI job passes on the pull request.

## Out of scope

The UI (the file viewer is a later brief), files on remote machines, watching files for changes, and
searching file contents.
