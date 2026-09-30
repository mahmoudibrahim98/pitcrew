// What features import from the shell. See README.md in this folder.

export {
  defineFeature,
  type Command,
  type CommandContext,
  type CreateEntry,
  type Feature,
  type LayoutId,
  type LayoutScope,
  type NavEntry,
} from './feature.ts';
export { LAYOUTS, useLayout, useWorkspaceId } from './layout.ts';
export { paths } from './paths.ts';
export type { WorkspaceRoute } from './routes.tsx';
