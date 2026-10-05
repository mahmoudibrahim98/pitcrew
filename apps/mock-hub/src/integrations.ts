// GitHub and Jira integrations (api-v1.md, "Integrations"); a sync only reads, over the recorded fixtures
// in `apps/mock-hub/fixtures/` (the sync crates' format; `pitcrewd serve --integration-fixtures`
// reads the same files). Device tokens only. Credentials stay in memory and are never returned.
//
// A sync runs at once, when an integration is added and on `POST …/sync`, with the daemon's rules,
// over a snapshot of what each sync read (as the sync crates keep one), so only what changed
// upstream since the last read acts:
// - open issues in a linked scope become tasks of the workstream that links their milestone or
//   epic, else their repository or project; so does an open issue a later read moves under a
//   milestone or epic that routes to a workstream; issues closed before they were first seen are
//   skipped;
// - upstream-owned fields (title, description, labels) are overwritten when upstream changes them;
// - an upstream close or reopen moves the task, following the sync's `can_move` (in-progress work
//   is never touched: a conflict ask instead); a task a person moved stays where they put it;
// - a milestone or epic seen closing ships the workstreams that link it, unless one of their tasks
//   is in progress (a conflict ask); one first seen closed ships nothing;
// - a merged pull request is noted on the task it closes.
// Everything a sync changes is authored by its integration's own member, `@sync` (an agent of the
// person who added it; `@tracker-sync` when `@sync` is another person's).
// Outward writes are `writes.ts`'s; what a sent one changed is laid over the fixtures here
// (`changeUpstream`), so the next sync agrees with it.
//
// The fixtures are read again at each sync and test, from `fixtures/` or the folder `useFixtures`
// names: a file whose name sorts first changes what "upstream" says (tests/conformance does).

import { readFileSync, readdirSync } from 'node:fs';
import { join } from 'node:path';
import { canMove } from './rules.ts';
import type { Hub } from './state.ts';
import {
  CREDENTIAL_SOURCES,
  JIRA_DEPLOYMENTS,
  type Ask,
  type ExternalRef,
  type Integration,
  type IntegrationCheck,
  type IntegrationSettings,
  type Member,
  type MemberId,
  type ScopeCheck,
  type SyncCounts,
  type SyncProblem,
  type Task,
  type TaskStatus,
  type Workstream,
} from './types.ts';
import { ulid } from './ulid.ts';
import { ApiFailure, conflict, invalid, isRecord, notFound } from './validate.ts';

const FIXTURES = new URL('../fixtures/', import.meta.url);
const SEPARATOR = '### pitcrew-github-fixture ###';
const GITHUB_API = 'https://api.github.com';
const SYNC_HANDLE = '@sync';
const SYNC_FALLBACK = '@tracker-sync';
const MAX_SCOPES = 50;
const MAX_LINKS = 16;

export interface Exchange {
  url: string;
  status: number;
  headers: Map<string, string>;
  body: string;
}

/**
 * Every exchange in the fixtures of `dir`, by method and URL (`GET https://…`); the first of two for
 * one method and URL, by file name, wins.
 */
function fixtures(dir: URL | string): Map<string, Exchange> {
  const exchanges = new Map<string, Exchange>();
  for (const name of readdirSync(dir).filter((n) => n.endsWith('.fixture')).sort()) {
    const text = readFileSync(typeof dir === 'string' ? join(dir, name) : new URL(name, dir), 'utf8');
    for (const block of text.split(SEPARATOR).map((b) => b.trim()).filter(Boolean)) {
      const lines = block.split('\n');
      const [method = '', url = ''] = (lines[0] ?? '').split(' ');
      let i = 1;
      while (i < lines.length && lines[i] !== '') i++;
      i++;
      const status = Number((lines[i] ?? '').split(' ')[1]);
      i++;
      const headers = new Map<string, string>();
      while (i < lines.length && lines[i] !== '') {
        const line = lines[i] ?? '';
        const colon = line.indexOf(':');
        headers.set(line.slice(0, colon).trim().toLowerCase(), line.slice(colon + 1).trim());
        i++;
      }
      const body = lines.slice(i + 1).join('\n');
      const key = `${method} ${url}`;
      if (!exchanges.has(key)) exchanges.set(key, { url, status, headers, body });
    }
  }
  return exchanges;
}

/** What "upstream" answers now, for one sync, test or write, by method and URL. */
type Upstream = Map<string, Exchange>;

function upstreamOf(hub: Hub): Upstream {
  return fixtures(state(hub).fixtures);
}

/**
 * The recorded answer to `method url`, if the fixtures hold one now: its URL, else its URL without
 * a `since=` parameter, as the daemon's fixture transport answers.
 */
export function exchange(hub: Hub, method: string, url: string): Exchange | undefined {
  const upstream = upstreamOf(hub);
  const exact = upstream.get(`${method} ${url}`);
  if (exact !== undefined) return exact;
  const [base, query] = url.split('?', 2);
  if (query === undefined) return undefined;
  const kept = query.split('&').filter((pair) => !pair.startsWith('since='));
  return upstream.get(`${method} ${kept.length === 0 ? base : `${base}?${kept.join('&')}`}`);
}

function json(exchange: Exchange | undefined): unknown {
  if (exchange === undefined || exchange.status !== 200) return undefined;
  try {
    return JSON.parse(exchange.body);
  } catch {
    return undefined;
  }
}

// ─── State ──────────────────────────────────────────────────────────────────────────────────────

/** An issue's upstream-owned fields as the last sync read them. */
interface Snapshot {
  title: string;
  body: string;
  labels: string[];
  open: boolean;
  parent: string | undefined;
}

interface Record {
  integration: Integration;
  /** The member this integration's sync acts as. */
  member: MemberId;
  secret: string | undefined;
  titles: Map<string, string>;
  /** Issues as last read, by key: what a read diffs against ("closed before first seen" too). */
  snapshots: Map<string, Snapshot>;
  /** Milestones and epics as last read: open or not, by key. Never reset by a link change. */
  parents: Map<string, boolean>;
  /** Link keys each scope was last synced with. */
  linked: Map<string, string>;
}

interface State {
  records: Record[];
  fixtures: URL | string;
}

const states = new WeakMap<Hub, State>();

function state(hub: Hub): State {
  let s = states.get(hub);
  if (s === undefined) {
    s = { records: [], fixtures: FIXTURES };
    states.set(hub, s);
  }
  return s;
}

/** Reads "upstream" from the fixtures in `dir` instead of `fixtures/` (tests). */
export function useFixtures(hub: Hub, dir: string): void {
  state(hub).fixtures = dir;
}

function record(hub: Hub, id: string): Record {
  const found = state(hub).records.find((r) => r.integration.id === id.toUpperCase());
  if (found === undefined) throw notFound(`No integration ${id}.`);
  return found;
}

/** The member the integration `id`'s sync acts as, while it is connected. */
export function syncMemberOf(hub: Hub, id: string): MemberId | undefined {
  return state(hub).records.find((r) => r.integration.id === id)?.member;
}

/** Whether `member` is any connected integration's sync member. */
export function isSyncMember(hub: Hub, member: MemberId): boolean {
  return state(hub).records.some((r) => r.member === member);
}

/** The integration with this id, if it is still connected. */
export function integrationById(hub: Hub, id: string): Integration | undefined {
  return state(hub).records.find((r) => r.integration.id === id)?.integration;
}

/** The integration that syncs `container` (a repository or Jira project), and its spelling of it. */
export function integrationFor(
  hub: Hub,
  system: string,
  container: string,
): { integration: Integration; container: string } | undefined {
  for (const r of state(hub).records) {
    const settings = r.integration.settings;
    if (settings.kind === 'github' && system === 'github') {
      const repo = settings.repos.find((x) => x.toLowerCase() === container.toLowerCase());
      if (repo !== undefined) return { integration: r.integration, container: repo };
    }
    if (settings.kind === 'jira' && system === 'jira' && settings.projects.includes(container)) {
      return { integration: r.integration, container };
    }
  }
  return undefined;
}

/** What a sent write changed upstream, by issue key: the mock's copy of upstream (see `sync`). */
export interface Overlay {
  title?: string;
  body?: string;
  labels?: string[];
  parent?: string;
  open?: boolean;
}

const overlays = new WeakMap<Hub, Map<string, Overlay>>();

/** Records what a sent write changed on `key`, so the next sync and the next write see it. */
export function changeUpstream(hub: Hub, key: string, change: Overlay): void {
  let map = overlays.get(hub);
  if (map === undefined) {
    map = new Map();
    overlays.set(hub, map);
  }
  map.set(key, { ...map.get(key), ...change });
}

/** `item` with what sent writes changed laid over it. */
export function withOverlay(hub: Hub, item: Item): Item {
  const change = overlays.get(hub)?.get(item.key);
  if (change === undefined) return item;
  const out: Item = { ...item };
  // What PitCrew sent is exactly what upstream then holds.
  if (change.title !== undefined) {
    out.title = change.title;
    out.titleExact = true;
  }
  if (change.body !== undefined) {
    out.body = change.body;
    out.bodyExact = true;
  }
  if (change.labels !== undefined) out.labels = [...change.labels].sort();
  if (change.parent !== undefined) out.parent = change.parent;
  if (change.open !== undefined) out.open = change.open;
  return out;
}

/** One issue as upstream has it now (the fixtures, then what writes changed), if it is known. */
export function upstreamIssue(hub: Hub, integration: Integration, key: string): Item | undefined {
  const settings = integration.settings;
  const upstream = upstreamOf(hub);
  const read = settings.kind === 'github' ? readGithub(upstream, settings) : readJira(upstream, settings);
  const item = read.items.find((i) => i.key === key);
  return item === undefined ? undefined : withOverlay(hub, item);
}

// ─── Links ──────────────────────────────────────────────────────────────────────────────────────

const GITHUB_NAME = /^[A-Za-z0-9._-]{1,100}$/;
const isGithubName = (name: string): boolean => GITHUB_NAME.test(name) && name !== '.' && name !== '..';
const isRepo = (repo: string): boolean => {
  const parts = repo.split('/');
  return parts.length === 2 && isGithubName(parts[0] ?? '') && isGithubName(parts[1] ?? '');
};
const isJiraProject = (key: string): boolean => /^[A-Z][A-Z0-9_]{0,254}$/.test(key);
const isSyncedJiraProject = (key: string): boolean => /^[A-Z][A-Z0-9]{1,9}$/.test(key);

type Scope =
  | { kind: 'repo'; repo: string }
  | { kind: 'milestone'; repo: string; key: string }
  | { kind: 'project'; project: string }
  | { kind: 'epic'; project: string; key: string };

/** What a link names, as `pitcrew_hub_work::links::scope_of` says. */
export function scopeOf(link: ExternalRef): Scope | undefined {
  if (link.system === 'github') {
    const at = link.key.indexOf('#milestone:');
    if (at < 0) return isRepo(link.key) ? { kind: 'repo', repo: link.key } : undefined;
    const repo = link.key.slice(0, at);
    const number = link.key.slice(at + '#milestone:'.length);
    return isRepo(repo) && /^[0-9]+$/.test(number) ? { kind: 'milestone', repo, key: link.key } : undefined;
  }
  if (link.system === 'jira') {
    const dash = link.key.lastIndexOf('-');
    if (dash < 0) return isJiraProject(link.key) ? { kind: 'project', project: link.key } : undefined;
    const project = link.key.slice(0, dash);
    const number = link.key.slice(dash + 1);
    return isJiraProject(project) && /^[0-9]{1,18}$/.test(number)
      ? { kind: 'epic', project, key: link.key }
      : undefined;
  }
  return undefined;
}

const HIDDEN = /[­؜ᅟᅠ឴឵᠋-᠏​-‏‪-‮⁠-⁯ㅤ︀-️﻿ﾠ￰-￸]/u;
// eslint-free: the mock's own copy of the hidden set's spirit; the daemon uses the protocol's.
const CONTROL = /[\u0000-\u001f\u007f-\u009f]/u;

function safeUrl(url: string): boolean {
  if (Buffer.byteLength(url) > 2048 || HIDDEN.test(url) || CONTROL.test(url)) return false;
  if (!url.startsWith('https://')) return false;
  const authority = url.slice('https://'.length).split(/[/?#]/)[0] ?? '';
  return authority !== '' && !authority.includes('@') && !authority.includes('\\');
}

/** Checks a workstream's new links (api-v1.md, "Linking a workstream upstream"). */
export function checkLinks(value: unknown): ExternalRef[] {
  if (!Array.isArray(value)) throw invalid('external must be an array of links.');
  if (value.length > MAX_LINKS) throw invalid(`A workstream has at most ${MAX_LINKS} links.`);
  const seen = new Set<string>();
  return value.map((item, i) => {
    if (!isRecord(item)) throw invalid(`external[${i}] must be an object.`);
    const { system, key, url } = item;
    if (typeof system !== 'string' || !['github', 'jira', 'linear', 'gitlab'].includes(system)) {
      throw invalid(`external[${i}].system is not a known system.`);
    }
    if (typeof key !== 'string' || key === '' || [...key].length > 300 || CONTROL.test(key) || HIDDEN.test(key)) {
      throw invalid(`external[${i}].key must be 1 to 300 characters, without control or hidden characters.`);
    }
    if (url !== undefined && url !== null && (typeof url !== 'string' || !safeUrl(url))) {
      throw invalid(`external[${i}].url must be an https:// URL of at most 2048 bytes, without a user name or password.`);
    }
    if (seen.has(`${system}\u0000${key}`)) throw invalid(`external[${i}] repeats a link already in the list.`);
    seen.add(`${system}\u0000${key}`);
    const link: ExternalRef = { system: system as ExternalRef['system'], key };
    if (typeof url === 'string') link.url = url;
    return link;
  });
}

// ─── Checks ─────────────────────────────────────────────────────────────────────────────────────

function httpsRoot(value: unknown, field: string): string {
  const bad = (): ApiFailure =>
    invalid(`${field} must be an https:// URL of at most 2048 bytes, with no user name, password, query or fragment.`);
  if (typeof value !== 'string') throw bad();
  const text = value.trim();
  if (Buffer.byteLength(text) > 2048 || CONTROL.test(text)) throw bad();
  let url: URL;
  try {
    url = new URL(text);
  } catch {
    throw bad();
  }
  if (url.protocol !== 'https:' || url.hostname === '' || url.username || url.password || url.search || url.hash) {
    throw bad();
  }
  return url.href.replace(/\/+$/, '');
}

function distinct(value: unknown, field: string, valid: (v: string) => boolean, rule: string): string[] {
  if (!Array.isArray(value) || value.length === 0 || value.length > MAX_SCOPES) {
    throw invalid(`${field} needs 1 to ${MAX_SCOPES} entries.`);
  }
  const seen = new Set<string>();
  return value.map((item, i) => {
    const text = typeof item === 'string' ? item.trim() : '';
    if (!valid(text)) throw invalid(`${field}[${i}] must be ${rule}.`);
    if (seen.has(text.toLowerCase())) throw invalid(`${field}[${i}] repeats an entry.`);
    seen.add(text.toLowerCase());
    return text;
  });
}

function checkSettings(value: unknown, credential: string): IntegrationSettings {
  if (!isRecord(value)) throw invalid('settings must be an object tagged by kind.');
  if (value['kind'] === 'github') {
    const settings: IntegrationSettings = {
      kind: 'github',
      repos: distinct(value['repos'], 'repos', isRepo, 'owner/repo'),
    };
    if (value['api_base'] !== undefined && value['api_base'] !== null) {
      const base = httpsRoot(value['api_base'], 'api_base');
      // github.com's own API root is the default, kept as none (as the daemon does).
      if (base.toLowerCase() !== GITHUB_API) settings.api_base = base;
    }
    return settings;
  }
  if (value['kind'] === 'jira') {
    if (credential === 'gh_cli') throw invalid('credential gh_cli is for GitHub only; Jira needs stored.');
    const deployment = value['deployment'];
    if (typeof deployment !== 'string' || !(JIRA_DEPLOYMENTS as readonly string[]).includes(deployment)) {
      throw invalid('deployment must be cloud or data_center.');
    }
    const settings: IntegrationSettings = {
      kind: 'jira',
      deployment: deployment as 'cloud' | 'data_center',
      site: httpsRoot(value['site'], 'site'),
      projects: distinct(value['projects'], 'projects', isSyncedJiraProject, 'a Jira project key'),
    };
    const email = value['email'];
    if (deployment === 'cloud') {
      if (typeof email !== 'string') throw invalid('email is required for Jira Cloud.');
      const trimmed = email.trim();
      if ([...trimmed].length > 254 || !/^[^@\s]+@[^\s]*[^@\s]$/u.test(trimmed) || CONTROL.test(trimmed)) {
        throw invalid('email must be an e-mail address of at most 254 characters.');
      }
      settings.email = trimmed;
    } else if (email !== undefined && email !== null) {
      throw invalid('email is for Jira Cloud only; Data Center uses a personal access token.');
    }
    const field = value['epic_link_field'];
    if (field !== undefined && field !== null) {
      if (typeof field !== 'string' || !/^customfield_[0-9]{1,18}$/.test(field.trim())) {
        throw invalid('epic_link_field must be customfield_<digits>.');
      }
      settings.epic_link_field = field.trim();
    }
    return settings;
  }
  throw invalid('settings.kind must be github or jira.');
}

/**
 * The scopes a connection syncs, without their host: links and task sources name a repository or
 * an issue key without one, so the same repository or project on two hosts would move each
 * other's tasks.
 */
function scopeKeys(settings: IntegrationSettings): string[] {
  return settings.kind === 'github'
    ? settings.repos.map((r) => `github:${r}`.toLowerCase())
    : settings.projects.map((p) => `jira:${p}`.toLowerCase());
}

// ─── Views ──────────────────────────────────────────────────────────────────────────────────────

function containerOf(settings: IntegrationSettings, scope: Scope): string | undefined {
  if (settings.kind === 'github' && (scope.kind === 'repo' || scope.kind === 'milestone')) {
    return settings.repos.find((r) => r.toLowerCase() === scope.repo.toLowerCase());
  }
  if (settings.kind === 'jira' && (scope.kind === 'project' || scope.kind === 'epic')) {
    return settings.projects.find((p) => p === scope.project);
  }
  return undefined;
}

function linksOf(hub: Hub, rec: Record): { workstream: Workstream; link: ExternalRef; container: string }[] {
  const out: { workstream: Workstream; link: ExternalRef; container: string }[] = [];
  const system = rec.integration.settings.kind;
  for (const workstream of hub.workstreams) {
    for (const link of workstream.external) {
      if (link.system !== system) continue;
      const scope = scopeOf(link);
      const container = scope === undefined ? undefined : containerOf(rec.integration.settings, scope);
      if (container !== undefined) out.push({ workstream, link, container });
    }
  }
  return out;
}

function view(hub: Hub, rec: Record): Integration {
  const integration = structuredClone(rec.integration);
  integration.credential.stored = integration.credential.source === 'stored' && rec.secret !== undefined;
  integration.links = linksOf(hub, rec).map(({ workstream, link }) => {
    const title = rec.titles.get(link.key);
    return title === undefined ? { workstream: workstream.id, scope: link } : { workstream: workstream.id, scope: link, title };
  });
  return integration;
}

// ─── Routes ─────────────────────────────────────────────────────────────────────────────────────

export interface Reply {
  status: number;
  body?: unknown;
}

export function list(hub: Hub): Reply {
  return { status: 200, body: state(hub).records.map((r) => view(hub, r)) };
}

export function get(hub: Hub, id: string): Reply {
  return { status: 200, body: view(hub, record(hub, id)) };
}

/** The sync's member for `owner`, found or added (`pitcrew_hub_work`'s `ensure_sync_member`). */
function ensureSyncMember(hub: Hub, owner: MemberId): MemberId {
  const person = hub.findMember(owner);
  if (person === undefined || person.kind !== 'human') {
    throw invalid(`${owner} is not a person of this workspace; only a person owns the tracker sync.`);
  }
  let free: string | undefined;
  for (const handle of [SYNC_HANDLE, SYNC_FALLBACK]) {
    const holder = hub.members.find((m) => m.handle === handle);
    if (holder?.kind === 'agent' && holder.owner === owner) return holder.id;
    if (holder === undefined) free ??= handle;
  }
  if (free === undefined) throw conflict(`${SYNC_HANDLE} and ${SYNC_FALLBACK} are both other members' handles.`);
  const member: Member = { id: ulid(), kind: 'agent', handle: free, name: 'Tracker sync', owner };
  hub.members.push(member);
  hub.append(owner, { type: 'member_added', data: { member } });
  return member.id;
}

export function add(hub: Hub, caller: MemberId, body: unknown): Reply {
  if (!isRecord(body)) throw invalid('body must be a JSON object');
  const name = typeof body['name'] === 'string' ? body['name'].trim() : '';
  if (name === '' || [...name].length > 80 || CONTROL.test(name)) {
    throw invalid('name must be 1 to 80 characters after trimming, without control characters.');
  }
  const credential = body['credential'];
  if (typeof credential !== 'string' || !(CREDENTIAL_SOURCES as readonly string[]).includes(credential)) {
    throw invalid('credential must be gh_cli or stored.');
  }
  const interval = body['interval_minutes'] ?? 15;
  if (typeof interval !== 'number' || !Number.isInteger(interval) || interval < 5 || interval > 1440) {
    throw invalid('interval_minutes must be 5 to 1440.');
  }
  const settings = checkSettings(body['settings'], credential);
  const wanted = new Set(scopeKeys(settings));
  if (state(hub).records.some((r) => scopeKeys(r.integration.settings).some((k) => wanted.has(k)))) {
    throw conflict(
      'Another integration already syncs one of these repositories or projects, on this host or another: links and tasks name a repository or project without its host.',
    );
  }
  const member = ensureSyncMember(hub, caller);
  const now = Date.now();
  const rec: Record = {
    integration: {
      id: ulid(),
      name,
      settings,
      credential: { source: credential as 'gh_cli' | 'stored', stored: false },
      interval_minutes: interval,
      added_by: caller,
      added_at: now,
      status: { running: false, problems: [], next_at: now },
      links: [],
    },
    member,
    secret: undefined,
    titles: new Map(),
    snapshots: new Map(),
    parents: new Map(),
    linked: new Map(),
  };
  state(hub).records.push(rec);
  sync(hub, rec);
  return { status: 201, body: view(hub, rec) };
}

export function remove(hub: Hub, id: string): Reply {
  const rec = record(hub, id);
  const s = state(hub);
  s.records.splice(s.records.indexOf(rec), 1);
  return { status: 204 };
}

export function setCredential(hub: Hub, id: string, body: unknown): Reply {
  const rec = record(hub, id);
  const secret = isRecord(body) && typeof body['secret'] === 'string' ? body['secret'].trim() : '';
  if (secret === '' || [...secret].length > 4096 || /\s/u.test(secret) || CONTROL.test(secret)) {
    throw invalid('secret must be 1 to 4096 characters, without whitespace or control characters.');
  }
  if (rec.integration.credential.source === 'gh_cli') {
    throw conflict("This integration reads `gh auth token` on the hub's machine and keeps no secret.");
  }
  rec.secret = secret;
  sync(hub, rec);
  return { status: 204 };
}

export function syncNow(hub: Hub, id: string): Reply {
  const rec = record(hub, id);
  sync(hub, rec);
  return { status: 202, body: view(hub, rec) };
}

function credentialProblem(rec: Record): string | undefined {
  if (rec.integration.credential.source === 'stored' && rec.secret === undefined) {
    return 'No credential yet: add one for this integration in the desktop app.';
  }
  return undefined;
}

/** Why the integration `id` has no credential to write with, if it has none. */
export function missingCredential(hub: Hub, id: string): string | undefined {
  const rec = state(hub).records.find((r) => r.integration.id === id);
  return rec === undefined ? 'Its integration was removed.' : credentialProblem(rec);
}

export function test(hub: Hub, id: string): Reply {
  const rec = record(hub, id);
  const at = Date.now();
  const missing = credentialProblem(rec);
  if (missing !== undefined) {
    const check: IntegrationCheck = { ok: false, at, checks: [{ scope: '', ok: false, message: missing }], warnings: [] };
    return { status: 200, body: check };
  }
  const checks: ScopeCheck[] = [{ scope: '', ok: true, message: 'The credential works.' }];
  const warnings: string[] = [];
  const settings = rec.integration.settings;
  const upstream = upstreamOf(hub);
  const read = (url: string): Exchange | undefined => upstream.get(`GET ${url}`);
  if (settings.kind === 'github') {
    const base = settings.api_base ?? GITHUB_API;
    for (const repo of settings.repos) {
      const reply = read(`${base}/repos/${repo}`);
      const body = json(reply);
      if (isRecord(body)) {
        const permissions = isRecord(body['permissions']) ? body['permissions'] : {};
        const canWrite = permissions['push'] === true || permissions['admin'] === true || permissions['maintain'] === true;
        checks.push({ scope: repo, ok: true, message: canWrite ? 'Readable. This credential could also change it; PitCrew only reads.' : 'Readable.' });
        if (canWrite) {
          warnings.push(`The credential can change ${repo}. PitCrew only reads: a fine-grained token with read-only access to these repositories is safer.`);
        }
      } else {
        checks.push({ scope: repo, ok: false, message: 'Not found, or this credential cannot read it.' });
      }
      const scopes = (reply?.headers.get('x-oauth-scopes') ?? '').split(',').map((x) => x.trim());
      const broad = scopes.filter((x) => ['repo', 'public_repo', 'workflow', 'delete_repo'].includes(x) || x.startsWith('write:') || x.startsWith('admin:'));
      if (broad.length > 0 && !warnings.some((w) => w.startsWith("This token's scopes"))) {
        warnings.push(`This token's scopes (${broad.join(', ')}) reach further than reading these repositories. A fine-grained, read-only token for them is safer.`);
      }
    }
  } else {
    const api = `${settings.site}/rest/api/${settings.deployment === 'cloud' ? 3 : 2}`;
    if (json(read(`${api}/myself`)) === undefined) {
      checks[0] = { scope: '', ok: false, message: 'Jira refused the credential.' };
    }
    for (const project of settings.projects) {
      const ok = json(read(`${api}/project/${project}`)) !== undefined;
      checks.push({ scope: project, ok, message: ok ? 'Readable.' : 'Not found, or this credential cannot read it.' });
    }
  }
  const check: IntegrationCheck = { ok: checks.every((c) => c.ok), at, checks, warnings };
  return { status: 200, body: check };
}

// ─── Sync ───────────────────────────────────────────────────────────────────────────────────────

/** One upstream issue, as either tracker reports it. */
export interface Item {
  key: string;
  url: string;
  title: string;
  body: string;
  labels: string[];
  open: boolean;
  /** Its milestone's or epic's key. */
  parent?: string;
  /** Whether the hub holds `title` exactly as upstream has it (the daemon's `title_lossless`). */
  titleExact: boolean;
  /** Whether `body` is upstream's whole body or description (the daemon's `body_lossless`). */
  bodyExact: boolean;
}

/** Whether `text` reaches the hub as is: nothing hidden to strip, and within `max` characters. */
function exact(text: string, max: number): boolean {
  return !HIDDEN.test(text) && [...text].length <= max;
}

/** ADF as PitCrew writes it: one paragraph of plain text per non-empty line. */
export function adf(text: string): unknown {
  return {
    type: 'doc',
    version: 1,
    content: text
      .split('\n')
      .filter((l) => l.trim() !== '')
      .map((l) => ({ type: 'paragraph', content: [{ type: 'text', text: l }] })),
  };
}

/** The daemon's `description_is_lossless`: `text` is the whole description `raw`. */
export function descriptionExact(raw: unknown, text: string): boolean {
  if (raw === undefined || raw === null) return text === '';
  if (typeof raw === 'string') return raw === text && exact(raw, 65_536);
  return isRecord(raw) && JSON.stringify(adf(text)) === JSON.stringify(raw) && exact(text, 65_536);
}

/** The labels the hub holds of upstream's (the daemon's `fit_labels`). */
export function heldLabels(labels: string[]): string[] {
  return fitLabels(labels);
}

function fit(text: string, max: number): string {
  return [...text.replace(new RegExp(HIDDEN.source, 'gu'), '').trim()].slice(0, max).join('').trim();
}

function fitLabels(labels: string[]): string[] {
  const out: string[] = [];
  for (const label of labels) {
    const fitted = fit(label, 64);
    if (fitted !== '' && !out.includes(fitted)) out.push(fitted);
    if (out.length === 32) break;
  }
  return out;
}

export function adfText(node: unknown, out: string[] = []): string[] {
  if (isRecord(node)) {
    if (typeof node['text'] === 'string') out.push(node['text']);
    if (Array.isArray(node['content'])) {
      for (const child of node['content']) adfText(child, out);
      if (node['type'] === 'paragraph') out.push('\n');
    }
  }
  return out;
}

interface Read {
  items: Item[];
  titles: Map<string, string>;
  /** Milestones or epics, by key: whether each is open, and its link. */
  parents: Map<string, { open: boolean; url: string | undefined }>;
  /** Pull requests merged upstream: the issue keys each closes, and its link. */
  merged: { url: string; closes: string[] }[];
  problems: SyncProblem[];
}

function readGithub(upstream: Upstream, settings: Extract<IntegrationSettings, { kind: 'github' }>): Read {
  const base = settings.api_base ?? GITHUB_API;
  const read = (url: string): unknown => json(upstream.get(`GET ${url}`));
  const out: Read = { items: [], titles: new Map(), parents: new Map(), merged: [], problems: [] };
  for (const repo of settings.repos) {
    const api = `${base}/repos/${repo}`;
    const milestones = read(`${api}/milestones?state=all&sort=due_on&direction=asc&per_page=100`);
    const issues = read(`${api}/issues?state=all&sort=updated&direction=asc&per_page=100`);
    const pulls = read(`${api}/pulls?state=all&sort=updated&direction=desc&per_page=100`);
    if (!Array.isArray(milestones) || !Array.isArray(issues) || !Array.isArray(pulls)) {
      out.problems.push({ scope: repo, message: 'no recorded fixture matches this repository' });
      continue;
    }
    for (const m of milestones) {
      if (!isRecord(m) || typeof m['title'] !== 'string') continue;
      const key = `${repo}#milestone:${String(m['number'])}`;
      out.titles.set(key, fit(m['title'], 512));
      out.parents.set(key, { open: m['state'] !== 'closed', url: typeof m['html_url'] === 'string' ? m['html_url'] : undefined });
    }
    for (const i of issues) {
      if (!isRecord(i) || i['pull_request'] !== undefined || typeof i['title'] !== 'string') continue;
      const milestone = isRecord(i['milestone']) ? `${repo}#milestone:${String(i['milestone']['number'])}` : undefined;
      const labels = Array.isArray(i['labels']) ? i['labels'].flatMap((l) => (isRecord(l) && typeof l['name'] === 'string' ? [l['name']] : [])) : [];
      const body = typeof i['body'] === 'string' ? i['body'] : '';
      const item: Item = {
        key: `${repo}#${String(i['number'])}`,
        url: typeof i['html_url'] === 'string' ? i['html_url'] : `https://github.com/${repo}/issues/${String(i['number'])}`,
        title: i['title'],
        body,
        labels: [...labels].sort(),
        open: i['state'] !== 'closed',
        titleExact: exact(i['title'], 512) && fit(i['title'], 500) === i['title'],
        bodyExact: exact(body, 65_536),
      };
      if (milestone !== undefined) item.parent = milestone;
      out.items.push(item);
    }
    for (const p of pulls) {
      if (!isRecord(p) || typeof p['merged_at'] !== 'string') continue;
      const body = typeof p['body'] === 'string' ? p['body'] : '';
      const closes = [...body.matchAll(/(?:close[sd]?|fix(?:e[sd])?|resolve[sd]?)\s+#(\d+)/giu)].map((m) => `${repo}#${m[1]}`);
      out.merged.push({ url: typeof p['html_url'] === 'string' ? p['html_url'] : '', closes });
    }
  }
  return out;
}

function readJira(upstream: Upstream, settings: Extract<IntegrationSettings, { kind: 'jira' }>): Read {
  const out: Read = { items: [], titles: new Map(), parents: new Map(), merged: [], problems: [] };
  const api = `${settings.site}/rest/api/${settings.deployment === 'cloud' ? 3 : 2}`;
  const fields = 'summary%2Cdescription%2Cstatus%2Cresolution%2Clabels%2Cassignee%2Cparent%2Cissuetype%2Cupdated';
  for (const project of settings.projects) {
    const jql = encodeURIComponent(`project in ("${project}") ORDER BY updated ASC, key ASC`).replace(/\(/g, '%28').replace(/\)/g, '%29');
    const path = settings.deployment === 'cloud' ? '/search/jql' : '/search';
    const paging = settings.deployment === 'cloud' ? '' : '&startAt=0';
    const page = json(upstream.get(`GET ${api}${path}?jql=${jql}${paging}&maxResults=100&fields=${fields}`));
    if (!isRecord(page) || !Array.isArray(page['issues'])) {
      out.problems.push({ scope: project, message: 'no recorded fixture matches this project' });
      continue;
    }
    for (const issue of page['issues']) {
      if (!isRecord(issue) || !isRecord(issue['fields']) || typeof issue['key'] !== 'string') continue;
      const f = issue['fields'];
      const summary = typeof f['summary'] === 'string' ? f['summary'] : '';
      const category = isRecord(f['status']) && isRecord(f['status']['statusCategory']) ? f['status']['statusCategory']['key'] : 'new';
      const epic = isRecord(f['issuetype']) && f['issuetype']['name'] === 'Epic';
      if (epic) {
        out.titles.set(issue['key'], fit(summary, 512));
        out.parents.set(issue['key'], { open: category !== 'done', url: `${settings.site}/browse/${issue['key']}` });
        continue;
      }
      const description = typeof f['description'] === 'string' ? f['description'] : adfText(f['description']).join('');
      const item: Item = {
        key: issue['key'],
        url: `${settings.site}/browse/${issue['key']}`,
        title: summary,
        body: description,
        labels: Array.isArray(f['labels']) ? f['labels'].filter((l): l is string => typeof l === 'string').sort() : [],
        open: category !== 'done',
        titleExact: exact(summary, 512) && fit(summary, 500) === summary,
        bodyExact: descriptionExact(f['description'], description),
      };
      if (isRecord(f['parent']) && typeof f['parent']['key'] === 'string') item.parent = f['parent']['key'];
      out.items.push(item);
    }
  }
  return out;
}

function sync(hub: Hub, rec: Record): void {
  const status = rec.integration.status;
  const started = Date.now();
  status.last_attempt_at = started;
  status.next_at = started + rec.integration.interval_minutes * 60_000;
  const missing = credentialProblem(rec);
  if (missing !== undefined) {
    status.problems = [{ scope: '', message: missing }];
    delete status.last_run;
    return;
  }
  const settings = rec.integration.settings;
  const upstream = upstreamOf(hub);
  const read = settings.kind === 'github' ? readGithub(upstream, settings) : readJira(upstream, settings);
  read.items = read.items.map((item) => withOverlay(hub, item));
  const counts: SyncCounts = { changes: read.items.length + read.merged.length, applied: 0, conflicts: 0, skipped: 0, malformed: 0 };
  const member = rec.member;
  const owner = rec.integration.added_by;
  const system = settings.kind;
  const tracker = system === 'github' ? 'GitHub' : 'Jira';
  for (const [key, title] of read.titles) rec.titles.set(key, title);

  // A scope whose links changed is read as if for the first time (its issues; not its milestones
  // or epics, so one closed long ago ships nothing when it is linked).
  const links = linksOf(hub, rec);
  for (const container of settings.kind === 'github' ? settings.repos : settings.projects) {
    const keys = links.filter((l) => l.container === container).map((l) => l.link.key).sort().join('\n');
    if (rec.linked.get(container) !== keys) {
      for (const key of [...rec.snapshots.keys()]) {
        const of = system === 'github' ? key.split('#')[0] : key.slice(0, key.lastIndexOf('-'));
        if (of?.toLowerCase() === container.toLowerCase()) rec.snapshots.delete(key);
      }
      rec.linked.set(container, keys);
    }
  }
  /** The workstream that links the item's milestone or epic, else its repository or project. */
  const route = (item: Item): Workstream | undefined => {
    const byParent = links.find((l) => item.parent !== undefined && l.link.key === item.parent);
    if (byParent !== undefined) return byParent.workstream;
    const container = system === 'github' ? item.key.split('#')[0] : item.key.slice(0, item.key.lastIndexOf('-'));
    return links.find((l) => {
      const scope = scopeOf(l.link);
      return (scope?.kind === 'repo' || scope?.kind === 'project') && l.container.toLowerCase() === (container ?? '').toLowerCase();
    })?.workstream;
  };
  const mirrored = (key: string): Task | undefined => hub.tasks.find((t) => t.source?.system === system && t.source.key === key);
  /** A `decision` ask to the person who added the integration, not raised twice while open. */
  const ask = (task: Task | undefined, title: string, reason: string, url: string | undefined): void => {
    if (hub.asks.some((a) => a.state === 'open' && a.from === member && a.task === task?.id && a.title === title)) return;
    const raised: Ask = {
      id: ulid(),
      kind: 'decision',
      from: member,
      to: owner,
      title,
      body: `${reason}. Nothing was changed in PitCrew; decide what to do here.${url === undefined ? '' : `\n\n${url}`}`,
      options: [],
      receipts: [],
      state: 'open',
      created: Date.now(),
    };
    if (task !== undefined) raised.task = task.id;
    hub.asks.push(raised);
    hub.append(member, { type: 'ask_raised', data: { ask: raised } });
    counts.conflicts++;
  };
  const move = (task: Task, to: TaskStatus, item: Item): void => {
    if (task.status === to) return;
    if (!canMove(task.status, to, { kind: 'sync' })) {
      ask(task, `${tracker}: ${item.key} changed, but ${task.key} cannot follow`, `${task.key} is ${task.status}, and a sync may not move it to ${to}`, item.url);
      return;
    }
    const from = task.status;
    task.status = to;
    hub.append(member, { type: 'task_moved', data: { task: task.id, from, to, mover: { kind: 'sync' } } });
    counts.applied++;
  };
  const create = (item: Item, workstream: Workstream): void => {
    const project = hub.findProject(workstream.project);
    if (project === undefined) return;
    const number = Math.max(0, ...hub.tasks.filter((t) => t.key.startsWith(`${project.key}-`)).map((t) => Number(t.key.split('-')[1]))) + 1;
    const task: Task = {
      id: ulid(),
      key: `${project.key}-${number}`,
      project: project.id,
      workstream: workstream.id,
      title: fit(item.title, 500) || item.key,
      description: item.body,
      status: 'todo',
      priority: 'none',
      labels: fitLabels(item.labels),
      blocked_by: [],
      source: { system, key: item.key, url: item.url },
      accept_auto: false,
      subtasks: [],
    };
    hub.tasks.push(task);
    hub.append(member, { type: 'task_created', data: { task } });
    counts.applied++;
  };

  for (const item of read.items) {
    const before = rec.snapshots.get(item.key);
    rec.snapshots.set(item.key, { title: item.title, body: item.body, labels: item.labels, open: item.open, parent: item.parent });
    const task = mirrored(item.key);
    if (task === undefined) {
      // A task is made from a first read of an open issue, or from a later read that moves it,
      // open, under a milestone or epic; either way only in a linked scope.
      const moved = before !== undefined && before.parent !== item.parent && item.parent !== undefined;
      const workstream = item.open && (before === undefined || moved) ? route(item) : undefined;
      if (workstream === undefined) counts.skipped++;
      else create(item, workstream);
      continue;
    }
    // Upstream-owned fields, and the workstream its milestone or epic routes to: only what
    // upstream changed since the last read (everything, on a first read).
    const patch: { title?: string; description?: string; labels?: string[]; workstream?: string } = {};
    const title = fit(item.title, 500) || item.key;
    if ((before === undefined || before.title !== item.title) && title !== task.title) patch.title = title;
    if ((before === undefined || before.body !== item.body) && item.body !== task.description) patch.description = item.body;
    const labels = fitLabels(item.labels);
    if ((before === undefined || JSON.stringify(before.labels) !== JSON.stringify(item.labels)) && JSON.stringify(labels) !== JSON.stringify(task.labels)) {
      patch.labels = labels;
    }
    if (before === undefined || before.parent !== item.parent) {
      const target = route(item);
      if (target !== undefined && target.project === task.project && target.id !== task.workstream) patch.workstream = target.id;
    }
    if (Object.keys(patch).length > 0) {
      Object.assign(task, patch);
      hub.append(member, { type: 'task_updated', data: { task: task.id, patch } });
      counts.applied++;
    }
    // Moves follow an upstream close or reopen, never the task's own status: a task a person
    // reopened stays reopened until upstream changes again.
    if (before === undefined ? !item.open : before.open !== item.open) move(task, item.open ? 'todo' : 'done', item);
  }

  // A milestone or epic seen closing ships the workstreams that link it; first seen closed, none.
  for (const [key, { open, url }] of read.parents) {
    const was = rec.parents.get(key);
    rec.parents.set(key, open);
    if (was !== true || open) continue;
    for (const workstream of hub.workstreams.filter((w) => w.external.some((l) => l.system === system && l.key === key))) {
      if (workstream.status === 'shipped' || workstream.status === 'dropped') continue;
      if (hub.tasks.some((t) => t.workstream === workstream.id && t.status === 'in_progress')) {
        const what = system === 'github' ? 'its milestone' : 'its epic';
        ask(
          undefined,
          `${tracker}: ${key} needs a decision`,
          `upstream closed ${what}, but "${workstream.name}" still has a task in progress, so a sync does not mark it shipped`,
          url,
        );
        continue;
      }
      workstream.status = 'shipped';
      hub.append(member, { type: 'workstream_changed', data: { workstream: workstream.id, status: 'shipped', health: workstream.health } });
      counts.applied++;
    }
  }

  for (const pr of read.merged) {
    for (const key of pr.closes) {
      const task = mirrored(key);
      if (task === undefined || pr.url === '') continue;
      const text = `Merged upstream: ${pr.url}`;
      const noted = hub.eventsAfter(0).some((e) => e.author === member && e.body.type === 'comment_posted' && e.body.data.task === task.id && e.body.data.text === text);
      if (noted) continue;
      hub.append(member, { type: 'comment_posted', data: { task: task.id, text, mentions: [] } });
      counts.applied++;
    }
  }
  status.problems = read.problems;
  status.last_run = counts;
  delete status.rate_limited_until;
  if (read.problems.length === 0) status.last_success_at = Date.now();
}
