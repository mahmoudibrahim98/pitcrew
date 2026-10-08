export const SECTIONS = [
  ['profile', 'Profile'], ['workspace', 'Workspace'], ['machines', 'Machines'],
  ['agents', 'Agents'], ['hooks', 'Hooks'], ['safety', 'Safety'],
  ['integrations', 'Integrations'], ['appearance', 'Appearance'],
  ['keyboard-shortcuts', 'Keyboard shortcuts'], ['updates', 'Updates'], ['about', 'About'],
] as const;
export type SettingsSection = typeof SECTIONS[number][0];
