// Why the hub refused a terminal. A browser cannot see the HTTP status of a refused WebSocket
// upgrade (every failure is a 1006 close), so after one the terminal asks the hub over HTTP, in
// the order the hub decides: the session (404), its machine (503), then its terminal (404). It
// gives up after 10 s, which counts as finding nothing.

import { ApiError, type Api, type Machine } from '../../data/index.ts';
import type { Diagnosis } from './socket.ts';

export interface DiagnosisOptions {
  /** How long to wait for the hub's answers. */
  timeoutMs?: number;
  /** The session's machine as last known, for when the hub cannot say (a 503 on the session). */
  machineOf?: () => string | undefined;
}

const unreachable = (machine: Machine | undefined) => ({
  status: 503,
  message: `${machine?.name ?? 'Its machine'} cannot be reached right now, so its terminal cannot be shown.`,
});

export function terminalDiagnosis(
  api: Pick<Api, 'session' | 'machines'>,
  sessionId: string,
  options: DiagnosisOptions = {},
): () => Promise<Diagnosis> {
  const find = async (signal: AbortSignal): Promise<Diagnosis> => {
    const machine = (id: string | undefined) =>
      id === undefined
        ? Promise.resolve(undefined)
        : api.machines(signal).then(
            (machines) => machines.find((m) => m.id === id),
            () => undefined,
          );
    let session;
    try {
      session = await api.session(sessionId, signal);
    } catch (error) {
      if (!(error instanceof ApiError)) return undefined;
      if (error.code === 'not_found') return { status: 404, message: 'This session no longer exists.' };
      if (error.code === 'unauthorized' || error.code === 'forbidden') {
        return { status: error.status, message: 'The hub refused this token, so the terminal cannot be shown.' };
      }
      if (error.code === 'unavailable' && error.status === 503) {
        // The hub's own 503 is the machine's reason only while the machine is not live.
        const known = await machine(options.machineOf?.());
        if (known !== undefined && known.liveness !== 'live') return unreachable(known);
      }
      // The hub itself could not be reached, or failed: the connection may come back.
      return undefined;
    }
    const found = await machine(session.machine);
    if (session.state === 'unreachable' || (found !== undefined && found.liveness !== 'live')) return unreachable(found);
    if (session.terminal === undefined) return { status: 404, message: 'This session has no terminal.' };
    return 'unexplained';
  };

  return () => {
    const controller = new AbortController();
    let timer: ReturnType<typeof setTimeout> | undefined;
    const timeout = new Promise<undefined>((done) => {
      timer = setTimeout(() => {
        controller.abort();
        done(undefined);
      }, options.timeoutMs ?? 10_000);
    });
    const found = find(controller.signal).catch(() => undefined);
    return Promise.race([found, timeout]).finally(() => clearTimeout(timer));
  };
}
