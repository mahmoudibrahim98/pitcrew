// The Orchestrator panel's conversation (API v1, "Orchestrator"). A lazy chunk of the panel.
//
// - Asking: the composer sends a new conversation's question, or a follow-up in the one on
//   screen (Enter sends, Shift+Enter breaks the line). The agent CLI is the person's choice,
//   remembered by the hub; a follow-up keeps its conversation's.
// - The answer streams in: the hub follows the session's transcript, and this polls while it
//   answers. It is untrusted text, shown as text (`answer.tsx`), with the references the hub
//   checked as links to app routes. Under it: what it took (from the transcript), or why it ended.
// - Suggestions are buttons; nothing happens until the person clicks one, and a move asks first.
// - Esc (in the panel) or Stop ends an answer under way. New conversation, the history of earlier
//   ones, and Clear history (which asks first) are in the toolbar.
// - No agent CLI installed on the hub's machine, or one that does not start answering (it may wait
//   in its terminal to sign in, or to trust its folder): the panel says so and links to signing in
//   and to the session's terminal.

import { Link, useRouter } from '@tanstack/react-router';
import { useEffect, useRef, useState, type KeyboardEvent } from 'react';
import {
  ApiError,
  useAsk,
  useCancelAnswer,
  useClearConversations,
  useMoveTask,
  useOrchestrator,
  type AnswerSuggestion,
  type Conversation,
  type Engine,
  type Orchestrator,
  type OrchestratorTurn,
} from '../data/index.ts';
import {
  Button,
  Dialog,
  DialogContent,
  DialogFooter,
  FOCUS_RING,
  Menu,
  MenuContent,
  MenuItem,
  MenuLabel,
  MenuRadioGroup,
  MenuRadioItem,
  MenuSeparator,
  MenuTrigger,
  PlusIcon,
  SparkleIcon,
} from '../design/index.ts';
import { cx } from '../lib/cx.ts';
import { Answer, referencePath } from './answer.tsx';
import { useWorkspaceId } from './layout.ts';
import { paths } from './paths.ts';

export const ENGINE_NAMES: Record<Engine, string> = { claude: 'Claude Code', codex: 'Codex', opencode: 'OpenCode' };

/** Questions offered on an empty panel. */
export const EXAMPLES = ['What did my agents do today?', 'What is blocked?', 'Which session touched the method section?'];

const LINK = 'rounded-sm text-accent-text underline underline-offset-2 hover:text-ink';

/** How long an answer may show nothing before the panel suggests looking at its CLI. */
const QUIET_MS = 20_000;

/** Seconds, minutes or hours, briefly. */
export function duration(ms: number): string {
  const s = Math.max(0, Math.round(ms / 1000));
  if (s < 60) return `${s} s`;
  const m = Math.round(s / 60);
  return m < 60 ? `${m} min` : `${Math.round(m / 6) / 10} h`;
}

/** Bytes, briefly. */
export function size(bytes: number): string {
  return bytes < 1024 ? `${bytes} B` : `${(bytes / 1024).toFixed(1)} KB`;
}

/** What a turn took, or why it ended: shown under its answer. */
export function usageLine(turn: OrchestratorTurn): string | undefined {
  const usage = turn.usage;
  const took =
    usage === undefined
      ? []
      : [
          duration(usage.duration_ms),
          `${usage.tool_runs} tool ${usage.tool_runs === 1 ? 'run' : 'runs'}`,
          size(usage.answer_bytes),
        ];
  switch (turn.state) {
    case 'answering':
      return undefined;
    case 'answered':
      return `Answered in ${took.join(' · ')}`;
    case 'canceled':
      return `Stopped after ${took.join(' · ')}`;
    case 'timed_out':
      return `Timed out after ${took.join(' · ')}`;
    case 'too_long':
      return `Cut at ${took.join(' · ')}`;
    case 'failed':
      return 'No answer';
  }
}

function messageOf(error: unknown): string {
  if (error instanceof ApiError) return error.message;
  return error instanceof Error ? error.message : 'Something went wrong.';
}

export function OrchestratorChat() {
  const state = useOrchestrator();
  const data = state.data;
  // Which conversation is on screen: the newest unless the person picked one, or a new one.
  const [picked, setPicked] = useState<string | 'new' | undefined>(undefined);
  const conversation =
    picked === 'new' ? undefined : (data?.conversations.find((c) => c.id === picked) ?? (picked === undefined ? data?.conversations[0] : undefined));
  const [engine, setEngine] = useState<Engine | undefined>(undefined);
  const chosen: Engine = conversation?.engine ?? engine ?? data?.engine ?? firstInstalled(data) ?? 'claude';
  const installed = data?.engines.some((e) => e.installed) ?? true;
  const cancel = useCancelAnswer();
  const under = conversation?.turns.find((t) => t.state === 'answering');

  const stop = () => {
    if (conversation !== undefined && under !== undefined && !cancel.isPending) cancel.mutate(conversation.id);
  };
  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    // Not from a menu or dialog of the panel's (portaled elsewhere): their Esc closes them.
    if (e.key === 'Escape' && under !== undefined && e.currentTarget.contains(e.target as Node)) {
      e.preventDefault();
      e.stopPropagation();
      stop();
    }
  };

  if (state.error !== null && data === undefined) {
    return <p className="p-4 text-sm text-risk">{messageOf(state.error)}</p>;
  }
  return (
    // Esc anywhere in the panel stops the answer under way.
    <div className="flex min-h-0 flex-1 flex-col" onKeyDown={onKeyDown}>
      <Toolbar
        data={data}
        conversation={conversation}
        engine={chosen}
        onEngine={setEngine}
        onNew={() => setPicked('new')}
        onPick={setPicked}
      />
      <div className="min-h-0 flex-1 overflow-y-auto" aria-live="polite">
        {data === undefined ? (
          <p className="p-4 text-sm text-ink-2">Loading…</p>
        ) : !installed ? (
          <NoEngine />
        ) : conversation === undefined ? (
          <Empty />
        ) : (
          <Turns conversation={conversation} />
        )}
      </div>
      {cancel.error !== null && <p className="px-3 pb-1 text-xs text-risk">{messageOf(cancel.error)}</p>}
      <Composer
        data={data}
        conversation={conversation}
        engine={chosen}
        busy={under !== undefined}
        stopping={cancel.isPending}
        onStop={stop}
        onAsked={(id) => setPicked(id)}
        disabled={data === undefined || !installed}
      />
    </div>
  );
}

function firstInstalled(data: Orchestrator | undefined): Engine | undefined {
  return data?.engines.find((e) => e.installed)?.engine;
}

function Toolbar({
  data,
  conversation,
  engine,
  onEngine,
  onNew,
  onPick,
}: {
  data: Orchestrator | undefined;
  conversation: Conversation | undefined;
  engine: Engine;
  onEngine: (engine: Engine) => void;
  onNew: () => void;
  onPick: (id: string) => void;
}) {
  const [confirming, setConfirming] = useState(false);
  const clear = useClearConversations();
  const history = data?.conversations ?? [];
  return (
    <div className="flex shrink-0 items-center gap-1 border-b border-line px-2 py-1.5">
      <Menu>
        <MenuTrigger asChild>
          <Button variant="ghost" className="h-6 px-2 text-xs" aria-label={`Agent CLI: ${ENGINE_NAMES[engine]}`}>
            {ENGINE_NAMES[engine]}
          </Button>
        </MenuTrigger>
        <MenuContent>
          <MenuLabel>
            {conversation === undefined ? 'Answer with' : 'This conversation answers with'}
          </MenuLabel>
          <MenuRadioGroup value={engine} onValueChange={(value) => onEngine(value as Engine)}>
            {(data?.engines ?? []).map((e) => (
              <MenuRadioItem key={e.engine} value={e.engine} disabled={!e.installed || conversation !== undefined}>
                {ENGINE_NAMES[e.engine]}
                {e.installed ? '' : ' (not installed)'}
              </MenuRadioItem>
            ))}
          </MenuRadioGroup>
        </MenuContent>
      </Menu>
      <Button variant="ghost" className="ml-auto h-6 px-2 text-xs" onClick={onNew} disabled={conversation === undefined}>
        <PlusIcon className="size-3.5" />
        New conversation
      </Button>
      <Menu>
        <MenuTrigger asChild>
          <Button variant="ghost" className="h-6 px-2 text-xs" disabled={history.length === 0}>
            History
          </Button>
        </MenuTrigger>
        <MenuContent align="end" className="max-w-80">
          <MenuLabel>Your conversations</MenuLabel>
          {history.map((c) => (
            <MenuItem key={c.id} onSelect={() => onPick(c.id)}>
              {c.turns[0]?.question ?? 'Untitled'}
            </MenuItem>
          ))}
          <MenuSeparator />
          <MenuItem onSelect={() => setConfirming(true)}>Clear history…</MenuItem>
        </MenuContent>
      </Menu>
      <Dialog open={confirming} onOpenChange={setConfirming}>
        <DialogContent
          title="Clear the Orchestrator's history?"
          description="Your questions and their answers are forgotten, and the session answering them ends. The sessions stay in the Agent console."
        >
          {clear.error !== null && <p className="px-4 pt-2 text-sm text-risk">{messageOf(clear.error)}</p>}
          <DialogFooter>
            <Button onClick={() => setConfirming(false)}>Keep it</Button>
            <Button
              variant="primary"
              disabled={clear.isPending}
              onClick={() => clear.mutate(undefined, { onSuccess: () => { setConfirming(false); onNew(); } })}
            >
              Clear history
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}

function NoEngine() {
  const ws = useWorkspaceId();
  return (
    <div className="flex flex-col gap-2 p-4 text-sm">
      <p className="font-medium">No agent CLI to ask with</p>
      <p className="text-ink-2">
        The Orchestrator answers with an agent CLI you already use: Claude Code or OpenCode, on this
        hub&apos;s machine. None of them is installed there yet.
      </p>
      <Link to={paths.signIn(ws)} className={LINK}>
        Install one and sign in
      </Link>
    </div>
  );
}

function Empty() {
  return (
    <div className="flex flex-col items-center gap-2 p-6 text-center">
      <span className="inline-flex size-10 items-center justify-center rounded-pill bg-accent-soft text-accent-text">
        <SparkleIcon className="size-5" />
      </span>
      <p className="text-sm font-medium">Ask about your work</p>
      <p className="max-w-64 text-sm text-ink-2">
        What your agents did, what is blocked, where something was decided, which session touched a file. Answers
        link to the sessions, tasks and recaps they used.
      </p>
    </div>
  );
}

function Turns({ conversation }: { conversation: Conversation }) {
  const end = useRef<HTMLDivElement>(null);
  const last = conversation.turns.at(-1);
  useEffect(() => {
    end.current?.scrollIntoView?.({ block: 'end' });
  }, [conversation.turns.length, last?.answer.length]);
  return (
    <>
      <ol aria-label="Conversation" className="flex flex-col gap-4 p-3">
        {conversation.turns.map((turn, i) => (
          <Turn key={`${turn.session}-${turn.asked}-${i}`} turn={turn} />
        ))}
      </ol>
      <div ref={end} />
    </>
  );
}

function Turn({ turn }: { turn: OrchestratorTurn }) {
  const ws = useWorkspaceId();
  const router = useRouter();
  const open = (path: string) => void router.navigate({ href: path });
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (turn.state !== 'answering') return;
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(timer);
  }, [turn.state]);
  const quiet = turn.state === 'answering' && turn.answer === '' && now - turn.asked > QUIET_MS;
  const usage = usageLine(turn);
  return (
    <li className="flex flex-col gap-2">
      <p className="self-end rounded-md bg-accent-soft px-2.5 py-1.5 text-sm whitespace-pre-wrap text-ink">
        {turn.question}
      </p>
      {turn.answer !== '' && <Answer text={turn.answer} references={turn.references} ws={ws} open={open} />}
      {turn.state === 'answering' && (
        <p className="text-xs text-ink-2" role="status">
          Answering… {duration(now - turn.asked)}
        </p>
      )}
      {quiet && (
        <p className="text-xs text-ink-2">
          Its agent CLI has not started answering. It may be waiting in its terminal, to sign in or to trust its
          folder.{' '}
          <Link to={paths.session(ws, turn.session)} className={LINK}>
            Open its terminal
          </Link>{' '}
          or{' '}
          <Link to={paths.signIn(ws)} className={LINK}>
            sign in
          </Link>
          .
        </p>
      )}
      {turn.suggestions.length > 0 && <Suggestions suggestions={turn.suggestions} open={open} />}
      {(usage !== undefined || turn.note !== undefined) && (
        <p className="text-xs text-ink-2">
          {usage}
          {turn.note !== undefined && `${usage === undefined ? '' : '. '}${turn.note}`}
        </p>
      )}
    </li>
  );
}

/**
 * What a suggestion is, whatever its place: its kind, its task and status, or its target. While an
 * answer streams its suggestions can be added or dropped, so one is followed by this, never by its
 * position, and a confirmation always moves the task it showed.
 */
export function suggestionKey(s: AnswerSuggestion): string {
  return s.kind === 'move_task' ? `move_task:${s.task}:${s.to}` : `open:${JSON.stringify(s.target)}`;
}

function Suggestions({ suggestions, open }: { suggestions: AnswerSuggestion[]; open: (path: string) => void }) {
  const ws = useWorkspaceId();
  const move = useMoveTask();
  const [confirming, setConfirming] = useState<string | undefined>(undefined);
  const [done, setDone] = useState<Set<string>>(() => new Set());
  const confirmed = suggestions.find(
    (s): s is Extract<AnswerSuggestion, { kind: 'move_task' }> =>
      s.kind === 'move_task' && suggestionKey(s) === confirming,
  );
  return (
    <div className="flex flex-col gap-1.5" role="group" aria-label="Suggestions">
      <p className="text-xs text-ink-2">Suggested, if you choose:</p>
      <div className="flex flex-wrap gap-1.5">
        {suggestions.map((s) => {
          const key = suggestionKey(s);
          return s.kind === 'open' ? (
            <Button key={key} className="h-6 text-xs" onClick={() => open(referencePath(ws, s.target))}>
              {s.label}
            </Button>
          ) : (
            <Button
              key={key}
              className="h-6 text-xs"
              disabled={done.has(key) || move.isPending}
              aria-pressed={confirming === key}
              onClick={() => setConfirming(confirming === key ? undefined : key)}
            >
              {done.has(key) ? `${s.label}: done` : s.label}
            </Button>
          );
        })}
      </div>
      {confirmed !== undefined && (
        <ConfirmMove
          suggestion={confirmed}
          pending={move.isPending}
          onCancel={() => setConfirming(undefined)}
          onConfirm={(s) =>
            move.mutate(
              { task: s.task, to: s.to },
              {
                onSuccess: () => {
                  setDone((d) => new Set(d).add(suggestionKey(s)));
                  setConfirming(undefined);
                },
              },
            )
          }
        />
      )}
      {move.error !== null && <p className="text-xs text-risk">{messageOf(move.error)}</p>}
    </div>
  );
}

function ConfirmMove({
  suggestion,
  pending,
  onCancel,
  onConfirm,
}: {
  suggestion: Extract<AnswerSuggestion, { kind: 'move_task' }>;
  pending: boolean;
  onCancel: () => void;
  onConfirm: (s: Extract<AnswerSuggestion, { kind: 'move_task' }>) => void;
}) {
  return (
    <div className="flex items-center gap-2 rounded-sm border border-line px-2 py-1.5 text-xs" role="group" aria-label="Confirm">
      <span className="min-w-0 flex-1">
        Move {suggestion.key} to {suggestion.to.replace('_', ' ')}? You make this change, not the agent.
      </span>
      <Button className="h-6 text-xs" onClick={onCancel}>
        Cancel
      </Button>
      <Button variant="primary" className="h-6 text-xs" disabled={pending} onClick={() => onConfirm(suggestion)}>
        Move
      </Button>
    </div>
  );
}

function Composer({
  data,
  conversation,
  engine,
  busy,
  stopping,
  onStop,
  onAsked,
  disabled,
}: {
  data: Orchestrator | undefined;
  conversation: Conversation | undefined;
  engine: Engine;
  busy: boolean;
  stopping: boolean;
  onStop: () => void;
  onAsked: (conversation: string) => void;
  disabled: boolean;
}) {
  const ask = useAsk();
  const [text, setText] = useState('');
  const max = data?.limits.question_chars ?? 4000;
  const length = [...text.trim()].length;
  const anyAnswering = data?.conversations.some((c) => c.turns.some((t) => t.state === 'answering')) === true;
  const full = conversation !== undefined && data !== undefined && conversation.turns.length >= data.limits.turns;
  const canSend = !disabled && !anyAnswering && !ask.isPending && length > 0 && length <= max && !full;
  const send = () => {
    if (!canSend) return;
    const question = text;
    ask.mutate(
      conversation === undefined ? { text: question, engine } : { text: question, conversation: conversation.id },
      {
        onSuccess: (answered) => {
          setText('');
          onAsked(answered.id);
        },
      },
    );
  };
  const onKeyDown = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === 'Enter' && !e.shiftKey && !e.nativeEvent.isComposing) {
      e.preventDefault();
      send();
    }
  };
  return (
    <form
      className="flex shrink-0 flex-col gap-1.5 border-t border-line p-2"
      onSubmit={(e) => {
        e.preventDefault();
        send();
      }}
    >
      {conversation === undefined && !disabled && (
        <div className="flex flex-wrap gap-1">
          {EXAMPLES.map((example) => (
            <button
              key={example}
              type="button"
              className={cx('rounded-pill border border-line px-2 py-0.5 text-xs text-ink-2 hover:bg-hover', FOCUS_RING)}
              onClick={() => setText(example)}
            >
              {example}
            </button>
          ))}
        </div>
      )}
      <textarea
        aria-label="Ask the Orchestrator"
        rows={2}
        value={text}
        disabled={disabled}
        placeholder={conversation === undefined ? 'Ask about your work…' : 'Ask a follow-up…'}
        onChange={(e) => setText(e.target.value)}
        onKeyDown={onKeyDown}
        className={cx(
          'resize-none rounded-sm border border-line-2 bg-bg px-2 py-1.5 text-sm text-ink outline-none placeholder:text-ink-3',
          FOCUS_RING,
        )}
      />
      <div className="flex items-center gap-2 text-xs text-ink-2">
        <span className="min-w-0 flex-1 truncate">
          {ask.error !== null
            ? <span className="text-risk">{messageOf(ask.error)}</span>
            : full
              ? 'This conversation is full: start a new one.'
              : anyAnswering && !busy
                ? 'Another conversation is answering: one answer at a time.'
              : length > max
                ? `${length} of ${max} characters`
                : `Answers with ${ENGINE_NAMES[engine]}, which can only read.`}
        </span>
        {busy ? (
          <Button className="h-6 text-xs" onClick={onStop} disabled={stopping} aria-keyshortcuts="Escape">
            Stop
          </Button>
        ) : (
          <Button variant="primary" type="submit" className="h-6 text-xs" disabled={!canSend}>
            Ask
          </Button>
        )}
      </div>
    </form>
  );
}
