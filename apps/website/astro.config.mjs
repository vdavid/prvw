// @ts-check
import { defineConfig } from 'astro/config'
import tailwindcss from '@tailwindcss/vite'

// https://astro.build/config
export default defineConfig({
  site: 'https://getprvw.com',
  output: 'static',
  server: {
    port: 14829,
  },
  vite: {
    server: {
      strictPort: true,
    },
    plugins: [tailwindcss()],
  },
})
