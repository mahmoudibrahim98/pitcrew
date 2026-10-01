import { createContext, use } from 'react';
import type { Registry } from './registry.ts';

export const RegistryContext = createContext<Registry | null>(null);

/** The composed features. Provided by the router the shell creates. */
export function useRegistry(): Registry {
  const registry = use(RegistryContext);
  if (registry === null) throw new Error('useRegistry must be used inside the shell router');
  return registry;
}
