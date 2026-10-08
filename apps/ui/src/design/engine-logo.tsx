import type { ReactNode } from 'react';
import type { Engine } from '../data/index.ts';
import { cx } from '../lib/cx.ts';

export const ENGINE_NAMES: Record<Engine, string> = { claude: 'Claude Code', codex: 'Codex', opencode: 'OpenCode' };

/**
 * A neutral shape per engine, to tell them apart at a glance: not the vendors' marks, whose
 * trademark terms restrict their use, in a public repository. Drawn inline in the surrounding
 * text colour, like the other icons, so it needs no image, mask or style in either theme.
 */
const GLYPHS: Record<Engine, ReactNode> = {
  claude: (
    <>
      <circle cx="8" cy="8" r="5.5" />
      <circle cx="8" cy="8" r="1.75" />
    </>
  ),
  codex: <path d="M8 2.25 13 5.1v5.8L8 13.75 3 10.9V5.1z" />,
  opencode: <path d="M8 2.75 13.5 12.75h-11z" />,
};

/** Decorative: the engine's name is in text beside it, or in the accessible name around it. */
export function EngineLogo({ engine, className }: { engine: Engine; className?: string }) {
  return (
    <svg
      viewBox="0 0 16 16"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.5}
      strokeLinejoin="round"
      aria-hidden
      focusable={false}
      data-engine={engine}
      className={cx('size-4 shrink-0', className)}
    >
      {GLYPHS[engine]}
    </svg>
  );
}
