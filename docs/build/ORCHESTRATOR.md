# PitCrew: you are now the orchestrator

You take over **all** development of PitCrew, an Apache-2.0 desktop app: a Rust daemon `pitcrewd`, a Tauri 2 shell and a React 19 UI. It tracks AI coding agents' sessions locally and on remote and HPC machines. The repository is **public**: `github.com/mahmoudibrahim98/pitcrew`.

You **plan, build, review and merge**, using your own subagents. Don't wait for Codex, other models or other agents: if something needs doing, launch a subagent for it. Ask the maintainer only for decisions that are theirs (section 8).

**Authority (from the maintainer).** This overrides the line in `CLAUDE.md` that says the main session doesn't merge. You may:
- merge pull requests into `main` once they are reviewed and CI is fully green;
- push docs-only commits to `main` (briefs and the brief index);
- push to any `integrator/*` branch, including PRs that other agents started.

You may not:
- force-push;
- `git push --all`;
- push code to `main` except through a merged PR.

## 1. Read first

1. `CLAUDE.md`, especially "Cloud sessions" and "Several briefs in one cloud session", which describe how to run subagents. Also `AGENTS.md`.
2. `docs/build/briefs/README.md`: the brief rules and the **index of every brief with its status**.
3. Then refresh:
```
git fetch origin && git log --oneline -10 origin/main
gh pr list -R mahmoudibrahim98/pitcrew --state open
gh pr view <N> -R mahmoudibrahim98/pitcrew --comments
```

## 2. The loop (repeat until the queue is empty)

For each work item:

1. **Build.** A subagent works in its own worktree and branch, in the brief's paths. It implements, runs every check the brief names, pushes, and opens a PR with its report as the body (`.github/pull_request_template.md`).
2. **Review.** A *separate*, fresh, read-only subagent reviews the PR diff against `origin/main`: security first, then correctness, data and migration safety, mock-hub parity, conformance, tests, scope, and what the brief asked for that's missing. It verifies every finding in the code. You post the result as **one PR comment**:
   - findings numbered as blocker, should-fix and minor, each with `file:line`, the defect, a concrete scenario and the fix;
   - then the brief gaps.
3. **Fix.** A subagent applies the review on the same branch (no new PR), merges `origin/main` (no rebase), reruns the checks, and pushes.
4. **Verify and merge.**
   - Check the fix commits against the comment, item by item. Anything left undone must be listed under "Not done" in the PR body.
   - When CI is **fully green**, merge: `gh pr merge <N> --merge` (a merge commit, never a squash). Give it a subject like `Merge PR #N: <what landed> (<branch>)` and a short body: what landed, "reviewed by the integrator", "CI green".
5. **Propagate.** Every other open PR merges `origin/main` (a subagent can do it).

**Capacity:**
- At most **two subagents building Rust at once**. UI-, docs- or Node-only work can run alongside. Check `df -h` first (30 GB disk; keep at least 6 GB free).
- Each worktree gets its own `CARGO_TARGET_DIR=/root/.cache/pitcrew-target-<name>`.
- Delete the cache and remove the worktree once its PR is merged or closed.
- Run long test suites in the background and read their logs.

**Overlapping PRs:** merge them as a **batch branch with its own PR**, so CI checks the combination. Resolve conflicts **region by region**, keeping both sides, never one side's whole file. Regenerate protocol bindings with `cargo test -p pitcrew-protocol --features ts` and commit `packages/protocol-ts`.

## 3. State at handoff

main = `e76acfa`; nothing has merged since 5 Oct. Recently merged:
- #57: the discovery race fix;
- #51: the updater;
- #56: onboarding hooks and safety;
- #61: a batch of #52 (GitHub/Jira read-only sync), #58 (approval-gated writes) and #55 (machine checks and agent sign-in);
- #60: the "+ New" dialogs, plus default agents `@claude`, `@codex` and `@opencode`;
- #62: starting sessions from the app, and stale states expiring.

**Open PRs.** Each review except #64's is a PR comment; read it with `gh pr view N --comments`. **You now own all of them.**

| # | Branch | Review summary | Next step |
|---|---|---|---|
| 54 draft board | `integrator/draft-board` | **Blocker**: the drafting CLI ran in the real repo with the persona's permissions and the agent's full token. Should-fix: a per-draft token, the prompt in a file (Windows `.cmd`), end and timeout, redaction holes, path redaction. | Fixes were pushed on 5 Oct. Verify them against the review, then merge. |
| 59 Orchestrator chat | `integrator/orchestrator-chat` (stacked on #54) | **Blocker**: the CLI ran with the person's own permission config, in a scratch folder inside the state dir, next to `device.token`. Should-fix: a fresh folder per question, the prompt in a file, the reader scope failing closed, transcripts private to their owner. | Verify the pushed fixes. Merge after #54 with `main` merged in. If it still targets #54's branch, retarget it to `main` (`gh pr edit 59 --base main`). |
| 67 data you can trust | `integrator/trusted-data` | Should-fix: re-parent sub-agents stored before the PR; `GET /v1/sessions` judges sub-agents by themselves; "Link to project" makes duplicate `Main` workstreams; recaps name the person. | Verify or apply the fixes, then merge. |
| 66 tasks that work | `integrator/tasks-that-work` | Should-fix: the editor sends a stale snapshot; archived tasks count as live and can be dispatched; the dispatches route leaks hidden sessions to agents; running sessions vanish from the drawer; toast "Open" reloads the app; "Copy link" copies a `tauri://` URL. | Apply the fixes (nobody is on them), then merge. |
| 65 shell polish | `integrator/shell-polish` | **Blocker**: the inline `<style>` in `apps/ui/index.html` makes Tauri add a style nonce, which disables `'unsafe-inline'` in the packaged app. Move it to a linked CSS file and make `csp-check.mjs` guard it. Plus 5 should-fix items. | Apply the fixes, then merge. |
| 64 Settings | `integrator/settings` | **Not reviewed yet.** Deferred by decision: desktop notification controls and hook "Remove". | Review, fix, merge. |
| 63 files everywhere | `integrator/files-everywhere` | Deferred by decision: the desktop "Reveal in folder / Open in editor" opener. | Rust (windows-latest) was failing: fix it. Review, fix, merge. |

**Queue after those.** Briefs are in `docs/build/briefs/`.
1. **`0-resume-sessions`:** resume sessions started outside PitCrew, a disabled composer with reasons, and recaps with substance.
2. **New brief to write: desktop follow-up.** The desktop opener (Reveal in folder, Open in editor) with a checked, workstream-scoped command and capability; desktop notification settings in Settings; hook removal with a retained uninstall plan in `crates/cli`.
3. **`L-home-inbox`:** gr8r-level Home, Inbox and My tasks. Starts after #66.
4. **`N-projects-boards`:** gr8r-level projects, boards and a Gantt timeline. Starts after #66 and #67.
5. **New brief to write: portable Windows build.** A CI job (on demand and on `main`) that uploads a zip of `pitcrew-desktop.exe`, `pitcrewd.exe`, `pitcrew-ptyd.exe`, `pitcrew-askpass.exe` and `pitcrew.exe`. The maintainer's laptop can't run installers or build locally, so they unzip this into a folder they're allowed to run from.
6. Then the remaining open briefs in the index: team hubs last, after the route-heavy work.

**Background:** a full UI audit on 5 Oct produced the briefs added at `b40ccf8`. The maintainer compares the UI with the gr8r Studio reference (`https://gr8r-studio.vercel.app`): dense dashboards, rich board cards, a task drawer, a Gantt timeline and a two-pane inbox. They want that level of finish, plus PitCrew's agent layer. Design-heavy briefs deserve your most capable subagents.

## 4. Writing briefs

Use the format of the existing briefs: stream, branch `integrator/<topic>`, paths, first read, suggested agent, goal, what to build, acceptance. Add each to the index in `docs/build/briefs/README.md`, as a docs-only commit to `main`.

Always include: **"Mechanical edits outside the paths are fine; list them. If one item needs a substantive change outside the paths, skip that item, list it under Not done, and finish the rest. Never stop the whole brief."**

## 5. Rules that never bend

- **Public repository.** Use synthetic data only. Never put private names, emails, hosts, cluster names or local paths in files, PR text or comments. CI's scrub gate runs with the maintainers' private patterns, so never merge when **Guards** fails, and never work around it. Never print or commit tokens.
- **Agent homes.** Nothing (daemon, tests, agents) ever runs against a real `~/.claude`, `~/.codex` or OpenCode store; tests use temporary homes.
- **Commit identity:** the maintainer's GitHub noreply address, with agent attribution as a `Co-Authored-By:` trailer, as on the existing commits.
- **Agent CLIs that PitCrew starts** (dispatch, drafts, the Orchestrator) use a strict read-only launch:
  - Claude: `--permission-mode=default --setting-sources=project --strict-mcp-config`;
  - Codex: `--sandbox=read-only --ask-for-approval=untrusted`;
  - OpenCode: a deny-all `opencode.json` except `pitcrew …`;
  - plus a fresh private folder outside the state directory, and a narrowly scoped token.
- **Windows:** ptyd refuses arguments containing `" % ! ^ & | < > ( )` or newlines for npm `.cmd` shims, so long prompts go in a file. Windows and macOS are checked only by CI, so read those jobs' logs when they fail.
- **Tauri CSP:** no inline `<style>`, no `style=""`, and no remote resources in the UI.
- **Stored types:** stay forward-compatible: no `deny_unknown_fields`, snake_case, and the event log is never rewritten.

## 6. Reporting

- After each merge or blocker, give the maintainer a short update: what landed, what's next, anything only they can do. A PR comment or a message works.
- Keep the brief index statuses current.

## 7. Watch out for

- **Stacked PRs:** GitHub won't mark them merged. Retarget them to `main`, or close them with a note.
- **A PR's draft flag is unreliable:** judge readiness by the commits and CI.
- **CI flakes:** re-run a failed job once before treating the failure as real (some tmux, remote and macOS tests are timing-sensitive).

## 8. Decisions that stay with the maintainer

- Code-signing certificates.
- Publishing the v0.1.0 draft release.
- The predecessor-portal importer's input format.
- GitHub email privacy settings.
- Anything that changes the product's direction.

## 9. Start now

1. Refresh (section 1).
2. Verify #54 and merge it. Then merge `main` into #59 (no rebase), verify it and merge it.
3. In parallel, within the limit of two Rust builders:
   - a fix subagent for #66;
   - a fix subagent for #65;
   - a review subagent for #64, then its fix subagent;
   - a subagent for #63's Windows failure and review.
4. #67: verify or fix, then merge.
5. Continue with the queue (section 3).
