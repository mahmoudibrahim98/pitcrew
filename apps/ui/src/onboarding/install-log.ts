// The live install log: one line per progress message of the desktop gateway's add
// (`gateway_remote_add`), whose steps are the plan's ("Copy pitcrewd 0.1.0 to ~/.pitcrew", …) and
// whose details say how each goes ("checking the copy already there", "40% sent", "verified",
// "job 4242 pending (Priority)"). The last message is the whole add's (`step: 'add'`).

import type { RemoteProgress } from '../data/index.ts';

/** The step name of the whole add, in its last message (desktop-gateway.md). */
export const ADD_STEP = 'add';

/** `progress` as one line of the log. */
export function installLine(progress: RemoteProgress): string {
  if (progress.step === ADD_STEP) {
    if (progress.state === 'done') return 'Connected.';
    if (progress.state === 'failed') return `Failed: ${progress.detail ?? 'the add did not finish'}.`;
  }
  const how = progress.detail ?? (progress.state === 'running' ? 'started' : progress.state);
  return `${progress.step}: ${how}`;
}

/** Appends `progress`'s line to `log`, unless it says what the last line said. */
export function withLine(log: readonly string[], progress: RemoteProgress): string[] {
  const next = installLine(progress);
  return log.at(-1) === next ? [...log] : [...log, next];
}
