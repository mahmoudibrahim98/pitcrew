# design (stream L)

Components built on `@pitcrew/tokens` and Radix primitives. Import from `index.ts`. Every one is
keyboard operable, works light and dark (Tailwind classes resolve to the `--pc-*` tokens), and
honours reduced motion through the tokens' durations. See `docs/build/streams/L.md`.

| Component | What |
|---|---|
| `Button` | `primary`, `secondary`, `ghost`; `asChild` to style a link. |
| `Menu`, `MenuTrigger`, `MenuContent`, `MenuItem`, `MenuLabel`, `MenuSeparator`, `MenuRadioGroup`, `MenuRadioItem` | Dropdown menus (Radix). Items take an `icon` and a shortcut hint (`keys`). |
| `Dialog`, `DialogContent`, `DialogFooter`, `DialogClose` | Modal dialogs (Radix): focus trapped, Esc closes. `DialogContent` takes `title` (required; `hideTitle` keeps it for screen readers only) and `description`. |
| `Tooltip`, `TooltipProvider` | A label on hover and focus, with an optional shortcut. It supplements an accessible name, never replaces one. |
| `Kbd` | A shortcut hint: `<Kbd keys={['mod', 'k']} />` reads Ctrl K, or ⌘ K on macOS. |
| `Avatar` | People are round with initials; agents are square and carry their owner's initial, and their name says "(agent of …)". |
| `Badge` | Counts and tags: `accent`, `warn`, `risk`, `neutral`, `plain`; `label` adds screen-reader text ("3 open asks"). |
| `StatusPill` | A status with a coloured dot. |
| `ResizablePanel` | A side panel with a draggable, focusable edge (ARIA window splitter: arrows, Home, End). |
| `Tree`, `TreeItem` | A navigation tree (ARIA treeview): one tab stop, arrows move and open, Home and End, type-ahead. Items are usually links (`TreeItem` wraps its child). |
| `ThemeToggle`, `useTheme`, `applyTheme` | Light, system or dark, persisted. |
| icons | A small stroke icon set (`HomeIcon`, `InboxIcon`, …), decorative (`aria-hidden`). |

## Focus rings

Tailwind v4 computes `outline-style: none` for the `outline-none` utility, so pairing it with
`focus-visible:outline-2` alone draws no ring: there is a width but no style to render it with.
Anything that sets `outline-none` for its resting state and wants a visible keyboard-focus ring
appends `FOCUS_RING` (`focus.ts`): `className={cx('... outline-none ...', FOCUS_RING)}`.

## Contrast

Text meets WCAG AA (4.5:1) on every surface in both themes. The `muted` token is for icons and
borders, not text: it is about 3.5:1 on light surfaces. The `ok`, `warn` and `progress` colours
are too light for small text on their soft backgrounds in the light theme, so pills and badges
put ink text on them (or `on-accent` text on the solid colour), and the colour goes in a dot or
the background.
