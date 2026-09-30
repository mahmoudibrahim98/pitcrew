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
  className,
}: {
  member: Member;
  owner?: Member | undefined;
  size?: 'sm' | 'md';
  className?: string;
}) {
  const agent = member.kind === 'agent';
  const label = memberLabel(member, owner);
  return (
    <span
      role="img"
      aria-label={label}
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
    <span className="inline-flex items-center gap-1.5 text-sm text-ink-2">
      <Avatar member={member} owner={owner} />
      <span>{member.handle}</span>
    </span>
  );
}
