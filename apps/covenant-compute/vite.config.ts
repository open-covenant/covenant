import react from '@vitejs/plugin-react';
import { defineConfig } from 'vite';

export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    host: '127.0.0.1',
    port: 1420,
    strictPort: true,
  },
  preview: {
    host: '127.0.0.1',
    port: 1421,
    strictPort: true,
  },
  build: {
    target: ['es2022', 'chrome105', 'safari13'],
    sourcemap: true,
  },
});
