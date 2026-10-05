import { useState } from 'react';

export function FileBreadcrumbs({ path, copyPath = path, label = 'File breadcrumbs', onFolder }: { path: string; copyPath?: string; label?: string | undefined; onFolder?: ((path: string) => void) | undefined }) {
  const [message, setMessage] = useState('');
  const parts = path.split('/');
  return <div className="space-y-1">
    <nav aria-label={label}><ol className="flex flex-wrap items-center gap-1 text-xs">
      <li>{onFolder ? <button onClick={() => onFolder('')}>Root</button> : <span>Root</span>}</li>
      {parts.map((part, index) => <li key={index} className="flex gap-1"><span aria-hidden>/</span>{index < parts.length - 1 && onFolder ? <button className="text-accent-text underline" onClick={() => onFolder(parts.slice(0, index + 1).join('/'))}>{part}</button> : <span aria-current={index === parts.length - 1 ? 'page' : undefined}>{part}</span>}</li>)}
    </ol></nav>
    <button className="rounded-sm border border-line px-2 py-1 text-xs" onClick={() => {
      void (async () => { try { await navigator.clipboard.writeText(copyPath); setMessage('Path copied'); } catch { setMessage('Could not copy path'); } })();
    }}>Copy path</button>
    {message && <p role="status" className="text-xs">{message}</p>}
  </div>;
}
