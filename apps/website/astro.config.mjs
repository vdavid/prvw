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
    // @ts-expect-error - @tailwindcss/vite uses vite 8 types, Astro bundles vite 6. Harmless at runtime.
    plugins: [tailwindcss()],
  },
})
