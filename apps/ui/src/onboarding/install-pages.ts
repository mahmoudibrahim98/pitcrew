// Where each tool the machine check looks at is installed from: the pages a row's "Install…" fix
// opens (`MachineCheckRow.fix === 'install-page'`). This table is the only place such a URL comes
// from: the hub and a remote machine say only *that* a row has an install page, never *which*, so
// neither can make the app open a page of its choosing.

import type { CheckRowId } from './api.ts';

export const INSTALL_PAGES: Readonly<Partial<Record<CheckRowId, string>>> = {
  'cli-claude': 'https://docs.anthropic.com/en/docs/claude-code/setup',
  'cli-codex': 'https://github.com/openai/codex#installing-and-running-codex-cli',
  'cli-opencode': 'https://opencode.ai/docs/',
  tmux: 'https://github.com/tmux/tmux/wiki/Installing',
  git: 'https://git-scm.com/downloads',
  gh: 'https://cli.github.com/',
};

/** The install page for `row`, if it has one. */
export function installPage(row: CheckRowId): string | undefined {
  return INSTALL_PAGES[row];
}
