// Wire types from `crates/protocol`, written by hand until generated types exist.
//
// Field names are exactly the serde names; enums are `snake_case` strings; optional fields are the
// Rust `Option`s serde skips when empty. Only what the UI uses so far is here; add more as needed,
// following `docs/build/contracts/api-v1.md`.

export type Ulid = string;
export type WorkspaceId = Ulid;
export type MachineId = Ulid;
export type MemberId = Ulid;
export type PersonaId = Ulid;
export type ProjectId = Ulid;
export type WorkstreamId = Ulid;
export type TaskId = Ulid;
export type SubtaskId = Ulid;
export type SessionId = Ulid;
export type DispatchId = Ulid;
export type AskId = Ulid;
export type EventId = Ulid;
export type TerminalId = Ulid;
/** A task key such as `PAP-4`. */
export type TaskKey = string;
/** Milliseconds since the Unix epoch, UTC. */
export type TimestampMs = number;
/** `YYYY-MM-DD`, no time zone. */
export type CalendarDate = string;

export type Engine = 'claude' | 'codex' | 'opencode';
export type MachineKind = 'local' | 'wsl' | 'ssh';
export type Liveness = 'live' | 'unverifiable' | 'stopped';
export type MemberKind = 'human' | 'agent';
export type ExternalSystem = 'github' | 'jira' | 'linear' | 'gitlab';
export type ProjectStatus = 'planning' | 'in_progress' | 'on_hold' | 'completed';
export type WorkstreamStatus = 'idea' | 'active' | 'paused' | 'shipped' | 'dropped';
export type Health = 'on_track' | 'at_risk' | 'blocked';
export type TaskStatus = 'backlog' | 'todo' | 'in_progress' | 'review' | 'done' | 'canceled';
export type Priority = 'urgent' | 'high' | 'medium' | 'low' | 'none';
export type SessionState = 'starting' | 'working' | 'waiting' | 'idle' | 'ended' | 'unreachable';
export type LinkBasis = 'dispatch' | 'claimed' | 'manual' | 'folder' | 'branch' | 'imported';
export type DispatchOutcome = 'succeeded' | 'failed' | 'canceled';
export type AskKind = 'question' | 'decision' | 'review' | 'approval' | 'mention';
export type AskState = 'open' | 'answered' | 'withdrawn';
export type Scheduler = 'slurm';
export type ErrorCode =
  | 'unauthorized'
  | 'forbidden'
  | 'not_found'
  | 'conflict'
  | 'invalid'
  | 'unavailable'
  | 'internal';

export interface Workspace {
  id: WorkspaceId;
  name: string;
}

export interface Machine {
  id: MachineId;
  name: string;
  kind: MachineKind;
  liveness: Liveness;
}

export interface Location {
  machine: MachineId;
  path: string;
  branch?: string;
}

export interface Member {
  id: MemberId;
  kind: MemberKind;
  handle: string;
  name: string;
  owner?: MemberId;
  persona?: PersonaId;
}

export interface ExternalRef {
  system: ExternalSystem;
  key: string;
  url?: string;
}

export interface Project {
  id: ProjectId;
  key: string;
  name: string;
  status: ProjectStatus;
  lead: MemberId;
  members: MemberId[];
  start?: CalendarDate;
  due?: CalendarDate;
  root?: Location;
  external: ExternalRef[];
}

export interface Workstream {
  id: WorkstreamId;
  project: ProjectId;
  name: string;
  status: WorkstreamStatus;
  health: Health;
  locations: Location[];
  external: ExternalRef[];
}

export type Mover =
  | { kind: 'person' }
  | { kind: 'agent'; on_own_task: boolean }
  | { kind: 'back_office'; accept_auto: boolean }
  | { kind: 'sync' };

export type SubtaskSource = { kind: 'human' } | { kind: 'agent_plan'; agent: MemberId };

export interface Subtask {
  id: SubtaskId;
  text: string;
  done: boolean;
  source: SubtaskSource;
}

export interface Task {
  id: TaskId;
  key: TaskKey;
  project: ProjectId;
  workstream?: WorkstreamId;
  title: string;
  description: string;
  status: TaskStatus;
  priority: Priority;
  assignee?: MemberId;
  labels: string[];
  start?: CalendarDate;
  due?: CalendarDate;
  blocked_by: TaskId[];
  source?: ExternalRef;
  accept_auto: boolean;
  subtasks: Subtask[];
}

export interface Session {
  id: SessionId;
  engine: Engine;
  native_id: string;
  machine: MachineId;
  cwd: string;
  branch?: string;
  title?: string;
  agent?: MemberId;
  workstream?: WorkstreamId;
  task?: TaskId;
  link_basis?: LinkBasis;
  state: SessionState;
  status_line?: string;
  started: TimestampMs;
  last_activity: TimestampMs;
  terminal?: TerminalId;
}

export interface Dispatch {
  id: DispatchId;
  task: TaskId;
  agent: MemberId;
  session?: SessionId;
  brief: string;
  started: TimestampMs;
  ended?: TimestampMs;
  outcome?: DispatchOutcome;
  summary?: string;
}

export type Receipt =
  | { kind: 'transcript'; session: SessionId; offset: number }
  | { kind: 'commit'; repo: string; sha: string }
  | { kind: 'pull_request'; url: string }
  | { kind: 'job'; scheduler: Scheduler; id: string }
  | { kind: 'file'; location: Location }
  | { kind: 'event'; id: EventId };

export type BriefTarget = { kind: 'project'; id: ProjectId } | { kind: 'workstream'; id: WorkstreamId };

export interface Answer {
  by: MemberId;
  option?: number;
  text?: string;
  at: TimestampMs;
}

export interface Ask {
  id: AskId;
  kind: AskKind;
  from: MemberId;
  to: MemberId;
  task?: TaskId;
  session?: SessionId;
  title: string;
  body: string;
  options: string[];
  receipts: Receipt[];
  state: AskState;
  answer?: Answer;
  created: TimestampMs;
}

export interface Event {
  id: EventId;
  at: TimestampMs;
  workspace: WorkspaceId;
  author: MemberId;
  on_behalf_of?: MemberId;
  body: EventBody;
}

/** On the wire: `{"type": "task_moved", "data": {…}}`. */
export type EventBody =
  | { type: 'machine_liveness'; data: { machine: MachineId; liveness: Liveness } }
  | { type: 'session_discovered'; data: { session: Session } }
  | {
      type: 'session_state_changed';
      data: { session: SessionId; from: SessionState; to: SessionState; status_line?: string };
    }
  | { type: 'turn_ended'; data: { session: SessionId; receipt: Receipt } }
  | {
      type: 'tool_ran';
      data: {
        session: SessionId;
        tool: string;
        target: string;
        outcome: string;
        failed: boolean;
        receipt: Receipt;
      };
    }
  | { type: 'file_edited'; data: { session: SessionId; path: string; added: number; removed: number } }
  | {
      type: 'session_linked';
      data: { session: SessionId; workstream?: WorkstreamId; task?: TaskId; basis: LinkBasis };
    }
  | { type: 'session_ended'; data: { session: SessionId } }
  | { type: 'project_created'; data: { project: Project } }
  | { type: 'workstream_created'; data: { workstream: Workstream } }
  | {
      type: 'workstream_changed';
      data: { workstream: WorkstreamId; status: WorkstreamStatus; health: Health };
    }
  | { type: 'task_created'; data: { task: Task } }
  | { type: 'task_moved'; data: { task: TaskId; from: TaskStatus; to: TaskStatus; mover: Mover } }
  | { type: 'task_assigned'; data: { task: TaskId; assignee?: MemberId } }
  | { type: 'subtasks_replaced'; data: { task: TaskId; subtasks: Subtask[] } }
  | { type: 'dispatch_started'; data: { dispatch: Dispatch } }
  | {
      type: 'dispatch_finished';
      data: { dispatch: DispatchId; outcome: DispatchOutcome; summary?: string };
    }
  | { type: 'ask_raised'; data: { ask: Ask } }
  | { type: 'ask_answered'; data: { ask: AskId; answer: Answer } }
  | {
      type: 'comment_posted';
      data: { task?: TaskId; workstream?: WorkstreamId; text: string; mentions: MemberId[] };
    }
  | { type: 'brief_proposed'; data: { target: BriefTarget; text: string; receipts: Receipt[] } }
  | { type: 'brief_accepted'; data: { target: BriefTarget; text: string; pinned: boolean } }
  | {
      type: 'decision_recorded';
      data: { workstream?: WorkstreamId; text: string; why?: string; receipts: Receipt[] };
    };

export type EventType = EventBody['type'];

/** Every `EventBody` type, so tests and the invalidation map can check they cover them all. */
export const EVENT_TYPES = [
  'machine_liveness',
  'session_discovered',
  'session_state_changed',
  'turn_ended',
  'tool_ran',
  'file_edited',
  'session_linked',
  'session_ended',
  'project_created',
  'workstream_created',
  'workstream_changed',
  'task_created',
  'task_moved',
  'task_assigned',
  'subtasks_replaced',
  'dispatch_started',
  'dispatch_finished',
  'ask_raised',
  'ask_answered',
  'comment_posted',
  'brief_proposed',
  'brief_accepted',
  'decision_recorded',
] as const satisfies readonly EventType[];

// Fails to compile if EVENT_TYPES misses a type of EventBody.
type Missing = Exclude<EventType, (typeof EVENT_TYPES)[number]>;
export const EVENT_TYPES_COMPLETE: [Missing] extends [never] ? true : Missing = true;

export interface ApiErrorBody {
  code: ErrorCode;
  message: string;
}

export type StreamFrame =
  | { type: 'hello'; rev: number; log: string }
  | { type: 'events'; from_rev: number; to_rev: number; events: Event[] }
  | { type: 'ping'; at: TimestampMs };

export interface TaskFilters {
  project?: ProjectId;
  workstream?: WorkstreamId;
  assignee?: MemberId;
  status?: TaskStatus[];
}

export interface SessionFilters {
  machine?: MachineId;
  workstream?: WorkstreamId;
  task?: TaskId;
  state?: SessionState;
}

export interface AskFilters {
  to?: MemberId;
  state?: AskState;
}
