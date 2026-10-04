// Synthetic hook configurations only: never reads an agent home.
import { randomUUID } from 'node:crypto';
import { ApiFailure, invalid, isRecord } from './validate.ts';
import type { Hub } from './state.ts';

export interface SafetySettings {
  permission_mode: 'default' | 'plan' | 'accept_edits' | 'bypass_permissions';
  back_office_enabled: boolean;
  back_office_caps: { max_auto_accept_per_hour: number };
}
export class Onboarding {
  safety: SafetySettings = { permission_mode: 'default', back_office_enabled: false, back_office_caps: { max_auto_accept_per_hour: 20 } };
  safetySaved = false;
  files = new Map<string, string>([['/home/sam/.claude/settings.json', '{\r\n  "hooks": {}\r\n}\r\n']]);
  previews = new Map<string, { owner: string; machine: string; created: number; files: { path: string; before: string | null; after: string }[] }>();
}
function machine(hub: Hub, id: string): void {
  if (!hub.machines.some((m) => m.id === id)) throw new ApiFailure('not_found', 'Unknown machine.');
  if (hub.machines.find((m) => m.kind === 'local')?.id !== id) throw new ApiFailure('unsupported', 'Hooks on another machine are not supported yet.');
}
export function hooksDiff(hub: Hub, owner: string, id: string) {
  machine(hub, id);
  const path = '/home/sam/.claude/settings.json';
  const after = '{\r\n  "hooks": {"Stop": [{"matcher":"", "hooks":[{"type":"command", "command":"/usr/local/bin/pitcrew hook claude Stop", "timeout":5}]}]}\r\n}\r\n';
  const before = hub.onboarding.files.get(path) ?? null;
  const files = before === after ? [] : [{ path, before, after }];
  const revision = randomUUID();
  for (const [key, value] of hub.onboarding.previews) if (Date.now() - value.created >= 600_000) hub.onboarding.previews.delete(key);
  if (hub.onboarding.previews.size >= 32) hub.onboarding.previews.delete(hub.onboarding.previews.keys().next().value!);
  hub.onboarding.previews.set(revision, { owner, machine: id, created: Date.now(), files });
  return { revision, files, engines: [{ engine: 'claude', status: files.length === 0 ? 'installed' : 'missing', detail: 'Synthetic Claude hook configuration.' }] };
}
export function installHooks(hub: Hub, owner: string, id: string, body: unknown) {
  if (!isRecord(body) || typeof body['revision'] !== 'string' || Object.keys(body).some((k) => k !== 'revision')) throw invalid('Supply the hook preview revision.');
  machine(hub, id);
  const preview = hub.onboarding.previews.get(body['revision']);
  if (!preview || preview.owner !== owner || preview.machine !== id || Date.now() - preview.created >= 600_000) throw new ApiFailure('conflict', 'The hook preview expired or is unknown.');
  for (const file of preview.files) {
    const current = hub.onboarding.files.get(file.path) ?? null;
    if (current !== file.before && current !== file.after) throw new ApiFailure('conflict', 'Preview again.');
  }
  for (const file of preview.files) hub.onboarding.files.set(file.path, file.after);
  return { installed: preview.files.length > 0, skipped: [] };
}
export function parseSafety(body: unknown): SafetySettings {
  if (!isRecord(body) || !['default', 'plan', 'accept_edits', 'bypass_permissions'].includes(String(body['permission_mode'])) || typeof body['back_office_enabled'] !== 'boolean' || !isRecord(body['back_office_caps'])) throw invalid('Invalid safety settings.');
  if (Object.keys(body).some((k) => !['permission_mode', 'back_office_enabled', 'back_office_caps'].includes(k))) throw invalid('Unknown safety field.');
  const cap = body['back_office_caps']['max_auto_accept_per_hour'];
  if (typeof cap !== 'number' || !Number.isInteger(cap) || cap < 0 || cap > 100 || Object.keys(body['back_office_caps']).some((k) => k !== 'max_auto_accept_per_hour')) throw invalid('Invalid hourly cap.');
  if (body['permission_mode'] === 'bypass_permissions') throw invalid('Bypass permissions cannot be saved as the workspace default while the runner disallows it.');
  return body as unknown as SafetySettings;
}
