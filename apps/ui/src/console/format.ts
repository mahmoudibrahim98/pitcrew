// Labels, tones and times the console's components share.

import { useSyncExternalStore } from 'react';
import type { Engine, Liveness, Machine, Session, SessionState } from '../data/index.ts';
import type { Tone } from '../design/index.ts';

export const ENGINE_LABEL: Record<Engine, string> = {
  claude: 'Claude',
  codex: 'Codex',
  opencode: 'OpenCode',
};

export const STATE: Record<SessionState, { label: string; tone: Tone }> = {
  starting: { label: 'Starting', tone: 'accent' },
  working: { label: 'Working', tone: 'progress' },
  waiting: { label: 'Waiting', tone: 'warn' },
  idle: { label: 'Idle', tone: 'neutral' },
  ended: { label: 'Ended', tone: 'neutral' },
  unreachable: { label: 'Unreachable', tone: 'risk' },
};

export const LIVENESS: Record<Liveness, { label: string; tone: Tone }> = {
  live: { label: 'Live', tone: 'ok' },
  unverifiable: { label: 'Unreachable', tone: 'risk' },
  stopped: { label: 'Stopped', tone: 'neutral' },
};

export const SESSION_STATES: readonly SessionState[] = ['starting', 'working', 'waiting', 'idle', 'ended', 'unreachable'];
export const ENGINES: readonly Engine[] = ['claude', 'codex', 'opencode'];

/** A session's name: its title, else the last folder of its working directory. */
export function sessionTitle(session: Pick<Session, 'title' | 'cwd'>): string {
  if (session.title !== undefined && session.title.trim() !== '') return session.title;
  const folder = session.cwd.replace(/[\\/]+$/, '').split(/[\\/]/).at(-1);
  return folder === undefined || folder === '' ? session.cwd : folder;
}

/**
 * Why the session cannot take input, or undefined if it can: it has ended, or it or its machine
 * cannot be reached (the API answers 503).
 */
export function inputBlocked(session: Session | undefined, machine: Machine | undefined): string | undefined {
  if (session === undefined) return 'Loading the session…';
  if (session.state === 'ended') return 'This session has ended.';
  const name = machine?.name ?? 'Its machine';
  if (session.state === 'unreachable' || (machine !== undefined && machine.liveness !== 'live')) {
    return `${name} cannot be reached right now, so the session cannot take input.`;
  }
  return undefined;
}

const MINUTE = 60_000;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;

/** "now", "5m", "3h", "2d", then the date. */
export function relativeTime(at: number, now: number): string {
  const ago = Math.max(0, now - at);
  if (ago < MINUTE) return 'now';
  if (ago < HOUR) return `${Math.floor(ago / MINUTE)}m`;
  if (ago < DAY) return `${Math.floor(ago / HOUR)}h`;
  if (ago < 7 * DAY) return `${Math.floor(ago / DAY)}d`;
  return new Date(at).toLocaleDateString(undefined, { month: 'short', day: 'numeric' });
}

export function clockTime(at: number): string {
  return new Date(at).toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit' });
}

export function fullTime(at: number): string {
  return new Date(at).toLocaleString();
}

// One shared clock for relative times, ticking while anything shows one.
let now = Date.now();
const ticking = new Set<() => void>();
let timer: ReturnType<typeof setInterval> | undefined;

function subscribeClock(listener: () => void): () => void {
  ticking.add(listener);
  if (timer === undefined) {
    now = Date.now();
    timer = setInterval(() => {
      now = Date.now();
      for (const tick of ticking) tick();
    }, 30_000);
  }
  return () => {
    ticking.delete(listener);
    if (ticking.size === 0 && timer !== undefined) {
      clearInterval(timer);
      timer = undefined;
    }
  };
}

/** The current time, updated every 30 seconds. */
export function useNow(): number {
  return useSyncExternalStore(subscribeClock, () => now);
}
