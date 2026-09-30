# Stream F · Recap and back office

**Goal:** answer "what happened, and where does it stand?" with **receipts**, and run the back
office that keeps briefs and statuses current without being asked.

**Owns:** `crates/recap/**`, `crates/office/**`, `crates/store/migrations/03*`.
**Depends on:** stream 0, C (recorded events are enough to start).  **Model:** Opus-class.
**Read first:** ADR-0007, ADR-0006; `Brief`, `BriefTarget`, `Receipt` in the protocol.

## Work packages

1. **Activity blocks** (`crates/recap`): group events per session and workstream into blocks
   (a burst of work with a start, an end and what changed), each with receipts.
2. **Summaries:** a one-line and a paragraph summary per block and per day, cached with the
   event range they cover. Rules first (counts, files, tests, jobs); a model call only for prose,
   behind a `Summarizer` trait with a deterministic fake.
3. **"Where it stands" proposals:** from recent blocks, propose a workstream brief and roll up a
   project brief; emit `brief_proposed` with receipts. Pinned briefs only get proposals; unpinned
   ones may be applied automatically if the workspace allows it.
4. **Back office** (`crates/office`): rules that act on evidence, e.g. move a task to review when
   its dispatch reports done, raise a decision ask when a job fails or diverges, nudge on stale
   asks. Model calls run the CLI on the primary machine (e.g. `claude -p`) for judgement only.
   **Caps** (calls and tokens per day), a **run log**, and a hard "never" list (never send
   outward, never mark done unless the task allows automatic acceptance).
5. **Prompts as files:** versioned templates under `crates/office/prompts/` per template
   (Research, Software).
6. **Per-member read cursors** so recaps can say "since you last looked".

## Acceptance

- From the fixture events, rule-only recaps are deterministic (snapshot tests).
- Every sentence in a proposal maps to at least one receipt.
- Caps stop model calls and log why; the fake summarizer counts calls.
- The back office never produces a move that `can_move` rejects.

## Do not

Change tasks directly (append events through E's commands), or call external services.
