// The scan's wire shapes (api-v1.md, "Machine scan") and their mapping to the wizard's types.

import { describe, expect, it } from 'vitest';
import { SCAN_REPORT as REPORT } from './scan-fixture.ts';
import { MalformedScan, parseScanFrames, toScanResult, type WireScanReport } from './scan-wire.ts';

describe('parseScanFrames', () => {
  it('reads one frame per line, skipping blank lines', () => {
    const body = [
      '{"type":"progress","scanned":0}',
      '{"type":"progress","scanned":5,"total":6,"path":"/home/sam/.claude/projects/-w/s1.jsonl"}',
      '',
      JSON.stringify({ type: 'done', report: REPORT }),
      '',
    ].join('\n');
    expect(parseScanFrames(body)).toEqual([
      { type: 'progress', scanned: 0 },
      { type: 'progress', scanned: 5, total: 6, path: '/home/sam/.claude/projects/-w/s1.jsonl' },
      { type: 'done', report: REPORT },
    ]);
    expect(parseScanFrames('{"type":"error","code":"internal","message":"The scan failed."}\n')).toEqual([
      { type: 'error', code: 'internal', message: 'The scan failed.' },
    ]);
  });

  it.each([
    ['not JSON', 'nope\n'],
    ['an array', '[1]\n'],
    ['an unknown frame', '{"type":"later"}\n'],
    ['progress without a count', '{"type":"progress"}\n'],
    ['a negative count', '{"type":"progress","scanned":-1}\n'],
    ['a report without suggestions', '{"type":"done","report":{"counts":{}}}\n'],
    ['an error without a message', '{"type":"error","code":"internal"}\n'],
  ])('refuses %s', (_what, body) => {
    expect(() => parseScanFrames(body)).toThrow(MalformedScan);
  });
});

describe('toScanResult', () => {
  it('maps the wire report to the wizard’s camelCase result, in order', () => {
    expect(toScanResult(REPORT)).toEqual({
      counts: {
        byEngine: { claude: 3, opencode: 2 },
        byFolder: [
          { path: '/home/sam/work/paper/drafts', count: 3 },
          { path: '/home/sam/work/tools', count: 2 },
        ],
        byMonth: [
          { month: '2026-09', count: 4 },
          { month: '2026-08', count: 1 },
        ],
      },
      suggestedProjects: [
        {
          id: '/home/sam/work/paper',
          name: 'paper',
          path: '/home/sam/work/paper',
          workstreams: [
            { id: '/home/sam/work/paper/drafts', name: 'drafts', sessionCount: 3 },
            { id: '/home/sam/work/paper#revision-2', name: 'revision-2', branch: 'revision-2', sessionCount: 1 },
          ],
        },
        { id: '/home/sam/work/tools', name: 'tools', path: '/home/sam/work/tools', workstreams: [] },
      ],
    });
  });

  it('maps an empty scan to empty lists', () => {
    const empty: WireScanReport = {
      counts: { sessions: 0, subagent_sessions: 0, by_engine: [], by_home: [], by_folder: [], by_month: [] },
      suggestions: [],
      unreadable: 0,
    };
    expect(toScanResult(empty)).toEqual({ counts: { byEngine: {}, byFolder: [], byMonth: [] }, suggestedProjects: [] });
  });
});
