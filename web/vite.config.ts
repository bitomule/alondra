import { defineConfig } from 'vite';
import { reactRouter } from '@react-router/dev/vite';
export default defineConfig({
  plugins: [reactRouter()],
  // Prerender fetches must use the same loopback address as the preview listener.
  preview: { host: '127.0.0.1' },
});
