// Members: people and agents in one table, owners shown for agents.

import { useMembers, useTasks } from './data.ts';
import { usePersonas, useTeams } from './new-entities.tsx';
import { Avatar, memberLabel } from './people.tsx';
import { ErrorNote } from './ui.tsx';

export function MembersPage() {
  const members = useMembers();
  const tasks = useTasks();
  const personas = usePersonas();
  const teams = useTeams();
  const byId = new Map((members.data ?? []).map((m) => [m.id, m]));
  const openByMember = new Map<string, number>();
  for (const t of tasks.data ?? []) {
    if (t.assignee === undefined || t.archived === true || t.status === 'done' || t.status === 'canceled') continue;
    openByMember.set(t.assignee, (openByMember.get(t.assignee) ?? 0) + 1);
  }
  const rows = [...(members.data ?? [])].sort((a, b) =>
    a.kind === b.kind ? a.handle.localeCompare(b.handle) : a.kind === 'human' ? -1 : 1,
  );

  return (
    <div className="mx-auto flex max-w-4xl flex-col gap-4 px-6 py-6">
      <h1 className="text-2xl font-semibold">Members</h1>
      <section aria-label="Agent recipes" className="flex flex-col gap-2">
        <h2 className="text-lg font-semibold">Agent recipes</h2>
        {personas.error !== null && <ErrorNote error={personas.error} what="load agent recipes" />}
        <ul>{(personas.data ?? []).map((p) => <li key={p.id} className="text-sm">{p.name} · {p.engine}{p.model === undefined ? '' : ` · ${p.model}`}</li>)}</ul>
      </section>
      <section aria-label="Teams" className="flex flex-col gap-2">
        <h2 className="text-lg font-semibold">Teams</h2>
        {teams.error !== null && <ErrorNote error={teams.error} what="load teams" />}
        <ul>{(teams.data ?? []).map((t) => <li key={t.id} className="text-sm">{t.name} · {t.members.map((id) => byId.get(id)?.name ?? id).join(', ')}</li>)}</ul>
      </section>
      {members.error !== null && <ErrorNote error={members.error} what="load the members" />}
      {members.data !== undefined && rows.length === 0 && <p className="text-sm text-ink-2">No members yet.</p>}
      {rows.length > 0 && (
        // tabIndex so a keyboard user can scroll it horizontally: the table has no links of its
        // own to tab through first (unlike WorkstreamsTable's names).
        <div tabIndex={0} className="overflow-x-auto rounded-md border border-line bg-card">
          <table className="w-full text-sm">
            <caption className="sr-only">Members</caption>
            <thead>
              <tr className="border-b border-line text-left text-xs text-ink-2">
                <th scope="col" className="px-4 py-2 font-medium">
                  Member
                </th>
                <th scope="col" className="px-4 py-2 font-medium">
                  Kind
                </th>
                <th scope="col" className="px-4 py-2 font-medium">
                  Owner
                </th>
                <th scope="col" className="px-4 py-2 text-right font-medium">
                  Open tasks
                </th>
              </tr>
            </thead>
            <tbody>
              {rows.map((m) => {
                const owner = m.owner === undefined ? undefined : byId.get(m.owner);
                return (
                  <tr key={m.id} className="border-b border-line last:border-0">
                    <th scope="row" className="flex items-center gap-2 px-4 py-2.5 text-left font-medium">
                      <Avatar member={m} owner={owner} decorative />
                      <span>{memberLabel(m, owner)}</span>
                      <span className="text-xs text-ink-2">{m.handle}</span>
                    </th>
                    <td className="px-4 py-2.5 text-ink-2">{m.kind === 'human' ? 'Person' : 'Agent'}</td>
                    <td className="px-4 py-2.5 text-ink-2">{owner?.handle ?? '—'}</td>
                    <td className="px-4 py-2.5 text-right tabular-nums">{openByMember.get(m.id) ?? 0}</td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}
