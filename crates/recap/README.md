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

Everything is pure and deterministic. Event text is untrusted: it is cleaned (control and
direction-changing characters removed) and capped before it is kept, and counts, files, facts,
tasks and receipts per block are capped by `Config`.

## Tests

- `tests/demo.rs`: snapshots of the blocks, lines and day paragraphs for the demo workspace.
- `tests/rules.rs`: one scenario per rule.
- `tests/incremental.rs`: property tests (batching never changes the blocks; hostile input never
  panics) and the receipt checks.
- Both `demo.rs` and `incremental.rs` check that two independent runs give byte-identical JSON.
  Every internal map hashes with its own random seed, so output order must never depend on map
  order: anything taken from a map is sorted first.
- `tests/perf.rs`: 100,000 generated events in under 500 ms. It runs only in release:
  `cargo test --release -p pitcrew-recap --test perf -- --nocapture`.
