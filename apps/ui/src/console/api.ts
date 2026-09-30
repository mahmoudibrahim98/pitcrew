// The console's API calls that `src/data/api.ts` does not have yet, built on its `request` so
// they share its auth header and `ApiError`s. They belong in `src/data/api.ts` once stream L
// takes them.

import type { Api, Ask } from '../data/index.ts';
import type { EndMode, Key, TranscriptPage } from './types.ts';

const id = encodeURIComponent;
const session = (sessionId: string) => `/v1/sessions/${id(sessionId)}`;

export function createConsoleApi(api: Api) {
  return {
    /** The newest page without `before`; the page ending before offset `before` with it. */
    transcript: (
      sessionId: string,
      page: { before?: number | undefined; limit?: number | undefined },
      signal?: AbortSignal,
    ) =>
      api.request<TranscriptPage>('GET', `${session(sessionId)}/transcript`, {
        query: { before: page.before?.toString(), limit: page.limit?.toString() },
        signal,
      }),
    /** Types the text and presses Enter. */
    send: (sessionId: string, text: string) =>
      api.request<undefined>('POST', `${session(sessionId)}/send`, { body: { text } }),
    keys: (sessionId: string, keys: readonly Key[]) =>
      api.request<undefined>('POST', `${session(sessionId)}/keys`, { body: { keys } }),
    interrupt: (sessionId: string) => api.request<undefined>('POST', `${session(sessionId)}/interrupt`),
    end: (sessionId: string, mode: EndMode) =>
      api.request<undefined>('POST', `${session(sessionId)}/end`, { body: { mode } }),
    answerAsk: (askId: string, answer: { option?: number; text?: string }) =>
      api.request<Ask>('POST', `/v1/asks/${id(askId)}/answer`, { body: answer }),
  };
}

export type ConsoleApi = ReturnType<typeof createConsoleApi>;

const cache = new WeakMap<Api, ConsoleApi>();

/** One console client per API client, so hooks get a stable object. */
export function consoleApiFor(api: Api): ConsoleApi {
  let found = cache.get(api);
  if (found === undefined) {
    found = createConsoleApi(api);
    cache.set(api, found);
  }
  return found;
}
