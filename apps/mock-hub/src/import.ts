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
export function includesSession(choice: ImportChoice, session: Session): boolean {
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
    return session === undefined || includesSession(hub.importChoice, session);
  });
}
// Rebuild affected day paragraphs from retained block lines; never retain text from excluded blocks.
export function includedRecaps(hub: Hub): DemoRecaps {
  const blocks = hub.recaps.blocks.filter(({ block }) => block.session === undefined ||
    includesSession(hub.importChoice, hub.findSession(block.session)!));
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
