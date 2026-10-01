// Dumps the mock hub's answers for the parity tests in seed.rs
// (`seeded_routes_answer_like_the_running_mock_hub` and `writes_answer_like_the_running_mock_hub`).
//
//   node crates/hub-work/tests/dump-mock-hub.mjs <folder>
//   PITCREW_MOCK_DUMP=<folder> cargo test -p pitcrew-hub-work --test seed -- --ignored
//
// It starts apps/mock-hub in this process (Node 24 runs its TypeScript directly) on a free port.
//
// - Reads: it asks each GET route in `paths` with the mock's device token, and writes each body to
//   <folder>/<name>.json, plus <folder>/index.json ({ name: path }) for the test to replay.
// - Writes: then, on the same server, it makes the requests in `writes` in order and writes them
//   with the mock's answers to <folder>/writes.json. A step may `bind` a name to the id its answer
//   creates; later steps say `{{NAME}}` where that id goes, and the test puts its own id there.
//   A `propose` step appends a `brief_proposed` from @office, as the back office would.
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';

const { startServer } = await import(
  new URL('../../../apps/mock-hub/src/server.ts', import.meta.url).href
);

const out = process.argv[2];
if (!out) {
  console.error('usage: node dump-mock-hub.mjs <folder>');
  process.exit(2);
}
mkdirSync(out, { recursive: true });

const SAM = '01JB000000000000000MEM0001';
const WRITER = '01JB000000000000000MEM0002';
const RUNNER = '01JB000000000000000MEM0003';
const OFFICE = '01JB000000000000000MEM0006';
const LAPTOP = '01JB000000000000000MCH0001';
const PAPER = '01JB000000000000000PRJ0001';
const TOOLING = '01JB000000000000000PRJ0002';
const SUBMISSION = '01JB000000000000000WST0001';
const SEEDS = '01JB000000000000000WST0002';
const PARSERS = '01JB000000000000000WST0003';
const PAP1 = '01JB000000000000000TSK0001';
const PAP2 = '01JB000000000000000TSK0002';
const TL1 = '01JB000000000000000TSK0008';
const UNKNOWN = {
  project: '01JB000000000000000PRJ0099',
  workstream: '01JB000000000000000WST0099',
  member: '01JB000000000000000MEM0099',
  machine: '01JB000000000000000MCH0099',
  task: '01JB000000000000000TSK0099',
};
const JOB = { kind: 'job', scheduler: 'slurm', id: '4815170' };

const paths = {
  me: '/v1/me',
  workspace: '/v1/workspace',
  machines: '/v1/machines',
  members: '/v1/members',
  personas: '/v1/personas',
  teams: '/v1/teams',
  projects: '/v1/projects',
  project_paper: `/v1/projects/${PAPER}`,
  workstreams: '/v1/workstreams',
  workstreams_tooling: `/v1/workstreams?project=${TOOLING}`,
  workstream_submission: `/v1/workstreams/${SUBMISSION}`,
  tasks: '/v1/tasks',
  tasks_todo_backlog: '/v1/tasks?status=todo&status=backlog',
  tasks_writer: `/v1/tasks?assignee=${WRITER}`,
  tasks_submission: `/v1/tasks?workstream=${SUBMISSION}`,
  task_pap4: '/v1/tasks/PAP-4',
  task_tl2: '/v1/tasks/tsk_01JB000000000000000TSK0009',
  sessions: '/v1/sessions',
  sessions_laptop: `/v1/sessions?machine=${LAPTOP}`,
  sessions_submission: `/v1/sessions?workstream=${SUBMISSION}`,
  sessions_pap1: `/v1/sessions?task=${PAP1}`,
  sessions_working_idle: '/v1/sessions?state=working&state=idle',
  session_ses3: '/v1/sessions/01JB000000000000000SES0003',
  asks: '/v1/asks',
  asks_inbox: `/v1/asks?to=${SAM}&state=open`,
  briefs: '/v1/briefs',
};

// The demo's pending proposal for the paper is its last event.
const fixture = JSON.parse(
  readFileSync(new URL('../../fixtures/data/demo-workspace.json', import.meta.url), 'utf8'),
);
const paperProposal = fixture.events.at(-1).body.data.text;

const many = (n) => Array.from({ length: n }, (_, i) => `label-${i}`);
const step = (name, method, path, body, extra = {}) => ({ name, method, path, body, ...extra });
const patch = (name, ref, body, extra) => step(name, 'PATCH', `/v1/tasks/${ref}`, body, extra);
const post = (name, path, body, extra) => step(name, 'POST', path, body, extra);
const put = (name, path, body, extra) => step(name, 'PUT', `/v1/briefs/${path}`, body, extra);
const read = (name, path) => step(name, 'GET', path);
const propose = (name, target, text, next) => ({
  name,
  propose: { target, text, ...(next === undefined ? {} : { next }), receipts: [JOB] },
});
const seeds = { kind: 'workstream', id: SEEDS };

const writes = [
  // PATCH /v1/tasks/{id-or-key}
  patch('patch_fields', 'PAP-2', {
    title: '  Make figure 3  ',
    description: 'Mean and spread over the four good seeds.',
    priority: 'medium',
    labels: [' figures ', 'paper', 'figures'],
    start: '2026-10-05',
    accept_auto: false,
    unknown_field: 'ignored',
    status: 'done',
    assignee: SAM,
  }),
  patch('patch_by_id', '01JB000000000000000TSK0006', { title: 'Aggregate the results' }),
  patch('patch_by_prefixed_id', 'tsk_01JB000000000000000TSK0006', { title: 'Aggregate them' }),
  patch('patch_nothing', 'PAP-1', {}),
  patch('patch_same_values', 'PAP-1', {
    title: 'Draft the method section ',
    priority: 'high',
    labels: ['writing'],
    due: '2026-10-10',
    workstream: SUBMISSION,
    blocked_by: [],
    accept_auto: false,
  }),
  patch('patch_null_plain_fields', 'PAP-1', { title: null, labels: null, priority: null }),
  patch('patch_start', 'PAP-1', { start: '2026-10-01' }),
  patch('patch_clear', 'PAP-1', { workstream: null, start: null, due: null }),
  patch('patch_clear_again', 'PAP-1', { workstream: null, due: null }),
  patch('patch_workstream', 'PAP-1', { workstream: SEEDS }),
  patch('patch_workstream_of_another_project', 'PAP-1', { workstream: PARSERS }),
  patch('patch_workstream_unknown', 'PAP-1', { workstream: UNKNOWN.workstream }),
  patch('patch_title_blank', 'PAP-1', { title: '   ' }),
  patch('patch_title_501', 'PAP-1', { title: 'x'.repeat(501) }),
  patch('patch_title_number', 'PAP-1', { title: 42 }),
  patch('patch_title_500', 'PAP-1', { title: ` ${'x'.repeat(500)} ` }),
  patch('patch_title_500_emoji', 'PAP-1', { title: '\u{1F9EA}'.repeat(500) }),
  patch('patch_title_501_emoji', 'PAP-1', { title: '\u{1F9EA}'.repeat(501) }),
  patch('patch_labels_32', 'PAP-1', { labels: [...many(32), ' label-0 ', 'label-31'] }),
  patch('patch_labels_33', 'PAP-1', { labels: many(33) }),
  patch('patch_labels_blank', 'PAP-1', { labels: ['ok', '  '] }),
  patch('patch_labels_65', 'PAP-1', { labels: ['y'.repeat(65)] }),
  patch('patch_labels_not_a_list', 'PAP-1', { labels: 'writing' }),
  patch('patch_labels_64', 'PAP-1', { labels: ['z'.repeat(64)] }),
  patch('patch_labels_none', 'PAP-1', { labels: [] }),
  patch('patch_blocker_unknown', 'PAP-1', { blocked_by: [UNKNOWN.task] }),
  patch('patch_blocker_malformed', 'PAP-1', { blocked_by: ['not-an-id'] }),
  patch('patch_blocker_itself', 'PAP-1', { blocked_by: [PAP1] }),
  patch('patch_blocker_cycle', 'PAP-4', { blocked_by: [PAP2] }),
  patch('patch_blocker_duplicates', 'PAP-4', { blocked_by: [PAP1, PAP1] }),
  patch('patch_blocker_cycle_through', 'PAP-1', { blocked_by: [PAP2] }),
  patch('patch_blocker_cycle_and_malformed', 'PAP-1', { blocked_by: [PAP2], priority: 'critical' }),
  patch('patch_blocker', 'PAP-1', { blocked_by: [TL1] }),
  patch('patch_due', 'PAP-3', { due: '2026-10-10', start: null }),
  patch('patch_start_bad_day', 'PAP-3', { start: '2026-10-32' }),
  patch('patch_due_bad_format', 'PAP-3', { due: '10/10/2026' }),
  patch('patch_start_after_due', 'PAP-3', { start: '2026-10-02', due: '2026-10-01' }),
  patch('patch_start_after_current_due', 'PAP-3', { start: '2026-10-11' }),
  patch('patch_start_on_due', 'PAP-3', { start: '2026-10-10' }),
  patch('patch_due_before_current_start', 'PAP-3', { due: '2026-10-09' }),
  patch('patch_no_due_any_start', 'PAP-3', { due: null, start: '2026-12-01' }),
  patch('patch_priority_unknown', 'PAP-1', { priority: 'critical' }),
  patch('patch_accept_auto_text', 'PAP-1', { accept_auto: 'yes' }),
  patch('patch_description_number', 'PAP-1', { description: 7 }),
  patch('patch_workstream_number', 'PAP-1', { workstream: 12 }),
  patch('patch_good_title_bad_label', 'PAP-1', { title: 'A new title', labels: [''] }),
  patch('patch_array_body', 'PAP-1', []),
  patch('patch_unknown_task', 'PAP-99', { title: 'x' }),
  patch('patch_unknown_task_bad_body', 'PAP-99', { title: 42 }),
  patch('patch_by_agent', 'PAP-1', { title: 'x' }, { token: 'agent' }),
  patch('patch_accept_auto_off', 'TL-2', { accept_auto: false }),
  // POST /v1/projects
  post('project_defaults', '/v1/projects', { key: 'THS', name: 'Thesis' }, { bind: 'THS' }),
  post('project_first_task', '/v1/tasks', { project: '{{THS}}', title: 'Outline chapter 1' }, { bind: 'THS_1' }),
  post(
    'project_every_field',
    '/v1/projects',
    {
      key: 'AB12',
      name: 'Ablations',
      lead: WRITER,
      members: [RUNNER, RUNNER, SAM],
      status: 'planning',
      start: '2026-10-01',
      due: '2026-10-01',
      root: { machine: LAPTOP, path: '/work/ablations', branch: 'main' },
    },
    { bind: 'AB12' },
  ),
  post('project_key_in_use', '/v1/projects', { key: 'PAP', name: 'Another paper' }),
  post('project_new_key_in_use', '/v1/projects', { key: 'THS', name: 'Another thesis' }),
  ...[
    { name: 'No key' },
    { key: 'pap', name: 'Lower case' },
    { key: 'P', name: 'Too short' },
    { key: '1AB', name: 'Starts with a digit' },
    { key: 'TOOLONGKEY1', name: 'Eleven characters' },
    { key: 'A-B', name: 'Punctuation' },
    { key: 'NEW' },
    { key: 'NEW', name: '   ' },
    { key: 'NEW', name: 'x', lead: UNKNOWN.member },
    { key: 'NEW', name: 'x', members: [SAM, UNKNOWN.member] },
    { key: 'NEW', name: 'x', status: 'someday' },
    { key: 'NEW', name: 'x', start: '2026-13-01' },
    { key: 'NEW', name: 'x', start: '2026-11-02', due: '2026-11-01' },
    { key: 'NEW', name: 'x', root: { machine: UNKNOWN.machine, path: '/work' } },
    { key: 'NEW', name: 'x', root: { machine: LAPTOP, path: '' } },
    { key: 'PAP', name: ' ' },
    ['NEW', 'Positional'],
  ].map((body, i) => post(`project_malformed_${i}`, '/v1/projects', body)),
  post('project_by_agent', '/v1/projects', { key: 'NEW', name: 'x' }, { token: 'agent' }),
  // POST /v1/workstreams
  post('workstream_defaults', '/v1/workstreams', { project: PAPER, name: 'Figures' }, { bind: 'FIG' }),
  post(
    'workstream_every_field',
    '/v1/workstreams',
    {
      project: TOOLING,
      name: 'Packaging',
      status: 'idea',
      locations: [{ machine: LAPTOP, path: '/work/paper/figures' }],
    },
    { bind: 'PKG' },
  ),
  post('workstream_in_a_new_project', '/v1/workstreams', { project: '{{THS}}', name: 'Chapter 1' }, { bind: 'CH1' }),
  post('workstream_unknown_project', '/v1/workstreams', { project: UNKNOWN.project, name: 'Lost' }),
  ...[
    { name: 'No project' },
    { project: PAPER },
    { project: PAPER, name: '' },
    { project: PAPER, name: 'x', status: 'finished' },
    { project: PAPER, name: 'x', health: 'on_track', status: 'on_track' },
    { project: PAPER, name: 'x', locations: [{ machine: UNKNOWN.machine, path: '/work' }] },
    { project: PAPER, name: 'x', locations: [{ machine: LAPTOP, path: ' ' }] },
    { project: PAPER, name: 'x', locations: { machine: LAPTOP, path: '/work' } },
    { project: UNKNOWN.project, name: ' ' },
    [PAPER, 'Positional'],
  ].map((body, i) => post(`workstream_malformed_${i}`, '/v1/workstreams', body)),
  post('workstream_by_agent', '/v1/workstreams', { project: PAPER, name: 'x' }, { token: 'agent' }),
  patch('patch_into_a_new_workstream', 'PAP-5', { workstream: '{{FIG}}' }),
  patch('patch_into_another_projects_workstream', 'PAP-5', { workstream: '{{CH1}}' }),
  patch('patch_new_projects_task', '{{THS_1}}', { workstream: '{{CH1}}', labels: ['outline'] }),
  // Briefs: next steps and pending proposals
  put('brief_next', `workstream/${SUBMISSION}`, {
    text: '§3.2 is drafted.',
    next: 'Send it to the co-authors.',
    pinned: false,
  }),
  read('briefs_with_the_demos_proposal', '/v1/briefs'),
  put('brief_accept_the_demos_proposal', `project/${PAPER}`, { text: paperProposal, pinned: false }),
  put('brief_keep_current', `project/${PAPER}`, { text: paperProposal, pinned: false }),
  propose('propose_for_seeds', seeds, 'Seed 3 reran and converged.', 'Make figure 3.'),
  read('briefs_with_a_new_proposal', '/v1/briefs'),
  put('brief_without_the_proposals_next', `workstream/${SEEDS}`, {
    text: 'Seed 3 reran and converged.',
    pinned: true,
  }),
  propose('propose_for_seeds_again', seeds, 'Seed 3 reran and converged.', 'Make figure 3.'),
  put('brief_accept_with_its_next', `workstream/${SEEDS}`, {
    text: 'Seed 3 reran and converged.',
    next: 'Make figure 3.',
    pinned: true,
  }),
  propose('propose_without_next', seeds, 'Seed 3 converged twice.'),
  read('briefs_with_a_newer_proposal', '/v1/briefs'),
  put('brief_keep_current_seeds', `workstream/${SEEDS}`, {
    text: 'Seed 3 reran and converged.',
    next: 'Make figure 3.',
    pinned: true,
  }),
  propose('propose_for_a_target_without_a_brief', { kind: 'workstream', id: '{{FIG}}' }, 'Not started.'),
  put('brief_unknown_project', `project/${UNKNOWN.project}`, { text: 'x', pinned: false }),
  put('brief_task_target', `task/${PAP1}`, { text: 'x', pinned: false }),
  put('brief_pinned_missing', `project/${PAPER}`, { text: 'x' }),
  put('brief_next_number', `project/${PAPER}`, { text: 'x', next: 3, pinned: false }),
  put('brief_by_agent', `project/${PAPER}`, { text: 'x', pinned: false }, { token: 'agent' }),
  // The state after it all.
  read('projects_after', '/v1/projects'),
  read('workstreams_after', '/v1/workstreams'),
  read('tasks_after', '/v1/tasks'),
  read('briefs_after', '/v1/briefs'),
];

/** `{{NAME}}` in a path or body, for the ids earlier steps created. */
function bindIds(value, ids) {
  const text = JSON.stringify(value).replace(/\{\{([A-Z0-9_]+)\}\}/g, (_, name) => {
    const id = ids.get(name);
    if (id === undefined) {
      throw new Error(`nothing bound to ${name} yet`);
    }
    return id;
  });
  return JSON.parse(text);
}

const TOKENS = { device: 'dev-device-token', agent: 'dev-agent-token' };

const server = await startServer({ port: 0 });
try {
  for (const [name, path] of Object.entries(paths)) {
    const res = await fetch(server.url + path, {
      headers: { authorization: `Bearer ${TOKENS.device}` },
    });
    if (res.status !== 200) {
      throw new Error(`${path}: ${res.status} ${await res.text()}`);
    }
    writeFileSync(join(out, `${name}.json`), await res.text());
  }
  writeFileSync(join(out, 'index.json'), JSON.stringify(paths, null, 2));

  const ids = new Map();
  const answered = [];
  for (const w of writes) {
    if (w.propose !== undefined) {
      server.hub.append(OFFICE, { type: 'brief_proposed', data: bindIds(w.propose, ids) });
      answered.push(w);
      continue;
    }
    const headers = { authorization: `Bearer ${TOKENS[w.token ?? 'device']}` };
    let body;
    if (w.body !== undefined) {
      headers['content-type'] = 'application/json';
      body = JSON.stringify(bindIds(w.body, ids));
    }
    const res = await fetch(server.url + bindIds(w.path, ids), { method: w.method, headers, body });
    const text = await res.text();
    const response = text === '' ? null : JSON.parse(text);
    if (w.bind !== undefined) {
      if (typeof response?.id !== 'string') {
        throw new Error(`${w.name}: ${res.status} ${text}`);
      }
      ids.set(w.bind, response.id);
    }
    answered.push({ ...w, status: res.status, response });
  }
  writeFileSync(join(out, 'writes.json'), JSON.stringify(answered, null, 2));
  console.log(
    `dumped ${Object.keys(paths).length} read answers and ${answered.length} write steps to ${out}`,
  );
} finally {
  await server.close();
}
