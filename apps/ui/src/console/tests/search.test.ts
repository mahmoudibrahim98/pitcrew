import { describe, expect, it } from 'vitest';
import { NO_FACETS } from '../facets.ts';
import { facetCount, facetsFromSearch, searchWithFacets, searchWithView, viewFromSearch } from '../search.ts';

describe('the facets in the URL', () => {
  it('reads comma-separated values, dropping unknown engines and states, blanks and repeats', () => {
    expect(
      facetsFromSearch({
        machine: 'M1, M2,,M1',
        engine: 'claude,gpt',
        state: ['waiting', 'asleep', 7],
        project: '',
        workstream: 42,
        other: 'kept elsewhere',
      }),
    ).toEqual({ machine: ['M1', 'M2'], engine: ['claude'], state: ['waiting'], project: [], workstream: [] });
    expect(facetsFromSearch({})).toEqual(NO_FACETS);
  });

  it('writes non-empty facets and keeps other parameters', () => {
    const search = searchWithFacets(
      { machine: 'old', tab: 'files' },
      { ...NO_FACETS, state: ['working', 'waiting'], project: ['P'] },
    );
    expect(search).toEqual({ tab: 'files', state: 'working,waiting', project: 'P' });
    expect(facetsFromSearch(search)).toEqual({ ...NO_FACETS, state: ['working', 'waiting'], project: ['P'] });
    expect(searchWithFacets(search, NO_FACETS)).toEqual({ tab: 'files' });
  });

  it('counts the chosen values', () => {
    expect(facetCount(NO_FACETS)).toBe(0);
    expect(facetCount({ ...NO_FACETS, machine: ['A', 'B'], engine: ['codex'] })).toBe(3);
  });
});

describe('the session pane view in the URL', () => {
  it('reads chat, terminal or work; anything else is chat', () => {
    expect(viewFromSearch({})).toBe('chat');
    expect(viewFromSearch({ view: 'terminal' })).toBe('terminal');
    expect(viewFromSearch({ view: 'work' })).toBe('work');
    expect(viewFromSearch({ view: 'bogus' })).toBe('chat');
  });

  it('writes terminal and work, but chat (the default) is no parameter at all', () => {
    expect(searchWithView({ machine: 'M1' }, 'chat')).toEqual({ machine: 'M1' });
    expect(searchWithView({ machine: 'M1' }, 'terminal')).toEqual({ machine: 'M1', view: 'terminal' });
    expect(searchWithView({ machine: 'M1' }, 'work')).toEqual({ machine: 'M1', view: 'work' });
    expect(searchWithView({ machine: 'M1', view: 'terminal' }, 'chat')).toEqual({ machine: 'M1' });
    expect(searchWithView({ view: 'terminal' }, 'work')).toEqual({ view: 'work' });
  });
});
