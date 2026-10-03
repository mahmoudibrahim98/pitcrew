import { useEffect, useState } from 'react';
import { useMoveCursor, useReadCursors } from '../data/cursors.ts';
import { useActivity, withRevisions } from './data.ts';
import { ErrorNote } from './ui.tsx';

/** Mounted for one visit; the initial loaded revision is marked after a short dwell. */
export function ReadScope({ scope }: { scope: string }) {
  // Unfiltered revisions are contiguous. A workspace revision is also a safe upper bound
  // for the project/workstream snapshot loaded on this visit.
  const activity = useActivity();
  const cursors = useReadCursors();
  const page = activity.data;
  const cursorData = cursors.data;
  if (page === undefined || cursorData === undefined) return null;
  const latest = withRevisions(page).filter(({ event }) => event.body.type !== 'cursor_moved').at(-1)?.rev ?? 0;
  const seen = cursorData.find((c) => c.scope === scope)?.rev ?? 0;
  return <Dwell key={scope} scope={scope} rev={latest} seen={seen} />;
}

function Dwell({ scope, rev, seen }: { scope: string; rev: number; seen: number }) {
  const [initial] = useState({ rev, seen });
  const { mutate, error } = useMoveCursor();
  useEffect(() => {
    if (initial.rev <= initial.seen) return;
    const controller = new AbortController();
    const timer = setTimeout(() => {
      mutate({ scope, rev: initial.rev, signal: controller.signal });
    }, 1000);
    return () => {
      clearTimeout(timer);
      controller.abort();
    };
  }, [scope, initial, mutate]);
  return error === null ? null : <ErrorNote error={error} what="save your read cursor" />;
}
