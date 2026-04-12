// @ts-check
import { defineConfig } from 'astro/config'
import tailwindcss from '@tailwindcss/vite'
import sitemap from '@astrojs/sitemap'

// https://astro.build/config
export default defineConfig({
  site: 'https://getprvw.com',
  output: 'static',
  integrations: [sitemap()],
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
