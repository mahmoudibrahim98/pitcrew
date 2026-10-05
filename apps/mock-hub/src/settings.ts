import type { Hub } from './state.ts';
import type { Member } from './types.ts';
import { conflict, forbidden, invalid, isRecord, notFound } from './validate.ts';

export function ownerOnly(hub: Hub, member: string) {
  if (hub.members.find((m) => m.kind === 'human')?.id !== member) throw forbidden('Only the workspace owner may change these settings.');
}
function object(body: unknown, fields: string[]): Record<string, unknown> {
  if (!isRecord(body) || Object.keys(body).some((k) => !fields.includes(k))) throw invalid('Supply only the supported settings fields.');
  return body;
}
export function name(value: unknown): string {
  if (typeof value !== 'string' || [...value.trim()].length < 1 || [...value.trim()].length > 80 || /[\u0000-\u001f\u007f-\u009f]/u.test(value.trim())) throw invalid('name must be 1 to 80 characters without controls.');
  return value.trim();
}
export function rename(hub: Hub, member: string, body: unknown) {
  ownerOnly(hub, member);
  hub.workspace.name = name(object(body, ['name']).name);
  return hub.workspace;
}
export function machine(hub: Hub, member: string, id: string, body: unknown) {
  ownerOnly(hub, member);
  const current = hub.machines.find((m) => m.id === id);
  if (!current) throw notFound('Unknown machine.');
  const display = name(object(body, ['name']).name);
  if ([...display].length > 60) throw invalid('Machine name must be at most 60 characters.');
  if (current.name !== display) { current.name = display; hub.append(member, { type: 'machine_added', data: { machine: { ...current } } }); }
  return current;
}
export function profile(hub: Hub, id: string, body: unknown): Member {
  const member = hub.members.find((m) => m.id === id);
  if (!member) throw notFound('Unknown person.');
  if (member.kind !== 'human') throw forbidden('A profile needs a person.');
  const input = object(body, ['name', 'handle', 'avatar']);
  const display = name(input.name);
  if (typeof input.handle !== 'string' || !/^@[a-z0-9_-]{1,32}$/.test(input.handle) || input.handle === '@office') throw invalid('Invalid handle.');
  const avatar = object(input.avatar, ['initials', 'colour']);
  if (typeof avatar.initials !== 'string' || [...avatar.initials.trim()].length < 1 || [...avatar.initials.trim()].length > 4 || /[\u0000-\u001f\u007f-\u009f\u00ad\u034f\u061c\u115f\u1160\u180e\u200b-\u200f\u2028-\u202e\u2060-\u2064\u2066-\u2069\u3164\ufe00-\ufe0f\ufeff\uffa0\ufff9-\ufffb\u{e0000}-\u{e007f}\u{e0100}-\u{e01ef}]/u.test(avatar.initials.trim())) throw invalid('Initials must be 1 to 4 visible characters.');
  if (typeof avatar.colour !== 'string' || !/^#[\da-f]{6}$/i.test(avatar.colour)) throw invalid('Colour must be #RRGGBB.');
  if (hub.members.some((m) => m.handle === input.handle && m.id !== id)) throw conflict('That handle is already taken.');
  const next = { ...member, name: display, handle: input.handle, avatar: { initials: avatar.initials.trim(), colour: avatar.colour.toLowerCase() } };
  if (JSON.stringify(member) !== JSON.stringify(next)) {
    Object.assign(member, next);
    hub.append(id, { type: 'member_added', data: { member: next } });
  }
  return next;
}
