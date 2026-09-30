# Brief F · Activity blocks and rule-based recaps

- **Stream:** F · Recap and back office. **Branch:** `s/F/activity-blocks`. **Paths:**
  `crates/recap/**` only (`crates/office` and migrations come later).
- **First read:** [README.md](README.md), then `docs/build/streams/F.md` (work packages 1 and 2),
  ADR-0007, `crates/protocol/src/{events,model}.rs` (`Event`, `EventBody`, `Receipt`, `Brief`),
  and the fixture events in `crates/fixtures/data/demo-workspace.json`.

## Goal

Turn a stream of events into **activity blocks** (bursts of work, each with what changed and
receipts) and **rule-based one-line and paragraph summaries**, without any model call and
without a database yet. This is the core of "what happened lately", and it is pure and
deterministic.

## What to build

1. **Blocks.** A pure function over events in revision order, e.g.
   `blocks(events) -> Vec<Block>`, plus an incremental builder that accepts new events and
   updates or closes blocks. A block groups one session's (or one workstream's) events separated
   by less than a gap (e.g. 20 min, configurable). It records:
   - start and end;
   - session, workstream and task links;
   - counts: tools run and failed, files edited (+/-), turns, asks raised and answered, task
     moves;
   - the files touched (capped);
   - notable facts, e.g. "tests failed then passed", "job diverged", "moved to review";
   - **receipts** for each fact (transcript offsets, events).
2. **Summaries**, rule-based and deterministic:
   - one line per block (e.g. "@writer drafted §3.2 in method.tex (+84 −12), 3 tool runs, moved
     PAP-1 to review");
   - a paragraph per workstream per day.

   **Every clause maps to at least one receipt**; expose the mapping, e.g.
   `Summary { text, spans: Vec<(Range<usize>, Vec<Receipt>)> }`.
3. **A `Summarizer` trait** for later model-backed prose. The rule-based one is the default;
   a deterministic fake is provided for tests.
4. **Limits:** all inputs are untrusted text; cap lengths and counts; never panic.

## Acceptance

- Snapshot tests (`insta`) over the fixture events: blocks and summaries.
- **Property test:** feeding events in random batch sizes to the incremental builder gives the
  same blocks as the pure function over all of them.
- Every summary span has at least one receipt, and every receipt points at a real event or
  offset in the input (a test enforces it).
- 100,000 generated events build blocks in under 500 ms in release (report the number).

## Out of scope

The back office (`crates/office`), storing blocks (projections come with brief C-projections),
model calls, "Where it stands" proposals (next F brief).
