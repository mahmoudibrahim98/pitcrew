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
