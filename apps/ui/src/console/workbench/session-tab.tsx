// One session in a pane: its header (with links to its task and workstream), the Chat | Terminal |
// Work switch, then the chat and composer, the terminal, or its work. In a narrow console the
// terminal and work views take the pane, with only the header's title row above them.

import { useRouter } from '@tanstack/react-router';
import { createElement, lazy, Suspense, type ComponentType, type LazyExoticComponent, type ReactNode } from 'react';
import { ApiError, useSession } from '../../data/index.ts';
import { ConsoleIcon } from '../../design/index.ts';
import { SessionWork } from '../../projects/index.ts';
import { paths } from '../../shell/index.ts';
import { ChatView } from '../chat-view.tsx';
import { Composer } from '../composer.tsx';
import type { SessionView } from '../search.ts';
import { SessionHeader } from '../session-header.tsx';
import { TerminalBoundary } from '../terminal-boundary.tsx';
import type { TerminalViewProps } from '../terminal/terminal-view.tsx';
import { ViewSwitch } from '../view-switch.tsx';
import { WorkBoundary } from '../work-boundary.tsx';

// Its own chunk, with xterm: nothing of it loads until a terminal is shown. One lazy component per
// attempt: React keeps a failed import's error, so trying again needs a new one.
const loadTerminalView = () => import('../terminal/terminal-view.tsx').then((m) => ({ default: m.TerminalView }));
const terminalViews: LazyExoticComponent<ComponentType<TerminalViewProps>>[] = [];
function terminalView(attempt: number): LazyExoticComponent<ComponentType<TerminalViewProps>> {
  terminalViews[attempt] ??= lazy(loadTerminalView);
  return terminalViews[attempt];
}

export function Placeholder({ title, children }: { title: string; children: ReactNode }) {
  return (
    <div className="flex flex-1 flex-col items-center justify-center gap-2 p-6 text-center">
      <ConsoleIcon className="size-6 text-ink-2" />
      <p className="text-sm font-medium">{title}</p>
      <div className="max-w-80 text-sm text-ink-2">{children}</div>
    </div>
  );
}

export interface SessionPaneProps {
  ws: string;
  sessionId: string;
  view: SessionView;
  onView(view: SessionView): void;
  narrow: boolean;
  /**
   * Which pane shows the session, when there are several: the session's landmarks ("Chat",
   * "Linked work") get it in their names, so each stays unique on the page.
   */
  pane?: number | undefined;
}

export function SessionPane({ ws, sessionId, view, onView, narrow, pane }: SessionPaneProps) {
  const suffix = pane === undefined || pane === 1 ? '' : `, pane ${pane}`;
  const router = useRouter();
  const session = useSession(sessionId);
  if (session.error instanceof ApiError && session.error.code === 'not_found') {
    return (
      <Placeholder title="No such session">
        It may have been in another workspace, or the link may be wrong. Choose one from the list.
      </Placeholder>
    );
  }
  const open = (href: string) => void router.navigate({ href });
  const taskHref = (task: { key: string }) => paths.task(ws, task.key);
  const workstreamHref = (w: { id: string; project: string }) => paths.workstream(ws, w.project, w.id);
  const loaded = session.data !== undefined;
  const hasTerminal = session.data?.terminal !== undefined;
  // Until the session is here, a link to its terminal waits for it rather than flashing the chat.
  // Work needs nothing from the session itself (its own query fetches the blocks), so it shows at
  // once, like the chat.
  let shown: SessionView | 'loading' | 'failed' =
    view === 'terminal' && hasTerminal ? 'terminal' : view === 'work' ? 'work' : 'chat';
  if (view === 'terminal' && !loaded) shown = session.error === null ? 'loading' : 'failed';
  const switchValue: SessionView = shown === 'chat' ? 'chat' : shown === 'work' ? 'work' : 'terminal';
  return (
    <>
      <SessionHeader
        sessionId={sessionId}
        taskHref={taskHref}
        onOpenTask={(task) => open(taskHref(task))}
        workstreamHref={workstreamHref}
        onOpenWorkstream={(w) => open(workstreamHref(w))}
        compact={narrow && (shown === 'terminal' || shown === 'work')}
        linksLabel={`Linked work${suffix}`}
      />
      <ViewSwitch
        value={switchValue}
        onChange={onView}
        terminalUnavailable={loaded && !hasTerminal ? 'This session has no terminal.' : undefined}
      />
      {shown === 'loading' && <p className="p-4 text-sm text-ink-2">Loading the session…</p>}
      {shown === 'failed' && (
        <p role="alert" className="p-4 text-sm text-ink-2">
          Could not load the session{session.error instanceof ApiError ? `: ${session.error.message}` : '.'}
        </p>
      )}
      {shown === 'terminal' && (
        <div data-pane="terminal" className="flex min-h-0 flex-1 flex-col">
          <TerminalBoundary>
            {(attempt) => (
              <Suspense fallback={<p className="p-4 text-sm text-ink-2">Loading the terminal…</p>}>
                {createElement(terminalView(attempt), { sessionId })}
              </Suspense>
            )}
          </TerminalBoundary>
        </div>
      )}
      {shown === 'work' && (
        <div data-pane="work" className="min-h-0 flex-1 overflow-y-auto">
          {/* Focusable but not a tab stop, like the chat scroller: F6 lands here. Unlabelled,
              since SessionWork's own section (role="region", named "Work" by its heading) is the
              pane's landmark. */}
          <div
            tabIndex={-1}
            data-work-focus=""
            className="p-4 outline-none focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-accent focus-visible:outline-solid"
          >
            <WorkBoundary>
              {(attempt) => (
                <Suspense fallback={<p className="text-sm text-ink-2">Loading the session's work…</p>}>
                  {/* The session title above is already an h2; Work nests under it as an h3. */}
                  <SessionWork key={attempt} session={sessionId} level={3} />
                </Suspense>
              )}
            </WorkBoundary>
          </div>
        </div>
      )}
      {shown === 'chat' && (
        <>
          <div data-pane="chat" className="min-h-0 flex-1">
            <ChatView sessionId={sessionId} label={`Chat${suffix}`} />
          </div>
          <div data-pane="composer" className="shrink-0">
            <Composer sessionId={sessionId} />
          </div>
        </>
      )}
    </>
  );
}
