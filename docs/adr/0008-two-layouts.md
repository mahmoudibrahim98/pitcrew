# 0008. Two layouts over the same data

- **Status:** Accepted
- **Date:** 2026-09-30

## Context

Two jobs pull the interface in different directions. Keeping track of work wants a calm,
familiar project layout: home, inbox, projects, boards, timelines. Driving agents wants a dense
console: session list, chat, terminal, files, all live. A single compromise layout serves
neither well; two separate apps split the data.

## Decision

Two layouts over **the same data**, switched with a toggle or a shortcut:

- **Projects:** Home, Inbox, My tasks, Overview, Projects, Project (Overview, Workstreams, Board,
  List, Timeline, Agents, Files, Activity), Workstream, Calendar, Members, Activity. Agents appear
  where the work is: live status lines and "Needs you" badges on task cards, the agent's plan as
  subtasks, "Agent run" in the task drawer, "Where it stands" on every workstream.
- **Agent console:** three panes (filters, session list, session) with chat, tool rows, question
  and prompt cards, the composer, the terminal, workbench tabs and splits, and a file viewer.
  Sessions group by project and workstream, plus *Unsorted*.
- **Everywhere:** the Orchestrator panel, search, "+ New", and the Inbox.

## Consequences

- UI streams split cleanly: L (foundation and shell), M (console), N (projects), O (onboarding),
  each in its own folder under `apps/ui/src/`, composed by the shell.
- Both layouts read the same queries and the same delta stream; neither owns data.
