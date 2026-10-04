// @vitest-environment happy-dom
// The shared file viewer (the Files tab's and the workbench's): colouring that stays text, long
// files drawn in part, PDFs through pdf.js (mocked here: happy-dom has no canvas; the Playwright
// spec draws a real one), and an edit kept outside the viewer saved with the revision it was read at.

import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { useState } from 'react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { ApiError } from '../../data/index.ts';
import type { FileClient, FileContent } from '../../data/files.ts';
import { FileViewer, viewerKind, type FileDraft } from '../file-viewer.tsx';

const pdf = vi.hoisted(() => {
  const render = vi.fn(() => ({ promise: Promise.resolve(), cancel: vi.fn() }));
  const page = { getViewport: ({ scale }: { scale: number }) => ({ width: 300 * scale, height: 144 * scale }), render };
  return {
    render,
    getDocument: vi.fn<(options: { data: Uint8Array; useWasm: boolean }) => { promise: Promise<unknown>; destroy(): Promise<void> }>(
      () => ({
        promise: Promise.resolve({ numPages: 2, getPage: () => Promise.resolve(page) }),
        destroy: vi.fn(() => Promise.resolve()),
      }),
    ),
  };
});
vi.mock('pdfjs-dist/legacy/build/pdf.mjs', () => ({ GlobalWorkerOptions: {}, getDocument: pdf.getDocument }));
vi.mock('pdfjs-dist/legacy/build/pdf.worker.min.mjs?url', () => ({ default: '/pdf.worker.mjs' }));

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  vi.clearAllMocks();
});

const REVISION = 'a'.repeat(64);
const text = (content: string, extra: Partial<FileContent> = {}): FileContent => ({
  size: content.length,
  media_type: 'text/plain',
  revision: REVISION,
  encoding: 'utf8',
  content,
  ...extra,
});

function fakeClient(file: FileContent, write?: FileClient['write']): FileClient {
  return {
    list: vi.fn(),
    read: vi.fn(() => Promise.resolve(file)),
    write: write ?? vi.fn((_path: string, _revision: string, content: string) => Promise.resolve(text(content, { revision: 'b'.repeat(64) }))),
  } as unknown as FileClient;
}

describe('the file viewer', () => {
  it('colours text by the file name, and it stays text', async () => {
    const { container } = render(
      <FileViewer client={fakeClient(text('const html = "<b>bold</b>";\n// a comment\n'))} path="src/page.ts" />,
    );
    const box = await screen.findByLabelText('File text');
    expect(box.getAttribute('data-language')).toBe('javascript');
    expect(box.querySelector('[data-token="keyword"]')?.textContent).toBe('const');
    expect(box.querySelector('[data-token="string"]')?.textContent).toBe('"<b>bold</b>"');
    expect(box.querySelector('[data-token="comment"]')?.textContent).toBe('// a comment');
    expect(container.querySelector('b')).toBeNull();
    // A name it does not know shows plain text.
    render(<FileViewer client={fakeClient(text('const x'))} path="notes.txt" />);
    await waitFor(() => expect(screen.getAllByLabelText('File text')[1]?.getAttribute('data-language')).toBe('plain'));
  });

  it('draws only part of a very long file', async () => {
    const long = Array.from({ length: 5_000 }, (_, i) => `line ${i}`).join('\n');
    render(<FileViewer client={fakeClient(text(long))} path="big.log" />);
    const box = await screen.findByLabelText('File text');
    expect(box.querySelectorAll('pre').length).toBeLessThan(500);
  });

  it('shows a PDF page by page, with zoom, through pdf.js without WebAssembly', async () => {
    vi.stubGlobal('IntersectionObserver', undefined);
    const bytes = '%PDF-1.4 synthetic';
    expect(viewerKind(text(bytes, { media_type: 'application/pdf' }))).toBe('pdf');
    expect(viewerKind(text(btoa(bytes), { media_type: 'application/pdf', encoding: 'base64' }))).toBe('pdf');
    render(
      <FileViewer
        client={fakeClient(text(btoa(bytes), { media_type: 'application/pdf', encoding: 'base64' }))}
        path="paper.pdf"
      />,
    );
    await screen.findByText('2 pages');
    const options = pdf.getDocument.mock.calls[0]?.[0];
    expect(options?.useWasm).toBe(false);
    expect(new TextDecoder().decode(options?.data)).toBe(bytes);
    await waitFor(() => expect(screen.getByRole('img', { name: 'Page 1 of 2' }).getAttribute('data-page-state')).toBe('drawn'));
    expect(screen.getByRole('img', { name: 'Page 2 of 2' })).toBeTruthy();
    expect(screen.getByText('100%')).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: 'Zoom in' }));
    expect(screen.getByText('125%')).toBeTruthy();
    // No Edit for a PDF.
    expect(screen.queryByRole('button', { name: 'Edit' })).toBeNull();
  });

  it('says why a PDF cannot be shown', async () => {
    pdf.getDocument.mockImplementationOnce(() => ({
      promise: Promise.reject(Object.assign(new Error('No password given'), { name: 'PasswordException' })),
      destroy: vi.fn(() => Promise.resolve()),
    }));
    render(<FileViewer client={fakeClient(text('%PDF', { media_type: 'application/pdf' }))} path="locked.pdf" />);
    await screen.findByText('This PDF is protected by a password, so it cannot be shown here.');
  });

  it('keeps an edit outside the viewer, and saves it with the revision it was read at', async () => {
    const conflict = new ApiError('conflict', 'File revision changed.', 409, { current_revision: 'c'.repeat(64) });
    const write = vi.fn<FileClient['write']>().mockRejectedValueOnce(conflict);
    const client = fakeClient(text('first\n'), write);
    let kept: FileDraft | undefined;
    function Host({ shown }: { shown: boolean }) {
      const [draft, setDraft] = useState<FileDraft>();
      kept = draft;
      return shown ? <FileViewer client={client} path="notes.md" draft={draft} onDraftChange={setDraft} /> : <p>elsewhere</p>;
    }
    const view = render(<Host shown />);
    fireEvent.click(await screen.findByRole('button', { name: 'Edit' }));
    fireEvent.change(screen.getByLabelText('Edit file text'), { target: { value: 'second\n' } });
    expect(kept).toEqual({ revision: REVISION, base: 'first\n', text: 'second\n' });

    // Off screen and back: the edit is where it was.
    view.rerender(<Host shown={false} />);
    view.rerender(<Host shown />);
    expect(((await screen.findByLabelText('Edit file text')) as HTMLTextAreaElement).value).toBe('second\n');

    // Saved with the revision read before; the 409 offers Reload and Overwrite, which re-reads first.
    await act(async () => fireEvent.click(screen.getByRole('button', { name: 'Save' })));
    expect(write).toHaveBeenLastCalledWith('notes.md', REVISION, 'second\n');
    await screen.findByText('Changed since you opened it');
    write.mockResolvedValueOnce(text('second\n', { revision: 'd'.repeat(64) }));
    await act(async () => fireEvent.click(screen.getByRole('button', { name: 'Overwrite' })));
    await screen.findByText('Saved');
    expect(kept).toBeUndefined();
  });
});
