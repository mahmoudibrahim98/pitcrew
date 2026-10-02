// SSH's prompts (desktop-gateway.md, "Prompts"), mounted once at the root: whatever is on screen,
// a password, a key's passphrase, a one-time code or a new host key is asked here. One at a time,
// oldest first; the rest wait. Nothing in a browser. The dialog itself is a lazy chunk, loaded
// when the first prompt arrives.

import { lazy, Suspense } from 'react';
import { useGatewayPrompts } from '../data/index.ts';

const PromptDialog = lazy(() => import('./prompt-dialog.tsx').then((m) => ({ default: m.PromptDialog })));

export function GatewayPrompts() {
  const gateway = useGatewayPrompts();
  const prompt = gateway?.prompts[0];
  if (gateway === null || prompt === undefined) return null;
  return (
    <Suspense fallback={null}>
      {/* Keyed by id: each prompt's answer starts empty, in its own state. */}
      <PromptDialog
        key={prompt.id}
        prompt={prompt}
        waiting={gateway.prompts.length - 1}
        reply={(reply) => gateway.reply(prompt.id, reply)}
      />
    </Suspense>
  );
}
