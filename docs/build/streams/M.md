# Stream M · UI: Agent console

**Goal:** the dense, live view for driving agents. It is a **port, at full depth, of the
predecessor portal's session interface**, restyled on PitCrew's tokens, not a redesign.

**Owns:** `apps/ui/src/console/**`.  **Depends on:** L (shell, design, data), the mock hub.
**Model:** Opus-class.
**Read first:** ADR-0008, ADR-0010; `docs/build/contracts/api-v1.md` (sessions, transcript,
terminal); `crates/protocol/src/transcript.rs`. The maintainers provide the predecessor's source
for reference.

## Work packages

1. **Three panes:** filters and facets (machine, engine, state, project, workstream, labels,
   flags), the session list (virtualised; grouped by project and workstream plus *Unsorted*;
   row menu; unread; a "Starting…" row), and the session view.
2. **Chat renderer:** markdown with **no raw HTML** (sanitised, `rel=noopener`), tool rows
   paired by `call_id` with expandable input and output, edits with diffs, thinking, images,
   **question and prompt cards** (answerable in place), the tasks bar (the agent's plan).
   Tail-first loading with `before=` paging; heavy parsing in a web worker.
3. **Composer:** send, keys, interrupt; model, effort and account chips.
4. **Terminal:** xterm.js (WebGL, falling back to canvas) on the terminal WebSocket; view and
   control modes; fit; reconnect by offset.
5. **Workbench:** tabs and splits for sessions, terminals and files; the file viewer (text,
   images, PDF with annotations, TeX compile); the details sidebar; the usage bar.
6. **Links to the work:** each session shows its task and workstream; actions Hand off, Fork,
   Review, Link to task.

## Acceptance

- Opening a session paints the newest page in < 300 ms regardless of transcript size (mock with a
  large synthetic transcript).
- Scrolling 10,000 sessions stays at 60 fps.
- Answering a question card sends the right keys/text (tested against the mock).
- Parity checklist rows for chat, terminal, workbench and files pass.

## Do not

Edit the shell or design system (ask L); invent API routes (propose them to the integrator).
