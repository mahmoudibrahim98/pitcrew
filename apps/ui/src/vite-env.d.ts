/// <reference types="vite/client" />

interface ImportMetaEnv {
  /** The API base URL. Default `http://127.0.0.1:47317` (the mock hub). */
  readonly VITE_PITCREW_API?: string;
  /** A bearer token for browser development. Never set it for a production build. */
  readonly VITE_PITCREW_TOKEN?: string;
}

interface ImportMeta {
  readonly env: ImportMetaEnv;
}
