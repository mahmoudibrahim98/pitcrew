import type { GatewayWorkspace } from '../data/index.ts';

type WorkspaceIdentity = Pick<GatewayWorkspace, 'name'> & { kind?: GatewayWorkspace['kind'] | undefined; host?: string | undefined };

export function workspaceLabel(workspace: WorkspaceIdentity): string {
  return workspace.kind === 'remote' && workspace.host
    ? `${workspace.name} · ${workspace.host}`
    : workspace.name;
}

/** The host is desktop-owned; the name can be supplied by an untrusted remote hub. */
export function WorkspaceName({ workspace, suffix = '' }: { workspace: WorkspaceIdentity; suffix?: string }) {
  return (
    <span className="inline-flex min-w-0 max-w-full items-baseline gap-1">
      <span className="min-w-0 truncate">{workspace.name}{!(workspace.kind === 'remote' && workspace.host) && suffix}</span>
      {workspace.kind === 'remote' && workspace.host && <span data-workspace-host className="shrink-0 whitespace-nowrap font-normal text-ink-2">{` · ${workspace.host}${suffix}`}</span>}
    </span>
  );
}
