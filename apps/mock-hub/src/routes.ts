// Every HTTP route in docs/build/contracts/api-v1.md.
//
// A route is a method, a path pattern and who may call it: routes marked **agent** in the
// contract accept both token scopes, the others need a device token. Agent tokens read the whole
// workspace but write only to their own tasks and sessions (403 otherwise). Handlers validate the
// whole request before changing anything, and every event they append is authored by the token's
// member, never by the request body.

import { blocksPage, daysPage } from './recaps.ts';
import { canMove } from './rules.ts';
import { startScan, type StreamedBody } from './scan.ts';
import { agentAccounts, checkMachine, signInStatus, startSignIn, stopSignIn } from './machine-setup.ts';
import {
  announceSession,
  createSession,
  endSession,
  interrupt,
  resumeAfterAnswer,
  sendText,
  waitOnAsk,
} from './simulate.ts';
import { DEV_AGENT_MEMBER, DEV_DEVICE_MEMBER, type Hub } from './state.ts';
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
  PROJECT_STATUSES,
  RECEIPT_KINDS,
  SCHEDULERS,
  SESSION_STATES,
  TASK_STATUSES,
  WORKSTREAM_STATUSES,
  type Answer,
  type Ask,
  type Brief,
  type BriefProposal,
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
  type TaskId,
  type TaskPatch,
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
  isRecord,
  notFound,
  oneOf,
  queryEnums,
  queryInt,
  queryId,
  queryLimit,
  unavailable,
} from './validate.ts';

import { files } from './files.ts';
import { parseImport, includesSession, eventVisible, includedRecaps } from './import.ts';
export const MOCK_VERSION = '0.1.0-mock';
/** `PROTOCOL_VERSION` and `PROTOCOL_MIN` in crates/protocol/src/version.rs. */
export const PROTOCOL_VERSION = 1;
export const PROTOCOL_MIN = 1;

// ─── Tokens ─────────────────────────────────────────────────────────────────────────────────────

/** The mock's tokens. A `Map`, so no inherited object property can pass for a token. */
const TOKENS = new Map<string, { member: MemberId; scope: TokenScope }>([
  ['dev-device-token', { member: DEV_DEVICE_MEMBER, scope: 'device' }],
  ['dev-second-device-token', { member: '01JB000000000000000MEM0007', scope: 'device' }],
  ['dev-agent-token', { member: DEV_AGENT_MEMBER, scope: 'agent' }],
]);

/**
 * Who is calling: the token's scope and bound member id, always known, and the `Member` record,
 * once one exists. Before `POST /v1/setup`, a fresh workspace knows no member yet, so `member` is
 * `undefined` even though the token authenticates fine (api-v1.md, "The first run").
 */
export interface Caller {
  memberId: MemberId;
  member: Member | undefined;
  scope: TokenScope;
}

/** The caller for a token, or a 401 for one the mock does not know at all. */
export function authenticate(hub: Hub, token: string | undefined): Caller {
  if (token === undefined) {
    throw new ApiFailure('unauthorized', 'No token: send it in an "Authorization: Bearer" header.');
  }
  const grant = TOKENS.get(token);
  if (grant === undefined) {
    throw new ApiFailure('unauthorized', 'Unknown token.');
  }
  return { memberId: grant.member, member: hub.findMember(grant.member), scope: grant.scope };
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
  /** Instead of `body`: an answer written over time (`POST /v1/machines/{id}/scan`). */
  stream?: StreamedBody;
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
const sessionAt = (hub: Hub, id: string): Session => {
  const session = found(hub.findSession(id), `No session ${id}.`);
  if (!includesSession(hub.importChoice, session)) throw notFound(`No session ${id}.`);
  return session;
};

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

/** A readable name for error messages, even before the caller's member exists (fresh mode). */
function callerHandle(caller: Caller): string {
  return caller.member?.handle ?? caller.memberId;
}

function requireOwnTask(hub: Hub, caller: Caller, task: Task): void {
  if (caller.scope === 'agent' && !hub.isOwnTask(task, caller.memberId)) {
    throw forbidden(`${callerHandle(caller)} may only change its own tasks; ${task.key} is not one.`);
  }
}

function requireOwnSession(caller: Caller, session: Session): void {
  if (caller.scope === 'agent' && session.agent !== caller.memberId) {
    throw forbidden(`${callerHandle(caller)} may only act on its own sessions.`);
  }
}

/**
 * Who may answer an ask: a person answers asks to themselves or to agents they own; an agent
 * answers only questions and mentions addressed to itself. Returns why not, or `undefined`.
 */
function answerRefusal(hub: Hub, caller: Caller, ask: Ask): string | undefined {
  if (caller.scope === 'agent') {
    if (ask.to !== caller.memberId) {
      return `${callerHandle(caller)} may only answer asks addressed to itself.`;
    }
    if (ask.kind !== 'question' && ask.kind !== 'mention') {
      return `A ${ask.kind} must be answered with a device token.`;
    }
    return undefined;
  }
  if (ask.to !== caller.memberId && hub.findMember(ask.to)?.owner !== caller.memberId) {
    return `${callerHandle(caller)} may only answer asks addressed to them or to their agents.`;
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

/** `@` followed by 1 to 32 of lower-case letters, digits, `_` or `-` (api-v1.md, "The first run"). */
const SETUP_HANDLE = /^@[a-z0-9_-]{1,32}$/;
/** The back office's handle, reserved: setup never gives it to a person. */
const OFFICE_HANDLE = '@office';
/** C0 and C1 control characters, disallowed in every `Setup` string. */
const CONTROL_CHARS = /[\u0000-\u001F\u007F-\u009F]/;

/**
 * A `Setup` name: trimmed of whitespace, then 1 to `max` characters (code points) with no control
 * character. Returns the trimmed value, which is what is stored (api-v1.md, "The first run").
 */
function setupText(value: string, field: string, max: number): string {
  const trimmed = value.trim();
  if (charCount(trimmed) < 1 || charCount(trimmed) > max) {
    throw invalid(`${field} must be 1 to ${max} characters after trimming.`);
  }
  if (CONTROL_CHARS.test(trimmed)) {
    throw invalid(`${field} must not contain control characters.`);
  }
  return trimmed;
}

/**
 * `POST /v1/setup`: the first run of a fresh hub. Device tokens only (an agent token never
 * reaches the handler; `handleApi` answers 403 first, since the route is `device`-only).
 *
 * In demo mode the workspace already has a person, so this always answers 409, whatever the body
 * holds (api-v1.md, "The mock starts with the demo's person").
 */
const setupHub: Handler = (hub, ctx) => {
  if (!hub.setupNeeded) {
    throw conflict('This workspace is already set up.');
  }
  const fields = new Fields(ctx.body);
  const workspaceName = setupText(fields.string('workspace_name'), 'workspace_name', 80);
  const person = new Fields(fields.raw('person'), fields.name('person'));
  const name = setupText(person.string('name'), 'person.name', 80);
  // The handle is not trimmed.
  const handle = person.string('handle');
  if (!SETUP_HANDLE.test(handle)) {
    throw invalid('person.handle must be "@" followed by 1 to 32 of a-z, 0-9, "_" or "-".');
  }
  const machineName = setupText(fields.string('machine_name'), 'machine_name', 60);
  // Reserved for the back office (the mock has none): always taken.
  if (handle === OFFICE_HANDLE) {
    throw conflict(`${OFFICE_HANDLE} is reserved for the back office.`);
  }
  if (hub.members.some((m) => m.handle === handle)) {
    throw conflict(`The handle ${handle} is already taken.`);
  }
  // The person is the device token's own member id, which nothing knew until now.
  const me = ctx.caller.memberId;
  const member: Member = { id: me, kind: 'human', handle, name };
  const machine: Machine = { id: ulid(), name: machineName, kind: 'local', liveness: 'live' };
  hub.members.push(member);
  hub.machines.push(machine);
  hub.workspace.name = workspaceName;
  // One append: both events reach a connected stream in the same batch.
  hub.append(me, { type: 'member_added', data: { member } });
  hub.append(me, { type: 'machine_added', data: { machine } });
  return ok({ workspace: hub.workspace, me: member, machine });
};

// ─── Projects and workstreams ───────────────────────────────────────────────────────────────────

/** `ProjectKey` in ids.rs: 2–10 characters, an uppercase letter, then uppercase letters or digits. */
const PROJECT_KEY = /^[A-Z][A-Z0-9]{1,9}$/;

/**
 * Creates a project. The lead defaults to the caller and is always a member; the status defaults
 * to `in_progress`. A key already in use is a 409.
 */
const createProject: Handler = (hub, ctx) => {
  const fields = new Fields(ctx.body);
  const key = fields.string('key');
  if (!PROJECT_KEY.test(key)) {
    throw invalid('key must be 2 to 10 characters: an uppercase letter, then uppercase letters or digits.');
  }
  const name = fields.text('name');
  const leadId = fields.optString('lead');
  const lead = leadId === undefined ? ctx.caller.memberId : memberRef(hub, leadId, 'lead').id;
  const members: MemberId[] = [];
  for (const [i, id] of (fields.optStringArray('members') ?? []).entries()) {
    const member = memberRef(hub, id, `members[${i}]`).id;
    if (!members.includes(member)) {
      members.push(member);
    }
  }
  if (!members.includes(lead)) {
    members.unshift(lead);
  }
  const status = fields.optEnum('status', PROJECT_STATUSES) ?? 'in_progress';
  const start = fields.optDate('start');
  const due = fields.optDate('due');
  requireStartBeforeDue(start, due);
  const rootValue = fields.raw('root');
  const root = rootValue === undefined ? undefined : knownLocation(hub, rootValue, fields.name('root'));
  const taken = hub.projects.find((p) => p.key === key);
  if (taken !== undefined) {
    throw conflict(`The key ${key} is already used by "${taken.name}".`);
  }
  const project: Project = {
    id: ulid(),
    key,
    name,
    status,
    lead,
    members,
    start,
    due,
    root,
    external: [],
  };
  hub.projects.push(project);
  hub.append(ctx.caller.memberId, { type: 'project_created', data: { project } });
  return created(project);
};

/** Creates a workstream: `active` and `on_track` unless the body says otherwise. */
const createWorkstream: Handler = (hub, ctx) => {
  const fields = new Fields(ctx.body);
  const projectId = fields.string('project');
  const name = fields.text('name');
  const status = fields.optEnum('status', WORKSTREAM_STATUSES) ?? 'active';
  const locations = (fields.optArray('locations') ?? []).map((value, i) =>
    knownLocation(hub, value, fields.name(`locations[${i}]`)),
  );
  // The contract makes an unknown project a 404 here, although it is in the body.
  const project = found(hub.findProject(projectId), `No project ${projectId}.`);
  const workstream: Workstream = {
    id: ulid(),
    project: project.id,
    name,
    status,
    health: 'on_track',
    locations,
    external: [],
  };
  hub.workstreams.push(workstream);
  hub.append(ctx.caller.memberId, { type: 'workstream_created', data: { workstream } });
  return created(workstream);
};

/** A location on a machine the workspace knows. */
function knownLocation(hub: Hub, value: unknown, where: string): Location {
  const location = readLocation(value, where);
  const machine = known(hub.findMachine(location.machine), `${where}.machine: no machine ${location.machine}.`);
  return { ...location, machine: machine.id };
}

/** Dates are `YYYY-MM-DD`, so text order is date order. */
function requireStartBeforeDue(start: string | undefined, due: string | undefined): void {
  if (start !== undefined && due !== undefined && start > due) {
    throw invalid(`start (${start}) must not be after due (${due}).`);
  }
}

const listWorkstreams: Handler = (hub, ctx) => {
  const project = queryId(ctx.query, 'project', 'prj');
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
    hub.append(ctx.caller.memberId, {
      type: 'workstream_changed',
      data: { workstream: workstream.id, ...next },
    });
  }
  return ok(workstream);
};

// ─── Tasks ──────────────────────────────────────────────────────────────────────────────────────

const listTasks: Handler = (hub, ctx) => {
  const project = queryId(ctx.query, 'project', 'prj');
  const workstream = queryId(ctx.query, 'workstream', 'wst');
  const assignee = queryId(ctx.query, 'assignee', 'mem');
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
  hub.append(ctx.caller.memberId, { type: 'task_created', data: { task } });
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
  hub.append(me.memberId, { type: 'task_moved', data: { task: task.id, from, to, mover } });
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
  assign(hub, ctx.caller.memberId, task, id === undefined ? undefined : memberRef(hub, id, 'assignee').id);
  return ok(task);
};

/** Sets the assignee and emits `task_assigned`, if it changes. */
function assign(hub: Hub, author: MemberId, task: Task, assignee: MemberId | undefined): void {
  if (task.assignee !== assignee) {
    task.assignee = assignee;
    hub.append(author, { type: 'task_assigned', data: { task: task.id, assignee } });
  }
}

const TITLE_MAX = 500;
const LABEL_MAX = 64;
const LABELS_MAX = 32;

/** Length in characters as Rust counts them (`chars().count()`): Unicode code points. */
const charCount = (text: string): number => [...text].length;

/**
 * Edits a task's fields (a `TaskPatch`). The whole patch is checked first; then only the fields
 * that change are written, and `task_updated` carries exactly those. A patch that changes nothing
 * returns the task and emits nothing.
 */
const patchTask: Handler = (hub, ctx) => {
  const task = taskAt(hub, ctx.param('id'));
  const patch = changedFields(task, readTaskPatch(hub, task, new Fields(ctx.body)));
  if (Object.keys(patch).length > 0) {
    applyPatch(task, patch);
    hub.append(ctx.caller.memberId, { type: 'task_updated', data: { task: task.id, patch } });
  }
  return ok(task);
};

/** The patch in the body, checked against the rules and normalised (trimmed, deduplicated). */
function readTaskPatch(hub: Hub, task: Task, fields: Fields): TaskPatch {
  const patch: TaskPatch = {};
  if (fields.isNull('workstream')) {
    patch.workstream = null;
  } else {
    const id = fields.optString('workstream');
    if (id !== undefined) {
      const workstream = known(hub.findWorkstream(id), `workstream: no workstream ${id}.`);
      if (workstream.project !== task.project) {
        throw invalid(`workstream "${workstream.name}" belongs to another project than ${task.key}.`);
      }
      patch.workstream = workstream.id;
    }
  }
  const title = fields.optString('title')?.trim();
  if (title !== undefined) {
    if (charCount(title) < 1 || charCount(title) > TITLE_MAX) {
      throw invalid(`title must be 1 to ${TITLE_MAX} characters after trimming.`);
    }
    patch.title = title;
  }
  const description = fields.optString('description');
  if (description !== undefined) {
    patch.description = description;
  }
  const priority = fields.optEnum('priority', PRIORITIES);
  if (priority !== undefined) {
    patch.priority = priority;
  }
  const labels = fields.optStringArray('labels');
  if (labels !== undefined) {
    patch.labels = readLabels(labels);
  }
  const start = fields.isNull('start') ? null : fields.optDate('start');
  const due = fields.isNull('due') ? null : fields.optDate('due');
  if (start !== undefined) {
    patch.start = start;
  }
  if (due !== undefined) {
    patch.due = due;
  }
  // The rule holds for the task as it will be, so a new start is checked against the old due.
  requireStartBeforeDue(
    start === undefined ? task.start : (start ?? undefined),
    due === undefined ? task.due : (due ?? undefined),
  );
  const blockers = fields.optStringArray('blocked_by');
  if (blockers !== undefined) {
    patch.blocked_by = readBlockers(hub, task, blockers);
  }
  if (fields.raw('accept_auto') !== undefined) {
    patch.accept_auto = fields.bool('accept_auto');
  }
  // Last, so a malformed body is a 400 even when it would also close a cycle.
  if (patch.blocked_by !== undefined) {
    requireNoCycle(hub, task, patch.blocked_by);
  }
  return patch;
}

/** Labels trimmed and deduplicated (first one wins), each 1–64 characters, at most 32. */
function readLabels(labels: string[]): string[] {
  const unique = [...new Set(labels.map((label) => label.trim()))];
  const bad = unique.find((label) => charCount(label) < 1 || charCount(label) > LABEL_MAX);
  if (bad !== undefined) {
    throw invalid(`Each label must be 1 to ${LABEL_MAX} characters after trimming; "${bad}" is not.`);
  }
  if (unique.length > LABELS_MAX) {
    throw invalid(`A task has at most ${LABELS_MAX} labels; this one would have ${unique.length}.`);
  }
  return unique;
}

/** Existing tasks other than `task`, deduplicated. */
function readBlockers(hub: Hub, task: Task, ids: string[]): TaskId[] {
  const blockers: TaskId[] = [];
  for (const [i, id] of ids.entries()) {
    const blocker = known(hub.findTaskById(id), `blocked_by[${i}]: no task ${id}.`);
    if (blocker.id === task.id) {
      throw invalid(`${task.key} cannot be blocked by itself.`);
    }
    if (!blockers.includes(blocker.id)) {
      blockers.push(blocker.id);
    }
  }
  return blockers;
}

/** A 409 if `task` waiting on `blockers` closes a cycle: some blocker already waits on `task`. */
function requireNoCycle(hub: Hub, task: Task, blockers: TaskId[]): void {
  for (const blocker of blockers) {
    const seen = new Set<TaskId>();
    const stack: TaskId[] = [blocker];
    for (let id = stack.pop(); id !== undefined; id = stack.pop()) {
      if (id === task.id) {
        const key = hub.findTaskById(blocker)?.key ?? blocker;
        throw conflict(`${key} already waits on ${task.key}, so ${task.key} cannot wait on it.`);
      }
      if (!seen.has(id)) {
        seen.add(id);
        stack.push(...(hub.findTaskById(id)?.blocked_by ?? []));
      }
    }
  }
}

/** The fields of `wanted` whose values differ from the task's (lists compared in order). */
function changedFields(task: Task, wanted: TaskPatch): TaskPatch {
  const sameList = (a: readonly string[], b: readonly string[]): boolean =>
    a.length === b.length && a.every((value, i) => value === b[i]);
  const patch: TaskPatch = {};
  if (wanted.workstream !== undefined && wanted.workstream !== (task.workstream ?? null)) {
    patch.workstream = wanted.workstream;
  }
  if (wanted.title !== undefined && wanted.title !== task.title) {
    patch.title = wanted.title;
  }
  if (wanted.description !== undefined && wanted.description !== task.description) {
    patch.description = wanted.description;
  }
  if (wanted.priority !== undefined && wanted.priority !== task.priority) {
    patch.priority = wanted.priority;
  }
  if (wanted.labels !== undefined && !sameList(wanted.labels, task.labels)) {
    patch.labels = wanted.labels;
  }
  if (wanted.start !== undefined && wanted.start !== (task.start ?? null)) {
    patch.start = wanted.start;
  }
  if (wanted.due !== undefined && wanted.due !== (task.due ?? null)) {
    patch.due = wanted.due;
  }
  if (wanted.blocked_by !== undefined && !sameList(wanted.blocked_by, task.blocked_by)) {
    patch.blocked_by = wanted.blocked_by;
  }
  if (wanted.accept_auto !== undefined && wanted.accept_auto !== task.accept_auto) {
    patch.accept_auto = wanted.accept_auto;
  }
  return patch;
}

/** `TaskPatch::apply`: `null` clears a field. */
function applyPatch(task: Task, patch: TaskPatch): void {
  if (patch.workstream !== undefined) {
    task.workstream = patch.workstream ?? undefined;
  }
  if (patch.title !== undefined) {
    task.title = patch.title;
  }
  if (patch.description !== undefined) {
    task.description = patch.description;
  }
  if (patch.priority !== undefined) {
    task.priority = patch.priority;
  }
  if (patch.labels !== undefined) {
    task.labels = [...patch.labels];
  }
  if (patch.start !== undefined) {
    task.start = patch.start ?? undefined;
  }
  if (patch.due !== undefined) {
    task.due = patch.due ?? undefined;
  }
  if (patch.blocked_by !== undefined) {
    task.blocked_by = [...patch.blocked_by];
  }
  if (patch.accept_auto !== undefined) {
    task.accept_auto = patch.accept_auto;
  }
}

const replaceSubtasks: Handler = (hub, ctx) => {
  const task = taskAt(hub, ctx.param('id'));
  requireOwnTask(hub, ctx.caller, task);
  if (!Array.isArray(ctx.body)) {
    throw invalid('The body must be an array of subtasks.');
  }
  const incoming = ctx.body.map((value, i) => readSubtask(hub, value, `[${i}]`));
  const me = ctx.caller.memberId;
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
    hub.append(ctx.caller.memberId, {
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
  const me = ctx.caller.memberId;
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
  const machine = queryId(ctx.query, 'machine', 'mch');
  const workstream = queryId(ctx.query, 'workstream', 'wst');
  const task = queryId(ctx.query, 'task', 'tsk');
  const states = queryEnums(ctx.query, 'state', SESSION_STATES);
  return ok(
    hub.sessions.filter(
      (s) =>
        includesSession(hub.importChoice, s) &&
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
  if (task !== undefined && workstream !== undefined && task.workstream !== workstream.id) {
    throw invalid(`${task.key} is not in workstream "${workstream.name}".`);
  }
  session.workstream = workstream?.id;
  session.task = task?.id;
  session.link_basis = 'manual';
  hub.append(ctx.caller.memberId, {
    type: 'session_linked',
    data: { session: session.id, workstream: workstream?.id, task: task?.id, basis: 'manual' },
  });
  return ok(session);
};

// ─── Asks ───────────────────────────────────────────────────────────────────────────────────────

const listAsks: Handler = (hub, ctx) => {
  const to = queryId(ctx.query, 'to', 'mem');
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
    from: me.memberId,
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
  hub.append(me.memberId, { type: 'ask_raised', data: { ask } });
  if (session !== undefined && session.agent === me.memberId) {
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
  const me = ctx.caller.memberId;
  if (ask.state !== 'open') {
    throw conflict(`This ask is already ${ask.state}.`);
  }
  const answer: Answer = { by: me, option, text, at: Date.now() };
  ask.state = 'answered';
  ask.answer = answer;
  hub.append(me, { type: 'ask_answered', data: { ask: ask.id, answer } });
  const session = ask.session === undefined ? undefined : hub.findSession(ask.session);
  if (session !== undefined) {
    resumeAfterAnswer(hub, session);
  }
  return ok(ask);
};

// ─── Briefs ─────────────────────────────────────────────────────────────────────────────────────

/**
 * A person's brief. Accepting the pending proposal unchanged (same text and next step) copies its
 * receipts into `brief_accepted`, and the brief stays the back office's; any other text is the
 * person's own, without receipts. "Keep current" is a PUT of the current text.
 */
const putBrief: Handler = (hub, ctx) => {
  const target = briefTarget(hub, ctx.param('kind'), ctx.param('id'));
  const fields = new Fields(ctx.body);
  const text = fields.string('text');
  const next = fields.optString('next');
  const pinned = fields.bool('pinned');
  const proposal = pendingProposal(hub, target);
  const acceptsProposal = proposal !== undefined && proposal.text === text && proposal.next === next;
  const receipts = acceptsProposal ? proposal.receipts : [];
  // Serde skips a `None` next and empty receipts, so the mock leaves them out too.
  const event = hub.append(ctx.caller.memberId, {
    type: 'brief_accepted',
    data: {
      target,
      text,
      ...(next === undefined ? {} : { next }),
      pinned,
      ...(receipts.length === 0 ? {} : { receipts }),
    },
  });
  const brief: Brief = {
    target,
    text,
    next,
    pinned,
    source: acceptsProposal ? 'back_office' : 'person',
    updated: event.at,
    receipts,
  };
  const index = hub.briefs.findIndex((b) => sameTarget(b.target, target));
  if (index === -1) {
    hub.briefs.push(brief);
  } else {
    hub.briefs[index] = brief;
  }
  // The brief just accepted is newer than any proposal, so nothing is pending.
  return ok(brief);
};

/** Every brief in force, each with its pending proposal when it has one. */
const listBriefs: Handler = (hub) =>
  ok(
    hub.briefs.map((brief) => {
      const proposal = pendingProposal(hub, brief.target);
      return proposal === undefined ? brief : { ...brief, proposal };
    }),
  );

const sameTarget = (a: BriefTarget, b: BriefTarget): boolean => a.kind === b.kind && a.id === b.id;

/**
 * The pending proposal: the newest `brief_proposed` for the target, if it is newer (a higher
 * revision) than the newest `brief_accepted`, which put the brief in force. A copy, so callers
 * may keep it.
 */
function pendingProposal(hub: Hub, target: BriefTarget): BriefProposal | undefined {
  for (let rev = hub.rev; rev >= 1; rev--) {
    const event = hub.eventAt(rev);
    const body = event?.body;
    if (event === undefined || body === undefined) {
      continue;
    }
    if (body.type === 'brief_accepted' && sameTarget(body.data.target, target)) {
      return undefined;
    }
    if (body.type === 'brief_proposed' && sameTarget(body.data.target, target)) {
      const { text, next, receipts } = structuredClone(body.data);
      return { text, ...(next === undefined ? {} : { next }), receipts, at: event.at };
    }
  }
  return undefined;
}

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
 *
 * With filters, one request examines at most `hub.scanWindow` revisions, as the contract allows, so
 * a page may hold fewer than `limit` events, even none. An empty page that is not at the start has
 * `to_rev` 0 and `from_rev` = the oldest revision examined, where the next request continues.
 * `at_start` is true only when the scan reached revision 1 without finding an older match.
 */
const listEvents: Handler = (hub, ctx) => {
  const limit = queryLimit(ctx.query, 100, 500);
  const before = queryInt(ctx.query, 'before', 0);
  const filter: EventFilter = {
    project: queryId(ctx.query, 'project', 'prj'),
    workstream: queryId(ctx.query, 'workstream', 'wst'),
    task: queryId(ctx.query, 'task', 'tsk'),
    session: queryId(ctx.query, 'session', 'ses'),
  };
  const filtered = Object.values(filter).some((v) => v !== undefined);
  let rev = Math.min(before ?? Infinity, hub.rev + 1) - 1;
  const floor = filtered ? Math.max(1, rev - hub.scanWindow + 1) : 1;
  const revs: number[] = [];
  for (; rev >= floor && revs.length <= limit; rev--) {
    const event = hub.eventAt(rev);
    if (event !== undefined && eventVisible(hub, event) && touches(hub, event.body, filter)) {
      revs.push(rev);
    }
  }
  // `rev` is now the newest revision not examined.
  const atStart = revs.length <= limit && rev < 1;
  const page = revs.slice(0, limit).reverse();
  return ok({
    events: page.map((r) => hub.eventAt(r)),
    revisions: page,
    from_rev: page[0] ?? (atStart ? 0 : rev + 1),
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
    case 'cursor_moved':
      return {};
    case 'session_discovered': {
      const { session } = body.data;
      return { session: session.id, task: session.task, workstream: session.workstream };
    }
    case 'session_state_changed':
    case 'turn_ended':
    case 'tool_ran':
    case 'file_edited':
    case 'session_ended':
    case 'session_updated':
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
    case 'task_updated':
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
    case 'machine_added':
    case 'member_added':
    case 'persona_saved':
    case 'team_saved':
      return {};
  }
}

// ─── Hooks ──────────────────────────────────────────────────────────────────────────────────────

const HOOK_EVENT = /^[A-Za-z][A-Za-z0-9_-]{0,63}$/;

/** Accepts a hook event from `pitcrew hook`. The mock checks its shape and otherwise ignores it. */
const receiveHook: Handler = (_hub, ctx) => {
  const engine = ctx.param('engine');
  if (!(ENGINES as readonly string[]).includes(engine)) {
    throw invalid(`Unknown engine "${engine}".`);
  }
  if (!HOOK_EVENT.test(ctx.param('event'))) {
    throw invalid('The hook event name is malformed.');
  }
  if (!isRecord(ctx.body)) {
    throw invalid('The body must be the hook payload, a JSON object.');
  }
  return { status: 202 };
};

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
  route('GET', '/v1/me/cursors', 'device', (hub, ctx) => ok(
    [...(hub.cursors.get(ctx.caller.memberId) ?? new Map<string, number>())]
      .sort(([a], [b]) => a.localeCompare(b)).map(([scope, rev]) => ({ scope, rev })),
  )),
  route('PUT', '/v1/me/cursors/:scope', 'device', (hub, ctx) => {
    const scope = ctx.param('scope');
    if (scope !== 'workspace') {
      const match = /^(project|workstream):([0-7][0-9A-HJKMNP-TV-Z]{25})$/.exec(scope);
      if (match === null) throw invalid('Invalid cursor scope.');
      const exists = match[1] === 'project' ? hub.findProject(match[2]!) : hub.findWorkstream(match[2]!);
      if (exists === undefined) throw notFound('Unknown cursor scope.');
    }
    if (!isRecord(ctx.body) || !Number.isSafeInteger(ctx.body['rev']) || typeof ctx.body['rev'] !== 'number' || ctx.body['rev'] < 0 || ctx.body['rev'] > hub.rev) {
      throw invalid('Revision must be an integer in the log.');
    }
    const cursors = hub.cursors.get(ctx.caller.memberId) ?? new Map<string, number>();
    const current = cursors.get(scope) ?? 0;
    const rev = Math.max(current, ctx.body['rev']);
    if (rev > current) {
      cursors.set(scope, rev);
      hub.cursors.set(ctx.caller.memberId, cursors);
      hub.append(ctx.caller.memberId, { type: 'cursor_moved', data: { scope, rev } });
    }
    return ok({ scope, rev });
  }),
  // Host and workspace (`GET /v1/host/info` is handled before auth, in `handleApi`).
  route('GET', '/v1/me', 'agent', (_hub, ctx) =>
    ok(found(ctx.caller.member, 'No member yet; set up the workspace first.')),
  ),
  route('GET', '/v1/workspace', 'device', (hub) =>
    ok({ workspace: hub.workspace, rev: hub.rev, ...(hub.setupNeeded ? { setup_needed: true } : {}) }),
  ),
  route('POST', '/v1/setup', 'device', setupHub),
  route('GET', '/v1/machines', 'device', (hub) => ok(hub.machines)),
  route('POST', '/v1/machines/:id/scan', 'device', (hub, ctx) => ({
    status: 200,
    stream: startScan(hub, ctx.param('id')),
  })),
  // Machine setup (machine-setup.ts).
  route('GET', '/v1/machines/:id/check', 'device', (hub, ctx) =>
    ok(checkMachine(hub, ctx.caller.memberId, ctx.param('id'), ctx.query)),
  ),
  route('GET', '/v1/machines/:id/agents', 'device', (hub, ctx) =>
    ok(agentAccounts(hub, ctx.caller.memberId, ctx.param('id'))),
  ),
  route('GET', '/v1/machines/:id/agents/:engine/sign-in', 'device', (hub, ctx) =>
    ok(signInStatus(hub, ctx.caller.memberId, ctx.param('id'), ctx.param('engine'))),
  ),
  route('POST', '/v1/machines/:id/agents/:engine/sign-in', 'device', (hub, ctx) =>
    startSignIn(hub, ctx.caller.memberId, ctx.param('id'), ctx.param('engine'), ctx.body),
  ),
  route('DELETE', '/v1/machines/:id/agents/:engine/sign-in', 'device', (hub, ctx) => {
    stopSignIn(hub, ctx.caller.memberId, ctx.param('id'), ctx.param('engine'));
    return noContent();
  }),
  route('GET', '/v1/members', 'agent', (hub) => ok(hub.members)),
  route('GET', '/v1/personas', 'device', (hub) => ok(hub.personas)),
  route('GET', '/v1/teams', 'device', (hub) => ok(hub.teams)),
  // Projects and workstreams.
  route('GET', '/v1/projects', 'device', (hub) => ok(hub.projects)),
  route('GET', '/v1/projects/:id', 'device', (hub, ctx) =>
    ok(found(hub.findProject(ctx.param('id')), `No project ${ctx.param('id')}.`)),
  ),
  route('POST', '/v1/projects', 'device', createProject),
  route('GET', '/v1/workstreams', 'device', listWorkstreams),
  route('GET', '/v1/workstreams/:id/files', 'device', (hub, ctx) => files(hub, ctx.param('id'), ctx.query, ctx.body, 'list')),
  route('GET', '/v1/workstreams/:id/files/content', 'device', (hub, ctx) => files(hub, ctx.param('id'), ctx.query, ctx.body, 'read')),
  route('PUT', '/v1/workstreams/:id/files/content', 'device', (hub, ctx) => files(hub, ctx.param('id'), ctx.query, ctx.body, 'write')),
  route('GET', '/v1/workstreams/:id', 'device', (hub, ctx) =>
    ok(found(hub.findWorkstream(ctx.param('id')), `No workstream ${ctx.param('id')}.`)),
  ),
  route('POST', '/v1/workstreams', 'device', createWorkstream),
  route('PATCH', '/v1/workstreams/:id', 'device', patchWorkstream),
  // Tasks.
  route('GET', '/v1/tasks', 'agent', listTasks),
  route('GET', '/v1/tasks/:id', 'agent', (hub, ctx) => ok(taskAt(hub, ctx.param('id')))),
  route('PATCH', '/v1/tasks/:id', 'device', patchTask),
  route('POST', '/v1/tasks', 'device', createTask),
  route('POST', '/v1/tasks/:id/move', 'agent', moveTask),
  route('POST', '/v1/tasks/:id/assign', 'device', assignTask),
  route('PUT', '/v1/tasks/:id/subtasks', 'agent', replaceSubtasks),
  route('POST', '/v1/tasks/:id/comments', 'agent', postComment),
  route('POST', '/v1/tasks/:id/dispatch', 'device', dispatchTask),
  // Sessions.
  route('GET', '/v1/import', 'device', (hub) => ok(hub.importChoice)),
  route('POST', '/v1/import/dry-run', 'device', (hub, ctx) => {
    const filter = parseImport(ctx.body);
    const count = hub.sessions.filter((s) => includesSession({ filter, committed_at: Date.now() }, s)).length;
    return ok({ count });
  }),
  route('PUT', '/v1/import', 'device', (hub, ctx) => {
    const filter = parseImport(ctx.body);
    hub.importChoice = { filter, committed_at: Date.now() };
    return ok({ imported: hub.sessions.filter((s) => includesSession(hub.importChoice, s)).length });
  }),
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
  route('GET', '/v1/briefs', 'device', listBriefs),
  route('PUT', '/v1/briefs/:kind/:id', 'device', putBrief),
  route('GET', '/v1/events', 'device', listEvents),
  route('GET', '/v1/activity', 'device', listEvents),
  // Recaps: from the fixture the recap engine wrote (recaps.ts).
  route('GET', '/v1/recaps/blocks', 'device', (hub, ctx) => ok(blocksPage(includedRecaps(hub), ctx.query))),
  route('GET', '/v1/recaps/days', 'device', (hub, ctx) => ok(daysPage(includedRecaps(hub), ctx.query))),
  route('POST', '/v1/hooks/:engine/:event', 'agent', receiveHook),
  route('GET', '/v1/stream', 'device', needsWebSocket),
];
