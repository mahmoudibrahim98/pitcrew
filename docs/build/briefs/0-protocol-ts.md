# Brief 0 · TypeScript types generated from the protocol crate

- **Stream:** 0 · Contracts (stream 0 owns `crates/protocol` and `packages/protocol-ts`; stream H
  proposed this). **Branch:** `integrator/protocol-ts`.
  **Paths:**
  - `crates/protocol/**` (derives behind a feature);
  - `packages/protocol-ts/**`;
  - a new workflow file under `.github/workflows/` for the freshness check;
  - the root `Cargo.toml` and `Cargo.lock`, and the root `package.json` and lockfile if the package
    joins the pnpm workspace.
- **First read:**
  - [README.md](README.md) and the root `AGENTS.md` (or `CLAUDE.md`);
  - `docs/build/streams/H.md` (item 7) and `docs/build/streams/0.md` ("Next contract work");
  - `crates/protocol`'s README;
  - `docs/build/contracts/api-v1.md`;
  - how the UI declares its API types today (`apps/ui/src/data/`).
- **Suggested agent:** Codex, or any coding agent.

## Goal

The UI keeps hand-written copies of the protocol's types, which can drift from the Rust. Generate
TypeScript from the Rust types, and make CI fail when the generated files are stale. Switching the UI
over is a later brief; this one reports how far the two differ today.

## What to build

1. **Derives behind a `ts` feature** in `crates/protocol`, using `ts-rs` (or `specta` if it fits
   better; say why):
   - cover every type the API sends or receives (`api.rs`, `model.rs`, the ids, events, errors);
   - serde attributes (`rename_all`, `tag`, `skip_serializing_if`, `Option`) must come out as the
     JSON really looks: an optional field is `field?: T`, not `T | null`, when the field is skipped
     rather than sent as null;
   - the feature is off by default, so the daemon's build doesn't change.
2. **Export into `packages/protocol-ts/`:**
   - one command regenerates it, e.g. `cargo test -p pitcrew-protocol --features ts export_bindings`;
   - the generated `.ts` files, an `index.ts`, and a `package.json` (`@pitcrew/protocol-ts`, private,
     no runtime code);
   - document the command in the package's README.
3. **A freshness check in CI:** regenerate, then `git diff --exit-code packages/protocol-ts`, in a new
   workflow file (to keep away from other briefs editing `ci.yml`).
4. **A drift report:** compare the generated types with the UI's hand-written ones. List each
   mismatch in the report as a table:
   - the type and field;
   - what the Rust sends;
   - what the UI declares;
   - which one the contract (`api-v1.md`) agrees with.

   Don't change the UI.

## Acceptance

- `tsc --noEmit` passes on the generated package.
- A test shows a few representative types round-trip: a Rust value serialised to JSON
  type-checks against the generated TypeScript (e.g. a vitest/tsd check with fixture JSON).
- The freshness check fails if the Rust types change without regenerating. Show it once.
- fmt, clippy (`--features ts` too), the guards, and every CI job pass.

## Out of scope

Switching the UI to the generated types, and changing any wire format.
