// Wire types from `crates/protocol`, written by hand for the mock hub.
//
// Field names are exactly the serde names. Enums are `snake_case` strings; each enum has a value
// list (`TASK_STATUSES`, …) so request bodies can be checked against it. Tagged unions keep their
// serde tags: `EventBody` is `{type, data}`; `Mover`, `Receipt`, `SubtaskSource`, `BriefTarget`,
// `BlockKey` and `TranscriptItem` are tagged by `kind`; `StreamFrame` and `FactKind` by `type`.
// Optional fields (`?`) are the Rust `Option`s that serde skips when empty.

// ─── Ids and scalars (ids.rs, model.rs) ─────────────────────────────────────────────────────────

/** A bare 26-character ULID. */
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
/** A project key such as `PAP`. */
export type ProjectKey = string;
/** A task key such as `PAP-4`. */
export type TaskKey = string;
/** Milliseconds since the Unix epoch, UTC. */
export type TimestampMs = number;
/** `Date` in Rust: `YYYY-MM-DD`, no time zone. */
export type CalendarDate = string;

// ─── Enums ──────────────────────────────────────────────────────────────────────────────────────

export const ENGINES = ['claude', 'codex', 'opencode'] as const;
export type Engine = (typeof ENGINES)[number];

export const MACHINE_KINDS = ['local', 'wsl', 'ssh'] as const;
export type MachineKind = (typeof MACHINE_KINDS)[number];

export const LIVENESSES = ['live', 'unverifiable', 'stopped'] as const;
export type Liveness = (typeof LIVENESSES)[number];

export const SCHEDULERS = ['slurm'] as const;
export type Scheduler = (typeof SCHEDULERS)[number];

export const MEMBER_KINDS = ['human', 'agent'] as const;
export type MemberKind = (typeof MEMBER_KINDS)[number];

export const PERMISSION_MODES = ['default', 'accept_edits', 'plan', 'bypass_permissions'] as const;
export type PermissionMode = (typeof PERMISSION_MODES)[number];

export const EXTERNAL_SYSTEMS = ['github', 'jira', 'linear', 'gitlab'] as const;
export type ExternalSystem = (typeof EXTERNAL_SYSTEMS)[number];

export const PROJECT_STATUSES = ['planning', 'in_progress', 'on_hold', 'completed'] as const;
export type ProjectStatus = (typeof PROJECT_STATUSES)[number];

export const WORKSTREAM_STATUSES = ['idea', 'active', 'paused', 'shipped', 'dropped'] as const;
export type WorkstreamStatus = (typeof WORKSTREAM_STATUSES)[number];

export const HEALTHS = ['on_track', 'at_risk', 'blocked'] as const;
export type Health = (typeof HEALTHS)[number];

export const TASK_STATUSES = ['backlog', 'todo', 'in_progress', 'review', 'done', 'canceled'] as const;
export type TaskStatus = (typeof TASK_STATUSES)[number];

export const PRIORITIES = ['urgent', 'high', 'medium', 'low', 'none'] as const;
export type Priority = (typeof PRIORITIES)[number];

export const SESSION_STATES = ['starting', 'working', 'waiting', 'idle', 'ended', 'unreachable'] as const;
export type SessionState = (typeof SESSION_STATES)[number];

export const LINK_BASES = ['dispatch', 'claimed', 'manual', 'folder', 'branch', 'imported'] as const;
export type LinkBasis = (typeof LINK_BASES)[number];

export const DISPATCH_OUTCOMES = ['succeeded', 'failed', 'canceled'] as const;
export type DispatchOutcome = (typeof DISPATCH_OUTCOMES)[number];

export const BRIEF_SOURCES = ['person', 'back_office'] as const;
export type BriefSource = (typeof BRIEF_SOURCES)[number];

export const ASK_KINDS = ['question', 'decision', 'review', 'approval', 'mention'] as const;
export type AskKind = (typeof ASK_KINDS)[number];

export const ASK_STATES = ['open', 'answered', 'withdrawn'] as const;
export type AskState = (typeof ASK_STATES)[number];

export const RECEIPT_KINDS = ['transcript', 'commit', 'pull_request', 'job', 'file', 'event'] as const;
export type ReceiptKind = (typeof RECEIPT_KINDS)[number];

export const PLAN_STATUSES = ['pending', 'in_progress', 'completed'] as const;
export type PlanStatus = (typeof PLAN_STATUSES)[number];

export const HOST_ROLES = ['hub', 'runner'] as const;
export type HostRole = (typeof HOST_ROLES)[number];

export const CAPABILITIES = ['tmux', 'pty', 'slurm', 'watch', 'scan'] as const;
export type Capability = (typeof CAPABILITIES)[number];

export const TOKEN_SCOPES = ['device', 'agent'] as const;
export type TokenScope = (typeof TOKEN_SCOPES)[number];

export const ERROR_CODES = [
  'unauthorized',
  'forbidden',
  'not_found',
  'conflict',
  'invalid',
  'unavailable',
  'too_large',
  'unsupported',
  'internal',
] as const;
export type ErrorCode = (typeof ERROR_CODES)[number];

/** Keys for `POST /v1/sessions/{id}/keys` (runner.rs `Key`). */
export const KEYS = ['enter', 'escape', 'tab', 'up', 'down', 'left', 'right', 'backspace', 'ctrl_c'] as const;
export type Key = (typeof KEYS)[number];

/** How to end a session (runner.rs `EndMode`). */
export const END_MODES = ['graceful', 'kill'] as const;
export type EndMode = (typeof END_MODES)[number];

// ─── Machines, members, personas, teams ─────────────────────────────────────────────────────────

export interface Workspace {
  id: WorkspaceId;
  name: string;
}

export interface MachineInfo {
  hostname: string;
  os: string;
  arch: string;
  has_tmux: boolean;
  scheduler?: Scheduler;
  home_on_network_fs: boolean;
}

export interface Machine {
  id: MachineId;
  name: string;
  kind: MachineKind;
  info?: MachineInfo;
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

// ─── Projects, workstreams, tasks ───────────────────────────────────────────────────────────────

export interface ExternalRef {
  system: ExternalSystem;
  key: string;
  url?: string;
}

export interface Project {
  id: ProjectId;
  key: ProjectKey;
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

/** Who is asking to move a task. */
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
 * `TaskPatch`: a partial update of a task. A field left out is unchanged; `null` clears
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

// ─── Sessions and dispatches ────────────────────────────────────────────────────────────────────

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

// ─── Asks, receipts, briefs ─────────────────────────────────────────────────────────────────────

export type Receipt =
  | { kind: 'transcript'; session: SessionId; offset: number }
  | { kind: 'commit'; repo: string; sha: string }
  | { kind: 'pull_request'; url: string }
  | { kind: 'job'; scheduler: Scheduler; id: string }
  | { kind: 'file'; location: Location }
  | { kind: 'event'; id: EventId };

/** Serde's adjacently tagged `{"kind": "project", "id": …}`. */
export type BriefTarget = { kind: 'project'; id: ProjectId } | { kind: 'workstream'; id: WorkstreamId };

export interface Brief {
  target: BriefTarget;
  text: string;
  next?: string;
  pinned: boolean;
  source: BriefSource;
  updated: TimestampMs;
  receipts: Receipt[];
  /** The pending proposal, present exactly when there is one. */
  proposal?: BriefProposal;
}

/** A proposed brief waiting for a person; `at` is the time of its `brief_proposed`. */
export interface BriefProposal {
  text: string;
  next?: string;
  receipts: Receipt[];
  at: TimestampMs;
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

// ─── Events (events.rs) ─────────────────────────────────────────────────────────────────────────

export interface Event {
  id: EventId;
  at: TimestampMs;
  workspace: WorkspaceId;
  author: MemberId;
  on_behalf_of?: MemberId;
  body: EventBody;
}

/** What happened. On the wire: `{"type": "task_moved", "data": {…}}`. */
export type EventBody =
  | { type: 'cursor_moved'; data: { scope: string; rev: number } }
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
  | { type: 'session_updated'; data: { session: SessionId; title?: string; branch?: string } }
  | { type: 'machine_added'; data: { machine: Machine } }
  | { type: 'member_added'; data: { member: Member } }
  | { type: 'persona_saved'; data: { persona: Persona } }
  | { type: 'team_saved'; data: { team: Team } }
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
  | {
      type: 'brief_proposed';
      data: { target: BriefTarget; text: string; next?: string; receipts: Receipt[] };
    }
  | {
      type: 'brief_accepted';
      data: { target: BriefTarget; text: string; next?: string; pinned: boolean; receipts?: Receipt[] };
    }
  | {
      type: 'decision_recorded';
      data: { workstream?: WorkstreamId; text: string; why?: string; receipts: Receipt[] };
    }
  | {
      type: 'board_draft_started';
      data: {
        draft: string;
        workstream: WorkstreamId;
        agent: MemberId;
        engine: Engine;
        session: SessionId;
        prompt: string;
        cost: DraftCost;
      };
    }
  | {
      type: 'board_proposed';
      data: { draft: string; workstream: WorkstreamId; tasks: ProposedTask[]; note?: string };
    }
  | {
      type: 'board_draft_reviewed';
      data: { draft: string; workstream: WorkstreamId; accepted: DraftedTask[]; rejected: number[] };
    };

// ─── Board drafts (board.rs) ─────────────────────────────────────────────────────────────────────

export interface UsageEstimate {
  input_tokens: number;
  output_tokens: number;
}

export interface DraftCost {
  sessions: number;
  sessions_left_out: number;
  tasks: number;
  summary_bytes: number;
  prompt_bytes: number;
  redacted: number;
  estimate: UsageEstimate;
}

export interface DraftPreview {
  workstream: WorkstreamId;
  prompt: string;
  cost: DraftCost;
  summary: string;
  digest: string;
}

export const DRAFT_STATES = ['running', 'proposed', 'reviewed', 'ended'] as const;
export type DraftState = (typeof DRAFT_STATES)[number];

export interface ProposedTask {
  title: string;
  status: TaskStatus;
  description?: string;
  evidence: SessionId[];
}

export interface BoardProposal {
  tasks: ProposedTask[];
  note?: string;
}

export interface DraftedTask {
  item: number;
  task: TaskId;
}

export interface BoardDraft {
  id: string;
  workstream: WorkstreamId;
  agent: MemberId;
  engine: Engine;
  session: SessionId;
  by: MemberId;
  prompt: string;
  cost: DraftCost;
  started: TimestampMs;
  state: DraftState;
  proposal?: BoardProposal;
  proposed?: TimestampMs;
  reviewed?: TimestampMs;
  accepted: DraftedTask[];
  rejected: number[];
}

// ─── API (api.rs) ───────────────────────────────────────────────────────────────────────────────

export interface HostInfo {
  name: string;
  version: string;
  protocol: number;
  protocol_min: number;
  roles: HostRole[];
  machine: MachineInfo;
  capabilities: Capability[];
}

export interface ApiError {
  code: ErrorCode;
  message: string;
}

/** Frames on `GET /v1/stream`. */
export type StreamFrame =
  | { type: 'hello'; rev: number; log: string }
  | { type: 'events'; from_rev: number; to_rev: number; events: Event[] }
  | { type: 'ping'; at: TimestampMs };

// ─── Transcripts (transcript.rs) ────────────────────────────────────────────────────────────────

export interface PlanItem {
  text: string;
  status: PlanStatus;
}

/** One meaningful thing in a transcript; `offset` is the byte offset of its record. */
export type TranscriptItem =
  | { kind: 'user_prompt'; at: TimestampMs; text: string; offset: number }
  | { kind: 'assistant_text'; at: TimestampMs; text: string; offset: number }
  | {
      kind: 'tool_use';
      at: TimestampMs;
      call_id: string;
      tool: string;
      target: string;
      input?: unknown;
      offset: number;
    }
  | {
      kind: 'tool_result';
      at: TimestampMs;
      call_id: string;
      is_error: boolean;
      summary: string;
      offset: number;
    }
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

export interface TranscriptPage {
  items: TranscriptItem[];
  from: number;
  to: number;
  at_start: boolean;
}

// ─── Recaps (recap.rs) ──────────────────────────────────────────────────────────────────────────

export const CHECKS = ['tests', 'lint', 'build'] as const;
export type Check = (typeof CHECKS)[number];

/** What a block groups. Serde's adjacently tagged `{"kind": "session", "id": …}`. */
export type BlockKey =
  | { kind: 'session'; id: SessionId }
  | { kind: 'workstream'; id: WorkstreamId }
  | { kind: 'project'; id: ProjectId };

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

export interface Fact {
  by: MemberId;
  at: TimestampMs;
  kind: FactKind;
  receipts: Receipt[];
}

/** A burst of work. `id` is its first event's id, `last` its last's. */
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
 * One clause and its evidence. `range` is a UTF-8 byte range of the summary's `text`, not a
 * JavaScript string index: convert before slicing.
 */
export interface Span {
  range: { start: number; end: number };
  receipts: Receipt[];
}

export interface Summary {
  text: string;
  spans: Span[];
}

export interface RecapBlock {
  block: Block;
  line: Summary;
}

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

export interface DaysPage {
  days: DayRecap[];
  at_start: boolean;
}

// ─── The fixture (crates/fixtures `DemoWorkspace`) ──────────────────────────────────────────────

export interface DemoWorkspace {
  workspace: Workspace;
  machines: Machine[];
  members: Member[];
  personas: Persona[];
  teams: Team[];
  projects: Project[];
  workstreams: Workstream[];
  tasks: Task[];
  sessions: Session[];
  dispatches: Dispatch[];
  asks: Ask[];
  briefs: Brief[];
  events: Event[];
}

/** crates/fixtures `DemoRecaps`: the demo's recaps, as the recap engine writes them. */
export interface DemoRecaps {
  /** Where its days begin, in minutes east of UTC. */
  tz: number;
  /** Every block with its line, by start then id. */
  blocks: RecapBlock[];
  /** Each project's days, by date; within a date, the one without a workstream first. */
  projects: { project: ProjectId; days: DayRecap[] }[];
}

// ─── The machine scan (crates/protocol `scan`) ─────────────────────────────────────────────────

export interface ScanProgress {
  scanned: number;
  total?: number;
  path?: string;
}

export interface ScanCounts {
  sessions: number;
  subagent_sessions: number;
  by_engine: { engine: Engine; count: number }[];
  by_home: { engine: Engine; home: string; count: number }[];
  by_folder: { path: string; count: number }[];
  /** `YYYY-MM`, most recent first. */
  by_month: { month: string; count: number }[];
  first_activity?: TimestampMs;
  last_activity?: TimestampMs;
}

export interface WorkstreamSuggestion {
  /** A folder's own path, or `<project path>#<branch>`. */
  id: string;
  name: string;
  branch?: string;
  session_count: number;
  recent_30d: number;
  recent_90d: number;
}

export interface Suggestion {
  id: string;
  name: string;
  path: string;
  is_git: boolean;
  session_count: number;
  recent_30d: number;
  recent_90d: number;
  workstreams: WorkstreamSuggestion[];
}

export interface ScanReport {
  counts: ScanCounts;
  suggestions: Suggestion[];
  unreadable: number;
}

/** One line of `POST /v1/machines/{id}/scan`'s answer. */
export type ScanFrame =
  | ({ type: 'progress' } & ScanProgress)
  | { type: 'done'; report: ScanReport }
  | { type: 'error'; code: ErrorCode; message: string };
