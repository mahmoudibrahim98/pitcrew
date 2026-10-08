import { expect, test } from '@playwright/test';
import { expectNoAxeViolations } from '../../../../e2e/axe.ts';
import { HUB_AUTH, HUB_URL } from '../../../../e2e/helpers.ts';

// "Draft board", walked through in the browser against the mock hub: the cost and what will be
// sent come first; only the CLIs found on the hub's machine are offered; the back office (the
// mock plays it) proposes; nothing starts accepted; only the task the person accepts is created,
// labelled "drafted"; and the draft's session ends once it has proposed.
const ws = '01JB000000000000000WSP0001';
const project = '01JB000000000000000PRJ0001';
const submission = '01JB000000000000000WST0001';

test('drafts a board from history, and creates only what the person accepts', async ({ page, request }) => {
  test.setTimeout(60_000);
  const before = (await (await request.get(`${HUB_URL}/v1/tasks?workstream=${submission}`, { headers: HUB_AUTH })).json()) as {
    id: string;
  }[];
  await page.goto(`/w/${ws}/projects/${project}/workstreams/${submission}`);
  await page.getByRole('button', { name: 'Draft board', exact: true }).click();
  const panel = page.getByRole('region', { name: 'Draft the board from history' });

  // Cost first: the sizes, the sessions, the redactions and the estimate; then what is sent.
  await expect(panel.getByText(/^2 sessions, 4 existing tasks:/)).toBeVisible();
  await expect(panel.getByText(/^Estimated usage: about/)).toBeVisible();
  await expect(panel.getByLabel('What will be sent')).toContainText('Session 01JB000000000000000SES0001');
  // The agent (the back office first), and only the CLIs found where drafts run.
  await expect(panel.getByLabel('Agent')).toHaveValue('01JB000000000000000MEM0006');
  const cli = panel.getByLabel('CLI');
  await expect(cli.locator('option')).toHaveText(['Claude Code', 'Codex', 'OpenCode']);
  expect(await request.get(`${HUB_URL}/v1/board-drafts?workstream=${submission}`, { headers: HUB_AUTH }).then((r) => r.json())).toEqual([]);
  await expectNoAxeViolations(page, 'the draft preview');

  await panel.getByRole('button', { name: 'Send and draft', exact: true }).click();
  // The back office proposes; the review shows, with nothing accepted yet.
  const list = panel.getByRole('list', { name: 'Proposed tasks' });
  await expect(list).toBeVisible({ timeout: 20_000 });
  const boxes = list.getByRole('checkbox');
  await expect(boxes).toHaveCount(2);
  for (const box of await boxes.all()) await expect(box).not.toBeChecked();
  await expect(panel.getByRole('button', { name: 'Create no tasks', exact: true })).toBeDisabled();
  await expectNoAxeViolations(page, 'the draft review');

  const [draft] = (await (await request.get(`${HUB_URL}/v1/board-drafts?workstream=${submission}`, { headers: HUB_AUTH })).json()) as {
    id: string;
    state: string;
    session: string;
  }[];
  expect(draft?.state).toBe('proposed');
  // Its session is ended once it has proposed.
  await expect
    .poll(async () => (await (await request.get(`${HUB_URL}/v1/sessions/${draft?.session}`, { headers: HUB_AUTH })).json()).state)
    .toBe('ended');

  await boxes.first().check();
  await panel.getByRole('button', { name: 'Create 1 task', exact: true }).click();
  await expect(panel.getByText(/^Created PAP-\d+ on the board, labelled “drafted”\.$/)).toBeVisible();
  const after = (await (await request.get(`${HUB_URL}/v1/tasks?workstream=${submission}`, { headers: HUB_AUTH })).json()) as {
    id: string;
    labels: string[];
  }[];
  expect(after).toHaveLength(before.length + 1);
  expect(after.filter((t) => t.labels.includes('drafted'))).toHaveLength(1);
  const reviewed = (await (await request.get(`${HUB_URL}/v1/board-drafts/${draft?.id}`, { headers: HUB_AUTH })).json()) as {
    state: string;
    rejected: number[];
  };
  expect(reviewed.state).toBe('reviewed');
  expect(reviewed.rejected).toEqual([1]);
});
