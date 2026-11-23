import { defineConfig } from 'vite'
import { svelte } from '@sveltejs/vite-plugin-svelte'

export default defineConfig({
  base: '/linux-assets/',
  plugins: [svelte()],
  build: {
    cssCodeSplit: false,
    rollupOptions: {
      output: {
        entryFileNames: 'shell.js',
        chunkFileNames: '[name].js',
        assetFileNames: '[name][extname]',
      },
    },
  },
})
