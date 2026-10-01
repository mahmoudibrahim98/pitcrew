import { describe, expect, it } from 'vitest';
import { NO_FACETS } from '../facets.ts';
import { facetCount, facetsFromSearch, searchWithFacets } from '../search.ts';

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
