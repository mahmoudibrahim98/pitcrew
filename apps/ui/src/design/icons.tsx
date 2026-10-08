// A small stroke icon set on a 16-unit grid. Icons are decorative (aria-hidden); the control that
// holds one carries the accessible name.

import type { ComponentType, ReactNode, SVGProps } from 'react';
import { cx } from '../lib/cx.ts';

export type IconProps = Omit<SVGProps<SVGSVGElement>, 'children'>;
export type Icon = ComponentType<IconProps>;

function icon(name: string, paths: ReactNode): Icon {
  function Glyph({ className, ...props }: IconProps) {
    return (
      <svg
        viewBox="0 0 16 16"
        fill="none"
        stroke="currentColor"
        strokeWidth={1.5}
        strokeLinecap="round"
        strokeLinejoin="round"
        aria-hidden
        focusable={false}
        className={cx('size-4 shrink-0', className)}
        {...props}
      >
        {paths}
      </svg>
    );
  }
  Glyph.displayName = `Icon${name}`;
  return Glyph;
}

export const HomeIcon = icon(
  'Home',
  <>
    <path d="M2.5 7 8 2.5 13.5 7" />
    <path d="M4 6v7.5h8V6" />
    <path d="M6.5 13.5v-4h3v4" />
  </>,
);

export const InboxIcon = icon(
  'Inbox',
  <>
    <path d="M2.5 9.5h3l1 2h3l1-2h3" />
    <path d="M4.2 3.5h7.6l1.7 6v3.5a.5.5 0 0 1-.5.5h-11a.5.5 0 0 1-.5-.5V9.5z" />
  </>,
);

export const CheckCircleIcon = icon(
  'CheckCircle',
  <>
    <circle cx="8" cy="8" r="5.5" />
    <path d="m5.8 8.2 1.5 1.5 3-3.2" />
  </>,
);

export const ConsoleIcon = icon(
  'Console',
  <>
    <rect x="2" y="3" width="12" height="10" rx="1.5" />
    <path d="m4.8 6.5 2 1.7-2 1.8" />
    <path d="M8.5 10.2h2.8" />
  </>,
);

export const FolderIcon = icon(
  'Folder',
  <path d="M2 4.5a1 1 0 0 1 1-1h3l1.5 1.5H13a1 1 0 0 1 1 1V12a1 1 0 0 1-1 1H3a1 1 0 0 1-1-1z" />,
);

export const WorkstreamIcon = icon(
  'Workstream',
  <>
    <circle cx="4.5" cy="4" r="1.5" />
    <circle cx="4.5" cy="12" r="1.5" />
    <circle cx="11.5" cy="6" r="1.5" />
    <path d="M4.5 5.5v5" />
    <path d="M11.5 7.5c0 2.2-2.4 2.9-5.6 3.6" />
  </>,
);

export const SearchIcon = icon(
  'Search',
  <>
    <circle cx="7" cy="7" r="4.5" />
    <path d="m10.5 10.5 3 3" />
  </>,
);

export const PlusIcon = icon('Plus', <path d="M8 3.5v9M3.5 8h9" />);

export const SparkleIcon = icon(
  'Sparkle',
  <>
    <path d="M7.5 2.5 8.8 5.7 12 7 8.8 8.3 7.5 11.5 6.2 8.3 3 7l3.2-1.3z" />
    <path d="M12.5 11v3M11 12.5h3" />
  </>,
);

export const SidebarIcon = icon(
  'Sidebar',
  <>
    <rect x="2" y="2.5" width="12" height="11" rx="1.5" />
    <path d="M6 2.5v11" />
  </>,
);

export const PanelRightIcon = icon(
  'PanelRight',
  <>
    <rect x="2" y="2.5" width="12" height="11" rx="1.5" />
    <path d="M10 2.5v11" />
  </>,
);

export const ChevronRightIcon = icon('ChevronRight', <path d="m6 4 4 4-4 4" />);

export const ChevronDownIcon = icon('ChevronDown', <path d="m4 6 4 4 4-4" />);

export const ChevronsUpDownIcon = icon('ChevronsUpDown', <path d="m5 6 3-3 3 3M5 10l3 3 3-3" />);

export const CloseIcon = icon('Close', <path d="m4 4 8 8M12 4l-8 8" />);

export const CheckIcon = icon('Check', <path d="m3.5 8.5 3 3 6-7" />);

export const CommandIcon = icon(
  'Command',
  <path d="M6 6V4.5A1.5 1.5 0 1 0 4.5 6H6Zm0 0h4m-4 0v4m4-4V4.5A1.5 1.5 0 1 1 11.5 6H10Zm0 0v4m0 0h1.5A1.5 1.5 0 1 1 10 11.5V10Zm0 0H6m0 0v1.5A1.5 1.5 0 1 1 4.5 10H6Z" />,
);

/** Split the pane to the right: a frame cut down the middle. */
export const SplitRightIcon = icon(
  'SplitRight',
  <>
    <rect x="2" y="2.5" width="12" height="11" rx="1.5" />
    <path d="M8 2.5v11" />
  </>,
);

/** Split the pane down: a frame cut across the middle. */
export const SplitDownIcon = icon(
  'SplitDown',
  <>
    <rect x="2" y="2.5" width="12" height="11" rx="1.5" />
    <path d="M2 8h12" />
  </>,
);

/** A priority: a flag on a pole. */
export const FlagIcon = icon(
  'Flag',
  <>
    <path d="M3.5 14V2.5" />
    <path d="M3.5 3h8l-1.8 2.75L11.5 8.5h-8" />
  </>,
);

/** A person: head and shoulders. */
export const UserIcon = icon(
  'User',
  <>
    <circle cx="8" cy="5.5" r="2.5" />
    <path d="M3 13.5c.6-2.4 2.6-3.8 5-3.8s4.4 1.4 5 3.8" />
  </>,
);

/** A date: a calendar page. */
export const CalendarIcon = icon(
  'Calendar',
  <>
    <rect x="2.5" y="3.5" width="11" height="10" rx="1.5" />
    <path d="M2.5 6.5h11M5.5 2v3M10.5 2v3" />
  </>,
);

/** A label: a tag with its hole. */
export const TagIcon = icon(
  'Tag',
  <>
    <path d="M2.5 3.5v4l6 6 5-5-6-6h-4a1 1 0 0 0-1 1Z" />
    <circle cx="5.5" cy="5.5" r="0.75" />
  </>,
);
