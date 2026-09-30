# Building PitCrew

PitCrew is built in **parallel streams**. Each stream is one agent (or person) in its own
worktree and branch, owning its own paths, working against shared contracts and mocks (ADR-0011).

| Document | What it covers |
|---|---|
| [workflow.md](workflow.md) | How a stream works: branches, the worker brief, checks, reports, what the integrator does |
| [ownership.md](ownership.md) | Who owns which paths ([ownership.json](ownership.json) is what CI enforces) |
| [contracts.md](contracts.md) | The contracts every stream codes against, and how to change them |
| [contracts/api-v1.md](contracts/api-v1.md) | The daemon's HTTP and WebSocket API |
| [streams/](streams/) | One card per stream: goal, paths, dependencies, work packages, acceptance, what not to touch |
| [briefs/](briefs/) | Ready-to-run assignments: tell an agent "Follow `docs/build/briefs/<brief>.md`" |

## The streams

| # | Stream | Paths | Depends on | Suggested model |
|---|---|---|---|---|
| [0](streams/0.md) | Contracts and skeleton (integrator) | manifests, `crates/{protocol,interfaces,fixtures,daemon}`, `apps/mock-hub`, CI, docs | — | Opus-class |
| [A](streams/A.md) | Ingest | `crates/ingest` | 0 | Opus-class |
| [B](streams/B.md) | Runtime | `crates/runtime`, `crates/ptyd` | 0 | Opus-class |
| [C](streams/C.md) | Store | `crates/store` (migrations 01xx) | 0 | Sonnet-class |
| [D](streams/D.md) | Runner service | `crates/runner` | 0; A and B via traits | Opus-class |
| [E](streams/E.md) | Hub: work model | `crates/hub-work` (migrations 02xx) | 0, C | Opus-class |
| [F](streams/F.md) | Recap and back office | `crates/recap`, `crates/office` (03xx) | 0, C | Opus-class |
| [G](streams/G.md) | Integrations | `crates/sync-github`, `crates/sync-jira` (04xx) | 0, E | Sonnet-class |
| [H](streams/H.md) | API and auth | `crates/api`, `crates/auth` | 0 | Opus-class |
| [I](streams/I.md) | Agent CLI and hooks | `crates/cli` | 0, H contract | Sonnet-class |
| [J](streams/J.md) | Remote and HPC | `crates/remote` | 0 | Opus-class |
| [K](streams/K.md) | Desktop shell | `apps/desktop` | 0, J and H interfaces | Opus-class |
| [L](streams/L.md) | UI foundation | `apps/ui` config, `src/{design,shell,data}` | 0 | Opus-class |
| [M](streams/M.md) | UI: Agent console | `apps/ui/src/console` | L, mock hub | Opus-class |
| [N](streams/N.md) | UI: Projects layout | `apps/ui/src/projects` | L, mock hub | Sonnet-class |
| [O](streams/O.md) | Onboarding and import | `apps/ui/src/onboarding`, `crates/legacy` (05xx) | L, A (scan), C | Sonnet-class |
| [P](streams/P.md) | Packaging and release | `packaging`, release workflows, `benches` | 0 | Sonnet-class |
| [Q](streams/Q.md) | Security (reviewer) | `docs/security`, `fuzz` | 0 | Opus-class |

## What depends on what

```
Stream 0 (contracts) ──► A · B · C · H · J · L · P · Q              start the same day
                     └─► D (A, B via traits) · E, F (C via schema) · I (H contract) · K (J, H)
                     └─► M, N (L + mock hub) · O (L, A scan, C) · G (E interfaces)
```

Only stream 0 blocks the others. A comfortable number of agents at once is 4–6; the limit is
review capacity.

## Milestones

Checkpoints the integrator assembles. They do not block work.

| Milestone | Assembled from | Done when |
|---|---|---|
| **M1 · Local console** | A B C D H I K L M | On one computer, local agents appear with live terminals and chat; you can start, resume, send, answer and end |
| **M2 · Remote and HPC workspace** | + J P | The laptop app sets up a SLURM cluster with no manual step (helper uploaded, SLURM launcher, site recipe); it survives sleep, a VPN drop and a job hand-over |
| **M3 · Projects live** | + E F N | Projects, workstreams and tasks with agents on them; tasks that move themselves; "Where it stands" with receipts |
| **M4 · Onboarding and integrations** | + G O | The wizard, scan and import; GitHub and Jira both ways; the legacy import |
| **M5 · v0.1** | all + Q review | Parity checklist green, budgets met, signed releases, two weeks of dogfooding |
| **M6 · People** | hub server mode | A second person joins; shared projects; hand-offs between people |

## Quick start for a stream agent

```bash
git switch -c s/<stream>/<topic>
cargo test --workspace          # Rust
npm test                        # mock hub and CI scripts
npm run mock-hub                # the fake daemon on http://127.0.0.1:47317 (UI streams)
```

Read your card in `streams/`, the ADRs it cites, and `contracts.md`. Then work only in your paths.
