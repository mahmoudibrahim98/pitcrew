// A workstream's links upstream (api-v1.md, "Linking a workstream upstream"): each link, the
// upstream title once a sync has seen it, and its integration's last sync; and a dialog to link a
// repository, milestone, Jira project or epic, or to unlink one.

import { useState, type FormEvent } from 'react';
import { Button, Dialog, DialogContent, DialogFooter } from '../../design/index.ts';
import type { ExternalRef, Workstream } from '../../data/index.ts';
import { formatWhen, isWebUrl } from '../format.ts';
import { ErrorNote, Field, inputClass } from '../ui.tsx';
import { describeScope, integrationOf, scopesOf, useIntegrationActions, useIntegrations } from './api.ts';

/** The links dialog's body: the links, each with Unlink, and a form to add one. */
export function LinkEditor({ workstream, onDone }: { workstream: Workstream; onDone(): void }) {
  const integrations = useIntegrations().data ?? [];
  const { link } = useIntegrationActions();
  const scopes = integrations.flatMap((i) => scopesOf(i).map((scope) => ({ integration: i, scope })));
  const [picked, setPicked] = useState(0);
  const [within, setWithin] = useState('');
  const current = workstream.external;
  const save = (external: ExternalRef[]) =>
    link.mutate({ workstream: workstream.id, external }, { onSuccess: () => setWithin('') });
  const add = (e: FormEvent) => {
    e.preventDefault();
    const choice = scopes[picked];
    if (choice === undefined) return;
    const narrower = within.trim();
    const base = choice.scope;
    let added: ExternalRef = base;
    if (narrower !== '' && base.system === 'github') {
      added = { system: 'github', key: `${base.key}#milestone:${narrower}`, url: `https://github.com/${base.key}/milestone/${narrower}` };
    } else if (narrower !== '' && base.system === 'jira') {
      const site = choice.integration.settings.kind === 'jira' ? choice.integration.settings.site : '';
      added = { system: 'jira', key: narrower, url: `${site}/browse/${narrower}` };
    }
    if (current.some((l) => l.system === added.system && l.key === added.key)) return;
    save([...current, added]);
  };
  const choice = scopes[picked];
  return (
    <div className="flex flex-col gap-3 px-4 py-4">
      {current.length === 0 ? (
        <p className="text-sm text-ink-2">Not linked to anything upstream.</p>
      ) : (
        <ul className="flex flex-col gap-1 text-sm">
          {current.map((l) => (
            <li key={`${l.system}-${l.key}`} className="flex items-center gap-2">
              <span>{describeScope(l)}</span>
              <Button
                variant="ghost"
                className="ml-auto"
                aria-label={`Unlink ${describeScope(l)}`}
                onClick={() => save(current.filter((x) => x !== l))}
              >
                Unlink
              </Button>
            </li>
          ))}
        </ul>
      )}
      {scopes.length === 0 ? (
        <p className="text-sm text-ink-2">Connect GitHub or Jira in Integrations first.</p>
      ) : (
        <form onSubmit={add} className="flex flex-col gap-3">
          <Field label="Link to">
            {(id) => (
              <select id={id} className={inputClass} value={picked} onChange={(e) => setPicked(Number(e.target.value))}>
                {scopes.map((s, i) => (
                  <option key={`${s.integration.id}-${s.scope.key}`} value={i}>
                    {describeScope(s.scope)}
                  </option>
                ))}
              </select>
            )}
          </Field>
          <Field
            label={choice?.scope.system === 'jira' ? 'Only this epic (optional)' : 'Only this milestone (optional)'}
            hint={choice?.scope.system === 'jira' ? 'An epic’s key, such as DEMO-5.' : 'The milestone’s number, such as 3.'}
          >
            {(id) => <input id={id} className={inputClass} value={within} onChange={(e) => setWithin(e.target.value)} />}
          </Field>
          {link.error !== null && <ErrorNote error={link.error} what="change the links" />}
          <DialogFooter>
            <Button onClick={onDone}>Done</Button>
            <Button variant="primary" type="submit" disabled={link.isPending}>
              Link
            </Button>
          </DialogFooter>
        </form>
      )}
    </div>
  );
}

/** The workstream's links upstream and their last sync, with a way to change them. */
export function WorkstreamLinks({ workstream }: { workstream: Workstream }) {
  const integrations = useIntegrations().data ?? [];
  const [editing, setEditing] = useState(false);
  const links = workstream.external;
  return (
    <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-ink-2" aria-label="Linked upstream">
      {links.length === 0 ? (
        <span>Not linked upstream.</span>
      ) : (
        links.map((l) => {
          const integration = integrationOf(integrations, l);
          const title = integration?.links.find((x) => x.workstream === workstream.id && x.scope.key === l.key)?.title;
          const last = integration?.status.last_success_at;
          const label = `${describeScope(l)}${title === undefined ? '' : ` · ${title}`}`;
          return (
            <span key={`${l.system}-${l.key}`} className="flex items-center gap-1">
              {l.url !== undefined && isWebUrl(l.url) ? (
                <a href={l.url} target="_blank" rel="noreferrer noopener" className="underline underline-offset-2 hover:text-ink">
                  {label}
                </a>
              ) : (
                <span>{label}</span>
              )}
              <span>
                {integration === undefined
                  ? '(not synced)'
                  : last === undefined
                    ? '(not synced yet)'
                    : `(last sync ${formatWhen(last)})`}
              </span>
            </span>
          );
        })
      )}
      <Button variant="ghost" className="h-6 px-1.5 text-xs" onClick={() => setEditing(true)}>
        Edit links
      </Button>
      <Dialog open={editing} onOpenChange={setEditing}>
        <DialogContent title={`Links of ${workstream.name}`}>
          <LinkEditor workstream={workstream} onDone={() => setEditing(false)} />
        </DialogContent>
      </Dialog>
    </div>
  );
}
