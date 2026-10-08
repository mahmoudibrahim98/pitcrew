// Reversible session inclusion and the shared activity/stream visibility rule.
import type { Hub } from './state.ts';
import type { Event, Session, DemoRecaps } from './types.ts';
import { invalid, isRecord } from './validate.ts';
export interface ImportFilter { mode: 'all' | 'filtered' | 'none'; since?: string; engines: string[]; folders: string[] }
export interface ImportChoice { filter: ImportFilter; committed_at: number | null }
export function parseImport(value: unknown): ImportFilter {
  if (!isRecord(value) || !['all', 'filtered', 'none'].includes(String(value['mode']))) throw invalid('Invalid import mode.');
  const engines = value['engines'] ?? [];
  const folders = value['folders'] ?? [];
  if (!Array.isArray(engines) || !engines.every((v) => ['claude', 'codex', 'opencode'].includes(v)) ||
      !Array.isArray(folders) || !folders.every((v) => typeof v === 'string')) throw invalid('Invalid import dimensions.');
  const since = value['since'];
  if (since !== undefined && typeof since !== 'string') throw invalid('Invalid since date.');
  if (value['mode'] === 'filtered') {
    if (since !== undefined && (!/^\d{4}-\d{2}-\d{2}$/.test(since) || !Number.isFinite(Date.parse(since)) || new Date(since).toISOString().slice(0,10) !== since)) throw invalid('Invalid since date.');
    if (folders.some((v) => v.trim() === '')) throw invalid('Empty folder.');
  }
  return { mode: value['mode'] as ImportFilter['mode'], ...(since === undefined ? {} : { since }), engines, folders };
}
/** Sub-agents are followed up their chain of parents for at most this many sessions. */
const MAX_PARENT_CHAIN = 16;
/**
 * The top of `session`'s chain of parents: the first session whose parent the hub does not know
 * (none, or one it never saw), `session` itself when that is it. Undefined when the chain ends
 * nowhere (a loop, or one too long): the session then stands on its own, as clients show it.
 */
export function rootOf(hub: Pick<Hub, 'findSession'>, session: Session): Session | undefined {
  let at = session;
  for (let i = 0; i < MAX_PARENT_CHAIN; i += 1) {
    const parent = at.parent === undefined ? undefined : hub.findSession(at.parent);
    if (parent === undefined) return at;
    at = parent;
  }
  return undefined;
}
/** The session whose inclusion decides `session`'s: the top of its chain, else itself. */
export function deciding(hub: Pick<Hub, 'findSession'>, session: Session): Session {
  return rootOf(hub, session) ?? session;
}
/** Whether `session` is included; with `hub`, a sub-agent exactly when its parent is. */
export function includesSession(choice: ImportChoice, session: Session, hub?: Pick<Hub, 'findSession'>): boolean {
  if (hub !== undefined) session = deciding(hub, session);
  const f = choice.filter;
  if (f.mode === 'all') return true;
  if (f.mode === 'none') return choice.committed_at !== null && session.started > choice.committed_at;
  return (f.since === undefined || session.started >= Date.parse(f.since)) &&
    (f.engines.length === 0 || f.engines.includes(session.engine)) &&
    (f.folders.length === 0 || f.folders.some((folder) => {
      const prefix = folder.replaceAll('\\', '/').replace(/\/+$/, '');
      const cwd = session.cwd.replaceAll('\\', '/');
      return cwd === prefix || cwd.startsWith(prefix + '/');
    }));
}
function sessionIds(value: unknown): string[] {
  if (Array.isArray(value)) return value.flatMap(sessionIds);
  if (!isRecord(value)) return [];
  return Object.entries(value).flatMap(([key, v]) => [
    ...(key === 'session' ? typeof v === 'string' ? [v] : isRecord(v) && typeof v['id'] === 'string' ? [v['id']] : [] : []),
    ...sessionIds(v),
  ]);
}
export function eventVisible(hub: Hub, event: Event, person?: string): boolean {
  if (event.body.type === 'cursor_moved') return person === event.author;
  const ids = sessionIds(event.body);
  if (event.body.type === 'dispatch_finished') {
    const dispatch = hub.findDispatch(event.body.data.dispatch);
    if (dispatch?.session !== undefined) ids.push(dispatch.session);
  }
  if (event.body.type === 'ask_answered') {
    const ask = hub.findAsk(event.body.data.ask);
    if (ask?.session !== undefined) ids.push(ask.session);
  }
  return ids.every((id) => {
    const session = hub.findSession(id);
    return session === undefined || includesSession(hub.importChoice, session, hub);
  });
}
// Rebuild affected day paragraphs from retained block lines; never retain text from excluded blocks.
export function includedRecaps(hub: Hub): DemoRecaps {
  const blocks = hub.recaps.blocks.filter(({ block }) => block.session === undefined ||
    includesSession(hub.importChoice, hub.findSession(block.session)!, hub));
  const byId = new Map(blocks.map((b) => [b.block.id, b]));
  return { ...hub.recaps, blocks, projects: hub.recaps.projects.map((project) => ({ ...project,
    days: project.days.flatMap((day) => {
      const retained = day.blocks.filter((id) => byId.has(id));
      if (retained.length === day.blocks.length) return [day];
      if (retained.length === 0) return [];
      let text = '';
      const spans: typeof day.summary.spans = [];
      for (const id of retained) {
        const line = byId.get(id)!.line;
        if (text !== '') text += ' ';
        const offset = Buffer.byteLength(text);
        spans.push(...line.spans.map((s) => ({ ...s, range: { start: offset + s.range.start, end: offset + s.range.end } })));
        text += line.text;
      }
      return [{ ...day, blocks: retained, summary: { text, spans } }];
    }),
  })) };
}
/**
 * Included sessions, and apart from them included sub-agents: sessions nested under the top of
 * their chain of parents. One naming a parent the hub never saw, or in a loop, is a session.
 */
export function importCounts(hub: Hub, choice: ImportChoice): { sessions: number; subagents: number } {
  let sessions = 0;
  let subagents = 0;
  for (const s of hub.sessions) {
    const top = deciding(hub, s);
    if (!includesSession(choice, top)) continue;
    if (top.id === s.id) sessions += 1;
    else subagents += 1;
  }
  return { sessions, subagents };
}
