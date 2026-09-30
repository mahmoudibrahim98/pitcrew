// Members: people are round, agents square with their owner named. Shared by every projects view.

import type { Member } from '../data/index.ts';
import { cx } from '../lib/cx.ts';
import { initials } from './format.ts';

export function memberLabel(member: Member, owner?: Member): string {
  if (member.kind === 'human') return member.name;
  return owner === undefined ? `${member.name}, agent` : `${member.name}, agent of ${owner.name}`;
}

export function Avatar({
  member,
  owner,
  size = 'sm',
  decorative = false,
  className,
}: {
  member: Member;
  owner?: Member | undefined;
  size?: 'sm' | 'md';
  /** Hidden from screen readers, for when the handle is written next to it. */
  decorative?: boolean;
  className?: string;
}) {
  const agent = member.kind === 'agent';
  const label = memberLabel(member, owner);
  return (
    <span
      {...(decorative ? { 'aria-hidden': true } : { role: 'img', 'aria-label': label })}
      title={`${label} (${member.handle})`}
      data-kind={member.kind}
      className={cx(
        'inline-flex shrink-0 items-center justify-center font-semibold select-none',
        size === 'sm' ? 'size-5 text-xs' : 'size-7 text-sm',
        agent ? 'rounded-sm bg-accent-soft text-accent-text' : 'rounded-pill bg-sunken text-ink-2',
        className,
      )}
    >
      {initials(member.name)}
    </span>
  );
}

/** Avatar and handle, for lists. */
export function MemberChip({ member, owner }: { member: Member; owner?: Member | undefined }) {
  return (
    <span className="inline-flex items-center gap-1.5 text-sm text-ink-2" title={memberLabel(member, owner)}>
      <Avatar member={member} owner={owner} decorative />
      <span>{member.handle}</span>
      {member.kind === 'agent' && <span className="sr-only"> ({memberLabel(member, owner)})</span>}
    </span>
  );
}
