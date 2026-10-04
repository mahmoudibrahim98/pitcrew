import { expect, test, type Page } from '@playwright/test';
import { expectNoAxeViolations } from './axe';
import { FIRST_RUN_HUB, MOCK_DEVICE_TOKEN } from './fresh-hubs';

// The first run in a browser, against a fresh mock hub (`fresh.config.ts`): the shell sends the
// empty workspace to the first-run wizard, the wizard is Welcome, Workspace, Machine check, Sign
// in, Scan, Create, Import, Done against the real `POST /v1/setup`, machine setup's routes (the
// mock's synthetic check, accounts and sign-in terminal), `POST /v1/machines/{id}/scan` (the mock's
// synthetic report), `POST /v1/projects` and `POST /v1/workstreams`, and Home stays Home
// afterwards. `GET /v1/me` is then the new person, and the projects and workstreams are where the
// scan found them. axe finds nothing on the new screens, light and dark.

const AUTH = { Authorization: `Bearer ${MOCK_DEVICE_TOKEN}` };

function heading(page: Page, name: string | RegExp) {
  return page.getByRole('heading', { level: 1, name });
}

test('the first run, from an empty workspace to Home as the new person', async ({ page, request }) => {
  test.setTimeout(120_000);
  const before = await request.get(`${FIRST_RUN_HUB}/v1/workspace`, { headers: AUTH });
  const { workspace, setup_needed } = (await before.json()) as { workspace: { id: string }; setup_needed?: boolean };
  expect(setup_needed).toBe(true);
  expect((await request.get(`${FIRST_RUN_HUB}/v1/me`, { headers: AUTH })).status()).toBe(404);
  const ws = workspace.id;

  // `/` opens the workspace, which sends it to setup, shown bare.
  await page.goto('/');
  await expect(page).toHaveURL(new RegExp(`/w/${ws}/onboarding$`));
  await expect(heading(page, 'Welcome to PitCrew')).toBeVisible();
  await expect(page.getByRole('complementary', { name: 'Sidebar' })).toHaveCount(0);
  // Only the steps the hub can serve.
  await expect(page.getByRole('tab')).toHaveText([
    /Welcome/,
    /Workspace/,
    /Machine check/,
    /Sign in/,
    /Scan/,
    /Create/,
    /Import/,
    /Done/,
  ]);
  await expectNoAxeViolations(page, 'welcome');

  // Any other page goes back to setup while it is not done.
  await page.goto(`/w/${ws}/inbox`);
  await expect(page).toHaveURL(new RegExp(`/w/${ws}/onboarding$`));
  await page.getByRole('button', { name: 'Get started' }).click();

  // Workspace: refused by field first, then sent.
  await expect(heading(page, 'Your first workspace')).toBeVisible();
  await page.getByLabel('Workspace name').fill('   ');
  await page.getByRole('button', { name: 'Continue' }).click();
  await expect(page.getByText('Give the workspace a name.')).toBeVisible();
  await expect(page.getByText('Enter your name.')).toBeVisible();
  await expect(page.getByLabel('Workspace name')).toBeFocused();
  await expectNoAxeViolations(page, 'workspace, refused');

  await page.getByLabel('Workspace name').fill('  Demo Lab ');
  await page.getByLabel('Your name').fill('Sam Rivera');
  await expect(page.getByLabel('Your handle')).toHaveValue('@sam');
  await page.getByLabel('This machine’s name').fill('This laptop');
  await expectNoAxeViolations(page, 'workspace');
  await page.getByRole('button', { name: 'Continue' }).click();

  // Machine check: the hub's own machine. A missing tool's fix opens its install page (stubbed:
  // nothing leaves the test) and says where it is; nothing is installed.
  await expect(heading(page, 'Checking the machine')).toBeVisible();
  const opencode = page.getByRole('listitem').filter({ hasText: 'OpenCode CLI' });
  await expect(opencode.getByText('Missing')).toBeVisible();
  await expect(page.getByRole('listitem').filter({ hasText: 'Claude Code CLI' }).getByText('2.1.3 (Claude Code)')).toBeVisible();
  await page.context().route('https://opencode.ai/**', (route) => route.fulfill({ body: 'An install page.' }));
  const opened = page.context().waitForEvent('page');
  await opencode.getByRole('button', { name: 'Install OpenCode CLI…' }).click();
  const tab = await opened;
  await tab.waitForLoadState();
  expect(tab.url()).toBe('https://opencode.ai/docs/');
  await tab.close();
  await expect(opencode.getByText('https://opencode.ai/docs/')).toBeVisible();
  await expectNoAxeViolations(page, 'machine check');
  await page.getByRole('button', { name: 'Check again' }).click();
  await expect(opencode.getByText('Missing')).toBeVisible();
  await page.getByRole('button', { name: 'Continue' }).click();

  // Sign in: Claude Code's own login in the console's terminal view; once it ends, the CLI says
  // who is signed in.
  await expect(heading(page, 'Sign in to your agents')).toBeVisible();
  const claude = page.getByRole('listitem').filter({ hasText: 'Claude Code' });
  await expect(claude.getByText('Not signed in')).toBeVisible();
  await expectNoAxeViolations(page, 'sign in');
  await page.getByRole('button', { name: 'Sign in to Claude Code' }).click();
  const login = page.getByRole('region', { name: 'Claude Code sign-in' });
  await expect(login.getByText('claude auth login', { exact: true })).toBeVisible();
  await expect(claude.getByText('sam@example.com')).toBeVisible({ timeout: 15_000 });
  await expect(claude.getByText('Signed in', { exact: true })).toBeVisible();
  await expect(login.getByText(/the login has ended/)).toBeVisible();
  await page.getByRole('button', { name: 'Continue' }).click();

  // Scan: the hub's own machine, its counts and suggestions.
  await expect(heading(page, 'Scanning for sessions')).toBeVisible();
  await expect(page.getByText(/Found 3 likely projects/)).toBeVisible({ timeout: 15_000 });
  await expect(page.getByText('/home/sam/work/diffusion-paper/paper')).toBeVisible();
  await expectNoAxeViolations(page, 'scan');
  await page.getByRole('button', { name: 'Continue' }).click();

  // Create: rename one, leave one out, drop a workstream; the rest is created.
  await expect(heading(page, 'Create projects and workstreams')).toBeVisible();
  await page.getByLabel('Include diffusion-runs').uncheck();
  await page.getByLabel('Include experiments').uncheck();
  const paper = page.getByRole('listitem').filter({ has: page.getByLabel('Include diffusion-paper') });
  await paper.getByLabel('Project name').fill('Diffusion paper');
  await expectNoAxeViolations(page, 'create');
  await page.getByRole('button', { name: 'Create', exact: true }).click();

  await expect(page.getByText('This will import 0 sessions.')).toBeVisible();
  await expectNoAxeViolations(page, 'import');
  await page.getByRole('button', { name: 'Continue' }).click();
  expect((await request.get(`${FIRST_RUN_HUB}/v1/import`, { headers: AUTH })).status()).toBe(200);

  // Done, then Home, which stays Home.
  await expect(heading(page, "You're set up")).toBeVisible();
  await expect(page.getByText('“Demo Lab” is ready, with you as Sam Rivera (@sam) on This laptop.')).toBeVisible();
  await expect(page.getByText('Created 2 projects.')).toBeVisible();
  await expectNoAxeViolations(page, 'done');
  await page.getByRole('button', { name: 'Go to Home' }).click();
  await expect(page).toHaveURL(new RegExp(`/w/${ws}/home$`));
  await expect(heading(page, 'Home')).toBeVisible();
  await expect(page.getByTestId('me')).toHaveText('Sam Rivera');

  const me = await request.get(`${FIRST_RUN_HUB}/v1/me`, { headers: AUTH });
  expect(me.status()).toBe(200);
  expect(await me.json()).toMatchObject({ kind: 'human', name: 'Sam Rivera', handle: '@sam' });
  const after = (await (await request.get(`${FIRST_RUN_HUB}/v1/workspace`, { headers: AUTH })).json()) as {
    workspace: { name: string };
    setup_needed?: boolean;
  };
  expect(after.workspace.name).toBe('Demo Lab');
  expect(after.setup_needed).toBeUndefined();

  // The projects at the scan's roots, keyed from their names, and their workstreams where the scan
  // found them: a folder, or the project's root on a branch.
  const machines = (await (await request.get(`${FIRST_RUN_HUB}/v1/machines`, { headers: AUTH })).json()) as { id: string }[];
  const machine = machines[0]?.id;
  const projects = (await (await request.get(`${FIRST_RUN_HUB}/v1/projects`, { headers: AUTH })).json()) as {
    id: string;
    key: string;
    name: string;
    root?: { machine: string; path: string };
  }[];
  expect(projects.map((p) => [p.key, p.name, p.root])).toEqual([
    ['DP', 'Diffusion paper', { machine, path: '/home/sam/work/diffusion-paper' }],
    ['LT', 'lab-tools', { machine, path: '/home/sam/work/lab-tools' }],
  ]);
  const workstreams = (await (await request.get(`${FIRST_RUN_HUB}/v1/workstreams`, { headers: AUTH })).json()) as {
    project: string;
    name: string;
    locations: { machine: string; path: string; branch?: string }[];
  }[];
  const [dp, lt] = projects.map((p) => p.id);
  expect(workstreams.map((w) => [w.project, w.name, w.locations])).toEqual([
    [dp, 'paper', [{ machine, path: '/home/sam/work/diffusion-paper/paper' }]],
    [dp, 'revision-2', [{ machine, path: '/home/sam/work/diffusion-paper', branch: 'revision-2' }]],
    [lt, 'parsers', [{ machine, path: '/home/sam/work/lab-tools', branch: 'parsers' }]],
  ]);

  // A fresh start lands on Home, not on setup.
  await page.goto('/');
  await expect(page).toHaveURL(new RegExp(`/w/${ws}/home$`));
  await expect(heading(page, 'Home')).toBeVisible();
});
