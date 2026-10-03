# Ownership

Every path in the repository has **exactly one owning stream**. The machine-readable map is
[ownership.json](ownership.json); CI enforces it in two ways:

- **Path guard** (`scripts/ci/path-guard.mjs`): a pull request from `s/<stream>/<topic>` may only
  change that stream's paths, plus the lockfiles (`Cargo.lock`, `pnpm-lock.yaml`).
- **Ownership audit** (`scripts/ci/ownership-audit.mjs`): every tracked file must match exactly one
  stream's globs. A new file outside every glob, or inside two, fails CI.

Globs: `*` matches within one path segment, `**` across segments.

## Map

| Stream | Owns |
|---|---|
| **0** Contracts and skeleton | Root manifests and configs (`Cargo.toml`, `package.json`, `pnpm-workspace.yaml`, `rust-toolchain.toml`, `clippy.toml`, `deny.toml`, dotfiles), root docs (`README`, `AGENTS.md`, `CLAUDE.md`, `LICENSE`, `NOTICE`, `CONTRIBUTING`, `CODE_OF_CONDUCT`, `SECURITY`), `.github/workflows/ci.yml`, `fuzz.yml` and the other `.github` files, `scripts/`, `.cargo/`, `.vscode/`, `docs/*.md`, `crates/protocol`, `crates/interfaces`, `crates/fixtures`, `crates/daemon`, `crates/store/migrations/00*` (and `*.md` there), `apps/mock-hub`, `packages/tokens`, `packages/protocol-ts`, `docs/adr`, `docs/build` |
| **A** Ingest | `crates/ingest/**` |
| **B** Runtime | `crates/runtime/**`, `crates/ptyd/**` |
| **C** Store | `crates/store/*` (manifest, README, `build.rs`), `src`, `tests`, `benches`, `crates/store/migrations/01*` |
| **D** Runner service | `crates/runner/**` |
| **E** Hub: work model | `crates/hub-work/**`, `crates/store/migrations/02*` |
| **F** Recap and back office | `crates/recap/**`, `crates/office/**`, `crates/store/migrations/03*` |
| **G** Integrations | `crates/sync-github/**`, `crates/sync-jira/**`, `crates/store/migrations/04*` |
| **H** API and auth | `crates/api/**`, `crates/auth/**` |
| **I** Agent CLI and hooks | `crates/cli/**` |
| **J** Remote and HPC | `crates/remote/**` |
| **K** Desktop shell | `apps/desktop/**` |
| **L** UI foundation | `apps/ui/*` (package.json, Vite and TS config, index.html), `apps/ui/public/**`, `apps/ui/src/*` (entry files), `apps/ui/src/{design,shell,data,lib,assets}/**`, `apps/ui/{tests,e2e}/**` |
| **M** UI: Agent console | `apps/ui/src/console/**` |
| **N** UI: Projects layout | `apps/ui/src/projects/**` |
| **O** Onboarding and import | `apps/ui/src/onboarding/**`, `crates/legacy/**`, `crates/store/migrations/05*` |
| **P** Packaging and release | `packaging/**`, `.github/workflows/release*.yml`, `benches/**` |
| **Q** Security | `docs/security/**`, `fuzz/**` |

The shared API conformance suite (`tests/conformance/**`) is also owned by stream **0**.

## Rules that make this work

- **Need something in a shared file?** Don't edit it. Expose instead:
  - a Rust crate exports `routes()` for the API crate to compose, and its services for the daemon
    to wire (`crates/daemon`, stream 0);
  - a UI feature exports its routes and navigation entries from its folder's `index.ts`, and the
    shell (L) composes them;
  - database changes go in your own migration number range.
- **Need a new dependency?** Add it to your crate's `Cargo.toml` (or `apps/ui/package.json` via L).
  If it is already in `[workspace.dependencies]`, use `name.workspace = true`. To add a shared
  version, ask the integrator.
- **Need a contract change?** Open a `s/0/contract-<topic>` pull request, or describe it in your
  report and the integrator will make it. See [contracts.md](contracts.md).
- **Branches `integrator/<topic>`** may touch anything; only the integrator uses them.
- **Dependabot** branches may touch only manifests, lockfiles and workflow files.
