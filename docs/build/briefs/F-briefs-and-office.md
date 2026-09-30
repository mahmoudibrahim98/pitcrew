# Brief F · "Where it stands" proposals and the back office's rules

- **Stream:** F · Recap and back office. **Branch:** `s/F/briefs-and-office`. **Paths:**
  `crates/recap/**`, `crates/office/**`, `crates/store/migrations/03*`.
- **First read:** [README.md](README.md), then `docs/build/streams/F.md` (work packages 3–6),
  ADR-0007, the merged `crates/recap`, `crates/store` (Projection rules in its README), and
  `Brief`, `BriefTarget`, `BriefProposed` and `Receipt` in `crates/protocol`.

## Goal

1. Propose each workstream's and project's **"Where it stands"** from recent activity, with
   receipts.
2. The **back office's rule engine**: small, deterministic rules that act on evidence, with caps
   and a run log. Model calls come later, behind the existing `Summarizer` trait.

## What to build

1. **Brief proposals** (`crates/recap`):
   - from the blocks and day recaps of a workstream, draft a proposed brief (text + `next` +
     receipts) and a project roll-up from its workstreams;
   - rule-based, deterministic, and every claim carries a receipt (reuse `Summary` spans);
   - emit the `BriefProposed` body; the caller appends it;
   - **pinned** briefs only get proposals; unpinned ones may be marked for automatic
     acceptance by policy.
2. **The office** (`crates/office`), as a rule engine over new events:
   - a `Rule` trait: `fn on_event(&mut self, ctx, event) -> Vec<Action>`;
   - `Action`s: append a body, raise an ask, or propose a brief. They are **applied by the
     caller through a `Commands` trait** that stream E will implement, so office never writes
     the store directly.
   - First rules:
     - a dispatch finished successfully → propose moving its task to review (`Mover::BackOffice`,
       respecting `can_move`);
     - a job diverged, or tests kept failing → raise a decision ask to the owner, with receipts;
     - an ask open for more than N hours → a reminder mention;
     - a workstream with no activity for N days while in progress → propose "paused?" in a
       brief proposal.
   - **Caps:** at most N actions per rule per hour, and a global cap; overflow is logged, not
     applied.
   - **A run log:** a projection table (migrations 03xx) with rule, event, actions and outcome,
     so people can audit what the office did.
   - **The hard "never" list**, enforced in code: never send anything outward (GitHub or Jira),
     never mark a task done unless `accept_auto`, never answer asks addressed to people.
3. Rules are deterministic given the events; time comes from event timestamps, not the clock.

## Acceptance

- Snapshot tests over the fixture events: proposals and office actions.
- Property test: replaying the same events gives the same actions (idempotent), and caps hold
  under a flood.
- A test for each "never" rule: a crafted event can't make the office violate it.
- The run-log projection rebuilds identically.

## Out of scope

Model-backed prose, notifications, applying actions (stream E's `Commands`).
