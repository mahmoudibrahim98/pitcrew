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
export type TeamId = Ulid;
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
export type PermissionMode = 'default' | 'accept_edits' | 'plan' | 'bypass_permissions';
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

/** A reusable recipe for new agents. */
export interface Persona {
  id: PersonaId;
  name: string;
  engine: Engine;
  model?: string;
  instructions?: string;
  permission_mode: PermissionMode;
}

export interface Team {
  id: TeamId;
  name: string;
  lead: MemberId;
  members: MemberId[];
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

/**
 * A partial update of a task (`TaskPatch`). A field left out is unchanged; `null` clears
 * `workstream`, `start` and `due`. In `task_updated` it holds only the fields that changed.
 */
export interface TaskPatch {
  workstream?: WorkstreamId | null;
  title?: string;
  description?: string;
  priority?: Priority;
  labels?: string[];
  start?: CalendarDate | null;
  due?: CalendarDate | null;
  blocked_by?: TaskId[];
  accept_auto?: boolean;
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
  /** For a sub-agent's session, the session that started it. */
  parent?: SessionId;
}

// ─── Transcripts (`crates/protocol/src/transcript.rs`) and session control (`runner.rs`) ─────────

export type PlanStatus = 'pending' | 'in_progress' | 'completed';

export interface PlanItem {
  text: string;
  status: PlanStatus;
}

/** Tagged by `kind`. Every item carries the byte offset of the record it came from. */
export type TranscriptItem =
  | { kind: 'user_prompt'; at: TimestampMs; text: string; offset: number }
  | { kind: 'assistant_text'; at: TimestampMs; text: string; offset: number }
  | { kind: 'tool_use'; at: TimestampMs; call_id: string; tool: string; target: string; input?: unknown; offset: number }
  | { kind: 'tool_result'; at: TimestampMs; call_id: string; is_error: boolean; summary: string; offset: number }
  | {
      kind: 'file_edit';
      at: TimestampMs;
      path: string;
      added: number;
      removed: number;
      diff?: string;
      offset: number;
    }
  | { kind: 'plan_updated'; at: TimestampMs; items: PlanItem[]; offset: number }
  | { kind: 'question'; at: TimestampMs; text: string; options: string[]; offset: number }
  | { kind: 'turn_ended'; at: TimestampMs; offset: number };

export type TranscriptKind = TranscriptItem['kind'];

/** One kind of item: `TranscriptItemOf<'tool_use'>`. */
export type TranscriptItemOf<K extends TranscriptKind> = Extract<TranscriptItem, { kind: K }>;

/** Every kind this build knows. The Rust enum is `#[non_exhaustive]`: skip kinds not listed. */
export const TRANSCRIPT_KINDS = [
  'user_prompt',
  'assistant_text',
  'tool_use',
  'tool_result',
  'file_edit',
  'plan_updated',
  'question',
  'turn_ended',
] as const satisfies readonly TranscriptKind[];

/**
 * `GET /v1/sessions/{id}/transcript`, tail-first: items oldest first within the page; pass `from`
 * as `before` for the previous page. `at_start` is true when nothing older exists.
 */
export interface TranscriptPage {
  items: TranscriptItem[];
  from: number;
  to: number;
  at_start: boolean;
}

export interface TranscriptQuery {
  /** A byte offset: the page ending before it. Absent for the newest page. */
  before?: number | undefined;
  /** Items; default 200, at most 1000. A page holds whole records, so it may exceed this. */
  limit?: number | undefined;
}

/** `POST /v1/sessions/{id}/keys`. */
export type Key = 'enter' | 'escape' | 'tab' | 'up' | 'down' | 'left' | 'right' | 'backspace' | 'ctrl_c';

/** `POST /v1/sessions/{id}/end`. */
export type EndMode = 'graceful' | 'kill';

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

export type BriefSource = 'person' | 'back_office';

/** "Where it stands" for a project or workstream, as in force. */
export interface Brief {
  target: BriefTarget;
  text: string;
  next?: string;
  /** Pinned by a person; the back office may then only propose changes. */
  pinned: boolean;
  source: BriefSource;
  updated: TimestampMs;
  receipts: Receipt[];
}

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
  | { type: 'machine_added'; data: { machine: Machine } }
  | { type: 'member_added'; data: { member: Member } }
  | { type: 'persona_saved'; data: { persona: Persona } }
  | { type: 'team_saved'; data: { team: Team } }
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
  | {
      type: 'file_edited';
      data: { session: SessionId; path: string; added: number; removed: number; receipt?: Receipt };
    }
  | { type: 'session_updated'; data: { session: SessionId; title?: string; branch?: string } }
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
  | { type: 'task_updated'; data: { task: TaskId; patch: TaskPatch } }
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
  'machine_added',
  'member_added',
  'persona_saved',
  'team_saved',
  'machine_liveness',
  'session_discovered',
  'session_state_changed',
  'turn_ended',
  'tool_ran',
  'file_edited',
  'session_updated',
  'session_linked',
  'session_ended',
  'project_created',
  'workstream_created',
  'workstream_changed',
  'task_created',
  'task_moved',
  'task_assigned',
  'task_updated',
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

// ─── Request bodies and pages ────────────────────────────────────────────────────────────────────

/**
 * `GET /v1/events`: events oldest first. `from_rev` and `to_rev` are the revisions of the first
 * and last events; an empty page that is not at the start has `to_rev` 0 and `from_rev` where the
 * hub's scan stopped. Only `at_start` ends paging: pass `from_rev` as `before` for older events.
 */
export interface EventsPage {
  events: Event[];
  from_rev: number;
  to_rev: number;
  at_start: boolean;
}

/** The name stream N's components use for an `EventsPage`. */
export type ActivityPage = EventsPage;

/**
 * Filters for `GET /v1/events`. They match events that name the entity directly. The real hub
 * answers `400 invalid` to `project` and `workstream` until it has an index for them (the mock
 * accepts them).
 */
export interface EventFilters {
  project?: ProjectId;
  workstream?: WorkstreamId;
  task?: TaskId;
  session?: SessionId;
}

export interface EventsQuery extends EventFilters {
  /** An exclusive revision: events before it. Absent for the newest page. */
  before?: number;
  /** Default 100, at most 500. */
  limit?: number;
}

/** `POST /v1/tasks`. The hub assigns the id and the next key in the project. */
export interface NewTask {
  project: ProjectId;
  workstream?: WorkstreamId;
  title: string;
  description?: string;
  /** Default `todo`. */
  status?: TaskStatus;
  priority?: Priority;
  assignee?: MemberId;
  labels?: string[];
  due?: CalendarDate;
}

/** `POST /v1/tasks/{id}/comments`. */
export interface NewComment {
  text: string;
  mentions: MemberId[];
}

/** `POST /v1/tasks/{id}/dispatch`. The machine and folder default to the workstream's. */
export interface DispatchRequest {
  agent: MemberId;
  brief?: string;
  machine?: MachineId;
}

/** `POST /v1/asks/{id}/answer`: an option, a text, or both. */
export interface AskAnswer {
  option?: number;
  text?: string;
}

/** `PUT /v1/briefs/{kind}/{id}`: a person's version (`source: person`). */
export interface BriefEdit {
  text: string;
  next?: string;
  pinned: boolean;
}
