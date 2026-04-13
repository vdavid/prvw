import { defineConfig } from 'vite'
import { sveltekit } from '@sveltejs/kit/vite'

const host = process.env.TAURI_DEV_HOST

export default defineConfig(async () => ({
    plugins: [sveltekit()],

    build: {
        chunkSizeWarningLimit: 1000,
        rollupOptions: {
            checks: { pluginTimings: false },
        },
    },

    clearScreen: false,
    server: {
        port: 14200,
        strictPort: true,
        host: host || false,
        hmr: host
            ? {
                  protocol: 'ws',
                  host,
                  port: 14201,
              }
            : undefined,
        watch: {
            ignored: ['**/src-tauri/**'],
        },
    },
}))
