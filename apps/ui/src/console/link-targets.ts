// What "Link to…" offers: a project (its default workstream), any workstream, or a new one; and
// which sessions go with the one being linked. React-free, so it is tested on its own.

import { subagentsByParent, type Location, type Project, type Session, type Workstream } from '../data/index.ts';

/** A path without trailing separators, `\` as `/`, for comparing folders. */
function folder(path: string): string {
  return path.replace(/\\/g, '/').replace(/\/+$/, '');
}

/** Whether `inner` is `outer` or a folder below it. */
export function within(inner: string, outer: string): boolean {
  const a = folder(inner);
  const b = folder(outer);
  return a === b || a.startsWith(`${b}/`);
}

/**
 * A project's default workstream: the one with a location at the project's root (same machine
 * and folder, no branch), which creating from a scan always makes (api-v1.md, "Sessions").
 */
export function defaultWorkstream(project: Project, workstreams: readonly Workstream[]): Workstream | undefined {
  const root = project.root;
  if (root === undefined) return undefined;
  return workstreams.find(
    (w) =>
      w.project === project.id &&
      w.locations.some((l) => l.machine === root.machine && l.branch === undefined && folder(l.path) === folder(root.path)),
  );
}

/** The workstream a project's default would be made as, when it has none: `Main`, at its root. */
export function newDefaultWorkstream(project: Project): { project: string; name: string; locations: Location[] } {
  return { project: project.id, name: 'Main', locations: project.root === undefined ? [] : [{ ...project.root }] };
}

/** The sessions that can be linked with `session` at once. */
export interface Companions {
  /** Its sub-agents: part of its work, so linked with it unless unticked. */
  subagents: Session[];
  /** Unsorted sessions (no workstream, no task) in its folder or below it, on its machine. */
  unsorted: Session[];
}

export function companions(session: Session, sessions: readonly Session[]): Companions {
  const byParent = subagentsByParent(sessions);
  const subagents = byParent.get(session.id) ?? [];
  const nested = new Set([...byParent.values()].flat().map((s) => s.id));
  const unsorted = sessions
    .filter(
      (s) =>
        s.id !== session.id &&
        !nested.has(s.id) &&
        s.workstream === undefined &&
        s.task === undefined &&
        s.machine === session.machine &&
        within(s.cwd, session.cwd),
    )
    .sort((a, b) => b.last_activity - a.last_activity);
  return { subagents, unsorted };
}

/** What the choice names: a workstream (its id), a project (`project:<id>`) or a new one in a project (`new:<id>`). */
export type LinkTarget =
  | { kind: 'workstream'; id: string }
  | { kind: 'project'; id: string }
  | { kind: 'new'; project: string };

export function parseTarget(value: string): LinkTarget | undefined {
  if (value === '') return undefined;
  const at = value.indexOf(':');
  if (at === -1) return { kind: 'workstream', id: value };
  const [kind, id] = [value.slice(0, at), value.slice(at + 1)];
  if (id === '') return undefined;
  if (kind === 'project') return { kind: 'project', id };
  if (kind === 'new') return { kind: 'new', project: id };
  return undefined;
}
