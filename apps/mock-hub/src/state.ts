// In-memory state: the demo workspace, the event log and the session transcripts.
//
// The event log is the fixture's 15 events (revisions 1–15) followed by everything the mock
// appends. A revision is an event's 1-based position in the log. Appended events reach stream
// subscribers in batches, BATCH_MS after the first event of the batch.

import { readFileSync } from 'node:fs';
import { cannedTranscripts, type TranscriptRecord } from './transcripts.ts';
import type {
  Ask,
  Brief,
  DemoRecaps,
  DemoWorkspace,
  Dispatch,
  Event,
  EventBody,
  Machine,
  Member,
  MemberId,
  Persona,
  Project,
  Session,
  SessionId,
  Task,
  Team,
  Workspace,
  Workstream,
} from './types.ts';
import { ulid } from './ulid.ts';
import { isRecord } from './validate.ts';

/**
 * The mock's two tokens' bound member ids (`dev-device-token`, `dev-agent-token`; see
 * `routes.ts`'s `TOKENS`). Fixed so a fresh workspace can give the device token's identity before
 * any `Member` exists for it: "the dev device token acts as a member id nothing knows" until
 * `POST /v1/setup` adds the person.
 */
export const DEV_DEVICE_MEMBER: MemberId = '01JB000000000000000MEM0001';
export const DEV_AGENT_MEMBER: MemberId = '01JB000000000000000MEM0002';

/**
 * An empty workspace, as a fresh hub starts: no members, machines or work (projects, tasks,
 * sessions, asks, events). `PITCREW_MOCK_FRESH=1` serves this instead of the demo fixture, so
 * `POST /v1/setup` can be exercised.
 */
export function freshWorkspace(): DemoWorkspace {
  return {
    workspace: { id: ulid(), name: '' },
    machines: [],
    members: [],
    personas: [],
    teams: [],
    projects: [],
    workstreams: [],
    tasks: [],
    sessions: [],
    dispatches: [],
    asks: [],
    briefs: [],
    events: [],
  };
}

/** How long simulated sessions take, in milliseconds. */
export interface Delays {
  /** From `starting` to `working` after a dispatch or a start. */
  start: number;
  /** From `send` (or an answered ask) to the canned reply and `turn_ended`. */
  reply: number;
  /** From a graceful `end` to `session_ended`. */
  end: number;
  /** A machine scan, from its first progress frame to its report (`scan.ts`). */
  scan: number;
  /** A sign-in terminal, from its start to its login ending by itself (`machine-setup.ts`). */
  signIn: number;
}

export const DEFAULT_DELAYS: Delays = { start: 1500, reply: 800, end: 300, scan: 1000, signIn: 2000 };

/** How many revisions one filtered `GET /v1/events` request examines at most. */
export const DEFAULT_SCAN_WINDOW = 500;

/** Events appended within this window reach stream subscribers as one batch. */
const BATCH_MS = 60;

const LISTS = [
  'machines',
  'members',
  'personas',
  'teams',
  'projects',
  'workstreams',
  'tasks',
  'sessions',
  'dispatches',
  'asks',
  'briefs',
  'events',
] as const;

/** Reads the demo workspace and fills in the fields serde would default. */
export function loadFixture(path: URL): DemoWorkspace {
  const parsed: unknown = JSON.parse(readFileSync(path, 'utf8'));
  if (!isRecord(parsed) || !isRecord(parsed['workspace'])) {
    throw new Error(`${path.pathname}: expected an object with a workspace`);
  }
  for (const key of LISTS) {
    const list = parsed[key] ?? [];
    if (!Array.isArray(list)) {
      throw new Error(`${path.pathname}: ${key} must be an array`);
    }
    parsed[key] = list;
  }
  const data = parsed as unknown as DemoWorkspace;
  applySerdeDefaults(data);
  return data;
}

/**
 * The Rust server always writes `#[serde(default)]` fields, so the mock fills the ones the
 * fixture leaves out (for example `description` on tasks without one).
 */
function applySerdeDefaults(data: DemoWorkspace): void {
  for (const machine of data.machines) {
    if (machine.info) {
      machine.info.home_on_network_fs ??= false;
    }
  }
  for (const persona of data.personas) {
    persona.permission_mode ??= 'default';
  }
  for (const project of data.projects) {
    project.members ??= [];
    project.external ??= [];
  }
  for (const workstream of data.workstreams) {
    workstream.locations ??= [];
    workstream.external ??= [];
  }
  for (const task of data.tasks) {
    task.description ??= '';
    task.priority ??= 'none';
    task.labels ??= [];
    task.blocked_by ??= [];
    task.accept_auto ??= false;
    task.subtasks ??= [];
  }
  for (const ask of data.asks) {
    ask.body ??= '';
    ask.options ??= [];
    ask.receipts ??= [];
  }
  for (const brief of data.briefs) {
    brief.receipts ??= [];
  }
}

/** Accepts a bare id or the prefixed form people see (`tsk_01JB…`), as `FromStr` in ids.rs does. */
function bareId(ref: string, prefix: string): string {
  const raw = ref.startsWith(`${prefix}_`) ? ref.slice(prefix.length + 1) : ref;
  return raw.toUpperCase();
}

export class Hub {
  importChoice: import("./import.ts").ImportChoice = { filter: { mode: "all", engines: [], folders: [] }, committed_at: null };
  readonly cursors = new Map<MemberId, Map<string, number>>();
  readonly workspace: Workspace;
  readonly machines: Machine[];
  readonly members: Member[];
  readonly personas: Persona[];
  readonly teams: Team[];
  readonly projects: Project[];
  readonly workstreams: Workstream[];
  readonly tasks: Task[];
  readonly sessions: Session[];
  readonly dispatches: Dispatch[];
  readonly asks: Ask[];
  readonly briefs: Brief[];
  readonly transcripts: Map<SessionId, TranscriptRecord[]>;
  /** The demo's recaps, as the recap engine wrote them; nothing the mock does changes them. */
  readonly recaps: DemoRecaps;
  readonly delays: Delays;
  /** How many revisions one filtered activity request examines at most. */
  readonly scanWindow: number;
  /** The workspace's person, who authors what no agent did. */
  readonly person: MemberId;
  /** A counter per session; a delayed reply lands only if its turn is still the current one. */
  readonly turns = new Map<SessionId, number>();
  /** Identifies this event log. A new one per mock start, so clients notice the reset. */
  readonly logId: string = ulid();

  readonly #log: Event[];
  readonly #listeners = new Set<() => void>();
  readonly #timers = new Set<ReturnType<typeof setTimeout>>();
  #flushTimer: ReturnType<typeof setTimeout> | undefined;
  #lastAt: number;
  #disposed = false;

  constructor(
    data: DemoWorkspace,
    delays: Delays,
    scanWindow = DEFAULT_SCAN_WINDOW,
    recaps: DemoRecaps = { tz: 0, blocks: [], projects: [] },
  ) {
    this.workspace = data.workspace;
    this.machines = data.machines;
    this.members = data.members;
    this.personas = data.personas;
    this.teams = data.teams;
    this.projects = data.projects;
    this.workstreams = data.workstreams;
    this.tasks = data.tasks;
    this.sessions = data.sessions;
    this.dispatches = data.dispatches;
    this.asks = data.asks;
    this.briefs = data.briefs;
    this.transcripts = cannedTranscripts();
    this.recaps = recaps;
    this.delays = delays;
    this.scanWindow = scanWindow;
    // Before setup a fresh workspace has no person yet; the device token's own id is who it will
    // be (api-v1.md, "The first run").
    this.person = data.members.find((m) => m.kind === 'human')?.id ?? DEV_DEVICE_MEMBER;
    this.#log = [...data.events];
    this.#lastAt = this.#log.at(-1)?.at ?? 0;
  }

  // ─── The event log ────────────────────────────────────────────────────────────────────────────

  /** The current revision: the number of events in the log. */
  get rev(): number {
    return this.#log.length;
  }

  /** `GET /v1/workspace`'s `setup_needed`: true while the workspace has no person. */
  get setupNeeded(): boolean {
    return !this.members.some((m) => m.kind === 'human');
  }

  /** The event at revision `rev` (1-based). */
  eventAt(rev: number): Event | undefined {
    return rev >= 1 ? this.#log[rev - 1] : undefined;
  }

  /** Every event after revision `rev`, oldest first. */
  eventsAfter(rev: number): Event[] {
    return this.#log.slice(Math.max(0, rev));
  }

  /**
   * Appends an event with a new ULID. `author` is the caller's member; an agent's events also
   * name its owner in `on_behalf_of`. The body is copied, so later changes to state never
   * rewrite history.
   */
  append(author: MemberId, body: EventBody): Event {
    const at = Math.max(Date.now(), this.#lastAt);
    this.#lastAt = at;
    const owner = this.#ownerOfAgent(author);
    const header = { id: ulid(), at, workspace: this.workspace.id, author };
    const event: Event =
      owner === undefined
        ? { ...header, body: structuredClone(body) }
        : { ...header, on_behalf_of: owner, body: structuredClone(body) };
    this.#log.push(event);
    this.#scheduleFlush();
    return event;
  }

  /** Calls `listener` after each batch of new events. Returns the unsubscribe function. */
  subscribe(listener: () => void): () => void {
    this.#listeners.add(listener);
    return () => this.#listeners.delete(listener);
  }

  /** Runs `task` after `ms`, unless the hub is disposed first. */
  later(ms: number, task: () => void): void {
    if (this.#disposed) {
      return;
    }
    const timer = setTimeout(() => {
      this.#timers.delete(timer);
      guarded(task);
    }, ms);
    this.#timers.add(timer);
  }

  /** Cancels pending timers and drops subscribers. */
  dispose(): void {
    this.#disposed = true;
    for (const timer of this.#timers) {
      clearTimeout(timer);
    }
    this.#timers.clear();
    clearTimeout(this.#flushTimer);
    this.#flushTimer = undefined;
    this.#listeners.clear();
  }

  // ─── Lookups ──────────────────────────────────────────────────────────────────────────────────

  findMember(id: string): Member | undefined {
    const bare = bareId(id, 'mem');
    return this.members.find((m) => m.id === bare);
  }

  findMachine(id: string): Machine | undefined {
    const bare = bareId(id, 'mch');
    return this.machines.find((m) => m.id === bare);
  }

  findPersona(id: string): Persona | undefined {
    const bare = bareId(id, 'per');
    return this.personas.find((p) => p.id === bare);
  }

  findProject(id: string): Project | undefined {
    const bare = bareId(id, 'prj');
    return this.projects.find((p) => p.id === bare);
  }

  findWorkstream(id: string): Workstream | undefined {
    const bare = bareId(id, 'wst');
    return this.workstreams.find((w) => w.id === bare);
  }

  /** A task by id only, as bodies and events refer to tasks. */
  findTaskById(id: string): Task | undefined {
    const bare = bareId(id, 'tsk');
    return this.tasks.find((t) => t.id === bare);
  }

  /** A task by id or by key (`PAP-4`), as task routes accept either. */
  findTask(ref: string): Task | undefined {
    return this.findTaskById(ref) ?? this.tasks.find((t) => t.key === ref);
  }

  findSession(id: string): Session | undefined {
    const bare = bareId(id, 'ses');
    return this.sessions.find((s) => s.id === bare);
  }

  findDispatch(id: string): Dispatch | undefined {
    const bare = bareId(id, 'dsp');
    return this.dispatches.find((d) => d.id === bare);
  }

  findAsk(id: string): Ask | undefined {
    const bare = bareId(id, 'ask');
    return this.asks.find((a) => a.id === bare);
  }

  /** Whether the task is the agent's own: it is the assignee, or holds an active dispatch. */
  isOwnTask(task: Task, agent: MemberId): boolean {
    return (
      task.assignee === agent ||
      this.dispatches.some((d) => d.task === task.id && d.agent === agent && d.ended === undefined)
    );
  }

  /** Whether the session's runner can be reached. */
  canReach(session: Session): boolean {
    const machine = this.machines.find((m) => m.id === session.machine);
    return session.state !== 'unreachable' && machine?.liveness === 'live';
  }

  #ownerOfAgent(id: MemberId): MemberId | undefined {
    const member = this.members.find((m) => m.id === id);
    return member?.kind === 'agent' ? member.owner : undefined;
  }

  #scheduleFlush(): void {
    if (this.#flushTimer !== undefined || this.#disposed) {
      return;
    }
    this.#flushTimer = setTimeout(() => {
      this.#flushTimer = undefined;
      for (const listener of [...this.#listeners]) {
        guarded(listener);
      }
    }, BATCH_MS);
  }
}

/** Runs a timer callback; a bug in one must not take the whole mock down. */
function guarded(task: () => void): void {
  try {
    task();
  } catch (error) {
    console.error('mock-hub: a timer failed:', error);
  }
}
