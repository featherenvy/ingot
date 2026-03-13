/// <reference types="vitest/config" />
import { fileURLToPath, URL } from 'node:url'
import tailwindcss from '@tailwindcss/vite'
import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

export default defineConfig({
  plugins: [react(), tailwindcss()],
  build: {
    rollupOptions: {
      output: {
        manualChunks(id) {
          if (!id.includes('/node_modules/')) return

          if (
            id.includes('/@tanstack/react-query/') ||
            id.includes('/react-router/') ||
            id.includes('/zustand/')
          ) {
            return 'app-runtime'
          }

          if (
            id.includes('/radix-ui/') ||
            id.includes('/react-hook-form/') ||
            id.includes('/sonner/') ||
            id.includes('/lucide-react/')
          ) {
            return 'ui-kit'
          }

          return 'vendor'
        },
      },
    },
  },
  resolve: {
    alias: {
      '@': fileURLToPath(new URL('./src', import.meta.url)),
    },
  },
  server: {
    port: 4191,
    proxy: {
      '/api': {
        target: 'http://127.0.0.1:4190',
        changeOrigin: true,
        ws: true,
      },
    },
  },
  test: {
    environment: 'jsdom',
    globals: true,
    setupFiles: ['./src/test/setup.ts'],
  },
})
