// The Projects layout feature (stream N). A stub from stream L so the app builds; stream N fills
// it in. The interface is documented in src/shell/README.md.

import { defineFeature } from '../shell/index.ts';

export const feature = defineFeature({ id: 'projects', layout: 'projects' });
