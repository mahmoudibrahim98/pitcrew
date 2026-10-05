import type { Engine } from '../data/index.ts';
import { cx } from '../lib/cx.ts';
import claude from './engines/claude.svg';
import codex from './engines/codex.svg';
import opencode from './engines/opencode.svg';

export const ENGINE_NAMES: Record<Engine, string> = { claude: 'Claude Code', codex: 'Codex', opencode: 'OpenCode' };
const LOGOS = { claude, codex, opencode };

/** Vendor marks use the surrounding text colour in both themes. */
export function EngineLogo({ engine, className }: { engine: Engine; className?: string }) {
  return <span aria-hidden className={cx('inline-block size-4 shrink-0 bg-current', className)} style={{ maskImage: `url(${LOGOS[engine]})`, maskSize: 'contain', maskRepeat: 'no-repeat', maskPosition: 'center' }} />;
}
