import { RouterProvider } from '@tanstack/react-router';
import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { apiBaseUrl, apiToken, createApi, createQueryClient, DataProvider } from './data/index.ts';
import './index.css';
import { router } from './router.tsx';

const api = createApi({ baseUrl: apiBaseUrl, token: apiToken });
const queryClient = createQueryClient();

const root = document.getElementById('root');
if (root === null) throw new Error('index.html has no #root');

createRoot(root).render(
  <StrictMode>
    <DataProvider api={api} queryClient={queryClient} token={apiToken}>
      <RouterProvider router={router} />
    </DataProvider>
  </StrictMode>,
);
