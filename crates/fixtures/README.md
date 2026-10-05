# pitcrew-fixtures

Shared, **synthetic** test data. Every stream builds against it, and the mock hub serves it.

| File | What |
|---|---|
| `data/demo-workspace.json` | "Demo Lab": 3 machines (laptop, a SLURM cluster, an unreachable GPU box), 1 person and 5 agents, 2 projects, 4 workstreams, 10 tasks, 7 sessions (one a sub-agent of a dispatched Codex session, started the day after it), 4 dispatches, 4 asks, 4 briefs, and 15 recent events. Parsed by `demo_workspace()`. |
| `data/demo-recaps.json` | The recaps of the demo's events, as the recap engine writes them with its rules: every block with its line, and each project's day paragraphs at UTC. Parsed by `demo_recaps()`; the mock hub serves its recap routes from it. **Generated**: `tests/recaps.rs` fails when it is stale, and `PITCREW_UPDATE_FIXTURES=1 cargo test -p pitcrew-fixtures --test recaps` rewrites it. |
| `data/transcripts/claude/demo-session.jsonl` | A Claude Code session: prompt, plan (`TodoWrite`), read, edit with a patch, shell, a question (`AskUserQuestion`), turn end, custom title, summary. |
| `data/transcripts/codex/rollout-demo.jsonl` | A Codex rollout: `session_meta`, `turn_context`, plan (`update_plan`), shell calls, `apply_patch`, agent message, token count, task complete. |
| `data/transcripts/opencode/schema.sql`, `seed.sql` | An approximation of an OpenCode SQLite store, with one session. |

## A home of the test's own

`homes::private_home` gives a program a test starts (`pitcrewd`, `pitcrew`) a home folder of the
test's own: `HOME`, `USERPROFILE`, `APPDATA` and `LOCALAPPDATA` point inside it, and the variables
that send a lookup elsewhere (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`, `XDG_DATA_HOME`,
`XDG_CONFIG_HOME`, `OPENCODE_CONFIG_DIR`, `HOMEDRIVE`, `HOMEPATH`) are removed. On Windows a home
lookup reads `USERPROFILE` (Windows' own known-folder lookup expands it too), not only `HOME`.
`homes::check_private_home`, called just before the program starts, panics if any of them is
inherited, removed where it must be set, or outside the temporary folder. Every harness that starts
one of our programs calls both.

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