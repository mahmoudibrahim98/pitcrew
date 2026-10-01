// Where recap days begin. The app leaves it to the data layer, which uses the viewer's own offset;
// tests run against the mock hub, which only has days for `tz=0`, and set it with this provider.

import { createContext, use, type ReactNode } from 'react';

const RecapTzContext = createContext<number | undefined>(undefined);

/** Minutes east of UTC for every day paragraph below; without it, the viewer's own offset. */
export function RecapTzProvider({ tz, children }: { tz: number; children: ReactNode }) {
  return <RecapTzContext value={tz}>{children}</RecapTzContext>;
}

export function useRecapTz(): number | undefined {
  return use(RecapTzContext);
}
