# Workflow

How a stream agent works, and what the integrator does. Short version: **your paths only, small
pull requests, green checks, honest reports, never merge.**

## For a stream agent

1. **Start from your card** (`streams/<X>.md`). Read the ADRs and contracts it cites.
2. **One worktree, one branch:** `s/<X>/<topic>`, e.g. `s/A/claude-parser`. Keep branches small:
   one work package, or part of one.
3. **Work only in your paths** (see `ownership.md`). CI rejects anything else. If you need a
   change elsewhere, describe it in your report.
4. **Build against contracts and mocks, not other streams' internals.** Use the fakes in
   `pitcrew-interfaces`, the fixtures in `pitcrew-fixtures`, and the mock hub for UI work.
5. **Before you open a pull request, run:**

   ```bash
   cargo fmt --all
   cargo clippy --workspace --all-targets -- -D warnings
   cargo test --workspace
   npm test                                                   # if you touched JavaScript
   node scripts/ci/path-guard.mjs --base origin/main          # your branch stays in its lane
   node scripts/ci/scrub-gate.mjs                             # no private data
   ```

   The scrub gate's patterns are a CI secret (`SCRUB_PATTERNS`), so a local run without them
   checks only the public hash list. CI runs the full check on every pull request.

6. **Open the pull request** with the template filled in. **Never merge**, never push to `main`,
   never force-push someone else's branch.
7. **Report** in the pull request: what changed, how you checked it (real output), contract impact,
   and **"What I did not do"**: anything skipped, stubbed, unverified or left for later.

**Never:**
- edit outside your paths, or "fix" another stream's code;
- commit secrets, tokens, real transcripts, host names, user names or personal paths;
- weaken a check (lints, tests, guards) to get green;
- add a dependency with a licence outside `deny.toml`'s allow list;
- use `unsafe` without a comment saying why it is sound, isolated in one module.

## The worker brief

Ready-made briefs live in [briefs/](briefs/). An agent is started with one line:
"Follow `docs/build/briefs/<brief>.md`". Each brief builds on `briefs/README.md`, which holds
the setup, environment, rules, definition of done and report format shared by all of them.

To write a new brief, copy an existing one. For a quick one-off, this template also works (fill
in the brackets):

```text
You are the agent for stream [X] of PitCrew: [stream name].
Repository: [path or URL]. Branch: s/[X]/[topic] from main, in your own worktree.

Read first: docs/build/streams/[X].md, docs/build/workflow.md, docs/build/contracts.md,
and the ADRs your card cites.

Goal for this branch: [one work package from the card].

You own only these paths: [globs from ownership.json]. Do not edit anything else. If you need a
change outside them, stop and describe it in your report.

Done means: the acceptance checks for this work package pass, and fmt, clippy -D warnings, tests,
the path guard and the scrub gate are green locally.

Report: a pull request using the template, ending with "What I did not do".
You never merge.
```

## For the integrator

- Keeps `main` green, and merges stream pull requests after review (and Q's review where the card
  says so).
- Owns every shared file. Makes contract changes (`s/0/contract-…`), bumps versions, tells
  affected streams.
- Wires crates together in `crates/daemon` as they land.
- Assembles milestones (see `README.md`), runs the parity checklist and the benchmarks.
- Keeps the stream cards current: when a work package lands, marks it done in the card.
