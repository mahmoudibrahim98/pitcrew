// Sub-agents in the console, on the audit's shape: nested under their parents in the list
// (collapsed, "2 sub-agents"), placed where they ran in the parent's chat, and linked with their
// parent by "Link to…".

import { describe, expect, it } from 'vitest';
import type { Project, Workstream } from '../../data/index.ts';
import { auditSessions, session } from '../../data/tests/audit-sessions.ts';
import type { SessionPlaces } from '../facets.ts';
import { companions, defaultWorkstream, newDefaultWorkstream, parseTarget, within } from '../link-targets.ts';
import { groupSessions } from '../session-list.tsx';
import { buildRows } from '../transcript.ts';

const MACHINE = '01JB000000000000000MCH0001';
const project: Project = {
  id: '01JB000000000000000PRJ0001',
  key: 'AT',
  name: 'atlas',
  status: 'in_progress',
  lead: 'm',
  members: ['m'],
  root: { machine: MACHINE, path: '/home/sam/work/atlas' },
  external: [],
};
const main: Workstream = {
  id: '01JB000000000000000WST0001',
  project: project.id,
  name: 'main',
  status: 'active',
  health: 'on_track',
  locations: [{ machine: MACHINE, path: '/home/sam/work/atlas/' }],
  external: [],
};
const places: SessionPlaces = { projects: [project], workstreams: [main], tasks: [] };

describe('the session list', () => {
  it('nests sub-agents under their parents, collapsed, and counts sessions without them', () => {
    const { sessions, parents } = auditSessions();
    // Every session linked to the default workstream: nothing is unsorted.
    const linked = sessions.map((s) => ({ ...s, workstream: main.id }));
    const rows = groupSessions(linked, places);
    expect(rows.filter((r) => r.type === 'project').map((r) => r.type === 'project' && r.label)).toEqual(['atlas']);
    expect(rows.find((r) => r.type === 'project')).toMatchObject({ count: 10 });
    const sessionRows = rows.filter((r) => r.type === 'session');
    expect(sessionRows).toHaveLength(10);
    expect(sessionRows.every((r) => r.type === 'session' && !r.nested)).toBe(true);
    expect(sessionRows.find((r) => r.key === parents.atlas.id)).toMatchObject({ subagents: 2 });
    expect(sessionRows.find((r) => r.key === parents.search.id)).toMatchObject({ subagents: 1 });

    // Shown: right under their parent, oldest first.
    const open = groupSessions(linked, places, new Set([parents.atlas.id]));
    const at = open.findIndex((r) => r.key === parents.atlas.id);
    expect(open.slice(at + 1, at + 3).map((r) => r.type === 'session' && [r.session.native_id, r.nested])).toEqual([
      ['a1', true],
      ['a2', true],
    ]);
    expect(open.filter((r) => r.type === 'session')).toHaveLength(12);
  });

  it('shows unlinked sessions as Unsorted, sub-agents still nested', () => {
    const { sessions } = auditSessions();
    const rows = groupSessions(sessions, places);
    expect(rows[0]).toMatchObject({ type: 'project', label: 'Unsorted', count: 10 });
  });
});

describe('the parent chat', () => {
  it('shows each sub-agent where it ran', () => {
    const parent = session('parent');
    const sub = session('sub', { parent: parent.id, started: 2_500 });
    const late = session('late', { parent: parent.id, started: 9_000 });
    const items = [
      { kind: 'user_prompt' as const, at: 1_000, offset: 0, text: 'Go' },
      { kind: 'assistant_text' as const, at: 2_000, offset: 10, text: 'Starting' },
      { kind: 'turn_ended' as const, at: 3_000, offset: 20 },
    ];
    const rows = buildRows({ items, gapsBefore: new Set(), atStart: true }, [], [late, sub]);
    expect(rows.map((r) => r.type)).toEqual(['start', 'prompt', 'text', 'subagent', 'turn', 'subagent']);
    expect(rows[3]).toMatchObject({ type: 'subagent', session: { native_id: 'sub' } });
    // One that started before the loaded page waits for it.
    const page = buildRows({ items: items.slice(2), gapsBefore: new Set(), atStart: false }, [], [sub]);
    expect(page.map((r) => r.type)).toEqual(['turn']);
  });
});

describe('Link to…', () => {
  it('takes a sub-agent with its parent, and offers unsorted sessions in its folder', () => {
    const { sessions, parents } = auditSessions();
    const { subagents, unsorted } = companions(parents.atlas, sessions);
    expect(subagents.map((s) => s.native_id)).toEqual(['a1', 'a2']);
    // In its folder or below; never another folder, and never a sub-agent of another session.
    expect(unsorted.map((s) => s.native_id).sort()).toEqual(['c-atlas-src', 'c-search', 'x-atlas']);
  });

  it('finds or makes a project’s default workstream', () => {
    expect(defaultWorkstream(project, [main])?.id).toBe(main.id);
    const elsewhere = { ...main, locations: [{ machine: MACHINE, path: '/home/sam/work/atlas', branch: 'dev' }] };
    expect(defaultWorkstream(project, [elsewhere])).toBeUndefined();
    expect(newDefaultWorkstream(project)).toEqual({ project: project.id, name: 'Main', locations: [project.root] });
  });

  it('reads its choice', () => {
    expect(parseTarget(main.id)).toEqual({ kind: 'workstream', id: main.id });
    expect(parseTarget(`project:${project.id}`)).toEqual({ kind: 'project', id: project.id });
    expect(parseTarget(`new:${project.id}`)).toEqual({ kind: 'new', project: project.id });
    expect(parseTarget('')).toBeUndefined();
    expect(parseTarget('other:x')).toBeUndefined();
    expect(within('C:\\work\\atlas\\src', 'C:/work/atlas')).toBe(true);
    expect(within('/w/atlas-hotfix', '/w/atlas')).toBe(false);
  });
});
