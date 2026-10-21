import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import { fileURLToPath, URL } from "node:url";

// Tauri dev host (e.g. when running on mobile / WSL). Empty in normal desktop dev.
const host = process.env.TAURI_DEV_HOST;

// https://vite.dev/config/
// Multi-page Vite setup so the `pet` and `panel` Tauri windows each load a
// distinct entry (pet.html / index.html) mounting their own Svelte app.
export default defineConfig({
  plugins: [svelte()],

  // Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
  //
  // 1. prevent Vite from obscuring rust errors
  clearScreen: false,
  // 2. tauri expects a fixed port, fail if that port is not available
  server: {
    port: 1420,
    strictPort: true,
    host: host || "127.0.0.1",
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 1421,
        }
      : undefined,
    watch: {
      // 3. tell Vite to ignore watching `src-tauri`
      ignored: ["**/src-tauri/**"],
    },
  },
  build: {
    rollupOptions: {
      input: {
        index: fileURLToPath(new URL("index.html", import.meta.url)),
        pet: fileURLToPath(new URL("pet.html", import.meta.url)),
      },
    },
  },
});
