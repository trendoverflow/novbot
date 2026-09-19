import fs from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import react from '@vitejs/plugin-react'
import { defineConfig, type ServerOptions } from 'vite'

const rootDir = path.dirname(fileURLToPath(import.meta.url))
const centerTarget = process.env.NOVBOT_CENTER_URL ?? 'http://127.0.0.1:8080'
const certDir = path.resolve(rootDir, '.certs')
const keyPath = path.join(certDir, 'localhost-key.pem')
const certPath = path.join(certDir, 'localhost.pem')

function optionalHttps(): ServerOptions['https'] | undefined {
  if (fs.existsSync(keyPath) && fs.existsSync(certPath)) {
    return {
      key: fs.readFileSync(keyPath),
      cert: fs.readFileSync(certPath),
    }
  }
  return undefined
}

// https://vite.dev/config/
export default defineConfig({
  plugins: [react()],
  server: {
    https: optionalHttps(),
    proxy: {
      // Browser always calls same-origin /v1; Vite forwards to center in dev.
      '/v1': {
        target: centerTarget,
        changeOrigin: true,
      },
      '/health': {
        target: centerTarget,
        changeOrigin: true,
      },
    },
  },
})
