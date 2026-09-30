// Glob matching for repo-relative, '/'-separated paths.
//   **   any number of whole path segments, including none ('a/**' needs at least one below 'a')
//   *    any run of characters within one segment
//   ?    exactly one character within one segment
// Everything else is literal. A '**' that is not a whole segment ('a**b') behaves like '*', as in .gitignore.

const compiled = new Map();

// Backslashes to '/', no leading './', no repeated '/'.
export function normalizePath(path) {
  let p = String(path).replace(/\\/g, '/').replace(/\/{2,}/g, '/');
  while (p.startsWith('./')) p = p.slice(2);
  return p;
}

function segmentSource(segment) {
  let out = '';
  for (let i = 0; i < segment.length; i++) {
    const c = segment[i];
    if (c === '*') {
      while (segment[i + 1] === '*') i++;
      out += '[^/]*';
    } else if (c === '?') {
      out += '[^/]';
    } else {
      out += c.replace(/[\\^$.*+?()[\]{}|]/g, '\\$&');
    }
  }
  return out;
}

export function globToRegExp(glob) {
  const segments = normalizePath(glob).split('/');
  const last = segments.length - 1;
  let source = '';
  segments.forEach((segment, i) => {
    if (segment === '**') {
      if (i < last) source += '(?:[^/]+/)*';
      else source += i === 0 ? '.*' : '.+';
    } else {
      source += segmentSource(segment) + (i < last ? '/' : '');
    }
  });
  return new RegExp(`^${source}$`);
}

export function matches(glob, path) {
  let re = compiled.get(glob);
  if (!re) {
    re = globToRegExp(glob);
    compiled.set(glob, re);
  }
  return re.test(normalizePath(path));
}

export function matchesAny(globs, path) {
  return globs.some((glob) => matches(glob, path));
}
