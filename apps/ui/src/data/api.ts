// The HTTP client for API v1. Everything the UI asks the daemon goes through here.

import type {
  ApiErrorBody,
  Ask,
  AskAnswer,
  AskFilters,
  Brief,
  BriefEdit,
  BriefTarget,
  Dispatch,
  DispatchRequest,
  EndMode,
  ErrorCode,
  Event,
  EventsPage,
  EventsQuery,
  Key,
  Machine,
  Member,
  MemberId,
  NewComment,
  NewTask,
  Persona,
  Project,
  Session,
  SessionFilters,
  Subtask,
  Task,
  TaskFilters,
  TaskStatus,
  Team,
  TranscriptPage,
  TranscriptQuery,
  Workspace,
  Workstream,
} from './types.ts';

const ERROR_CODES: readonly ErrorCode[] = [
  'unauthorized',
  'forbidden',
  'not_found',
  'conflict',
  'invalid',
  'unavailable',
  'internal',
];

/** For error bodies that do not follow the contract (a proxy, a crash page). */
const CODE_BY_STATUS: Partial<Record<number, ErrorCode>> = {
  400: 'invalid',
  401: 'unauthorized',
  403: 'forbidden',
  404: 'not_found',
  409: 'conflict',
  503: 'unavailable',
};

/** A failed request. `status` is 0 when the hub could not be reached at all. */
export class ApiError extends Error {
  readonly code: ErrorCode;
  readonly status: number;

  constructor(code: ErrorCode, message: string, status: number) {
    super(message);
    this.name = 'ApiError';
    this.code = code;
    this.status = status;
  }
}

function isErrorBody(value: unknown): value is ApiErrorBody {
  if (typeof value !== 'object' || value === null) {
    return false;
  }
  const { code, message } = value as Record<string, unknown>;
  return typeof message === 'string' && ERROR_CODES.includes(code as ErrorCode);
}

/** Turns a non-2xx response into an `ApiError`, trusting the body only if it has the contract's shape. */
export async function errorFromResponse(res: Response): Promise<ApiError> {
  let body: unknown;
  try {
    body = await res.json();
  } catch {
    body = undefined;
  }
  if (isErrorBody(body)) {
    return new ApiError(body.code, body.message, res.status);
  }
  const code = CODE_BY_STATUS[res.status] ?? 'internal';
  return new ApiError(code, `HTTP ${res.status} ${res.statusText}`.trim(), res.status);
}

export interface ApiOptions {
  /** For example `http://127.0.0.1:47317`, without a trailing slash. */
  baseUrl: string;
  /** Sent as `Authorization: Bearer`. Absent in the desktop app, whose gateway adds it. */
  token?: string | undefined;
  fetch?: typeof fetch;
}

type Query = Record<string, string | readonly string[] | undefined>;

function queryString(query: Query | undefined): string {
  if (query === undefined) {
    return '';
  }
  const params = new URLSearchParams();
  for (const [name, value] of Object.entries(query)) {
    if (value === undefined) continue;
    for (const one of typeof value === 'string' ? [value] : value) {
      params.append(name, one);
    }
  }
  const text = params.toString();
  return text === '' ? '' : `?${text}`;
}

export function createApi(options: ApiOptions) {
  const baseUrl = options.baseUrl.replace(/\/+$/, '');
  const doFetch = options.fetch ?? globalThis.fetch.bind(globalThis);

  async function request<T>(
    method: string,
    path: string,
    init: { query?: Query; body?: unknown; signal?: AbortSignal | undefined } = {},
  ): Promise<T> {
    const headers: Record<string, string> = { Accept: 'application/json' };
    if (options.token !== undefined) {
      headers.Authorization = `Bearer ${options.token}`;
    }
    if (init.body !== undefined) {
      headers['Content-Type'] = 'application/json';
    }
    let res: Response;
    try {
      res = await doFetch(`${baseUrl}${path}${queryString(init.query)}`, {
        method,
        headers,
        body: init.body === undefined ? null : JSON.stringify(init.body),
        signal: init.signal ?? null,
      });
    } catch (cause) {
      if (cause instanceof DOMException && cause.name === 'AbortError') {
        throw cause;
      }
      throw new ApiError('unavailable', `Cannot reach ${baseUrl}`, 0);
    }
    if (!res.ok) {
      throw await errorFromResponse(res);
    }
    if (res.status === 204) {
      return undefined as T;
    }
    try {
      return (await res.json()) as T;
    } catch {
      throw new ApiError('internal', `${method} ${path} answered ${res.status} without JSON`, res.status);
    }
  }

  const get = <T>(path: string, query?: Query, signal?: AbortSignal) =>
    request<T>('GET', path, query === undefined ? { signal } : { query, signal });
  const id = encodeURIComponent;
  const number = (value: number | undefined) => (value === undefined ? undefined : String(value));

  const briefs = (signal?: AbortSignal) => get<Brief[]>('/v1/briefs', undefined, signal);
  /** Also accepts a back-office proposal, and pins or unpins: every person's change is an edit. */
  const editBrief = (target: BriefTarget, edit: BriefEdit) =>
    request<Brief>('PUT', `/v1/briefs/${target.kind}/${id(target.id)}`, { body: edit });

  return {
    baseUrl,
    request,
    me: (signal?: AbortSignal) => get<Member>('/v1/me', undefined, signal),
    workspace: (signal?: AbortSignal) =>
      get<{ workspace: Workspace; rev: number }>('/v1/workspace', undefined, signal),
    machines: (signal?: AbortSignal) => get<Machine[]>('/v1/machines', undefined, signal),
    members: (signal?: AbortSignal) => get<Member[]>('/v1/members', undefined, signal),
    personas: (signal?: AbortSignal) => get<Persona[]>('/v1/personas', undefined, signal),
    teams: (signal?: AbortSignal) => get<Team[]>('/v1/teams', undefined, signal),
    projects: (signal?: AbortSignal) => get<Project[]>('/v1/projects', undefined, signal),
    project: (project: string, signal?: AbortSignal) =>
      get<Project>(`/v1/projects/${id(project)}`, undefined, signal),
    workstreams: (project?: string, signal?: AbortSignal) =>
      get<Workstream[]>('/v1/workstreams', { project }, signal),
    workstream: (workstream: string, signal?: AbortSignal) =>
      get<Workstream>(`/v1/workstreams/${id(workstream)}`, undefined, signal),
    tasks: (filters: TaskFilters = {}, signal?: AbortSignal) =>
      get<Task[]>('/v1/tasks', { ...filters }, signal),
    task: (idOrKey: string, signal?: AbortSignal) =>
      get<Task>(`/v1/tasks/${id(idOrKey)}`, undefined, signal),
    moveTask: (task: string, to: TaskStatus) =>
      request<Task>('POST', `/v1/tasks/${id(task)}/move`, { body: { to } }),
    sessions: (filters: SessionFilters = {}, signal?: AbortSignal) =>
      get<Session[]>('/v1/sessions', { ...filters }, signal),
    session: (session: string, signal?: AbortSignal) =>
      get<Session>(`/v1/sessions/${id(session)}`, undefined, signal),
    /** The newest page without `before`; with it, the page ending before that byte offset. */
    transcript: (session: string, page: TranscriptQuery = {}, signal?: AbortSignal) =>
      get<TranscriptPage>(
        `/v1/sessions/${id(session)}/transcript`,
        { before: number(page.before), limit: number(page.limit) },
        signal,
      ),
    /** Types the text and presses Enter. 503 when the machine is unreachable. */
    send: (session: string, text: string) =>
      request<undefined>('POST', `/v1/sessions/${id(session)}/send`, { body: { text } }),
    keys: (session: string, keys: readonly Key[]) =>
      request<undefined>('POST', `/v1/sessions/${id(session)}/keys`, { body: { keys } }),
    interrupt: (session: string) => request<undefined>('POST', `/v1/sessions/${id(session)}/interrupt`),
    /** `session_ended` follows once it has ended. */
    end: (session: string, mode: EndMode) =>
      request<undefined>('POST', `/v1/sessions/${id(session)}/end`, { body: { mode } }),
    asks: (filters: AskFilters = {}, signal?: AbortSignal) =>
      get<Ask[]>('/v1/asks', { ...filters }, signal),

    // ─── Writes. They do not touch the cache: the event each one emits does. ───────────────────

    createTask: (task: NewTask) => request<Task>('POST', '/v1/tasks', { body: task }),
    /** `null` unassigns. */
    assignTask: (task: string, assignee: MemberId | null) =>
      request<Task>('POST', `/v1/tasks/${id(task)}/assign`, { body: { assignee } }),
    /** A device token replaces the whole list; an agent's replaces only its own plan lines. */
    replaceSubtasks: (task: string, subtasks: Subtask[]) =>
      request<Task>('PUT', `/v1/tasks/${id(task)}/subtasks`, { body: subtasks }),
    /** Answers 201 with the `comment_posted` event. */
    comment: (task: string, comment: NewComment) =>
      request<Event>('POST', `/v1/tasks/${id(task)}/comments`, { body: comment }),
    /** 409 for a done or canceled task. Assigns an unassigned task to the agent. */
    dispatchTask: (task: string, dispatch: DispatchRequest) =>
      request<Dispatch>('POST', `/v1/tasks/${id(task)}/dispatch`, { body: dispatch }),
    answerAsk: (ask: string, answer: AskAnswer) =>
      request<Ask>('POST', `/v1/asks/${id(ask)}/answer`, { body: answer }),

    // ─── Briefs ("Where it stands") ─────────────────────────────────────────────────────────────

    briefs,
    /** The brief in force for a project or workstream, if any. There is no single-brief route. */
    brief: async (target: BriefTarget, signal?: AbortSignal) =>
      (await briefs(signal)).find((b) => b.target.kind === target.kind && b.target.id === target.id),
    editBrief,
    /** Pins or unpins, keeping the text. */
    pinBrief: (brief: Brief, pinned: boolean) =>
      editBrief(brief.target, {
        text: brief.text,
        pinned,
        ...(brief.next === undefined ? {} : { next: brief.next }),
      }),
    /** Makes a back-office proposal (`brief_proposed`) the brief in force. */
    acceptBrief: (target: BriefTarget, proposal: { text: string; next?: string }, pinned: boolean) =>
      editBrief(target, { ...proposal, pinned }),

    // ─── Activity ───────────────────────────────────────────────────────────────────────────────

    /** One page of `GET /v1/events` (see `EventsPage` for how to page). */
    events: (query: EventsQuery = {}, signal?: AbortSignal) =>
      get<EventsPage>(
        '/v1/events',
        {
          project: query.project,
          workstream: query.workstream,
          task: query.task,
          session: query.session,
          before: number(query.before),
          limit: number(query.limit),
        },
        signal,
      ),
  };
}

export type Api = ReturnType<typeof createApi>;
