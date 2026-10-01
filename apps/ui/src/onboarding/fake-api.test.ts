import { describe, expect, it } from 'vitest';
import { createFakeOnboardingApi } from './fake-api.ts';

const FAST = { speed: 0 };

describe('createFakeOnboardingApi', () => {
  it('checks a machine and reports a fixable row', async () => {
    const api = createFakeOnboardingApi(FAST);
    const result = await api.checkMachine({ kind: 'local' });
    expect(result.rows.length).toBeGreaterThan(0);
    const opencode = result.rows.find((r) => r.id === 'cli-opencode');
    expect(opencode?.status).toBe('missing');
    expect(opencode?.fixable).toBe(true);
  });

  it('fixing a row turns it ok, and remembers the fix on the next check', async () => {
    const api = createFakeOnboardingApi(FAST);
    const target = { kind: 'local' as const };
    await api.checkMachine(target);
    const fixed = await api.fixMachineRow(target, 'cli-opencode');
    expect(fixed.status).toBe('ok');
    const again = await api.checkMachine(target);
    expect(again.rows.find((r) => r.id === 'cli-opencode')?.status).toBe('ok');
  });

  it('rejects fixing a row that is not fixable', async () => {
    const api = createFakeOnboardingApi(FAST);
    const target = { kind: 'local' as const };
    await api.checkMachine(target);
    await expect(api.fixMachineRow(target, 'git')).rejects.toThrow();
  });

  it('streams install progress and a SLURM script preview only for the slurm launcher', async () => {
    const api = createFakeOnboardingApi(FAST);
    const sshEvents: string[] = [];
    await new Promise<void>((resolve) => {
      api.streamInstallHelper({ machine: { kind: 'ssh', host: 'hpc-login' }, launcher: 'slurm' }, (event) => {
        sshEvents.push(event.type);
        if (event.type === 'done') resolve();
      });
    });
    expect(sshEvents[0]).toBe('script-preview');
    expect(sshEvents.at(-1)).toBe('done');

    const directEvents: string[] = [];
    await new Promise<void>((resolve) => {
      api.streamInstallHelper({ machine: { kind: 'local' }, launcher: 'direct' }, (event) => {
        directEvents.push(event.type);
        if (event.type === 'done') resolve();
      });
    });
    expect(directEvents).not.toContain('script-preview');
  });

  it('cancelling a stream stops further events', async () => {
    const api = createFakeOnboardingApi({ speed: 1 });
    let count = 0;
    const streamed = api.streamScan({ machine: { kind: 'local' } }, () => {
      count += 1;
    });
    streamed.cancel();
    await new Promise((resolve) => setTimeout(resolve, 50));
    expect(count).toBe(0);
  });

  it('a dry-run import count matches the commit', async () => {
    const api = createFakeOnboardingApi(FAST);
    const dryRun = await api.importSessions({ mode: 'all' });
    const committed = await api.commitImport({ mode: 'all' });
    expect(committed.imported).toBe(dryRun.count);
    expect(await api.importSessions({ mode: 'none' })).toEqual({ count: 0 });
  });

  it('creates projects and workstreams only from the selection', async () => {
    const api = createFakeOnboardingApi(FAST);
    const result = await api.createFromScan([
      { suggestionId: 'sp-1', name: 'Paper', template: 'research', workstreams: [{ suggestionId: 'sw-1', name: 'Drafts' }] },
    ]);
    expect(result.projects).toHaveLength(1);
    expect(result.projects[0]?.name).toBe('Paper');
    expect(result.workstreams).toHaveLength(1);
    expect(result.workstreams[0]?.project).toBe(result.projects[0]?.id);
  });

  it('gives a hooks diff with at least one file', async () => {
    const api = createFakeOnboardingApi(FAST);
    const diff = await api.hooksDiff();
    expect(diff.files.length).toBeGreaterThan(0);
  });
});
