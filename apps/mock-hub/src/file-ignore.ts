// In-memory .gitignore presentation hints; never reads the host filesystem or Git config.
type Rule = { pattern: RegExp; directory: boolean; negate: boolean };

function rule(line: string): Rule | undefined {
  line = line.replace(/(?<!\\)\s+$/, '');
  if (!line || line.startsWith('#')) return undefined;
  const negate = line.startsWith('!');
  if (negate) line = line.slice(1);
  const directory = line.endsWith('/');
  if (directory) line = line.slice(0, -1);
  const anchored = line.startsWith('/') || line.includes('/');
  if (line.startsWith('/')) line = line.slice(1);
  let source = anchored ? '^' : '(?:^|/)';
  for (let index = 0; index < line.length; index++) {
    const char = line[index]!;
    if (char === '\\' && index + 1 < line.length) {
      source += line[++index]!.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
    } else if (char === '*' && line[index + 1] === '*') {
      index++;
      if (line[index + 1] === '/') { index++; source += '(?:.*/)?'; }
      else source += '.*';
    } else if (char === '*') source += '[^/]*';
    else if (char === '?') source += '[^/]';
    else if (char === '[') {
      const end = line.indexOf(']', index + 1);
      if (end < 0) source += '\\[';
      else { source += '[' + line.slice(index + 1, end).replace(/^!/, '^') + ']'; index = end; }
    } else source += char.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  }
  try { return { pattern: new RegExp(source + '$'), directory, negate }; }
  catch { return undefined; }
}

export function ignoreMatcher(path: string, read: (path: string) => string | undefined) {
  const directories = ['', ...path.split('/').filter(Boolean).map((_, index, parts) => parts.slice(0, index + 1).join('/'))].slice(0, 64);
  let bytes = 256 * 1024;
  let lines = 4096;
  const matchers = directories.flatMap(directory => {
    const content = read(directory ? `${directory}/.gitignore` : '.gitignore');
    if (content === undefined || Buffer.byteLength(content) > bytes) return [];
    bytes -= Buffer.byteLength(content);
    const rules: Rule[] = [];
    for (const line of content.split('\n').slice(0, lines)) { lines--; const parsed = rule(line.replace(/\r$/, '')); if (parsed) rules.push(parsed); }
    return [{ directory, rules }];
  });
  return (relative: string, folder: boolean): boolean => {
    const parts = relative.split('/');
    // An ignored parent cannot be re-included by a child's own ignore file.
    for (let length = 1; length <= parts.length; length++) {
      const candidate = parts.slice(0, length).join('/');
      const isDirectory = length < parts.length || folder;
      let ignored = false;
      for (const { directory, rules } of matchers) {
        if (directory && !candidate.startsWith(directory + '/')) continue;
        const local = directory ? candidate.slice(directory.length + 1) : candidate;
        for (const entry of rules) {
          if ((!entry.directory || isDirectory) && entry.pattern.test(local)) ignored = !entry.negate;
        }
      }
      if (ignored) return true;
    }
    return false;
  };
}
