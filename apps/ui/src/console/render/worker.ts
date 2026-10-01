// The render worker: parses markdown and diffs off the main thread.

import { parseAny, type ParseRequest, type ParseResponse } from './parse.ts';

// The app's TypeScript lib is the DOM's; this file runs in a dedicated worker.
const scope = self as unknown as {
  onmessage: ((event: MessageEvent<ParseRequest>) => void) | null;
  postMessage(message: ParseResponse): void;
};

scope.onmessage = (event) => {
  const { id, kind, text } = event.data;
  try {
    scope.postMessage({ id, result: parseAny(kind, text) });
  } catch (error) {
    scope.postMessage({ id, error: String(error) });
  }
};
