// Dumps the mock hub's answers for the parity test in seed.rs
// (`seeded_routes_answer_like_the_running_mock_hub`).
//
//   node crates/hub-work/tests/dump-mock-hub.mjs <folder>
//   PITCREW_MOCK_DUMP=<folder> cargo test -p pitcrew-hub-work --test seed -- --ignored
//
// It starts apps/mock-hub in this process (Node 24 runs its TypeScript directly) on a free port,
// asks each GET route below with the mock's device token, and writes each body to
// <folder>/<name>.json, plus <folder>/index.json ({ name: path }) for the test to replay.
import { mkdirSync, writeFileSync } from 'node:fs';
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
const LAPTOP = '01JB000000000000000MCH0001';
const PAPER = '01JB000000000000000PRJ0001';
const TOOLING = '01JB000000000000000PRJ0002';
const SUBMISSION = '01JB000000000000000WST0001';
const PAP1 = '01JB000000000000000TSK0001';
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

const server = await startServer({ port: 0 });
try {
  for (const [name, path] of Object.entries(paths)) {
    const res = await fetch(server.url + path, {
      headers: { authorization: 'Bearer dev-device-token' },
    });
    if (res.status !== 200) {
      throw new Error(`${path}: ${res.status} ${await res.text()}`);
    }
    writeFileSync(join(out, `${name}.json`), await res.text());
  }
  writeFileSync(join(out, 'index.json'), JSON.stringify(paths, null, 2));
  console.log(`dumped ${Object.keys(paths).length} answers to ${out}`);
} finally {
  await server.close();
}
