import assert from 'node:assert/strict';

const text = (v) => assert.equal(typeof v, 'string');
const bool = (v) => assert.equal(typeof v, 'boolean');
const integer = (v) => assert.ok(Number.isSafeInteger(v));
const id = (v) => {
  text(v);
  assert.match(v, /^[0-7][0-9A-HJKMNP-TV-Z]{25}$/);
};
const enumeration =
  (...values) =>
  (v) =>
    assert.ok(values.includes(v), `Unknown enum value ${v}`);
const list = (check) => (v) => {
  assert.ok(Array.isArray(v));
  v.forEach(check);
};
const object = (fields) => (v) => {
  assert.ok(v && typeof v === 'object' && !Array.isArray(v));
  for (const [key, check] of Object.entries(fields)) {
    const optional = key.endsWith('?');
    const name = optional ? key.slice(0, -1) : key;
    if (optional && !Object.hasOwn(v, name)) continue;
    assert.ok(Object.hasOwn(v, name), `Missing ${name}`);
    check(v[name]);
  }
};
const date = (v) => {
  text(v);
  assert.match(v, /^\d{4}-\d{2}-\d{2}$/);
};
const tagged = (tag, variants) => (v) => {
  object({ [tag]: enumeration(...Object.keys(variants)) })(v);
  variants[v[tag]](v);
};
const empty = object({});
const location = object({ machine: id, path: text, 'branch?': text });
const external = object({
  system: enumeration('github', 'jira', 'linear', 'gitlab'),
  key: text,
  'url?': text,
});
const status = enumeration('backlog', 'todo', 'in_progress', 'review', 'done', 'canceled');
const receipt = tagged('kind', {
  transcript: object({ session: id, offset: integer }),
  commit: object({ repo: text, sha: text }),
  pull_request: object({ url: text }),
  job: object({ scheduler: enumeration('slurm'), id: text }),
  file: object({ location }),
  event: object({ id }),
});
const subtask = object({
  id,
  text,
  done: bool,
  source: tagged('kind', { human: empty, agent_plan: object({ agent: id }) }),
});
const machineInfo = object({
  hostname: text,
  os: text,
  arch: text,
  has_tmux: bool,
  'scheduler?': enumeration('slurm'),
  home_on_network_fs: bool,
});
const member = object({
  id,
  kind: enumeration('human', 'agent'),
  handle: text,
  name: text,
  'owner?': id,
  'persona?': id,
});
const machine = object({
  id,
  name: text,
  kind: enumeration('local', 'wsl', 'ssh'),
  liveness: enumeration('live', 'unverifiable', 'stopped'),
  'info?': machineInfo,
});
const workspace = object({ id, name: text });
const project = object({
  id,
  key: text,
  name: text,
  status: enumeration('planning', 'in_progress', 'on_hold', 'completed'),
  lead: id,
  members: list(id),
  'start?': date,
  'due?': date,
  'root?': location,
  external: list(external),
});
const workstream = object({
  id,
  project: id,
  name: text,
  status: enumeration('idea', 'active', 'paused', 'shipped', 'dropped'),
  health: enumeration('on_track', 'at_risk', 'blocked'),
  locations: list(location),
  external: list(external),
});
const task = object({
  id,
  key: text,
  project: id,
  'workstream?': id,
  title: text,
  description: text,
  'archived?': bool,
  status,
  priority: enumeration('urgent', 'high', 'medium', 'low', 'none'),
  'assignee?': id,
  labels: list(text),
  'start?': date,
  'due?': date,
  blocked_by: list(id),
  'source?': external,
  accept_auto: bool,
  subtasks: list(subtask),
});
const session = object({
  id,
  engine: enumeration('claude', 'codex', 'opencode'),
  native_id: text,
  machine: id,
  cwd: text,
  'branch?': text,
  'title?': text,
  'agent?': id,
  'workstream?': id,
  'task?': id,
  'link_basis?': enumeration('dispatch', 'claimed', 'manual', 'folder', 'branch', 'imported'),
  state: enumeration('starting', 'working', 'waiting', 'idle', 'ended', 'unreachable'),
  'status_line?': text,
  started: integer,
  last_activity: integer,
  'terminal?': id,
  'parent?': id,
});
const answer = object({ by: id, 'option?': integer, 'text?': text, at: integer });
const ask = object({
  id,
  kind: enumeration('question', 'decision', 'review', 'approval', 'mention'),
  from: id,
  to: id,
  'task?': id,
  'session?': id,
  title: text,
  body: text,
  options: list(text),
  receipts: list(receipt),
  state: enumeration('open', 'answered', 'withdrawn'),
  'answer?': answer,
  created: integer,
});
const briefTarget = tagged('kind', { project: object({ id }), workstream: object({ id }) });
const proposal = object({ text, 'next?': text, receipts: list(receipt), at: integer });
const brief = object({
  target: briefTarget,
  text,
  'next?': text,
  pinned: bool,
  source: enumeration('person', 'back_office'),
  updated: integer,
  receipts: list(receipt),
  'proposal?': proposal,
});
const nullable = (check) => (value) => {
  if (value !== null) check(value);
};
const patch = object({
  'workstream?': nullable(id),
  'title?': text,
  'description?': text,
  'archived?': bool,
  'priority?': enumeration('urgent', 'high', 'medium', 'low', 'none'),
  'labels?': list(text),
  'start?': nullable(date),
  'due?': nullable(date),
  'blocked_by?': list(id),
  'accept_auto?': bool,
});
const dispatch = object({
  id,
  task: id,
  agent: id,
  'session?': id,
  brief: text,
  started: integer,
  'ended?': integer,
  'outcome?': enumeration('succeeded', 'failed', 'canceled'),
  'summary?': text,
});
const writeFields = object({
  'title?': text,
  'body?': text,
  'labels?': list(text),
  'add_labels?': list(text),
  'remove_labels?': list(text),
  'milestone?': text,
  'epic?': text,
  'state?': enumeration('open', 'closed'),
  'close_reason?': enumeration('completed', 'not_planned'),
  'comment?': text,
});
const writeProposal = object({
  ask: id,
  integration: id,
  system: enumeration('github', 'jira'),
  scope: text,
  'target?': external,
  'task?': id,
  operation: enumeration('create_issue', 'comment', 'update', 'close', 'reopen'),
  before: writeFields,
  after: writeFields,
  requested_by: id,
  'cause?': id,
});
const writeResult = tagged('outcome', {
  sent: object({ 'created?': external, 'url?': text }),
  failed: object({ message: text, 'status?': integer }),
  not_sent: object({ reason: text }),
});
const upstreamWrite = object({
  proposal: writeProposal,
  state: enumeration('pending', 'approved', 'denied', 'sending', 'sent', 'failed', 'not_sent'),
  attempts: integer,
  proposed_at: integer,
  'answered_at?': integer,
  'answered_by?': id,
  'finished_at?': integer,
  'result?': writeResult,
  'retry_requested_by?': id,
});
// Board drafts (api-v1.md, "Board drafts").
const draftCost = object({
  sessions: integer,
  sessions_left_out: integer,
  tasks: integer,
  summary_bytes: integer,
  prompt_bytes: integer,
  redacted: integer,
  estimate: object({ input_tokens: integer, output_tokens: integer }),
});
const proposedTask = object({
  title: text,
  status: enumeration('backlog', 'todo', 'in_progress', 'review', 'done'),
  'description?': text,
  evidence: list(id),
});
const boardProposal = object({ tasks: list(proposedTask), 'note?': text });
const draftedTask = object({ item: integer, task: id });
const boardDraft = object({
  id,
  workstream: id,
  agent: id,
  engine: enumeration('claude', 'codex', 'opencode'),
  session: id,
  by: id,
  prompt: text,
  cost: draftCost,
  started: integer,
  state: enumeration('running', 'proposed', 'reviewed', 'ended'),
  'proposal?': boardProposal,
  'proposed?': integer,
  'reviewed?': integer,
  accepted: list(draftedTask),
  rejected: list(integer),
});
const eventData = {
  safety_changed: object({ settings: object({ permission_mode: enumeration("default", "plan", "accept_edits", "bypass_permissions"), back_office_enabled: bool, back_office_caps: object({ max_auto_accept_per_hour: integer }) }) }),
  cursor_moved: object({ scope: text, rev: integer }),
  machine_added: object({ machine }),
  member_added: object({ member }),
  persona_saved: object({
    persona: object({
      id,
      name: text,
      engine: enumeration('claude', 'codex', 'opencode'),
      permission_mode: enumeration('default', 'accept_edits', 'plan', 'bypass_permissions'),
      'model?': text,
      'instructions?': text,
    }),
  }),
  team_saved: object({ team: object({ id, name: text, lead: id, members: list(id) }) }),
  machine_liveness: object({
    machine: id,
    liveness: enumeration('live', 'unverifiable', 'stopped'),
  }),
  session_discovered: object({ session }),
  session_state_changed: object({ session: id, from: text, to: text, 'status_line?': text }),
  turn_ended: object({ session: id, receipt }),
  tool_ran: object({ session: id, tool: text, target: text, outcome: text, failed: bool, receipt }),
  file_edited: object({
    session: id,
    path: text,
    added: integer,
    removed: integer,
    'receipt?': receipt,
  }),
  session_updated: object({ session: id, 'title?': text, 'branch?': text }),
  session_linked: object({
    session: id,
    'workstream?': id,
    'task?': id,
    basis: enumeration('dispatch', 'claimed', 'manual', 'folder', 'branch', 'imported'),
  }),
  session_ended: object({ session: id }),
  project_created: object({ project }),
  workstream_created: object({ workstream }),
  workstream_changed: object({
    workstream: id,
    status: enumeration('idea', 'active', 'paused', 'shipped', 'dropped'),
    health: enumeration('on_track', 'at_risk', 'blocked'),
  }),
  workstream_linked: object({ workstream: id, external: list(external) }),
  task_created: object({ task }),
  task_moved: object({
    task: id,
    from: status,
    to: status,
    mover: tagged('kind', {
      person: empty,
      agent: object({ on_own_task: bool }),
      back_office: object({ accept_auto: bool }),
      sync: empty,
    }),
  }),
  task_assigned: object({ task: id, 'assignee?': id }),
  task_updated: object({ task: id, patch }),
  subtasks_replaced: object({ task: id, subtasks: list(subtask) }),
  dispatch_started: object({ dispatch }),
  dispatch_finished: object({
    dispatch: id,
    outcome: enumeration('succeeded', 'failed', 'canceled'),
    'summary?': text,
  }),
  ask_raised: object({ ask }),
  ask_answered: object({ ask: id, answer }),
  comment_posted: object({ 'task?': id, 'workstream?': id, text, mentions: list(id) }),
  brief_proposed: object({ target: briefTarget, text, 'next?': text, receipts: list(receipt) }),
  brief_accepted: object({
    target: briefTarget,
    text,
    'next?': text,
    pinned: bool,
    'receipts?': list(receipt),
  }),
  decision_recorded: object({ 'workstream?': id, text, 'why?': text, receipts: list(receipt) }),
  write_proposed: object({ write: writeProposal }),
  write_started: object({ ask: id, 'task?': id, attempt: integer }),
  write_retry_requested: object({ ask: id, 'task?': id, by: id }),
  write_finished: object({ ask: id, 'task?': id, result: writeResult }),
  board_draft_started: object({
    draft: id,
    workstream: id,
    agent: id,
    engine: enumeration('claude', 'codex', 'opencode'),
    session: id,
    prompt: text,
    cost: draftCost,
  }),
  board_proposed: object({ draft: id, workstream: id, tasks: list(proposedTask), 'note?': text }),
  board_draft_reviewed: object({
    draft: id,
    workstream: id,
    accepted: list(draftedTask),
    rejected: list(integer),
  }),
};
const eventBody = (v) => {
  object({ type: enumeration(...Object.keys(eventData)), data: empty })(v);
  eventData[v.type](v.data);
};
const event = object({
  id,
  at: integer,
  workspace: id,
  author: id,
  'on_behalf_of?': id,
  body: eventBody,
});
const transcript = tagged('kind', {
  user_prompt: object({ at: integer, text, offset: integer }),
  assistant_text: object({ at: integer, text, offset: integer }),
  tool_use: object({ at: integer, call_id: text, tool: text, target: text, offset: integer }),
  tool_result: object({
    at: integer,
    call_id: text,
    is_error: bool,
    summary: text,
    offset: integer,
  }),
  file_edit: object({
    at: integer,
    path: text,
    added: integer,
    removed: integer,
    'diff?': text,
    offset: integer,
  }),
  plan_updated: object({
    at: integer,
    items: list(object({ text, status: enumeration('pending', 'in_progress', 'completed') })),
    offset: integer,
  }),
  question: object({ at: integer, text, options: list(text), offset: integer }),
  turn_ended: object({ at: integer, offset: integer }),
});
const summary = (v) => {
  object({
    text,
    spans: list(
      object({ range: object({ start: integer, end: integer }), receipts: list(receipt) }),
    ),
  })(v);
  const bytes = Buffer.from(v.text);
  let end = 0;
  for (const span of v.spans) {
    assert.ok(
      span.range.start >= end &&
        span.range.end > span.range.start &&
        span.range.end <= bytes.length,
    );
    assert.ok(span.receipts.length);
    assert.ok(
      !bytes.subarray(span.range.start, span.range.end).toString('utf8').includes('\uFFFD'),
    );
    end = span.range.end;
  }
};
const counts = object(
  Object.fromEntries(
    [
      'events',
      'tools_run',
      'tools_failed',
      'file_edits',
      'lines_added',
      'lines_removed',
      'turns',
      'asks_raised',
      'asks_answered',
      'task_moves',
      'comments',
    ].map((k) => [k, integer]),
  ),
);
const block = object({
  id,
  last: id,
  key: tagged('kind', {
    session: object({ id }),
    workstream: object({ id }),
    project: object({ id }),
  }),
  start: integer,
  end: integer,
  'session?': id,
  'workstream?': id,
  'project?': id,
  tasks: list(id),
  'agent?': id,
  actors: list(id),
  counts,
  files: list(
    object({
      path: text,
      edits: integer,
      added: integer,
      removed: integer,
      receipts: list(receipt),
    }),
  ),
  files_omitted: integer,
  facts: list(
    object({ by: id, at: integer, kind: object({ type: text }), receipts: list(receipt) }),
  ),
  facts_omitted: integer,
  tool_receipts: list(receipt),
  turn_receipts: list(receipt),
});
const integrationSettings = tagged('kind', {
  github: object({ repos: list(text), 'api_base?': text }),
  jira: object({
    deployment: enumeration('cloud', 'data_center'),
    site: text,
    projects: list(text),
    'email?': text,
    'epic_link_field?': text,
  }),
});
const syncCounts = object({
  changes: integer,
  applied: integer,
  conflicts: integer,
  skipped: integer,
  malformed: integer,
});
const integration = object({
  id,
  name: text,
  settings: integrationSettings,
  credential: object({ source: enumeration('gh_cli', 'stored'), stored: bool }),
  interval_minutes: integer,
  added_by: id,
  added_at: integer,
  status: object({
    running: bool,
    'last_attempt_at?': integer,
    'last_success_at?': integer,
    'next_at?': integer,
    'rate_limited_until?': integer,
    problems: list(object({ scope: text, message: text })),
    'last_run?': syncCounts,
  }),
  links: list(object({ workstream: id, scope: external, 'title?': text })),
});
export const schemas = {
  host: object({
    name: text,
    version: text,
    protocol: integer,
    protocol_min: integer,
    roles: list(enumeration('hub', 'runner')),
    machine: machineInfo,
    capabilities: list(enumeration('tmux', 'pty', 'slurm', 'watch', 'scan')),
  }),
  workspace: object({ workspace, rev: integer, 'setup_needed?': bool }),
  member,
  machine,
  project,
  workstream,
  task,
  session,
  ask,
  brief,
  event,
  personas: list(
    object({
      id,
      name: text,
      engine: enumeration('claude', 'codex', 'opencode'),
      'model?': text,
      'instructions?': text,
      permission_mode: enumeration('default', 'accept_edits', 'plan', 'bypass_permissions'),
    }),
  ),
  teams: list(object({ id, name: text, lead: id, members: list(id) })),
  dispatch: object({
    id,
    task: id,
    agent: id,
    'session?': id,
    brief: text,
    started: integer,
    'ended?': integer,
    'outcome?': enumeration('succeeded', 'failed', 'canceled'),
    'summary?': text,
  }),
  events: object({ events: list(event), from_rev: integer, to_rev: integer, at_start: bool }),
  transcript: object({ items: list(transcript), from: integer, to: integer, at_start: bool }),
  blocks: object({ blocks: list(object({ block, line: summary })), at_start: bool }),
  days: object({
    days: list(object({ 'workstream?': id, date, blocks: list(id), summary })),
    at_start: bool,
  }),
  setup: object({ workspace, me: member, machine }),
  integration,
  upstreamWrite,
  integrationCheck: object({
    ok: bool,
    at: integer,
    checks: list(object({ scope: text, ok: bool, message: text })),
    warnings: list(text),
  }),
  boardDraft,
  draftPreview: object({ workstream: id, prompt: text, cost: draftCost, summary: text, digest: text }),
  draftReviewed: object({ draft: boardDraft, tasks: list(task) }),
  error: object({
    code: enumeration(
      'unauthorized',
      'forbidden',
      'not_found',
      'conflict',
      'invalid',
      'unavailable',
      'too_large',
      'unsupported',
      'internal',
    ),
    message: text,
  }),
};
export { bool, enumeration, integer, list, object, tagged, text };
