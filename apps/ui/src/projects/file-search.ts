import type { FileClient, FileListing } from '../data/files.ts';

export const SEARCH_LIMITS = { directories: 128, entries: 5000 };
export const visibleFile = (entry: FileListing['entries'][number], hidden: boolean) =>
  hidden || (!entry.name.startsWith('.') && entry.ignored !== true);

/** Subsequence scoring: contiguous matches and path boundaries rank first. */
export function fileScore(path: string, query: string): number | undefined {
  const text = path.toLowerCase();
  let at = -1;
  let score = 0;
  for (const char of query.toLowerCase().trim()) {
    const next = text.indexOf(char, at + 1);
    if (next === -1) return undefined;
    score += next === at + 1 ? 8 : 0;
    score += next === 0 || '/._-'.includes(text[next - 1] ?? '') ? 5 : 0;
    score -= next - at;
    at = next;
  }
  return score - path.length / 100;
}

/** Lists only; never reads file contents or traverses links. */
export async function collectFiles(client: Pick<FileClient, 'list'>, hidden: boolean, signal: AbortSignal) {
  const queue = [''];
  const paths: string[] = [];
  let count = 0;
  let directories = 0;
  let truncated = false;
  let unavailable = false;
  while (queue.length > 0 && directories < SEARCH_LIMITS.directories && count < SEARCH_LIMITS.entries) {
    signal.throwIfAborted();
    const directory = queue.shift() as string;
    directories += 1;
    let list: FileListing;
    try { list = await client.list(directory, signal); }
    catch (error) { if (signal.aborted) throw error; unavailable = true; continue; }
    truncated ||= list.truncated;
    for (const entry of list.entries) {
      if (++count > SEARCH_LIMITS.entries) { truncated = true; break; }
      if (!visibleFile(entry, hidden)) continue;
      const path = directory ? `${directory}/${entry.name}` : entry.name;
      if (entry.kind === 'file') paths.push(path);
      if (entry.kind === 'folder') queue.push(path);
    }
  }
  return { paths, truncated: truncated || queue.length > 0, unavailable };
}
