# Stream N · UI: Projects layout

**Goal:** the calm layout for keeping track of the work: where everything stands, what
happened lately, and what needs you, with agents visible where the work is.

**Owns:** `apps/ui/src/projects/**`.  **Depends on:** L, the mock hub.  **Model:** Sonnet-class.
**Read first:** ADR-0007, ADR-0008; `docs/build/contracts/api-v1.md`; the demo workspace fixture.

## Work packages

1. **Home:** "Where things stand" across projects, agents now (live status lines), what changed
   since you last looked, and what needs you.
2. **Inbox:** asks addressed to me (questions, decisions, reviews, approvals, mentions),
   answerable in place, with receipts.
3. **My tasks**, **Overview**, **Projects** list.
4. **Project** page: Overview (Where the project stands, open questions, recap), Workstreams,
   Board (by status or by workstream), List, Timeline (workstreams with their tasks), Agents,
   Files (link into the console's viewer), Activity.
5. **Workstream** page: Where it stands (edit, pin, accept proposals), Board, Tasks, Agents,
   Activity.
6. **Task drawer:** fields, subtasks (agent-plan lines marked), dependencies, comments with
   mentions, **Agent run** (open chat or terminal, dispatch), source link, history.
7. **Calendar**, **Members** (people and agents in one table, owners shown), **Activity**
   (Summary / All events).

## Acceptance

- Every page renders the demo workspace from the mock hub; drag-and-drop moves obey the server's
  answer (a 409 snaps back with the reason).
- "Needs you" badges and live status lines update from the stream without a reload.
- Lists and boards virtualise at 10,000 tasks.
- Keyboard and screen-reader checks pass (axe).

## Do not

Edit the shell, design system or data layer (ask L); build console views (M).
