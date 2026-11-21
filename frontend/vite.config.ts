/// <reference types="vitest/config" />
import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'

// TASK-9設計7章・D5: dev server portは40512。/api は本番serve(40510)へproxy。
// 本番frontendはrust-embedでbinary埋め込みされる(dist二階建て・設計10章)。
export default defineConfig({
  plugins: [vue()],
  server: {
    port: 40512,
    strictPort: true,
    proxy: {
      '/api': 'http://127.0.0.1:40510',
    },
  },
  build: {
    outDir: 'dist',
  },
  test: {
    environment: 'node',
  },
})
