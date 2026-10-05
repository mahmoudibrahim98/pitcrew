import type { Api } from './api.ts';

export interface FileListing {
  entries: { name: string; kind: 'file' | 'folder' | 'link'; size: number; modified_at: number | null }[];
  truncated: boolean;
}

export interface FileContent {
  size: number;
  media_type: string;
  revision: string;
  encoding: 'utf8' | 'base64';
  content: string;
}

export function fileClient(api: Api, workstream: string, location: number) {
  const root = `/v1/workstreams/${encodeURIComponent(workstream)}/files`;
  const query = (path: string) => ({ loc: String(location), path });
  return {
    list: (path: string, signal?: AbortSignal) => api.request<FileListing>('GET', root, { query: query(path), signal }),
    read: (path: string, signal?: AbortSignal) => api.request<FileContent>('GET', `${root}/content`, { query: query(path), signal }),
    write: (path: string, revision: string, content: string) => api.request<FileContent>('PUT', `${root}/content`, {
      query: query(path), body: { revision, encoding: 'utf8', content },
    }),
  };
}

/** The files routes for one workstream location, as `fileClient` makes them. */
export type FileClient = ReturnType<typeof fileClient>;
