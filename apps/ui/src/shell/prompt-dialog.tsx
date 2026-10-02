// One SSH prompt (desktop-gateway.md, "Prompts"). It says which host is asking, where an answer
// goes (`kind` says who asks: the host for `password` and `otp`, nowhere off this computer for
// `passphrase`), and shows ssh's own text as plain text (it is untrusted: never markup), in a box
// that scrolls so the field and the buttons always show.
//
// - A password, passphrase or code goes in a password field with autocomplete off. The field is
//   uncontrolled: React would copy a controlled input's value into its `value` attribute, and so
//   into the DOM, snapshots and traces. The answer is read from the field once, as it is sent, and
//   the field is cleared at that moment. It is never in state, a store, a cache, the URL, storage
//   or a log.
// - A host key shows its fingerprint; `confirm` is ssh's other yes/no questions: both have Reject
//   and Accept. A `notice` ("touch your security key") has nothing to answer: it stays until the
//   gateway withdraws it, and only Stop stops ssh (no close button, Esc does nothing).
// - Every other kind can be cancelled, which stops the sign-in: by its "Cancel sign-in" button or
//   by Esc. A click outside the dialog does nothing, so a stray click never cancels.
// - Enter does nothing for a moment after the dialog opens, so typing meant for somewhere else
//   cannot send a half-typed password.

import { useEffect, useId, useRef, useState, type KeyboardEvent } from 'react';
import type { GatewayPrompt, PromptKind, PromptReply } from '../data/index.ts';
import { Button, Dialog, DialogContent, DialogFooter, FOCUS_RING } from '../design/index.ts';
import { cx } from '../lib/cx.ts';

/** How long Enter is ignored after the dialog opens. */
export const ENTER_GRACE_MS = 300;

const TITLE: Record<PromptKind, (host: string) => string> = {
  password: (host) => `${host} asks for a password`,
  passphrase: (host) => `Unlock your key to reach ${host}`,
  otp: (host) => `${host} asks for a one-time code`,
  host_key: (host) => `Check ${host}'s host key`,
  confirm: (host) => `ssh asks about ${host}`,
  notice: (host) => `${host} is waiting for you`,
};

/** Where the answer goes, or what the prompt is: the contract's provenance rule, in words. */
const WHERE: Record<PromptKind, (host: string) => string> = {
  password: (host) => `Sent to ${host}. Not kept here.`,
  otp: (host) => `Sent to ${host}. Not kept here.`,
  passphrase: (host) => `Unlocks your key on this computer; not sent to ${host}.`,
  host_key: () => 'Trust this host’s key?',
  confirm: () => 'A yes-or-no question from ssh, on this computer.',
  notice: () => 'Information only: there is nothing to answer.',
};

/** Whose words the text is: the host's for `password` and `otp`; ssh's own, on this computer, else. */
const SAYS: Record<PromptKind, (host: string) => string> = {
  password: (host) => `${host} says`,
  otp: (host) => `${host} says`,
  passphrase: (host) => `ssh says, for ${host}`,
  host_key: (host) => `ssh says, for ${host}`,
  confirm: (host) => `ssh says, for ${host}`,
  notice: (host) => `ssh says, for ${host}`,
};

const FIELD: Record<'password' | 'passphrase' | 'otp', string> = {
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
  const notice = prompt.kind === 'notice';
  return (
    // Esc is the only way the dialog asks to close (outside clicks are refused below, and there is
    // no close button): it cancels the sign-in, except on a notice.
    <Dialog open onOpenChange={(open) => !open && !notice && reply({})}>
      <DialogContent
        title={TITLE[prompt.kind](prompt.host)}
        description={WHERE[prompt.kind](prompt.host)}
        showClose={false}
        onPointerDownOutside={(event) => event.preventDefault()}
        onInteractOutside={(event) => event.preventDefault()}
        onEscapeKeyDown={(event) => notice && event.preventDefault()}
        data-testid="gateway-prompt"
      >
        <div className="flex min-h-0 flex-col gap-3 px-4 py-3">
          <figure className="flex min-h-0 flex-col gap-1">
            <figcaption className="text-xs text-ink-2">{SAYS[prompt.kind](prompt.host)}:</figcaption>
            <div
              data-testid="prompt-text"
              role="region"
              aria-label={SAYS[prompt.kind](prompt.host)}
              // Focusable, so the box can be scrolled from the keyboard.
              tabIndex={0}
              className={cx(
                'max-h-48 overflow-auto rounded-sm bg-sunken px-3 py-2 font-mono text-sm break-words whitespace-pre-wrap outline-none',
                FOCUS_RING,
              )}
            >
              {prompt.text}
            </div>
          </figure>
          {waiting > 0 && (
            <p className="text-xs text-ink-2">
              {waiting === 1 ? 'One more prompt is waiting.' : `${waiting} more prompts are waiting.`}
            </p>
          )}
        </div>
        {prompt.kind === 'host_key' && <HostKey prompt={prompt} reply={reply} />}
        {prompt.kind === 'confirm' && (
          <DialogFooter>
            <CancelSignIn reply={reply} />
            <Button onClick={() => reply({ accept: false })}>Reject</Button>
            <Button variant="primary" onClick={() => reply({ accept: true })}>
              Accept
            </Button>
          </DialogFooter>
        )}
        {notice && (
          <>
            <p className="px-4 pb-4 text-sm text-ink-2">This closes by itself once ssh moves on.</p>
            <DialogFooter>
              {/* Nothing to answer; replying with neither field stops ssh. */}
              <Button onClick={() => reply({})}>Stop sign-in</Button>
            </DialogFooter>
          </>
        )}
        {(prompt.kind === 'password' || prompt.kind === 'passphrase' || prompt.kind === 'otp') && (
          <Answer label={FIELD[prompt.kind]} reply={reply} />
        )}
      </DialogContent>
    </Dialog>
  );
}

/** Replies with neither field: ssh stops. The same as Esc, and it says so. */
function CancelSignIn({ reply, onBeforeReply }: { reply(reply: PromptReply): void; onBeforeReply?: () => void }) {
  return (
    <Button
      variant="ghost"
      aria-keyshortcuts="Escape"
      className="mr-auto"
      onClick={() => {
        onBeforeReply?.();
        reply({});
      }}
    >
      Cancel sign-in
    </Button>
  );
}

function Answer({ label, reply }: { label: string; reply(reply: PromptReply): void }) {
  const field = useRef<HTMLInputElement>(null);
  const [empty, setEmpty] = useState(true);
  const openedAt = useRef<number | null>(null);
  const id = useId();
  useEffect(() => {
    openedAt.current = performance.now();
  }, []);

  const clear = () => {
    if (field.current !== null) field.current.value = '';
    setEmpty(true);
  };

  return (
    <form
      onSubmit={(event) => {
        event.preventDefault();
        const input = field.current;
        if (input === null || input.value === '') return;
        const value = input.value;
        // Cleared before it goes: nothing holds it once sent.
        input.value = '';
        setEmpty(true);
        reply({ answer: value });
      }}
    >
      <div className="flex flex-col gap-1.5 px-4 pb-4">
        <label htmlFor={id} className="text-sm font-medium text-ink">
          {label}
        </label>
        <input
          ref={field}
          id={id}
          type="password"
          autoComplete="off"
          autoCapitalize="off"
          autoCorrect="off"
          spellCheck={false}
          // The dialog opened for this: the field is where the person types.
          autoFocus
          onChange={(event) => setEmpty(event.target.value === '')}
          onKeyDown={(event: KeyboardEvent<HTMLInputElement>) => {
            // Typing meant for somewhere else, when the dialog took focus mid-word.
            const since = openedAt.current === null ? 0 : performance.now() - openedAt.current;
            if (event.key === 'Enter' && since < ENTER_GRACE_MS) event.preventDefault();
          }}
          className={cx('h-8 rounded-sm border border-line-2 bg-card px-2.5 text-sm text-ink outline-none', FOCUS_RING)}
        />
      </div>
      <DialogFooter>
        <CancelSignIn reply={reply} onBeforeReply={clear} />
        <Button type="submit" variant="primary" disabled={empty}>
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
          <dd data-testid="prompt-fingerprint" className="font-mono break-all text-ink">
            {known ? prompt.fingerprint : 'The gateway gave no fingerprint, so there is nothing to compare.'}
          </dd>
        </dl>
        <p className="text-ink-2">
          Accept only if it matches the fingerprint you were given for {prompt.host}. Accepting adds it to your own
          known_hosts.
        </p>
      </div>
      <DialogFooter>
        <CancelSignIn reply={reply} />
        <Button onClick={() => reply({ accept: false })}>Reject</Button>
        <Button variant="primary" disabled={!known} onClick={() => reply({ accept: true })}>
          Accept
        </Button>
      </DialogFooter>
    </>
  );
}
