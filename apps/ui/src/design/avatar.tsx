import { cx } from '../lib/cx.ts';

export interface AvatarMember {
  kind: 'human' | 'agent';
  name: string;
}

type Size = 'sm' | 'md' | 'lg';

const SIZES: Record<Size, { box: string; owner: string }> = {
  sm: { box: 'size-5 text-[9px]', owner: 'size-2.5 text-[6px]' },
  md: { box: 'size-6 text-[10px]', owner: 'size-3 text-[7px]' },
  lg: { box: 'size-8 text-xs', owner: 'size-3.5 text-[8px]' },
};

export function initials(name: string): string {
  const words = name.trim().split(/\s+/).filter(Boolean);
  const letters = words.length > 1 ? [words[0], words[1]] : [words[0]];
  return letters.map((w) => (w === undefined ? '' : [...w][0]?.toUpperCase() ?? '')).join('') || '?';
}

/** The accessible name: people by name; agents say so, and whose they are. */
export function avatarLabel(member: AvatarMember, owner?: AvatarMember): string {
  if (member.kind === 'human') return member.name;
  return owner === undefined ? `${member.name} (agent)` : `${member.name} (agent of ${owner.name})`;
}

/**
 * People are round; agents are square and carry their owner's initials in the corner, so an
 * agent is never mistaken for a person.
 */
export function Avatar({
  member,
  owner,
  size = 'md',
  className,
}: {
  member: AvatarMember;
  /** The agent's owner. Ignored for people. */
  owner?: AvatarMember | undefined;
  size?: Size;
  className?: string;
}) {
  const label = avatarLabel(member, owner);
  const agent = member.kind === 'agent';
  return (
    <span
      role="img"
      aria-label={label}
      title={label}
      className={cx('relative inline-flex shrink-0', className)}
    >
      <span
        aria-hidden
        className={cx(
          'inline-flex items-center justify-center font-semibold select-none',
          SIZES[size].box,
          agent
            ? 'rounded-sm border border-line bg-accent-soft text-accent-text'
            : 'rounded-pill bg-ink text-bg',
        )}
      >
        {initials(member.name)}
      </span>
      {agent && owner !== undefined && (
        <span
          aria-hidden
          className={cx(
            'absolute -right-1 -bottom-1 inline-flex items-center justify-center rounded-pill bg-ink font-semibold text-bg ring-2 ring-card',
            SIZES[size].owner,
          )}
        >
          {initials(owner.name).slice(0, 1)}
        </span>
      )}
    </span>
  );
}
