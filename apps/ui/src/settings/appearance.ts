import { useEffect } from 'react';
import { create } from 'zustand';
import { persist } from 'zustand/middleware';

export type Density = 'comfortable' | 'compact';
export const useAppearance = create<{ density: Density; setDensity(density: Density): void }>()(
  persist((set) => ({ density: 'comfortable', setDensity: (density) => set({ density }) }), { name: 'pitcrew.appearance' }),
);
export function useApplyDensity() {
  const density = useAppearance((s) => s.density);
  useEffect(() => { document.documentElement.dataset.density = density; }, [density]);
}
