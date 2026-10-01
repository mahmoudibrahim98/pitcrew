// A question for a person, answerable in place. Two sources:
// - an ask (`POST /v1/asks/{id}/answer`), as in the Inbox or a transcript question raised as one;
// - a question in a live transcript with no ask, answered by typing into the session: an option
//   is picked with arrow keys and Enter (the CLI's picker starts on the first option), free text
//   is sent as a prompt.

import { useId, useState, type FormEvent } from 'react';
import { ApiError, useMembers, type Ask, type AskKind, type Session } from '../data/index.ts';
import { Button } from '../design/index.ts';
import { cx } from '../lib/cx.ts';
import { useAnswerAsk, useSendKeys, useSendText } from './data.ts';
import type { ItemOf, Key } from './types.ts';

const KIND_LABEL: Record<AskKind, string> = {
  question: 'Question',
  decision: 'Decision',
  review: 'Review',
  approval: 'Approval',
  mention: 'Mention',
};

export type QuestionCardProps =
  | {
      ask: Ask;
      session?: undefined;
      question?: undefined;
      answer?: undefined;
      open?: undefined;
      className?: string;
    }
  | {
      session: Session;
      question: ItemOf<'question'>;
      /** The ask the question was raised as, if any: answers then go to it. */
      ask?: Ask | undefined;
      /** The answer the transcript already records. */
      answer?: string | undefined;
      /** The question is the last thing in the transcript and unanswered. */
      open: boolean;
      className?: string;
    };

/** The keys that pick option `index` in a CLI's picker. */
export function keysForOption(index: number): Key[] {
  return [...Array.from({ length: index }, (): Key => 'down'), 'enter'];
}

function errorText(error: unknown): string {
  if (error instanceof ApiError) return error.message;
  return 'Something went wrong; try again.';
}

export function QuestionCard(props: QuestionCardProps) {
  const members = useMembers();
  const answerAsk = useAnswerAsk();
  const sessionId = props.session?.id ?? '';
  const sendKeys = useSendKeys(sessionId);
  const sendText = useSendText(sessionId);
  const [draft, setDraft] = useState('');
  const [sent, setSent] = useState<string | undefined>();
  const inputId = useId();

  // An ask the server has closed wins over this card's own answer; until then, the answer this
  // card sent shows at once.
  const ask = props.ask === undefined ? undefined : props.ask.state !== 'open' ? props.ask : (answerAsk.data ?? props.ask);
  const title = ask?.title ?? props.question?.text ?? '';
  const body = ask?.body ?? '';
  const options = ask?.options ?? props.question?.options ?? [];
  const from = ask === undefined ? undefined : members.data?.find((m) => m.id === ask.from)?.handle;

  let answered: string | undefined;
  if (ask !== undefined && ask.state !== 'open') {
    const a = ask.answer;
    answered =
      ask.state === 'withdrawn'
        ? 'Withdrawn'
        : [a?.option === undefined ? undefined : options[a.option], a?.text].filter(Boolean).join(' · ') || 'Answered';
  } else if (ask === undefined) {
    answered = props.answer?.replace(/^User answered:\s*/i, '') ?? sent;
  }

  const sessionLive =
    props.session !== undefined && props.session.state !== 'ended' && props.session.state !== 'unreachable';
  const answerable =
    answered === undefined && (ask !== undefined ? ask.state === 'open' : props.open === true && sessionLive);
  const busy = answerAsk.isPending || sendKeys.isPending || sendText.isPending;
  const error = answerAsk.error ?? sendKeys.error ?? sendText.error;

  const choose = (index: number) => {
    const label = options[index] ?? '';
    if (ask !== undefined) answerAsk.mutate({ ask: ask.id, option: index });
    else sendKeys.mutate(keysForOption(index), { onSuccess: () => setSent(label) });
  };

  const submitText = (event: FormEvent) => {
    event.preventDefault();
    const text = draft.trim();
    if (text === '' || !answerable || busy) return;
    if (ask !== undefined) answerAsk.mutate({ ask: ask.id, text }, { onSuccess: () => setDraft('') });
    else
      sendText.mutate(text, {
        onSuccess: () => {
          setDraft('');
          setSent(text);
        },
      });
  };

  const chosen = ask?.answer?.option;
  return (
    <section
      aria-label={`${KIND_LABEL[ask?.kind ?? 'question']}: ${title}`}
      data-answered={answered !== undefined}
      className={cx(
        'rounded-lg border bg-card px-3 py-2.5',
        answerable ? 'border-warn' : 'border-line',
        props.className,
      )}
    >
      <div className="mb-1 flex items-center gap-2 text-xs text-muted">
        <span className={cx('font-medium', answerable ? 'text-warn' : 'text-ink-2')}>
          {KIND_LABEL[ask?.kind ?? 'question']}
        </span>
        {from !== undefined && <span>from {from}</span>}
      </div>
      <p className="text-sm font-medium whitespace-pre-wrap">{title}</p>
      {body !== '' && <p className="mt-1 text-sm whitespace-pre-wrap text-ink-2">{body}</p>}

      {options.length > 0 && (
        <div className="mt-2 flex flex-wrap gap-1.5" role="group" aria-label="Options">
          {options.map((option, i) => (
            <Button
              key={i}
              variant={chosen === i || (answered !== undefined && answered === option) ? 'primary' : 'secondary'}
              disabled={!answerable || busy}
              onClick={() => choose(i)}
            >
              {option}
            </Button>
          ))}
        </div>
      )}

      {/* A CLI's option picker takes keys, not typed text, so free text is for asks and open questions. */}
      {answerable && (ask !== undefined || options.length === 0) && (
        <form onSubmit={submitText} className="mt-2 flex gap-1.5">
          <label htmlFor={inputId} className="sr-only">
            {options.length > 0 ? 'Or answer in your own words' : 'Your answer'}
          </label>
          <input
            id={inputId}
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            placeholder={options.length > 0 ? 'Or answer in your own words…' : 'Your answer…'}
            className="h-7 min-w-0 flex-1 rounded-sm border border-line-2 bg-bg px-2 text-sm"
          />
          <Button type="submit" disabled={busy || draft.trim() === ''}>
            {ask !== undefined ? 'Answer' : 'Send'}
          </Button>
        </form>
      )}

      {answered !== undefined && (
        <p className="mt-2 text-xs text-ok" data-testid="answer">
          {ask !== undefined || props.answer !== undefined ? `Answered: ${answered}` : `Sent: ${answered}`}
        </p>
      )}
      {answered === undefined && !answerable && ask === undefined && props.open === true && !sessionLive && (
        <p className="mt-2 text-xs text-muted">The session cannot take input, so this cannot be answered here.</p>
      )}
      {error !== null && (
        <p role="alert" className="mt-2 text-xs text-risk">
          {errorText(error)}
        </p>
      )}
    </section>
  );
}
