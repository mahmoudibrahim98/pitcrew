// `gateway://navigate`: deep links (`pitcrew://w/<ws>/…`) and clicks on the app's own
// notifications (docs/build/contracts/desktop-gateway.md, "Navigation from outside the window").
// The gateway has already checked the link's shape; the UI checks the target again and maps it to
// a route with the shell's own path helpers — never by treating a field as a URL or a path to
// concatenate. An unknown workspace (not one the gateway currently lists) goes to `/` with a
// notice; anything else invalid is dropped and logged, shortened.

import { useRouter } from '@tanstack/react-router';
import { useCallback } from 'react';
import { useGatewayNavigate, type GatewayWorkspace } from '../data/index.ts';
import { paths } from './paths.ts';
import { useShell } from './store.ts';

export interface NavigateTarget {
  workspace: string;
  kind: 'inbox' | 'task' | 'session' | 'project' | 'workstream';
  id?: string;
}

const KINDS: readonly string[] = ['inbox', 'task', 'session', 'project', 'workstream'];
// api-v1.md: "ids are bare 26-character ULIDs" (Crockford's base32: no I, L, O or U). Only the
// workspace is checked against this: `id` is never a ULID alone — a task's may be its key
// (`PAP-4`) instead (api-v1.md, "Ids in paths") — so the shell leaves it to the route it lands on.
const ULID = /^[0-9A-HJKMNP-TV-Z]{26}$/;

/** Checks the event payload is a well-formed target; anything that is not one is never forwarded. */
export function parseNavigateTarget(payload: unknown): NavigateTarget | undefined {
  if (typeof payload !== 'object' || payload === null) return undefined;
  const { workspace, kind, id } = payload as Record<string, unknown>;
  if (typeof workspace !== 'string' || !ULID.test(workspace)) return undefined;
  if (typeof kind !== 'string' || !KINDS.includes(kind)) return undefined;
  if (id !== undefined && typeof id !== 'string') return undefined;
  if (kind !== 'inbox' && (id === undefined || id === '')) return undefined;
  return { workspace, kind: kind as NavigateTarget['kind'], ...(id === undefined ? {} : { id }) };
}

/**
 * The app's path for a validated target, or `undefined` when the gateway no longer lists its
 * workspace: the caller then goes to `/` with a notice, never guesses a path for it.
 */
export function navigateHref(target: NavigateTarget, workspaces: readonly GatewayWorkspace[]): string | undefined {
  if (!workspaces.some((w) => w.id === target.workspace)) return undefined;
  const { workspace: ws, id } = target;
  switch (target.kind) {
    case 'inbox':
      return paths.inbox(ws);
    case 'task':
      return id === undefined ? undefined : paths.task(ws, id);
    case 'session':
      return id === undefined ? undefined : paths.session(ws, id);
    case 'project':
      return id === undefined ? undefined : paths.project(ws, id);
    case 'workstream':
      return id === undefined ? undefined : paths.workstreamById(ws, id);
  }
}

/** A short, safe-to-log glimpse of a dropped payload. */
function shorten(payload: unknown): string {
  let text: string;
  try {
    text = JSON.stringify(payload) ?? String(payload);
  } catch {
    text = String(payload);
  }
  return text.length > 200 ? `${text.slice(0, 200)}…` : text;
}

/**
 * Wires `gateway://navigate` to the router. A no-op in a browser (`useGatewayNavigate` is one
 * there too). Mount once, near the root, above the redirect `/` does, so the notice survives it.
 *
 * `useGatewayNavigate` only calls back once the workspace list is known, with that list passed
 * alongside the target — never a list read separately here, which could still be the one from
 * before a target that arrived at launch (the gateway holds a deep link until the webview's first
 * `gateway_workspaces()` call, which can race the read that would otherwise answer "is this
 * workspace known?").
 */
export function useGatewayNavigation(): void {
  const router = useRouter();
  const setNotice = useShell((s) => s.setNotice);
  const onTarget = useCallback(
    (payload: unknown, workspaces: readonly GatewayWorkspace[]) => {
      const target = parseNavigateTarget(payload);
      if (target === undefined) {
        console.warn('pitcrew: dropped an invalid gateway://navigate target:', shorten(payload));
        return;
      }
      const href = navigateHref(target, workspaces);
      if (href === undefined) {
        setNotice('That link’s workspace isn’t one this app can reach right now.');
        void router.navigate({ href: '/' });
        return;
      }
      void router.navigate({ href });
    },
    [router, setNotice],
  );
  useGatewayNavigate(onTarget);
}
