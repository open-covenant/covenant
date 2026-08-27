/// <reference types="vite/client" />

interface ImportMetaEnv {
  readonly VITE_COMPUTE_DEMO?: string;
}

interface ImportMeta {
  readonly env: ImportMetaEnv;
}
