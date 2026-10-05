/** One unified hunk, retaining three lines of context around the changed region. */
export function hookDiff(path: string, before: string | null, after: string): string {
  const lines = (text: string): string[] => {
    if (text === '') return [];
    const result = text.replace(/\r\n/g, '\n').split('\n');
    if (text.endsWith('\n')) result.pop();
    return result;
  };
  const old = lines(before ?? '');
  const next = lines(after);
  let prefix = 0;
  while (prefix < old.length && prefix < next.length && old[prefix] === next[prefix]) prefix += 1;
  let suffix = 0;
  while (suffix < old.length - prefix && suffix < next.length - prefix && old[old.length - 1 - suffix] === next[next.length - 1 - suffix]) suffix += 1;
  const start = Math.max(0, prefix - 3);
  const context = Math.min(3, suffix);
  const rows = [
    `--- ${before === null ? '/dev/null' : path}`,
    `+++ ${path}`,
    `@@ -${old.length === 0 ? 0 : start + 1},${old.length - suffix - start + context} +${start + 1},${next.length - suffix - start + context} @@`,
    ...old.slice(start, prefix).map(line => ` ${line}`),
    ...old.slice(prefix, old.length - suffix).map(line => `-${line}`),
    ...next.slice(prefix, next.length - suffix).map(line => `+${line}`),
    ...next.slice(next.length - suffix, next.length - suffix + context).map(line => ` ${line}`),
  ];
  return rows.join('\n');
}
