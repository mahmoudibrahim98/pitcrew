// @vitest-environment happy-dom
// @vitest-environment-options {"url": "http://localhost:5173/"}
//
// The real hub answers `400 invalid` to `GET /v1/events?project=` and `?workstream=` until its
// index lands (api-v1.md; the mock hub accepts them, so this fakes the 400 at the fetch layer).

import { screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { ActivityFeed } from '../activity.tsx';
import { demo, renderWithHub, startHub, stopHub, type Hub } from './harness.tsx';

/** Answers 400 invalid to `project`/`workstream`-filtered `/v1/events`; passes everything else on. */
function rejectScopedEvents(): typeof fetch {
  return (async (input, init) => {
    const href = typeof input === 'string' ? input : input instanceof Request ? input.url : input.toString();
    const url = new URL(href);
    if (url.pathname === '/v1/events' && (url.searchParams.has('project') || url.searchParams.has('workstream'))) {
      return new Response(JSON.stringify({ code: 'invalid', message: 'no index for this filter yet' }), {
        status: 400,
        headers: { 'Content-Type': 'application/json' },
      });
    }
    return fetch(input, init);
  }) as typeof fetch;
}

describe('ActivityFeed, when the hub has no index for project/workstream activity yet', () => {
  let hub: Hub;

  beforeEach(async () => {
    hub = await startHub();
  });

  afterEach(async () => {
    await stopHub(hub);
  });

  it('shows a short note instead of an error, for a project filter', async () => {
    renderWithHub(<ActivityFeed filters={{ project: demo.paper }} />, hub, { fetch: rejectScopedEvents() });
    await screen.findByText('Activity isn’t available here yet.');
    expect(screen.queryByRole('alert')).toBeNull();
  });

  it('shows the note for a workstream filter too', async () => {
    renderWithHub(<ActivityFeed filters={{ workstream: demo.seedRuns }} />, hub, { fetch: rejectScopedEvents() });
    await screen.findByText('Activity isn’t available here yet.');
    expect(screen.queryByRole('alert')).toBeNull();
  });

  it('keeps task-scoped activity working under the same fetch', async () => {
    renderWithHub(<ActivityFeed filters={{ task: demo.pap1 }} />, hub, { fetch: rejectScopedEvents() });
    await screen.findByRole('list', { name: 'Events' });
    expect(screen.queryByText('Activity isn’t available here yet.')).toBeNull();
  });

  it('still shows a real error for anything other than the project/workstream 400', async () => {
    const unreachable: typeof fetch = async () => {
      throw new TypeError('network down');
    };
    renderWithHub(<ActivityFeed filters={{ project: demo.paper }} />, hub, { fetch: unreachable });
    await screen.findByRole('alert');
  });
});
