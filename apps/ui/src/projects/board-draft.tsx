// "Draft board": an agent drafts a workstream's board from its history (api-v1.md, "Board
// drafts"). The person sees what will be sent and an estimate before anything is, confirms, and
// then reviews the proposal task by task: only what they accept becomes a task (labelled
// `drafted`); the rest creates nothing.
//
// - `DraftBoardPanel`: the workstream page's panel. Shows the newest draft's review when one waits,
//   its progress while it runs, and otherwise the preview and the start (`DraftStart`).
// - `DraftStart`: cost first. The sizes, the number of sessions, the redactions and the estimate;
//   what will be sent, on request; the agent and the CLI (only those found on the hub's own
//   machine, where drafts run); then the start, with the preview's digest.
// - `DraftReview`: accept or reject each proposed task, or all, or none. Nothing starts accepted:
//   the person chooses.
//
// Everything the agent wrote is shown as text, never as markup.

import { useId, useMemo, useState } from 'react';
import { Button, StatusPill } from '../design/index.ts';
import { ApiError, useApi, useLiveQuery, type Engine, type MemberId, type WorkstreamId } from '../data/index.ts';
import {
  costLine,
  estimateLine,
  useBoardDrafts,
  useDraftPreview,
  useReviewDraft,
  useStartDraft,
  useStopDraft,
  type BoardDraft,
  type DraftReviewed,
} from './board-drafts.ts';
import { useMachines, useMe, useMembers, useNames } from './data.ts';
import { TASK_STATUS } from './format.ts';
import { useProjectsNav } from './nav.tsx';
import { ErrorNote, Field, Panel, inputClass } from './ui.tsx';

const ENGINE_LABEL: Record<Engine, string> = { claude: 'Claude Code', codex: 'Codex', opencode: 'OpenCode' };

/**
 * The agent CLIs found on the hub's own machine, where every draft runs (its runner's
 * `session-options`). `loading` until known; none when the machine has none, or cannot be asked.
 */
export function useDraftEngines() {
  const api = useApi();
  const machines = useMachines();
  const machine = machines.data?.find((m) => m.kind === 'local')?.id;
  const options = useLiveQuery({
    queryKey: ['session-options', machine ?? ''],
    queryFn: ({ signal }) => api.sessionOptions(machine ?? '', signal),
    enabled: machine !== undefined,
  });
  const engines = useMemo(() => (options.data?.engines ?? []).map((e) => e.engine), [options.data]);
  const error = machines.error ?? options.error;
  const none = machines.data !== undefined && machine === undefined;
  return { engines, error, loading: error === null && !none && options.data === undefined };
}

/**
 * The caller's own agents, the back office first: those a draft may run as. `loading` until both
 * the caller and the members are known, so no one is told they have none before then.
 */
export function useMyAgents() {
  const me = useMe();
  const members = useMembers();
  const agents = useMemo(() => {
    const mine = (members.data ?? []).filter((m) => m.kind === 'agent' && m.owner === me.data?.id);
    return [...mine.filter((m) => m.handle === '@office'), ...mine.filter((m) => m.handle !== '@office')];
  }, [members.data, me.data?.id]);
  const error = me.error ?? members.error;
  return { agents, error, loading: error === null && (me.data === undefined || members.data === undefined) };
}

/** The preview, and the start of a draft of `workstream`: cost first. */
export function DraftStart({
  workstream,
  onStarted,
  compact = false,
}: {
  workstream: WorkstreamId;
  onStarted?: (draft: BoardDraft) => void;
  /** Without the "what will be sent" text open by default, for a list of workstreams. */
  compact?: boolean;
}) {
  const preview = useDraftPreview(workstream);
  const { agents, error: agentsError, loading: agentsLoading } = useMyAgents();
  const { engines, error: enginesError, loading: enginesLoading } = useDraftEngines();
  const start = useStartDraft(workstream);
  const [agent, setAgent] = useState<MemberId | ''>('');
  const [engine, setEngine] = useState<Engine | ''>('');
  const [showSummary, setShowSummary] = useState(!compact);
  const summaryId = useId();
  const chosen = agent !== '' ? agent : agents[0]?.id;
  // Only a CLI found where the draft runs: the one picked, else the first found.
  const cli = engine !== '' && engines.includes(engine) ? engine : engines[0];

  if (preview.error !== null) return <ErrorNote error={preview.error} what="preview the draft" />;
  if (preview.data === undefined) {
    return (
      <p role="status" className="text-sm text-ink-2">
        Measuring what would be sent…
      </p>
    );
  }
  const cost = preview.data.cost;
  const changed = start.error instanceof ApiError && start.error.status === 409;

  return (
    <div className="flex flex-col gap-3">
      <div className="flex flex-col gap-1 text-sm text-ink">
        <p>{costLine(cost)}</p>
        <p className="text-ink-2">{estimateLine(cost)}</p>
        <p className="text-xs text-ink-2">
          Sent: session titles, recaps of their work and the board’s tasks, with anything that looks like a secret,
          an e-mail address or a home folder replaced. Never a transcript. Nothing is created until you review the
          proposal.
        </p>
      </div>
      <div>
        <Button
          variant="ghost"
          aria-expanded={showSummary}
          aria-controls={summaryId}
          onClick={() => setShowSummary((s) => !s)}
        >
          {showSummary ? 'Hide what will be sent' : 'Show what will be sent'}
        </Button>
        {showSummary && (
          <pre
            id={summaryId}
            tabIndex={0}
            aria-label="What will be sent"
            className="mt-2 max-h-64 overflow-auto whitespace-pre-wrap rounded-sm border border-line bg-sunken p-2 text-xs text-ink"
          >
            {preview.data.summary}
          </pre>
        )}
      </div>
      {agentsError !== null ? (
        <ErrorNote error={agentsError} what="load your agents" />
      ) : enginesError !== null ? (
        <ErrorNote error={enginesError} what="find the agent CLIs on this hub's machine" />
      ) : agentsLoading || enginesLoading ? (
        <p role="status" className="text-sm text-ink-2">
          Loading your agents…
        </p>
      ) : agents.length === 0 ? (
        <p className="text-sm text-ink-2">None of your agents can draft: add an agent to this workspace first.</p>
      ) : engines.length === 0 ? (
        <p className="text-sm text-ink-2">
          No agent CLI was found on this hub’s machine, where drafts run: install Claude Code, Codex or OpenCode, and
          check the machine in Settings.
        </p>
      ) : (
        <form
          className="flex flex-wrap items-end gap-3"
          onSubmit={(e) => {
            e.preventDefault();
            if (chosen === undefined || cli === undefined || preview.data === undefined) return;
            start.mutate(
              {
                agent: chosen,
                engine: cli,
                digest: preview.data.digest,
              },
              { onSuccess: (draft) => onStarted?.(draft) },
            );
          }}
        >
          <Field label="Agent" className="w-48">
            {(id) => (
              <select id={id} value={chosen ?? ''} onChange={(e) => setAgent(e.target.value)} className={inputClass}>
                {agents.map((a) => (
                  <option key={a.id} value={a.id}>
                    {a.handle === '@office' ? `${a.handle} (back office)` : a.handle}
                  </option>
                ))}
              </select>
            )}
          </Field>
          <Field label="CLI" className="w-48">
            {(id) => (
              <select
                id={id}
                value={cli ?? ''}
                onChange={(e) => setEngine(e.target.value as Engine)}
                className={inputClass}
              >
                {engines.map((e) => (
                  <option key={e} value={e}>
                    {ENGINE_LABEL[e]}
                  </option>
                ))}
              </select>
            )}
          </Field>
          <Button type="submit" variant="primary" disabled={start.isPending || chosen === undefined || cli === undefined}>
            {start.isPending ? 'Starting…' : 'Send and draft'}
          </Button>
        </form>
      )}
      {start.error !== null &&
        (changed ? (
          <p role="alert" className="text-sm text-risk">
            {start.error.message} The preview above is up to date: check it, then send again.
          </p>
        ) : (
          <ErrorNote error={start.error} what="start the draft" />
        ))}
    </div>
  );
}

/** A running draft: who is drafting, where to watch it, and how to stop it. */
function DraftRunning({ draft }: { draft: BoardDraft }) {
  const names = useNames();
  const nav = useProjectsNav();
  const stop = useStopDraft(draft.workstream);
  return (
    <div className="flex flex-col gap-2 text-sm">
      <p role="status">
        {names.member(draft.agent)} is drafting the board in {ENGINE_LABEL[draft.engine]}, in a private folder of its own
        and allowed only to read its prompt and send its proposal. Its proposal shows here when it is ready; it may ask
        you to allow a command in its terminal first (Codex asks before it sends). A draft stops after 30 minutes.
      </p>
      <div className="flex gap-2">
        {nav.openSession !== undefined && (
          <Button onClick={() => nav.openSession?.(draft.session, 'terminal')}>Open its session</Button>
        )}
        <Button variant="ghost" disabled={stop.isPending} onClick={() => stop.mutate(draft.session)}>
          Stop drafting
        </Button>
      </div>
      {stop.error !== null && <ErrorNote error={stop.error} what="stop the draft" />}
    </div>
  );
}

/** The review: each proposed task accepted or not; then only the accepted ones are created. */
export function DraftReview({ draft, onReviewed }: { draft: BoardDraft; onReviewed?: (done: DraftReviewed) => void }) {
  const names = useNames();
  const review = useReviewDraft(draft.workstream);
  // A draft's proposal never changes, so its review keeps its own choices (keyed by draft). Nothing
  // starts accepted: each task is the person's choice.
  const tasks = draft.proposal?.tasks ?? [];
  const [accepted, setAccepted] = useState<boolean[]>(() => tasks.map(() => false));
  const count = accepted.filter(Boolean).length;
  const send = (accept: number[]) =>
    review.mutate({ draft: draft.id, accept }, { onSuccess: (done) => onReviewed?.(done) });

  return (
    <div className="flex flex-col gap-3">
      <p className="text-sm text-ink">
        {names.member(draft.agent)} proposes {tasks.length} task{tasks.length === 1 ? '' : 's'}. Nothing is created
        until you accept it.
      </p>
      {draft.proposal?.note !== undefined && (
        <p className="rounded-sm border border-line bg-sunken p-2 text-sm text-ink-2">
          <span className="font-medium text-ink">Note: </span>
          {draft.proposal.note}
        </p>
      )}
      <ul aria-label="Proposed tasks" className="flex flex-col gap-2">
        {tasks.map((task, i) => (
          <li key={i} className="flex gap-2 rounded-sm border border-line p-2">
            <input
              type="checkbox"
              id={`${draft.id}-${i}`}
              checked={accepted[i] ?? false}
              onChange={(e) => setAccepted((all) => all.map((v, j) => (j === i ? e.target.checked : v)))}
              className="mt-1"
            />
            <div className="flex min-w-0 flex-1 flex-col gap-1">
              <label htmlFor={`${draft.id}-${i}`} className="flex flex-wrap items-center gap-2 text-sm font-medium text-ink">
                {task.title}
                <StatusPill tone={TASK_STATUS[task.status].tone}>{TASK_STATUS[task.status].label}</StatusPill>
              </label>
              {task.description !== undefined && <p className="text-sm text-ink-2">{task.description}</p>}
              {task.evidence.length > 0 && (
                <p className="text-xs text-ink-2">
                  From: {task.evidence.map((s) => `“${names.session(s)}”`).join(', ')}
                </p>
              )}
            </div>
          </li>
        ))}
      </ul>
      <div className="flex flex-wrap items-center gap-2">
        <Button variant="ghost" onClick={() => setAccepted(tasks.map(() => true))}>
          Select all
        </Button>
        <Button variant="ghost" onClick={() => setAccepted(tasks.map(() => false))}>
          Select none
        </Button>
        <span className="ml-auto" />
        <Button variant="secondary" disabled={review.isPending} onClick={() => send([])}>
          Reject all
        </Button>
        <Button
          variant="primary"
          disabled={review.isPending || count === 0}
          onClick={() => send(accepted.flatMap((on, i) => (on ? [i] : [])))}
        >
          {count === 0 ? 'Create no tasks' : `Create ${count} task${count === 1 ? '' : 's'}`}
        </Button>
      </div>
      {review.error !== null && <ErrorNote error={review.error} what="review the draft" />}
    </div>
  );
}

/** The workstream page's "Draft board" panel. */
export function DraftBoardPanel({ workstream, onClose }: { workstream: WorkstreamId; onClose: () => void }) {
  const drafts = useBoardDrafts(workstream);
  const [done, setDone] = useState<DraftReviewed>();
  const newest = drafts.data?.[0];

  let body;
  if (done !== undefined) {
    body = (
      <p role="status" className="text-sm text-ink">
        {done.tasks.length === 0
          ? 'Rejected: no task was created.'
          : `Created ${done.tasks.map((t) => t.key).join(', ')} on the board, labelled “drafted”.`}
      </p>
    );
  } else if (drafts.error !== null) {
    body = <ErrorNote error={drafts.error} what="load the drafts" />;
  } else if (drafts.data === undefined) {
    body = (
      <p role="status" className="text-sm text-ink-2">
        Loading…
      </p>
    );
  } else if (newest?.state === 'proposed') {
    body = <DraftReview key={newest.id} draft={newest} onReviewed={setDone} />;
  } else if (newest?.state === 'running') {
    body = <DraftRunning draft={newest} />;
  } else {
    body = (
      <div className="flex flex-col gap-2">
        {newest?.state === 'ended' && (
          <p className="text-sm text-ink-2">The last draft ended without a proposal.</p>
        )}
        <DraftStart workstream={workstream} />
      </div>
    );
  }

  return (
    <Panel
      title="Draft the board from history"
      actions={
        <Button variant="ghost" onClick={onClose}>
          Close
        </Button>
      }
    >
      {body}
    </Panel>
  );
}

/** Whether `workstream` has a proposal waiting for review, for the page's notice. */
export function useWaitingProposal(workstream: WorkstreamId): BoardDraft | undefined {
  const drafts = useBoardDrafts(workstream);
  const newest = drafts.data?.[0];
  return newest?.state === 'proposed' ? newest : undefined;
}
