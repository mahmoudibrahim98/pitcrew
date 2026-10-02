import type { GatewayWorkspace } from '../data/index.ts';

type WorkspaceIdentity = Pick<GatewayWorkspace, 'name'> & Partial<Pick<GatewayWorkspace, 'kind' | 'host'>>;

export function workspaceLabel(workspace: WorkspaceIdentity): string {
  return workspace.kind === 'remote' && workspace.host
    ? `${workspace.name} · ${workspace.host}`
    : workspace.name;
}

/** The host is desktop-owned; the name can be supplied by an untrusted remote hub. */
export function WorkspaceName({ workspace, suffix = '' }: { workspace: WorkspaceIdentity; suffix?: string }) {
  return (
    <>
      {workspace.name}{!(workspace.kind === 'remote' && workspace.host) && suffix}
      {workspace.kind === 'remote' && workspace.host && <span className="font-normal text-ink-2"> {`· ${workspace.host}${suffix}`}</span>}
    </>
  );
}
