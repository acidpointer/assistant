import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import { createRequire } from 'node:module';
import { fileURLToPath, URL } from 'node:url';
const require = createRequire(import.meta.url);

/** Oxc handles TS/JSX and minification; Rolldown owns both module graphs. */
export default defineConfig({
  plugins: [react()],
  resolve: {
    alias: {
      '@': fileURLToPath(new URL('./src', import.meta.url)),
      // This dependency's browser export uses document.createElement. Its public
      // default export uses a static table and is safe in dedicated Web Workers.
      'decode-named-character-reference': require.resolve('decode-named-character-reference'),
    },
  },
  build: {
    target: ['chrome100', 'firefox102', 'safari15.4'],
    cssTarget: ['chrome100', 'firefox102', 'safari15.4'],
    assetsInlineLimit: 0,
    sourcemap: false,
    manifest: true,
  },
  worker: { format: 'es' },
  server: {
    proxy: {
      '/api': {
        target: 'http://127.0.0.1:8080',
        changeOrigin: true,
        configure(proxy) {
          // The production server compares Host and Origin. This loopback-only
          // development proxy rewrites both; production policy stays unchanged.
          proxy.on('proxyReq', (request) => {
            if (request.getHeader('origin')) request.setHeader('origin', 'http://127.0.0.1:8080');
          });
        },
      },
    },
  },
});
