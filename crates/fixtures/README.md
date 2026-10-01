# pitcrew-fixtures

Shared, **synthetic** test data. Every stream builds against it, and the mock hub serves it.

| File | What |
|---|---|
| `data/demo-workspace.json` | "Demo Lab": 3 machines (laptop, a SLURM cluster, an unreachable GPU box), 1 person and 5 agents, 2 projects, 4 workstreams, 10 tasks, 6 sessions, 4 dispatches, 4 asks, 4 briefs, and 15 recent events. Parsed by `demo_workspace()`. |
| `data/demo-recaps.json` | The recaps of the demo's events, as the recap engine writes them with its rules: every block with its line, and each project's day paragraphs at UTC. Parsed by `demo_recaps()`; the mock hub serves its recap routes from it. **Generated**: `tests/recaps.rs` fails when it is stale, and `PITCREW_UPDATE_FIXTURES=1 cargo test -p pitcrew-fixtures --test recaps` rewrites it. |
| `data/transcripts/claude/demo-session.jsonl` | A Claude Code session: prompt, plan (`TodoWrite`), read, edit with a patch, shell, a question (`AskUserQuestion`), turn end, custom title, summary. |
| `data/transcripts/codex/rollout-demo.jsonl` | A Codex rollout: `session_meta`, `turn_context`, plan (`update_plan`), shell calls, `apply_patch`, agent message, token count, task complete. |
| `data/transcripts/opencode/schema.sql`, `seed.sql` | An approximation of an OpenCode SQLite store, with one session. |

The transcript shapes follow the CLIs' formats as of late 2026, from public documentation and
observation. They are **approximations**: stream A checks them against real files and corrects
them through a contract change (`s/0/contract-…`).

## Rules

- Everything here is made up. **Never commit real transcripts, host names, paths, user names or
  e-mail addresses.** The scrub gate in CI blocks known private tokens.
- For local testing against real data, put files in `data/private/`. It is ignored by git.
- IDs follow a readable pattern: `01JB` + 15 zeros + a three-letter tag + a four-digit number,
  e.g. `01JB000000000000000TSK0004`. Tags use Crockford base-32 letters only (no I, L, O, U).
- `cargo test -p pitcrew-fixtures` checks that every reference resolves, every task move is legal,
  and that no JSON field is silently ignored.