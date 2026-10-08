// A file in a pane: the Files tab's viewer (src/projects), with its unsaved edit kept in
// `drafts.ts`, so switching tabs (which unmounts this one) loses nothing.

import { Suspense, useMemo } from 'react';
import { fileClient } from '../../data/files.ts';
import { useApi } from '../../data/index.ts';
import { FileViewer } from '../../projects/index.ts';
import { useWorkstreamById } from '../data.ts';
import { getDraft, setDraft, useDrafts } from './drafts.ts';

export interface FileTabProps {
  ws: string;
  tabId: string;
  paneNumber: number;
  workstream: string;
  location: number;
  path: string;
  line?: number | undefined;
}

export function FileTab({ ws, tabId, paneNumber, workstream, location, path, line }: FileTabProps) {
  const api = useApi();
  const client = useMemo(() => fileClient(api, workstream, location), [api, workstream, location]);
  const stream = useWorkstreamById(workstream).data;
  useDrafts();
  const draft = getDraft(ws, tabId);
  const folder = stream?.locations[location]?.path;
  return (
    <div data-pane="file" className="min-h-0 flex-1 overflow-y-auto">
      {/* Focusable but not a tab stop, like the chat scroller: F6 lands here. */}
      <div
        tabIndex={-1}
        data-file-focus=""
        className="space-y-1 p-4 outline-none focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-accent focus-visible:outline-solid"
      >
        <p className="text-xs break-all text-ink-2">
          {stream?.name ?? 'Workstream'}
          {folder !== undefined && ` · ${folder}`}
        </p>
        <Suspense fallback={<p role="status">Loading the file viewer…</p>}>
          <FileViewer
            client={client}
            path={path}
            line={line}
            breadcrumbsLabel={`File breadcrumbs in pane ${paneNumber}`}
            copyPath={folder === undefined ? path : `${folder.replace(/[\\/]$/, '')}/${path}`}
            onFolder={(directory) => window.dispatchEvent(new CustomEvent('pitcrew:file-folder', { detail: { workstream, location, directory } }))}
            draft={draft}
            onDraftChange={(next) => setDraft(ws, tabId, next)}
          />
        </Suspense>
      </div>
    </div>
  );
}
