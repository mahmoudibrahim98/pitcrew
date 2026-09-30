# @pitcrew/tokens

Colours, type, spacing and radii for PitCrew, as CSS custom properties (`tokens.css`) and JSON
(`tokens.json`). Every property is named `--pc-<name>`.

- **Look:** quiet, neutral surfaces with one indigo accent, in the spirit of modern project tools.
  Status colours carry meaning only: `ok` (on track, done), `warn` (waiting, at risk), `risk`
  (blocked, failed), `progress` (in progress).
- **Themes:** light by default. Dark follows the system unless `data-theme="light"` is set on
  `<html>`; `data-theme="dark"` forces dark.
- **Fonts:** Geist and Geist Mono (SIL Open Font License). The desktop app must **bundle** them
  (stream L) — no font CDN at runtime, so the app works offline and leaks nothing.
- **Motion:** use `--pc-duration-*`; they drop to 0 under `prefers-reduced-motion`.

Owned by stream 0. Stream L builds the component library on these tokens; propose changes in a
`s/0/contract-tokens-…` pull request.