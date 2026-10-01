import { ToggleGroup } from 'radix-ui';
import { useEffect } from 'react';
import { create } from 'zustand';
import { persist } from 'zustand/middleware';

export type ThemeChoice = 'light' | 'system' | 'dark';

/** UI-only state. `system` leaves `data-theme` off so the tokens follow the OS. */
export const useTheme = create<{ theme: ThemeChoice; setTheme(theme: ThemeChoice): void }>()(
  persist((set) => ({ theme: 'system', setTheme: (theme) => set({ theme }) }), {
    name: 'pitcrew.theme',
  }),
);

export function applyTheme(theme: ThemeChoice): void {
  const root = document.documentElement;
  if (theme === 'system') root.removeAttribute('data-theme');
  else root.setAttribute('data-theme', theme);
}

export function useApplyTheme(): void {
  const theme = useTheme((s) => s.theme);
  useEffect(() => applyTheme(theme), [theme]);
}

const CHOICES: { value: ThemeChoice; label: string }[] = [
  { value: 'light', label: 'Light' },
  { value: 'system', label: 'System' },
  { value: 'dark', label: 'Dark' },
];

export function ThemeToggle() {
  const theme = useTheme((s) => s.theme);
  const setTheme = useTheme((s) => s.setTheme);
  return (
    <ToggleGroup.Root
      type="single"
      value={theme}
      onValueChange={(value) => value !== '' && setTheme(value as ThemeChoice)}
      aria-label="Theme"
      className="inline-flex rounded-sm border border-line bg-sunken p-0.5"
    >
      {CHOICES.map((choice) => (
        <ToggleGroup.Item
          key={choice.value}
          value={choice.value}
          className="h-6 rounded-sm px-2 text-xs text-ink-2 data-[state=on]:bg-card data-[state=on]:text-ink data-[state=on]:shadow-sm"
        >
          {choice.label}
        </ToggleGroup.Item>
      ))}
    </ToggleGroup.Root>
  );
}
