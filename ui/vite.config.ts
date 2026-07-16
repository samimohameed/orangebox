import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import { viteSingleFile } from 'vite-plugin-singlefile'

// The production build is a single self-contained index.html that the Rust
// binary embeds at compile time (crates/cli/src/ui.rs), so `cargo install`
// ships the whole GUI. In dev, /api proxies to a running `orangebox ui`.
export default defineConfig({
  plugins: [react(), viteSingleFile()],
  server: {
    proxy: {
      '/api': 'http://127.0.0.1:7171',
    },
  },
})
