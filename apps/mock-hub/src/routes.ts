// Every HTTP route in docs/build/contracts/api-v1.md.
//
// A route is a method, a path pattern and who may call it: routes marked **agent** in the
// contract accept both token scopes, the others need a device token. Agent tokens read the whole
// workspace but write only to their own tasks and sessions (403 otherwise). Handlers validate the
// whole request before changing anything, and every event they append is authored by the token's
// member, never by the request body.

import { canMove } from './rules.ts';
import {
  announceSession,
  createSession,
  endSession,
  interrupt,
  resumeAfterAnswer,
  sendText,
  waitOnAsk,
} from './simulate.ts';
import type { Hub } from './state.ts';
import { transcriptPage } from './transcripts.ts';
import {
  ASK_KINDS,
  ASK_STATES,
  END_MODES,
  ENGINES,
  HEALTHS,
  KEYS,
  PERMISSION_MODES,
  PRIORITIES,
  RECEIPT_KINDS,
  SCHEDULERS,
  SESSION_STATES,
  TASK_STATUSES,
  WORKSTREAM_STATUSES,
  type Answer,
  type Ask,
  type Brief,
  type BriefTarget,
  type EventBody,
  type HostInfo,
  type Location,
  type Machine,
  type Member,
  type MemberId,
  type Mover,
  type Project,
  type Receipt,
  type Session,
  type Subtask,
  type Task,
  type TaskStatus,
  type TokenScope,
  type Workstream,
} from './types.ts';
import { ulid } from './ulid.ts';
import {
  ApiFailure,
  Fields,
  conflict,
  forbidden,
  invalid,
  notFound,
  oneOf,
  queryEnums,
  queryInt,
  queryLimit,
  queryValue,
  unavailable,
} from './validate.ts';

export const MOCK_VERSION = '0.1.0-mock';
/** `PROTOCOL_VERSION` and `PROTOCOL_MIN` in crates/protocol/src/version.rs. */
export const PROTOCOL_VERSION = 1;
export const PROTOCOL_MIN = 1;

// ─── Tokens ─────────────────────────────────────────────────────────────────────────────────────

/** The mock's tokens. A `Map`, so no inherited object property can pass for a token. */
const TOKENS = new Map<string, { member: MemberId; scope: TokenScope }>([
  ['dev-device-token', { member: '01JB000000000000000MEM0001', scope: 'device' }],
  ['dev-agent-token', { member: '01JB000000000000000MEM0002', scope: 'agent' }],
]);

/** Who is calling: the token's member and scope. */
export interface Caller {
  member: Member;
  scope: TokenScope;
}

/** The caller for a token, or a 401. */
export function authenticate(hub: Hub, token: string | undefined): Caller {
  if (token === undefined) {
    throw new ApiFailure('unauthorized', 'No token: send it in an "Authorization: Bearer" header.');
  }
  const grant = TOKENS.get(token);
  const member = grant === undefined ? undefined : hub.findMember(grant.member);
  if (grant === undefined || member === undefined) {
    throw new ApiFailure('unauthorized', 'Unknown token.');
  }
  return { member, scope: grant.scope };
}

/** The token in an `Authorization: Bearer <token>` header. */
export function bearerToken(header: string | undefined): string | undefined {
  return /^Bearer +(\S+) *$/i.exec(header ?? '')?.[1];
}

// ─── Routing ────────────────────────────────────────────────────────────────────────────────────

export interface ApiRequest {
  method: string;
  path: string;
  query: URLSearchParams;
  authorization: string | undefined;
  readBody: () => Promise<unknown>;
}

export interface Reply {
  status: number;
  body?: unknown;
}

interface Context {
  caller: Caller;
  query: URLSearchParams;
  body: unknown;
  param: (name: string) => string;
}

type Handler = (hub: Hub, ctx: Context) => Reply;

interface Route {
  method: string;
  pattern: string;
  /** `agent` routes accept both scopes; `device` routes only device tokens. */
  access: 'agent' | 'device';
  handler: Handler;
}

const ok = (body: unknown): Reply => ({ status: 200, body });
const created = (body: unknown): Reply => ({ status: 201, body });
const accepted = (body: unknown): Reply => ({ status: 202, body });
const noContent = (): Reply => ({ status: 204 });

/** Answers one API request. Throws `ApiFailure` for every error the contract lists. */
export async function handleApi(hub: Hub, request: ApiRequest): Promise<Reply> {
  // The one route without auth, so clients can check versions first.
  if (request.method === 'GET' && request.path === '/v1/host/info') {
    return ok(hostInfo(hub));
  }
  const match = matchRoute(request.method, request.path);
  if (match === undefined) {
    throw notFound(`No route for ${request.method} ${request.path}.`);
  }
  const caller = authenticate(hub, bearerToken(request.authorization));
  const { pattern, access, handler } = match.route;
  if (access === 'device' && caller.scope !== 'device') {
    throw forbidden(`${request.method} ${pattern} needs a device token.`);
  }
  const body = request.method === 'GET' ? undefined : await request.readBody();
  const param = (name: string): string => {
    const value = match.params.get(name);
    if (value === undefined) {
      throw new Error(`route ${pattern} has no :${name}`);
    }
    return value;
  };
  return handler(hub, { caller, query: request.query, body, param });
}

function matchRoute(
  method: string,
  path: string,
): { route: Route; params: Map<string, string> } | undefined {
  const parts = path.split('/');
  for (const route of ROUTES) {
    const pattern = route.pattern.split('/');
    if (route.method !== method || pattern.length !== parts.length) {
      continue;
    }
    const params = new Map<string, string>();
    const matches = pattern.every((segment, i) => {
      const part = parts[i] ?? '';
      if (segment.startsWith(':')) {
        params.set(segment.slice(1), decodeSegment(part));
        return part !== '';
      }
      return segment === part;
    });
    if (matches) {
      return { route, params };
    }
  }
  return undefined;
}

function decodeSegment(segment: string): string {
  try {
    return decodeURIComponent(segment);
  } catch {
    throw invalid(`Malformed path segment "${segment}".`);
  }
}

// ─── Lookups ────────────────────────────────────────────────────────────────────────────────────
// A missing id in the path is a 404. A missing id in a body is a malformed request, so a 400.

function found<T>(value: T | undefined, message: string): T {
  if (value === undefined) {
    throw notFound(message);
  }
  return value;
}

function known<T>(value: T | undefined, message: string): T {
  if (value === undefined) {
    throw invalid(message);
  }
  return value;
}

const taskAt = (hub: Hub, ref: string): Task => found(hub.findTask(ref), `No task ${ref}.`);
const sessionAt = (hub: Hub, id: string): Session => found(hub.findSession(id), `No session ${id}.`);

const memberRef = (hub: Hub, id: string, field: string): Member =>
  known(hub.findMember(id), `${field}: no member ${id}.`);

function agentRef(hub: Hub, id: string, field: string): Member {
  const member = memberRef(hub, id, field);
  if (member.kind !== 'agent') {
    throw invalid(`${field} must be an agent; ${member.handle} is a person.`);
  }
  return member;
}

// ─── Scope rules ────────────────────────────────────────────────────────────────────────────────
// Agents read everything on their routes; they write only to their own tasks and sessions.

function requireOwnTask(hub: Hub, caller: Caller, task: Task): void {
  if (caller.scope === 'agent' && !hub.isOwnTask(task, caller.member.id)) {
    throw forbidden(`${caller.member.handle} may only change its own tasks; ${task.key} is not one.`);
  }
}

function requireOwnSession(caller: Caller, session: Session): void {
  if (caller.scope === 'agent' && session.agent !== caller.member.id) {
    throw forbidden(`${caller.member.handle} may only act on its own sessions.`);
  }
}

/**
 * Who may answer an ask: a person answers asks to themselves or to agents they own; an agent
 * answers only questions and mentions addressed to itself. Returns why not, or `undefined`.
 */
function answerRefusal(hub: Hub, caller: Caller, ask: Ask): string | undefined {
  const me = caller.member;
  if (caller.scope === 'agent') {
    if (ask.to !== me.id) {
      return `${me.handle} may only answer asks addressed to itself.`;
    }
    if (ask.kind !== 'question' && ask.kind !== 'mention') {
      return `A ${ask.kind} must be answered with a device token.`;
    }
    return undefined;
  }
  if (ask.to !== me.id && hub.findMember(ask.to)?.owner !== me.id) {
    return `${me.handle} may only answer asks addressed to them or to their agents.`;
  }
  return undefined;
}

/** A session that can take input: not ended (409) and on a reachable machine (503). */
function requireRunning(hub: Hub, session: Session): void {
  if (session.state === 'ended') {
    throw conflict(`Session ${session.id} has ended.`);
  }
  requireReachable(hub, session);
}

export function requireReachable(hub: Hub, session: Session): void {
  if (!hub.canReach(session)) {
    const machine = hub.findMachine(session.machine);
    throw unavailable(`${machine?.name ?? 'The machine'} cannot be reached right now.`);
  }
}

function requireLive(machine: Machine): void {
  if (machine.liveness !== 'live') {
    throw unavailable(`${machine.name} is ${machine.liveness}; its runner cannot be reached.`);
  }
}

// ─── Host and workspace ─────────────────────────────────────────────────────────────────────────

function hostInfo(hub: Hub): HostInfo {
  const local = hub.machines.find((m) => m.kind === 'local');
  return {
    name: 'pitcrewd',
    version: MOCK_VERSION,
    protocol: PROTOCOL_VERSION,
    protocol_min: PROTOCOL_MIN,
    roles: ['hub', 'runner'],
    machine: local?.info ?? {
      hostname: 'localhost',
      os: 'linux',
      arch: 'x86_64',
      has_tmux: false,
      home_on_network_fs: false,
    },
    capabilities: ['tmux', 'watch', 'scan'],
  };
}

// ─── Projects and workstreams ───────────────────────────────────────────────────────────────────

const listWorkstreams: Handler = (hub, ctx) => {
  const project = queryValue(ctx.query, 'project');
  return ok(hub.workstreams.filter((w) => project === undefined || w.project === project));
};

const patchWorkstream: Handler = (hub, ctx) => {
  const id = ctx.param('id');
  const workstream = found(hub.findWorkstream(id), `No workstream ${id}.`);
  const fields = new Fields(ctx.body);
  const status = fields.optEnum('status', WORKSTREAM_STATUSES);
  const health = fields.optEnum('health', HEALTHS);
  if (status === undefined && health === undefined) {
    throw invalid('Give a status, a health, or both.');
  }
  const next = { status: status ?? workstream.status, health: health ?? workstream.health };
  if (next.status !== workstream.status || next.health !== workstream.health) {
    workstream.status = next.status;
    workstream.health = next.health;
    hub.append(ctx.caller.member.id, {
      type: 'workstream_changed',
      data: { workstream: workstream.id, ...next },
    });
  }
  return ok(workstream);
};

// ─── Tasks ──────────────────────────────────────────────────────────────────────────────────────

const listTasks: Handler = (hub, ctx) => {
  const project = queryValue(ctx.query, 'project');
  const workstream = queryValue(ctx.query, 'workstream');
  const assignee = queryValue(ctx.query, 'assignee');
  const statuses = queryEnums(ctx.query, 'status', TASK_STATUSES);
  return ok(
    hub.tasks.filter(
      (t) =>
        (project === undefined || t.project === project) &&
        (workstream === undefined || t.workstream === workstream) &&
        (assignee === undefined || t.assignee === assignee) &&
        (statuses.length === 0 || statuses.includes(t.status)),
    ),
  );
};

const createTask: Handler = (hub, ctx) => {
  const fields = new Fields(ctx.body);
  const projectId = fields.string('project');
  const project = known(hub.findProject(projectId), `project: no project ${projectId}.`);
  const workstream = optionalWorkstream(hub, fields.optString('workstream'));
  if (workstream !== undefined && workstream.project !== project.id) {
    throw invalid(`workstream "${workstream.name}" belongs to another project.`);
  }
  const assignee = fields.optString('assignee');
  const task: Task = {
    id: ulid(),
    key: nextTaskKey(hub, project),
    project: project.id,
    workstream: workstream?.id,
    title: fields.text('title'),
    description: fields.optString('description') ?? '',
    status: fields.optEnum('status', TASK_STATUSES) ?? 'todo',
    priority: fields.optEnum('priority', PRIORITIES) ?? 'none',
    assignee: assignee === undefined ? undefined : memberRef(hub, assignee, 'assignee').id,
    labels: fields.optStringArray('labels') ?? [],
    due: fields.optDate('due'),
    blocked_by: [],
    accept_auto: false,
    subtasks: [],
  };
  hub.tasks.push(task);
  hub.append(ctx.caller.member.id, { type: 'task_created', data: { task } });
  return created(task);
};

function optionalWorkstream(hub: Hub, id: string | undefined): Workstream | undefined {
  return id === undefined ? undefined : known(hub.findWorkstream(id), `workstream: no workstream ${id}.`);
}

/** The project's key and one more than the highest task number in it. */
function nextTaskKey(hub: Hub, project: Project): string {
  const prefix = `${project.key}-`;
  const highest = hub.tasks
    .filter((t) => t.project === project.id && t.key.startsWith(prefix))
    .map((t) => Number(t.key.slice(prefix.length)))
    .reduce((max, n) => (Number.isSafeInteger(n) && n > max ? n : max), 0);
  return `${prefix}${highest + 1}`;
}

/** An agent may move only its own task (else 403); a move `can_move` rejects is a 409. */
const moveTask: Handler = (hub, ctx) => {
  const task = taskAt(hub, ctx.param('id'));
  const to = new Fields(ctx.body).enumOf('to', TASK_STATUSES);
  const me = ctx.caller;
  requireOwnTask(hub, me, task);
  const mover: Mover =
    me.scope === 'device' ? { kind: 'person' } : { kind: 'agent', on_own_task: true };
  if (!canMove(task.status, to, mover)) {
    throw conflict(moveRefusal(task, to));
  }
  const from = task.status;
  task.status = to;
  hub.append(me.member.id, { type: 'task_moved', data: { task: task.id, from, to, mover } });
  return ok(task);
};

function moveRefusal(task: Task, to: TaskStatus): string {
  if (task.status === to) {
    return `${task.key} is already ${to}.`;
  }
  return `An agent may only move a task from backlog or todo to in_progress, or from in_progress to review; not ${task.status} → ${to}.`;
}

const assignTask: Handler = (hub, ctx) => {
  const task = taskAt(hub, ctx.param('id'));
  const fields = new Fields(ctx.body);
  if (!fields.has('assignee')) {
    throw invalid('assignee is required; send null to unassign.');
  }
  const id = fields.optString('assignee');
  assign(hub, ctx.caller.member.id, task, id === undefined ? undefined : memberRef(hub, id, 'assignee').id);
  return ok(task);
};

/** Sets the assignee and emits `task_assigned`, if it changes. */
function assign(hub: Hub, author: MemberId, task: Task, assignee: MemberId | undefined): void {
  if (task.assignee !== assignee) {
    task.assignee = assignee;
    hub.append(author, { type: 'task_assigned', data: { task: task.id, assignee } });
  }
}

const replaceSubtasks: Handler = (hub, ctx) => {
  const task = taskAt(hub, ctx.param('id'));
  requireOwnTask(hub, ctx.caller, task);
  if (!Array.isArray(ctx.body)) {
    throw invalid('The body must be an array of subtasks.');
  }
  const incoming = ctx.body.map((value, i) => readSubtask(hub, value, `[${i}]`));
  const me = ctx.caller.member.id;
  let subtasks = incoming;
  if (ctx.caller.scope === 'agent') {
    const ownPlan = (s: Subtask): boolean => s.source.kind === 'agent_plan' && s.source.agent === me;
    if (!incoming.every(ownPlan)) {
      throw forbidden(
        `An agent may only write its own plan: every subtask needs source {"kind":"agent_plan","agent":"${me}"}.`,
      );
    }
    // The agent replaces its own plan; lines from people and other agents stay.
    subtasks = [...task.subtasks.filter((s) => !ownPlan(s)), ...incoming];
  }
  if (new Set(subtasks.map((s) => s.id)).size !== subtasks.length) {
    throw invalid('Subtask ids must be unique.');
  }
  task.subtasks = subtasks;
  hub.append(me, { type: 'subtasks_replaced', data: { task: task.id, subtasks } });
  return ok(task);
};

function readSubtask(hub: Hub, value: unknown, where: string): Subtask {
  const fields = new Fields(value, where);
  const source = new Fields(fields.raw('source'), `${where}.source`);
  const kind = source.enumOf('kind', ['human', 'agent_plan'] as const);
  return {
    id: fields.ulid('id'),
    text: fields.text('text'),
    done: fields.bool('done'),
    source:
      kind === 'human'
        ? { kind }
        : { kind, agent: agentRef(hub, source.string('agent'), source.name('agent')).id },
  };
}

const postComment: Handler = (hub, ctx) => {
  const task = taskAt(hub, ctx.param('id'));
  requireOwnTask(hub, ctx.caller, task);
  const fields = new Fields(ctx.body);
  const text = fields.text('text');
  const mentions = (fields.optStringArray('mentions') ?? []).map(
    (id, i) => memberRef(hub, id, `mentions[${i}]`).id,
  );
  return created(
    hub.append(ctx.caller.member.id, {
      type: 'comment_posted',
      data: { task: task.id, text, mentions },
    }),
  );
};

/**
 * Starts a session for the agent on the task. An unassigned task is assigned to the agent first,
 * so events come as `task_assigned` (if any), `dispatch_started`, `session_discovered`.
 */

const dispatchTask: Handler = (hub, ctx) => {
  const task = taskAt(hub, ctx.param('id'));
  const fields = new Fields(ctx.body);
  const agent = agentRef(hub, fields.string('agent'), 'agent');
  const brief = fields.optString('brief') ?? (task.description !== '' ? task.description : task.title);
  const place = placeFor(hub, task, fields.optString('machine'));
  if (task.status === 'done' || task.status === 'canceled') {
    throw conflict(`${task.key} is ${task.status}; reopen it before dispatching.`);
  }
  requireLive(place.machine);
  const me = ctx.caller.member.id;
  if (task.assignee === undefined) {
    assign(hub, me, task, agent.id);
  }
  const persona = agent.persona === undefined ? undefined : hub.findPersona(agent.persona);
  const session = createSession(hub, {
    engine: persona?.engine ?? 'claude',
    machine: place.machine.id,
    cwd: place.path,
    branch: place.branch,
    title: task.title,
    agent: agent.id,
    workstream: task.workstream,
    task: task.id,
    link_basis: 'dispatch',
    brief,
  });
  const dispatch = {
    id: ulid(),
    task: task.id,
    agent: agent.id,
    session: session.id,
    brief,
    started: session.started,
  };
  hub.dispatches.push(dispatch);
  hub.append(me, { type: 'dispatch_started', data: { dispatch } });
  announceSession(hub, session);
  return accepted(dispatch);
};

/**
 * Where a dispatched session runs: the requested machine, or the first location of the task's
 * workstream, or the project's root folder, or the local machine's home.
 */
function placeFor(
  hub: Hub,
  task: Task,
  machineId: string | undefined,
): { machine: Machine; path: string; branch?: string | undefined } {
  const workstream = task.workstream === undefined ? undefined : hub.findWorkstream(task.workstream);
  const root = hub.findProject(task.project)?.root;
  const locations: Location[] = [...(workstream?.locations ?? []), ...(root ? [root] : [])];
  const machine =
    machineId !== undefined
      ? known(hub.findMachine(machineId), `machine: no machine ${machineId}.`)
      : hub.findMachine(locations[0]?.machine ?? '') ?? hub.machines.find((m) => m.kind === 'local');
  if (machine === undefined) {
    throw new ApiFailure('internal', 'The workspace has no machine to run on.');
  }
  const location = locations.find((l) => l.machine === machine.id);
  return { machine, path: location?.path ?? '~', branch: location?.branch };
}

// ─── Sessions ───────────────────────────────────────────────────────────────────────────────────

const listSessions: Handler = (hub, ctx) => {
  const machine = queryValue(ctx.query, 'machine');
  const workstream = queryValue(ctx.query, 'workstream');
  const task = queryValue(ctx.query, 'task');
  const states = queryEnums(ctx.query, 'state', SESSION_STATES);
  return ok(
    hub.sessions.filter(
      (s) =>
        (machine === undefined || s.machine === machine) &&
        (workstream === undefined || s.workstream === workstream) &&
        (task === undefined || s.task === task) &&
        (states.length === 0 || states.includes(s.state)),
    ),
  );
};

const getTranscript: Handler = (hub, ctx) => {
  const session = sessionAt(hub, ctx.param('id'));
  const before = queryInt(ctx.query, 'before', 0);
  const limit = queryLimit(ctx.query, 200, 1000);
  return ok(transcriptPage(hub.transcripts.get(session.id) ?? [], before, limit));
};

const startSession: Handler = (hub, ctx) => {
  const fields = new Fields(ctx.body);
  const machineId = fields.string('machine');
  const machine = known(hub.findMachine(machineId), `machine: no machine ${machineId}.`);
  const engine = fields.enumOf('engine', ENGINES);
  const cwd = fields.text('cwd');
  const agentId = fields.optString('agent');
  const agent = agentId === undefined ? undefined : agentRef(hub, agentId, 'agent');
  const taskId = fields.optString('task');
  const task = taskId === undefined ? undefined : known(hub.findTaskById(taskId), `task: no task ${taskId}.`);
  const personaId = fields.optString('persona');
  if (personaId !== undefined) {
    known(hub.findPersona(personaId), `persona: no persona ${personaId}.`);
  }
  const brief = fields.optString('brief');
  // Checked for the contract's sake; a session does not record them.
  fields.optString('model');
  fields.optEnum('permission_mode', PERMISSION_MODES);
  requireLive(machine);
  const folder = task === undefined ? workstreamByFolder(hub, machine.id, cwd) : undefined;
  const session = createSession(hub, {
    engine,
    machine: machine.id,
    cwd,
    title: task?.title,
    agent: agent?.id,
    workstream: task?.workstream ?? folder?.id,
    task: task?.id,
    link_basis: task !== undefined ? 'manual' : folder !== undefined ? 'folder' : undefined,
    brief,
  });
  announceSession(hub, session);
  return accepted(session);
};

/** The workstream whose location on `machine` contains `cwd`, as the hub links by folder. */
function workstreamByFolder(hub: Hub, machine: string, cwd: string): Workstream | undefined {
  const inside = (l: Location): boolean =>
    l.machine === machine && (cwd === l.path || cwd.startsWith(`${l.path.replace(/\/+$/, '')}/`));
  return hub.workstreams.find((w) => w.locations.some(inside));
}

const sendToSession: Handler = (hub, ctx) => {
  const session = sessionAt(hub, ctx.param('id'));
  const text = new Fields(ctx.body).string('text');
  requireRunning(hub, session);
  sendText(hub, session, text);
  return noContent();
};

const sendKeys: Handler = (hub, ctx) => {
  const session = sessionAt(hub, ctx.param('id'));
  const keys = (new Fields(ctx.body).optArray('keys') ?? []).map((k, i) => oneOf(k, KEYS, `keys[${i}]`));
  if (keys.length === 0) {
    throw invalid('keys must list at least one key.');
  }
  requireRunning(hub, session);
  if (keys.includes('escape') || keys.includes('ctrl_c')) {
    interrupt(hub, session);
  }
  return noContent();
};

const interruptSession: Handler = (hub, ctx) => {
  const session = sessionAt(hub, ctx.param('id'));
  requireRunning(hub, session);
  interrupt(hub, session);
  return noContent();
};

const endSessionRoute: Handler = (hub, ctx) => {
  const session = sessionAt(hub, ctx.param('id'));
  const mode = new Fields(ctx.body).enumOf('mode', END_MODES);
  requireRunning(hub, session);
  endSession(hub, session, mode);
  return noContent();
};

const linkSession: Handler = (hub, ctx) => {
  const session = sessionAt(hub, ctx.param('id'));
  const fields = new Fields(ctx.body);
  const taskId = fields.optString('task');
  const task = taskId === undefined ? undefined : known(hub.findTaskById(taskId), `task: no task ${taskId}.`);
  const workstream = optionalWorkstream(hub, fields.optString('workstream') ?? task?.workstream);
  if (task === undefined && workstream === undefined) {
    throw invalid('Give a workstream, a task, or both.');
  }
  if (task?.workstream !== undefined && workstream !== undefined && task.workstream !== workstream.id) {
    throw invalid(`${task.key} is not in workstream "${workstream.name}".`);
  }
  session.workstream = workstream?.id;
  session.task = task?.id;
  session.link_basis = 'manual';
  hub.append(ctx.caller.member.id, {
    type: 'session_linked',
    data: { session: session.id, workstream: workstream?.id, task: task?.id, basis: 'manual' },
  });
  return ok(session);
};

// ─── Asks ───────────────────────────────────────────────────────────────────────────────────────

const listAsks: Handler = (hub, ctx) => {
  const to = queryValue(ctx.query, 'to');
  const states = queryEnums(ctx.query, 'state', ASK_STATES);
  return ok(
    hub.asks.filter(
      (a) => (to === undefined || a.to === to) && (states.length === 0 || states.includes(a.state)),
    ),
  );
};

const raiseAsk: Handler = (hub, ctx) => {
  const fields = new Fields(ctx.body);
  const me = ctx.caller;
  const taskId = fields.optString('task');
  const sessionId = fields.optString('session');
  const task = taskId === undefined ? undefined : known(hub.findTaskById(taskId), `task: no task ${taskId}.`);
  const session =
    sessionId === undefined ? undefined : known(hub.findSession(sessionId), `session: no session ${sessionId}.`);
  const ask: Ask = {
    id: ulid(),
    kind: fields.enumOf('kind', ASK_KINDS),
    from: me.member.id,
    to: memberRef(hub, fields.string('to'), 'to').id,
    task: task?.id,
    session: session?.id,
    title: fields.text('title'),
    body: fields.optString('body') ?? '',
    options: fields.optStringArray('options') ?? [],
    receipts: (fields.optArray('receipts') ?? []).map((r, i) => readReceipt(r, `receipts[${i}]`)),
    state: 'open',
    created: Date.now(),
  };
  if (task !== undefined) {
    requireOwnTask(hub, me, task);
  }
  if (session !== undefined) {
    requireOwnSession(me, session);
  }
  hub.asks.push(ask);
  hub.append(me.member.id, { type: 'ask_raised', data: { ask } });
  if (session !== undefined && session.agent === me.member.id) {
    waitOnAsk(hub, session, ask);
  }
  return created(ask);
};

function readReceipt(value: unknown, where: string): Receipt {
  const fields = new Fields(value, where);
  const kind = fields.enumOf('kind', RECEIPT_KINDS);
  switch (kind) {
    case 'transcript':
      return { kind, session: fields.string('session'), offset: fields.int('offset') };
    case 'commit':
      return { kind, repo: fields.text('repo'), sha: fields.text('sha') };
    case 'pull_request':
      return { kind, url: fields.text('url') };
    case 'job':
      return { kind, scheduler: fields.enumOf('scheduler', SCHEDULERS), id: fields.text('id') };
    case 'file':
      return { kind, location: readLocation(fields.raw('location'), fields.name('location')) };
    case 'event':
      return { kind, id: fields.text('id') };
  }
}

function readLocation(value: unknown, where: string): Location {
  const fields = new Fields(value, where);
  return { machine: fields.text('machine'), path: fields.text('path'), branch: fields.optString('branch') };
}

const answerAsk: Handler = (hub, ctx) => {
  const id = ctx.param('id');
  const ask = found(hub.findAsk(id), `No ask ${id}.`);
  const fields = new Fields(ctx.body);
  const option = fields.optInt('option');
  const text = fields.optString('text');
  if (option === undefined && text === undefined) {
    throw invalid('Give an option, a text, or both.');
  }
  if (option !== undefined && option >= ask.options.length) {
    throw invalid(
      ask.options.length === 0
        ? 'This ask offers no options; answer with text.'
        : `option must be below ${ask.options.length}.`,
    );
  }
  const refusal = answerRefusal(hub, ctx.caller, ask);
  if (refusal !== undefined) {
    throw forbidden(refusal);
  }
  const me = ctx.caller.member;
  if (ask.state !== 'open') {
    throw conflict(`This ask is already ${ask.state}.`);
  }
  const answer: Answer = { by: me.id, option, text, at: Date.now() };
  ask.state = 'answered';
  ask.answer = answer;
  hub.append(me.id, { type: 'ask_answered', data: { ask: ask.id, answer } });
  const session = ask.session === undefined ? undefined : hub.findSession(ask.session);
  if (session !== undefined) {
    resumeAfterAnswer(hub, session);
  }
  return ok(ask);
};

// ─── Briefs ─────────────────────────────────────────────────────────────────────────────────────

const putBrief: Handler = (hub, ctx) => {
  const target = briefTarget(hub, ctx.param('kind'), ctx.param('id'));
  const fields = new Fields(ctx.body);
  const brief: Brief = {
    target,
    text: fields.string('text'),
    next: fields.optString('next'),
    pinned: fields.bool('pinned'),
    source: 'person',
    updated: Date.now(),
    receipts: [],
  };
  const index = hub.briefs.findIndex((b) => b.target.kind === target.kind && b.target.id === target.id);
  if (index === -1) {
    hub.briefs.push(brief);
  } else {
    hub.briefs[index] = brief;
  }
  hub.append(ctx.caller.member.id, {
    type: 'brief_accepted',
    data: { target, text: brief.text, pinned: brief.pinned },
  });
  return ok(brief);
};

function briefTarget(hub: Hub, kind: string, id: string): BriefTarget {
  if (kind === 'project') {
    return { kind, id: found(hub.findProject(id), `No project ${id}.`).id };
  }
  if (kind === 'workstream') {
    return { kind, id: found(hub.findWorkstream(id), `No workstream ${id}.`).id };
  }
  throw notFound(`Briefs belong to a project or a workstream, not to "${kind}".`);
}

// ─── Activity ───────────────────────────────────────────────────────────────────────────────────

interface EventFilter {
  project?: string | undefined;
  workstream?: string | undefined;
  task?: string | undefined;
  session?: string | undefined;
}

/**
 * Activity: the newest `limit` matching events below revision `before` (exclusive), oldest first.
 * One extra match is looked for to tell whether older ones exist (`at_start`).
 */
const listEvents: Handler = (hub, ctx) => {
  const limit = queryLimit(ctx.query, 100, 500);
  const before = queryInt(ctx.query, 'before', 0);
  const filter: EventFilter = {
    project: queryValue(ctx.query, 'project'),
    workstream: queryValue(ctx.query, 'workstream'),
    task: queryValue(ctx.query, 'task'),
    session: queryValue(ctx.query, 'session'),
  };
  const revs: number[] = [];
  for (let rev = Math.min(before ?? Infinity, hub.rev + 1) - 1; rev >= 1 && revs.length <= limit; rev--) {
    const event = hub.eventAt(rev);
    if (event !== undefined && touches(hub, event.body, filter)) {
      revs.push(rev);
    }
  }
  const atStart = revs.length <= limit;
  const page = revs.slice(0, limit).reverse();
  return ok({
    events: page.map((rev) => hub.eventAt(rev)),
    from_rev: page[0] ?? 0,
    to_rev: page.at(-1) ?? 0,
    at_start: atStart,
  });
};

/** Whether an event is about the filter's project, workstream, task and session. */
function touches(hub: Hub, body: EventBody, filter: EventFilter): boolean {
  if (Object.values(filter).every((v) => v === undefined)) {
    return true;
  }
  const direct = directRefs(hub, body);
  const session = direct.session === undefined ? undefined : hub.findSession(direct.session);
  const taskId = direct.task ?? session?.task;
  const task = taskId === undefined ? undefined : hub.findTaskById(taskId);
  const workstreamId = direct.workstream ?? task?.workstream ?? session?.workstream;
  const workstream = workstreamId === undefined ? undefined : hub.findWorkstream(workstreamId);
  const projectId = direct.project ?? task?.project ?? workstream?.project;
  return (
    (filter.session === undefined || direct.session === filter.session) &&
    (filter.task === undefined || taskId === filter.task) &&
    (filter.workstream === undefined || workstreamId === filter.workstream) &&
    (filter.project === undefined || projectId === filter.project)
  );
}

/** What an event names directly; `touches` adds the parents. */
function directRefs(hub: Hub, body: EventBody): EventFilter {
  switch (body.type) {
    case 'session_discovered': {
      const { session } = body.data;
      return { session: session.id, task: session.task, workstream: session.workstream };
    }
    case 'session_state_changed':
    case 'turn_ended':
    case 'tool_ran':
    case 'file_edited':
    case 'session_ended':
      return { session: body.data.session };
    case 'session_linked':
      return { session: body.data.session, task: body.data.task, workstream: body.data.workstream };
    case 'project_created':
      return { project: body.data.project.id };
    case 'workstream_created':
      return { workstream: body.data.workstream.id };
    case 'workstream_changed':
      return { workstream: body.data.workstream };
    case 'task_created':
      return { task: body.data.task.id };
    case 'task_moved':
    case 'task_assigned':
    case 'subtasks_replaced':
      return { task: body.data.task };
    case 'dispatch_started':
      return { task: body.data.dispatch.task, session: body.data.dispatch.session };
    case 'dispatch_finished': {
      const dispatch = hub.findDispatch(body.data.dispatch);
      return { task: dispatch?.task, session: dispatch?.session };
    }
    case 'ask_raised':
      return { task: body.data.ask.task, session: body.data.ask.session };
    case 'ask_answered': {
      const ask = hub.findAsk(body.data.ask);
      return { task: ask?.task, session: ask?.session };
    }
    case 'comment_posted':
      return { task: body.data.task, workstream: body.data.workstream };
    case 'brief_proposed':
    case 'brief_accepted':
      return body.data.target.kind === 'project'
        ? { project: body.data.target.id }
        : { workstream: body.data.target.id };
    case 'decision_recorded':
      return { workstream: body.data.workstream };
    case 'machine_liveness':
      return {};
  }
}

// ─── WebSocket routes, reached without an upgrade ───────────────────────────────────────────────

const needsWebSocket: Handler = () => {
  throw invalid('This route is a WebSocket; connect with "Upgrade: websocket".');
};

// ─── The table ──────────────────────────────────────────────────────────────────────────────────

const route = (method: string, pattern: string, access: Route['access'], handler: Handler): Route => ({
  method,
  pattern,
  access,
  handler,
});

const ROUTES: Route[] = [
  // Host and workspace (`GET /v1/host/info` is handled before auth, in `handleApi`).
  route('GET', '/v1/me', 'agent', (_hub, ctx) => ok(ctx.caller.member)),
  route('GET', '/v1/workspace', 'device', (hub) => ok({ workspace: hub.workspace, rev: hub.rev })),
  route('GET', '/v1/machines', 'device', (hub) => ok(hub.machines)),
  route('GET', '/v1/members', 'agent', (hub) => ok(hub.members)),
  route('GET', '/v1/personas', 'device', (hub) => ok(hub.personas)),
  route('GET', '/v1/teams', 'device', (hub) => ok(hub.teams)),
  // Projects and workstreams.
  route('GET', '/v1/projects', 'device', (hub) => ok(hub.projects)),
  route('GET', '/v1/projects/:id', 'device', (hub, ctx) =>
    ok(found(hub.findProject(ctx.param('id')), `No project ${ctx.param('id')}.`)),
  ),
  route('GET', '/v1/workstreams', 'device', listWorkstreams),
  route('GET', '/v1/workstreams/:id', 'device', (hub, ctx) =>
    ok(found(hub.findWorkstream(ctx.param('id')), `No workstream ${ctx.param('id')}.`)),
  ),
  route('PATCH', '/v1/workstreams/:id', 'device', patchWorkstream),
  // Tasks.
  route('GET', '/v1/tasks', 'agent', listTasks),
  route('GET', '/v1/tasks/:id', 'agent', (hub, ctx) => ok(taskAt(hub, ctx.param('id')))),
  route('POST', '/v1/tasks', 'device', createTask),
  route('POST', '/v1/tasks/:id/move', 'agent', moveTask),
  route('POST', '/v1/tasks/:id/assign', 'device', assignTask),
  route('PUT', '/v1/tasks/:id/subtasks', 'agent', replaceSubtasks),
  route('POST', '/v1/tasks/:id/comments', 'agent', postComment),
  route('POST', '/v1/tasks/:id/dispatch', 'device', dispatchTask),
  // Sessions.
  route('GET', '/v1/sessions', 'device', listSessions),
  route('GET', '/v1/sessions/:id', 'device', (hub, ctx) => ok(sessionAt(hub, ctx.param('id')))),
  route('GET', '/v1/sessions/:id/transcript', 'device', getTranscript),
  route('GET', '/v1/sessions/:id/terminal', 'device', needsWebSocket),
  route('POST', '/v1/sessions', 'device', startSession),
  route('POST', '/v1/sessions/:id/send', 'device', sendToSession),
  route('POST', '/v1/sessions/:id/keys', 'device', sendKeys),
  route('POST', '/v1/sessions/:id/interrupt', 'device', interruptSession),
  route('POST', '/v1/sessions/:id/end', 'device', endSessionRoute),
  route('POST', '/v1/sessions/:id/link', 'device', linkSession),
  // Asks, briefs, activity.
  route('GET', '/v1/asks', 'agent', listAsks),
  route('POST', '/v1/asks', 'agent', raiseAsk),
  route('POST', '/v1/asks/:id/answer', 'agent', answerAsk),
  route('GET', '/v1/briefs', 'device', (hub) => ok(hub.briefs)),
  route('PUT', '/v1/briefs/:kind/:id', 'device', putBrief),
  route('GET', '/v1/events', 'device', listEvents),
  route('GET', '/v1/stream', 'device', needsWebSocket),
];
