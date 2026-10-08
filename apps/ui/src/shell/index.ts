// What features import from the shell. See README.md in this folder.

export {
  defineFeature,
  type Command,
  type CommandContext,
  type CreateDefaults,
  type CreateEntry,
  type Feature,
  type LayoutId,
  type LayoutScope,
  type NavEntry,
} from './feature.ts';
export { LAYOUTS, useLayout, useWorkspaceId } from './layout.ts';
export { paths } from './paths.ts';
export { ownsShellKeys, SHELL_KEYS_ATTRIBUTE, SHORTCUTS } from './shortcuts.ts';
export type { ShellRootRoute, WorkspaceRoute } from './routes.tsx';
