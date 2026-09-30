# Stream O · Onboarding and import

**Goal:** from install to a working workspace in minutes: pick machines, connect, sign in to
agents, scan, turn history into projects and workstreams, import sessions, install hooks. Plus
an importer from the predecessor portal.

**Owns:** `apps/ui/src/onboarding/**`, `crates/legacy/**`, `crates/store/migrations/05*`.
**Depends on:** L; A (scan); C.  **Model:** Sonnet-class.
**Read first:** ADR-0009, ADR-0010, ADR-0007.

## Work packages

1. **First-run wizard:** welcome (theme, density) → first workspace and its primary machine
   (this computer, a WSL distro, an SSH host) → machine check with a fix button per row (CLIs and
   versions, tmux, git and gh, disk, SLURM) → install the helper (launcher, SLURM preview, live
   log) → sign in to agents (runs each CLI's own login in a terminal on that machine) →
   integrations (skippable) → scan → create projects and workstreams from the scan (template:
   Research, Software, Blank) → import sessions (all, by filter, or start fresh; read in place,
   reversible) → optional "draft the board from history" (cost shown first) → hooks (diff first)
   → safety (permission mode, back-office caps) → Home.
2. **Machine-setup wizard** reused for adding machines later; **Scan again** adds more sessions.
3. **Legacy importer** (`crates/legacy`): read the predecessor portal's state directory and map
   chapters → projects, cards → workstreams, candidate cards → idea workstreams, items → asks /
   decisions, tasks, dispatches, queue, links, labels, messages; dry run with a report first;
   never touches the old files.
4. Propose the scan and machine-setup API contracts with A and J (see `contracts.md`).

## Acceptance

- The wizard runs end to end against the mock hub (Playwright), including skip paths.
- Legacy import of a synthetic legacy state (fixtures inside `crates/legacy/tests/`) produces the
  expected events; the dry-run report matches.
- Nothing is written to agent configs or old files without an explicit confirmation step.

## Do not

Read or copy CLI OAuth tokens; commit any real legacy state.
