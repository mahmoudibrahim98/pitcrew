// Device-only in-memory file trees, scoped by machine and stored location root.
import { createHash } from 'node:crypto';
import type { Hub } from './state.ts';
import { ApiFailure, forbidden, invalid, isRecord, notFound } from './validate.ts';

const CAP = 8 * 1024 * 1024;
type Entry = { kind: 'file' | 'folder' | 'link'; bytes: Buffer; at: number };
const trees = new WeakMap<Hub, Map<string, Map<string, Entry>>>();
const revision = (bytes: Buffer): string => createHash('sha256').update(bytes).digest('hex');
function tree(hub: Hub, key: string): Map<string, Entry> {
  let roots = trees.get(hub);
  if (!roots) { roots = new Map(); trees.set(hub, roots); }
  let entries = roots.get(key);
  if (!entries) {
    entries = new Map([
      ['', { kind: 'folder', bytes: Buffer.alloc(0), at: 0 }],
      ['src', { kind: 'folder', bytes: Buffer.alloc(0), at: 0 }],
      ['src/hello.txt', { kind: 'file', bytes: Buffer.from('hello\n'), at: 0 }],
      ['large.bin', { kind: 'file', bytes: Buffer.alloc(CAP + 1), at: 0 }],
      ['outside', { kind: 'link', bytes: Buffer.alloc(0), at: 0 }],
    ]);
    roots.set(key, entries);
  }
  return entries;
}
function validate(path: string, list: boolean, write: boolean): void {
  if (path === '' && list) return;
  if (!path || path.startsWith('/') || /[\0\\]/.test(path) || path.split('/').some(p => !p || p === '.' || p === '..')) throw invalid('Invalid relative path.');
  if (process.platform === 'win32' && path.split('/').some(p => /:|[. ]$/.test(p) || /^(CON|PRN|AUX|NUL|CONIN\$|CONOUT\$|COM[1-9¹²³]|LPT[1-9¹²³])(?:\.|$)/i.test(p))) throw invalid('Invalid relative path.');
  if (write && path.split('/').some(p => process.platform === 'win32' ? p.toLowerCase() === '.git' : p === '.git')) throw forbidden('Git writes are refused.');
}
function content(path: string, bytes: Buffer): unknown {
  const text = bytes.toString('utf8');
  const utf8 = Buffer.from(text).equals(bytes);
  const ext = path.split('.').pop();
  const media_type = ['txt','md','rs','ts','js','toml','yaml','yml'].includes(ext ?? '') ? 'text/plain'
    : ({ json: 'application/json', png: 'image/png', jpg: 'image/jpeg', jpeg: 'image/jpeg', pdf: 'application/pdf' } as Record<string,string>)[ext ?? ''] ?? 'application/octet-stream';
  return { size: bytes.length, media_type, revision: revision(bytes), encoding: utf8 ? 'utf8' : 'base64', content: utf8 ? text : bytes.toString('base64') };
}
export function files(hub: Hub, id: string, query: URLSearchParams, body: unknown, operation: 'list' | 'read' | 'write'): { status: number; body: unknown } {
  const loc = query.get('loc'); const path = query.get('path');
  if (loc === null || !/^\d+$/.test(loc) || !Number.isSafeInteger(Number(loc)) || path === null || [...query.keys()].some(k => k !== 'loc' && k !== 'path') || query.getAll('loc').length !== 1 || query.getAll('path').length !== 1) throw invalid('Invalid file query.');
  validate(path, operation === 'list', operation === 'write');
  const stream = hub.findWorkstream(id);
  const location = stream?.locations[Number(loc)];
  if (!stream || !location) throw notFound('Workstream or location not found.');
  if (hub.machines.find(m => m.kind === 'local')?.id !== location.machine) throw new ApiFailure('unsupported', 'Files on another machine are unsupported.');
  if (operation === 'write' && location.path.split(/[\\/]/).some(p => process.platform === 'win32' ? p.toLowerCase() === '.git' : p === '.git')) throw forbidden('Git writes are refused.');
  const entries = tree(hub, `${location.machine}\0${location.path}`);
  let parent = '';
  for (const component of path.split('/').slice(0, -1)) {
    parent = parent ? `${parent}/${component}` : component;
    const entry = entries.get(parent);
    if (!entry) throw notFound('Directory not found.');
    if (entry.kind !== 'folder') throw forbidden('Unsafe directory.');
  }
  const entry = entries.get(path);
  if (entry?.kind === 'link') throw forbidden('Links are refused.');
  if (operation === 'list') {
    if (!entry) throw notFound('Directory not found.');
    if (entry.kind !== 'folder') throw invalid('Not a directory.');
    const prefix = path ? `${path}/` : '';
    const all = [...entries].filter(([p]) => p.startsWith(prefix) && p !== path && !p.slice(prefix.length).includes('/'))
      .map(([p, e]) => ({ name: p.slice(prefix.length), kind: e.kind, size: e.bytes.length, modified_at: e.at }))
      .sort((a,b) => Buffer.compare(Buffer.from(a.name), Buffer.from(b.name)));
    return { status: 200, body: { entries: all.slice(0, 5000), truncated: all.length > 5000 } };
  }
  if (entry && entry.kind !== 'file') throw forbidden('Not a file.');
  if (entry && entry.bytes.length > CAP) return { status: 413, body: { code: 'too_large', message: 'File exceeds the size cap.', size: entry.bytes.length } };
  if (operation === 'read') {
    if (!entry) throw notFound('File not found.');
    return { status: 200, body: content(path, entry.bytes) };
  }
  if (!isRecord(body) || !Object.hasOwn(body, 'revision') || body['revision'] !== null && (typeof body['revision'] !== 'string' || !/^[0-9a-f]{64}$/.test(body['revision'])) || !['utf8','base64'].includes(String(body['encoding'])) || typeof body['content'] !== 'string' || Object.keys(body).some(k => !['revision','encoding','content'].includes(k))) throw invalid('Invalid file body.');
  const encoded = body['content'];
  if (body['encoding'] === 'base64' && !/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/.test(encoded)) throw invalid('Invalid base64.');
  const bytes = Buffer.from(encoded, body['encoding'] === 'utf8' ? 'utf8' : 'base64');
  if (body['encoding'] === 'base64' && bytes.toString('base64') !== encoded) throw invalid('Invalid base64.');
  if (bytes.length > CAP) return { status: 413, body: { code: 'too_large', message: 'File exceeds the size cap.', size: bytes.length } };
  const current = entry ? revision(entry.bytes) : null;
  if (current !== body['revision']) return { status: 409, body: { code: 'conflict', message: 'File revision changed.', current_revision: current } };
  entries.set(path, { kind: 'file', bytes, at: Date.now() });
  return { status: 200, body: content(path, bytes) };
}
