# apps/ui

The PitCrew interface: React 19, TypeScript, Vite. Runs in the Tauri desktop app and, for development, in a browser against `apps/mock-hub`.

| Folder | Stream | What |
|---|---|---|
| `src/design` | L | Components on `@pitcrew/tokens` |
| `src/shell` | L | Sidebar, layouts and switcher, routing, palette, Orchestrator panel frame |
| `src/data` | L | API client, delta stream, query invalidation |
| `src/console` | M | Agent console |
| `src/projects` | N | Projects layout |
| `src/onboarding` | O | First-run and machine-setup wizards |

Stream L sets up the package (`package.json`, Vite and TypeScript config) first. Each feature folder exports its routes and navigation entries from `index.ts`; the shell composes them. See `docs/build/streams/`.

## Run it

```sh
npm run mock-hub                          # repo root: the fake daemon on 127.0.0.1:47317
corepack pnpm --filter @pitcrew/ui dev    # the UI on http://127.0.0.1:5173
```

| Script | What |
|---|---|
| `dev` | Vite dev server on 127.0.0.1:5173 |
| `build` | Type-check, then a production build into `dist/` |
| `preview` | Serve `dist/` on 127.0.0.1:4173 |
| `test` | Vitest: the data layer against the real mock hub |
| `e2e` | Playwright: the page against its own mock hub (ports 47399 and 5199). `PLAYWRIGHT_CHANNEL=msedge` or `chrome` uses an installed browser. |
| `typecheck` | `tsc -b` |
| `lint` | ESLint |

## Environment (development only)

| Variable | Default | What |
|---|---|---|
| `VITE_PITCREW_API` | `http://127.0.0.1:47317` | The API base URL. |
| `VITE_PITCREW_TOKEN` | `dev-device-token` | The bearer token the browser sends. Ignored outside `dev`, and a production build **fails** while it is set: Vite would inline it into the bundle, and the desktop app's webview never holds a token (ADR-0003). |

## Fonts

Geist and Geist Mono (SIL OFL 1.1) are bundled: the Latin subsets and Geist Mono's box-drawing
subset from `@fontsource-variable`, plus the full variable fonts in `src/assets/fonts` for arrows,
which no Fontsource subset has (loaded only when an arrow is on screen). No font CDN.
