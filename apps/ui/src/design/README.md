# design (stream L)

`EngineLogo` draws a neutral glyph per engine (a ringed dot for Claude Code, a hexagon for Codex,
a triangle for OpenCode), inline and in the text colour. They are not the vendors' marks: those
are trademarks whose terms restrict their use, and this repository is public.

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
| `Popover` | A non-modal popover for evidence and details: hover or focus previews it, activating it opens it. |
| `Avatar` | People are round with initials; agents are square and carry their owner's initial, and their name says "(agent of …)". |
| `Badge` | Counts and tags: `accent`, `warn`, `risk`, `neutral`, `plain`; `label` adds screen-reader text ("3 open asks"). |
| `StatusPill` | A status with a coloured dot. |
| `ResizablePanel` | A side panel with a draggable, focusable edge (ARIA window splitter: arrows, Home, End). |
| `Tree`, `TreeItem` | A navigation tree (ARIA treeview): one tab stop, arrows move and open, Home and End, type-ahead. Items are usually links (`TreeItem` wraps its child). |
| `ThemeToggle`, `useTheme`, `applyTheme` | Light, system or dark, persisted. |
| icons | A small stroke icon set (`HomeIcon`, `InboxIcon`, …), decorative (`aria-hidden`). |

## Popover

Built from the popover `projects/recap-text.tsx` built for recap evidence (stream N will switch
to this one; `src/projects` stays as it is until then). `children` is the trigger's content (often
text); `content` is what the popover shows. Four states, the same as a recap clause's:

- **`closed`**: nothing shown.
- **`hover`**: the pointer rests on the trigger for 300 ms (closes 200 ms after it leaves, unless
  it moved into the content, say to follow a link there); touch never previews.
- **`focus`**: the trigger has keyboard focus. The content is `inert`, so Tab moves on past it
  instead of landing inside a preview.
- **`open`**: activated (click, Enter or Space, or the trigger already focused). Focus moves into
  the content; Escape closes it and returns focus to the trigger, without previewing it again.

Renders inline by default, right after the trigger in the DOM (so it reads in document order and
positions against it with no extra setup); pass `portal` for a trigger inside a container that
would clip the content or stack it under something else.

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
