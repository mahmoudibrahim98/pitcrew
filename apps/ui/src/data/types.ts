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

/** `GET /v1/workspace`. */
export interface WorkspaceInfo {
  workspace: Workspace;
  /** The current event revision. */
  rev: number;
  /** `true` while the workspace has no person (a fresh hub); omitted means `false`. */
  setup_needed?: boolean;
}

/**
 * `POST /v1/setup`: the first run (api-v1.md, "The first run"). The three names are 1–80, 1–80
 * and 1–60 code points after trimming; the handle is `@` and 1–32 of `a-z 0-9 _ -`, not trimmed.
 * None may hold a control character.
 */
export interface Setup {
  workspace_name: string;
  person: { name: string; handle: string };
  machine_name: string;
}

/** `POST /v1/setup`'s answer: the person is the device token's member from now on. */
export interface SetupResult {
  workspace: Workspace;
  me: Member;
  machine: Machine;
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

/**
 * A new "Where it stands" the back office proposed, waiting for a person to accept it or keep the
 * current one (see `Brief.proposal`). `at` is the time of its `brief_proposed`.
 */
export interface BriefProposal {
  text: string;
  next?: string;
  receipts: Receipt[];
  at: TimestampMs;
}

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
  /**
   * The pending proposal, present exactly when there is one: the newest `brief_proposed` for the
   * target, newer than the `brief_accepted` that put this brief in force. Accepting it, or keeping
   * the current brief, clears it.
   */
  proposal?: BriefProposal;
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

// ─── Recaps (`crates/protocol/src/recap.rs`) ───────────────────────────────────────────────────
//
// `GET /v1/recaps/blocks` and `GET /v1/recaps/days` (API v1, "Recaps"). Derived from the event
// log, never stored; see `src/data/recaps.ts` for `clauses()` and the hooks.

export const CHECKS = ['tests', 'lint', 'build'] as const;
export type Check = (typeof CHECKS)[number];

/** What a block groups. Serde's adjacently tagged `{"kind": "session", "id": …}`. */
export type BlockKey =
  | { kind: 'session'; id: SessionId }
  | { kind: 'workstream'; id: WorkstreamId }
  | { kind: 'project'; id: ProjectId };

/** Counts over all of a block's events. Never capped. */
export interface Counts {
  events: number;
  tools_run: number;
  tools_failed: number;
  file_edits: number;
  lines_added: number;
  lines_removed: number;
  turns: number;
  asks_raised: number;
  asks_answered: number;
  task_moves: number;
  comments: number;
}

/** One file edited in a block. */
export interface FileTouch {
  path: string;
  edits: number;
  added: number;
  removed: number;
  receipts: Receipt[];
}

/** What a fact says, tagged by `type` (internally, so the fields sit beside it). */
export type FactKind =
  | { type: 'session_started'; title?: string }
  | { type: 'session_linked'; workstream?: WorkstreamId; task?: TaskId }
  | { type: 'session_waiting'; status_line?: string }
  | { type: 'session_ended' }
  | { type: 'dispatch_started'; task: TaskId; agent: MemberId }
  | { type: 'dispatch_finished'; task?: TaskId; outcome: DispatchOutcome; summary?: string }
  | { type: 'task_created'; task: TaskId }
  | { type: 'task_moved'; task: TaskId; from: TaskStatus; to: TaskStatus }
  | { type: 'task_assigned'; task: TaskId; assignee?: MemberId }
  | { type: 'plan_updated'; task: TaskId; done: number; total: number }
  | { type: 'checks'; check: Check; runs: number; failures: number; last_failed: boolean }
  | { type: 'job_diverged'; jobs: string[] }
  | { type: 'ask_raised'; ask: AskId; ask_kind: AskKind; to: MemberId; title: string }
  | { type: 'ask_answered'; ask: AskId }
  | { type: 'commented'; task?: TaskId; workstream?: WorkstreamId; mentions: MemberId[] }
  | { type: 'decision_recorded'; text: string }
  | { type: 'workstream_created'; workstream: WorkstreamId }
  | { type: 'workstream_changed'; workstream: WorkstreamId; status: WorkstreamStatus; health: Health }
  | { type: 'brief_accepted'; target: BriefTarget; pinned: boolean };

/** A notable fact, with the evidence for it. */
export interface Fact {
  by: MemberId;
  at: TimestampMs;
  kind: FactKind;
  receipts: Receipt[];
}

/** A burst of one session's (or workstream's/project's) work. `id` is its first event's id, `last` its last's. */
export interface Block {
  id: EventId;
  last: EventId;
  key: BlockKey;
  start: TimestampMs;
  end: TimestampMs;
  session?: SessionId;
  workstream?: WorkstreamId;
  project?: ProjectId;
  tasks: TaskId[];
  agent?: MemberId;
  actors: MemberId[];
  counts: Counts;
  files: FileTouch[];
  files_omitted: number;
  facts: Fact[];
  facts_omitted: number;
  tool_receipts: Receipt[];
  turn_receipts: Receipt[];
}

/**
 * One clause of a summary and its evidence. `range` is a **UTF-8 byte range** of the summary's
 * `text`, on character boundaries — not a JavaScript string index. Use `clauses()` in
 * `src/data/recaps.ts` rather than slicing `text` with it directly.
 */
export interface Span {
  range: { start: number; end: number };
  receipts: Receipt[];
}

/** Text whose every clause is a span with receipts; the text between spans is only punctuation. */
export interface Summary {
  text: string;
  spans: Span[];
}

/** A block with its one-line summary, e.g. "@writer edited method.tex (+84 −12)". */
export interface RecapBlock {
  block: Block;
  line: Summary;
}

/** `GET /v1/recaps/blocks`: a page of blocks, newest first by block id. Only `at_start` ends paging. */
export interface BlocksPage {
  blocks: RecapBlock[];
  at_start: boolean;
}

/** One workstream's day; `workstream` is absent for a project's work outside any workstream. */
export interface DayRecap {
  workstream?: WorkstreamId;
  date: CalendarDate;
  blocks: EventId[];
  summary: Summary;
}

/** `GET /v1/recaps/days`: a page of day paragraphs, newest date first. Only `at_start` ends paging. */
export interface DaysPage {
  days: DayRecap[];
  at_start: boolean;
}

/** `GET /v1/recaps/blocks` filters: a block must match every one given. */
export interface RecapBlockFilters {
  session?: SessionId;
  task?: TaskId;
  workstream?: WorkstreamId;
  project?: ProjectId;
}

/** `GET /v1/recaps/days`: exactly one of `workstream` or `project`. */
export type RecapDayScope = { workstream: WorkstreamId } | { project: ProjectId };

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
  | { type: 'cursor_moved'; data: { scope: string; rev: number } }
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
  | { type: 'brief_proposed'; data: { target: BriefTarget; text: string; next?: string; receipts: Receipt[] } }
  | {
      type: 'brief_accepted';
      data: { target: BriefTarget; text: string; next?: string; pinned: boolean; receipts?: Receipt[] };
    }
  | {
      type: 'decision_recorded';
      data: { workstream?: WorkstreamId; text: string; why?: string; receipts: Receipt[] };
    };

export type EventType = EventBody['type'];

/** Every `EventBody` type, so tests and the invalidation map can check they cover them all. */
export const EVENT_TYPES = [
  'cursor_moved',
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

/** `POST /v1/projects`. The hub assigns `id`; `external` starts empty. */
export interface NewProject {
  /** `ProjectKey`: 2 to 10 characters, an uppercase letter, then uppercase letters or digits. */
  key: string;
  name: string;
  /** Defaults to the caller; always a member (put first when `members` leaves it out). */
  lead?: MemberId;
  /** Defaults to the lead alone. Duplicates are dropped. */
  members?: MemberId[];
  /** Defaults to `in_progress`. */
  status?: ProjectStatus;
  /** `start` must not be after `due` when both are set. */
  start?: CalendarDate;
  due?: CalendarDate;
  root?: Location;
}

/** `POST /v1/workstreams`. The hub assigns `id`; `health` starts `on_track`, `external` empty. */
export interface NewWorkstream {
  project: ProjectId;
  name: string;
  /** Defaults to `active`. */
  status?: WorkstreamStatus;
  locations?: Location[];
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
