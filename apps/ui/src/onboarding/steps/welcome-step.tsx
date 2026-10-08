// Step 1 (first run only): theme and density. Both apply live, through the same `useTheme` store
// the rest of the app reads, so the choice is visible immediately.

import { RadioGroup } from 'radix-ui';
import { applyTheme, useTheme, type ThemeChoice } from '../../design/index.ts';
import { cx } from '../../lib/cx.ts';
import { StepFooter } from '../step-footer.tsx';
import { useWizard } from '../wizard-context.tsx';
import type { Density } from '../wizard-state.ts';
import { useAppearance } from '../../settings/appearance.ts';

const THEMES: { value: ThemeChoice; label: string }[] = [
  { value: 'light', label: 'Light' },
  { value: 'system', label: 'System' },
  { value: 'dark', label: 'Dark' },
];

const DENSITIES: { value: Density; label: string; hint: string }[] = [
  { value: 'comfortable', label: 'Comfortable', hint: 'More space between rows' },
  { value: 'compact', label: 'Compact', hint: 'Fit more on screen' },
];

export function WelcomeStep() {
  const { state, patch, next } = useWizard();
  const setTheme = useTheme((s) => s.setTheme);

  return (
    <form
      onSubmit={(e) => {
        e.preventDefault();
        next();
      }}
    >
      <p className="text-sm text-ink-2">
        PitCrew tracks your AI coding agents across every machine you work on. Let's get your first
        workspace set up.
      </p>

      <fieldset className="mt-6 flex flex-col gap-2">
        <legend className="text-sm font-medium text-ink">Theme</legend>
        <RadioGroup.Root
          value={state.theme}
          onValueChange={(value) => {
            const theme = value as ThemeChoice;
            patch({ theme });
            setTheme(theme);
            applyTheme(theme);
          }}
          className="flex gap-2"
        >
          {THEMES.map((t) => (
            <RadioGroup.Item
              key={t.value}
              value={t.value}
              className={cx(
                'rounded-sm border px-3 py-1.5 text-sm',
                state.theme === t.value
                  ? 'border-accent bg-accent-soft text-accent-text'
                  : 'border-line text-ink-2 hover:bg-hover',
              )}
            >
              {t.label}
            </RadioGroup.Item>
          ))}
        </RadioGroup.Root>
      </fieldset>

      <fieldset className="mt-4 flex flex-col gap-2">
        <legend className="text-sm font-medium text-ink">Density</legend>
        <RadioGroup.Root
          value={state.density}
          onValueChange={(value) => { patch({ density: value as Density }); useAppearance.getState().setDensity(value as Density); }}
          className="flex gap-2"
        >
          {DENSITIES.map((d) => (
            <RadioGroup.Item
              key={d.value}
              value={d.value}
              className={cx(
                'rounded-sm border px-3 py-1.5 text-left text-sm',
                state.density === d.value
                  ? 'border-accent bg-accent-soft text-accent-text'
                  : 'border-line text-ink-2 hover:bg-hover',
              )}
            >
              <span className="block font-medium">{d.label}</span>
              <span className="block text-xs text-ink-2">{d.hint}</span>
            </RadioGroup.Item>
          ))}
        </RadioGroup.Root>
      </fieldset>

      <StepFooter nextLabel="Get started" />
    </form>
  );
}
