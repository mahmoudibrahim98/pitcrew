// Why the hub refused a terminal. A browser cannot see the HTTP status of a refused WebSocket
// upgrade (every failure is a 1006 close), so after one the terminal asks the hub over HTTP, in
// the order the hub decides: the session (404), its machine (503), then its terminal (404).

import { ApiError, type Api } from '../../data/index.ts';
import type { TerminalProblem } from './socket.ts';

export function terminalDiagnosis(
  api: Pick<Api, 'session' | 'machines'>,
  sessionId: string,
): () => Promise<TerminalProblem | undefined> {
  return async () => {
    let session;
    try {
      session = await api.session(sessionId);
    } catch (error) {
      if (error instanceof ApiError) {
        if (error.code === 'not_found') return { status: 404, message: 'This session no longer exists.' };
        if (error.code === 'unauthorized' || error.code === 'forbidden') {
          return { status: error.status, message: 'The hub refused this token, so the terminal cannot be shown.' };
        }
        // The hub answered 503 itself: the session's machine is out of reach.
        if (error.code === 'unavailable' && error.status === 503) return { status: 503, message: error.message };
      }
      // The hub itself could not be reached, or failed: the connection may come back.
      return undefined;
    }
    const machine = await api.machines().then(
      (machines) => machines.find((m) => m.id === session.machine),
      () => undefined,
    );
    if (session.state === 'unreachable' || (machine !== undefined && machine.liveness !== 'live')) {
      const name = machine?.name ?? 'Its machine';
      return { status: 503, message: `${name} cannot be reached right now, so its terminal cannot be shown.` };
    }
    if (session.terminal === undefined) return { status: 404, message: 'This session has no terminal.' };
    return undefined;
  };
}
