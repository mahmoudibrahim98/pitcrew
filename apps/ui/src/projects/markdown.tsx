import type { ReactNode } from 'react';
import { ExternalLink, safeHref } from '../console/render/links.tsx';

// Render a conservative Markdown subset as React text. No HTML parser, raw HTML or image fetches.
// Links (absolute http, https or mailto only) are the console's `ExternalLink`, as in transcripts:
// they open through the host's opener when it provides one (the desktop window refuses a plain
// `target="_blank"`), else in a new tab.
function inline(text: string): ReactNode[] {
  return text.split(/(`[^`]+`|\*\*[^*]+\*\*|\*[^*]+\*|\[[^\]]+\]\([^\s)]+\))/g).map((part, index) => {
    if (part.startsWith('`') && part.endsWith('`')) return <code key={index} className="rounded-sm bg-sunken px-1 font-mono">{part.slice(1, -1)}</code>;
    if (part.startsWith('**') && part.endsWith('**')) return <strong key={index}>{part.slice(2, -2)}</strong>;
    if (part.startsWith('*') && part.endsWith('*')) return <em key={index}>{part.slice(1, -1)}</em>;
    const link = part.match(/^\[([^\]]+)\]\(([^\s)]+)\)$/);
    const href = link === null ? undefined : safeHref(link[2] ?? '');
    if (link !== null && href !== undefined) return <ExternalLink key={index} href={href}>{link[1]}</ExternalLink>;
    return part;
  });
}
export function Markdown({ text }: { text: string }) {
  const blocks: ReactNode[] = [];
  const lines = text.split('\n');
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i] ?? '';
    if (line.startsWith('```')) {
      const code: string[] = [];
      while (++i < lines.length && !lines[i]?.startsWith('```')) code.push(lines[i] ?? '');
      blocks.push(<pre key={i} className="overflow-auto rounded-md bg-sunken p-3"><code>{code.join('\n')}</code></pre>);
    } else if (/^#{1,6} /.test(line)) {
      blocks.push(<p key={i} className="font-semibold">{inline(line.replace(/^#{1,6} /, ''))}</p>);
    } else if (/^[-*] /.test(line)) {
      const items: string[] = [line.slice(2)];
      while (i + 1 < lines.length && /^[-*] /.test(lines[i + 1] ?? '')) items.push((lines[++i] ?? '').slice(2));
      blocks.push(<ul key={i} className="list-disc pl-5">{items.map((item, index) => <li key={index}>{inline(item)}</li>)}</ul>);
    } else if (line.startsWith('> ')) {
      blocks.push(<blockquote key={i} className="border-l-2 border-line pl-3">{inline(line.slice(2))}</blockquote>);
    } else if (line !== '') blocks.push(<p key={i} className="whitespace-pre-wrap">{inline(line)}</p>);
  }
  return <div className="flex flex-col gap-2 text-md leading-relaxed">{blocks}</div>;
}
