// "+ New" → Session. Starting an agent from PitCrew needs the start-session flow, which does not
// exist yet; until it does, the dialog says so and how sessions arrive meanwhile.

import { Button, DialogFooter } from '../design/index.ts';

export default function NewSession({ close }: { close(): void }) {
  return (
    <>
      <div className="flex flex-col gap-2 px-4 py-4 text-sm">
        <p className="font-medium">Starting a session from PitCrew is not available yet.</p>
        <p className="text-ink-2">
          For now, start the agent's own CLI (Claude Code, Codex or OpenCode) on a machine PitCrew
          watches. Its session appears in the Agent console by itself.
        </p>
      </div>
      <DialogFooter>
        <Button variant="primary" onClick={close}>
          Close
        </Button>
      </DialogFooter>
    </>
  );
}
