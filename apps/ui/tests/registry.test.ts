import { createRootRoute, createRoute, type AnyRoute } from '@tanstack/react-router';
import { lazy } from 'react';
import { describe, expect, it } from 'vitest';
import { defineFeature, type Feature } from '../src/shell/feature.ts';
import { layoutOfMatches } from '../src/shell/layout.ts';
import { composeFeatures, servedPaths, withLayout } from '../src/shell/registry.ts';

const Empty = () => null;

const shell = defineFeature({
  id: 'shell',
  layout: 'both',
  nav: [
    { id: 'home', label: 'Home', to: 'home', layout: 'projects', order: 10 },
    { id: 'inbox', label: 'Inbox', to: 'inbox', order: 20 },
  ],
  commands: [{ id: 'go-home', label: 'Go to Home', run: () => undefined }],
  create: [
    { id: 'task', label: 'Task', order: 10, dialog: Empty },
    { id: 'team', label: 'Team', order: 40, dialog: Empty },
  ],
});

describe('composeFeatures', () => {
  it('resolves layouts and order, the shell first', () => {
    const consoleFeature = defineFeature({
      id: 'console',
      layout: 'console',
      nav: [
        { id: 'machines', label: 'Machines', to: 'console/machines', section: 'Console' },
        { id: 'sessions', label: 'Sessions', to: 'console', order: 15 },
      ],
      commands: [{ id: 'new-session', label: 'Start a session', run: () => undefined }],
      sidebar: Empty,
    });
    const registry = composeFeatures(shell, [consoleFeature]);
    expect(registry.features.map((f) => f.id)).toEqual(['shell', 'console']);
    expect(registry.nav.map((e) => [e.id, e.layout, e.order, e.feature])).toEqual([
      ['home', 'projects', 10, 'shell'],
      ['sessions', 'console', 15, 'console'],
      ['inbox', 'both', 20, 'shell'],
      ['machines', 'console', 100, 'console'],
    ]);
    expect(registry.commands.map((c) => [c.id, c.layout])).toEqual([
      ['go-home', 'both'],
      ['new-session', 'console'],
    ]);
    expect(registry.sidebars).toEqual([{ feature: 'console', layout: 'console', component: Empty }]);
  });

  it('lets a feature replace a "+ New" placeholder and add its own', () => {
    const Form = () => null;
    const projects = defineFeature({
      id: 'projects',
      layout: 'projects',
      create: [
        { id: 'task', label: 'Task', dialog: Form },
        // Lazy components fit wherever the interface takes a component.
        { id: 'workstream', label: 'Workstream', order: 15, dialog: lazy(async () => ({ default: Form })) },
      ],
    });
    const registry = composeFeatures(shell, [projects]);
    expect(registry.create.map((e) => [e.id, e.feature, e.order])).toEqual([
      ['task', 'projects', 10],
      ['workstream', 'projects', 15],
      ['team', 'shell', 40],
    ]);
    expect(registry.create[0]?.dialog).toBe(Form);
  });

  it('rejects duplicate ids', () => {
    const a = defineFeature({ id: 'a', layout: 'both', nav: [{ id: 'inbox', label: 'Mine', to: 'x' }] });
    expect(() => composeFeatures(shell, [a])).toThrow('Two features register the nav entry "inbox"');
    const b: Feature = { id: 'shell', layout: 'both' };
    expect(() => composeFeatures(shell, [b])).toThrow('Two features register the feature "shell"');
    const c = defineFeature({ id: 'c', layout: 'both', create: [{ id: 'x', label: 'X', dialog: Empty }] });
    const d = defineFeature({ id: 'd', layout: 'both', create: [{ id: 'x', label: 'X', dialog: Empty }] });
    expect(() => composeFeatures(shell, [c, d])).toThrow('"+ New" item "x"');
  });
});

describe('servedPaths and withLayout', () => {
  const root = createRootRoute();
  const parent = createRoute({ getParentRoute: () => root, path: 'w/$ws' });

  it('lists every path a subtree serves, with params written $', () => {
    const project = createRoute({ getParentRoute: () => parent, path: 'projects/$projectId' });
    const board = createRoute({ getParentRoute: () => project, path: 'board' });
    const layout = createRoute({ getParentRoute: () => project, id: 'tabs' });
    const workstream = createRoute({ getParentRoute: () => layout, path: '/workstreams/$ws2/' });
    project.addChildren([board, layout.addChildren([workstream])]);
    expect(servedPaths(project as AnyRoute)).toEqual([
      'projects/$',
      'projects/$/board',
      'projects/$/workstreams/$',
    ]);
    const index = createRoute({ getParentRoute: () => parent, path: '/' });
    expect(servedPaths(index as AnyRoute)).toEqual(['']);
  });

  it('gives top-level routes their feature layout unless they declare one', () => {
    const plain = createRoute({ getParentRoute: () => parent, path: 'a' });
    const declared = createRoute({ getParentRoute: () => parent, path: 'b', staticData: { layout: 'both' } });
    withLayout([plain, declared] as AnyRoute[], 'console');
    expect(plain.options.staticData?.layout).toBe('console');
    expect(declared.options.staticData?.layout).toBe('both');
  });

  it('reads the deepest declared layout from the matches', () => {
    expect(layoutOfMatches([{ staticData: {} }, { staticData: { layout: 'console' } }, { staticData: {} }])).toBe(
      'console',
    );
    expect(layoutOfMatches([{ staticData: { layout: 'projects' } }, { staticData: { layout: 'both' } }])).toBe(
      'both',
    );
    expect(layoutOfMatches([{ staticData: {} }])).toBeUndefined();
  });
});
