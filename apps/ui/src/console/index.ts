// The Agent console feature (stream M). A stub from stream L so the app builds; stream M fills it
// in. The interface is documented in src/shell/README.md.

import { defineFeature } from '../shell/index.ts';

export const feature = defineFeature({ id: 'console', layout: 'console' });
