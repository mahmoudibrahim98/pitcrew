// One SSH prompt (desktop-gateway.md, "Prompts"). It says which host is asking and shows ssh's own
// question as plain text (it is untrusted: never markup). A password, passphrase or code goes in a
// password field with autocomplete off; a new host key shows its fingerprint, with Accept and
// Reject. Closing it (Cancel, Esc, the close button) replies with neither, which cancels.
//
// The answer lives in this component's state and nowhere else: not in a store, a query cache, the
// URL, storage or a log. It is cleared as it is sent.

import { useId, useState } from 'react';
import type { GatewayPrompt, PromptKind, PromptReply } from '../data/index.ts';
import { Button, Dialog, DialogContent, DialogFooter, FOCUS_RING } from '../design/index.ts';
import { cx } from '../lib/cx.ts';

const TITLE: Record<PromptKind, (host: string) => string> = {
  password: (host) => `${host} asks for a password`,
  passphrase: (host) => `${host} asks for a key passphrase`,
  otp: (host) => `${host} asks for a one-time code`,
  host_key: (host) => `Check ${host}'s host key`,
};

const FIELD: Record<Exclude<PromptKind, 'host_key'>, string> = {
  password: 'Password',
  passphrase: 'Passphrase',
  otp: 'One-time code',
};

export function PromptDialog({
  prompt,
  waiting,
  reply,
}: {
  prompt: GatewayPrompt;
  /** Prompts queued behind this one. */
  waiting: number;
  reply(reply: PromptReply): void;
}) {
  return (
    <Dialog open onOpenChange={(open) => !open && reply({})}>
      <DialogContent
        title={TITLE[prompt.kind](prompt.host)}
        description={`SSH on ${prompt.host} is asking. Your answer goes to ssh once and is not kept.`}
        data-testid="gateway-prompt"
      >
        <div className="flex flex-col gap-3 px-4 py-3">
          <figure className="flex flex-col gap-1">
            <figcaption className="text-xs text-ink-2">{prompt.host} says:</figcaption>
            <p data-testid="prompt-text" className="whitespace-pre-wrap break-words rounded-sm bg-sunken px-3 py-2 font-mono text-sm">
              {prompt.text}
            </p>
          </figure>
          {waiting > 0 && (
            <p className="text-xs text-ink-2">
              {waiting === 1 ? 'One more prompt is waiting.' : `${waiting} more prompts are waiting.`}
            </p>
          )}
        </div>
        {prompt.kind === 'host_key' ? (
          <HostKey prompt={prompt} reply={reply} />
        ) : (
          <Answer label={FIELD[prompt.kind]} reply={reply} />
        )}
      </DialogContent>
    </Dialog>
  );
}

function Answer({ label, reply }: { label: string; reply(reply: PromptReply): void }) {
  const [answer, setAnswer] = useState('');
  const id = useId();
  return (
    <form
      onSubmit={(event) => {
        event.preventDefault();
        if (answer === '') return;
        const value = answer;
        // Cleared before it goes: nothing holds it once sent.
        setAnswer('');
        reply({ answer: value });
      }}
    >
      <div className="flex flex-col gap-1.5 px-4 pb-4">
        <label htmlFor={id} className="text-sm font-medium text-ink">
          {label}
        </label>
        <input
          id={id}
          type="password"
          autoComplete="off"
          autoCapitalize="off"
          autoCorrect="off"
          spellCheck={false}
          // The dialog opened for this: the field is where the person types.
          autoFocus
          value={answer}
          onChange={(event) => setAnswer(event.target.value)}
          className={cx('h-8 rounded-sm border border-line-2 bg-card px-2.5 text-sm text-ink outline-none', FOCUS_RING)}
        />
      </div>
      <DialogFooter>
        <Button
          onClick={() => {
            setAnswer('');
            reply({});
          }}
        >
          Cancel
        </Button>
        <Button type="submit" variant="primary" disabled={answer === ''}>
          Send
        </Button>
      </DialogFooter>
    </form>
  );
}

function HostKey({ prompt, reply }: { prompt: GatewayPrompt; reply(reply: PromptReply): void }) {
  const known = prompt.fingerprint !== undefined && prompt.fingerprint !== '';
  return (
    <>
      <div className="flex flex-col gap-2 px-4 pb-4 text-sm">
        <dl className="flex flex-col gap-1">
          <dt className="font-medium text-ink">Fingerprint</dt>
          <dd data-testid="prompt-fingerprint" className="break-all font-mono text-ink">
            {known ? prompt.fingerprint : 'The gateway gave no fingerprint, so there is nothing to compare.'}
          </dd>
        </dl>
        <p className="text-ink-2">
          Accept only if it matches the fingerprint you were given for {prompt.host}. Accepting adds it to your own
          known_hosts.
        </p>
      </div>
      <DialogFooter>
        <Button onClick={() => reply({ accept: false })}>Reject</Button>
        <Button variant="primary" disabled={!known} onClick={() => reply({ accept: true })}>
          Accept
        </Button>
      </DialogFooter>
    </>
  );
}
