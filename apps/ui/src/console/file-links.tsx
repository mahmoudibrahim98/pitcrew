import { createContext, useContext } from 'react';
import { useSession, type Workstream } from '../data/index.ts';
import { useWorkstreamById } from './data.ts';
import type { TabRef } from './workbench/layout.ts';

export const FileOpenContext = createContext<((ref: TabRef) => void) | undefined>(undefined);

/** A transcript path becomes a file target only inside a location on this session's machine. */
export function resolveFilePath(path: string, cwd: string, machine: string, stream: Workstream) {
  const normalize = (value: string) => value.replaceAll('\\', '/').replace(/\/$/, '');
  const absolute = /^(?:\/|[A-Za-z]:[\\/])/.test(path);
  const full = normalize(absolute ? path : `${cwd}/${path}`);
  const windows = /^[A-Za-z]:\//.test(full);
  const compare = (value: string) => windows ? value.toLowerCase() : value;
  const locations = stream.locations.map((loc, index) => ({ ...loc, index, root: normalize(loc.path) }))
    .filter(loc => loc.machine === machine && compare(full).startsWith(`${compare(loc.root)}/`))
    .sort((a, b) => b.root.length - a.root.length);
  const location = locations[0];
  if (!location) return undefined;
  const relative = full.slice(location.root.length + 1);
  if (relative.split('/').some(part => !part || part === '.' || part === '..') || relative.includes('\0')) return undefined;
  return { kind: 'file' as const, workstream: stream.id, location: location.index, path: relative };
}

export function useTranscriptFile(sessionId: string | undefined, path: string) {
  const session = useSession(sessionId).data;
  const stream = useWorkstreamById(session?.workstream).data;
  const open = useContext(FileOpenContext);
  const ref = session && stream ? resolveFilePath(path, session.cwd, session.machine, stream) : undefined;
  return open && ref ? (line?: number) => open({ ...ref, ...(line === undefined ? {} : { line }) }) : undefined;
}

export function TranscriptFile({ sessionId, path, line }: { sessionId: string; path: string; line?: number | undefined }) {
  const open = useTranscriptFile(sessionId, path);
  return open ? <button className="px-2 py-1 text-left font-mono text-xs text-accent-text underline" onClick={() => open(line)}>Open {path}{line === undefined ? '' : `:${line}`}</button> : null;
}

export function changedLine(diff: string | undefined): number | undefined {
  if (!diff) return undefined;
  let line: number | undefined;
  for (const text of diff.split('\n')) {
    const header = text.match(/^@@ -\d+(?:,\d+)? \+(\d+)/);
    if (header) { line = Number(header[1]); continue; }
    if (line === undefined) continue;
    if (text.startsWith('+') || text.startsWith('-')) return Math.max(1, line);
    if (text.startsWith(' ')) line++;
  }
  return undefined;
}
