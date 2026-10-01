# pitcrew-recap

Recap engine: activity blocks, summaries with receipts, and Where-it-stands proposals.

**Owned by stream F** — see [docs/build/streams/F.md](../../docs/build/streams/F.md).

## What is here

- **Blocks.** `blocks(events, &directory, &config)` groups events (in log order) into bursts of
  one session's work, or of one workstream's work outside any session, split by pauses longer
  than `config.gap_ms` (20 minutes by default). Each `Block` has its links (session, workstream,
  project, tasks, agent), counts, files touched and notable facts ("tests failed then passed",
  "job diverged", "moved PAP-1 to review"), and every fact carries receipts. `BlockBuilder` does
  the same incrementally; `push_batch` reports the blocks that changed and closed, and any
  batching gives the same blocks as `blocks`.
- **Directory.** What is known before the first event (members, workstreams, tasks, sessions,
  dispatches, asks), seeded from projections with `add_*` and kept current from events.
- **Summaries.** `draft_line` (one block) and `draft_paragraph` (e.g. one workstream's day, see
  `days`) turn blocks into clauses with receipts. A `Summarizer` writes the prose:
  `RuleSummarizer` is the default, `FakeSummarizer` stands in for a model in tests, and `verify`
  checks that every span of a summary cites receipts from its draft. `day_recaps` does it all
  for a list of blocks.
- **Where it stands.** `standing(workstream, &blocks, &directory)` reads a workstream's recent
  blocks into what is true now: its state ("Seed runs is at risk"), where each task is ("PAP-1
  is in progress (2 of 4 steps done)"), checks, diverged jobs and the latest decision, open asks
  ("waiting on @sam to decide …"), and the most pressing next step. Every point is a clause with
  the receipts of the facts behind it. `propose_workstream` writes it as a `BriefProposal`
  through a `Summarizer` (verified like any summary), and `propose_project` rolls a project's
  standings up, with the most pressing next step of them all. `BriefProposal::body()` is the
  `BriefProposed` event the caller appends, with the next step ("Review PAP-3.") in its own
  `next` field. Pinned briefs only get proposals; an unpinned one is marked `AutoAccept` when the
  workspace's `BriefPolicy` allows it, and `accepted_body()` gives the `BriefAccepted` to append
  after it: the same text and next step, with the proposal's receipts. A proposal that says what
  the brief in force, or its pending proposal, already says (text and next step) is not made.
  `propose_paused` is the back office's "paused?" question for a quiet workstream.

Everything is pure and deterministic. Event text is untrusted: it is cleaned (control and
direction-changing characters removed) and capped before it is kept, and counts, files, facts,
tasks and receipts per block are capped by `Config`.

## Tests

- `tests/demo.rs`: snapshots of the blocks, lines and day paragraphs for the demo workspace.
- `tests/rules.rs`: one scenario per rule.
- `tests/briefs.rs`: a snapshot of the demo workspace's proposals (workstreams and projects),
  scenarios for each "Where it stands" rule, pinned and automatic acceptance, and a property test
  that every proposal over any generated log verifies and cites only receipts from it.
- `tests/incremental.rs`: property tests (batching never changes the blocks; hostile input never
  panics) and the receipt checks.
- Both `demo.rs` and `incremental.rs` check that two independent runs give byte-identical JSON.
  Every internal map hashes with its own random seed, so output order must never depend on map
  order: anything taken from a map is sorted first.
- `tests/perf.rs`: 100,000 generated events in under 500 ms. It runs only in release:
  `cargo test --release -p pitcrew-recap --test perf -- --nocapture`.
