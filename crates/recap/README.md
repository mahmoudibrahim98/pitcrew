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
- **Who did it** (api-v1.md, "Sessions": "Who did it"). A session block's `agent` is its
  session's agent, never the person the runner's events are stamped with. When it has none (a
  session found on disk, or one a person started), the prose names the session itself, by its
  CLI and title (or folder): "Claude · Fix the parser ran 3 tools". Its start is the person's
  only when they started it from PitCrew (the directory keeps that: stated with no agent and a
  terminal, or recorded before its CLI ran). The directory keeps each session's CLI and name,
  so `names_version` moves when one is renamed (`session_updated`) or dropped.
- **Directory.** What is known before the first event (members, workstreams, tasks, sessions,
  dispatches, asks), seeded from projections with `add_*` and kept current from events (members
  too, from `member_added`, so one directory serves both the builder and the names in prose). It
  follows the hub's rules (`crates/hub-work`), so recaps say what the hub shows:
  - *Firm links stay.* An explicit assignment (`dispatch`, `manual`, `claimed`, `imported`) is
    replaced only by another firm one, never by an inferred one (`folder`, `branch`) or by a
    re-stated `session_discovered` without a link. A link is replaced whole. A re-stated session that names no agent keeps the one it had. A dispatch links its
    session only when it has no firm link yet (the hub never links from `dispatch_started`; the
    `session_discovered` after it does). A `session_linked` for a session not discovered yet makes
    its entry with that link, so a firm one holds against the discovery that follows.
  - *Stale moves are not moves.* A `task_moved` whose `from` is not the task's status (only a
    second writer appends one) is ignored by the hub's tasks projection. The status is known from
    the task's `task_created` and the moves counted since; a move that ends where a `task_created`
    put the task still counts (a log that states tasks as they are now and then replays older
    moves, as the hub's seed writes). A seed's status is not checked: it may be ahead of the
    events that follow. A move on a task the directory does not know (never stated, or dropped
    for the limit) counts too, where the hub ignores a move on a task it does not have.
  - Events the hub ignores (a stale move, a link that would replace a firm one) are not activity:
    no block holds them, and `BlockBuilder::skipped` counts them.
  - *Bounded, without a first-come cap.* At most 100,000 entries of each kind
    (`Directory::with_limit` sets another limit). Past that, the entry used longest ago goes: an
    entry is used by every `add_*` and every event that states or names it (a session's activity,
    a task's events for the task and its link to a session, a dispatch's end, an ask's answer, a
    member's own events). So a log of any length keeps being learned from, and what goes is what
    nothing mentioned for longest, in practice sessions that ended long ago with their dispatches
    and asks. Use depends only on the
    order of events, so batching never changes the directory. A dropped session or task is placed
    as if new; a dropped name reads "someone", "a task", "a workstream" or "an ask". Every kind
    full (100,000 each, which only a flood of made-up ids reaches) measured about 125 MB, and
    170 MB with every name at its 60-character cap in 4-byte characters (release build, Linux
    x86-64).
  - *For a cache of prose.* `Directory::ask` says what an answer to an ask is described by (its
    kind and asker), and `Directory::names_version` moves on whenever prose may now read
    differently: a handle, task key or workstream name re-stated differently, an ask raised again
    differently, any of them dropped for the limit, or, after a drop, any name of that kind
    learned. A name learned for the first time does not move it.
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

The types the API serves (`Block` and its parts, `Check`, `Summary`, `Span`, `DayRecap`) live in
`pitcrew_protocol::recap`, and this crate re-exports them; `docs/build/contracts/api-v1.md`
("Recaps") says how they are served.

Everything is pure and deterministic. Event text is untrusted: it is cleaned and capped before it
is kept. Cleaning removes control characters, direction-changing and invisible characters, and
Unicode tag characters (U+E0000–E007F, which can carry text a person does not see but a model
reading the recap does). The set is `pitcrew_protocol::text::is_hidden`, which the GitHub and Jira
syncs and the CLI drop too (`text.rs` pins it in a test, as each of them does). Of that set, the
line and paragraph separators
(U+2028, U+2029) become a space, like a line break, so the words around them stay apart. Counts,
files, facts, tasks and receipts per block are capped by `Config`.

## Tests

- `tests/demo.rs`: snapshots of the blocks, lines and day paragraphs for the demo workspace.
- `tests/rules.rs`: one scenario per rule.
- `tests/briefs.rs`: a snapshot of the demo workspace's proposals (workstreams and projects),
  scenarios for each "Where it stands" rule, pinned and automatic acceptance, and a property test
  that every proposal over any generated log verifies and cites only receipts from it.
- `tests/incremental.rs`: property tests (batching never changes the blocks or the directory,
  also with a directory small enough to drop entries all the time; hostile input never panics)
  and the receipt checks.
- `tests/directory.rs`: one scenario per hub rule (firm links, agents, stale moves), the bound
  (learning past the limit, active sessions outliving idle ones), and `names_version`.
- Both `demo.rs` and `incremental.rs` check that two independent runs give byte-identical JSON.
  Every internal map hashes with its own random seed, so output order must never depend on map
  order: anything taken from a map is sorted first.
- `tests/perf.rs`: 100,000 generated events in under 500 ms. It runs only in release:
  `cargo test --release -p pitcrew-recap --test perf -- --nocapture`.
