// An ask to answer in place: its question, context, receipts, the offered options and a free reply.
//
// A local stand-in: stream M builds the console's `QuestionCard` in src/console on another branch.
// Once it merges, the Inbox should use that one (see README.md).

import { useId, useState, type FormEvent } from 'react';
import { Button, StatusPill } from '../design/index.ts';
import type { Ask } from '../data/index.ts';
import { useAnswerAsk, useMemberMap, useNames } from './data.ts';
import { ASK_KIND, formatWhen } from './format.ts';
import { useProjectsNav } from './nav.tsx';
import { MemberChip } from './people.tsx';
import { Receipts } from './receipts.tsx';
import { ErrorNote, MaybeLink, inputClass } from './ui.tsx';

export function QuestionCard({
  ask,
  headingLevel = 3,
  onAnswered,
}: {
  ask: Ask;
  headingLevel?: 3 | 4;
  /** Called once the hub accepts the answer, with the answer as text. */
  onAnswered?: (ask: Ask, answer: string) => void;
}) {
  const titleId = useId();
  const answer = useAnswerAsk();
  const members = useMemberMap();
  const names = useNames();
  const nav = useProjectsNav();
  const [reply, setReply] = useState('');
  const Heading = headingLevel === 3 ? 'h3' : 'h4';
  const from = members.get(ask.from);
  const owner = from?.owner === undefined ? undefined : members.get(from.owner);
  const openTask = nav.openTask;
  const task = ask.task;
  const answered = answer.isSuccess || ask.state !== 'open';

  const send = (body: { option: number } | { text: string }, said: string) =>
    answer.mutate(
      { ask: ask.id, ...body },
      {
        onSuccess: () => {
          setReply('');
          onAnswered?.(ask, said);
        },
      },
    );

  const submit = (e: FormEvent) => {
    e.preventDefault();
    const text = reply.trim();
    if (text !== '') send({ text }, text);
  };

  return (
    <article
      aria-labelledby={titleId}
      data-ask={ask.id}
      className="flex flex-col gap-2 rounded-md border border-line bg-card p-3"
    >
      <div className="flex flex-wrap items-center gap-2 text-xs text-ink-2">
        <StatusPill tone={ask.kind === 'decision' || ask.kind === 'approval' ? 'warn' : 'accent'}>
          {ASK_KIND[ask.kind].one}
        </StatusPill>
        {from !== undefined && <MemberChip member={from} owner={owner} />}
        {task !== undefined && (
          <MaybeLink onOpen={openTask === undefined ? undefined : () => openTask(task)} className="font-mono">
            {names.task(task)}
          </MaybeLink>
        )}
        <time dateTime={new Date(ask.created).toISOString()} className="ml-auto">
          {formatWhen(ask.created)}
        </time>
      </div>
      <Heading id={titleId} className="text-md font-semibold">
        {ask.title}
      </Heading>
      {ask.body !== '' && <p className="text-sm text-ink-2">{ask.body}</p>}
      <Receipts receipts={ask.receipts} names={names} />
      {answered ? (
        <p role="status" className="text-sm font-medium text-ok">
          Answered
        </p>
      ) : (
        <div className="flex flex-col gap-2">
          {ask.options.length > 0 && (
            <div role="group" aria-label="Options" className="flex flex-wrap gap-2">
              {ask.options.map((option, index) => (
                <Button
                  key={`${index}:${option}`}
                  variant={index === 0 ? 'primary' : 'secondary'}
                  disabled={answer.isPending}
                  onClick={() => send({ option: index }, option)}
                >
                  {option}
                </Button>
              ))}
            </div>
          )}
          <form onSubmit={submit} className="flex gap-2">
            <input
              aria-label={`Reply to “${ask.title}”`}
              placeholder={ask.options.length > 0 ? 'Or answer in your own words' : 'Your answer'}
              value={reply}
              onChange={(e) => setReply(e.target.value)}
              className={inputClass}
            />
            <Button type="submit" disabled={answer.isPending || reply.trim() === ''}>
              Send
            </Button>
          </form>
        </div>
      )}
      {answer.error !== null && <ErrorNote error={answer.error} what="answer" />}
    </article>
  );
}
