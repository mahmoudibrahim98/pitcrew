# 0007. Workspace → Project → Workstream → Task → Subtask; people and agents as members

- **Status:** Accepted
- **Date:** 2026-09-30

## Context

PitCrew's job is to let a developer or researcher **keep track of the state of their work** while
many agents do much of it: what is going on, what the latest notes say, what happened lately,
and who needs an answer. A plain task tracker with agents bolted on makes everything a ticket.
A plain session list loses the work structure.

## Decision

```
Workspace → Project → Workstream → Task → Subtask
```

- **Project:** a deliverable (a paper, a product, a thesis part). Has "Where the project
  stands", rolled up from its workstreams.
- **Workstream:** one line of work inside a project (e.g. "Seed runs"). Has "Where it stands",
  next step, status (idea, active, paused, shipped, dropped) and health, and **locations**
  (folders or branches on machines) that link sessions to it.
- **Task / Subtask:** things a person or an agent finishes. A task can sit directly under a
  project.
- **Members are people and agents.** Every agent has an **owner** (a person), and its authority is
  bounded by its owner's. Teams mix both.
- **Sessions** are CLI conversations on machines, linked to a workstream (by folder, branch,
  dispatch or by hand) and usually a task.
- **Asks** are what needs a member's answer: questions, decisions, reviews, approvals, mentions.
- **Briefs** ("Where it stands") are proposed by the back office from events, with receipts, and
  accepted or pinned by people.

**Tasks move themselves, within rules** (`TaskStatus::can_move`):

| Mover | Allowed |
|---|---|
| Person | Any change |
| Agent (own task only) | backlog/todo → in progress; in progress → review |
| Back office | in progress → review on evidence; review → done only if the task allows automatic acceptance |
| Sync (GitHub, Jira) | an upstream close → done; a reopen → todo; never touches in-progress work |

An agent's live plan is mirrored as the task's subtasks.

## Consequences

- The model is in `crates/protocol/src/model.rs`; the move rules are a contract test.
- Every claim in a brief or recap links to receipts (transcript offsets, commits, jobs, files).
- The same model carries collaboration later: a second person is another member.
