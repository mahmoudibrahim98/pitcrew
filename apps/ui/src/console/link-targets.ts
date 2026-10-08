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
 * and folder, no branch), which creating from a scan always makes (api-v1.md, "Sessions"). A
 * project without a root has its `Main` with no location, as "Link to…" makes it.
 */
export function defaultWorkstream(project: Project, workstreams: readonly Workstream[]): Workstream | undefined {
  const root = project.root;
  if (root === undefined) {
    return workstreams.find((w) => w.project === project.id && w.name === DEFAULT_NAME && w.locations.length === 0);
  }
  return workstreams.find(
    (w) =>
      w.project === project.id &&
      w.locations.some((l) => l.machine === root.machine && l.branch === undefined && folder(l.path) === folder(root.path)),
  );
}

const DEFAULT_NAME = 'Main';

/** The workstream a project's default would be made as, when it has none: `Main`, at its root. */
export function newDefaultWorkstream(project: Project): { project: string; name: string; locations: Location[] } {
  return { project: project.id, name: DEFAULT_NAME, locations: project.root === undefined ? [] : [{ ...project.root }] };
}

/** What [`linkedWorkstream`] needs from the hub. */
export interface TargetHub {
  /** The project, as listed. */
  project(id: string): Project | undefined;
  /** The workstreams as the hub has them now (fetched again, not a cache that may be stale). */
  workstreams(): Promise<readonly Workstream[]>;
  createWorkstream(w: { project: string; name: string; locations?: Location[] }): Promise<{ id: string }>;
}

/**
 * The workstream a link to `target` goes to, made at most once. A project's default is looked up
 * in the workstreams as the hub has them now, so one made elsewhere since the list was loaded is
 * used; it is made only when there is none. One made for a choice is kept in `made` (by choice),
 * so trying again after a link failed uses it rather than making another.
 */
export async function linkedWorkstream(
  target: LinkTarget,
  newName: string,
  hub: TargetHub,
  made: Map<string, string>,
): Promise<string> {
  if (target.kind === 'workstream') return target.id;
  const key = target.kind === 'project' ? `project:${target.id}` : `new:${target.project}:${newName}`;
  const before = made.get(key);
  if (before !== undefined) return before;
  let id: string;
  if (target.kind === 'project') {
    const project = hub.project(target.id);
    if (project === undefined) throw new Error('That project is gone; choose again.');
    const existing = defaultWorkstream(project, await hub.workstreams());
    id = existing?.id ?? (await hub.createWorkstream(newDefaultWorkstream(project))).id;
  } else {
    if (newName === '') throw new Error('Name the new workstream.');
    id = (await hub.createWorkstream({ project: target.project, name: newName })).id;
  }
  made.set(key, id);
  return id;
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
