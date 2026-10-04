// An approval in the Inbox: exactly what will be sent upstream, field by field (what upstream has
// now → what PitCrew sends), and Send / Don't send. Answering is answering the ask; the hub sends
// only after "Send", and only what is shown here.

import { useId } from 'react';
import { Button, StatusPill } from '../../design/index.ts';
import type { Ask, UpstreamWrite } from '../../data/index.ts';
import { useAnswerAsk, useNames } from '../data.ts';
import { WRITE_OPERATION, WRITE_STATE, isWebUrl, tracker } from '../format.ts';
import { ErrorNote } from '../ui.tsx';
import { DONT_SEND, SEND, fieldRows } from './api.ts';

/** The diff of one write: a row per field it sends. */
export function WriteDiff({ write }: { write: UpstreamWrite }) {
  const rows = fieldRows(write);
  const changes = write.proposal.operation === 'update' || write.proposal.operation === 'close' || write.proposal.operation === 'reopen';
  return (
    <table className="w-full table-fixed border-collapse text-sm">
      <thead className="sr-only">
        <tr>
          <th scope="col">Field</th>
          {changes && <th scope="col">Upstream now</th>}
          <th scope="col">Sent</th>
        </tr>
      </thead>
      <tbody>
        {rows.map((row) => (
          <tr key={row.field} className="align-top" data-field={row.field}>
            <th scope="row" className="w-24 py-0.5 pr-2 text-left font-medium text-ink-2">
              {row.field}
            </th>
            {changes && (
              <td className="py-0.5 pr-2 break-words whitespace-pre-wrap text-ink-2 line-through decoration-ink-2/50">
                {row.before ?? <span className="no-underline">(not read yet)</span>}
              </td>
            )}
            <td className="py-0.5 break-words whitespace-pre-wrap">{row.after}</td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

/** One line naming the write: "Close example-org/demo-repo#1 on GitHub". */
export function writeHeadline(write: UpstreamWrite): string {
  const { operation, target, scope, system } = write.proposal;
  const verb = WRITE_OPERATION[operation];
  return `${verb.charAt(0).toUpperCase()}${verb.slice(1)} ${target?.key ?? scope} on ${tracker(system)}`;
}

export function ApprovalCard({ ask, write }: { ask: Ask; write: UpstreamWrite }) {
  const headingId = useId();
  const names = useNames();
  const answer = useAnswerAsk();
  const open = ask.state === 'open' && write.state === 'pending' && answer.data === undefined;
  const target = write.proposal.target;
  const choose = (option: number) => answer.mutate({ ask: ask.id, option });
  return (
    <section
      aria-labelledby={headingId}
      data-write={write.proposal.ask}
      className="flex flex-col gap-2 rounded-lg border border-line bg-card p-3"
    >
      <header className="flex flex-wrap items-center gap-2">
        <h3 id={headingId} className="text-md font-semibold">
          {writeHeadline(write)}
        </h3>
        <StatusPill tone={WRITE_STATE[write.state].tone}>{WRITE_STATE[write.state].label}</StatusPill>
      </header>
      <p className="text-sm text-ink-2">
        Nothing is sent to {tracker(write.proposal.system)} unless you choose Send.{' '}
        {write.proposal.cause === undefined ? 'Asked for' : 'Implied by a change'} by{' '}
        {names.member(write.proposal.requested_by)}
        {write.proposal.task === undefined ? '' : ` on ${names.task(write.proposal.task)}`}.
        {target?.url !== undefined && isWebUrl(target.url) && (
          <>
            {' '}
            <a href={target.url} target="_blank" rel="noreferrer noopener" className="underline">
              Open {target.key}
            </a>
          </>
        )}
      </p>
      <WriteDiff write={write} />
      {open ? (
        <div className="flex gap-2">
          <Button variant="primary" disabled={answer.isPending} onClick={() => choose(SEND)}>
            {ask.options[SEND] ?? 'Send'}
          </Button>
          <Button disabled={answer.isPending} onClick={() => choose(DONT_SEND)}>
            {ask.options[DONT_SEND] ?? 'Don’t send'}
          </Button>
        </div>
      ) : (
        <p role="status" className="text-sm text-ink-2">
          {answer.data !== undefined && answer.data.answer?.option === SEND ? 'Approved: sending.' : WRITE_STATE[write.state].label}
        </p>
      )}
      {answer.error !== null && <ErrorNote error={answer.error} what="answer" />}
    </section>
  );
}
