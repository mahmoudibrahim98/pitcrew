// The composer: types a prompt into the session. Enter sends, Shift+Enter adds a line. Esc and
// Ctrl+C send those keys; Stop interrupts the turn. Disabled, with the reason, when the session
// has ended or cannot be reached.

import { useId, useState, type KeyboardEvent } from 'react';
import { ApiError, useMachines, useSession } from '../data/index.ts';
import { Button } from '../design/index.ts';
import { cx } from '../lib/cx.ts';
import { useInterrupt, useSendKeys, useSendText } from './data.ts';
import { inputBlocked } from './format.ts';
import { isComposing } from './ime.ts';

export interface ComposerProps {
  sessionId: string;
  className?: string;
}

export function Composer({ sessionId, className }: ComposerProps) {
  const session = useSession(sessionId);
  const machines = useMachines();
  const machine = machines.data?.find((m) => m.id === session.data?.machine);
  const send = useSendText(sessionId);
  const keys = useSendKeys(sessionId);
  const interrupt = useInterrupt(sessionId);
  const [text, setText] = useState('');
  // A 503 blocks input until the session or its machine changes.
  const [unreachable, setUnreachable] = useState<{ at: string; message: string } | undefined>();
  const reasonId = useId();
  const inputId = useId();

  const snapshot = `${session.data?.state}:${session.data?.last_activity}:${machine?.liveness}`;
  const blocked =
    inputBlocked(session.data, machine) ?? (unreachable?.at === snapshot ? unreachable.message : undefined);

  const error = [send.error, keys.error, interrupt.error].find(
    (e): e is Error => e !== null && !(e instanceof ApiError && e.code === 'unavailable'),
  );

  const onError = (e: Error) => {
    if (e instanceof ApiError && e.code === 'unavailable') setUnreachable({ at: snapshot, message: e.message });
  };

  const submit = () => {
    const value = text;
    if (value.trim() === '' || blocked !== undefined) return;
    setText('');
    send.mutate(value, {
      onError: (e) => {
        onError(e);
        setText((current) => (current === '' ? value : current));
      },
    });
  };

  const onKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    if (event.key === 'Enter' && !event.shiftKey && !isComposing(event)) {
      event.preventDefault();
      submit();
    }
  };

  const disabled = blocked !== undefined;
  const rows = Math.min(8, Math.max(2, text.split('\n').length));
  return (
    <div className={cx('border-t border-line bg-card px-3 py-2', className)}>
      {blocked !== undefined && (
        <p id={reasonId} role="status" className="mb-1.5 text-xs text-ink-2">
          {blocked}
        </p>
      )}
      <label htmlFor={inputId} className="sr-only">
        Message to the agent
      </label>
      <textarea
        id={inputId}
        value={text}
        rows={rows}
        disabled={disabled}
        aria-describedby={disabled ? reasonId : undefined}
        onChange={(e) => setText(e.target.value)}
        onKeyDown={onKeyDown}
        placeholder={disabled ? '' : 'Message the agent. Enter sends, Shift+Enter adds a line.'}
        className="block w-full resize-none rounded-md border border-line-2 bg-bg px-2.5 py-1.5 text-sm disabled:opacity-60"
      />
      <div className="mt-1.5 flex items-center gap-1.5">
        <Button
          variant="ghost"
          disabled={disabled || keys.isPending}
          onClick={() => keys.mutate(['escape'], { onError })}
          aria-label="Send Escape"
          title="Send Escape"
        >
          Esc
        </Button>
        <Button
          variant="ghost"
          disabled={disabled || keys.isPending}
          onClick={() => keys.mutate(['ctrl_c'], { onError })}
          aria-label="Send Ctrl+C"
          title="Send Ctrl+C"
        >
          Ctrl+C
        </Button>
        <Button
          variant="ghost"
          disabled={disabled || interrupt.isPending || session.data?.state !== 'working'}
          onClick={() => interrupt.mutate(undefined, { onError })}
          title="Stop the current turn"
        >
          Stop
        </Button>
        {error !== undefined && (
          <span role="alert" className="min-w-0 truncate text-xs text-risk">
            {error instanceof ApiError ? error.message : 'Could not reach the hub.'}
          </span>
        )}
        <Button
          variant="primary"
          className="ml-auto"
          disabled={disabled || text.trim() === ''}
          onClick={submit}
        >
          Send
        </Button>
      </div>
    </div>
  );
}
