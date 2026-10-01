// Tailwind v4 computes `outline-style: none` for the `outline-none` utility. Pairing that with
// `focus-visible:outline-2` alone draws nothing: the ring gets a width but no style to render it
// with. `FOCUS_RING` adds `outline-solid` so the ring actually shows.
//
// Use it on anything that sets `outline-none` for its resting state and wants a visible
// keyboard-focus ring: `className={cx('... outline-none ...', FOCUS_RING)}`.
export const FOCUS_RING = 'focus-visible:outline-2 focus-visible:outline-accent focus-visible:outline-solid';
