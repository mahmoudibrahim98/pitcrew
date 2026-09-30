// Links out of transcripts: only absolute http, https and mailto targets become links, and they
// open outside the app, never in its window.

import { createContext, use, type ReactNode } from 'react';

const SAFE_PROTOCOLS = new Set(['http:', 'https:', 'mailto:']);

/** The URL to link to, or undefined for anything but an absolute http, https or mailto URL. */
export function safeHref(raw: string): string | undefined {
  let url: URL;
  try {
    // The URL parser drops tabs, newlines and surrounding spaces, as a browser would.
    url = new URL(raw);
  } catch {
    return undefined;
  }
  return SAFE_PROTOCOLS.has(url.protocol) ? url.href : undefined;
}

/** How the host opens a link outside the app (the desktop shell passes its opener). */
const OpenExternalContext = createContext<((url: string) => void) | undefined>(undefined);

export function OpenExternalProvider(props: { open: (url: string) => void; children: ReactNode }) {
  return <OpenExternalContext value={props.open}>{props.children}</OpenExternalContext>;
}

export function ExternalLink({ href, children }: { href: string; children: ReactNode }) {
  const open = use(OpenExternalContext);
  return (
    <a
      href={href}
      target="_blank"
      rel="noopener noreferrer"
      className="text-accent-text underline decoration-line-2 underline-offset-2 hover:decoration-current"
      onClick={
        open === undefined
          ? undefined
          : (event) => {
              event.preventDefault();
              open(href);
            }
      }
    >
      {children}
    </a>
  );
}
