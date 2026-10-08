// Owned by stream 0. Check the policy as written, before Tauri adds its own script hashes.
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

export function checkPolicy(config) {
  const errors = [];
  const raw = config?.app?.security?.csp;
  const directives = new Map();
  const add = (name, value) => {
    name = name.toLowerCase();
    if (directives.has(name)) errors.push(`duplicate CSP directive: ${name}`);
    if (Array.isArray(value)) value = value.join(' ');
    if (typeof value !== 'string') {
      errors.push(`invalid CSP directive: ${name}`);
      return;
    }
    directives.set(name, value.trim().split(/\s+/).filter(Boolean));
  };
  if (typeof raw === 'string') {
    for (const part of raw.split(';')) {
      const [name, ...sources] = part.trim().split(/\s+/);
      if (name) add(name, sources.join(' '));
    }
  } else if (raw && typeof raw === 'object' && !Array.isArray(raw)) {
    for (const [name, value] of Object.entries(raw)) add(name, value);
  } else {
    return ['desktop CSP is missing or disabled'];
  }
  if (!directives.has('default-src')) errors.push('default-src must be explicit');
  for (const name of ['default-src', 'script-src', 'script-src-elem', 'script-src-attr', 'connect-src', 'img-src']) {
    const scriptFallback = name.startsWith('script-src-') ? directives.get('script-src') : undefined;
    const sources = directives.get(name) ?? scriptFallback ?? directives.get('default-src');
    if (!sources?.length) {
      errors.push(`${name} has no source policy`);
      continue;
    }
    for (const source of sources) {
      if (["'self'", "'none'"].includes(source)) continue;
      if (name === 'img-src' && source === 'blob:') continue;
      if (name.startsWith('script-src') &&
          (source === "'report-sample'" ||
           /^'(?:nonce-|sha(?:256|384|512)-)[A-Za-z0-9+/_-]+=*'$/.test(source))) continue;
      // Tauri's reserved local IPC transport is not a remote origin (threat model T43).
      if (name === 'connect-src' && ['ipc:', 'http://ipc.localhost'].includes(source)) continue;
      errors.push(`${name} forbids source ${source}`);
    }
  }
  return errors;
}

// A quote-aware tag scanner avoids treating text in comments, raw-text elements or quoted
// attributes as executable markup. Fail closed on malformed tags in the built entry point.
export function checkHtml(html) {
  const errors = [];
  let cursor = 0;
  while ((cursor = html.indexOf('<', cursor)) !== -1) {
    if (html.startsWith('<!--', cursor)) {
      const end = html.indexOf('-->', cursor + 4);
      if (end === -1) return [...errors, 'unterminated HTML comment'];
      cursor = end + 3;
      continue;
    }
    let end = cursor + 1;
    let quote;
    for (; end < html.length; end++) {
      const char = html[end];
      if (quote) { if (char === quote) quote = undefined; }
      else if (char === '"' || char === "'") quote = char;
      else if (char === '>') break;
    }
    if (end === html.length) return [...errors, 'unterminated HTML tag'];
    const tag = html.slice(cursor + 1, end);
    cursor = end + 1;
    const match = /^([a-z][a-z0-9:-]*)(?=\s|\/|$)/i.exec(tag);
    if (!match) continue;
    const name = match[1].toLowerCase();
    const attrs = new Map();
    const rest = tag.slice(match[0].length).replace(/\/$/, '');
    const pattern = /([^\s"'<>/=]+)(?:\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s"'<>`=]+)))?/g;
    let attr;
    while ((attr = pattern.exec(rest)) !== null) {
      const key = attr[1].toLowerCase();
      if (/^on/.test(key)) errors.push(`event-handler attribute ${key} on <${name}>`);
      // Inline styles in the entry point: Tauri adds a nonce to each <style>, which switches off
      // style-src 'unsafe-inline' for the app's run-time styles, and a nonce never covers style="".
      if (key === 'style') errors.push(`style attribute on <${name}> in built UI`);
      if (attrs.has(key)) errors.push(`duplicate attribute ${key} on <${name}>`);
      attrs.set(key, attr[2] ?? attr[3] ?? attr[4] ?? '');
    }
    if (name === 'script' && !attrs.get('src')?.trim()) errors.push('inline <script> in built UI');
    if (name === 'style') errors.push('inline <style> in built UI');
    if (['script', 'style', 'textarea', 'title'].includes(name)) {
      const close = new RegExp(`</${name}\\s*>`, 'ig');
      close.lastIndex = cursor;
      const found = close.exec(html);
      if (!found) return [...errors, `unclosed <${name}>`];
      cursor = close.lastIndex;
    }
  }
  return errors;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const root = fileURLToPath(new URL('../../', import.meta.url));
    const config = JSON.parse(readFileSync(resolve(root, 'apps/desktop/src-tauri/tauri.conf.json'), 'utf8'));
    const html = readFileSync(resolve(root, 'apps/ui/dist/index.html'), 'utf8');
    const errors = [...checkPolicy(config), ...checkHtml(html)];
    if (errors.length) {
      for (const error of errors) console.error(`csp-check: ${error}`);
      process.exitCode = 1;
    } else console.log('csp-check: desktop policy and built UI passed');
  } catch (error) {
    console.error(`csp-check: ${error.message}`);
    process.exitCode = 1;
  }
}
