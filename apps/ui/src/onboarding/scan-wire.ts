// The machine scan on the wire (api-v1.md, "Machine scan"; `pitcrew_protocol::scan`), and its
// mapping to the wizard's own types (`ScanResult`, camelCase, `byEngine` as a record). The answer
// to `POST /v1/machines/{id}/scan` is newline-delimited JSON: `progress` frames, then one `done`
// (or `error`) frame.

import type { Engine, ErrorCode } from '../data/index.ts';
import type { ScanResult } from './api.ts';

export interface WireScanProgress {
  scanned: number;
  total?: number;
  path?: string;
}

export interface WireWorkstreamSuggestion {
  /** A sub-folder's own path (also where it is), or `<project path>#<branch>`. */
  id: string;
  name: string;
  branch?: string;
  session_count: number;
  recent_30d: number;
  recent_90d: number;
}

export interface WireSuggestion {
  /** The project's root path. */
  id: string;
  name: string;
  path: string;
  is_git: boolean;
  session_count: number;
  recent_30d: number;
  recent_90d: number;
  workstreams: WireWorkstreamSuggestion[];
}

export interface WireScanReport {
  counts: {
    sessions: number;
    subagent_sessions: number;
    by_engine: { engine: Engine; count: number }[];
    by_home: { engine: Engine; home: string; count: number }[];
    by_folder: { path: string; count: number }[];
    by_month: { month: string; count: number }[];
    first_activity?: number;
    last_activity?: number;
  };
  suggestions: WireSuggestion[];
  unreadable: number;
}

export type WireScanFrame =
  | ({ type: 'progress' } & WireScanProgress)
  | { type: 'done'; report: WireScanReport }
  | { type: 'error'; code: ErrorCode; message: string };

/** The answer is not what the contract says: never acted on. */
export class MalformedScan extends Error {
  constructor(detail: string) {
    super(`The hub's scan answer was malformed (${detail}).`);
    this.name = 'MalformedScan';
  }
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

const isCount = (value: unknown): value is number => Number.isSafeInteger(value) && (value as number) >= 0;

/** Checks one frame's shape as far as the wizard relies on it. */
function frameOf(value: unknown): WireScanFrame {
  if (!isRecord(value)) throw new MalformedScan('a frame is not an object');
  switch (value['type']) {
    case 'progress':
      if (!isCount(value['scanned'])) throw new MalformedScan('progress without a count');
      if (value['total'] !== undefined && !isCount(value['total'])) throw new MalformedScan('a bad total');
      return value as WireScanFrame;
    case 'done': {
      const report = value['report'];
      if (!isRecord(report) || !isRecord(report['counts']) || !Array.isArray(report['suggestions'])) {
        throw new MalformedScan('a report without its counts or suggestions');
      }
      return value as WireScanFrame;
    }
    case 'error':
      if (typeof value['message'] !== 'string') throw new MalformedScan('an error without a message');
      return value as WireScanFrame;
    default:
      throw new MalformedScan(`an unknown frame "${String(value['type'])}"`);
  }
}

/** The frames in a (whole or partial) answer: one JSON object per line; blank lines are skipped. */
export function parseScanFrames(body: string): WireScanFrame[] {
  return body
    .split('\n')
    .filter((line) => line.trim() !== '')
    .map((line) => {
      let value: unknown;
      try {
        value = JSON.parse(line);
      } catch {
        throw new MalformedScan('a line is not JSON');
      }
      return frameOf(value);
    });
}

/** The report as the wizard shows it: engines as a record, camelCase, the suggestions in order. */
export function toScanResult(report: WireScanReport): ScanResult {
  const byEngine: Partial<Record<Engine, number>> = {};
  for (const { engine, count } of report.counts.by_engine) byEngine[engine] = count;
  return {
    counts: {
      byEngine,
      byFolder: report.counts.by_folder.map(({ path, count }) => ({ path, count })),
      byMonth: report.counts.by_month.map(({ month, count }) => ({ month, count })),
    },
    suggestedProjects: report.suggestions.map((s) => ({
      id: s.id,
      name: s.name,
      path: s.path,
      workstreams: s.workstreams.map((w) => ({
        id: w.id,
        name: w.name,
        ...(w.branch === undefined ? {} : { branch: w.branch }),
        sessionCount: w.session_count,
      })),
    })),
  };
}
