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

Each feature folder exports `feature` from `index.ts` (its routes, sidebar entries, palette commands and "+ New" items); `src/router.tsx` hands them to the shell, which composes them. The interface is in [`src/shell/README.md`](src/shell/README.md). See `docs/build/streams/`.

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
| `test` | Vitest: the data layer, the shell and the design components, against the real mock hub on a free port |
| `e2e` | Playwright: the shell against its own mock hub (ports 47399 and 5199; `E2E_HUB_PORT` and `E2E_UI_PORT` move them), with axe checks. `PLAYWRIGHT_CHANNEL=msedge` or `chrome` uses an installed browser. |
| `size` | After `build`: fails if the initial JS is over 250 kB gzipped |
| `typecheck` | `tsc -b` |
| `lint` | ESLint |

The dev server also serves the data layer's proof page at `/dev/proof`; production builds leave it out.

### Running the e2e suite against a real `pitcrewd`

By default `corepack pnpm --filter @pitcrew/ui e2e` starts its own mock hub (`apps/mock-hub`) and
talks to that. To run the same suite against a real daemon instead:

```sh
pitcrewd serve --demo --listen tcp:127.0.0.1:47317   # a separate shell; --demo seeds the demo workspace
pitcrewd token show-path                             # prints where the device token file is kept
```

Then, with that file's contents as the token:

```sh
E2E_HUB_URL=http://127.0.0.1:47317 E2E_HUB_TOKEN=<token from the file above> \
  corepack pnpm --filter @pitcrew/ui e2e
```

`E2E_HUB_URL` points the suite at that hub and stops `playwright.config.ts` from starting the mock
hub; `E2E_HUB_TOKEN` is the bearer token both the UI (`VITE_PITCREW_TOKEN`) and the specs' own
direct hub calls use — the mock hub's fixed `dev-device-token` (the default when `E2E_HUB_TOKEN` is
unset) is not a token `pitcrewd` recognizes. `tcp:127.0.0.1:<port>` is for development only:
`pitcrewd serve` without `--listen` uses the platform's private transport, which this suite cannot
reach. See `apps/ui/e2e/helpers.ts`.

## Environment (development only)

| Variable | Default | What |
|---|---|---|
| `VITE_PITCREW_API` | `http://127.0.0.1:47317` | The API base URL. |
| `VITE_PITCREW_TOKEN` | `dev-device-token` | The bearer token the browser sends. Ignored outside `dev`, and a production build **fails** while it is set: Vite would inline it into the bundle, and the desktop app's webview never holds a token (ADR-0003). |

## Fonts

Geist and Geist Mono (SIL OFL 1.1) are bundled: the Latin subsets and Geist Mono's box-drawing
subset from `@fontsource-variable`, plus the full variable fonts in `src/assets/fonts` for arrows,
which no Fontsource subset has (loaded only when an arrow is on screen). No font CDN.
