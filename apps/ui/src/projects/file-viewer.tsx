// The file viewer, shared by the workstream page's Files tab and the console's workbench: text with
// line numbers (coloured for common languages, `highlight.ts`), PNG and JPEG images, PDFs
// (`pdf-view.tsx`, a lazy chunk), anything else as its size. Text is edited in place and saved with
// the revision it was read at; a 409 offers Reload and Overwrite. No raw HTML is ever rendered:
// text stays text, an SVG stays binary, a PDF is only drawn.

import { useVirtualizer } from '@tanstack/react-virtual';
import { Component, lazy, Suspense, useEffect, useId, useMemo, useRef, useState, type KeyboardEvent, type ReactNode } from 'react';
import { ApiError, useLiveQuery } from '../data/index.ts';
import type { FileClient, FileContent } from '../data/files.ts';
import { cx } from '../lib/cx.ts';
import { highlightLines, type Token, type TokenKind } from './highlight.ts';

const PdfView = lazy(() => import('./pdf-view.tsx'));

const BUTTON = 'rounded-sm border border-line px-3 py-1 text-sm disabled:opacity-50';

/** An unsaved edit: its text, what it started from, and the revision that was read. */
export interface FileDraft {
  revision: string;
  base: string;
  text: string;
}

export function fileProblem(error: unknown): string {
  if (error instanceof ApiError) {
    if (error.status === 403) return 'Not allowed';
    if (error.status === 404) return 'Gone';
    if (error.status === 501) return "Files on another machine aren't supported yet.";
    if (error.status === 413) return `Too large to show (${error.size ?? 'unknown'} bytes)`;
  }
  return error instanceof Error ? error.message : 'Could not load files';
}

export type ViewerKind = 'image' | 'pdf' | 'text' | 'binary';

export function viewerKind(file: FileContent): ViewerKind {
  if (file.media_type === 'image/png' || file.media_type === 'image/jpeg') return 'image';
  if (file.media_type === 'application/pdf') return 'pdf';
  if (file.media_type === 'image/svg+xml') return 'binary';
  return file.encoding === 'utf8' ? 'text' : 'binary';
}

/** The file's bytes, whichever way the API carried them. */
export function fileBytes(file: FileContent): Uint8Array {
  return file.encoding === 'base64'
    ? Uint8Array.from(atob(file.content), (char) => char.charCodeAt(0))
    : new TextEncoder().encode(file.content);
}

function ImagePreview({ file, path }: { file: FileContent; path: string }) {
  const image = useRef<HTMLImageElement>(null);
  useEffect(() => {
    const url = URL.createObjectURL(new Blob([fileBytes(file) as BlobPart], { type: file.media_type }));
    if (image.current) image.current.src = url;
    return () => URL.revokeObjectURL(url);
  }, [file]);
  return <img ref={image} alt={path} className="max-w-full" />;
}

class PdfBoundary extends Component<{ children: ReactNode }, { failed: boolean }> {
  override state = { failed: false };
  static getDerivedStateFromError() {
    return { failed: true };
  }
  override render() {
    return this.state.failed ? <p role="alert">Could not load the PDF viewer.</p> : this.props.children;
  }
}

function PdfPreview({ file, path }: { file: FileContent; path: string }) {
  const bytes = useMemo(() => fileBytes(file), [file]);
  return (
    <PdfBoundary>
      <Suspense fallback={<p role="status">Loading the PDF viewer…</p>}>
        <PdfView bytes={bytes} label={path} />
      </Suspense>
    </PdfBoundary>
  );
}

// ─── Text ───────────────────────────────────────────────────────────────────────────────────────

const TOKEN_CLASS: Record<TokenKind, string> = {
  comment: 'text-ink-2 italic',
  string: 'text-accent-text',
  keyword: 'text-risk',
  number: 'text-accent-text',
  type: 'text-warn',
  function: 'text-ok',
  tag: 'text-ok',
  attr: 'text-warn',
  meta: 'text-ink-2',
  inserted: 'text-ok',
  deleted: 'text-risk',
  heading: 'font-semibold text-accent-text',
};

/** Above this many lines only the lines in view are drawn. */
const VIRTUAL_FROM = 2_000;
const LINE_HEIGHT = 20;

function Line({ number, tokens, width }: { number: number; tokens: readonly Token[]; width: number }) {
  return (
    <div className="flex" style={{ height: LINE_HEIGHT }}>
      <span aria-hidden="true" className="mr-4 shrink-0 text-right text-ink-2 select-none" style={{ width: `${width}ch` }}>
        {number}
      </span>
      <pre className="m-0">
        {tokens.length === 0
          ? ' '
          : tokens.map((token, i) =>
              token.kind === null ? (
                token.text
              ) : (
                <span key={i} className={TOKEN_CLASS[token.kind]} data-token={token.kind}>
                  {token.text}
                </span>
              ),
            )}
      </pre>
    </div>
  );
}

/** A file's text with line numbers, coloured when its name says a language the viewer knows. */
export function TextView({ text, path }: { text: string; path: string }) {
  'use no memo'; // TanStack Virtual's instance changes under the React Compiler's memoisation.
  const { language, lines } = useMemo(() => highlightLines(text, path), [text, path]);
  const scroller = useRef<HTMLDivElement>(null);
  const virtual = lines.length > VIRTUAL_FROM;
  // eslint-disable-next-line react-hooks/incompatible-library -- this component opts out of the compiler ('use no memo').
  const virtualizer = useVirtualizer({
    count: virtual ? lines.length : 0,
    getScrollElement: () => scroller.current,
    estimateSize: () => LINE_HEIGHT,
    overscan: 40,
  });
  const width = Math.max(3, String(lines.length).length);
  return (
    <div
      ref={scroller}
      className="max-h-[65vh] overflow-auto rounded-sm border border-line bg-card p-3 font-mono text-sm leading-5"
      tabIndex={0}
      aria-label="File text"
      data-language={language?.id ?? 'plain'}
    >
      {virtual ? (
        <div className="relative min-w-max" style={{ height: virtualizer.getTotalSize() }}>
          {virtualizer.getVirtualItems().map((item) => (
            <div key={item.key} className="absolute top-0 left-0" style={{ transform: `translateY(${item.start}px)` }}>
              <Line number={item.index + 1} tokens={lines[item.index] ?? []} width={width} />
            </div>
          ))}
        </div>
      ) : (
        lines.map((tokens, index) => <Line key={index} number={index + 1} tokens={tokens} width={width} />)
      )}
    </div>
  );
}

// ─── The viewer ─────────────────────────────────────────────────────────────────────────────────

export interface FileViewerProps {
  client: FileClient;
  path: string;
  onDirty?: ((dirty: boolean) => void) | undefined;
  onBusy?: ((busy: boolean) => void) | undefined;
  /**
   * An unsaved edit kept by the caller (with `onDraftChange`), so it outlives the viewer: the
   * workbench unmounts a tab that is off screen. Without them the viewer keeps its own.
   */
  draft?: FileDraft | undefined;
  onDraftChange?: ((draft: FileDraft | undefined) => void) | undefined;
}

export function FileViewer({ client, path, onDirty, onBusy, draft: keptDraft, onDraftChange }: FileViewerProps) {
  const [file, setFile] = useState<FileContent>();
  const [ownDraft, setOwnDraft] = useState<FileDraft>();
  const [error, setError] = useState<unknown>();
  const [busy, setBusy] = useState(false);
  const [saved, setSaved] = useState(false);
  const heading = useId();
  const kept = onDraftChange !== undefined;
  const draft = kept ? keptDraft : ownDraft;
  const setDraft = (next: FileDraft | undefined) => (kept ? onDraftChange(next) : setOwnDraft(next));
  const dirty = draft !== undefined && draft.text !== draft.base;
  useEffect(() => {
    onDirty?.(dirty);
  }, [dirty, onDirty]);
  useEffect(() => {
    onBusy?.(busy);
  }, [busy, onBusy]);
  useEffect(() => {
    const controller = new AbortController();
    void client
      .read(path, controller.signal)
      .then((value) => {
        if (!controller.signal.aborted) setFile(value);
      })
      .catch((reason: unknown) => {
        if (!controller.signal.aborted) setError(reason);
      });
    return () => controller.abort();
  }, [client, path]);

  async function save(overwrite = false) {
    if (draft === undefined) return;
    setBusy(true);
    setError(undefined);
    setSaved(false);
    try {
      const revision = overwrite ? (await client.read(path)).revision : draft.revision;
      const result = await client.write(path, revision, draft.text);
      setFile(result);
      setDraft(undefined);
      setSaved(true);
    } catch (reason) {
      setError(reason);
    } finally {
      setBusy(false);
    }
  }
  async function reload() {
    if (dirty && !window.confirm('Discard unsaved changes and reload?')) return;
    setBusy(true);
    try {
      setFile(await client.read(path));
      setDraft(undefined);
      setError(undefined);
      setSaved(false);
    } catch (reason) {
      setError(reason);
    } finally {
      setBusy(false);
    }
  }
  const conflict = error instanceof ApiError && error.status === 409;
  const kind = file === undefined ? undefined : viewerKind(file);
  // An edit kept from before (the tab was off screen) shows as soon as the file is known again.
  const editing = draft !== undefined && kind === 'text';
  return (
    <div role="group" aria-labelledby={heading} className="min-w-0 flex-1 space-y-3">
      <h2 id={heading} className="font-medium break-all">
        {path}
      </h2>
      {error !== undefined && (
        <div role="alert">
          <p>{conflict ? 'Changed since you opened it' : fileProblem(error)}</p>
          {conflict && (
            <div className="flex gap-2">
              <button className={BUTTON} disabled={busy} onClick={() => void reload()}>
                Reload
              </button>
              <button className={BUTTON} disabled={busy} onClick={() => void save(true)}>
                Overwrite
              </button>
            </div>
          )}
          {!conflict && file === undefined && (
            <button className={BUTTON} disabled={busy} onClick={() => void reload()}>
              Retry file
            </button>
          )}
        </div>
      )}
      {file === undefined && error === undefined && <p role="status">Loading file…</p>}
      {saved && <p role="status">Saved</p>}
      {file !== undefined && (
        <>
          {kind === 'image' && <ImagePreview file={file} path={path} />}
          {kind === 'pdf' && <PdfPreview file={file} path={path} />}
          {kind === 'binary' && <p>Binary, {file.size} bytes</p>}
          {kind === 'text' &&
            (editing ? (
              <>
                <textarea
                  aria-label="Edit file text"
                  className="min-h-80 w-full rounded-sm border border-line bg-card p-3 font-mono text-sm"
                  value={draft.text}
                  disabled={busy}
                  onChange={(event) => setDraft({ ...draft, text: event.target.value })}
                />
                <div className="flex gap-2">
                  <button className={BUTTON} disabled={busy} onClick={() => void save()}>
                    Save
                  </button>
                  <button
                    className={BUTTON}
                    disabled={busy}
                    onClick={() => {
                      if (!dirty || window.confirm('Discard unsaved changes?')) setDraft(undefined);
                    }}
                  >
                    Cancel edit
                  </button>
                </div>
              </>
            ) : (
              <>
                <button
                  className={BUTTON}
                  onClick={() => {
                    setDraft({ revision: file.revision, base: file.content, text: file.content });
                    setSaved(false);
                  }}
                >
                  Edit
                </button>
                <TextView text={file.content} path={path} />
              </>
            ))}
        </>
      )}
    </div>
  );
}

// ─── The folder tree ────────────────────────────────────────────────────────────────────────────

function Folder({ client, scope, path, openFile }: { client: FileClient; scope: readonly unknown[]; path: string; openFile: (path: string) => void }) {
  const listing = useLiveQuery({
    queryKey: [...scope, 'list', path],
    queryFn: ({ signal }) => client.list(path, signal),
    retry: false,
    staleTime: 0,
  });
  const [expanded, setExpanded] = useState<string[]>([]);
  if (listing.isPending) return <p role="status">Loading folder…</p>;
  if (listing.error) {
    return (
      <div>
        <p role="alert">{fileProblem(listing.error)}</p>
        <button className={BUTTON} onClick={() => void listing.refetch()}>
          Retry folder
        </button>
      </div>
    );
  }
  return (
    <>
      {listing.data.entries.length === 0 && <p className="text-sm text-ink-2">Empty folder</p>}
      <ul className="space-y-1 pl-3">
        {listing.data.entries.map((entry) => {
          const child = path ? `${path}/${entry.name}` : entry.name;
          const isOpen = expanded.includes(child);
          return (
            <li key={entry.name}>
              {entry.kind === 'link' ? (
                <span className="text-sm text-ink-2">{entry.name} (link, cannot open)</span>
              ) : (
                <button
                  className="rounded-sm px-2 py-1 text-left text-sm break-all hover:bg-sunken"
                  aria-expanded={entry.kind === 'folder' ? isOpen : undefined}
                  onClick={() =>
                    entry.kind === 'folder'
                      ? setExpanded(isOpen ? expanded.filter((p) => p !== child) : [...expanded, child])
                      : openFile(child)
                  }
                  onKeyDown={(event) => {
                    if (entry.kind === 'folder' && (event.key === 'ArrowRight' || event.key === 'ArrowLeft')) {
                      event.preventDefault();
                      setExpanded(
                        event.key === 'ArrowRight' ? [...new Set([...expanded, child])] : expanded.filter((p) => p !== child),
                      );
                    }
                  }}
                >
                  {entry.kind === 'folder' ? `${isOpen ? '▾' : '▸'} ${entry.name}` : entry.name}
                </button>
              )}
              {entry.kind === 'folder' && isOpen && <Folder client={client} scope={scope} path={child} openFile={openFile} />}
            </li>
          );
        })}
      </ul>
      {listing.data.truncated && (
        <p role="status" className="text-sm text-ink-2">
          Listing truncated; some entries are omitted.
        </p>
      )}
    </>
  );
}

function treeKeys(event: KeyboardEvent<HTMLElement>) {
  if (!['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(event.key)) return;
  const buttons = [...event.currentTarget.querySelectorAll<HTMLButtonElement>('button')];
  const index = buttons.indexOf(event.target as HTMLButtonElement);
  if (index < 0) return;
  event.preventDefault();
  const next =
    event.key === 'Home'
      ? 0
      : event.key === 'End'
        ? buttons.length - 1
        : Math.max(0, Math.min(buttons.length - 1, index + (event.key === 'ArrowDown' ? 1 : -1)));
  buttons[next]?.focus();
}

/**
 * A location's folders, one level at a time: Up/Down and Home/End move between entries, Left/Right
 * close and open folders. Links are listed but cannot be opened. `scope` keys the listings in the
 * query cache (listings only; file contents never enter it).
 */
export function FileTree({
  client,
  scope,
  openFile,
  className,
}: {
  client: FileClient;
  scope: readonly unknown[];
  openFile: (path: string) => void;
  className?: string;
}) {
  return (
    <nav aria-label="Folder tree" onKeyDown={treeKeys} className={cx('min-w-0', className)}>
      <Folder client={client} scope={scope} path="" openFile={openFile} />
    </nav>
  );
}
