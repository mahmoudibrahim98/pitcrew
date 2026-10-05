// Settings › Integrations: connect GitHub repositories or Jira projects, hand over a credential,
// test a connection, sync now, and see each one's status, problems and linked workstreams.
// Read-only upstream (api-v1.md, "Integrations"); links are made on each workstream's page.

import { useRef, useState, type FormEvent } from 'react';
import { Button, Dialog, DialogContent, DialogFooter, StatusPill, type Tone } from '../../design/index.ts';
import { useNames } from '../data.ts';
import { formatWhen } from '../format.ts';
import { ErrorNote, Field, inputClass } from '../ui.tsx';
import {
  describeScope,
  useIntegrationActions,
  useIntegrations,
  useStoreCredential,
  type Integration,
  type IntegrationCheck,
  type JiraDeployment,
  type NewIntegration,
} from './api.ts';

/** Where an integration's sync stands, for a pill. */
export function syncState(integration: Integration, now = Date.now()): { tone: Tone; label: string } {
  const status = integration.status;
  if (status.running) return { tone: 'progress', label: 'Syncing' };
  if (status.rate_limited_until !== undefined && status.rate_limited_until > now) {
    return { tone: 'warn', label: `Waiting until ${formatWhen(status.rate_limited_until)}` };
  }
  if (integration.credential.source === 'stored' && !integration.credential.stored) {
    return { tone: 'warn', label: 'Needs a credential' };
  }
  if (status.problems.length > 0) return { tone: 'risk', label: 'Problems' };
  if (status.last_success_at !== undefined) return { tone: 'ok', label: 'In sync' };
  return { tone: 'neutral', label: 'Not synced yet' };
}

function lines(text: string): string[] {
  return text
    .split(/[\n,]/)
    .map((s) => s.trim())
    .filter((s) => s !== '');
}

function ConnectGithub({ onDone }: { onDone(): void }) {
  const { add } = useIntegrationActions();
  const [name, setName] = useState('GitHub');
  const [repos, setRepos] = useState('');
  const [apiBase, setApiBase] = useState('');
  const [credential, setCredential] = useState<'gh_cli' | 'stored'>('gh_cli');
  const submit = (e: FormEvent) => {
    e.preventDefault();
    const settings: NewIntegration['settings'] = { kind: 'github', repos: lines(repos) };
    if (apiBase.trim() !== '') settings.api_base = apiBase.trim();
    add.mutate({ name, settings, credential }, { onSuccess: onDone });
  };
  return (
    <form onSubmit={submit} className="flex flex-col gap-3 px-4 py-4">
      <Field label="Name">{(id) => <input id={id} className={inputClass} value={name} onChange={(e) => setName(e.target.value)} />}</Field>
      <Field label="Repositories" hint="owner/repo, one per line.">
        {(id) => <textarea id={id} className={inputClass} rows={3} value={repos} onChange={(e) => setRepos(e.target.value)} />}
      </Field>
      <Field label="GitHub Enterprise API (optional)" hint="https://ghe.example.com/api/v3; leave empty for github.com.">
        {(id) => <input id={id} className={inputClass} value={apiBase} onChange={(e) => setApiBase(e.target.value)} />}
      </Field>
      <fieldset className="flex flex-col gap-1 text-sm">
        <legend className="text-xs font-medium text-ink-2">Credential</legend>
        <label className="flex items-center gap-2">
          <input type="radio" name="credential" checked={credential === 'gh_cli'} onChange={() => setCredential('gh_cli')} />
          The GitHub CLI on the hub’s machine (<code>gh auth token</code>, read at each sync, never kept)
        </label>
        <label className="flex items-center gap-2">
          <input type="radio" name="credential" checked={credential === 'stored'} onChange={() => setCredential('stored')} />
          A token I enter next (a fine-grained, read-only token for these repositories is safest)
        </label>
      </fieldset>
      {add.error !== null && <ErrorNote error={add.error} what="connect GitHub" />}
      <DialogFooter>
        <Button variant="primary" type="submit" disabled={add.isPending}>
          Connect
        </Button>
      </DialogFooter>
    </form>
  );
}

function ConnectJira({ onDone }: { onDone(): void }) {
  const { add } = useIntegrationActions();
  const [name, setName] = useState('Jira');
  const [deployment, setDeployment] = useState<JiraDeployment>('cloud');
  const [site, setSite] = useState('');
  const [projects, setProjects] = useState('');
  const [email, setEmail] = useState('');
  const submit = (e: FormEvent) => {
    e.preventDefault();
    const settings: NewIntegration['settings'] = { kind: 'jira', deployment, site: site.trim(), projects: lines(projects) };
    if (deployment === 'cloud') settings.email = email.trim();
    add.mutate({ name, settings, credential: 'stored' }, { onSuccess: onDone });
  };
  return (
    <form onSubmit={submit} className="flex flex-col gap-3 px-4 py-4">
      <Field label="Name">{(id) => <input id={id} className={inputClass} value={name} onChange={(e) => setName(e.target.value)} />}</Field>
      <Field label="Deployment">
        {(id) => (
          <select id={id} className={inputClass} value={deployment} onChange={(e) => setDeployment(e.target.value as JiraDeployment)}>
            <option value="cloud">Jira Cloud (e-mail and API token)</option>
            <option value="data_center">Jira Data Center (personal access token)</option>
          </select>
        )}
      </Field>
      <Field label="Site" hint="https://jira.example.com">
        {(id) => <input id={id} className={inputClass} value={site} onChange={(e) => setSite(e.target.value)} />}
      </Field>
      <Field label="Projects" hint="Project keys, one per line (DEMO).">
        {(id) => <textarea id={id} className={inputClass} rows={2} value={projects} onChange={(e) => setProjects(e.target.value)} />}
      </Field>
      {deployment === 'cloud' && (
        <Field label="Account e-mail">
          {(id) => <input id={id} type="email" className={inputClass} value={email} onChange={(e) => setEmail(e.target.value)} />}
        </Field>
      )}
      {add.error !== null && <ErrorNote error={add.error} what="connect Jira" />}
      <DialogFooter>
        <Button variant="primary" type="submit" disabled={add.isPending}>
          Connect
        </Button>
      </DialogFooter>
    </form>
  );
}

/**
 * The secret's field. Uncontrolled: the value lives in the input until it is sent, then the input
 * is cleared; no React state, query or mutation cache, or log ever holds it.
 */
function CredentialForm({ integration }: { integration: Integration }) {
  const storeCredential = useStoreCredential();
  const input = useRef<HTMLInputElement>(null);
  const [saved, setSaved] = useState(false);
  const submit = (e: FormEvent) => {
    e.preventDefault();
    const field = input.current;
    if (field === null || field.value.trim() === '' || storeCredential.isPending) return;
    const secret = field.value;
    field.value = '';
    setSaved(false);
    void storeCredential.store(integration.id, secret).then(setSaved);
  };
  const what = integration.settings.kind === 'github' ? 'Token' : integration.settings.deployment === 'cloud' ? 'API token' : 'Personal access token';
  return (
    <form onSubmit={submit} className="flex flex-wrap items-end gap-2">
      <Field label={integration.credential.stored ? `Replace the ${what.toLowerCase()}` : what} className="min-w-64 flex-1">
        {(id) => <input id={id} ref={input} type="password" autoComplete="off" spellCheck={false} className={inputClass} />}
      </Field>
      <Button type="submit" disabled={storeCredential.isPending}>
        Save
      </Button>
      {saved && <span className="text-xs text-ink-2">Saved on the hub.</span>}
      {storeCredential.error !== null && <ErrorNote error={storeCredential.error} what="save the credential" />}
    </form>
  );
}

function CheckResult({ check }: { check: IntegrationCheck }) {
  return (
    <div className="flex flex-col gap-1 rounded-sm bg-sunken px-3 py-2 text-sm" aria-live="polite">
      <p className="font-medium">{check.ok ? 'The connection works.' : 'The connection has problems.'}</p>
      <ul className="flex flex-col gap-0.5">
        {check.checks.map((c, i) => (
          <li key={`${c.scope}-${i}`} className={c.ok ? 'text-ink-2' : 'text-risk'}>
            {c.scope === '' ? 'Credential' : c.scope}: {c.message}
          </li>
        ))}
      </ul>
      {check.warnings.map((w) => (
        <p key={w} className="text-ink-2">
          ⚠ {w}
        </p>
      ))}
    </div>
  );
}

function IntegrationCard({ integration }: { integration: Integration }) {
  const names = useNames();
  const actions = useIntegrationActions();
  const [check, setCheck] = useState<IntegrationCheck | undefined>(undefined);
  const [confirming, setConfirming] = useState(false);
  const state = syncState(integration);
  const settings = integration.settings;
  const scopes = settings.kind === 'github' ? settings.repos : settings.projects;
  const status = integration.status;
  return (
    <section aria-label={integration.name} className="flex flex-col gap-3 rounded-md border border-line bg-card p-4">
      <header className="flex flex-wrap items-center gap-2">
        <h2 className="text-lg font-semibold">{integration.name}</h2>
        <StatusPill tone="neutral">{settings.kind === 'github' ? 'GitHub' : 'Jira'}</StatusPill>
        <StatusPill tone={state.tone}>{state.label}</StatusPill>
        <div className="ml-auto flex items-center gap-2">
          <Button
            onClick={() => actions.test.mutate(integration.id, { onSuccess: setCheck })}
            disabled={actions.test.isPending}
          >
            Test
          </Button>
          <Button onClick={() => actions.sync.mutate(integration.id)} disabled={status.running || actions.sync.isPending}>
            Sync now
          </Button>
          <Button variant="ghost" onClick={() => setConfirming(true)}>
            Remove
          </Button>
        </div>
      </header>
      <dl className="grid grid-cols-[max-content_1fr] gap-x-6 gap-y-1 text-sm">
        <dt className="text-ink-2">{settings.kind === 'github' ? 'Repositories' : `Projects on ${settings.site}`}</dt>
        <dd>{scopes.join(', ')}</dd>
        <dt className="text-ink-2">Credential</dt>
        <dd>
          {integration.credential.source === 'gh_cli'
            ? 'The GitHub CLI on the hub’s machine'
            : integration.credential.stored
              ? 'Stored on the hub'
              : 'None yet'}
        </dd>
        <dt className="text-ink-2">Last sync</dt>
        <dd>{status.last_success_at === undefined ? 'Never' : formatWhen(status.last_success_at)}</dd>
        {status.next_at !== undefined && (
          <>
            <dt className="text-ink-2">Next sync</dt>
            <dd>{formatWhen(status.next_at)}</dd>
          </>
        )}
        {status.last_run !== undefined && (
          <>
            <dt className="text-ink-2">Last run</dt>
            <dd>
              {status.last_run.changes} changes read, {status.last_run.applied} applied, {status.last_run.conflicts} asked
              about, {status.last_run.skipped} out of scope
            </dd>
          </>
        )}
      </dl>
      {status.problems.length > 0 && (
        <div role="alert">
          <ul className="flex flex-col gap-0.5 text-sm text-risk">
            {status.problems.map((p, i) => (
              <li key={`${p.scope}-${i}`}>
                {p.scope === '' ? '' : `${p.scope}: `}
                {p.message}
              </li>
            ))}
          </ul>
        </div>
      )}
      {integration.credential.source === 'stored' && <CredentialForm integration={integration} />}
      {check !== undefined && <CheckResult check={check} />}
      {actions.test.error !== null && <ErrorNote error={actions.test.error} what="test the connection" />}
      <div className="text-sm">
        <h3 className="text-xs font-medium text-ink-2">Linked workstreams</h3>
        {integration.links.length === 0 ? (
          <p className="text-ink-2">None yet. Link a workstream from its page: issues in linked scopes become its tasks.</p>
        ) : (
          <ul>
            {integration.links.map((l) => (
              <li key={`${l.workstream}-${l.scope.key}`}>
                {names.workstream(l.workstream)} · {describeScope(l.scope)}
                {l.title === undefined ? '' : ` (${l.title})`}
              </li>
            ))}
          </ul>
        )}
      </div>
      <Dialog open={confirming} onOpenChange={setConfirming}>
        <DialogContent
          title={`Remove ${integration.name}?`}
          description="The hub forgets the connection, its stored credential and what it read. Tasks it made stay, and workstream links stay as plain links."
        >
          <DialogFooter>
            <Button onClick={() => setConfirming(false)}>Keep</Button>
            <Button variant="primary" onClick={() => actions.remove.mutate(integration.id, { onSuccess: () => setConfirming(false) })}>
              Remove
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </section>
  );
}

export function IntegrationsPage() {
  const integrations = useIntegrations();
  const [connecting, setConnecting] = useState<'github' | 'jira' | undefined>(undefined);
  const list = integrations.data ?? [];
  return (
    <div className="mx-auto flex max-w-4xl flex-col gap-4 px-6 py-6">
      <header className="flex flex-wrap items-center gap-2">
        <h1 className="text-2xl font-semibold">Integrations</h1>
        <div className="ml-auto flex gap-2">
          <Button onClick={() => setConnecting('github')}>Connect GitHub</Button>
          <Button onClick={() => setConnecting('jira')}>Connect Jira</Button>
        </div>
      </header>
      <p className="text-sm text-ink-2">
        The hub reads GitHub and Jira and keeps tasks in step with them. It never writes there. Credentials stay on the
        hub, and no page ever shows one again.
      </p>
      {integrations.error !== null && <ErrorNote error={integrations.error} what="load the integrations" />}
      {integrations.data !== undefined && list.length === 0 && <p className="text-sm text-ink-2">Nothing connected yet.</p>}
      {list.map((i) => (
        <IntegrationCard key={i.id} integration={i} />
      ))}
      <Dialog open={connecting !== undefined} onOpenChange={(open) => !open && setConnecting(undefined)}>
        <DialogContent title={connecting === 'jira' ? 'Connect Jira' : 'Connect GitHub'}>
          {connecting === 'jira' ? (
            <ConnectJira onDone={() => setConnecting(undefined)} />
          ) : (
            <ConnectGithub onDone={() => setConnecting(undefined)} />
          )}
        </DialogContent>
      </Dialog>
    </div>
  );
}
