// A PDF, drawn page by page on canvases by a bundled pdf.js (its legacy build: the modern one needs
// newer JavaScript than the webviews we ship have). Its own lazy chunk; nothing of pdf.js loads
// until a PDF is opened.
//
// Within the desktop CSP as it is (`apps/desktop/src-tauri/tauri.conf.json`):
// - pdf.js parses in a worker where workers may run (a browser); the desktop's `worker-src 'none'`
//   refuses it, and pdf.js then runs the same bundled script on the page (`script-src 'self'`);
// - no WebAssembly (`useWasm: false`; the CSP has no 'wasm-unsafe-eval'), so JPEG 2000 and JBIG2
//   images, rare outside scans, do not show;
// - fonts load from the file's bytes (the FontFace API), never from a URL; pdf.js 6 evaluates no
//   code (no `eval`, no `new Function`);
// - only the pages are drawn: no annotation, form or link layer, so nothing in a file can navigate,
//   run script or reach the network.

import { getDocument, GlobalWorkerOptions, type PDFDocumentProxy, type RenderTask } from 'pdfjs-dist/legacy/build/pdf.mjs';
import workerUrl from 'pdfjs-dist/legacy/build/pdf.worker.min.mjs?url';
import { useEffect, useId, useRef, useState } from 'react';

GlobalWorkerOptions.workerSrc = workerUrl;

/** Zoom steps, as a share of the page's printed size. */
export const ZOOMS = [0.5, 0.75, 1, 1.25, 1.5, 2, 3] as const;
const DEFAULT_ZOOM = 2;
/** PDF units (1/72 in) to CSS pixels (1/96 in). */
const CSS_UNITS = 96 / 72;

const BUTTON = 'rounded-sm border border-line px-2 py-0.5 text-sm disabled:opacity-50';

type Loaded = { doc: PDFDocumentProxy; width: number; height: number };

function problem(error: unknown): string {
  const name = error instanceof Error ? error.name : '';
  if (name === 'PasswordException') return 'This PDF is protected by a password, so it cannot be shown here.';
  if (name === 'InvalidPDFException') return 'This file is not a readable PDF.';
  return `Could not show this PDF${error instanceof Error && error.message !== '' ? `: ${error.message}` : '.'}`;
}

export default function PdfView({ bytes, label }: { bytes: Uint8Array; label: string }) {
  const [loaded, setLoaded] = useState<Loaded>();
  const [error, setError] = useState<string>();
  const [zoom, setZoom] = useState(DEFAULT_ZOOM);
  const status = useId();

  useEffect(() => {
    let live = true;
    // pdf.js takes the buffer to its worker, so it gets a copy.
    // Without an `onPassword` handler, a protected file rejects with a PasswordException.
    const task = getDocument({ data: bytes.slice(), useWasm: false, enableXfa: false, stopAtErrors: false });
    task.promise
      .then(async (doc) => {
        const first = await doc.getPage(1);
        const viewport = first.getViewport({ scale: 1 });
        if (live) setLoaded({ doc, width: viewport.width, height: viewport.height });
      })
      .catch((reason: unknown) => {
        if (live) setError(problem(reason));
      });
    return () => {
      live = false;
      void task.destroy();
    };
  }, [bytes]);

  if (error !== undefined) return <p role="alert">{error}</p>;
  if (loaded === undefined) return <p role="status">Loading the PDF…</p>;
  const scale = (ZOOMS[zoom] ?? 1) * CSS_UNITS;
  const pages = loaded.doc.numPages;
  return (
    <div className="flex min-h-0 flex-col gap-2" data-pdf="" data-pages={pages}>
      <div className="flex flex-wrap items-center gap-2 text-sm" role="group" aria-label="Zoom">
        <button type="button" className={BUTTON} disabled={zoom === 0} onClick={() => setZoom(zoom - 1)} aria-label="Zoom out">
          −
        </button>
        <span id={status} className="min-w-12 text-center tabular-nums">
          {Math.round((ZOOMS[zoom] ?? 1) * 100)}%
        </span>
        <button
          type="button"
          className={BUTTON}
          disabled={zoom === ZOOMS.length - 1}
          onClick={() => setZoom(zoom + 1)}
          aria-label="Zoom in"
        >
          +
        </button>
        <span className="text-ink-2">
          {pages} {pages === 1 ? 'page' : 'pages'}
        </span>
      </div>
      <div
        className="max-h-[75vh] overflow-auto rounded-sm border border-line bg-sunken p-3"
        tabIndex={0}
        aria-label={`Pages of ${label}`}
        aria-describedby={status}
      >
        <div className="flex flex-col items-center gap-3">
          {Array.from({ length: pages }, (_, i) => (
            <PdfPage
              key={i}
              doc={loaded.doc}
              number={i + 1}
              pages={pages}
              scale={scale}
              estimate={{ width: loaded.width * scale, height: loaded.height * scale }}
            />
          ))}
        </div>
      </div>
    </div>
  );
}

/** One page: drawn once it comes near the view, again when the zoom changes. */
function PdfPage(props: {
  doc: PDFDocumentProxy;
  number: number;
  pages: number;
  scale: number;
  estimate: { width: number; height: number };
}) {
  const { doc, number, scale } = props;
  const frame = useRef<HTMLDivElement>(null);
  const canvas = useRef<HTMLCanvasElement>(null);
  // Without IntersectionObserver every page is drawn at once.
  const [near, setNear] = useState(() => typeof IntersectionObserver !== 'function');
  const [size, setSize] = useState(props.estimate);
  const [state, setState] = useState<'waiting' | 'drawn' | 'failed'>('waiting');

  useEffect(() => {
    const element = frame.current;
    if (element === null || typeof IntersectionObserver !== 'function') return;
    const observer = new IntersectionObserver((entries) => {
      if (entries.some((e) => e.isIntersecting)) {
        setNear(true);
        observer.disconnect();
      }
    }, { rootMargin: '600px' });
    observer.observe(element);
    return () => observer.disconnect();
  }, []);

  useEffect(() => {
    const target = canvas.current;
    if (!near || target === null) return;
    let live = true;
    let render: RenderTask | undefined;
    void doc
      .getPage(number)
      .then((page) => {
        if (!live) return;
        const viewport = page.getViewport({ scale });
        const ratio = Math.min(window.devicePixelRatio || 1, 3);
        target.width = Math.floor(viewport.width * ratio);
        target.height = Math.floor(viewport.height * ratio);
        setSize({ width: viewport.width, height: viewport.height });
        render = page.render({
          canvas: target,
          viewport,
          ...(ratio === 1 ? {} : { transform: [ratio, 0, 0, ratio, 0, 0] }),
        });
        return render.promise.then(() => {
          if (live) setState('drawn');
        });
      })
      .catch((reason: unknown) => {
        if (live && !(reason instanceof Error && reason.name === 'RenderingCancelledException')) setState('failed');
      });
    return () => {
      live = false;
      render?.cancel();
    };
  }, [near, doc, number, scale]);

  return (
    <div
      ref={frame}
      role="img"
      aria-label={`Page ${number} of ${props.pages}`}
      data-page={number}
      data-page-state={state}
      className="relative shrink-0 shadow-sm"
      // Paper is white in either theme, as the file draws it.
      style={{ width: size.width, height: size.height, background: '#fff' }}
    >
      <canvas ref={canvas} aria-hidden className="block" style={{ width: size.width, height: size.height }} />
      {state === 'failed' && (
        <p className="absolute inset-0 flex items-center justify-center bg-card text-sm text-ink-2">
          Could not draw page {number}.
        </p>
      )}
    </div>
  );
}
