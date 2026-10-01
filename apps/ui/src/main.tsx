import { RouterProvider } from '@tanstack/react-router';
import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { AppData } from './data/index.ts';
import { applyTheme, useTheme } from './design/index.ts';
import './index.css';
import { router } from './router.tsx';

// Before the first paint, so a persisted theme does not flash (the store reads localStorage
// synchronously).
applyTheme(useTheme.getState().theme);

const root = document.getElementById('root');
if (root === null) throw new Error('index.html has no #root');

// The data layer picks its transport once: the desktop gateway in the app, HTTP in a browser.
createRoot(root).render(
  <StrictMode>
    <AppData>
      <RouterProvider router={router} />
    </AppData>
  </StrictMode>,
);
