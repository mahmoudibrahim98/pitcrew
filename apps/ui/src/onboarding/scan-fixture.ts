// A synthetic scan report on the wire, for tests (`scan-wire.test.ts`, `hub-api.test.tsx`). Not
// imported by the app. Every path, name and count is made up.

import type { WireScanReport } from './scan-wire.ts';

export const SCAN_REPORT: WireScanReport = {
  counts: {
    sessions: 5,
    subagent_sessions: 1,
    by_engine: [
      { engine: 'claude', count: 3 },
      { engine: 'opencode', count: 2 },
    ],
    by_home: [
      { engine: 'claude', home: '/home/sam/.claude', count: 3 },
      { engine: 'opencode', home: '/home/sam/.local/share/opencode', count: 2 },
    ],
    by_folder: [
      { path: '/home/sam/work/paper/drafts', count: 3 },
      { path: '/home/sam/work/tools', count: 2 },
    ],
    by_month: [
      { month: '2026-09', count: 4 },
      { month: '2026-08', count: 1 },
    ],
    first_activity: 1_785_000_000_000,
    last_activity: 1_790_000_000_000,
  },
  suggestions: [
    {
      id: '/home/sam/work/paper',
      name: 'paper',
      path: '/home/sam/work/paper',
      is_git: true,
      session_count: 3,
      recent_30d: 3,
      recent_90d: 3,
      workstreams: [
        { id: '/home/sam/work/paper/drafts', name: 'drafts', session_count: 3, recent_30d: 3, recent_90d: 3 },
        {
          id: '/home/sam/work/paper#revision-2',
          name: 'revision-2',
          branch: 'revision-2',
          session_count: 1,
          recent_30d: 1,
          recent_90d: 1,
        },
      ],
    },
    {
      id: '/home/sam/work/tools',
      name: 'tools',
      path: '/home/sam/work/tools',
      is_git: false,
      session_count: 2,
      recent_30d: 0,
      recent_90d: 2,
      workstreams: [],
    },
  ],
  unreadable: 0,
};
