// Synthetic hook configurations only: never reads an agent home.
import { randomUUID } from 'node:crypto';
import { ApiFailure, invalid, isRecord } from './validate.ts';
import type { Hub } from './state.ts';

export interface SafetySettings {
  permissionMode: 'default' | 'plan' | 'accept-edits' | 'bypass-permissions';
  backOfficeEnabled: boolean;
  backOfficeCaps: { maxAutoAcceptPerHour: number };
}
export class Onboarding {
  safety: SafetySettings = { permissionMode: 'default', backOfficeEnabled: false, backOfficeCaps: { maxAutoAcceptPerHour: 20 } };
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
  machine(hub, id);
  if (!isRecord(body) || typeof body['revision'] !== 'string' || Object.keys(body).some((k) => k !== 'revision')) throw invalid('Supply the hook preview revision.');
  const preview = hub.onboarding.previews.get(body['revision']);
  if (!preview || preview.owner !== owner || preview.machine !== id || Date.now() - preview.created >= 600_000) throw new ApiFailure('conflict', 'The hook preview expired or is unknown.');
  for (const file of preview.files) {
    const current = hub.onboarding.files.get(file.path) ?? null;
    if (current !== file.before && current !== file.after) throw new ApiFailure('conflict', 'Preview again.');
  }
  for (const file of preview.files) hub.onboarding.files.set(file.path, file.after);
  return { installed: true };
}
export function parseSafety(body: unknown): SafetySettings {
  if (!isRecord(body) || !['default', 'plan', 'accept-edits', 'bypass-permissions'].includes(String(body['permissionMode'])) || typeof body['backOfficeEnabled'] !== 'boolean' || !isRecord(body['backOfficeCaps'])) throw invalid('Invalid safety settings.');
  if (Object.keys(body).some((k) => !['permissionMode', 'backOfficeEnabled', 'backOfficeCaps'].includes(k))) throw invalid('Unknown safety field.');
  const cap = body['backOfficeCaps']['maxAutoAcceptPerHour'];
  if (typeof cap !== 'number' || !Number.isInteger(cap) || cap < 0 || cap > 100 || Object.keys(body['backOfficeCaps']).some((k) => k !== 'maxAutoAcceptPerHour')) throw invalid('Invalid hourly cap.');
  return body as unknown as SafetySettings;
}
