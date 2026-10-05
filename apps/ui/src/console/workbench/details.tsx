// The details sidebar: what the tab on screen in the active pane is. For a session, its task,
// workstream, machine, state, model and account, its actions (link to a task; hand off once that
// exists) and its workstream's files; for a file, where it is and its folder.

import { useRouter } from '@tanstack/react-router';
import { Suspense, useId, useMemo, useState, type MouseEvent, type ReactNode } from 'react';
import { fileClient } from '../../data/files.ts';
import { useApi, useMachines, useMembers, useSession, type Workstream } from '../../data/index.ts';
import { Button, StatusPill } from '../../design/index.ts';
import { FileTree } from '../../projects/index.ts';
import { paths } from '../../shell/index.ts';
import { usePersonas, useTaskById, useWorkstreamById } from '../data.ts';
import { ENGINE_LABEL, fullTime, LIVENESS, STATE } from '../format.ts';
import { LinkSessionDialog } from '../link-session.tsx';
import type { SessionView, Tab, TabRef } from './layout.ts';

export interface DetailsProps {
  ws: string;
  tab: Tab | undefined;
  /** Opens a file of a workstream's folder in the active pane. */
  onOpenFile(ref: TabRef): void;
  /** Opens the session in another view, in a new pane beside the active one. */
  onOpenBeside(session: string, view: SessionView): void;
}

const LINK = 'text-accent-text underline-offset-2 hover:underline';

function useNavigateLink() {
  const router = useRouter();
  return (href: string) => (event: MouseEvent<HTMLAnchorElement>) => {
    if (event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
    event.preventDefault();
    void router.navigate({ href });
  };
}

function Row({ term, children }: { term: string; children: ReactNode }) {
  return (
    <>
      <dt className="text-ink-2">{term}</dt>
      <dd className="min-w-0 break-words">{children}</dd>
    </>
  );
}

export function Details({ ws, tab, onOpenFile, onOpenBeside }: DetailsProps) {
  const heading = useId();
  return (
    <div data-pane="details" className="min-h-0 flex-1 overflow-y-auto">
      <section aria-labelledby={heading} className="space-y-4 p-4 text-sm">
        <h2 id={heading} tabIndex={-1} data-details-focus="" className="text-md font-semibold outline-none">
          Details
        </h2>
        {tab === undefined && <p className="text-ink-2">Nothing is open in this pane.</p>}
        {tab?.ref.kind === 'session' && (
          <SessionDetails ws={ws} session={tab.ref.session} onOpenFile={onOpenFile} onOpenBeside={onOpenBeside} />
        )}
        {tab?.ref.kind === 'file' && (
          <FileDetails file={tab.ref} onOpenFile={onOpenFile} />
        )}
      </section>
    </div>
  );
}

function SessionDetails({
  ws,
  session: id,
  onOpenFile,
  onOpenBeside,
}: {
  ws: string;
  session: string;
  onOpenFile(ref: TabRef): void;
  onOpenBeside(session: string, view: SessionView): void;
}) {
  const session = useSession(id);
  const machines = useMachines();
  const members = useMembers();
  const personas = usePersonas();
  const task = useTaskById(session.data?.task);
  const workstream = useWorkstreamById(session.data?.workstream);
  const follow = useNavigateLink();
  const [linking, setLinking] = useState(false);
  const handOff = useId();
  if (session.data === undefined) {
    return <p className="text-ink-2">{session.error !== null ? 'Could not load the session.' : 'Loading the session…'}</p>;
  }
  const s = session.data;
  const machine = machines.data?.find((m) => m.id === s.machine);
  const agent = s.agent === undefined ? undefined : members.data?.find((m) => m.id === s.agent);
  const persona = agent?.persona === undefined ? undefined : personas.data?.find((p) => p.id === agent.persona);
  // What the transcript records first; else an agent's persona may name its model.
  const model =
    s.model ?? persona?.model ?? (persona !== undefined ? `The CLI's default (${persona.name})` : 'Not recorded yet');
  const linkedTask = task.data;
  const linkedWorkstream = workstream.data;
  const taskHref = linkedTask === undefined ? undefined : paths.task(ws, linkedTask.key);
  const workstreamHref =
    linkedWorkstream === undefined ? undefined : paths.workstream(ws, linkedWorkstream.project, linkedWorkstream.id);
  return (
    <>
      <dl className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1.5">
        <Row term="Task">
          {linkedTask !== undefined && taskHref !== undefined ? (
            <a href={taskHref} className={LINK} onClick={follow(taskHref)}>
              {linkedTask.key} · {linkedTask.title}
            </a>
          ) : (
            <span className="text-ink-2">None</span>
          )}
        </Row>
        <Row term="Workstream">
          {linkedWorkstream !== undefined && workstreamHref !== undefined ? (
            <a href={workstreamHref} className={LINK} onClick={follow(workstreamHref)}>
              {linkedWorkstream.name}
            </a>
          ) : (
            <span className="text-ink-2">None</span>
          )}
        </Row>
        <Row term="Machine">
          {machine?.name ?? '…'}
          {machine !== undefined && machine.liveness !== 'live' && (
            <span className="ml-1 text-risk">({LIVENESS[machine.liveness].label.toLowerCase()})</span>
          )}
        </Row>
        <Row term="State">
          <StatusPill tone={STATE[s.state].tone}>{STATE[s.state].label}</StatusPill>
        </Row>
        <Row term="Model">{model}</Row>
        <Row term="Account">
          {s.account === undefined ? (
            <span className="text-ink-2">Not reported</span>
          ) : (
            <span className="font-mono text-xs" title="The CLI account home its transcript is in">
              {s.account}
            </span>
          )}
        </Row>
        <Row term="Engine">{ENGINE_LABEL[s.engine]}</Row>
        <Row term="Agent">{agent?.handle ?? (s.parent !== undefined ? 'None (a sub-agent of its session)' : 'None (run by a person)')}</Row>
        <Row term="Started">{fullTime(s.started)}</Row>
      </dl>

      <div className="space-y-2">
        <h3 className="font-medium">Actions</h3>
        <div className="flex flex-wrap gap-1.5">
          <Button onClick={() => setLinking(true)}>Link to a task…</Button>
          <Button aria-disabled="true" aria-describedby={handOff} className="cursor-not-allowed opacity-60">
            Hand off
          </Button>
          {s.terminal !== undefined && <Button onClick={() => onOpenBeside(s.id, 'terminal')}>Terminal beside</Button>}
          <Button onClick={() => onOpenBeside(s.id, 'chat')}>Chat beside</Button>
        </div>
        <p id={handOff} className="text-xs text-ink-2">
          Hand off is not available yet: the hub has no way to pass a session on.
        </p>
      </div>
      {linking && <LinkSessionDialog session={s} onClose={() => setLinking(false)} />}

      {linkedWorkstream !== undefined && <WorkstreamFiles workstream={linkedWorkstream} onOpenFile={onOpenFile} />}
    </>
  );
}

function FileDetails({ file, onOpenFile }: { file: Extract<TabRef, { kind: 'file' }>; onOpenFile(ref: TabRef): void }) {
  const workstream = useWorkstreamById(file.workstream).data;
  return (
    <>
      <dl className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1.5">
        <Row term="Workstream">{workstream?.name ?? '…'}</Row>
        <Row term="Folder">
          <span className="font-mono text-xs">{workstream?.locations[file.location]?.path ?? '…'}</span>
        </Row>
        <Row term="File">
          <span className="font-mono text-xs">{file.path}</span>
        </Row>
      </dl>
      {workstream !== undefined && <WorkstreamFiles workstream={workstream} initial={file.location} onOpenFile={onOpenFile} />}
    </>
  );
}

/** A workstream's folders, to open files from. */
function WorkstreamFiles({
  workstream,
  initial = 0,
  onOpenFile,
}: {
  workstream: Workstream;
  initial?: number;
  onOpenFile(ref: TabRef): void;
}) {
  const api = useApi();
  const [location, setLocation] = useState(initial);
  const client = useMemo(() => fileClient(api, workstream.id, location), [api, workstream.id, location]);
  const heading = useId();
  if (workstream.locations.length === 0) return null;
  return (
    <div className="space-y-2">
      <h3 id={heading} className="font-medium">
        Files
      </h3>
      {workstream.locations.length > 1 ? (
        <label className="block text-xs text-ink-2">
          Location
          <select
            className="ml-2 max-w-full rounded-sm border border-line bg-card p-1 text-ink"
            value={location}
            onChange={(event) => setLocation(Number(event.target.value))}
          >
            {workstream.locations.map((loc, index) => (
              <option key={index} value={index}>
                {loc.path}
              </option>
            ))}
          </select>
        </label>
      ) : (
        <p className="font-mono text-xs break-all text-ink-2">{workstream.locations[0]?.path}</p>
      )}
      <Suspense fallback={<p role="status">Loading the folders…</p>}>
        <FileTree
          key={location}
          client={client}
          scope={['files', workstream.id, location]}
          openFile={(path) => onOpenFile({ kind: 'file', workstream: workstream.id, location, path })}
        />
      </Suspense>
    </div>
  );
}
