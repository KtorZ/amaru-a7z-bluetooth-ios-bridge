import { defineConfig } from "vite";

const host = process.env.TAURI_DEV_HOST;

export default defineConfig({
  clearScreen: false,
  server: {
    host: host ?? false,
    port: 1420,
    strictPort: true,
  },
});
