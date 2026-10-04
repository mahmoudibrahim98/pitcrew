// @vitest-environment happy-dom
import { fireEvent, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { ApiError, type Workstream } from '../../data/index.ts';
import { fileClient, type FileContent } from '../../data/files.ts';
import { FilesTab, fileProblem, viewerKind } from '../files.tsx';
import { demo, otherClient, renderWithHub, startHub, stopHub, type Hub } from './harness.tsx';

let hub: Hub;
let stream: Workstream;
beforeEach(async () => { hub = await startHub(); stream = await otherClient(hub).workstream(demo.submission); });
afterEach(async () => { vi.restoreAllMocks(); vi.unstubAllGlobals(); await stopHub(hub); });
const content: FileContent = { size: 3, revision: 'a'.repeat(64), encoding: 'utf8', content: '<b>', media_type: 'text/plain' };
const openText = async () => {
  fireEvent.click(await screen.findByRole('button', { name: '▸ src' }));
  fireEvent.click(await screen.findByRole('button', { name: 'hello.txt' }));
  await screen.findByRole('button', { name: 'Edit' });
};
it('chooses safe viewers and labels API failures', () => {
  expect(viewerKind(content)).toBe('text');
  expect(viewerKind({ ...content, encoding: 'base64' })).toBe('binary');
  for (const media_type of ['image/png', 'image/jpeg']) expect(viewerKind({ ...content, media_type, encoding: 'base64' })).toBe('image');
  expect(viewerKind({ ...content, media_type: 'text/html' })).toBe('text');
  expect(viewerKind({ ...content, media_type: 'image/svg+xml' })).toBe('binary');
  expect(fileProblem(new ApiError('forbidden', 'private', 403))).toBe('Not allowed');
  expect(fileProblem(new ApiError('not_found', 'private', 404))).toBe('Gone');
  expect(fileProblem(new ApiError('too_large', 'large', 413, { size: 9000000 }))).toBe('Too large to show (9000000 bytes)');
});
it('decodes image bytes into object URLs and revokes previews on change and unmount', async () => {
  const create = vi.spyOn(URL, 'createObjectURL').mockReturnValue('blob:preview');
  const revoke = vi.spyOn(URL, 'revokeObjectURL').mockImplementation(() => {});
  const view = renderWithHub(<FilesTab workstream={stream} onDirty={vi.fn()} onBusy={vi.fn()} />, hub, { fetch: async (...args) => {
    if (String(args[0]).includes('/files/content?')) return new Response(JSON.stringify({ ...content, media_type: 'image/png', encoding: 'base64', content: 'AQID' }), { headers: { 'Content-Type': 'application/json' } });
    return fetch(...args);
  } });
  fireEvent.click(await screen.findByRole('button', { name: '▸ src' }));
  fireEvent.click(await screen.findByRole('button', { name: 'hello.txt' }));
  await waitFor(() => expect(screen.getByRole('img').getAttribute('src')).toBe('blob:preview'));
  const blob = create.mock.calls[0]?.[0] as Blob;
  expect(blob.type).toBe('image/png');
  expect([...new Uint8Array(await blob.arrayBuffer())]).toEqual([1, 2, 3]);
  create.mockReturnValue('blob:second');
  fireEvent.click(screen.getByRole('button', { name: 'large.bin' }));
  await waitFor(() => expect(screen.getByRole('img').getAttribute('src')).toBe('blob:second'));
  expect(revoke).toHaveBeenCalledWith('blob:preview');
  view.unmount();
  expect(revoke).toHaveBeenCalledWith('blob:second');
});
it('loads one level at a time, refuses links and displays the size cap', async () => {
  const reads: string[] = [];
  renderWithHub(<FilesTab workstream={stream} onDirty={vi.fn()} onBusy={vi.fn()} />, hub, { fetch: async (...args) => {
    reads.push(String(args[0])); return fetch(...args);
  } });
  await screen.findByText('outside (link, cannot open)');
  expect(screen.queryByRole('button', { name: 'hello.txt' })).toBeNull();
  expect(reads.some((url) => url.includes('path=src'))).toBe(false);
  await openText();
  expect(screen.getByLabelText('File text').textContent).toContain('hello');
  fireEvent.click(screen.getByRole('button', { name: 'large.bin' }));
  await screen.findByText('Too large to show (8388609 bytes)');
});
it('saves text, retains a conflicting draft and reloads the revision before overwrite', async () => {
  const dirty = vi.fn();
  renderWithHub(<FilesTab workstream={stream} onDirty={dirty} onBusy={vi.fn()} />, hub);
  await openText();
  fireEvent.click(screen.getByRole('button', { name: 'Edit' }));
  fireEvent.change(screen.getByLabelText('Edit file text'), { target: { value: '<script>literal</script>\n' } });
  await waitFor(() => expect(dirty).toHaveBeenLastCalledWith(true));
  const client = fileClient(otherClient(hub), stream.id, 0);
  await client.write('src/hello.txt', (await client.read('src/hello.txt')).revision, 'someone else\n');
  fireEvent.click(screen.getByRole('button', { name: 'Save' }));
  await screen.findByText('Changed since you opened it');
  expect((screen.getByLabelText('Edit file text') as HTMLTextAreaElement).value).toContain('<script>');
  fireEvent.click(screen.getByRole('button', { name: 'Overwrite' }));
  await screen.findByText('Saved');
  expect((await client.read('src/hello.txt')).content).toBe('<script>literal</script>\n');
  expect(screen.getByLabelText('File text').querySelector('script')).toBeNull();
  await waitFor(() => expect(dirty).toHaveBeenLastCalledWith(false));
});
it('asks before switching files and reloads a conflict when approved', async () => {
  renderWithHub(<FilesTab workstream={stream} onDirty={vi.fn()} onBusy={vi.fn()} />, hub);
  await openText();
  fireEvent.click(screen.getByRole('button', { name: 'Edit' }));
  fireEvent.change(screen.getByLabelText('Edit file text'), { target: { value: 'draft' } });
  const confirm = vi.fn().mockReturnValue(false);
  vi.stubGlobal('confirm', confirm);
  fireEvent.click(screen.getByRole('button', { name: 'large.bin' }));
  expect(confirm).toHaveBeenCalled();
  expect(screen.getByLabelText('Edit file text')).toBeTruthy();
  const client = fileClient(otherClient(hub), stream.id, 0);
  await client.write('src/hello.txt', (await client.read('src/hello.txt')).revision, 'new bytes');
  fireEvent.click(screen.getByRole('button', { name: 'Save' }));
  await screen.findByText('Changed since you opened it');
  confirm.mockReturnValue(true);
  fireEvent.click(screen.getByRole('button', { name: 'Reload' }));
  await screen.findByText('new bytes');
  expect(screen.queryByLabelText('Edit file text')).toBeNull();
});
it('shows empty and truncated folders', async () => {
  renderWithHub(<FilesTab workstream={stream} onDirty={vi.fn()} onBusy={vi.fn()} />, hub, { fetch: async (...args) => {
    if (String(args[0]).includes('/files?')) return new Response(JSON.stringify({ entries: [], truncated: true }), { headers: { 'Content-Type': 'application/json' } });
    return fetch(...args);
  } });
  await screen.findByText('Empty folder');
  await screen.findByText('Listing truncated; some entries are omitted.');
});
it('shows unsupported locations and preserves their index', async () => {
  const remote = { ...stream, locations: [...stream.locations.slice(0, 1), { machine: '01JB000000000000000MAC0002', path: '/home/sam/remote' }] };
  renderWithHub(<FilesTab workstream={remote} onDirty={vi.fn()} onBusy={vi.fn()} />, hub, { fetch: async (...args) => {
    if (String(args[0]).includes('/files?loc=1')) return new Response(JSON.stringify({ code: 'unsupported', message: 'Unsupported' }), { status: 501, headers: { 'Content-Type': 'application/json' } });
    return fetch(...args);
  } });
  await screen.findByRole('button', { name: '▸ src' });
  fireEvent.change(screen.getByLabelText('Location'), { target: { value: '1' } });
  await screen.findByText("Files on another machine aren't supported yet.");
});
