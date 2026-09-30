// The first-run and machine-setup wizards (stream O). A stub from stream L so the app builds;
// stream O fills it in. The interface is documented in src/shell/README.md.

import { defineFeature } from '../shell/index.ts';

export const feature = defineFeature({ id: 'onboarding', layout: 'both' });
