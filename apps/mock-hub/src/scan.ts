// `POST /v1/machines/{id}/scan` (api-v1.md, "Machine scan"): what the daemon would answer for a
// person whose agent homes hold the fixtures' sessions and a few more, all synthetic. The report is
// fixed: the mock reads no folder. Its frames are written over `delays.scan` milliseconds: a
// progress frame at once, a few more, the last with `scanned == total`, then `done`.
//
// The rules are the daemon's: only the hub's own machine (its first `local` one; another is 409),
// and one scan at a time per machine, held from the request until its report is written, even if
// the client went away meanwhile (409 for a second one).

import type { Hub } from './state.ts';
import type { MachineId, ScanFrame, ScanReport } from './types.ts';
import { conflict, notFound } from './validate.ts';

/** Where a streamed answer's lines go: `server.ts` writes them to the response. */
export interface LineSink {
  /** Writes one line (it ends with `\n`); dropped once the client has gone. */
  write(line: string): void;
  end(): void;
}

/** A route's answer written over time, as newline-delimited JSON. */
export interface StreamedBody {
  contentType: string;
  start(sink: LineSink): void;
}

/** Progress frames after the first, before `done`. */
const TICKS = 4;

/** The machines with a scan running, per hub. */
const running = new WeakMap<Hub, Set<MachineId>>();

function runningOn(hub: Hub): Set<MachineId> {
  let machines = running.get(hub);
  if (machines === undefined) {
    machines = new Set();
    running.set(hub, machines);
  }
  return machines;
}

/** Checks the request and claims the machine; the answer, written once `server.ts` starts it. */
export function startScan(hub: Hub, id: string): StreamedBody {
  const machine = hub.findMachine(id);
  if (machine === undefined) {
    throw notFound(`No machine ${id}.`);
  }
  const own = hub.machines.find((m) => m.kind === 'local');
  if (own?.id !== machine.id) {
    throw conflict(
      `Scanning ${machine.name} is not supported yet: this hub scans only its own machine${own === undefined ? '' : `, ${own.name}`}.`,
    );
  }
  const machines = runningOn(hub);
  if (machines.has(machine.id)) {
    throw conflict(`A scan of ${machine.name} is already running; wait for it to finish.`);
  }
  machines.add(machine.id);
  return {
    contentType: 'application/x-ndjson',
    start: (sink) => {
      const report = scanReport();
      const total = report.counts.sessions + report.counts.subagent_sessions;
      const send = (frame: ScanFrame): void => sink.write(`${JSON.stringify(frame)}\n`);
      send({ type: 'progress', scanned: 0 });
      const step = hub.delays.scan / (TICKS + 1);
      for (let i = 1; i <= TICKS; i += 1) {
        const scanned = Math.round((total * i) / TICKS);
        const path = PATHS[(i - 1) % PATHS.length];
        hub.later(step * i, () => send({ type: 'progress', scanned, total, ...(path === undefined ? {} : { path }) }));
      }
      hub.later(hub.delays.scan, () => {
        send({ type: 'done', report });
        machines.delete(machine.id);
        sink.end();
      });
    },
  };
}

/** Transcripts the progress frames name, as the daemon's would. */
const PATHS = [
  '/home/sam/.claude/projects/-home-sam-work-diffusion-paper-paper/2b6f1a8e-4c1d-4f5e-9a37-0c8d1e2f3a4b.jsonl',
  '/home/sam/.claude/projects/-home-sam-work-lab-tools/5e1f0c2a-7b3d-4e8f-9a1b-2c3d4e5f6a7b.jsonl',
  '/home/sam/.codex/sessions/2026/09/29/rollout-2026-09-29T08-00-00-7c1e9d2a-0b3f-4e6a-8d5c-1f2e3a4b5c6d.jsonl',
  '/home/sam/.local/share/opencode/opencode.db',
];

/**
 * The synthetic machine: 14 sessions and 2 sub-agent sessions in three agent homes. Its folders
 * are the fixtures' (`crates/fixtures/data/transcripts`: the paper, the runs on scratch, the lab's
 * tools) and two more of the paper's; `diffusion-paper` and `lab-tools` are git repositories, the
 * runs folder is not. A fresh object each time, so a client cannot change the next scan's.
 */
export function scanReport(): ScanReport {
  return {
    counts: {
      sessions: 14,
      subagent_sessions: 2,
      by_engine: [
        { engine: 'claude', count: 9 },
        { engine: 'codex', count: 3 },
        { engine: 'opencode', count: 2 },
      ],
      by_home: [
        { engine: 'claude', home: '/home/sam/.claude', count: 9 },
        { engine: 'codex', home: '/home/sam/.codex', count: 3 },
        { engine: 'opencode', home: '/home/sam/.local/share/opencode', count: 2 },
      ],
      by_folder: [
        { path: '/home/sam/work/diffusion-paper/paper', count: 4 },
        { path: '/home/sam/work/lab-tools', count: 4 },
        { path: '/scratch/sam/diffusion-runs', count: 3 },
        { path: '/home/sam/work/diffusion-paper/experiments', count: 2 },
        { path: '/home/sam/work/diffusion-paper', count: 1 },
      ],
      by_month: [
        { month: '2026-09', count: 8 },
        { month: '2026-08', count: 4 },
        { month: '2026-07', count: 2 },
      ],
      // 2026-07-06T09:00:00Z and 2026-09-30T10:10:00Z.
      first_activity: 1783328400000,
      last_activity: 1790763000000,
    },
    suggestions: [
      {
        id: '/home/sam/work/diffusion-paper',
        name: 'diffusion-paper',
        path: '/home/sam/work/diffusion-paper',
        is_git: true,
        session_count: 7,
        recent_30d: 5,
        recent_90d: 7,
        workstreams: [
          {
            id: '/home/sam/work/diffusion-paper',
            kind: 'main',
            name: 'main',
            session_count: 7,
            recent_30d: 5,
            recent_90d: 7,
          },
          {
            id: '/home/sam/work/diffusion-paper/paper',
            kind: 'folder',
            name: 'paper',
            session_count: 4,
            recent_30d: 3,
            recent_90d: 4,
          },
          {
            id: '/home/sam/work/diffusion-paper#revision-2',
            kind: 'branch',
            name: 'revision-2',
            branch: 'revision-2',
            session_count: 2,
            recent_30d: 2,
            recent_90d: 2,
          },
          {
            id: '/home/sam/work/diffusion-paper/experiments',
            kind: 'folder',
            name: 'experiments',
            session_count: 2,
            recent_30d: 0,
            recent_90d: 2,
          },
        ],
      },
      {
        id: '/home/sam/work/lab-tools',
        name: 'lab-tools',
        path: '/home/sam/work/lab-tools',
        is_git: true,
        session_count: 4,
        recent_30d: 3,
        recent_90d: 4,
        workstreams: [
          {
            id: '/home/sam/work/lab-tools',
            kind: 'main',
            name: 'main',
            session_count: 4,
            recent_30d: 3,
            recent_90d: 4,
          },
          {
            id: '/home/sam/work/lab-tools#parsers',
            kind: 'branch',
            name: 'parsers',
            branch: 'parsers',
            session_count: 2,
            recent_30d: 1,
            recent_90d: 2,
          },
        ],
      },
      {
        id: '/scratch/sam/diffusion-runs',
        name: 'diffusion-runs',
        path: '/scratch/sam/diffusion-runs',
        is_git: false,
        session_count: 3,
        recent_30d: 0,
        recent_90d: 3,
        workstreams: [
          {
            id: '/scratch/sam/diffusion-runs',
            kind: 'main',
            name: 'Main',
            session_count: 3,
            recent_30d: 0,
            recent_90d: 3,
          },
        ],
      },
    ],
    unreadable: 0,
  };
}
