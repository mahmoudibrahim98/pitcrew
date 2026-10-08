// Optional step after Import: draft each new workstream's board from its history (see
// `../draft-board.tsx`). Per workstream: what will be sent, its size and the agent's estimated
// usage, then the person's confirmation. The proposals arrive later; each is reviewed on its
// workstream's page ("Draft board"), and nothing is created until it is.

import { useState } from 'react';
import { DraftStart } from '../../projects/board-draft.tsx';
import type { BoardDraft } from '../../projects/board-drafts.ts';
import { StepFooter } from '../step-footer.tsx';
import { useWizard } from '../wizard-context.tsx';

export function DraftStep() {
  const { state, next } = useWizard();
  const workstreams = state.createResult?.workstreams ?? [];
  const [started, setStarted] = useState<Record<string, BoardDraft>>({});
  const count = Object.keys(started).length;

  return (
    <div className="flex flex-col gap-4">
      <p className="text-sm text-ink-2">
        An agent you already use can draft each workstream’s board (open tasks, what is in progress, what looks
        done) from the sessions you imported. No API key: it runs in your agent’s own CLI. You see what will be sent
        and its estimated usage first, and nothing is created until you review the proposal on the workstream’s page.
      </p>
      <ul aria-label="Workstreams to draft" className="flex flex-col gap-3">
        {workstreams.map((w) => (
          <li key={w.id} className="rounded-sm border border-line p-3">
            <h2 className="mb-2 text-sm font-semibold text-ink">{w.name}</h2>
            {started[w.id] === undefined ? (
              <DraftStart
                compact
                workstream={w.id}
                onStarted={(draft) => setStarted((all) => ({ ...all, [w.id]: draft }))}
              />
            ) : (
              <p role="status" className="text-sm text-ink">
                Drafting. Review the proposal on the workstream’s page when it is ready (“Draft board”).
              </p>
            )}
          </li>
        ))}
      </ul>
      <form
        onSubmit={(e) => {
          e.preventDefault();
          next();
        }}
      >
        <StepFooter nextLabel={count === 0 ? 'Continue without drafting' : 'Continue'} />
      </form>
    </div>
  );
}
