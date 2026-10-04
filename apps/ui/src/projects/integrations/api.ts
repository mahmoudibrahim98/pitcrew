// GitHub and Jira integrations (api-v1.md, "Integrations"): the wire types, a client over the
// API, and the hooks the settings page and the workstream page use. Read-only upstream: nothing
// here writes to GitHub or Jira.
//
// A secret goes from the form to `storeCredential` once and is kept nowhere: in the desktop app
// it travels through the gateway's own command, never `gateway_request` (desktop-gateway.md,
// "Integration credentials"); no answer ever carries it back.

import { useMutation, useQueryClient } from '@tanstack/react-query';
import { ApiError, useApi, useLiveQuery, type Api, type ExternalRef, type Workstream } from '../../data/index.ts';

export type JiraDeployment = 'cloud' | 'data_center';
export type CredentialSource = 'gh_cli' | 'stored';

export type IntegrationSettings =
  | { kind: 'github'; repos: string[]; api_base?: string }
  | {
      kind: 'jira';
      deployment: JiraDeployment;
      site: string;
      projects: string[];
      email?: string;
      epic_link_field?: string;
    };

export interface SyncProblem {
  scope: string;
  message: string;
}

export interface SyncCounts {
  changes: number;
  applied: number;
  conflicts: number;
  skipped: number;
  malformed: number;
}

export interface SyncStatus {
  running: boolean;
  last_attempt_at?: number;
  last_success_at?: number;
  next_at?: number;
  rate_limited_until?: number;
  problems: SyncProblem[];
  last_run?: SyncCounts;
}

export interface IntegrationLink {
  workstream: string;
  scope: ExternalRef;
  title?: string;
}

export interface Integration {
  id: string;
  name: string;
  settings: IntegrationSettings;
  credential: { source: CredentialSource; stored: boolean };
  interval_minutes: number;
  added_by: string;
  added_at: number;
  status: SyncStatus;
  links: IntegrationLink[];
}

export interface NewIntegration {
  name: string;
  settings: IntegrationSettings;
  credential: CredentialSource;
  interval_minutes?: number;
}

export interface ScopeCheck {
  scope: string;
  ok: boolean;
  message: string;
}

export interface IntegrationCheck {
  ok: boolean;
  at: number;
  checks: ScopeCheck[];
  warnings: string[];
}

const ROOT = '/v1/integrations';
const one = (id: string) => `${ROOT}/${encodeURIComponent(id)}`;

/** The integration routes over `api`. */
export function integrationClient(api: Api) {
  return {
    list: (signal?: AbortSignal) => api.request<Integration[]>('GET', ROOT, { signal }),
    add: (integration: NewIntegration) => api.request<Integration>('POST', ROOT, { body: integration }),
    remove: (id: string) => api.request<undefined>('DELETE', one(id)),
    test: (id: string) => api.request<IntegrationCheck>('POST', `${one(id)}/test`),
    sync: (id: string) => api.request<Integration>('POST', `${one(id)}/sync`),
    /** Hands the secret over once (see the file's head). Resolves when the daemon stored it. */
    async storeCredential(id: string, secret: string): Promise<void> {
      const transport = api.transport;
      if (transport.storeCredential !== undefined) {
        const res = await transport.storeCredential(id, secret);
        if (res.status >= 400) throw credentialError(res.status, res.body);
        return;
      }
      await api.request<undefined>('PUT', `${one(id)}/credential`, { body: { secret } });
    },
    /** Links (or, with `[]`, unlinks) a workstream's upstream scopes. */
    link: (workstream: string, external: ExternalRef[]) =>
      api.request<Workstream>('PATCH', `/v1/workstreams/${encodeURIComponent(workstream)}`, { body: { external } }),
  };
}

/** An error for a refused credential, from the daemon's `ApiError` body, without echoing input. */
function credentialError(status: number, body: string): ApiError {
  let message = `The credential was refused (${status}).`;
  let code: ApiError['code'] = status === 409 ? 'conflict' : status === 400 ? 'invalid' : 'internal';
  try {
    const parsed: unknown = JSON.parse(body);
    if (typeof parsed === 'object' && parsed !== null) {
      const { message: m, code: c } = parsed as { message?: unknown; code?: unknown };
      if (typeof m === 'string') message = m;
      if (typeof c === 'string') code = c as ApiError['code'];
    }
  } catch {
    // Not the contract's body: keep the generic message.
  }
  return new ApiError(code, message, status);
}

export const integrationKeys = {
  all: ['integrations'] as const,
};

/** Every integration. Polls while one syncs, slowly otherwise: a sync's status has no event. */
export function useIntegrations() {
  const api = useApi();
  return useLiveQuery({
    queryKey: integrationKeys.all,
    queryFn: ({ signal }) => integrationClient(api).list(signal),
    refetchInterval: (query) => (query.state.data?.some((i) => i.status.running) === true ? 2_000 : 30_000),
  });
}

/** The integrations' mutations; each refreshes the list. */
export function useIntegrationActions() {
  const api = useApi();
  const queryClient = useQueryClient();
  const client = integrationClient(api);
  const refresh = () => queryClient.invalidateQueries({ queryKey: integrationKeys.all });
  return {
    add: useMutation({ mutationFn: (n: NewIntegration) => client.add(n), onSettled: refresh }),
    remove: useMutation({ mutationFn: (id: string) => client.remove(id), onSettled: refresh }),
    test: useMutation({ mutationFn: (id: string) => client.test(id) }),
    sync: useMutation({ mutationFn: (id: string) => client.sync(id), onSettled: refresh }),
    storeCredential: useMutation({
      mutationFn: ({ id, secret }: { id: string; secret: string }) => client.storeCredential(id, secret),
      onSettled: refresh,
    }),
    link: useMutation({
      mutationFn: ({ workstream, external }: { workstream: string; external: ExternalRef[] }) =>
        client.link(workstream, external),
      onSettled: refresh,
    }),
  };
}

/** What a link names, as the hub reads it (`pitcrew_hub_work::links::scope_of`). */
export function describeScope(link: ExternalRef): string {
  if (link.system === 'github') {
    const at = link.key.indexOf('#milestone:');
    return at < 0 ? `GitHub repository ${link.key}` : `GitHub milestone ${link.key.slice(at + 11)} of ${link.key.slice(0, at)}`;
  }
  if (link.system === 'jira') {
    return /-\d+$/.test(link.key) ? `Jira epic ${link.key}` : `Jira project ${link.key}`;
  }
  return link.key;
}

/** The scopes an integration can be linked to, for the link picker: its repositories or projects. */
export function scopesOf(integration: Integration): ExternalRef[] {
  const settings = integration.settings;
  if (settings.kind === 'github') {
    return settings.repos.map((repo) => ({ system: 'github', key: repo, url: `https://github.com/${repo}` }));
  }
  return settings.projects.map((project) => ({ system: 'jira', key: project, url: `${settings.site}/browse/${project}` }));
}

/** The integration whose scopes include `link`, if any. */
export function integrationOf(integrations: readonly Integration[], link: ExternalRef): Integration | undefined {
  return integrations.find((i) => {
    if (i.settings.kind === 'github' && link.system === 'github') {
      const repo = link.key.split('#')[0]?.toLowerCase();
      return i.settings.repos.some((r) => r.toLowerCase() === repo);
    }
    if (i.settings.kind === 'jira' && link.system === 'jira') {
      const project = /-\d+$/.test(link.key) ? link.key.slice(0, link.key.lastIndexOf('-')) : link.key;
      return i.settings.projects.includes(project);
    }
    return false;
  });
}
