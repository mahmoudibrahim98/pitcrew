import assert from 'node:assert/strict';
import { test } from 'node:test';
import { withServer } from './helpers.ts';
import { hooksDiff, installHooks, parseSafety } from '../src/onboarding.ts';

test('synthetic previews are exact, person-bound, stale-safe and idempotent', async () => {
  await withServer(async ({ hub }) => {
  const machine = hub.machines.find((machine) => machine.kind === 'local')!.id;
  const preview = hooksDiff(hub, 'synthetic-person', machine);
  const file = preview.files[0]!;
  assert.equal(hub.onboarding.files.get(file.path), file.before);
  assert.throws(() => installHooks(hub, 'second-person', machine, { revision: preview.revision }), /unknown/);
  hub.onboarding.files.set(file.path, 'synthetic concurrent edit');
  assert.throws(() => installHooks(hub, 'synthetic-person', machine, { revision: preview.revision }), /Preview again/);
  assert.equal(hub.onboarding.files.get(file.path), 'synthetic concurrent edit');
  hub.onboarding.files.set(file.path, file.before!);
  for (let i = 0; i < 2; i++) assert.deepEqual(installHooks(hub, 'synthetic-person', machine, { revision: preview.revision }), { installed: true, skipped: [] });
  assert.equal(hub.onboarding.files.get(file.path), file.after);
  const noChanges = hooksDiff(hub, 'synthetic-person', machine);
  assert.equal(noChanges.files.length, 0);
  assert.deepEqual(installHooks(hub, 'synthetic-person', machine, { revision: noChanges.revision }), { installed: false, skipped: [] });
  assert.throws(() => installHooks(hub, 'synthetic-person', 'unknown', { extra: 1 }), /revision/);
  hub.onboarding.previews.get(preview.revision)!.created -= 600_000;
  assert.throws(() => installHooks(hub, 'synthetic-person', machine, { revision: preview.revision }), /expired/);
  assert.throws(() => parseSafety({ permission_mode: 'default', back_office_enabled: false, back_office_caps: { max_auto_accept_per_hour: 1.5 } }), /cap/);
  });
});
